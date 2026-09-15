//! Benchmarks live as one folder per record under the active game space:
//! `benchmarks/<id>/benchmark.json` plus the log slice. The files are the
//! source of truth; the database only indexes them, so a database wipe never
//! loses a benchmark unless the user asks for that explicitly.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::warn;

use super::record::BenchmarkRecord;
use crate::core::utils::fs_safety::atomic_write;

pub const RECORD_FILE: &str = "benchmark.json";
pub const LOG_FILE: &str = "benchmark.log";
const FOLDER_NAME: &str = "benchmarks";

pub fn benchmarks_dir() -> PathBuf {
    crate::core::game::spaces::active_game_space_dir().join(FOLDER_NAME)
}

pub fn benchmark_dir(id: &str) -> PathBuf {
    benchmarks_dir().join(id)
}

/// `bm-<local timestamp>-<kind>`; the timestamp has second resolution and two
/// actions cannot finish in the same second, so it is unique per space.
pub fn new_benchmark_id(kind_slug: &str, now: chrono::DateTime<chrono::Local>) -> String {
    format!("bm-{}-{}", now.format("%Y%m%d-%H%M%S"), kind_slug)
}

pub fn save_record(record: &BenchmarkRecord, log_text: Option<&str>) -> Result<PathBuf> {
    let dir = benchmark_dir(&record.id);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    if let Some(text) = log_text {
        atomic_write(&dir.join(LOG_FILE), text.as_bytes()).context("write benchmark log")?;
    }
    write_record(record)?;
    Ok(dir)
}

/// Rewrite only the record file (flags, name, notes); the log is untouched.
pub fn write_record(record: &BenchmarkRecord) -> Result<()> {
    let dir = benchmark_dir(&record.id);
    let json = serde_json::to_vec_pretty(record).context("serialize benchmark")?;
    atomic_write(&dir.join(RECORD_FILE), &json).context("write benchmark record")
}

pub fn load_record(dir: &Path) -> Result<BenchmarkRecord> {
    let bytes = fs::read(dir.join(RECORD_FILE))
        .with_context(|| format!("read {}", dir.join(RECORD_FILE).display()))?;
    serde_json::from_slice(&bytes).context("parse benchmark record")
}

/// Every readable record in the space, newest first. Unreadable folders are
/// logged and skipped so one corrupt file cannot hide the rest.
pub fn load_all_records() -> Vec<BenchmarkRecord> {
    let root = benchmarks_dir();
    let mut records: Vec<BenchmarkRecord> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|dir| match load_record(&dir) {
            Ok(record) => Some(record),
            Err(err) => {
                warn!(
                    "Skipping unreadable benchmark folder {}: {err:#}",
                    dir.file_name().unwrap_or_default().to_string_lossy()
                );
                None
            }
        })
        .collect();
    records.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    records
}

pub fn read_log(record: &BenchmarkRecord) -> Option<String> {
    let name = record.log_file.as_deref()?;
    fs::read_to_string(benchmark_dir(&record.id).join(name)).ok()
}

pub fn delete_record(id: &str) -> Result<()> {
    let dir = benchmark_dir(id);
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    Ok(())
}

/// Remove every benchmark of the active space (the database wipe opt-in).
pub fn delete_all() -> Result<usize> {
    let root = benchmarks_dir();
    if !root.exists() {
        return Ok(0);
    }
    let count = fs::read_dir(&root)
        .map(|entries| entries.filter_map(|entry| entry.ok()).count())
        .unwrap_or(0);
    fs::remove_dir_all(&root).with_context(|| format!("remove {}", root.display()))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_carry_timestamp_and_kind() {
        let at = chrono::Local
            .with_ymd_and_hms(2026, 9, 15, 11, 24, 23)
            .single()
            .expect("local time");
        assert_eq!(new_benchmark_id("update", at), "bm-20260915-112423-update");
    }

    use chrono::TimeZone;
}
