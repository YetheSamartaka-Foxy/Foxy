use super::*;
use std::io::Read;

const END_DELAY_MS: u32 = 15_000;
const SCENARIO_END_TIMEOUT: Duration = Duration::from_secs(180);
const RELOAD_OBSERVATION: Duration = Duration::from_secs(45);
const SCENARIO_WORLD_ARGUMENT: &str = "worlds/GameMaster/GM_Arland.ent";

#[derive(Serialize)]
struct ScenarioEndReport {
    schema_version: u32,
    probe: &'static str,
    launch_route: &'static str,
    production_configuration_identity_verified: bool,
    selected_policies: Vec<String>,
    result: &'static str,
    source_unchanged: bool,
    copied_payload_unchanged: bool,
    source_inventory_before: Vec<TreeEntry>,
    source_inventory_after: Vec<TreeEntry>,
    copied_inventory_before: Vec<TreeEntry>,
    copied_inventory_after: Vec<TreeEntry>,
    phases: Vec<PhaseEvidence>,
    report_path: PathBuf,
}

#[derive(Serialize)]
struct PhaseEvidence {
    name: &'static str,
    arguments: Vec<String>,
    nonce: String,
    event_log_path: PathBuf,
    server_world_argument: &'static str,
    a2s_name_observed: Option<String>,
    containment_root_pid: u32,
    process_exit: Option<TreeExit>,
    stop_exit: Option<TreeExit>,
    process_relaunch_if_present: bool,
    cleanup_outcome: &'static str,
    end_request_observed: bool,
    native_event_sequence_verified: bool,
    loaded_world_observed: bool,
    scenario_ready_before_end: bool,
    a2s_observed_before_end: bool,
    starts: Vec<u32>,
    end_request_after_start_ms: Option<u128>,
    reload_after_end_request_ms: Option<u128>,
    exit_after_end_request_ms: Option<u128>,
    stop_after_end_request_ms: Option<u128>,
    process_version_lines: usize,
    retained_consumer_before_stop: bool,
    consumer_released_after_wait: bool,
    stop_won_pending_reload: bool,
    no_reload: bool,
    log_path: PathBuf,
    inspected_log_files: Vec<String>,
}

struct EndPolicy {
    name: &'static str,
    args: &'static [&'static str],
    expect_exit: bool,
    expect_reload: bool,
    stop_during_delay: bool,
    inspect_end_window: bool,
}

const POLICIES: &[EndPolicy] = &[
    EndPolicy {
        name: "shutdown_zero",
        args: &["-autoreload", "0", "-autoshutdown"],
        expect_exit: true,
        expect_reload: false,
        stop_during_delay: false,
        inspect_end_window: false,
    },
    EndPolicy {
        name: "reload_zero",
        args: &["-autoreload", "0"],
        expect_exit: false,
        expect_reload: true,
        stop_during_delay: false,
        inspect_end_window: false,
    },
    EndPolicy {
        name: "shutdown_three",
        args: &["-autoreload", "3", "-autoshutdown"],
        expect_exit: true,
        expect_reload: false,
        stop_during_delay: false,
        inspect_end_window: false,
    },
    EndPolicy {
        name: "reload_three",
        args: &["-autoreload", "3"],
        expect_exit: false,
        expect_reload: true,
        stop_during_delay: false,
        inspect_end_window: false,
    },
    EndPolicy {
        name: "reload_three_stop_wins",
        args: &["-autoreload", "3"],
        expect_exit: false,
        expect_reload: false,
        stop_during_delay: true,
        inspect_end_window: false,
    },
    EndPolicy {
        name: "disabled_autoshutdown",
        args: &["-autoreload", "disabled", "-autoshutdown"],
        expect_exit: true,
        expect_reload: false,
        stop_during_delay: false,
        inspect_end_window: false,
    },
    EndPolicy {
        name: "disabled_without_autoshutdown",
        args: &["-autoreload", "disabled"],
        expect_exit: false,
        expect_reload: false,
        stop_during_delay: false,
        inspect_end_window: true,
    },
];

