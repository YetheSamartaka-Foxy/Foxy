use super::*;
use crate::core::db::{DbErr, DbValue, FoxyDb, params};

pub fn calculate_hash_from_items<T: HasLocalChecksum>(items: &mut [T]) -> String {
    items.sort_by_key(|item| item.order());

    let first_checksum = items
        .iter()
        .find(|i| !i.local_checksum().is_empty())
        .map(|i| i.local_checksum())
        .unwrap_or("");
    let mut hasher = FlexHasher::from_checksum(first_checksum);

    let mut checker: i64 = -1;
    for item in items.iter() {
        assert!(
            checker <= item.order(),
            "Incorrect order detected (context: {}, items: {}): {} == {}",
            item.local_identifier(),
            items.len(),
            checker + 1,
            item.order()
        );
        checker = item.order();
        hasher.update(item.local_checksum().as_bytes());
    }
    hasher.finalize_hex()
}

fn sqlite_upsert_batch_size(columns_per_row: usize) -> usize {
    (SQLITE_MAX_VARIABLES / columns_per_row)
        .saturating_sub(1)
        .max(1)
}

/// Rows per statement for the bulk part-hash persist `UPDATE … FROM (VALUES …)`.
/// Turso's per-statement cost is superlinear in row count, so a small tuned chunk
/// (~256 rows) is ~3–4× faster than packing thousands of rows per statement
/// (`bench_bulk_update_chunk`: 256 rows ≈ 12 µs/row vs 8 190 rows ≈ 45 µs/row).
/// See [`bulk_write_chunk_rows`]. Capped by the bind-variable ceiling for safety.
fn bulk_persist_update_batch_size(params_per_row: usize) -> usize {
    use crate::core::tasks::init_database::{bulk_write_chunk_rows, sqlite_variable_limit};
    bulk_write_chunk_rows()
        .min(
            (sqlite_variable_limit() / params_per_row)
                .saturating_sub(1)
                .max(1),
        )
        .max(1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CleanPartMark {
    pub(super) file_id: u64,
    pub(super) part_count: usize,
}

#[allow(dead_code)]
fn clean_part_mark_batch_size() -> usize {
    use crate::core::tasks::init_database::sqlite_variable_limit;
    sqlite_variable_limit().saturating_sub(10).max(1)
}

/// Suppress WAL autocheckpoint to avoid mid-bulk-write fsyncs. Returns whether
/// the PRAGMA was successfully set (so the caller can restore it).
async fn suppress_wal_autocheckpoint(db: &FoxyDb) -> bool {
    db.execute("PRAGMA wal_autocheckpoint = 0", params![])
        .await
        .is_ok()
}

/// Restore the default WAL autocheckpoint interval (256 pages).
async fn restore_wal_autocheckpoint(db: &FoxyDb) {
    let _ = db
        .execute("PRAGMA wal_autocheckpoint = 256", params![])
        .await;
}

pub(super) async fn persist_part_checksums<F>(
    db: &FoxyDb,
    part_updates: &[FoxyModFilePart],
    mut on_chunk_persisted: F,
) where
    F: FnMut(usize),
{
    if part_updates.is_empty() {
        return;
    }

    // Every non-empty batch uses the set-based `UPDATE … FROM (VALUES …)` path.
    // The earlier per-row `UPDATE … WHERE id = ?` fast path (used for batches up to
    // 512 rows) issued one statement per row: acceptable under bundled SQLite, but
    // ~7x slower per row under Turso (after_turso_regression_analysis.md §"Hash
    // Persistence Details" / Likely Cause #4 - 106 calls, 20.8k rows, 10.6s). The
    // single chunked CTE statement below is strictly fewer round-trips for the same
    // rows, so it now serves small batches too.
    //
    // Bulk path: one transaction, chunked `UPDATE … FROM (VALUES …)`. Each part
    // row is written exactly once. The previous temp-table strategy wrote every
    // row twice - once into a `_part_hash_batch` temp B-tree, then again into
    // `subfiles` via the join - which roughly doubled the write volume and, under
    // Turso's MVCC commit, dominated the persist cost (measured ~12x slower than
    // the SQLite baseline). A CTE of bound VALUES feeds the same set-based UPDATE
    // without materializing a temp table: no DDL, no second write, no teardown.
    // Turso manages its own WAL, so the old `wal_autocheckpoint`/`wal_checkpoint`
    // PRAGMAs (no-ops on this engine) are gone as well. Callers pre-sort by PK so
    // each batch walks the `subfiles` B-tree roughly sequentially.
    let params_per_row = 4usize;
    let update_batch_size = bulk_persist_update_batch_size(params_per_row);

    let persist_started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();

    // Acquire the write semaphore once for the entire persist - no concurrent
    // writers exist during Phase 2, so holding it for the full duration is safe.
    let write_wait_started = Instant::now();
    let _db_permit = DB_WRITE_SEMAPHORE.clone().acquire_owned().await.ok();
    let write_wait_elapsed = write_wait_started.elapsed();
    let _write_scope = sqlite_labeled_write_scope("persist bulk part hashes");

    let mut attempt = 0;
    let mut begin_elapsed = std::time::Duration::ZERO;
    let mut update_elapsed = std::time::Duration::ZERO;
    let mut update_batch_max_elapsed = std::time::Duration::ZERO;
    let mut commit_elapsed = std::time::Duration::ZERO;
    let mut update_batches = 0usize;
    let mut update_rows_affected = 0u64;
    let mut committed = false;
    const MAX_RETRIES: usize = 5;

    loop {
        let begin_started = Instant::now();
        let txn = match db.begin().await {
            Ok(t) => t,
            Err(e) if attempt < MAX_RETRIES && sqlite_is_locked_error(&e.to_string()) => {
                sqlite_sleep_for_lock_retry(attempt).await;
                attempt += 1;
                continue;
            }
            Err(e) => {
                error!("Failed to begin persist parts transaction: {}", e);
                break;
            }
        };
        begin_elapsed += begin_started.elapsed();

        // Per-attempt accumulators; only the committed attempt's figures are logged.
        update_elapsed = std::time::Duration::ZERO;
        update_batch_max_elapsed = std::time::Duration::ZERO;
        update_batches = 0;
        update_rows_affected = 0;
        let mut rows_done = 0usize;
        let mut last_reported = 0usize;
        let mut update_err: Option<DbErr> = None;

        for sub_batch in part_updates.chunks(update_batch_size) {
            let values_rows = vec!["(?, ?, ?, ?)"; sub_batch.len()].join(", ");
            let sql = format!(
                "WITH v(id, local_checksum, local_length, local_start) AS (VALUES {values_rows}) \
                 UPDATE subfiles SET local_checksum = v.local_checksum, \
                 local_length = v.local_length, local_start = v.local_start \
                 FROM v WHERE subfiles.id = v.id"
            );
            let mut values: Vec<DbValue> = Vec::with_capacity(sub_batch.len() * params_per_row);
            for part in sub_batch {
                values.push((part.id as i64).into());
                values.push(part.local_checksum.clone().into());
                values.push((part.local_length as i64).into());
                values.push((part.local_start as i64).into());
            }
            let update_started = Instant::now();
            match txn.execute(&sql, values).await {
                Ok(affected) => {
                    let batch_elapsed = update_started.elapsed();
                    update_elapsed += batch_elapsed;
                    update_batch_max_elapsed = update_batch_max_elapsed.max(batch_elapsed);
                    update_batches += 1;
                    update_rows_affected = update_rows_affected.saturating_add(affected);
                    rows_done += sub_batch.len();
                    let reported = rows_done - (rows_done % PERSIST_LOG_INTERVAL);
                    if reported > last_reported {
                        on_chunk_persisted(reported - last_reported);
                        last_reported = reported;
                    }
                }
                Err(e) => {
                    update_err = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = update_err {
            if attempt < MAX_RETRIES && sqlite_is_locked_error(&e.to_string()) {
                drop(txn); // implicit rollback
                sqlite_sleep_for_lock_retry(attempt).await;
                attempt += 1;
                continue;
            }
            error!("Failed to update subfiles batch: {}", e);
            let _ = txn.rollback().await;
            break;
        }

        let commit_started = Instant::now();
        match txn.commit().await {
            Ok(_) => {
                commit_elapsed += commit_started.elapsed();
                committed = true;
                // Flush any rows not yet sent to the progress callback.
                let remaining = part_updates.len() - last_reported;
                if remaining > 0 {
                    on_chunk_persisted(remaining);
                }
                break;
            }
            Err(e) if attempt < MAX_RETRIES && sqlite_is_locked_error(&e.to_string()) => {
                commit_elapsed += commit_started.elapsed();
                sqlite_sleep_for_lock_retry(attempt).await;
                attempt += 1;
                continue;
            }
            Err(e) => {
                commit_elapsed += commit_started.elapsed();
                error!("Failed to commit persist parts transaction: {}", e);
                break;
            }
        }
    }

    let sqlite_delta = sqlite_perf_snapshot().delta_since(sqlite_baseline);
    info!(
        "Part hash persistence metrics: strategy=bulk_cte rows={} committed={} attempts={} retries={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} update_batch_size={} update_batches={} update_rows_affected={} write_wait={:.3}s begin={:.3}s update={:.3}s update_batch_max={:.3}s commit={:.3}s total={:.3}s",
        part_updates.len(),
        committed,
        attempt + 1,
        attempt,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        update_batch_size,
        update_batches,
        update_rows_affected,
        write_wait_elapsed.as_secs_f64(),
        begin_elapsed.as_secs_f64(),
        update_elapsed.as_secs_f64(),
        update_batch_max_elapsed.as_secs_f64(),
        commit_elapsed.as_secs_f64(),
        persist_started.elapsed().as_secs_f64()
    );
}

#[allow(dead_code)]
pub(super) async fn mark_clean_part_checksums_for_files<F>(
    db: &FoxyDb,
    clean_files: &[CleanPartMark],
    mut on_chunk_persisted: F,
) -> bool
where
    F: FnMut(usize),
{
    if clean_files.is_empty() {
        return true;
    }

    let mut clean_files = clean_files.to_vec();
    clean_files.sort_by_key(|entry| entry.file_id);
    clean_files.dedup_by_key(|entry| entry.file_id);

    let total_parts: usize = clean_files.iter().map(|entry| entry.part_count).sum();
    let batch_size = clean_part_mark_batch_size();
    let persist_started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();

    let write_wait_started = Instant::now();
    let _db_permit = DB_WRITE_SEMAPHORE.clone().acquire_owned().await.ok();
    let write_wait_elapsed = write_wait_started.elapsed();
    let _write_scope = sqlite_labeled_write_scope("mark clean part hashes");

    let mut attempt = 0;
    let mut begin_elapsed = std::time::Duration::ZERO;
    let mut update_elapsed = std::time::Duration::ZERO;
    let mut update_batch_max_elapsed = std::time::Duration::ZERO;
    let mut commit_elapsed = std::time::Duration::ZERO;
    let mut update_batches = 0usize;
    let mut update_rows_affected = 0u64;
    let mut committed = false;
    const MAX_RETRIES: usize = 5;

    loop {
        let begin_started = Instant::now();
        let txn = match db.begin().await {
            Ok(t) => t,
            Err(e) if attempt < MAX_RETRIES && sqlite_is_locked_error(&e.to_string()) => {
                sqlite_sleep_for_lock_retry(attempt).await;
                attempt += 1;
                continue;
            }
            Err(e) => {
                error!("Failed to begin clean part mark transaction: {}", e);
                break;
            }
        };
        begin_elapsed += begin_started.elapsed();

        update_elapsed = std::time::Duration::ZERO;
        update_batch_max_elapsed = std::time::Duration::ZERO;
        update_batches = 0;
        update_rows_affected = 0;
        let mut update_err: Option<DbErr> = None;

        for batch in clean_files.chunks(batch_size) {
            let placeholders = vec!["?"; batch.len()].join(", ");
            let sql = format!(
                "UPDATE subfiles \
                 SET local_checksum = remote_checksum, \
                     local_length = remote_length, \
                     local_start = remote_start \
                 WHERE file_id IN ({placeholders}) \
                   AND remote_checksum IS NOT NULL \
                   AND remote_checksum != '' \
                   AND remote_length IS NOT NULL \
                   AND remote_start IS NOT NULL \
                   AND (COALESCE(local_checksum, '') != remote_checksum \
                        OR COALESCE(local_length, -1) != remote_length \
                        OR COALESCE(local_start, -1) != remote_start)"
            );
            let values: Vec<DbValue> = batch
                .iter()
                .map(|entry| (entry.file_id as i64).into())
                .collect();
            let update_started = Instant::now();
            match txn.execute(&sql, values).await {
                Ok(affected) => {
                    let batch_elapsed = update_started.elapsed();
                    update_elapsed += batch_elapsed;
                    update_batch_max_elapsed = update_batch_max_elapsed.max(batch_elapsed);
                    update_batches += 1;
                    update_rows_affected = update_rows_affected.saturating_add(affected);
                }
                Err(e) => {
                    update_err = Some(e);
                    break;
                }
            }
        }

        if let Some(e) = update_err {
            if attempt < MAX_RETRIES && sqlite_is_locked_error(&e.to_string()) {
                drop(txn);
                sqlite_sleep_for_lock_retry(attempt).await;
                attempt += 1;
                continue;
            }
            error!("Failed to mark clean subfile batch: {}", e);
            let _ = txn.rollback().await;
            break;
        }

        let commit_started = Instant::now();
        match txn.commit().await {
            Ok(_) => {
                commit_elapsed += commit_started.elapsed();
                committed = true;
                on_chunk_persisted(total_parts);
                break;
            }
            Err(e) if attempt < MAX_RETRIES && sqlite_is_locked_error(&e.to_string()) => {
                commit_elapsed += commit_started.elapsed();
                sqlite_sleep_for_lock_retry(attempt).await;
                attempt += 1;
                continue;
            }
            Err(e) => {
                commit_elapsed += commit_started.elapsed();
                error!("Failed to commit clean part mark transaction: {}", e);
                break;
            }
        }
    }

    let sqlite_delta = sqlite_perf_snapshot().delta_since(sqlite_baseline);
    info!(
        "Part hash persistence metrics: strategy=clean_file_mark files={} parts={} committed={} attempts={} retries={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} update_batch_size={} update_batches={} update_rows_affected={} write_wait={:.3}s begin={:.3}s update={:.3}s update_batch_max={:.3}s commit={:.3}s total={:.3}s",
        clean_files.len(),
        total_parts,
        committed,
        attempt + 1,
        attempt,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        batch_size,
        update_batches,
        update_rows_affected,
        write_wait_elapsed.as_secs_f64(),
        begin_elapsed.as_secs_f64(),
        update_elapsed.as_secs_f64(),
        update_batch_max_elapsed.as_secs_f64(),
        commit_elapsed.as_secs_f64(),
        persist_started.elapsed().as_secs_f64()
    );
    committed
}

pub(super) async fn persist_file_checksums<F>(
    db: &FoxyDb,
    file_updates: &[FoxyModFile],
    mut on_chunk_persisted: F,
) where
    F: FnMut(usize),
{
    if file_updates.is_empty() {
        return;
    }
    let started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();
    let mut chunks = 0usize;
    let batch_size = sqlite_upsert_batch_size(9);
    let suppressed = suppress_wal_autocheckpoint(db).await;
    for chunk in file_updates.chunks(PERSIST_LOG_INTERVAL) {
        chunks += 1;
        let chunk_rows = Arc::new(chunk.to_vec());
        if let Err(e) = db
            .transaction("persist files", move |txn| {
                let chunk_rows = Arc::clone(&chunk_rows);
                Box::pin(async move {
                    for batch in chunk_rows.chunks(batch_size) {
                        let placeholders =
                            vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) VALUES {placeholders} ON CONFLICT(id) DO UPDATE SET local_checksum = excluded.local_checksum"
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 9);
                        for f in batch {
                            values.push((f.id as i64).into());
                            values.push(f.name.clone().into());
                            values.push(f.remote_path.clone().into());
                            values.push(f.local_path.clone().into());
                            values.push(f.local_checksum.clone().into());
                            values.push(f.remote_checksum.clone().into());
                            values.push(f.local_content_hash.clone().into());
                            values.push((f.length as i64).into());
                            values.push(f.data_order.into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                    Ok(())
                })
            })
            .await
        {
            error!("Failed to persist files chunk: {}", e);
        }
        on_chunk_persisted(chunk.len());
    }
    if suppressed {
        restore_wal_autocheckpoint(db).await;
    }
    log_rollup_persistence_metrics(
        "files",
        file_updates.len(),
        chunks,
        sqlite_perf_snapshot().delta_since(sqlite_baseline),
        started.elapsed(),
    );
}

pub(super) async fn persist_mod_checksums<F>(
    db: &FoxyDb,
    mod_updates: &[FoxyMod],
    mut on_chunk_persisted: F,
) where
    F: FnMut(usize),
{
    if mod_updates.is_empty() {
        return;
    }
    let started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();
    let mut chunks = 0usize;
    let batch_size = sqlite_upsert_batch_size(10);
    let suppressed = suppress_wal_autocheckpoint(db).await;
    for chunk in mod_updates.chunks(PERSIST_LOG_INTERVAL) {
        chunks += 1;
        let chunk_rows = Arc::new(chunk.to_vec());
        if let Err(e) = db
            .transaction("persist mods", move |txn| {
                let chunk_rows = Arc::clone(&chunk_rows);
                Box::pin(async move {
                    for batch in chunk_rows.chunks(batch_size) {
                        let placeholders =
                            vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO addons (id, name, display_name, remote_path, local_path, client_side, enabled, local_checksum, remote_checksum, local_content_hash, required, data_order) VALUES {placeholders} ON CONFLICT(id) DO UPDATE SET local_checksum = excluded.local_checksum"
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 12);
                        for m in batch {
                            values.push((m.id as i64).into());
                            values.push(m.name.clone().into());
                            values.push(m.display_name.clone().into());
                            values.push(m.remote_path.clone().into());
                            values.push(m.local_path.clone().into());
                            values.push(m.client_side.into());
                            values.push(m.enabled.into());
                            values.push(m.local_checksum.clone().into());
                            values.push(m.remote_checksum.clone().into());
                            values.push(m.local_content_hash.clone().into());
                            values.push(m.required.into());
                            values.push(m.data_order.into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                    Ok(())
                })
            })
            .await
        {
            error!("Failed to persist mods chunk: {}", e);
        }
        on_chunk_persisted(chunk.len());
    }
    if suppressed {
        restore_wal_autocheckpoint(db).await;
    }
    log_rollup_persistence_metrics(
        "addons",
        mod_updates.len(),
        chunks,
        sqlite_perf_snapshot().delta_since(sqlite_baseline),
        started.elapsed(),
    );
}

pub(super) async fn persist_repository_checksums<F>(
    db: &FoxyDb,
    repo_updates: &[FoxyRepository],
    mut on_chunk_persisted: F,
) where
    F: FnMut(usize),
{
    if repo_updates.is_empty() {
        return;
    }
    let started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();
    let mut chunks = 0usize;
    let batch_size = sqlite_upsert_batch_size(8);
    for chunk in repo_updates.chunks(PERSIST_LOG_INTERVAL) {
        chunks += 1;
        let chunk_rows = Arc::new(chunk.to_vec());
        if let Err(e) = db
            .transaction("persist repos", move |txn| {
                let chunk_rows = Arc::clone(&chunk_rows);
                Box::pin(async move {
                    for batch in chunk_rows.chunks(batch_size) {
                        let placeholders =
                            vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO repositories (id, name, remote_url, local_path, image, local_checksum, remote_checksum, local_content_hash, foxy_mode) VALUES {placeholders} ON CONFLICT(id) DO UPDATE SET local_checksum = excluded.local_checksum"
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 9);
                        for r in batch {
                            values.push((r.id as i64).into());
                            values.push(r.name.clone().into());
                            values.push(r.remote_url.clone().into());
                            values.push(r.local_path.clone().into());
                            values.push(r.image.clone().into());
                            values.push(r.local_checksum.clone().into());
                            values.push(r.remote_checksum.clone().into());
                            values.push(r.local_content_hash.clone().into());
                            values.push(r.foxy_mode.as_db_str().to_string().into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                    Ok(())
                })
            })
            .await
        {
            error!("Failed to persist repos chunk: {}", e);
        }
        on_chunk_persisted(chunk.len());
    }
    log_rollup_persistence_metrics(
        "repositories",
        repo_updates.len(),
        chunks,
        sqlite_perf_snapshot().delta_since(sqlite_baseline),
        started.elapsed(),
    );
}

fn log_rollup_persistence_metrics(
    kind: &str,
    rows: usize,
    chunks: usize,
    sqlite_delta: crate::core::tasks::init_database::SqlitePerfSnapshot,
    elapsed: std::time::Duration,
) {
    info!(
        "Hash rollup persistence metrics: kind={} rows={} chunks={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} total={:.3}s",
        kind,
        rows,
        chunks,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        elapsed.as_secs_f64()
    );
}

pub(super) fn calculate_compound_content_hash(ordered_hashes: &[(i64, String)]) -> String {
    if ordered_hashes.is_empty() {
        return String::new();
    }
    let mut values = ordered_hashes.to_vec();
    values.sort_by_key(|(order, _)| *order);
    let mut hasher = blake3::Hasher::new();
    for (_, value) in values {
        hasher.update(value.as_bytes());
    }
    crate::core::utils::content_hash::blake3_hex(hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── sqlite_upsert_batch_size ──────────────────────────────────────

    #[test]
    fn upsert_batch_size_10_columns() {
        // 999 / 10 - 1 = 98
        assert_eq!(sqlite_upsert_batch_size(10), 98);
    }

    #[test]
    fn upsert_batch_size_9_columns() {
        // 999 / 9 - 1 = 110
        assert_eq!(sqlite_upsert_batch_size(9), 110);
    }

    #[test]
    fn upsert_batch_size_4_columns() {
        // 999 / 4 - 1 = 248
        assert_eq!(sqlite_upsert_batch_size(4), 248);
    }

    #[test]
    fn upsert_batch_size_2_columns() {
        // 999 / 2 - 1 = 498
        assert_eq!(sqlite_upsert_batch_size(2), 498);
    }

    #[test]
    fn upsert_batch_size_large_column_count_floors_to_one() {
        // 999 / 1000 = 0, saturating_sub(1) = 0, max(1) = 1
        assert_eq!(sqlite_upsert_batch_size(1000), 1);
    }

    // ── persist_part_checksums SQL generation ──────────────────────────

    #[test]
    fn part_persist_update_batch_size_uses_bulk_limit() {
        // The bulk UPDATE … FROM (VALUES …) uses 4 params per row. With Turso's
        // high variable limit this packs thousands of rows per statement, well
        // above the 248-row batches the 999-variable limit would force.
        let params_per_row = 4usize;
        let batch_size = bulk_persist_update_batch_size(params_per_row);
        assert!(
            batch_size >= 248,
            "bulk batch size should be >= 248, got {}",
            batch_size
        );
    }

    #[test]
    fn part_persist_values_clause_generation() {
        let count = 3;
        let values_rows = vec!["(?, ?, ?, ?)"; count].join(", ");
        let sql = format!(
            "WITH v(id, local_checksum, local_length, local_start) AS (VALUES {values_rows}) \
             UPDATE subfiles SET local_checksum = v.local_checksum, \
             local_length = v.local_length, local_start = v.local_start \
             FROM v WHERE subfiles.id = v.id"
        );
        assert!(sql.contains("VALUES (?, ?, ?, ?), (?, ?, ?, ?), (?, ?, ?, ?)"));
        assert!(sql.contains("UPDATE subfiles SET"));
        assert_eq!(sql.matches('?').count(), 12);
    }

    #[test]
    fn part_persist_values_clause_single_row() {
        let values_rows = "(?, ?, ?, ?)";
        let sql = format!(
            "WITH v(id, local_checksum, local_length, local_start) AS (VALUES {values_rows}) \
             UPDATE subfiles SET local_checksum = v.local_checksum FROM v WHERE subfiles.id = v.id"
        );
        assert_eq!(sql.matches('?').count(), 4);
    }

    // ── calculate_compound_content_hash ────────────────────────────────

    #[test]
    fn compound_content_hash_empty_returns_empty() {
        assert_eq!(calculate_compound_content_hash(&[]), String::new());
    }

    #[test]
    fn compound_content_hash_sorts_by_order() {
        let a = calculate_compound_content_hash(&[(2, "b".into()), (1, "a".into())]);
        let b = calculate_compound_content_hash(&[(1, "a".into()), (2, "b".into())]);
        assert_eq!(a, b, "hash should be order-independent (sorted internally)");
    }

    #[test]
    fn compound_content_hash_different_values_differ() {
        let a = calculate_compound_content_hash(&[(1, "abc".into())]);
        let b = calculate_compound_content_hash(&[(1, "xyz".into())]);
        assert_ne!(a, b);
    }

    // ── bulk part-hash persist against a real Turso engine ─────────────

    /// Seeds enough subfiles to span several chunked `UPDATE … FROM (VALUES …)`
    /// statements, then verifies every part's local columns are written and the
    /// progress callback reports the full count.
    #[tokio::test]
    async fn bulk_persist_updates_every_part_via_cte() {
        use crate::core::tasks::db_turso::build_test_database;

        let db = FoxyDb::from_handle(build_test_database().await);
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f', 'rp', 'lp')",
            params![],
        )
        .await
        .expect("seed parent file");

        let n = 562usize;
        for start in (0..n).step_by(200) {
            let end = (start + 200).min(n);
            let rows: Vec<String> = (start..end)
                .map(|i| format!("({}, 1, 'p{}', '', 0, 0)", i + 1, i))
                .collect();
            let sql = format!(
                "INSERT INTO subfiles (id, file_id, path, local_checksum, local_length, local_start) VALUES {}",
                rows.join(", ")
            );
            db.execute(&sql, params![]).await.expect("seed subfiles");
        }

        let updates: Vec<FoxyModFilePart> = (0..n)
            .map(|i| FoxyModFilePart {
                id: i as u64 + 1,
                file_id: 1,
                local_checksum: format!("LC{i}"),
                local_length: i as u64 + 10,
                local_start: i as u64 + 1,
                ..Default::default()
            })
            .collect();

        let mut persisted = 0usize;
        persist_part_checksums(&db, &updates, |c| persisted += c).await;
        assert_eq!(persisted, n, "progress callback should report every row");

        let row = db
            .query_one(
                "SELECT local_checksum, local_length, local_start FROM subfiles WHERE id = 500",
                params![],
            )
            .await
            .expect("query row")
            .expect("row 500 exists");
        assert_eq!(row.get_string("local_checksum").unwrap(), "LC499");
        assert_eq!(row.get_i64("local_length").unwrap(), 509);
        assert_eq!(row.get_i64("local_start").unwrap(), 500);

        let updated = db
            .query_one(
                "SELECT COUNT(*) AS c FROM subfiles WHERE local_checksum != ''",
                params![],
            )
            .await
            .expect("count query")
            .expect("count row")
            .get_i64("c")
            .unwrap();
        assert_eq!(updated, n as i64, "every seeded part must be updated");
    }

    #[tokio::test]
    async fn clean_part_mark_copies_remote_columns_for_file_ids() {
        use crate::core::tasks::db_turso::build_test_database;

        let db = FoxyDb::from_handle(build_test_database().await);
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES (1, 'f1', 'rp1', 'lp1'), (2, 'f2', 'rp2', 'lp2')",
            params![],
        )
        .await
        .expect("seed parent files");
        db.execute(
            "INSERT INTO subfiles \
                 (id, file_id, path, local_checksum, local_length, local_start, remote_checksum, remote_length, remote_start) \
             VALUES \
                 (1, 1, 'p1', '', 0, 0, 'R1', 10, 0), \
                 (2, 1, 'p2', 'OLD', 3, 5, 'R2', 20, 10), \
                 (3, 2, 'p3', 'OLD', 3, 5, 'R3', 30, 0)",
            params![],
        )
        .await
        .expect("seed subfiles");

        let mut persisted = 0usize;
        mark_clean_part_checksums_for_files(
            &db,
            &[CleanPartMark {
                file_id: 1,
                part_count: 2,
            }],
            |count| persisted += count,
        )
        .await;
        assert_eq!(persisted, 2);

        let rows = db
            .query_all(
                "SELECT id, local_checksum, local_length, local_start FROM subfiles ORDER BY id",
                params![],
            )
            .await
            .expect("query subfiles");
        assert_eq!(rows[0].get_string("local_checksum").unwrap(), "R1");
        assert_eq!(rows[0].get_i64("local_length").unwrap(), 10);
        assert_eq!(rows[0].get_i64("local_start").unwrap(), 0);
        assert_eq!(rows[1].get_string("local_checksum").unwrap(), "R2");
        assert_eq!(rows[1].get_i64("local_length").unwrap(), 20);
        assert_eq!(rows[1].get_i64("local_start").unwrap(), 10);
        assert_eq!(rows[2].get_string("local_checksum").unwrap(), "OLD");
        assert_eq!(rows[2].get_i64("local_length").unwrap(), 3);
        assert_eq!(rows[2].get_i64("local_start").unwrap(), 5);
    }
}
