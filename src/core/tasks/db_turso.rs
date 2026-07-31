//! Turso data-layer keystone.
//!
//! This is the Turso counterpart of the SeaORM/SQLite keystone in
//! `init_database.rs`: it builds the local database, applies the single
//! authoritative bootstrap schema (`sql/turso_schema.sql`), hands out tuned
//! connections, and wraps write transactions in a retry loop matched to Turso's
//! error variants. This is the live persistence engine for the GUI, CLI, and
//! `foxy-server-backend-cli`.
//!
//! Compatibility findings carried into this module:
//! - FKs are OFF by default → enabled per-connection.
//! - Honored PRAGMAs: `foreign_keys`, `synchronous`, `temp_store`, `cache_size`.
//! - `busy_timeout` is a `Connection` method, not a PRAGMA.
//! - `wal_autocheckpoint` / `journal_size_limit` / `mmap_size` are no-ops (Turso
//!   manages its own WAL) → dropped, not reproduced.
//! - `connect()` is ~16µs → per-task connections, no pool.
#![allow(dead_code)] // A few helpers (db_retry_transaction, etc.) are exercised only by tests.

use std::fs;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::FutureExt;
use log::{debug, info, warn};
use turso::{Builder, Connection, Database, Error};

use crate::core::utils::format::sanitize_log_path;

/// The folded bootstrap schema (migrations 01..21 in final state). Applied once
/// to a fresh database; the auto-wipe gate (`db_schema_version.rs`) guarantees a
/// clean rebuild on the breaking Turso upgrade so no incremental replay is needed.
const TURSO_BOOTSTRAP_SCHEMA: &str = include_str!("../../../sql/turso_schema.sql");

/// Canonical `subfiles` table DDL, kept token-identical to `sql/turso_schema.sql`.
pub(crate) const SUBFILES_CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS subfiles (\
    id INTEGER PRIMARY KEY, \
    file_id INTEGER NOT NULL, \
    path TEXT, \
    local_length INTEGER, \
    local_start INTEGER, \
    remote_length INTEGER, \
    remote_start INTEGER, \
    local_checksum TEXT, \
    remote_checksum TEXT, \
    data_order INTEGER, \
    FOREIGN KEY (file_id) REFERENCES files(id))";

/// `subfiles` index `CREATE` statements (the unique `(file_id, path)` index that
/// backs `ON CONFLICT`, plus the `(file_id, data_order, id)` ordered-read index).
/// The whole-wipe purge recreates these with the table. Order: unique first so reads
/// have it as soon as possible.
///
/// The old `idx_subfiles_path_remote_checksum (path, remote_checksum)` index was
/// dropped in schema v24 (after_turso_regression_analysis6.md): every `subfiles`
/// query filters by `file_id` (covered by the two indexes below), so the
/// `(path, remote_checksum)` tree had no primary user - it only appeared as a
/// secondary join filter in sibling hash propagation, which is already narrowed to a
/// single file's parts by `file_id`. Removing it cuts every part insert/delete from
/// 4 → 3 B-trees (~25% less write work on the 66k-row TFR_40K force-redownload).
pub(crate) const SUBFILES_INDEX_CREATE_SQL: [&str; 2] = [
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_subfiles_file_id_path ON subfiles(file_id, path)",
    "CREATE INDEX IF NOT EXISTS idx_subfiles_file_id_data_order ON subfiles(file_id, data_order, id)",
];

/// `subfiles` index names, for `DROP INDEX IF EXISTS` when rebuilding the indexes.
pub(crate) const SUBFILES_INDEX_NAMES: [&str; 2] = [
    "idx_subfiles_file_id_path",
    "idx_subfiles_file_id_data_order",
];

const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
const DB_MAX_RETRIES: usize = 5;

/// Every file the Turso engine may leave beside `database.db`. Shared by the
/// startup wipe and the game-space layout migration.
pub(crate) const DATABASE_ARTIFACT_FILE_NAMES: [&str; 9] = [
    "database.db",
    "database.db-wal",
    "database.db-shm",
    "database.db.compacting",
    "database.db.compacting-wal",
    "database.db.compacting-shm",
    "database.db.bak",
    "database.db.bak-wal",
    "database.db.bak-shm",
];

pub(crate) const DATABASE_REBUILD_BACKUP_PREFIX: &str = "database.db.rebuild-backup-";

pub(crate) const WIPE_MARKER_FILE_NAME: &str = ".wipe_database_on_next_start";

/// Filesystem path to `database.db` in the active game space dir, creating the
/// parent directory and an empty file if missing (Turso's `Builder::new_local`
/// takes a path, not a `sqlite://` URL).
pub(crate) fn database_file_path() -> PathBuf {
    let db_path = crate::core::game::spaces::active_game_space_dir().join("database.db");
    if let Some(parent) = db_path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        log::error!(
            "Failed to create database directory {}: {}",
            sanitize_log_path(parent),
            e
        );
    }
    db_path
}

/// Open + bootstrap a Turso database at `path`. Factored out of
/// [`init_turso_database`] so the engine and schema can be validated against a
/// temp path in tests without the process-wide handle slot.
pub(crate) async fn build_and_bootstrap(path: &str) -> turso::Result<Database> {
    let db = Builder::new_local(path).build().await?;
    // Schema creation needs FK enforcement on so the CASCADE chains are recorded.
    let conn = connect_tuned(&db).await?;
    apply_schema(&conn, TURSO_BOOTSTRAP_SCHEMA).await?;
    Ok(db)
}

/// Apply a multi-statement schema. Turso 0.6.1 has `execute_batch`, but the
/// bootstrap file is heavily commented (including inline `-- …` column notes),
/// so we strip comments and execute statement-by-statement for deterministic
/// behavior regardless of the engine's batch tokenizer.
async fn apply_schema(conn: &Connection, schema: &str) -> turso::Result<()> {
    let stripped: String = schema
        .lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    for statement in stripped.split(';') {
        let trimmed = statement.trim();
        if trimmed.is_empty() {
            continue;
        }
        conn.execute(trimmed, ()).await?;
    }
    Ok(())
}

/// Live wipe-and-rebuild for the running app (Turso counterpart of
/// `init_database::wipe_database_live`). Drops every known table on an existing
/// connection, then re-applies the bootstrap schema - so the in-process handle
/// stays valid across the wipe (used by the schema-version wipe prompt).
pub(crate) async fn wipe_and_rebuild_live(db: &Database) -> turso::Result<()> {
    // Full live wipe is a destructive bulk operation. Hold the same exclusive
    // barrier as repository purge so this cannot race another seam read/write on
    // the shared Turso handle and surface as "database is locked".
    let _exclusive = crate::core::tasks::init_database::acquire_db_exclusive().await;
    // FK enforcement is on per tuned connection; drop in dependency order so the
    // CASCADE chains don't fight the explicit DROPs.
    let conn = connect_tuned(db).await?;
    let tables = [
        "pending_updates",
        "download_patch_op",
        "download_patch_file",
        "download_target_file_part",
        "download_target_file",
        "addon_files",
        "repository_addons",
        "subfiles",
        "files",
        "addons",
        "repositories",
    ];
    info!("Wiping Turso database tables...");
    for table in tables {
        conn.execute(&format!("DROP TABLE IF EXISTS {table}"), ())
            .await?;
    }
    info!("Re-applying Turso bootstrap schema...");
    apply_schema(&conn, TURSO_BOOTSTRAP_SCHEMA).await?;
    info!("Database wipe complete.");
    Ok(())
}

/// Whether Turso's MVCC concurrent-write mode is active.
///
/// Defaults **OFF**. MVCC (`journal_mode='mvcc'` + `BEGIN CONCURRENT`) is still
/// beta and measured *much* slower than single-writer WAL for this app's
/// write-heavy metadata-rebuild / hash-persist workload: the per-Database version
/// store accumulates across a session, so sustained sequential upserts run ~8×
/// slower and, fanned out across the ~16-way concurrent mod rebuild, degrade to
/// O(N²) (a single 16k-row batch hit 372s on TFR_40K). It also caused
/// cross-connection read-after-write misses ("Part record missing after upsert")
/// and the cross-runtime purge deadlock. Single-writer WAL matches the proven-fast
/// pre-Turso SQLite baseline and handles the concurrent fan-out cleanly via
/// busy_timeout + retry (benchmarked: 16 writers, 0 retries). Reproducers:
/// `bench_mvcc_write_degradation` / `bench_mvcc_concurrent_writers`.
///
/// Set `FOXY_DB_MVCC=1` (or `true`/`on`/`yes`) to opt back in for experiments
/// once the engine's MVCC write path matures.
pub(crate) fn mvcc_enabled() -> bool {
    matches!(
        std::env::var("FOXY_DB_MVCC")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("on") | Some("yes")
    )
}

/// Read back the engine's effective `journal_mode` for a tuned connection.
/// Used at startup to confirm the file is on single-writer WAL (not a leftover
/// `mvcc` from the default-on era); the regression-analysis benchmark keys off
/// this line.
pub(crate) async fn read_journal_mode(conn: &Connection) -> Option<String> {
    let mut rows = conn.query("PRAGMA journal_mode", ()).await.ok()?;
    let row = rows.next().await.ok()??;
    row.get::<String>(0).ok()
}

/// Open a fresh connection and apply the honored PRAGMAs + busy timeout. Per the
/// spike, connections are ~16µs to create, so callers take one per task.
pub(crate) async fn connect_tuned(db: &Database) -> turso::Result<Connection> {
    let conn = db.connect()?;
    // Honored PRAGMAs only (spike §11). `pragma_update` issues `PRAGMA x = v`
    // and drains any returned row.
    conn.pragma_update("foreign_keys", "ON").await?;
    conn.pragma_update("synchronous", "NORMAL").await?;
    conn.pragma_update("temp_store", "MEMORY").await?;
    conn.pragma_update("cache_size", "-16384").await?; // 16 MiB page cache
    // MVCC concurrent writes are opt-in only. When enabled, set per-connection
    // so every task's connection shares the mode; paired with `BEGIN CONCURRENT`.
    //
    // Default (off) sets single-writer WAL. Setting it EXPLICITLY (rather than
    // leaving it unset) is required to MIGRATE an existing database created under
    // the old default-on MVCC era: Turso persists `journal_mode='mvcc'` in the
    // file, so a connection that merely skips the pragma keeps running the MVCC
    // engine - which is catastrophically slow even without `BEGIN CONCURRENT`
    // (measured on the real DB: a 66k-row purge took ~900s under mvcc vs ~33s
    // under WAL - the source of the force-redownload "hang"). The switch is
    // idempotent (a no-op once the file is WAL) and runs on the bootstrap
    // connection before any concurrency, when exclusive access is guaranteed.
    if mvcc_enabled() {
        conn.pragma_update("journal_mode", "mvcc").await?;
    } else {
        conn.pragma_update("journal_mode", "wal").await?;
    }
    // busy_timeout is a method in Turso, not a PRAGMA.
    conn.busy_timeout(DB_BUSY_TIMEOUT)?;
    Ok(conn)
}

