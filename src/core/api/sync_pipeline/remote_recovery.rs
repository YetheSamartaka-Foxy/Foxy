use crate::core::db::{DbErr, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::repository::load_repository_by_remote_url;
use std::sync::Arc;

pub(super) async fn invalidate_remote_graph(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> Result<(), DbErr> {
    let repository = load_repository_by_remote_url(context.clone(), repository_url).await?;
    let repository_id = repository.id as i64;
    let db = context.db();
    let addons = db
        .query_all(
            "SELECT a.remote_path FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id \
             WHERE ra.repository_id = ? AND a.enabled = 1",
            params![repository_id],
        )
        .await?;
    if let Some(cache) = context.manifest_cache.as_ref() {
        let paths = addons
            .iter()
            .map(|row| row.get_string("remote_path"))
            .collect::<Result<Vec<_>, _>>()?;
        let cache = cache.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for path in paths {
                let path = path.trim_end_matches('/');
                cache.forget(&format!("{path}/foxy_addon.json"));
                cache.forget(&format!("{path}/mod.srf"));
            }
        })
        .await;
    }

    // Content rollups omit names, so a missing URL invalidates the whole instance's file list.
    db.transaction("invalidate missing remote file graph", move |txn| {
        Box::pin(async move {
            let addon_scope = "SELECT a.id FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id \
                               WHERE ra.repository_id = ? AND a.enabled = 1";
            let file_scope = format!("SELECT file_id FROM addon_files WHERE addon_id IN ({addon_scope})");
            txn.execute(
                &format!("UPDATE repositories SET remote_checksum = '' WHERE id = ? OR id IN \
                         (SELECT repository_id FROM repository_addons WHERE addon_id IN ({addon_scope}))"),
                params![repository_id, repository_id],
            ).await?;
            txn.execute(
                &format!("UPDATE addons SET remote_checksum = '' WHERE id IN ({addon_scope})"),
                params![repository_id],
            ).await?;
            txn.execute(
                &format!("DELETE FROM download_target_file_part WHERE subfile_id IN \
                         (SELECT id FROM subfiles WHERE file_id IN ({file_scope}))"),
                params![repository_id],
            ).await?;
            for table in ["download_target_file", "download_patch_op", "download_patch_file"] {
                txn.execute(
                    &format!("DELETE FROM {table} WHERE file_id IN ({file_scope})"),
                    params![repository_id],
                ).await?;
            }
            Ok(())
        })
    }).await?;
    crate::core::models::pending_update::clear_pending_update_for_context(context, repository_url)
        .await
}

#[cfg(test)]
#[path = "remote_recovery_tests.rs"]
mod tests;
