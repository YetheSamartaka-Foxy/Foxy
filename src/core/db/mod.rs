//! Database seam over the Turso engine (plan.md §5.1).
//!
//! A thin facade over the handful of operations the codebase performs:
//! `execute`, `query_all`, `query_one`, and retrying `transaction`/`execute_retry`
//! That keeps the ~40 DB call sites independent of the concrete engine type.
//! Originally introduced (Phase 1) to decouple call sites from `sea_orm::*`; the
//! Phase-4 cutover removed the SeaORM storage layer, so this now wraps `turso`
//! directly.
//!
//! Call sites read with `db.query_one("SELECT ... WHERE ... = ?", params![..])` +
//! a [`DbRow`] getter, and write with `db.execute("INSERT ...", params![..])`.
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

/// The concrete connection-handle type stored in
/// [`crate::core::models::context::FoxyContext`] and returned by
/// `init_database()` (plan.md §5.1).
pub(crate) type DbHandle = Arc<turso::Database>;

/// The shared database handle.
#[derive(Clone)]
pub(crate) struct FoxyDb {
    db: Arc<turso::Database>,
}

impl FoxyDb {
    pub(crate) fn from_turso(db: Arc<turso::Database>) -> Self {
        FoxyDb { db }
    }

    /// Wrap a storage-native handle (the kind `init_database()` returns) in the
    /// seam. Kept as a distinct name from [`FoxyDb::from_turso`] for the
    /// standalone `init_database()` call sites and `FoxyContext::db()`.
    pub(crate) fn from_handle(db: DbHandle) -> Self {
        FoxyDb { db }
    }

