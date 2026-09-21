use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

/// Accept a baseline from the rows a finished run already wrote to its
/// `summary.json`, so a run made without `--accept` (or one whose acceptance
/// was refused by a kit bug) does not have to be repeated. The run itself must
/// have been made on a clean worktree; the current tree's state is irrelevant
/// because the measured binary is the one the rows record.
pub fn execute(repo_root: &Path, run_dir: &Path) -> Result<Value> {
    let summary = crate::case::read_json(&run_dir.join("summary.json"))
        .with_context(|| format!("read run summary in {}", run_dir.display()))?;
    let rows = summary["rows"]
        .as_array()
        .context("run summary has no rows")?;
    let first = rows.first().context("run summary has no rows")?;
    ensure!(
        rows.iter().all(|row| row["git_dirty"] == false),
        "Baseline acceptance requires rows from a clean Git worktree"
    );
    let case_id = first["case_id"].as_str().context("row has no case_id")?;
    let case_hash = first["case_hash"]
        .as_str()
        .context("row has no case_hash")?;
    let git_sha = first["git_sha"].as_str().context("row has no git_sha")?;
    ensure!(
        rows.iter()
            .all(|row| row["case_hash"] == case_hash && row["git_sha"] == git_sha),
        "Run rows disagree on case hash or Git SHA"
    );
    let baseline = repo_root
        .join("testkit/ledger")
        .join(baseline_file_name(first)?);
    crate::ledger::save_baseline(rows, &baseline, case_hash, git_sha)?;
    Ok(json!({
        "status": "accepted",
        "case_id": case_id,
        "run_id": first["run_id"],
        "git_sha": git_sha,
        "baseline": baseline,
    }))
}

fn baseline_file_name(row: &Value) -> Result<String> {
    let field = |key: &str| {
        row[key]
            .as_str()
            .map(str::to_owned)
            .or_else(|| row[key].as_u64().map(|value| value.to_string()))
            .with_context(|| format!("row has no {key}"))
    };
    Ok(format!(
        "{}.{}.{}.{}.gate-{}.baseline.json",
        field("case_id")?,
        field("harness")?,
        field("build_kind")?,
        field("database_mode")?,
        field("db_write_gate")?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(iteration: u64, cache_state: &str, dirty: bool) -> Value {
        json!({"run_id":"r1","iteration":iteration,"case_id":"perf-x","case_hash":"h","git_sha":"abc1234","git_dirty":dirty,"op":"download","cache_state":cache_state,"verdict":"ok","flags":[],"elapsed_s":10.0 + iteration as f64,"harness":"gui","build_kind":"release","database_mode":"wal","db_write_gate":4,"db_pool_idle":1,"storage_class":"hdd","diagnostics":"none","mutation_profile":null,"mutation_seed":null,"origin_checksum":"origin","references":{}})
    }

    fn write_summary(root: &Path, rows: Vec<Value>) -> std::path::PathBuf {
        let run_dir = root.join("testkit/runs/perf-x/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::create_dir_all(root.join("testkit/ledger")).unwrap();
        crate::case::write_json(&run_dir.join("summary.json"), &json!({"rows": rows})).unwrap();
        run_dir
    }

    #[test]
    fn accepts_a_clean_completed_run_into_the_profiled_baseline_file() {
        let root = tempfile::tempdir().unwrap();
        let mut rows = vec![row(0, "cold", false)];
        rows.extend((1..=5).map(|iteration| row(iteration, "evicted", false)));
        let run_dir = write_summary(root.path(), rows);

        let result = execute(root.path(), &run_dir).unwrap();

        assert_eq!(result["status"], "accepted");
        let baseline = root
            .path()
            .join("testkit/ledger/perf-x.gui.release.wal.gate-4.baseline.json");
        let saved = crate::case::read_json(&baseline).unwrap();
        assert_eq!(saved["git_sha"], "abc1234");
        assert_eq!(saved["operations"]["download@evicted"]["samples"], 5);
    }

    #[test]
    fn refuses_rows_from_a_dirty_worktree() {
        let root = tempfile::tempdir().unwrap();
        let mut rows: Vec<Value> = (1..=5)
            .map(|iteration| row(iteration, "warm", false))
            .collect();
        rows[2]["git_dirty"] = true.into();
        let run_dir = write_summary(root.path(), rows);

        assert!(execute(root.path(), &run_dir).is_err());
    }
}
