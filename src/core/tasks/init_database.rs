use log::info;
use once_cell::sync::Lazy;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use crate::core::utils::format::sanitize_log_path;

pub(crate) const SQLITE_MAX_VARIABLES: usize = 999;

const SQLITE_BULK_VARIABLE_LIMIT: usize = 32_766;
const SQLITE_WRITE_PERMITS_CAP: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SqlitePerfSnapshot {
    pub(crate) lock_retries: u64,
    pub(crate) lock_backoff_ms_total: u64,
    pub(crate) db_write_time_ns_total: u64,

    pub(crate) rows_affected: u64,
    pub(crate) insert_rows_affected: u64,
    pub(crate) update_rows_affected: u64,
    pub(crate) delete_rows_affected: u64,
    pub(crate) other_rows_affected: u64,
}

impl SqlitePerfSnapshot {
    pub(crate) fn delta_since(self, baseline: Self) -> Self {
        Self {
            lock_retries: self.lock_retries.saturating_sub(baseline.lock_retries),
            lock_backoff_ms_total: self
                .lock_backoff_ms_total
                .saturating_sub(baseline.lock_backoff_ms_total),
            db_write_time_ns_total: self
                .db_write_time_ns_total
                .saturating_sub(baseline.db_write_time_ns_total),
            rows_affected: self.rows_affected.saturating_sub(baseline.rows_affected),
            insert_rows_affected: self
                .insert_rows_affected
                .saturating_sub(baseline.insert_rows_affected),
            update_rows_affected: self
                .update_rows_affected
                .saturating_sub(baseline.update_rows_affected),
            delete_rows_affected: self
                .delete_rows_affected
                .saturating_sub(baseline.delete_rows_affected),
            other_rows_affected: self
                .other_rows_affected
                .saturating_sub(baseline.other_rows_affected),
        }
    }

    pub(crate) fn avg_backoff_ms(self) -> f64 {
        if self.lock_retries == 0 {
            return 0.0;
        }
        self.lock_backoff_ms_total as f64 / self.lock_retries as f64
    }

    pub(crate) fn db_write_time_ms(self) -> f64 {
        self.db_write_time_ns_total as f64 / 1_000_000.0
    }
}

#[derive(Default)]
struct SqlitePerfCounters {
    lock_retries: AtomicU64,
    lock_backoff_ms_total: AtomicU64,
    db_write_time_ns_total: AtomicU64,
    rows_affected: AtomicU64,
    insert_rows_affected: AtomicU64,
    update_rows_affected: AtomicU64,
    delete_rows_affected: AtomicU64,
    other_rows_affected: AtomicU64,
}

impl SqlitePerfCounters {
    fn snapshot(&self) -> SqlitePerfSnapshot {
        SqlitePerfSnapshot {
            lock_retries: self.lock_retries.load(Ordering::Relaxed),
            lock_backoff_ms_total: self.lock_backoff_ms_total.load(Ordering::Relaxed),
            db_write_time_ns_total: self.db_write_time_ns_total.load(Ordering::Relaxed),
            rows_affected: self.rows_affected.load(Ordering::Relaxed),
            insert_rows_affected: self.insert_rows_affected.load(Ordering::Relaxed),
            update_rows_affected: self.update_rows_affected.load(Ordering::Relaxed),
            delete_rows_affected: self.delete_rows_affected.load(Ordering::Relaxed),
            other_rows_affected: self.other_rows_affected.load(Ordering::Relaxed),
        }
    }

    fn record_lock_retry(&self, backoff: Duration) {
        self.lock_retries.fetch_add(1, Ordering::Relaxed);
        let backoff_ms = backoff.as_millis().min(u128::from(u64::MAX)) as u64;
        self.lock_backoff_ms_total
            .fetch_add(backoff_ms, Ordering::Relaxed);
    }

    fn record_rows_affected(&self, kind: SqliteStatementKind, rows: u64) {
        let counter = match kind {
            SqliteStatementKind::Insert => &self.insert_rows_affected,
            SqliteStatementKind::Update => &self.update_rows_affected,
            SqliteStatementKind::Delete => &self.delete_rows_affected,
            SqliteStatementKind::Other => &self.other_rows_affected,
            SqliteStatementKind::Ignored => return,
        };
        self.rows_affected.fetch_add(rows, Ordering::Relaxed);
        counter.fetch_add(rows, Ordering::Relaxed);
    }

    fn record_db_write_time(&self, elapsed: Duration) {
        let elapsed_ns = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        self.db_write_time_ns_total
            .fetch_add(elapsed_ns, Ordering::Relaxed);
    }
}

