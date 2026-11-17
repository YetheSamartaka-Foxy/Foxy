use super::*;
use serde::Serialize;
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

const RCON_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const RCON_LISTEN: Duration = Duration::from_millis(2_000);

struct PermissionCase {
    name: &'static str,
    permission: &'static str,
    whitelist: &'static [&'static str],
    blacklist: &'static [&'static str],
    allowed_command: Option<&'static str>,
    denied_command: &'static str,
}

const CASES: &[PermissionCase] = &[
    PermissionCase {
        name: "monitor",
        permission: "monitor",
        whitelist: &[],
        blacklist: &[],
        allowed_command: Some("#players"),
        denied_command: "#restart",
    },
    PermissionCase {
        name: "admin_whitelist_players",
        permission: "admin",
        whitelist: &["players"],
        blacklist: &[],
        allowed_command: Some("#players"),
        denied_command: "#restart",
    },
    PermissionCase {
        name: "admin_blacklist_players",
        permission: "admin",
        whitelist: &[],
        blacklist: &["players"],
        allowed_command: None,
        denied_command: "#players",
    },
];

#[derive(Serialize)]
struct PermissionReport {
    schema_version: u32,
    probe: &'static str,
    result: &'static str,
    source_inventory_unchanged: bool,
    prepared_payload_inventory_unchanged: bool,
    source_inventory_before: Vec<TreeEntry>,
    source_inventory_after: Vec<TreeEntry>,
    payload_inventory_before: Vec<TreeEntry>,
    payload_inventory_after: Vec<TreeEntry>,
    cases: Vec<PermissionEvidence>,
    report_path: PathBuf,
}

#[derive(Serialize)]
struct PermissionEvidence {
    name: &'static str,
    permission: &'static str,
    whitelist: Vec<String>,
    blacklist: Vec<String>,
    endpoint: String,
    password_echoed: bool,
    allowed_command: Option<ProbeEvidence>,
    denied_command: ProbeEvidence,
    semantic_denial_observed: bool,
    denial_text: Vec<String>,
    startup_version_lines: usize,
    scenario_loads_before_probe: usize,
    scenario_loads_after_probe: usize,
    scenario_loads_after_cleanup: usize,
    process_relaunch_if_present: bool,
    process_exit_after_probe: Option<TreeExit>,
    containment_root_pid: u32,
    scenario_ready_before_probe: bool,
    a2s_observed_before_probe: bool,
    retained_consumer_before_probe: bool,
    stop_exit: Option<TreeExit>,
    cleanup_outcome: &'static str,
    consumer_released_after_wait: bool,
    inspected_log_files: Vec<String>,
    evidence_dir: PathBuf,
}

#[derive(Serialize)]
struct ProbeEvidence {
    command: String,
    login_accepted: bool,
    keepalive_replied: bool,
    cli_message: Option<String>,
    command_outcome: Option<String>,
    response: Option<String>,
    asynchronous_messages: Vec<String>,
    exit_code: i32,
    password_echoed: bool,
    cli_output_path: PathBuf,
}

