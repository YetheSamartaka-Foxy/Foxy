use super::super::*;
use super::db_helpers::load_patch_download_bytes_by_file_ids;
use crate::core::db::{DbValue, FoxyDb, params};
use crate::core::models::download_target_file::fetch_all_download_targets_with_mod_and_name;
use crate::core::models::modification::ADDON_COLUMNS;
use crate::core::models::modification_file::FILE_COLUMNS;
use crate::core::models::modification_file_part::{
    FoxyModFilePart, SUBFILE_COLUMNS, part_display_path,
};
use crate::core::tasks::download_files::{
    apply_download_plan_bytes, build_download_estimate_diffs,
};
use crate::core::tasks::remote_file_parts::{
    FilePartData, FilePartsPayload, remote_file_parts_batch,
};

fn normalize_pending_file_name(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches('/')
        .to_lowercase()
}

fn pending_file_name_leaf(name: &str) -> &str {
    name.rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(name)
}

fn patch_plan_estimate_bytes(file_length: u64, planned_patch_bytes: Option<u64>) -> u64 {
    planned_patch_bytes.unwrap_or(file_length).min(file_length)
}

pub(crate) async fn persist_pending_updates(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mods: &[ModDiffSummary],
) {
    let started = Instant::now();
    let has_updates = mods.iter().any(|m| m.needs_update);

    if has_updates {
        let update_mods = mods.iter().filter(|m| m.needs_update).count();
        let update_files: usize = mods
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.files.len())
            .sum();
        let estimated_bytes: u64 = mods
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.total_bytes)
            .sum();
        info!(
            "Caching pending updates for repo {}: mods={} files={} estimated_transfer_bytes={}",
            repo_url, update_mods, update_files, estimated_bytes
        );
        match serde_json::to_string(mods) {
            Ok(payload) => {
                if let Err(e) = save_pending_update_for_context(context, repo_url, &payload).await {
                    warn!("Failed to cache pending updates for {}: {}", repo_url, e);
                } else {
                    info!(
                        "Pending update persistence completed: repo={} action=save mods={} update_mods={} update_files={} elapsed={:.2?}",
                        repo_url,
                        mods.len(),
                        update_mods,
                        update_files,
                        started.elapsed()
                    );
                }
            }
            Err(e) => warn!(
                "Failed to serialize pending updates for {}: {}",
                repo_url, e
            ),
        }
    } else if let Err(e) = clear_pending_update_for_context(context, repo_url).await {
        warn!("Failed to clear pending updates for {}: {}", repo_url, e);
    } else {
        info!(
            "Pending update persistence completed: repo={} action=clear mods={} elapsed={:.2?}",
            repo_url,
            mods.len(),
            started.elapsed()
        );
    }
}

pub(crate) async fn collect_repo_download_targets(
    context: Arc<FoxyContext>,
    repo_url: &str,
    allowed_mod_names: Option<&HashSet<String>>,
) -> (HashSet<u64>, HashSet<u64>) {
    let mut file_ids = HashSet::new();
    let mut mod_ids = HashSet::new();

    let repo = match load_repository_by_remote_url(context.clone(), repo_url).await {
        Ok(repo) => repo,
        Err(err) => {
            warn!("Failed to load repository for cached targets: {}", err);
            return (file_ids, mod_ids);
        }
    };

    let db = context.db();
    let repo_mod_ids: HashSet<u64> = match db
        .query_all(
            "SELECT addon_id FROM repository_addons WHERE repository_id = ?",
            params![repo.id as i64],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_i64("addon_id").ok())
            .map(|id| id as u64)
            .collect(),
        Err(err) => {
            warn!("Failed to load repository mods for cached targets: {}", err);
            return (file_ids, mod_ids);
        }
    };
    if repo_mod_ids.is_empty() {
        return (file_ids, mod_ids);
    }

    // When a mod-name filter is provided, use the name-aware query so we can
    // restrict the download queue to only the addons the quick scan identified
    // as needing updates.  Without this filter, stale download-target rows
    // created by remote_repository() for *all* mismatched files would leak
    // unrelated mods into the download queue.
    if let Some(allowed) = allowed_mod_names {
        let targets = match fetch_all_download_targets_with_mod_and_name(context.clone()).await {
            Ok(targets) => targets,
            Err(err) => {
                warn!(
                    "Failed to load download targets with mod names for cached targets: {}",
                    err
                );
                return (file_ids, mod_ids);
            }
        };

        for target in targets {
            if repo_mod_ids.contains(&target.mod_id)
                && allowed.contains(&target.mod_name.to_lowercase())
            {
                file_ids.insert(target.download.file_id);
                mod_ids.insert(target.mod_id);
            }
        }
    } else {
        let targets = match fetch_all_download_targets_with_mod(context.clone()).await {
            Ok(targets) => targets,
            Err(err) => {
                warn!(
                    "Failed to load download targets for cached targets: {}",
                    err
                );
                return (file_ids, mod_ids);
            }
        };

        for target in targets {
            if repo_mod_ids.contains(&target.mod_id) {
                file_ids.insert(target.download.file_id);
                mod_ids.insert(target.mod_id);
            }
        }
    }

    (file_ids, mod_ids)
}

