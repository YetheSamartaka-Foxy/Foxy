use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::{inventory, setup::PreparedInstance, transport};

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;
#[path = "../../server_monitoring_native/proof.rs"]
mod monitoring;

pub(super) struct StartPlan {
    pub activation_generation: String,
    pub fingerprint: String,
    pub runtime_id: String,
    value: Value,
}

pub(super) struct Run {
    pub generation: u64,
    pub owner_id: String,
    process_id: u64,
}

pub(super) struct LogBaseline(BTreeMap<String, Vec<u8>>);

#[derive(Serialize)]
pub(super) struct RunEvidence {
    run_generation: u64,
    owner_id: String,
    process_id: u64,
    hostname: String,
    scenario: &'static str,
    engine_version: &'static str,
    updated_engine_logs: Vec<String>,
    evidence_blake3: String,
}

pub(super) fn start_plan(config: &Path, id: &str) -> io::Result<StartPlan> {
    let value = transport::invoke(config, &["server", "instance", "start-plan", id], 0)?;
    let plan = &value["data"]["plan"];
    Ok(StartPlan {
        activation_generation: plan["activation_generation"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Start plan omitted activation generation"))?
            .to_string(),
        fingerprint: string(&plan["fingerprint"], "start plan fingerprint")?.to_owned(),
        runtime_id: string(&plan["runtime_id"], "runtime id")?.to_owned(),
        value,
    })
}

pub(super) fn verify_start_plan(plan: &StartPlan, instance: &PreparedInstance) -> io::Result<()> {
    if plan.runtime_id != instance.runtime_id {
        return Err(io::Error::other("Start plan selected a different runtime"));
    }
    let launch = &plan.value["data"]["plan"]["launch"];
    let executable = PathBuf::from(string(&launch["executable"], "planned executable")?);
    let profiles = PathBuf::from(string(&launch["profiles"], "planned profile root")?);
    if executable.parent() != Some(instance.engine_root.as_path()) {
        return Err(io::Error::other(
            "Start plan is not using its private engine copy",
        ));
    }
    let arguments = launch["arguments"]
        .as_array()
        .ok_or_else(|| io::Error::other("Start plan omitted launch arguments"))?;
    if argument_path(arguments, "-profile")? != instance.profile_argument_root
        || instance.profile_argument_root.join("profile") != instance.profile_root
        || profiles.canonicalize()? != instance.profile_root.canonicalize()?
        || argument_path(arguments, "-config")? != instance.server_config
        || argument_path(arguments, "-logsDir")? != instance.engine_log_root
        || !instance.profile_root.is_dir()
        || !instance.server_config.is_file()
        || !instance.engine_log_root.is_dir()
    {
        return Err(io::Error::other(
            "Start plan profile/config mapping is not instance-local",
        ));
    }
    Ok(())
}

pub(super) fn verify_server_config(
    instance: &PreparedInstance,
    game_port: u16,
    a2s_port: u16,
) -> io::Result<()> {
    let config: Value = serde_json::from_slice(&fs::read(&instance.server_config)?)
        .map_err(|_| io::Error::other("Private Reforger server config is invalid JSON"))?;
    if config["bindAddress"] != "127.0.0.1"
        || config["bindPort"] != game_port
        || config["a2s"]["address"] != "127.0.0.1"
        || config["a2s"]["port"] != a2s_port
        || config["game"]["name"] != instance.name
        || config["game"]["scenarioId"] != "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf"
        || config["game"]["maxPlayers"] != 4
        || config["game"]["visible"] != false
        || config["game"]["mods"] != instance.expected_mods
    {
        return Err(io::Error::other(
            "Private Reforger config differs from reviewed intent",
        ));
    }
    Ok(())
}

pub(super) fn check_start_gates(config: &Path, id: &str, plan: &StartPlan) -> io::Result<()> {
    let inputs = [
        "server",
        "instance",
        "start",
        id,
        "--expected-activation-generation",
        &plan.activation_generation,
        "--expected-plan-fingerprint",
        &plan.fingerprint,
    ];
    let mut dry = vec!["--dry-run"];
    dry.extend(inputs);
    let dry_result = transport::invoke(config, &dry, 0)?;
    if dry_result["data"]["process_started"] != false
        || dry_result["data"]["owner_contacted"] != false
    {
        return Err(io::Error::other(
            "Start dry run contacted the owner or started a process",
        ));
    }
    let (code, _) = transport::invoke_raw(config, &inputs)?;
    if code != 2 {
        return Err(io::Error::other(
            "Unconfirmed Reforger Start was not refused",
        ));
    }
    let stale = plan
        .activation_generation
        .parse::<u64>()
        .unwrap_or_default()
        .saturating_sub(1)
        .to_string();
    let (code, _) = transport::invoke_raw(
        config,
        &[
            "server",
            "instance",
            "start",
            id,
            "--expected-activation-generation",
            &stale,
            "--expected-plan-fingerprint",
            &plan.fingerprint,
            "--yes",
        ],
    )?;
    if code != 2 {
        return Err(io::Error::other(
            "Stale Reforger Start review was not refused",
        ));
    }
    let (code, _) = transport::invoke_raw(
        config,
        &[
            "server",
            "instance",
            "start",
            id,
            "--expected-activation-generation",
            &plan.activation_generation,
            "--expected-plan-fingerprint",
            "stale",
            "--yes",
        ],
    )?;
    if code != 2 {
        return Err(io::Error::other(
            "Stale Reforger Start fingerprint was not refused",
        ));
    }
    let status = transport::invoke(config, &["server", "instance", "status", id], 0)?;
    if status["data"]["status"]["phase"] != "not_owned"
        || status["data"]["status"]["process_exists"] != false
    {
        return Err(io::Error::other(
            "Rejected Start gates changed runtime ownership",
        ));
    }
    Ok(())
}

pub(super) fn start(config: &Path, id: &str, plan: &StartPlan) -> io::Result<()> {
    transport::invoke(
        config,
        &[
            "server",
            "instance",
            "start",
            id,
            "--expected-activation-generation",
            &plan.activation_generation,
            "--expected-plan-fingerprint",
            &plan.fingerprint,
            "--yes",
        ],
        0,
    )?;
    Ok(())
}

pub(super) fn owned_running(config: &Path, id: &str) -> io::Result<Run> {
    let status = transport::invoke(config, &["server", "instance", "status", id], 0)?;
    let status = &status["data"]["status"];
    if status["phase"] != "running" || status["process_exists"] != true {
        return Err(io::Error::other(
            "Foreground owner did not report a live Reforger process",
        ));
    }
    let generation = status["run_generation"]
        .as_u64()
        .filter(|generation| *generation > 0)
        .ok_or_else(|| io::Error::other("Running status omitted run generation"))?;
    let process_id = status["process_id"]
        .as_u64()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::other("Running status omitted process identity"))?;
    Ok(Run {
        generation,
        owner_id: string(&status["owner_id"], "owner id")?.to_owned(),
        process_id,
    })
}

