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
//! - `connect()` builds a fresh pager, reads page 1 and clones the schema, and
//!   its statement cache dies with it → tuned connections are pooled per
//!   database and hand out cached programs (see `connect_pooled`).
//! - No statement interrupt / query timeout on `Connection` in 0.7.2 (unlike the
//!   Python SDK) → cancellation stays cooperative at the task level.
#![allow(dead_code)] // A few helpers (db_retry_transaction, etc.) are exercised only by tests.

use std::fs;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::FutureExt;
use log::{debug, info, warn};
use turso::{Builder, Connection, Database, Error, Statement};

use crate::core::utils::format::sanitize_log_path;

#[cfg(test)]
#[path = "db_turso/round2_benches.rs"]
mod round2_benches;

/// The folded bootstrap schema (migrations 01..21 in final state). Applied once
/// to a fresh database; the auto-wipe gate (`db_schema_version.rs`) guarantees a
/// clean rebuild on the breaking Turso upgrade so no incremental replay is needed.
pub(crate) const TURSO_BOOTSTRAP_SCHEMA: &str = include_str!("../../../sql/turso_schema.sql");

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

/// `subfiles` unique `(file_id, path)` index that backs `ON CONFLICT`.
/// The whole-wipe purge recreates it with the table.
///
/// Schema v24 dropped `idx_subfiles_path_remote_checksum`. Schema v25 dropped
/// `idx_subfiles_file_id_data_order`; ordered part reloads sort in process.
pub(crate) const SUBFILES_INDEX_CREATE_SQL: [&str; 1] =
    ["CREATE UNIQUE INDEX IF NOT EXISTS idx_subfiles_file_id_path ON subfiles(file_id, path)"];

/// `subfiles` index names, for `DROP INDEX IF EXISTS` when rebuilding the indexes.
pub(crate) const SUBFILES_INDEX_NAMES: [&str; 1] = ["idx_subfiles_file_id_path"];

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
    apply_schema(conn.raw(), TURSO_BOOTSTRAP_SCHEMA).await?;
    Ok(db)
}

/// Apply a multi-statement schema. Turso 0.7.2 has `execute_batch`, but the
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
    bump_schema_epoch();
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
    apply_schema(conn.raw(), TURSO_BOOTSTRAP_SCHEMA).await?;
    info!("Database wipe complete.");
    Ok(())
}

/// Whether Turso's MVCC concurrent-write mode is active.
///
/// Defaults **OFF**. `FOXY_DB_MVCC=1` (or `true`/`on`/`yes`) opts into
/// `journal_mode='mvcc'` + `BEGIN CONCURRENT`. On Turso 0.7.2 that is still
/// worse for measured Foxy sync and purge; see
/// `conventions/CORE_CONVENTIONS.md` (WAL vs MVCC).
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

/// Statement programs kept per connection. `prepare_cached` stores compiled
/// programs in an unbounded map inside the engine, so admission is capped here:
/// without a cap a long session's one-off `IN (?, ?, ...)` shapes would pin a
/// program each.
const MAX_CACHED_STATEMENTS: usize = 128;

/// Idle connections kept warm per database. Each holds its own pager and its own
/// page cache (`cache_size = -16384`, so 16 MiB apiece at full), which bounds the
/// idle pool's resident cost at six times that, roughly 96 MiB. The limit does
/// not cap active concurrency: a burst may open more, and connections above it
/// are discarded on return. Override with `FOXY_DB_POOL_IDLE`, where `0` is the
/// unpooled A/B control.
const DEFAULT_MAX_IDLE_CONNECTIONS: usize = 6;

fn max_idle_connections() -> usize {
    static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var("FOXY_DB_POOL_IDLE")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_IDLE_CONNECTIONS)
            .min(32)
    })
}

