use super::*;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Instant;

use super::native::{required_directory, tree_digest};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_OUTPUT: usize = 4 * 1024 * 1024;

fn invoke_reforger(root: &Path, artifacts: &Path, step: &str, args: &[&str]) -> (i32, Value) {
    let mut command_args = vec![
        "--config-dir".to_owned(),
        root.to_string_lossy().into_owned(),
        "--json".to_owned(),
    ];
    command_args.extend(args.iter().map(|arg| (*arg).to_owned()));
    command_args.extend(["--game-space".to_owned(), "reforger".to_owned()]);
    fs::write(
        artifacts.join(format!("{step}.command.json")),
        serde_json::to_vec_pretty(&command_args).unwrap(),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(&command_args[3..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let started = Instant::now();
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("{step}: {error}"));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let status = child
        .wait()
        .unwrap_or_else(|error| panic!("{step}: {error}"));
    let stdout = stdout_reader.join().unwrap();
    let stderr = stderr_reader.join().unwrap();
    fs::write(artifacts.join(format!("{step}.stdout")), &stdout).unwrap();
    fs::write(artifacts.join(format!("{step}.stderr")), &stderr).unwrap();
    assert!(stdout.len() <= MAX_OUTPUT, "{step} stdout exceeded limit");
    assert!(stderr.len() <= MAX_OUTPUT, "{step} stderr exceeded limit");
    let value = serde_json::from_slice(&stdout).unwrap_or_else(|_| {
        panic!(
            "{step} emitted no JSON: {}",
            String::from_utf8_lossy(&stderr)
        )
    });
    fs::write(
        artifacts.join(format!("{step}.json")),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
    eprintln!(
        "D20 step={step} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    (status.code().unwrap_or(1), value)
}

fn read_bounded(mut reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        if bytes.len() < MAX_OUTPUT + 1 {
            let retain = read.min(MAX_OUTPUT + 1 - bytes.len());
            bytes.extend_from_slice(&buffer[..retain]);
        }
    }
    bytes
}

fn fresh_run(parent: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for attempt in 0..100u32 {
        let path = parent.join(format!(
            "d20-{:x}-{nonce:x}-{attempt:x}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("Cannot create acceptance directory: {error}"),
        }
    }
    panic!("Cannot allocate a fresh acceptance directory")
}

fn step(root: &Path, artifacts: &Path, label: &str, args: &[String]) -> (i32, Value) {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    invoke_reforger(root, artifacts, label, &refs)
}

fn installation_args(
    operation: &str,
    phase: &str,
    id: &str,
    directory: &Path,
    channel: Option<&str>,
    account: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "server".into(),
        "installation".into(),
        operation.into(),
        phase.into(),
        id.into(),
        "--directory".into(),
        directory.to_string_lossy().into_owned(),
    ];
    if let Some(channel) = channel {
        args.extend(["--channel".into(), channel.into()]);
    }
    if let Some(account) = account {
        args.extend(["--account".into(), account.into()]);
    } else {
        args.push("--anonymous".into());
    }
    args
}

fn expected_app(channel: &str) -> u64 {
    if channel == "stable" {
        1_874_900
    } else {
        1_890_870
    }
}

fn depot_manifest(value: &Value) -> Vec<(u64, String)> {
    let mut depots = value
        .as_array()
        .unwrap()
        .iter()
        .map(|depot| {
            (
                depot["depot_id"].as_u64().unwrap(),
                depot["manifest_gid"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| depot["manifest_gid"].to_string()),
            )
        })
        .collect::<Vec<_>>();
    depots.sort();
    depots
}

fn verify_receipt(directory: &Path, channel: &str) -> Value {
    let receipt: Value =
        serde_json::from_slice(&fs::read(directory.join("foxy-installation.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["game"], "reforger");
    assert_eq!(
        receipt["platform"],
        if cfg!(windows) {
            "windows_x64"
        } else {
            "linux_x64"
        }
    );
    assert_eq!(receipt["app_id"], expected_app(channel));
    if channel == "stable" {
        assert!(receipt.get("channel").is_none() || receipt["channel"] == "stable");
    } else {
        assert_eq!(receipt["channel"], "experimental");
    }
    assert_eq!(receipt["branch"], "public");
    assert!(receipt["build_id"].as_u64().is_some());
    assert!(!receipt["depots"].as_array().unwrap().is_empty());
    let executable = if cfg!(windows) {
        "ArmaReforgerServer.exe"
    } else {
        "ArmaReforgerServer"
    };
    assert_eq!(receipt["executable"]["path"], executable);
    assert!(directory.join(executable).is_file());
    receipt
}

fn damageable_file(root: &Path) -> PathBuf {
    let mut stack = vec![root.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.len() > 32 * 1024
                || path
                    .file_name()
                    .is_some_and(|name| name == "foxy-installation.json")
            {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 != 0 {
                    continue;
                }
            }
            #[cfg(windows)]
            if path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    ["exe", "com", "bat", "cmd", "ps1"]
                        .iter()
                        .any(|value| extension.eq_ignore_ascii_case(value))
                })
            {
                continue;
            }
            candidates.push(path);
        }
    }
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .expect("no small data file to damage")
}

#[cfg(unix)]
fn make_parent_writable(path: &Path, root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut parent = path.parent().unwrap();
    loop {
        assert!(parent.starts_with(root));
        let metadata = fs::metadata(parent).unwrap();
        fs::set_permissions(
            parent,
            fs::Permissions::from_mode(metadata.permissions().mode() | 0o700),
        )
        .unwrap();
        if parent == root {
            break;
        }
        parent = parent.parent().unwrap();
    }
}

fn create_instance(root: &Path, artifacts: &Path, name: &str) -> String {
    let (code, value) = invoke_reforger(
        root,
        artifacts,
        &format!("{name}_add"),
        &["server", "instance", "add", "--name", name],
    );
    assert_eq!(code, 0, "{}", value["errors"]);
    value["data"]["instance"]["id"].as_str().unwrap().to_owned()
}

fn show(root: &Path, artifacts: &Path, id: &str, step_name: &str) -> Value {
    let (code, value) = invoke_reforger(
        root,
        artifacts,
        step_name,
        &["server", "instance", "show", id],
    );
    assert_eq!(code, 0, "{}", value["errors"]);
    value
}

fn catalog(root: &Path, artifacts: &Path, label: &str) -> Value {
    let (code, value) = invoke_reforger(root, artifacts, label, &["server", "instance", "list"]);
    assert_eq!(code, 0, "{}", value["errors"]);
    value["data"]["catalog"]["instances"].clone()
}

struct Fixture<'a> {
    root: &'a Path,
    artifacts: &'a Path,
    account: Option<&'a str>,
}

impl Fixture<'_> {
    fn reviewed_apply(
        &self,
        operation: &str,
        id: &str,
        directory: &Path,
        channel: Option<&str>,
        label: &str,
    ) -> Value {
        let plan_args = installation_args(operation, "plan", id, directory, channel, self.account);
        let (code, plan) = step(
            self.root,
            self.artifacts,
            &format!("{label}_plan"),
            &plan_args,
        );
        assert_eq!(code, 0, "{}", plan["errors"]);
        let plan = &plan["data"]["plan"];
        assert!(plan["blockers"].as_array().unwrap().is_empty(), "{plan}");
        let expected_channel = channel.unwrap_or_else(|| {
            if label.starts_with("stable") {
                "stable"
            } else {
                "experimental"
            }
        });
        assert_eq!(plan["channel"], expected_channel);
        assert_eq!(plan["app_id"], expected_app(expected_channel));
        assert_eq!(
            plan["platform"],
            if cfg!(windows) {
                "windows_x64"
            } else {
                "linux_x64"
            }
        );
        assert!(plan["review"]["selection"]["build_id"].as_u64().is_some());
        let fingerprint = plan["fingerprint"].as_str().unwrap().to_owned();
        let mut dry_args =
            installation_args(operation, "apply", id, directory, channel, self.account);
        dry_args.extend(["--expected-plan-fingerprint".into(), fingerprint.clone()]);
        let dry_args = vec!["--dry-run".into()]
            .into_iter()
            .chain(dry_args)
            .collect::<Vec<_>>();
        let (code, dry) = step(
            self.root,
            self.artifacts,
            &format!("{label}_dry_apply"),
            &dry_args,
        );
        assert_eq!(code, 0, "{}", dry["errors"]);
        assert_eq!(dry["data"]["writes_performed"], false);
        assert_eq!(dry["data"]["downloads_started"], false);
        let mut apply_args =
            installation_args(operation, "apply", id, directory, channel, self.account);
        apply_args.extend([
            "--expected-plan-fingerprint".into(),
            fingerprint,
            "--yes".into(),
        ]);
        let (code, applied) = step(
            self.root,
            self.artifacts,
            &format!("{label}_apply"),
            &apply_args,
        );
        assert_eq!(code, 0, "{}", applied["errors"]);
        assert_eq!(applied["data"]["outcome"]["bound"], true);
        let receipt: Value =
            serde_json::from_slice(&fs::read(directory.join("foxy-installation.json")).unwrap())
                .unwrap();
        let plan_file: Value = serde_json::from_slice(
            &fs::read(self.artifacts.join(format!("{label}_plan.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            receipt["build_id"],
            plan_file["data"]["plan"]["review"]["selection"]["build_id"]
        );
        assert_eq!(
            depot_manifest(&receipt["depots"]),
            depot_manifest(&plan_file["data"]["plan"]["review"]["selection"]["depots"])
        );
        applied
    }
}

fn validate(root: &Path, artifacts: &Path, id: &str, label: &str) {
    let (code, value) = invoke_reforger(
        root,
        artifacts,
        label,
        &["server", "installation", "validate", id],
    );
    assert_eq!(code, 0, "{}", value["errors"]);
    assert_eq!(value["data"]["verification"]["valid"], true);
    assert_eq!(value["data"]["writes_performed"], false);
    assert_eq!(value["data"]["downloads_started"], false);
}

#[test]
#[ignore = "downloads stable and experimental Reforger depots and exercises real CLI maintenance"]
fn native_reforger_channels_install_and_maintain() {
    #[cfg(target_os = "linux")]
    assert_ne!(unsafe { libc::geteuid() }, 0, "run as an ordinary user");

    let config = required_directory("FOXY_REFORGER_INSTALL_ROOT");
    let scratch = required_directory("FOXY_REFORGER_INSTALL_SCRATCH");
    let games: Value =
        serde_json::from_slice(&fs::read(config.join("games.json")).unwrap()).unwrap();
    assert!(
        games["game_spaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|space| { space["id"] == "reforger" && space["game_id"] == "reforger" })
    );
    assert!(!scratch.starts_with(&config) && !config.starts_with(&scratch));
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::metadata(&scratch).unwrap();
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    let run = fresh_run(&scratch);
    let artifacts = run.join("artifacts");
    fs::create_dir(&artifacts).unwrap();
    let installs = run.join("installations");
    fs::create_dir(&installs).unwrap();
    let account = std::env::var("FOXY_REFORGER_INSTALL_ACCOUNT").ok();
    let fixture = Fixture {
        root: &config,
        artifacts: &artifacts,
        account: account.as_deref(),
    };
    let before_instances = catalog(&config, &artifacts, "instances_before");
    let stable_id = create_instance(&config, &artifacts, "ChannelStable");
    let experimental_id = create_instance(&config, &artifacts, "ChannelExperimental");
    let stable = installs.join("Stable");
    let experimental = installs.join("Experimental");

    fixture.reviewed_apply("install", &stable_id, &stable, None, "stable_install");
    fixture.reviewed_apply(
        "install",
        &experimental_id,
        &experimental,
        Some("experimental"),
        "experimental_install",
    );
    let stable_receipt = verify_receipt(&stable, "stable");
    let experimental_receipt = verify_receipt(&experimental, "experimental");
    let stable_install_binding = show(&config, &artifacts, &stable_id, "stable_after_install");
    let experimental_install_binding = show(
        &config,
        &artifacts,
        &experimental_id,
        "experimental_after_install",
    );
    let stable_before = tree_digest(&stable);
    let experimental_before = tree_digest(&experimental);
    for (id, label) in [
        (&stable_id, "stable_initial"),
        (&experimental_id, "experimental_initial"),
    ] {
        validate(&config, &artifacts, id, &format!("{label}_validate"));
        let directory = if id == &stable_id {
            &stable
        } else {
            &experimental
        };
        let (code, inspected) = invoke_reforger(
            &config,
            &artifacts,
            &format!("{label}_inspect"),
            &[
                "server",
                "installation",
                "inspect",
                "--directory",
                directory.to_str().unwrap(),
            ],
        );
        assert_eq!(code, 0, "{}", inspected["errors"]);
        assert_eq!(inspected["data"]["installation"]["game"], "reforger");
        assert_eq!(
            inspected["data"]["installation"]["platform"],
            if cfg!(windows) {
                "windows_x64"
            } else {
                "linux_x64"
            }
        );
        assert_eq!(
            inspected["data"]["installation"]["executable"],
            if cfg!(windows) {
                "ArmaReforgerServer.exe"
            } else {
                "ArmaReforgerServer"
            }
        );
        assert_eq!(inspected["data"]["build_version_verified"], false);
    }

    let stable_marker = stable.join("operator-marker.txt");
    fs::write(&stable_marker, b"operator-owned stable marker\n").unwrap();
    let target_file = damageable_file(&stable);
    #[cfg(unix)]
    make_parent_writable(&target_file, &stable);
    fs::remove_file(&target_file).unwrap();
    let stable_predecessor = tree_digest(&stable);
    let stable_repaired = installs.join("StableRepaired");
    fixture.reviewed_apply(
        "repair",
        &stable_id,
        &stable_repaired,
        None,
        "stable_repair",
    );
    assert_eq!(tree_digest(&stable), stable_predecessor);
    validate(&config, &artifacts, &stable_id, "stable_repaired_validate");
    let stable_after_repair = show(&config, &artifacts, &stable_id, "stable_bound_repair");
    assert_eq!(
        fs::read(&stable_marker).unwrap(),
        b"operator-owned stable marker\n"
    );
    let stable_repaired_receipt = verify_receipt(&stable_repaired, "stable");

    let experimental_marker = experimental.join("operator-marker.txt");
    fs::write(
        &experimental_marker,
        b"operator-owned experimental marker\n",
    )
    .unwrap();
    let experimental_predecessor = tree_digest(&experimental);
    let experimental_updated = installs.join("ExperimentalUpdated");
    fixture.reviewed_apply(
        "update",
        &experimental_id,
        &experimental_updated,
        None,
        "experimental_update",
    );
    assert_eq!(tree_digest(&experimental), experimental_predecessor);
    validate(
        &config,
        &artifacts,
        &experimental_id,
        "experimental_updated_validate",
    );
    assert_eq!(
        fs::read(&experimental_marker).unwrap(),
        b"operator-owned experimental marker\n"
    );
    let experimental_updated_receipt = verify_receipt(&experimental_updated, "experimental");

    for (id, operation, wrong_channel, label) in [
        (&stable_id, "update", "experimental", "stable_cross_update"),
        (&stable_id, "repair", "experimental", "stable_cross_repair"),
        (
            &experimental_id,
            "update",
            "stable",
            "experimental_cross_update",
        ),
        (
            &experimental_id,
            "repair",
            "stable",
            "experimental_cross_repair",
        ),
    ] {
        let destination = installs.join(format!("{label}_refused"));
        let args = installation_args(
            operation,
            "plan",
            id,
            &destination,
            Some(wrong_channel),
            account.as_deref(),
        );
        let (code, value) = step(&config, &artifacts, label, &args);
        assert_ne!(code, 0, "{value}");
        assert!(
            value["errors"]
                .to_string()
                .to_lowercase()
                .contains("channel"),
            "{value}"
        );
        assert!(!destination.exists());
    }

    assert_eq!(tree_digest(&stable), stable_predecessor);
    assert_eq!(tree_digest(&experimental), experimental_predecessor);
    assert_ne!(stable_before.0, stable_predecessor.0);
    assert_ne!(experimental_before.0, experimental_predecessor.0);
    assert_eq!(stable_receipt["channel"], Value::Null);
    assert_eq!(experimental_receipt["channel"], "experimental");
    assert_eq!(stable_repaired_receipt["app_id"], expected_app("stable"));
    assert_eq!(
        experimental_updated_receipt["app_id"],
        expected_app("experimental")
    );
    let stable_after = show(&config, &artifacts, &stable_id, "stable_after_repair");
    let experimental_after = show(
        &config,
        &artifacts,
        &experimental_id,
        "experimental_after_update",
    );
    assert_ne!(
        stable_after["data"]["instance"]["installation_id"],
        Value::Null
    );
    assert_ne!(
        experimental_after["data"]["instance"]["installation_id"],
        Value::Null
    );
    assert_ne!(
        stable_after["data"]["instance"]["installation_id"],
        experimental_after["data"]["instance"]["installation_id"]
    );
    assert_ne!(
        stable_after["data"]["instance"]["installation_id"],
        stable_install_binding["data"]["instance"]["installation_id"]
    );
    assert_ne!(
        experimental_after["data"]["instance"]["installation_id"],
        experimental_install_binding["data"]["instance"]["installation_id"]
    );
    assert_eq!(
        stable_after["data"]["instance"]["installation_id"],
        stable_after_repair["data"]["instance"]["installation_id"]
    );
    let after_instances = catalog(&config, &artifacts, "instances_after");
    for previous in before_instances.as_array().unwrap() {
        let retained = after_instances
            .as_array()
            .unwrap()
            .iter()
            .find(|current| current["id"] == previous["id"])
            .expect("pre-existing instance disappeared");
        assert_eq!(retained, previous, "pre-existing binding changed");
    }
    eprintln!(
        "D20 result stable_files={} experimental_files={} evidence={}",
        stable_predecessor.1,
        experimental_predecessor.1,
        run.display()
    );
}
