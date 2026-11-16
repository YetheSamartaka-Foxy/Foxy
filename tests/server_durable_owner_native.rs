#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use serde_json::Value;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND_DEADLINE: Duration = Duration::from_secs(360);
const MAX_STDOUT: usize = 2 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;

#[test]
#[ignore = "Requires an explicitly prepared private native server configuration and runtime"]
fn native_detached_owner_survives_controller_recreation() -> io::Result<()> {
    let config = required_directory("FOXY_DURABLE_CONFIG")?;
    let space = required_identifier("FOXY_DURABLE_SPACE")?;
    let instance = required_identifier("FOXY_DURABLE_INSTANCE")?;
    let before = invoke_raw(&config, &["server", "owner", "--game-space", &space])?;
    match before.0 {
        0 => {
            return Err(io::Error::other(
                "Refusing to attach to an owner that existed before this test",
            ));
        }
        4 => {}
        code => {
            return Err(io::Error::other(format!(
                "Could not establish that no owner existed before test; exit {code}"
            )));
        }
    }
    let installation_evidence = installation_evidence(&config, &space, &instance)?;

    let mut evidence = Vec::new();
    let started = invoke(
        &config,
        &[
            "server",
            "supervise",
            "--detach",
            "--yes",
            "--game-space",
            &space,
        ],
        0,
    )?;
    evidence.push(started.clone());
    check_owner(&started, &space)?;

    let owner_before = owner_data(&started)?.clone();
    let mut cleanup = Cleanup {
        config: config.clone(),
        space: space.clone(),
        instance: instance.clone(),
        owner: owner_before.clone(),
        run: None,
        active: true,
    };
    let initial_events = owner_events(&config, &space, 0)?;
    assert_event_batch(&initial_events, 0, &owner_before)?;
    let start_event_cursor = initial_events["data"]["events"]["head"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Initial owner replay omitted head cursor"))?;
    let plan = invoke(
        &config,
        &[
            "server",
            "instance",
            "start-plan",
            &instance,
            "--game-space",
            &space,
        ],
        0,
    )?;
    evidence.push(plan.clone());
    let generation = required_number_string(
        &plan["data"]["plan"]["activation_generation"],
        "activation generation",
    )?;
    let fingerprint = required_string(&plan["data"]["plan"]["fingerprint"], "plan fingerprint")?;
    let started_run = invoke(
        &config,
        &[
            "server",
            "instance",
            "start",
            &instance,
            "--detach",
            "--expected-activation-generation",
            &generation,
            "--expected-plan-fingerprint",
            fingerprint,
            "--yes",
            "--game-space",
            &space,
        ],
        0,
    )?;
    evidence.push(started_run.clone());
    let status = started_run
        .pointer("/data/outcome/status")
        .unwrap_or(&Value::Null);
    let run = required_number_string(&status["run_generation"], "run generation")?;
    cleanup.run = Some(run.clone());
    let ready = wait_ready(
        &config,
        &space,
        &instance,
        &owner_before,
        &run,
        &mut evidence,
    )?;
    let identity = monitor_identity(&ready)?;
    let lifecycle_events = owner_events(&config, &space, start_event_cursor)?;
    assert_event_batch(&lifecycle_events, start_event_cursor, &owner_before)?;
    let lifecycle_batch = &lifecycle_events["data"]["events"];
    if !lifecycle_batch["events"].as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| lifecycle_completion_for_run(event, &run))
    }) {
        return Err(io::Error::other(
            "Owner replay omitted the completed lifecycle result for the started run",
        ));
    }
    let event_list = lifecycle_batch["events"]
        .as_array()
        .ok_or_else(|| io::Error::other("Owner replay omitted events"))?;
    let cursor = event_list
        .last()
        .and_then(|event| event["sequence"].as_u64())
        .ok_or_else(|| io::Error::other("Owner replay had no sequenced events"))?;
    let next_events = owner_events(&config, &space, cursor)?;
    assert_event_batch(&next_events, cursor, &owner_before)?;
    if next_events["data"]["events"]["events"]
        .as_array()
        .is_none_or(|events| !events.is_empty())
        || next_events["data"]["events"]["head"] != lifecycle_batch["head"]
    {
        return Err(io::Error::other(
            "Owner replay cursor did not return an empty bounded batch at the current head",
        ));
    }
    evidence.push(serde_json::json!({
        "owner_events_after_zero": initial_events,
        "owner_events_after_start": lifecycle_events,
        "owner_events_after_cursor": next_events,
    }));

    for _ in 0..2 {
        let connected = invoke(
            &config,
            &[
                "server",
                "supervise",
                "--detach",
                "--yes",
                "--game-space",
                &space,
            ],
            0,
        )?;
        evidence.push(connected.clone());
        check_same_owner(&connected, &owner_before)?;
        thread::sleep(Duration::from_secs(6));
        let current = monitor(&config, &space, &instance)?;
        if monitor_identity(&current)? != identity || !genuine_running_status(&current) {
            return Err(io::Error::other(
                "Detached run identity or genuine process observation changed after controller recreation",
            ));
        }
        evidence.push(current);
        let competing = invoke_raw(
            &config,
            &[
                "server",
                "supervise",
                "--parent-bound",
                "--game-space",
                &space,
            ],
        )?;
        evidence.push(competing.1);
        if competing.0 == 0 {
            return Err(io::Error::other(
                "A second parent-bound owner unexpectedly claimed the active game space",
            ));
        }
        if competing.0 != 6 {
            return Err(io::Error::other(format!(
                "Second owner failed with {}, expected DATABASE_BUSY (6)",
                competing.0
            )));
        }
        let after = owner(&config, &space)?;
        check_same_owner(&after, &owner_before)?;
    }

    let shutdown = invoke_raw(
        &config,
        &[
            "server",
            "supervise",
            "--shutdown",
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(shutdown.1);
    if shutdown.0 == 0 {
        return Err(io::Error::other(
            "Owner shutdown unexpectedly succeeded while a native session was active",
        ));
    }
    check_same_owner(&owner(&config, &space)?, &owner_before)?;

    let stopped = invoke(
        &config,
        &[
            "server",
            "instance",
            "stop",
            &instance,
            "--expected-run-generation",
            &run,
            "--timeout-ms",
            "30000",
            "--yes",
            "--game-space",
            &space,
        ],
        0,
    )?;
    evidence.push(stopped.clone());
    if stopped.pointer("/data/outcome/completed") != Some(&Value::Bool(true))
        || stopped.pointer("/data/outcome/status/cleanup_verified") != Some(&Value::Bool(true))
        || stopped.pointer("/data/outcome/status/forced") != Some(&Value::Bool(false))
    {
        return Err(io::Error::other(format!(
            "Native run did not stop gracefully: {stopped}"
        )));
    }
    let released = invoke(
        &config,
        &[
            "server",
            "supervise",
            "--shutdown",
            "--yes",
            "--game-space",
            &space,
        ],
        0,
    )?;
    evidence.push(released.clone());
    let after_release = invoke_raw(&config, &["server", "owner", "--game-space", &space])?;
    if released["data"]["released"] != true || after_release.0 == 0 || after_release.0 != 4 {
        return Err(io::Error::other("Idle detached owner was not released"));
    }
    cleanup.active = false;
    verify_installation_evidence(&installation_evidence)?;
    evidence.push(serde_json::json!({
        "installation_executable_bytes": installation_evidence.bytes,
        "installation_executable_blake3": installation_evidence.blake3,
    }));
    retain_evidence(&config, &evidence)?;
    Ok(())
}

fn wait_ready(
    config: &Path,
    space: &str,
    instance: &str,
    owner: &Value,
    run: &str,
    evidence: &mut Vec<Value>,
) -> io::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let snapshot = monitor(config, space, instance)?;
        if snapshot["ok"] != true {
            return Err(io::Error::other(format!(
                "Monitor query failed: {snapshot}"
            )));
        }
        let data = &snapshot["data"]["monitoring"];
        if data["query_ready"] == true {
            if data["status"]["run_generation"]
                .as_u64()
                .is_none_or(|generation| generation.to_string() != run)
                || data["status"]["owner_id"] != owner["scope"]["owner_id"]
                || !genuine_running_status(&snapshot)
            {
                return Err(io::Error::other(
                    "Fresh query readiness did not match the started native run",
                ));
            }
            evidence.push(snapshot.clone());
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            evidence.push(snapshot);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Native server did not reach fresh query readiness within 180 seconds",
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn genuine_running_status(value: &Value) -> bool {
    let status = &value["data"]["monitoring"]["status"];
    let monitoring = &value["data"]["monitoring"];
    let observation = &monitoring["observation"];
    let resource = observation["resources"]
        .as_array()
        .and_then(|samples| samples.last());
    let Some(process_id) = status["process_id"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    let Some(sample) = resource.map(|resource| &resource["sample"]) else {
        return false;
    };
    let Some(game_pid) = sample["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    let fresh = monitoring["observation_age_ms"]
        .as_u64()
        .is_some_and(|age| age <= 10_000);
    let resource_evidence = sample["cpu_percent"].as_f64().is_some_and(f64::is_finite)
        && sample["resident_bytes"].as_u64().is_some();
    let log_entries = observation["logs"]["entries"].as_array();
    let actual_logs =
        log_entries.is_some_and(|entries| !entries.is_empty() && entries.len() <= 128);
    let process_relationship = {
        #[cfg(windows)]
        {
            game_pid == process_id
        }
        #[cfg(target_os = "linux")]
        {
            linux_live_child_of(game_pid, process_id).unwrap_or(false)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            false
        }
    };
    status["process_exists"] == true
        && fresh
        && resource_evidence
        && actual_logs
        && process_relationship
}

#[cfg(target_os = "linux")]
fn linux_live_child_of(child_pid: u32, expected_parent_pid: u32) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let proc_root = Path::new("/proc");
    let self_uid = std::fs::metadata(proc_root.join("self"))?.uid();
    let child_dir = proc_root.join(child_pid.to_string());
    let child_metadata = std::fs::metadata(&child_dir)?;
    if child_metadata.uid() != self_uid {
        return Ok(false);
    }
    let stat = std::fs::read(child_dir.join("stat"))?;
    if stat.len() > 4096 {
        return Err(io::Error::other("Linux process stat exceeded its bound"));
    }
    let stat = std::str::from_utf8(&stat)
        .map_err(|_| io::Error::other("Linux process stat was not UTF-8"))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| io::Error::other("Linux process stat omitted comm terminator"))?;
    let mut fields = stat[close + 1..].split_whitespace();
    let _state = fields
        .next()
        .ok_or_else(|| io::Error::other("Linux process stat omitted state"))?;
    let parent_pid = fields
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| io::Error::other("Linux process stat omitted parent pid"))?;
    Ok(parent_pid == expected_parent_pid)
}

fn monitor_identity(value: &Value) -> io::Result<(String, u64, u64, u32)> {
    let status = &value["data"]["monitoring"]["status"];
    Ok((
        required_string(&status["owner_id"], "monitor owner")?.to_owned(),
        status["owner_generation"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Monitor omitted owner generation"))?,
        status["run_generation"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Monitor omitted run generation"))?,
        status["process_id"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| io::Error::other("Monitor omitted process id"))?,
    ))
}

fn owner(config: &Path, space: &str) -> io::Result<Value> {
    invoke(config, &["server", "owner", "--game-space", space], 0)
}

fn owner_events(config: &Path, space: &str, after: u64) -> io::Result<Value> {
    let after = after.to_string();
    let result = invoke(
        config,
        &[
            "server",
            "owner",
            "--events-after",
            &after,
            "--game-space",
            space,
        ],
        0,
    )?;
    Ok(result)
}

fn assert_event_batch(replay: &Value, after: u64, expected_owner: &Value) -> io::Result<()> {
    check_same_owner(replay, expected_owner)?;
    let batch = &replay["data"]["events"];
    let events = batch["events"]
        .as_array()
        .ok_or_else(|| io::Error::other("Owner event replay omitted events"))?;
    let earliest = batch["earliest_available"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Owner event replay omitted earliest cursor"))?;
    let head = batch["head"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Owner event replay omitted head cursor"))?;
    if batch["after"].as_u64() != Some(after)
        || batch["gap"].as_bool().is_none()
        || batch["gap"] != Value::Bool(after.saturating_add(1) < earliest)
        || earliest == 0
        || earliest > head.saturating_add(1)
        || events.len() > 8
    {
        return Err(io::Error::other(format!(
            "Owner replay cursor metadata or event bound is invalid: {batch}"
        )));
    }
    let mut previous = after.max(earliest.saturating_sub(1));
    for event in events {
        let sequence = event["sequence"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Owner event omitted sequence"))?;
        if sequence <= previous || sequence > head {
            return Err(io::Error::other(
                "Owner replay events were out of sequence or beyond head",
            ));
        }
        previous = sequence;
    }
    Ok(())
}

fn lifecycle_completion_for_run(event: &Value, run: &str) -> bool {
    if event["kind"] != "lifecycle_result" {
        return false;
    }
    let Some(bytes) = event["payload"].as_array() else {
        return false;
    };
    let payload: Vec<u8> = bytes
        .iter()
        .filter_map(Value::as_u64)
        .filter_map(|byte| u8::try_from(byte).ok())
        .collect();
    let Ok(result) = serde_json::from_slice::<Value>(&payload) else {
        return false;
    };
    result["outcome"]["completed"] == true
        && result["outcome"]["status"]["run_generation"]
            .as_u64()
            .is_some_and(|generation| generation.to_string() == run)
}

fn owner_data(value: &Value) -> io::Result<&Value> {
    value
        .pointer("/data/owner")
        .filter(|owner| owner.is_object())
        .ok_or_else(|| io::Error::other("Owner snapshot is missing"))
}

fn check_owner(value: &Value, space: &str) -> io::Result<()> {
    let owner = owner_data(value)?;
    if owner["scope"]["space_id"] != space
        || owner["database_claimed"] != true
        || owner["database_opened"] != false
        || owner["lifetime"] != "detached"
    {
        return Err(io::Error::other(format!(
            "Unexpected detached owner snapshot: {owner}"
        )));
    }
    Ok(())
}

fn check_same_owner(value: &Value, expected: &Value) -> io::Result<()> {
    let current = owner_data(value)?;
    if current["scope"]["owner_id"] != expected["scope"]["owner_id"]
        || current["scope"]["generation"] != expected["scope"]["generation"]
        || current["scope"]["context_key"] != expected["scope"]["context_key"]
        || current["database_claimed"] != true
        || current["database_opened"] != false
    {
        return Err(io::Error::other(
            "Controller did not reconnect to the same database-claiming owner",
        ));
    }
    Ok(())
}

fn monitor(config: &Path, space: &str, instance: &str) -> io::Result<Value> {
    invoke(
        config,
        &[
            "server",
            "instance",
            "monitor",
            instance,
            "--game-space",
            space,
        ],
        0,
    )
}

fn invoke(config: &Path, args: &[&str], expected: i32) -> io::Result<Value> {
    let (code, value) = invoke_raw(config, args)?;
    if code != expected || value["ok"] != (expected == 0) {
        return Err(io::Error::other(format!(
            "CLI exit {code}, expected {expected}, args={args:?}: {value}"
        )));
    }
    Ok(value)
}

fn invoke_raw(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args(["--json"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing CLI stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing CLI stderr"))?;
    let out = thread::spawn(move || read_bounded(stdout, MAX_STDOUT));
    let err = thread::spawn(move || read_bounded(stderr, MAX_STDERR));
    let deadline = Instant::now() + COMMAND_DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("CLI deadline exceeded: {args:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = out
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))??;
    let stderr = err
        .join()
        .map_err(|_| io::Error::other("stderr reader panicked"))??;
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| {
        io::Error::other(format!(
            "Invalid CLI JSON; stderr={}",
            String::from_utf8_lossy(&stderr)
        ))
    })?;
    Ok((status.code().unwrap_or(-1), value))
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > limit {
            return Err(io::Error::other("CLI output exceeded bound"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} must be set explicitly")))?;
    if !path.is_absolute() {
        return Err(io::Error::other(format!("{name} must be an absolute path")));
    }
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(io::Error::other(format!("{name} must name a directory")));
    }
    Ok(path)
}

fn required_identifier(name: &str) -> io::Result<String> {
    let value = std::env::var(name)
        .map_err(|_| io::Error::other(format!("{name} must be set explicitly")))?;
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(io::Error::other(format!("{name} is invalid")));
    }
    Ok(value)
}

fn required_string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

fn required_number_string(value: &Value, label: &str) -> io::Result<String> {
    value
        .as_u64()
        .map(|number| number.to_string())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

struct InstallationEvidence {
    executable: PathBuf,
    bytes: u64,
    blake3: String,
}

fn installation_evidence(
    config: &Path,
    space: &str,
    instance_id: &str,
) -> io::Result<InstallationEvidence> {
    let listing = invoke(
        config,
        &["server", "instance", "list", "--game-space", space],
        0,
    )?;
    let catalog = &listing["data"]["catalog"];
    let instance = catalog["instances"]
        .as_array()
        .and_then(|instances| {
            instances
                .iter()
                .find(|item| item["id"].as_str() == Some(instance_id))
        })
        .ok_or_else(|| io::Error::other("Prepared instance was not in the owner space catalog"))?;
    let installation_id = required_string(&instance["installation_id"], "installation id")?;
    let installation = catalog["installations"]
        .as_array()
        .and_then(|installations| {
            installations
                .iter()
                .find(|item| item["id"].as_str() == Some(installation_id))
        })
        .ok_or_else(|| io::Error::other("Prepared instance installation was not in catalog"))?;
    let directory = PathBuf::from(required_string(
        &installation["directory"],
        "installation directory",
    )?);
    if !directory.is_absolute() {
        return Err(io::Error::other("Installation directory was not absolute"));
    }
    let directory = directory.canonicalize()?;
    let executable_name = required_string(&installation["executable"], "installation executable")?;
    let executable = directory.join(executable_name);
    let canonical_executable = executable.canonicalize()?;
    if canonical_executable != executable || !canonical_executable.starts_with(&directory) {
        return Err(io::Error::other(
            "Installation executable was not an ordinary file under its canonical directory",
        ));
    }
    let metadata = std::fs::symlink_metadata(&canonical_executable)?;
    if !metadata.file_type().is_file() || metadata.len() > 2 * 1024 * 1024 * 1024 {
        return Err(io::Error::other(
            "Installation executable is not a bounded ordinary file",
        ));
    }
    let (bytes, blake3) = hash_file(&canonical_executable)?;
    if bytes
        != installation["evidence"]["bytes"]
            .as_u64()
            .unwrap_or_default()
        || installation["evidence"]["blake3"].as_str() != Some(blake3.as_str())
    {
        return Err(io::Error::other(
            "Installation executable hash did not match catalog evidence",
        ));
    }
    Ok(InstallationEvidence {
        executable: canonical_executable,
        bytes,
        blake3,
    })
}

fn verify_installation_evidence(expected: &InstallationEvidence) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(&expected.executable)?;
    if !metadata.file_type().is_file() || metadata.len() != expected.bytes {
        return Err(io::Error::other(
            "Installation executable changed size or file type during lifecycle test",
        ));
    }
    let (bytes, blake3) = hash_file(&expected.executable)?;
    if bytes != expected.bytes || blake3 != expected.blake3 {
        return Err(io::Error::other(
            "Installation executable changed during lifecycle test",
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let file = File::open(path)?;
    let limit = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    let bytes = io::copy(&mut file.take(limit.saturating_add(1)), &mut hasher)?;
    if bytes != limit {
        return Err(io::Error::other(
            "Installation executable changed while being hashed",
        ));
    }
    Ok((bytes, hasher.finalize().to_hex().to_string()))
}

fn retain_evidence(config: &Path, evidence: &[Value]) -> io::Result<()> {
    let dir = config.join("d24-evidence");
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let bytes = serde_json::to_vec_pretty(evidence)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("D24 evidence exceeded its bound"));
    }
    std::fs::write(dir.join("run.json"), bytes)
}

struct Cleanup {
    config: PathBuf,
    space: String,
    instance: String,
    owner: Value,
    run: Option<String>,
    active: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.active || !self.owner_matches() {
            return;
        }
        if let Some(run) = &self.run {
            let _ = invoke_with_deadline(
                &self.config,
                &[
                    "server",
                    "instance",
                    "stop",
                    &self.instance,
                    "--expected-run-generation",
                    run,
                    "--timeout-ms",
                    "30000",
                    "--yes",
                    "--game-space",
                    &self.space,
                ],
                Duration::from_secs(45),
            );
        }
        if !self.owner_matches() || self.active_sessions() != Some(0) {
            return;
        }
        let _ = invoke_with_deadline(
            &self.config,
            &[
                "server",
                "supervise",
                "--shutdown",
                "--yes",
                "--game-space",
                &self.space,
            ],
            Duration::from_secs(15),
        );
    }
}

impl Cleanup {
    fn owner_matches(&self) -> bool {
        invoke_with_deadline(
            &self.config,
            &["server", "owner", "--game-space", &self.space],
            Duration::from_secs(8),
        )
        .ok()
        .and_then(|value| owner_data(&value).ok().cloned())
        .is_some_and(|current| {
            current["scope"]["owner_id"] == self.owner["scope"]["owner_id"]
                && current["scope"]["generation"] == self.owner["scope"]["generation"]
                && current["scope"]["context_key"] == self.owner["scope"]["context_key"]
        })
    }

    fn active_sessions(&self) -> Option<usize> {
        invoke_with_deadline(
            &self.config,
            &["server", "owner", "--game-space", &self.space],
            Duration::from_secs(8),
        )
        .ok()?
        .pointer("/data/owner/active_sessions")?
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
    }
}

fn invoke_with_deadline(config: &Path, args: &[&str], deadline: Duration) -> io::Result<Value> {
    invoke_raw_deadline(config, args, deadline.max(Duration::from_millis(1))).and_then(
        |(code, value)| {
            if code == 0 {
                Ok(value)
            } else {
                Err(io::Error::other(format!("Cleanup CLI exited {code}")))
            }
        },
    )
}

fn invoke_raw_deadline(
    config: &Path,
    args: &[&str],
    deadline: Duration,
) -> io::Result<(i32, Value)> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args(["--json"])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing cleanup stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing cleanup stderr"))?;
    let out = thread::spawn(move || read_bounded(stdout, MAX_STDOUT));
    let err = thread::spawn(move || read_bounded(stderr, MAX_STDERR));
    let until = Instant::now() + deadline;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Authenticated cleanup CLI timed out",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = out
        .join()
        .map_err(|_| io::Error::other("cleanup stdout reader panicked"))??;
    let _stderr = err
        .join()
        .map_err(|_| io::Error::other("cleanup stderr reader panicked"))??;
    let value = serde_json::from_slice(&stdout).map_err(io::Error::other)?;
    Ok((status.code().unwrap_or(-1), value))
}
