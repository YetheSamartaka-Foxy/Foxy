use crate::core::db::{DbValue, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::repository::FoxyRepository;
use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;
use log::warn;
use std::collections::HashSet;
use std::sync::Arc;

pub(super) async fn reconcile_repository_addon_links(
    context: Arc<FoxyContext>,
    repository_parent: Arc<FoxyRepository>,
    desired_mod_ids: HashSet<i64>,
    expected_pair_count: usize,
) {
    let db = context.db();

    let can_prune_stale = desired_mod_ids.len() == expected_pair_count;
    if !can_prune_stale && expected_pair_count > 0 {
        warn!(
            "Skipping stale repository_addons cleanup for {}: resolved {} of {} desired mods",
            repository_parent.remote_url,
            desired_mod_ids.len(),
            expected_pair_count
        );
    }

    let repo_id = repository_parent.id as i64;
    let mut desired_ids: Vec<i64> = desired_mod_ids.iter().copied().collect();
    desired_ids.sort_unstable();

    if let Err(e) = db
        .transaction("reconcile repository_addons", |txn| {
            let desired_ids = desired_ids.clone();
            let desired_mod_ids = desired_mod_ids.clone();
            Box::pin(async move {
                // Insert desired links (chunked multi-row INSERT ... ON CONFLICT DO NOTHING).
                if !desired_ids.is_empty() {
                    let chunk_size = crate::core::tasks::init_database::bulk_write_rows_for(2);
                    for chunk in desired_ids.chunks(chunk_size) {
                        let placeholders = vec!["(?, ?)"; chunk.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO repository_addons (repository_id, addon_id) VALUES {} \
                             ON CONFLICT(repository_id, addon_id) DO NOTHING",
                            placeholders
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 2);
                        for addon_id in chunk {
                            values.push(repo_id.into());
                            values.push((*addon_id).into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                }

                if can_prune_stale {
                    if desired_mod_ids.is_empty() {
                        txn.execute(
                            "DELETE FROM repository_addons WHERE repository_id = ?",
                            params![repo_id],
                        )
                        .await?;
                    } else {
                        let rows = txn
                            .query_all(
                                "SELECT addon_id FROM repository_addons WHERE repository_id = ?",
                                params![repo_id],
                            )
                            .await?;
                        let stale_ids: Vec<i64> = rows
                            .iter()
                            .filter_map(|row| row.get_i64("addon_id").ok())
                            .filter(|id| !desired_mod_ids.contains(id))
                            .collect();

                        if !stale_ids.is_empty() {
                            let delete_chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(1).max(1);
                            for chunk in stale_ids.chunks(delete_chunk_size) {
                                let placeholders = vec!["?"; chunk.len()].join(", ");
                                let sql = format!(
                                    "DELETE FROM repository_addons \
                                     WHERE repository_id = ? AND addon_id IN ({})",
                                    placeholders
                                );
                                let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() + 1);
                                values.push(repo_id.into());
                                for id in chunk {
                                    values.push((*id).into());
                                }
                                txn.execute(&sql, values).await?;
                            }
                        }
                    }
                }

                Ok(())
            })
        })
        .await
    {
        warn!(
            "Failed to reconcile repository_addons for {}: {}",
            repository_parent.remote_url, e
        );
    }
}