static SQLITE_PERF_COUNTERS: Lazy<SqlitePerfCounters> = Lazy::new(SqlitePerfCounters::default);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SqliteStatementKind {
    Insert,
    Update,
    Delete,
    Other,
    Ignored,
}

fn sqlite_statement_kind(sql: &str) -> SqliteStatementKind {
    match crate::core::utils::profiling::statement_verb(sql) {
        "INSERT" | "REPLACE" => SqliteStatementKind::Insert,
        "UPDATE" => SqliteStatementKind::Update,
        "DELETE" => SqliteStatementKind::Delete,
        "BEGIN" | "COMMIT" | "ROLLBACK" | "PRAGMA" | "SELECT" => SqliteStatementKind::Ignored,
        _ => SqliteStatementKind::Other,
    }
}

#[cfg(test)]
mod statement_kind_tests {
    use super::{SqliteStatementKind, sqlite_statement_kind};

    #[test]
    fn classifies_affected_row_statement_kinds() {
        assert_eq!(
            sqlite_statement_kind("  INSERT OR IGNORE INTO t VALUES (1)"),
            SqliteStatementKind::Insert
        );
        assert_eq!(
            sqlite_statement_kind("REPLACE INTO t VALUES (1)"),
            SqliteStatementKind::Insert
        );
        assert_eq!(
            sqlite_statement_kind("\nUPDATE t SET value = 1"),
            SqliteStatementKind::Update
        );
        assert_eq!(
            sqlite_statement_kind("DELETE FROM t"),
            SqliteStatementKind::Delete
        );
        assert_eq!(
            sqlite_statement_kind("CREATE TABLE t (id INTEGER)"),
            SqliteStatementKind::Other
        );
        assert_eq!(
            sqlite_statement_kind("COMMIT"),
            SqliteStatementKind::Ignored
        );
        assert_eq!(
            sqlite_statement_kind(
                "WITH v(id, value) AS (VALUES (1, 2)) UPDATE t SET value = v.value FROM v"
            ),
            SqliteStatementKind::Update
        );
    }
}

pub(crate) fn record_sqlite_rows_affected(sql: &str, rows: u64) {
    SQLITE_PERF_COUNTERS.record_rows_affected(sqlite_statement_kind(sql), rows);
}
static SQLITE_WRITE_METRICS: Lazy<Mutex<HashMap<String, SqliteWriteMetricSnapshot>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SqliteWriteMetricSnapshot {
    pub(crate) calls: u64,
    pub(crate) committed: u64,
    pub(crate) failed: u64,
    pub(crate) lock_retries: u64,
    pub(crate) lock_backoff_ms_total: u64,
    pub(crate) permit_wait_ns_total: u64,
    pub(crate) txn_time_ns_total: u64,
}

impl SqliteWriteMetricSnapshot {
    pub(crate) fn delta_since(self, baseline: Self) -> Self {
        Self {
            calls: self.calls.saturating_sub(baseline.calls),
            committed: self.committed.saturating_sub(baseline.committed),
            failed: self.failed.saturating_sub(baseline.failed),
            lock_retries: self.lock_retries.saturating_sub(baseline.lock_retries),
            lock_backoff_ms_total: self
                .lock_backoff_ms_total
                .saturating_sub(baseline.lock_backoff_ms_total),
            permit_wait_ns_total: self
                .permit_wait_ns_total
                .saturating_sub(baseline.permit_wait_ns_total),
            txn_time_ns_total: self
                .txn_time_ns_total
                .saturating_sub(baseline.txn_time_ns_total),
        }
    }

    pub(crate) fn txn_time_ms(self) -> f64 {
        self.txn_time_ns_total as f64 / 1_000_000.0
    }

    pub(crate) fn permit_wait_ms(self) -> f64 {
        self.permit_wait_ns_total as f64 / 1_000_000.0
    }
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse::<usize>().ok()
}

fn sqlite_write_permits() -> usize {
    env_usize("FOXY_SQLITE_WRITE_PERMITS")
        .unwrap_or_else(default_write_permits)
        .clamp(1, SQLITE_WRITE_PERMITS_CAP)
}

fn default_write_permits() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

pub(crate) static DB_WRITE_PERMITS: Lazy<usize> = Lazy::new(sqlite_write_permits);

pub(crate) static DB_WRITE_SEMAPHORE: Lazy<std::sync::Arc<Semaphore>> =
    Lazy::new(|| std::sync::Arc::new(Semaphore::new(*DB_WRITE_PERMITS)));