// --- Startup bloat inspection + opt-in compaction (analysis4 P0) --------------
//
// MVCC-era churn left `database.db` ~97% free pages (e.g. 349 648 pages / 338 750
// free = 1.37 GB on disk for ~44 MB of live rows). Under WAL the file no longer
// hangs, but every B-tree walk (the 66k-row `subfiles` upsert / hash-persist /
// purge, plus the post-write read scans) pays the bloat: the production purge txn
// is 30.6 s vs 0.51 s pre-Turso, and `db_write_time_ms` is 15× the SQLite
// baseline - almost entirely on `subfiles`. Compacting the file rebuilds those
// B-trees densely and is the single highest-value lever (analysis4 §P0).
//
// In-place `VACUUM` is gated behind Turso's `--experimental-vacuum` (unusable at
// runtime), but `VACUUM INTO '<file>'` - which writes a fully compacted *copy* to
// a new file - works in 0.6.1 (probe `probe_vacuum_into`: 463→36 pages). So we
// compact by VACUUM INTO a sibling temp file then atomically swap it in.

/// Compact only when the file is BOTH substantially free-paged AND large enough
/// that the walk cost matters - a fresh/small db is never churned.
const COMPACT_MIN_FREE_PAGES: i64 = 20_000;
const COMPACT_MIN_FREE_RATIO: f64 = 0.5;
/// Automatic startup compaction live-row ceiling.
const COMPACT_AUTO_MAX_LIVE_MIB: f64 = 512.0;
/// How long a post-compaction backup is kept.
const DB_BACKUP_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

#[derive(Debug, Clone, Copy)]
struct DbBloatStats {
    page_count: i64,
    page_size: i64,
    freelist_count: i64,
}

impl DbBloatStats {
    fn free_ratio(&self) -> f64 {
        if self.page_count <= 0 {
            0.0
        } else {
            self.freelist_count as f64 / self.page_count as f64
        }
    }
    fn file_mib(&self) -> f64 {
        (self.page_count.max(0) * self.page_size) as f64 / (1024.0 * 1024.0)
    }
    fn live_mib(&self) -> f64 {
        ((self.page_count - self.freelist_count).max(0) * self.page_size) as f64 / (1024.0 * 1024.0)
    }
    fn is_bloated(&self) -> bool {
        self.freelist_count >= COMPACT_MIN_FREE_PAGES && self.free_ratio() >= COMPACT_MIN_FREE_RATIO
    }
}

async fn read_pragma_i64(conn: &Connection, pragma: &str) -> Option<i64> {
    let mut rows = conn.query(&format!("PRAGMA {pragma}"), ()).await.ok()?;
    let row = rows.next().await.ok()??;
    match row.get_value(0).ok()? {
        turso::Value::Integer(i) => Some(i),
        _ => None,
    }
}

async fn read_db_bloat_stats(conn: &Connection) -> Option<DbBloatStats> {
    Some(DbBloatStats {
        page_count: read_pragma_i64(conn, "page_count").await?,
        page_size: read_pragma_i64(conn, "page_size").await.unwrap_or(4096),
        freelist_count: read_pragma_i64(conn, "freelist_count").await.unwrap_or(0),
    })
}

/// Startup compaction mode from `FOXY_DB_COMPACT`.
fn compact_mode() -> String {
    std::env::var("FOXY_DB_COMPACT")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Append a literal suffix to a path's filename (e.g. `database.db` + `-wal` →
/// `database.db-wal`; + `.bak` → `database.db.bak`). Not `set_extension`, which
/// would clobber the existing `.db`.
fn with_suffix(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn remove_db_artifacts(path: &std::path::Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(with_suffix(path, "-wal"));
    let _ = fs::remove_file(with_suffix(path, "-shm"));
}

/// Remove stale maintenance artifacts next to `database.db`.
fn sweep_stale_db_artifacts(path: &Path) {
    let compacting = with_suffix(path, ".compacting");
    if compacting.exists()
        || with_suffix(&compacting, "-wal").exists()
        || with_suffix(&compacting, "-shm").exists()
    {
        info!(
            "STARTUP: removing stale compaction artifacts {}",
            sanitize_log_path(&compacting)
        );
        remove_db_artifacts(&compacting);
    }

    let bak = with_suffix(path, ".bak");
    if let Ok(metadata) = fs::metadata(&bak) {
        let expired = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > DB_BACKUP_MAX_AGE);
        if expired {
            info!(
                "STARTUP: removing expired database backup {}",
                sanitize_log_path(&bak)
            );
            remove_db_artifacts(&bak);
        }
    }
}

/// Move WAL/SHM sidecars alongside their main database file destination.
async fn move_db_sidecars(from: &Path, to: &Path) -> std::io::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let from_s = with_suffix(from, suffix);
        if from_s.exists() {
            let to_s = with_suffix(to, suffix);
            let _ = fs::remove_file(&to_s);
            rename_with_retry(&from_s, &to_s).await?;
        }
    }
    Ok(())
}

fn rebuild_backup_path(path: &Path) -> PathBuf {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    for attempt in 0..100 {
        let suffix = if attempt == 0 {
            format!(".rebuild-backup-{seconds}")
        } else {
            format!(".rebuild-backup-{seconds}-{attempt}")
        };
        let candidate = with_suffix(path, &suffix);
        if !candidate.exists()
            && !with_suffix(&candidate, "-wal").exists()
            && !with_suffix(&candidate, "-shm").exists()
        {
            return candidate;
        }
    }
    with_suffix(path, ".rebuild-backup")
}

/// Preserve a failed database before creating a clean replacement.
async fn move_db_artifacts_to_rebuild_backup(path: &Path) -> std::io::Result<PathBuf> {
    let backup = rebuild_backup_path(path);
    if path.exists() {
        rename_with_retry(path, &backup).await?;
    }
    move_db_sidecars(path, &backup).await?;
    Ok(backup)
}

async fn build_or_rebuild_after_failure(path: &Path, path_str: &str) -> turso::Result<Database> {
    match build_and_bootstrap(path_str).await {
        Ok(db) => Ok(db),
        Err(first_err) => {
            warn!(
                "STARTUP: Turso database initialization failed ({}); attempting clean rebuild",
                first_err
            );
            match move_db_artifacts_to_rebuild_backup(path).await {
                Ok(backup) => info!(
                    "STARTUP: moved failed database artifacts to {} before rebuild",
                    sanitize_log_path(&backup)
                ),
                Err(move_err) => {
                    warn!(
                        "STARTUP: could not move failed database aside ({}); removing database artifacts before rebuild",
                        move_err
                    );
                    remove_db_artifacts(path);
                }
            }
            match build_and_bootstrap(path_str).await {
                Ok(db) => {
                    crate::core::tasks::db_schema_version::mark_wiped();
                    info!("STARTUP: rebuilt Turso database after initialization failure");
                    Ok(db)
                }
                Err(second_err) => {
                    log::error!(
                        "Failed to initialize Turso database after clean rebuild: {}",
                        second_err
                    );
                    Err(second_err)
                }
            }
        }
    }
}

/// Tables copied during a manual compaction, FK-parent-first (FK enforcement is
/// disabled on the destination during the copy, so order is not strictly
/// required, but parent-first keeps it intuitive). Mirrors the bootstrap schema.
const COMPACT_COPY_TABLES: &[&str] = &[
    "repositories",
    "pending_updates",
    "addons",
    "files",
    "subfiles",
    "repository_addons",
    "addon_files",
    "download_target_file",
    "download_target_file_part",
    "download_patch_file",
    "download_patch_op",
];

/// Inspect the open database and decide whether startup should compact it.
async fn should_compact_database(db: &Database) -> bool {
    let conn = match connect_tuned(db).await {
        Ok(c) => c,
        Err(e) => {
            warn!("STARTUP: could not connect for bloat inspection: {}", e);
            return false;
        }
    };
    let stats = read_db_bloat_stats(&conn).await;
    if let Some(s) = stats {
        info!(
            "STARTUP: Turso db file≈{:.1}MiB pages={} free_pages={} ({:.0}% free) live≈{:.1}MiB",
            s.file_mib(),
            s.page_count,
            s.freelist_count,
            s.free_ratio() * 100.0,
            s.live_mib(),
        );
    }
    let mode = compact_mode();
    if mode == "force" {
        return true;
    }
    if mode == "off" {
        return false;
    }
    if let Some(s) = stats.filter(|s| s.is_bloated()) {
        if s.live_mib() < COMPACT_AUTO_MAX_LIVE_MIB {
            return true;
        }
        info!(
            "STARTUP: database is bloated but its live size ({:.0}MiB) exceeds the automatic compaction bound; set FOXY_DB_COMPACT=force to rebuild it",
            s.live_mib()
        );
    }
    false
}

/// Copy every row of `table` from `src` into `dst` in chunked multi-row inserts.
/// Generic over schema: reads the column list from the prepared statement and
/// round-trips raw `turso::Value`s, so it needs no per-table knowledge.
async fn copy_table(src: &Connection, dst: &Connection, table: &str) -> turso::Result<usize> {
    const CHUNK_ROWS: usize = 256;
    let mut rows = src.query(&format!("SELECT * FROM {table}"), ()).await?;
    let cols = rows.column_names();
    let ncols = cols.len();
    if ncols == 0 {
        return Ok(0);
    }
    let col_list = cols.join(", ");
    let row_ph = format!("({})", vec!["?"; ncols].join(", "));

    let mut buf: Vec<turso::Value> = Vec::with_capacity(CHUNK_ROWS * ncols);
    let mut pending = 0usize;
    let mut total = 0usize;

    while let Some(row) = rows.next().await? {
        for i in 0..ncols {
            buf.push(row.get_value(i)?);
        }
        pending += 1;
        total += 1;
        if pending >= CHUNK_ROWS {
            let sql = format!(
                "INSERT INTO {table} ({col_list}) VALUES {}",
                vec![row_ph.as_str(); pending].join(", ")
            );
            dst.execute(&sql, std::mem::take(&mut buf)).await?;
            pending = 0;
        }
    }
    if pending > 0 {
        let sql = format!(
            "INSERT INTO {table} ({col_list}) VALUES {}",
            vec![row_ph.as_str(); pending].join(", ")
        );
        dst.execute(&sql, std::mem::take(&mut buf)).await?;
    }
    Ok(total)
}

