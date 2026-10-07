//! The `benchmarks` table mirrors the record folders so other tooling can
//! query them through the database. Folders win on every disagreement: the
//! index is rebuilt from them after a wipe or a hand edit.

use std::collections::HashSet;

use crate::core::db::{DbErr, FoxyDb, params};
use crate::core::tasks::init_database::init_database;

use super::record::BenchmarkRecord;

/// Shared with the startup schema probe so an older database that lacks the
/// table is caught before the first save silently fails.
pub(crate) const BENCHMARK_UPSERT_SQL: &str = "INSERT INTO benchmarks (id, created_at, kind, name, repository_url, local_path, outcome, elapsed_ms, favourite, hidden) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
     ON CONFLICT (id) DO UPDATE SET \
     created_at = excluded.created_at, kind = excluded.kind, name = excluded.name, \
     repository_url = excluded.repository_url, local_path = excluded.local_path, \
     outcome = excluded.outcome, elapsed_ms = excluded.elapsed_ms, \
     favourite = excluded.favourite, hidden = excluded.hidden";

async fn upsert(db: &FoxyDb, record: &BenchmarkRecord) -> Result<(), DbErr> {
    db.execute_retry(
        "upsert benchmark",
        BENCHMARK_UPSERT_SQL,
        params![
            &record.id,
            record.started_at,
            record.kind.slug(),
            &record.name,
            &record.repository.url,
            &record.repository.local_path,
            record.outcome.slug(),
            record.elapsed_ms as i64,
            record.favourite,
            record.hidden
        ],
    )
    .await?;
    Ok(())
}

pub async fn upsert_record(record: &BenchmarkRecord) -> Result<(), DbErr> {
    let db = FoxyDb::from_handle(init_database().await);
    upsert(&db, record).await
}

pub async fn delete_record(id: &str) -> Result<(), DbErr> {
    let db = FoxyDb::from_handle(init_database().await);
    db.execute_retry(
        "delete benchmark",
        "DELETE FROM benchmarks WHERE id = ?",
        params![id],
    )
    .await?;
    Ok(())
}

/// Make the table match `records`: insert or refresh every folder, drop rows
/// whose folder is gone.
pub async fn reconcile(records: &[BenchmarkRecord]) -> Result<usize, DbErr> {
    let db = FoxyDb::from_handle(init_database().await);
    let known: HashSet<String> = records.iter().map(|record| record.id.clone()).collect();
    let rows = db.query_all("SELECT id FROM benchmarks", params![]).await?;
    let mut removed = 0usize;
    for row in rows {
        let id = row.get_string("id")?;
        if !known.contains(&id) {
            db.execute_retry(
                "delete stale benchmark",
                "DELETE FROM benchmarks WHERE id = ?",
                params![id],
            )
            .await?;
            removed += 1;
        }
    }
    for record in records {
        upsert(&db, record).await?;
    }
    Ok(removed)
}
