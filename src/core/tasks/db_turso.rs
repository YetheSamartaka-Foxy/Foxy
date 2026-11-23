//! Tuned connections enable foreign keys and are pooled because opening one
//! rebuilds its pager; task cancellation remains cooperative.
#![allow(dead_code)]

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

static CORRUPT_REBUILD_REQUIRES_DISCARD: Mutex<Option<PathBuf>> = Mutex::new(None);

pub(crate) fn corrupt_rebuild_requires_discard() -> bool {
    CORRUPT_REBUILD_REQUIRES_DISCARD
        .lock()
        .ok()
        .is_some_and(|slot| {
            slot.as_ref()
                .is_some_and(|path| *path == database_file_path())
        })
}

pub(crate) fn discard_corrupt_database() -> Result<(), String> {
    let path = database_file_path();
    if !crate::core::tasks::db_process_lock::holds(&path) {
        return Err("The database is not owned by this process".to_string());
    }
    for name in DATABASE_ARTIFACT_FILE_NAMES {
        let artifact = path
            .parent()
            .ok_or("Database directory is unavailable")?
            .join(name);
        match fs::remove_file(artifact) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Could not discard the corrupt database: {err}")),
        }
    }
    if let Ok(mut slot) = CORRUPT_REBUILD_REQUIRES_DISCARD.lock() {
        *slot = None;
    }
    Ok(())
}
mod lifecycle;
pub(crate) use lifecycle::{
    close_active_database, init_turso_database, init_turso_database_with_path,
    resume_database_access,
};

#[cfg(test)]
#[path = "db_turso/round2_benches.rs"]
mod round2_benches;

pub(crate) const TURSO_BOOTSTRAP_SCHEMA: &str = include_str!("../../../sql/turso_schema.sql");

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

pub(crate) const SUBFILES_INDEX_CREATE_SQL: [&str; 1] =
    ["CREATE UNIQUE INDEX IF NOT EXISTS idx_subfiles_file_id_path ON subfiles(file_id, path)"];

pub(crate) const SUBFILES_INDEX_NAMES: [&str; 1] = ["idx_subfiles_file_id_path"];

const DB_BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
const DB_MAX_RETRIES: usize = 5;

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

pub(crate) async fn build_and_bootstrap(path: &str) -> turso::Result<Database> {
    let db = Builder::new_local(path).build().await?;

    let conn = connect_tuned(&db).await?;
    apply_schema(conn.raw(), TURSO_BOOTSTRAP_SCHEMA).await?;
    Ok(db)
}

/// Strip SQL comments before statement-by-statement execution because inline schema notes can confuse batch tokenization.
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