/// Build a fully compacted copy of the database at `path` into a sibling temp
/// file by re-inserting every live row into a fresh schema (a dense rebuild -
/// the only free pages it has are its own). Returns the temp path on success.
///
/// NB: we do NOT use `VACUUM INTO` - it **panics inside Turso 0.6.1**
/// (`vdbe/vacuum.rs:845`) on large/bloated files (it worked only on the tiny
/// probe db). The manual SELECT/INSERT copy avoids that engine path entirely.
async fn build_compacted_copy(path: &Path) -> turso::Result<PathBuf> {
    let tmp = with_suffix(path, ".compacting");
    remove_db_artifacts(&tmp);
    let tmp_str = tmp.to_string_lossy().to_string();

    // Source: a fresh handle (the live handle was dropped by the caller).
    let src_db = Builder::new_local(&path.to_string_lossy()).build().await?;
    let src = connect_tuned(&src_db).await?;
    // Destination: fresh file with the bootstrap schema; FK off so insert order
    // never blocks, synchronous OFF since a crash just discards this temp file.
    let dst_db = build_and_bootstrap(&tmp_str).await?;
    let dst = dst_db.connect()?;
    dst.pragma_update("foreign_keys", "OFF").await?;
    dst.pragma_update("synchronous", "OFF").await?;

    dst.execute("BEGIN", ()).await?;
    for table in COMPACT_COPY_TABLES {
        let n = copy_table(&src, &dst, table).await?;
        debug!("STARTUP: compaction copied {n} rows from {table}");
    }
    dst.execute("COMMIT", ()).await?;
    // Best-effort checkpoint to shrink the WAL; Turso may not honor it, so we do
    // NOT rely on it - `swap_compacted_file` moves the `<tmp>-wal`/`-shm`
    // sidecars alongside the main file so committed rows are never left behind.
    let _ = dst.execute("PRAGMA wal_checkpoint(TRUNCATE)", ()).await;

    // Close every handle so the file can be swapped on Windows.
    drop(dst);
    drop(dst_db);
    drop(src);
    drop(src_db);
    Ok(tmp)
}

/// Run a panic-safe compaction of the database file at `path`, swapping the
/// dense copy in on success. Returns true if the live file was replaced. The
/// source file is only read (the copy is written to a sibling temp), so any
/// failure - including a Turso panic - leaves the original intact; we just log
/// and continue uncompacted. **Call only after the live `Database` handle is
/// dropped.**
async fn compact_database_file(path: &Path) -> bool {
    let started = Instant::now();
    info!(
        "STARTUP: database is bloated; compacting (one-time rebuild) {}",
        sanitize_log_path(path)
    );
    let outcome = AssertUnwindSafe(build_compacted_copy(path))
        .catch_unwind()
        .await;
    let tmp = match outcome {
        Ok(Ok(tmp)) => {
            info!(
                "STARTUP: compacted copy built in {:.2}s",
                started.elapsed().as_secs_f64()
            );
            tmp
        }
        Ok(Err(e)) => {
            warn!(
                "STARTUP: compaction failed ({}); keeping existing database uncompacted",
                e
            );
            remove_db_artifacts(&with_suffix(path, ".compacting"));
            return false;
        }
        Err(_) => {
            warn!(
                "STARTUP: compaction panicked inside the engine; keeping existing database uncompacted"
            );
            remove_db_artifacts(&with_suffix(path, ".compacting"));
            return false;
        }
    };
    swap_compacted_file(path, &tmp).await
}

