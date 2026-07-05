use super::persistence::{calculate_compound_content_hash, persist_repository_checksums};
use super::*;
use crate::core::db::{FoxyDb, params};
use crate::core::models::pending_update::clear_pending_update_for_context;

/// Lightweight pre-propagation: before running expensive hashing for a repository,
/// copy `local_checksum` values from sibling files/parts that share the same
/// `local_path` and `remote_checksum`. This lets the hashing phase skip files
/// that were already hashed by a sibling repository on the same disk.
///
/// Returns the number of file-level checksum updates applied.
pub(crate) async fn pre_propagate_sibling_checksums(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> u64 {
    let db = context.db();
    let start = Instant::now();

    // Early bail-out: check if this repo has any unsynced files with a synced
    // sibling. This cheap EXISTS avoids running expensive UPDATEs when there is
    // nothing to propagate (e.g. single-repository setups).
    let has_propagatable = matches!(
        db.query_one(
            r#"SELECT 1
               FROM repository_addons ra
               JOIN repositories r ON r.id = ra.repository_id
               JOIN addons a ON a.id = ra.addon_id
               JOIN addon_files af ON af.addon_id = a.id
               JOIN files tgt_f ON tgt_f.id = af.file_id
               WHERE r.remote_url = ?
               AND tgt_f.local_checksum != tgt_f.remote_checksum
               AND tgt_f.remote_checksum != ''
               AND EXISTS (
                   SELECT 1 FROM files src_f
                   WHERE src_f.local_path = tgt_f.local_path
                   AND src_f.remote_checksum = tgt_f.remote_checksum
                   AND src_f.local_checksum = src_f.remote_checksum
                   AND src_f.local_checksum != ''
                   AND src_f.id != tgt_f.id
               )
               LIMIT 1"#,
            params![repository_url],
        )
        .await,
        Ok(Some(_))
    );

    if !has_propagatable {
        debug!(
            "No sibling checksums to pre-propagate for repo={} (checked in {:.2?})",
            repository_url,
            start.elapsed()
        );
        return 0;
    }

    // NOTE: Part-level pre-propagation is intentionally skipped here.
    // When a file is selected for hashing, build_file_hash_jobs hashes ALL its
    // parts regardless of existing checksums - propagated part checksums would
    // just be overwritten. Only file-level and addon-level propagation matter
    // because they prevent entire files/addons from entering the hash pipeline.

    // Propagate file-level checksums from sibling files with the same local_path
    // and remote_checksum that are already synced.
    let file_updates = match db
        .execute_retry(
            "pre-propagate sibling file checksums",
            r#"UPDATE files SET local_checksum = remote_checksum
               WHERE local_checksum != remote_checksum
               AND remote_checksum != ''
               AND id IN (
                   SELECT tgt_f.id
                   FROM repository_addons ra
                   JOIN repositories r ON r.id = ra.repository_id
                   JOIN addons a ON a.id = ra.addon_id
                   JOIN addon_files af ON af.addon_id = a.id
                   JOIN files tgt_f ON tgt_f.id = af.file_id
                   WHERE r.remote_url = ?
                   AND tgt_f.local_checksum != tgt_f.remote_checksum
                   AND tgt_f.remote_checksum != ''
                   AND EXISTS (
                       SELECT 1 FROM files src_f
                       WHERE src_f.local_path = tgt_f.local_path
                       AND src_f.remote_checksum = tgt_f.remote_checksum
                       AND src_f.local_checksum = src_f.remote_checksum
                       AND src_f.local_checksum != ''
                       AND src_f.id != tgt_f.id
                   )
               )"#,
            params![repository_url],
        )
        .await
    {
        Ok(rows_affected) => rows_affected,
        Err(e) => {
            warn!(
                "Pre-propagation of sibling file checksums failed for repo={}: {}",
                repository_url, e
            );
            0
        }
    };

    // Propagate addon-level checksums from siblings with same name + local_path.
    let addon_updates = match db
        .execute_retry(
            "pre-propagate sibling addon checksums",
            r#"UPDATE addons SET local_checksum = remote_checksum
               WHERE local_checksum != remote_checksum
               AND remote_checksum != ''
               AND id IN (
                   SELECT a.id
                   FROM repository_addons ra
                   JOIN repositories r ON r.id = ra.repository_id
                   JOIN addons a ON a.id = ra.addon_id
                   WHERE r.remote_url = ?
                   AND a.local_checksum != a.remote_checksum
                   AND a.remote_checksum != ''
                   AND EXISTS (
                       SELECT 1 FROM addons src
                       WHERE src.name = a.name
                       AND src.local_path = a.local_path
                       AND src.remote_checksum = a.remote_checksum
                       AND src.local_checksum = src.remote_checksum
                       AND src.local_checksum != ''
                       AND src.id != a.id
                   )
               )"#,
            params![repository_url],
        )
        .await
    {
        Ok(rows_affected) => rows_affected,
        Err(e) => {
            warn!(
                "Pre-propagation of sibling addon checksums failed for repo={}: {}",
                repository_url, e
            );
            0
        }
    };

    info!(
        "Pre-propagated sibling checksums for repo={}: files={} addons={} in {:.2?}",
        repository_url,
        file_updates,
        addon_updates,
        start.elapsed()
    );

    file_updates
}

#[derive(Clone, Copy, Debug, Default)]
struct SiblingPropagationStats {
    addon_checksum_updates: u64,
    file_checksum_updates: u64,
    part_checksum_updates: u64,
    addon_content_hash_updates: u64,
    file_content_hash_updates: u64,
}

/// Propagate synced checksum state to sibling repositories that share the same
/// addon local paths and expect the same remote content. This prevents redundant
/// re-downloads when multiple repositories in the same space reference identical addons.
///
/// Only propagates where the sibling's `remote_checksum` matches the source's, guarding
/// against incorrectly marking a different-version addon as synced.
pub(crate) async fn propagate_checksums_to_siblings(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> Vec<String> {
    let db = context.db();
    let start = Instant::now();

    // Early bail-out: check if this repository has any sibling addons at all.
    // This is a cheap EXISTS query that avoids running expensive 12-table JOINs
    // when there are no siblings to propagate to.
    let has_siblings = matches!(
        db.query_one(
            r#"SELECT 1 FROM repository_addons ra_source
               JOIN repositories r_source ON r_source.id = ra_source.repository_id
               JOIN addons source ON source.id = ra_source.addon_id
               JOIN addons sibling ON source.name = sibling.name
                   AND source.local_path = sibling.local_path
                   AND source.id != sibling.id
               WHERE r_source.remote_url = ?
               LIMIT 1"#,
            params![repository_url],
        )
        .await,
        Ok(Some(_))
    );

    if !has_siblings {
        debug!(
            "No sibling addons found for repo={}, skipping propagation (checked in {:.2?})",
            repository_url,
            start.elapsed()
        );
        return Vec::new();
    }
    info!(
        "Sibling addons detected for repo={}, starting propagation (existence check took {:.2?})",
        repository_url,
        start.elapsed()
    );

    let repository_url_owned = repository_url.to_owned();
    let stats_state = Arc::new(std::sync::Mutex::new(SiblingPropagationStats::default()));

    // Transaction 1: Propagate checksums (addon, file, part).
    // Split from content hash propagation to reduce write-lock hold time.
    {
        let stats_state_cloned = Arc::clone(&stats_state);
        let repo_url = repository_url_owned.clone();
        if let Err(e) = db.transaction("propagate sibling checksums", move |txn| {
            let repo_url = repo_url.clone();
            let stats_state = Arc::clone(&stats_state_cloned);
            Box::pin(async move {
                let addon_start = Instant::now();
                let addon_checksum_updates = txn
                    .execute(
                        r#"UPDATE addons SET local_checksum = remote_checksum
                           WHERE local_checksum != remote_checksum
                           AND remote_checksum != ''
                           AND id IN (
                               SELECT sibling.id
                               FROM repository_addons ra_source
                               JOIN repositories r_source ON r_source.id = ra_source.repository_id
                               JOIN addons source ON source.id = ra_source.addon_id
                               JOIN addons sibling ON source.name = sibling.name
                                   AND source.local_path = sibling.local_path
                                   AND source.remote_checksum = sibling.remote_checksum
                                   AND source.id != sibling.id
                               WHERE r_source.remote_url = ?
                               AND source.local_checksum = source.remote_checksum
                               AND source.remote_checksum != ''
                           )"#,
                        params![repo_url.clone()],
                    )
                    .await?;
                info!(
                    "Sibling propagation: addon checksums updated={} in {:.2?}",
                    addon_checksum_updates,
                    addon_start.elapsed()
                );

                let file_start = Instant::now();
                let file_checksum_updates = txn
                    .execute(
                        r#"UPDATE files SET local_checksum = remote_checksum
                           WHERE local_checksum != remote_checksum
                           AND remote_checksum != ''
                           AND id IN (
                               SELECT sibling_f.id
                               FROM repository_addons ra_source
                               JOIN repositories r_source ON r_source.id = ra_source.repository_id
                               JOIN addons source ON source.id = ra_source.addon_id
                               JOIN addon_files af_source ON af_source.addon_id = source.id
                               JOIN files source_f ON source_f.id = af_source.file_id
                               JOIN addons sibling ON source.name = sibling.name
                                   AND source.local_path = sibling.local_path
                                   AND source.remote_checksum = sibling.remote_checksum
                                   AND source.id != sibling.id
                               JOIN addon_files af_sibling ON af_sibling.addon_id = sibling.id
                               JOIN files sibling_f ON sibling_f.id = af_sibling.file_id
                                   AND source_f.local_path = sibling_f.local_path
                                   AND source_f.remote_checksum = sibling_f.remote_checksum
                               WHERE r_source.remote_url = ?
                               AND source_f.local_checksum = source_f.remote_checksum
                               AND source_f.remote_checksum != ''
                           )"#,
                        params![repo_url.clone()],
                    )
                    .await?;
                info!(
                    "Sibling propagation: file checksums updated={} in {:.2?}",
                    file_checksum_updates,
                    file_start.elapsed()
                );

                let part_start = Instant::now();
                let part_checksum_updates = if addon_checksum_updates > 0 || file_checksum_updates > 0 {
                    let rows = txn
                        .execute(
                            r#"UPDATE subfiles SET local_checksum = remote_checksum
                               WHERE local_checksum != remote_checksum
                               AND remote_checksum != ''
                               AND id IN (
                                   SELECT sibling_sf.id
                                   FROM repository_addons ra_source
                                   JOIN repositories r_source ON r_source.id = ra_source.repository_id
                                   JOIN addons source ON source.id = ra_source.addon_id
                                   JOIN addon_files af_source ON af_source.addon_id = source.id
                                   JOIN files source_f ON source_f.id = af_source.file_id
                                   JOIN subfiles source_sf ON source_sf.file_id = source_f.id
                                   JOIN addons sibling ON source.name = sibling.name
                                       AND source.local_path = sibling.local_path
                                       AND source.remote_checksum = sibling.remote_checksum
                                       AND source.id != sibling.id
                                   JOIN addon_files af_sibling ON af_sibling.addon_id = sibling.id
                                   JOIN files sibling_f ON sibling_f.id = af_sibling.file_id
                                       AND source_f.local_path = sibling_f.local_path
                                   JOIN subfiles sibling_sf ON sibling_sf.file_id = sibling_f.id
                                       AND source_sf.path = sibling_sf.path
                                       AND source_sf.remote_checksum = sibling_sf.remote_checksum
                                   WHERE r_source.remote_url = ?
                                   AND source_sf.local_checksum = source_sf.remote_checksum
                                   AND source_sf.remote_checksum != ''
                               )"#,
                            params![repo_url],
                        )
                        .await?;
                    info!(
                        "Sibling propagation: part checksums updated={} in {:.2?}",
                        rows,
                        part_start.elapsed()
                    );
                    rows
                } else {
                    info!("Sibling propagation: part checksums skipped (no addon/file updates needed)");
                    0
                };

                match stats_state.lock() {
                    Ok(mut guard) => {
                        guard.addon_checksum_updates = addon_checksum_updates;
                        guard.file_checksum_updates = file_checksum_updates;
                        guard.part_checksum_updates = part_checksum_updates;
                    }
                    Err(poisoned) => {
                        let mut guard = poisoned.into_inner();
                        guard.addon_checksum_updates = addon_checksum_updates;
                        guard.file_checksum_updates = file_checksum_updates;
                        guard.part_checksum_updates = part_checksum_updates;
                    }
                }
                Ok(())
            })
        })
        .await
        {
            warn!(
                "Failed to propagate checksums to siblings for repo={}: {}",
                repository_url, e
            );
            return Vec::new();
        }
    }

    // Transaction 2: Propagate content hashes (addon, file).
    // Logically independent from checksums - a partial failure here is recoverable
    // because a subsequent run will pick up any missing content hashes.
    {
        let stats_state_cloned = Arc::clone(&stats_state);
        let repo_url = repository_url_owned.clone();
        if let Err(e) = db
            .transaction("propagate sibling content hashes", move |txn| {
                let repo_url = repo_url.clone();
                let stats_state = Arc::clone(&stats_state_cloned);
                Box::pin(async move {
                    let content_start = Instant::now();
                    let addon_content_hash_updates = txn
                        .execute(
                            r#"UPDATE addons
                           SET local_content_hash = (
                               SELECT source.local_content_hash
                               FROM repository_addons ra_source
                               JOIN repositories r_source ON r_source.id = ra_source.repository_id
                               JOIN addons source ON source.id = ra_source.addon_id
                               JOIN addons sibling ON source.name = sibling.name
                                   AND source.local_path = sibling.local_path
                                   AND source.remote_checksum = sibling.remote_checksum
                                   AND source.id != sibling.id
                               WHERE r_source.remote_url = ?
                               AND source.local_checksum = source.remote_checksum
                               AND source.remote_checksum != ''
                               AND source.local_content_hash != ''
                               AND sibling.local_content_hash != source.local_content_hash
                               AND sibling.id = addons.id
                               LIMIT 1
                           )
                           WHERE id IN (
                               SELECT sibling.id
                               FROM repository_addons ra_source
                               JOIN repositories r_source ON r_source.id = ra_source.repository_id
                               JOIN addons source ON source.id = ra_source.addon_id
                               JOIN addons sibling ON source.name = sibling.name
                                   AND source.local_path = sibling.local_path
                                   AND source.remote_checksum = sibling.remote_checksum
                                   AND source.id != sibling.id
                               WHERE r_source.remote_url = ?
                               AND source.local_checksum = source.remote_checksum
                               AND source.remote_checksum != ''
                               AND source.local_content_hash != ''
                               AND sibling.local_content_hash != source.local_content_hash
                           )"#,
                            params![repo_url.clone(), repo_url.clone()],
                        )
                        .await?;

                    let file_content_hash_updates = txn
                        .execute(
                            r#"UPDATE files
                           SET local_content_hash = (
                               SELECT source_f.local_content_hash
                               FROM repository_addons ra_source
                               JOIN repositories r_source ON r_source.id = ra_source.repository_id
                               JOIN addons source ON source.id = ra_source.addon_id
                               JOIN addon_files af_source ON af_source.addon_id = source.id
                               JOIN files source_f ON source_f.id = af_source.file_id
                               JOIN addons sibling ON source.name = sibling.name
                                   AND source.local_path = sibling.local_path
                                   AND source.remote_checksum = sibling.remote_checksum
                                   AND source.id != sibling.id
                               JOIN addon_files af_sibling ON af_sibling.addon_id = sibling.id
                               JOIN files sibling_f ON sibling_f.id = af_sibling.file_id
                                   AND source_f.local_path = sibling_f.local_path
                                   AND source_f.remote_checksum = sibling_f.remote_checksum
                               WHERE r_source.remote_url = ?
                               AND source_f.local_checksum = source_f.remote_checksum
                               AND source_f.remote_checksum != ''
                               AND source_f.local_content_hash != ''
                               AND sibling_f.local_content_hash != source_f.local_content_hash
                               AND sibling_f.id = files.id
                               LIMIT 1
                           )
                           WHERE id IN (
                               SELECT sibling_f.id
                               FROM repository_addons ra_source
                               JOIN repositories r_source ON r_source.id = ra_source.repository_id
                               JOIN addons source ON source.id = ra_source.addon_id
                               JOIN addon_files af_source ON af_source.addon_id = source.id
                               JOIN files source_f ON source_f.id = af_source.file_id
                               JOIN addons sibling ON source.name = sibling.name
                                   AND source.local_path = sibling.local_path
                                   AND source.remote_checksum = sibling.remote_checksum
                                   AND source.id != sibling.id
                               JOIN addon_files af_sibling ON af_sibling.addon_id = sibling.id
                               JOIN files sibling_f ON sibling_f.id = af_sibling.file_id
                                   AND source_f.local_path = sibling_f.local_path
                                   AND source_f.remote_checksum = sibling_f.remote_checksum
                               WHERE r_source.remote_url = ?
                               AND source_f.local_checksum = source_f.remote_checksum
                               AND source_f.remote_checksum != ''
                               AND source_f.local_content_hash != ''
                               AND sibling_f.local_content_hash != source_f.local_content_hash
                           )"#,
                            params![repo_url.clone(), repo_url],
                        )
                        .await?;
                    info!(
                        "Sibling propagation: content hashes addon={} file={} in {:.2?}",
                        addon_content_hash_updates,
                        file_content_hash_updates,
                        content_start.elapsed()
                    );

                    match stats_state.lock() {
                        Ok(mut guard) => {
                            guard.addon_content_hash_updates = addon_content_hash_updates;
                            guard.file_content_hash_updates = file_content_hash_updates;
                        }
                        Err(poisoned) => {
                            let mut guard = poisoned.into_inner();
                            guard.addon_content_hash_updates = addon_content_hash_updates;
                            guard.file_content_hash_updates = file_content_hash_updates;
                        }
                    }
                    Ok(())
                })
            })
            .await
        {
            warn!(
                "Failed to propagate content hashes to siblings for repo={}: {}",
                repository_url, e
            );
            // Don't return - checksum propagation already succeeded
        }
    }

    let stats = match stats_state.lock() {
        Ok(guard) => *guard,
        Err(poisoned) => *poisoned.into_inner(),
    };

    let any_checksum_updates = stats.addon_checksum_updates > 0
        || stats.file_checksum_updates > 0
        || stats.part_checksum_updates > 0;
    let any_content_hash_updates =
        stats.addon_content_hash_updates > 0 || stats.file_content_hash_updates > 0;

    if !any_checksum_updates && !any_content_hash_updates {
        info!(
            "No sibling propagation changes needed for repo={} (total elapsed={:.2?})",
            repository_url,
            start.elapsed()
        );
        return Vec::new();
    }

    info!(
        "Propagated state to sibling repositories: addon_checksums={} file_checksums={} part_checksums={} addon_content_hashes={} file_content_hashes={} elapsed={:.2?} source_repo={}",
        stats.addon_checksum_updates,
        stats.file_checksum_updates,
        stats.part_checksum_updates,
        stats.addon_content_hash_updates,
        stats.file_content_hash_updates,
        start.elapsed(),
        repository_url
    );

    // Finalize repository-level hashes for affected sibling repos so their
    // quick scan sees a fully consistent tree. Only needed when checksum-level
    // propagation occurred (content-hash-only changes don't affect repo checksums).
    if !any_checksum_updates && !any_content_hash_updates {
        return Vec::new();
    }

    let sibling_urls = match db
        .query_all(
            r#"SELECT DISTINCT r_sibling.remote_url
               FROM repository_addons ra_source
               JOIN repositories r_source ON r_source.id = ra_source.repository_id
               JOIN addons source ON source.id = ra_source.addon_id
               JOIN addons sibling ON source.name = sibling.name
                   AND source.local_path = sibling.local_path
                   AND source.remote_checksum = sibling.remote_checksum
                   AND source.id != sibling.id
               JOIN repository_addons ra_sibling ON ra_sibling.addon_id = sibling.id
               JOIN repositories r_sibling ON r_sibling.id = ra_sibling.repository_id
               WHERE r_source.remote_url = ?
               AND source.local_checksum = source.remote_checksum
               AND source.remote_checksum != ''
               AND r_sibling.remote_url != ?"#,
            params![repository_url, repository_url],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_string("remote_url").ok())
            .collect::<Vec<_>>(),
        Err(e) => {
            warn!(
                "Failed to query sibling repository URLs for hash finalization: {}",
                e
            );
            Vec::new()
        }
    };

    let finalize_start = Instant::now();
    for url in &sibling_urls {
        if any_checksum_updates && !finalize_repository_hashes_from_mods(context.clone(), url).await
        {
            warn!(
                "Failed to finalize repository hash for sibling repo={}",
                url
            );
        }
        if any_content_hash_updates
            && !finalize_repository_content_hashes_from_mods(context.clone(), url).await
        {
            warn!(
                "Failed to finalize repository content hash for sibling repo={}",
                url
            );
        }
    }
    if !sibling_urls.is_empty() {
        info!(
            "Finalized repository hashes for {} sibling repos in {:.2?} (total propagation={:.2?})",
            sibling_urls.len(),
            finalize_start.elapsed(),
            start.elapsed()
        );
    }

    for url in &sibling_urls {
        if let Err(err) = clear_pending_update_for_context(context.clone(), url).await {
            warn!(
                "Failed to clear sibling pending updates after propagation for repo={}: {}",
                url, err
            );
        }
    }

    sibling_urls
}

pub(crate) async fn finalize_repository_content_hashes_from_mods(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> bool {
    let db = context.db();
    let started = Instant::now();

    let repo_row = match db
        .query_one(
            r#"SELECT id, local_content_hash
               FROM repositories
               WHERE remote_url = ?"#,
            params![repository_url],
        )
        .await
    {
        Ok(row) => row,
        Err(err) => {
            warn!(
                "Failed to load repository row for content-hash finalization {}: {}",
                repository_url, err
            );
            return false;
        }
    };

    let Some(repo_row) = repo_row else {
        warn!(
            "Repository content-hash finalization skipped for {}: repository not found",
            repository_url
        );
        return false;
    };

    let repo_id = match repo_row.get_i64("id") {
        Ok(id) => id,
        Err(err) => {
            warn!(
                "Failed to read repository id for content-hash finalization {}: {}",
                repository_url, err
            );
            return false;
        }
    };
    let old_content_hash = repo_row
        .get_string("local_content_hash")
        .unwrap_or_default();

    let addon_rows = match db
        .query_all(
            r#"SELECT a.data_order, a.local_content_hash
               FROM repository_addons ra
               JOIN addons a ON a.id = ra.addon_id
               WHERE ra.repository_id = ?
               ORDER BY a.data_order ASC, a.id ASC"#,
            params![repo_id],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            warn!(
                "Failed to load addon content hashes for repository content-hash finalization {}: {}",
                repository_url, err
            );
            return false;
        }
    };

    let mut ordered_hashes = Vec::with_capacity(addon_rows.len());
    let mut all_addons_hashed = true;
    for row in addon_rows.iter() {
        let data_order = row.get_i64("data_order").unwrap_or_default();
        let local_content_hash = row.get_string("local_content_hash").unwrap_or_default();
        if local_content_hash.is_empty() {
            all_addons_hashed = false;
            break;
        }
        ordered_hashes.push((data_order, local_content_hash));
    }

    let content_hash = if all_addons_hashed {
        calculate_compound_content_hash(&ordered_hashes)
    } else {
        String::new()
    };

    if content_hash != old_content_hash {
        let new_hash = content_hash.clone();
        let persist_result = db
            .transaction("persist repo content hashes", move |txn| {
                let new_hash = new_hash.clone();
                Box::pin(async move {
                    txn.execute(
                        r#"UPDATE repositories
                           SET local_content_hash = ?
                           WHERE id = ?"#,
                        params![new_hash, repo_id],
                    )
                    .await?;
                    Ok(())
                })
            })
            .await;
        if let Err(err) = persist_result {
            warn!(
                "Failed to persist repository content hash for {}: {}",
                repository_url, err
            );
            return false;
        }
    }

    info!(
        "Finalized repository content hash from addon rows: repo={} addons={} updated={} elapsed={:.2?}",
        repository_url,
        addon_rows.len(),
        content_hash != old_content_hash,
        started.elapsed()
    );
    true
}

pub(super) fn update_repository_hashes_from_mods(data_tree: &mut Tree) {
    for repo_idx in 0..data_tree.repo_nodes.len() {
        update_repository_hash_for_repo(data_tree, repo_idx);
    }
}

pub(super) fn update_repository_hashes_for_repos(
    data_tree: &mut Tree,
    repo_indices: &HashSet<usize>,
) {
    for &repo_idx in repo_indices {
        update_repository_hash_for_repo(data_tree, repo_idx);
    }
}

fn update_repository_hash_for_repo(data_tree: &mut Tree, repo_idx: usize) {
    let mut repo_mods: Vec<FoxyMod> = {
        let Some(repo_node) = data_tree.repo_nodes.get(repo_idx) else {
            return;
        };
        repo_node
            .mods
            .iter()
            .filter_map(|&mod_idx| data_tree.mods.get(mod_idx).cloned())
            .collect()
    };

    let all_mods_match = repo_mods
        .iter()
        .all(|m| m.local_checksum == m.remote_checksum);

    if let Some(repo) = data_tree.repositories.get_mut(repo_idx) {
        if repo_mods.is_empty() || all_mods_match {
            repo.local_checksum = repo.remote_checksum.clone();
        } else {
            repo.local_checksum = calculate_hash_from_items(&mut repo_mods).to_uppercase();
        }
    }
}

pub(super) fn collect_repo_indices_for_mods(
    data_tree: &Tree,
    mod_indices: &HashSet<usize>,
) -> HashSet<usize> {
    let mut repo_indices = HashSet::new();
    if mod_indices.is_empty() {
        return repo_indices;
    }

    for (repo_idx, repo_node) in data_tree.repo_nodes.iter().enumerate() {
        if repo_node
            .mods
            .iter()
            .any(|mod_idx| mod_indices.contains(mod_idx))
        {
            repo_indices.insert(repo_idx);
        }
    }

    repo_indices
}

pub(crate) async fn finalize_repository_hashes_from_mods(
    context: Arc<FoxyContext>,
    repository_url: &str,
) -> bool {
    let db = context.db();
    let mut data_tree: Tree = match Tree::load(context.clone(), repository_url).await {
        Ok(tree) => tree,
        Err(err) => {
            warn!(
                "Failed to load tree for repository-hash finalization {}: {}",
                repository_url, err
            );
            return false;
        }
    };

    if data_tree.repositories.is_empty() {
        warn!(
            "Repository-hash finalization skipped for {}: no repositories in tree",
            repository_url
        );
        return false;
    }

    finalize_repository_hashes_from_tree(&db, &mut data_tree, repository_url).await
}

pub(crate) async fn finalize_repository_hashes_from_tree(
    db: &FoxyDb,
    data_tree: &mut Tree,
    repository_url: &str,
) -> bool {
    if data_tree.repositories.is_empty() {
        warn!(
            "Repository-hash finalization skipped for {}: no repositories in tree",
            repository_url
        );
        return false;
    }

    update_repository_hashes_from_mods(data_tree);
    let repo_updates: Vec<_> = data_tree.repositories.clone();
    persist_repository_checksums(db, &repo_updates, |_| {}).await;
    true
}

pub(super) fn update_mod_hashes_for_mods(
    data_tree: &mut Tree,
    mod_indices: Option<&HashSet<usize>>,
) -> Vec<usize> {
    let mut updated = Vec::new();
    let count = data_tree.mod_nodes.len();
    for mod_idx in 0..count {
        if let Some(indices) = mod_indices
            && !indices.contains(&mod_idx)
        {
            continue;
        }
        let Some(mod_node) = data_tree.mod_nodes.get(mod_idx) else {
            continue;
        };
        let mut mod_files: Vec<FoxyModFile> = mod_node
            .files
            .iter()
            .filter_map(|&file_idx| data_tree.files.get(file_idx).cloned())
            .collect();
        if mod_files.is_empty() {
            continue;
        }
        mod_files.sort_by_key(|f| f.data_order);

        let all_match_remote = mod_files
            .iter()
            .all(|f| !f.local_checksum.is_empty() && f.local_checksum == f.remote_checksum);
        let new_hash = calculate_hash_from_items(&mut mod_files);

        if let Some(m) = data_tree.mods.get_mut(mod_idx) {
            let old = &m.local_checksum;
            let new_val = if all_match_remote {
                m.remote_checksum.clone()
            } else {
                new_hash.to_uppercase()
            };
            if *old != new_val {
                updated.push(mod_idx);
            }
            m.local_checksum = new_val;
        }
    }
    updated
}
