use super::*;
use std::io::Read;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) fn invoke_saved(
    root: &Path,
    args: &[&str],
    artifacts: &Path,
    step: &str,
) -> (i32, Value) {
    let started = Instant::now();
    let result = invoke(root, args);
    fs::write(
        artifacts.join(format!("{step}.json")),
        serde_json::to_vec_pretty(&result.1).unwrap(),
    )
    .unwrap();
    eprintln!(
        "D16 step={step} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    result
}

pub(super) fn required_directory(name: &str) -> PathBuf {
    let path =
        std::env::var_os(name).unwrap_or_else(|| panic!("{name} must name an existing directory"));
    let path = PathBuf::from(path);
    let metadata = fs::symlink_metadata(&path)
        .unwrap_or_else(|error| panic!("{name} is unavailable: {error}"));
    assert!(
        metadata.is_dir() && ordinary(&metadata),
        "{name} must be an ordinary directory"
    );
    path.canonicalize().unwrap()
}

fn ordinary(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

pub(super) fn fresh_child(parent: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    for attempt in 0..100u32 {
        let path = parent.join(format!(
            "native-d16-{}-{nonce:032x}-{attempt:02x}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("Cannot create the fresh acceptance directory: {error}"),
        }
    }
    panic!("Cannot allocate a fresh acceptance directory")
}

pub(super) fn copy_ordinary_tree(source: &Path, destination: &Path) -> (u64, u64) {
    fn copy_directory(source: &Path, destination: &Path, files: &mut u64, bytes: &mut u64) {
        let metadata = fs::symlink_metadata(source).unwrap();
        assert!(
            metadata.is_dir() && ordinary(&metadata),
            "source contains a linked directory"
        );
        fs::create_dir(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path).unwrap();
            assert!(
                ordinary(&metadata),
                "source contains a link or reparse point"
            );
            if metadata.is_dir() {
                copy_directory(&source_path, &destination_path, files, bytes);
            } else {
                assert!(
                    metadata.is_file(),
                    "source contains a special filesystem entry"
                );
                fs::copy(&source_path, &destination_path).unwrap();
                fs::set_permissions(&destination_path, metadata.permissions()).unwrap();
                *files += 1;
                *bytes = (*bytes).saturating_add(metadata.len());
            }
        }
        fs::set_permissions(destination, metadata.permissions()).unwrap();
    }

    let mut files = 0;
    let mut bytes = 0;
    copy_directory(source, destination, &mut files, &mut bytes);
    (files, bytes)
}

pub(super) fn tree_digest(root: &Path) -> (String, u64, u64) {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(
                ordinary(&metadata),
                "inventory contains a link or reparse point"
            );
            if metadata.is_dir() {
                entries.push((path.clone(), true, 0));
                stack.push(path);
            } else {
                assert!(
                    metadata.is_file(),
                    "inventory contains a special filesystem entry"
                );
                entries.push((path, false, metadata.len()));
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = blake3::Hasher::new();
    let mut files = 0;
    let mut bytes = 0;
    for (path, directory, size) in entries {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        hasher.update(relative.as_bytes());
        hasher.update(&[0, u8::from(directory)]);
        hasher.update(&size.to_le_bytes());
        if directory {
            continue;
        }
        files += 1;
        bytes += size;
        let mut file = fs::File::open(path).unwrap();
        let mut buffer = [0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    (hasher.finalize().to_hex().to_string(), files, bytes)
}

fn small_non_executable_file(root: &Path) -> PathBuf {
    let mut stack = vec![root.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(ordinary(&metadata));
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if !metadata.is_file()
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
            {
                let executable_extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| {
                        ["exe", "com", "bat", "cmd", "ps1"]
                            .iter()
                            .any(|extension| value.eq_ignore_ascii_case(extension))
                    });
                if executable_extension || metadata.permissions().readonly() {
                    continue;
                }
            }
            candidates.push(path);
        }
    }
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .expect("source has no small non-executable file")
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

#[test]
#[ignore = "downloads complete native D16 content twice from the prepared Steam session"]
fn native_d16_managed_maintenance_preserves_shared_and_previous_installations() {
    #[cfg(target_os = "linux")]
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "run native acceptance as an ordinary user"
    );

    let config = required_directory("FOXY_NATIVE_MAINTENANCE_CONFIG");
    let source = required_directory("FOXY_NATIVE_MAINTENANCE_SOURCE");
    let scratch = required_directory("FOXY_NATIVE_MAINTENANCE_SCRATCH");
    let config_path = config.join("games.json");
    let games: Value = serde_json::from_slice(&fs::read(config_path).unwrap()).unwrap();
    assert_eq!(games["active_game_space_id"], "alpha");
    assert!(
        games["game_spaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|space| { space["id"] == "alpha" && space["game_id"] == "arma3" })
    );
    assert!(!scratch.starts_with(&config) && !config.starts_with(&scratch));
    assert!(!scratch.starts_with(&source) && !source.starts_with(&scratch));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&scratch).unwrap();
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    let run = fresh_child(&scratch);
    let artifacts = run.join("artifacts");
    fs::create_dir(&artifacts).unwrap();
    let original = run.join("ServerOriginal");
    let copy_started = Instant::now();
    let (copied_files, copied_bytes) = copy_ordinary_tree(&source, &original);
    eprintln!(
        "D16 step=copy_source files={copied_files} bytes={copied_bytes} elapsed_ms={}",
        copy_started.elapsed().as_millis()
    );
    let source_digest = tree_digest(&source).0;
    assert_eq!(tree_digest(&original).0, source_digest);

    let (code, primary_add) = invoke_saved(
        &config,
        &["server", "instance", "add", "--name", "D16Primary"],
        &artifacts,
        "instance_primary_add",
    );
    assert_eq!(code, 0, "{}", errors(&primary_add));
    let primary = primary_add["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (code, peer_add) = invoke_saved(
        &config,
        &["server", "instance", "add", "--name", "D16Peer"],
        &artifacts,
        "instance_peer_add",
    );
    assert_eq!(code, 0, "{}", errors(&peer_add));
    let peer = peer_add["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let original_path = original.to_str().unwrap();
    for (id, step) in [(&primary, "adopt_primary"), (&peer, "adopt_peer")] {
        let (code, value) = invoke_saved(
            &config,
            &[
                "server",
                "instance",
                "adopt",
                id,
                "--directory",
                original_path,
                "--expected-revision",
                "1",
                "--yes",
            ],
            &artifacts,
            step,
        );
        assert_eq!(code, 0, "{step}: {}", errors(&value));
    }
    let (code, primary_before) = invoke_saved(
        &config,
        &["server", "instance", "show", &primary],
        &artifacts,
        "primary_before",
    );
    assert_eq!(code, 0);
    let original_installation = primary_before["data"]["instance"]["installation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (code, peer_before) = invoke_saved(
        &config,
        &["server", "instance", "show", &peer],
        &artifacts,
        "peer_before",
    );
    assert_eq!(code, 0);
    assert_eq!(
        peer_before["data"]["instance"]["installation_id"],
        original_installation
    );

    let (code, initial_validation) = invoke_saved(
        &config,
        &["server", "installation", "validate", &primary],
        &artifacts,
        "original_validate",
    );
    assert_eq!(code, 0, "{}", errors(&initial_validation));
    assert_eq!(initial_validation["data"]["verification"]["valid"], true);

    for (id, step) in [(&primary, "check_primary"), (&peer, "check_peer")] {
        let (code, value) = invoke_saved(
            &config,
            &[
                "server",
                "installation",
                "check",
                id,
                "--account",
                "default",
            ],
            &artifacts,
            step,
        );
        assert_eq!(code, 0, "{step}: {}", errors(&value));
        let installed: Value =
            serde_json::from_slice(&fs::read(source.join("foxy-installation.json")).unwrap())
                .unwrap();
        let target = &value["data"]["check"]["review"]["selection"];
        let depots = |entries: &Value| {
            entries
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    (
                        entry["depot_id"].as_u64().unwrap(),
                        entry["manifest_gid"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| entry["manifest_gid"].to_string()),
                    )
                })
                .collect::<Vec<_>>()
        };
        let expected_change = installed["build_id"] != target["build_id"]
            || installed["branch"] != target["branch"]
            || depots(&installed["depots"]) != depots(&target["depots"]);
        assert_eq!(
            value["data"]["check"]["update_available"], expected_change,
            "{value}"
        );
    }

    let removed_file = small_non_executable_file(&original);
    #[cfg(unix)]
    {
        make_parent_writable(&original.join("operator-notes.txt"), &original);
        make_parent_writable(&removed_file, &original);
    }
    fs::write(
        original.join("operator-notes.txt"),
        b"operator-owned marker\n",
    )
    .unwrap();
    let executable_name = if cfg!(windows) {
        "arma3server_x64.exe"
    } else {
        "arma3server_x64"
    };
    assert!(original.join("foxy-installation.json").is_file());
    assert!(original.join(executable_name).is_file());
    let removed_bytes = fs::metadata(&removed_file).unwrap().len();
    fs::remove_file(&removed_file).unwrap();
    let damaged_digest = tree_digest(&original);
    eprintln!(
        "D16 step=damage files={} bytes={} removed_bytes={removed_bytes}",
        damaged_digest.1, damaged_digest.2
    );
    let (code, damaged_validation) = invoke_saved(
        &config,
        &["server", "installation", "validate", &primary],
        &artifacts,
        "damaged_validate",
    );
    assert_eq!(code, 4, "{}", errors(&damaged_validation));
    assert_eq!(damaged_validation["data"]["verification"]["valid"], false);

    let repaired = run.join("ServerRepaired");
    let repaired_path = repaired.to_str().unwrap();
    let (code, repair_plan) = invoke_saved(
        &config,
        &[
            "server",
            "installation",
            "repair",
            "plan",
            &primary,
            "--directory",
            repaired_path,
            "--account",
            "default",
        ],
        &artifacts,
        "repair_plan",
    );
    assert_eq!(code, 0, "{}", errors(&repair_plan));
    assert!(
        repair_plan["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let repair_fingerprint = repair_plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    let (code, dry_repair) = invoke_saved(
        &config,
        &[
            "--dry-run",
            "server",
            "installation",
            "repair",
            "apply",
            &primary,
            "--directory",
            repaired_path,
            "--account",
            "default",
            "--expected-plan-fingerprint",
            repair_fingerprint,
        ],
        &artifacts,
        "repair_dry_apply",
    );
    assert_eq!(code, 0, "{}", errors(&dry_repair));
    assert_eq!(dry_repair["data"]["writes_performed"], false);
    assert_eq!(dry_repair["data"]["dry_run"], true);
    let (code, stale_repair) = invoke_saved(
        &config,
        &[
            "server",
            "installation",
            "repair",
            "apply",
            &primary,
            "--directory",
            repaired_path,
            "--account",
            "default",
            "--expected-plan-fingerprint",
            &"0".repeat(64),
            "--yes",
        ],
        &artifacts,
        "repair_stale_apply",
    );
    assert_eq!(code, 2, "{stale_repair}");
    assert_eq!(stale_repair["ok"], false);
    assert!(stale_repair["data"].is_null());
    assert_eq!(tree_digest(&original).0, damaged_digest.0);
    assert!(!repaired.exists());

    let (code, repair_apply) = invoke_saved(
        &config,
        &[
            "server",
            "installation",
            "repair",
            "apply",
            &primary,
            "--directory",
            repaired_path,
            "--account",
            "default",
            "--expected-plan-fingerprint",
            repair_fingerprint,
            "--yes",
        ],
        &artifacts,
        "repair_apply",
    );
    assert_eq!(code, 0, "{}", errors(&repair_apply));
    assert_eq!(repair_apply["data"]["outcome"]["bound"], true);
    let (code, repaired_validation) = invoke_saved(
        &config,
        &["server", "installation", "validate", &primary],
        &artifacts,
        "repaired_validate",
    );
    assert_eq!(code, 0, "{}", errors(&repaired_validation));
    assert_eq!(repaired_validation["data"]["verification"]["valid"], true);
    let repaired_digest = tree_digest(&repaired);
    let (code, primary_after_repair) = invoke_saved(
        &config,
        &["server", "instance", "show", &primary],
        &artifacts,
        "primary_after_repair",
    );
    assert_eq!(code, 0);
    let repaired_installation = primary_after_repair["data"]["instance"]["installation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(repaired_installation, original_installation);
    let (code, peer_after_repair) = invoke_saved(
        &config,
        &["server", "instance", "show", &peer],
        &artifacts,
        "peer_after_repair",
    );
    assert_eq!(code, 0);
    assert_eq!(
        peer_after_repair["data"]["instance"]["installation_id"],
        original_installation
    );
    assert_eq!(tree_digest(&original).0, damaged_digest.0);

    let updated = run.join("ServerUpdated");
    let updated_path = updated.to_str().unwrap();
    let (code, update_plan) = invoke_saved(
        &config,
        &[
            "server",
            "installation",
            "update",
            "plan",
            &peer,
            "--directory",
            updated_path,
            "--account",
            "default",
        ],
        &artifacts,
        "update_plan",
    );
    assert_eq!(code, 0, "{}", errors(&update_plan));
    assert!(
        update_plan["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let update_fingerprint = update_plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    let (code, update_apply) = invoke_saved(
        &config,
        &[
            "server",
            "installation",
            "update",
            "apply",
            &peer,
            "--directory",
            updated_path,
            "--account",
            "default",
            "--expected-plan-fingerprint",
            update_fingerprint,
            "--yes",
        ],
        &artifacts,
        "update_apply",
    );
    assert_eq!(code, 0, "{}", errors(&update_apply));
    assert_eq!(update_apply["data"]["outcome"]["bound"], true);
    let (code, updated_validation) = invoke_saved(
        &config,
        &["server", "installation", "validate", &peer],
        &artifacts,
        "updated_validate",
    );
    assert_eq!(code, 0, "{}", errors(&updated_validation));
    assert_eq!(updated_validation["data"]["verification"]["valid"], true);
    assert_eq!(tree_digest(&repaired).0, repaired_digest.0);
    assert_eq!(tree_digest(&original).0, damaged_digest.0);
    assert_eq!(tree_digest(&source).0, source_digest);
    let updated_digest = tree_digest(&updated);

    let (code, primary_after_update) = invoke_saved(
        &config,
        &["server", "instance", "show", &primary],
        &artifacts,
        "primary_after_update",
    );
    let (code_peer, peer_after_update) = invoke_saved(
        &config,
        &["server", "instance", "show", &peer],
        &artifacts,
        "peer_after_update",
    );
    assert_eq!(code, 0);
    assert_eq!(code_peer, 0);
    assert_eq!(
        primary_after_update["data"]["instance"]["installation_id"],
        repaired_installation
    );
    let updated_installation = peer_after_update["data"]["instance"]["installation_id"]
        .as_str()
        .unwrap();
    assert_ne!(updated_installation, original_installation);

    let (code, installations) = invoke_saved(
        &config,
        &["server", "installation", "list"],
        &artifacts,
        "installation_list_final",
    );
    assert_eq!(code, 0);
    let directory_for = |id: &str| {
        PathBuf::from(
            installations["data"]["installations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|installation| installation["id"] == id)
                .unwrap()["directory"]
                .as_str()
                .unwrap(),
        )
        .canonicalize()
        .unwrap()
    };
    assert_eq!(
        directory_for(&repaired_installation),
        repaired.canonicalize().unwrap()
    );
    assert_eq!(
        directory_for(updated_installation),
        updated.canonicalize().unwrap()
    );
    assert_eq!(
        directory_for(&original_installation),
        original.canonicalize().unwrap()
    );
    eprintln!(
        "D16 result files_original={} bytes_original={} files_repaired={} bytes_repaired={} files_updated={} bytes_updated={}",
        damaged_digest.1,
        damaged_digest.2,
        repaired_digest.1,
        repaired_digest.2,
        updated_digest.1,
        updated_digest.2
    );
}
