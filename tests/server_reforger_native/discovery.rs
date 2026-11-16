use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::CommandSpec;
#[cfg(windows)]
use foxy_supervisor::process::ConsoleSignalHelper;
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 100_000;
const MAX_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(90);
const SOFT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const FORCED_STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, PartialEq, Eq)]
struct Entry {
    path: String,
    size: u64,
    blake3: String,
}

struct RetainedSession(ForegroundSession);

impl Drop for RetainedSession {
    fn drop(&mut self) {
        if self.0.event().exit.is_none() {
            let _ = self.0.force_stop(Duration::from_secs(10));
        }
    }
}

pub(super) fn run() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other(
            "Native process containment is unavailable",
        ));
    }
    let source = input_directory("FOXY_REFORGER_NATIVE_SOURCE")?;
    let scratch = input_directory("FOXY_REFORGER_NATIVE_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Source and scratch must be separate directories",
        ));
    }
    #[cfg(target_os = "linux")]
    check_linux_prerequisites(&scratch)?;

    let evidence = tempfile::Builder::new()
        .prefix("reforger-discovery-")
        .tempdir_in(&scratch)?
        .keep();
    private_directory(&evidence)?;
    let payload = evidence.join("payload");
    fs::create_dir(&payload)?;
    let source_before = inventory(&source)?;
    copy_tree(&source, &payload)?;
    let payload_before = inventory(&payload)?;
    if source_before != payload_before {
        return Err(io::Error::other(
            "Copied payload inventory differs from source",
        ));
    }

    let executable = payload.join(executable_name());
    if !executable.is_file() {
        return Err(io::Error::other(
            "Dedicated executable is missing from source root",
        ));
    }
    let project = payload
        .join("addons")
        .join("data")
        .join("ArmaReforger.gproj");
    if !project.is_file() {
        return Err(io::Error::other(
            "Dedicated source is missing addons/data/ArmaReforger.gproj",
        ));
    }
    let profile = evidence.join("profile");
    let downloads = evidence.join("downloads");
    let temporary = evidence.join("temporary");
    let logs = evidence.join("logs");
    let home = evidence.join("home");
    for directory in [&profile, &downloads, &temporary, &logs, &home] {
        fs::create_dir(directory)?;
    }

    let stdout_path = evidence.join("engine.stdout.log");
    let stdout = File::create(&stdout_path)?;
    let locks = InstallationLocks::capture_in(&payload, &evidence.join("installation-locks"))?;
    let admission = locks.try_admit()?;
    let identity = SessionIdentity {
        owner_id: "native_reforger_discovery".into(),
        owner_generation: 1,
        game_space_id: "reforger".into(),
        instance_id: "discovery".into(),
        operation_id: "list_scenarios".into(),
        run_generation: 1,
    };
    let spec = CommandSpec {
        executable: PathBuf::from(argument_path(&executable)?),
        directory: PathBuf::from(argument_path(&payload)?),
        arguments: vec![
            "-listScenarios".into(),
            "-nothrow".into(),
            "-maxFPS".into(),
            "30".into(),
            "-gproj".into(),
            argument_path(&project)?.into(),
            "-profile".into(),
            argument_path(&profile)?.into(),
            "-addonDownloadDir".into(),
            argument_path(&downloads)?.into(),
            "-addonTempDir".into(),
            argument_path_with_trailing_separator(&temporary)?.into(),
            "-logsDir".into(),
            argument_path(&logs)?.into(),
        ],
        environment: private_environment(&home),
    };
    let mut session = RetainedSession(ForegroundSession::spawn_controlled(
        identity,
        &spec,
        &stdout,
        admission,
        &AtomicBool::new(false),
        || Ok(()),
    )?);
    #[cfg(windows)]
    let signal_helper = ConsoleSignalHelper {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_Foxy")),
        arguments: vec!["server".into(), "console-signal".into()],
    };
    #[cfg(windows)]
    let signal_helper = Some(&signal_helper);

    let effective_profile = profile.join("profile");
    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
    let (natural_exit, listing_output) = loop {
        if let Some(exit) = session.0.observe()? {
            break (Some(exit), read_all_engine_output(&stdout_path, &logs)?);
        }
        let output = read_all_engine_output(&stdout_path, &logs)?;
        if discovery_evidence_observed(&output, &effective_profile) || Instant::now() >= deadline {
            break (None, output);
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let listing_observed_before_stop =
        discovery_evidence_observed(&listing_output, &effective_profile);
    #[cfg(windows)]
    let helper = signal_helper;
    #[cfg(target_os = "linux")]
    let helper = None;
    let (process_exit, cleanup_outcome) = if let Some(exit) = natural_exit {
        (Some(exit), "natural_exit")
    } else {
        let soft_stop = session.0.deliver_soft_stop(helper);
        if let Ok(Some(exit)) = soft_stop.and_then(|()| session.0.wait(SOFT_STOP_TIMEOUT)) {
            (Some(exit), "soft_stop_waited")
        } else if let Some(exit) = session.0.force_stop(FORCED_STOP_TIMEOUT)? {
            let outcome = if exit.forced {
                "forced_waited"
            } else {
                "waited_after_force_request"
            };
            (Some(exit), outcome)
        } else {
            (None, "cleanup_uncertain")
        }
    };
    let stopped = process_exit.is_some();

    let stdout_text = read_bounded(&stdout_path, MAX_OUTPUT_BYTES)?;
    let logs_inventory = inventory(&logs)?;
    let profile_inventory = inventory(&profile)?;
    let downloads_inventory = inventory(&downloads)?;
    let temporary_inventory = inventory(&temporary)?;
    let log_text = collect_engine_logs(&logs)?;
    let combined_output = format!("{stdout_text}\n{log_text}");
    let official_scenario_lines = official_scenario_lines(&combined_output);
    let product_parser_agreed = match official_scenario_lines.as_ref() {
        Some(lines) => product_parser_agrees(&combined_output, &evidence, lines.len())?,
        None => false,
    };
    let output_has_scenario = listing_observed_before_stop && official_scenario_lines.is_some();
    let version_line = engine_version_line(&combined_output);
    let resolved_profile_line = resolved_profile_line(&combined_output, &effective_profile);
    let output_has_runtime_paths = resolved_profile_line.is_some()
        && effective_profile.is_dir()
        && !profile_inventory.is_empty()
        && logs_inventory
            .iter()
            .any(|entry| entry.path.eq_ignore_ascii_case("console.log") && entry.size > 0);
    let source_after = inventory(&source)?;
    let payload_after = inventory(&payload)?;
    let source_unchanged = source_before == source_after;
    let payload_unchanged = payload_before == payload_after;
    let report = serde_json::json!({
        "schema_version": 1,
        "probe": "reforger_scenario_discovery",
        "result": if output_has_scenario && product_parser_agreed && version_line.is_some() && output_has_runtime_paths && source_unchanged && payload_unchanged && stopped { "observed" } else { "incomplete" },
        "source_unchanged": source_unchanged,
        "copied_payload_unchanged": payload_unchanged,
        "process_exit": process_exit,
        "waited_tree_cleanup": stopped,
        "cleanup_outcome": cleanup_outcome,
        "listing_observed_before_stop": listing_observed_before_stop,
        "save_state": if cleanup_outcome == "forced_waited" { "uncertain_after_forced_cleanup" } else { "not_claimed_by_discovery_probe" },
        "scenario_resource_observed": output_has_scenario,
        "product_parser_agreed": product_parser_agreed,
        "official_scenario_lines": official_scenario_lines,
        "engine_version_log_line": version_line,
        "resolved_profile_log_line": resolved_profile_line,
        "runtime_paths_created": output_has_runtime_paths,
        "source_inventory_before": source_before,
        "source_inventory_after": source_after,
        "copied_inventory_before": payload_before,
        "copied_inventory_after": payload_after,
        "stdout_log": stdout_path,
        "logs_directory": logs,
        "profile_directory": profile,
        "downloads_directory": downloads,
        "temporary_directory": temporary,
        "logs_inventory": logs_inventory,
        "profile_inventory": profile_inventory,
        "downloads_inventory": downloads_inventory,
        "temporary_inventory": temporary_inventory
    });
    let report_path = evidence.join("report.json");
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("Private native evidence: {}", report_path.display());
    if !stopped {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Contained Reforger process tree cleanup is uncertain",
        ));
    }
    if !source_unchanged || !payload_unchanged {
        return Err(io::Error::other(
            "Source or copied payload changed during probe",
        ));
    }
    if !output_has_scenario
        || !product_parser_agreed
        || version_line.is_none()
        || !output_has_runtime_paths
        || !source_unchanged
        || !payload_unchanged
    {
        return Err(io::Error::other(
            "Official scenario listing, product parser, engine version, resolved profile path, or unchanged inventories were not observed",
        ));
    }
    Ok(())
}

