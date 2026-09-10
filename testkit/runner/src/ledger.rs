use crate::collect::{expect::dotted, sol};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::Path};

fn fallback(value: &Value, default: Value) -> Value {
    if value.is_null() {
        default
    } else {
        value.clone()
    }
}
pub fn round(value: f64, places: i32) -> f64 {
    let scale = 10_f64.powi(places);
    (value * scale).round_ties_even() / scale
}
pub fn median(values: &[f64]) -> Option<f64> {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n == 0 {
        None
    } else if n % 2 == 1 {
        Some(values[n / 2])
    } else {
        Some((values[n / 2 - 1] + values[n / 2]) / 2.0)
    }
}
pub fn percentile(values: &[f64], percentile: f64) -> Option<f64> {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        None
    } else {
        Some(
            values[((percentile / 100.0 * values.len() as f64).ceil() as usize)
                .saturating_sub(1)
                .min(values.len() - 1)],
        )
    }
}
pub fn delta_savings(summary: &Value) -> f64 {
    let full = summary["full_download_bytes"].as_f64().unwrap_or(0.0);
    if full <= 0.0 {
        0.0
    } else {
        100.0 * summary["patch_savings_bytes"].as_f64().unwrap_or(0.0) / full
    }
}
fn db_metric(breakdown: &Value, names: &[&str]) -> Value {
    breakdown["db"]["values"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| names.iter().filter_map(|name| item[name].as_f64()))
        .max_by(f64::total_cmp)
        .map_or(Value::Null, Value::from)
}
pub fn build_row(
    mut metadata: Value,
    summary: &Value,
    sol: &[Value],
    breakdown: &Value,
    mutation: &Value,
) -> Value {
    let metrics = &breakdown["run_metrics"];
    if let Some(elapsed) = metadata["elapsed_s"].as_f64() {
        metadata["elapsed_s"] = round(elapsed, 6).into();
    }
    for key in ["profile", "seed"] {
        metadata[format!("mutation_{key}")] = mutation[key].clone();
    }
    for key in ["mutated_parts", "mutated_bytes"] {
        metadata[key] = fallback(&mutation[key], 0.into());
    }
    for (field, op) in [
        ("download", "download"),
        ("hash", "hash"),
        ("quick_scan", "quick_scan"),
        ("startup", "startup"),
        ("startup_probe", "startup_probe"),
        ("app_update_check", "app_update_check"),
    ] {
        metadata[field] = sol::operation(sol, op);
    }
    let mut sums = json!({"delta_savings_percent":round(delta_savings(summary),4)});
    for key in [
        "download_stage_ms",
        "hash_stage_ms",
        "total_ms",
        "startup_total_ms",
        "first_frame_ms",
        "dispatch_ms",
        "eligibility_ms",
        "verdict_ms",
    ] {
        sums[key] = summary[key].clone();
    }
    for (key, metric) in [("files_updated", "files"), ("downloaded_bytes", "bytes")] {
        sums[key] = fallback(&summary[key], fallback(&metrics[metric], 0.into()));
    }
    for key in [
        "parts_updated",
        "patched_files",
        "patch_savings_bytes",
        "full_download_bytes",
    ] {
        sums[key] = fallback(&summary[key], 0.into());
    }
    metadata["summary"] = sums;
    // Null when the artifact predates the memory lane, which keeps `replay`
    // byte-identical over recorded runs.
    metadata["memory"] = summary["memory"].clone();
    if let Some(block) = metadata["memory"].as_object_mut() {
        block.remove("series");
    }
    let samples: Vec<&Value> = summary["telemetry_samples"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let field = |key: &str| {
        samples
            .iter()
            .map(|sample| sample[key].as_f64().unwrap_or(0.0))
            .collect::<Vec<_>>()
    };
    metadata["telemetry"] = json!({"cpu_p50":percentile(&field("cpu_percent"),50.0),"cpu_p95":percentile(&field("cpu_percent"),95.0),"mem_peak_bytes":field("memory_bytes").into_iter().max_by(f64::total_cmp),"disk_write_p50_bps":percentile(&field("disk_write_bps"),50.0)});
    let mut database = json!({});
    for (target, source, names) in [
        (
            "write_time_ms",
            "db_write_time_ms",
            &["db_write_time_ms", "write_ms"][..],
        ),
        (
            "retries",
            "lock_retries",
            &["retries", "lock_retries", "total_retries"][..],
        ),
        ("backoff_ms", "total_backoff_ms", &["backoff_ms"][..]),
        (
            "permit_wait_ms",
            "permit_wait_ms_total",
            &["permit_wait_ms"][..],
        ),
    ] {
        database[target] = fallback(&metrics[source], db_metric(breakdown, names));
    }
    database["purge_txn_s"] = db_metric(breakdown, &["txn"]);
    for (target, source) in [
        ("write_calls", "write_calls_total"),
        ("write_failures", "write_failures_total"),
        ("write_retries", "write_retries_total"),
        ("checkpoint_s", "checkpoint_total_s"),
    ] {
        database[target] = metrics[source].clone();
    }
    metadata["database"] = database;
    metadata["breakdown"] = breakdown.clone();
    if metadata["flags"].is_null() {
        metadata["flags"] = json!([]);
    }
    metadata["verdict"] = if metadata["flags"].as_array().is_some_and(|f| !f.is_empty()) {
        "invalid"
    } else {
        "ok"
    }
    .into();
    metadata
}

pub fn append(row: &Value, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", serde_json::to_string(row)?)?;
    Ok(())
}
pub fn read(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::fs::read_to_string(path)?
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}
pub const DEFINITIONS: &[(&str, bool, f64)] = &[
    ("elapsed_s", false, 0.12),
    ("download.sol", true, 0.08),
    ("download.actual_bps", true, 0.08),
    ("hash.sol", true, 0.08),
    ("hash.actual_s", false, 0.12),
    ("quick_scan.actual_s", false, 0.12),
    ("summary.total_ms", false, 0.12),
    ("summary.download_stage_ms", false, 0.12),
    ("summary.hash_stage_ms", false, 0.12),
    ("summary.delta_savings_percent", true, 0.0),
    ("database.write_time_ms", false, 0.12),
    ("database.permit_wait_ms", false, 0.12),
    ("database.write_retries", false, 0.0),
    ("database.purge_txn_s", false, 0.12),
    // Footprint moves in steps, not in percent, so it gets a looser band than
    // wall clock: a 10% commit swing is ordinary allocator behaviour, and a
    // real regression clears it easily.
    ("memory.peak_private_bytes", false, 0.15),
    ("memory.retained_private_bytes", false, 0.15),
    ("memory.growth_private_bytes", false, 0.5),
];
const COUNTERS: &[&str] = &[
    "summary.downloaded_bytes",
    "summary.files_updated",
    "summary.parts_updated",
    "mutated_parts",
];
pub fn warm_medians(rows: &[Value]) -> Value {
    let mut groups: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| r["cache_state"] == "warm" && r["verdict"] != "invalid")
    {
        groups
            .entry(row["op"].as_str().unwrap_or(""))
            .or_default()
            .push(row);
    }
    let mut result = json!({});
    for (op, rows) in groups {
        let mut metrics = json!({});
        for path in DEFINITIONS
            .iter()
            .map(|d| d.0)
            .chain(COUNTERS.iter().copied())
        {
            let values: Vec<f64> = rows
                .iter()
                .filter_map(|row| dotted(row, path).as_f64())
                .collect();
            if let Some(value) = median(&values) {
                metrics[path] = value.into();
            }
        }
        result[op] = json!({"samples":rows.len(),"metrics":metrics});
    }
    result
}
pub fn save_baseline(rows: &[Value], path: &Path, case_hash: &str, git_sha: &str) -> Result<()> {
    ensure!(
        !rows.iter().any(|row| row["verdict"] == "invalid"),
        "Cannot accept a baseline containing invalid rows"
    );
    if path.exists() {
        ensure!(
            crate::case::read_json(path)?["case_hash"] == case_hash,
            "Existing baseline has a different case hash; a changed case starts a new history"
        );
    }
    let medians = warm_medians(rows);
    let operations = medians.as_object().unwrap();
    ensure!(
        !operations.is_empty(),
        "No warm samples are available for a baseline"
    );
    for (op, entry) in operations {
        ensure!(
            entry["samples"].as_u64().unwrap_or(0) >= 2,
            "Baseline operation {op} needs at least two warm samples"
        );
    }
    crate::case::write_json(
        path,
        &json!({"accepted_utc":chrono::Utc::now().to_rfc3339(),"git_sha":git_sha,"case_hash":case_hash,"operations":medians,"tolerances":{"sol":0.08,"duration":0.12,"correctness":0.0}}),
    )
}
pub fn compare(rows: &[Value], baseline_path: &Path, ledger_path: &Path) -> Result<Value> {
    if !baseline_path.exists() {
        return Ok(json!({"verdict":"no-baseline","deltas":[],"flags":[]}));
    }
    let baseline = crate::case::read_json(baseline_path)?;
    let current = rows.first().unwrap_or(&Value::Null);
    if baseline["case_hash"] != current["case_hash"] {
        return Ok(
            json!({"verdict":"case-hash-changed","deltas":[],"flags":["case-hash-changed"]}),
        );
    }
    let history: Vec<Value> = read(ledger_path)?
        .into_iter()
        .filter(|row| {
            row["run_id"] != current["run_id"]
                && [
                    "case_hash",
                    "harness",
                    "build_kind",
                    "database_mode",
                    "db_write_gate",
                    "db_pool_idle",
                    "storage_class",
                ]
                .iter()
                .all(|key| row[key] == current[key])
        })
        .collect();
    let last = history.last().map(|r| r["run_id"].clone());
    let previous = warm_medians(
        &history
            .into_iter()
            .filter(|r| Some(&r["run_id"]) == last.as_ref())
            .collect::<Vec<_>>(),
    );
    let medians = warm_medians(rows);
    let mut deltas = Vec::new();
    let mut flags = Vec::new();
    let (mut regression, mut improvement, mut confirmed_bad, mut confirmed_good) =
        (false, false, false, false);
    for (op, entry) in medians.as_object().unwrap() {
        let base = &baseline["operations"][op];
        if base.is_null() {
            continue;
        }
        for &(path, higher, tolerance) in DEFINITIONS {
            let (Some(now), Some(was)) = (
                entry["metrics"][path].as_f64(),
                base["metrics"][path].as_f64(),
            ) else {
                continue;
            };
            if was == 0.0 {
                continue;
            }
            let change = (now - was) / was.abs();
            let normalized = if higher { -change } else { change };
            let bad = normalized > tolerance;
            let good = normalized < -tolerance;
            regression |= bad;
            improvement |= good;
            let prior = previous[op]["metrics"][path]
                .as_f64()
                .map(|n| (n - was) / was.abs());
            let prior_normalized = prior.map(|n| if higher { -n } else { n });
            let previous_bad = prior_normalized.is_some_and(|n| n > tolerance);
            let previous_good = prior_normalized.is_some_and(|n| n < -tolerance);
            confirmed_bad |= bad && previous_bad;
            confirmed_good |= good && previous_good;
            let status = if bad && previous_bad {
                "regression"
            } else if bad {
                "candidate-regression"
            } else if good && previous_good {
                "improvement"
            } else if good {
                "candidate-improvement"
            } else {
                "within-tolerance"
            };
            deltas.push(json!({"op":op,"metric":path,"baseline":was,"current":now,"change_percent":round(100.0*change,2),"previous_run_change_percent":prior.map(|p|round(100.0*p,2)),"status":status}));
        }
        if improvement {
            for &counter in COUNTERS {
                if let (Some(now), Some(was)) = (
                    entry["metrics"][counter].as_f64(),
                    base["metrics"][counter].as_f64(),
                ) && now < was
                {
                    if flags.is_empty() {
                        flags.push("work-conservation");
                    }
                    deltas.push(json!({"op":op,"metric":counter,"baseline":was,"current":now,"change_percent":if was==0.0{None}else{Some(round(100.0*(now-was)/was.abs(),2))},"previous_run_change_percent":null,"status":"work-conservation"}));
                }
            }
        }
    }
    let verdict = if confirmed_bad {
        "regression"
    } else if regression {
        "candidate-regression"
    } else if confirmed_good {
        "improvement"
    } else if improvement {
        "candidate-improvement"
    } else {
        "ok"
    };
    Ok(json!({"verdict":verdict,"deltas":deltas,"flags":flags}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statistics() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0, 4.0]), Some(2.5));
        assert_eq!(percentile(&[1.0, 3.0, 2.0], 95.0), Some(3.0));
        assert_eq!(
            delta_savings(&json!({"full_download_bytes":100,"patch_savings_bytes":97})),
            97.0
        );
    }
    #[test]
    fn warm_filter() {
        let rows = vec![
            json!({"op":"download","cache_state":"warm","verdict":"ok","elapsed_s":2}),
            json!({"op":"download","cache_state":"warm","verdict":"invalid","elapsed_s":100}),
            json!({"op":"download","cache_state":"cold","verdict":"ok","elapsed_s":50}),
        ];
        assert_eq!(warm_medians(&rows)["download"]["metrics"]["elapsed_s"], 2.0);
    }
}