pub(crate) async fn wipe_and_rebuild_live(db: &Database) -> turso::Result<()> {
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
        "file_subfiles",
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

/// Default to single-writer WAL; MVCC remains opt-in because it performed worse for Foxy's write workload.
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

pub(crate) async fn read_journal_mode(conn: &Connection) -> Option<String> {
    let mut rows = conn.query("PRAGMA journal_mode", ()).await.ok()?;
    let row = rows.next().await.ok()??;
    row.get::<String>(0).ok()
}

const MAX_CACHED_STATEMENTS: usize = 128;

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

/// Cached programs may reference roots replaced by DDL; changing the epoch
/// retires connections opened against the old schema.
static DB_SCHEMA_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Retire every pooled connection: their cached programs may reference roots the
/// DDL just replaced.
pub(crate) fn bump_schema_epoch() {
    DB_SCHEMA_EPOCH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

fn schema_epoch() -> u64 {
    DB_SCHEMA_EPOCH.load(std::sync::atomic::Ordering::SeqCst)
}

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

pub(crate) fn connection_counters() -> (u64, u64) {
    (
        DB_CONNECTIONS_OPENED.load(std::sync::atomic::Ordering::Relaxed),
        DB_CONNECTIONS_REUSED.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Clones share one connection; callers must not issue statements concurrently on it.
#[derive(Clone)]
pub(crate) struct TunedConnection {
    conn: Connection,
    admission: Arc<Mutex<StatementAdmission>>,
    epoch: u64,
}

#[derive(Default)]
struct StatementAdmission {
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

    pub(crate) fn raw(&self) -> &Connection {
        &self.conn
    }

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

/// Checkout owns the connection until drop, preventing concurrent use of one Turso connection.
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

/// Idle connections keep the database alive; drain pools before releasing or deleting a game space.
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

fn drain_connection_pool(database: &Arc<Database>) {
    let mut pools = CONNECTION_POOLS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    pools.retain(|(weak, pool)| {
        if weak.as_ptr() == Arc::as_ptr(database) {
            pool.idle
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
            false
        } else {
            true
        }
    });
}

pub(crate) struct PooledConnection {
    conn: TunedConnection,
    pool: Arc<ConnectionPool>,
    retire: bool,

    _database: Arc<Database>,
}

impl PooledConnection {
    /// Retire connections whose scoped settings changed so they cannot leak to the next borrower.
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

pub(crate) async fn connect_pooled(db: &Arc<Database>) -> turso::Result<PooledConnection> {
    let pool = pool_for(db);
    if let Some(conn) = pool.take() {
        DB_CONNECTIONS_REUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return Ok(PooledConnection {
            _database: db.clone(),
            conn,
            pool,
            retire: false,
        });
    }
    let conn = connect_tuned(db.as_ref()).await?;
    Ok(PooledConnection {
        _database: db.clone(),
        conn,
        pool,
        retire: false,
    })
}

/// Use pooling except for bootstrap, compaction, or callers changing connection-scoped pragmas.
pub(crate) async fn connect_tuned(db: &Database) -> turso::Result<TunedConnection> {
    DB_CONNECTIONS_OPENED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let conn = TunedConnection::new(db.connect()?);

    conn.pragma_update("foreign_keys", "ON").await?;
    conn.pragma_update("synchronous", "NORMAL").await?;
    conn.pragma_update("temp_store", "MEMORY").await?;
    conn.pragma_update("cache_size", "-16384").await?; // Explicitly set WAL to migrate files that may retain the old MVCC mode; initialize before concurrent access.
    if mvcc_enabled() {
        conn.pragma_update("journal_mode", "mvcc").await?;
    } else {
        conn.pragma_update("journal_mode", "wal").await?;
    }

    conn.busy_timeout(DB_BUSY_TIMEOUT)?;
    Ok(conn)
}

// Compaction rebuilds dense B-trees; keep row-copy until VACUUM INTO passes a large-file probe.
// Compact only large files with substantial free-page bloat.
const COMPACT_MIN_FREE_PAGES: i64 = 20_000;
const COMPACT_MIN_FREE_RATIO: f64 = 0.5;

const COMPACT_AUTO_MAX_LIVE_MIB: f64 = 512.0;

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

fn compact_mode() -> String {
    std::env::var("FOXY_DB_COMPACT")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

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

fn sweep_stale_db_artifacts(path: &Path) {
    let compacting = with_suffix(path, ".compacting");
    let bak = with_suffix(path, ".bak");
    // A failed swap must preserve the original database; otherwise reopening can silently create an empty one.
    if !path.exists() && bak.exists() && compacting.exists() {
        warn!(
            "STARTUP: restoring {} from an interrupted compaction swap",
            sanitize_log_path(path)
        );
        match fs::rename(&bak, path) {
            Ok(()) => {
                for suffix in ["-wal", "-shm"] {
                    let from = with_suffix(&bak, suffix);
                    if from.exists() {
                        let to = with_suffix(path, suffix);
                        let _ = fs::remove_file(&to);
                        if let Err(err) = fs::rename(&from, &to) {
                            warn!(
                                "STARTUP: could not restore database sidecar {}: {}",
                                sanitize_log_path(&from),
                                err
                            );
                        }
                    }
                }
            }
            Err(err) => warn!(
                "STARTUP: could not restore the database backup {}: {}",
                sanitize_log_path(&bak),
                err
            ),
        }
    }
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
        Err(first_err) if !matches!(first_err, Error::Corrupt(_) | Error::NotAdb(_)) => {
            warn!(
                "STARTUP: Turso database initialization failed ({}); preserving database artifacts",
                first_err
            );
            Err(first_err)
        }
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
                        "STARTUP: could not preserve corrupt database ({}); explicit discard is required",
                        move_err
                    );
                    if let Ok(mut slot) = CORRUPT_REBUILD_REQUIRES_DISCARD.lock() {
                        *slot = Some(path.to_path_buf());
                    }
                    return Err(first_err);
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

/// Keep row-copy compaction until `VACUUM INTO` passes a large-file probe.
async fn build_compacted_copy(path: &Path) -> turso::Result<PathBuf> {
    let tmp = with_suffix(path, ".compacting");
    remove_db_artifacts(&tmp);
    let tmp_str = tmp.to_string_lossy().to_string();

    let src_db = Builder::new_local(&path.to_string_lossy()).build().await?;
    let src = connect_tuned(&src_db).await?;

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
    // Move WAL/SHM sidecars with the database so committed rows remain available after the swap.
    let _ = dst.execute("PRAGMA wal_checkpoint(TRUNCATE)", ()).await;

    drop(dst);
    drop(dst_db);
    drop(src);
    drop(src_db);
    Ok(tmp)
}

/// Any failure leaves the original intact; call only after dropping the live database handle.
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

/// Restore the original on swap failure; call only after dropping the live database handle.
async fn swap_compacted_file(path: &Path, tmp: &Path) -> bool {
    let bak = with_suffix(path, ".bak");

    remove_db_artifacts(&bak);

    if let Err(e) = rename_with_retry(path, &bak).await {
        warn!(
            "STARTUP: could not move old database aside ({}); discarding compacted copy",
            e
        );
        remove_db_artifacts(tmp);
        return false;
    }
    let _ = move_db_sidecars(path, &bak).await;

    // Keep the original available until the compacted replacement is installed.
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

static DB_STARTUP_COMPACTION_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static DB_STARTUP_COMPACTION_STARTED: std::sync::Mutex<Option<Instant>> =
    std::sync::Mutex::new(None);

pub(crate) fn db_startup_compaction_active() -> bool {
    DB_STARTUP_COMPACTION_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

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

async fn open_and_prepare_database(path: &Path) -> Arc<Database> {
    let init_start = Instant::now();
    let path_str = path.to_string_lossy().to_string();
    info!("Ensuring Turso database {}", sanitize_log_path(path));

    // Enforce the process claim at the database seam because Turso does not support multi-process access.
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
            crate::core::tasks::db_process_lock::LockOutcome::Unavailable(_) => {
                panic!("Refusing to open database: exclusive access could not be verified");
            }
        }
    }

    sweep_stale_db_artifacts(path);
    crate::core::tasks::db_schema_version::bootstrap_if_fresh(path);

    let mut db = build_or_rebuild_after_failure(path, &path_str)
        .await
        .unwrap_or_else(|e| {
            log::error!("Failed to initialize Turso database: {}", e);
            panic!("Failed to initialize Turso database: {}", e);
        });

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

    // Verify single-writer WAL on open unless MVCC was explicitly enabled.
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

    // CREATE IF NOT EXISTS does not repair old schemas; probe the live database before syncing.
    let _ = crate::core::tasks::db_schema_check::probe_database(path, &db).await;

    info!(
        "STARTUP: Turso database initialized in {:.2}s",
        init_start.elapsed().as_secs_f64()
    );
    Arc::new(db)
}

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

/// Same-connection overlap cannot be fixed by waiting; let the in-flight statement finish instead of retrying.
fn db_error_is_same_connection_overlap(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("sql statements in progress")
}

pub(crate) fn db_error_message_is_retryable(message: &str) -> bool {
    if db_error_is_same_connection_overlap(message) {
        return false;
    }
    let m = message.to_ascii_lowercase();
    m.contains("conflict")
        || m.contains("busy")
        || m.contains("locked")
        || m.contains("no transaction is active")
        || m.contains("snapshot is stale")
}

/// Retry transient lock/conflict errors, but not same-connection statement overlap.
pub(crate) fn db_is_retryable(err: &Error) -> bool {
    match err {
        Error::Busy(msg) if !db_error_is_same_connection_overlap(msg) => true,
        Error::BusySnapshot(_) => true,
        Error::Error(msg) => db_error_message_is_retryable(msg),
        _ => false,
    }
}

fn db_retry_backoff(attempt: usize) -> Duration {
    Duration::from_millis(50 * 2u64.saturating_pow(attempt as u32))
}

/// Transaction state belongs to the connection; retry the whole transaction after transient conflicts.
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

    #[test]
    fn an_interrupted_compaction_swap_restores_the_original_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        std::fs::write(with_suffix(&path, ".bak"), b"original").unwrap();
        std::fs::write(with_suffix(&path, ".bak-wal"), b"original wal").unwrap();
        std::fs::write(with_suffix(&path, ".compacting"), b"compacted").unwrap();

        sweep_stale_db_artifacts(&path);

        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(
            std::fs::read(with_suffix(&path, "-wal")).unwrap(),
            b"original wal"
        );
        assert!(!with_suffix(&path, ".bak").exists());
        assert!(!with_suffix(&path, ".compacting").exists());
    }

    #[test]
    fn a_kept_backup_is_not_restored_over_a_missing_database_without_a_swap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        std::fs::write(with_suffix(&path, ".bak"), b"older").unwrap();

        sweep_stale_db_artifacts(&path);

        assert!(!path.exists());
        assert!(with_suffix(&path, ".bak").exists());
    }

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
        assert!(db_error_message_is_retryable(
            "database snapshot is stale, rollback and retry the transaction"
        ));
        assert!(db_error_message_is_retryable(
            "turso: database snapshot is stale, rollback and retry the transaction"
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
    async fn initialization_failure_preserves_existing_database_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let path_str = path.to_str().unwrap();
        let db = Builder::new_local(path_str).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE addons (id INTEGER PRIMARY KEY, marker TEXT)",
            (),
        )
        .await
        .unwrap();
        conn.execute("INSERT INTO addons VALUES (1, 'keep')", ())
            .await
            .unwrap();
        drop(conn);
        drop(db);

        assert!(
            build_or_rebuild_after_failure(&path, path_str)
                .await
                .is_err()
        );

        let db = Builder::new_local(path_str).build().await.unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query("SELECT marker FROM addons WHERE id = 1", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "keep"
        );
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("rebuild-backup")
        }));
    }

    #[tokio::test]
    async fn initialization_reuses_existing_database_rows() {
        let (dir, db) = temp_db().await;
        let conn = db.connect().unwrap();
        conn.execute(
            "INSERT INTO repositories (name, remote_url, local_path) VALUES ('RepoAlpha', 'https://repo.example.invalid/', 'R:/Mods/RepoAlpha')",
            (),
        )
        .await
        .unwrap();
        drop(conn);
        drop(db);

        let path = dir.path().join("database.db");
        let db = build_or_rebuild_after_failure(&path, path.to_str().unwrap())
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let mut rows = conn
            .query("SELECT name FROM repositories", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "RepoAlpha"
        );
        assert!(rows.next().await.unwrap().is_none());
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

    #[tokio::test]
    async fn live_wipe_removes_legacy_part_links() {
        let db = build_test_database().await;
        let conn = connect_tuned(&db).await.unwrap();
        apply_schema(
            conn.raw(),
            include_str!("../../../migrations/07_files_parts.sql"),
        )
        .await
        .unwrap();
        drop(conn);

        wipe_and_rebuild_live(&db).await.unwrap();

        let conn = connect_tuned(&db).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'file_subfiles'",
                (),
            )
            .await
            .unwrap();
        assert!(rows.next().await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_fresh_insert_vs_upsert() {
        const ROWS: usize = 66_336;
        const PARAMS_PER_ROW: usize = 6;

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

    #[tokio::test]
    async fn bootstrap_creates_all_tables() {
        let (_dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();
        let expected = [
            "repositories",
            "pending_updates",
            "storage_read_measurements",
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

        for name in SUBFILES_INDEX_NAMES {
            conn.execute(&format!("DROP INDEX IF EXISTS {name}"), ())
                .await
                .unwrap();
        }

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

        for sql in SUBFILES_INDEX_CREATE_SQL {
            conn.execute(sql, ()).await.unwrap();
        }

        let mut rows = conn
            .query("SELECT COUNT(*) FROM subfiles", ())
            .await
            .unwrap();
        let n = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(n, 500, "all bulk-loaded rows must be present");

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn probe_vacuum_into() {
        let (dir, db) = temp_db().await;
        let conn = connect_tuned(&db).await.unwrap();

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

        assert!(with_suffix(&path, ".bak").exists(), "original kept as .bak");
    }

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

        conn.pragma_update("foreign_keys", fk.as_str())
            .await
            .expect("fk");
        eprintln!("[diag-fk] repo_like={repo_like} foreign_keys={fk}");

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
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let foxy = FoxyDb::from_turso(db.clone());
                    let mut n: i64 = 0;
                    while !stop.load(Ordering::Relaxed) {
                        if bypass {
                            if let Ok(conn) = connect_tuned(&db).await
                                && let Ok(mut rows) =
                                    conn.query("SELECT COUNT(*) FROM subfiles", ()).await
                            {
                                let _ = rows.next().await;
                            }
                        } else {
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_connection_reuse() {
        const ROWS: usize = 20_000;
        const CALLS: usize = 2_000;
        let (_dir, db) = temp_db().await;

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

        let scoped = format!(
            "SELECT {COLUMNS} FROM subfiles WHERE file_id IN ({in_list})              ORDER BY file_id ASC, data_order ASC, id ASC"
        );
        let scoped_binds: Vec<crate::core::db::DbValue> = (1..=FILES as i64)
            .map(crate::core::db::DbValue::from)
            .collect();
        let sql = bare.clone();
        let shared = Arc::new(db);
        let fdb = crate::core::db::FoxyDb::from_turso(shared.clone());

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_subfiles_checksum_encoding() {
        const ROWS: usize = 433_248;
        const FILES: usize = 3_738;
        const CHUNK: usize = 256;

        fn plain_sql(n: usize) -> String {
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; n].join(", ");
            format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            )
        }

        fn checksum(file_id: i64, order: i64) -> [u8; 32] {
            let mut out = [0u8; 32];
            let mut state = (file_id as u64) << 32 ^ order as u64 ^ 0x9e37_79b9_7f4a_7c15;
            for byte in &mut out {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            out
        }

        for (label, blob, foreign_keys) in [
            ("hex_text", false, "ON"),
            ("blob_32", true, "ON"),
            ("hex_fk_off", false, "OFF"),
        ] {
            for round in 0..2 {
                let (_dir, db) = temp_db().await;
                let conn = connect_tuned(&db).await.unwrap();
                conn.pragma_update("foreign_keys", foreign_keys)
                    .await
                    .unwrap();
                for file in 1..=FILES {
                    conn.execute(
                        "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                        (file as i64, format!("f{file}"), "rp", "lp"),
                    )
                    .await
                    .unwrap();
                }
                let per_file = (ROWS / FILES) as i64;
                let rows: Vec<(i64, i64)> = (1..=FILES as i64)
                    .flat_map(|file| (0..per_file).map(move |order| (file, order)))
                    .collect();
                let mut statements = Vec::new();
                for chunk in rows.chunks(CHUNK) {
                    let mut binds = Vec::with_capacity(chunk.len() * 6);
                    for (file_id, order) in chunk {
                        binds.push(turso::Value::Integer(*file_id));
                        binds.push(turso::Value::Text(format!(
                            "a3/data_f/lod/texture_{order:05}_co.paa\u{1F}{order}"
                        )));
                        binds.push(turso::Value::Integer(65_536));
                        binds.push(turso::Value::Integer(order * 65_536));
                        let sum = checksum(*file_id, *order);
                        binds.push(if blob {
                            turso::Value::Blob(sum.to_vec())
                        } else {
                            turso::Value::Text(
                                sum.iter().map(|byte| format!("{byte:02X}")).collect(),
                            )
                        });
                        binds.push(turso::Value::Integer(*order));
                    }
                    statements.push((plain_sql(chunk.len()), binds));
                }
                let started = Instant::now();
                conn.execute("BEGIN", ()).await.unwrap();
                for (sql, binds) in statements {
                    conn.execute(&sql, binds).await.unwrap();
                }
                conn.execute("COMMIT", ()).await.unwrap();
                let total = started.elapsed().as_secs_f64();
                println!(
                    "[bench subfiles-checksum] {label:<9} round={round} total={total:.3}s per_row_us={:.2}",
                    total * 1_000_000.0 / rows.len() as f64
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_subfiles_bulk_knobs() {
        const ROWS: usize = 433_248;
        const FILES: usize = 3_738;

        fn plain_sql(n: usize) -> String {
            let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; n].join(", ");
            format!(
                "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
                 remote_start, local_checksum, remote_checksum, data_order) VALUES {ph}"
            )
        }

        let arms: [(&str, &str, usize); 6] = [
            ("cache16m_chunk256", "-16384", 256),
            ("cache256m_chunk256", "-262144", 256),
            ("cache1g_chunk256", "-1048576", 256),
            ("cache16m_chunk64", "-16384", 64),
            ("cache16m_chunk1024", "-16384", 1024),
            ("cache256m_chunk1024", "-262144", 1024),
        ];
        for round in 0..2 {
            for (label, cache, chunk_rows) in arms {
                let (_dir, db) = temp_db().await;
                let conn = connect_tuned(&db).await.unwrap();
                conn.pragma_update("foreign_keys", "OFF").await.unwrap();
                conn.pragma_update("cache_size", cache).await.unwrap();
                for file in 1..=FILES {
                    conn.execute(
                        "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                        (file as i64, format!("f{file}"), "rp", "lp"),
                    )
                    .await
                    .unwrap();
                }
                let per_file = (ROWS / FILES) as i64;
                let rows: Vec<(i64, i64)> = (1..=FILES as i64)
                    .flat_map(|file| (0..per_file).map(move |order| (file, order)))
                    .collect();
                let mut statements = Vec::new();
                for chunk in rows.chunks(chunk_rows) {
                    let mut binds = Vec::with_capacity(chunk.len() * 6);
                    for (file_id, order) in chunk {
                        binds.push(turso::Value::Integer(*file_id));
                        binds.push(turso::Value::Text(format!(
                            "a3/data_f/lod/texture_{order:05}_co.paa\u{1F}{order}"
                        )));
                        binds.push(turso::Value::Integer(65_536));
                        binds.push(turso::Value::Integer(order * 65_536));
                        binds.push(turso::Value::Text(format!(
                            "{:064X}",
                            (*file_id as u128) << 64 | *order as u128
                        )));
                        binds.push(turso::Value::Integer(*order));
                    }
                    statements.push((plain_sql(chunk.len()), binds));
                }
                let started = Instant::now();
                conn.execute("BEGIN", ()).await.unwrap();
                for (sql, binds) in statements {
                    conn.execute(&sql, binds).await.unwrap();
                }
                let insert_s = started.elapsed().as_secs_f64();
                let commit_started = Instant::now();
                conn.execute("COMMIT", ()).await.unwrap();
                let commit_s = commit_started.elapsed().as_secs_f64();
                let checkpoint_started = Instant::now();
                let mut checkpoint = conn
                    .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
                    .await
                    .unwrap();
                while checkpoint.next().await.unwrap().is_some() {}
                let checkpoint_s = checkpoint_started.elapsed().as_secs_f64();
                println!(
                    "[bench subfiles-knobs] {label:<20} round={round} insert={insert_s:.3}s \
                     commit={commit_s:.3}s checkpoint={checkpoint_s:.3}s"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_subfiles_real_manifest_order() {
        let Ok(dir) = std::env::var("FOXY_BENCH_MANIFEST_DIR") else {
            println!("[bench subfiles-real-order] FOXY_BENCH_MANIFEST_DIR is not set");
            return;
        };
        let mut rows: Vec<(i64, String, i64, i64, String, i64)> = Vec::new();
        let mut file_id = 0i64;
        let mut bodies: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "body"))
            .collect();
        bodies.sort();
        for body in bodies {
            let manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(body).unwrap()).unwrap();
            for file in manifest["files"].as_array().into_iter().flatten() {
                file_id += 1;
                for (order, part) in file["parts"].as_array().into_iter().flatten().enumerate() {
                    rows.push((
                        file_id,
                        crate::core::models::modification_file_part::part_storage_path(
                            part["path"].as_str().unwrap_or_default(),
                            order as i64,
                        ),
                        part["length"].as_i64().unwrap_or_default(),
                        part["start"].as_i64().unwrap_or_default(),
                        part["checksum"].as_str().unwrap_or_default().to_owned(),
                        order as i64,
                    ));
                }
            }
        }
        let in_path_order = rows
            .windows(2)
            .filter(|pair| pair[0].0 == pair[1].0 && pair[0].1 <= pair[1].1)
            .count();
        println!(
            "[bench subfiles-real-order] files={file_id} rows={} adjacent_in_path_order={in_path_order}",
            rows.len()
        );
        for round in 0..2 {
            for label in ["data_order", "path"] {
                let mut sorted = rows.clone();
                if label == "path" {
                    sorted.sort_unstable_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
                }
                let (_dir, db) = temp_db().await;
                let conn = connect_tuned(&db).await.unwrap();
                conn.pragma_update("foreign_keys", "OFF").await.unwrap();
                conn.pragma_update("cache_size", "-262144").await.unwrap();
                for id in 1..=file_id {
                    conn.execute(
                        "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                        (id, format!("f{id}"), "rp", "lp"),
                    )
                    .await
                    .unwrap();
                }
                let started = Instant::now();
                conn.execute("BEGIN", ()).await.unwrap();
                for chunk in sorted.chunks(256) {
                    let ph = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; chunk.len()].join(", ");
                    let sql = format!(
                        "INSERT INTO subfiles (file_id, path, local_length, local_start, \
                         remote_length, remote_start, local_checksum, remote_checksum, \
                         data_order) VALUES {ph}"
                    );
                    let mut binds = Vec::with_capacity(chunk.len() * 6);
                    for (file_id, path, length, start, checksum, order) in chunk {
                        binds.push(turso::Value::Integer(*file_id));
                        binds.push(turso::Value::Text(path.clone()));
                        binds.push(turso::Value::Integer(*length));
                        binds.push(turso::Value::Integer(*start));
                        binds.push(turso::Value::Text(checksum.clone()));
                        binds.push(turso::Value::Integer(*order));
                    }
                    conn.execute(&sql, binds).await.unwrap();
                }
                conn.execute("COMMIT", ()).await.unwrap();
                println!(
                    "[bench subfiles-real-order] sorted_by={label:<10} round={round} total={:.3}s",
                    started.elapsed().as_secs_f64()
                );
            }
        }
    }

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

    #[tokio::test]
    async fn pooled_connection_is_reused_after_release() {
        let (_dir, db) = temp_db().await;
        let db = Arc::new(db);

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
