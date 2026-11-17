#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const OWNER_START_DEADLINE: Duration = Duration::from_secs(30);
const READY_DEADLINE: Duration = Duration::from_secs(300);
const SESSION_DEADLINE: Duration = Duration::from_secs(180);

struct Case {
    config: PathBuf,
    space: String,
    instance: String,
    port: u16,
    password: String,
}

#[test]
#[ignore = "Requires an explicitly prepared private native server configuration and runtime"]
fn native_rcon_sessions_authenticate_keep_alive_reconnect_and_never_resend() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_RCON_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_RCON_SPACE")?;
    let arma = match space.as_str() {
        "arma3" => true,
        "reforger" => false,
        _ => return Err(io::Error::other("RCon space must be arma3 or reforger")),
    };
    let instance = required_identifier("FOXY_NATIVE_RCON_INSTANCE")?;
    let output = required_directory("FOXY_NATIVE_RCON_OUTPUT")?;
    let port: u16 = std::env::var("FOXY_NATIVE_RCON_PORT")
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)?;
    let case = Case {
        config,
        space,
        instance,
        port,
        password: format!("fx{:024x}", rand::random::<u128>() >> 32),
    };
    let started_at = Instant::now();
    let mut evidence = Evidence::new(&output, &case.password);
    let mut report = Vec::new();
    let (code, _) = case.raw(&["server", "owner"], None)?;
    if code != 4 {
        return Err(io::Error::other(
            "Refusing to run with a pre-existing owner",
        ));
    }
    let executable_before = installation_hash(&case)?;

    let mut instance_json =
        case.ok(&["server", "instance", "show", &case.instance])?["data"]["instance"].clone();
    let mut revision = number(&instance_json["revision"], "draft revision")?;
    if arma && instance_json["definition"]["settings"]["battleye"] != true {
        instance_json["definition"]["settings"]["battleye"] = Value::Bool(true);
        let file = output.join("draft-battleye.json");
        fs::write(&file, serde_json::to_vec(&instance_json)?)?;
        case.ok(&[
            "server",
            "instance",
            "replace",
            "--file",
            path_arg(&file)?,
            "--expected-revision",
            &revision.to_string(),
        ])?;
        revision += 1;
    }
    let port = case.port.to_string();
    let mut configure = vec![
        "server",
        "rcon",
        "configure",
        &case.instance,
        "--expected-revision",
    ];
    let expected = revision.to_string();
    configure.push(&expected);
    configure.extend_from_slice(&["--port", &port]);
    if !arma {
        configure.extend_from_slice(&["--permission", "admin"]);
    }
    evidence.push(case.ok(&configure)?)?;
    revision += 1;
    let stored = case.ok_with_stdin(
        &[
            "server",
            "rcon",
            "set-password",
            &case.instance,
            "--expected-revision",
            &revision.to_string(),
        ],
        &format!("{}\n", case.password),
    )?;
    evidence.push(stored)?;
    revision += 1;
    redeploy(&case, revision, &mut evidence)?;
    let plan = case.ok(&["server", "instance", "start-plan", &case.instance])?;
    evidence.push(plan.clone())?;
    let launch = &plan["data"]["plan"]["launch"];
    let endpoint = format!("127.0.0.1:{}", case.port);
    if launch["rcon"]["endpoint"] != endpoint.as_str() {
        return Err(io::Error::other(format!(
            "Start plan has no RCon endpoint: {launch}"
        )));
    }
    let arguments: Vec<String> = launch["arguments"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if arma {
        if !arguments
            .iter()
            .any(|argument| argument.starts_with("-bepath=") && argument.contains("battleye"))
        {
            return Err(io::Error::other(
                "Arma start plan lacks the private -bepath",
            ));
        }
    } else if !arguments
        .iter()
        .any(|argument| argument.contains("private-configuration"))
    {
        return Err(io::Error::other(
            "Reforger start plan does not use the private server.json",
        ));
    }
    report.push(format!("committed RCon {endpoint} in revision {revision}"));

    let mut owner = start_parent_bound_owner(&case)?;
    wait_owner(&case, &mut owner)?;
    let mut cleanup = Cleanup {
        case: &case,
        run: None,
        active: true,
    };
    recover_leftovers(&case, &mut evidence)?;
    let run_1 = start_and_wait(&case, &mut evidence)?;
    cleanup.run = Some(run_1);
    let first = wait_session(&case, run_1, 1, &mut evidence)?;
    report.push(format!(
        "owner session connected for run {run_1} after {} failed attempts",
        number(&first["failed_attempts"], "failed attempts")?
    ));
    if arma {
        let bepath = case
            .config
            .join("games")
            .join(&case.space)
            .join("server/instances")
            .join(&case.instance)
            .join("deployments/runtime-state/home/battleye");
        let names: Vec<String> = fs::read_dir(&bepath)?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        if !names
            .iter()
            .any(|name| name.to_ascii_lowercase().starts_with("beserver_x64"))
        {
            return Err(io::Error::other(format!(
                "Private BattlEye state is empty: {names:?}"
            )));
        }
        report.push(format!("private BattlEye state {} entries", names.len()));
    }

    let tested = case.ok(&["server", "rcon", "test", &case.instance])?;
    evidence.push(tested.clone())?;
    let players = if arma { "players" } else { "#players" };
    let replied = case.ok(&[
        "--yes",
        "server",
        "rcon",
        "command",
        &case.instance,
        "--text",
        players,
    ])?;
    evidence.push(replied.clone())?;
    if replied["data"]["state"] != "replied" {
        return Err(io::Error::other(format!(
            "Owner command did not reply: {replied}"
        )));
    }
    if arma
        && !replied["data"]["record"]["response"]
            .as_str()
            .is_some_and(|text| text.contains("Players on server"))
    {
        return Err(io::Error::other(format!(
            "Unexpected players reply: {replied}"
        )));
    }
    report.push(format!(
        "owner command {players} replied ({} bytes), test {} ms",
        replied["data"]["record"]["response"]
            .as_str()
            .map(str::len)
            .unwrap_or(0),
        tested["data"]["round_trip_ms"]
    ));

    let (code, refused) = case.raw(
        &[
            "server",
            "rcon",
            "probe",
            "--endpoint",
            &endpoint,
            "--password-stdin",
        ],
        Some("wrong-password-value\n"),
    )?;
    evidence.push(refused.clone())?;
    if code != 4 || refused["data"]["login"] != "rejected" {
        return Err(io::Error::other(format!(
            "Wrong password was not refused: {refused}"
        )));
    }
    let direct = case.ok(&["server", "rcon", "probe", &case.instance])?;
    evidence.push(direct.clone())?;
    if direct["data"]["login"] != "accepted" || direct["data"]["keepalive"]["outcome"] != "replied"
    {
        return Err(io::Error::other(format!("Direct probe failed: {direct}")));
    }
    report.push("wrong password refused, protected-password probe accepted".into());

    let nonce = format!("FoxyLostAck{:08x}", rand::random::<u32>());
    let lost_text = if arma {
        format!("say -1 {nonce}")
    } else {
        "#players".to_owned()
    };
    let relay = Relay::start(
        endpoint.parse().map_err(io::Error::other)?,
        lost_text.as_bytes().to_vec(),
    )?;
    let (code, lost) = case.raw(
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--endpoint",
            &relay.address.to_string(),
            "--timeout-ms",
            "3000",
            "--command",
            &lost_text,
        ],
        None,
    )?;
    evidence.push(lost.clone())?;
    thread::sleep(Duration::from_millis(500));
    let transmissions = relay.matching.load(Ordering::Acquire);
    let dropped = relay.dropped.load(Ordering::Acquire);
    drop(relay);
    if code != 5 || lost["data"]["command"]["outcome"] != "unknown" {
        return Err(io::Error::other(format!(
            "A lost reply was not unknown: {lost}"
        )));
    }
    if transmissions != 1 || dropped == 0 {
        return Err(io::Error::other(format!(
            "Lost command transmissions {transmissions}, dropped replies {dropped}"
        )));
    }
    if arma {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = session(&case)?;
            if status["messages"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(&nonce))
                })
            }) {
                evidence.push(status)?;
                break;
            }
            if Instant::now() >= deadline {
                evidence.push(status)?;
                return Err(io::Error::other(
                    "The owner session never saw the effect of the unacknowledged command",
                ));
            }
            thread::sleep(Duration::from_millis(300));
        }
        report
            .push("lost reply unknown, sent once, its effect observed by the owner session".into());
    } else {
        report.push("lost reply unknown and sent once".into());
    }

    let before_idle = session(&case)?;
    let generation = number(&before_idle["session_generation"], "session generation")?;
    let acknowledged = number(&before_idle["messages_acknowledged"], "acknowledged")?;
    thread::sleep(Duration::from_secs(70));
    let after_idle = session(&case)?;
    evidence.push(after_idle.clone())?;
    if after_idle["connection"] != "connected"
        || number(&after_idle["session_generation"], "session generation")? != generation
    {
        return Err(io::Error::other(format!(
            "The idle session did not survive by keepalive: {after_idle}"
        )));
    }
    case.ok(&["server", "rcon", "test", &case.instance])?;
    report.push(format!(
        "session {generation} kept alive across 70 s idle; {} server messages acknowledged",
        number(&after_idle["messages_acknowledged"], "acknowledged")?.max(acknowledged)
    ));

    if !arma {
        let logout = case.raw(
            &[
                "--yes",
                "server",
                "rcon",
                "command",
                &case.instance,
                "--text",
                "@logout",
            ],
            None,
        )?;
        evidence.push(logout.1.clone())?;
        let reconnected = wait_session(&case, run_1, generation + 1, &mut evidence)?;
        report.push(format!(
            "@logout ended session {generation}; reconnected as session {}",
            reconnected["session_generation"]
        ));
    }

    stop(&case, run_1, &mut evidence)?;
    cleanup.run = None;
    let run_2 = start_and_wait(&case, &mut evidence)?;
    cleanup.run = Some(run_2);
    let renewed = wait_session(&case, run_2, 1, &mut evidence)?;
    if number(&renewed["run_generation"], "run generation")? != run_2 {
        return Err(io::Error::other(
            "The new run did not get its own RCon session",
        ));
    }
    report.push(format!("server restart {run_1}->{run_2} reconnected RCon"));

    let silent = UdpSocket::bind("127.0.0.1:0")?;
    let silent_endpoint = silent.local_addr()?.to_string();
    let (code, no_reply) = case.raw(
        &[
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--endpoint",
            &silent_endpoint,
            "--timeout-ms",
            "2000",
        ],
        None,
    )?;
    evidence.push(no_reply.clone())?;
    drop(silent);
    let closed = UdpSocket::bind("127.0.0.1:0")?.local_addr()?.to_string();
    let (closed_code, unreachable) = case.raw(
        &[
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--endpoint",
            &closed,
            "--timeout-ms",
            "2000",
        ],
        None,
    )?;
    evidence.push(unreachable.clone())?;
    if code != 4
        || no_reply["data"]["login"] != "no_reply"
        || closed_code != 4
        || !matches!(
            unreachable["data"]["login"].as_str(),
            Some("unreachable" | "no_reply")
        )
    {
        return Err(io::Error::other(format!(
            "Timeouts were not reported: {no_reply} {unreachable}"
        )));
    }
    report.push(format!(
        "silent endpoint no_reply, closed port {}",
        unreachable["data"]["login"]
    ));

    stop(&case, run_2, &mut evidence)?;
    cleanup.run = None;
    let history = case.ok(&["server", "rcon", "history", &case.instance])?;
    evidence.push(history.clone())?;
    let records = history["data"]["history"]["records"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if records
        .iter()
        .any(|record| matches!(record["state"].as_str(), Some("queued" | "sending")))
        || !records
            .iter()
            .any(|record| record["text"] == players && record["state"] == "replied")
    {
        return Err(io::Error::other(format!(
            "Unexpected RCon history: {history}"
        )));
    }
    cleanup.active = false;
    let shutdown = case.ok(&["server", "supervise", "--shutdown", "--yes"])?;
    evidence.push(shutdown)?;
    drop(owner);
    if installation_hash(&case)? != executable_before {
        return Err(io::Error::other("The adopted executable changed"));
    }
    evidence.finish()?;
    println!(
        "Native RCon acceptance passed for {} in {:.1}s: {}",
        case.space,
        started_at.elapsed().as_secs_f64(),
        report.join("; ")
    );
    Ok(())
}

