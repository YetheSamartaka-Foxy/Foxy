use super::helpers::{join_path, mod_task_limit, resolve_mod_local_path};
use crate::core::addon_metadata::{
    extract_addon_display_name, regenerate_addon_display_names_for_ids,
};
use crate::core::db::{DbValue, FoxyDb};
use crate::core::models::context::FoxyContext;
use crate::core::models::modification::{ADDON_COLUMNS, FoxyMod};
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::models::repository::FoxyRepository;
use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;
use crate::core::tasks::remote_files::{ModRecheckStats, remote_files_transaction};
use log::{debug, warn};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
struct ParsedMod {
    mod_name: String,
    remote_path: String,
    local_path: String,
    enabled: bool,
    client_side: bool,
    checksum: String,
    data_order: i64,
    key: String,
}

#[derive(Clone, Debug, Default)]
struct ModRemoteGraphState {
    linked_to_repo_before: bool,
    file_count: i64,
    part_count: i64,
    missing_file_remote_checksums: i64,
    missing_part_remote_checksums: i64,
}

impl ModRemoteGraphState {
    fn complete(&self) -> bool {
        self.linked_to_repo_before
            && self.file_count > 0
            && self.part_count > 0
            && self.missing_file_remote_checksums == 0
            && self.missing_part_remote_checksums == 0
    }
}

fn normalize_path_key(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path)
}

async fn load_mod_remote_graph_states(
    db: &FoxyDb,
    mod_ids: &HashSet<i64>,
    linked_before: &HashSet<i64>,
) -> HashMap<i64, ModRemoteGraphState> {
    let mut states: HashMap<i64, ModRemoteGraphState> = mod_ids
        .iter()
        .map(|id| {
            (
                *id,
                ModRemoteGraphState {
                    linked_to_repo_before: linked_before.contains(id),
                    ..Default::default()
                },
            )
        })
        .collect();

    if mod_ids.is_empty() {
        return states;
    }

    let mut ids: Vec<i64> = mod_ids.iter().copied().collect();
    ids.sort_unstable();
    let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
    let mut idx = 0usize;
    while idx < ids.len() {
        let end = (idx + chunk_size).min(ids.len());
        let chunk = &ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            r#"SELECT
                   a.id AS addon_id,
                   COUNT(DISTINCT f.id) AS file_count,
                   COUNT(sf.id) AS part_count,
                   SUM(CASE WHEN f.id IS NOT NULL AND f.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_file_remote_checksums,
                   SUM(CASE WHEN sf.id IS NOT NULL AND sf.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_part_remote_checksums
               FROM addons a
               LEFT JOIN addon_files af ON af.addon_id = a.id
               LEFT JOIN files f ON f.id = af.file_id
               LEFT JOIN subfiles sf ON sf.file_id = f.id
               WHERE a.id IN ({})
               GROUP BY a.id"#,
            placeholders
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();

        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    let Ok(addon_id) = row.get_i64("addon_id") else {
                        continue;
                    };
                    let state = states
                        .entry(addon_id)
                        .or_insert_with(|| ModRemoteGraphState {
                            linked_to_repo_before: linked_before.contains(&addon_id),
                            ..Default::default()
                        });
                    state.file_count = row.get_i64("file_count").unwrap_or(0);
                    state.part_count = row.get_i64("part_count").unwrap_or(0);
                    state.missing_file_remote_checksums =
                        row.get_i64("missing_file_remote_checksums").unwrap_or(0);
                    state.missing_part_remote_checksums =
                        row.get_i64("missing_part_remote_checksums").unwrap_or(0);
                }
            }
            Err(err) => {
                warn!("Failed to load addon remote graph states: {}", err);
            }
        }

        idx = end;
    }

    states
}

