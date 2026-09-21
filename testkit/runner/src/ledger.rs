use crate::collect::{expect::dotted, sol};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write, path::Path};

pub const DERIVED_SCHEMA_VERSION: u64 = 8;

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

fn operation_identity(metadata: &Value) -> &str {
    let requested_op = metadata["op"].as_str().unwrap_or("unknown");
    if requested_op == "startup" {
        "O8 startup"
    } else if requested_op == "recheck-integrity" {
        "O3 tree hash verification"
    } else if requested_op.starts_with("quick-check") {
        "O4 quick scan"
    } else if requested_op == "remote-refresh" {
        "O5 remote metadata refresh"
    } else if requested_op == "recheck" {
        "O6 no-change sync"
    } else if !metadata["delta_patch"].is_null() {
        let fallbacks = metadata["delta_patch"]["fallbacks"].as_u64().unwrap_or(0);
        let patched = metadata["delta_patch"]["patched_files"]
            .as_u64()
            .unwrap_or(0);
        if fallbacks > 0 {
            "O2 delta patch + O1 full-download fallback"
        } else if patched > 0 {
            "O2 delta patch"
        } else {
            "O2 delta patch attempt"
        }
    } else if !metadata["download"].is_null() {
        "O1 full-file download"
    } else {
        requested_op
    }
}
pub fn build_row(
    mut metadata: Value,
    summary: &Value,
    sol: &[Value],
    breakdown: &Value,
    mutation: &Value,
) -> Value {
    metadata["derived_schema_version"] = DERIVED_SCHEMA_VERSION.into();
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
    let mut aggregate = json!({});
    for (field, op) in [
        ("download", "download"),
        ("delta_patch", "delta_patch"),
        ("hash", "hash"),
        ("quick_scan", "quick_scan"),
        ("startup", "startup"),
        ("startup_probe", "startup_probe"),
        ("app_update_check", "app_update_check"),
        ("remote_refresh", "remote_refresh"),
        ("sync_action", "sync_action"),
        ("db_persist", "db_persist"),
        ("db_purge", "db_purge"),
        ("space_switch", "space_switch"),
    ] {
        metadata[field] = sol::operation(sol, op);
        aggregate[field] = sol::aggregate(sol, op);
    }
    metadata["sol_aggregate"] = aggregate;
    metadata["delta_patch_stages"] = sol::records(sol, "delta_patch_stage");
    metadata["operation_identity"] = operation_identity(&metadata).into();
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
        "dependency_bound_ms",
        "join_ms",
        "dependency_coverage_percent",
        "cancel_quiescent_ms",
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
    if summary["ui_probe"].is_object() {
        sums["ui_probe"] = summary["ui_probe"].clone();
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
    crate::references::attach(&mut metadata);
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
    // Calibrated estimates against the references the row cites.
    ("download.sol_calibrated", true, 0.08),
    ("hash.sol_calibrated", true, 0.08),
    ("startup_probe.sol_calibrated", true, 0.08),
    ("quick_scan.sol_calibrated", true, 0.08),
    ("sync_action.sol_calibrated", true, 0.08),
    ("hash.sol", true, 0.08),
    // Total over every hash run of the operation: the `hash` record itself is
    // only the last run, one arbitrary page-cache batch on a download row.
    ("breakdown.run_metrics.hash_total_s", false, 0.12),
    ("quick_scan.actual_s", false, 0.12),
    // Complete-action records (O5, O6 and the pipeline exit itself).
    ("remote_refresh.actual_s", false, 0.12),
    ("sync_action.actual_s", false, 0.12),
    ("summary.total_ms", false, 0.12),
    ("summary.download_stage_ms", false, 0.12),
    ("summary.hash_stage_ms", false, 0.12),
    ("summary.cancel_quiescent_ms", false, 0.12),
    // Frame probe beside the operation: the worst frame interval the app saw.
    ("summary.ui_probe.frame_ms_max", false, 0.25),
    ("summary.ui_probe.frame_ms_p95_worst", false, 0.25),
    ("download.ramp_s", false, 0.25),
    ("download.tail_s", false, 0.25),
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
    "summary.join_ms",
    "summary.dependency_coverage_percent",
];
/// The row fields that must match before two rows measure the same thing.
/// Persisted with an accepted baseline and checked on every comparison, so a
/// debug build, a GUI harness or another storage class cannot inherit a
/// baseline recorded under different conditions.
pub const PROFILE_KEYS: &[&str] = &[
    "harness",
    "build_kind",
    "database_mode",
    "db_write_gate",
    "db_pool_idle",
    "storage_class",
    "diagnostics",
];

/// Environment properties (from `guards::environment_fingerprint`) whose change
/// expires an accepted baseline: the numbers were measured on other hardware
/// or against another origin and are not a reference for this one.
pub const ENVIRONMENT_KEYS: &[&str] = &["cpu", "os", "memory_gb", "origin"];
const BASELINE_FORMAT_VERSION: u64 = 2;
const MIN_BASELINE_SAMPLES: u64 = 5;
const BASELINE_REQUIRED_ROW_KEYS: &[&str] = &[
    "mutation_profile",
    "mutation_seed",
    "origin_checksum",
    "references",
    "diagnostics",
    "cache_state",
];

pub fn profile_of(row: &Value) -> Value {
    let mut profile = json!({});
    for key in PROFILE_KEYS {
        profile[*key] = row[*key].clone();
    }
    if !row["environment"].is_null() {
        profile["environment"] = row["environment"].clone();
    }
    profile
}

/// The identity a row's measurements are grouped under: the operation label
/// when the case gave one (two `quick-check` operations with different
/// initial states are different measurements), else the operation name, and
/// a lane suffix for rows that ran against an evicted cache.
pub fn op_key(row: &Value) -> String {
    let name = row["label"]
        .as_str()
        .or(row["op"].as_str())
        .unwrap_or("")
        .to_owned();
    if row["cache_state"] == "evicted" {
        format!("{name}@evicted")
    } else {
        name
    }
}

fn operation_compatibility(row: &Value) -> Value {
    json!({
        "op": row["op"],
        "label": row["label"],
        "cache_state": row["cache_state"],
        "mutation_profile": row["mutation_profile"],
        "mutation_seed": row["mutation_seed"],
        "origin_checksum": row["origin_checksum"],
        "references": crate::references::ids(&row["references"]),
        "diagnostics": row["diagnostics"],
    })
}

fn terminal_outcome(row: &Value) -> Value {
    for record in ["sync_action", "download", "hash", "quick_scan", "startup"] {
        if let Some(outcome) = row[record]["outcome"].as_str() {
            return outcome.into();
        }
    }
    row["verdict"].clone()
}

fn oracle_outcome(row: &Value) -> String {
    if row["flags"]
        .as_array()
        .is_some_and(|flags| flags.iter().any(|flag| flag == "oracle-failed"))
    {
        "failed".to_owned()
    } else if let Some(outcome) = row["oracle_outcome"].as_str() {
        outcome.to_owned()
    } else {
        "not-recorded".to_owned()
    }
}

/// Medians per operation key over the steady-state rows: warm rows and
/// evicted rows, each in its own lane; cold iteration-zero rows and invalid
/// rows are left out.
/// Iteration 0 without warmup is the unprepared first pass and never a
/// baseline sample; a warm or evicted valid row is.
fn is_baseline_sample(row: &Value) -> bool {
    (row["cache_state"] == "warm" || row["cache_state"] == "evicted") && row["verdict"] != "invalid"
}

pub fn warm_medians(rows: &[Value]) -> Value {
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for row in rows.iter().filter(|row| is_baseline_sample(row)) {
        groups.entry(op_key(row)).or_default().push(row);
    }
    let mut result = json!({});
    for (op, rows) in groups {
        let mut metrics = json!({});
        let mut spread = json!({});
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
                let min = values.iter().copied().reduce(f64::min).unwrap_or(value);
                let max = values.iter().copied().reduce(f64::max).unwrap_or(value);
                spread[path] = json!({"min":min,"max":max,"absolute_range":max-min});
            }
        }
        let mut outcomes: Vec<Value> = rows.iter().map(|row| terminal_outcome(row)).collect();
        outcomes.sort_by_key(Value::to_string);
        outcomes.dedup();
        let mut oracle_outcomes: BTreeMap<String, usize> = BTreeMap::new();
        for row in &rows {
            *oracle_outcomes.entry(oracle_outcome(row)).or_default() += 1;
        }
        result[op] = json!({
            "samples": rows.len(),
            "cache_state": rows[0]["cache_state"],
            "op": rows[0]["op"],
            "label": rows[0]["label"],
            "compatibility": operation_compatibility(rows[0]),
            "metrics": metrics,
            "spread": spread,
            "outcomes": outcomes,
            "oracle_outcomes": oracle_outcomes,
        });
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
    let profile = profile_of(rows.first().unwrap_or(&Value::Null));
    ensure!(
        profile["build_kind"] == "release",
        "Baseline acceptance requires a release build"
    );
    for row in rows {
        for key in BASELINE_REQUIRED_ROW_KEYS {
            ensure!(
                row.get(*key).is_some(),
                "Baseline row is missing compatibility field {key}"
            );
        }
    }
    ensure!(
        rows.iter().all(|row| profile_of(row) == profile),
        "Cannot accept a baseline from rows with different run profiles"
    );
    let mut medians = warm_medians(rows);
    // An incidental stage (a `wipe-db` before the measured refresh) collects
    // one sample fewer than the measured operation when iteration 0 is its
    // unprepared pass, so it stays out of the baseline rather than blocking
    // the operation the case exists to measure.
    let mut unbaselined = json!({});
    for (op, entry) in medians.as_object_mut().unwrap() {
        let expected = &entry["compatibility"];
        ensure!(
            rows.iter()
                .filter(|row| op_key(row) == *op && is_baseline_sample(row))
                .all(|row| operation_compatibility(row) == *expected),
            "Baseline operation {op} contains incompatible samples"
        );
        if entry["samples"].as_u64().unwrap_or(0) < MIN_BASELINE_SAMPLES {
            unbaselined[op] = json!({"samples": entry["samples"]});
        }
    }
    for op in unbaselined.as_object().unwrap().keys() {
        medians.as_object_mut().unwrap().remove(op);
    }
    ensure!(
        !medians.as_object().unwrap().is_empty(),
        "No operation has at least {MIN_BASELINE_SAMPLES} compatible successful samples"
    );
    let first = rows.first().unwrap_or(&Value::Null);
    crate::case::write_json(
        path,
        &json!({"format_version":BASELINE_FORMAT_VERSION,"accepted_utc":chrono::Utc::now().to_rfc3339(),"git_sha":git_sha,"case_hash":case_hash,"origin_checksum":first["origin_checksum"],"references":crate::references::ids(&first["references"]),"profile":profile,"operations":medians,"unbaselined_operations":unbaselined,"tolerances":{"sol":0.08,"duration":0.12,"correctness":0.0}}),
    )
}