    /// Run a write/DDL statement; returns the number of affected rows.
    pub(crate) async fn execute(&self, sql: &str, params: Vec<DbValue>) -> Result<u64, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute(&conn, sql, params).await
    }

    /// Run an INSERT; returns the rowid of the inserted row.
    pub(crate) async fn execute_insert(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<i64, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute_insert(&conn, sql, params).await
    }

    /// Execute a single write statement with transient-error retry, returning the
    /// number of affected rows. The statement runs in its own `BEGIN CONCURRENT`
    /// transaction when MVCC is enabled; otherwise it uses plain `BEGIN`.
    /// The statement may re-run after rollback, so it must be idempotent.
    pub(crate) async fn execute_retry(
        &self,
        label: &'static str,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<u64, DbErr> {
        const MAX_RETRIES: usize = 5;
        // Shared barrier: coexists with other readers/writers but yields to an
        // exclusive purge (see `DB_EXCLUSIVE`). Acquired before the connection so
        // a running purge drains this call too.
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        let begin_sql = if crate::core::tasks::db_turso::mvcc_enabled() {
            "BEGIN CONCURRENT"
        } else {
            "BEGIN"
        };
        // Release the write gate before retry backoff, then re-acquire it.
        let (mut gate, mut gate_wait) =
            crate::core::tasks::init_database::acquire_db_write_gate().await;
        // Per-category write instrumentation (plan.md §5.4) so this label shows up
        // in the final write-category report again. Timer starts AFTER the gate so
        // `total_ms` is pure write work and `permit_wait_ms` is the gate wait.
        let metric_baseline = crate::core::tasks::init_database::sqlite_perf_snapshot();
        let metric_started = std::time::Instant::now();
        let mut attempt = 0;
        loop {
            let step: Result<u64, DbErr> = async {
                turso_execute(&conn, begin_sql, Vec::new()).await?;
                let n = turso_execute(&conn, sql, params.clone()).await?;
                turso_execute(&conn, "COMMIT", Vec::new()).await?;
                Ok(n)
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
                    let _ = turso_execute(&conn, "ROLLBACK", Vec::new()).await;
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

    /// Run a query and collect all rows.
    pub(crate) async fn query_all(
        &self,
        sql: &str,
        params: Vec<DbValue>,
    ) -> Result<Vec<DbRow>, DbErr> {
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        let conn = connect_turso(&self.db).await?;
        turso_query_all(&conn, sql, params).await
    }

    /// Run a query and return the first row, if any.
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

    /// Run `work` inside a **read** transaction for a consistent snapshot. Takes
    /// no write permit and does not retry - the seam equivalent of a bare
    /// `BEGIN` used by multi-query read paths so concurrent reads are not
    /// serialized.
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

    /// Begin a transaction and return an **owned** handle the caller drives
    /// explicitly (`commit`/`rollback`). The seam equivalent of a bare `BEGIN`
    /// used by the bulk hash-persist path, which hand-rolls its own
    /// begin/insert/update/commit loop with fine-grained perf instrumentation.
    pub(crate) async fn begin(&self) -> Result<OwnedDbTxn, DbErr> {
        // Shared barrier first so a running purge drains this writer too; rides in
        // the returned handle and releases with the permit on commit/rollback/drop.
        let shared = crate::core::tasks::init_database::acquire_db_shared().await;
        // Hold the write gate for the whole caller-driven begin→commit window so
        // this transaction serializes with the seam's other writers on Turso's
        // single internal writer (after_turso_regression_analysis2.md). The permit
        // rides inside the returned handle and releases on commit/rollback/drop.
        let (permit, _gate_wait) = crate::core::tasks::init_database::acquire_db_write_gate().await;
        let conn = connect_turso(&self.db).await?;
        turso_execute(&conn, "BEGIN", Vec::new()).await?;
        Ok(OwnedDbTxn {
            conn,
            _permit: permit,
            _barrier: OwnedDbBarrierGuard::Shared(shared),
        })
    }

    /// Begin an owned transaction while holding the exclusive DB barrier. Use this
    /// for schema-level bulk operations that must not overlap readers, while still
    /// keeping normal foreign key enforcement on.
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

    /// Run idempotent work inside a transaction with transient-error retry.
    pub(crate) async fn transaction<F>(&self, label: &str, work: F) -> Result<(), DbErr>
    where
        F: for<'a> Fn(
            &'a DbTxn<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
    {
        // Shared barrier: coexists with other readers/writers, yields to a purge.
        let _shared = crate::core::tasks::init_database::acquire_db_shared().await;
        turso_transaction(&self.db, label, false, work).await
    }

    /// Like [`FoxyDb::transaction`] but for the repository purge: runs with
    /// `foreign_keys=OFF` (the purge's real hang - see `turso_transaction`) AND
    /// with **exclusive** DB access (no other seam read/write proceeds until it
    /// commits) as defense-in-depth against Turso wedging a long delete overlapped
    /// from another connection/runtime (see [`DB_EXCLUSIVE`]).
    pub(crate) async fn transaction_exclusive<F>(&self, label: &str, work: F) -> Result<(), DbErr>
    where
        F: for<'a> Fn(
            &'a DbTxn<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
    {
        let _exclusive = crate::core::tasks::init_database::acquire_db_exclusive().await;
        // FK enforcement OFF: the purge deletes children before parents, so it is
        // redundant, and ON it makes Turso scan surviving sibling child tables per
        // deleted parent row (the force-redownload wedge). See `turso_transaction`.
        turso_transaction(&self.db, label, true, work).await
    }
}

/// A transaction handle passed to [`FoxyDb::transaction`]'s closure. Statements
/// run on the enclosing connection so they share its atomic scope.
pub(crate) struct DbTxn<'a>(&'a turso::Connection);

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
}