/// Best-effort `fs::rename` with a short bounded retry - on Windows the OS may
/// hold the file briefly after the Turso handle is dropped.
async fn rename_with_retry(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    let mut last = None;
    for attempt in 0..20 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(25 + attempt * 5)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// Swap the freshly compacted copy at `tmp` into `path`, keeping the original as
/// `database.db.bak`. The compacted copy is a complete standalone db, so the old
/// WAL/SHM sidecars are discarded. Returns true on success; on any failure the
/// original is restored and left in place. Call only after the live `Database`
/// handle has been dropped.
async fn swap_compacted_file(path: &Path, tmp: &Path) -> bool {
    let bak = with_suffix(path, ".bak");
    // Clear any stale backup (main + sidecars).
    remove_db_artifacts(&bak);

    // Move the original aside as a complete backup (main + its WAL/SHM).
    if let Err(e) = rename_with_retry(path, &bak).await {
        warn!(
            "STARTUP: could not move old database aside ({}); discarding compacted copy",
            e
        );
        remove_db_artifacts(tmp);
        return false;
    }
    let _ = move_db_sidecars(path, &bak).await;

    // Install the compacted copy (main + its WAL/SHM) at the live path.
    if let Err(e) = rename_with_retry(tmp, path).await {
        warn!(
            "STARTUP: could not install compacted database ({}); restoring original",
            e
        );
        let _ = rename_with_retry(&bak, path).await;
        let _ = move_db_sidecars(&bak, path).await;
        remove_db_artifacts(tmp);
        return false;
    }
    let _ = move_db_sidecars(tmp, path).await;
    info!(
        "STARTUP: compacted database installed; previous file kept as {}",
        sanitize_log_path(&bak)
    );
    true
}

/// Set while startup compaction and reopen hold the database.
static DB_STARTUP_COMPACTION_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static DB_STARTUP_COMPACTION_STARTED: std::sync::Mutex<Option<Instant>> =
    std::sync::Mutex::new(None);

/// True while startup compaction is holding the database.
pub(crate) fn db_startup_compaction_active() -> bool {
    DB_STARTUP_COMPACTION_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Elapsed time for the active startup compaction.
pub(crate) fn db_startup_compaction_elapsed() -> Option<Duration> {
    if !db_startup_compaction_active() {
        return None;
    }
    DB_STARTUP_COMPACTION_STARTED
        .lock()
        .ok()
        .and_then(|started| started.map(|at| at.elapsed()))
}

fn set_db_startup_compaction_active(active: bool) {
    if active && let Ok(mut started) = DB_STARTUP_COMPACTION_STARTED.lock() {
        *started = Some(Instant::now());
    }
    DB_STARTUP_COMPACTION_ACTIVE.store(active, std::sync::atomic::Ordering::Relaxed);
}

/// Slot holding the handle for the active game space's database, keyed by
/// file path so switching the active space at runtime opens the target
/// space's database. Dropping the previous handle here is safe: in-flight
/// tasks that cloned it keep the old database alive until they finish.
static DB_SLOT: tokio::sync::Mutex<Option<(PathBuf, Arc<Database>)>> =
    tokio::sync::Mutex::const_new(None);

/// Process-wide Turso database handle for the active game space, mirroring
/// the `Arc<DatabaseConnection>` shape so it slots into `FoxyContext`.
pub(crate) async fn init_turso_database() -> Arc<Database> {
    init_turso_database_with_path().await.1
}

/// Same as [`init_turso_database`], also returning the file path the handle
/// was opened for, so callers that key one-time per-database work never race
/// a concurrent game-space switch by re-resolving the path themselves.
pub(crate) async fn init_turso_database_with_path() -> (PathBuf, Arc<Database>) {
    let path = database_file_path();
    let mut slot = DB_SLOT.lock().await;
    if let Some((open_path, db)) = slot.as_ref()
        && *open_path == path
    {
        return (path, db.clone());
    }
    if slot.is_some() {
        info!(
            "Active game space changed; opening database {}",
            sanitize_log_path(&path)
        );
    }
    let db = open_and_prepare_database(&path).await;
    *slot = Some((path.clone(), db.clone()));
    (path, db)
}

/// Drop the cached handle for whichever database is currently open, releasing
/// the file so the previous game space's directory can be deleted. In-flight
/// tasks that cloned the handle keep their database alive until they finish;
/// the next [`init_turso_database`] reopens whatever the active space now
/// resolves to.
pub(crate) async fn close_active_database() {
    let mut slot = DB_SLOT.lock().await;
    if let Some((path, _)) = slot.take() {
        info!(
            "Released the database handle for {}",
            sanitize_log_path(&path)
        );
    }
}

async fn open_and_prepare_database(path: &Path) -> Arc<Database> {
    let init_start = Instant::now();
    let path_str = path.to_string_lossy().to_string();
    info!("Ensuring Turso database {}", sanitize_log_path(path));

    sweep_stale_db_artifacts(path);

    let mut db = build_or_rebuild_after_failure(path, &path_str)
        .await
        .unwrap_or_else(|e| {
            log::error!("Failed to initialize Turso database: {}", e);
            panic!("Failed to initialize Turso database: {}", e);
        });

    // Inspect for free-page bloat and, if needed, rebuild a dense copy
    // (analysis4 P0). The file swap needs no open handle to the source, so
    // drop `db` first, compact (panic-safe - never crashes startup), reopen.
    if should_compact_database(&db).await {
        let compact_start = Instant::now();
        set_db_startup_compaction_active(true);
        drop(db);
        let installed = compact_database_file(path).await;
        db = build_or_rebuild_after_failure(path, &path_str)
            .await
            .unwrap_or_else(|e| {
                log::error!("Failed to reopen Turso database after compaction: {}", e);
                panic!("Failed to reopen Turso database after compaction: {}", e);
            });
        set_db_startup_compaction_active(false);
        if installed {
            info!(
                "STARTUP: database compaction complete in {:.2}s",
                compact_start.elapsed().as_secs_f64()
            );
        }
    }

    // Confirm once per open that the file is on single-writer WAL unless
    // MVCC was explicitly opted in.
    // An unexpected `mvcc` here is the signature of the force-redownload hang
    // (see `mvcc_enabled` rationale).
    match connect_tuned(&db).await {
        Ok(conn) => {
            let mode = read_journal_mode(&conn)
                .await
                .unwrap_or_else(|| "unknown".to_string());
            info!(
                "STARTUP: Turso journal_mode={} mvcc_enabled={} write_gate_permits={} var_limit={}",
                mode,
                mvcc_enabled(),
                crate::core::tasks::init_database::DB_WRITE_GATE.available_permits(),
                crate::core::tasks::init_database::sqlite_variable_limit(),
            );
        }
        Err(e) => warn!("STARTUP: could not read Turso journal_mode: {}", e),
    }

    info!(
        "STARTUP: Turso database initialized in {:.2}s",
        init_start.elapsed().as_secs_f64()
    );
    Arc::new(db)
}

/// Build a throwaway file-backed Turso database with the full bootstrap schema,
/// for tests that need a real engine behind a `FoxyContext`. The temp dir is
/// intentionally leaked so the database file stays valid for the duration of the
/// test (test-only; the OS reclaims it).
#[cfg(test)]
pub(crate) async fn build_test_database() -> Arc<Database> {
    let dir = tempfile::tempdir().expect("create temp dir for test database");
    let path = dir.path().join("database.db");
    let db = build_and_bootstrap(path.to_str().unwrap())
        .await
        .expect("bootstrap test database");
    std::mem::forget(dir);
    Arc::new(db)
}

/// Shared classifier for transient DB errors used by every retry loop.
pub(crate) fn db_error_message_is_retryable(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("conflict")
        || m.contains("busy")
        || m.contains("locked")
        || m.contains("no transaction is active")
}

/// Whether a Turso error should be retried by [`db_retry_transaction`].
///
/// Default-mode busy/locked → `Busy`/`BusySnapshot`. MVCC write–write conflicts
/// surface as `Error(msg)` containing `"conflict"`, and an aborted conflicting
/// txn can report `"no transaction is active"` at COMMIT (spike §11). All are
/// transient and safe to retry after a fresh `BEGIN`.
pub(crate) fn db_is_retryable(err: &Error) -> bool {
    match err {
        Error::Busy(_) | Error::BusySnapshot(_) => true,
        Error::Error(msg) => db_error_message_is_retryable(msg),
        _ => false,
    }
}

/// Exponential backoff identical in shape to the SQLite path
/// (`init_database::sqlite_lock_backoff`).
fn db_retry_backoff(attempt: usize) -> Duration {
    Duration::from_millis(50 * 2u64.saturating_pow(attempt as u32))
}

/// Run `work` inside a transaction on `conn`, retrying on Turso's transient
/// busy/conflict errors with exponential backoff. The `concurrent` flag selects
/// `BEGIN CONCURRENT` (MVCC) vs a plain `BEGIN`; under MVCC
/// the conflict is detected at the write or commit and the whole txn is retried.
///
/// Statements inside `work` run on the same `conn` (Turso ties transaction state
/// to the connection), matching the `concurrent_writes` example and the existing
/// `sqlite_retry_transaction` contract.
pub(crate) async fn db_retry_transaction<F>(
    conn: &Connection,
    label: &str,
    concurrent: bool,
    work: F,
) -> turso::Result<()>
where
    F: for<'a> Fn(&'a Connection) -> Pin<Box<dyn Future<Output = turso::Result<()>> + Send + 'a>>,
{
    let begin_sql = if concurrent {
        "BEGIN CONCURRENT"
    } else {
        "BEGIN"
    };
    let started = Instant::now();
    let mut attempt = 0;

    loop {
        let step: turso::Result<()> = async {
            conn.execute(begin_sql, ()).await?;
            work(conn).await?;
            conn.execute("COMMIT", ()).await?;
            Ok(())
        }
        .await;

        match step {
            Ok(()) => {
                let total = started.elapsed();
                let line = format!(
                    "DB transaction metrics: label={} committed=true attempts={} total={:.3}s",
                    label,
                    attempt + 1,
                    total.as_secs_f64()
                );
                if attempt > 0 || total >= Duration::from_millis(100) {
                    info!("{}", line);
                } else {
                    debug!("{}", line);
                }
                return Ok(());
            }
            Err(e) if attempt < DB_MAX_RETRIES && db_is_retryable(&e) => {
                // Best-effort rollback; a conflicting MVCC txn may already be
                // aborted, so ignore the rollback error.
                let _ = conn.execute("ROLLBACK", ()).await;
                let backoff = db_retry_backoff(attempt);
                debug!(
                    "{}: retryable DB error (attempt {}): {}",
                    label,
                    attempt + 1,
                    e
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                warn!(
                    "DB transaction metrics: label={} committed=false attempts={} total={:.3}s error={}",
                    label,
                    attempt + 1,
                    started.elapsed().as_secs_f64(),
                    e
                );
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let db = build_and_bootstrap(path.to_str().unwrap()).await.unwrap();
        (dir, db)
    }

    fn normalize_ddl(sql: &str) -> String {
        sql.lines()
            .map(|line| match line.find("--") {
                Some(idx) => &line[..idx],
                None => line,
            })
            .collect::<Vec<_>>()
            .join(" ")
            .replace('(', " ( ")
            .replace(')', " ) ")
            .replace(',', " , ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    }

    #[test]
    fn subfiles_ddl_matches_bootstrap_schema() {
        let statements: Vec<String> = TURSO_BOOTSTRAP_SCHEMA
            .lines()
            .map(|line| match line.find("--") {
                Some(idx) => &line[..idx],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .split(';')
            .map(normalize_ddl)
            .filter(|statement| !statement.is_empty())
            .collect();

        let schema_table = statements
            .iter()
            .find(|statement| statement.starts_with("create table if not exists subfiles"))
            .expect("bootstrap schema declares the subfiles table");
        assert_eq!(
            *schema_table,
            normalize_ddl(SUBFILES_CREATE_TABLE),
            "SUBFILES_CREATE_TABLE drifted from sql/turso_schema.sql"
        );

        for index_sql in SUBFILES_INDEX_CREATE_SQL {
            let normalized = normalize_ddl(index_sql);
            assert!(
                statements.contains(&normalized),
                "subfiles index DDL drifted from sql/turso_schema.sql: {index_sql}"
            );
        }
    }

    #[test]
    fn retryable_classifier_matches_transient_errors_only() {
        assert!(db_error_message_is_retryable("database is locked"));
        assert!(db_error_message_is_retryable("Write-write conflict"));
        assert!(db_error_message_is_retryable("Busy: database busy"));
        assert!(db_error_message_is_retryable(
            "cannot commit - no transaction is active"
        ));
        assert!(!db_error_message_is_retryable(
            "UNIQUE constraint failed: repositories.remote_url"
        ));
        assert!(!db_error_message_is_retryable("no such table: files"));
        assert!(!db_error_message_is_retryable("disk I/O error"));
    }

    #[tokio::test]
    async fn rebuild_backup_moves_database_and_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        fs::write(&path, b"db").unwrap();
        fs::write(with_suffix(&path, "-wal"), b"wal").unwrap();
        fs::write(with_suffix(&path, "-shm"), b"shm").unwrap();

        let backup = move_db_artifacts_to_rebuild_backup(&path).await.unwrap();

        assert!(!path.exists());
        assert!(!with_suffix(&path, "-wal").exists());
        assert!(!with_suffix(&path, "-shm").exists());
        assert_eq!(fs::read(&backup).unwrap(), b"db");
        assert_eq!(fs::read(with_suffix(&backup, "-wal")).unwrap(), b"wal");
        assert_eq!(fs::read(with_suffix(&backup, "-shm")).unwrap(), b"shm");
    }

    #[tokio::test]
    async fn live_wipe_waits_for_shared_db_access() {
        let db = build_test_database().await;
        let shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let wipe_db = db.clone();

        let wipe = tokio::spawn(async move { wipe_and_rebuild_live(&wipe_db).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            !wipe.is_finished(),
            "live wipe should wait for active shared DB access"
        );

        drop(shared);
        tokio::time::timeout(Duration::from_secs(10), wipe)
            .await
            .expect("live wipe should finish after shared access is released")
            .expect("live wipe task should not panic")
            .expect("live wipe should succeed");
    }

    /// Benchmark (run explicitly: `cargo test --release bench_fresh_insert_vs_upsert
    /// -- --ignored --nocapture`). Settles analysis3.md bucket A: on a freshly
    /// purged `subfiles` table (the force-redownload case, where no row can
    /// conflict), is a plain `INSERT` materially faster than the production
    /// `INSERT … ON CONFLICT (file_id, path) DO UPDATE`, and where is the
    /// per-statement chunk-size knee? Inserts 66 336 rows (real TFR_40K part
    /// count) under single-writer WAL, one transaction, chunked - mirroring
    /// `remote_file_parts::batch::file_part_upsert_*`. Prints wall time for both
    /// statement shapes across several chunk sizes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_fresh_insert_vs_upsert() {
        const ROWS: usize = 66_336;
        const PARAMS_PER_ROW: usize = 6;

        // file_id=1 must exist (FK target for subfiles.file_id).
        async fn seed_file(db: &Database) -> Connection {
            let conn = connect_tuned(db).await.unwrap();
            conn.execute(
                "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
                (),
            )
            .await
            .unwrap();
            conn
        }

        fn rows() -> Vec<(i64, String, i64, i64, String, i64)> {
            (0..ROWS)
                .map(|i| {
                    (
                        1i64,
                        format!("p{i}"),
                        4096i64,
                        0i64,
                        format!("rc{i}"),
                        i as i64,
                    )
                })
                .collect()
        }

        // Build the chunk SQL for `n` rows in each shape.
        fn upsert_sql(n: usize) -> String {
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; n].join(", ");
            format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph} \
                 ON CONFLICT (file_id, path) DO UPDATE SET remote_length = excluded.remote_length, \
                 remote_start = excluded.remote_start, remote_checksum = excluded.remote_checksum, \
                 data_order = excluded.data_order"
            )
        }
        fn plain_sql(n: usize) -> String {
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; n].join(", ");
            format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            )
        }
        fn binds(chunk: &[(i64, String, i64, i64, String, i64)]) -> Vec<turso::Value> {
            let mut v = Vec::with_capacity(chunk.len() * PARAMS_PER_ROW);
            for (file_id, path, _ll, _ls, rc, ord) in chunk {
                // Matches the 6 bound params: file_id, path, remote_length,
                // remote_start, remote_checksum, data_order.
                v.push(turso::Value::Integer(*file_id));
                v.push(turso::Value::Text(path.clone()));
                v.push(turso::Value::Integer(4096));
                v.push(turso::Value::Integer(0));
                v.push(turso::Value::Text(rc.clone()));
                v.push(turso::Value::Integer(*ord));
            }
            v
        }

        for chunk_rows in [64usize, 128, 256, 512, 1_024] {
            for shape in ["plain", "upsert"] {
                let (_dir, db) = temp_db().await;
                let conn = seed_file(&db).await;
                let data = rows();
                let started = Instant::now();
                conn.execute("BEGIN", ()).await.unwrap();
                for chunk in data.chunks(chunk_rows) {
                    let sql = if shape == "plain" {
                        plain_sql(chunk.len())
                    } else {
                        upsert_sql(chunk.len())
                    };
                    conn.execute(&sql, binds(chunk)).await.unwrap();
                }
                conn.execute("COMMIT", ()).await.unwrap();
                let elapsed = started.elapsed().as_secs_f64();
                let stmts = ROWS.div_ceil(chunk_rows);
                println!(
                    "[bench fresh-insert] shape={shape:<6} chunk_rows={chunk_rows:<5} \
                     statements={stmts:<4} rows={ROWS} total={elapsed:.3}s \
                     per_row_us={:.1}",
                    elapsed * 1_000_000.0 / ROWS as f64
                );
            }
        }
    }

    /// Benchmark (run explicitly: `cargo test --release bench_bulk_update_chunk
    /// -- --ignored --nocapture`). Confirms the chunk-size knee also applies to
    /// the bulk part-hash persist `UPDATE … FROM (VALUES …)` shape (analysis3.md
    /// bucket A `persist bulk part hashes`), whose default packs ~8190 rows/stmt.
    /// Seeds 66 336 subfiles then re-updates their local checksums chunked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_bulk_update_chunk() {
        const ROWS: usize = 66_336;
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        conn.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
            (),
        )
        .await
        .unwrap();
        // Seed in small (fast) chunks.
        conn.execute("BEGIN", ()).await.unwrap();
        for chunk_start in (0..ROWS).step_by(256) {
            let chunk_end = (chunk_start + 256).min(ROWS);
            let ph = vec!["(1, ?, 0, 0, 4096, 0, '', ?, ?)"; chunk_end - chunk_start].join(", ");
            let sql = format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            );
            let mut binds: Vec<turso::Value> = Vec::new();
            for i in chunk_start..chunk_end {
                binds.push(turso::Value::Text(format!("p{i}")));
                binds.push(turso::Value::Text(format!("rc{i}")));
                binds.push(turso::Value::Integer(i as i64));
            }
            conn.execute(&sql, binds).await.unwrap();
        }
        conn.execute("COMMIT", ()).await.unwrap();

        // ids to update (sorted by PK, as the real persist path does).
        let mut ids = Vec::with_capacity(ROWS);
        let mut rows = conn
            .query("SELECT id FROM subfiles ORDER BY id", ())
            .await
            .unwrap();
        while let Some(r) = rows.next().await.unwrap() {
            ids.push(r.get::<i64>(0).unwrap());
        }

        for chunk_rows in [256usize, 1_024, 8_190] {
            let started = Instant::now();
            conn.execute("BEGIN", ()).await.unwrap();
            for chunk in ids.chunks(chunk_rows) {
                // UPDATE subfiles SET local_checksum/length/start FROM (VALUES …) - 4 binds/row.
                let vals = vec!["(?, ?, ?, ?)"; chunk.len()].join(", ");
                let sql = format!(
                    "WITH v(id, lc, ll, ls) AS (VALUES {vals}) \
                     UPDATE subfiles SET local_checksum = v.lc, local_length = v.ll, \
                     local_start = v.ls FROM v WHERE subfiles.id = v.id"
                );
                let mut binds: Vec<turso::Value> = Vec::with_capacity(chunk.len() * 4);
                for id in chunk {
                    binds.push(turso::Value::Integer(*id));
                    binds.push(turso::Value::Text(format!("lc{id}")));
                    binds.push(turso::Value::Integer(4096));
                    binds.push(turso::Value::Integer(0));
                }
                conn.execute(&sql, binds).await.unwrap();
            }
            conn.execute("COMMIT", ()).await.unwrap();
            let elapsed = started.elapsed().as_secs_f64();
            println!(
                "[bench bulk-update] chunk_rows={chunk_rows:<5} statements={:<4} rows={ROWS} \
                 total={elapsed:.3}s per_row_us={:.1}",
                ROWS.div_ceil(chunk_rows),
                elapsed * 1_000_000.0 / ROWS as f64
            );
        }
    }

    /// Benchmark (run explicitly: `cargo test --release bench_mvcc_write_degradation
    /// -- --ignored --nocapture`). Reproduces the production TFR_40K pattern: a
    /// populated `subfiles` table re-upserted by a long series of independent write
    /// transactions, each on its own fresh connection (matching the seam). Prints
    /// per-batch timings for MVCC on vs off so the O(N²) version-store growth is
    /// visible directly rather than only at 24 GB scale.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_mvcc_write_degradation() {
        const ROWS: usize = 40_000;
        const BATCHES: usize = 40;
        const BATCH_ROWS: usize = ROWS / BATCHES;

        for mvcc in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bench.db");
            let path_str = path.to_str().unwrap().to_string();
            let db = Builder::new_local(&path_str).build().await.unwrap();

            // One tuned-equivalent connection just to bootstrap + seed.
            let seed = db.connect().unwrap();
            seed.pragma_update("foreign_keys", "ON").await.unwrap();
            seed.pragma_update("synchronous", "NORMAL").await.unwrap();
            if mvcc {
                seed.pragma_update("journal_mode", "mvcc").await.unwrap();
            }
            apply_schema(&seed, TURSO_BOOTSTRAP_SCHEMA).await.unwrap();
            seed.execute(
                "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
                (),
            )
            .await
            .unwrap();
            for chunk_start in (0..ROWS).step_by(2_000) {
                let chunk_end = (chunk_start + 2_000).min(ROWS);
                let rows: Vec<String> = (chunk_start..chunk_end)
                    .map(|i| format!("(1, 'p{i}', 0, 0, 1, 0, '', 'r{i}', {i})"))
                    .collect();
                let sql = format!(
                    "INSERT INTO subfiles (file_id, path, local_length, local_start, \
                     remote_length, remote_start, local_checksum, remote_checksum, data_order) \
                     VALUES {}",
                    rows.join(", ")
                );
                seed.execute(&sql, ()).await.unwrap();
            }
            drop(seed);

            // Re-upsert the whole table across BATCHES independent transactions,
            // each on a fresh connection (as the seam does). Time each batch.
            let mut timings = Vec::with_capacity(BATCHES);
            let begin_sql = if mvcc { "BEGIN CONCURRENT" } else { "BEGIN" };
            for b in 0..BATCHES {
                let lo = b * BATCH_ROWS;
                let hi = lo + BATCH_ROWS;
                let rows: Vec<String> = (lo..hi)
                    .map(|i| format!("(1, 'p{i}', 0, 0, 1, 0, '', 'r{i}v2', {i})"))
                    .collect();
                let sql = format!(
                    "INSERT INTO subfiles (file_id, path, local_length, local_start, \
                     remote_length, remote_start, local_checksum, remote_checksum, data_order) \
                     VALUES {} ON CONFLICT (file_id, path) DO UPDATE SET \
                     remote_checksum = excluded.remote_checksum",
                    rows.join(", ")
                );
                let conn = db.connect().unwrap();
                conn.pragma_update("foreign_keys", "ON").await.unwrap();
                conn.pragma_update("synchronous", "NORMAL").await.unwrap();
                if mvcc {
                    conn.pragma_update("journal_mode", "mvcc").await.unwrap();
                }
                let started = Instant::now();
                conn.execute(begin_sql, ()).await.unwrap();
                conn.execute(&sql, ()).await.unwrap();
                conn.execute("COMMIT", ()).await.unwrap();
                timings.push(started.elapsed().as_secs_f64());
            }

            let total: f64 = timings.iter().sum();
            let first5: f64 = timings.iter().take(5).sum::<f64>() / 5.0;
            let last5: f64 = timings.iter().rev().take(5).sum::<f64>() / 5.0;
            println!(
                "[bench mvcc={mvcc}] rows={ROWS} batches={BATCHES} total={total:.3}s \
                 first5_avg={first5:.4}s last5_avg={last5:.4}s growth={:.1}x",
                if first5 > 0.0 { last5 / first5 } else { 0.0 }
            );
            let sample: Vec<String> = timings.iter().map(|t| format!("{t:.3}")).collect();
            println!("[bench mvcc={mvcc}] per-batch: {}", sample.join(" "));
        }
    }

    /// Benchmark (run explicitly: `cargo test --release bench_mvcc_concurrent_writers
    /// -- --ignored --nocapture`). Validates that the non-MVCC single-writer WAL path
    /// handles the production metadata-rebuild fan-out (many concurrent upsert
    /// transactions, each on its own connection) via busy_timeout + retry without a
    /// lock storm. Reports total wall time and failures for MVCC on vs off.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_mvcc_concurrent_writers() {
        const WRITERS: usize = 16;
        const ROWS_PER_WRITER: usize = 2_000;

        for mvcc in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bench.db");
            let path_str = path.to_str().unwrap().to_string();
            let db = Arc::new(Builder::new_local(&path_str).build().await.unwrap());

            let seed = db.connect().unwrap();
            seed.pragma_update("synchronous", "NORMAL").await.unwrap();
            if mvcc {
                seed.pragma_update("journal_mode", "mvcc").await.unwrap();
            }
            apply_schema(&seed, TURSO_BOOTSTRAP_SCHEMA).await.unwrap();
            seed.execute(
                "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
                (),
            )
            .await
            .unwrap();
            let total = WRITERS * ROWS_PER_WRITER;
            for chunk_start in (0..total).step_by(2_000) {
                let chunk_end = (chunk_start + 2_000).min(total);
                let rows: Vec<String> = (chunk_start..chunk_end)
                    .map(|i| format!("(1, 'p{i}', 0, 0, 1, 0, '', 'r{i}', {i})"))
                    .collect();
                let sql = format!(
                    "INSERT INTO subfiles (file_id, path, local_length, local_start, \
                     remote_length, remote_start, local_checksum, remote_checksum, data_order) \
                     VALUES {}",
                    rows.join(", ")
                );
                seed.execute(&sql, ()).await.unwrap();
            }
            drop(seed);

            let started = Instant::now();
            let mut handles = Vec::with_capacity(WRITERS);
            for w in 0..WRITERS {
                let db = db.clone();
                handles.push(tokio::spawn(async move {
                    let lo = w * ROWS_PER_WRITER;
                    let hi = lo + ROWS_PER_WRITER;
                    let rows: Vec<String> = (lo..hi)
                        .map(|i| format!("(1, 'p{i}', 0, 0, 1, 0, '', 'r{i}v2', {i})"))
                        .collect();
                    let sql = format!(
                        "INSERT INTO subfiles (file_id, path, local_length, local_start, \
                         remote_length, remote_start, local_checksum, remote_checksum, data_order) \
                         VALUES {} ON CONFLICT (file_id, path) DO UPDATE SET \
                         remote_checksum = excluded.remote_checksum",
                        rows.join(", ")
                    );
                    let conn = db.connect().unwrap();
                    conn.pragma_update("synchronous", "NORMAL").await.unwrap();
                    if mvcc {
                        conn.pragma_update("journal_mode", "mvcc").await.unwrap();
                    }
                    conn.busy_timeout(DB_BUSY_TIMEOUT).unwrap();
                    let begin_sql = if mvcc { "BEGIN CONCURRENT" } else { "BEGIN" };
                    // Mirror the seam's 5-retry loop so busy/conflict is handled.
                    let mut attempt = 0;
                    loop {
                        let step: turso::Result<()> = async {
                            conn.execute(begin_sql, ()).await?;
                            conn.execute(&sql, ()).await?;
                            conn.execute("COMMIT", ()).await?;
                            Ok(())
                        }
                        .await;
                        match step {
                            Ok(()) => return (true, attempt),
                            Err(e) if attempt < 5 && db_is_retryable(&e) => {
                                let _ = conn.execute("ROLLBACK", ()).await;
                                tokio::time::sleep(db_retry_backoff(attempt)).await;
                                attempt += 1;
                            }
                            Err(_) => return (false, attempt),
                        }
                    }
                }));
            }
            let mut ok = 0usize;
            let mut total_retries = 0usize;
            for h in handles {
                let (success, retries) = h.await.unwrap();
                if success {
                    ok += 1;
                }
                total_retries += retries;
            }
            println!(
                "[bench-concurrent mvcc={mvcc}] writers={WRITERS} ok={ok} total_retries={total_retries} wall={:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
    }

    /// The bootstrap schema applies cleanly and creates every expected table.
    #[tokio::test]
    async fn bootstrap_creates_all_tables() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        let expected = [
            "repositories",
            "pending_updates",
            "addons",
            "files",
            "subfiles",
            "repository_addons",
            "addon_files",
            "download_target_file",
            "download_target_file_part",
            "download_patch_file",
            "download_patch_op",
        ];
        for table in expected {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                )
                .await
                .unwrap();
            let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            assert_eq!(n, 1, "table {table} should exist after bootstrap");
        }
        // file_subfiles was dropped by migration 20 - must NOT exist.
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='file_subfiles'",
                (),
            )
            .await
            .unwrap();
        let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(n, 0, "file_subfiles must be absent (migration 20)");
    }

    /// Tuned connections enforce FK cascades from the bootstrap schema.
    #[tokio::test]
    async fn cascade_chain_from_bootstrap_schema() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        conn.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (1, 'r', 'u', 'p')",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required) VALUES (1, 'a', 'rp', 'lp', 1)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 1)",
            (),
        )
        .await
        .unwrap();
        conn.execute("DELETE FROM repositories WHERE id = 1", ())
            .await
            .unwrap();
        let mut rows = conn
            .query("SELECT COUNT(*) FROM repository_addons", ())
            .await
            .unwrap();
        let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(
            n, 0,
            "repository_addons rows must cascade-delete with the repository"
        );
    }

    /// P0-b (after_turso_regression_analysis5.md): the deferred-index bulk load
    /// drops the subfiles indexes, plain-INSERTs into the bare table, then rebuilds
    /// the indexes once. Verify the cycle is sound: a plain bulk load succeeds with
    /// indexes dropped, and after the rebuild the unique index is enforced again.
    #[tokio::test]
    async fn deferred_index_bulk_load_rebuilds_enforced_unique_index() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        conn.execute(
            "INSERT INTO files (id, name, remote_path, local_path, length, data_order) \
             VALUES (1, 'f', 'rp', 'lp', 1, 0)",
            (),
        )
        .await
        .unwrap();

        // Drop indexes (mirrors entry.rs::drop_subfiles_indexes).
        for name in SUBFILES_INDEX_NAMES {
            conn.execute(&format!("DROP INDEX IF EXISTS {name}"), ())
                .await
                .unwrap();
        }

        // Plain conflict-free bulk INSERT into the bare table.
        for i in 0..500i64 {
            conn.execute(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) \
                 VALUES (1, ?1, 0, 0, 1, 0, '', '', ?2)",
                (format!("p{i}"), i),
            )
            .await
            .unwrap();
        }

        // Rebuild indexes once (mirrors entry.rs::rebuild_subfiles_indexes).
        for sql in SUBFILES_INDEX_CREATE_SQL {
            conn.execute(sql, ()).await.unwrap();
        }

        let mut rows = conn
            .query("SELECT COUNT(*) FROM subfiles", ())
            .await
            .unwrap();
        let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(n, 500, "all bulk-loaded rows must be present");

        // The rebuilt unique index must now reject a duplicate (file_id, path).
        let dup = conn
            .execute(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) \
                 VALUES (1, 'p0', 0, 0, 1, 0, '', '', 999)",
                (),
            )
            .await;
        assert!(
            dup.is_err(),
            "rebuilt idx_subfiles_file_id_path must enforce (file_id, path) uniqueness"
        );
    }

    /// The retry wrapper commits a successful transaction.
    #[tokio::test]
    async fn retry_transaction_commits() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        db_retry_transaction(&conn, "test insert", false, |c| {
            Box::pin(async move {
                c.execute(
                    "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (2, 'n', 'u2', 'p2')",
                    (),
                )
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let mut rows = conn
            .query("SELECT name FROM repositories WHERE id = 2", ())
            .await
            .unwrap();
        let name = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(name, "n");
    }

    /// A non-retryable failure inside the txn rolls back (no partial write).
    #[tokio::test]
    async fn retry_transaction_rolls_back_on_constraint() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        let result = db_retry_transaction(&conn, "bad insert", false, |c| {
            Box::pin(async move {
                c.execute(
                    "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (3, 'ok', 'u3', 'p3')",
                    (),
                )
                .await?;
                // Duplicate (remote_url, local_path) violates the UNIQUE constraint.
                c.execute(
                    "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (4, 'dup', 'u3', 'p3')",
                    (),
                )
                .await?;
                Ok(())
            })
        })
        .await;
        assert!(
            result.is_err(),
            "constraint violation should surface as an error"
        );
        let mut rows = conn
            .query("SELECT COUNT(*) FROM repositories WHERE id IN (3,4)", ())
            .await
            .unwrap();
        let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(n, 0, "both inserts must roll back together");
    }

    #[test]
    fn retryable_classification() {
        assert!(db_is_retryable(&Error::Busy("x".into())));
        assert!(db_is_retryable(&Error::BusySnapshot("x".into())));
        assert!(db_is_retryable(&Error::Error(
            "write-write conflict".into()
        )));
        assert!(db_is_retryable(&Error::Error(
            "cannot commit - no transaction is active".into()
        )));
        assert!(!db_is_retryable(&Error::Error("syntax error".into())));
        assert!(!db_is_retryable(&Error::Constraint("unique".into())));
    }

    /// Diagnostic: inspect a copy of the production database for bloat (free
    /// pages) vs real rows. Set FOXY_INSPECT_DB to the db path. Run:
    /// `cargo test -p Foxy inspect_db -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn inspect_db() {
        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let db = Builder::new_local(&path).build().await.expect("open db");
        let conn = db.connect().expect("connect");
        for sql in [
            "PRAGMA page_count",
            "PRAGMA page_size",
            "PRAGMA freelist_count",
            "PRAGMA auto_vacuum",
            "PRAGMA journal_mode",
        ] {
            match conn.query(sql, ()).await {
                Ok(mut rows) => {
                    let v = rows
                        .next()
                        .await
                        .ok()
                        .flatten()
                        .map(|r| r.get_value(0).ok());
                    eprintln!("{sql:<24} = {v:?}");
                }
                Err(e) => eprintln!("{sql:<24} ERR {e}"),
            }
        }
        for t in [
            "repositories",
            "addons",
            "files",
            "subfiles",
            "repository_addons",
            "addon_files",
            "download_target_file",
            "download_target_file_part",
            "download_patch_file",
            "download_patch_op",
            "pending_updates",
        ] {
            let sql = format!("SELECT COUNT(*) FROM {t}");
            match conn.query(&sql, ()).await {
                Ok(mut rows) => {
                    let v = rows
                        .next()
                        .await
                        .ok()
                        .flatten()
                        .map(|r| r.get_value(0).ok());
                    eprintln!("count {t:<28} = {v:?}");
                }
                Err(e) => eprintln!("count {t:<28} ERR {e}"),
            }
        }
    }

    /// Diagnostic: dump the repository/addon link graph of a database copy:
    /// which repositories exist, how many `repository_addons` links each has,
    /// and whether any links or addons dangle. Set FOXY_INSPECT_DB to the db
    /// path (copy only). Run:
    /// `cargo test -p Foxy inspect_repo_links -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn inspect_repo_links() {
        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let db = Builder::new_local(&path).build().await.expect("open db");
        let conn = db.connect().expect("connect");

        let mut rows = conn
            .query(
                "SELECT id, name, remote_url, local_path FROM repositories ORDER BY id",
                (),
            )
            .await
            .expect("repositories");
        while let Ok(Some(row)) = rows.next().await {
            eprintln!(
                "repo id={:?} name={:?} url={:?} local={:?}",
                row.get_value(0).ok(),
                row.get_value(1).ok(),
                row.get_value(2).ok(),
                row.get_value(3).ok()
            );
        }

        let mut rows = conn
            .query(
                "SELECT repository_id, COUNT(*) FROM repository_addons GROUP BY repository_id",
                (),
            )
            .await
            .expect("link counts");
        while let Ok(Some(row)) = rows.next().await {
            eprintln!(
                "links repository_id={:?} count={:?}",
                row.get_value(0).ok(),
                row.get_value(1).ok()
            );
        }

        for (label, sql) in [
            (
                "dangling links (addon gone)",
                "SELECT COUNT(*) FROM repository_addons ra LEFT JOIN addons a ON a.id = ra.addon_id WHERE a.id IS NULL",
            ),
            (
                "dangling links (repo gone)",
                "SELECT COUNT(*) FROM repository_addons ra LEFT JOIN repositories r ON r.id = ra.repository_id WHERE r.id IS NULL",
            ),
            (
                "unlinked addons",
                "SELECT COUNT(*) FROM addons a LEFT JOIN repository_addons ra ON ra.addon_id = a.id WHERE ra.addon_id IS NULL",
            ),
            ("addons total", "SELECT COUNT(*) FROM addons"),
            ("subfiles total", "SELECT COUNT(*) FROM subfiles"),
            (
                "subfiles with local checksum",
                "SELECT COUNT(*) FROM subfiles WHERE local_checksum != ''",
            ),
            (
                "files with local checksum",
                "SELECT COUNT(*) FROM files WHERE local_checksum != ''",
            ),
            ("files total", "SELECT COUNT(*) FROM files"),
        ] {
            match conn.query(sql, ()).await {
                Ok(mut rows) => {
                    let v = rows
                        .next()
                        .await
                        .ok()
                        .flatten()
                        .map(|r| r.get_value(0).ok());
                    eprintln!("{label:<32} = {v:?}");
                }
                Err(e) => eprintln!("{label:<32} ERR {e}"),
            }
        }

        let mut rows = conn
            .query(
                "SELECT a.local_path, COUNT(*) FROM addons a \
                 LEFT JOIN repository_addons ra ON ra.addon_id = a.id \
                 WHERE ra.addon_id IS NULL GROUP BY a.local_path LIMIT 20",
                (),
            )
            .await
            .expect("unlinked addon paths");
        while let Ok(Some(row)) = rows.next().await {
            eprintln!(
                "unlinked addon local_path={:?} count={:?}",
                row.get_value(0).ok(),
                row.get_value(1).ok()
            );
        }

        // Id churn: max id far above the row count means rows are being
        // deleted and recreated across rechecks instead of upserted in place.
        for t in ["addons", "files", "subfiles", "repositories"] {
            let sql = format!("SELECT COUNT(*), MIN(id), MAX(id) FROM {t}");
            match conn.query(&sql, ()).await {
                Ok(mut rows) => {
                    if let Ok(Some(row)) = rows.next().await {
                        eprintln!(
                            "ids {t:<14} count={:?} min={:?} max={:?}",
                            row.get_value(0).ok(),
                            row.get_value(1).ok(),
                            row.get_value(2).ok()
                        );
                    }
                }
                Err(e) => eprintln!("ids {t:<14} ERR {e}"),
            }
        }

        let mut rows = conn
            .query(
                "SELECT substr(local_path, 1, 3), COUNT(*), \
                 SUM(CASE WHEN local_checksum = '' THEN 1 ELSE 0 END) \
                 FROM addons GROUP BY substr(local_path, 1, 3)",
                (),
            )
            .await
            .expect("addon path roots");
        while let Ok(Some(row)) = rows.next().await {
            eprintln!(
                "addon root={:?} count={:?} empty_local_checksum={:?}",
                row.get_value(0).ok(),
                row.get_value(1).ok(),
                row.get_value(2).ok()
            );
        }

        // Which repo owns which id range: reveals creation order and whether a
        // repo's graph was recreated after the other repo's.
        for (label, sql) in [
            (
                "addon id ranges",
                "SELECT substr(local_path, 1, 3), MIN(id), MAX(id) FROM addons GROUP BY substr(local_path, 1, 3)",
            ),
            (
                "file id ranges",
                "SELECT substr(local_path, 1, 3), MIN(id), MAX(id) FROM files GROUP BY substr(local_path, 1, 3)",
            ),
            (
                "subfile id ranges",
                "SELECT substr(f.local_path, 1, 3), MIN(sf.id), MAX(sf.id), COUNT(*) FROM subfiles sf JOIN files f ON f.id = sf.file_id GROUP BY substr(f.local_path, 1, 3)",
            ),
        ] {
            match conn.query(sql, ()).await {
                Ok(mut rows) => {
                    while let Ok(Some(row)) = rows.next().await {
                        eprintln!(
                            "{label}: root={:?} min={:?} max={:?} extra={:?}",
                            row.get_value(0).ok(),
                            row.get_value(1).ok(),
                            row.get_value(2).ok(),
                            row.get_value(3).ok()
                        );
                    }
                }
                Err(e) => eprintln!("{label} ERR {e}"),
            }
        }
    }

    /// Probe: does Turso 0.6.1 support `VACUUM INTO 'file'` (compacted copy to a
    /// new file, which does NOT need the experimental in-place vacuum flag)? If
    /// yes, startup compaction is a one-liner + file swap. Bloats a fresh db, then
    /// tries VACUUM INTO. Run:
    /// `cargo test -p Foxy probe_vacuum_into -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn probe_vacuum_into() {
        let (dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        // Make some bloat: insert a file + many subfiles, then delete them.
        conn.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1,'f','rp','lp')",
            (),
        )
        .await
        .unwrap();
        conn.execute("BEGIN", ()).await.unwrap();
        for i in 0..20_000 {
            conn.execute(
                &format!("INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) VALUES (1, 'p{i}', 0, 0, 4096, 0, '', 'rc{i}', {i})"),
                (),
            )
            .await
            .ok();
        }
        conn.execute("COMMIT", ()).await.unwrap();
        conn.execute("DELETE FROM subfiles", ()).await.unwrap();

        async fn pc(conn: &Connection, sql: &str) -> Option<turso::Value> {
            let mut rows = conn.query(sql, ()).await.ok()?;
            rows.next()
                .await
                .ok()
                .flatten()
                .and_then(|r| r.get_value(0).ok())
        }
        eprintln!(
            "before: page_count={:?} freelist={:?}",
            pc(&conn, "PRAGMA page_count").await,
            pc(&conn, "PRAGMA freelist_count").await
        );

        let into = dir.path().join("compacted.db");
        let sql = format!(
            "VACUUM INTO '{}'",
            into.to_string_lossy().replace('\\', "/")
        );
        match conn.execute(&sql, ()).await {
            Ok(_) => {
                eprintln!("VACUUM INTO OK -> {}", into.display());
                let db2 = Builder::new_local(into.to_str().unwrap())
                    .build()
                    .await
                    .unwrap();
                let c2 = db2.connect().unwrap();
                eprintln!(
                    "after:  page_count={:?} freelist={:?}",
                    pc(&c2, "PRAGMA page_count").await,
                    pc(&c2, "PRAGMA freelist_count").await
                );
            }
            Err(e) => eprintln!("VACUUM INTO ERR: {e}"),
        }
    }

    /// The shipped compaction path (manual SELECT/INSERT rebuild + file swap)
    /// must preserve every live row and actually shrink the free list. Seeds a
    /// file, bloats it with deletes, runs `compact_database_file`, then reopens
    /// and verifies the swapped-in file has the same rows and far fewer free
    /// pages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn compaction_rebuilds_dense_and_preserves_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let path_str = path.to_string_lossy().to_string();

        async fn count(conn: &Connection, table: &str) -> i64 {
            let mut rows = conn
                .query(&format!("SELECT COUNT(*) FROM {table}"), ())
                .await
                .unwrap();
            match rows.next().await.unwrap().unwrap().get_value(0).unwrap() {
                turso::Value::Integer(i) => i,
                _ => -1,
            }
        }

        // Seed: 1 repo, 1 addon, 1 file, link rows, and 6000 subfiles; then delete
        // 5000 subfiles to strand free pages.
        {
            let db = build_and_bootstrap(&path_str).await.unwrap();
            let conn = connect_tuned(&db).await.unwrap();
            conn.execute("INSERT INTO repositories (id,name,remote_url,local_path,image,local_checksum,remote_checksum,local_content_hash,foxy_mode) VALUES (1,'r','u','p','','','','','')", ()).await.unwrap();
            conn.execute(
                "INSERT INTO files (id,name,remote_path,local_path) VALUES (1,'f','rp','lp')",
                (),
            )
            .await
            .unwrap();
            conn.execute("BEGIN", ()).await.unwrap();
            for i in 0..6000 {
                conn.execute(
                    &format!("INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) VALUES (1, 'p{i}', 0, 0, 4096, 0, '', 'rc{i}', {i})"),
                    (),
                )
                .await
                .unwrap();
            }
            conn.execute("COMMIT", ()).await.unwrap();
            conn.execute("DELETE FROM subfiles WHERE data_order >= 1000", ())
                .await
                .unwrap();
            let free = read_pragma_i64(&conn, "freelist_count").await.unwrap_or(0);
            assert!(
                free > 20,
                "expected bloat before compaction, got {free} free pages"
            );
            drop(conn);
            drop(db);
        }

        // End-to-end through the panic-safe wrapper (rebuild copy + swap).
        assert!(
            compact_database_file(&path).await,
            "compaction should install the dense copy"
        );

        let db = build_and_bootstrap(&path_str).await.unwrap();
        let conn = connect_tuned(&db).await.unwrap();
        assert_eq!(
            count(&conn, "subfiles").await,
            1000,
            "subfile rows preserved"
        );
        assert_eq!(count(&conn, "files").await, 1, "file rows preserved");
        assert_eq!(count(&conn, "repositories").await, 1, "repo rows preserved");
        let free_after = read_pragma_i64(&conn, "freelist_count").await.unwrap_or(-1);
        assert!(
            free_after < 50,
            "expected dense file, got {free_after} free pages"
        );
        // The original is retained as a backup.
        assert!(with_suffix(&path, ".bak").exists(), "original kept as .bak");
    }

    /// Diagnostic: probe which repair ops Turso supports on a bloated db copy
    /// (journal_mode switch off mvcc, VACUUM, incremental_vacuum). Operates on
    /// the FOXY_INSPECT_DB copy (mutates it - copy only). Run:
    /// `cargo test -p Foxy repair_db_probe -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn repair_db_probe() {
        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let db = Builder::new_local(&path).build().await.expect("open db");
        let conn = db.connect().expect("connect");

        async fn run(conn: &Connection, sql: &str) {
            let t = Instant::now();
            match conn.query(sql, ()).await {
                Ok(mut rows) => {
                    let v = rows
                        .next()
                        .await
                        .ok()
                        .flatten()
                        .map(|r| r.get_value(0).ok());
                    eprintln!("OK  {:>7.2}s  {sql} -> {v:?}", t.elapsed().as_secs_f64());
                }
                Err(e) => eprintln!("ERR {:>7.2}s  {sql} -> {e}", t.elapsed().as_secs_f64()),
            }
        }

        run(&conn, "PRAGMA journal_mode").await;
        run(&conn, "PRAGMA journal_mode=wal").await;
        run(&conn, "PRAGMA journal_mode=delete").await;
        run(&conn, "PRAGMA journal_mode").await;
        run(&conn, "PRAGMA page_count").await;
        run(&conn, "PRAGMA freelist_count").await;
        run(&conn, "VACUUM").await;
        run(&conn, "PRAGMA page_count").await;
        run(&conn, "PRAGMA freelist_count").await;
    }

    /// Decisive: run the real purge transaction on a copy of the bloated
    /// production DB, after migrating it off mvcc to WAL. If this is ~tens of
    /// seconds (vs the multi-minute production hang), switching journal mode off
    /// mvcc fixes the hang in-place with no data loss. Set FOXY_INSPECT_DB to a
    /// throwaway copy (mutated). `MODE=mvcc` env keeps mvcc as a control.
    /// Run: `cargo test -p Foxy bench_purge_on_real_copy -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn bench_purge_on_real_copy() {
        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let mode = std::env::var("MODE").unwrap_or_else(|_| "wal".into());
        let db = Builder::new_local(&path).build().await.expect("open db");
        let conn = connect_tuned(&db).await.expect("connect");
        conn.pragma_update("journal_mode", &mode)
            .await
            .expect("set journal_mode");
        let jm = {
            let mut rows = conn.query("PRAGMA journal_mode", ()).await.unwrap();
            rows.next()
                .await
                .ok()
                .flatten()
                .map(|r| r.get_value(0).ok())
        };
        eprintln!("[purge-copy] journal_mode={jm:?}");

        let step = |label: &'static str, sql: &'static str| {
            let conn = &conn;
            async move {
                let t = Instant::now();
                conn.execute(sql, ())
                    .await
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                eprintln!(
                    "[purge-copy]   {label:<28} {:.3}s",
                    t.elapsed().as_secs_f64()
                );
            }
        };

        let txn = Instant::now();
        conn.execute("BEGIN", ()).await.unwrap();
        step(
            "create repo_ids",
            "CREATE TEMP TABLE temp.fp_repo (id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "create orphan_addon_ids",
            "CREATE TEMP TABLE temp.fp_oaddon (addon_id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "create orphan_file_ids",
            "CREATE TEMP TABLE temp.fp_ofile (file_id INTEGER PRIMARY KEY)",
        )
        .await;
        step(
            "insert repo_ids",
            "INSERT OR IGNORE INTO temp.fp_repo SELECT id FROM repositories",
        )
        .await;
        step("insert orphan_addon_ids", "INSERT OR IGNORE INTO temp.fp_oaddon SELECT addon_id FROM repository_addons WHERE repository_id IN (SELECT id FROM temp.fp_repo)").await;
        step("insert orphan_file_ids", "INSERT OR IGNORE INTO temp.fp_ofile SELECT file_id FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)").await;
        step(
            "delete repositories",
            "DELETE FROM repositories WHERE id IN (SELECT id FROM temp.fp_repo)",
        )
        .await;
        step("delete dtfp (nested)", "DELETE FROM download_target_file_part WHERE subfile_id IN (SELECT id FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile))").await;
        step(
            "delete subfiles",
            "DELETE FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete download_patch_op",
            "DELETE FROM download_patch_op WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete download_patch_file",
            "DELETE FROM download_patch_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete download_target_file",
            "DELETE FROM download_target_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete addon_files",
            "DELETE FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)",
        )
        .await;
        step(
            "delete addons",
            "DELETE FROM addons WHERE id IN (SELECT addon_id FROM temp.fp_oaddon)",
        )
        .await;
        step(
            "delete files",
            "DELETE FROM files WHERE id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        let commit = Instant::now();
        conn.execute("COMMIT", ()).await.unwrap();
        eprintln!(
            "[purge-copy]   {:<28} {:.3}s",
            "COMMIT",
            commit.elapsed().as_secs_f64()
        );
        eprintln!(
            "[purge-copy] TOTAL {:.3}s (mode={mode})",
            txn.elapsed().as_secs_f64()
        );
    }

    /// Isolate the REAL production wedge with ZERO concurrency: a single
    /// connection running the SCOPED purge of ONE repo (leaving sibling repos'
    /// rows in place), toggling `foreign_keys`. The whole-DB `bench_purge_on_real_copy`
    /// never wedges because it deletes every child row first, so its FK checks are
    /// trivial. Production deletes one repo among many, so `delete addons`/`delete
    /// files` must FK-verify against the huge surviving sibling tables (~400k
    /// `subfiles`) per deleted row. Run FK=ON (default) vs FK=OFF:
    ///   FOXY_INSPECT_DB=<copy> REPO_LIKE=TFR_40K FK=ON  cargo test -p Foxy diag_scoped_purge_fk -- --ignored --nocapture
    ///   FOXY_INSPECT_DB=<copy> REPO_LIKE=TFR_40K FK=OFF cargo test -p Foxy diag_scoped_purge_fk -- --ignored --nocapture
    /// If ON wedges/crawls and OFF is fast, FK enforcement is the hang.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn diag_scoped_purge_fk() {
        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let repo_like = std::env::var("REPO_LIKE").unwrap_or_else(|_| "TFR_40K".into());
        let fk = std::env::var("FK").unwrap_or_else(|_| "ON".into());
        let db = Builder::new_local(&path).build().await.expect("open db");
        let conn = connect_tuned(&db).await.expect("connect");
        conn.pragma_update("journal_mode", "wal")
            .await
            .expect("wal");
        // foreign_keys MUST be set outside any transaction (ignored inside BEGIN).
        conn.pragma_update("foreign_keys", fk.as_str())
            .await
            .expect("fk");
        eprintln!("[diag-fk] repo_like={repo_like} foreign_keys={fk}");

        // Build the scoped id sets in autocommit first (cheap, not the subject).
        for sql in [
            "CREATE TEMP TABLE temp.fp_repo (id INTEGER PRIMARY KEY)",
            "CREATE TEMP TABLE temp.fp_oaddon (addon_id INTEGER PRIMARY KEY)",
            "CREATE TEMP TABLE temp.fp_ofile (file_id INTEGER PRIMARY KEY)",
        ] {
            conn.execute(sql, ()).await.unwrap();
        }
        conn.execute(
            "INSERT OR IGNORE INTO temp.fp_repo SELECT id FROM repositories WHERE remote_url LIKE ?",
            (format!("%{repo_like}%"),),
        )
        .await
        .unwrap();
        conn.execute("INSERT OR IGNORE INTO temp.fp_oaddon SELECT addon_id FROM repository_addons WHERE repository_id IN (SELECT id FROM temp.fp_repo)", ()).await.unwrap();
        conn.execute("INSERT OR IGNORE INTO temp.fp_ofile SELECT file_id FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)", ()).await.unwrap();
        for t in ["temp.fp_repo", "temp.fp_oaddon", "temp.fp_ofile"] {
            let mut rows = conn
                .query(&format!("SELECT COUNT(*) FROM {t}"), ())
                .await
                .unwrap();
            let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
            eprintln!("[diag-fk] scoped {t:<16} = {n}");
        }

        let step = |label: &'static str, sql: &'static str| {
            let conn = &conn;
            async move {
                let t = Instant::now();
                conn.execute(sql, ())
                    .await
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                eprintln!("[diag-fk]   {label:<28} {:.3}s", t.elapsed().as_secs_f64());
            }
        };

        let txn = Instant::now();
        conn.execute("BEGIN", ()).await.unwrap();
        step(
            "delete repositories",
            "DELETE FROM repositories WHERE id IN (SELECT id FROM temp.fp_repo)",
        )
        .await;
        step("delete dtfp (nested)", "DELETE FROM download_target_file_part WHERE subfile_id IN (SELECT id FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile))").await;
        step(
            "delete subfiles",
            "DELETE FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete download_patch_op",
            "DELETE FROM download_patch_op WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete download_patch_file",
            "DELETE FROM download_patch_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete download_target_file",
            "DELETE FROM download_target_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        step(
            "delete addon_files",
            "DELETE FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)",
        )
        .await;
        step(
            "delete addons",
            "DELETE FROM addons WHERE id IN (SELECT addon_id FROM temp.fp_oaddon)",
        )
        .await;
        step(
            "delete files",
            "DELETE FROM files WHERE id IN (SELECT file_id FROM temp.fp_ofile)",
        )
        .await;
        let c = Instant::now();
        conn.execute("COMMIT", ()).await.unwrap();
        eprintln!(
            "[diag-fk]   COMMIT                       {:.3}s",
            c.elapsed().as_secs_f64()
        );
        eprintln!(
            "[diag-fk] TOTAL {:.3}s (fk={fk}) - NO WEDGE",
            txn.elapsed().as_secs_f64()
        );
    }

    /// Reproduce the production force-redownload hang AND validate the fix: the
    /// purge's big single write transaction runs while OTHER connections read the
    /// same `Arc<turso::Database>` from SEPARATE runtimes (exactly what the UI /
    /// background tasks do during the download). The single-connection
    /// `bench_purge_on_real_copy` always completes in WAL, so a wedge here pins the
    /// hang on cross-connection contention, not the SQL itself.
    ///
    /// Default routes the purge through the seam's exclusive transaction and the
    /// readers through `FoxyDb::query_all`, so the `DB_EXCLUSIVE` barrier makes
    /// readers yield and the purge completes - the test PASSES. Set
    /// `BYPASS_BARRIER=1` to issue raw ungated connections instead (no barrier):
    /// that wedges forever, demonstrating the original bug. Tune the concurrent
    /// reader count with `READERS` (default 2).
    /// Run: `FOXY_INSPECT_DB=<copy> cargo test -p Foxy repro_purge_wedge_under_concurrency -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn repro_purge_wedge_under_concurrency() {
        use crate::core::db::FoxyDb;
        use std::sync::atomic::{AtomicBool, Ordering};

        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let readers: usize = std::env::var("READERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);
        let bypass = std::env::var("BYPASS_BARRIER").is_ok();
        let db = Arc::new(Builder::new_local(&path).build().await.expect("open db"));
        // Migrate the copy off any persisted mvcc journal_mode to WAL (matches prod).
        {
            let conn = connect_tuned(&db).await.expect("connect");
            conn.pragma_update("journal_mode", "wal")
                .await
                .expect("wal");
        }
        eprintln!("[repro] readers={readers} bypass_barrier={bypass}");

        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = Vec::new();
        for _ in 0..readers {
            let db = db.clone();
            let stop = stop.clone();
            workers.push(std::thread::spawn(move || {
                // Each background reader gets its OWN tokio runtime and its OWN
                // connection from the shared Arc<Database> - the production shape.
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let foxy = FoxyDb::from_turso(db.clone());
                    let mut n: i64 = 0;
                    while !stop.load(Ordering::Relaxed) {
                        if bypass {
                            // Raw, ungated read on a fresh connection (pre-fix path).
                            if let Ok(conn) = connect_tuned(&db).await
                                && let Ok(mut rows) =
                                    conn.query("SELECT COUNT(*) FROM subfiles", ()).await
                            {
                                let _ = rows.next().await;
                            }
                        } else {
                            // Seam read - takes the shared barrier, yields to purge.
                            let _ = foxy
                                .query_all("SELECT COUNT(*) FROM subfiles", vec![])
                                .await;
                        }
                        n += 1;
                        tokio::time::sleep(Duration::from_millis(3)).await;
                    }
                    n
                })
            }));
        }

        const STEPS: &[(&str, &str)] = &[
            (
                "create repo_ids",
                "CREATE TEMP TABLE temp.fp_repo (id INTEGER PRIMARY KEY)",
            ),
            (
                "create orphan_addon_ids",
                "CREATE TEMP TABLE temp.fp_oaddon (addon_id INTEGER PRIMARY KEY)",
            ),
            (
                "create orphan_file_ids",
                "CREATE TEMP TABLE temp.fp_ofile (file_id INTEGER PRIMARY KEY)",
            ),
            // Scope to ONE repository (matching REPO_LIKE, default TFR_40K) so the
            // purge matches a production single-repo force-redownload (~66k
            // subfiles) and leaves sibling repos' rows in place.
            (
                "insert repo_ids",
                "INSERT OR IGNORE INTO temp.fp_repo SELECT id FROM repositories WHERE remote_url LIKE '%TFR_40K%'",
            ),
            (
                "insert orphan_addon_ids",
                "INSERT OR IGNORE INTO temp.fp_oaddon SELECT addon_id FROM repository_addons WHERE repository_id IN (SELECT id FROM temp.fp_repo)",
            ),
            (
                "insert orphan_file_ids",
                "INSERT OR IGNORE INTO temp.fp_ofile SELECT file_id FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)",
            ),
            (
                "delete repositories",
                "DELETE FROM repositories WHERE id IN (SELECT id FROM temp.fp_repo)",
            ),
            (
                "delete dtfp (nested)",
                "DELETE FROM download_target_file_part WHERE subfile_id IN (SELECT id FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile))",
            ),
            (
                "delete subfiles",
                "DELETE FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
            ),
            (
                "delete download_patch_op",
                "DELETE FROM download_patch_op WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
            ),
            (
                "delete download_patch_file",
                "DELETE FROM download_patch_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
            ),
            (
                "delete download_target_file",
                "DELETE FROM download_target_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)",
            ),
            (
                "delete addon_files",
                "DELETE FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)",
            ),
            (
                "delete addons",
                "DELETE FROM addons WHERE id IN (SELECT addon_id FROM temp.fp_oaddon)",
            ),
            (
                "delete files",
                "DELETE FROM files WHERE id IN (SELECT file_id FROM temp.fp_ofile)",
            ),
        ];

        let txn = Instant::now();
        if bypass {
            // Pre-fix path: raw connection, no barrier - wedges under readers.
            let conn = connect_tuned(&db).await.expect("purge connect");
            conn.execute("BEGIN", ()).await.unwrap();
            for (label, sql) in STEPS {
                let t = Instant::now();
                conn.execute(*sql, ())
                    .await
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                eprintln!("[repro]   {label:<28} {:.3}s", t.elapsed().as_secs_f64());
            }
            conn.execute("COMMIT", ()).await.unwrap();
        } else {
            // Fixed path: exclusive seam transaction quiesces the readers.
            let foxy = FoxyDb::from_turso(db.clone());
            foxy.transaction_exclusive("repro purge", |tx| {
                Box::pin(async move {
                    for (label, sql) in STEPS {
                        let t = Instant::now();
                        tx.execute(sql, vec![]).await?;
                        let label: &str = label;
                        eprintln!("[repro]   {label:<28} {:.3}s", t.elapsed().as_secs_f64());
                    }
                    Ok(())
                })
            })
            .await
            .expect("exclusive purge");
        }
        eprintln!(
            "[repro] TOTAL {:.3}s (readers={readers} bypass={bypass}) - NO WEDGE",
            txn.elapsed().as_secs_f64()
        );

        stop.store(true, Ordering::Relaxed);
        for w in workers {
            let iters = w.join().unwrap();
            eprintln!("[repro] background reader did {iters} iterations");
        }
    }
}