pub(crate) async fn refresh_patch_plan_metadata_for_pending_updates(
    context: Arc<FoxyContext>,
    repository_url: &str,
    allowed_mod_names: Option<&HashSet<String>>,
) -> usize {
    let started = Instant::now();
    let repo = match load_repository_by_remote_url(context.clone(), repository_url).await {
        Ok(repo) => repo,
        Err(err) => {
            warn!(
                "Skipping patch-plan metadata refresh for repo={}: failed to load repository: {}",
                repository_url, err
            );
            return 0;
        }
    };

    let db = context.db();
    let mut mod_ids: Vec<i64> = match db
        .query_all(
            "SELECT addon_id FROM repository_addons WHERE repository_id = ?",
            params![repo.id as i64],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_i64("addon_id").ok())
            .collect(),
        Err(err) => {
            warn!(
                "Skipping patch-plan metadata refresh for repo={}: failed to load repository addons: {}",
                repository_url, err
            );
            return 0;
        }
    };
    mod_ids.sort_unstable();
    mod_ids.dedup();
    if mod_ids.is_empty() {
        return 0;
    }

    let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
    let mut allowed_mod_ids = HashSet::new();
    for chunk in mod_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let enabled_clause = if allowed_mod_names.is_none() {
            " AND enabled = 1"
        } else {
            ""
        };
        let sql =
            format!("SELECT id, name FROM addons WHERE id IN ({placeholders}){enabled_clause}");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                allowed_mod_ids.extend(rows.into_iter().filter_map(|row| {
                    let id = row.get_i64("id").ok()?;
                    let name = row.get_string("name").ok()?;
                    if allowed_mod_names
                        .map(|names| names.is_empty() || names.contains(&name.to_lowercase()))
                        .unwrap_or(true)
                    {
                        Some(id)
                    } else {
                        None
                    }
                }));
            }
            Err(err) => {
                warn!(
                    "Failed to load addons for patch-plan metadata refresh repo={}: {}",
                    repository_url, err
                );
                return 0;
            }
        }
    }

    if allowed_mod_ids.is_empty() {
        return 0;
    }

    let mut file_ids = Vec::new();
    for chunk in allowed_mod_ids
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .chunks(chunk_size)
    {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("SELECT file_id FROM addon_files WHERE addon_id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => file_ids.extend(rows.iter().filter_map(|row| row.get_i64("file_id").ok())),
            Err(err) => {
                warn!(
                    "Failed to load addon-file links for patch-plan metadata refresh repo={}: {}",
                    repository_url, err
                );
                return 0;
            }
        }
    }
    file_ids.sort_unstable();
    file_ids.dedup();
    if file_ids.is_empty() {
        return 0;
    }

    let mut files_by_id: HashMap<i64, FoxyModFile> = HashMap::new();
    for chunk in file_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE id IN ({placeholders}) ORDER BY data_order ASC"
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    if let Ok(file) = FoxyModFile::from_row(&row) {
                        files_by_id.insert(file.id as i64, file);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "Failed to load files for patch-plan metadata refresh repo={}: {}",
                    repository_url, err
                );
                return 0;
            }
        }
    }

    let mut parts_by_file_id: HashMap<i64, Vec<FoxyModFilePart>> = HashMap::new();
    for chunk in file_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT {SUBFILE_COLUMNS} FROM subfiles WHERE file_id IN ({placeholders}) \
             ORDER BY data_order ASC"
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    if let Ok(part) = FoxyModFilePart::from_row(&row) {
                        parts_by_file_id
                            .entry(part.file_id as i64)
                            .or_default()
                            .push(part);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "Failed to load file parts for patch-plan metadata refresh repo={}: {}",
                    repository_url, err
                );
                return 0;
            }
        }
    }

    let mut payloads = Vec::with_capacity(files_by_id.len());
    let mut files = files_by_id.into_iter().collect::<Vec<_>>();
    files.sort_by_key(|(_, file)| file.data_order);
    for (file_id, file) in files {
        let mut parts = parts_by_file_id.remove(&file_id).unwrap_or_default();
        parts.sort_by_key(|part| part.data_order);
        payloads.push(FilePartsPayload {
            file,
            previous_file: None,
            parts: parts
                .into_iter()
                .map(|part| FilePartData {
                    path: part_display_path(&part.path).to_string(),
                    checksum: part.remote_checksum,
                    start: part.remote_start as i64,
                    length: part.remote_length as i64,
                    data_order: part.data_order,
                })
                .collect(),
        });
    }

    let payload_count = payloads.len();
    if payload_count > 0 {
        let refresh_context = Arc::new(
            context
                .as_ref()
                .clone()
                .with_patch_plan_metadata_refresh(true),
        );
        remote_file_parts_batch(refresh_context, payloads).await;
    }
    info!(
        "Patch-plan metadata refresh finished for repo={} files={} elapsed={:.2?}",
        repository_url,
        payload_count,
        started.elapsed()
    );
    payload_count
}