pub(super) fn run() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other(
            "Native process containment is unavailable",
        ));
    }
    let source = input_directory("FOXY_REFORGER_NATIVE_SOURCE")?;
    let payload = input_directory("FOXY_REFORGER_PERMISSION_PAYLOAD")?;
    let scratch = input_directory("FOXY_REFORGER_NATIVE_SCRATCH")?;
    if source.starts_with(&scratch)
        || scratch.starts_with(&source)
        || payload == scratch
        || !payload.starts_with(&scratch)
        || scratch.starts_with(&payload)
        || source.starts_with(&payload)
        || payload.starts_with(&source)
    {
        return Err(io::Error::other(
            "Source, prepared payload, and private scratch must be separate",
        ));
    }
    #[cfg(target_os = "linux")]
    check_linux_prerequisites(&scratch)?;

    let root = tempfile::Builder::new()
        .prefix("reforger-permissions-")
        .tempdir_in(&scratch)?
        .keep();
    private_directory(&root)?;
    let source_locks = InstallationLocks::capture_in(&source, &root.join("source-locks"))?;
    let _source_runtime = source_locks.try_admit()?.try_runtime()?;
    let source_inventory_before = snapshot_tree(&source)?;
    let payload_inventory_before = snapshot_tree(&payload)?;
    if source_inventory_before != payload_inventory_before {
        return Err(io::Error::other(
            "Prepared permission payload does not exactly match the read-only source",
        ));
    }
    let executable = payload.join(executable_name());
    let project = payload.join("addons/data/ArmaReforger.gproj");
    if !executable.is_file() || !project.is_file() {
        return Err(io::Error::other(
            "Prepared payload is missing its dedicated executable or data project",
        ));
    }
    let registry = payload
        .parent()
        .ok_or_else(|| io::Error::other("Prepared payload has no owner root"))?
        .join("runtime-locks");
    let locks = InstallationLocks::capture_in(&payload, &registry)?;
    if !locks.inspect_consumers()?.is_empty() {
        return Err(io::Error::other(
            "Prepared native payload has unresolved consumers",
        ));
    }
    let mut cases = Vec::with_capacity(CASES.len());
    for (index, case) in CASES.iter().enumerate() {
        cases.push(run_case(
            &payload,
            &executable,
            &project,
            &locks,
            &root,
            case,
            index as u64 + 1,
        )?);
    }
    let source_inventory_after = snapshot_tree(&source)?;
    let payload_inventory_after = snapshot_tree(&payload)?;
    let source_inventory_unchanged = source_inventory_before == source_inventory_after;
    let prepared_payload_inventory_unchanged = payload_inventory_before == payload_inventory_after;
    let cases_observed = cases.len() == CASES.len()
        && cases.iter().zip(CASES).all(|(evidence, case)| {
            evidence.scenario_ready_before_probe
                && evidence.a2s_observed_before_probe
                && evidence.retained_consumer_before_probe
                && evidence.semantic_denial_observed
                && evidence.cleanup_outcome == "soft_stop_waited"
                && evidence.stop_exit.is_some_and(|exit| !exit.forced)
                && evidence.consumer_released_after_wait
                && evidence.scenario_loads_before_probe > 0
                && evidence.scenario_loads_before_probe == evidence.scenario_loads_after_probe
                && evidence.scenario_loads_after_probe == evidence.scenario_loads_after_cleanup
                && evidence.startup_version_lines == 1
                && !evidence.process_relaunch_if_present
                && evidence.process_exit_after_probe.is_none()
                && !evidence.password_echoed
                && evidence.denied_command.command == case.denied_command
                && evidence.denied_command.login_accepted
                && evidence.denied_command.keepalive_replied
                && evidence.allowed_command.as_ref().is_none_or(|probe| {
                    probe.login_accepted
                        && probe.keepalive_replied
                        && players_transcript_observed(probe)
                })
        });
    let report_path = root.join("report.json");
    let report = PermissionReport {
        schema_version: 1,
        probe: "reforger_native_rcon_permissions",
        result: if cases_observed
            && source_inventory_unchanged
            && prepared_payload_inventory_unchanged
        {
            "observed"
        } else {
            "incomplete"
        },
        source_inventory_unchanged,
        prepared_payload_inventory_unchanged,
        source_inventory_before,
        source_inventory_after,
        payload_inventory_before,
        payload_inventory_after,
        cases,
        report_path: report_path.clone(),
    };
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "Private native permission evidence: {}",
        report_path.display()
    );
    if report.result != "observed" {
        return Err(io::Error::other(
            "Native RCon permission denial or waited cleanup was not observed",
        ));
    }
    Ok(())
}

