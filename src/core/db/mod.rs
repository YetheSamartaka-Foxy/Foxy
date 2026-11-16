#![allow(dead_code)] // Seam surface intentionally exposes a few unused conveniences.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

mod error;
mod row;
mod value;

pub(crate) use error::DbErr;
pub(crate) use row::DbRow;
pub(crate) use value::{DbValue, params};

use crate::core::tasks::db_turso::{PooledConnection, TunedConnection};

pub(crate) type DbHandle = Arc<turso::Database>;

#[derive(Clone)]
pub(crate) struct FoxyDb {
    db: Arc<turso::Database>,
}

impl FoxyDb {
    pub(crate) fn from_turso(db: Arc<turso::Database>) -> Self {
        FoxyDb { db }
    }

    pub(crate) fn from_handle(db: DbHandle) -> Self {
        FoxyDb { db }
    }

    pub(crate) async fn execute(&self, sql: &str, params: Vec<DbValue>) -> Result<u64, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute(&conn, sql, params).await
    }

    pub(crate) async fn execute_insert(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<i64, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute_insert(&conn, sql, params).await
    }

    /// Writes must be idempotent because transient errors can retry them.
    /// WAL uses atomic autocommit; MVCC uses BEGIN CONCURRENT so conflicts can roll back.
    pub(crate) async fn execute_retry(
        &self,
        label: &'static str,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<u64, DbErr> {
        const MAX_RETRIES: usize = 5;
        // Acquire the shared barrier before a connection so exclusive purge drains this call too.
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        let mvcc = crate::core::tasks::db_turso::mvcc_enabled();
        // Release the write gate before retry backoff, then re-acquire it.
        let (mut gate, mut gate_wait) =
            crate::core::tasks::init_database::acquire_db_write_gate().await;
        // txn_ms starts after the write gate and includes Turso's internal single-writer wait, unlike permit_wait_ms.
        let metric_baseline = crate::core::tasks::init_database::sqlite_perf_snapshot();
        let metric_started = std::time::Instant::now();
        let mut attempt = 0;
        loop {
            let step: Result<u64, DbErr> = async {
                if mvcc {
                    turso_execute(&conn, "BEGIN CONCURRENT", Vec::new()).await?;
                    let n = turso_execute(&conn, sql, params.clone()).await?;
                    turso_execute(&conn, "COMMIT", Vec::new()).await?;
                    Ok(n)
                } else {
                    turso_execute(&conn, sql, params.clone()).await
                }
            }
            .await;
            match step {
                Ok(n) => {
                    crate::core::tasks::init_database::record_db_transaction_metrics(
                        label,
                        true,
                        gate_wait,
                        metric_started.elapsed(),
                        metric_baseline,
                    );
                    return Ok(n);
                }
                Err(e) if attempt < MAX_RETRIES && dberr_is_retryable(&e) => {
                    if mvcc {
                        let _ = turso_execute(&conn, "ROLLBACK", Vec::new()).await;
                    }
                    drop(gate);
                    let backoff =
                        std::time::Duration::from_millis(50 * 2u64.saturating_pow(attempt as u32));
                    tokio::time::sleep(backoff).await;
                    let (reacquired, extra_wait) =
                        crate::core::tasks::init_database::acquire_db_write_gate().await;
                    gate = reacquired;
                    gate_wait += extra_wait;
                    attempt += 1;
                }
                Err(e) => {
                    if mvcc {
                        let _ = turso_execute(&conn, "ROLLBACK", Vec::new()).await;
                    }
                    crate::core::tasks::init_database::record_db_transaction_metrics(
                        label,
                        false,
                        gate_wait,
                        metric_started.elapsed(),
                        metric_baseline,
                    );
                    return Err(e);
                }
            }
        }
    }

    pub(crate) async fn query_all(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<Vec<DbRow>, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_query_all(&conn, sql, params).await
    }

    pub(crate) async fn query_one(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<Option<DbRow>, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        Ok(turso_query_all(&conn, sql, params)
            .await?
            .into_iter()
            .next())
    }

    /// Consistent read snapshots take no write permit and do not retry, so independent readers remain concurrent.
    pub(crate) async fn read_transaction<T, F>(&self, work: F) -> Result<T, DbErr>
    where
        F: for<'a> FnOnce(
            &'a DbTxn<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<T, DbErr>> + Send + 'a>>,
        T: Send,
    {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute(&conn, "BEGIN", Vec::new()).await?;
        let dbtxn = DbTxn(&conn);
        match work(&dbtxn).await {
            Ok(v) => {
                turso_execute(&conn, "COMMIT", Vec::new()).await?;
                Ok(v)
            }
            Err(e) => {
                let _ = turso_execute(&conn, "ROLLBACK", Vec::new()).await;
                Err(e)
            }
        }
    }

    /// The caller owns commit/rollback; retain the write permit until the owned transaction settles.
    pub(crate) async fn begin(&self) -> Result<OwnedDbTxn, DbErr> {
        // Shared barrier first so a running purge drains this writer too; rides in
        // the returned handle and releases with the permit on commit/rollback/drop.
        let shared = crate::core::tasks::init_database::acquire_db_shared().await;
        // Hold the write gate through commit/rollback/drop so caller-driven transactions share Turso's single writer safely.
        let (permit, _gate_wait) = crate::core::tasks::init_database::acquire_db_write_gate().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute(&conn, "BEGIN", Vec::new()).await?;
        Ok(OwnedDbTxn {
            conn,
            _permit: permit,
            _barrier: OwnedDbBarrierGuard::Shared(shared),
        })
    }

    /// Schema bulk operations require exclusive reader/writer admission while retaining normal foreign-key enforcement.
    pub(crate) async fn begin_exclusive(&self) -> Result<OwnedDbTxn, DbErr> {
        let exclusive = crate::core::tasks::init_database::acquire_db_exclusive().await;
        let (permit, _gate_wait) = crate::core::tasks::init_database::acquire_db_write_gate().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute(&conn, "BEGIN", Vec::new()).await?;
        Ok(OwnedDbTxn {
            conn,
            _permit: permit,
            _barrier: OwnedDbBarrierGuard::Exclusive(exclusive),
        })
    }

    pub(crate) async fn transaction<F>(&self, label: &str, work: F) -> Result<(), DbErr>
    where
        F: for<'a> Fn(
            &'a DbTxn<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
    {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        turso_transaction(&self.db, label, false, false, false, work).await
    }

    /// Bulk inserts disable foreign keys on a dedicated connection; the caller must verify parents before commit.
    pub(crate) async fn bulk_insert_transaction<F>(&self, label: &str, work: F) -> Result<(), DbErr>
    where
        F: for<'a> Fn(
            &'a DbTxn<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
    {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        turso_transaction(&self.db, label, true, false, true, work).await
    }

    /// Repository purge requires exclusive DB access and a dedicated foreign_keys=OFF connection.
    pub(crate) async fn transaction_exclusive<F>(&self, label: &str, work: F) -> Result<(), DbErr>
    where
        F: for<'a> Fn(
            &'a DbTxn<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
    {
        let _exclusive = crate::core::tasks::init_database::acquire_db_exclusive().await;
        // Purge deletes children first; FK enforcement would redundantly scan surviving child tables for each deleted parent.
        turso_transaction(&self.db, label, true, true, false, work).await
    }
}