/// Load existing addon rows for the given (remote_path, local_path) identity pairs,
/// keyed by `"{remote_path}|{local_path}"`. Chunked to stay under the SQLite bind limit.
async fn load_existing_mods_by_identity(
    db: &FoxyDb,
    parsed_mods: &[ParsedMod],
) -> HashMap<String, FoxyMod> {
    let mut map: HashMap<String, FoxyMod> = HashMap::new();
    // Each pair uses two bind params; keep below the SQLite bind limit.
    let chunk_size = (SQLITE_MAX_VARIABLES / 2).saturating_sub(4).max(1);
    let mut idx = 0;
    while idx < parsed_mods.len() {
        let end = usize::min(idx + chunk_size, parsed_mods.len());
        let chunk = &parsed_mods[idx..end];
        let clauses = vec!["(remote_path = ? AND local_path = ?)"; chunk.len()].join(" OR ");
        let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 2);
        for m in chunk {
            values.push(m.remote_path.clone().into());
            values.push(m.local_path.clone().into());
        }
        let sql = format!("SELECT {ADDON_COLUMNS} FROM addons WHERE {clauses}");

        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    match FoxyMod::from_row(&row) {
                        Ok(m) => {
                            let key = format!("{}|{}", m.remote_path, m.local_path);
                            map.insert(key, m);
                        }
                        Err(err) => warn!("Failed to read addon row: {}", err),
                    }
                }
            }
            Err(err) => {
                warn!("Failed to load existing mod records batch: {}", err);
            }
        }
        idx = end;
    }
    map
}

