use crate::core::db::{DbErr, DbValue, params};
use crate::core::models::context::FoxyContext;
use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;
use crate::core::utils::content_hash::fast_file_content_hash;
use log::debug;
use std::collections::HashSet;
use std::sync::Arc;

pub async fn truncate_all_download_tables(context: Arc<FoxyContext>) -> Result<(), DbErr> {
    debug!("Truncating download target tables");

    context
        .db()
        .transaction("truncate download tables", |txn| {
            Box::pin(async move {
                // Order: children first, then parents (respects FK dependencies)
                txn.execute("DELETE FROM download_patch_op", params![])
                    .await?;
                txn.execute("DELETE FROM download_patch_file", params![])
                    .await?;
                txn.execute("DELETE FROM download_target_file_part", params![])
                    .await?;
                txn.execute("DELETE FROM download_target_file", params![])
                    .await?;
                Ok(())
            })
        })
        .await?;

    debug!("Download target tables truncated");
    Ok(())
}

/// Drop queued download targets whose file is already verified on disk (a
/// non-empty local checksum equal to the remote one). A cancelled download
/// leaves the prepared queue with the files it finished and hashed, so the
/// next run would otherwise fetch them again. Returns the pruned file ids.
pub async fn prune_verified_download_targets(
    context: Arc<FoxyContext>,
    file_ids: &HashSet<u64>,
) -> Result<Vec<u64>, DbErr> {
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }
    let db = context.db();
    let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
    let mut ids: Vec<i64> = file_ids.iter().map(|id| *id as i64).collect();
    ids.sort_unstable();

    // The checksum says the bytes were verified once; the stat says they are
    // still there at full length (a rollback can revert a promoted file).
    let mut verified: Vec<i64> = Vec::new();
    for chunk in ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        let rows = db
            .query_all(
                &format!(
                    "SELECT f.id, f.local_content_hash, t.download_local_path, t.size FROM files f \
                     JOIN download_target_file t ON t.file_id = f.id \
                     WHERE f.id IN ({placeholders}) \
                       AND f.local_checksum != '' \
                       AND f.local_content_hash != '' \
                       AND lower(f.local_checksum) = lower(f.remote_checksum)"
                ),
                values,
            )
            .await?;
        for row in &rows {
            let (Ok(id), Ok(content_hash), Ok(path), Ok(size)) = (
                row.get_i64("id"),
                row.get_string("local_content_hash"),
                row.get_string("download_local_path"),
                row.get_i64("size"),
            ) else {
                continue;
            };
            let full_length = tokio::fs::metadata(&path)
                .await
                .is_ok_and(|meta| meta.is_file() && i64::try_from(meta.len()) == Ok(size));
            if !full_length {
                continue;
            }
            let current_hash = tokio::task::spawn_blocking(move || fast_file_content_hash(&path))
                .await
                .ok()
                .and_then(Result::ok);
            if current_hash.is_some_and(|hash| hash.eq_ignore_ascii_case(&content_hash)) {
                verified.push(id);
            }
        }
    }
    if verified.is_empty() {
        return Ok(Vec::new());
    }

    let verified = Arc::new(verified);
    let pruned = Arc::clone(&verified);
    db.transaction("prune verified download targets", move |txn| {
        let verified = Arc::clone(&verified);
        Box::pin(async move {
            for chunk in verified.chunks(chunk_size) {
                let placeholders = vec!["?"; chunk.len()].join(", ");
                for sql in [
                    format!("DELETE FROM download_patch_op WHERE file_id IN ({placeholders})"),
                    format!("DELETE FROM download_patch_file WHERE file_id IN ({placeholders})"),
                    format!(
                        "DELETE FROM download_target_file_part \
                         WHERE subfile_id IN (SELECT id FROM subfiles WHERE file_id IN ({placeholders}))"
                    ),
                    format!("DELETE FROM download_target_file WHERE file_id IN ({placeholders})"),
                ] {
                    let values: Vec<DbValue> =
                        chunk.iter().copied().map(DbValue::from).collect();
                    txn.execute(&sql, values).await?;
                }
            }
            Ok(())
        })
    })
    .await?;

    Ok(pruned.iter().map(|id| *id as u64).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::FoxyDb;

    #[tokio::test]
    async fn prune_drops_only_verified_files_and_their_children() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        let dir = tempfile::tempdir().expect("tempdir");
        let done = dir.path().join("done.pbo");
        std::fs::write(&done, b"12345").expect("write done");
        let done = done.to_string_lossy().replace('\\', "/");
        let done_hash = fast_file_content_hash(&done).expect("hash done");
        let reverted = dir
            .path()
            .join("reverted.pbo")
            .to_string_lossy()
            .replace('\\', "/");
        fdb.execute(
            "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash) VALUES \
             (1, 'done.pbo', 'r/done.pbo', 'l/done.pbo', 'abc', 'ABC', ?), \
             (2, 'partial.pbo', 'r/partial.pbo', 'l/partial.pbo', '', 'DEF', ''), \
             (3, 'stale.pbo', 'r/stale.pbo', 'l/stale.pbo', 'OLD', 'NEW', ''), \
             (4, 'reverted.pbo', 'r/reverted.pbo', 'l/reverted.pbo', 'ghi', 'GHI', '')",
            params![done_hash],
        )
        .await
        .expect("insert files");
        fdb.execute(
            "INSERT INTO subfiles (id, file_id, path) VALUES (10, 1, 'a'), (11, 1, 'b'), (20, 2, 'a')",
            params![],
        )
        .await
        .expect("insert subfiles");
        fdb.execute(
            "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size) VALUES \
             (1, 'u', ?, 5), (2, 'u', 'p', 1), (3, 'u', 'p', 1), (4, 'u', ?, 5)",
            params![done, reverted],
        )
        .await
        .expect("insert targets");
        fdb.execute(
            "INSERT INTO download_target_file_part (subfile_id, download_remote_url, download_local_path, size, offset) VALUES \
             (10, 'u', 'p', 1, 0), (11, 'u', 'p', 1, 1), (20, 'u', 'p', 1, 0)",
            params![],
        )
        .await
        .expect("insert target parts");
        fdb.execute(
            "INSERT INTO download_patch_file (file_id, patch_json_path, patch_blob_path) VALUES (1, 'j', 'b')",
            params![],
        )
        .await
        .expect("insert patch file");

        let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
        // File 4 is verified in the database but no longer on disk (reverted).
        let queued: HashSet<u64> = [1, 2, 3, 4].into_iter().collect();
        let pruned = prune_verified_download_targets(context.clone(), &queued)
            .await
            .expect("prune");
        assert_eq!(pruned, vec![1]);

        let count = |sql: &'static str| {
            let fdb = fdb.clone();
            async move {
                fdb.query_one(sql, params![])
                    .await
                    .expect("count")
                    .expect("row")
                    .get_i64("n")
                    .expect("n")
            }
        };
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM download_target_file").await,
            3
        );
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM download_target_file_part").await,
            1
        );
        assert_eq!(
            count("SELECT COUNT(*) AS n FROM download_patch_file").await,
            0
        );
        assert!(
            prune_verified_download_targets(context, &HashSet::new())
                .await
                .expect("empty prune")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn prune_keeps_same_length_file_when_its_fingerprint_changed() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("changed.pbo");
        std::fs::write(&path, b"before").expect("write original");
        let path = path.to_string_lossy().replace('\\', "/");
        let original_hash = fast_file_content_hash(&path).expect("hash original");
        std::fs::write(&path, b"edited").expect("write edit");

        fdb.execute(
            "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash) \
             VALUES (1, 'changed.pbo', 'r/changed.pbo', 'l/changed.pbo', 'abc', 'ABC', ?)",
            params![original_hash],
        )
        .await
        .expect("insert file");
        fdb.execute(
            "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size) \
             VALUES (1, 'u', ?, 6)",
            params![path],
        )
        .await
        .expect("insert target");

        let context = Arc::new(FoxyContext::new(db, reqwest::Client::new()));
        let queued = [1].into_iter().collect();
        assert!(
            prune_verified_download_targets(context, &queued)
                .await
                .expect("prune")
                .is_empty()
        );
    }
}
