use crate::{
    collect::expect::dotted,
    ledger::{DEFINITIONS, median, round},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn sweep(rows: &[Value], op: Option<&str>, cache: &str) -> Result<Value> {
    let mut groups: BTreeMap<(String, u64), Vec<&Value>> = BTreeMap::new();
    for row in rows.iter().filter(|r| {
        r["verdict"] != "invalid"
            && (cache == "all" || r["cache_state"] == cache)
            && op.is_none_or(|op| r["op"] == op)
    }) {
        groups
            .entry((
                row["database_mode"].as_str().unwrap_or("").into(),
                row["db_write_gate"].as_u64().unwrap_or(0),
            ))
            .or_default()
            .push(row);
    }
    ensure!(!groups.is_empty(), "No usable rows in ledger");
    let mut report = Vec::new();
    for ((mode, gate), rows) in groups {
        let field = |path: &str| {
            rows.iter()
                .filter_map(|row| dotted(row, path).as_f64())
                .collect::<Vec<_>>()
        };
        let elapsed = field("elapsed_s");
        let mut row = json!({"variant":format!("{mode}/gate-{gate}"),"mode":mode,"gate":gate,"n":elapsed.len(),"elapsed_s":round(median(&elapsed).unwrap_or(0.0),3),"elapsed_min":elapsed.iter().copied().min_by(f64::total_cmp).map(|n|round(n,3)),"elapsed_max":elapsed.iter().copied().max_by(f64::total_cmp).map(|n|round(n,3))});
        for (name, path, places) in [
            ("total_ms", "summary.total_ms", Some(0)),
            ("db_write_ms", "database.write_time_ms", Some(1)),
            ("permit_wait_ms", "database.permit_wait_ms", Some(1)),
            ("retries", "database.write_retries", None),
            ("lock_retries", "database.retries", None),
            ("failures", "database.write_failures", None),
            ("files", "summary.files_updated", None),
        ] {
            let value = median(&field(path));
            row[name] = match places {
                Some(p) => round(value.unwrap_or(0.0), p).into(),
                None => value.map_or(Value::Null, Value::from),
            };
        }
        report.push(row);
    }
    Ok(report.into())
}

pub fn table(report: &Value) -> String {
    let mut text = String::from(
        "variant\tn\telapsed_s\tmin\tmax\ttotal_ms\tdb_write_ms\tpermit_wait_ms\tretries\tfailures\tfiles\n",
    );
    for row in report.as_array().into_iter().flatten() {
        text.push_str(
            &[
                "variant",
                "n",
                "elapsed_s",
                "elapsed_min",
                "elapsed_max",
                "total_ms",
                "db_write_ms",
                "permit_wait_ms",
                "retries",
                "failures",
                "files",
            ]
            .iter()
            .map(|key| {
                row[key]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| row[key].to_string())
            })
            .collect::<Vec<_>>()
            .join("\t"),
        );
        text.push('\n');
    }
    text
}

fn variant(row: &Value) -> String {
    format!(
        "{}/gate-{}",
        row["database_mode"].as_str().unwrap_or(""),
        row["db_write_gate"].as_u64().unwrap_or(0)
    )
}

