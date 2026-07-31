//! Database lifecycle + write instrumentation.
//!
//! The Turso engine itself is built and bootstrapped in
//! [`crate::core::tasks::db_turso`]; this module keeps the
//! process-wide `init_database()` entry point (delegating to Turso), the
//! filesystem wipe markers, and the write-path instrumentation (perf counters,
//! write permits, lock-retry helpers) that the bulk write paths and sync
//! pipeline read for their metrics. The names retain the `sqlite_*` prefix for
//! call-site stability; the semantics are storage-neutral.

use log::info;
use once_cell::sync::Lazy;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

use crate::core::utils::format::sanitize_log_path;

/// Legacy SQLite-era bind budget for SQL shapes that do not use tuned chunk helpers.
pub(crate) const SQLITE_MAX_VARIABLES: usize = 999;
/// Raw SQL bulk operations (not a query builder) can use a higher bind-variable
/// ceiling. Turso accepts far more than 250k; the historical SQLite
/// 3.32+ limit of 32,766 is kept as a conservative, well-tested chunking bound.
const SQLITE_BULK_VARIABLE_LIMIT: usize = 32_766;
const SQLITE_WRITE_PERMITS_CAP: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SqlitePerfSnapshot {
    pub(crate) lock_retries: u64,
    pub(crate) lock_backoff_ms_total: u64,
    pub(crate) db_write_time_ns_total: u64,
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
}

impl SqlitePerfCounters {
    fn snapshot(&self) -> SqlitePerfSnapshot {
        SqlitePerfSnapshot {
            lock_retries: self.lock_retries.load(Ordering::Relaxed),
            lock_backoff_ms_total: self.lock_backoff_ms_total.load(Ordering::Relaxed),
            db_write_time_ns_total: self.db_write_time_ns_total.load(Ordering::Relaxed),
        }
    }

    fn record_lock_retry(&self, backoff: Duration) {
        self.lock_retries.fetch_add(1, Ordering::Relaxed);
        let backoff_ms = backoff.as_millis().min(u128::from(u64::MAX)) as u64;
        self.lock_backoff_ms_total
            .fetch_add(backoff_ms, Ordering::Relaxed);
    }

    fn record_db_write_time(&self, elapsed: Duration) {
        let elapsed_ns = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
        self.db_write_time_ns_total
            .fetch_add(elapsed_ns, Ordering::Relaxed);
    }
}