pub(super) fn run() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other(
            "Native process containment is unavailable",
        ));
    }
    let source = input_directory("FOXY_REFORGER_NATIVE_SOURCE")?;
    let scratch = input_directory("FOXY_REFORGER_NATIVE_SCRATCH")?;
    let selected = std::env::var("FOXY_REFORGER_END_ONLY").ok();
    let policies: Vec<_> = POLICIES
        .iter()
        .filter(|policy| {
            selected
                .as_ref()
                .is_none_or(|selected| selected == policy.name)
        })
        .collect();
    if policies.is_empty() {
        return Err(io::Error::other("Unknown requested scenario-end policy"));
    }
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and private scratch must be separate",
        ));
    }
    #[cfg(target_os = "linux")]
    check_linux_prerequisites(&scratch)?;

    let root = tempfile::Builder::new()
        .prefix("reforger-scenario-end-")
        .tempdir_in(&scratch)?
        .keep();
    private_directory(&root)?;
    let source_locks = InstallationLocks::capture_in(&source, &root.join("source-locks"))?;
    let _source_runtime = source_locks.try_admit()?.try_runtime()?;
    let source_inventory_before = snapshot_tree(&source)?;
    let prepared = std::env::var_os("FOXY_REFORGER_END_PAYLOAD").is_some();
    let payload = if prepared {
        let payload = input_directory("FOXY_REFORGER_END_PAYLOAD")?;
        if !payload.starts_with(&scratch) || payload == scratch || payload.starts_with(&source) {
            return Err(io::Error::other(
                "Prepared scenario-end payload must belong to private scratch",
            ));
        }
        payload
    } else {
        let payload = root.join("payload");
        fs::create_dir(&payload)?;
        copy_tree(&source, &payload)?;
        payload
    };
    let copied_inventory_before = snapshot_tree(&payload)?;
    if source_inventory_before != copied_inventory_before {
        return Err(io::Error::other(
            "Copied Reforger payload differs from read-only source",
        ));
    }
    let executable = payload.join(executable_name());
    let base_project = payload.join("addons/data/ArmaReforger.gproj");
    if !executable.is_file() || !base_project.is_file() {
        return Err(io::Error::other(
            "Copied payload is missing its dedicated executable or data project",
        ));
    }
    let addon_root = root.join("fixture-addons");
    let addon_id = format!("{:016X}", rand::random::<u64>());
    write_fixture_addon(&addon_root, &addon_id)?;
    let registry = if prepared {
        payload
            .parent()
            .ok_or_else(|| io::Error::other("Prepared payload has no owner root"))?
            .join("runtime-locks")
    } else {
        root.join("runtime-locks")
    };
    let locks = InstallationLocks::capture_in(&payload, &registry)?;
    if !locks.inspect_consumers()?.is_empty() {
        return Err(io::Error::other(
            "Prepared native payload has unresolved consumers",
        ));
    }
    let mut phases = Vec::with_capacity(policies.len());
    for (index, policy) in policies.iter().copied().enumerate() {
        let phase = run_phase(
            &payload,
            &addon_root,
            &addon_id,
            &locks,
            &root,
            policy,
            index as u64 + 1,
        )?;
        let script_started = !phase.starts.is_empty();
        phases.push(phase);
        if !script_started {
            break;
        }
    }

    let source_inventory_after = snapshot_tree(&source)?;
    let copied_inventory_after = snapshot_tree(&payload)?;
    let source_unchanged = source_inventory_before == source_inventory_after;
    let copied_payload_unchanged = copied_inventory_before == copied_inventory_after;
    let phases_observed = phases.len() == policies.len()
        && phases.iter().zip(&policies).all(|(phase, policy)| {
            phase.end_request_observed
                && phase.native_event_sequence_verified
                && phase.process_version_lines == 1
                && phase.loaded_world_observed
                && phase.scenario_ready_before_end
                && phase.retained_consumer_before_stop
                && phase.consumer_released_after_wait
                && phase.stop_exit.as_ref().is_none_or(|exit| !exit.forced)
                && phase.process_exit.as_ref().is_none_or(|exit| !exit.forced)
                && match policy.name {
                    "shutdown_zero" | "shutdown_three" | "disabled_autoshutdown" => {
                        phase.process_exit.is_some()
                            && phase.cleanup_outcome == "natural_exit_waited"
                            && phase.starts.len() == 1
                            && phase
                                .end_request_after_start_ms
                                .is_some_and(|ms| (10_000..=30_000).contains(&ms))
                            && phase.exit_after_end_request_ms.is_some_and(|ms| {
                                let minimum = if policy.name == "shutdown_three" {
                                    2_500
                                } else {
                                    0
                                };
                                (minimum..=15_000).contains(&ms)
                            })
                    }
                    "reload_zero" | "reload_three" => {
                        phase.stop_exit.is_some()
                            && phase.process_exit.is_none()
                            && phase.cleanup_outcome == "soft_stop_waited"
                            && phase.starts.len() == 2
                            && phase.starts[0] == 1
                            && phase.starts[1] == 2
                            && phase.process_version_lines == 1
                            && phase
                                .end_request_after_start_ms
                                .is_some_and(|ms| (10_000..=30_000).contains(&ms))
                            && phase.reload_after_end_request_ms.is_some_and(|ms| {
                                let minimum = if policy.name == "reload_three" {
                                    2_500
                                } else {
                                    0
                                };
                                (minimum..=15_000).contains(&ms)
                            })
                    }
                    "reload_three_stop_wins" => {
                        phase.stop_won_pending_reload
                            && phase.no_reload
                            && phase.stop_exit.is_some()
                            && phase.cleanup_outcome == "soft_stop_waited"
                            && phase.starts == [1]
                            && phase.stop_after_end_request_ms.is_some_and(|ms| ms < 3_000)
                    }
                    "disabled_without_autoshutdown" => {
                        phase.starts == [1]
                            && phase.process_exit.is_none()
                            && phase.process_version_lines == 1
                            && phase.no_reload
                            && phase.stop_exit.is_some()
                            && phase.cleanup_outcome == "soft_stop_waited"
                            && phase
                                .end_request_after_start_ms
                                .is_some_and(|ms| (10_000..=30_000).contains(&ms))
                    }
                    _ => false,
                }
        });
    let report_path = root.join("report.json");
    let report = ScenarioEndReport {
        schema_version: 1,
        probe: "reforger_native_scenario_end",
        launch_route: "native_server_world_argument",
        production_configuration_identity_verified: false,
        selected_policies: policies.iter().map(|policy| policy.name.into()).collect(),
        result: if phases_observed && source_unchanged && copied_payload_unchanged {
            "observed"
        } else {
            "incomplete"
        },
        source_unchanged,
        copied_payload_unchanged,
        source_inventory_before,
        source_inventory_after,
        copied_inventory_before,
        copied_inventory_after,
        phases,
        report_path: report_path.clone(),
    };
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "Private native scenario-end evidence: {}",
        report_path.display()
    );
    if report.result != "observed" {
        return Err(io::Error::other(
            "Native scenario-end behavior or waited cleanup was not observed",
        ));
    }
    Ok(())
}