fn input_directory(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} must be set explicitly")))?;
    if !path.is_absolute()
        || !path.is_dir()
        || fs::symlink_metadata(&path)?.file_type().is_symlink()
    {
        return Err(io::Error::other(format!(
            "{name} must be an absolute ordinary directory"
        )));
    }
    path.canonicalize()
}

fn executable_name() -> &'static str {
    if cfg!(windows) {
        "ArmaReforgerServer.exe"
    } else {
        "ArmaReforgerServer"
    }
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf(), false)];
    while let Some((from, to, finish_directory)) = stack.pop() {
        if finish_directory {
            fs::set_permissions(&to, fs::metadata(&from)?.permissions())?;
            continue;
        }
        for entry in fs::read_dir(&from)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native source contains a symbolic link"));
            }
            let target = to.join(entry.file_name());
            if metadata.is_dir() {
                fs::create_dir(&target)?;
                stack.push((entry.path(), target, true));
                stack.push((entry.path(), to.join(entry.file_name()), false));
            } else if metadata.is_file() {
                fs::copy(entry.path(), &target)?;
                fs::set_permissions(&target, metadata.permissions())?;
            } else {
                return Err(io::Error::other(
                    "Native source contains a non-ordinary entry",
                ));
            }
        }
    }
    Ok(())
}

