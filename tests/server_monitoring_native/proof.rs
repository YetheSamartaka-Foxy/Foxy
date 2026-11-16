use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

#[path = "query.rs"]
mod query;

type Invoke = fn(&Path, &[&str], i32) -> io::Result<Value>;

pub(crate) fn wait(
    config: &Path,
    id: &str,
    hostname: &str,
    owner: &str,
    generation: u64,
    endpoint: SocketAddr,
    invoke: Invoke,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = Value::Null;
    loop {
        let value = invoke(config, &["server", "instance", "monitor", id], 0)?;
        let report = &value["data"]["monitoring"];
        if report["status"]["owner_id"] != owner
            || report["status"]["run_generation"].as_u64() != Some(generation)
            || report["status"]["process_exists"] != true
            || report["status"]["readiness_verified"] != false
        {
            return Err(io::Error::other(
                "Monitoring lost the expected retained native run identity",
            ));
        }
        if complete(report, hostname) {
            let observation = &report["observation"];
            if observation["owner_id"] != owner
                || observation["run_generation"].as_u64() != Some(generation)
            {
                return Err(io::Error::other(
                    "Cached monitoring belongs to a different native run",
                ));
            }
            let resources = observation["resources"]
                .as_array()
                .ok_or_else(|| io::Error::other("Missing resource history"))?;
            let retained_game_pid = resources
                .last()
                .and_then(|point| point["sample"]["pid"].as_u64())
                .ok_or_else(|| io::Error::other("Missing retained game PID"))?;
            if resources.len() > 60
                || observation["logs"]["entries"]
                    .as_array()
                    .is_none_or(|lines| lines.len() > 128)
            {
                return Err(io::Error::other(
                    "Native observation exceeded its history bounds",
                ));
            }
            #[cfg(target_os = "linux")]
            {
                let anchor = report["status"]["process_id"]
                    .as_u64()
                    .ok_or_else(|| io::Error::other("Missing Linux anchor"))?;
                let game = resources
                    .last()
                    .and_then(|point| point["sample"]["pid"].as_u64())
                    .ok_or_else(|| io::Error::other("Missing game PID"))?;
                let children =
                    fs::read_to_string(format!("/proc/{anchor}/task/{anchor}/children"))?;
                if game == anchor
                    || !children
                        .split_whitespace()
                        .any(|child| child.parse::<u64>() == Ok(game))
                {
                    return Err(io::Error::other(
                        "Resource sample does not describe the retained namespace's game child",
                    ));
                }
            }
            let logs = invoke(
                config,
                &["server", "instance", "logs", id, "--limit", "1"],
                0,
            )?;
            fs::write(
                config.join(format!("monitoring_before_loss_{id}_{generation}.json")),
                serde_json::to_vec_pretty(report)?,
            )?;
            let transport = match query::prove_lost_reply_timeout(endpoint, hostname) {
                Ok(proof) => proof,
                Err(error) => {
                    let _ = fs::write(
                        config.join(format!("monitoring_loss_failed_{id}_{generation}.txt")),
                        error.to_string(),
                    );
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                    return Err(error);
                }
            };
            let retained = invoke(config, &["server", "instance", "monitor", id], 0)?;
            let retained_status = &retained["data"]["monitoring"]["status"];
            let selected_log = logs["data"]["logs"]["entries"]
                .as_array()
                .filter(|lines| lines.len() == 1)
                .and_then(|lines| lines.first())
                .ok_or_else(|| {
                    io::Error::other("CLI bounded log tail did not return exactly one line")
                })?;
            let same_log = [report, &retained["data"]["monitoring"]]
                .iter()
                .any(|candidate| {
                    candidate["observation"]["logs"]["entries"]
                        .as_array()
                        .is_some_and(|lines| lines.contains(selected_log))
                });
            if !same_log
                || logs["data"]["owner_id"] != owner
                || logs["data"]["run_generation"].as_u64() != Some(generation)
            {
                return Err(io::Error::other(
                    "CLI log tail differs from the authenticated current-run monitoring snapshot",
                ));
            }
            if retained_status["owner_id"] != owner
                || retained_status["run_generation"].as_u64() != Some(generation)
                || retained_status["process_exists"] != true
            {
                return Err(io::Error::other(
                    "Retained native process did not remain alive after the isolated A2S loss probe",
                ));
            }
            let retained_game = retained["data"]["monitoring"]["observation"]["resources"]
                .as_array()
                .and_then(|history| history.last())
                .map(|point| &point["sample"]);
            if !retained_game.is_some_and(|sample| {
                sample["pid"].as_u64() == Some(retained_game_pid) && sample["state"] == "Available"
            }) {
                return Err(io::Error::other(
                    "Native game process was not independently retained after the A2S loss probe",
                ));
            }
            fs::write(
                config.join(format!("monitoring_{id}_{generation}.json")),
                serde_json::to_vec_pretty(
                    &serde_json::json!({"monitoring":report,"logs_snapshot":logs,"transport_loss_proof":transport,"retained_after_transport_loss":retained_status}),
                )?,
            )?;
            eprintln!(
                "Fresh owned monitoring verified: query identity, resource birth/CPU/RSS and bounded logs, run {generation}"
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            fs::write(
                config.join(format!("monitoring_failed_{id}_{generation}.json")),
                serde_json::to_vec_pretty(&value)?,
            )?;
            return Err(io::Error::other(format!(
                "Native monitoring did not become complete: {last}"
            )));
        }
        last = serde_json::json!({"health":report["health"], "query":report["observation"]["query"], "resource":report["observation"]["resources"].as_array().and_then(|items|items.last()), "log_count":report["observation"]["logs"]["entries"].as_array().map(Vec::len)});
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn complete(report: &Value, hostname: &str) -> bool {
    let observation = &report["observation"];
    let resource = observation["resources"]
        .as_array()
        .and_then(|samples| samples.last())
        .map(|sample| &sample["sample"]);
    report["query_ready"] == true
        && report["health"] == "healthy"
        && observation["query"]["last_good"]["name"] == hostname
        && observation["query"]["last_good"]["max_players"]
            .as_u64()
            .is_some_and(|players| players > 0)
        && report["observation_age_ms"]
            .as_u64()
            .is_some_and(|age| age <= 10_000)
        && resource.is_some_and(|sample| {
            sample["state"] == "Available"
                && sample["cpu_percent"]
                    .as_f64()
                    .is_some_and(|cpu| cpu.is_finite() && cpu >= 0.0)
                && sample["resident_bytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes > 0)
                && sample["process_start_time_secs"]
                    .as_u64()
                    .is_some_and(|birth| birth > 0)
        })
        && observation["logs"]["entries"]
            .as_array()
            .is_some_and(|lines| !lines.is_empty())
}

#[cfg(test)]
mod tests {
    use super::complete;
    use serde_json::json;

    fn complete_report() -> serde_json::Value {
        json!({
            "query_ready": true,
            "health": "healthy",
            "observation_age_ms": 100,
            "observation": {
                "query": {"last_good": {"name": "Native fixture", "max_players": 4}},
                "resources": [{"sample": {"state": "Available", "cpu_percent": 0.0, "resident_bytes": 1, "process_start_time_secs": 1}}],
                "logs": {"entries": ["native log"]}
            }
        })
    }

    #[test]
    fn monitoring_completion_rejects_missing_or_mismatched_evidence() {
        let report = complete_report();
        assert!(complete(&report, "Native fixture"));

        let mut mismatch = report.clone();
        mismatch["observation"]["query"]["last_good"]["name"] = json!("Other server");
        assert!(!complete(&mismatch, "Native fixture"));

        let mut unknown_resource = report.clone();
        unknown_resource["observation"]["resources"][0]["sample"]["state"] = json!("Unknown");
        assert!(!complete(&unknown_resource, "Native fixture"));

        let mut stale = report.clone();
        stale["observation_age_ms"] = json!(10_001);
        assert!(!complete(&stale, "Native fixture"));

        let mut empty_logs = report;
        empty_logs["observation"]["logs"]["entries"] = json!([]);
        assert!(!complete(&empty_logs, "Native fixture"));
    }
}