pub(crate) async fn apply_patch_plan_estimates_to_pending_updates(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mods: &[ModDiffSummary],
) -> Option<Vec<ModDiffSummary>> {
    let old_estimated_bytes: u64 = mods
        .iter()
        .filter(|m| m.needs_update)
        .map(|m| m.total_bytes)
        .sum();
    let update_mod_names: HashSet<String> = mods
        .iter()
        .filter(|m| m.needs_update)
        .map(|m| m.name.to_lowercase())
        .collect();
    if update_mod_names.is_empty() {
        return None;
    }

    let db = context.db();
    let repo = match load_repository_by_remote_url(context.clone(), repo_url).await {
        Ok(repo) => repo,
        Err(err) => {
            warn!(
                "Failed to load repository for patch-plan pending estimate repo={}: {}",
                repo_url, err
            );
            return None;
        }
    };
    let scoped_mod_ids: Vec<i64> = match db
        .query_all(
            "SELECT addon_id FROM repository_addons WHERE repository_id = ?",
            params![repo.id as i64],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_i64("addon_id").ok())
            .collect(),
        Err(err) => {
            warn!(
                "Failed to load repository addons for patch-plan pending estimate repo={}: {}",
                repo_url, err
            );
            return None;
        }
    };
    if scoped_mod_ids.is_empty() {
        return None;
    }

    let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
    let mut mod_rows = Vec::new();
    for chunk in scoped_mod_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("SELECT {ADDON_COLUMNS} FROM addons WHERE id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                mod_rows.extend(rows.iter().filter_map(|row| {
                    let addon = FoxyMod::from_row(row).ok()?;
                    update_mod_names
                        .contains(&addon.name.to_lowercase())
                        .then_some(addon)
                }));
            }
            Err(err) => {
                warn!(
                    "Failed to load addons for patch-plan pending estimate repo={}: {}",
                    repo_url, err
                );
                return None;
            }
        }
    }
    if mod_rows.is_empty() {
        return None;
    }

    let mut mod_id_by_name = HashMap::new();
    let mut mod_ids = Vec::with_capacity(mod_rows.len());
    for row in mod_rows {
        mod_id_by_name.insert(row.name.to_lowercase(), row.id as i64);
        mod_ids.push(row.id as i64);
    }

    let mut mod_file_links: Vec<(i64, i64)> = Vec::new();
    for chunk in mod_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql =
            format!("SELECT addon_id, file_id FROM addon_files WHERE addon_id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => mod_file_links.extend(rows.iter().filter_map(|row| {
                Some((row.get_i64("addon_id").ok()?, row.get_i64("file_id").ok()?))
            })),
            Err(err) => {
                warn!(
                    "Failed to load addon files for patch-plan pending estimate repo={}: {}",
                    repo_url, err
                );
                return None;
            }
        }
    }
    if mod_file_links.is_empty() {
        return None;
    }

    let mut file_ids: Vec<i64> = mod_file_links.iter().map(|(_, file_id)| *file_id).collect();
    file_ids.sort_unstable();
    file_ids.dedup();

    let mut files_by_id = HashMap::new();
    for chunk in file_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    if let Ok(file) = FoxyModFile::from_row(&row) {
                        files_by_id.insert(file.id as i64, file);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "Failed to load files for patch-plan pending estimate repo={}: {}",
                    repo_url, err
                );
                return None;
            }
        }
    }
    if files_by_id.is_empty() {
        return None;
    }

    let patch_bytes_by_file_id = load_patch_download_bytes_by_file_ids(&db, &file_ids, chunk_size)
        .await
        .into_iter()
        .collect::<HashMap<_, _>>();

    let mut file_lookup = HashMap::<(i64, String), (i64, u64)>::new();
    for (addon_id, file_id) in &mod_file_links {
        if let Some(file) = files_by_id.get(file_id) {
            let full_key = normalize_pending_file_name(&file.name);
            file_lookup.insert((*addon_id, full_key), (file.id as i64, file.length));
            let leaf_key = normalize_pending_file_name(pending_file_name_leaf(&file.name));
            file_lookup.insert((*addon_id, leaf_key), (file.id as i64, file.length));
        }
    }

    let mut adjusted = mods.to_vec();
    let mut resolved_files = 0usize;
    let mut unresolved_files = 0usize;
    let mut patch_files = 0usize;
    let mut full_files = 0usize;

    for mod_summary in adjusted.iter_mut().filter(|m| m.needs_update) {
        let Some(mod_id) = mod_id_by_name
            .get(&mod_summary.name.to_lowercase())
            .copied()
        else {
            unresolved_files = unresolved_files.saturating_add(mod_summary.files.len());
            continue;
        };

        let mut total_bytes = 0u64;
        for file_summary in &mut mod_summary.files {
            let key = normalize_pending_file_name(&file_summary.name);
            let Some((file_id, file_length)) = file_lookup.get(&(mod_id, key)).copied() else {
                total_bytes = total_bytes.saturating_add(file_summary.total_bytes);
                unresolved_files += 1;
                continue;
            };

            let patch_bytes = patch_bytes_by_file_id.get(&file_id).copied();
            let estimate = patch_plan_estimate_bytes(file_length, patch_bytes);
            file_summary.total_bytes = estimate;
            total_bytes = total_bytes.saturating_add(estimate);
            resolved_files += 1;
            if patch_bytes.is_some() && estimate < file_length {
                patch_files += 1;
            } else {
                full_files += 1;
            }
        }
        mod_summary.total_bytes = total_bytes;
    }

    if resolved_files == 0 {
        return None;
    }

    let new_estimated_bytes: u64 = adjusted
        .iter()
        .filter(|m| m.needs_update)
        .map(|m| m.total_bytes)
        .sum();
    info!(
        "Patch-plan pending estimate applied for repo={}: mods={} resolved_files={} unresolved_files={} patch_files={} full_files={} old_estimated_transfer_bytes={} adjusted_estimated_transfer_bytes={}",
        repo_url,
        adjusted.iter().filter(|m| m.needs_update).count(),
        resolved_files,
        unresolved_files,
        patch_files,
        full_files,
        old_estimated_bytes,
        new_estimated_bytes
    );

    Some(adjusted)
}