fn inventory(root: &Path) -> io::Result<Vec<Entry>> {
    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Inventory encountered a symbolic link"));
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                if result.len() >= MAX_ENTRIES {
                    return Err(io::Error::other(
                        "Native payload inventory exceeded entry limit",
                    ));
                }
                let path = entry.path();
                let relative = path.strip_prefix(root).map_err(io::Error::other)?;
                let mut file = File::open(entry.path())?;
                let mut hasher = blake3::Hasher::new();
                let mut size = 0_u64;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    size = size.saturating_add(read as u64);
                    hasher.update(&buffer[..read]);
                }
                result.push(Entry {
                    path: relative.to_string_lossy().into_owned(),
                    size,
                    blake3: hasher.finalize().to_hex().to_string(),
                });
            } else {
                return Err(io::Error::other(
                    "Inventory encountered a non-ordinary entry",
                ));
            }
        }
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

fn product_parser_agrees(output: &str, evidence: &Path, count: usize) -> io::Result<bool> {
    let log = evidence.join("product-listing.log");
    fs::write(&log, output)?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--json")
        .arg("--config-dir")
        .arg(evidence)
        .args(["server", "configuration", "scenario", "parse-log", "--file"])
        .arg(&log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let produced = command.output()?;
    if !produced.status.success() {
        return Ok(false);
    }
    let value: serde_json::Value = match serde_json::from_slice(&produced.stdout) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    Ok(value["ok"] == true && value["data"]["count"].as_u64() == Some(count as u64))
}

fn official_scenario_lines(output: &str) -> Option<Vec<&str>> {
    let lines: Vec<_> = output.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let Some(count) = script_message(line).and_then(official_scenario_count) else {
            continue;
        };
        let mut start = index + 1;
        if lines.get(start).and_then(|line| script_message(line))
            == Some("--------------------------------------------------")
        {
            start += 1;
        }
        let listed: Vec<_> = lines[start..]
            .iter()
            .take_while(|line| script_message(line).is_some_and(is_scenario_resource))
            .copied()
            .collect();
        let unique: HashSet<_> = listed
            .iter()
            .filter_map(|line| script_message(line).and_then(scenario_resource_path))
            .collect();
        if count > 0 && listed.len() == count && unique.len() == count {
            return Some(listed);
        }
    }
    None
}

fn discovery_evidence_observed(output: &str, effective_profile: &Path) -> bool {
    official_scenario_lines(output).is_some()
        && engine_version_line(output).is_some()
        && resolved_profile_line(output, effective_profile).is_some()
}

fn script_message(line: &str) -> Option<&str> {
    let script = line.find("SCRIPT")?;
    line[script + "SCRIPT".len()..]
        .split_once(':')
        .map(|(_, message)| message.trim())
}

