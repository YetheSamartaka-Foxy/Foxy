use foxy_supervisor::leases::InstallationLocks;
#[cfg(windows)]
use foxy_supervisor::process::ConsoleSignalHelper;
use foxy_supervisor::process::{CommandSpec, TreeExit};
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[path = "layout/addons.rs"]
mod addons;
#[path = "layout/files.rs"]
mod files;
#[path = "layout/logs.rs"]
mod logs;
#[path = "layout/network.rs"]
mod network;
#[path = "layout/paths.rs"]
mod paths;

#[cfg(target_os = "linux")]
use files::check_linux_prerequisites;
use files::{copy_tree, executable_name, input_directory, private_directory, snapshot_tree};
use logs::{capture_log_delta, generation_proof};
pub(super) use network::query_a2s_hostname;
pub(super) const SCENARIO_WORLD: &str = "{77C41EA429B6AC35}worlds/GameMaster/GM_Arland.ent";
use paths::{
    argument_path, argument_path_with_trailing_separator, fresh_token, is_under,
    private_environment,
};

const SCENARIO: &str = "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(90);
const SOFT_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct TreeEntry {
    path: String,
    kind: &'static str,
    size: u64,
    blake3: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct RequestedPaths {
    payload: PathBuf,
    config: PathBuf,
    project: PathBuf,
    private_environment_roots: Vec<PathBuf>,
    profile_parent: PathBuf,
    effective_profile: PathBuf,
    download_parent: PathBuf,
    cache_with_separator: String,
    logs: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct GenerationEvidence {
    instance: String,
    generation: u64,
    hostname: String,
    requested_paths: RequestedPaths,
    bind_port: u16,
    a2s_port: u16,
    natural_exit: Option<TreeExit>,
    stop_exit: Option<TreeExit>,
    cleanup_outcome: String,
    soft_stop_error: Option<String>,
    waited_tree_cleanup: bool,
    deadline_reached: bool,
    readiness_before_stop: bool,
    a2s_observed_before_stop: bool,
    a2s_hostname_observation: Option<String>,
    scenario_observed: bool,
    online_game_observed: bool,
    hostname_observed: bool,
    version_line: Option<String>,
    resolved_profile_line: Option<String>,
    launch_paths_observed: bool,
    inspected_log_files: Vec<String>,
    instance_effects: Vec<TreeEntry>,
    effects_outside_mutable_roots: Vec<String>,
    marker_preserved: Option<bool>,
}

#[derive(Serialize)]
struct LayoutReport {
    schema_version: u32,
    probe: &'static str,
    result: &'static str,
    source_unchanged: bool,
    copied_payload_unchanged: bool,
    source_changed_paths: Vec<String>,
    copied_payload_changed_paths: Vec<String>,
    source_inventory_before: Vec<TreeEntry>,
    source_inventory_after: Vec<TreeEntry>,
    copied_inventory_before: Vec<TreeEntry>,
    copied_inventory_after: Vec<TreeEntry>,
    generations: Vec<GenerationEvidence>,
    report_path: PathBuf,
}

struct Instance {
    name: &'static str,
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    profile_parent: PathBuf,
    effective_profile: PathBuf,
    download_parent: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    private_environment_roots: Vec<PathBuf>,
}

struct RetainedSession(ForegroundSession);

impl Drop for RetainedSession {
    fn drop(&mut self) {
        if self.0.event().exit.is_none() {
            let _ = self.0.force_stop(FORCE_STOP_TIMEOUT);
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
            "Native source and private scratch must be separate",
        ));
    }
    #[cfg(target_os = "linux")]
    check_linux_prerequisites(&scratch)?;

    let evidence_root = tempfile::Builder::new()
        .prefix("reforger-layout-")
        .tempdir_in(&scratch)?
        .keep();
    private_directory(&evidence_root)?;
    let source_inventory_before = snapshot_tree(&source)?;
    let payload = evidence_root.join("payload");
    fs::create_dir(&payload)?;
    copy_tree(&source, &payload)?;
    let copied_inventory_before = snapshot_tree(&payload)?;
    if source_inventory_before != copied_inventory_before {
        return Err(io::Error::other(
            "Copied Reforger payload differs from read-only source",
        ));
    }
    let executable = payload.join(executable_name());
    let project = payload.join("addons/data/ArmaReforger.gproj");
    if !executable.is_file() || !project.is_file() {
        return Err(io::Error::other(
            "Copied payload is missing its dedicated executable or data project",
        ));
    }

    let source_locks = InstallationLocks::capture_in(&source, &evidence_root.join("source-locks"))?;
    let _source_runtime = source_locks.try_admit()?.try_runtime()?;
    let locks = InstallationLocks::capture_in(&payload, &evidence_root.join("runtime-locks"))?;
    let alpha = create_instance(&evidence_root, "alpha")?;
    let beta = create_instance(&evidence_root, "beta")?;
    let mut generations = Vec::new();

    let first_alpha = run_generation(&payload, &executable, &project, &locks, &alpha, 1)?;
    let alpha_marker = alpha.effective_profile.join("foxy-private-layout.marker");
    let marker_value = format!("{}-{}", fresh_token(), first_alpha.hostname);
    let marker_written = first_alpha.waited_tree_cleanup
        && first_alpha.resolved_profile_line.is_some()
        && alpha.effective_profile.is_dir();
    if marker_written {
        fs::write(&alpha_marker, marker_value.as_bytes())?;
    }
    generations.push(first_alpha);

    if generations[0].waited_tree_cleanup {
        generations.push(run_generation(
            &payload,
            &executable,
            &project,
            &locks,
            &beta,
            2,
        )?);
    }
    if generations.len() == 2 && generations[1].waited_tree_cleanup && marker_written {
        let mut restarted = run_generation(&payload, &executable, &project, &locks, &alpha, 3)?;
        let preserved = fs::read(&alpha_marker)
            .map(|contents| contents == marker_value.as_bytes())
            .unwrap_or(false);
        restarted.marker_preserved = Some(preserved);
        generations.push(restarted);
    }

    let source_inventory_after = snapshot_tree(&source)?;
    let copied_inventory_after = snapshot_tree(&payload)?;
    let source_unchanged = source_inventory_before == source_inventory_after;
    let copied_payload_unchanged = copied_inventory_before == copied_inventory_after;
    let source_changed_paths = changed_paths(&source_inventory_before, &source_inventory_after);
    let copied_payload_changed_paths =
        changed_paths(&copied_inventory_before, &copied_inventory_after);
    let every_generation_observed = generations.len() == 3
        && generations.iter().all(generation_observed)
        && generations[0].hostname != generations[1].hostname
        && generations[0].hostname != generations[2].hostname
        && generations[1].hostname != generations[2].hostname
        && generations[0].requested_paths.effective_profile
            != generations[1].requested_paths.effective_profile
        && generations[2].marker_preserved == Some(true);
    let report_path = evidence_root.join("report.json");
    let report = LayoutReport {
        schema_version: 1,
        probe: "reforger_native_layout",
        result: if every_generation_observed && source_unchanged && copied_payload_unchanged {
            "observed"
        } else {
            "incomplete"
        },
        source_unchanged,
        copied_payload_unchanged,
        source_changed_paths,
        copied_payload_changed_paths,
        source_inventory_before,
        source_inventory_after,
        copied_inventory_before,
        copied_inventory_after,
        generations,
        report_path: report_path.clone(),
    };
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("Private native layout evidence: {}", report_path.display());
    if report.result != "observed" {
        return Err(io::Error::other(
            "Native layout did not observe all isolated generations and restart evidence",
        ));
    }
    Ok(())
}

pub(super) fn run_addons() -> io::Result<()> {
    addons::run()
}

fn create_instance(root: &Path, name: &'static str) -> io::Result<Instance> {
    let instance_root = root.join(format!("instance-{name}"));
    let home = instance_root.join("home");
    let profile_parent = home.join("profiles");
    let download_parent = home.join("data");
    let cache = home.join("cache");
    let logs = home.join("runtime");
    let environment_root = home.join("environment");
    let private_environment_roots = vec![
        environment_root.join("config"),
        environment_root.join("data"),
        environment_root.join("cache"),
        environment_root.join("runtime"),
        environment_root.join("temp"),
    ];
    #[cfg(windows)]
    let private_environment_roots = {
        let mut roots = private_environment_roots;
        roots.extend([
            environment_root.join("roaming"),
            environment_root.join("local"),
        ]);
        roots
    };
    let mut directories = vec![
        &instance_root,
        &home,
        &environment_root,
        &profile_parent,
        &download_parent,
        &cache,
        &logs,
    ];
    directories.extend(private_environment_roots.iter());
    for directory in directories {
        fs::create_dir(directory)?;
    }
    private_directory(&instance_root)?;
    private_directory(&home)?;
    for directory in &private_environment_roots {
        private_directory(directory)?;
    }
    Ok(Instance {
        name,
        root: instance_root.clone(),
        home,
        config: instance_root.join("server.json"),
        profile_parent: profile_parent.clone(),
        effective_profile: profile_parent.join("profile"),
        download_parent,
        cache,
        logs,
        private_environment_roots,
    })
}

fn run_generation(
    payload: &Path,
    executable: &Path,
    project: &Path,
    locks: &InstallationLocks,
    instance: &Instance,
    generation: u64,
) -> io::Result<GenerationEvidence> {
    let hostname = format!(
        "FoxyRefLayout_{}_{}_{}",
        instance.name,
        generation,
        fresh_token()
    );
    let mut ports = HashSet::new();
    let bind_port = loopback_udp_port(&mut ports)?;
    let a2s_port = loopback_udp_port(&mut ports)?;
    let configuration = json!({
        "bindAddress": "127.0.0.1",
        "bindPort": bind_port,
        "a2s": {"address": "127.0.0.1", "port": a2s_port},
        "game": {
            "name": hostname.clone(),
            "maxPlayers": 4,
            "visible": false,
            "mods": [],
            "scenarioId": SCENARIO
        }
    });
    fs::write(&instance.config, serde_json::to_vec_pretty(&configuration)?)?;
    let stdout_path = instance
        .logs
        .join(format!("engine-{}.stdout.log", generation));
    let stdout = File::create(&stdout_path)?;
    let mut cursors = BTreeMap::new();
    let mut captured_logs = String::new();
    capture_log_delta(&instance.logs, &mut cursors, &mut captured_logs)?;
    captured_logs.clear();

    let identity = SessionIdentity {
        owner_id: "native_reforger_layout".into(),
        owner_generation: 1,
        game_space_id: "reforger".into(),
        instance_id: instance.name.into(),
        operation_id: "native_layout_probe".into(),
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
        ],
        environment: private_environment(&instance.home, &instance.private_environment_roots),
    };
    let admission = locks.try_admit()?;
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
        arguments: vec![
            "server".into(),
            "console-signal".into(),
            "--ctrl-break".into(),
        ],
    };
    #[cfg(windows)]
    let signal_helper = Some(&signal_helper);

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
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut deadline_reached = false;
    let mut natural_exit = None;
    let mut a2s_hostname_observation = None;
    loop {
        if let Some(exit) = session.0.observe()? {
            natural_exit = Some(exit);
            break;
        }
        capture_log_delta(&instance.logs, &mut cursors, &mut captured_logs)?;
        if query_a2s_hostname(a2s_port).as_deref() == Some(hostname.as_str()) {
            a2s_hostname_observation = Some(hostname.clone());
        }
        if generation_proof(&captured_logs, &hostname, &paths).is_complete()
            && a2s_hostname_observation.is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            deadline_reached = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let a2s_observed_before_stop = a2s_hostname_observation.is_some();
    let ready_before_stop = generation_proof(&captured_logs, &hostname, &paths).is_complete()
        && a2s_observed_before_stop;
    #[cfg(windows)]
    let helper = signal_helper;
    #[cfg(target_os = "linux")]
    let helper = None;
    let mut soft_stop_error = None;
    let (stop_exit, cleanup_outcome) = if let Some(exit) = natural_exit {
        (Some(exit), "natural_exit")
    } else {
        let stop = session.0.deliver_soft_stop(helper);
        soft_stop_error = stop.as_ref().err().map(ToString::to_string);
        if let Ok(Some(exit)) = session.0.wait(SOFT_STOP_TIMEOUT) {
            (Some(exit), "soft_stop_waited")
        } else if let Some(exit) = session.0.force_stop(FORCE_STOP_TIMEOUT)? {
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
    let waited_tree_cleanup = stop_exit.is_some();
    capture_log_delta(&instance.logs, &mut cursors, &mut captured_logs)?;
    fs::write(
        instance
            .logs
            .join(format!("captured-generation-{generation}.txt")),
        captured_logs.as_bytes(),
    )?;
    let proof = generation_proof(&captured_logs, &hostname, &paths);
    let instance_effects = snapshot_tree(&instance.root)?;
    let effects_outside_mutable_roots = instance_effects
        .iter()
        .filter(|entry| {
            let allowed = ["home/profiles", "home/data", "home/cache", "home/runtime"];
            entry.path != "server.json"
                && entry.path != "home"
                && entry.path != "home/environment"
                && !allowed.iter().any(|root| is_under(&entry.path, root))
                && !instance.private_environment_roots.iter().any(|root| {
                    root.strip_prefix(&instance.root)
                        .ok()
                        .map(|path| path.to_string_lossy().replace('\\', "/"))
                        .is_some_and(|root| is_under(&entry.path, &root))
                })
        })
        .map(|entry| entry.path.clone())
        .collect();
    let inspected_log_files = cursors.keys().cloned().collect();
    Ok(GenerationEvidence {
        instance: instance.name.to_owned(),
        generation,
        hostname,
        requested_paths: paths,
        bind_port,
        a2s_port,
        natural_exit,
        stop_exit,
        cleanup_outcome: cleanup_outcome.to_owned(),
        soft_stop_error,
        waited_tree_cleanup,
        deadline_reached,
        readiness_before_stop: ready_before_stop,
        a2s_observed_before_stop,
        a2s_hostname_observation,
        scenario_observed: proof.scenario_observed,
        online_game_observed: proof.online_game_observed,
        hostname_observed: proof.hostname_observed || a2s_observed_before_stop,
        version_line: proof.version_line.map(str::to_owned),
        resolved_profile_line: proof.profile_line.map(str::to_owned),
        launch_paths_observed: proof.launch_paths_observed,
        inspected_log_files,
        instance_effects,
        effects_outside_mutable_roots,
        marker_preserved: None,
    })
}

fn generation_observed(generation: &GenerationEvidence) -> bool {
    generation.waited_tree_cleanup
        && generation.readiness_before_stop
        && !generation.deadline_reached
        && generation.online_game_observed
        && generation.scenario_observed
        && generation.hostname_observed
        && generation.version_line.is_some()
        && generation.resolved_profile_line.is_some()
        && generation.launch_paths_observed
        && generation.a2s_observed_before_stop
        && generation.effects_outside_mutable_roots.is_empty()
}

fn changed_paths(before: &[TreeEntry], after: &[TreeEntry]) -> Vec<String> {
    before
        .iter()
        .filter(|entry| !after.contains(entry))
        .map(|entry| entry.path.clone())
        .chain(
            after
                .iter()
                .filter(|entry| !before.contains(entry))
                .map(|entry| entry.path.clone()),
        )
        .collect()
}

fn loopback_udp_port(used: &mut HashSet<u16>) -> io::Result<u16> {
    for _ in 0..32 {
        let socket = UdpSocket::bind(("127.0.0.1", 0))?;
        let port = socket.local_addr()?.port();
        drop(socket);
        if used.insert(port) {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "Could not select distinct loopback UDP ports",
    ))
}
