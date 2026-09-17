//! Render the curated measurement table from structured records, so the
//! current block of `conventions/SPEED_OF_LIGHT_MEASUREMENTS.md` is generated
//! from accepted baselines and the latest valid ledger rows rather than typed
//! by hand.

use crate::{
    collect::expect::dotted,
    ledger::{self, ENVIRONMENT_KEYS, median, op_key, round},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// One line of the table: the latest valid lane of one case operation, with
/// the accepted baseline it compares against when one exists.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasurementRow {
    pub date: String,
    pub case_id: String,
    pub op: String,
    pub cache_state: String,
    pub storage_class: String,
    pub build: String,
    pub samples: usize,
    pub elapsed_s: Option<f64>,
    pub sol: Option<f64>,
    pub sol_kind: String,
    pub work: String,
    pub outcome: String,
    pub baseline: String,
    pub run_id: String,
}

fn text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_default()
}

fn metric_kind(row: &Value) -> String {
    let record = &row["download"];
    if record.is_null() {
        return "n/a".to_owned();
    }
    match record["metric_kind"].as_str() {
        Some(kind) => kind.replace('_', " "),
        None => match record["light_src"].as_str() {
            Some("limiter_cap") => "modeled bound".to_owned(),
            Some("peak_1s") => "peak consistency".to_owned(),
            _ => "reference missing".to_owned(),
        },
    }
}

/// The ratio for the operation's primary work. An unrated primary operation
/// stays `n/a`; a rated child with narrower scope must not become the action's
/// headline merely because it has a number.
fn sol_pick(rows: &[&Value]) -> (Option<f64>, String) {
    let median_of = |path: &str| {
        median(
            &rows
                .iter()
                .filter_map(|row| dotted(row, path).as_f64())
                .collect::<Vec<_>>(),
        )
        .map(|v| round(v, 3))
    };
    if rows.iter().any(|row| !row["download"].is_null()) {
        return median_of("download.sol_calibrated")
            .map(|value| (Some(value), "calibrated, network B1".to_owned()))
            .unwrap_or_else(|| (median_of("download.sol"), metric_kind(rows[0])));
    }
    if rows.iter().any(|row| !row["startup"].is_null()) {
        return (
            median_of("startup_probe.sol_calibrated"),
            "calibrated, probe B4".to_owned(),
        );
    }
    if rows.iter().any(|row| {
        dotted(row, "breakdown.run_metrics.hash_work_bytes")
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    }) {
        return (
            median_of("hash.sol_calibrated"),
            "calibrated, hash B2+B6".to_owned(),
        );
    }
    for (path, kind) in [
        ("sync_action.sol_calibrated", "calibrated, no-change B4"),
        ("quick_scan.sol_calibrated", "calibrated, metadata B5"),
    ] {
        if let Some(value) = median_of(path) {
            return (Some(value), kind.to_owned());
        }
    }
    (None, "reference missing".to_owned())
}

fn work_summary(row: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(files) = row["summary"]["files_updated"].as_u64().filter(|n| *n > 0) {
        parts.push(format!("{files} files"));
    }
    if let Some(bytes) = row["summary"]["downloaded_bytes"]
        .as_f64()
        .filter(|n| *n > 0.0)
    {
        parts.push(format!("{:.2} GB", bytes / 1e9));
    }
    if let Some(bytes) = row["breakdown"]["run_metrics"]["hash_work_bytes"]
        .as_f64()
        .filter(|n| *n > 0.0)
    {
        parts.push(format!("{:.2} GB hashed", bytes / 1e9));
    }
    if let Some(repos) = row["startup"]["repos"].as_u64() {
        parts.push(format!("{repos} repos"));
    }
    if parts.is_empty() {
        "n/a".to_owned()
    } else {
        parts.join(", ")
    }
}