fn official_scenario_count(message: &str) -> Option<usize> {
    message
        .strip_prefix("Official scenarios (")?
        .strip_suffix(" entries)")?
        .parse()
        .ok()
}

fn is_scenario_resource(message: &str) -> bool {
    scenario_resource_path(message).is_some()
}

fn scenario_resource_path(message: &str) -> Option<&str> {
    let after_guid = message.strip_prefix('{')?;
    let (guid, resource) = after_guid.split_once('}')?;
    if guid.len() != 16 || !guid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let resource = resource.strip_prefix("Missions/")?;
    let resource = resource.split_whitespace().next().unwrap_or_default();
    (!resource.is_empty()
        && resource.ends_with(".conf")
        && !resource.starts_with('/')
        && !resource.contains("..")
        && !resource.contains(['\\', ':', '{', '}']))
    .then_some(resource)
}

fn engine_version_line(output: &str) -> Option<&str> {
    output.lines().find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("version ") && lower.contains(" built ")
    })
}

fn resolved_profile_line<'a>(output: &'a str, profile: &Path) -> Option<&'a str> {
    let expected = normalized_path(profile);
    output.lines().find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("adding relative directory")
            && lower.contains("under name profile")
            && normalized_path_text(line).contains(&expected)
    })
}

fn normalized_path(path: &Path) -> String {
    normalized_path_text(&path.to_string_lossy())
}

fn normalized_path_text(path: &str) -> String {
    let value = path.replace('\\', "/");
    let value = value
        .strip_prefix("//?/UNC/")
        .map(|tail| format!("//{tail}"))
        .or_else(|| value.strip_prefix("//?/").map(str::to_owned))
        .unwrap_or(value);
    value.replace('\\', "/").to_ascii_lowercase()
}

fn argument_path(path: &Path) -> io::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::other("Native argument path is not valid Unicode"))?
        .replace('\\', "/");
    let value = value
        .strip_prefix("//?/UNC/")
        .map(|tail| format!("//{tail}"))
        .or_else(|| value.strip_prefix("//?/").map(str::to_owned))
        .unwrap_or(value);
    #[cfg(windows)]
    let value = value.replace('/', "\\");
    Ok(value)
}

fn argument_path_with_trailing_separator(path: &Path) -> io::Result<String> {
    let mut value = argument_path(path)?;
    if !value.ends_with(['/', '\\']) {
        value.push(std::path::MAIN_SEPARATOR);
    }
    Ok(value)
}

fn read_all_engine_output(stdout: &Path, logs: &Path) -> io::Result<String> {
    let stdout = read_bounded(stdout, MAX_OUTPUT_BYTES)?;
    let logs = collect_engine_logs(logs)?;
    if stdout.len().saturating_add(logs.len()) as u64 > MAX_OUTPUT_BYTES {
        return Err(io::Error::other(
            "Combined native engine output exceeded size limit",
        ));
    }
    Ok(format!("{stdout}\n{logs}"))
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;

fn collect_engine_logs(root: &Path) -> io::Result<String> {
    let mut output = String::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other(
                    "Engine log directory contains a symbolic link",
                ));
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file()
                && entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
                })
            {
                let contents = read_bounded(&entry.path(), MAX_OUTPUT_BYTES)?;
                if output.len().saturating_add(contents.len()) as u64 > MAX_OUTPUT_BYTES {
                    return Err(io::Error::other("Combined native logs exceeded size limit"));
                }
                output.push_str(&contents);
                output.push('\n');
            }
        }
    }
    Ok(output)
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("Native engine output exceeded size limit"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn private_environment(home: &Path) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let names: &[&str] = if cfg!(windows) {
        &["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"]
    } else {
        &[
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ]
    };
    names
        .iter()
        .map(|name| ((*name).into(), home.as_os_str().to_owned()))
        .collect()
}

fn private_directory(_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(_path, fs::Permissions::from_mode(0o700))?;
        if fs::metadata(_path)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other("Native evidence directory is not private"));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn check_linux_prerequisites(scratch: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::metadata(scratch)?;
    if metadata.permissions().mode() & 0o111 == 0 || metadata.dev() == 0 {
        return Err(io::Error::other(
            "Linux scratch must be executable ordinary local storage",
        ));
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::other(
            "Native Linux acceptance requires an ordinary user",
        ));
    }
    Ok(())
}