/// Default to four committers at most; larger gates increase in-engine queuing.
fn db_write_gate_permits() -> usize {
    env_usize("FOXY_DB_WRITE_GATE")
        .unwrap_or_else(default_db_write_gate)
        .clamp(1, SQLITE_WRITE_PERMITS_CAP)
}

fn default_db_write_gate() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(4)
}

/// Serialize writers at the seam; the separate semaphore bounds metadata fan-out while reads stay ungated.
pub(crate) static DB_WRITE_GATE_PERMITS: Lazy<usize> = Lazy::new(db_write_gate_permits);
pub(crate) static DB_WRITE_GATE: Lazy<std::sync::Arc<Semaphore>> =
    Lazy::new(|| std::sync::Arc::new(Semaphore::new(*DB_WRITE_GATE_PERMITS)));

pub(crate) async fn acquire_db_write_gate() -> (Option<tokio::sync::OwnedSemaphorePermit>, Duration)
{
    let started = Instant::now();
    let permit = DB_WRITE_GATE.clone().acquire_owned().await.ok();
    (permit, started.elapsed())
}

/// Exclusive bulk work must avoid reentrant seam access; transaction closures use `DbTxn`.
pub(crate) static DB_EXCLUSIVE: Lazy<std::sync::Arc<tokio::sync::RwLock<()>>> =
    Lazy::new(|| std::sync::Arc::new(tokio::sync::RwLock::new(())));

/// Shared guard for ordinary reads/writes (see [`DB_EXCLUSIVE`]). Held for the
/// duration of a single seam operation.
pub(crate) async fn acquire_db_shared() -> tokio::sync::OwnedRwLockReadGuard<()> {
    DB_EXCLUSIVE.clone().read_owned().await
}

/// Hold the exclusive barrier for the full purge transaction so no DB access overlaps it.
pub(crate) async fn acquire_db_exclusive() -> tokio::sync::OwnedRwLockWriteGuard<()> {
    DB_EXCLUSIVE.clone().write_owned().await
}

pub(crate) fn sqlite_perf_snapshot() -> SqlitePerfSnapshot {
    SQLITE_PERF_COUNTERS.snapshot()
}

/// Use one retry classifier so transient-error handling cannot drift across write paths.
pub(crate) fn sqlite_is_locked_error(message: &str) -> bool {
    crate::core::tasks::db_turso::db_error_message_is_retryable(message)
}

pub(crate) fn sqlite_lock_backoff(attempt: usize) -> Duration {
    Duration::from_millis(50 * 2u64.saturating_pow(attempt as u32))
}

pub(crate) async fn sqlite_sleep_for_lock_retry(attempt: usize) {
    let backoff = sqlite_lock_backoff(attempt);
    SQLITE_PERF_COUNTERS.record_lock_retry(backoff);
    tokio::time::sleep(backoff).await;
}

pub(crate) struct SqliteWriteScope {
    started_at: Instant,
    label: Option<&'static str>,
    baseline: SqlitePerfSnapshot,
}

impl Drop for SqliteWriteScope {
    fn drop(&mut self) {
        let elapsed = self.started_at.elapsed();
        SQLITE_PERF_COUNTERS.record_db_write_time(elapsed);
        if let Some(label) = self.label {
            record_sqlite_write_metrics(
                label,
                true,
                Duration::ZERO,
                elapsed,
                sqlite_perf_snapshot().delta_since(self.baseline),
            );
        }
    }
}

pub(crate) fn sqlite_labeled_write_scope(label: &'static str) -> SqliteWriteScope {
    SqliteWriteScope {
        started_at: Instant::now(),
        label: Some(label),
        baseline: sqlite_perf_snapshot(),
    }
}

pub(crate) fn record_db_transaction_metrics(
    label: &str,
    committed: bool,
    permit_wait: Duration,
    elapsed: Duration,
    baseline: SqlitePerfSnapshot,
) {
    SQLITE_PERF_COUNTERS.record_db_write_time(elapsed);
    record_sqlite_write_metrics(
        label,
        committed,
        permit_wait,
        elapsed,
        sqlite_perf_snapshot().delta_since(baseline),
    );
}