pub(super) fn log_baseline(instance: &PreparedInstance) -> io::Result<LogBaseline> {
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    let mut pending = vec![instance.engine_log_root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native Reforger log tree contains a link"));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file()
                && entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
                })
            {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| io::Error::other("Native log baseline size overflow"))?;
                if metadata.len() > MAX_LOG_BYTES || total > MAX_LOG_BYTES {
                    return Err(io::Error::other("Native log baseline exceeded bound"));
                }
                let path = entry
                    .path()
                    .strip_prefix(&instance.engine_log_root)
                    .map_err(io::Error::other)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("Native log path is not UTF-8"))?
                    .replace('\\', "/");
                files.insert(path, fs::read(entry.path())?);
            } else {
                return Err(io::Error::other(
                    "Native Reforger log tree has a special file",
                ));
            }
        }
    }
    Ok(LogBaseline(files))
}

pub(super) fn wait_for_native_ready(
    instance: &PreparedInstance,
    run: &Run,
    baseline: &LogBaseline,
    timeout: Duration,
) -> io::Result<RunEvidence> {
    let log = run_log(instance, run);
    let deadline = Instant::now() + timeout;
    loop {
        let status = transport::invoke(
            &instance.config_root,
            &["server", "instance", "status", &instance.id],
            0,
        )?;
        let current = &status["data"]["status"];
        if current["process_exists"] != true
            || current["run_generation"].as_u64() != Some(run.generation)
            || current["owner_id"] != run.owner_id
            || current["process_id"].as_u64() != Some(run.process_id)
        {
            return Err(io::Error::other(
                "Could not confirm Reforger process ownership",
            ));
        }
        if let Ok((text, updated_logs)) = collect_run_logs(instance, &log, baseline)
            && native_log_ready(&text, instance)
            && !updated_logs.is_empty()
            && super::super::layout::query_a2s_hostname(instance.a2s_port).as_deref()
                == Some(instance.name.as_str())
        {
            monitoring::wait(
                &instance.config_root,
                &instance.id,
                &instance.name,
                &run.owner_id,
                run.generation,
                (std::net::Ipv4Addr::LOCALHOST, instance.a2s_port).into(),
                transport::invoke,
            )?;
            return Ok(RunEvidence {
                run_generation: run.generation,
                owner_id: run.owner_id.clone(),
                process_id: run.process_id,
                hostname: instance.name.clone(),
                scenario: super::SCENARIO,
                engine_version: super::ENGINE_VERSION,
                evidence_blake3: blake3::hash(text.as_bytes()).to_hex().to_string(),
                updated_engine_logs: updated_logs,
            });
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned Reforger process did not report exact native scenario readiness",
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn native_log_ready(text: &str, instance: &PreparedInstance) -> bool {
    let lower = text.to_ascii_lowercase();
    let scenario = super::super::layout::SCENARIO_WORLD.to_ascii_lowercase();
    let profile = instance
        .profile_root
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let log = lower.replace('\\', "/");
    let mounted_addons = instance.expected_mods.as_array().is_some_and(|mods| {
        mods.iter().all(|item| {
            let Some(id) = item["modId"].as_str() else {
                return false;
            };
            let path = instance
                .engine_root
                .join("foxy-content")
                .join(id)
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            log.lines()
                .any(|line| line.contains("filesystem: adding package") && line.contains(&path))
                && log
                    .lines()
                    .any(|line| line.contains("resourcedb: loading cache") && line.contains(&path))
        })
    });
    mounted_addons
        && log
            .lines()
            .any(|line| line.contains("entities load") && line.contains(&scenario))
        && log.contains("entered online game state.")
        && log.contains(&super::ENGINE_VERSION.to_ascii_lowercase())
        && log.lines().any(|line| {
            line.contains("adding relative directory")
                && line.contains("under name profile")
                && line.contains(&profile)
        })
}

fn collect_run_logs(
    instance: &PreparedInstance,
    console: &Path,
    baseline: &LogBaseline,
) -> io::Result<(String, Vec<String>)> {
    let mut output = read_bounded(console)?;
    let mut updated_logs = Vec::new();
    let mut pending = vec![instance.engine_log_root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native Reforger log tree contains a link"));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                let path = entry
                    .path()
                    .strip_prefix(&instance.engine_log_root)
                    .map_err(io::Error::other)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("Native log path is not UTF-8"))?
                    .replace('\\', "/");
                if !entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
                }) {
                    continue;
                }
                if metadata.len() > MAX_LOG_BYTES {
                    return Err(io::Error::other(
                        "Native Reforger engine log exceeded bound",
                    ));
                }
                let current = fs::read(entry.path())?;
                let previous = baseline.0.get(&path).map(Vec::as_slice).unwrap_or_default();
                if current == previous {
                    continue;
                }
                let fresh = current
                    .strip_prefix(previous)
                    .map_or(current.as_slice(), |suffix| suffix);
                if fresh.is_empty() {
                    continue;
                }
                output.push('\n');
                output.push_str(&String::from_utf8_lossy(fresh));
                updated_logs.push(path);
                if output.len() as u64 > MAX_LOG_BYTES {
                    return Err(io::Error::other("Combined Reforger logs exceeded bound"));
                }
            }
        }
    }
    updated_logs.sort();
    Ok((output, updated_logs))
}