fn run_case(
    payload: &Path,
    executable: &Path,
    project: &Path,
    locks: &InstallationLocks,
    root: &Path,
    case: &PermissionCase,
    generation: u64,
) -> io::Result<PermissionEvidence> {
    let instance = create_instance(root, case.name)?;
    let cli_config = instance.root.join("foxy-config");
    fs::create_dir(&cli_config)?;
    private_directory(&cli_config)?;
    let mut games: Value =
        serde_json::from_str(include_str!("../../../examples/json/appdata/games.json"))?;
    games["active_game_space_id"] = "reforger".into();
    games["last_opened_game_space_id"] = "reforger".into();
    fs::write(
        cli_config.join("games.json"),
        serde_json::to_vec_pretty(&games)?,
    )?;

    let hostname = format!("FoxyRefPermission_{}_{}", case.name, fresh_token());
    let mut ports = HashSet::new();
    let bind_port = loopback_udp_port(&mut ports)?;
    let a2s_port = loopback_udp_port(&mut ports)?;
    let rcon_port = loopback_udp_port(&mut ports)?;
    let password = format!("fpx{}", fresh_token());
    let configuration = json!({
        "bindAddress": "127.0.0.1",
        "bindPort": bind_port,
        "a2s": {"address": "127.0.0.1", "port": a2s_port},
        "rcon": {
            "address": "127.0.0.1",
            "port": rcon_port,
            "password": password,
            "permission": case.permission,
            "maxClients": 4,
            "whitelist": case.whitelist,
            "blacklist": case.blacklist
        },
        "game": {
            "name": hostname,
            "maxPlayers": 4,
            "visible": false,
            "mods": [],
            "scenarioId": SCENARIO
        }
    });
    fs::write(&instance.config, serde_json::to_vec_pretty(&configuration)?)?;
    let stdout = File::create(instance.logs.join("engine.stdout.log"))?;
    let paths = RequestedPaths {
        payload: payload.to_path_buf(),
        config: instance.config.clone(),
        project: project.to_path_buf(),
        private_environment_roots: instance.private_environment_roots.clone(),
        profile_parent: instance.profile_parent.clone(),
        effective_profile: instance.effective_profile.clone(),
        download_parent: instance.download_parent.clone(),
        cache_with_separator: argument_path_with_trailing_separator(&instance.cache)?,
        logs: instance.logs.clone(),
    };
    let identity = SessionIdentity {
        owner_id: "native_reforger_rcon_permissions".into(),
        owner_generation: 1,
        game_space_id: "reforger".into(),
        instance_id: case.name.into(),
        operation_id: "rcon_permission_acceptance".into(),
        run_generation: generation,
    };
    let spec = CommandSpec {
        executable: PathBuf::from(argument_path(executable)?),
        directory: PathBuf::from(argument_path(payload)?),
        arguments: vec![
            "-config".into(),
            argument_path(&instance.config)?.into(),
            "-gproj".into(),
            argument_path(project)?.into(),
            "-profile".into(),
            argument_path(&instance.profile_parent)?.into(),
            "-addonDownloadDir".into(),
            argument_path(&instance.download_parent)?.into(),
            "-addonTempDir".into(),
            argument_path_with_trailing_separator(&instance.cache)?.into(),
            "-logsDir".into(),
            argument_path(&instance.logs)?.into(),
            "-nothrow".into(),
            "-maxFPS".into(),
            "30".into(),
            "-autoreload".into(),
            "disabled".into(),
        ],
        environment: private_environment(&instance.home, &instance.private_environment_roots),
    };
    let admission = locks.try_admit()?;
    let mut session = RetainedSession(ForegroundSession::spawn_controlled(
        identity.clone(),
        &spec,
        &stdout,
        admission,
        &AtomicBool::new(false),
        || Ok(()),
    )?);
    let pid = session.0.process_id();
    let endpoint = format!("127.0.0.1:{rcon_port}");
    let mut cursors = BTreeMap::new();
    let mut captured = String::new();
    let mut primary = PermissionPrimaryLog::new(instance.logs.join("console.log"));
    let readiness = RequestedPaths { ..paths };
    let mut ready = false;
    let mut a2s_observed = false;
    let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < startup_deadline {
        if session.0.observe()?.is_some() {
            break;
        }
        capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
        primary.capture()?;
        a2s_observed |= query_a2s_hostname(a2s_port).as_deref() == Some(hostname.as_str());
        ready = generation_proof(&captured, &hostname, &readiness).is_complete() && a2s_observed;
        if ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let scenario_loads_before_probe = scenario_load_count(&primary.output);
    let retained_consumer_before_probe = locks
        .inspect_consumers()?
        .iter()
        .any(|consumer| consumer.session == identity);
    let mut probe_index = 0;
    let allowed_command = if let Some(command) = case.allowed_command {
        probe_index += 1;
        let probe_dir = instance.root.join(format!("probe-{probe_index}"));
        fs::create_dir(&probe_dir)?;
        Some(run_probe(
            &cli_config,
            &probe_dir,
            &endpoint,
            &password,
            command,
        )?)
    } else {
        None
    };
    capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
    primary.capture()?;
    let denied_log_start = captured.len();
    probe_index += 1;
    let denied_dir = instance.root.join(format!("probe-{probe_index}"));
    fs::create_dir(&denied_dir)?;
    let denied_command = run_probe(
        &cli_config,
        &denied_dir,
        &endpoint,
        &password,
        case.denied_command,
    )?;
    capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
    primary.capture()?;
    let password_echoed_in_logs = captured.contains(&password);
    let sanitized_logs = captured.replace(&password, "[REDACTED]");
    save_permission_log(&instance.logs, &sanitized_logs)?;
    let denied_log_delta = captured.get(denied_log_start..).unwrap_or_default();
    let denial_text = denial_lines(&denied_command, denied_log_delta)
        .into_iter()
        .map(|line| line.replace(&password, "[REDACTED]"))
        .collect::<Vec<_>>();
    let semantic_denial_observed = !denial_text.is_empty();
    let process_exit = session.0.observe()?;
    let startup_version_lines = version_line_count(&primary.output);
    let scenario_loads_after_probe = scenario_load_count(&primary.output);
    let (stop_exit, cleanup_outcome) = wait_for_stop(&mut session)?;
    capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
    primary.capture()?;
    let scenario_loads_after_cleanup = scenario_load_count(&primary.output);
    save_permission_log(&instance.logs, &captured.replace(&password, "[REDACTED]"))?;
    fs::write(
        instance.logs.join("captured-primary-console.log"),
        primary.output.as_bytes(),
    )?;
    let consumer_released_after_wait = locks.inspect_consumers()?.is_empty();
    let password_echoed = password_echoed_in_logs
        || allowed_command
            .as_ref()
            .is_some_and(|probe| probe.password_echoed)
        || denied_command.password_echoed;
    Ok(PermissionEvidence {
        name: case.name,
        permission: case.permission,
        whitelist: case.whitelist.iter().map(|value| (*value).into()).collect(),
        blacklist: case.blacklist.iter().map(|value| (*value).into()).collect(),
        endpoint,
        password_echoed,
        allowed_command,
        denied_command,
        semantic_denial_observed,
        denial_text,
        startup_version_lines,
        scenario_loads_before_probe,
        scenario_loads_after_probe,
        scenario_loads_after_cleanup,
        process_relaunch_if_present: startup_version_lines > 1,
        process_exit_after_probe: process_exit,
        containment_root_pid: pid,
        scenario_ready_before_probe: ready,
        a2s_observed_before_probe: a2s_observed,
        retained_consumer_before_probe,
        stop_exit,
        cleanup_outcome,
        consumer_released_after_wait,
        inspected_log_files: cursors.keys().cloned().collect(),
        evidence_dir: instance.root,
    })
}

fn run_probe(
    config: &Path,
    evidence: &Path,
    endpoint: &str,
    password: &str,
    command_text: &str,
) -> io::Result<ProbeEvidence> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    let listen_ms = RCON_LISTEN.as_millis().to_string();
    let timeout_ms = RCON_PROBE_TIMEOUT.as_millis().to_string();
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args([
            "--json",
            "--yes",
            "server",
            "rcon",
            "probe",
            "--game-space",
            "reforger",
            "--endpoint",
            endpoint,
            "--password-stdin",
            "--command",
            command_text,
            "--listen-ms",
            &listen_ms,
            "--timeout-ms",
            &timeout_ms,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("RCon probe stdin was unavailable"))?
        .write_all(format!("{password}\n").as_bytes())?;
    let output = child.wait_with_output()?;
    let raw_stdout = String::from_utf8_lossy(&output.stdout);
    let raw_stderr = String::from_utf8_lossy(&output.stderr);
    let password_echoed = raw_stdout.contains(password) || raw_stderr.contains(password);
    let stdout = raw_stdout.replace(password, "[REDACTED]");
    let stderr = raw_stderr.replace(password, "[REDACTED]");
    let stdout_path = evidence.join("cli.stdout.json");
    fs::write(&stdout_path, stdout.as_bytes())?;
    fs::write(evidence.join("cli.stderr.txt"), stderr.as_bytes())?;
    let value: Value = serde_json::from_str(&stdout).map_err(|error| {
        io::Error::other(format!(
            "RCon probe returned invalid JSON (exit {}): {error}; evidence at {}",
            output.status.code().unwrap_or(-1),
            evidence.display()
        ))
    })?;
    let response = value["data"]["command"]["response"]
        .as_str()
        .map(str::to_owned);
    let asynchronous_messages = value["data"]["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["text"].as_str().map(str::to_owned))
        .collect();
    Ok(ProbeEvidence {
        command: command_text.into(),
        login_accepted: value["data"]["login"] == "accepted",
        keepalive_replied: value["data"]["keepalive"]["outcome"] == "replied",
        cli_message: value["message"].as_str().map(str::to_owned),
        command_outcome: value["data"]["command"]["outcome"]
            .as_str()
            .map(str::to_owned),
        response,
        asynchronous_messages,
        exit_code: output.status.code().unwrap_or(-1),
        password_echoed,
        cli_output_path: stdout_path,
    })
}

fn denial_lines(probe: &ProbeEvidence, logs: &str) -> Vec<String> {
    let mut texts = probe.asynchronous_messages.clone();
    let command_name = probe.command.trim_start_matches('#').to_ascii_lowercase();
    texts.extend(
        logs.lines()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains(&probe.command.to_ascii_lowercase())
                    || lower.contains(&command_name)
                    || lower.contains("rcon") && lower.contains("command")
            })
            .map(str::to_owned),
    );
    texts
        .into_iter()
        .filter(|text| missing_permission_observed(std::iter::once(text.as_str()), &probe.command))
        .collect()
}

