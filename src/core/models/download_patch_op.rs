use crate::core::db::{DbErr, DbRow, DbValue, params};
use crate::core::models::context::FoxyContext;
use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;
use std::sync::Arc;

/// Column list for selecting a [`DownloadPatchOp`] via [`DownloadPatchOp::from_row`].
const PATCH_OP_COLUMNS: &str = "id, file_id, data_order, op_type, dest_start, length, target_checksum, source_start, \
     source_checksum, blob_offset, downloaded_bytes, retry_count";

/// Columns written per row on insert (everything except the autoincrement `id`).
const PATCH_OP_INSERT_COLUMNS: &str = "file_id, data_order, op_type, dest_start, length, target_checksum, source_start, \
     source_checksum, blob_offset, downloaded_bytes, retry_count";

/// Bound variables per inserted row (matches [`PATCH_OP_INSERT_COLUMNS`]).
const PATCH_OP_INSERT_BINDS: usize = 11;

#[derive(Debug, Default, Clone)]
pub(crate) struct DownloadPatchOp {
    /// Materialized from the row for completeness; not currently consumed.
    #[allow(dead_code)]
    pub(crate) id: u64,
    pub(crate) file_id: u64,
    pub(crate) data_order: i64,
    pub(crate) op_type: String,
    pub(crate) dest_start: u64,
    pub(crate) length: u64,
    pub(crate) target_checksum: String,
    pub(crate) source_start: Option<u64>,
    pub(crate) source_checksum: Option<String>,
    pub(crate) blob_offset: Option<u64>,
    pub(crate) downloaded_bytes: u64,
    pub(crate) retry_count: u32,
}

impl DownloadPatchOp {
    /// Materialize from a seam [`DbRow`] selected with [`PATCH_OP_COLUMNS`].
    fn from_row(row: &DbRow) -> Result<Self, DbErr> {
        Ok(DownloadPatchOp {
            id: row.get_i64("id")? as u64,
            file_id: row.get_i64("file_id")? as u64,
            data_order: row.get_i64("data_order")?,
            op_type: row.get_string("op_type")?,
            dest_start: row.get_i64("dest_start")? as u64,
            length: row.get_i64("length")? as u64,
            target_checksum: row.get_string("target_checksum")?,
            source_start: row.get_opt_i64("source_start")?.map(|v| v as u64),
            source_checksum: row.get_opt_string("source_checksum")?,
            blob_offset: row.get_opt_i64("blob_offset")?.map(|v| v as u64),
            downloaded_bytes: row.get_i64("downloaded_bytes")? as u64,
            retry_count: row.get_i64("retry_count")? as u32,
        })
    }
}

pub(crate) async fn delete_download_patch_ops_for_file(
    context: Arc<FoxyContext>,
    file_id: i64,
) -> Result<(), DbErr> {
    context
        .db()
        .transaction("delete patch ops", move |txn| {
            Box::pin(async move {
                txn.execute(
                    "DELETE FROM download_patch_op WHERE file_id = ?",
                    params![file_id],
                )
                .await?;
                Ok(())
            })
        })
        .await
}

/// Delete patch ops for many files in a single transaction, chunked to stay
/// under the SQLite bound-variable limit. Replaces per-file delete round trips
/// when a whole batch of files needs its stale patch plans cleared.
pub(crate) async fn delete_download_patch_ops_for_files(
    context: Arc<FoxyContext>,
    file_ids: &[i64],
) -> Result<(), DbErr> {
    if file_ids.is_empty() {
        return Ok(());
    }
    let file_ids = file_ids.to_vec();
    context
        .db()
        .transaction("bulk delete patch ops", move |txn| {
            let file_ids = file_ids.clone();
            Box::pin(async move {
                let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
                for chunk in file_ids.chunks(chunk_size) {
                    let placeholders = vec!["?"; chunk.len()].join(", ");
                    let sql =
                        format!("DELETE FROM download_patch_op WHERE file_id IN ({placeholders})");
                    let chunk_params: Vec<DbValue> =
                        chunk.iter().map(|id| DbValue::from(*id)).collect();
                    txn.execute(&sql, chunk_params).await?;
                }
                Ok(())
            })
        })
        .await
}

