use serde_json::{Value, json};
use std::io;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const CONTROLLERS: usize = 4;
const CHECKS_PER_CONTROLLER: usize = 16;
const RESOURCE_HISTORY_LIMIT: usize = 60;

pub(super) fn poll_owner_and_monitor(
    config: &Path,
    space: &str,
    instance: &str,
    owner: &Value,
    run: &str,
    ready: &Value,
) -> io::Result<Value> {
    let config = config.to_owned();
    let space = space.to_owned();
    let instance = instance.to_owned();
    let owner_id = owner
        .pointer("/scope/owner_id")
        .cloned()
        .ok_or_else(|| io::Error::other("Owner response omitted owner identity"))?;
    let owner_generation = owner
        .pointer("/scope/generation")
        .cloned()
        .ok_or_else(|| io::Error::other("Owner response omitted owner generation"))?;
    let run = run.parse::<u64>().map_err(io::Error::other)?;
    let process_id = ready
        .pointer("/data/monitoring/status/process_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| io::Error::other("Ready monitor omitted process identity"))?;
    let barrier = Arc::new(Barrier::new(CONTROLLERS));
    let started_unix_ms = unix_ms();
    let started = Instant::now();

    let reports = thread::scope(|scope_thread| {
        let mut handles = Vec::with_capacity(CONTROLLERS);
        for _ in 0..CONTROLLERS {
            let barrier = Arc::clone(&barrier);
            let config = config.clone();
            let space = space.clone();
            let instance = instance.clone();
            let owner_id = owner_id.clone();
            let owner_generation = owner_generation.clone();
            handles.push(scope_thread.spawn(move || -> io::Result<Value> {
                let mut owner_checks = 0usize;
                let mut monitor_checks = 0usize;
                let mut query_ready_checks = 0usize;
                let mut returned_resource_points = 0usize;
                let mut largest_resource_history = 0usize;
                let mut peak_sampled_resident_bytes = None;
                barrier.wait();
                let first_check_unix_ms = unix_ms();
                for _ in 0..CHECKS_PER_CONTROLLER {
                    let owner_result =
                        super::raw_cli(&config, &["server", "owner", "--game-space", &space])?;
                    if owner_result.0 != 0
                        || owner_result.1.pointer("/data/owner/scope/owner_id") != Some(&owner_id)
                        || owner_result.1.pointer("/data/owner/scope/generation")
                            != Some(&owner_generation)
                    {
                        return Err(io::Error::other(
                            "Load poll observed missing or changed owner identity",
                        ));
                    }
                    owner_checks += 1;

                    let monitor = super::cli(
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
                    let status = &monitor["data"]["monitoring"]["status"];
                    if status["process_exists"] != true
                        || status["owner_id"] != owner_id
                        || status["run_generation"].as_u64() != Some(run)
                        || status["process_id"].as_u64() != Some(process_id)
                    {
                        return Err(io::Error::other(
                            "Load poll observed missing, stale, or cross-run monitor state",
                        ));
                    }
                    if monitor.pointer("/data/monitoring/query_ready") == Some(&Value::Bool(true)) {
                        query_ready_checks += 1;
                    }
                    if let Some(resources) = monitor
                        .pointer("/data/monitoring/observation/resources")
                        .and_then(Value::as_array)
                    {
                        if resources.len() > RESOURCE_HISTORY_LIMIT {
                            return Err(io::Error::other("Resource history exceeded 60 samples"));
                        }
                        returned_resource_points += resources.len();
                        largest_resource_history = largest_resource_history.max(resources.len());
                        let sample_peak = resources
                            .iter()
                            .filter_map(|point| point.pointer("/sample/resident_bytes")?.as_u64())
                            .max();
                        peak_sampled_resident_bytes = peak_sampled_resident_bytes.max(sample_peak);
                    }
                    monitor_checks += 1;
                }
                Ok(json!({
                    "owner_checks": owner_checks,
                    "monitor_checks": monitor_checks,
                    "query_ready_checks": query_ready_checks,
                    "returned_resource_points": returned_resource_points,
                    "largest_resource_history": largest_resource_history,
                    "peak_sampled_resident_bytes": peak_sampled_resident_bytes,
                    "first_check_unix_ms": first_check_unix_ms,
                    "last_check_unix_ms": unix_ms(),
                }))
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| io::Error::other("Owner load controller panicked"))?
            })
            .collect::<io::Result<Vec<_>>>()
    })?;

    let owner_checks: usize = reports
        .iter()
        .filter_map(|report| report["owner_checks"].as_u64())
        .map(|count| count as usize)
        .sum();
    let monitor_checks: usize = reports
        .iter()
        .filter_map(|report| report["monitor_checks"].as_u64())
        .map(|count| count as usize)
        .sum();
    let query_ready_checks: usize = reports
        .iter()
        .filter_map(|report| report["query_ready_checks"].as_u64())
        .map(|count| count as usize)
        .sum();
    let returned_resource_points: usize = reports
        .iter()
        .filter_map(|report| report["returned_resource_points"].as_u64())
        .map(|count| count as usize)
        .sum();
    let end_unix_ms = unix_ms();
    let duration_ms = started.elapsed().as_millis();
    let largest_resource_history = reports
        .iter()
        .filter_map(|report| report["largest_resource_history"].as_u64())
        .max();
    let peak_sampled_resident_bytes = reports
        .iter()
        .filter_map(|report| report["peak_sampled_resident_bytes"].as_u64())
        .max();

    Ok(json!({
        "enabled": true,
        "controllers": CONTROLLERS,
        "checks_per_controller": CHECKS_PER_CONTROLLER,
        "owner_checks": owner_checks,
        "monitor_checks": monitor_checks,
        "query_ready_checks": query_ready_checks,
        "returned_resource_points": returned_resource_points,
        "largest_resource_history": largest_resource_history,
        "resource_history_limit": RESOURCE_HISTORY_LIMIT,
        "peak_sampled_resident_bytes": peak_sampled_resident_bytes,
        "started_unix_ms": started_unix_ms,
        "ended_unix_ms": end_unix_ms,
        "duration_ms": duration_ms,
        "owner_identity": {"owner_id": owner_id, "generation": owner_generation},
        "expected_run_generation": run,
        "expected_process_id": process_id,
        "process_exists_expected": true,
        "resource_point_note": "Returned native observer snapshot points; repeated responses may contain the same point and are not throughput samples.",
        "controllers_report": reports,
    }))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