fn missing_permission_observed<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    command: &str,
) -> bool {
    let command_name = command.trim_start_matches('#').to_ascii_lowercase();
    let expected = format!("missing permission '{command_name}'");
    texts
        .into_iter()
        .any(|text| text.to_ascii_lowercase().contains(&expected))
}

fn players_transcript_observed(probe: &ProbeEvidence) -> bool {
    let processing = format!("processing command: {}", probe.command.to_ascii_lowercase());
    let has_processing_boundary = probe
        .asynchronous_messages
        .iter()
        .any(|message| message.to_ascii_lowercase().contains(&processing));
    let has_player_list = probe
        .asynchronous_messages
        .iter()
        .any(|message| message.to_ascii_lowercase().contains("players on server:"));
    has_processing_boundary && has_player_list
}

fn version_line_count(log: &str) -> usize {
    log.lines()
        .filter(|line| {
            line.to_ascii_lowercase()
                .contains("initializing engine, version ")
        })
        .count()
}

fn scenario_load_count(log: &str) -> usize {
    log.lines()
        .filter(|line| {
            line.contains(SCENARIO_WORLD) && line.to_ascii_lowercase().contains("entities load")
        })
        .count()
}

struct PermissionPrimaryLog {
    path: PathBuf,
    previous: Vec<u8>,
    output: String,
}