pub(super) fn stop(config: &Path, id: &str, generation: u64) -> io::Result<()> {
    transport::invoke(
        config,
        &[
            "server",
            "instance",
            "stop",
            id,
            "--expected-run-generation",
            &generation.to_string(),
            "--timeout-ms",
            "120000",
            "--yes",
        ],
        0,
    )?;
    Ok(())
}

pub(super) fn verify_stopped(
    config: &Path,
    id: &str,
    marker_path: &Path,
    marker: &[u8],
) -> io::Result<()> {
    let value = transport::invoke(config, &["server", "instance", "status", id], 0)?;
    let status = &value["data"]["status"];
    if status["process_exists"] != false || status["cleanup_verified"] != true {
        return Err(io::Error::other(
            "Stop did not verify Reforger process cleanup",
        ));
    }
    verify_marker(marker_path, marker)
}

pub(super) fn restart(config: &Path, id: &str, generation: u64) -> io::Result<()> {
    let plan = start_plan(config, id)?;
    transport::invoke(
        config,
        &[
            "server",
            "instance",
            "restart",
            id,
            "--expected-run-generation",
            &generation.to_string(),
            "--timeout-ms",
            "120000",
            "--yes",
            "--expected-activation-generation",
            &plan.activation_generation,
            "--expected-plan-fingerprint",
            &plan.fingerprint,
        ],
        0,
    )?;
    Ok(())
}

pub(super) fn wait_for_new_run(
    config: &Path,
    id: &str,
    previous_generation: u64,
) -> io::Result<Run> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(run) = owned_running(config, id)
            && run.generation > previous_generation
        {
            return Ok(run);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Restart did not create a newer owned Reforger run",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

pub(super) fn verify_marker(path: &Path, expected: &[u8]) -> io::Result<()> {
    if fs::read(path)? != expected {
        return Err(io::Error::other("Private Reforger profile marker changed"));
    }
    Ok(())
}

pub(super) fn assert_snapshot(root: &Path, expected: &inventory::Snapshot) -> io::Result<()> {
    if &inventory::snapshot(root)? != expected {
        return Err(io::Error::other(
            "Private Reforger engine inventory changed",
        ));
    }
    Ok(())
}

fn run_log(instance: &PreparedInstance, run: &Run) -> PathBuf {
    instance
        .instance_root
        .join("deployments")
        .join("runtime-state")
        .join(format!("run_{}_{}.log", run.owner_id, run.generation))
}

fn read_bounded(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_LOG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOG_BYTES {
        return Err(io::Error::other("Reforger run log exceeded bound"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn argument_path(arguments: &[Value], flag: &str) -> io::Result<PathBuf> {
    let index = arguments
        .iter()
        .position(|value| value.as_str() == Some(flag))
        .ok_or_else(|| io::Error::other("Start plan omitted a runtime path flag"))?;
    arguments
        .get(index + 1)
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Start plan runtime path is invalid"))
}

fn string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}
