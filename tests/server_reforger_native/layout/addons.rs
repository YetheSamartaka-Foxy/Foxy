use super::*;
use serde::Serialize;
use std::ffi::OsStr;

const ADDON_TIMEOUT: Duration = Duration::from_secs(240);
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_METADATA_FILES: usize = 64;

#[derive(Serialize)]
struct AddonReport {
    schema_version: u32,
    probe: &'static str,
    result: &'static str,
    requested_addon_id: String,
    requested_version: Option<String>,
    source_unchanged: bool,
    copied_payload_unchanged: bool,
    readiness_before_stop: bool,
    a2s_hostname_observed: bool,
    graceful_waited_cleanup: bool,
    cleanup_outcome: String,
    deadline_reached: bool,
    source_changed_paths: Vec<String>,
    copied_payload_changed_paths: Vec<String>,
    download_inventory: Vec<TreeEntry>,
    server_data_json: Vec<MetadataArtifact>,
    requested_metadata_observed: bool,
    report_path: PathBuf,
}

#[derive(Serialize)]
struct MetadataArtifact {
    relative_path: String,
    bytes: u64,
    blake3: String,
    json: serde_json::Value,
}

pub(super) fn run() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other(
            "Native process containment is unavailable",
        ));
    }
    let addon_id = std::env::var("FOXY_REFORGER_ADDON_ID")
        .map_err(|_| io::Error::other("FOXY_REFORGER_ADDON_ID must be set explicitly"))?;
    if addon_id.trim().is_empty() {
        return Err(io::Error::other("FOXY_REFORGER_ADDON_ID must not be empty"));
    }
    if addon_id.len() != 16 || !addon_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other(
            "FOXY_REFORGER_ADDON_ID must be exactly 16 hexadecimal characters",
        ));
    }
    let requested_version = std::env::var("FOXY_REFORGER_ADDON_VERSION").ok();
    if requested_version.as_deref().is_some_and(|version| {
        version.is_empty()
            || version.len() > 128
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    }) {
        return Err(io::Error::other(
            "FOXY_REFORGER_ADDON_VERSION must be 1-128 ASCII letters, digits, dots, underscores, or hyphens",
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

    let root = tempfile::Builder::new()
        .prefix("reforger-addon-acquisition-")
        .tempdir_in(&scratch)?
        .keep();
    private_directory(&root)?;
    let source_locks = InstallationLocks::capture_in(&source, &root.join("source-locks"))?;
    let _source_runtime = source_locks.try_admit()?.try_runtime()?;
    let source_before = snapshot_tree(&source)?;
    let payload = root.join("payload");
    fs::create_dir(&payload)?;
    copy_tree(&source, &payload)?;
    let copied_before = snapshot_tree(&payload)?;
    if source_before != copied_before {
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
    let locks = InstallationLocks::capture_in(&payload, &root.join("runtime-locks"))?;
    let instance = create_instance(&root, "addon")?;
    let (evidence, inventory, metadata) = run_acquisition(
        &payload,
        &executable,
        &project,
        &locks,
        &instance,
        &addon_id,
        requested_version.as_deref(),
    )?;
    let source_after = snapshot_tree(&source)?;
    let copied_after = snapshot_tree(&payload)?;
    let source_unchanged = source_before == source_after;
    let copied_payload_unchanged = copied_before == copied_after;
    let requested_metadata_observed = metadata
        .iter()
        .any(|artifact| metadata_matches(&artifact.json, &addon_id, requested_version.as_deref()));
    let complete = evidence.readiness_before_stop
        && evidence.a2s_observed_before_stop
        && evidence.waited_tree_cleanup
        && !evidence.deadline_reached
        && source_unchanged
        && copied_payload_unchanged
        && requested_metadata_observed
        && evidence.cleanup_outcome == "soft_stop_waited";
    let report_path = root.join("report.json");
    let report = AddonReport {
        schema_version: 1,
        probe: "reforger_native_addon_acquisition",
        result: if complete { "observed" } else { "incomplete" },
        requested_addon_id: addon_id,
        requested_version,
        source_unchanged,
        copied_payload_unchanged,
        readiness_before_stop: evidence.readiness_before_stop,
        a2s_hostname_observed: evidence.a2s_observed_before_stop,
        graceful_waited_cleanup: evidence.cleanup_outcome == "soft_stop_waited",
        cleanup_outcome: evidence.cleanup_outcome,
        deadline_reached: evidence.deadline_reached,
        source_changed_paths: changed_paths(&source_before, &source_after),
        copied_payload_changed_paths: changed_paths(&copied_before, &copied_after),
        download_inventory: inventory,
        server_data_json: metadata,
        requested_metadata_observed,
        report_path: report_path.clone(),
    };
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "Private addon acquisition evidence: {}",
        report_path.display()
    );
    if !complete {
        return Err(io::Error::other(
            "Addon acquisition did not meet native observation criteria",
        ));
    }
    Ok(())
}

fn run_acquisition(
    payload: &Path,
    executable: &Path,
    project: &Path,
    locks: &InstallationLocks,
    instance: &Instance,
    addon_id: &str,
    version: Option<&str>,
) -> io::Result<(GenerationEvidence, Vec<TreeEntry>, Vec<MetadataArtifact>)> {
    let hostname = format!("FoxyRefAddon_{}", fresh_token());
    let mut ports = HashSet::new();
    let bind_port = loopback_udp_port(&mut ports)?;
    let a2s_port = loopback_udp_port(&mut ports)?;
    let mut addon = serde_json::Map::new();
    addon.insert("modId".into(), addon_id.into());
    if let Some(version) = version {
        addon.insert("version".into(), version.into());
    }
    let configuration = json!({
        "bindAddress": "127.0.0.1", "bindPort": bind_port,
        "a2s": {"address": "127.0.0.1", "port": a2s_port},
        "game": {"name": hostname, "maxPlayers": 4, "visible": false,
            "mods": [addon], "scenarioId": SCENARIO}
    });
    fs::write(&instance.config, serde_json::to_vec_pretty(&configuration)?)?;
    let stdout_path = instance.logs.join("engine-addon.stdout.log");
    let stdout = File::create(&stdout_path)?;
    let mut cursors = BTreeMap::new();
    let mut captured_logs = String::new();
    capture_log_delta(&instance.logs, &mut cursors, &mut captured_logs)?;
    captured_logs.clear();
    let identity = SessionIdentity {
        owner_id: "native_reforger_addon_probe".into(),
        owner_generation: 1,
        game_space_id: "reforger".into(),
        instance_id: instance.name.into(),
        operation_id: "native_addon_acquisition".into(),
        run_generation: 1,
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
    #[cfg(target_os = "linux")]
    let signal_helper = None;
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
    let deadline = Instant::now() + ADDON_TIMEOUT;
    let mut deadline_reached = false;
    let mut a2s_observation = None;
    let mut natural_exit = None;
    let mut proof_scenario = false;
    let mut proof_online = false;
    let mut proof_hostname = false;
    let mut proof_version = None;
    let mut proof_profile = None;
    let mut proof_paths = false;
    let mut proof_complete = false;
    loop {
        if let Some(exit) = session.0.observe()? {
            natural_exit = Some(exit);
            break;
        }
        capture_log_delta(&instance.logs, &mut cursors, &mut captured_logs)?;
        if query_a2s_hostname(a2s_port).as_deref() == Some(hostname.as_str()) {
            a2s_observation = Some(hostname.clone());
        }
        let proof = generation_proof(&captured_logs, &hostname, &paths);
        proof_scenario = proof.scenario_observed;
        proof_online = proof.online_game_observed;
        proof_hostname = proof.hostname_observed;
        proof_version = proof.version_line.map(str::to_owned);
        proof_profile = proof.profile_line.map(str::to_owned);
        proof_paths = proof.launch_paths_observed;
        proof_complete = proof.is_complete();
        if proof_complete
            && a2s_observation.is_some()
            && !snapshot_tree(&instance.download_parent)?.is_empty()
        {
            break;
        }
        if Instant::now() >= deadline {
            deadline_reached = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let a2s_observed = a2s_observation.is_some();
    let readiness = proof_complete && a2s_observed;
    let (stop_exit, cleanup_outcome) = if let Some(exit) = natural_exit {
        (Some(exit), "natural_exit")
    } else {
        let _ = session.0.deliver_soft_stop(signal_helper);
        if let Ok(Some(exit)) = session.0.wait(SOFT_STOP_TIMEOUT) {
            (Some(exit), "soft_stop_waited")
        } else if let Some(exit) = session.0.force_stop(FORCE_STOP_TIMEOUT)? {
            (
                Some(exit),
                if exit.forced {
                    "forced_waited"
                } else {
                    "waited_after_force_request"
                },
            )
        } else {
            (None, "cleanup_uncertain")
        }
    };
    capture_log_delta(&instance.logs, &mut cursors, &mut captured_logs)?;
    let download_inventory = snapshot_tree(&instance.download_parent)?;
    let metadata = read_server_data(&instance.download_parent)?;
    let evidence = GenerationEvidence {
        instance: instance.name.to_owned(),
        generation: 1,
        hostname,
        requested_paths: paths,
        bind_port,
        a2s_port,
        natural_exit,
        stop_exit,
        cleanup_outcome: cleanup_outcome.to_owned(),
        soft_stop_error: None,
        waited_tree_cleanup: cleanup_outcome != "cleanup_uncertain",
        deadline_reached,
        readiness_before_stop: readiness,
        a2s_observed_before_stop: a2s_observed,
        a2s_hostname_observation: a2s_observation,
        scenario_observed: proof_scenario,
        online_game_observed: proof_online,
        hostname_observed: proof_hostname || a2s_observed,
        version_line: proof_version,
        resolved_profile_line: proof_profile,
        launch_paths_observed: proof_paths,
        inspected_log_files: cursors.keys().cloned().collect(),
        instance_effects: snapshot_tree(&instance.root)?,
        effects_outside_mutable_roots: Vec::new(),
        marker_preserved: None,
    };
    Ok((evidence, download_inventory, metadata))
}

fn metadata_matches(value: &serde_json::Value, addon_id: &str, version: Option<&str>) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let id_matches = object
        .get("id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(addon_id));
    let Some(revision) = object
        .get("revision")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let version_matches = version.is_none_or(|requested| {
        revision
            .get("version")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == requested)
    });
    let not_corrupted = revision
        .get("corrupted")
        .and_then(serde_json::Value::as_bool)
        == Some(false);
    id_matches && version_matches && not_corrupted
}

fn parse_server_data(bytes: &[u8]) -> io::Result<serde_json::Value> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    serde_json::from_slice(bytes).map_err(io::Error::other)
}

fn read_server_data(root: &Path) -> io::Result<Vec<MetadataArtifact>> {
    let mut stack = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() && !metadata.is_dir() {
                return Err(io::Error::other(
                    "Addon download tree contains a link or special entry",
                ));
            }
            if metadata.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if entry.file_name() != OsStr::new("ServerData.json") {
                continue;
            }
            if found.len() >= MAX_METADATA_FILES || metadata.len() > MAX_METADATA_BYTES {
                return Err(io::Error::other(
                    "Addon metadata exceeded bounded report limits",
                ));
            }
            let bytes = fs::read(entry.path())?;
            let json = parse_server_data(&bytes)?;
            found.push(MetadataArtifact {
                relative_path: entry
                    .path()
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                bytes: bytes.len() as u64,
                blake3: blake3::hash(&bytes).to_hex().to_string(),
                json,
            });
        }
    }
    found.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_ID: &str = "A1B2C3D4E5F60718";

    #[test]
    fn server_data_requires_exact_root_identity_revision_and_integrity() {
        let valid = json!({
            "id": FIXTURE_ID,
            "revision": {"version": "1.4.3", "corrupted": false, "dependencies": [], "scenarios": [], "env": 7}
        });
        assert!(metadata_matches(&valid, FIXTURE_ID, Some("1.4.3")));
        assert!(!metadata_matches(&valid, FIXTURE_ID, Some("1.4.4")));

        let nested_identity = json!({
            "id": "FFFFFFFFFFFFFFFF",
            "revision": {"version": "1.4.3", "corrupted": false},
            "dependency": {"id": FIXTURE_ID}
        });
        assert!(!metadata_matches(
            &nested_identity,
            FIXTURE_ID,
            Some("1.4.3")
        ));

        let corrupted = json!({
            "id": FIXTURE_ID,
            "revision": {"version": "1.4.3", "corrupted": true}
        });
        assert!(!metadata_matches(&corrupted, FIXTURE_ID, Some("1.4.3")));
    }

    #[test]
    fn server_data_parser_accepts_vendor_utf8_bom() {
        let bytes = b"\xEF\xBB\xBF{\"id\":\"A1B2C3D4E5F60718\"}";
        assert_eq!(parse_server_data(bytes).unwrap()["id"], FIXTURE_ID);
    }
}