static SQLITE_PERF_COUNTERS: Lazy<SqlitePerfCounters> = Lazy::new(SqlitePerfCounters::default);
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
    pub(crate) total_time_ns_total: u64,
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
            total_time_ns_total: self
                .total_time_ns_total
                .saturating_sub(baseline.total_time_ns_total),
        }
    }

    pub(crate) fn total_time_ms(self) -> f64 {
        self.total_time_ns_total as f64 / 1_000_000.0
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

/// Default write-permit count. On the Turso/MVCC storage layer the write path
/// no longer acquires the permit at all - the value
/// only feeds the metadata-rebuild fan-out ceilings (`mod_task_limit` /
/// `part_task_limit`), so default it to CPU count to widen concurrent-writer
/// fan-out.
fn default_write_permits() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

pub(crate) static DB_WRITE_PERMITS: Lazy<usize> = Lazy::new(sqlite_write_permits);
// Bounds concurrent write-heavy transactions. Retained as the fan-out budget for
// the metadata rebuild; the Turso write path itself no longer serializes on it.
pub(crate) static DB_WRITE_SEMAPHORE: Lazy<std::sync::Arc<Semaphore>> =
    Lazy::new(|| std::sync::Arc::new(Semaphore::new(*DB_WRITE_PERMITS)));

/// Permits for the Turso **write-serialization gate** (`DB_WRITE_GATE`). Defaults
/// to **1** - single-writer serialization that matches Turso's one internal
/// writer. Overridable via `FOXY_DB_WRITE_GATE` for sweeps (see
/// `after_turso_regression_analysis2.md`).
fn db_write_gate_permits() -> usize {
    env_usize("FOXY_DB_WRITE_GATE")
        .unwrap_or(1)
        .clamp(1, SQLITE_WRITE_PERMITS_CAP)
}

/// Serializes the seam's write transactions (`transaction` / `execute_retry` /
/// `begin`) so the metadata-rebuild fan-out (up to `mod_task_limit()` ≈ 16
/// concurrent mod tasks) no longer convoys on Turso's single internal writer.
///
/// This is distinct from [`DB_WRITE_SEMAPHORE`], which bounds *fan-out* (how many
/// mods fetch/hash concurrently). The gate bounds *concurrent committers*. Turso
/// serializes writes internally regardless; without the gate the waiters block
/// **inside** `conn.execute` (no `Busy` surfaced, so retry/backoff never engages)
/// and every batch's timing is inflated by the queue wait - the
/// `after_turso_regression_analysis2.md` convoy (87-row and 18 008-row batches
/// both ~24s). Gating at 1 lets the writer run flat-out back-to-back instead.
/// Reads (`read_transaction`, `query_*`) are intentionally NOT gated.
pub(crate) static DB_WRITE_GATE: Lazy<std::sync::Arc<Semaphore>> =
    Lazy::new(|| std::sync::Arc::new(Semaphore::new(db_write_gate_permits())));

/// Acquire the write-serialization gate, returning the held permit and the time
/// spent waiting for it. The permit is released when dropped; callers hold it for
/// the full begin→commit window. A `None` permit (semaphore closed - never in
/// practice) degrades to ungated rather than blocking forever.
pub(crate) async fn acquire_db_write_gate() -> (Option<tokio::sync::OwnedSemaphorePermit>, Duration)
{
    let started = Instant::now();
    let permit = DB_WRITE_GATE.clone().acquire_owned().await.ok();
    (permit, started.elapsed())
}

/// Exclusive-access barrier for bulk operations that MUST run strictly alone on
/// the shared `Arc<turso::Database>`. Turso (beta) hard-wedges when one long-held
/// write transaction (the force-redownload purge: a ~17s sequence deleting 66k+
/// `subfiles` rows) overlaps reads/writes issued from another connection on
/// another runtime - the UI list-cache pending-update workers, quick scans, etc.
/// all `block_on` their own runtime against this same handle. Reproduced
/// deterministically by `db_turso::tests::repro_purge_wedge_under_concurrency`
/// (single connection completes; +2 concurrent readers wedge forever) and seen
/// twice in production force-redownloads (purge stalls after `delete addon_files`
/// and never commits). The write gate only serializes *writers*; reads are
/// deliberately ungated, so it cannot prevent this.
///
/// Normal seam operations take a shared `read()` guard - uncontended and ~free
/// while no purge runs (an `RwLock` read is a relaxed atomic in that case). The
/// purge takes the exclusive `write()` guard, so it drains in-flight access and
/// blocks new access until it commits. Safe against reentrancy because the seam's
/// transaction closures only ever touch the lock-free `DbTxn` handle, never the
/// outer `FoxyDb`, so no task re-acquires the guard while holding it.
pub(crate) static DB_EXCLUSIVE: Lazy<std::sync::Arc<tokio::sync::RwLock<()>>> =
    Lazy::new(|| std::sync::Arc::new(tokio::sync::RwLock::new(())));

/// Shared guard for ordinary reads/writes (see [`DB_EXCLUSIVE`]). Held for the
/// duration of a single seam operation.
pub(crate) async fn acquire_db_shared() -> tokio::sync::OwnedRwLockReadGuard<()> {
    DB_EXCLUSIVE.clone().read_owned().await
}

/// Exclusive guard for bulk operations that must not run concurrently with any
/// other DB access (the repository purge). Held across the whole begin→commit
/// window.
pub(crate) async fn acquire_db_exclusive() -> tokio::sync::OwnedRwLockWriteGuard<()> {
    DB_EXCLUSIVE.clone().write_owned().await
}

pub(crate) fn sqlite_perf_snapshot() -> SqlitePerfSnapshot {
    SQLITE_PERF_COUNTERS.snapshot()
}

/// Whether a DB error message is a transient lock/conflict that should be
/// retried by the hand-rolled retry loops (bulk part-hash persist, etc.).
/// Delegates to the single classifier in `db_turso` so the retryable set
/// cannot drift between the seam, the bulk loops, and the typed fallback.
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

/// Record one completed seam transaction into the per-category write metrics and
/// the global write-time counter. Mirrors [`SqliteWriteScope`]'s `Drop`
/// accounting but takes a runtime `&str` label and an explicit committed flag, so
/// the seam's `transaction` / `execute_retry` wrappers (which don't hold a
/// `'static` label) can attribute their time per category again. Restores the
/// observability the Turso cutover dropped - before this, the final
/// `SQLite write category metrics` report listed only `persist bulk part hashes`
/// (after_turso_regression_analysis.md §"Observability regressed", rec #4).
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
    metric.total_time_ns_total += duration_ns(elapsed);
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
    deltas.sort_by_key(|entry| std::cmp::Reverse(entry.1.total_time_ns_total));

    info!(
        "SQLite write category summary: context={} categories={}",
        context,
        deltas.len()
    );
    for (label, metric) in deltas.into_iter().take(12) {
        info!(
            "SQLite write category metrics: context={} label={} calls={} committed={} failed={} retries={} backoff_ms={} permit_wait_ms={:.1} total_ms={:.1}",
            context,
            label,
            metric.calls,
            metric.committed,
            metric.failed,
            metric.lock_retries,
            metric.lock_backoff_ms_total,
            metric.permit_wait_ms(),
            metric.total_time_ms()
        );
    }
}

