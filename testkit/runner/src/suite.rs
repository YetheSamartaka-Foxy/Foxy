use crate::{case, ledger::round, run};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

pub struct SuiteOptions {
    pub filter: String,
    pub tag: Option<String>,
    pub database_mode: String,
    pub db_write_gate: u32,
    pub no_build: bool,
    pub validate_only: bool,
}

/// A filter is one or more comma-separated glob patterns; a case matches
/// when any pattern does, so a fixed lane list such as the flagship trio
/// (`perf-redownload-small-ssd,perf-startup-arma3-live`)
/// runs as one suite without tagging the case files (tags are part of the
/// case hash and would start a new history).
fn matches(name: &str, pattern: &str) -> bool {
    if pattern.contains(',') {
        return pattern
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .any(|part| matches(name, part));
    }
    let segments: Vec<&str> = pattern.split('*').collect();
    let (Some(first), Some(last)) = (segments.first(), segments.last()) else {
        return false;
    };
    if segments.len() == 1 {
        return name == pattern;
    }
    let Some(mut remaining) = name.strip_prefix(first) else {
        return false;
    };
    for segment in &segments[1..segments.len() - 1] {
        let Some(index) = remaining.find(segment) else {
            return false;
        };
        remaining = &remaining[index + segment.len()..];
    }
    remaining.ends_with(last)
}

fn selected(root: &Path, options: &SuiteOptions) -> Result<Vec<std::path::PathBuf>> {
    let directory = root.join("testkit/cases");
    let mut cases: Vec<_> = std::fs::read_dir(&directory)
        .with_context(|| format!("Reading {}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "json")
        })
        .filter(|path| {
            matches(
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_ref(),
                &options.filter,
            )
        })
        .collect();
    cases.sort();
    if let Some(tag) = &options.tag {
        cases.retain(|path| {
            case::read_json(path).is_ok_and(|document| {
                document["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|value| value == tag))
            })
        });
    }
    ensure!(
        !cases.is_empty(),
        "No cases matched filter '{}'",
        options.filter
    );
    Ok(cases)
}

pub fn execute(root: &Path, options: &SuiteOptions) -> Result<Value> {
    let mut results = Vec::new();
    let mut failed = 0;
    for path in selected(root, options)? {
        let started = Instant::now();
        let outcome = run::execute(
            root,
            &run::RunOptions {
                case: path.clone(),
                database_mode: options.database_mode.clone(),
                db_write_gate: options.db_write_gate,
                accept: false,
                validate_only: options.validate_only,
                no_build: options.no_build,
            },
        );
        let (status, detail) = match &outcome {
            Ok(value) => (
                value["status"].as_str().unwrap_or("pass").to_owned(),
                value["run_dir"].as_str().unwrap_or_default().to_owned(),
            ),
            Err(error) => {
                failed += 1;
                ("fail".to_owned(), format!("{error:#}"))
            }
        };
        let seconds = round(started.elapsed().as_secs_f64(), 1);
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        eprintln!("{name:<32} {:<5} {seconds:>8}s", status.to_uppercase());
        results.push(json!({"case": name, "status": status, "seconds": seconds, "detail": detail}));
    }
    Ok(json!({"cases": results.len(), "failed": failed, "results": results}))
}

pub fn table(report: &Value) -> String {
    let mut text = String::new();
    for result in report["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|result| result["status"] == "fail")
    {
        text.push_str(&format!(
            "\nFAILED {}: {}\n",
            result["case"].as_str().unwrap_or_default(),
            result["detail"].as_str().unwrap_or_default()
        ));
    }
    text.push_str(&format!(
        "\n{} case(s), {} failed\n",
        report["cases"], report["failed"]
    ));
    text
}

pub fn sweep(
    root: &Path,
    case_path: &Path,
    modes: &[String],
    gates: &[u32],
    fresh: bool,
) -> Result<Value> {
    ensure!(
        !modes.is_empty() && !gates.is_empty(),
        "A sweep needs at least one mode and one gate"
    );
    let id = case::read_json(case_path)?["id"]
        .as_str()
        .context("Case needs an id")?
        .to_owned();
    let ledger = root.join("testkit/ledger").join(format!("{id}.jsonl"));
    if fresh && ledger.exists() {
        let archive = root.join("testkit/ledger/archive");
        std::fs::create_dir_all(&archive)?;
        std::fs::rename(
            &ledger,
            archive.join(format!(
                "{id}.{}.jsonl",
                chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
            )),
        )?;
    }
    let mut results = Vec::new();
    for mode in modes {
        for gate in gates {
            let started = Instant::now();
            let outcome = run::execute(
                root,
                &run::RunOptions {
                    case: case_path.to_path_buf(),
                    database_mode: mode.clone(),
                    db_write_gate: *gate,
                    accept: false,
                    validate_only: false,
                    no_build: true,
                },
            );
            let status = match &outcome {
                Ok(value) => value["status"].as_str().unwrap_or("pass").to_owned(),
                Err(error) => format!("FAILED: {error:#}"),
            };
            let seconds = round(started.elapsed().as_secs_f64(), 1);
            eprintln!("{mode:<5} gate={gate:<2} {seconds:>7}s  {status}");
            results.push(json!({"mode": mode, "gate": gate, "seconds": seconds, "status": status}));
        }
    }
    Ok(json!({"case_id": id, "ledger": ledger, "results": results}))
}

pub fn sweep_table(report: &Value) -> String {
    format!(
        "sweep complete: {}\n",
        report["ledger"].as_str().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_globs_match_the_powershell_shape() {
        assert!(matches("ux-modals", "*"));
        assert!(matches("ux-modals", "ux-*"));
        assert!(!matches("perf-db", "ux-*"));
        assert!(matches("perf-db-refresh-main", "perf-*-main"));
        assert!(matches("ux-modals", "ux-modals"));
        assert!(!matches("ux-modals", "ux-modal"));
    }
    #[test]
    fn comma_separated_filters_match_any_pattern() {
        let flagship = "perf-redownload-small-ssd, perf-tfr-scifi-recheck-hdd,perf-startup-*";
        assert!(matches("perf-redownload-small-ssd", flagship));
        assert!(matches("perf-startup-arma3-live", flagship));
        assert!(!matches("perf-redownload-small-hdd", flagship));
        assert!(!matches("anything", ","));
    }
}