pub(crate) struct DbTxn<'a>(&'a TunedConnection);

impl DbTxn<'_> {
    pub(crate) async fn execute(&self, sql: &str, params: Vec<DbValue>) -> Result<u64, DbErr> {
        turso_execute(self.0, sql, params).await
    }

    pub(crate) async fn execute_insert(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<i64, DbErr> {
        turso_execute_insert(self.0, sql, params).await
    }

    pub(crate) async fn query_all(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<Vec<DbRow>, DbErr> {
        turso_query_all(self.0, sql, params).await
    }

    pub(crate) async fn query_one(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<Option<DbRow>, DbErr> {
        Ok(turso_query_all(self.0, sql, params)
            .await?
            .into_iter()
            .next())
    }

    pub(crate) async fn query_each<F>(
        &self,
        sql: &str,
        params: Vec<DbValue>,
        visit: F,
    ) -> Result<u64, DbErr>
    where
        F: FnMut(&DbRow) -> Result<(), DbErr>,
    {
        turso_query_each(self.0, sql, params, visit).await
    }
}

/// Uncommitted owned transactions roll back when their connection drops; callers drive commit/rollback explicitly.
pub(crate) struct OwnedDbTxn {
    conn: PooledConnection,
    /// Retain the permit until commit/rollback/drop; None is possible only after semaphore closure.
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,

    _barrier: OwnedDbBarrierGuard,
}

impl OwnedDbTxn {
    pub(crate) async fn execute(&self, sql: &str, params: Vec<DbValue>) -> Result<u64, DbErr> {
        turso_execute(&self.conn, sql, params).await
    }

    pub(crate) async fn query_all(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<Vec<DbRow>, DbErr> {
        turso_query_all(&self.conn, sql, params).await
    }

    pub(crate) async fn commit(self) -> Result<(), DbErr> {
        turso_execute(&self.conn, "COMMIT", Vec::new())
            .await
            .map(|_| ())
    }

    pub(crate) async fn rollback(self) -> Result<(), DbErr> {
        turso_execute(&self.conn, "ROLLBACK", Vec::new())
            .await
            .map(|_| ())
    }
}

enum OwnedDbBarrierGuard {
    Shared(tokio::sync::OwnedRwLockReadGuard<()>),
    Exclusive(tokio::sync::OwnedRwLockWriteGuard<()>),
}

fn map_turso_err(e: turso::Error) -> DbErr {
    DbErr::from(e)
}

fn turso_value_to_db(v: turso::Value) -> DbValue {
    match v {
        turso::Value::Null => DbValue::Null,
        turso::Value::Integer(i) => DbValue::Int(i),
        turso::Value::Real(f) => DbValue::Real(f),
        turso::Value::Text(s) => DbValue::Text(s),
        turso::Value::Blob(b) => DbValue::Blob(b),
    }
}

async fn connect_turso(db: &Arc<turso::Database>) -> Result<PooledConnection, DbErr> {
    crate::core::tasks::db_turso::connect_pooled(db)
        .await
        .map_err(map_turso_err)
}

async fn turso_execute(
    conn: &TunedConnection,
    sql: &str,
    params: Vec<DbValue>,
) -> Result<u64, DbErr> {
    let values: Vec<turso::Value> = params.into_iter().map(DbValue::into_turso_value).collect();
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let affected = conn.execute(sql, values).await.map_err(map_turso_err)?;
    profiled.stop_db("write", sql, affected);
    crate::core::tasks::init_database::record_sqlite_rows_affected(sql, affected);
    // Cached programs are compiled against the roots the schema had at prepare
    // time, so DDL has to retire every pooled connection's cache.
    if crate::core::tasks::db_turso::sql_is_ddl(sql) {
        crate::core::tasks::db_turso::bump_schema_epoch();
    }
    Ok(affected)
}

async fn turso_execute_insert(
    conn: &TunedConnection,
    sql: &str,
    params: Vec<DbValue>,
) -> Result<i64, DbErr> {
    turso_execute(conn, sql, params).await?;
    Ok(conn.last_insert_rowid())
}

async fn turso_query_all(
    conn: &TunedConnection,
    sql: &str,
    params: Vec<DbValue>,
) -> Result<Vec<DbRow>, DbErr> {
    let values: Vec<turso::Value> = params.into_iter().map(DbValue::into_turso_value).collect();
    // Timed around the whole drain, not just `query`: the engine streams rows, so
    // the cost of a read is in `next`, not in handing back the cursor.
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let mut rows = conn.query(sql, values).await.map_err(map_turso_err)?;
    let columns = Arc::new(rows.column_names());
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(map_turso_err)? {
        let mut vals = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            vals.push(turso_value_to_db(row.get_value(i).map_err(map_turso_err)?));
        }
        out.push(DbRow {
            columns: columns.clone(),
            values: vals,
        });
    }
    profiled.stop_db("read", sql, out.len() as u64);
    Ok(out)
}

/// Stream mapped rows to avoid retaining a second full raw-row representation during large reads.
async fn turso_query_each<F>(
    conn: &TunedConnection,
    sql: &str,
    params: Vec<DbValue>,
    mut visit: F,
) -> Result<u64, DbErr>
where
    F: FnMut(&DbRow) -> Result<(), DbErr>,
{
    let values: Vec<turso::Value> = params.into_iter().map(DbValue::into_turso_value).collect();
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let mut rows = conn.query(sql, values).await.map_err(map_turso_err)?;
    let columns = Arc::new(rows.column_names());
    let mut seen = 0u64;
    while let Some(row) = rows.next().await.map_err(map_turso_err)? {
        let mut vals = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            vals.push(turso_value_to_db(row.get_value(i).map_err(map_turso_err)?));
        }
        visit(&DbRow {
            columns: columns.clone(),
            values: vals,
        })?;
        seen += 1;
    }
    profiled.stop_db("read", sql, seen);
    Ok(seen)
}

fn dberr_is_retryable(e: &DbErr) -> bool {
    crate::core::tasks::db_turso::db_error_message_is_retryable(&e.to_string())
}

const BULK_INSERT_PAGE_CACHE_KIB: u32 = 262_144;

async fn turso_transaction<F>(
    db: &Arc<turso::Database>,
    label: &str,
    disable_foreign_keys: bool,
    exclusive: bool,
    bulk_page_cache: bool,
    work: F,
) -> Result<(), DbErr>
where
    F: for<'a> Fn(&'a DbTxn<'a>) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
{
    const MAX_RETRIES: usize = 5;
    let mut conn = connect_turso(db).await?;
    // Disable redundant FK scans before BEGIN; changing the pragma inside a transaction is a no-op.
    // Retire this dedicated connection so disabled enforcement never reaches other writers.
    if disable_foreign_keys {
        // The pragma is connection-scoped, so this connection must not go back to
        // the pool with enforcement still off.
        conn.retire();
        turso_execute(&conn, "PRAGMA foreign_keys = OFF", Vec::new()).await?;
    }
    if bulk_page_cache {
        conn.retire();
        turso_execute(
            &conn,
            &format!("PRAGMA cache_size = -{BULK_INSERT_PAGE_CACHE_KIB}"),
            Vec::new(),
        )
        .await?;
    }
    // MVCC conflicts retry BEGIN CONCURRENT; exclusive operations use plain BEGIN because they run DDL without concurrency.
    let begin_sql = if exclusive || !crate::core::tasks::db_turso::mvcc_enabled() {
        "BEGIN"
    } else {
        "BEGIN CONCURRENT"
    };
    // Release the write gate before retry backoff, then re-acquire it.
    let (mut gate, mut gate_wait) =
        crate::core::tasks::init_database::acquire_db_write_gate().await;
    // Starts after the gate, so this is the transaction window (`txn_ms`), which
    // includes any wait on Turso's single writer inside `conn.execute`.
    let metric_baseline = crate::core::tasks::init_database::sqlite_perf_snapshot();
    let metric_started = std::time::Instant::now();
    let mut attempt = 0;
    loop {
        let step: Result<(), DbErr> = async {
            turso_execute(&conn, begin_sql, Vec::new()).await?;
            let dbtxn = DbTxn(&conn);
            work(&dbtxn).await?;
            turso_execute(&conn, "COMMIT", Vec::new()).await?;
            Ok(())
        }
        .await;
        match step {
            Ok(()) => {
                crate::core::tasks::init_database::record_db_transaction_metrics(
                    label,
                    true,
                    gate_wait,
                    metric_started.elapsed(),
                    metric_baseline,
                );
                return Ok(());
            }
            Err(e) if attempt < MAX_RETRIES && dberr_is_retryable(&e) => {
                let _ = turso_execute(&conn, "ROLLBACK", Vec::new()).await;
                drop(gate);
                let backoff =
                    std::time::Duration::from_millis(50 * 2u64.saturating_pow(attempt as u32));
                log::debug!("{label}: retryable DB error (attempt {}): {e}", attempt + 1);
                tokio::time::sleep(backoff).await;
                let (reacquired, extra_wait) =
                    crate::core::tasks::init_database::acquire_db_write_gate().await;
                gate = reacquired;
                gate_wait += extra_wait;
                attempt += 1;
            }
            Err(e) => {
                let _ = turso_execute(&conn, "ROLLBACK", Vec::new()).await;
                crate::core::tasks::init_database::record_db_transaction_metrics(
                    label,
                    false,
                    gate_wait,
                    metric_started.elapsed(),
                    metric_baseline,
                );
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::db_turso::build_test_database;
    use crate::core::tasks::init_database::acquire_db_write_gate;

    async fn test_db() -> FoxyDb {
        FoxyDb::from_turso(build_test_database().await)
    }

    #[tokio::test]
    async fn gated_transaction_commits_and_releases_gate() {
        let db = test_db().await;
        db.transaction("test insert repo", |txn| {
            Box::pin(async move {
                txn.execute(
                    "INSERT INTO repositories (id, name, remote_url, local_path) \
                     VALUES (1, 'n', 'u', 'p')",
                    Vec::new(),
                )
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let row = db
            .query_one("SELECT name FROM repositories WHERE id = 1", Vec::new())
            .await
            .unwrap()
            .expect("inserted row present");
        assert_eq!(row.get_string("name").unwrap(), "n");

        let (permit, _wait) = acquire_db_write_gate().await;
        assert!(
            permit.is_some(),
            "write gate should be acquirable post-commit"
        );
    }

    #[tokio::test]
    async fn owned_txn_releases_gate_on_rollback() {
        let db = test_db().await;
        let txn = db.begin().await.unwrap();
        txn.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) \
             VALUES (2, 'n2', 'u2', 'p2')",
            Vec::new(),
        )
        .await
        .unwrap();
        txn.rollback().await.unwrap();

        let txn2 = db.begin().await.unwrap();
        txn2.commit().await.unwrap();
        let row = db
            .query_one(
                "SELECT COUNT(*) AS c FROM repositories WHERE id = 2",
                Vec::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("c").unwrap(), 0);
    }

    #[tokio::test]
    async fn execute_retry_autocommit_insert_is_visible() {
        let db = test_db().await;
        let affected = db
            .execute_retry(
                "test autocommit insert",
                "INSERT INTO repositories (id, name, remote_url, local_path) \
                 VALUES (3, 'n3', 'u3', 'p3')",
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(affected, 1);

        let row = db
            .query_one("SELECT name FROM repositories WHERE id = 3", Vec::new())
            .await
            .unwrap()
            .expect("autocommit insert visible");
        assert_eq!(row.get_string("name").unwrap(), "n3");
    }

    #[tokio::test]
    async fn a_transaction_retries_when_another_writer_makes_its_snapshot_stale() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let raw = build_test_database().await;
        let db = FoxyDb::from_turso(raw.clone());
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (1, 'n1', 'u1', 'p1')",
            Vec::new(),
        )
        .await
        .unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        db.transaction("stale snapshot", |txn| {
            let attempts = attempts.clone();
            let raw = raw.clone();
            Box::pin(async move {
                txn.query_all("SELECT id FROM repositories", Vec::new())
                    .await?;
                // A raw connection, because a nested seam call would queue on the
                // shared barrier behind any other test's exclusive purge.
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    raw.connect()?
                        .execute("UPDATE repositories SET name = 'other' WHERE id = 1", ())
                        .await?;
                }
                txn.execute("DELETE FROM repositories WHERE id = 1", Vec::new())
                    .await?;
                Ok(())
            })
        })
        .await
        .expect("a stale snapshot must be retried");

        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(
            db.query_all("SELECT id FROM repositories", Vec::new())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