/// Maximum number of bind variables for raw SQL bulk operations. Turso accepts
/// far more than SQLite, but the conservative SQLite-era
/// 32,766 ceiling is kept as a well-tested chunking bound.
///
/// Overridable via `FOXY_DB_VAR_LIMIT` so the bulk-statement chunk size can be
/// swept without recompiling (after_turso_regression_analysis2.md P1: find the
/// per-statement knee for Turso's writer - very large statements may be slower
/// than several medium ones). Every bulk chunk size that derives from this
/// (part upsert, download-target upsert, IN-list prefetch/delete) moves together.
/// Clamped to a sane floor/ceiling.
pub(crate) fn sqlite_variable_limit() -> usize {
    env_usize("FOXY_DB_VAR_LIMIT")
        .map(|v| v.clamp(60, SQLITE_BULK_VARIABLE_LIMIT))
        .unwrap_or(SQLITE_BULK_VARIABLE_LIMIT)
}

/// Chunk size for read-side IN-list queries.
pub(crate) fn read_chunk_ids() -> usize {
    sqlite_variable_limit().saturating_sub(10).max(1)
}

/// Rows per multi-row bulk write statement (`INSERT … VALUES` /
/// `UPDATE … FROM (VALUES …)`).
///
/// Turso's per-statement parse/bind/plan cost is **superlinear in row count**,
/// so many SMALL statements beat a few huge ones - the opposite of the SQLite-era
/// "pack as many rows per statement as the variable limit allows" rule. Measured
/// on 66 336 subfile rows under single-writer WAL (`bench_fresh_insert_vs_upsert`,
/// `bench_bulk_update_chunk`): ~256 rows/stmt ≈ 12–22 µs/row vs ≈ 45–70 µs/row at
/// 5 460–8 190 rows/stmt - roughly **3–4× faster**. The curve is flat from ~64 to
/// ~256, so 256 sits at the knee without exploding statement count (and the
/// per-statement async round-trip the micro-bench understates). Override with
/// `FOXY_DB_CHUNK_ROWS` to re-tune if the engine's statement cost changes.
pub(crate) fn bulk_write_chunk_rows() -> usize {
    env_usize("FOXY_DB_CHUNK_ROWS")
        .unwrap_or(256)
        .clamp(16, SQLITE_BULK_VARIABLE_LIMIT)
}

/// Rows per multi-row bulk write statement for a row shape with `params_per_row` binds.
pub(crate) fn bulk_write_rows_for(params_per_row: usize) -> usize {
    bulk_write_chunk_rows()
        .min(
            (sqlite_variable_limit() / params_per_row.max(1))
                .saturating_sub(1)
                .max(1),
        )
        .max(1)
}

/// Process-wide database handle for the active game space. Builds/bootstraps
/// the Turso engine via [`crate::core::tasks::db_turso`] and runs the
/// post-init maintenance passes (content-hash baseline retirement, addon
/// display-name backfill) once per database file, so a runtime game-space
/// switch maintains the newly opened space's database too.
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

/// Blank stale content-hash baselines so scans lazily rebuild them in the current format.
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

/// Release the cached database handle from the UI thread.
///
/// A runtime game-space switch must not leave the previous space's
/// `database.db` open: the slot only swaps on the next database access, so a
/// space that is switched away from and then removed would hit `remove_dir_all`
/// against a live handle and half-delete its workspace on Windows.
pub fn close_active_database_sync() {
    let Some(runtime) = crate::core::api::background_runtime() else {
        log::warn!("No background runtime available to release the database handle");
        return;
    };
    runtime.block_on(crate::core::tasks::db_turso::close_active_database());
}

/// Check for wipe marker and delete database files if present.
/// Call this BEFORE init_database() to ensure files aren't locked.
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

    // Remove the marker file
    if let Err(e) = fs::remove_file(&marker_path) {
        log::error!("Failed to remove wipe marker: {}", e);
    } else {
        info!("Database wipe complete.");
    }
}

/// Wipe the database by dropping all tables and re-applying the bootstrap schema
/// on the live Turso handle. Works while the app is running because it reuses
/// the existing engine handle.
pub async fn wipe_database_live() -> Result<(), String> {
    let db = crate::core::tasks::db_turso::init_turso_database().await;
    crate::core::tasks::db_turso::wipe_and_rebuild_live(&db)
        .await
        .map_err(|e| format!("Failed to wipe Turso database: {e}"))
}