pub(super) async fn process_mods_upsert(
    context: Arc<FoxyContext>,
    repository_parent: Arc<FoxyRepository>,
    mods_data: Value,
    required: bool,
    enabled_overrides: Option<Arc<HashMap<String, bool>>>,
) -> (Vec<ModRecheckStats>, HashSet<i64>) {
    let parsed_mods: Vec<ParsedMod> = mods_data
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter_map(|(data_order, mod_data)| {
            let mod_name = mod_data
                .get("modName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if mod_name.is_empty() {
                return None; // Skip if mod_name is empty
            }

            let enabled = mod_data
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let enabled = enabled_overrides
                .as_ref()
                .and_then(|overrides| overrides.get(&mod_name.to_lowercase()).copied())
                .unwrap_or(enabled);
            let checksum = mod_data
                .get("checkSum")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let client_side = mod_data
                .get("clientSide")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let remote_path = join_path(&repository_parent.remote_url, &mod_name);
            let local_path = resolve_mod_local_path(
                &repository_parent.local_path,
                context.repository_space_shared_path.as_deref(),
                &mod_name,
            );

            let key = format!("{}|{}", remote_path, local_path);
            Some(ParsedMod {
                mod_name,
                remote_path,
                local_path,
                enabled,
                client_side,
                checksum,
                data_order: data_order as i64,
                key,
            })
        })
        .collect();

    if parsed_mods.is_empty() {
        return (Vec::new(), HashSet::new());
    }

    let db = context.db();

    // Prefetch existing mods to preserve local checksums and reuse after upsert
    let mut existing_mods: HashMap<String, FoxyMod> =
        load_existing_mods_by_identity(&db, &parsed_mods).await;

    // Single batched upsert instead of per-row insert + fetch
    let previous_mods_by_key = existing_mods.clone();
    {
        let build_values = || -> Vec<DbValue> {
            let mut values: Vec<DbValue> = Vec::with_capacity(parsed_mods.len() * 11);
            for m in &parsed_mods {
                let existing = existing_mods.get(&m.key);
                values.push(m.mod_name.clone().into());
                values.push(
                    extract_addon_display_name(&m.local_path)
                        .unwrap_or_default()
                        .into(),
                );
                values.push(m.remote_path.clone().into());
                values.push(m.local_path.clone().into());
                values.push(m.client_side.into());
                values.push(m.enabled.into());
                values.push(
                    existing
                        .map(|e| e.local_checksum.clone())
                        .unwrap_or_default()
                        .into(),
                );
                values.push(m.checksum.clone().into());
                values.push(
                    existing
                        .map(|e| e.local_content_hash.clone())
                        .unwrap_or_default()
                        .into(),
                );
                values.push(required.into());
                values.push(m.data_order.into());
            }
            values
        };

        let row_placeholder = "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        let placeholders = vec![row_placeholder; parsed_mods.len()].join(", ");
        let sql = format!(
            "INSERT INTO addons \
             (name, display_name, remote_path, local_path, client_side, enabled, \
              local_checksum, remote_checksum, local_content_hash, required, data_order) \
             VALUES {placeholders} \
             ON CONFLICT(name, remote_path, local_path) DO UPDATE SET \
                enabled = excluded.enabled, \
                client_side = excluded.client_side, \
                display_name = excluded.display_name, \
                remote_checksum = excluded.remote_checksum, \
                required = excluded.required, \
                data_order = excluded.data_order"
        );

        if let Err(e) = db.execute_retry("addon upsert", &sql, build_values()).await {
            warn!(
                "Failed to upsert mods for repository {}: {}",
                repository_parent.remote_url, e
            );
            return (Vec::new(), HashSet::new());
        }
    }

    // Reuse prefetched models when no new mods were added; re-query only for new IDs
    let has_new_mods = parsed_mods
        .iter()
        .any(|m| !existing_mods.contains_key(&m.key));

    let mods_by_key: HashMap<String, FoxyMod> = if has_new_mods {
        load_existing_mods_by_identity(&db, &parsed_mods).await
    } else {
        // Overlay fields updated by the upsert onto prefetched models
        for m in &parsed_mods {
            if let Some(existing) = existing_mods.get_mut(&m.key) {
                existing.remote_checksum = m.checksum.clone();
                existing.enabled = m.enabled;
                existing.client_side = m.client_side;
                existing.required = required;
                existing.data_order = m.data_order;
            }
        }
        existing_mods
    };

    let mut all_mods: Vec<FoxyMod> = Vec::with_capacity(parsed_mods.len());
    for parsed in &parsed_mods {
        if let Some(mut m) = mods_by_key.get(&parsed.key).cloned() {
            m.enabled = parsed.enabled;
            all_mods.push(m);
        } else {
            warn!("Missing mod record after upsert: {}", parsed.remote_path);
        }
    }

    let resolved_mod_ids: HashSet<i64> = all_mods.iter().map(|m| m.id as i64).collect();
    regenerate_addon_display_names_for_ids(
        &db,
        &resolved_mod_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await;

    let existing_repo_linked_mod_ids: HashSet<i64> = if resolved_mod_ids.is_empty() {
        HashSet::new()
    } else {
        let mut linked: HashSet<i64> = HashSet::new();
        let mut ids: Vec<i64> = resolved_mod_ids.iter().copied().collect();
        ids.sort_unstable();
        let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(1).max(1);
        for chunk in ids.chunks(chunk_size) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT addon_id FROM repository_addons \
                 WHERE repository_id = ? AND addon_id IN ({placeholders})"
            );
            let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() + 1);
            values.push((repository_parent.id as i64).into());
            for id in chunk {
                values.push((*id).into());
            }
            match db.query_all(&sql, values).await {
                Ok(rows) => {
                    for row in rows {
                        if let Ok(addon_id) = row.get_i64("addon_id") {
                            linked.insert(addon_id);
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        "Failed to load existing repository addon links for {}: {}",
                        repository_parent.remote_url, err
                    );
                }
            }
        }
        linked
    };

    if !all_mods.is_empty() {
        let repo_id = repository_parent.id as i64;
        let build_values = || -> Vec<DbValue> {
            let mut values: Vec<DbValue> = Vec::with_capacity(all_mods.len() * 2);
            for m in &all_mods {
                values.push(repo_id.into());
                values.push((m.id as i64).into());
            }
            values
        };
        let placeholders = vec!["(?, ?)"; all_mods.len()].join(", ");
        let sql = format!(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES {placeholders} \
             ON CONFLICT(repository_id, addon_id) DO NOTHING"
        );

        if let Err(e) = db
            .execute_retry("repository addon upsert", &sql, build_values())
            .await
        {
            warn!(
                "Failed to link mods to repository {}: {}",
                repository_parent.remote_url, e
            );
        }
    }

    let graph_states =
        load_mod_remote_graph_states(&db, &resolved_mod_ids, &existing_repo_linked_mod_ids).await;

    // Process mods
    let mod_limit = mod_task_limit();
    let mod_semaphore = Arc::new(Semaphore::new(mod_limit));
    let mut tasks = Vec::new();

    for mod_entry in all_mods {
        if !mod_entry.enabled {
            debug!(
                "Skipping disabled mod during remote recheck: {}",
                mod_entry.remote_path
            );
            continue;
        }
        let repository_parent_clone = repository_parent.clone();
        let context_clone = context.clone();
        let mod_semaphore_clone = mod_semaphore.clone();
        let previous_key = format!("{}|{}", mod_entry.remote_path, mod_entry.local_path);
        let previous_mod = previous_mods_by_key.get(&previous_key).cloned();
        let graph_state = graph_states
            .get(&(mod_entry.id as i64))
            .cloned()
            .unwrap_or_default();
        tasks.push(tokio::spawn(async move {
            let _permit = mod_semaphore_clone.acquire_owned().await.ok();
            let local_mod_exists = Path::new(mod_entry.local_path.trim()).exists();
            let has_content_hash = !mod_entry.local_content_hash.trim().is_empty();
            let force_mod_refresh = context_clone
                .forced_mod_refreshes
                .contains(&mod_entry.name.to_lowercase());
            let remote_graph_unchanged = previous_mod.as_ref().is_some_and(|previous| {
                previous.remote_checksum == mod_entry.remote_checksum
                    && normalize_path_key(&previous.local_path)
                        == normalize_path_key(&mod_entry.local_path)
                    && previous.enabled == mod_entry.enabled
                    && previous.client_side == mod_entry.client_side
                    && previous.required == mod_entry.required
            });
            if remote_graph_unchanged
                && graph_state.complete()
                && local_mod_exists
                && has_content_hash
                && context_clone.recheck_level < RecheckLevel::MOD
            {
                debug!(
                    "Up-to-date remote graph: Mod: {} (files={} parts={} local_checksum_match={} pending_forced={}).",
                    mod_entry.remote_path,
                    graph_state.file_count,
                    graph_state.part_count,
                    mod_entry.remote_checksum == mod_entry.local_checksum,
                    force_mod_refresh
                );
                return None;
            }
            if mod_entry.remote_checksum == mod_entry.local_checksum
                && local_mod_exists
                && has_content_hash
                && !force_mod_refresh
                && context_clone.recheck_level < RecheckLevel::MOD
            {
                debug!("Up-to-date: Mod: {}.", mod_entry.remote_path.clone());
                return None;
            }
            if force_mod_refresh {
                debug!(
                    "Recheck needed: Mod: {} (forced by pending local mismatch)",
                    mod_entry.remote_path.clone()
                );
            } else {
                debug!("Recheck needed: Mod: {}", mod_entry.remote_path.clone());
            }

            let mod_entry = Arc::new(mod_entry);
            let mut stats =
                remote_files_transaction(context_clone, repository_parent_clone, mod_entry.clone())
                    .await;
            stats.mod_concurrency_limit = mod_limit;
            Some(stats)
        }))
    }

    let mut collected = Vec::new();
    for task in tasks {
        match task.await {
            Ok(Some(stats)) => collected.push(stats),
            Ok(None) => {}
            Err(err) => {
                warn!("Mod file processing task failed: {}", err);
            }
        }
    }

    (collected, resolved_mod_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_remote_graph_state_complete_requires_existing_link_files_and_parts() {
        let state = ModRemoteGraphState {
            linked_to_repo_before: true,
            file_count: 2,
            part_count: 8,
            missing_file_remote_checksums: 0,
            missing_part_remote_checksums: 0,
        };
        assert!(state.complete());
    }

    #[test]
    fn mod_remote_graph_state_incomplete_without_previous_repo_link() {
        let state = ModRemoteGraphState {
            linked_to_repo_before: false,
            file_count: 2,
            part_count: 8,
            missing_file_remote_checksums: 0,
            missing_part_remote_checksums: 0,
        };
        assert!(!state.complete());
    }

    #[test]
    fn mod_remote_graph_state_incomplete_with_missing_remote_part_checksum() {
        let state = ModRemoteGraphState {
            linked_to_repo_before: true,
            file_count: 2,
            part_count: 8,
            missing_file_remote_checksums: 0,
            missing_part_remote_checksums: 1,
        };
        assert!(!state.complete());
    }
}