#[test]
#[ignore = "Requires a prepared private Arma 3 native runtime with protected RCon"]
fn native_arma_administration_command_catalog() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_RCON_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_RCON_SPACE")?;
    if space != "arma3" {
        return Err(io::Error::other(
            "Administration catalog requires the arma3 space",
        ));
    }
    let redaction_sentinel = format!("fx{:024x}", rand::random::<u128>() >> 32);
    let case = Case {
        config,
        space,
        instance: required_identifier("FOXY_NATIVE_RCON_INSTANCE")?,
        port: std::env::var("FOXY_NATIVE_RCON_PORT")
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?,
        password: redaction_sentinel.clone(),
    };
    let output = required_directory("FOXY_NATIVE_RCON_OUTPUT")?;
    let executable_before = installation_hash(&case)?;
    let mut evidence = Evidence::new(&output, &redaction_sentinel);
    let (code, _) = case.raw(&["server", "owner"], None)?;
    if code != 4 {
        return Err(io::Error::other(
            "Refusing to run with a pre-existing owner",
        ));
    }
    let shown = case.ok(&["server", "instance", "show", &case.instance])?;
    let instance = &shown["data"]["instance"];
    if instance["definition"]["settings"]["rcon"]["port"] != case.port {
        return Err(io::Error::other(
            "Prepared Arma RCon port does not match the supplied port",
        ));
    }
    let mission_template = string(
        &instance["definition"]["settings"]["mission_cycle"][0]["template"],
        "prepared mission template",
    )?;
    if mission_template != "foxy_hc.vr" {
        return Err(io::Error::other(
            "Prepared runtime mission is not foxy_hc.vr",
        ));
    }
    let plan = case.ok(&["server", "instance", "start-plan", &case.instance])?;
    if plan["data"]["plan"]["launch"]["rcon"]["endpoint"] != format!("127.0.0.1:{}", case.port) {
        return Err(io::Error::other(
            "Prepared runtime does not use loopback RCon",
        ));
    }

    let mut owner = start_parent_bound_owner(&case)?;
    wait_owner(&case, &mut owner)?;
    let mut cleanup = Cleanup {
        case: &case,
        run: None,
        active: true,
    };
    recover_leftovers(&case, &mut evidence)?;
    let run = start_and_wait(&case, &mut evidence)?;
    cleanup.run = Some(run);
    wait_session(&case, run, 1, &mut evidence)?;

    let nonce = format!("FoxyCatalog{:08x}", rand::random::<u32>());
    let commands = ["version", "players", "bans", "missions"];
    let mut roster = None;
    for command in commands {
        let (code, result) = case.raw(
            &[
                "--yes",
                "server",
                "rcon",
                "probe",
                &case.instance,
                "--command",
                command,
            ],
            None,
        )?;
        evidence
            .push(serde_json::json!({"command": command, "exit_code": code, "result": result}))?;
        if command == "players" {
            roster = result["data"]["command"]["response"]
                .as_str()
                .map(str::to_owned);
        }
    }
    for command in [format!("say -1 {nonce}"), "#lock".into(), "#unlock".into()] {
        let (code, result) = case.raw(
            &[
                "--yes",
                "server",
                "rcon",
                "probe",
                &case.instance,
                "--command",
                &command,
            ],
            None,
        )?;
        evidence
            .push(serde_json::json!({"command": command, "exit_code": code, "result": result}))?;
    }

    let guid = "0123456789abcdef0123456789abcdef";
    let ban_command = format!("addBan {guid} 1 {nonce}");
    let (code, added) = case.raw(
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--command",
            &ban_command,
        ],
        None,
    )?;
    evidence
        .push(serde_json::json!({"command": ban_command, "exit_code": code, "result": added}))?;
    if added["data"]["command"]["outcome"] == "replied" {
        let (show_code, bans) = case.raw(
            &[
                "--yes",
                "server",
                "rcon",
                "probe",
                &case.instance,
                "--command",
                "bans",
            ],
            None,
        )?;
        evidence
            .push(serde_json::json!({"command": "bans", "exit_code": show_code, "result": bans}))?;
        let response = bans["data"]["command"]["response"]
            .as_str()
            .unwrap_or_default();
        let index = response.lines().find_map(|line| {
            (line.contains(guid) && line.contains(&nonce))
                .then(|| {
                    line.split_whitespace()
                        .next()?
                        .trim_matches(['[', '#', ':'])
                        .parse::<u32>()
                        .ok()
                })
                .flatten()
        });
        if let Some(index) = index {
            let remove = format!("removeBan {index}");
            let (remove_code, removed) = case.raw(
                &[
                    "--yes",
                    "server",
                    "rcon",
                    "probe",
                    &case.instance,
                    "--command",
                    &remove,
                ],
                None,
            )?;
            evidence.push(
                serde_json::json!({"command": remove, "exit_code": remove_code, "result": removed}),
            )?;
            if removed["data"]["command"]["outcome"] == "replied" {
                let (verify_code, verified) = case.raw(
                    &[
                        "--yes",
                        "server",
                        "rcon",
                        "probe",
                        &case.instance,
                        "--command",
                        "bans",
                    ],
                    None,
                )?;
                let absent = !verified["data"]["command"]["response"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(guid);
                evidence.push(serde_json::json!({"command": "bans", "exit_code": verify_code, "result": verified, "synthetic_ban_absent": absent}))?;
                if verify_code != 0 || !absent {
                    return Err(io::Error::other(
                        "Synthetic ban removal was not independently verified",
                    ));
                }
            } else {
                evidence.push(serde_json::json!({"limitation": "Synthetic ban removal outcome was not acknowledged; no retry was sent"}))?;
                return Err(io::Error::other(
                    "Synthetic ban removal outcome was not acknowledged",
                ));
            }
        } else {
            evidence.push(serde_json::json!({"limitation": "Could not strictly parse the exact synthetic ban index; no removeBan command was sent"}))?;
            return Err(io::Error::other(
                "Could not parse the exact synthetic ban index for removal",
            ));
        }
    } else {
        evidence.push(serde_json::json!({"limitation": "Synthetic addBan outcome was not acknowledged; no retry was sent"}))?;
        return Err(io::Error::other(
            "Synthetic addBan outcome was not acknowledged",
        ));
    }

    if let Some(roster) = roster {
        let loopback_headless = roster.lines().find_map(|line| {
            let lower = line.to_ascii_lowercase();
            if !line.contains("127.0.0.1") || !lower.contains("(ok) headlessclient") {
                return None;
            }
            line.split_whitespace()
                .next()?
                .strip_suffix(':')?
                .parse::<u32>()
                .ok()
                .map(|id| id.to_string())
        });
        if let Some(target) = loopback_headless {
            let command = format!("#kick {target}");
            let (code, result) = case.raw(
                &[
                    "--yes",
                    "server",
                    "rcon",
                    "probe",
                    &case.instance,
                    "--command",
                    &command,
                ],
                None,
            )?;
            evidence.push(serde_json::json!({"command": command, "exit_code": code, "result": result, "semantic_success_asserted": false}))?;
        } else {
            evidence.push(serde_json::json!({"limitation": "Roster contained no unambiguous loopback headless client; kick was not sent"}))?;
        }
    }

    let (mission_code, mission) = case.raw(
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--command",
            &format!("#mission {mission_template}"),
        ],
        None,
    )?;
    evidence.push(serde_json::json!({"command": "#mission foxy_hc.vr", "exit_code": mission_code, "result": mission}))?;
    let (mission_monitor_code, mission_monitor) =
        case.raw(&["server", "instance", "monitor", &case.instance], None)?;
    evidence.push(serde_json::json!({"after": "#mission", "exit_code": mission_monitor_code, "result": mission_monitor}))?;
    let (restart_code, restart) = case.raw(
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--command",
            "#restart",
        ],
        None,
    )?;
    evidence.push(
        serde_json::json!({"command": "#restart", "exit_code": restart_code, "result": restart}),
    )?;
    let (restart_monitor_code, restart_monitor) =
        case.raw(&["server", "instance", "monitor", &case.instance], None)?;
    evidence.push(serde_json::json!({"after": "#restart", "exit_code": restart_monitor_code, "result": restart_monitor}))?;
    let (shutdown_code, shutdown) = case.raw(
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            &case.instance,
            "--command",
            "#shutdown",
        ],
        None,
    )?;
    evidence.push(
        serde_json::json!({"command": "#shutdown", "exit_code": shutdown_code, "result": shutdown}),
    )?;
    cleanup.run = None;
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut exited = false;
    while Instant::now() < deadline {
        let (status_code, status) =
            case.raw(&["server", "instance", "status", &case.instance], None)?;
        evidence.push(status.clone())?;
        if status_code == 0
            && status
                .pointer("/data/status/run_generation")
                .and_then(Value::as_u64)
                == Some(run)
            && status.pointer("/data/status/phase").and_then(Value::as_str) == Some("exited")
            && status
                .pointer("/data/status/cleanup_verified")
                .and_then(Value::as_bool)
                == Some(true)
            && status
                .pointer("/data/status/process_exists")
                .and_then(Value::as_bool)
                == Some(false)
            && status
                .pointer("/data/status/headless_running")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                == 0
        {
            exited = true;
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    if !exited {
        return Err(io::Error::other(
            "Exact native run did not reach Exited after #shutdown",
        ));
    }
    case.ok(&["server", "supervise", "--shutdown", "--yes"])?;
    cleanup.active = false;
    drop(owner);
    if installation_hash(&case)? != executable_before {
        return Err(io::Error::other("The adopted executable changed"));
    }
    evidence.finish()?;
    Ok(())
}

#[test]
#[ignore = "Requires a prepared private Arma 3 runtime with joined headless clients and protected RCon"]
fn native_arma_admin_and_maintenance_preserve_owned_effects() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_RCON_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_RCON_SPACE")?;
    if space != "arma3" {
        return Err(io::Error::other(
            "Typed Arma administration requires the arma3 space",
        ));
    }
    let output = required_directory("FOXY_NATIVE_RCON_OUTPUT")?;
    let case = Case {
        config,
        space,
        instance: required_identifier("FOXY_NATIVE_RCON_INSTANCE")?,
        port: std::env::var("FOXY_NATIVE_RCON_PORT")
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?,
        password: format!("fx{:024x}", rand::random::<u128>() >> 32),
    };
    let executable_before = installation_hash(&case)?;
    let artifact_dir = output.join(format!("admin-maintenance-{:016x}", rand::random::<u64>()));
    fs::create_dir(&artifact_dir)?;
    let mut evidence = Evidence::new(&artifact_dir, &case.password);
    let nonce = format!("FoxyAdmin{:08x}", rand::random::<u32>());
    let (owner_code, _) = case.raw(&["server", "owner"], None)?;
    if owner_code != 4 {
        return Err(io::Error::other(
            "Refusing to run with a pre-existing owner",
        ));
    }

    let mut owner = start_parent_bound_owner(&case)?;
    wait_owner(&case, &mut owner)?;
    let mut cleanup = Cleanup {
        case: &case,
        run: None,
        active: true,
    };
    let mut owned_jobs = AdminJobCleanup::new(&case);
    recover_leftovers(&case, &mut evidence)?;
    let run = start_and_wait(&case, &mut evidence)?;
    cleanup.run = Some(run);
    wait_session(&case, run, 1, &mut evidence)?;

    let joined_client = wait_joined_headless(&case, run, &mut evidence)?;
    let (players_code, players) = typed_admin(&case, "players", run, &[], false)?;
    evidence.push(
        serde_json::json!({"typed_admin": "players", "exit_code": players_code, "result": players}),
    )?;
    if players_code != 0 || players.pointer("/data/parsed/complete") != Some(&Value::Bool(true)) {
        return Err(io::Error::other(format!(
            "Typed player roster was incomplete: {players}"
        )));
    }
    let roster = players["data"]["parsed"]["players"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let selected = roster
        .iter()
        .find(|player| {
            player["address"]
                .as_str()
                .is_some_and(|address| address.starts_with("127.0.0.1"))
                && player["verified"] == true
                && player["guid"].as_str().is_some()
        })
        .ok_or_else(|| {
            io::Error::other("No verified loopback player was available for guarded kick refusal")
        })?;
    if roster.is_empty() {
        return Err(io::Error::other(
            "Typed player roster was empty before the kick probe",
        ));
    }
    evidence.push(
        serde_json::json!({"joined_headless_client": joined_client, "loopback_player": selected}),
    )?;

    for command in ["bans", "missions"] {
        let (code, result) = typed_admin(&case, command, run, &[], false)?;
        evidence.push(
            serde_json::json!({"typed_admin": command, "exit_code": code, "result": result}),
        )?;
        if code != 0 || result.pointer("/data/parsed/complete") != Some(&Value::Bool(true)) {
            return Err(io::Error::other(format!(
                "Typed {command} list was incomplete: {result}"
            )));
        }
    }

    let player_id = number(&selected["player_id"], "loopback player id")?.to_string();
    let current_guid = string(&selected["guid"], "loopback player GUID")?;
    let wrong_guid = wrong_guid(current_guid);
    let (kick_code, wrong_kick) = typed_admin(
        &case,
        "kick",
        run,
        &[
            "--player-id",
            &player_id,
            "--expected-guid",
            &wrong_guid,
            "--reason",
            &nonce,
        ],
        true,
    )?;
    evidence.push(serde_json::json!({"typed_admin": "kick_wrong_guid", "exit_code": kick_code, "result": wrong_kick}))?;
    if kick_code == 0 || wrong_kick["data"]["state"] != "not_sent" {
        return Err(io::Error::other(format!(
            "Wrong-GUID kick was not refused: {wrong_kick}"
        )));
    }
    let (unknown_kick_code, unknown_kick) = typed_admin(
        &case,
        "kick",
        run,
        &[
            "--player-id",
            "4294967295",
            "--expected-guid",
            "00000000000000000000000000000000",
            "--reason",
            &nonce,
        ],
        true,
    )?;
    evidence.push(serde_json::json!({"typed_admin": "kick_unknown_target", "exit_code": unknown_kick_code, "result": unknown_kick}))?;
    if unknown_kick_code == 0 || unknown_kick["data"]["state"] != "not_sent" {
        return Err(io::Error::other(format!(
            "Unknown kick target was not refused: {unknown_kick}"
        )));
    }
    let (players_after_kick_code, players_after_kick) =
        typed_admin(&case, "players", run, &[], false)?;
    evidence.push(serde_json::json!({"typed_admin": "players_after_refused_kicks", "exit_code": players_after_kick_code, "result": players_after_kick}))?;
    let player_still_present = players_after_kick["data"]["parsed"]["players"]
        .as_array()
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item["player_id"] == selected["player_id"])
        });
    if players_after_kick_code != 0 || !player_still_present {
        return Err(io::Error::other(
            "The protected loopback player disappeared after refused kicks",
        ));
    }

    let synthetic_guid = "0123456789abcdef0123456789abcdef";
    let (initial_bans_code, initial_bans) = typed_admin(&case, "bans", run, &[], false)?;
    evidence.push(serde_json::json!({"typed_admin": "initial_bans", "exit_code": initial_bans_code, "result": initial_bans}))?;
    if initial_bans_code != 0
        || initial_bans.pointer("/data/parsed/complete") != Some(&Value::Bool(true))
        || initial_bans["data"]["parsed"]["bans"]
            .as_array()
            .is_some_and(|items| items.iter().any(|ban| ban["guid"] == synthetic_guid))
    {
        return Err(io::Error::other(
            "Refusing synthetic-ban test because the initial complete list already contains its GUID",
        ));
    }
    let (add_code, added) = typed_admin(
        &case,
        "add-ban",
        run,
        &[
            "--guid",
            synthetic_guid,
            "--minutes",
            "1",
            "--reason",
            &nonce,
        ],
        true,
    )?;
    evidence.push(serde_json::json!({"typed_admin": "add_synthetic_ban", "exit_code": add_code, "result": added}))?;
    if add_code != 0 || added["data"]["state"] != "replied" {
        return Err(io::Error::other(format!(
            "Synthetic ban command was not acknowledged: {added}"
        )));
    }
    let (bans_code, bans) = typed_admin(&case, "bans", run, &[], false)?;
    evidence.push(serde_json::json!({"typed_admin": "bans_after_add", "exit_code": bans_code, "result": bans}))?;
    let synthetic = bans["data"]["parsed"]["bans"]
        .as_array()
        .and_then(|items| items.iter().find(|ban| ban["guid"] == synthetic_guid))
        .cloned()
        .ok_or_else(|| {
            io::Error::other(format!(
                "Synthetic GUID did not appear in complete bans output: {bans}"
            ))
        })?;
    if bans_code != 0 || bans.pointer("/data/parsed/complete") != Some(&Value::Bool(true)) {
        return Err(io::Error::other(
            "The ban list after insertion was not complete",
        ));
    }
    let ban_id = number(&synthetic["ban_id"], "synthetic ban id")?.to_string();
    let (wrong_remove_code, wrong_remove) = typed_admin(
        &case,
        "remove-ban",
        run,
        &[
            "--ban-id",
            &ban_id,
            "--expected-guid",
            "ffffffffffffffffffffffffffffffff",
        ],
        true,
    )?;
    evidence.push(serde_json::json!({"typed_admin": "remove_synthetic_ban_wrong_guid", "exit_code": wrong_remove_code, "result": wrong_remove}))?;
    if wrong_remove_code == 0 || wrong_remove["data"]["state"] != "not_sent" {
        return Err(io::Error::other(format!(
            "Wrong-GUID ban removal was not refused: {wrong_remove}"
        )));
    }
    let (ban_after_refusal_code, ban_after_refusal) = typed_admin(&case, "bans", run, &[], false)?;
    evidence.push(serde_json::json!({"typed_admin": "bans_after_refused_remove", "exit_code": ban_after_refusal_code, "result": ban_after_refusal}))?;
    if ban_after_refusal_code != 0
        || !ban_after_refusal["data"]["parsed"]["bans"]
            .as_array()
            .is_some_and(|items| items.iter().any(|ban| ban["guid"] == synthetic_guid))
    {
        return Err(io::Error::other(
            "The synthetic ban was not preserved after guarded removal refusal",
        ));
    }
    let (remove_code, removed) = typed_admin(
        &case,
        "remove-ban",
        run,
        &["--ban-id", &ban_id, "--expected-guid", synthetic_guid],
        true,
    )?;
    evidence.push(serde_json::json!({"typed_admin": "remove_exact_synthetic_ban", "exit_code": remove_code, "result": removed}))?;
    if remove_code != 0 || removed["data"]["state"] != "replied" {
        return Err(io::Error::other(format!(
            "Exact synthetic ban removal was not acknowledged: {removed}"
        )));
    }
    let (verify_bans_code, verify_bans) = typed_admin(&case, "bans", run, &[], false)?;
    let synthetic_absent = !verify_bans["data"]["parsed"]["bans"]
        .as_array()
        .is_some_and(|items| items.iter().any(|ban| ban["guid"] == synthetic_guid));
    evidence.push(serde_json::json!({"typed_admin": "verify_synthetic_ban_absent", "exit_code": verify_bans_code, "synthetic_ban_absent": synthetic_absent, "result": verify_bans}))?;
    if verify_bans_code != 0
        || !synthetic_absent
        || verify_bans.pointer("/data/parsed/complete") != Some(&Value::Bool(true))
    {
        return Err(io::Error::other(
            "Synthetic ban removal was not independently verified",
        ));
    }

    let (fresh_status_code, fresh_status) =
        case.raw(&["server", "instance", "status", &case.instance], None)?;
    evidence.push(serde_json::json!({"before_stale_generation": fresh_status}))?;
    let stale_generation = (run + 1).to_string();
    let (stale_code, stale) = typed_admin(&case, "lock", run + 1, &[], true)?;
    evidence.push(serde_json::json!({"typed_admin": "stale_run_lock", "expected_run_generation": stale_generation, "exit_code": stale_code, "result": stale}))?;
    if fresh_status_code != 0 || stale_code == 0 {
        return Err(io::Error::other("A stale run generation was not refused"));
    }
    let (history_code, history) = case.raw(&["server", "rcon", "history", &case.instance], None)?;
    evidence.push(history.clone())?;
    if history_code != 0
        || history["data"]["history"]["records"]
            .as_array()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|record| record["run_generation"] == run + 1 && record["text"] == "#lock")
            })
    {
        return Err(io::Error::other(
            "Stale-generation lock created an RCon command record",
        ));
    }

    let (logs_before_code, logs_before) =
        case.raw(&["server", "instance", "logs", &case.instance], None)?;
    if logs_before_code != 0 {
        return Err(io::Error::other(
            "The current-run logs were unavailable before the lock check",
        ));
    }
    evidence.push(logs_before.clone())?;
    let before_sequence = number(
        &logs_before["data"]["logs"]["next_sequence"],
        "log sequence",
    )?;
    let hc_before_lock = headless_status(&case)?;
    evidence.push(hc_before_lock.clone())?;
    let stopped_hc = hc_before_lock["data"]["clients"]
        .as_array()
        .and_then(|clients| {
            clients
                .iter()
                .find(|client| client["wanted"] == false && client["state"] == "stopped")
                .or_else(|| (clients.len() > 1).then(|| clients.last()).flatten())
        })
        .and_then(|client| client["id"].as_str())
        .map(str::to_owned);
    if stopped_hc.is_none() {
        return Err(io::Error::other(format!(
            "Prepared run needs a second HC for the operator-stop and lock witness: {}",
            hc_before_lock["data"]["clients"]
        )));
    }
    if let Some(hc_id) = &stopped_hc {
        evidence.push(case.ok(&[
            "--yes",
            "server",
            "headless",
            "stop",
            &case.instance,
            "--expected-run-generation",
            &run.to_string(),
            "--client",
            hc_id,
            "--timeout-ms",
            "30000",
        ])?)?;
        wait_operator_stopped_headless(&case, hc_id, &mut evidence)?;
    }
    let (lock_code, locked) = typed_admin(&case, "lock", run, &[], true)?;
    evidence.push(
        serde_json::json!({"typed_admin": "lock", "exit_code": lock_code, "result": locked}),
    )?;
    if lock_code != 0 || locked["data"]["state"] != "replied" {
        return Err(io::Error::other(format!(
            "Lock command did not receive a reply: {locked}"
        )));
    }
    let lock_log = wait_log_evidence(
        &case,
        run,
        before_sequence,
        &["locked", "server locked"],
        &mut evidence,
    )?;
    let mut hc_joined_while_locked = None;
    let mut hc_start_succeeded = false;
    if let Some(hc_id) = &stopped_hc {
        let (start_code, start) = case.raw(
            &[
                "--yes",
                "server",
                "headless",
                "start",
                &case.instance,
                "--expected-run-generation",
                &run.to_string(),
                "--client",
                hc_id,
            ],
            None,
        )?;
        evidence.push(serde_json::json!({"headless_start_while_locked": hc_id, "exit_code": start_code, "result": start}))?;
        let after_start = headless_status(&case)?;
        let client_started = after_start["data"]["clients"]
            .as_array()
            .is_some_and(|clients| {
                clients.iter().any(|client| {
                    client["id"] == hc_id.as_str()
                        && (client["wanted"] == true || client["state"] != "stopped")
                })
            });
        evidence.push(after_start)?;
        if start_code == 0 || client_started {
            hc_start_succeeded = true;
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                let status = headless_status(&case)?;
                let joined = status["data"]["clients"].as_array().is_some_and(|clients| {
                    clients.iter().any(|client| {
                        client["id"] == hc_id.as_str() && client["connection"] == "joined"
                    })
                });
                if joined {
                    hc_joined_while_locked = Some(status.clone());
                    evidence.push(status)?;
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
    let lock_refusal = match stopped_hc.as_deref() {
        Some(id) => headless_lock_refusal(&case, run, id)?,
        None => false,
    };
    evidence.push(serde_json::json!({"current_headless_reported_server_locked": lock_refusal}))?;
    let (unlock_code, unlocked) = typed_admin(&case, "unlock", run, &[], true)?;
    evidence.push(
        serde_json::json!({"typed_admin": "unlock", "exit_code": unlock_code, "result": unlocked}),
    )?;
    if unlock_code != 0 || unlocked["data"]["state"] != "replied" {
        return Err(io::Error::other(format!(
            "Unlock command did not receive a reply: {unlocked}"
        )));
    }
    let unlock_log = wait_log_evidence(
        &case,
        run,
        before_sequence,
        &["unlocked", "server unlocked"],
        &mut evidence,
    )?;
    let mut lock_via_headless_witness = false;
    if let Some(hc_id) = &stopped_hc {
        if hc_joined_while_locked.is_none() {
            if hc_start_succeeded {
                evidence.push(case.ok(&[
                    "--yes",
                    "server",
                    "headless",
                    "stop",
                    &case.instance,
                    "--expected-run-generation",
                    &run.to_string(),
                    "--client",
                    hc_id,
                    "--timeout-ms",
                    "30000",
                ])?)?;
                evidence.push(case.ok(&[
                    "--yes",
                    "server",
                    "headless",
                    "start",
                    &case.instance,
                    "--expected-run-generation",
                    &run.to_string(),
                    "--client",
                    hc_id,
                ])?)?;
            }
            let deadline = Instant::now() + SESSION_DEADLINE;
            while Instant::now() < deadline {
                let status = headless_status(&case)?;
                let joined = status["data"]["clients"].as_array().is_some_and(|clients| {
                    clients.iter().any(|client| {
                        client["id"] == hc_id.as_str() && client["connection"] == "joined"
                    })
                });
                if joined {
                    evidence.push(status)?;
                    lock_via_headless_witness = lock_refusal;
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
        if hc_start_succeeded {
            let (stop_hc_code, stop_hc) = case.raw(
                &[
                    "--yes",
                    "server",
                    "headless",
                    "stop",
                    &case.instance,
                    "--expected-run-generation",
                    &run.to_string(),
                    "--client",
                    hc_id,
                    "--timeout-ms",
                    "30000",
                ],
                None,
            )?;
            evidence.push(serde_json::json!({"restore_operator_stopped_headless": hc_id, "exit_code": stop_hc_code, "result": stop_hc}))?;
            if stop_hc_code != 0 {
                return Err(io::Error::other(format!(
                    "Could not restore operator-stopped HC {hc_id}"
                )));
            }
            wait_operator_stopped_headless(&case, hc_id, &mut evidence)?;
        } else {
            wait_operator_stopped_headless(&case, hc_id, &mut evidence)?;
            evidence.push(serde_json::json!({"limitation": "The operator-stopped HC could not be started under lock, so the join-based lock witness was unavailable"}))?;
        }
    }
    if (!lock_log || !unlock_log) && !lock_via_headless_witness {
        evidence.push(serde_json::json!({"limitation": "Current-run logs or the guarded stopped-headless join probe did not independently expose both lock and unlock state; empty RCon acknowledgments are not treated as semantic proof"}))?;
    } else if hc_joined_while_locked.is_some() {
        evidence.push(serde_json::json!({"limitation": "The headless client joined while locked; that client path did not prove locked admission"}))?;
    }

    let announcement = format!("{nonce}_operator_announcement");
    let (announce_code, announced) =
        typed_admin(&case, "announce", run, &["--message", &announcement], true)?;
    evidence.push(serde_json::json!({"typed_admin": "announce", "exit_code": announce_code, "result": announced}))?;
    if announce_code != 0 || announced["data"]["state"] != "replied" {
        return Err(io::Error::other(format!(
            "Announcement was not acknowledged: {announced}"
        )));
    }
    if !wait_owner_message(&case, &announcement, &mut evidence)? {
        return Err(io::Error::other(
            "The owner console did not independently observe the announcement",
        ));
    }

    let job_suffix = format!("{:08x}", rand::random::<u32>());
    let backup_job = add_admin_job(
        &case,
        &mut owned_jobs,
        &mut evidence,
        &format!("D28_Backup_{job_suffix}"),
        "rotate-mission",
        &["--require-backup", "--mission-template", "foxy_hc.vr"],
    )?;
    let cancel_job = add_admin_job(
        &case,
        &mut owned_jobs,
        &mut evidence,
        &format!("D28_Cancel_{job_suffix}"),
        "restart",
        &[
            "--announcement",
            &format!("D28_cancel_{nonce}"),
            "--announcement-delay-seconds",
            "10",
        ],
    )?;
    let restart_job = add_admin_job(
        &case,
        &mut owned_jobs,
        &mut evidence,
        &format!("D28_Restart_{job_suffix}"),
        "restart",
        &[
            "--announcement",
            "D28 restart accepted",
            "--announcement-delay-seconds",
            "0",
        ],
    )?;
    let rotate_job = add_admin_job(
        &case,
        &mut owned_jobs,
        &mut evidence,
        &format!("D28_Rotate_{job_suffix}"),
        "rotate-mission",
        &["--mission-template", "foxy_hc.vr"],
    )?;

    let before_backup = case.ok(&["server", "instance", "status", &case.instance])?;
    let before_backup_pid = before_backup["data"]["status"]["process_id"].clone();
    let backup_occurrence = run_admin_job(&case, &backup_job, &mut evidence)?;
    let blocked = wait_occurrence(&case, &backup_occurrence, &mut evidence)?;
    if blocked["state"] != "blocked" {
        return Err(io::Error::other(format!(
            "Backup-required mission job was not blocked: {blocked}"
        )));
    }
    let after_backup = case.ok(&["server", "instance", "status", &case.instance])?;
    if after_backup["data"]["status"]["process_id"] != before_backup_pid
        || after_backup["data"]["status"]["run_generation"] != run
    {
        return Err(io::Error::other(
            "Blocked backup-required job changed the active server run",
        ));
    }
    evidence.push(after_backup)?;
    let after_backup_monitor = case.ok(&["server", "instance", "monitor", &case.instance])?;
    evidence.push(after_backup_monitor.clone())?;
    if after_backup_monitor["data"]["monitoring"]["query_ready"] != true {
        return Err(io::Error::other(
            "Blocked backup-required job degraded query readiness",
        ));
    }

    let before_cancel = case.ok(&["server", "instance", "status", &case.instance])?;
    let before_cancel_pid = number(
        &before_cancel["data"]["status"]["process_id"],
        "server process id",
    )?;
    let cancel_occurrence = run_admin_job(&case, &cancel_job, &mut evidence)?;
    if !wait_owner_message(&case, &format!("D28_cancel_{nonce}"), &mut evidence)? {
        return Err(io::Error::other(
            "The delayed restart announcement was not observed before cancellation",
        ));
    }
    let cancel_request = case.ok(&[
        "server",
        "maintenance",
        "cancel",
        &case.instance,
        &cancel_occurrence,
        "--yes",
    ])?;
    evidence.push(cancel_request)?;
    let cancelled = wait_occurrence(&case, &cancel_occurrence, &mut evidence)?;
    if cancelled["state"] != "cancelled" {
        return Err(io::Error::other(format!(
            "Announced restart was not cancelled during its delay: {cancelled}"
        )));
    }
    let after_cancel = case.ok(&["server", "instance", "monitor", &case.instance])?;
    evidence.push(after_cancel.clone())?;
    if after_cancel["data"]["monitoring"]["status"]["run_generation"] != run
        || after_cancel["data"]["monitoring"]["status"]["process_id"] != before_cancel_pid
        || after_cancel["data"]["monitoring"]["query_ready"] != true
    {
        return Err(io::Error::other(
            "Cancelled announced restart changed or degraded the native run",
        ));
    }

    let hc_before_restart = headless_status(&case)?;
    evidence.push(hc_before_restart.clone())?;
    let restart_occurrence = run_admin_job(&case, &restart_job, &mut evidence)?;
    let restarted = wait_occurrence(&case, &restart_occurrence, &mut evidence)?;
    if restarted["state"] != "succeeded" {
        return Err(io::Error::other(format!(
            "Announced restart job did not succeed: {restarted}"
        )));
    }
    let restarted_status = wait_new_ready_run(&case, run, &mut evidence)?;
    let new_run = number(
        &restarted_status["data"]["monitoring"]["status"]["run_generation"],
        "new run generation",
    )?;
    let hc_after_restart = wait_joined_headless(&case, new_run, &mut evidence)?;
    let before_clients = hc_before_restart["data"]["clients"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let after_clients = hc_after_restart["data"]["clients"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let manual_clients: Vec<_> = before_clients
        .iter()
        .filter(|client| client["wanted"] == false)
        .collect();
    if manual_clients.is_empty() {
        return Err(io::Error::other(
            "Prepared current run has no operator-stopped headless client to preserve",
        ));
    }
    if after_clients.len() != before_clients.len()
        || before_clients.iter().any(|before| {
            after_clients
                .iter()
                .find(|after| after["id"] == before["id"])
                .is_none_or(|after| after["wanted"] != before["wanted"])
        })
    {
        return Err(io::Error::other(
            "Headless restart changed managed client count or operator-stopped wanted state",
        ));
    }
    let unique_ids: std::collections::HashSet<_> = after_clients
        .iter()
        .filter_map(|client| client["id"].as_str())
        .collect();
    if unique_ids.len() != after_clients.len() {
        return Err(io::Error::other(
            "Restart produced duplicate managed headless client identities",
        ));
    }
    for before in &manual_clients {
        let after = after_clients
            .iter()
            .find(|client| client["id"] == before["id"]);
        if after.is_none_or(|client| client["wanted"] != false) {
            return Err(io::Error::other(
                "Operator-stopped HC wanted state changed across the server restart",
            ));
        }
    }
    evidence.push(serde_json::json!({"headless_after_restart": hc_after_restart, "operator_stopped_clients_preserved": manual_clients.len()}))?;

    let before_rotate_status = case.ok(&["server", "instance", "monitor", &case.instance])?;
    let before_rotate_pid = number(
        &before_rotate_status["data"]["monitoring"]["status"]["process_id"],
        "server process id",
    )?;
    let before_rotate_sequence = logs_next_sequence(&case, &mut evidence)?;
    let rotate_occurrence = run_admin_job(&case, &rotate_job, &mut evidence)?;
    let rotated = wait_occurrence(&case, &rotate_occurrence, &mut evidence)?;
    if rotated["state"] != "succeeded" {
        return Err(io::Error::other(format!(
            "Mission rotation job did not succeed: {rotated}"
        )));
    }
    if !rotated["message"]
        .as_str()
        .is_some_and(|message| message.contains("answered a fresh query"))
    {
        return Err(io::Error::other(format!(
            "Rotation occurrence did not retain its native fresh-query proof: {rotated}"
        )));
    }
    let after_rotate = case.ok(&["server", "instance", "monitor", &case.instance])?;
    evidence.push(after_rotate.clone())?;
    let after_rotate_status = &after_rotate["data"]["monitoring"]["status"];
    if after_rotate_status["run_generation"] != new_run
        || after_rotate_status["process_id"] != before_rotate_pid
        || after_rotate["data"]["monitoring"]["query_ready"] != true
    {
        return Err(io::Error::other(
            "Mission rotation restarted or degraded its retained native process",
        ));
    }
    if !wait_mission_read(&case, new_run, before_rotate_sequence, &mut evidence)? {
        return Err(io::Error::other(
            "No fresh current-run mission-read log and query response proved rotation",
        ));
    }

    wait_joined_headless(&case, new_run, &mut evidence)?;
    let (code, current_players) = typed_admin(&case, "players", new_run, &[], false)?;
    evidence.push(serde_json::json!({"typed_admin": "players_before_guarded_kick", "exit_code": code, "result": current_players}))?;
    if code != 0 {
        return Err(io::Error::other("The final native roster was incomplete"));
    }
    let target = current_players["data"]["parsed"]["players"]
        .as_array()
        .and_then(|players| {
            players.iter().find(|player| {
                player["verified"] == true
                    && player["address"]
                        .as_str()
                        .is_some_and(|address| address.starts_with("127.0.0.1:"))
                    && player["guid"]
                        .as_str()
                        .is_some_and(|guid| guid.eq_ignore_ascii_case(current_guid))
            })
        })
        .cloned()
        .ok_or_else(|| {
            io::Error::other("No verified private HC was available for the native kick")
        })?;
    let target_id = number(&target["player_id"], "native kick player id")?.to_string();
    let target_guid = string(&target["guid"], "native kick GUID")?;
    let (code, kicked) = typed_admin(
        &case,
        "kick",
        new_run,
        &[
            "--player-id",
            &target_id,
            "--expected-guid",
            target_guid,
            "--reason",
            &nonce,
        ],
        true,
    )?;
    evidence.push(serde_json::json!({"typed_admin": "kick_exact_private_hc", "exit_code": code, "result": kicked}))?;
    if code != 0 {
        return Err(io::Error::other(
            "The guarded native HC kick was not acknowledged",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (code, roster) = typed_admin(&case, "players", new_run, &[], false)?;
        if code == 0
            && roster["data"]["parsed"]["complete"] == true
            && roster["data"]["parsed"]["players"]
                .as_array()
                .is_some_and(|players| !players.iter().any(|player| player["guid"] == target_guid))
        {
            evidence.push(
                serde_json::json!({"native_kick_absence_verified": true, "roster": roster}),
            )?;
            break;
        }
        if Instant::now() >= deadline {
            evidence.push(roster)?;
            return Err(io::Error::other(
                "The native HC kick effect was not independently observed",
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }

    let stop = case.ok(&[
        "server",
        "instance",
        "stop",
        &case.instance,
        "--expected-run-generation",
        &new_run.to_string(),
        "--timeout-ms",
        "60000",
        "--yes",
    ])?;
    evidence.push(stop.clone())?;
    if stop["data"]["outcome"]["completed"] != true
        || stop["data"]["outcome"]["status"]["forced"] != false
    {
        return Err(io::Error::other(format!(
            "Graceful retained-owner stop did not complete without force: {stop}"
        )));
    }
    cleanup.run = None;
    let stopped_status = case.ok(&["server", "instance", "status", &case.instance])?;
    evidence.push(stopped_status.clone())?;
    let status = &stopped_status["data"]["status"];
    if status["run_generation"] != new_run
        || status["phase"] != "exited"
        || status["cleanup_verified"] != true
        || status["process_exists"] != false
        || status["forced"] != false
        || status["headless_running"].as_u64().unwrap_or(0) != 0
    {
        return Err(io::Error::other(format!(
            "Exact run lacks graceful cleanup evidence: {stopped_status}"
        )));
    }
    let (rcon_history_code, rcon_history) =
        case.raw(&["server", "rcon", "history", &case.instance], None)?;
    evidence.push(rcon_history.clone())?;
    let shutdown_record = rcon_history["data"]["history"]["records"]
        .as_array()
        .and_then(|records| {
            records
                .iter()
                .find(|record| record["run_generation"] == new_run && record["text"] == "#shutdown")
        })
        .ok_or_else(|| io::Error::other("Graceful stop has no durable #shutdown RCon record"))?;
    if rcon_history_code != 0
        || shutdown_record["state"] != "replied"
        || shutdown_record["sent_unix_ms"].as_u64().is_none()
    {
        return Err(io::Error::other(format!(
            "#shutdown record does not prove one acknowledged transmission: {shutdown_record}"
        )));
    }
    evidence.push(serde_json::json!({
        "shutdown_acknowledgment": shutdown_record["response"],
        "empty_acknowledgment": shutdown_record["response"] == ""
    }))?;

    owned_jobs.remove_all(&mut evidence)?;
    case.ok(&["server", "supervise", "--shutdown", "--yes"])?;
    cleanup.active = false;
    drop(owner);
    if installation_hash(&case)? != executable_before {
        return Err(io::Error::other("The adopted executable changed"));
    }
    evidence.finish()?;
    Ok(())
}

/// Forwards one client's datagrams and drops every reply to the watched command.
struct Relay {
    address: SocketAddr,
    matching: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Relay {
    fn start(server: SocketAddr, watched: Vec<u8>) -> io::Result<Self> {
        let front = UdpSocket::bind("127.0.0.1:0")?;
        let back = UdpSocket::bind("127.0.0.1:0")?;
        back.connect(server)?;
        front.set_read_timeout(Some(Duration::from_millis(10)))?;
        back.set_read_timeout(Some(Duration::from_millis(10)))?;
        let address = front.local_addr()?;
        let matching = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (matching, dropped, stop) = (matching.clone(), dropped.clone(), stop.clone());
            thread::spawn(move || {
                let mut client = None;
                let watched_sequence = Mutex::new(None::<u8>);
                let mut buffer = [0u8; 65_536];
                while !stop.load(Ordering::Acquire) {
                    if let Ok((length, peer)) = front.recv_from(&mut buffer) {
                        client = Some(peer);
                        let datagram = &buffer[..length];
                        if datagram.len() > 8 && datagram[7] == 0x01 && datagram[9..] == watched[..]
                        {
                            matching.fetch_add(1, Ordering::AcqRel);
                            *watched_sequence.lock().unwrap() = Some(datagram[8]);
                        }
                        let _ = back.send(datagram);
                    }
                    if let Ok(length) = back.recv(&mut buffer) {
                        let datagram = &buffer[..length];
                        let watched_reply = datagram.len() > 8
                            && datagram[7] == 0x01
                            && Some(datagram[8]) == *watched_sequence.lock().unwrap();
                        if watched_reply {
                            dropped.fetch_add(1, Ordering::AcqRel);
                        } else if let Some(peer) = client {
                            let _ = front.send_to(datagram, peer);
                        }
                    }
                }
            })
        };
        Ok(Self {
            address,
            matching,
            dropped,
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn session(case: &Case) -> io::Result<Value> {
    let status = case.ok(&["server", "rcon", "status", &case.instance])?;
    Ok(status["data"]["session"].clone())
}

fn wait_session(
    case: &Case,
    run: u64,
    generation: u64,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let deadline = Instant::now() + SESSION_DEADLINE;
    let mut states = Vec::new();
    loop {
        let status = session(case)?;
        if let Some(state) = status["connection"].as_str()
            && states.last().is_none_or(|last: &String| last != state)
        {
            states.push(state.to_owned());
        }
        if status["connection"] == "connected"
            && status["run_generation"] == run
            && status["session_generation"]
                .as_u64()
                .is_some_and(|value| value >= generation)
        {
            evidence.push(serde_json::json!({"session_states": states, "status": status}))?;
            return Ok(status);
        }
        if Instant::now() >= deadline {
            evidence.push(status)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("RCon session {generation} of run {run} did not connect: {states:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn redeploy(case: &Case, revision: u64, evidence: &mut Evidence) -> io::Result<()> {
    let plan = case.ok(&["server", "configuration", "plan", &case.instance])?;
    let configuration = number(
        &plan["data"]["expected_generation"],
        "configuration generation",
    )?;
    let revision_text = revision.to_string();
    case.ok(&[
        "server",
        "configuration",
        "commit",
        &case.instance,
        "--expected-revision",
        &revision_text,
        "--expected-generation",
        &configuration.to_string(),
        "--expected-fingerprint",
        string(
            &plan["data"]["preview"]["fingerprint"],
            "configuration fingerprint",
        )?,
    ])?;
    let committed = (configuration + 1).to_string();
    let shown = case.ok(&["server", "content", "bindings", "show", &case.instance])?;
    let bindings: Vec<Value> = shown["data"]["state"]["document"]["bindings"]
        .as_array()
        .map(|items| items.iter().map(|item| item["binding"].clone()).collect())
        .unwrap_or_default();
    let file = std::env::temp_dir().join(format!(
        "foxy-rcon-bindings-{:016x}.json",
        rand::random::<u64>()
    ));
    fs::write(&file, serde_json::to_vec(&bindings)?)?;
    let bindings_plan = case.ok(&[
        "server",
        "content",
        "bindings",
        "plan",
        &case.instance,
        "--bindings",
        path_arg(&file)?,
    ]);
    let saved = bindings_plan.and_then(|plan| {
        case.ok(&[
            "server",
            "content",
            "bindings",
            "save",
            &case.instance,
            "--bindings",
            path_arg(&file)?,
            "--expected-plan-fingerprint",
            string(&plan["data"]["plan"]["fingerprint"], "binding fingerprint")?,
        ])
    });
    let _ = fs::remove_file(&file);
    evidence.push(saved?)?;
    let stage_plan = case.ok(&["server", "content", "stage", "plan", &case.instance])?;
    let stage = string(&stage_plan["data"]["plan"]["stage_id"], "stage id")?.to_owned();
    case.ok(&[
        "server",
        "content",
        "stage",
        "apply",
        &case.instance,
        "--expected-plan-fingerprint",
        string(
            &stage_plan["data"]["plan"]["fingerprint"],
            "stage fingerprint",
        )?,
    ])?;
    let records = case.ok(&["server", "deploy", "list", &case.instance])?;
    let deployment = number(
        &records["data"]["state"]["generation"],
        "deployment generation",
    )?;
    let deployment_text = deployment.to_string();
    let prepare_args = [
        "--stage",
        stage.as_str(),
        "--expected-revision",
        revision_text.as_str(),
        "--expected-configuration-generation",
        committed.as_str(),
        "--expected-deployment-generation",
        deployment_text.as_str(),
    ];
    let mut args = vec!["server", "deploy", "prepare", case.instance.as_str()];
    args.extend_from_slice(&prepare_args);
    let prepared = case.ok(&args)?;
    let fingerprint = string(
        &prepared["data"]["plan"]["fingerprint"],
        "record fingerprint",
    )?
    .to_owned();
    let mut args = vec!["server", "deploy", "commit", case.instance.as_str()];
    args.extend_from_slice(&prepare_args);
    args.extend_from_slice(&["--expected-plan-fingerprint", &fingerprint]);
    case.ok(&args)?;
    let selection = (deployment + 1).to_string();
    let runtime = case.ok(&[
        "server",
        "deploy",
        "runtime",
        "plan",
        &case.instance,
        "--expected-deployment-generation",
        &selection,
    ])?;
    let built = case.ok(&[
        "server",
        "deploy",
        "runtime",
        "build",
        &case.instance,
        "--expected-deployment-generation",
        &selection,
        "--expected-plan-fingerprint",
        string(
            &runtime["data"]["plan"]["fingerprint"],
            "runtime fingerprint",
        )?,
    ])?;
    let preparation = string(&built["data"]["report"]["preparation_id"], "preparation")?.to_owned();
    let (_, current) = case.raw(&["server", "instance", "start-plan", &case.instance], None)?;
    let activation = current["data"]["plan"]["activation_generation"]
        .as_u64()
        .unwrap_or(0)
        .to_string();
    let reviewed = case.ok(&[
        "server",
        "deploy",
        "activation",
        "plan",
        &case.instance,
        &preparation,
        "--expected-deployment-generation",
        &selection,
        "--expected-activation-generation",
        &activation,
    ])?;
    let applied = case.ok(&[
        "--yes",
        "server",
        "deploy",
        "activation",
        "apply",
        &case.instance,
        &preparation,
        "--expected-deployment-generation",
        &selection,
        "--expected-activation-generation",
        &activation,
        "--expected-plan-fingerprint",
        string(
            &reviewed["data"]["plan"]["fingerprint"],
            "activation fingerprint",
        )?,
    ])?;
    evidence.push(applied)?;
    Ok(())
}

fn start_and_wait(case: &Case, evidence: &mut Evidence) -> io::Result<u64> {
    let plan = case.ok(&["server", "instance", "start-plan", &case.instance])?;
    let generation =
        number(&plan["data"]["plan"]["activation_generation"], "activation")?.to_string();
    let fingerprint = string(&plan["data"]["plan"]["fingerprint"], "fingerprint")?.to_owned();
    let started = case.ok(&[
        "server",
        "instance",
        "start",
        &case.instance,
        "--expected-activation-generation",
        &generation,
        "--expected-plan-fingerprint",
        &fingerprint,
        "--yes",
    ])?;
    evidence.push(started.clone())?;
    let run = number(
        &started["data"]["outcome"]["status"]["run_generation"],
        "run",
    )?;
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let value = case.ok(&["server", "instance", "monitor", &case.instance])?;
        let report = &value["data"]["monitoring"];
        if report["query_ready"] == true && report["status"]["run_generation"] == run {
            evidence.push(value)?;
            return Ok(run);
        }
        if Instant::now() >= deadline {
            evidence.push(value)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Run {run} did not become query-ready"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn stop(case: &Case, run: u64, evidence: &mut Evidence) -> io::Result<()> {
    let stopped = case.ok(&[
        "server",
        "instance",
        "stop",
        &case.instance,
        "--expected-run-generation",
        &run.to_string(),
        "--timeout-ms",
        "60000",
        "--yes",
    ])?;
    evidence.push(stopped.clone())?;
    if stopped["data"]["outcome"]["completed"] != true {
        return Err(io::Error::other(format!(
            "Graceful stop did not complete: {stopped}"
        )));
    }
    Ok(())
}

fn installation_hash(case: &Case) -> io::Result<String> {
    let listing = case.ok(&["server", "instance", "list"])?;
    let catalog = &listing["data"]["catalog"];
    let item = catalog["instances"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["id"] == case.instance.as_str())
        })
        .ok_or_else(|| io::Error::other("Prepared instance missing"))?;
    let installation_id = string(&item["installation_id"], "installation id")?;
    let installation = catalog["installations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == installation_id))
        .ok_or_else(|| io::Error::other("Prepared installation missing"))?;
    let executable = PathBuf::from(string(
        &installation["directory"],
        "installation directory",
    )?)
    .join(string(
        &installation["executable"],
        "installation executable",
    )?);
    let mut file = File::open(&executable)?;
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

fn start_parent_bound_owner(case: &Case) -> io::Result<OwnedChild> {
    let mut command = base_command(&case.config);
    command
        .args([
            "server",
            "supervise",
            "--parent-bound",
            "--game-space",
            &case.space,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(OwnedChild(Some(command.spawn()?)))
}

fn wait_owner(case: &Case, child: &mut OwnedChild) -> io::Result<()> {
    let deadline = Instant::now() + OWNER_START_DEADLINE;
    loop {
        if child
            .0
            .as_mut()
            .and_then(|process| process.try_wait().ok().flatten())
            .is_some()
        {
            return Err(io::Error::other("Owner exited before becoming ready"));
        }
        let (code, value) = case.raw(&["server", "owner"], None)?;
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
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Evidence {
    path: PathBuf,
    secret: String,
    items: Vec<Value>,
}
impl Evidence {
    fn new(output: &Path, secret: &str) -> Self {
        Self {
            path: output.join("rcon.json"),
            secret: secret.to_owned(),
            items: Vec::new(),
        }
    }
    fn push(&mut self, value: Value) -> io::Result<()> {
        if value.to_string().contains(&self.secret) {
            return Err(io::Error::other(
                "The RCon password appeared in command output",
            ));
        }
        self.items.push(value);
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        fs::write(&self.path, serde_json::to_vec_pretty(&self.items)?)
    }
}
impl Drop for Evidence {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

impl Case {
    fn args<'a>(&'a self, args: &[&'a str]) -> Vec<&'a str> {
        let mut all = args.to_vec();
        all.extend_from_slice(&["--game-space", &self.space]);
        all
    }

    fn raw(&self, args: &[&str], stdin: Option<&str>) -> io::Result<(i32, Value)> {
        let mut command = base_command(&self.config);
        command
            .args(self.args(args))
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("Missing stdin"))?
                .write_all(input.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            io::Error::other(format!(
                "CLI emitted invalid JSON: {error}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            ))
        })?;
        if String::from_utf8_lossy(&output.stderr).contains(&self.password) {
            return Err(io::Error::other("The RCon password appeared on stderr"));
        }
        Ok((output.status.code().unwrap_or(-1), value))
    }

    fn ok(&self, args: &[&str]) -> io::Result<Value> {
        let (code, value) = self.raw(args, None)?;
        if code != 0 || value["ok"] != true {
            return Err(io::Error::other(format!(
                "CLI exit {code}, args={args:?}: {value}"
            )));
        }
        Ok(value)
    }

    fn ok_with_stdin(&self, args: &[&str], stdin: &str) -> io::Result<Value> {
        let (code, value) = self.raw(args, Some(stdin))?;
        if code != 0 || value["ok"] != true {
            return Err(io::Error::other(format!(
                "CLI exit {code}, args={args:?}: {value}"
            )));
        }
        Ok(value)
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

fn path_arg(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other("Non-UTF-8 path"))
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

/// Stops the run gracefully and shuts the owner down when the test fails midway, so a failed
/// run never leaves the owner to be killed with a server still running.
struct Cleanup<'a> {
    case: &'a Case,
    run: Option<u64>,
    active: bool,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(run) = self.run {
            let _ = self.case.raw(
                &[
                    "server",
                    "instance",
                    "stop",
                    &self.case.instance,
                    "--expected-run-generation",
                    &run.to_string(),
                    "--timeout-ms",
                    "60000",
                    "--yes",
                ],
                None,
            );
        }
        let _ = self
            .case
            .raw(&["server", "supervise", "--shutdown", "--yes"], None);
    }
}

/// Retires only exit-proven records an earlier killed owner left behind.
fn recover_leftovers(case: &Case, evidence: &mut Evidence) -> io::Result<()> {
    let plan = case.ok(&["server", "instance", "recovery-plan", &case.instance])?;
    let plan = plan["data"]["plan"].clone();
    if plan["pending_count"].as_u64().unwrap_or(1) != 0 {
        return Err(io::Error::other(format!(
            "Unresolved consumer records: {plan}"
        )));
    }
    if plan["eligible_count"].as_u64().unwrap_or(0) > 0 {
        let applied = case.ok(&[
            "--yes",
            "server",
            "instance",
            "recover",
            &case.instance,
            "--expected-plan-fingerprint",
            string(&plan["fingerprint"], "recovery fingerprint")?,
        ])?;
        evidence.push(applied)?;
    }
    Ok(())
}

fn typed_admin(
    case: &Case,
    verb: &str,
    run: u64,
    extra: &[&str],
    yes: bool,
) -> io::Result<(i32, Value)> {
    let mut args = Vec::with_capacity(extra.len() + 6);
    if yes {
        args.push("--yes".to_owned());
    }
    args.extend([
        "server".to_owned(),
        "admin".to_owned(),
        verb.to_owned(),
        case.instance.clone(),
        "--expected-run-generation".to_owned(),
        run.to_string(),
    ]);
    args.extend(extra.iter().map(|value| (*value).to_owned()));
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    case.raw(&refs, None)
}

fn headless_status(case: &Case) -> io::Result<Value> {
    case.ok(&["server", "headless", "status", &case.instance])
}

fn headless_lock_refusal(case: &Case, run: u64, client_id: &str) -> io::Result<bool> {
    let plan = case.ok(&["server", "instance", "start-plan", &case.instance])?;
    let root = plan["data"]["plan"]["launch"]["headless"]
        .as_array()
        .and_then(|clients| clients.iter().find(|client| client["id"] == client_id))
        .and_then(|client| client["state_root"].as_str())
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("The private HC state root is unavailable"))?;
    let session = session(case)?;
    let owner_id = string(&session["owner_id"], "current RCon owner")?;
    let prefix = format!("run_{owner_id}_{run}_");
    for entry in fs::read_dir(root)?.take(512) {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) || !name.ends_with(".log") || !entry.file_type()?.is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        File::open(entry.path())?
            .take(256 * 1024)
            .read_to_end(&mut bytes)?;
        if String::from_utf8_lossy(&bytes).contains("This server is currently locked.") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn wait_operator_stopped_headless(
    case: &Case,
    client_id: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let status = headless_status(case)?;
        let stopped = status["data"]["clients"].as_array().is_some_and(|clients| {
            clients.iter().any(|client| {
                client["id"] == client_id
                    && client["wanted"] == false
                    && client["state"] == "stopped"
                    && client["reason"] == "stopped_by_operator"
            })
        });
        last = status;
        if stopped {
            evidence.push(last)?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    evidence.push(last.clone())?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("Headless client {client_id} did not return to its operator-stopped state: {last}"),
    ))
}

fn wait_joined_headless(case: &Case, run: u64, evidence: &mut Evidence) -> io::Result<Value> {
    let deadline = Instant::now() + SESSION_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let status = headless_status(case)?;
        let joined = status["data"]["run_generation"] == run
            && status["data"]["clients"].as_array().is_some_and(|clients| {
                clients
                    .iter()
                    .any(|client| client["state"] == "running" && client["connection"] == "joined")
            });
        let (code, roster) = typed_admin(case, "players", run, &[], false)?;
        if joined
            && code == 0
            && roster.pointer("/data/parsed/complete") == Some(&Value::Bool(true))
            && roster["data"]["parsed"]["players"]
                .as_array()
                .is_some_and(|players| players.iter().any(|player| player["verified"] == true))
        {
            evidence.push(status.clone())?;
            evidence.push(
                serde_json::json!({"typed_admin": "players_joined_headless", "result": roster}),
            )?;
            return Ok(status);
        }
        last =
            serde_json::json!({"headless": status, "players_exit_code": code, "players": roster});
        thread::sleep(Duration::from_millis(500));
    }
    evidence.push(last.clone())?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("No joined headless client and nonempty complete RCon roster: {last}"),
    ))
}

fn wait_owner_message(case: &Case, message: &str, evidence: &mut Evidence) -> io::Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let status = case.ok(&["server", "rcon", "status", &case.instance])?;
        let observed = status["data"]["session"]["messages"]
            .as_array()
            .is_some_and(|messages| {
                messages.iter().any(|item| {
                    item["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(message))
                })
            });
        last = status;
        if observed {
            evidence.push(last)?;
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(250));
    }
    evidence.push(last)?;
    Ok(false)
}

fn wait_log_evidence(
    case: &Case,
    run: u64,
    after_sequence: u64,
    needles: &[&str],
    evidence: &mut Evidence,
) -> io::Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (code, logs) = case.raw(
            &[
                "server",
                "instance",
                "logs",
                &case.instance,
                "--after-sequence",
                &after_sequence.to_string(),
            ],
            None,
        )?;
        let matches = code == 0
            && logs["data"]["run_generation"] == run
            && logs["data"]["logs"]["entries"]
                .as_array()
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry["sequence"]
                            .as_u64()
                            .is_some_and(|sequence| sequence >= after_sequence)
                            && entry["text"].as_str().is_some_and(|text| {
                                let text = text.to_ascii_lowercase();
                                needles
                                    .iter()
                                    .any(|needle| text.contains(&needle.to_ascii_lowercase()))
                            })
                    })
                });
        last = logs;
        if matches {
            evidence.push(last)?;
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(250));
    }
    evidence.push(last)?;
    Ok(false)
}

fn wrong_guid(current: &str) -> String {
    let mut bytes = current.as_bytes().to_vec();
    bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
    String::from_utf8(bytes).expect("GUIDs are ASCII")
}

fn add_admin_job(
    case: &Case,
    owned: &mut AdminJobCleanup<'_>,
    evidence: &mut Evidence,
    name: &str,
    action: &str,
    extra: &[&str],
) -> io::Result<Value> {
    let before = case.ok(&["server", "maintenance", "list", &case.instance])?;
    let revision = number(
        &before["data"]["overview"]["catalog_revision"],
        "maintenance catalog revision",
    )?
    .to_string();
    let mut args = vec![
        "server",
        "maintenance",
        "add",
        &case.instance,
        "--expected-revision",
        &revision,
        "--name",
        name,
        "--action",
        action,
        "--at",
        "2099-01-01T00:00:00Z",
        "--disabled",
    ];
    args.extend_from_slice(extra);
    let added = case.ok(&args)?;
    let job_id = string(&added["data"]["job_id"], "created maintenance job id")?.to_owned();
    owned.jobs.push(job_id.clone());
    evidence.push(serde_json::json!({"maintenance_add": name, "result": added}))?;
    let listed = case.ok(&["server", "maintenance", "list", &case.instance])?;
    let job = listed["data"]["overview"]["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|item| item["job"]["id"] == job_id))
        .cloned()
        .ok_or_else(|| {
            io::Error::other("Created maintenance job did not appear in the saved catalog")
        })?;
    if job["job"]["definition"]["enabled"] != false {
        return Err(io::Error::other(
            "Fixture maintenance job was not saved disabled",
        ));
    }
    evidence.push(serde_json::json!({"maintenance_list_after_add": listed}))?;
    Ok(job)
}

fn run_admin_job(case: &Case, job: &Value, evidence: &mut Evidence) -> io::Result<String> {
    let id = string(&job["job"]["id"], "maintenance job id")?;
    let revision = number(&job["job"]["revision"], "maintenance policy revision")?.to_string();
    let result = case.ok(&[
        "--yes",
        "server",
        "maintenance",
        "run",
        &case.instance,
        id,
        "--expected-policy-revision",
        &revision,
    ])?;
    let occurrence = string(
        &result["data"]["occurrence_id"],
        "maintenance occurrence id",
    )?
    .to_owned();
    evidence.push(serde_json::json!({"maintenance_run": id, "result": result}))?;
    Ok(occurrence)
}

fn wait_occurrence(case: &Case, occurrence_id: &str, evidence: &mut Evidence) -> io::Result<Value> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let history = case.ok(&["server", "maintenance", "history", &case.instance])?;
        let occurrence = history["data"]["occurrences"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["occurrence_id"] == occurrence_id)
            })
            .cloned();
        let last = serde_json::json!({"history": history, "occurrence": occurrence});
        if let Some(item) = occurrence
            && item["state"].as_str().is_some_and(|state| {
                matches!(
                    state,
                    "succeeded" | "blocked" | "cancelled" | "failed" | "unknown" | "skipped"
                )
            })
        {
            evidence.push(last)?;
            return Ok(item);
        }
        if Instant::now() >= deadline {
            evidence.push(last.clone())?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Maintenance occurrence did not finish: {last}"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn wait_new_ready_run(case: &Case, previous: u64, evidence: &mut Evidence) -> io::Result<Value> {
    let deadline = Instant::now() + READY_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let status = case.raw(&["server", "instance", "monitor", &case.instance], None)?;
        let ready = status.0 == 0
            && status.1["data"]["monitoring"]["query_ready"] == true
            && status.1["data"]["monitoring"]["status"]["run_generation"]
                .as_u64()
                .is_some_and(|run| run > previous);
        last = status.1;
        if ready {
            evidence.push(last.clone())?;
            return Ok(last);
        }
        thread::sleep(Duration::from_millis(500));
    }
    evidence.push(last.clone())?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("No new query-ready run appeared: {last}"),
    ))
}

fn logs_next_sequence(case: &Case, evidence: &mut Evidence) -> io::Result<u64> {
    let logs = case.ok(&["server", "instance", "logs", &case.instance])?;
    let sequence = number(&logs["data"]["logs"]["next_sequence"], "log sequence")?;
    evidence.push(logs)?;
    Ok(sequence)
}

fn wait_mission_read(
    case: &Case,
    run: u64,
    after_sequence: u64,
    evidence: &mut Evidence,
) -> io::Result<bool> {
    let deadline = Instant::now() + SESSION_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (code, value) = case.raw(
            &[
                "server",
                "instance",
                "logs",
                &case.instance,
                "--after-sequence",
                &after_sequence.to_string(),
            ],
            None,
        )?;
        let read = code == 0
            && value["data"]["run_generation"] == run
            && value["data"]["logs"]["entries"]
                .as_array()
                .is_some_and(|entries| {
                    let fresh = entries
                        .iter()
                        .filter(|entry| {
                            entry["sequence"]
                                .as_u64()
                                .is_some_and(|sequence| sequence >= after_sequence)
                        })
                        .filter_map(|entry| entry["text"].as_str())
                        .collect::<Vec<_>>();
                    fresh
                        .iter()
                        .any(|text| text.contains("Mission file: foxy_hc (__cur_mp)"))
                        && fresh.iter().any(|text| text.contains("Mission world: vr"))
                        && fresh.iter().any(|text| {
                            text.split_once("Mission id:").is_some_and(|(_, id)| {
                                let id = id.trim();
                                id.len() == 40 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                        })
                });
        last = value;
        if read {
            evidence.push(last)?;
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(300));
    }
    evidence.push(last)?;
    Ok(false)
}

struct AdminJobCleanup<'a> {
    case: &'a Case,
    jobs: Vec<String>,
    active: bool,
}

impl<'a> AdminJobCleanup<'a> {
    fn new(case: &'a Case) -> Self {
        Self {
            case,
            jobs: Vec::new(),
            active: true,
        }
    }

    fn remove_all(&mut self, evidence: &mut Evidence) -> io::Result<()> {
        for job_id in self.jobs.clone() {
            let listed = self
                .case
                .ok(&["server", "maintenance", "list", &self.case.instance])?;
            let revision = number(
                &listed["data"]["overview"]["catalog_revision"],
                "maintenance catalog revision",
            )?
            .to_string();
            let exists = listed["data"]["overview"]["jobs"]
                .as_array()
                .is_some_and(|jobs| jobs.iter().any(|job| job["job"]["id"] == job_id));
            evidence.push(listed)?;
            if exists {
                let removed = self.case.ok(&[
                    "--yes",
                    "server",
                    "maintenance",
                    "remove",
                    &self.case.instance,
                    &job_id,
                    "--expected-revision",
                    &revision,
                ])?;
                evidence.push(removed)?;
            }
        }
        self.active = false;
        Ok(())
    }
}

impl Drop for AdminJobCleanup<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        for job_id in self.jobs.clone() {
            let Ok(listed) = self
                .case
                .ok(&["server", "maintenance", "list", &self.case.instance])
            else {
                continue;
            };
            let Ok(revision) = number(
                &listed["data"]["overview"]["catalog_revision"],
                "maintenance catalog revision",
            ) else {
                continue;
            };
            if !listed["data"]["overview"]["jobs"]
                .as_array()
                .is_some_and(|jobs| jobs.iter().any(|job| job["job"]["id"] == job_id))
            {
                continue;
            }
            let _ = self.case.raw(
                &[
                    "--yes",
                    "server",
                    "maintenance",
                    "remove",
                    &self.case.instance,
                    &job_id,
                    "--expected-revision",
                    &revision.to_string(),
                ],
                None,
            );
        }
    }
}
