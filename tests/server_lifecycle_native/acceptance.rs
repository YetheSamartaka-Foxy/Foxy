use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[path = "inventory.rs"]
mod inventory;
#[path = "observation.rs"]
mod observation;
#[path = "setup.rs"]
mod setup;
#[path = "transport.rs"]
mod transport;

#[path = "headless.rs"]
pub(super) mod headless;
#[path = "headless_lifecycle.rs"]
pub(super) mod headless_lifecycle;
#[path = "../server_activation_native/preparation.rs"]
mod preparation;
#[path = "proof.rs"]
mod proof;
use preparation::{PreparationRevision, prepare, setup_content};
use setup::{commit_runtime, configure, select_predecessor, update_configuration};

const GAME: &str = "arma3";
const MISSION: &str = "foxy_native.vr";

pub(super) fn run() -> io::Result<()> {
    let source = inventory::required_directory("FOXY_ARMA_NATIVE_SOURCE")?;
    let scratch = inventory::required_directory("FOXY_ARMA_LIFECYCLE_SCRATCH")?;
    if source.starts_with(&scratch) || scratch.starts_with(&source) {
        return Err(io::Error::other(
            "Native source and lifecycle scratch must be separate",
        ));
    }
    let case = inventory::fresh_child(&scratch)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&case, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    if case.to_string_lossy().len() > 60 {
        return Err(io::Error::other(
            "Lifecycle scratch path is too long for native Windows arguments",
        ));
    }
    let source_hash = inventory::tree_hash(&source)?;
    let adopted = case.join("adopted");
    inventory::copy_tree(&source, &adopted)?;
    if inventory::tree_hash(&source)? != source_hash {
        return Err(io::Error::other("Native source changed during copy"));
    }
    let adopted_hash = inventory::tree_hash(&adopted)?;
    if source_hash != adopted_hash {
        return Err(io::Error::other(
            "Adopted source copy does not match source",
        ));
    }
    let config = case.join("config");
    fs::create_dir(&config)?;
    fs::write(
        config.join("games.json"),
        include_bytes!("../../examples/json/appdata/games.json"),
    )?;
    let id_value = invoke_cli(
        &config,
        &["server", "instance", "add", "--name", "LifecycleNative"],
        0,
    )?;
    let id = string(&id_value["data"]["instance"]["id"], "instance id")?.to_owned();
    let first_identity = format!("FoxyNativeAlpha_{:x}", nonce());
    require_port_range_free(27102)?;
    require_port_range_free(27112)?;
    require_port_range_free(27122)?;
    configure(&config, &case, &id, &adopted, 27102, &first_identity)?;
    let installation = string(
        &invoke_cli(&config, &["server", "instance", "show", &id], 0)?["data"]["instance"]["installation_id"],
        "installation id",
    )?.to_owned();
    setup_content(&config, &case, &id)?;
    let peer_added = invoke_cli(
        &config,
        &["server", "instance", "add", "--name", "LifecyclePeer"],
        0,
    )?;
    let peer_id = string(&peer_added["data"]["instance"]["id"], "peer instance id")?.to_owned();
    let peer_identity = format!("FoxyNativePeer_{:x}", nonce());
    setup::configure(&config, &case, &peer_id, &adopted, 27112, &peer_identity)?;
    let peer_installation =
        string(
            &invoke_cli(&config, &["server", "instance", "show", &peer_id], 0)?["data"]["instance"]
                ["installation_id"],
            "peer installation id",
        )?
        .to_owned();
    if peer_installation != installation {
        return Err(io::Error::other(
            "Both native instances must share the exact adopted installation",
        ));
    }
    setup::bind_existing_content(&config, &case, &peer_id)?;
    let peer_prepared = prepare(
        &config,
        &case,
        &peer_id,
        "beta",
        &peer_identity,
        PreparationRevision {
            deployment: 0,
            draft: 3,
            configuration: 1,
        },
    )?;
    setup::commit_runtime(&config, &peer_id, &peer_prepared, 1, 0)?;
    let peer_plan = start_plan(&config, &peer_id)?;
    let first = prepare(
        &config,
        &case,
        &id,
        "alpha",
        &first_identity,
        PreparationRevision {
            deployment: 0,
            draft: 3,
            configuration: 1,
        },
    )?;
    commit_runtime(&config, &id, &first, 1, 0)?;
    let (predecessor_engine, predecessor_runtime) = committed_engine(&config, &id)?;
    let predecessor_hash = inventory::tree_hash(&predecessor_engine)?;
    let second_identity = format!("FoxyNativeBeta_{:x}", nonce());
    update_configuration(&config, &id, &second_identity, 27122)?;
    let second = prepare(
        &config,
        &case,
        &id,
        "beta",
        &second_identity,
        PreparationRevision {
            deployment: 1,
            draft: 4,
            configuration: 2,
        },
    )?;
    commit_runtime(&config, &id, &second, 2, 1)?;
    let plan_b = start_plan(&config, &id)?;

    let mut owner = transport::start_owner(&config)?;
    let dry = invoke_cli(
        &config,
        &[
            "--dry-run",
            "server",
            "instance",
            "start",
            &id,
            "--expected-activation-generation",
            &plan_b.0,
            "--expected-plan-fingerprint",
            &plan_b.1,
        ],
        0,
    )?;
    if dry["data"]["process_started"] != false || dry["data"]["owner_contacted"] != false {
        return Err(io::Error::other(
            "Dry Start did not report its no-process/no-owner contract",
        ));
    }
    let (confirmation_code, confirmation) = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "start",
            &id,
            "--expected-activation-generation",
            &plan_b.0,
            "--expected-plan-fingerprint",
            &plan_b.1,
        ],
    )?;
    if confirmation_code != 2 || confirmation["action"] != "server.instance.start" {
        return Err(io::Error::other(format!(
            "Unconfirmed Start returned an unexpected result: {confirmation}"
        )));
    }
    let stale_generation = plan_b
        .0
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_sub(1)
        .to_string();
    let (stale_code, stale) = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "start",
            &id,
            "--expected-activation-generation",
            &stale_generation,
            "--expected-plan-fingerprint",
            &plan_b.1,
            "--yes",
        ],
    )?;
    if stale_code != 2 || stale["action"] != "server.instance.start" {
        return Err(io::Error::other(format!(
            "Stale Start returned an unexpected result: {stale}"
        )));
    }
    let (unknown_code, unknown) = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "start-plan",
            "server_00000000000000000000000000000000",
        ],
    )?;
    if unknown_code != 3 || unknown["action"] != "server.instance.start-plan" {
        return Err(io::Error::other(format!(
            "Unknown StartPlan returned an unexpected result: {unknown}"
        )));
    }
    if status(&config, &id)?["data"]["status"]["phase"] != "not_owned" {
        return Err(io::Error::other(
            "Dry, unconfirmed, stale, or unknown gates changed instance state",
        ));
    }
    start(&config, &id, &plan_b)?;
    start(&config, &peer_id, &peer_plan)?;
    let first_status = status(&config, &id)?;
    let run = number(
        &first_status["data"]["status"]["run_generation"],
        "run generation",
    )?;
    if first_status["data"]["status"]["phase"] != "running"
        || first_status["data"]["status"]["activation_generation"] != 2
        || first_status["data"]["status"]["runtime_id"] != plan_b.2
    {
        return Err(io::Error::other(
            "First native run did not reach the committed second runtime",
        ));
    }
    proof::wait_for_mission(
        &config,
        &id,
        (&second.identity, 27123),
        string(&first_status["data"]["status"]["owner_id"], "owner id")?,
        run,
        Duration::from_secs(180),
    )?;
    let peer_status = status(&config, &peer_id)?;
    if peer_status["data"]["status"]["phase"] != "running"
        || peer_status["data"]["status"]["activation_generation"] != 1
        || peer_status["data"]["status"]["runtime_id"] != peer_plan.2
    {
        return Err(io::Error::other(
            "Peer did not start its exact independent runtime",
        ));
    }
    proof::wait_for_mission(
        &config,
        &peer_id,
        (&peer_identity, 27113),
        string(&peer_status["data"]["status"]["owner_id"], "peer owner id")?,
        number(
            &peer_status["data"]["status"]["run_generation"],
            "peer run generation",
        )?,
        Duration::from_secs(180),
    )?;
    let (remove_code, remove_result) = invoke_raw(
        &config,
        &[
            "--yes",
            "server",
            "instance",
            "remove",
            &id,
            "--expected-revision",
            "4",
        ],
    )?;
    if remove_code == 0 || remove_result["action"] != "server.instance.remove" {
        return Err(io::Error::other(format!(
            "Removing a live instance did not hit its consumer barrier: {remove_result}"
        )));
    }
    let (rebind_code, rebind_result) = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "bind",
            &id,
            "--clear",
            "--expected-revision",
            "4",
        ],
    )?;
    if rebind_code == 0 || rebind_result["action"] != "server.instance.bind" {
        return Err(io::Error::other(format!(
            "Rebinding a live instance was not refused: {rebind_result}"
        )));
    }
    let live_consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &installation],
        0,
    )?;
    if !live_consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_some_and(|records| {
            records.iter().any(|record| {
                record["session"]["instance_id"] == id
                    && record["session"]["run_generation"].as_u64() == Some(run)
            })
        })
    {
        return Err(io::Error::other(
            "Live native run had no persistent consumer registration while mutation refusals were checked",
        ));
    }
    if status(&config, &id)?["data"]["status"]["run_generation"].as_u64() != Some(run)
        || status(&config, &peer_id)?["data"]["status"]["process_exists"] != true
    {
        return Err(io::Error::other(
            "Failed remove/rebind changed either live run",
        ));
    }
    let (duplicate_code, duplicate) = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "start",
            &id,
            "--expected-activation-generation",
            &plan_b.0,
            "--expected-plan-fingerprint",
            &plan_b.1,
            "--yes",
        ],
    )?;
    if duplicate_code != 4
        || duplicate["ok"] != false
        || duplicate["action"] != "server.instance.start"
        || !duplicate["errors"].as_array().is_some_and(|errors| {
            errors.iter().any(|error| {
                error.as_str().is_some_and(|message| {
                    message.contains("already has a retained foreground session")
                })
            })
        })
    {
        return Err(io::Error::other(format!(
            "Duplicate Start returned an unexpected refusal: {duplicate}"
        )));
    }
    let after_attachment = status(&config, &id)?;
    if after_attachment["data"]["status"]["run_generation"].as_u64() != Some(run)
        || after_attachment["data"]["status"]["process_exists"] != true
    {
        return Err(io::Error::other(
            "Attached CLI controller return changed the retained external run",
        ));
    }
    let timeout = lifecycle(&config, "stop", &id, run, 1, false);
    let after = status(&config, &id)?;
    if after["data"]["status"]["run_generation"].as_u64() != Some(run)
        || after["data"]["status"]["cleanup_verified"] == true
        || after["data"]["status"]["process_exists"] != true
        || after["data"]["status"]["stop_timed_out"] != true
    {
        eprintln!(
            "Timeout retention gap: stop result was {}; retained status was {}",
            timeout.is_ok(),
            after["data"]["status"]
        );
        return Err(io::Error::other(
            "Native Stop timeout did not prove retained session and consumer barrier",
        ));
    }
    let peer_after_timeout = status(&config, &peer_id)?;
    if peer_after_timeout["data"]["status"]["phase"] != "running"
        || peer_after_timeout["data"]["status"]["process_exists"] != true
    {
        return Err(io::Error::other(
            "A timed-out Stop disturbed the separately owned peer",
        ));
    }
    let timeout_consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &installation],
        0,
    )?;
    if !timeout_consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_some_and(|records| {
            records.iter().any(|record| {
                record["session"]["instance_id"] == id
                    && record["session"]["run_generation"].as_u64() == Some(run)
            }) && records
                .iter()
                .any(|record| record["session"]["instance_id"] == peer_id)
        })
    {
        return Err(io::Error::other(
            "Timed-out Stop did not retain both shared installation consumers",
        ));
    }
    lifecycle(&config, "stop", &id, run, 1, true)?;
    let forced_status = status(&config, &id)?;
    if forced_status["data"]["status"]["forced"] != true
        || forced_status["data"]["status"]["cleanup_verified"] != true
    {
        return Err(io::Error::other(
            "Explicit force Stop did not report waited cleanup",
        ));
    }
    let peer_after_force = status(&config, &peer_id)?;
    if peer_after_force["data"]["status"]["phase"] != "running"
        || peer_after_force["data"]["status"]["process_exists"] != true
    {
        return Err(io::Error::other(
            "Force Stop of one instance disturbed its live peer",
        ));
    }
    let shared_consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &installation],
        0,
    )?;
    let shared_records = shared_consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .ok_or_else(|| io::Error::other("Shared installation consumer records are missing"))?;
    if shared_records
        .iter()
        .any(|record| record["session"]["instance_id"] == id)
        || !shared_records
            .iter()
            .any(|record| record["session"]["instance_id"] == peer_id)
    {
        return Err(io::Error::other(
            "Forced cleanup did not retire only the stopped instance registration",
        ));
    }

    start(&config, &id, &plan_b)?;
    let current = status(&config, &id)?;
    let run = number(
        &current["data"]["status"]["run_generation"],
        "run generation",
    )?;
    lifecycle(&config, "restart", &id, run, 120_000, false)?;
    let restarted = status(&config, &id)?;
    if restarted["data"]["status"]["activation_generation"] != 2
        || restarted["data"]["status"]["runtime_id"] != plan_b.2
    {
        return Err(io::Error::other(
            "Restart did not retain the reviewed runtime generation",
        ));
    }
    let restarted_run = number(
        &restarted["data"]["status"]["run_generation"],
        "run generation",
    )?;
    if restarted_run <= run
        || restarted["data"]["status"]["phase"] != "running"
        || restarted["data"]["status"]["process_id"].as_u64().is_none()
    {
        return Err(io::Error::other(
            "Restart did not create a newer running generation with a root process",
        ));
    }
    proof::wait_for_mission(
        &config,
        &id,
        (&second.identity, 27123),
        string(&restarted["data"]["status"]["owner_id"], "owner id")?,
        restarted_run,
        Duration::from_secs(180),
    )?;
    lifecycle(&config, "stop", &id, restarted_run, 120_000, false)?;
    let peer_running = status(&config, &peer_id)?;
    lifecycle(
        &config,
        "stop",
        &peer_id,
        number(
            &peer_running["data"]["status"]["run_generation"],
            "peer run generation",
        )?,
        120_000,
        false,
    )?;
    select_predecessor(&config, &id, &first)?;
    let predecessor_plan = start_plan(&config, &id)?;
    start(&config, &id, &predecessor_plan)?;
    let predecessor_status = status(&config, &id)?;
    if predecessor_status["data"]["status"]["activation_generation"] != 3
        || predecessor_status["data"]["status"]["runtime_id"] != predecessor_plan.2
    {
        return Err(io::Error::other(
            "Predecessor runtime did not start at its committed activation generation",
        ));
    }
    proof::wait_for_mission(
        &config,
        &id,
        (&first.identity, 27103),
        string(
            &predecessor_status["data"]["status"]["owner_id"],
            "owner id",
        )?,
        number(
            &predecessor_status["data"]["status"]["run_generation"],
            "run generation",
        )?,
        Duration::from_secs(180),
    )?;
    start(&config, &peer_id, &peer_plan)?;
    let peer_restarted = status(&config, &peer_id)?;
    proof::wait_for_mission(
        &config,
        &peer_id,
        (&peer_identity, 27113),
        string(
            &peer_restarted["data"]["status"]["owner_id"],
            "peer owner id",
        )?,
        number(
            &peer_restarted["data"]["status"]["run_generation"],
            "peer run generation",
        )?,
        Duration::from_secs(180),
    )?;
    owner.stdin.take();
    transport::wait_child(&mut owner, Duration::from_secs(30))?;
    let orderly_consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &installation],
        0,
    )?;
    if orderly_consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_none_or(|records| !records.is_empty())
    {
        return Err(io::Error::other(
            "Ordinary owner close did not retire the waited native consumer registration",
        ));
    }
    let peer_orderly_consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &peer_installation],
        0,
    )?;
    if peer_orderly_consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_none_or(|records| !records.is_empty())
    {
        return Err(io::Error::other(
            "Ordinary owner close left a peer consumer registration",
        ));
    }
    if inventory::tree_hash(&source)? != source_hash
        || inventory::tree_hash(&adopted)? != adopted_hash
        || inventory::tree_hash(&predecessor_engine)? != predecessor_hash
    {
        return Err(io::Error::other(
            "Native source or adopted predecessor changed",
        ));
    }

    let mut crash_owner = transport::start_owner(&config)?;
    let plan_a = start_plan(&config, &id)?;
    if plan_a.2 != predecessor_runtime {
        return Err(io::Error::other(
            "Crash case selected a runtime other than the hashed predecessor",
        ));
    }
    start(&config, &id, &plan_a)?;
    start(&config, &peer_id, &peer_plan)?;
    let before = status(&config, &id)?;
    let peer_before = status(&config, &peer_id)?;
    let crash_run = number(
        &before["data"]["status"]["run_generation"],
        "run generation",
    )?;
    proof::wait_for_mission(
        &config,
        &id,
        (&first.identity, 27103),
        string(&before["data"]["status"]["owner_id"], "owner id")?,
        crash_run,
        Duration::from_secs(180),
    )?;
    proof::wait_for_mission(
        &config,
        &peer_id,
        (&peer_identity, 27113),
        string(&peer_before["data"]["status"]["owner_id"], "peer owner id")?,
        number(
            &peer_before["data"]["status"]["run_generation"],
            "peer run generation",
        )?,
        Duration::from_secs(180),
    )?;
    if !proof::query_info(27103, &first_identity) || !proof::query_info(27113, &peer_identity) {
        return Err(io::Error::other(
            "Native loopback query did not prove both processes live before owner interruption",
        ));
    }
    let root_id = number(&before["data"]["status"]["process_id"], "root process id")? as u32;
    let peer_root_id = number(
        &peer_before["data"]["status"]["process_id"],
        "peer root process id",
    )? as u32;
    let root_process = observation::RootProcess::observe(root_id)?;
    let peer_root_process = observation::RootProcess::observe(peer_root_id)?;
    crash_owner.kill()?;
    let _ = crash_owner.wait()?;
    if !root_process.wait_exited(Duration::from_secs(30))?
        || !peer_root_process.wait_exited(Duration::from_secs(30))?
    {
        return Err(io::Error::other(
            "Abrupt owner death did not positively observe both native root processes exit",
        ));
    }
    let query_deadline = Instant::now() + Duration::from_secs(30);
    let mut consecutive_query_cessation = 0;
    while Instant::now() < query_deadline && consecutive_query_cessation < 3 {
        if !proof::query_info(27103, &first_identity) && !proof::query_info(27113, &peer_identity) {
            consecutive_query_cessation += 1;
        } else {
            consecutive_query_cessation = 0;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if consecutive_query_cessation < 3 {
        return Err(io::Error::other(
            "Abrupt owner death did not produce repeated native loopback query cessation",
        ));
    }
    let mut owner = transport::start_owner(&config)?;
    let unresolved = status(&config, &id)?;
    let consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &installation],
        0,
    )?;
    let peer_consumers = invoke_cli(
        &config,
        &["server", "installation", "consumers", &peer_installation],
        0,
    )?;
    let records = consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .ok_or_else(|| io::Error::other("Consumer inspection omitted registration records"))?;
    if records.is_empty()
        || !records.iter().any(|record| {
            record["session"]["instance_id"] == id
                && record["session"]["run_generation"].as_u64() == Some(crash_run)
        })
        || unresolved["data"]["status"]["cleanup_verified"] == true
    {
        return Err(io::Error::other(
            "Abrupt owner death did not preserve the unresolved consumer barrier",
        ));
    }
    if peer_consumers["data"]["consumer_inspection"]["recorded_consumers"]
        .as_array()
        .is_none_or(|records| {
            !records
                .iter()
                .any(|record| record["session"]["instance_id"] == peer_id)
        })
    {
        return Err(io::Error::other(
            "Abrupt owner death did not preserve the peer barrier",
        ));
    }
    eprintln!(
        "Abrupt retained owner death preserved run generation {crash_run}; cleanup remains unverified"
    );
    owner.stdin.take();
    transport::wait_child(&mut owner, Duration::from_secs(30))?;
    Ok(())
}

