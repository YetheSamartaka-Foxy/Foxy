use std::collections::HashMap;
use std::sync::Arc;

use log::{info, warn};

use crate::core::db::DbValue;
use crate::core::models::context::FoxyContext;
use crate::core::models::repository::load_repository_by_remote_url_and_local_path;
use crate::core::tasks::init_database::read_chunk_ids;
use crate::core::utils::format::sanitize_log_url;

/// Write the caller's effective addon selection into `addons.enabled` for one
/// repository instance.
///
/// Quick scan readiness, the repository content-hash rollup and the pending
/// update scope all read `addons.enabled` as the "is this addon in scope"
/// signal, but until now it was only written during a remote metadata rebuild.
/// A rebuild is skipped whenever the remote checksum is unchanged, so a
/// deselected optional addon could keep `enabled = 1` forever and hold the
/// whole repository out of the quick scan fast path.
///
/// `overrides` maps lowercase addon names to their effective enabled state.
/// Returns the number of addon rows whose stored value changed.
pub(crate) async fn persist_repository_addon_enabled_states(
    context: Arc<FoxyContext>,
    repository_url: &str,
    local_path: &str,
    overrides: &HashMap<String, bool>,
) -> u64 {
    if overrides.is_empty() {
        return 0;
    }

    let repository = match load_repository_by_remote_url_and_local_path(
        context.clone(),
        repository_url,
        local_path,
    )
    .await
    {
        Ok(repository) => repository,
        Err(err) => {
            warn!(
                "Failed to load repository for addon-selection persist {}: {}",
                sanitize_log_url(repository_url),
                err
            );
            return 0;
        }
    };

    let db = context.db();
    let repository_id = repository.id as i64;
    let chunk_size = read_chunk_ids().saturating_sub(2).max(1);
    let mut updated = 0u64;

    for target in [true, false] {
        let names: Vec<&String> = overrides
            .iter()
            .filter(|(_, enabled)| **enabled == target)
            .map(|(name, _)| name)
            .collect();
        if names.is_empty() {
            continue;
        }

        for chunk in names.chunks(chunk_size) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "UPDATE addons SET enabled = ? \
                 WHERE id IN (SELECT addon_id FROM repository_addons WHERE repository_id = ?) \
                 AND enabled != ? \
                 AND LOWER(name) IN ({placeholders})"
            );
            let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() + 3);
            values.push(target.into());
            values.push(repository_id.into());
            values.push(target.into());
            values.extend(chunk.iter().map(|name| DbValue::from(name.as_str())));

            match db
                .execute_retry("addon enabled state persist", &sql, values)
                .await
            {
                Ok(changed) => updated = updated.saturating_add(changed),
                Err(err) => warn!(
                    "Failed to persist addon selection for {}: {}",
                    sanitize_log_url(repository_url),
                    err
                ),
            }
        }
    }

    if updated > 0 {
        info!(
            "Persisted addon selection for repo={} changed_rows={}",
            sanitize_log_url(repository_url),
            updated
        );
    }
    updated
}
