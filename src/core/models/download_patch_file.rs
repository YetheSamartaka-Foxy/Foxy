use crate::core::db::{DbErr, DbRow, DbValue, params};
use crate::core::models::context::FoxyContext;
use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;
use chrono::Utc;
use std::sync::Arc;

/// Column list for selecting a [`DownloadPatchFile`] via [`DownloadPatchFile::from_row`].
const PATCH_FILE_COLUMNS: &str = "file_id, patch_json_path, patch_blob_path, planned_copy_bytes, planned_download_bytes, \
     status, last_error, created_at, updated_at";

#[derive(Debug, Default, Clone)]
pub(crate) struct DownloadPatchFile {
    pub(crate) file_id: u64,
    pub(crate) patch_json_path: String,
    pub(crate) patch_blob_path: String,
    pub(crate) planned_copy_bytes: u64,
    pub(crate) planned_download_bytes: u64,
    pub(crate) status: String,
    pub(crate) last_error: Option<String>,
    pub(crate) created_at: String,
    /// Materialized from the row for completeness; not currently consumed.
    #[allow(dead_code)]
    pub(crate) updated_at: String,
}

impl DownloadPatchFile {
    /// Materialize from a seam [`DbRow`] selected with [`PATCH_FILE_COLUMNS`].
    fn from_row(row: &DbRow) -> Result<Self, DbErr> {
        Ok(DownloadPatchFile {
            file_id: row.get_i64("file_id")? as u64,
            patch_json_path: row.get_string("patch_json_path")?,
            patch_blob_path: row.get_string("patch_blob_path")?,
            planned_copy_bytes: row.get_i64("planned_copy_bytes")? as u64,
            planned_download_bytes: row.get_i64("planned_download_bytes")? as u64,
            status: row.get_string("status")?,
            last_error: row.get_opt_string("last_error")?,
            created_at: row.get_string("created_at")?,
            updated_at: row.get_string("updated_at")?,
        })
    }
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

pub(crate) async fn load_download_patch_file(
    context: Arc<FoxyContext>,
    file_id: i64,
) -> Result<Option<DownloadPatchFile>, DbErr> {
    let row = context
        .db()
        .query_one(
            &format!("SELECT {PATCH_FILE_COLUMNS} FROM download_patch_file WHERE file_id = ?"),
            params![file_id],
        )
        .await?;
    match row {
        Some(row) => Ok(Some(DownloadPatchFile::from_row(&row)?)),
        None => Ok(None),
    }
}

pub(crate) async fn save_download_patch_file(
    context: Arc<FoxyContext>,
    patch_file: &DownloadPatchFile,
) -> Result<(), DbErr> {
    let file_id = patch_file.file_id as i64;
    let patch_json_path = patch_file.patch_json_path.clone();
    let patch_blob_path = patch_file.patch_blob_path.clone();
    let planned_copy_bytes = patch_file.planned_copy_bytes as i64;
    let planned_download_bytes = patch_file.planned_download_bytes as i64;
    let status = patch_file.status.clone();
    let last_error = patch_file.last_error.clone();
    // Preserve created_at on conflict; only stamp it for new rows.
    let created_at = if patch_file.created_at.is_empty() {
        now_timestamp()
    } else {
        patch_file.created_at.clone()
    };
    let updated_at = now_timestamp();

    context
        .db()
        .transaction("save patch file", move |txn| {
            let patch_json_path = patch_json_path.clone();
            let patch_blob_path = patch_blob_path.clone();
            let status = status.clone();
            let last_error = last_error.clone();
            let created_at = created_at.clone();
            let updated_at = updated_at.clone();
            Box::pin(async move {
                txn.execute(
                    "INSERT INTO download_patch_file \
                     (file_id, patch_json_path, patch_blob_path, planned_copy_bytes, \
                      planned_download_bytes, status, last_error, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (file_id) DO UPDATE SET \
                     patch_json_path = excluded.patch_json_path, \
                     patch_blob_path = excluded.patch_blob_path, \
                     planned_copy_bytes = excluded.planned_copy_bytes, \
                     planned_download_bytes = excluded.planned_download_bytes, \
                     status = excluded.status, last_error = excluded.last_error, \
                     updated_at = excluded.updated_at",
                    params![
                        file_id,
                        patch_json_path,
                        patch_blob_path,
                        planned_copy_bytes,
                        planned_download_bytes,
                        status,
                        last_error,
                        created_at,
                        updated_at
                    ],
                )
                .await?;
                Ok(())
            })
        })
        .await
}

pub(crate) async fn delete_download_patch_file_by_file_id(
    context: Arc<FoxyContext>,
    file_id: i64,
) -> Result<(), DbErr> {
    context
        .db()
        .transaction("delete patch file", move |txn| {
            Box::pin(async move {
                txn.execute(
                    "DELETE FROM download_patch_file WHERE file_id = ?",
                    params![file_id],
                )
                .await?;
                Ok(())
            })
        })
        .await
}

/// Delete patch-file rows for many files in a single transaction, chunked to
/// stay under the SQLite bound-variable limit.
pub(crate) async fn delete_download_patch_files_by_file_ids(
    context: Arc<FoxyContext>,
    file_ids: &[i64],
) -> Result<(), DbErr> {
    if file_ids.is_empty() {
        return Ok(());
    }
    let file_ids = file_ids.to_vec();
    context
        .db()
        .transaction("bulk delete patch files", move |txn| {
            let file_ids = file_ids.clone();
            Box::pin(async move {
                let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
                for chunk in file_ids.chunks(chunk_size) {
                    let placeholders = vec!["?"; chunk.len()].join(", ");
                    let sql = format!(
                        "DELETE FROM download_patch_file WHERE file_id IN ({placeholders})"
                    );
                    let chunk_params: Vec<DbValue> =
                        chunk.iter().map(|id| DbValue::from(*id)).collect();
                    txn.execute(&sql, chunk_params).await?;
                }
                Ok(())
            })
        })
        .await
}

pub(crate) async fn update_download_patch_file_status(
    context: Arc<FoxyContext>,
    file_id: i64,
    status: &str,
    last_error: Option<&str>,
) -> Result<(), DbErr> {
    let status = status.to_owned();
    let last_error = last_error.map(str::to_owned);
    let updated_at = now_timestamp();
    context
        .db()
        .transaction("update patch file status", move |txn| {
            let status = status.clone();
            let last_error = last_error.clone();
            let updated_at = updated_at.clone();
            Box::pin(async move {
                txn.execute(
                    "UPDATE download_patch_file SET status = ?, last_error = ?, updated_at = ? \
                     WHERE file_id = ?",
                    params![status, last_error, updated_at, file_id],
                )
                .await?;
                Ok(())
            })
        })
        .await
}