pub(crate) async fn apply_download_target_estimates_to_pending_updates(
    context: Arc<FoxyContext>,
    repo_url: &str,
    allowed_mod_names: Option<&HashSet<String>>,
) -> Option<Vec<ModDiffSummary>> {
    let (file_ids, _) =
        collect_repo_download_targets(context.clone(), repo_url, allowed_mod_names).await;
    if file_ids.is_empty() {
        return None;
    }

    let mut targets = match fetch_all_download_targets_with_mod_and_name(context.clone()).await {
        Ok(targets) => targets,
        Err(err) => {
            warn!(
                "Failed to load download targets for pending estimate repo={}: {}",
                repo_url, err
            );
            return None;
        }
    };
    let pre_filter_targets = targets.len();
    targets.retain(|target| file_ids.contains(&target.download.file_id));
    if targets.is_empty() {
        return None;
    }

    let (patchable_file_ids, planned_bytes, full_bytes) =
        apply_download_plan_bytes(context, &mut targets).await;
    let mods = build_download_estimate_diffs(&targets);
    info!(
        "Download-target pending estimate applied for repo={}: mods={} files={} dropped_targets={} patch_files={} planned_transfer_bytes={} full_bytes={}",
        repo_url,
        mods.len(),
        targets.len(),
        pre_filter_targets.saturating_sub(targets.len()),
        patchable_file_ids.len(),
        planned_bytes,
        full_bytes
    );

    Some(mods)
}