fn duration_ns(elapsed: Duration) -> u64 {
    elapsed.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn record_sqlite_write_metrics(
    label: &str,
    committed: bool,
    permit_wait: Duration,
    elapsed: Duration,
    retry_delta: SqlitePerfSnapshot,
) {
    let Ok(mut metrics) = SQLITE_WRITE_METRICS.lock() else {
        return;
    };
    let metric = metrics.entry(label.to_owned()).or_default();
    metric.calls += 1;
    if committed {
        metric.committed += 1;
    } else {
        metric.failed += 1;
    }
    metric.lock_retries += retry_delta.lock_retries;
    metric.lock_backoff_ms_total += retry_delta.lock_backoff_ms_total;
    metric.permit_wait_ns_total += duration_ns(permit_wait);
    metric.txn_time_ns_total += duration_ns(elapsed);
}

pub(crate) fn sqlite_write_metrics_snapshot() -> BTreeMap<String, SqliteWriteMetricSnapshot> {
    SQLITE_WRITE_METRICS
        .lock()
        .map(|metrics| {
            metrics
                .iter()
                .map(|(label, metric)| (label.clone(), *metric))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn log_sqlite_write_metrics_since(
    baseline: &BTreeMap<String, SqliteWriteMetricSnapshot>,
    context: &str,
) {
    let current = sqlite_write_metrics_snapshot();
    let mut deltas = current
        .into_iter()
        .filter_map(|(label, metric)| {
            let delta = metric.delta_since(baseline.get(&label).copied().unwrap_or_default());
            (delta.calls > 0).then_some((label, delta))
        })
        .collect::<Vec<_>>();
    deltas.sort_by_key(|entry| std::cmp::Reverse(entry.1.txn_time_ns_total));

    info!(
        "SQLite write category summary: context={} categories={} write_gate={}",
        context,
        deltas.len(),
        *DB_WRITE_GATE_PERMITS
    );
    for (label, metric) in deltas.into_iter().take(12) {
        info!(
            "SQLite write category metrics: context={} label={} calls={} committed={} failed={} retries={} backoff_ms={} permit_wait_ms={:.1} txn_ms={:.1} write_gate={}",
            context,
            label,
            metric.calls,
            metric.committed,
            metric.failed,
            metric.lock_retries,
            metric.lock_backoff_ms_total,
            metric.permit_wait_ms(),
            metric.txn_time_ms(),
            *DB_WRITE_GATE_PERMITS
        );
    }
}

pub(crate) fn sqlite_variable_limit() -> usize {
    env_usize("FOXY_DB_VAR_LIMIT")
        .map(|v| v.clamp(60, SQLITE_BULK_VARIABLE_LIMIT))
        .unwrap_or(SQLITE_BULK_VARIABLE_LIMIT)
}

pub(crate) fn read_chunk_ids() -> usize {
    sqlite_variable_limit().saturating_sub(10).max(1)
}

pub(crate) fn bulk_write_chunk_rows() -> usize {
    env_usize("FOXY_DB_CHUNK_ROWS")
        .unwrap_or(256)
        .clamp(16, SQLITE_BULK_VARIABLE_LIMIT)
}

pub(crate) fn bulk_write_rows_for(params_per_row: usize) -> usize {
    bulk_write_chunk_rows()
        .min(
            (sqlite_variable_limit() / params_per_row.max(1))
                .saturating_sub(1)
                .max(1),
        )
        .max(1)
}

pub(crate) async fn init_database() -> crate::core::db::DbHandle {
    static MAINTAINED_DB_PATHS: tokio::sync::Mutex<Option<std::collections::HashSet<PathBuf>>> =
        tokio::sync::Mutex::const_new(None);

    let (path, db) = crate::core::tasks::db_turso::init_turso_database_with_path().await;
    let mut maintained = MAINTAINED_DB_PATHS.lock().await;
    let maintained = maintained.get_or_insert_with(std::collections::HashSet::new);
    if maintained.insert(path) {
        retire_stale_content_hash_baselines(&db).await;
        let backfill_db = crate::core::db::FoxyDb::from_turso(db.clone());
        crate::core::addon_metadata::backfill_missing_addon_display_names(&backfill_db).await;
    }
    db
}

async fn retire_stale_content_hash_baselines(db: &crate::core::db::DbHandle) {
    use crate::core::tasks::db_schema_version;

    if !db_schema_version::content_hash_baselines_need_retire() {
        return;
    }

    let foxy_db = crate::core::db::FoxyDb::from_turso(db.clone());
    let started = Instant::now();
    let mut total_rows = 0u64;
    for (label, sql) in [
        (
            "retire file content-hash baselines",
            "UPDATE files SET local_content_hash = '' WHERE local_content_hash != ''",
        ),
        (
            "retire addon content-hash baselines",
            "UPDATE addons SET local_content_hash = '' WHERE local_content_hash != ''",
        ),
        (
            "retire repository content-hash baselines",
            "UPDATE repositories SET local_content_hash = '' WHERE local_content_hash != ''",
        ),
    ] {
        match foxy_db.execute_retry(label, sql, vec![]).await {
            Ok(rows) => total_rows += rows,
            Err(err) => {
                log::warn!(
                    "Failed to {} for content-hash format upgrade (will retry next launch): {}",
                    label,
                    err
                );
                return;
            }
        }
    }
    db_schema_version::mark_content_hash_format_current();
    info!(
        "Retired {} stale content-hash baselines for format {} in {:.2?}; repos will re-baseline lazily via RefreshContentBaseline",
        total_rows,
        db_schema_version::CONTENT_HASH_FORMAT,
        started.elapsed()
    );
}

pub fn wipe_database_sync() {
    let base_dir = crate::core::game::spaces::active_game_space_dir();
    let marker_path = base_dir.join(crate::core::tasks::db_turso::WIPE_MARKER_FILE_NAME);

    info!("Marking database for wipe on next startup...");

    match fs::File::create(&marker_path) {
        Ok(_) => info!("Created wipe marker: {}", sanitize_log_path(&marker_path)),
        Err(e) => log::error!("Failed to create wipe marker: {}", e),
    }
}

/// Close the prior space's handle before removal; Windows cannot remove a live database directory.
pub fn close_active_database_sync() -> Result<(), String> {
    let Some(runtime) = crate::core::api::background_runtime() else {
        return Err("No background runtime available to close the database safely".to_string());
    };
    runtime.block_on(crate::core::tasks::db_turso::close_active_database())
}

pub fn resume_database_access_sync(path: &Path) -> Result<(), String> {
    let runtime = crate::core::api::background_runtime()
        .ok_or_else(|| "No background runtime available to resume database access".to_string())?;
    runtime.block_on(crate::core::tasks::db_turso::resume_database_access(path))
}

pub fn check_and_wipe_database() {
    let base_dir = crate::core::game::spaces::active_game_space_dir();
    let marker_path = base_dir.join(crate::core::tasks::db_turso::WIPE_MARKER_FILE_NAME);

    if !marker_path.exists() {
        return;
    }

    info!("Wipe marker found, deleting database files...");

    for name in crate::core::tasks::db_turso::DATABASE_ARTIFACT_FILE_NAMES {
        let path = base_dir.join(name);
        if path.exists() {
            match fs::remove_file(&path) {
                Ok(_) => info!("Deleted: {}", sanitize_log_path(&path)),
                Err(e) => log::error!("Failed to delete {}: {}", sanitize_log_path(&path), e),
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&base_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name
                .to_string_lossy()
                .starts_with(crate::core::tasks::db_turso::DATABASE_REBUILD_BACKUP_PREFIX)
            {
                let path = entry.path();
                match fs::remove_file(&path) {
                    Ok(_) => info!("Deleted: {}", sanitize_log_path(&path)),
                    Err(e) => log::error!("Failed to delete {}: {}", sanitize_log_path(&path), e),
                }
            }
        }
    }

    if let Err(e) = fs::remove_file(&marker_path) {
        log::error!("Failed to remove wipe marker: {}", e);
    } else {
        info!("Database wipe complete.");
    }
}

pub async fn wipe_database_live() -> Result<(), String> {
    let (path, db) = crate::core::tasks::db_turso::init_turso_database_with_path().await;
    // The marker completes a partially interrupted live wipe on the next start.
    let marker_path = path
        .parent()
        .map(|dir| dir.join(crate::core::tasks::db_turso::WIPE_MARKER_FILE_NAME));
    if let Some(marker_path) = marker_path.as_ref()
        && let Err(err) = fs::write(marker_path, b"")
    {
        log::warn!("Could not mark the database wipe as in progress: {}", err);
    }
    crate::core::tasks::db_turso::wipe_and_rebuild_live(&db)
        .await
        .map_err(|e| format!("Failed to wipe Turso database: {e}"))?;

    let result = crate::core::tasks::db_schema_check::probe_database(&path, &db)
        .await
        .ok_or_else(|| "Failed to validate the rebuilt database schema".to_string())?;
    if !result.is_compatible() {
        return Err(format!(
            "Rebuilt database is incompatible: {}",
            result.problems.join("; ")
        ));
    }
    crate::core::tasks::db_schema_version::mark_wiped();
    if let Some(marker_path) = marker_path.as_ref()
        && let Err(err) = fs::remove_file(marker_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("Could not clear the database wipe marker: {}", err);
    }
    Ok(())
}
