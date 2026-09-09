use crate::{case, collect::sol, ledger};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

fn differences(expected: &Value, actual: &Value, path: &str, out: &mut Vec<Value>) {
    if matches!(path, "case_hash" | "started_utc") {
        return;
    }
    if let (Some(a), Some(b)) = (expected.as_object(), actual.as_object()) {
        let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
        for key in keys {
            differences(
                a.get(key).unwrap_or(&Value::Null),
                b.get(key).unwrap_or(&Value::Null),
                &if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                },
                out,
            );
        }
    } else if let (Some(a), Some(b)) = (expected.as_array(), actual.as_array()) {
        if a.len() != b.len() {
            out.push(json!({"path":path,"expected":expected,"actual":actual}));
        } else {
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                differences(a, b, &format!("{path}.{index}"), out);
            }
        }
    } else if expected != actual
        && !matches!((expected.as_f64(),actual.as_f64()),(Some(a),Some(b)) if a==b)
    {
        out.push(json!({"path":path,"expected":expected,"actual":actual}));
    }
}

pub fn run(dir: &Path) -> Result<Value> {
    let recorded = case::read_json(&dir.join("summary.json"))?;
    let Some(rows) = recorded["rows"].as_array() else {
        return Ok(
            json!({"run_dir":dir,"kind":"ux","rows":[],"differences":[],"limitations":["UX transcript is retained; replay cannot repeat GUI interactions"]}),
        );
    };
    let resolved = case::read_json(&dir.join("resolved-case.json"))?;
    let hash = case::hash(&resolved)?;
    let mut rebuilt = Vec::new();
    let mut diffs = Vec::new();
    let mut limitations=vec!["Provenance, elapsed wall time, mutation counters, runtime guard flags and historical comparison verdicts are retained from recorded rows".to_owned()];
    for original in rows {
        let stem = format!(
            "{}-{}",
            original["iteration"].as_u64().unwrap_or(0),
            original["op"].as_str().unwrap_or("")
        );
        let summary = case::read_json(&dir.join(format!("summary-{stem}.json")))?;
        let mut breakdown = case::read_json(&dir.join(format!("breakdown-{stem}.json")))?;
        let log_path = dir.join(format!("operation-{stem}.log"));
        let records = if log_path.exists() {
            let log = std::fs::read_to_string(log_path)?;
            breakdown = crate::collect::metrics::breakdown(&log);
            sol::parse(&log)
        } else {
            let mut records = Vec::new();
            for field in ["download", "hash", "quick_scan"] {
                if let Some(raw) = original[field]["raw"].as_str() {
                    records.extend(sol::parse(raw));
                }
            }
            if let Some(lines) = breakdown["db"]["lines"].as_array() {
                breakdown["db"]["values"] = lines
                    .iter()
                    .map(|line| sol::key_values(line.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .into();
            }
            if limitations.len() == 1 {
                limitations.push("Legacy artifacts have no per-operation raw logs or SOL boundaries; breakdown grouping/run_metrics are retained, DB key/value and SOL records are reparsed from retained raw lines".into());
            }
            records
        };
        let mutation = json!({"profile":original["mutation_profile"],"seed":original["mutation_seed"],"mutated_parts":original["mutated_parts"],"mutated_bytes":original["mutated_bytes"]});
        let mut row =
            ledger::build_row(original.clone(), &summary, &records, &breakdown, &mutation);
        row["case_hash"] = hash.clone().into();
        row["verdict"] = original["verdict"].clone();
        let mut row_diffs = Vec::new();
        differences(original, &row, "", &mut row_diffs);
        for difference in row_diffs {
            diffs.push(json!({"artifact":stem,"difference":difference}));
        }
        rebuilt.push(row);
    }
    Ok(
        json!({"run_dir":dir,"rows":rebuilt,"differences":diffs,"equal":diffs.is_empty(),"limitations":limitations}),
    )
}

pub fn corpus(root: &Path, run_dir: Option<&Path>, all: bool) -> Result<Value> {
    let mut directories = Vec::new();
    if let Some(dir) = run_dir {
        directories.push(dir.to_path_buf());
    }
    if all || run_dir.is_none() {
        let runs = root.join("testkit/runs");
        ensure!(
            runs.is_dir(),
            "No recorded runs under {}; pass a run directory",
            runs.display()
        );
        for case in std::fs::read_dir(&runs)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
        {
            directories.extend(
                std::fs::read_dir(&case)?
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.join("summary.json").is_file()),
            );
        }
        directories.sort();
    }
    ensure!(
        !directories.is_empty(),
        "No run directories with a summary.json to replay"
    );
    let mut reports = Vec::new();
    let (mut replayed, mut differing, mut skipped) = (0usize, 0usize, 0usize);
    for directory in directories {
        match run(&directory) {
            Ok(report) => {
                if report["rows"].as_array().is_none_or(Vec::is_empty) {
                    skipped += 1;
                } else {
                    replayed += 1;
                    if report["equal"] != Value::Bool(true) {
                        differing += 1;
                    }
                }
                reports.push(report);
            }
            Err(error) => {
                skipped += 1;
                reports.push(json!({"run_dir":directory,"error":format!("{error:#}"),"differences":[],"equal":Value::Null}));
            }
        }
    }
    Ok(
        json!({"replayed":replayed,"differing":differing,"skipped":skipped,"equal":differing==0,"runs":reports}),
    )
}

pub fn table(report: &Value) -> String {
    let mut text = String::new();
    for entry in report["runs"].as_array().into_iter().flatten() {
        let directory = entry["run_dir"].as_str().unwrap_or_default();
        if let Some(error) = entry["error"].as_str() {
            text.push_str(&format!("SKIP {directory}: {error}\n"));
            continue;
        }
        let differences = entry["differences"].as_array().map_or(0, Vec::len);
        if differences == 0 {
            continue;
        }
        text.push_str(&format!("DIFF {directory} ({differences})\n"));
        for difference in entry["differences"]
            .as_array()
            .into_iter()
            .flatten()
            .take(10)
        {
            text.push_str(&format!(
                "     {} {}: recorded {} rebuilt {}\n",
                difference["artifact"].as_str().unwrap_or_default(),
                difference["difference"]["path"]
                    .as_str()
                    .unwrap_or_default(),
                difference["difference"]["expected"],
                difference["difference"]["actual"]
            ));
        }
    }
    text.push_str(&format!(
        "\n{} run(s) replayed, {} differing, {} skipped\n",
        report["replayed"], report["differing"], report["skipped"]
    ));
    text
}

pub fn migrate_ledger(mapping_path: &Path, ledger_dir: &Path) -> Result<Value> {
    let mapping = case::read_json(mapping_path)?;
    let entries = mapping
        .as_array()
        .or_else(|| mapping["mappings"].as_array())
        .ok_or_else(|| anyhow::anyhow!("Mapping must be an array or contain mappings"))?;
    let mut hashes = std::collections::BTreeMap::new();
    let mut computed = Vec::new();
    for entry in entries {
        let old = entry["legacy_hash"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing legacy_hash"))?;
        // The PowerShell emits the resolved case rather than a hash, so the JCS
        // side of the mapping is derived here by the implementation that will use it.
        let new = match entry["jcs_hash"].as_str() {
            Some(new) => new.to_owned(),
            None => case::hash(
                entry
                    .get("case")
                    .filter(|value| !value.is_null())
                    .ok_or_else(|| anyhow::anyhow!("Mapping entry needs jcs_hash or case"))?,
            )?,
        };
        computed.push((old.to_owned(), new));
    }
    for (old, new) in &computed {
        if let Some(previous) = hashes.insert(old.as_str(), new.as_str()) {
            ensure!(previous == new, "Conflicting mapping for legacy hash {old}");
        }
    }
    let files: Vec<_> = std::fs::read_dir(ledger_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("jsonl" | "json")
                )
                && path != mapping_path
        })
        .collect();
    let archive = ledger_dir.join("archive/pre-rust");
    std::fs::create_dir_all(&archive)?;
    for path in &files {
        let target = archive.join(path.file_name().unwrap());
        if !target.exists() {
            std::fs::copy(path, target)?;
        }
    }
    let (mut changed, mut unchanged) = (0usize, 0usize);
    let mut unmapped = Vec::new();
    for path in files {
        let jsonl = path.extension().is_some_and(|e| e == "jsonl");
        let mut values = if jsonl {
            ledger::read(&path)?
        } else {
            vec![case::read_json(&path)?]
        };
        let mut touched = false;
        for (index, value) in values.iter_mut().enumerate() {
            if let Some(old) = value["case_hash"].as_str() {
                if let Some(new) = hashes.get(old) {
                    value["case_hash"] = (*new).into();
                    changed += 1;
                    touched = true;
                } else if hashes.values().any(|new| *new == old) {
                    unchanged += 1;
                } else {
                    unmapped.push(json!({"file":path.file_name().unwrap_or_default().to_string_lossy(),"row":index+1,"case_hash":old}));
                }
            }
        }
        if touched {
            let staged = path.with_extension("migration-tmp");
            if jsonl {
                let text = values
                    .iter()
                    .map(serde_json::to_string)
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .join("\n")
                    + "\n";
                std::fs::write(&staged, text)?;
            } else {
                case::write_json(&staged, &values[0])?;
            }
            std::fs::rename(&staged, &path)?;
        }
    }
    Ok(
        json!({"changed":changed,"already_migrated":unchanged,"unmapped":unmapped,"archive":archive}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_equality_and_real_difference() {
        let mut diff = Vec::new();
        differences(
            &json!({"n":1,"case_hash":"a"}),
            &json!({"n":1.0,"case_hash":"b"}),
            "",
            &mut diff,
        );
        assert!(diff.is_empty());
        differences(&json!(1), &json!(2), "n", &mut diff);
        assert_eq!(diff.len(), 1);
    }
}