pub(crate) async fn pending_update_mod_scope(
    db: &FoxyDb,
    repo_url: &str,
) -> Option<HashSet<String>> {
    let row = match db
        .query_one(
            "SELECT diff_json FROM pending_updates WHERE repository_url = ? LIMIT 1",
            params![repo_url],
        )
        .await
    {
        Ok(row) => row,
        Err(err) => {
            warn!(
                "Failed to load pending-update scope for repo {}: {}",
                repo_url, err
            );
            return None;
        }
    };

    let payload = row?;
    let diff_json = payload.get_string("diff_json").unwrap_or_default();

    let mods: Vec<ModDiffSummary> = match serde_json::from_str(&diff_json) {
        Ok(mods) => mods,
        Err(err) => {
            warn!(
                "Failed to parse pending-update scope for repo {}: {}",
                repo_url, err
            );
            return None;
        }
    };

    let names: HashSet<String> = mods
        .into_iter()
        .filter(|m| m.needs_update)
        .map(|m| m.name.to_lowercase())
        .collect();
    if names.is_empty() { None } else { Some(names) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_summary(name: &str, total_bytes: u64) -> FileDiffSummary {
        FileDiffSummary {
            name: name.to_owned(),
            needs_update: true,
            total_bytes,
            changed_parts: 1,
        }
    }

    fn mod_summary(name: &str, total_bytes: u64, files: Vec<FileDiffSummary>) -> ModDiffSummary {
        ModDiffSummary {
            name: name.to_owned(),
            needs_update: true,
            total_bytes,
            files,
        }
    }

    async fn seed_same_url_repositories_for_patch_estimate(
        db: &FoxyDb,
        handle: crate::core::db::DbHandle,
    ) -> (Arc<FoxyContext>, Arc<FoxyContext>) {
        db.execute(
            "INSERT INTO repositories \
             (id, name, remote_url, local_path, image, local_checksum, remote_checksum, local_content_hash, foxy_mode) \
             VALUES \
             (1, 'Repo A', 'https://example.test/repo/', 'c:/repo-a', '', '', '', '', ''), \
             (2, 'Repo B', 'https://example.test/repo/', 'c:/repo-b', '', '', '', '', '')",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addons \
             (id, name, display_name, remote_path, local_path, client_side, enabled, local_checksum, remote_checksum, local_content_hash, required, data_order) \
             VALUES \
             (10, '@ace3', '', '@ace3', 'c:/repo-a/@ace3', 0, 1, '', '', '', 1, 1), \
             (20, '@ace3', '', '@ace3', 'c:/repo-b/@ace3', 0, 1, '', '', '', 1, 1)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files \
             (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) \
             VALUES \
             (100, 'addons/foo.pbo', 'addons/foo.pbo', 'c:/repo-a/@ace3/addons/foo.pbo', '', '', '', 1000, 1), \
             (200, 'addons/foo.pbo', 'addons/foo.pbo', 'c:/repo-b/@ace3/addons/foo.pbo', '', '', '', 2000, 1)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 10), (2, 20)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (10, 100), (20, 200)",
            params![],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO download_patch_file \
             (file_id, patch_json_path, patch_blob_path, planned_copy_bytes, planned_download_bytes, status, last_error) \
             VALUES (100, 'patch.json', 'patch.blob', 990, 10, 'ready', '')",
            params![],
        )
        .await
        .unwrap();

        let context_a = Arc::new(
            FoxyContext::new(handle.clone(), reqwest::Client::new())
                .with_target_local_path("c:/repo-a"),
        );
        let context_b = Arc::new(
            FoxyContext::new(handle, reqwest::Client::new()).with_target_local_path("c:/repo-b"),
        );
        (context_a, context_b)
    }

    #[tokio::test]
    async fn patch_plan_estimate_is_scoped_by_repository_local_path() {
        let handle = crate::core::tasks::db_turso::build_test_database().await;
        let db = FoxyDb::from_turso(handle.clone());
        let (context_a, context_b) =
            seed_same_url_repositories_for_patch_estimate(&db, handle).await;
        let mods = vec![mod_summary(
            "@ace3",
            1000,
            vec![file_summary("addons/foo.pbo", 1000)],
        )];

        let adjusted_a = apply_patch_plan_estimates_to_pending_updates(
            context_a,
            "https://example.test/repo/",
            &mods,
        )
        .await
        .unwrap();
        assert_eq!(adjusted_a[0].files[0].total_bytes, 10);
        assert_eq!(adjusted_a[0].total_bytes, 10);

        let adjusted_b = apply_patch_plan_estimates_to_pending_updates(
            context_b,
            "https://example.test/repo/",
            &mods,
        )
        .await
        .unwrap();
        assert_eq!(adjusted_b[0].files[0].total_bytes, 2000);
        assert_eq!(adjusted_b[0].total_bytes, 2000);
    }

    #[test]
    fn patch_plan_estimate_uses_full_size_without_plan() {
        assert_eq!(patch_plan_estimate_bytes(100, None), 100);
    }

    #[test]
    fn patch_plan_estimate_uses_capped_plan_size() {
        assert_eq!(patch_plan_estimate_bytes(100, Some(25)), 25);
        assert_eq!(patch_plan_estimate_bytes(100, Some(125)), 100);
    }

    #[test]
    fn pending_file_name_normalization_matches_paths_and_leafs() {
        assert_eq!(
            normalize_pending_file_name("\\Addons\\Foo.PBO"),
            "addons/foo.pbo"
        );
        assert_eq!(pending_file_name_leaf("addons/foo.pbo"), "foo.pbo");
    }

    // ── normalize_pending_file_name ─────────────────────────────────────

    #[test]
    fn normalize_pending_file_name_already_normalized_is_unchanged() {
        assert_eq!(
            normalize_pending_file_name("addons/foo.pbo"),
            "addons/foo.pbo"
        );
    }

    #[test]
    fn normalize_pending_file_name_strips_leading_slashes() {
        assert_eq!(normalize_pending_file_name("/foo.pbo"), "foo.pbo");
        assert_eq!(normalize_pending_file_name("///foo.pbo"), "foo.pbo");
    }

    #[test]
    fn normalize_pending_file_name_lowercases() {
        assert_eq!(normalize_pending_file_name("ACE/MAIN.PBO"), "ace/main.pbo");
    }

    #[test]
    fn normalize_pending_file_name_mixed_separators() {
        assert_eq!(
            normalize_pending_file_name("\\addons/Sub\\Deep.PBO"),
            "addons/sub/deep.pbo"
        );
    }

    #[test]
    fn normalize_pending_file_name_empty_is_empty() {
        assert_eq!(normalize_pending_file_name(""), "");
    }

    #[test]
    fn normalize_pending_file_name_only_separators() {
        // Leading separators are trimmed; the remaining backslashes are converted.
        assert_eq!(normalize_pending_file_name("\\\\"), "");
    }

    // ── pending_file_name_leaf ──────────────────────────────────────────

    #[test]
    fn pending_file_name_leaf_no_separator_returns_whole() {
        assert_eq!(pending_file_name_leaf("foo.pbo"), "foo.pbo");
    }

    #[test]
    fn pending_file_name_leaf_backslash_separator() {
        assert_eq!(pending_file_name_leaf("addons\\foo.pbo"), "foo.pbo");
    }

    #[test]
    fn pending_file_name_leaf_trailing_separator_skips_empty() {
        assert_eq!(pending_file_name_leaf("addons/foo/"), "foo");
    }

    #[test]
    fn pending_file_name_leaf_deeply_nested() {
        assert_eq!(pending_file_name_leaf("a/b/c/d/leaf.bin"), "leaf.bin");
    }

    #[test]
    fn pending_file_name_leaf_empty_returns_empty() {
        assert_eq!(pending_file_name_leaf(""), "");
    }

    // ── patch_plan_estimate_bytes ───────────────────────────────────────

    #[test]
    fn patch_plan_estimate_equal_plan_and_file() {
        assert_eq!(patch_plan_estimate_bytes(100, Some(100)), 100);
    }

    #[test]
    fn patch_plan_estimate_zero_plan_means_no_transfer() {
        assert_eq!(patch_plan_estimate_bytes(100, Some(0)), 0);
    }

    #[test]
    fn patch_plan_estimate_zero_length_file() {
        assert_eq!(patch_plan_estimate_bytes(0, None), 0);
        assert_eq!(patch_plan_estimate_bytes(0, Some(50)), 0);
    }

    #[test]
    fn patch_plan_estimate_never_exceeds_full_size() {
        // A plan larger than the file is always capped at the full file size.
        assert_eq!(patch_plan_estimate_bytes(64, Some(u64::MAX)), 64);
    }
}
