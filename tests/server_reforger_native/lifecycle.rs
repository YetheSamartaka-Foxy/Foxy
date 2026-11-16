use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

#[path = "lifecycle/inventory.rs"]
mod inventory;
#[path = "lifecycle/proof.rs"]
mod proof;
#[path = "lifecycle/setup.rs"]
mod setup;
#[path = "lifecycle/transport.rs"]
mod transport;

const SCENARIO: &str = "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf";
const ENGINE_VERSION: &str = "1.9.0.37";
const HOST_TIMEOUT: Duration = Duration::from_secs(180);

pub(super) fn run() -> io::Result<()> {
    run_inner(false)
}

pub(super) fn run_addons() -> io::Result<()> {
    run_inner(true)
}

fn run_inner(with_addons: bool) -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other(
            "Native process containment is unavailable",
        ));
    }
    let source = inventory::required_directory("FOXY_REFORGER_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory("FOXY_REFORGER_NATIVE_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and scratch must be separate",
        ));
    }
    #[cfg(target_os = "linux")]
    inventory::check_linux_prerequisites(&scratch)?;

    let case = inventory::fresh_child(&scratch)?;
    inventory::private_directory(&case)?;
    #[cfg(windows)]
    if case.to_string_lossy().len() > 60 {
        return Err(io::Error::other(
            "Native scratch path is too long for Windows",
        ));
    }
    let source_before = inventory::snapshot(&source)?;
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;

    let first_ports = inventory::udp_ports(2)?;
    let peer_ports = inventory::udp_ports_excluding(2, &first_ports)?;
    let nonce = case
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Native case has no identity"))?;
    let first_name = format!("FoxyRefAlpha_{nonce}");
    let peer_name = format!("FoxyRefBeta_{nonce}");
    let first = setup::prepare_instance(
        &config,
        &case,
        &source,
        &first_name,
        first_ports[0],
        first_ports[1],
        with_addons,
    )?;
    let peer = setup::prepare_instance(
        &config,
        &case,
        &source,
        &peer_name,
        peer_ports[0],
        peer_ports[1],
        false,
    )?;
    if first.installation_id != peer.installation_id
        || first.runtime_id == peer.runtime_id
        || first.profile_root == peer.profile_root
        || first.server_config == peer.server_config
    {
        return Err(io::Error::other(
            "Reforger instances do not have independent prepared state roots",
        ));
    }
    proof::verify_server_config(&first, first_ports[0], first_ports[1])?;
    proof::verify_server_config(&peer, peer_ports[0], peer_ports[1])?;
    if fs::read_dir(&first.engine_log_root)?.next().is_some()
        || fs::read_dir(&peer.engine_log_root)?.next().is_some()
    {
        return Err(io::Error::other(
            "Fresh Reforger runtime already contains engine logs",
        ));
    }

    let marker = b"foxy-reforger-profile-retention-v1";
    fs::create_dir(&first.profile_root)?;
    inventory::private_directory(&first.profile_root)?;
    fs::create_dir(&peer.profile_root)?;
    inventory::private_directory(&peer.profile_root)?;
    let first_marker = first.profile_root.join("foxy-native-marker.bin");
    let peer_marker = peer.profile_root.join("foxy-native-marker.bin");
    fs::write(&first_marker, marker)?;
    fs::write(&peer_marker, marker)?;
    let engine_before = inventory::snapshot(&first.engine_root)?;
    let peer_engine_before = inventory::snapshot(&peer.engine_root)?;

    let owner = transport::start_owner(&config)?;
    let first_start_plan = proof::start_plan(&config, &first.id)?;
    proof::verify_start_plan(&first_start_plan, &first)?;
    proof::check_start_gates(&config, &first.id, &first_start_plan)?;
    let mut run_evidence = Vec::new();
    let baseline = proof::log_baseline(&first)?;
    proof::start(&config, &first.id, &first_start_plan)?;
    let first_run = proof::owned_running(&config, &first.id)?;
    run_evidence.push(proof::wait_for_native_ready(
        &first,
        &first_run,
        &baseline,
        HOST_TIMEOUT,
    )?);
    record_run_evidence(&case, &run_evidence)?;

    proof::stop(&config, &first.id, first_run.generation)?;
    proof::verify_stopped(&config, &first.id, &first_marker, marker)?;

    let second_start_plan = proof::start_plan(&config, &first.id)?;
    let baseline = proof::log_baseline(&first)?;
    proof::start(&config, &first.id, &second_start_plan)?;
    let second_run = proof::owned_running(&config, &first.id)?;
    run_evidence.push(proof::wait_for_native_ready(
        &first,
        &second_run,
        &baseline,
        HOST_TIMEOUT,
    )?);
    record_run_evidence(&case, &run_evidence)?;
    let baseline = proof::log_baseline(&first)?;
    proof::restart(&config, &first.id, second_run.generation)?;
    let restarted = proof::wait_for_new_run(&config, &first.id, second_run.generation)?;
    run_evidence.push(proof::wait_for_native_ready(
        &first,
        &restarted,
        &baseline,
        HOST_TIMEOUT,
    )?);
    record_run_evidence(&case, &run_evidence)?;
    proof::verify_marker(&first_marker, marker)?;
    proof::stop(&config, &first.id, restarted.generation)?;
    proof::verify_stopped(&config, &first.id, &first_marker, marker)?;

    let peer_status = transport::invoke(&config, &["server", "instance", "status", &peer.id], 0)?;
    if peer_status["data"]["status"]["phase"] != "not_owned"
        || peer_status["data"]["status"]["process_exists"] != false
    {
        return Err(io::Error::other(
            "Unstarted peer unexpectedly owns a process",
        ));
    }
    proof::verify_marker(&peer_marker, marker)?;
    proof::assert_snapshot(&peer.engine_root, &peer_engine_before)?;

    owner.finish()?;
    for instance in [&first, &peer] {
        let (code, _) =
            transport::invoke_raw(&config, &["server", "instance", "status", &instance.id])?;
        if code != 4 {
            return Err(io::Error::other(
                "Closed owner still exposes authenticated runtime status",
            ));
        }
    }
    let consumers = transport::invoke(
        &config,
        &[
            "server",
            "installation",
            "consumers",
            &first.installation_id,
        ],
        0,
    )?;
    if consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_none_or(|records| !records.is_empty())
    {
        return Err(io::Error::other(
            "Witnessed owner shutdown left installation registrations",
        ));
    }
    if inventory::snapshot(&source)? != source_before {
        return Err(io::Error::other(
            "Adopted Reforger source changed during lifecycle",
        ));
    }
    proof::assert_snapshot(&first.engine_root, &engine_before)?;
    proof::assert_snapshot(&peer.engine_root, &peer_engine_before)?;
    Ok(())
}

fn record_run_evidence(case: &Path, evidence: &[proof::RunEvidence]) -> io::Result<()> {
    fs::write(
        case.join("native-run-evidence.json"),
        serde_json::to_vec_pretty(evidence)?,
    )
}