fn start_plan(config: &Path, id: &str) -> io::Result<(String, String, String)> {
    let plan = invoke_cli(config, &["server", "instance", "start-plan", id], 0)?;
    Ok((
        number_string(&plan["data"]["plan"]["activation_generation"])?,
        string(&plan["data"]["plan"]["fingerprint"], "start fingerprint")?.to_owned(),
        string(&plan["data"]["plan"]["runtime_id"], "runtime id")?.to_owned(),
    ))
}

fn committed_engine(config: &Path, id: &str) -> io::Result<(PathBuf, String)> {
    let value = invoke_cli(config, &["server", "instance", "start-plan", id], 0)?;
    let executable = PathBuf::from(string(
        &value["data"]["plan"]["launch"]["executable"],
        "planned executable",
    )?);
    let engine = executable
        .parent()
        .ok_or_else(|| io::Error::other("Start plan executable has no engine directory"))?
        .to_path_buf();
    let runtime_id = string(&value["data"]["plan"]["runtime_id"], "planned runtime id")?.to_owned();
    Ok((engine, runtime_id))
}

fn start(config: &Path, id: &str, plan: &(String, String, String)) -> io::Result<()> {
    invoke_cli(
        config,
        &[
            "server",
            "instance",
            "start",
            id,
            "--expected-activation-generation",
            &plan.0,
            "--expected-plan-fingerprint",
            &plan.1,
            "--yes",
        ],
        0,
    )?;
    Ok(())
}