pub(crate) async fn replace_download_patch_ops_for_file(
    context: Arc<FoxyContext>,
    file_id: i64,
    ops: &[DownloadPatchOp],
) -> Result<(), DbErr> {
    let ops = ops.to_vec();
    context
        .db()
        .transaction("replace patch ops", move |txn| {
            let ops = ops.clone();
            Box::pin(async move {
                txn.execute(
                    "DELETE FROM download_patch_op WHERE file_id = ?",
                    params![file_id],
                )
                .await?;

                if ops.is_empty() {
                    return Ok(());
                }

                let rows_per_chunk =
                    crate::core::tasks::init_database::bulk_write_rows_for(PATCH_OP_INSERT_BINDS);
                let row_placeholder = format!("({})", ["?"; PATCH_OP_INSERT_BINDS].join(", "));
                for chunk in ops.chunks(rows_per_chunk) {
                    let placeholders =
                        vec![row_placeholder.as_str(); chunk.len()].join(", ");
                    let sql = format!(
                        "INSERT INTO download_patch_op ({PATCH_OP_INSERT_COLUMNS}) VALUES {placeholders} \
                         ON CONFLICT (file_id, data_order) DO UPDATE SET \
                         op_type = excluded.op_type, dest_start = excluded.dest_start, \
                         length = excluded.length, target_checksum = excluded.target_checksum, \
                         source_start = excluded.source_start, \
                         source_checksum = excluded.source_checksum, \
                         blob_offset = excluded.blob_offset, \
                         downloaded_bytes = excluded.downloaded_bytes, \
                         retry_count = excluded.retry_count"
                    );
                    let mut values: Vec<DbValue> =
                        Vec::with_capacity(chunk.len() * PATCH_OP_INSERT_BINDS);
                    for op in chunk {
                        values.push(DbValue::from(op.file_id as i64));
                        values.push(DbValue::from(op.data_order));
                        values.push(DbValue::from(op.op_type.clone()));
                        values.push(DbValue::from(op.dest_start as i64));
                        values.push(DbValue::from(op.length as i64));
                        values.push(DbValue::from(op.target_checksum.clone()));
                        values.push(DbValue::from(op.source_start.map(|v| v as i64)));
                        values.push(DbValue::from(op.source_checksum.clone()));
                        values.push(DbValue::from(op.blob_offset.map(|v| v as i64)));
                        values.push(DbValue::from(op.downloaded_bytes as i64));
                        values.push(DbValue::from(i64::from(op.retry_count)));
                    }
                    txn.execute(&sql, values).await?;
                }

                Ok(())
            })
        })
        .await
}

pub(crate) async fn fetch_download_patch_ops_for_file(
    context: Arc<FoxyContext>,
    file_id: i64,
) -> Result<Vec<DownloadPatchOp>, DbErr> {
    context
        .db()
        .query_all(
            &format!(
                "SELECT {PATCH_OP_COLUMNS} FROM download_patch_op WHERE file_id = ? \
                 ORDER BY data_order ASC"
            ),
            params![file_id],
        )
        .await?
        .iter()
        .map(DownloadPatchOp::from_row)
        .collect()
}

pub(crate) async fn update_download_patch_op_progress(
    context: Arc<FoxyContext>,
    file_id: i64,
    data_order: i64,
    downloaded_bytes: u64,
    retry_count: u32,
) -> Result<(), DbErr> {
    context
        .db()
        .transaction("update patch op progress", move |txn| {
            Box::pin(async move {
                txn.execute(
                    "UPDATE download_patch_op SET downloaded_bytes = ?, retry_count = ? \
                     WHERE file_id = ? AND data_order = ?",
                    params![
                        downloaded_bytes as i64,
                        i64::from(retry_count),
                        file_id,
                        data_order
                    ],
                )
                .await?;
                Ok(())
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn in_memory_context() -> Arc<FoxyContext> {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        // download_patch_op.file_id has an FK to files(id); seed the parent rows
        // the tests reference (1..=3) so ops insert under FK enforcement.
        crate::core::db::FoxyDb::from_turso(db.clone())
            .execute(
                "INSERT INTO files (id) VALUES (1), (2), (3)",
                crate::core::db::params![],
            )
            .await
            .expect("seed parent files");
        Arc::new(FoxyContext::new(db, reqwest::Client::new()))
    }

    fn op(file_id: u64) -> DownloadPatchOp {
        DownloadPatchOp {
            file_id,
            op_type: "InsertRemote".to_string(),
            length: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn bulk_delete_removes_only_listed_files() {
        let context = in_memory_context().await;
        for file_id in [1u64, 2, 3] {
            replace_download_patch_ops_for_file(context.clone(), file_id as i64, &[op(file_id)])
                .await
                .expect("seed patch op");
        }

        delete_download_patch_ops_for_files(context.clone(), &[1, 3])
            .await
            .expect("bulk delete");

        assert!(
            fetch_download_patch_ops_for_file(context.clone(), 1)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fetch_download_patch_ops_for_file(context.clone(), 3)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fetch_download_patch_ops_for_file(context.clone(), 2)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn bulk_delete_empty_list_is_noop() {
        let context = in_memory_context().await;
        replace_download_patch_ops_for_file(context.clone(), 1, &[op(1)])
            .await
            .expect("seed patch op");

        delete_download_patch_ops_for_files(context.clone(), &[])
            .await
            .expect("bulk delete noop");

        assert_eq!(
            fetch_download_patch_ops_for_file(context.clone(), 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
