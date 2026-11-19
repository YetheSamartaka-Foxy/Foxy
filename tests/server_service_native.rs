#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

#[path = "server_service_native/load.rs"]
mod load;

use serde_json::Value;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CLI_TIMEOUT: Duration = Duration::from_secs(360);
const START_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_OUTPUT: usize = 2 * 1024 * 1024;

#[test]
#[ignore = "Requires a prepared private native server config, instance, and output directory"]
fn native_exported_foreground_service_owns_and_drains_native_server() -> io::Result<()> {
    let config = required_dir("FOXY_SERVICE_CONFIG")?;
    let space = required_id("FOXY_SERVICE_SPACE")?;
    let instance = required_id("FOXY_SERVICE_INSTANCE")?;
    let output = required_dir("FOXY_SERVICE_OUTPUT")?;
    let steam_config = optional_dir("FOXY_SERVICE_STEAM_CONFIG")?.unwrap_or_else(|| config.clone());
    let steam_space = std::env::var("FOXY_SERVICE_STEAM_SPACE").unwrap_or_else(|_| space.clone());
    let manager_unit = std::env::var("FOXY_SERVICE_MANAGER_UNIT").ok();
    #[cfg(windows)]
    if manager_unit.is_some() {
        return Err(io::Error::other("FOXY_SERVICE_MANAGER_UNIT is Linux-only"));
    }
    #[cfg(target_os = "linux")]
    if manager_unit
        .as_deref()
        .is_some_and(|unit| !valid_manager_unit(unit))
    {
        return Err(io::Error::other(
            "Manager mode requires a foxy-d33-<safe>.service basename",
        ));
    }

    let before_owner = raw_cli(&config, &["server", "owner", "--game-space", &space])?;
    if before_owner.0 == 0 && manager_unit.is_none() {
        return Err(io::Error::other(
            "Refusing to attach to an owner that existed before this test",
        ));
    }
    if before_owner.0 != 0 && before_owner.0 != 4 {
        return Err(io::Error::other(format!(
            "Could not establish owner state before test: exit {}",
            before_owner.0
        )));
    }
    if before_owner.0 == 0 {
        let monitor = cli(
            &config,
            &[
                "server",
                "instance",
                "monitor",
                &instance,
                "--game-space",
                &space,
            ],
            0,
        )?;
        if monitor.pointer("/data/monitoring/status/process_exists") == Some(&Value::Bool(true)) {
            return Err(io::Error::other(
                "Refusing manager-mode reuse while a native server is live",
            ));
        }
    }

    let steam_resume = steam_resume(&steam_config, &steam_space)?;
    if steam_resume["data"]["outcome"]["authenticated"] != true
        || steam_resume["data"]["outcome"]["disconnected"] != true
        || steam_resume["data"]["outcome"]["prepared"] != true
    {
        return Err(io::Error::other(
            "Prepared Steam session did not authenticate and disconnect cleanly",
        ));
    }

    let format = if cfg!(windows) {
        "windows-user-task"
    } else {
        "systemd-user"
    };
    let preview = cli(
        &config,
        &[
            "server",
            "service",
            "export",
            "--format",
            format,
            "--game-space",
            &space,
        ],
        0,
    )?;
    let plan = &preview["data"]["plan"];
    let expected_game = if space == "arma3" || space == "reforger" {
        space.as_str()
    } else {
        return Err(io::Error::other(
            "Prepared game space must be arma3 or reforger",
        ));
    };
    if plan["game_space_id"] != space
        || plan["game_id"] != expected_game
        || plan["format"] != format.replace('-', "_")
        || plan["registration_performed"] != false
        || plan["writes_performed"] != false
        || preview["data"]["database_opened"] != false
        || preview["data"]["writes_performed"] != false
        || preview["data"]["registration_performed"] != false
        || plan["arguments"].as_array().is_none_or(|args| {
            let mut expected = vec![
                "--config-dir",
                plan["arguments"][1].as_str().unwrap_or_default(),
                "server",
                "supervise",
                "--game-space",
                space.as_str(),
            ];
            if cfg!(windows) {
                expected.push("--background");
            }
            args.iter().filter_map(Value::as_str).collect::<Vec<_>>() != expected
        })
        || plan["stop_arguments"]
            .as_array()
            .is_none_or(|args| args.len() != 8 || args[6] != "--shutdown" || args[7] != "--yes")
    {
        return Err(io::Error::other(format!(
            "Invalid service preview: {preview}"
        )));
    }
    assert_owner_unchanged(&config, &space, &before_owner)?;
    let definition = plan["definition"]
        .as_str()
        .ok_or_else(|| io::Error::other("Service preview omitted definition"))?;
    if definition.len() > 64 * 1024 {
        return Err(io::Error::other("Service definition exceeded its bound"));
    }
    #[cfg(target_os = "linux")]
    if !definition.contains("Type=exec")
        || !definition.contains("KillMode=mixed")
        || !definition.contains("TimeoutStopSec=15min")
        || definition.contains("ExecStop=")
    {
        return Err(io::Error::other(
            "Systemd definition violated foreground drain policy",
        ));
    }
    #[cfg(windows)]
    if !definition.contains("InteractiveToken") {
        return Err(io::Error::other(
            "Task definition omitted the logged-in user token",
        ));
    }
    let executable = PathBuf::from(
        plan["executable"]
            .as_str()
            .ok_or_else(|| io::Error::other("Service preview omitted executable"))?,
    );
    let args = plan["arguments"]
        .as_array()
        .and_then(|items| items.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        .ok_or_else(|| io::Error::other("Service preview arguments were malformed"))?;

    let definition_path = output.join("foxy-service-native.definition");
    if definition_path.exists() {
        return Err(io::Error::other(
            "Refusing to overwrite an existing private service definition",
        ));
    }
    let destination = definition_path.to_string_lossy().into_owned();
    let dry_run = raw_cli(
        &config,
        &[
            "--dry-run",
            "server",
            "service",
            "export",
            "--format",
            format,
            "--output",
            &destination,
            "--game-space",
            &space,
        ],
    )?;
    if dry_run.0 != 0
        || dry_run.1["data"]["database_opened"] != false
        || dry_run.1["data"]["dry_run"] != true
        || definition_path.exists()
    {
        return Err(io::Error::other(
            "Service export dry-run wrote output or failed its read-only contract",
        ));
    }
    assert_owner_unchanged(&config, &space, &before_owner)?;
    let written = cli(
        &config,
        &[
            "server",
            "service",
            "export",
            "--format",
            format,
            "--output",
            &destination,
            "--game-space",
            &space,
        ],
        0,
    )?;
    if written["data"]["writes_performed"] != true
        || written["data"]["registration_performed"] != false
        || written["data"]["database_opened"] != false
        || std::fs::read_to_string(&definition_path)? != definition
    {
        return Err(io::Error::other(
            "Service definition write differed from preview",
        ));
    }
    let overwrite = raw_cli(
        &config,
        &[
            "server",
            "service",
            "export",
            "--format",
            format,
            "--output",
            &destination,
            "--game-space",
            &space,
        ],
    )?;
    if overwrite.0 == 0 || std::fs::read_to_string(&definition_path)? != definition {
        return Err(io::Error::other(
            "Service export overwrote an existing definition",
        ));
    }
    assert_owner_unchanged(&config, &space, &before_owner)?;
    let installation = installation_evidence(&config, &space, &instance)?;
    let mut save_backup = if space == "reforger" {
        Some(SaveBackup::prepare(
            &config,
            &space,
            &instance,
            &installation.id,
            &output,
        )?)
    } else {
        None
    };
    let mut cleanup = Cleanup {
        config: config.clone(),
        space: space.clone(),
        instance: instance.clone(),
        run: None,
        owner: None,
        child: None,
        #[cfg(target_os = "linux")]
        manager_unit: None,
        active: true,
    };

    #[cfg(target_os = "linux")]
    if let Some(unit) = manager_unit.as_deref() {
        validate_installed_unit(unit, definition)?;
        if before_owner.0 == 0 {
            let status = systemctl_show(unit)?;
            if status.get("ActiveState").map(String::as_str) != Some("active")
                || !manager_process_matches(unit, &executable, &args)?
            {
                return Err(io::Error::other(
                    "Pre-existing owner was not the exact validated manager unit",
                ));
            }
        } else {
            run_systemctl(&["start", unit])?;
        }
        cleanup.manager_unit = Some(unit.to_owned());
        let status = systemctl_show(unit)?;
        if status.get("ActiveState").map(String::as_str) != Some("active")
            || status.get("Type").map(String::as_str) != Some("exec")
            || !manager_process_matches(unit, &executable, &args)?
        {
            return Err(io::Error::other(
                "Exported foreground unit did not become active",
            ));
        }
        let pid = status
            .get("MainPID")
            .and_then(|pid| pid.parse::<u32>().ok())
            .ok_or_else(|| io::Error::other("Active service omitted MainPID"))?;
        assert_process_has_no_desktop_environment(pid)?;
    }
    if manager_unit.is_none() {
        let mut command = Command::new(&executable);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        command
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        cleanup.child = Some(command.spawn()?);
        #[cfg(target_os = "linux")]
        assert_process_has_no_desktop_environment(cleanup.child.as_ref().unwrap().id())?;
    }

    let owner = wait_owner(&config, &space)?;
    if owner["lifetime"] != "foreground"
        || owner["database_claimed"] != true
        || owner["database_opened"] != false
    {
        return Err(io::Error::other(format!(
            "Unexpected foreground owner: {owner}"
        )));
    }
    cleanup.owner = Some(owner.clone());
    assert_update_blocked(&config, &space, true)?;
    let competing = raw_cli(
        &config,
        &[
            "server",
            "supervise",
            "--parent-bound",
            "--game-space",
            &space,
        ],
    )?;
    if competing.0 != 6
        || raw_cli(&config, &["server", "owner", "--game-space", &space])?
            .1
            .pointer("/data/owner/scope/owner_id")
            != owner.pointer("/scope/owner_id")
    {
        return Err(io::Error::other(
            "A competing owner was not refused with the owner unchanged",
        ));
    }
    let start_plan = cli(
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
    let planned = &start_plan["data"]["plan"];
    let generation = required_number(&planned["activation_generation"], "activation generation")?;
    let fingerprint = required_str(&planned["fingerprint"], "plan fingerprint")?;
    let started = cli(
        &config,
        &[
            "server",
            "instance",
            "start",
            &instance,
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
    let run = required_number(
        &started["data"]["outcome"]["status"]["run_generation"],
        "run generation",
    )?;
    cleanup.run = Some(run.clone());
    let active_shutdown = raw_cli(
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
    if active_shutdown.0 != 4 || !cleanup.owns_current(&owner) {
        return Err(io::Error::other(
            "Idle shutdown did not refuse the active native service",
        ));
    }
    #[cfg(windows)]
    let stop_result;
    #[cfg(target_os = "linux")]
    let stop_result: Option<Value> = None;
    #[cfg(windows)]
    let owner_exit;
    #[cfg(target_os = "linux")]
    let mut owner_exit = None;
    #[cfg(windows)]
    let manager_exit: Option<Value> = None;
    #[cfg(target_os = "linux")]
    let mut manager_exit = None;
    let ready = wait_ready(
        &config,
        &space,
        &instance,
        run.parse().map_err(io::Error::other)?,
    )?;
    if ready["data"]["monitoring"]["status"]["owner_id"] != owner["scope"]["owner_id"]
        || !genuine_observation(&ready)
    {
        return Err(io::Error::other(
            "Fresh native readiness did not match the service owner",
        ));
    }
    assert_update_blocked(&config, &space, true)?;
    wait_rcon(&config, &space, &instance, &run)?;
    let rcon_test = cli(
        &config,
        &["server", "rcon", "test", &instance, "--game-space", &space],
        0,
    )?;
    if rcon_test["ok"] != true {
        return Err(io::Error::other("Native RCon test did not succeed"));
    }
    if space == "reforger" {
        let (players_code, players) = raw_cli(
            &config,
            &[
                "server",
                "reforger-admin",
                "players",
                &instance,
                "--game-space",
                &space,
            ],
        )?;
        if players_code != 5
            || players["data"]["record"]["text"] != "#players"
            || players["data"]["record"]["run_generation"]
                .as_u64()
                .is_none_or(|generation| generation.to_string() != run)
            || players["data"]["record"]["state"] != "replied"
        {
            return Err(io::Error::other(
                "Typed Reforger players command was not a fresh reply",
            ));
        }
    } else {
        let players = cli(
            &config,
            &[
                "server",
                "admin",
                "players",
                &instance,
                "--game-space",
                &space,
            ],
            0,
        )?;
        if players.pointer("/data/parsed/complete") != Some(&Value::Bool(true))
            || players.pointer("/data/record/state") != Some(&Value::String("replied".into()))
        {
            return Err(io::Error::other(
                "Typed native Arma players command did not receive a complete listing",
            ));
        }
    }
    let reconnected = cli(&config, &["server", "owner", "--game-space", &space], 0)?;
    if reconnected.pointer("/data/owner/scope/owner_id") != owner.pointer("/scope/owner_id")
        || reconnected.pointer("/data/owner/scope/generation") != owner.pointer("/scope/generation")
    {
        return Err(io::Error::other(
            "Owner reconnection changed foreground process identity",
        ));
    }
    let reconnected_monitor = cli(
        &config,
        &[
            "server",
            "instance",
            "monitor",
            &instance,
            "--game-space",
            &space,
        ],
        0,
    )?;
    if reconnected_monitor
        .pointer("/data/monitoring/status/run_generation")
        .and_then(Value::as_u64)
        .is_none_or(|generation| generation.to_string() != run)
        || reconnected_monitor.pointer("/data/monitoring/status/process_id")
            != ready.pointer("/data/monitoring/status/process_id")
    {
        return Err(io::Error::other(
            "Reconnect changed native run or process identity",
        ));
    }
    assert_update_blocked(&config, &space, true)?;
    let load_report = if std::env::var("FOXY_SERVICE_LOAD_ACCEPTANCE").as_deref() == Ok("1") {
        Some(load::poll_owner_and_monitor(
            &config, &space, &instance, &owner, &run, &ready,
        )?)
    } else {
        None
    };

    #[cfg(target_os = "linux")]
    if manager_unit.is_none() {
        signal_owner_pidfd(
            cleanup
                .child
                .as_mut()
                .ok_or_else(|| io::Error::other("Missing retained service child"))?,
        )?;
        wait_child_exit(cleanup.child.as_mut().unwrap())?;
        owner_exit = Some(serde_json::json!({"success": true, "signal": "pidfd_sigterm"}));
        cleanup.child = None;
    } else if let Some(unit) = manager_unit.as_deref() {
        run_systemctl(&["stop", unit])?;
        let status = systemctl_show(unit)?;
        if status.get("ActiveState").map(String::as_str) != Some("inactive")
            || status.get("Result").map(String::as_str) != Some("success")
            || status.get("ExecMainStatus").map(String::as_str) != Some("0")
        {
            return Err(io::Error::other(
                "Manager stop did not report a clean service exit",
            ));
        }
        manager_exit = Some(serde_json::to_value(status)?);
    }
    #[cfg(windows)]
    {
        let stopped = cli(
            &config,
            &[
                "server",
                "instance",
                "stop",
                &instance,
                "--expected-run-generation",
                &run.to_string(),
                "--timeout-ms",
                "60000",
                "--yes",
                "--game-space",
                &space,
            ],
            0,
        )?;
        if stopped["data"]["outcome"]["completed"] != true
            || stopped["data"]["outcome"]["status"]["forced"] != false
        {
            return Err(io::Error::other("Native instance did not stop gracefully"));
        }
        stop_result = Some(stopped.clone());
        let shutdown = cli(
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
        if shutdown["data"]["released"] != true {
            return Err(io::Error::other("Idle foreground owner was not released"));
        }
        owner_exit = Some(shutdown.clone());
        wait_child_exit(
            cleanup
                .child
                .as_mut()
                .ok_or_else(|| io::Error::other("Missing service child"))?,
        )?;
        cleanup.child = None;
    }
    verify_installation(&installation)?;
    let after = raw_cli(&config, &["server", "owner", "--game-space", &space])?;
    if after.0 != 4 {
        return Err(io::Error::other(
            "Foreground service owner remained after shutdown",
        ));
    }
    let consumers = cli(
        &config,
        &[
            "server",
            "installation",
            "consumers",
            &installation.id,
            "--game-space",
            &space,
        ],
        0,
    )?;
    if consumers.pointer("/data/consumer_inspection/inspection_complete")
        != Some(&Value::Bool(true))
        || consumers
            .pointer("/data/consumer_inspection/recorded_consumers")
            .and_then(Value::as_array)
            .is_none_or(|items| !items.is_empty())
    {
        return Err(io::Error::other(
            "Native service left consumer records after owner exit",
        ));
    }
    if let Some(backup) = save_backup.as_mut() {
        backup.verify_savepoint_and_restore()?;
    }
    cleanup.active = false;
    let evidence_path = output.join("foxy-service-native-evidence.json");
    if evidence_path.exists() {
        return Err(io::Error::other(
            "Refusing to overwrite prior native acceptance evidence",
        ));
    }
    std::fs::write(
        evidence_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "preview": preview,
            "write": written,
        "steam_resume": {
            "authenticated": steam_resume["data"]["outcome"]["authenticated"],
            "disconnected": steam_resume["data"]["outcome"]["disconnected"],
            "prepared": steam_resume["data"]["outcome"]["prepared"],
        },
            "owner": owner,
            "start_plan": start_plan,
            "start": started,
            "ready": ready,
            "owner_monitor_load": load_report,
            "rcon_test": rcon_test,
            "stop_result": stop_result,
            "owner_exit": owner_exit,
            "manager_exit": manager_exit,
            "consumers_after_exit": consumers,
            "installation_bytes": installation.bytes,
            "installation_blake3": installation.blake3,
            "reforger_save_delta": save_backup.as_ref().and_then(|backup| backup.delta.clone()),
        }))?,
    )?;
    Ok(())
}

fn assert_update_blocked(config: &Path, space: &str, expected: bool) -> io::Result<()> {
    let result = cli(
        config,
        &[
            "server",
            "owner",
            "--app-update-check",
            "--game-space",
            space,
        ],
        0,
    )?;
    if result["data"]["app_update_blocked"] != expected {
        return Err(io::Error::other(format!(
            "Unexpected update gate: {result}"
        )));
    }
    Ok(())
}

fn assert_owner_unchanged(config: &Path, space: &str, before: &(i32, Value)) -> io::Result<()> {
    let after = raw_cli(config, &["server", "owner", "--game-space", space])?;
    if before.0 != after.0 {
        return Err(io::Error::other(
            "Service export changed owner availability",
        ));
    }
    if before.0 == 0
        && (before.1.pointer("/data/owner/scope/owner_id")
            != after.1.pointer("/data/owner/scope/owner_id")
            || before.1.pointer("/data/owner/scope/generation")
                != after.1.pointer("/data/owner/scope/generation"))
    {
        return Err(io::Error::other(
            "Service export changed the existing owner identity",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn assert_process_has_no_desktop_environment(pid: u32) -> io::Result<()> {
    let path = PathBuf::from("/proc").join(pid.to_string()).join("environ");
    let bytes = std::fs::read(path)?;
    if bytes.len() > 256 * 1024 {
        return Err(io::Error::other("Service environment exceeded its bound"));
    }
    let forbidden = [
        b"DISPLAY".as_slice(),
        b"WAYLAND_DISPLAY",
        b"DBUS_SESSION_BUS_ADDRESS",
    ];
    for entry in bytes.split(|byte| *byte == 0) {
        let key = entry.split(|byte| *byte == b'=').next().unwrap_or_default();
        if forbidden.iter().any(|name| key == *name) {
            return Err(io::Error::other(
                "Foreground service inherited desktop session variables",
            ));
        }
    }
    Ok(())
}

fn genuine_observation(value: &Value) -> bool {
    let monitoring = &value["data"]["monitoring"];
    let status = &monitoring["status"];
    let sample = monitoring["observation"]["resources"]
        .as_array()
        .and_then(|samples| samples.last())
        .map(|item| &item["sample"]);
    status["process_exists"] == true
        && monitoring["observation_age_ms"]
            .as_u64()
            .is_some_and(|age| age <= 10_000)
        && sample.is_some_and(|sample| {
            sample["pid"].as_u64().is_some_and(|pid| pid > 0)
                && sample["cpu_percent"].as_f64().is_some_and(f64::is_finite)
                && sample["resident_bytes"].as_u64().is_some()
        })
        && monitoring["observation"]["logs"]["entries"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty() && entries.len() <= 128)
}

fn wait_ready(config: &Path, space: &str, instance: &str, run: u64) -> io::Result<Value> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let value = cli(
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
        )?;
        if value.pointer("/data/monitoring/query_ready") == Some(&Value::Bool(true))
            && value
                .pointer("/data/monitoring/status/run_generation")
                .and_then(Value::as_u64)
                == Some(run)
        {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Native server did not become query-ready",
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn wait_owner(config: &Path, space: &str) -> io::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let result = raw_cli(config, &["server", "owner", "--game-space", space])?;
        if result.0 == 0 {
            return Ok(result.1["data"]["owner"].clone());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Foreground service owner did not start",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_rcon(config: &Path, space: &str, instance: &str, run: &str) -> io::Result<()> {
    let run = run.parse::<u64>().map_err(io::Error::other)?;
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let status = cli(
            config,
            &["server", "rcon", "status", instance, "--game-space", space],
            0,
        )?;
        let session = &status["data"]["session"];
        if session["connection"] == "connected" && session["run_generation"] == run {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "RCon did not connect for the current run (state {})",
                session["connection"]
            )));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn cli(config: &Path, args: &[&str], expected: i32) -> io::Result<Value> {
    let (code, value) = raw_cli(config, args)?;
    if code != expected || value["ok"] != (expected == 0 || expected == 5) {
        return Err(io::Error::other(format!(
            "CLI exit {code}, expected {expected}: {value}"
        )));
    }
    Ok(value)
}

fn raw_cli(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
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
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + CLI_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "CLI command exceeded deadline",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| io::Error::other("CLI stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| io::Error::other("CLI stderr reader panicked"))??;
    let value = serde_json::from_slice(&stdout).map_err(|_| {
        io::Error::other(format!("Invalid CLI JSON (stderr {} bytes)", stderr.len()))
    })?;
    Ok((status.code().unwrap_or(-1), value))
}

fn steam_resume(config: &Path, space: &str) -> io::Result<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args([
            "--json",
            "server",
            "steam",
            "resume",
            "--account",
            "default",
            "--game-space",
            space,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    command
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS");
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing Steam stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing Steam stderr"))?;
    let out = thread::spawn(move || read_bounded(stdout));
    let err = thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Prepared Steam resume exceeded deadline",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    let stdout = out
        .join()
        .map_err(|_| io::Error::other("Steam stdout reader panicked"))??;
    let stderr = err
        .join()
        .map_err(|_| io::Error::other("Steam stderr reader panicked"))??;
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| {
        io::Error::other(format!(
            "Steam resume returned invalid JSON (stderr {} bytes)",
            stderr.len()
        ))
    })?;
    if !status.success() || value["ok"] != true {
        return Err(io::Error::other("Prepared Steam resume failed"));
    }
    Ok(value)
}

fn read_bounded(mut reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > MAX_OUTPUT {
            return Err(io::Error::other("CLI output exceeded bound"));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn required_dir(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} must be set explicitly")))?;
    if !path.is_absolute() {
        return Err(io::Error::other(format!("{name} must be absolute")));
    }
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(io::Error::other(format!("{name} must name a directory")));
    }
    Ok(path)
}

fn optional_dir(name: &str) -> io::Result<Option<PathBuf>> {
    let Some(path) = std::env::var_os(name).map(PathBuf::from) else {
        return Ok(None);
    };
    if !path.is_absolute() {
        return Err(io::Error::other(format!("{name} must be absolute")));
    }
    let path = path.canonicalize()?;
    if !path.is_dir() {
        return Err(io::Error::other(format!("{name} must name a directory")));
    }
    Ok(Some(path))
}

fn required_id(name: &str) -> io::Result<String> {
    let value = std::env::var(name).map_err(io::Error::other)?;
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(io::Error::other(format!("{name} is invalid")));
    }
    Ok(value)
}

fn required_str<'a>(value: &'a Value, name: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}

fn required_number(value: &Value, name: &str) -> io::Result<String> {
    value
        .as_u64()
        .map(|number| number.to_string())
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}

struct Installation {
    id: String,
    exe: PathBuf,
    bytes: u64,
    blake3: String,
}

struct SaveBackup {
    config: PathBuf,
    space: String,
    instance: String,
    installation: String,
    original: PathBuf,
    backup: PathBuf,
    baseline: (u64, u64),
    original_manifest: std::collections::BTreeMap<String, (u64, String)>,
    delta: Option<Value>,
}

impl SaveBackup {
    fn prepare(
        config: &Path,
        space: &str,
        instance: &str,
        installation: &str,
        output: &Path,
    ) -> io::Result<Self> {
        let inventory = cli(
            config,
            &["server", "session", "list", instance, "--game-space", space],
            0,
        )?;
        let sessions = inventory
            .pointer("/data/inventory/sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| io::Error::other("Prepared Reforger session inventory was missing"))?;
        let baseline = sessions
            .iter()
            .filter(|item| item["compatible"] == true)
            .filter_map(|item| Some((item["playthrough"].as_u64()?, item["savepoint"].as_u64()?)))
            .max()
            .ok_or_else(|| {
                io::Error::other("Prepared Reforger runtime has no compatible savepoint")
            })?;
        let original = config
            .join("games/reforger/server/instances")
            .join(instance)
            .join("deployments/runtime-state/home/profiles/profile/.save");
        let original = original.canonicalize()?;
        if !original.is_dir() || original.starts_with(output) || output.starts_with(&original) {
            return Err(io::Error::other(
                "Reforger save tree and private output paths are unsafe",
            ));
        }
        let backup = output.join("foxy-service-save-before");
        if backup.exists() {
            return Err(io::Error::other(
                "Refusing to overwrite an existing save backup",
            ));
        }
        let original_manifest = tree_manifest(&original)?;
        copy_tree(&original, &backup)?;
        if tree_manifest(&backup)? != original_manifest {
            return Err(io::Error::other(
                "Private Reforger save backup did not match its source",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            config: config.to_owned(),
            space: space.to_owned(),
            instance: instance.to_owned(),
            installation: installation.to_owned(),
            original,
            backup,
            baseline,
            original_manifest,
            delta: None,
        })
    }

    fn verify_savepoint_and_restore(&mut self) -> io::Result<()> {
        let inventory = cli(
            &self.config,
            &[
                "server",
                "session",
                "list",
                &self.instance,
                "--game-space",
                &self.space,
            ],
            0,
        )?;
        let sessions = inventory
            .pointer("/data/inventory/sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                io::Error::other("Post-shutdown Reforger session inventory was missing")
            })?;
        let later = sessions
            .iter()
            .filter_map(|item| Some((item["playthrough"].as_u64()?, item["savepoint"].as_u64()?)))
            .any(|(playthrough, savepoint)| {
                playthrough > self.baseline.0
                    || (playthrough == self.baseline.0 && savepoint > self.baseline.1)
            });
        if !later {
            return Err(io::Error::other(
                "Foreground SIGTERM did not produce a later native Reforger savepoint",
            ));
        }
        self.delta = Some(serde_json::json!({
            "before_playthrough": self.baseline.0,
            "before_savepoint": self.baseline.1,
            "after_playthroughs": sessions.iter().filter_map(|item| item["playthrough"].as_u64()).max(),
            "later_savepoint_observed": true,
        }));
        self.restore_if_safe()
    }

    fn safe_to_restore(&self) -> bool {
        let owner = raw_cli(
            &self.config,
            &["server", "owner", "--game-space", &self.space],
        );
        if !owner.is_ok_and(|(code, _)| code == 4) {
            return false;
        }
        let consumers = cli(
            &self.config,
            &[
                "server",
                "installation",
                "consumers",
                &self.installation,
                "--game-space",
                &self.space,
            ],
            0,
        );
        consumers.is_ok_and(|value| {
            value.pointer("/data/consumer_inspection/inspection_complete")
                == Some(&Value::Bool(true))
                && value
                    .pointer("/data/consumer_inspection/recorded_consumers")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        })
    }

    fn restore_if_safe(&mut self) -> io::Result<()> {
        if !self.safe_to_restore() {
            return Err(io::Error::other(
                "Refusing to restore Reforger saves before owner and consumers exit",
            ));
        }
        match std::fs::symlink_metadata(&self.original) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(io::Error::other(
                    "Refusing to replace a symlinked Reforger save tree",
                ));
            }
            Ok(_) if tree_manifest(&self.original)? == self.original_manifest => return Ok(()),
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::other(
                    "Reforger save tree was replaced by a non-directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = self
            .original
            .parent()
            .ok_or_else(|| io::Error::other("Save path omitted parent"))?;
        let stage = parent.join(format!(
            ".foxy-service-restore-{:016x}",
            rand::random::<u64>()
        ));
        let displaced = parent.join(format!(
            ".foxy-service-after-{:016x}",
            rand::random::<u64>()
        ));
        if stage.exists() || displaced.exists() {
            return Err(io::Error::other("Save restore staging path already exists"));
        }
        copy_tree(&self.backup, &stage)?;
        if tree_manifest(&stage)? != self.original_manifest {
            return Err(io::Error::other(
                "Save restore staging copy failed byte verification",
            ));
        }
        let had_original = self.original.exists();
        if had_original {
            std::fs::rename(&self.original, &displaced)?;
        }
        if let Err(error) = std::fs::rename(&stage, &self.original) {
            if had_original {
                let _ = std::fs::rename(&displaced, &self.original);
            }
            return Err(error);
        }
        if had_original {
            std::fs::remove_dir_all(displaced)?;
        }
        if tree_manifest(&self.original)? != self.original_manifest {
            return Err(io::Error::other(
                "Reforger save tree did not restore exactly",
            ));
        }
        Ok(())
    }
}

impl Drop for SaveBackup {
    fn drop(&mut self) {
        if self.safe_to_restore() {
            let _ = self.restore_if_safe();
        }
    }
}

fn tree_manifest(root: &Path) -> io::Result<std::collections::BTreeMap<String, (u64, String)>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    let mut total = 0u64;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = std::fs::symlink_metadata(&path)?.file_type();
            if kind.is_symlink() {
                return Err(io::Error::other("Symlink in native save tree"));
            }
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() || files.len() >= 4096 {
                return Err(io::Error::other(
                    "Unsupported or excessive native save tree entry",
                ));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            let (bytes, hash) = hash_file(&path)?;
            total = total.saturating_add(bytes);
            if bytes > 512 * 1024 * 1024 || total > 2 * 1024 * 1024 * 1024 {
                return Err(io::Error::other("Native save tree exceeded its bound"));
            }
            files.insert(relative, (bytes, hash));
        }
    }
    Ok(files)
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::other("Symlink in native save tree"));
        }
        if metadata.is_dir() {
            copy_tree(&from, &to)?;
        } else if metadata.is_file() {
            std::fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::other("Unsupported native save tree entry"));
        }
        std::fs::set_permissions(&to, metadata.permissions())?;
    }
    let metadata = std::fs::metadata(source)?;
    std::fs::set_permissions(destination, metadata.permissions())?;
    Ok(())
}

fn installation_evidence(config: &Path, space: &str, id: &str) -> io::Result<Installation> {
    let listing = cli(
        config,
        &["server", "instance", "list", "--game-space", space],
        0,
    )?;
    let catalog = &listing["data"]["catalog"];
    let instance = catalog["instances"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == id))
        .ok_or_else(|| io::Error::other("Prepared native instance was absent from catalog"))?;
    let installation_id = required_str(&instance["installation_id"], "installation id")?;
    let installation = catalog["installations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == installation_id))
        .ok_or_else(|| io::Error::other("Prepared installation was absent from catalog"))?;
    let directory = PathBuf::from(required_str(
        &installation["directory"],
        "installation directory",
    )?)
    .canonicalize()?;
    let exe = directory
        .join(required_str(
            &installation["executable"],
            "installation executable",
        )?)
        .canonicalize()?;
    if !exe.starts_with(&directory) || !std::fs::symlink_metadata(&exe)?.file_type().is_file() {
        return Err(io::Error::other(
            "Native executable was not a regular file in its installation",
        ));
    }
    let (bytes, blake3) = hash_file(&exe)?;
    if installation["evidence"]["bytes"].as_u64() != Some(bytes)
        || installation["evidence"]["blake3"].as_str() != Some(blake3.as_str())
    {
        return Err(io::Error::other(
            "Native executable hash did not match prepared catalog",
        ));
    }
    Ok(Installation {
        id: installation_id.to_owned(),
        exe,
        bytes,
        blake3,
    })
}

fn verify_installation(installation: &Installation) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(&installation.exe)?;
    if !metadata.file_type().is_file() || metadata.len() != installation.bytes {
        return Err(io::Error::other(
            "Native executable size changed during acceptance",
        ));
    }
    let (bytes, hash) = hash_file(&installation.exe)?;
    if bytes != installation.bytes || hash != installation.blake3 {
        return Err(io::Error::other(
            "Native executable hash changed during acceptance",
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    let bytes = io::copy(&mut file.take(length.saturating_add(1)), &mut hasher)?;
    if bytes != length {
        return Err(io::Error::other("Executable changed while hashing"));
    }
    Ok((bytes, hasher.finalize().to_hex().to_string()))
}

fn wait_child_exit(child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(io::Error::other(format!(
                "Foreground service exited unsuccessfully: {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Service owner did not exit",
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "linux")]
fn signal_owner_pidfd(child: &mut Child) -> io::Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as libc::pid_t, 0) };
    if pidfd < 0 {
        return Err(io::Error::last_os_error());
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as i32) };
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            libc::SIGTERM,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn valid_manager_unit(unit: &str) -> bool {
    unit.strip_prefix("foxy-d33-")
        .and_then(|value| value.strip_suffix(".service"))
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 48
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

#[cfg(target_os = "linux")]
fn validate_installed_unit(unit: &str, definition: &str) -> io::Result<()> {
    let output = Command::new("systemctl")
        .args(["--user", "show", unit, "-p", "FragmentPath", "--value"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("Could not inspect private manager unit"));
    }
    let path = String::from_utf8(output.stdout)
        .map_err(io::Error::other)?
        .trim()
        .to_owned();
    let path = PathBuf::from(path).canonicalize()?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Unit path had no parent"))?
        .canonicalize()?;
    if !path.starts_with(&parent)
        || !std::fs::symlink_metadata(&path)?.file_type().is_file()
        || std::fs::read_to_string(path)? != definition
    {
        return Err(io::Error::other(
            "Manager unit did not exactly match the exported definition",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> io::Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other("User systemd command failed"))
    }
}

#[cfg(target_os = "linux")]
fn systemctl_show(unit: &str) -> io::Result<std::collections::HashMap<String, String>> {
    let output = Command::new("systemctl")
        .args([
            "--user",
            "show",
            unit,
            "-p",
            "ActiveState",
            "-p",
            "Type",
            "-p",
            "Result",
            "-p",
            "ExecMainStatus",
            "-p",
            "MainPID",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("Could not inspect user unit state"));
    }
    let values = String::from_utf8(output.stdout)
        .map_err(io::Error::other)?
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect::<std::collections::HashMap<_, _>>();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn manager_process_matches(unit: &str, executable: &Path, arguments: &[&str]) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let status = systemctl_show(unit)?;
    let Some(pid) = status
        .get("MainPID")
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return Ok(false);
    };
    let process = PathBuf::from("/proc").join(pid.to_string());
    if std::fs::metadata(&process)?.uid() != std::fs::metadata("/proc/self")?.uid()
        || std::fs::canonicalize(process.join("exe"))? != executable.canonicalize()?
    {
        return Ok(false);
    }
    let bytes = std::fs::read(process.join("cmdline"))?;
    if bytes.len() > 16 * 1024 || bytes.last() != Some(&0) {
        return Ok(false);
    }
    let values = bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .map(|part| std::str::from_utf8(part).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    Ok(values.len() == arguments.len() + 1
        && values[0] == executable.to_string_lossy()
        && values
            .iter()
            .skip(1)
            .map(String::as_str)
            .eq(arguments.iter().copied()))
}

struct Cleanup {
    config: PathBuf,
    space: String,
    instance: String,
    run: Option<String>,
    owner: Option<Value>,
    child: Option<Child>,
    #[cfg(target_os = "linux")]
    manager_unit: Option<String>,
    active: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.active
            || self
                .owner
                .as_ref()
                .is_some_and(|owner| !self.owns_current(owner))
        {
            return;
        }
        #[cfg(target_os = "linux")]
        if let Some(unit) = self.manager_unit.as_deref() {
            let _ = run_systemctl(&["stop", unit]);
            return;
        }
        if let Some(run) = self.run.as_deref() {
            let _ = raw_cli(
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
            );
        }
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| self.owns_current(owner))
        {
            let _ = raw_cli(
                &self.config,
                &[
                    "server",
                    "supervise",
                    "--shutdown",
                    "--yes",
                    "--game-space",
                    &self.space,
                ],
            );
        }
        if let Some(child) = &mut self.child {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

impl Cleanup {
    fn owns_current(&self, expected: &Value) -> bool {
        raw_cli(
            &self.config,
            &["server", "owner", "--game-space", &self.space],
        )
        .ok()
        .is_some_and(|(code, value)| {
            code == 0
                && value.pointer("/data/owner/scope/owner_id")
                    == expected.pointer("/scope/owner_id")
                && value.pointer("/data/owner/scope/generation")
                    == expected.pointer("/scope/generation")
        })
    }
}