static DB_CONNECTIONS_OPENED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DB_CONNECTIONS_REUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bumped whenever DDL runs. A cached statement program is only rechecked
/// against its own connection's schema snapshot, and that snapshot is refreshed
/// only when a statement is compiled - so a connection that DDL happened
/// *around* can serve a program built against dropped roots. Pooled connections
/// carry the epoch they were opened at and are retired once it moves.
static DB_SCHEMA_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Retire every pooled connection: their cached programs may reference roots the
/// DDL just replaced.
pub(crate) fn bump_schema_epoch() {
    DB_SCHEMA_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

fn schema_epoch() -> u64 {
    DB_SCHEMA_EPOCH.load(std::sync::atomic::Ordering::SeqCst)
}

/// Whether a statement changes the schema, and so has to retire cached programs.
pub(crate) fn sql_is_ddl(sql: &str) -> bool {
    let head = sql.trim_start();
    let word_len = head
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(head.len());
    matches!(
        head[..word_len].to_ascii_uppercase().as_str(),
        "CREATE" | "DROP" | "ALTER" | "REINDEX" | "VACUUM"
    )
}

/// Connections opened and connections served from the pool since process start.
pub(crate) fn connection_counters() -> (u64, u64) {
    (
        DB_CONNECTIONS_OPENED.load(std::sync::atomic::Ordering::Relaxed),
        DB_CONNECTIONS_REUSED.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// A tuned connection plus the admission record for its statement cache.
///
/// Cloneable because the seam hands the same connection to a transaction handle
/// and its statements; clones share one engine connection, so they must not run
/// statements concurrently (Turso rejects overlapping use of one connection).
#[derive(Clone)]
pub(crate) struct TunedConnection {
    conn: Connection,
    admission: Arc<Mutex<StatementAdmission>>,
    epoch: u64,
}

#[derive(Default)]
struct StatementAdmission {
    /// SQL seen once; a shape is only admitted to the engine cache on its second
    /// use so single-shot statements never consume a cache slot.
    seen: std::collections::HashSet<u64>,
    cached: std::collections::HashSet<u64>,
}

fn sql_fingerprint(sql: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sql.hash(&mut hasher);
    hasher.finish()
}

impl TunedConnection {
    fn new(conn: Connection) -> Self {
        Self {
            conn,
            admission: Arc::new(Mutex::new(StatementAdmission::default())),
            epoch: schema_epoch(),
        }
    }

    /// The underlying engine connection, for call sites that need it directly.
    pub(crate) fn raw(&self) -> &Connection {
        &self.conn
    }

    /// Prepare `sql`, reusing this connection's compiled program when the shape
    /// has been seen before and the cache still has room.
    pub(crate) async fn prepare_tuned(&self, sql: &str) -> turso::Result<Statement> {
        let cacheable = {
            let fingerprint = sql_fingerprint(sql);
            let mut admission = self
                .admission
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if admission.cached.contains(&fingerprint) {
                true
            } else if admission.seen.remove(&fingerprint)
                && admission.cached.len() < MAX_CACHED_STATEMENTS
            {
                admission.cached.insert(fingerprint);
                true
            } else {
                if admission.seen.len() >= MAX_CACHED_STATEMENTS * 4 {
                    admission.seen.clear();
                }
                admission.seen.insert(fingerprint);
                false
            }
        };
        if cacheable {
            self.conn.prepare_cached(sql).await
        } else {
            self.conn.prepare(sql).await
        }
    }

    pub(crate) async fn execute(
        &self,
        sql: &str,
        params: impl turso::IntoParams,
    ) -> turso::Result<u64> {
        let mut stmt = self.prepare_tuned(sql).await?;
        stmt.execute(params).await
    }

    pub(crate) async fn query(
        &self,
        sql: &str,
        params: impl turso::IntoParams,
    ) -> turso::Result<turso::Rows> {
        let mut stmt = self.prepare_tuned(sql).await?;
        stmt.query(params).await
    }

    pub(crate) fn last_insert_rowid(&self) -> i64 {
        self.conn.last_insert_rowid()
    }

    pub(crate) async fn pragma_update<V: std::fmt::Display>(
        &self,
        name: &str,
        value: V,
    ) -> turso::Result<Vec<turso::Row>> {
        self.conn.pragma_update(name, value).await
    }

    pub(crate) fn busy_timeout(&self, duration: Duration) -> turso::Result<()> {
        self.conn.busy_timeout(duration)
    }

    pub(crate) fn is_autocommit(&self) -> turso::Result<bool> {
        self.conn.is_autocommit()
    }
}

/// Warm tuned connections for one database handle.
///
/// A checkout is exclusive: the guard owns the connection until it drops, so
/// pooling changes only where a connection comes from, never how many tasks
/// share one.
struct ConnectionPool {
    idle: Mutex<Vec<TunedConnection>>,
}

impl ConnectionPool {
    fn take(&self) -> Option<TunedConnection> {
        let epoch = schema_epoch();
        let mut idle = self
            .idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while let Some(conn) = idle.pop() {
            if conn.epoch == epoch {
                return Some(conn);
            }
        }
        None
    }

    fn put(&self, conn: TunedConnection) {
        if conn.epoch != schema_epoch() {
            return;
        }
        let mut idle = self
            .idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if idle.len() < max_idle_connections() {
            idle.push(conn);
        }
    }
}

/// Pools keyed by database identity. `Weak` so a pool never keeps a closed
/// database alive; dead entries are pruned on the next lookup.
static CONNECTION_POOLS: Mutex<Vec<(Weak<Database>, Arc<ConnectionPool>)>> = Mutex::new(Vec::new());

fn pool_for(db: &Arc<Database>) -> Arc<ConnectionPool> {
    let mut pools = CONNECTION_POOLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pools.retain(|(weak, _)| weak.strong_count() > 0);
    if let Some((_, pool)) = pools
        .iter()
        .find(|(weak, _)| weak.upgrade().is_some_and(|other| Arc::ptr_eq(&other, db)))
    {
        return pool.clone();
    }
    let pool = Arc::new(ConnectionPool {
        idle: Mutex::new(Vec::new()),
    });
    pools.push((Arc::downgrade(db), pool.clone()));
    pool
}

/// Drop every idle connection for every database.
///
/// Idle connections hold the engine database alive, so releasing a game space's
/// file (a space switch, a live wipe) has to drain them or the directory stays
/// locked.
pub(crate) fn drain_connection_pools() {
    let mut pools = CONNECTION_POOLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (_, pool) in pools.iter() {
        pool.idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
    pools.clear();
}

/// A tuned connection borrowed from its database's pool, returned on drop.
pub(crate) struct PooledConnection {
    conn: TunedConnection,
    pool: Arc<ConnectionPool>,
    retire: bool,
}

impl PooledConnection {
    /// Drop this connection on release instead of returning it to the pool.
    /// Callers that change connection-scoped state (the purge's
    /// `foreign_keys = OFF`) must retire it so the change cannot leak into the
    /// next borrower.
    pub(crate) fn retire(&mut self) {
        self.retire = true;
    }
}

impl std::ops::Deref for PooledConnection {
    type Target = TunedConnection;
    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        // A connection left inside a transaction would hand its open write to the
        // next borrower, so only autocommit connections go back.
        if self.retire || !self.conn.is_autocommit().unwrap_or(false) {
            return;
        }
        self.pool.put(self.conn.clone());
    }
}

/// Borrow a tuned connection from `db`'s pool, opening one if none is idle.
pub(crate) async fn connect_pooled(db: &Arc<Database>) -> turso::Result<PooledConnection> {
    let pool = pool_for(db);
    if let Some(conn) = pool.take() {
        DB_CONNECTIONS_REUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Ok(PooledConnection {
            conn,
            pool,
            retire: false,
        });
    }
    let conn = connect_tuned(db.as_ref()).await?;
    Ok(PooledConnection {
        conn,
        pool,
        retire: false,
    })
}

/// Open a fresh connection and apply the honored PRAGMAs + busy timeout.
///
/// Prefer [`connect_pooled`]: a connect builds a pager, reads page 1 and clones
/// the schema, and the five pragmas below are five more prepared statements.
/// This unpooled form is for bootstrap, compaction, and callers that mutate
/// connection-scoped pragmas.
pub(crate) async fn connect_tuned(db: &Database) -> turso::Result<TunedConnection> {
    DB_CONNECTIONS_OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let conn = TunedConnection::new(db.connect()?);
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
// In-place `VACUUM` is still experimental in Turso 0.7.2. `VACUUM INTO` is
// supported and 0.7 claims a nested-yield panic fix, but 0.6.1 panicked on
// large/bloated files (`vdbe/vacuum.rs`) even though the tiny
// `probe_vacuum_into` passed. Keep the manual row copy until a large bloated
// file survives VACUUM INTO; then swap in VACUUM INTO with this copy as fallback.

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
    let stats = read_db_bloat_stats(conn.raw()).await;
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
            // Sequential await: 0.7 rejects a second write on the same connection
            // while one is in flight (`StatementsInProgress`, not lock Busy).
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
/// NB: we do NOT use `VACUUM INTO` yet. It panicked inside Turso 0.6.1 on
/// large/bloated files (the tiny `probe_vacuum_into` passed). 0.7.2 claims a
/// nested-yield fix, but compaction stays on this copy path until a large-file
/// probe is green.
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
        let n = copy_table(src.raw(), &dst, table).await?;
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
    // Idle pooled connections hold the engine database alive, so they have to go
    // before the handle for the space directory to become deletable.
    drain_connection_pools();
    if let Some((path, _)) = slot.take() {
        info!(
            "Released the database handle for {}",
            sanitize_log_path(&path)
        );
    }
    // The lock file lives in the space directory this call exists to free, so
    // the claim has to go with the handle.
    crate::core::tasks::db_process_lock::release();
}

async fn open_and_prepare_database(path: &Path) -> Arc<Database> {
    let init_start = Instant::now();
    let path_str = path.to_string_lossy().to_string();
    info!("Ensuring Turso database {}", sanitize_log_path(path));

    // Backstop for the entry-point claims in the GUI startup and CLI dispatch.
    // Turso has no multi-process access, so opening a database this process does
    // not own is a correctness bug wherever it happens; make it loud rather than
    // let it corrupt quietly.
    if !crate::core::tasks::db_process_lock::holds(path) {
        match crate::core::tasks::db_process_lock::acquire(path) {
            crate::core::tasks::db_process_lock::LockOutcome::Acquired => {}
            crate::core::tasks::db_process_lock::LockOutcome::Busy { holder_pid } => {
                log::error!(
                    "Refusing to open database {}: another Foxy process{} owns it",
                    sanitize_log_path(path),
                    holder_pid
                        .map(|pid| format!(" (PID {pid})"))
                        .unwrap_or_default()
                );
                panic!("database is owned by another Foxy process");
            }
            crate::core::tasks::db_process_lock::LockOutcome::Unavailable(reason) => warn!(
                "STARTUP: could not verify exclusive database access ({}); continuing",
                reason
            ),
        }
    }

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
            let mode = read_journal_mode(conn.raw())
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

    // The bootstrap above is `CREATE ... IF NOT EXISTS`, so it silently no-ops on
    // a database an older Foxy built. Confirm the live schema can actually run
    // this build's statements before the app starts syncing against it.
    crate::core::tasks::db_schema_check::probe_database(path, &db).await;

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

/// Same-connection overlap: Turso 0.7 maps `StatementsInProgress` to `Error::Busy`
/// (SQLITE_BUSY class) but waiting cannot help - only finishing or resetting the
/// in-flight statement can. Must not burn [`DB_BUSY_TIMEOUT`] / retry backoff.
fn db_error_is_same_connection_overlap(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("sql statements in progress")
}

/// Shared classifier for transient DB errors used by every retry loop.
pub(crate) fn db_error_message_is_retryable(message: &str) -> bool {
    if db_error_is_same_connection_overlap(message) {
        return false;
    }
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
/// transient and safe to retry after a fresh `BEGIN`. Same-connection
/// `StatementsInProgress` arrives as `Busy` but is not lock contention.
pub(crate) fn db_is_retryable(err: &Error) -> bool {
    match err {
        Error::Busy(msg) if !db_error_is_same_connection_overlap(msg) => true,
        Error::BusySnapshot(_) => true,
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
        assert!(!db_error_message_is_retryable(
            "cannot start a write statement - SQL statements in progress"
        ));
        assert!(!db_error_message_is_retryable(
            "Busy: cannot commit transaction - SQL statements in progress"
        ));
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
    /// per-statement chunk-size knee? Inserts 66 336 rows (a large-repo part
    /// count) under single-writer WAL, one transaction, chunked - mirroring
    /// `remote_file_parts::batch::file_part_upsert_*`. Prints wall time for both
    /// statement shapes across several chunk sizes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_fresh_insert_vs_upsert() {
        const ROWS: usize = 66_336;
        const PARAMS_PER_ROW: usize = 6;

        // file_id=1 must exist (FK target for subfiles.file_id).
        async fn seed_file(db: &Database) -> TunedConnection {
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
    /// -- --ignored --nocapture`). Reproduces a large production-repo pattern: a
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
        db_retry_transaction(conn.raw(), "test insert", false, |c| {
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
        let result = db_retry_transaction(conn.raw(), "bad insert", false, |c| {
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
        assert!(db_is_retryable(&Error::Busy("Database is busy".into())));
        assert!(db_is_retryable(&Error::BusySnapshot("x".into())));
        assert!(db_is_retryable(&Error::Error(
            "write-write conflict".into()
        )));
        assert!(db_is_retryable(&Error::Error(
            "cannot commit - no transaction is active".into()
        )));
        assert!(!db_is_retryable(&Error::Error("syntax error".into())));
        assert!(!db_is_retryable(&Error::Constraint("unique".into())));
        assert!(!db_is_retryable(&Error::Busy(
            "cannot start a write statement - SQL statements in progress".into()
        )));
        assert!(!db_is_retryable(&Error::Interrupt("cancelled".into())));
    }

    #[tokio::test]
    async fn connect_tuned_defaults_to_wal_with_mvcc_off() {
        assert!(
            !mvcc_enabled(),
            "FOXY_DB_MVCC must stay unset in the default test process"
        );
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        let mode = read_journal_mode(conn.raw()).await.expect("journal_mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[tokio::test]
    async fn statements_in_progress_is_not_retried_as_lock_busy() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        let started = Instant::now();
        let err = db_retry_transaction(conn.raw(), "overlap", false, |_| {
            Box::pin(async {
                Err(Error::Busy(
                    "cannot start a write statement - SQL statements in progress".into(),
                ))
            })
        })
        .await
        .expect_err("same-connection overlap must fail");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "must not burn DB_BUSY_TIMEOUT, took {elapsed:?}"
        );
        assert!(!db_is_retryable(&err));
    }

    #[tokio::test]
    async fn same_connection_overlapping_writes_do_not_burn_busy_timeout() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        conn.execute(
            "CREATE TABLE overlap_t (id INTEGER PRIMARY KEY, v BLOB)",
            (),
        )
        .await
        .unwrap();
        let blob = vec![0u8; 256 * 1024];
        let conn2 = conn.clone();
        let started = Instant::now();
        let (a, b) = tokio::join!(
            conn.execute("INSERT INTO overlap_t (v) VALUES (?)", [blob.clone()]),
            conn2.execute("INSERT INTO overlap_t (v) VALUES (?)", [blob]),
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < DB_BUSY_TIMEOUT,
            "overlapping writes must not wait the full busy_timeout, took {elapsed:?}"
        );
        for result in [a, b] {
            if let Err(err) = result {
                assert!(
                    !db_is_retryable(&err),
                    "rejected second write must not be classified as lock Busy: {err}"
                );
            }
        }
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

    /// Probe: does Turso 0.7.2 `VACUUM INTO 'file'` survive a bloated copy
    /// (compacted copy to a new file, which does NOT need the experimental
    /// in-place vacuum flag)? Tiny dbs passed on 0.6.1; large files panicked.
    /// Keep `copy_table` until a large bloated file is green. Run:
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
            pc(conn.raw(), "PRAGMA page_count").await,
            pc(conn.raw(), "PRAGMA freelist_count").await
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

    /// Large-file companion to [`probe_vacuum_into`]. The tiny probe passed on
    /// 0.6.1 while production-sized files panicked in `vdbe/vacuum.rs`, so
    /// `build_compacted_copy` cannot be replaced by VACUUM INTO until a bloated
    /// multi-hundred-MB file is green. Bloats the db to `FOXY_VACUUM_PROBE_MB`
    /// (default 512), deletes every row, then vacuums into a sibling file. Run:
    /// `cargo test --release -p Foxy probe_vacuum_into_large_bloated_file -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn probe_vacuum_into_large_bloated_file() {
        const ROW_PAYLOAD_BYTES: usize = 64 * 1024;

        let target_mb: u64 = std::env::var("FOXY_VACUUM_PROBE_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(512);
        let target_bytes = target_mb * 1024 * 1024;
        let rows = (target_bytes / ROW_PAYLOAD_BYTES as u64).max(1);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let path_str = path.to_string_lossy().to_string();
        let db = build_and_bootstrap(&path_str).await.unwrap();
        let conn = connect_tuned(&db).await.unwrap();

        conn.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1,'f','rp','lp')",
            (),
        )
        .await
        .unwrap();

        let payload = "x".repeat(ROW_PAYLOAD_BYTES);
        let sql = "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) VALUES (1, ?, 0, 0, 0, 0, '', ?, ?)";
        for i in 0..rows {
            conn.execute(sql, (payload.clone(), format!("rc{i}"), i as i64))
                .await
                .unwrap();
        }
        conn.execute("DELETE FROM subfiles", ()).await.unwrap();

        async fn pc(conn: &Connection, sql: &str) -> Option<turso::Value> {
            let mut rows = conn.query(sql, ()).await.ok()?;
            rows.next()
                .await
                .ok()
                .flatten()
                .and_then(|r| r.get_value(0).ok())
        }
        let file_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        eprintln!(
            "before: bytes={file_bytes} page_count={:?} freelist={:?}",
            pc(conn.raw(), "PRAGMA page_count").await,
            pc(conn.raw(), "PRAGMA freelist_count").await
        );
        assert!(
            file_bytes >= target_bytes / 2,
            "probe did not build a large enough file: {file_bytes} bytes"
        );

        let into = dir.path().join("compacted.db");
        let vacuum_sql = format!(
            "VACUUM INTO '{}'",
            into.to_string_lossy().replace('\\', "/")
        );
        // A panic here (not an Err) is the 0.6.1 failure this probe exists to catch.
        match conn.execute(&vacuum_sql, ()).await {
            Ok(_) => {
                let vacuumed_bytes = fs::metadata(&into).map(|m| m.len()).unwrap_or(0);
                eprintln!(
                    "VACUUM INTO OK -> {} bytes={vacuumed_bytes}",
                    into.display()
                );
                assert!(
                    vacuumed_bytes > 0 && vacuumed_bytes < file_bytes,
                    "VACUUM INTO did not shrink: {file_bytes} -> {vacuumed_bytes}"
                );
            }
            Err(e) => eprintln!("VACUUM INTO ERR (still not usable for compaction): {e}"),
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
            let free = read_pragma_i64(conn.raw(), "freelist_count")
                .await
                .unwrap_or(0);
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
            count(conn.raw(), "subfiles").await,
            1000,
            "subfile rows preserved"
        );
        assert_eq!(count(conn.raw(), "files").await, 1, "file rows preserved");
        assert_eq!(
            count(conn.raw(), "repositories").await,
            1,
            "repo rows preserved"
        );
        let free_after = read_pragma_i64(conn.raw(), "freelist_count")
            .await
            .unwrap_or(-1);
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
    ///   FOXY_INSPECT_DB=<copy> REPO_LIKE=<url-substring> FK=ON  cargo test -p Foxy diag_scoped_purge_fk -- --ignored --nocapture
    ///   FOXY_INSPECT_DB=<copy> REPO_LIKE=<url-substring> FK=OFF cargo test -p Foxy diag_scoped_purge_fk -- --ignored --nocapture
    /// If ON wedges/crawls and OFF is fast, FK enforcement is the hang.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn diag_scoped_purge_fk() {
        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let repo_like =
            std::env::var("REPO_LIKE").expect("set REPO_LIKE to a remote_url substring");
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
    /// Run: `FOXY_INSPECT_DB=<copy> REPO_LIKE=<url-substring> cargo test -p Foxy repro_purge_wedge_under_concurrency -- --ignored --nocapture`
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn repro_purge_wedge_under_concurrency() {
        use crate::core::db::FoxyDb;
        use std::sync::atomic::{AtomicBool, Ordering};

        let path = std::env::var("FOXY_INSPECT_DB").expect("set FOXY_INSPECT_DB");
        let repo_like =
            std::env::var("REPO_LIKE").expect("set REPO_LIKE to a remote_url substring");
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
        eprintln!("[repro] readers={readers} bypass_barrier={bypass} repo_like={repo_like}");

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

        let insert_repo_sql = format!(
            "INSERT OR IGNORE INTO temp.fp_repo SELECT id FROM repositories WHERE remote_url LIKE '%{}%'",
            repo_like.replace('\'', "''")
        );
        // Scope to ONE repository (REPO_LIKE substring of remote_url) so the
        // purge matches a production single-repo force-redownload and leaves
        // sibling repos' rows in place.
        let steps: Vec<(&str, String)> = vec![
            (
                "create repo_ids",
                "CREATE TEMP TABLE temp.fp_repo (id INTEGER PRIMARY KEY)".into(),
            ),
            (
                "create orphan_addon_ids",
                "CREATE TEMP TABLE temp.fp_oaddon (addon_id INTEGER PRIMARY KEY)".into(),
            ),
            (
                "create orphan_file_ids",
                "CREATE TEMP TABLE temp.fp_ofile (file_id INTEGER PRIMARY KEY)".into(),
            ),
            ("insert repo_ids", insert_repo_sql),
            (
                "insert orphan_addon_ids",
                "INSERT OR IGNORE INTO temp.fp_oaddon SELECT addon_id FROM repository_addons WHERE repository_id IN (SELECT id FROM temp.fp_repo)".into(),
            ),
            (
                "insert orphan_file_ids",
                "INSERT OR IGNORE INTO temp.fp_ofile SELECT file_id FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)".into(),
            ),
            (
                "delete repositories",
                "DELETE FROM repositories WHERE id IN (SELECT id FROM temp.fp_repo)".into(),
            ),
            (
                "delete dtfp (nested)",
                "DELETE FROM download_target_file_part WHERE subfile_id IN (SELECT id FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile))".into(),
            ),
            (
                "delete subfiles",
                "DELETE FROM subfiles WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)".into(),
            ),
            (
                "delete download_patch_op",
                "DELETE FROM download_patch_op WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)".into(),
            ),
            (
                "delete download_patch_file",
                "DELETE FROM download_patch_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)".into(),
            ),
            (
                "delete download_target_file",
                "DELETE FROM download_target_file WHERE file_id IN (SELECT file_id FROM temp.fp_ofile)".into(),
            ),
            (
                "delete addon_files",
                "DELETE FROM addon_files WHERE addon_id IN (SELECT addon_id FROM temp.fp_oaddon)".into(),
            ),
            (
                "delete addons",
                "DELETE FROM addons WHERE id IN (SELECT addon_id FROM temp.fp_oaddon)".into(),
            ),
            (
                "delete files",
                "DELETE FROM files WHERE id IN (SELECT file_id FROM temp.fp_ofile)".into(),
            ),
        ];

        let txn = Instant::now();
        if bypass {
            // Pre-fix path: raw connection, no barrier - wedges under readers.
            let conn = connect_tuned(&db).await.expect("purge connect");
            conn.execute("BEGIN", ()).await.unwrap();
            for (label, sql) in &steps {
                let t = Instant::now();
                conn.execute(sql.as_str(), ())
                    .await
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                eprintln!("[repro]   {label:<28} {:.3}s", t.elapsed().as_secs_f64());
            }
            conn.execute("COMMIT", ()).await.unwrap();
        } else {
            // Fixed path: exclusive seam transaction quiesces the readers.
            let foxy = FoxyDb::from_turso(db.clone());
            foxy.transaction_exclusive("repro purge", |tx| {
                let steps = steps.clone();
                Box::pin(async move {
                    for (label, sql) in &steps {
                        let t = Instant::now();
                        tx.execute(sql.as_str(), vec![]).await?;
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

    /// Benchmark (`cargo test --release bench_connection_reuse -- --ignored --nocapture`).
    /// The seam opens a fresh tuned connection for every `execute`/`query`, on the
    /// 0.5-era claim that connections are ~16us. Turso 0.7.2 builds a new `Pager`,
    /// reads page 1, clones the schema and runs five pragmas per connect, and its
    /// statement cache dies with the connection - so this measures what the seam
    /// pays per call and what reuse plus `prepare_cached` would save.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_connection_reuse() {
        const ROWS: usize = 20_000;
        const CALLS: usize = 2_000;
        let (_dir, db) = temp_db().await;

        // Seed a realistically sized subfiles table so page-cache warmth matters.
        let seed = connect_tuned(&db).await.unwrap();
        seed.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
            (),
        )
        .await
        .unwrap();
        seed.execute("BEGIN", ()).await.unwrap();
        for start in (0..ROWS).step_by(256) {
            let end = (start + 256).min(ROWS);
            let ph = vec!["(1, ?, 0, 0, 4096, 0, '', ?, ?)"; end - start].join(", ");
            let sql = format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            );
            let mut binds: Vec<turso::Value> = Vec::new();
            for i in start..end {
                binds.push(turso::Value::Text(format!("p{i}")));
                binds.push(turso::Value::Text(format!("rc{i}")));
                binds.push(turso::Value::Integer(i as i64));
            }
            seed.execute(&sql, binds).await.unwrap();
        }
        seed.execute("COMMIT", ()).await.unwrap();
        drop(seed);

        let t = Instant::now();
        for _ in 0..CALLS {
            let _c = db.connect().unwrap();
        }
        println!(
            "[bench conn] raw db.connect()          {:>8.1} us/call",
            t.elapsed().as_secs_f64() * 1e6 / CALLS as f64
        );

        let t = Instant::now();
        for _ in 0..CALLS {
            let _c = connect_tuned(&db).await.unwrap();
        }
        println!(
            "[bench conn] connect_tuned()           {:>8.1} us/call",
            t.elapsed().as_secs_f64() * 1e6 / CALLS as f64
        );

        let sql = "SELECT id, remote_checksum FROM subfiles WHERE file_id = 1 AND path = ?";

        let t = Instant::now();
        for i in 0..CALLS {
            let conn = connect_tuned(&db).await.unwrap();
            let mut rows = conn
                .query(sql, vec![turso::Value::Text(format!("p{i}"))])
                .await
                .unwrap();
            let _ = rows.next().await.unwrap();
        }
        let per_fresh = t.elapsed().as_secs_f64() * 1e6 / CALLS as f64;
        println!("[bench conn] query, fresh conn/call    {per_fresh:>8.1} us/call");

        let conn = connect_tuned(&db).await.unwrap();
        let t = Instant::now();
        for i in 0..CALLS {
            let mut rows = conn
                .query(sql, vec![turso::Value::Text(format!("p{i}"))])
                .await
                .unwrap();
            let _ = rows.next().await.unwrap();
        }
        let per_reused = t.elapsed().as_secs_f64() * 1e6 / CALLS as f64;
        println!("[bench conn] query, reused conn        {per_reused:>8.1} us/call");

        let t = Instant::now();
        for i in 0..CALLS {
            let mut stmt = conn.raw().prepare_cached(sql).await.unwrap();
            let mut rows = stmt
                .query(vec![turso::Value::Text(format!("p{i}"))])
                .await
                .unwrap();
            let _ = rows.next().await.unwrap();
        }
        let per_cached = t.elapsed().as_secs_f64() * 1e6 / CALLS as f64;
        println!("[bench conn] query, reused + cached    {per_cached:>8.1} us/call");
        println!(
            "[bench conn] speedup reuse={:.1}x reuse+cached={:.1}x",
            per_fresh / per_reused,
            per_fresh / per_cached
        );

        // Same comparison for a small write statement, which is what the seam's
        // `execute_retry` and per-row upserts actually do.
        let wsql = "UPDATE subfiles SET data_order = data_order WHERE file_id = 1 AND path = ?";
        let t = Instant::now();
        for i in 0..500 {
            let c = connect_tuned(&db).await.unwrap();
            c.execute(wsql, vec![turso::Value::Text(format!("p{i}"))])
                .await
                .unwrap();
        }
        let w_fresh = t.elapsed().as_secs_f64() * 1e6 / 500.0;
        let t = Instant::now();
        for i in 0..500 {
            let mut stmt = conn.raw().prepare_cached(wsql).await.unwrap();
            stmt.execute(vec![turso::Value::Text(format!("p{i}"))])
                .await
                .unwrap();
        }
        let w_cached = t.elapsed().as_secs_f64() * 1e6 / 500.0;
        println!(
            "[bench conn] write fresh={w_fresh:.1} us  reused+cached={w_cached:.1} us  speedup={:.1}x",
            w_fresh / w_cached
        );
    }

    /// Benchmark (`cargo test --release bench_cached_chunk_knee -- --ignored --nocapture`).
    /// `bulk_write_chunk_rows` sits at 256 because Turso's per-statement parse and
    /// plan cost is superlinear in row count. A pooled connection reuses compiled
    /// programs, so this re-measures the knee with and without that reuse.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_cached_chunk_knee() {
        const ROWS: usize = 66_336;

        fn plain_sql(n: usize) -> String {
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; n].join(", ");
            format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            )
        }
        fn binds(start: usize, end: usize) -> Vec<turso::Value> {
            let mut v = Vec::with_capacity((end - start) * 6);
            for i in start..end {
                v.push(turso::Value::Integer(1));
                v.push(turso::Value::Text(format!("p{i}")));
                v.push(turso::Value::Integer(4096));
                v.push(turso::Value::Integer(0));
                v.push(turso::Value::Text(format!("rc{i}")));
                v.push(turso::Value::Integer(i as i64));
            }
            v
        }

        for chunk_rows in [256usize, 512, 1_024, 2_048, 4_096] {
            for cached in [false, true] {
                let (_dir, db) = temp_db().await;
                let conn = connect_tuned(&db).await.unwrap();
                conn.execute(
                    "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
                    (),
                )
                .await
                .unwrap();
                let started = Instant::now();
                conn.execute("BEGIN", ()).await.unwrap();
                let mut start = 0;
                while start < ROWS {
                    let end = (start + chunk_rows).min(ROWS);
                    let sql = plain_sql(end - start);
                    if cached {
                        conn.execute(&sql, binds(start, end)).await.unwrap();
                    } else {
                        let mut stmt = conn.raw().prepare(&sql).await.unwrap();
                        stmt.execute(binds(start, end)).await.unwrap();
                    }
                    start = end;
                }
                conn.execute("COMMIT", ()).await.unwrap();
                let elapsed = started.elapsed().as_secs_f64();
                println!(
                    "[bench chunk-knee] chunk_rows={chunk_rows:<5} cached={cached:<5} \
                     total={elapsed:.3}s per_row_us={:.1}",
                    elapsed * 1_000_000.0 / ROWS as f64
                );
            }
        }
    }

    /// Benchmark (`cargo test --release bench_bulk_read_path -- --ignored --nocapture`).
    /// The incremental hasher reloads every `subfiles` row for the repository in
    /// one statement (433k rows, 1.66 s on the profiled case). Splits that between
    /// the engine scan and the seam's per-row `DbRow`/`DbValue` materialization.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_bulk_read_path() {
        const ROWS: usize = 433_248;
        const FILES: usize = 864;
        const COLUMNS: &str = "id, file_id, path, remote_length, local_length, remote_start,              local_start, remote_checksum, local_checksum, data_order";

        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        for file in 1..=FILES {
            conn.execute(
                "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                (file as i64, format!("f{file}"), "rp", "lp"),
            )
            .await
            .unwrap();
        }
        let per_file = ROWS / FILES;
        conn.execute("BEGIN", ()).await.unwrap();
        let mut written = 0usize;
        while written < ROWS {
            let batch = 256.min(ROWS - written);
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; batch].join(", ");
            let sql = format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length,                  remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            );
            let mut binds = Vec::with_capacity(batch * 6);
            for row in written..written + batch {
                let order = (row % per_file) as i64;
                let file_id = ((row / per_file) as i64 + 1).min(FILES as i64);
                binds.push(turso::Value::Integer(file_id));
                binds.push(turso::Value::Text(format!("data/e_{row:06}.bin")));
                binds.push(turso::Value::Integer(64));
                binds.push(turso::Value::Integer(order * 64));
                binds.push(turso::Value::Text(format!("rc{row}")));
                binds.push(turso::Value::Integer(order));
            }
            conn.execute(&sql, binds).await.unwrap();
            written += batch;
        }
        conn.execute("COMMIT", ()).await.unwrap();

        let bare = format!("SELECT {COLUMNS} FROM subfiles");
        let ordered =
            format!("SELECT {COLUMNS} FROM subfiles ORDER BY file_id ASC, data_order ASC, id ASC");
        let in_list = vec!["?"; FILES].join(", ");
        // The shape the incremental hasher actually issues (`model_tree.rs`).
        let scoped = format!(
            "SELECT {COLUMNS} FROM subfiles WHERE file_id IN ({in_list})              ORDER BY file_id ASC, data_order ASC, id ASC"
        );
        let scoped_binds: Vec<crate::core::db::DbValue> = (1..=FILES as i64)
            .map(crate::core::db::DbValue::from)
            .collect();
        let sql = bare.clone();
        let shared = Arc::new(db);
        let fdb = crate::core::db::FoxyDb::from_turso(shared.clone());

        // Warm the page cache so this measures materialization, not first-touch IO.
        let _ = fdb
            .query_all(&sql, crate::core::db::params![])
            .await
            .unwrap();

        let started = Instant::now();
        let seam = fdb
            .query_all(&sql, crate::core::db::params![])
            .await
            .unwrap();
        let seam_s = started.elapsed().as_secs_f64();
        assert_eq!(seam.len(), ROWS);

        let conn = connect_tuned(shared.as_ref()).await.unwrap();
        let started = Instant::now();
        let mut rows = conn.query(&sql, ()).await.unwrap();
        let mut counted = 0usize;
        let mut checksum_bytes = 0usize;
        while let Some(row) = rows.next().await.unwrap() {
            let _id = row.get_value(0).unwrap();
            let _file_id = row.get_value(1).unwrap();
            if let turso::Value::Text(text) = row.get_value(2).unwrap() {
                checksum_bytes += text.len();
            }
            counted += 1;
        }
        let raw_s = started.elapsed().as_secs_f64();
        assert_eq!(counted, ROWS);
        assert!(checksum_bytes > 0);

        let started = Instant::now();
        let ordered_rows = fdb
            .query_all(&ordered, crate::core::db::params![])
            .await
            .unwrap();
        let ordered_s = started.elapsed().as_secs_f64();
        assert_eq!(ordered_rows.len(), ROWS);

        let started = Instant::now();
        let scoped_rows = fdb.query_all(&scoped, scoped_binds).await.unwrap();
        let scoped_s = started.elapsed().as_secs_f64();
        assert_eq!(scoped_rows.len(), ROWS);

        // The same scoping expressed as a subquery instead of a literal id list.
        let subquery = format!(
            "SELECT {COLUMNS} FROM subfiles WHERE file_id IN (SELECT id FROM files)              ORDER BY file_id ASC, data_order ASC, id ASC"
        );
        let started = Instant::now();
        let subquery_rows = fdb
            .query_all(&subquery, crate::core::db::params![])
            .await
            .unwrap();
        let subquery_s = started.elapsed().as_secs_f64();
        assert_eq!(subquery_rows.len(), ROWS);

        // Unordered scan plus an in-process sort, for comparison with ORDER BY.
        let started = Instant::now();
        let mut sorted = fdb
            .query_all(&bare, crate::core::db::params![])
            .await
            .unwrap();
        sorted.sort_by_key(|row| {
            (
                row.get_i64("file_id").unwrap_or_default(),
                row.get_i64("data_order").unwrap_or_default(),
                row.get_i64("id").unwrap_or_default(),
            )
        });
        let rust_sort_s = started.elapsed().as_secs_f64();
        assert_eq!(sorted.len(), ROWS);
        // Keep the database-side scoping but sort in process.
        let scoped_unordered =
            format!("SELECT {COLUMNS} FROM subfiles WHERE file_id IN ({in_list})");
        let binds: Vec<crate::core::db::DbValue> = (1..=FILES as i64)
            .map(crate::core::db::DbValue::from)
            .collect();
        let started = Instant::now();
        let mut scoped_sorted = fdb.query_all(&scoped_unordered, binds).await.unwrap();
        scoped_sorted.sort_by_key(|row| {
            (
                row.get_i64("file_id").unwrap_or_default(),
                row.get_i64("data_order").unwrap_or_default(),
                row.get_i64("id").unwrap_or_default(),
            )
        });
        let scoped_sort_s = started.elapsed().as_secs_f64();
        assert_eq!(scoped_sorted.len(), ROWS);
        println!(
            "[bench bulk-read] seam_bare_plus_rust_sort={rust_sort_s:.3}s              seam_scoped_plus_rust_sort={scoped_sort_s:.3}s subquery_scoped={subquery_s:.3}s"
        );

        println!(
            "[bench bulk-read] rows={ROWS} raw_stream={raw_s:.3}s seam_bare={seam_s:.3}s              seam_ordered={ordered_s:.3}s seam_scoped_ordered={scoped_s:.3}s              seam_overhead={:.3}s order_by_cost={:.3}s in_list_cost={:.3}s",
            seam_s - raw_s,
            ordered_s - seam_s,
            scoped_s - ordered_s,
        );
    }

    /// Benchmark (`cargo test --release bench_subfiles_index_cost -- --ignored --nocapture`).
    /// The download-overlapped deferred flush inserts 433k `subfiles` rows against
    /// live indexes. Splits that cost between the table write, index maintenance in
    /// key order, and index maintenance in arrival order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_subfiles_index_cost() {
        const ROWS: usize = 433_248;
        const FILES: usize = 864;
        const CHUNK: usize = 256;

        fn plain_sql(n: usize) -> String {
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; n].join(", ");
            format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length,                  remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            )
        }

        // (file_id, data_order) in either key order or the mod-completion order the
        // deferred buffer actually arrives in.
        fn rows(sorted: bool) -> Vec<(i64, i64)> {
            let per_file = ROWS / FILES;
            let mut out = Vec::with_capacity(ROWS);
            for file in 0..FILES {
                for order in 0..per_file {
                    out.push((file as i64 + 1, order as i64));
                }
            }
            if !sorted {
                let mut state = 0x2545_f491_4f6c_dd1d_u64;
                for i in (1..out.len()).rev() {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    out.swap(i, (state % (i as u64 + 1)) as usize);
                }
            }
            out
        }

        for (label, sorted, drop_names) in [
            ("live_indexes_arrival_order", false, &[][..]),
            ("live_indexes_key_order", true, &[][..]),
            (
                "dropped_unique_index_key_order",
                true,
                &SUBFILES_INDEX_NAMES[..],
            ),
        ] {
            let (_dir, db) = temp_db().await;
            let conn = connect_tuned(&db).await.unwrap();
            for file in 1..=FILES {
                conn.execute(
                    "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                    (file as i64, format!("f{file}"), "rp", "lp"),
                )
                .await
                .unwrap();
            }
            let data = rows(sorted);
            let started = Instant::now();
            conn.execute("BEGIN", ()).await.unwrap();
            let mut drop_s = 0.0;
            if !drop_names.is_empty() {
                let t = Instant::now();
                for name in drop_names {
                    conn.execute(&format!("DROP INDEX IF EXISTS {name}"), ())
                        .await
                        .unwrap();
                }
                drop_s = t.elapsed().as_secs_f64();
            }
            let insert_started = Instant::now();
            for chunk in data.chunks(CHUNK) {
                let mut binds = Vec::with_capacity(chunk.len() * 6);
                for (file_id, order) in chunk {
                    binds.push(turso::Value::Integer(*file_id));
                    binds.push(turso::Value::Text(format!("data/e_{order:05}.bin")));
                    binds.push(turso::Value::Integer(64));
                    binds.push(turso::Value::Integer(order * 64));
                    binds.push(turso::Value::Text(format!("rc{file_id}_{order}")));
                    binds.push(turso::Value::Integer(*order));
                }
                conn.execute(&plain_sql(chunk.len()), binds).await.unwrap();
            }
            let insert_s = insert_started.elapsed().as_secs_f64();
            let mut rebuild_s = 0.0;
            if drop_names.len() == SUBFILES_INDEX_NAMES.len() {
                let t = Instant::now();
                for sql in SUBFILES_INDEX_CREATE_SQL {
                    conn.execute(sql, ()).await.unwrap();
                }
                rebuild_s = t.elapsed().as_secs_f64();
            }
            let commit_started = Instant::now();
            conn.execute("COMMIT", ()).await.unwrap();
            let commit_s = commit_started.elapsed().as_secs_f64();
            let total = started.elapsed().as_secs_f64();
            println!(
                "[bench subfiles-index] {label:<26} total={total:.3}s drop={drop_s:.3}s                  insert={insert_s:.3}s rebuild={rebuild_s:.3}s commit={commit_s:.3}s                  per_row_us={:.1}",
                total * 1_000_000.0 / ROWS as f64
            );
        }
    }

    /// Benchmark (`cargo test --release bench_addon_files_insert -- --ignored --nocapture`).
    /// `addon_files insert` costs ~190 µs/row on the metadata rebuild, more than
    /// twice the eight-column `files` upsert it accompanies, which points at its
    /// two `ON DELETE CASCADE` parents rather than at row width. Splits the cost
    /// between FK enforcement, the conflict clause, and the write itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_addon_files_insert() {
        const MODS: usize = 97;
        const FILES_PER_MOD: usize = 38;

        for foreign_keys in ["ON", "OFF"] {
            for shape in ["on-conflict", "plain"] {
                let (_dir, db) = temp_db().await;
                let conn = connect_tuned(&db).await.unwrap();
                conn.pragma_update("foreign_keys", foreign_keys)
                    .await
                    .unwrap();
                for m in 0..MODS {
                    conn.execute(
                        "INSERT INTO addons (id, name, remote_path, local_path, required) \
                         VALUES (?, 'a', ?, 'lp', 1)",
                        vec![
                            turso::Value::Integer(m as i64 + 1),
                            turso::Value::Text(format!("rp{m}")),
                        ],
                    )
                    .await
                    .unwrap();
                }
                for f in 0..MODS * FILES_PER_MOD {
                    conn.execute(
                        "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, 'f', ?, 'lp')",
                        vec![
                            turso::Value::Integer(f as i64 + 1),
                            turso::Value::Text(format!("rp{f}")),
                        ],
                    )
                    .await
                    .unwrap();
                }

                let started = Instant::now();
                for m in 0..MODS {
                    let ph = vec!["(?, ?)"; FILES_PER_MOD].join(", ");
                    let sql = if shape == "on-conflict" {
                        format!(
                            "INSERT INTO addon_files (addon_id, file_id) VALUES {ph} \
                             ON CONFLICT(addon_id, file_id) DO NOTHING"
                        )
                    } else {
                        format!("INSERT INTO addon_files (addon_id, file_id) VALUES {ph}")
                    };
                    let mut values = Vec::with_capacity(FILES_PER_MOD * 2);
                    for f in 0..FILES_PER_MOD {
                        values.push(turso::Value::Integer(m as i64 + 1));
                        values.push(turso::Value::Integer((m * FILES_PER_MOD + f) as i64 + 1));
                    }
                    conn.execute("BEGIN", ()).await.unwrap();
                    conn.execute(&sql, values).await.unwrap();
                    conn.execute("COMMIT", ()).await.unwrap();
                }
                let elapsed = started.elapsed().as_secs_f64();
                let rows = MODS * FILES_PER_MOD;
                println!(
                    "[bench addon_files] fk={foreign_keys:<3} shape={shape:<11} \
                     total={elapsed:.3}s per_row_us={:.1}",
                    elapsed * 1_000_000.0 / rows as f64
                );
            }
        }
    }

    /// A released connection goes back to its database's pool, and the next
    /// borrow reuses it rather than paying another connect.
    #[tokio::test]
    async fn pooled_connection_is_reused_after_release() {
        let (_dir, db) = temp_db().await;
        let db = Arc::new(db);
        // Asserted against this database's own pool and connection identity, not
        // the global connect counters: every other database test shares those and
        // runs in parallel with this one.
        let first = {
            let conn = connect_pooled(&db).await.unwrap();
            conn.query("SELECT 1", ()).await.unwrap();
            conn.conn.admission.clone()
        };
        assert_eq!(
            pool_for(&db).idle.lock().unwrap().len(),
            1,
            "a released connection has to go back to its pool"
        );
        let conn = connect_pooled(&db).await.unwrap();
        conn.query("SELECT 1", ()).await.unwrap();
        assert!(
            Arc::ptr_eq(&first, &conn.conn.admission),
            "second borrow must be served the pooled connection, not a fresh one"
        );
        assert!(
            pool_for(&db).idle.lock().unwrap().is_empty(),
            "a borrowed connection must not stay listed as idle"
        );
    }

    /// A connection left inside a transaction is dropped rather than pooled, so
    /// the next borrower never inherits an open write.
    #[tokio::test]
    async fn connection_in_a_transaction_is_not_pooled() {
        let (_dir, db) = temp_db().await;
        let db = Arc::new(db);
        {
            let conn = connect_pooled(&db).await.unwrap();
            conn.execute("BEGIN", ()).await.unwrap();
        }
        let conn = connect_pooled(&db).await.unwrap();
        assert!(
            conn.is_autocommit().unwrap(),
            "a fresh borrow must not be inside a transaction"
        );
    }

    /// DDL retires pooled connections: a cached program compiled against the old
    /// index roots must not survive a DROP/CREATE of that index.
    #[tokio::test]
    async fn ddl_retires_pooled_connections() {
        let (_dir, db) = temp_db().await;
        let db = Arc::new(db);
        let fdb = crate::core::db::FoxyDb::from_turso(db.clone());
        fdb.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
            crate::core::db::params![],
        )
        .await
        .unwrap();
        // Run the read twice so its program is admitted to the statement cache.
        let sql = "SELECT COUNT(*) AS c FROM subfiles WHERE file_id = 1";
        for _ in 0..2 {
            fdb.query_one(sql, crate::core::db::params![])
                .await
                .unwrap();
        }
        let before = schema_epoch();
        for name in SUBFILES_INDEX_NAMES {
            fdb.execute(
                &format!("DROP INDEX IF EXISTS {name}"),
                crate::core::db::params![],
            )
            .await
            .unwrap();
        }
        assert!(
            schema_epoch() > before,
            "DDL through the seam must bump the schema epoch"
        );
        fdb.execute(
            "INSERT INTO subfiles (file_id, path, remote_length, remote_start, remote_checksum, \
             data_order) VALUES (1, 'p', 1, 0, 'c', 0)",
            crate::core::db::params![],
        )
        .await
        .unwrap();
        for sql in SUBFILES_INDEX_CREATE_SQL {
            fdb.execute(sql, crate::core::db::params![]).await.unwrap();
        }
        let row = fdb
            .query_one(sql, crate::core::db::params![])
            .await
            .unwrap()
            .expect("count row");
        assert_eq!(
            row.get_i64("c").unwrap(),
            1,
            "the read after the index rebuild must see the inserted part"
        );
    }

    /// A database written under WAL can be reopened as MVCC and back without
    /// losing rows. The engine mode is a file property, so switching the default
    /// would have to migrate live databases in place rather than rebuild them.
    #[tokio::test]
    async fn wal_database_round_trips_through_mvcc() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let path_str = path.to_str().unwrap().to_string();

        let db = build_and_bootstrap(&path_str).await.unwrap();
        let conn = connect_tuned(&db).await.unwrap();
        conn.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) \
             VALUES (1, 'n', 'u', 'p')",
            (),
        )
        .await
        .unwrap();
        assert_eq!(read_journal_mode(conn.raw()).await.as_deref(), Some("wal"));
        drop(conn);
        drop(db);

        let db = Builder::new_local(&path_str).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.pragma_update("journal_mode", "mvcc").await.unwrap();
        {
            // A live `Rows` pins a read transaction, and dropping the database
            // under one leaves the file locked for the next open.
            let mut rows = conn
                .query("SELECT name FROM repositories WHERE id = 1", ())
                .await
                .unwrap();
            let row = rows.next().await.unwrap().expect("row survives the switch");
            assert_eq!(row.get::<String>(0).unwrap(), "n");
        }
        conn.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) \
             VALUES (2, 'n2', 'u2', 'p2')",
            (),
        )
        .await
        .unwrap();
        drop(conn);
        drop(db);

        let db = Builder::new_local(&path_str).build().await.unwrap();
        let conn = connect_tuned(&db).await.unwrap();
        assert_eq!(read_journal_mode(conn.raw()).await.as_deref(), Some("wal"));
        let mut rows = conn
            .query("SELECT COUNT(*) FROM repositories", ())
            .await
            .unwrap();
        let row = rows.next().await.expect("count step").expect("count row");
        assert_eq!(
            row.get::<i64>(0).unwrap(),
            2,
            "both rows must survive the wal -> mvcc -> wal round trip"
        );
    }

    #[test]
    fn ddl_detection_covers_the_statements_the_seam_runs() {
        assert!(sql_is_ddl("DROP INDEX IF EXISTS idx_subfiles_file_id_path"));
        assert!(sql_is_ddl("  create unique index idx ON t(a)"));
        assert!(sql_is_ddl("CREATE TEMP TABLE temp.x (id INTEGER)"));
        assert!(sql_is_ddl("ALTER TABLE t ADD COLUMN c TEXT"));
        assert!(!sql_is_ddl("INSERT INTO subfiles (file_id) VALUES (1)"));
        assert!(!sql_is_ddl("SELECT 1"));
        assert!(!sql_is_ddl("BEGIN"));
        assert!(!sql_is_ddl("  update files SET length = 1"));
    }
}