/// Validate the global and operation-specific identity of an accepted baseline.
/// Older baselines remain readable, but cannot silently compare as current.
pub fn compatibility(baseline: &Value, current: &Value, operation: Option<&Value>) -> Value {
    if baseline["format_version"].as_u64() != Some(BASELINE_FORMAT_VERSION) {
        return json!({"verdict":"rebaseline-required","flags":["baseline-format-missing"]});
    }
    if baseline["case_hash"] != current["case_hash"] {
        return json!({"verdict":"case-hash-changed","flags":["case-hash-changed"]});
    }
    if baseline["profile"].is_null() {
        return json!({"verdict":"rebaseline-required","flags":["baseline-profile-missing"]});
    }
    let mismatched: Vec<String> = PROFILE_KEYS
        .iter()
        .filter(|key| baseline["profile"][**key] != current[**key])
        .map(|key| format!("profile-mismatch:{key}"))
        .collect();
    if !mismatched.is_empty() {
        return json!({"verdict":"profile-mismatch","flags":mismatched});
    }
    if baseline["origin_checksum"] != current["origin_checksum"] {
        return json!({"verdict":"case-hash-changed","flags":["origin-changed"]});
    }
    let current_ids = crate::references::ids(&current["references"]);
    if baseline["references"] != current_ids {
        return json!({"verdict":"rebaseline-required","flags":["reference-changed"]});
    }
    if !baseline["profile"]["environment"].is_null() {
        let expired: Vec<String> = ENVIRONMENT_KEYS
            .iter()
            .filter(|key| {
                baseline["profile"]["environment"][**key] != current["environment"][**key]
            })
            .map(|key| format!("environment-changed:{key}"))
            .collect();
        if !expired.is_empty() {
            return json!({"verdict":"rebaseline-required","flags":expired});
        }
    }
    if let Some(operation) = operation
        && operation["compatibility"] != operation_compatibility(current)
    {
        return json!({"verdict":"rebaseline-required","flags":["operation-profile-changed"]});
    }
    json!({"verdict":"compatible","flags":[]})
}
/// Absolute change below which a duration metric is noise whatever the percent
/// says: a 12 percent band over a 9 ms hash stage, a 4 ms quick scan or a 63 ms
/// page-cache hash batch flags scheduler jitter as a verdict. Seconds-valued paths end in `_s`, millisecond
/// paths in `_ms`; ratios, rates, bytes and counters have no floor.
pub fn noise_floor(path: &str) -> f64 {
    if path.ends_with("_s") {
        0.05
    } else if path.ends_with("_ms") {
        50.0
    } else {
        0.0
    }
}