/// The rows of one case: the latest run's valid steady-state rows per
/// operation key, reduced to medians, plus the baseline verdict.
pub fn rows_for_case(
    case_id: &str,
    rows: &[Value],
    baseline: Option<&Value>,
) -> Vec<MeasurementRow> {
    let Some(last_run) = rows
        .iter()
        .rev()
        .find(|row| row["verdict"] != "invalid")
        .map(|row| row["run_id"].clone())
    else {
        return Vec::new();
    };
    let mut groups: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|row| row["run_id"] == last_run && row["verdict"] != "invalid")
    {
        // Cold iteration-zero rows are their own lane here as evicted rows
        // are in the ledger: a table row never mixes cache states.
        let key = if row["cache_state"] == "cold" {
            format!("{}@cold", op_key(row))
        } else {
            op_key(row)
        };
        groups.entry(key).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|(op, rows)| {
            let first = rows[0];
            let values = |path: &str| -> Vec<f64> {
                rows.iter()
                    .filter_map(|row| dotted(row, path).as_f64())
                    .collect()
            };
            let baseline = match baseline {
                None => "none".to_owned(),
                Some(baseline) => {
                    let entry = &baseline["operations"][&op];
                    if entry.is_null() {
                        "no lane".to_owned()
                    } else if baseline["profile"].is_null() {
                        "legacy (rebaseline)".to_owned()
                    } else if ENVIRONMENT_KEYS.iter().any(|key| {
                        !baseline["profile"]["environment"].is_null()
                            && baseline["profile"]["environment"][*key]
                                != first["environment"][*key]
                    }) {
                        "expired (environment)".to_owned()
                    } else {
                        format!(
                            "{:.2} s median of {}",
                            entry["metrics"]["elapsed_s"].as_f64().unwrap_or(0.0),
                            entry["samples"].as_u64().unwrap_or(0)
                        )
                    }
                }
            };
            let (sol, sol_kind) = sol_pick(&rows);
            let outcomes: Vec<&str> = rows
                .iter()
                .filter_map(|row| row["download"]["outcome"].as_str())
                .collect();
            MeasurementRow {
                date: text(&first["started_utc"]).chars().take(10).collect(),
                case_id: case_id.to_owned(),
                op,
                cache_state: text(&first["cache_state"]),
                storage_class: text(&first["storage_class"]),
                build: format!(
                    "{}{}",
                    text(&first["git_sha"]).chars().take(7).collect::<String>(),
                    if first["git_dirty"] == true {
                        "-dirty"
                    } else {
                        ""
                    }
                ),
                samples: rows.len(),
                elapsed_s: median(&values("elapsed_s")).map(|v| round(v, 3)),
                sol,
                sol_kind,
                work: work_summary(first),
                outcome: if outcomes.is_empty() {
                    text(&first["verdict"])
                } else {
                    outcomes.join(",")
                },
                baseline,
                run_id: text(&first["run_id"]),
            }
        })
        .collect()
}

/// Markdown table with operation and SoL first, one line per case lane.
pub fn markdown(rows: &[MeasurementRow]) -> String {
    let mut out = String::from(
        "| Date | Case | Operation | SoL (kind) | Lane | Elapsed | Baseline | Samples | Work | Outcome | Build | Run |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for row in rows {
        let sol = match row.sol {
            Some(sol) => format!("{:.0}% ({})", sol * 100.0, row.sol_kind),
            None => "n/a".to_owned(),
        };
        let elapsed = row
            .elapsed_s
            .map_or_else(|| "n/a".to_owned(), |s| format!("{s:.2} s"));
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} {} | {} | {} | {} | {} | {} | {} | `{}` |\n",
            row.date,
            row.case_id,
            row.op,
            sol,
            row.storage_class,
            row.cache_state,
            elapsed,
            row.baseline,
            row.samples,
            row.work,
            row.outcome,
            row.build,
            row.run_id
        ));
    }
    out
}

