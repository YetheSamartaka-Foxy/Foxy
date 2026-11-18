#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OWNER_START_DEADLINE: Duration = Duration::from_secs(30);
const READY_DEADLINE: Duration = Duration::from_secs(240);
const OWNER_JOIN_DEADLINE: Duration = Duration::from_secs(90);

#[test]
#[ignore = "Requires an explicitly prepared native server config with an accepted runtime"]
fn native_restore_roundtrip_preserves_unrelated_files_and_runtime() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_RESTORE_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_RESTORE_SPACE")?;
    if !matches!(space.as_str(), "arma3" | "reforger") {
        return Err(io::Error::other("Restore space must be arma3 or reforger"));
    }
    let instance = required_identifier("FOXY_NATIVE_RESTORE_INSTANCE")?;
    let output = required_directory("FOXY_NATIVE_RESTORE_OUTPUT")?;
    let mut evidence = Evidence::new(&output)?;

    let (owner_code, owner_status) =
        invoke_raw(&config, &["server", "owner", "--game-space", &space])?;
    if owner_code != 4 {
        return Err(io::Error::other(format!(
            "Refusing restore acceptance with a pre-existing owner: {owner_status}"
        )));
    }
    let restore_status = invoke(
        &config,
        &[
            "server",
            "restore",
            "status",
            &instance,
            "--game-space",
            &space,
        ],
    )?;
    if restore_status["data"]["status"]["pending"].is_object() {
        return Err(io::Error::other(
            "Refusing restore acceptance with pending recovery",
        ));
    }

    let config_space = config.join("games").join(&space);
    let save_root = config_space
        .join("server/instances")
        .join(&instance)
        .join("deployments/runtime-state/home/profiles");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let sentinel_name = format!("foxy-restore-acceptance-{nonce}.bin");
    let unknown_name = format!("foxy-restore-acceptance-{nonce}.unknown");
    let sentinel = save_root.join(&sentinel_name);
    let unknown = save_root.join(&unknown_name);
    if sentinel.exists() || unknown.exists() {
        return Err(io::Error::other(
            "Refusing to overwrite an existing restore acceptance sentinel",
        ));
    }
    let before_executable = installation_hash(&config, &space, &instance)?;
    let original = format!("native-restore-sentinel-before-{nonce}").into_bytes();
    let changed = format!("native-restore-sentinel-current-{nonce}").into_bytes();
    let changed_after_review = format!("native-restore-sentinel-stale-{nonce}").into_bytes();
    fs::create_dir_all(&save_root)?;
    fs::write(&sentinel, &original)?;

    let mut owner = start_parent_bound_owner(&config, &space)?;
    wait_owner(&config, &space, &mut owner)?;
    let first_run = start_and_wait(&config, &space, &instance, &mut evidence)?;
    let stopped = stop(&config, &space, &instance, first_run, &mut evidence)?;
    if stopped["data"]["outcome"]["status"]["cleanup_verified"] != true {
        return Err(io::Error::other(format!(
            "Initial graceful stop did not verify cleanup: {stopped}"
        )));
    }

    let backup = invoke(
        &config,
        &[
            "server",
            "backup",
            "create",
            &instance,
            "--no-retention",
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(backup.clone())?;
    let manifest = &backup["data"]["outcome"]["manifest"];
    if manifest["game_space_id"] != space || manifest["instance_id"] != instance {
        return Err(io::Error::other(format!(
            "Backup manifest is not mapped to the requested game space and instance: {manifest}"
        )));
    }
    let backup_id = string(&manifest["backup_id"], "backup id")?.to_owned();
    if manifest["consistency"]["mode"] != "stopped_verified"
        || manifest["consistency"]["stopped_verified"] != true
    {
        return Err(io::Error::other("Restore backup is not stopped-verified"));
    }
    if !manifest["entries"].as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry["set"] == "save"
                && entry["archive_path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(&sentinel_name))
        })
    }) {
        return Err(io::Error::other(
            "The stopped backup does not contain the acceptance sentinel",
        ));
    }

    fs::write(&sentinel, &changed)?;
    fs::write(&unknown, b"unrelated file created after backup")?;
    let planned = restore_plan(&config, &space, &instance, &backup_id)?;
    evidence.push(planned.clone())?;
    let plan = &planned["data"]["plan"];
    if plan["blockers"]
        .as_array()
        .is_none_or(|blockers| !blockers.is_empty())
    {
        return Err(io::Error::other(format!(
            "Compatible restore plan has blockers: {}",
            plan["blockers"]
        )));
    }
    let fingerprint = string(&plan["fingerprint"], "restore fingerprint")?.to_owned();

    let dry_run = invoke(
        &config,
        &[
            "--dry-run",
            "server",
            "restore",
            "apply",
            &instance,
            "--backup-id",
            &backup_id,
            "--expected-plan-fingerprint",
            &fingerprint,
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(dry_run.clone())?;
    if dry_run["data"]["writes_performed"] != false
        || dry_run["data"]["dry_run"] != true
        || fs::read(&sentinel)? != changed
        || fs::read(&unknown)? != b"unrelated file created after backup"
    {
        return Err(io::Error::other(
            "Restore dry run changed files or reported writes",
        ));
    }

    fs::write(&sentinel, &changed_after_review)?;
    let (stale_code, stale) = invoke_raw(
        &config,
        &[
            "server",
            "restore",
            "apply",
            &instance,
            "--backup-id",
            &backup_id,
            "--expected-plan-fingerprint",
            &fingerprint,
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(stale.clone())?;
    if stale_code != 2 || fs::read(&sentinel)? != changed_after_review {
        return Err(io::Error::other(format!(
            "Stale restore review was not rejected without writes: {stale}"
        )));
    }

    let fresh = restore_plan(&config, &space, &instance, &backup_id)?;
    evidence.push(fresh.clone())?;
    let fresh_plan = &fresh["data"]["plan"];
    if fresh_plan["blockers"]
        .as_array()
        .is_none_or(|blockers| !blockers.is_empty())
    {
        return Err(io::Error::other(format!(
            "Fresh compatible restore plan has blockers: {}",
            fresh_plan["blockers"]
        )));
    }
    let fresh_fingerprint = string(&fresh_plan["fingerprint"], "fresh restore fingerprint")?;
    let applied = invoke(
        &config,
        &[
            "server",
            "restore",
            "apply",
            &instance,
            "--backup-id",
            &backup_id,
            "--expected-plan-fingerprint",
            fresh_fingerprint,
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(applied.clone())?;
    if applied["data"]["writes_performed"] != true
        || fs::read(&sentinel)? != original
        || fs::read(&unknown)? != b"unrelated file created after backup"
    {
        return Err(io::Error::other(
            "Restore did not restore exact sentinel bytes while preserving the unknown sibling",
        ));
    }
    let clear_status = invoke(
        &config,
        &[
            "server",
            "restore",
            "status",
            &instance,
            "--game-space",
            &space,
        ],
    )?;
    if !clear_status["data"]["status"]["pending"].is_null() {
        return Err(io::Error::other(format!(
            "Restore journal remains pending after successful apply: {clear_status}"
        )));
    }

    let second_run = start_and_wait(&config, &space, &instance, &mut evidence)?;
    if second_run <= first_run {
        return Err(io::Error::other(
            "Restored runtime did not start a fresh native run",
        ));
    }
    let final_stop = stop(&config, &space, &instance, second_run, &mut evidence)?;
    if final_stop["data"]["outcome"]["status"]["cleanup_verified"] != true {
        return Err(io::Error::other(format!(
            "Final graceful stop did not verify cleanup: {final_stop}"
        )));
    }
    let after_executable = installation_hash(&config, &space, &instance)?;
    if fs::read(&sentinel)? != original
        || fs::read(&unknown)? != b"unrelated file created after backup"
    {
        return Err(io::Error::other(
            "Restored and unrelated files did not survive the fresh native run",
        ));
    }
    if before_executable != after_executable {
        return Err(io::Error::other(
            "Restore changed the adopted/source server executable",
        ));
    }
    fs::remove_file(&sentinel)?;
    fs::remove_file(&unknown)?;
    owner.close_and_wait()?;
    evidence.summary(json!({
        "result": "passed",
        "game_space_id": space,
        "instance_id": instance,
        "backup_id": backup_id,
        "first_run_generation": first_run,
        "second_run_generation": second_run,
        "restored_sentinel_bytes": original.len(),
        "unknown_sibling_preserved": true,
        "executable_blake3": before_executable,
        "native_save_format_recovery": false
    }))?;
    Ok(())
}

fn restore_plan(config: &Path, space: &str, instance: &str, backup: &str) -> io::Result<Value> {
    invoke(
        config,
        &[
            "server",
            "restore",
            "plan",
            instance,
            "--backup-id",
            backup,
            "--game-space",
            space,
        ],
    )
}

fn start_and_wait(
    config: &Path,
    space: &str,
    instance: &str,
    evidence: &mut Evidence,
) -> io::Result<u64> {
    let plan = invoke(
        config,
        &[
            "server",
            "instance",
            "start-plan",
            instance,
            "--game-space",
            space,
        ],
    )?;
    let generation =
        number(&plan["data"]["plan"]["activation_generation"], "activation")?.to_string();
    let fingerprint = string(
        &plan["data"]["plan"]["fingerprint"],
        "activation fingerprint",
    )?
    .to_owned();
    let started = invoke(
        config,
        &[
            "server",
            "instance",
            "start",
            instance,
            "--expected-activation-generation",
            &generation,
            "--expected-plan-fingerprint",
            &fingerprint,
            "--yes",
            "--game-space",
            space,
        ],
    )?;
    evidence.push(started.clone())?;
    let run = number(
        &started["data"]["outcome"]["status"]["run_generation"],
        "run generation",
    )?;
    wait_ready(config, space, instance, run, evidence)?;
    Ok(run)
}

fn wait_ready(
    config: &Path,
    space: &str,
    instance: &str,
    run: u64,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let report = invoke(
            config,
            &[
                "server",
                "instance",
                "monitor",
                instance,
                "--game-space",
                space,
            ],
        )?;
        let monitoring = &report["data"]["monitoring"];
        if monitoring["query_ready"] == true && monitoring["status"]["run_generation"] == run {
            evidence.push(report)?;
            return Ok(());
        }
        evidence.push(report.clone())?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Run {run} did not become query-ready: {monitoring}"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn stop(
    config: &Path,
    space: &str,
    instance: &str,
    run: u64,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let run = run.to_string();
    let stopped = invoke(
        config,
        &[
            "server",
            "instance",
            "stop",
            instance,
            "--expected-run-generation",
            &run,
            "--timeout-ms",
            "60000",
            "--yes",
            "--game-space",
            space,
        ],
    )?;
    evidence.push(stopped.clone())?;
    if stopped["data"]["outcome"]["completed"] != true {
        return Err(io::Error::other(format!(
            "Graceful stop did not complete: {stopped}"
        )));
    }
    Ok(stopped)
}

fn installation_hash(config: &Path, space: &str, instance: &str) -> io::Result<String> {
    let listing = invoke(
        config,
        &["server", "instance", "list", "--game-space", space],
    )?;
    let catalog = &listing["data"]["catalog"];
    let item = catalog["instances"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == instance))
        .ok_or_else(|| io::Error::other("Prepared instance is missing"))?;
    let installation_id = string(&item["installation_id"], "installation id")?;
    let installation = catalog["installations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == installation_id))
        .ok_or_else(|| io::Error::other("Prepared installation is missing"))?;
    let executable = PathBuf::from(string(
        &installation["directory"],
        "installation directory",
    )?)
    .join(string(
        &installation["executable"],
        "installation executable",
    )?);
    let mut file = File::open(executable)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn start_parent_bound_owner(config: &Path, space: &str) -> io::Result<OwnedChild> {
    let mut command = base_command(config);
    command
        .args([
            "server",
            "supervise",
            "--parent-bound",
            "--game-space",
            space,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(OwnedChild(Some(command.spawn()?)))
}

fn wait_owner(config: &Path, space: &str, child: &mut OwnedChild) -> io::Result<()> {
    let deadline = Instant::now() + OWNER_START_DEADLINE;
    loop {
        if child
            .0
            .as_mut()
            .and_then(|process| process.try_wait().ok().flatten())
            .is_some()
        {
            return Err(io::Error::other(
                "Parent-bound owner exited before readiness",
            ));
        }
        let (code, value) = invoke_raw(config, &["server", "owner", "--game-space", space])?;
        if code == 0 && value.pointer("/data/owner/database_claimed") == Some(&Value::Bool(true)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owner did not become ready",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

struct OwnedChild(Option<Child>);
impl OwnedChild {
    fn close_and_wait(&mut self) -> io::Result<()> {
        let Some(mut child) = self.0.take() else {
            return Ok(());
        };
        drop(child.stdin.take());
        wait_bounded(&mut child)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            drop(child.stdin.take());
            let _ = wait_bounded(&mut child);
        }
    }
}

fn wait_bounded(child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + OWNER_JOIN_DEADLINE;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Parent-bound owner did not exit after stdin closed",
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct Evidence {
    path: PathBuf,
    values: Vec<Value>,
}
impl Evidence {
    fn new(output: &Path) -> io::Result<Self> {
        Ok(Self {
            path: output.join("server_restore_native.json"),
            values: Vec::new(),
        })
    }
    fn push(&mut self, value: Value) -> io::Result<()> {
        self.values.push(value);
        self.flush()
    }
    fn summary(&mut self, value: Value) -> io::Result<()> {
        self.values.push(value);
        self.flush()
    }
    fn flush(&self) -> io::Result<()> {
        fs::write(&self.path, serde_json::to_vec_pretty(&self.values)?)
    }
}
impl Drop for Evidence {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn required_identifier(name: &str) -> io::Result<String> {
    let value = std::env::var(name).map_err(io::Error::other)?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::other(format!("Invalid {name}")));
    }
    Ok(value)
}

fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(name).ok_or_else(|| io::Error::other(format!("Missing {name}")))?,
    );
    if !path.is_absolute() || !path.is_dir() {
        return Err(io::Error::other(format!(
            "{name} must be an existing absolute directory"
        )));
    }
    path.canonicalize()
}

fn string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

fn number(value: &Value, label: &str) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

fn invoke(config: &Path, args: &[&str]) -> io::Result<Value> {
    let (code, value) = invoke_raw(config, args)?;
    if code != 0 || value["ok"] != true {
        return Err(io::Error::other(format!(
            "CLI exit {code}, args={args:?}: {value}"
        )));
    }
    Ok(value)
}

fn invoke_raw(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
    let mut command = base_command(config);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output()?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        io::Error::other(format!(
            "CLI emitted invalid JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ))
    })?;
    Ok((output.status.code().unwrap_or(-1), value))
}

fn base_command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args(["--json"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