fn write_fixture_addon(root: &Path, addon_id: &str) -> io::Result<()> {
    if addon_id.len() != 16 || !addon_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other(
            "Fixture addon GUID must have exactly 16 hex digits",
        ));
    }
    let addon = root.join(addon_id);
    let scripts = addon.join("scripts/Game");
    fs::create_dir_all(&scripts)?;
    private_directory(root)?;
    let project = format!(
        "GameProject {{\n ID \"FoxyNativeScenarioEnd\"\n GUID \"{addon_id}\"\n TITLE \"Foxy native scenario-end fixture\"\n Dependencies {{\n  \"58D0FB3206B6F859\"\n }}\n Configurations {{\n  GameProjectConfig PC {{\n  }}\n  GameProjectConfig HEADLESS {{\n  }}\n }}\n}}\n"
    );
    fs::write(addon.join("addon.gproj"), project)?;
    Ok(())
}

fn script_for(nonce: &str) -> String {
    format!(
        r#"modded class SCR_BaseGameMode
{{
    static int s_FoxyScenarioEndStartCount;
    void FoxyWriteEvent(string kind)
    {{
        string line = "FOXY_SCENARIO_END nonce={nonce} event=" + kind;
        FileHandle file = FileIO.OpenFile("$profile:foxy-scenario-end.txt", FileMode.APPEND);
        if (file)
        {{
            file.WriteLine(line);
            file.Close();
        }}
        Print(line);
    }}
    override protected void OnGameStart()
    {{
        super.OnGameStart();
        if (!Replication.IsServer())
            return;
        s_FoxyScenarioEndStartCount++;
        FoxyWriteEvent("START index=" + s_FoxyScenarioEndStartCount);
        if (s_FoxyScenarioEndStartCount == 1)
            GetGame().GetCallqueue().CallLater(FoxyRequestScenarioEnd, {END_DELAY_MS}, false);
    }}
    void FoxyRequestScenarioEnd()
    {{
        FoxyWriteEvent("END_REQUEST");
        EndGameMode(SCR_GameModeEndData.CreateSimple(EGameOverTypes.ENDREASON_UNDEFINED));
    }}
}}
"#
    )
}