fn status(config: &Path, id: &str) -> io::Result<Value> {
    invoke_cli(config, &["server", "instance", "status", id], 0)
}

fn lifecycle(
    config: &Path,
    operation: &str,
    id: &str,
    run: u64,
    timeout: u32,
    force: bool,
) -> io::Result<Value> {
    let mut args = vec![
        "server",
        "instance",
        operation,
        id,
        "--expected-run-generation",
        "",
        "--timeout-ms",
        "",
        "--yes",
    ];
    let run_arg = run.to_string();
    let timeout_arg = timeout.to_string();
    args[5] = &run_arg;
    args[7] = &timeout_arg;
    if force {
        args.push("--force");
    }
    let plan = if operation == "restart" {
        Some(start_plan(config, id)?)
    } else {
        None
    };
    if let Some(plan) = &plan {
        args.extend([
            "--expected-activation-generation",
            plan.0.as_str(),
            "--expected-plan-fingerprint",
            plan.1.as_str(),
        ]);
    }
    invoke_cli(config, &args, 0)
}

fn invoke_cli(config: &Path, args: &[&str], expected: i32) -> io::Result<Value> {
    transport::invoke(config, args, expected)
}

fn invoke_raw(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
    transport::invoke_raw(config, args)
}

fn path_arg(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other("Path is not representable"))
}
fn string<'a>(value: &'a Value, name: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}
fn number(value: &Value, name: &str) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}
fn number_string(value: &Value) -> io::Result<String> {
    Ok(number(value, "numeric value")?.to_string())
}
fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

fn require_port_range_free(port: u16) -> io::Result<()> {
    let mut reservations = Vec::new();
    for current in port..=port + 4 {
        reservations.push((
            std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, current))?,
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, current))?,
        ));
    }
    drop(reservations);
    Ok(())
}

fn invoke(config: &Path, args: &[&str], operation: &str) -> io::Result<Value> {
    invoke_cli(config, args, 0).map_err(|error| io::Error::other(format!("{operation}: {error}")))
}
