use crate::core::db::{DbErr, DbRow, DbValue, params};
use crate::core::models::context::FoxyContext;
use crate::core::tasks::init_database::{SqlitePerfSnapshot, sqlite_perf_snapshot};
use log::info;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Column list for selecting a [`DownloadTargetFile`] via [`DownloadTargetFile::from_row`].
const DOWNLOAD_TARGET_COLUMNS: &str =
    "file_id, download_remote_url, download_local_path, size, download_total, download_cycle";

#[derive(Debug, Default, Clone)]
pub(crate) struct DownloadTargetFile {
    /// File ID in a database if applicable
    pub(crate) file_id: u64,
    /// Remote file source url
    pub(crate) download_remote_url: Arc<str>,
    /// Local file target path
    pub(crate) download_local_path: Arc<str>,
    /// File size
    pub(crate) size: usize,
    /// Expected bytes transferred over network (can be lower than `size` for delta patching)
    pub(crate) expected_download_bytes: usize,
    /// Total downloaded bytes
    pub(crate) download_total: Arc<AtomicUsize>,
    /// Bytes downloaded per cycle (usually per second)
    pub(crate) download_cycle: Arc<AtomicUsize>,
}

impl DownloadTargetFile {
    /// Materialize from a seam [`DbRow`] selected with [`DOWNLOAD_TARGET_COLUMNS`].
    pub(crate) fn from_row(row: &DbRow) -> Result<Self, DbErr> {
        let size = row.get_i64("size")? as usize;
        Ok(DownloadTargetFile {
            file_id: row.get_i64("file_id")? as u64,
            download_remote_url: Arc::from(row.get_string("download_remote_url")?),
            download_local_path: Arc::from(row.get_string("download_local_path")?),
            size,
            expected_download_bytes: size,
            download_total: Arc::new(AtomicUsize::new(row.get_i64("download_total")? as usize)),
            download_cycle: Arc::new(AtomicUsize::new(row.get_i64("download_cycle")? as usize)),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadTargetWithMod {
    pub download: DownloadTargetFile,
    pub mod_id: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadTargetWithModName {
    pub download: DownloadTargetFile,
    pub mod_id: u64,
    pub mod_name: String,
}

pub(crate) async fn save_download_target_file(
    context: Arc<FoxyContext>,
    f: &DownloadTargetFile,
) -> Result<(), DbErr> {
    let file_id = f.file_id as i64;
    let remote_url = f.download_remote_url.to_string();
    let local_path = f.download_local_path.to_string();
    let size = f.size as i64;
    let download_total = f.download_total.load(Ordering::Relaxed) as i64;
    let download_cycle = f.download_cycle.load(Ordering::Relaxed) as i64;
    context
        .db()
        .transaction("save download target file", move |txn| {
            let remote_url = remote_url.clone();
            let local_path = local_path.clone();
            Box::pin(async move {
                txn.execute(
                    "INSERT INTO download_target_file \
                     (file_id, download_remote_url, download_local_path, size, download_total, download_cycle) \
                     VALUES (?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (file_id) DO UPDATE SET \
                     download_remote_url = excluded.download_remote_url, \
                     download_local_path = excluded.download_local_path, \
                     size = excluded.size, download_total = excluded.download_total, \
                     download_cycle = excluded.download_cycle",
                    params![
                        file_id,
                        remote_url,
                        local_path,
                        size,
                        download_total,
                        download_cycle
                    ],
                )
                .await?;
                Ok(())
            })
        })
        .await
}

/// Batch-update download progress for dirty files in a single transaction.
/// More efficient than per-row upserts when only progress counters changed.
pub(crate) async fn update_download_target_progress_batch(
    context: Arc<FoxyContext>,
    updates: &[DownloadProgressUpdate],
) -> Result<DownloadProgressPersistResult, DbErr> {
    if updates.is_empty() {
        return Ok(DownloadProgressPersistResult::default());
    }

    let started = std::time::Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();
    let rows = Arc::new(updates.to_vec());
    let statements = rows.len().div_ceil(download_progress_update_batch_size());
    let result = context
        .db()
        .transaction("persist download progress", move |txn| {
            let rows = Arc::clone(&rows);
            Box::pin(async move {
                for batch in rows.chunks(download_progress_update_batch_size()) {
                    let placeholders = std::iter::repeat_n("(?, ?, ?)", batch.len())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let sql = format!(
                        "WITH progress(file_id, download_total, download_cycle) AS (VALUES {}) \
                         UPDATE download_target_file \
                         SET download_total = (SELECT download_total FROM progress WHERE progress.file_id = download_target_file.file_id), \
                             download_cycle = (SELECT download_cycle FROM progress WHERE progress.file_id = download_target_file.file_id) \
                         WHERE file_id IN (SELECT file_id FROM progress)",
                        placeholders
                    );
                    let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 3);
                    for update in batch {
                        values.push(DbValue::from(update.file_id as i64));
                        values.push(DbValue::from(update.download_total as i64));
                        values.push(DbValue::from(update.download_cycle as i64));
                    }
                    txn.execute(&sql, values).await?;
                }
                Ok(())
            })
        })
        .await;
    info!(
        "Download progress persistence metrics: rows={} committed={} statements={} total={:.3}s",
        updates.len(),
        result.is_ok(),
        statements,
        started.elapsed().as_secs_f64()
    );
    result?;
    Ok(DownloadProgressPersistResult {
        rows: updates.len(),
        statements,
        elapsed: started.elapsed(),
        sqlite_delta: sqlite_perf_snapshot().delta_since(sqlite_baseline),
    })
}

/// Lightweight progress snapshot for batch persistence.
#[derive(Clone)]
pub(crate) struct DownloadProgressUpdate {
    pub(crate) file_id: u64,
    pub(crate) download_total: usize,
    pub(crate) download_cycle: usize,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct DownloadProgressPersistResult {
    pub(crate) rows: usize,
    pub(crate) statements: usize,
    pub(crate) elapsed: std::time::Duration,
    pub(crate) sqlite_delta: SqlitePerfSnapshot,
}

fn download_progress_update_batch_size() -> usize {
    crate::core::tasks::init_database::bulk_write_rows_for(3)
}

pub(crate) async fn fetch_all_download_targets_with_mod(
    context: Arc<FoxyContext>,
) -> Result<Vec<DownloadTargetWithMod>, DbErr> {
    let db = context.db();
    let downloads: Vec<DownloadTargetFile> = db
        .query_all(
            &format!(
                "SELECT {DOWNLOAD_TARGET_COLUMNS} FROM download_target_file ORDER BY size DESC"
            ),
            params![],
        )
        .await?
        .iter()
        .map(DownloadTargetFile::from_row)
        .collect::<Result<_, DbErr>>()?;
    let file_ids: Vec<i64> = downloads.iter().map(|d| d.file_id as i64).collect();

    let mut file_to_mod: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    if !file_ids.is_empty() {
        let chunk_size = crate::core::tasks::init_database::read_chunk_ids();
        for chunk in file_ids.chunks(chunk_size) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT addon_id, file_id FROM addon_files WHERE file_id IN ({placeholders})"
            );
            let chunk_params: Vec<DbValue> = chunk.iter().map(|id| DbValue::from(*id)).collect();
            for link in db.query_all(&sql, chunk_params).await? {
                file_to_mod.insert(link.get_i64("file_id")?, link.get_i64("addon_id")?);
            }
        }
    }

    let mut result = Vec::with_capacity(downloads.len());
    for download in downloads {
        // Files without a known addon stay in the queue with a sentinel mod id so they still download.
        let mod_id = file_to_mod
            .get(&(download.file_id as i64))
            .map(|m| *m as u64)
            .unwrap_or(u64::MAX);
        result.push(DownloadTargetWithMod { download, mod_id });
    }

    Ok(result)
}

pub(crate) async fn fetch_all_download_targets_with_mod_and_name(
    context: Arc<FoxyContext>,
) -> Result<Vec<DownloadTargetWithModName>, DbErr> {
    // Single JOIN query replaces 3 sequential queries with unbounded IN clauses
    let rows = context
        .db()
        .query_all(
            r#"SELECT dt.file_id, dt.download_remote_url, dt.download_local_path,
                      dt.size, dt.download_total, dt.download_cycle,
                      COALESCE(af.addon_id, -1) as mod_id,
                      COALESCE(a.name, 'Unknown') as mod_name
               FROM download_target_file dt
               LEFT JOIN addon_files af ON af.file_id = dt.file_id
               LEFT JOIN addons a ON a.id = af.addon_id
               ORDER BY dt.size DESC"#,
            params![],
        )
        .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in &rows {
        let file_id = row.get_i64("file_id")?;
        let download_remote_url = row.get_string("download_remote_url")?;
        let download_local_path = row.get_string("download_local_path")?;
        let size = row.get_i64("size")?;
        let download_total = row.get_i64("download_total")?;
        let download_cycle = row.get_i64("download_cycle")?;
        let mod_id = row.get_i64("mod_id").unwrap_or(-1);
        let mod_name = row
            .get_string("mod_name")
            .unwrap_or_else(|_| "Unknown".into());

        result.push(DownloadTargetWithModName {
            download: DownloadTargetFile {
                file_id: file_id as u64,
                download_remote_url: Arc::from(download_remote_url),
                download_local_path: Arc::from(download_local_path),
                size: size as usize,
                expected_download_bytes: size as usize,
                download_total: Arc::new(AtomicUsize::new(download_total as usize)),
                download_cycle: Arc::new(AtomicUsize::new(download_cycle as usize)),
            },
            mod_id: if mod_id < 0 { u64::MAX } else { mod_id as u64 },
            mod_name,
        });
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_update_batch_size_uses_tuned_write_chunk() {
        let batch_size = download_progress_update_batch_size();

        assert!(batch_size > 1);
        assert_eq!(
            batch_size,
            crate::core::tasks::init_database::bulk_write_chunk_rows()
        );
        // A full chunk stays under the bind-variable budget (3 binds per row).
        assert!(batch_size * 3 < 32_766);
    }
}