/// An owned transaction handle returned by [`FoxyDb::begin`]. The caller drives
/// `commit`/`rollback` explicitly; dropping without committing leaves the
/// connection's transaction to be rolled back when the connection drops.
pub(crate) struct OwnedDbTxn {
    conn: turso::Connection,
    /// Write-gate permit held for the life of the transaction; released on
    /// commit/rollback (which consume `self`) or on drop. `None` only if the gate
    /// semaphore was closed (never in practice).
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
    /// DB barrier guard (see [`DB_EXCLUSIVE`]); released with the txn.
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

// --- Turso storage layer --------------------------------------------------------

fn map_turso_err(e: turso::Error) -> DbErr {
    DbErr::Custom(format!("turso: {e}"))
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

async fn connect_turso(db: &Arc<turso::Database>) -> Result<turso::Connection, DbErr> {
    crate::core::tasks::db_turso::connect_tuned(db)
        .await
        .map_err(map_turso_err)
}

async fn turso_execute(
    conn: &turso::Connection,
    sql: &str,
    params: Vec<DbValue>,
) -> Result<u64, DbErr> {
    let values: Vec<turso::Value> = params.into_iter().map(DbValue::into_turso_value).collect();
    conn.execute(sql, values).await.map_err(map_turso_err)
}

async fn turso_execute_insert(
    conn: &turso::Connection,
    sql: &str,
    params: Vec<DbValue>,
) -> Result<i64, DbErr> {
    turso_execute(conn, sql, params).await?;
    Ok(conn.last_insert_rowid())
}

async fn turso_query_all(
    conn: &turso::Connection,
    sql: &str,
    params: Vec<DbValue>,
) -> Result<Vec<DbRow>, DbErr> {
    let values: Vec<turso::Value> = params.into_iter().map(DbValue::into_turso_value).collect();
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
    Ok(out)
}

fn dberr_is_retryable(e: &DbErr) -> bool {
    crate::core::tasks::db_turso::db_error_message_is_retryable(&e.to_string())
}

async fn turso_transaction<F>(
    db: &Arc<turso::Database>,
    label: &str,
    disable_foreign_keys: bool,
    work: F,
) -> Result<(), DbErr>
where
    F: for<'a> Fn(&'a DbTxn<'a>) -> Pin<Box<dyn Future<Output = Result<(), DbErr>> + Send + 'a>>,
{
    const MAX_RETRIES: usize = 5;
    let conn = connect_turso(db).await?;
    // The purge deletes child rows before their parents, so FK enforcement is
    // redundant - and catastrophic: with `foreign_keys=ON`, deleting a repo's
    // ~1.5k `files`/`addons` while sibling repos' rows survive makes Turso scan
    // the ~400k-row `subfiles` (and other child) tables ONCE PER DELETED PARENT
    // ROW, wedging `delete files` for minutes (the force-redownload "hang";
    // proven single-connection by `diag_scoped_purge_fk`: FK=ON wedges, FK=OFF
    // = 0.3s). Must be set OUTSIDE any transaction (a no-op inside BEGIN) and
    // before the retry loop; the pragma rides this connection only (connections
    // are per-transaction), so it never weakens other writers' enforcement.
    if disable_foreign_keys {
        turso_execute(&conn, "PRAGMA foreign_keys = OFF", Vec::new()).await?;
    }
    // Stage B (plan.md §5.2): under MVCC, independent write transactions run
    // concurrently via `BEGIN CONCURRENT` - write–write conflicts abort at COMMIT
    // and are retried by this loop (`dberr_is_retryable` matches the conflict
    // variants). Falls back to plain `BEGIN` (single-writer WAL) when MVCC is off.
    let begin_sql = if crate::core::tasks::db_turso::mvcc_enabled() {
        "BEGIN CONCURRENT"
    } else {
        "BEGIN"
    };
    // Release the write gate before retry backoff, then re-acquire it.
    let (mut gate, mut gate_wait) =
        crate::core::tasks::init_database::acquire_db_write_gate().await;
    // Start after the gate so total_ms is write work and permit_wait_ms is queue wait.
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

    /// The gated `transaction` write path commits, and the gate is released after
    /// the transaction so a follow-up acquisition does not block. (If the gate
    /// leaked, the `acquire_db_write_gate()` below would hang under the default
    /// 1-permit gate while other parallel tests hold it.)
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

        // Gate is free again.
        let (permit, _wait) = acquire_db_write_gate().await;
        assert!(
            permit.is_some(),
            "write gate should be acquirable post-commit"
        );
    }

    /// An owned transaction from `begin()` holds the gate and releases it on
    /// rollback - the second `begin()` proceeds (it would deadlock on a 1-permit
    /// gate if the permit leaked) and the rolled-back insert leaves no row.
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

        // Must not block on the gate; rollback must have undone the insert.
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
}