/// Metrics that are guardrails rather than gates. Foxy deliberately spends
/// memory and disk to be faster, so a footprint move is reported and never
/// decides a verdict (conventions/SPEED_OF_LIGHT.md, resource trade policy).
pub fn is_advisory(path: &str) -> bool {
    path.starts_with("memory.")
}

/// `(regression, improvement)` for one metric against its baseline median.
pub fn classify(now: f64, was: f64, higher: bool, tolerance: f64, floor: f64) -> (bool, bool) {
    if (now - was).abs() < floor {
        return (false, false);
    }
    let change = (now - was) / was.abs();
    let normalized = if higher { -change } else { change };
    (normalized > tolerance, normalized < -tolerance)
}

pub fn compare(rows: &[Value], baseline_path: &Path, ledger_path: &Path) -> Result<Value> {
    if !baseline_path.exists() {
        return Ok(json!({"verdict":"no-baseline","deltas":[],"flags":[]}));
    }
    let baseline = crate::case::read_json(baseline_path)?;
    let current = rows.first().unwrap_or(&Value::Null);
    let compatible = compatibility(&baseline, current, None);
    if compatible["verdict"] != "compatible" {
        return Ok(
            json!({"verdict":compatible["verdict"],"deltas":[],"flags":compatible["flags"]}),
        );
    }
    let history: Vec<Value> = read(ledger_path)?
        .into_iter()
        .filter(|row| {
            row["run_id"] != current["run_id"]
                && row["case_hash"] == current["case_hash"]
                && PROFILE_KEYS.iter().all(|key| row[*key] == current[*key])
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
    let mut operation_verdicts = serde_json::Map::new();
    let (mut regression, mut improvement, mut confirmed_bad, mut confirmed_good) =
        (false, false, false, false);
    let mut rebaseline = false;
    for (op, entry) in medians.as_object().unwrap() {
        let base = &baseline["operations"][op];
        if base.is_null() && !baseline["unbaselined_operations"][op].is_null() {
            operation_verdicts.insert(op.clone(), "no-baseline".into());
            continue;
        }
        if base.is_null() {
            // The baseline has no lane for this operation (an evicted lane
            // recorded before lanes existed, a new label); say so instead of
            // passing an unmeasured operation.
            flags.push(format!("baseline-missing-op:{op}"));
            rebaseline = true;
            operation_verdicts.insert(op.clone(), "rebaseline-required".into());
            continue;
        }
        let operation_row = rows
            .iter()
            .find(|row| op_key(row) == *op)
            .unwrap_or(current);
        let compatible = compatibility(&baseline, operation_row, Some(base));
        if compatible["verdict"] != "compatible" {
            flags.extend(
                compatible["flags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
            rebaseline = true;
            operation_verdicts.insert(op.clone(), "rebaseline-required".into());
            continue;
        }
        if !base["cache_state"].is_null() && base["cache_state"] != entry["cache_state"] {
            flags.push(format!("cache-lane-mismatch:{op}"));
            rebaseline = true;
            operation_verdicts.insert(op.clone(), "rebaseline-required".into());
            continue;
        }
        let (mut op_regression, mut op_improvement, mut op_confirmed_bad, mut op_confirmed_good) =
            (false, false, false, false);
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
            let (bad, good) = classify(now, was, higher, tolerance, noise_floor(path));
            let prior = previous[op]["metrics"][path]
                .as_f64()
                .map(|n| (n - was) / was.abs());
            let (previous_bad, previous_good) = previous[op]["metrics"][path]
                .as_f64()
                .map_or((false, false), |n| {
                    classify(n, was, higher, tolerance, noise_floor(path))
                });
            // Footprint is a guardrail, not a gate: Foxy trades memory and
            // disk for speed on purpose, so a footprint move is reported as
            // advisory and never flips the run's verdict.
            if is_advisory(path) {
                let status = if bad {
                    "advisory-regression"
                } else if good {
                    "advisory-improvement"
                } else {
                    "within-tolerance"
                };
                if bad && !flags.iter().any(|flag| flag == "memory-advisory") {
                    flags.push("memory-advisory".to_owned());
                }
                deltas.push(json!({"op":op,"metric":path,"baseline":was,"current":now,"absolute_difference":round(now-was,6),"change_percent":round(100.0*change,2),"previous_run_change_percent":prior.map(|p|round(100.0*p,2)),"status":status}));
                continue;
            }
            op_regression |= bad;
            op_improvement |= good;
            op_confirmed_bad |= bad && previous_bad;
            op_confirmed_good |= good && previous_good;
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
            deltas.push(json!({"op":op,"metric":path,"baseline":was,"current":now,"absolute_difference":round(now-was,6),"change_percent":round(100.0*change,2),"previous_run_change_percent":prior.map(|p|round(100.0*p,2)),"status":status}));
        }
        if op_improvement {
            for &counter in COUNTERS {
                if let (Some(now), Some(was)) = (
                    entry["metrics"][counter].as_f64(),
                    base["metrics"][counter].as_f64(),
                ) && now < was
                {
                    if !flags.iter().any(|flag| flag == "work-conservation") {
                        flags.push("work-conservation".to_owned());
                    }
                    deltas.push(json!({"op":op,"metric":counter,"baseline":was,"current":now,"change_percent":if was==0.0{None}else{Some(round(100.0*(now-was)/was.abs(),2))},"previous_run_change_percent":null,"status":"work-conservation"}));
                }
            }
        }
        regression |= op_regression;
        improvement |= op_improvement;
        confirmed_bad |= op_confirmed_bad;
        confirmed_good |= op_confirmed_good;
        let operation_verdict = if op_confirmed_bad {
            "regression"
        } else if op_regression {
            "candidate-regression"
        } else if op_confirmed_good {
            "improvement"
        } else if op_improvement {
            "candidate-improvement"
        } else {
            "ok"
        };
        operation_verdicts.insert(op.clone(), operation_verdict.into());
    }
    let verdict = if confirmed_bad {
        "regression"
    } else if regression {
        "candidate-regression"
    } else if rebaseline && deltas.is_empty() {
        "rebaseline-required"
    } else if confirmed_good {
        "improvement"
    } else if improvement {
        "candidate-improvement"
    } else {
        "ok"
    };
    Ok(
        json!({"verdict":verdict,"operation_verdicts":operation_verdicts,"deltas":deltas,"flags":flags}),
    )
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
    fn operation_identity_uses_the_requested_action_before_incidental_stages() {
        let startup = json!({"op":"startup","quick_scan":{},"remote_refresh":{}});
        assert_eq!(operation_identity(&startup), "O8 startup");

        let recheck = json!({"op":"recheck-integrity","remote_refresh":{},"hash":{}});
        assert_eq!(operation_identity(&recheck), "O3 tree hash verification");

        let no_change = json!({"op":"recheck","remote_refresh":{},"quick_scan":{}});
        assert_eq!(operation_identity(&no_change), "O6 no-change sync");
    }
    #[test]
    fn duration_verdicts_need_both_the_band_and_the_floor() {
        // 9.5 ms -> 4.0 ms clears 12 percent but not 50 ms: noise.
        assert_eq!(
            classify(4.0, 9.5, false, 0.12, noise_floor("summary.hash_stage_ms")),
            (false, false)
        );
        // 63 ms -> 90 ms on a seconds path: the same.
        assert_eq!(
            classify(
                0.090,
                0.063,
                false,
                0.12,
                noise_floor("breakdown.run_metrics.hash_total_s")
            ),
            (false, false)
        );
        // 4 ms -> 200 ms clears both: a real quick-scan regression still flags.
        assert_eq!(
            classify(0.2, 0.004, false, 0.12, noise_floor("quick_scan.actual_s")),
            (true, false)
        );
        // 40.5 s -> 45.5 s regresses; 40.5 s -> 35.5 s improves.
        assert_eq!(
            classify(45.5, 40.5, false, 0.12, noise_floor("elapsed_s")),
            (true, false)
        );
        assert_eq!(
            classify(35.5, 40.5, false, 0.12, noise_floor("elapsed_s")),
            (false, true)
        );
        // Ratios have no floor and flip direction.
        assert_eq!(noise_floor("download.sol"), 0.0);
        assert_eq!(classify(0.80, 0.92, true, 0.08, 0.0), (true, false));
        assert_eq!(classify(0.99, 0.92, true, 0.08, 0.0), (false, false));
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
    #[test]
    fn labels_and_evicted_lanes_are_separate_groups() {
        let rows = vec![
            json!({"op":"quick-check","label":"quick-check-stale","cache_state":"warm","verdict":"ok","elapsed_s":0.4}),
            json!({"op":"quick-check","cache_state":"warm","verdict":"ok","elapsed_s":0.1}),
            json!({"op":"download","cache_state":"evicted","verdict":"ok","elapsed_s":58.0}),
            json!({"op":"download","cache_state":"warm","verdict":"ok","elapsed_s":29.0}),
        ];
        let medians = warm_medians(&rows);
        assert_eq!(medians["quick-check-stale"]["metrics"]["elapsed_s"], 0.4);
        assert_eq!(medians["quick-check"]["metrics"]["elapsed_s"], 0.1);
        assert_eq!(medians["download@evicted"]["metrics"]["elapsed_s"], 58.0);
        assert_eq!(medians["download@evicted"]["cache_state"], "evicted");
        assert_eq!(medians["download"]["metrics"]["elapsed_s"], 29.0);
    }
    fn row(elapsed: f64) -> Value {
        json!({"run_id":"r2","op":"download","cache_state":"warm","verdict":"ok","flags":[],"elapsed_s":elapsed,"case_hash":"h","harness":"cli","build_kind":"release","database_mode":"wal","db_write_gate":4,"db_pool_idle":1,"storage_class":"ssd","diagnostics":"none","mutation_profile":null,"mutation_seed":null,"origin_checksum":"origin","references":{}})
    }
    fn five(elapsed: f64) -> Vec<Value> {
        (0..5)
            .map(|offset| row(elapsed + f64::from(offset) / 10.0))
            .collect()
    }
    #[test]
    fn acceptance_ignores_the_unprepared_first_pass() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = five(10.0);
        let mut first = row(30.0);
        first["cache_state"] = "cold".into();
        rows.insert(0, first);
        let baseline = dir.path().join("baseline.json");
        save_baseline(&rows, &baseline, "h", "sha").unwrap();
        let saved = crate::case::read_json(&baseline).unwrap();
        assert_eq!(saved["operations"]["download"]["samples"], 5);
        assert_eq!(saved["operations"]["download"]["cache_state"], "warm");
    }
    #[test]
    fn an_under_sampled_incidental_stage_stays_out_of_the_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        let ledger = dir.path().join("ledger.jsonl");
        let mut rows = five(10.0);
        for offset in 0..4 {
            let mut wipe = row(1.0 + f64::from(offset) / 10.0);
            wipe["op"] = "wipe-db".into();
            rows.push(wipe);
        }
        save_baseline(&rows, &baseline, "h", "sha").unwrap();
        let saved = crate::case::read_json(&baseline).unwrap();
        assert_eq!(saved["operations"]["download"]["samples"], 5);
        assert!(saved["operations"]["wipe-db"].is_null());
        assert_eq!(saved["unbaselined_operations"]["wipe-db"]["samples"], 4);

        let comparison = compare(&rows, &baseline, &ledger).unwrap();
        assert_eq!(comparison["verdict"], "ok");
        assert_eq!(comparison["operation_verdicts"]["wipe-db"], "no-baseline");
        assert_eq!(comparison["flags"], json!([]));

        let only_wipes: Vec<Value> = rows
            .iter()
            .filter(|r| r["op"] == "wipe-db")
            .cloned()
            .collect();
        assert!(save_baseline(&only_wipes, &dir.path().join("wipes.json"), "h", "sha").is_err());
    }
    #[test]
    fn baselines_persist_and_validate_the_run_profile() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        let ledger = dir.path().join("ledger.jsonl");
        assert!(
            save_baseline(&five(9.0)[..4], &dir.path().join("short.json"), "h", "sha").is_err()
        );
        let mut debug_rows = five(9.0);
        for row in &mut debug_rows {
            row["build_kind"] = "debug".into();
        }
        assert!(save_baseline(&debug_rows, &dir.path().join("debug.json"), "h", "sha").is_err());
        let rows = five(10.0);
        save_baseline(&rows, &baseline, "h", "sha").unwrap();
        let saved = crate::case::read_json(&baseline).unwrap();
        assert_eq!(saved["profile"]["build_kind"], "release");
        assert_eq!(saved["profile"]["storage_class"], "ssd");
        assert_eq!(saved["operations"]["download"]["cache_state"], "warm");
        assert_eq!(
            saved["operations"]["download"]["spread"]["elapsed_s"]["min"],
            10.0
        );
        assert_eq!(
            saved["operations"]["download"]["spread"]["elapsed_s"]["max"],
            10.4
        );

        let same = compare(&rows, &baseline, &ledger).unwrap();
        assert_eq!(same["verdict"], "ok");

        let mut debug = row(10.0);
        debug["build_kind"] = "debug".into();
        let mismatch = compare(&[debug], &baseline, &ledger).unwrap();
        assert_eq!(mismatch["verdict"], "profile-mismatch");
        assert_eq!(mismatch["flags"], json!(["profile-mismatch:build_kind"]));

        let mut mixed = rows.clone();
        mixed[1]["storage_class"] = "hdd".into();
        assert!(save_baseline(&mixed, &dir.path().join("mixed.json"), "h", "sha").is_err());
    }
    #[test]
    fn comparison_verdicts_are_scoped_to_their_operation() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        let ledger = dir.path().join("ledger.jsonl");
        let mut baseline_rows = five(10.0);
        baseline_rows.extend(five(1.0).into_iter().map(|mut row| {
            row["op"] = "recheck".into();
            row
        }));
        save_baseline(&baseline_rows, &baseline, "h", "sha").unwrap();
        let mut recheck = row(1.2);
        recheck["op"] = "recheck".into();
        let comparison = compare(&[row(12.0), recheck], &baseline, &ledger).unwrap();
        assert_eq!(comparison["verdict"], "candidate-regression");
        assert_eq!(
            comparison["operation_verdicts"]["download"],
            "candidate-regression"
        );
        assert_eq!(comparison["operation_verdicts"]["recheck"], "ok");
    }
    #[test]
    fn a_changed_environment_expires_the_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        let ledger = dir.path().join("ledger.jsonl");
        let env = json!({"cpu":"9950X3D","os":"Windows 11","memory_gb":96,"origin":"a3.example.test:8080"});
        let with_env = |elapsed: f64| {
            let mut row = row(elapsed);
            row["environment"] = env.clone();
            row
        };
        let rows: Vec<Value> = (0..5)
            .map(|offset| with_env(10.0 + f64::from(offset) / 10.0))
            .collect();
        save_baseline(&rows, &baseline, "h", "sha").unwrap();
        let saved = crate::case::read_json(&baseline).unwrap();
        assert_eq!(
            saved["profile"]["environment"]["origin"],
            "a3.example.test:8080"
        );
        assert_eq!(
            compare(&[with_env(10.2)], &baseline, &ledger).unwrap()["verdict"],
            "ok"
        );
        let mut moved = with_env(10.2);
        moved["environment"]["origin"] = "loopback".into();
        moved["environment"]["cpu"] = "other".into();
        let expired = compare(&[moved], &baseline, &ledger).unwrap();
        assert_eq!(expired["verdict"], "rebaseline-required");
        assert_eq!(
            expired["flags"],
            json!(["environment-changed:cpu", "environment-changed:origin"])
        );
        // A row without an environment (older kit) against an environment-bearing
        // baseline is also not a match.
        let legacy = compare(&[row(10.2)], &baseline, &ledger).unwrap();
        assert_eq!(legacy["verdict"], "rebaseline-required");
    }
    #[test]
    fn a_footprint_move_is_advisory_and_never_a_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        let ledger = dir.path().join("ledger.jsonl");
        let with_memory = |elapsed: f64, peak: f64| {
            let mut row = row(elapsed);
            row["memory"] = json!({"peak_private_bytes":peak,"retained_private_bytes":peak/2.0});
            row
        };
        let rows: Vec<Value> = (0..5)
            .map(|offset| with_memory(10.0 + f64::from(offset) / 10.0, 4.0e8))
            .collect();
        save_baseline(&rows, &baseline, "h", "sha").unwrap();
        // Twice the memory at the same speed: reported, not a regression.
        let heavier = compare(&[with_memory(10.2, 8.0e8)], &baseline, &ledger).unwrap();
        assert_eq!(heavier["verdict"], "ok");
        assert_eq!(heavier["flags"], json!(["memory-advisory"]));
        let statuses: Vec<&str> = heavier["deltas"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["metric"].as_str().unwrap().starts_with("memory."))
            .map(|d| d["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["advisory-regression", "advisory-regression"]);
        // Less memory does not make a slower run an improvement either.
        let slower = compare(&[with_memory(12.0, 1.0e8)], &baseline, &ledger).unwrap();
        assert_eq!(slower["verdict"], "candidate-regression");
        assert!(is_advisory("memory.peak_private_bytes"));
        assert!(!is_advisory("elapsed_s"));
    }
    #[test]
    fn a_baseline_without_a_profile_or_lane_asks_for_rebaseline() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = dir.path().join("baseline.json");
        let ledger = dir.path().join("ledger.jsonl");
        crate::case::write_json(
            &baseline,
            &json!({"case_hash":"h","operations":{"download":{"samples":2,"metrics":{"elapsed_s":10.0}}}}),
        )
        .unwrap();
        let legacy = compare(&[row(10.0)], &baseline, &ledger).unwrap();
        assert_eq!(legacy["verdict"], "rebaseline-required");
        assert_eq!(legacy["flags"], json!(["baseline-format-missing"]));

        save_baseline(&five(10.0), &baseline, "h", "sha").unwrap();
        let mut evicted = row(58.0);
        evicted["cache_state"] = "evicted".into();
        let lane = compare(&[evicted], &baseline, &ledger).unwrap();
        assert_eq!(lane["verdict"], "rebaseline-required");
        assert_eq!(
            lane["flags"],
            json!(["baseline-missing-op:download@evicted"])
        );
    }
}