/// Every case with a ledger under `ledger_dir`, newest run first per case.
pub fn render(ledger_dir: &Path, filter: Option<&str>, as_json: bool) -> Result<String> {
    let mut rows = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(ledger_dir)
        .with_context(|| format!("read {}", ledger_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    entries.sort();
    for path in entries {
        let case_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned();
        if filter.is_some_and(|filter| !case_id.contains(filter)) {
            continue;
        }
        // Ratios are re-derived from the references each row carries, so a
        // row recorded before a derivation rule existed still reads by it.
        let mut ledger_rows = ledger::read(&path)?;
        for row in &mut ledger_rows {
            crate::references::attach(row);
        }
        let baseline = std::fs::read_dir(ledger_dir)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&format!("{case_id}.")) && name.ends_with(".baseline.json")
                    })
            })
            .and_then(|candidate| crate::case::read_json(&candidate).ok());
        rows.extend(rows_for_case(&case_id, &ledger_rows, baseline.as_ref()));
    }
    if as_json {
        let values: Vec<Value> = rows
            .iter()
            .map(|row| {
                json!({"date":row.date,"case_id":row.case_id,"op":row.op,"cache_state":row.cache_state,"storage_class":row.storage_class,"build":row.build,"samples":row.samples,"elapsed_s":row.elapsed_s,"sol":row.sol,"sol_kind":row.sol_kind,"work":row.work,"outcome":row.outcome,"baseline":row.baseline,"run_id":row.run_id})
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&values)?);
    }
    Ok(markdown(&rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(run: &str, op: &str, label: Option<&str>, cache: &str, elapsed: f64) -> Value {
        let mut row = json!({"run_id":run,"iteration":0,"started_utc":"2026-09-16T11:19:54Z","op":op,"cache_state":cache,"verdict":"ok","elapsed_s":elapsed,"git_sha":"6cbfc25abcdef","git_dirty":true,"storage_class":"ssd","environment":{"cpu":"x","os":"Windows 11","memory_gb":96,"origin":"o"},"summary":{"files_updated":40,"downloaded_bytes":424266472},"download":{"sol":0.47,"metric_kind":"peak_consistency","outcome":"completed"},"breakdown":{"run_metrics":{"hash_work_bytes":0}}});
        if let Some(label) = label {
            row["label"] = label.into();
        }
        row
    }

    #[test]
    fn latest_run_lanes_become_rows_with_operation_and_sol_first() {
        let rows = vec![
            row("r1", "download", None, "warm", 16.0),
            row("r2", "download", None, "warm", 12.7),
            row("r2", "download", None, "warm", 11.6),
            row("r2", "quick-check", Some("quick-check-stale"), "warm", 0.4),
            row("r2", "download", None, "evicted", 58.0),
            row("r2", "download", None, "cold", 20.0),
        ];
        let baseline = json!({"profile":{"harness":"gui","environment":{"cpu":"x","os":"Windows 11","memory_gb":96,"origin":"o"}},"operations":{"download":{"samples":2,"metrics":{"elapsed_s":16.5}}}});
        let table = rows_for_case("perf-x", &rows, Some(&baseline));
        let ops: Vec<&str> = table.iter().map(|r| r.op.as_str()).collect();
        assert_eq!(
            ops,
            [
                "download",
                "download@cold",
                "download@evicted",
                "quick-check-stale"
            ]
        );
        assert_eq!(table[0].samples, 2);
        assert_eq!(table[0].elapsed_s, Some(12.15));
        assert_eq!(table[0].sol, Some(0.47));
        assert_eq!(table[0].sol_kind, "peak consistency");
        assert_eq!(table[0].baseline, "16.50 s median of 2");
        assert_eq!(table[1].baseline, "no lane");
        assert_eq!(table[0].work, "40 files, 0.42 GB");
        assert_eq!(table[0].build, "6cbfc25-dirty");
        let md = markdown(&table);
        assert!(md.starts_with("| Date | Case | Operation | SoL (kind) |"));
        assert!(md.contains("| download | 47% (peak consistency) | ssd warm | 12.15 s |"));
    }

    #[test]
    fn expired_and_legacy_baselines_are_named() {
        let rows = vec![row("r1", "download", None, "warm", 1.0)];
        let legacy = json!({"operations":{"download":{"samples":2,"metrics":{"elapsed_s":1.0}}}});
        assert_eq!(
            rows_for_case("c", &rows, Some(&legacy))[0].baseline,
            "legacy (rebaseline)"
        );
        let moved = json!({"profile":{"environment":{"cpu":"other","os":"Windows 11","memory_gb":96,"origin":"o"}},"operations":{"download":{"samples":2,"metrics":{"elapsed_s":1.0}}}});
        assert_eq!(
            rows_for_case("c", &rows, Some(&moved))[0].baseline,
            "expired (environment)"
        );
        assert_eq!(rows_for_case("c", &rows, None)[0].baseline, "none");
        assert!(rows_for_case("c", &[], None).is_empty());
    }

    #[test]
    fn unrated_hash_work_does_not_fall_back_to_a_narrow_quick_scan_ratio() {
        let mut candidate = row("r1", "remote-refresh", None, "cold", 1.0);
        candidate["download"] = Value::Null;
        candidate["breakdown"]["run_metrics"]["hash_work_bytes"] = 4_000_000_000u64.into();
        candidate["quick_scan"] = json!({"sol_calibrated":0.03});
        let table = rows_for_case("c", &[candidate], None);
        assert_eq!(table[0].sol, None);
        assert_eq!(table[0].sol_kind, "calibrated, hash B2+B6");
    }
}