pub fn compare(
    rows: &[Value],
    case_id: &str,
    baseline: &str,
    candidate: &str,
    cache: &str,
) -> Result<Value> {
    ensure!(
        baseline != candidate,
        "The baseline and candidate variants are identical"
    );
    let rows: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row["verdict"] != "invalid" && (cache == "all" || row["cache_state"] == cache)
        })
        .collect();
    ensure!(
        !rows.is_empty(),
        "No usable rows for case {case_id} at cache state {cache}"
    );
    let rows: Vec<&Value> = rows
        .into_iter()
        .filter(|row| {
            let name = variant(row);
            name == baseline || name == candidate
        })
        .collect();
    for side in [baseline, candidate] {
        ensure!(
            rows.iter().any(|row| variant(row) == side),
            "No rows for {side} in the {case_id} ledger"
        );
    }
    let unique = |extract: &dyn Fn(&Value) -> String| {
        let mut values: Vec<String> = rows.iter().map(|row| extract(row)).collect();
        values.sort();
        values.dedup();
        values
    };
    let hashes = unique(&|row| row["case_hash"].as_str().unwrap_or("").to_owned());
    let profiles = unique(&|row| {
        format!(
            "{}/{}/{}",
            row["harness"].as_str().unwrap_or(""),
            row["build_kind"].as_str().unwrap_or(""),
            row["storage_class"].as_str().unwrap_or("")
        )
    });
    let builds = unique(&|row| {
        format!(
            "{}{}",
            row["git_sha"].as_str().unwrap_or(""),
            if row["git_dirty"] == true {
                "-dirty"
            } else {
                ""
            }
        )
    });
    let mut operations: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for row in &rows {
        operations
            .entry(row["op"].as_str().unwrap_or(""))
            .or_default()
            .push(row);
    }
    let mut comparisons = Vec::new();
    for (op, rows) in operations {
        for &(path, higher, tolerance) in DEFINITIONS {
            let sample = |name: &str| {
                let values: Vec<f64> = rows
                    .iter()
                    .filter(|row| variant(row) == name)
                    .filter_map(|row| dotted(row, path).as_f64())
                    .collect();
                median(&values).map(|median| (median, values.len()))
            };
            let (Some((was, base_samples)), Some((now, candidate_samples))) =
                (sample(baseline), sample(candidate))
            else {
                continue;
            };
            if was == 0.0 {
                continue;
            }
            let change = (now - was) / was.abs();
            let better = if higher { change > 0.0 } else { change < 0.0 };
            comparisons.push(json!({"op":op,"metric":path,"direction":if higher{"higher"}else{"lower"},
                "baseline_mode":baseline,"baseline_median":round(was,4),"baseline_samples":base_samples,
                "candidate_mode":candidate,"candidate_median":round(now,4),"candidate_samples":candidate_samples,
                "change_percent":round(100.0*change,2),
                "verdict":if change.abs()<=tolerance {"no-difference"} else if better {"candidate-better"} else {"candidate-worse"}}));
        }
    }
    let comparable = hashes.len() == 1 && profiles.len() == 1;
    let mut warnings = Vec::new();
    if !comparable {
        warnings.push("Rows differ in case hash or run profile; the workloads are not the same and these numbers are not a fair comparison".to_owned());
    }
    if builds.len() > 1 {
        warnings.push(format!("Rows come from more than one build ({}); confirm the difference between them cannot affect this metric",builds.join(", ")));
    }
    Ok(
        json!({"case_id":case_id,"cache_state":cache,"baseline":baseline,"candidate":candidate,"case_hashes":hashes,"run_profiles":profiles,"builds":builds,"comparable":comparable,"variants":unique(&variant),"warnings":warnings,"comparisons":comparisons}),
    )
}

pub fn compare_table(report: &Value) -> String {
    let mut text = format!(
        "case={} cache={} baseline={} candidate={}\n\nop\tmetric\tdirection\tbaseline_median\tcandidate_median\tchange_percent\tverdict\n",
        report["case_id"].as_str().unwrap_or(""),
        report["cache_state"].as_str().unwrap_or(""),
        report["baseline"].as_str().unwrap_or(""),
        report["candidate"].as_str().unwrap_or("")
    );
    for row in report["comparisons"].as_array().into_iter().flatten() {
        text.push_str(
            &[
                "op",
                "metric",
                "direction",
                "baseline_median",
                "candidate_median",
                "change_percent",
                "verdict",
            ]
            .iter()
            .map(|key| {
                row[*key]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| row[*key].to_string())
            })
            .collect::<Vec<_>>()
            .join("\t"),
        );
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_variants_and_refuses_identical_sides() {
        let row = |mode: &str, gate: u64, elapsed: f64| json!({"database_mode":mode,"db_write_gate":gate,"cache_state":"warm","verdict":"ok","op":"download","case_hash":"h","harness":"cli","build_kind":"release","storage_class":"ssd","git_sha":"a","git_dirty":false,"elapsed_s":elapsed});
        let rows = vec![row("wal", 1, 10.0), row("mvcc", 1, 5.0)];
        let report = compare(&rows, "c", "wal/gate-1", "mvcc/gate-1", "warm").unwrap();
        assert!(report["comparable"] == true && report["warnings"].as_array().unwrap().is_empty());
        let elapsed = report["comparisons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["metric"] == "elapsed_s")
            .unwrap();
        assert_eq!(
            (
                elapsed["change_percent"].as_f64().unwrap(),
                elapsed["verdict"].as_str().unwrap()
            ),
            (-50.0, "candidate-better")
        );
        assert!(compare(&rows, "c", "wal/gate-1", "wal/gate-1", "warm").is_err());
        assert!(compare(&rows, "c", "wal/gate-1", "mvcc/gate-4", "warm").is_err());
    }
    #[test]
    fn isolates_variants() {
        let rows = vec![
            json!({"database_mode":"wal","db_write_gate":1,"cache_state":"warm","verdict":"ok","elapsed_s":2}),
            json!({"database_mode":"wal","db_write_gate":2,"cache_state":"warm","verdict":"ok","elapsed_s":4}),
        ];
        let report = sweep(&rows, None, "warm").unwrap();
        assert_eq!(report.as_array().unwrap().len(), 2);
        assert!(sweep(&[], None, "warm").is_err());
    }
}