fn run_phase(
    payload: &Path,
    addon_root: &Path,
    addon_id: &str,
    locks: &InstallationLocks,
    root: &Path,
    policy: &EndPolicy,
    generation: u64,
) -> io::Result<PhaseEvidence> {
    let instance = create_instance(root, policy.name)?;
    let nonce = fresh_token();
    let script_path = addon_root
        .join(addon_id)
        .join("scripts/Game/FoxyScenarioEnd.c");
    fs::write(&script_path, script_for(&nonce))?;
    let mut ports = HashSet::new();
    let bind_port = loopback_udp_port(&mut ports)?;
    let a2s_port = loopback_udp_port(&mut ports)?;
    let stdout_path = instance.logs.join("engine.stdout.log");
    let stdout = File::create(&stdout_path)?;
    let mut cursors = BTreeMap::new();
    let mut captured = String::new();
    let mut primary = PrimaryLog::new(instance.logs.join("console.log"));
    let event_log_path = instance.effective_profile.join("foxy-scenario-end.txt");
    let mut events = PrimaryLog::new(event_log_path.clone());
    let identity = SessionIdentity {
        owner_id: "native_reforger_scenario_end".into(),
        owner_generation: 1,
        game_space_id: "reforger".into(),
        instance_id: policy.name.into(),
        operation_id: "scenario_end_acceptance".into(),
        run_generation: generation,
    };
    let spec = CommandSpec {
        executable: PathBuf::from(argument_path(&payload.join(executable_name()))?),
        directory: PathBuf::from(argument_path(payload)?),
        arguments: vec![
            "-server".into(),
            SCENARIO_WORLD_ARGUMENT.into(),
            "-bindIP".into(),
            "127.0.0.1".into(),
            "-bindPort".into(),
            bind_port.to_string().into(),
            "-a2sIpAddress".into(),
            "127.0.0.1".into(),
            "-a2sPort".into(),
            a2s_port.to_string().into(),
            "-profile".into(),
            argument_path(&instance.profile_parent)?.into(),
            "-addonDownloadDir".into(),
            argument_path(&instance.download_parent)?.into(),
            "-addonTempDir".into(),
            argument_path_with_trailing_separator(&instance.cache)?.into(),
            "-logsDir".into(),
            argument_path(&instance.logs)?.into(),
            "-addonsDir".into(),
            argument_path(addon_root)?.into(),
            "-addons".into(),
            addon_id.into(),
            "-nothrow".into(),
            "-maxFPS".into(),
            "30".into(),
        ]
        .into_iter()
        .chain(policy.args.iter().map(|argument| (*argument).into()))
        .collect(),
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
    if pid == 0 {
        return Err(io::Error::other(
            "Native engine returned an invalid process id",
        ));
    }
    let scenario_nonce = format!("FOXY_SCENARIO_END nonce={nonce}");
    let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut ready = false;
    let a2s_name_observed = None;
    let mut natural_exit = None;
    while Instant::now() < startup_deadline {
        capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
        primary.capture()?;
        events.capture()?;
        natural_exit = session.0.observe()?;
        ready = start_indices(&events.output, &scenario_nonce) == [1];
        if ready || natural_exit.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let start_observed_at = Instant::now();
    if !ready || natural_exit.is_some() {
        let (stop_exit, cleanup_outcome) = stop_and_wait(&mut session)?;
        capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
        primary.capture()?;
        save_phase_log(&instance.logs, &captured)?;
        let released = locks.inspect_consumers()?.is_empty();
        let a2s_observed_before_end = a2s_name_observed.is_some();
        return Ok(PhaseEvidence {
            name: policy.name,
            arguments: policy
                .args
                .iter()
                .map(|argument| (*argument).into())
                .collect(),
            nonce,
            event_log_path,
            server_world_argument: SCENARIO_WORLD_ARGUMENT,
            a2s_name_observed,
            containment_root_pid: pid,
            process_exit: natural_exit,
            stop_exit,
            process_relaunch_if_present: version_line_count(&primary.output) > 1,
            cleanup_outcome,
            end_request_observed: false,
            native_event_sequence_verified: false,
            loaded_world_observed: loaded_world_observed(&captured)
                || loaded_world_observed(&primary.output),
            scenario_ready_before_end: ready,
            a2s_observed_before_end,
            starts: start_indices(&primary.output, &scenario_nonce),
            end_request_after_start_ms: None,
            reload_after_end_request_ms: None,
            exit_after_end_request_ms: None,
            stop_after_end_request_ms: None,
            process_version_lines: version_line_count(&primary.output),
            retained_consumer_before_stop: false,
            consumer_released_after_wait: released,
            stop_won_pending_reload: false,
            no_reload: false,
            log_path: instance.logs.join("captured-scenario-end.txt"),
            inspected_log_files: cursors.keys().cloned().collect(),
        });
    }

    let end_deadline = Instant::now()
        + if policy.expect_reload {
            RELOAD_OBSERVATION
        } else {
            SCENARIO_END_TIMEOUT
        };
    let mut end_request_observed;
    let mut starts;
    let mut process_exit = natural_exit;
    let mut stop_exit = None;
    let mut cleanup_outcome = "uncertain";
    let mut stop_won_pending_reload = false;
    let mut no_reload = false;
    let retained_consumer_before_stop = locks
        .inspect_consumers()?
        .iter()
        .any(|consumer| consumer.session == identity);
    let mut end_request_at = None;
    let mut reload_after_end_request_ms = None;
    let mut exit_after_end_request_ms = None;
    let mut stop_after_end_request_ms = None;
    loop {
        if process_exit.is_none() {
            process_exit = session.0.observe()?;
        }
        capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
        primary.capture()?;
        events.capture()?;
        starts = start_indices(&events.output, &scenario_nonce);
        end_request_observed = events
            .output
            .contains(&format!("{scenario_nonce} event=END_REQUEST"));
        if end_request_observed {
            end_request_at.get_or_insert_with(Instant::now);
            if policy.expect_exit {
                if process_exit.is_some() {
                    exit_after_end_request_ms =
                        end_request_at.map(|time: Instant| time.elapsed().as_millis());
                    cleanup_outcome = "natural_exit_waited";
                    break;
                }
            } else if policy.expect_reload && starts.len() >= 2 {
                reload_after_end_request_ms =
                    end_request_at.map(|time: Instant| time.elapsed().as_millis());
                (stop_exit, cleanup_outcome) = stop_and_wait(&mut session)?;
                break;
            } else if policy.stop_during_delay {
                stop_won_pending_reload = starts.len() == 1
                    && end_request_at
                        .is_some_and(|requested| requested.elapsed() < Duration::from_secs(3));
                if stop_won_pending_reload {
                    stop_after_end_request_ms =
                        end_request_at.map(|time: Instant| time.elapsed().as_millis());
                    (stop_exit, cleanup_outcome) = stop_and_wait(&mut session)?;
                    capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
                    primary.capture()?;
                    events.capture()?;
                    starts = start_indices(&events.output, &scenario_nonce);
                    no_reload = starts.len() == 1;
                    break;
                }
            } else if policy.inspect_end_window {
                let pending_deadline = Instant::now() + Duration::from_secs(6);
                while Instant::now() < pending_deadline {
                    process_exit = session.0.observe()?;
                    capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
                    primary.capture()?;
                    events.capture()?;
                    starts = start_indices(&events.output, &scenario_nonce);
                    if process_exit.is_some() {
                        break;
                    }
                    if policy.name == "reload_zero" && starts.len() >= 2 {
                        (stop_exit, cleanup_outcome) = stop_and_wait(&mut session)?;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                if process_exit.is_none() {
                    no_reload = starts == [1];
                    (stop_exit, cleanup_outcome) = stop_and_wait(&mut session)?;
                }
                break;
            }
        }
        if Instant::now() >= end_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if cleanup_outcome == "uncertain" {
        if process_exit.is_some() {
            cleanup_outcome = "natural_exit_waited";
        } else {
            (stop_exit, cleanup_outcome) = stop_and_wait(&mut session)?;
        }
    }
    capture_log_delta(&instance.logs, &mut cursors, &mut captured)?;
    primary.capture()?;
    events.capture()?;
    starts = start_indices(&events.output, &scenario_nonce);
    let process_version_lines = version_line_count(&primary.output);
    save_phase_log(&instance.logs, &captured)?;
    let consumer_released_after_wait = locks.inspect_consumers()?.is_empty();
    let a2s_observed_before_end = a2s_name_observed.is_some();
    Ok(PhaseEvidence {
        name: policy.name,
        arguments: policy
            .args
            .iter()
            .map(|argument| (*argument).into())
            .collect(),
        nonce,
        event_log_path,
        server_world_argument: SCENARIO_WORLD_ARGUMENT,
        a2s_name_observed,
        containment_root_pid: pid,
        process_exit,
        stop_exit,
        process_relaunch_if_present: process_version_lines > 1,
        cleanup_outcome,
        end_request_observed,
        native_event_sequence_verified: event_sequence_matches(
            &events.output,
            &scenario_nonce,
            &starts,
        ) && start_indices(&primary.output, &scenario_nonce)
            == starts
            && primary
                .output
                .matches(&format!("{scenario_nonce} event=END_REQUEST"))
                .count()
                == 1
            && primary.output.contains("Entered online game state."),
        loaded_world_observed: loaded_world_observed(&captured)
            || loaded_world_observed(&primary.output),
        scenario_ready_before_end: ready,
        a2s_observed_before_end,
        starts,
        end_request_after_start_ms: end_request_at.map(|time| {
            time.saturating_duration_since(start_observed_at)
                .as_millis()
        }),
        reload_after_end_request_ms,
        exit_after_end_request_ms,
        stop_after_end_request_ms,
        process_version_lines,
        retained_consumer_before_stop,
        consumer_released_after_wait,
        stop_won_pending_reload,
        no_reload,
        log_path: instance.logs.join("captured-scenario-end.txt"),
        inspected_log_files: cursors.keys().cloned().collect(),
    })
}

fn loaded_world_observed(log: &str) -> bool {
    log.lines().any(|line| {
        line.to_ascii_lowercase().contains("entities load")
            && (line.contains(SCENARIO_WORLD)
                || line.contains(&format!("'$ArmaReforger:{SCENARIO_WORLD_ARGUMENT}'")))
    })
}

#[test]
fn native_world_resource_accepts_guid_or_virtual_filesystem_name() {
    assert!(loaded_world_observed(
        "WORLD : Entities load '$ArmaReforger:worlds/GameMaster/GM_Arland.ent'"
    ));
    assert!(loaded_world_observed(&format!(
        "WORLD : Entities load @\"{SCENARIO_WORLD}\""
    )));
    assert!(!loaded_world_observed(
        "WORLD : Entities load '$Other:worlds/GameMaster/GM_Arland.ent'"
    ));
    assert!(!loaded_world_observed(
        "WORLD : Entities load '$ArmaReforger:worlds/GameMaster/Other.ent'"
    ));
}

fn event_sequence_matches(log: &str, nonce: &str, starts: &[u32]) -> bool {
    let expected = if starts == [1] {
        vec![
            format!("{nonce} event=START index=1"),
            format!("{nonce} event=END_REQUEST"),
        ]
    } else if starts == [1, 2] {
        vec![
            format!("{nonce} event=START index=1"),
            format!("{nonce} event=END_REQUEST"),
            format!("{nonce} event=START index=2"),
        ]
    } else {
        return false;
    };
    log.lines().eq(expected.iter().map(String::as_str))
}

#[test]
fn event_sequence_requires_the_fresh_nonce_and_one_end_before_reload() {
    let nonce = "FOXY_SCENARIO_END nonce=synthetic";
    let end = format!("{nonce} event=START index=1\n{nonce} event=END_REQUEST\n");
    assert!(event_sequence_matches(&end, nonce, &[1]));
    let reload = format!("{end}{nonce} event=START index=2\n");
    assert!(event_sequence_matches(&reload, nonce, &[1, 2]));
    assert!(!event_sequence_matches(&reload, nonce, &[1]));
    assert!(!event_sequence_matches(
        &reload.replace("synthetic", "other"),
        nonce,
        &[1, 2]
    ));
    assert!(!event_sequence_matches(
        &format!("{end}{nonce} event=START index=1\n"),
        nonce,
        &[1, 1]
    ));
    assert!(!event_sequence_matches(
        &format!("{nonce} event=END_REQUEST\n{nonce} event=START index=1\n"),
        nonce,
        &[1]
    ));
}

fn version_line_count(log: &str) -> usize {
    log.lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            line.contains("initializing engine, version ")
        })
        .count()
}

fn start_indices(log: &str, nonce: &str) -> Vec<u32> {
    let prefix = format!("{nonce} event=START index=");
    log.lines()
        .filter_map(|line| line.split_once(&prefix).map(|(_, value)| value.trim()))
        .filter_map(|value| value.split_whitespace().next())
        .filter_map(|value| value.trim_end_matches('\'').parse().ok())
        .collect()
}

#[test]
fn start_indices_accept_native_print_variable_quotes() {
    let nonce = "FOXY_SCENARIO_END nonce=synthetic";
    assert_eq!(
        start_indices(
            &format!("SCRIPT : string line = '{nonce} event=START index=1'\n"),
            nonce
        ),
        [1]
    );
    assert_eq!(
        start_indices(&format!("{nonce} event=START index=2\n"), nonce),
        [2]
    );
    assert!(start_indices(&format!("{nonce} event=START index=invalid\n"), nonce).is_empty());
}

struct PrimaryLog {
    path: PathBuf,
    previous: Vec<u8>,
    output: String,
}

impl PrimaryLog {
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

#[test]
fn primary_log_preserves_replacement_and_reload_without_duplicate_streams() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("console.log");
    let initial = "ENGINE : Initializing engine, version 1\nFOXY_SCENARIO_END nonce=fixture event=START index=1\n";
    fs::write(&path, initial)?;
    let mut primary = PrimaryLog::new(path.clone());
    primary.capture()?;
    primary.capture()?;
    fs::write(
        &path,
        format!("{initial}FOXY_SCENARIO_END nonce=fixture event=START index=2\n"),
    )?;
    primary.capture()?;
    assert_eq!(version_line_count(&primary.output), 1);
    assert_eq!(
        start_indices(&primary.output, "FOXY_SCENARIO_END nonce=fixture"),
        [1, 2]
    );
    fs::write(&path, initial.replace("version 1", "version 2"))?;
    primary.capture()?;
    assert_eq!(version_line_count(&primary.output), 2);
    assert_eq!(
        start_indices(&primary.output, "FOXY_SCENARIO_END nonce=fixture"),
        [1, 2, 1]
    );
    Ok(())
}

fn stop_and_wait(session: &mut RetainedSession) -> io::Result<(Option<TreeExit>, &'static str)> {
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

fn save_phase_log(root: &Path, captured: &str) -> io::Result<()> {
    fs::write(root.join("captured-scenario-end.txt"), captured.as_bytes())
}