impl PermissionPrimaryLog {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            previous: Vec::new(),
            output: String::new(),
        }
    }

    fn capture(&mut self) -> io::Result<()> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        let offset = if bytes.starts_with(&self.previous) {
            self.previous.len()
        } else {
            0
        };
        if self.output.len() + bytes.len() - offset > 32 * 1024 * 1024 {
            return Err(io::Error::other(
                "Native primary log exceeded capture limit",
            ));
        }
        self.output
            .push_str(&String::from_utf8_lossy(&bytes[offset..]));
        self.previous = bytes;
        Ok(())
    }
}

fn save_permission_log(root: &Path, captured: &str) -> io::Result<()> {
    fs::write(root.join("captured-permission.log"), captured.as_bytes())
}

fn wait_for_stop(session: &mut RetainedSession) -> io::Result<(Option<TreeExit>, &'static str)> {
    if let Some(exit) = session.0.event().exit {
        return Ok((Some(exit), "natural_exit_waited"));
    }
    #[cfg(windows)]
    let helper = ConsoleSignalHelper {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_Foxy")),
        arguments: vec![
            "server".into(),
            "console-signal".into(),
            "--ctrl-break".into(),
        ],
    };
    #[cfg(windows)]
    let helper = Some(&helper);
    #[cfg(target_os = "linux")]
    let helper = None;
    let _ = session.0.deliver_soft_stop(helper);
    if let Some(exit) = session.0.wait(SOFT_STOP_TIMEOUT)? {
        return Ok((Some(exit), "soft_stop_waited"));
    }
    if let Some(exit) = session.0.force_stop(FORCE_STOP_TIMEOUT)? {
        let outcome = if exit.forced {
            "forced_waited"
        } else {
            "waited_after_force_request"
        };
        return Ok((Some(exit), outcome));
    }
    Ok((None, "cleanup_uncertain"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(
        command: &str,
        asynchronous_messages: &[&str],
        response: Option<&str>,
    ) -> ProbeEvidence {
        ProbeEvidence {
            command: command.into(),
            login_accepted: true,
            keepalive_replied: true,
            cli_message: None,
            command_outcome: Some("replied".into()),
            response: response.map(str::to_owned),
            asynchronous_messages: asynchronous_messages
                .iter()
                .map(|message| (*message).into())
                .collect(),
            exit_code: 0,
            password_echoed: false,
            cli_output_path: PathBuf::new(),
        }
    }

    #[test]
    fn players_oracle_requires_native_processing_boundary_and_list() {
        let observed = probe(
            "#players",
            &["Processing Command: #players", "Players on server: 0"],
            Some(""),
        );
        assert!(players_transcript_observed(&observed));

        let ack_only = probe("#players", &[], Some(""));
        assert!(!players_transcript_observed(&ack_only));
        let missing_boundary = probe("#players", &["Players on server: 0"], None);
        assert!(!players_transcript_observed(&missing_boundary));
        let wrong_command = probe(
            "#players",
            &["Processing Command: #restart", "Players on server: 0"],
            None,
        );
        assert!(!players_transcript_observed(&wrong_command));
    }

    #[test]
    fn denial_oracle_requires_matching_native_permission_message() {
        assert!(missing_permission_observed(
            ["Server: missing permission 'restart'"],
            "#restart"
        ));
        assert!(!missing_permission_observed(
            ["Server: missing permission 'players'"],
            "#restart"
        ));
        assert!(!missing_permission_observed(
            ["permission denied"],
            "#restart"
        ));
        assert_eq!(
            denial_lines(
                &probe("#restart", &["missing permission 'restart'"], Some("")),
                ""
            ),
            ["missing permission 'restart'"]
        );
        assert!(denial_lines(&probe("#restart", &[], Some("")), "").is_empty());
    }

    #[test]
    fn primary_log_counts_one_stream_and_detects_replacement() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("console.log");
        let initial = format!(
            "ENGINE : Initializing engine, version 1\n{} entities load\n",
            SCENARIO_WORLD
        );
        fs::write(&path, &initial)?;
        let mut primary = PermissionPrimaryLog::new(path.clone());
        primary.capture()?;
        primary.capture()?;
        assert_eq!(version_line_count(&primary.output), 1);
        assert_eq!(scenario_load_count(&primary.output), 1);

        fs::write(&path, initial.replace("version 1", "version 2"))?;
        primary.capture()?;
        assert_eq!(version_line_count(&primary.output), 2);
        assert_eq!(scenario_load_count(&primary.output), 2);
        Ok(())
    }
}
