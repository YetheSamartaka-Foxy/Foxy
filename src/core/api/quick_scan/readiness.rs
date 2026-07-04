use super::super::*;
#[cfg(test)]
use super::db_helpers::{content_hash_baseline_ready_joined, remote_checksum_state_ready_joined};
#[cfg(test)]
use crate::core::db::DbValue;
use crate::core::db::{FoxyDb, params};
use crate::core::models::repository::{REPOSITORY_COLUMNS, repository_from_row};

pub(super) async fn repo_has_cached_pending_update(
    db: &FoxyDb,
    repo_url: &str,
    local_path: &str,
) -> bool {
    match db
        .query_one(
            "SELECT 1 FROM pending_updates WHERE repository_url = ? AND local_path = ? LIMIT 1",
            params![repo_url, local_path],
        )
        .await
    {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(err) => {
            warn!("Failed to check pending updates for {}: {}", repo_url, err);
            false
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QuickScanBootstrapPlan {
    None,
    RefreshContentBaseline,
    InitializeTreeAndRefreshContent,
    LoadTreeAndRepairMissingChecksums,
}

pub(super) struct QuickScanPreflightResult {
    pub remote_ready: bool,
    pub bootstrap_plan: QuickScanBootstrapPlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupQuickScanEligibility {
    Ineligible,
    NeedsBootstrap,
    Prevalidated,
}

pub(super) async fn quick_scan_preflight_combined(
    context: Arc<FoxyContext>,
    repo_url: &str,
) -> Option<QuickScanPreflightResult> {
    quick_scan_preflight_combined_inner(context, repo_url, false).await
}

pub(super) async fn quick_scan_preflight_for_local_check(
    context: Arc<FoxyContext>,
    repo_url: &str,
) -> Option<QuickScanPreflightResult> {
    quick_scan_preflight_combined_inner(context, repo_url, true).await
}

async fn quick_scan_preflight_combined_inner(
    context: Arc<FoxyContext>,
    repo_url: &str,
    allow_addon_fast_path: bool,
) -> Option<QuickScanPreflightResult> {
    let preflight_started = Instant::now();
    let db = context.db();

    // When several repository rows share this URL (the same repo installed in
    // different folders), pick the instance the caller targeted instead of an
    // arbitrary `.one()` - otherwise an empty new-folder install would inherit a
    // sibling's "complete" status.
    let repository_query_started = Instant::now();
    let repository = {
        let rows: Vec<_> = match db
            .query_all(
                &format!(
                    "SELECT {REPOSITORY_COLUMNS} FROM repositories \
                     WHERE remote_url = ? ORDER BY id ASC"
                ),
                params![repo_url],
            )
            .await
        {
            Ok(rows) => rows
                .iter()
                .filter_map(|row| repository_from_row(row).ok())
                .collect(),
            Err(err) => {
                warn!(
                    "Failed to load repository for preflight {}: {}",
                    repo_url, err
                );
                return None;
            }
        };

        let chosen = match context.target_local_path.as_deref() {
            Some(target) => {
                let target_key =
                    crate::core::models::repository::normalize_repository_local_path_identity(
                        target,
                    );
                rows.into_iter().find(|repo| {
                    crate::core::models::repository::normalize_repository_local_path_identity(
                        &repo.local_path,
                    ) == target_key
                })
            }
            None => rows.into_iter().next(),
        };

        match chosen {
            Some(repo) => repo,
            None => {
                info!(
                    "Quick scan preflight timings: repo={} outcome=no_repository repository_query={:.2?} total={:.2?}",
                    repo_url,
                    repository_query_started.elapsed(),
                    preflight_started.elapsed()
                );
                return Some(QuickScanPreflightResult {
                    remote_ready: false,
                    bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
                });
            }
        }
    };
    let repository_query_elapsed = repository_query_started.elapsed();

    if repository.remote_checksum.trim().is_empty() {
        info!(
            "Quick scan preflight timings: repo={} outcome=missing_repository_remote_checksum repository_query={:.2?} total={:.2?}",
            repo_url,
            repository_query_elapsed,
            preflight_started.elapsed()
        );
        return Some(QuickScanPreflightResult {
            remote_ready: false,
            bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
        });
    }

    // Query 1: mod stats via subquery (avoids JOIN row inflation)
    let mod_stats_started = Instant::now();
    let mod_row = match db
        .query_one(
            r#"SELECT
                COUNT(*) AS total,
                SUM(CASE WHEN remote_checksum = '' THEN 1 ELSE 0 END) AS missing_remote,
                SUM(CASE WHEN local_checksum = '' THEN 1 ELSE 0 END) AS missing_local,
                SUM(CASE WHEN local_content_hash = '' THEN 1 ELSE 0 END) AS missing_content
            FROM addons
            WHERE id IN (SELECT addon_id FROM repository_addons WHERE repository_id = ?)"#,
            params![repository.id as i64],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Some(QuickScanPreflightResult {
                remote_ready: false,
                bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
            });
        }
        Err(err) => {
            warn!(
                "Failed to query mod stats for preflight {}: {}",
                repo_url, err
            );
            return None;
        }
    };
    let mod_stats_elapsed = mod_stats_started.elapsed();

    let mod_count: i64 = mod_row.get_i64("total").unwrap_or(0);
    let missing_mod_remote: i64 = mod_row.get_i64("missing_remote").unwrap_or(0);
    let missing_mod_local: i64 = mod_row.get_i64("missing_local").unwrap_or(0);
    let missing_mod_content: i64 = mod_row.get_i64("missing_content").unwrap_or(0);

    if mod_count == 0 {
        info!(
            "Quick scan preflight timings: repo={} outcome=no_addons repository_query={:.2?} mod_stats={:.2?} total={:.2?}",
            repo_url,
            repository_query_elapsed,
            mod_stats_elapsed,
            preflight_started.elapsed()
        );
        return Some(QuickScanPreflightResult {
            remote_ready: false,
            bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
        });
    }

    let repo_missing_tree = repository.local_checksum.trim().is_empty();
    let repo_missing_content = repository.local_content_hash.trim().is_empty();

    // Clean fastest path: a populated repository tree/content checksum plus
    // populated addon tree/content checksums is enough to run the local folder
    // fingerprint check. File and part checksums are inputs to those rollups, so
    // scanning every file row on every clean QuickCheckOnly run is redundant and
    // is the dominant Turso regression (~135 ms for the 40K repo).
    let addon_levels_complete = missing_mod_remote == 0
        && missing_mod_local == 0
        && missing_mod_content == 0
        && !repo_missing_tree
        && !repo_missing_content;
    if allow_addon_fast_path && addon_levels_complete {
        info!(
            "Quick scan preflight timings: repo={} outcome=addon_fast_path bootstrap_plan=None repository_query={:.2?} mod_stats={:.2?} file_stats=skipped part_stats=skipped total={:.2?} addons={}",
            repo_url,
            repository_query_elapsed,
            mod_stats_elapsed,
            preflight_started.elapsed(),
            mod_count
        );
        return Some(QuickScanPreflightResult {
            remote_ready: true,
            bootstrap_plan: QuickScanBootstrapPlan::None,
        });
    }

    let file_stats_started = Instant::now();
    let file_row = match db
        .query_one(
            r#"SELECT
                COUNT(*) AS total,
                SUM(CASE WHEN remote_checksum = '' THEN 1 ELSE 0 END) AS missing_remote,
                SUM(CASE WHEN local_checksum = '' THEN 1 ELSE 0 END) AS missing_local,
                SUM(CASE WHEN local_content_hash = '' THEN 1 ELSE 0 END) AS missing_content
            FROM files f
            WHERE EXISTS (
                SELECT 1
                FROM addon_files af
                JOIN repository_addons ra
                    ON ra.addon_id = af.addon_id
                   AND ra.repository_id = ?
                WHERE af.file_id = f.id
            )"#,
            params![repository.id as i64],
        )
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Some(QuickScanPreflightResult {
                remote_ready: false,
                bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
            });
        }
        Err(err) => {
            warn!(
                "Failed to query file stats for preflight {}: {}",
                repo_url, err
            );
            return None;
        }
    };
    let file_stats_elapsed = file_stats_started.elapsed();

    let file_count: i64 = file_row.get_i64("total").unwrap_or(0);
    let missing_file_remote: i64 = file_row.get_i64("missing_remote").unwrap_or(0);
    let missing_file_local: i64 = file_row.get_i64("missing_local").unwrap_or(0);
    let missing_file_content: i64 = file_row.get_i64("missing_content").unwrap_or(0);

    if file_count == 0 {
        info!(
            "Quick scan preflight timings: repo={} outcome=no_files repository_query={:.2?} mod_stats={:.2?} file_stats={:.2?} total={:.2?}",
            repo_url,
            repository_query_elapsed,
            mod_stats_elapsed,
            file_stats_elapsed,
            preflight_started.elapsed()
        );
        return Some(QuickScanPreflightResult {
            remote_ready: false,
            bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
        });
    }

    // Clean fast-path: when every repository-, addon-, and file-level checksum is
    // already populated, the part level is guaranteed complete too. File/addon/
    // repo checksums are rolled UP from part checksums - `calculate_hashes`
    // persists parts (Phase 2) before files (Phase 3) before addons (Phase 4)
    // before repos (Phase 5) - so a populated higher-level checksum implies its
    // parts were persisted first. In that state we can skip the part-stats
    // aggregate, which otherwise scans every subfile row (tens of thousands) on
    // each clean quick scan and dominates the Turso quick-check cost. The full
    // part audit below still runs whenever any higher level is incomplete
    // (bootstrap/repair), where part-level detail picks the bootstrap plan.
    let higher_levels_complete = missing_mod_remote == 0
        && missing_file_remote == 0
        && missing_mod_local == 0
        && missing_file_local == 0
        && missing_mod_content == 0
        && missing_file_content == 0
        && !repo_missing_tree
        && !repo_missing_content;
    if higher_levels_complete {
        info!(
            "Quick scan preflight timings: repo={} outcome=fast_path bootstrap_plan=None repository_query={:.2?} mod_stats={:.2?} file_stats={:.2?} part_stats=skipped total={:.2?} addons={} files={}",
            repo_url,
            repository_query_elapsed,
            mod_stats_elapsed,
            file_stats_elapsed,
            preflight_started.elapsed(),
            mod_count,
            file_count
        );
        return Some(QuickScanPreflightResult {
            remote_ready: true,
            bootstrap_plan: QuickScanBootstrapPlan::None,
        });
    }

    let part_stats_started = Instant::now();
    let (part_count, missing_part_remote, missing_part_local) = match db
        .query_one(
            r#"SELECT
                COUNT(*) AS total,
                SUM(CASE WHEN sf.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_remote,
                SUM(CASE
                    WHEN f.local_checksum IS NOT NULL
                         AND f.local_checksum != ''
                         AND f.local_checksum = f.remote_checksum
                         AND f.remote_checksum != ''
                    THEN 0
                    WHEN sf.local_checksum IS NULL OR sf.local_checksum = ''
                    THEN 1
                    ELSE 0
                END) AS missing_local
            FROM subfiles sf
            JOIN files f ON f.id = sf.file_id
            WHERE EXISTS (
                SELECT 1
                FROM addon_files af
                JOIN repository_addons ra
                    ON ra.addon_id = af.addon_id
                   AND ra.repository_id = ?
                WHERE af.file_id = sf.file_id
            )"#,
            params![repository.id as i64],
        )
        .await
    {
        Ok(Some(row)) => {
            let total: i64 = row.get_i64("total").unwrap_or(0);
            let mr: i64 = row.get_i64("missing_remote").unwrap_or(0);
            let ml: i64 = row.get_i64("missing_local").unwrap_or(0);
            (total, mr, ml)
        }
        Ok(None) => (0i64, 0i64, 0i64),
        Err(err) => {
            warn!(
                "Failed to query part stats for preflight {}: {}",
                repo_url, err
            );
            return None;
        }
    };
    let part_stats_elapsed = part_stats_started.elapsed();

    let parts_metadata_available = part_count > 0 || context.deferred_part_count() > 0;
    if !parts_metadata_available {
        info!(
            "Quick scan preflight for repo {}: part metadata is missing (files={} parts=0, no deferred rows); remote metadata refresh required before local hashing",
            repo_url, file_count
        );
    }
    let remote_ready = missing_mod_remote == 0
        && missing_file_remote == 0
        && missing_part_remote == 0
        && parts_metadata_available;

    // Bootstrap plan determination (same logic as determine_quick_scan_bootstrap_plan).
    // `repo_missing_tree` / `repo_missing_content` were computed above for the
    // clean fast-path check.
    let tree_missing = repo_missing_tree
        || missing_mod_local > 0
        || missing_file_local > 0
        || missing_part_local > 0;
    let content_ready =
        !repo_missing_content && missing_mod_content == 0 && missing_file_content == 0;
    let content_missing_all = repo_missing_content
        && missing_mod_content == mod_count
        && missing_file_content == file_count;

    let bootstrap_plan = if content_missing_all {
        QuickScanBootstrapPlan::InitializeTreeAndRefreshContent
    } else if tree_missing {
        QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums
    } else if !content_ready {
        QuickScanBootstrapPlan::RefreshContentBaseline
    } else {
        QuickScanBootstrapPlan::None
    };

    info!(
        "Quick scan preflight timings: repo={} outcome=full bootstrap_plan={:?} repository_query={:.2?} mod_stats={:.2?} file_stats={:.2?} part_stats={:.2?} total={:.2?} addons={} files={} parts={} missing_remote={}/{}/{} missing_local={}/{}/{} missing_content={}/{}",
        repo_url,
        bootstrap_plan,
        repository_query_elapsed,
        mod_stats_elapsed,
        file_stats_elapsed,
        part_stats_elapsed,
        preflight_started.elapsed(),
        mod_count,
        file_count,
        part_count,
        missing_mod_remote,
        missing_file_remote,
        missing_part_remote,
        missing_mod_local,
        missing_file_local,
        missing_part_local,
        missing_mod_content,
        missing_file_content
    );

    Some(QuickScanPreflightResult {
        remote_ready,
        bootstrap_plan,
    })
}

pub(crate) fn tree_local_checksums_missing(tree: &Tree) -> bool {
    if tree.repositories.is_empty() || tree.mods.is_empty() || tree.files.is_empty() {
        return true;
    }
    let parent_missing = tree
        .repositories
        .iter()
        .any(|repo| repo.local_checksum.trim().is_empty())
        || tree
            .mods
            .iter()
            .any(|addon| addon.local_checksum.trim().is_empty())
        || tree
            .files
            .iter()
            .any(|file| file.local_checksum.trim().is_empty());
    parent_missing || tree_has_missing_effective_part_checksums(tree)
}

pub(crate) fn tree_local_checksums_baseline_missing(tree: &Tree) -> bool {
    if tree.repositories.is_empty() || tree.mods.is_empty() || tree.files.is_empty() {
        return true;
    }
    let parent_baseline_missing = tree
        .repositories
        .iter()
        .all(|repo| repo.local_checksum.trim().is_empty())
        && tree
            .mods
            .iter()
            .all(|addon| addon.local_checksum.trim().is_empty())
        && tree
            .files
            .iter()
            .all(|file| file.local_checksum.trim().is_empty());
    parent_baseline_missing && tree_has_no_effective_part_checksums(tree)
}

fn tree_has_missing_effective_part_checksums(tree: &Tree) -> bool {
    if tree.file_nodes.is_empty() {
        return tree
            .parts
            .iter()
            .any(|part| part.local_checksum.trim().is_empty());
    }

    for (file_idx, file) in tree.files.iter().enumerate() {
        let Some(file_node) = tree.file_nodes.get(file_idx) else {
            return true;
        };
        if file_node.parts.is_empty() {
            continue;
        }
        if file_node.parts.iter().any(|part_idx| {
            tree.parts
                .get(*part_idx)
                .map(|part| {
                    !part.has_effective_local_checksum_for_file(
                        &file.local_checksum,
                        &file.remote_checksum,
                    )
                })
                .unwrap_or(true)
        }) {
            return true;
        }
    }
    false
}

fn tree_has_no_effective_part_checksums(tree: &Tree) -> bool {
    if tree.file_nodes.is_empty() {
        return tree
            .parts
            .iter()
            .all(|part| part.local_checksum.trim().is_empty());
    }

    for (file_idx, file) in tree.files.iter().enumerate() {
        let Some(file_node) = tree.file_nodes.get(file_idx) else {
            continue;
        };
        for part_idx in &file_node.parts {
            if tree.parts.get(*part_idx).is_some_and(|part| {
                part.has_effective_local_checksum_for_file(
                    &file.local_checksum,
                    &file.remote_checksum,
                )
            }) {
                return false;
            }
        }
    }
    true
}

pub(crate) fn collect_files_with_missing_local_tree_hashes(tree: &Tree) -> HashSet<u64> {
    let mut file_ids = HashSet::new();

    for (file_idx, file) in tree.files.iter().enumerate() {
        let file_missing = file.local_checksum.trim().is_empty();
        let part_missing = tree
            .file_nodes
            .get(file_idx)
            .map(|file_node| {
                !file_node.parts.is_empty()
                    && file_node.parts.iter().any(|part_idx| {
                        tree.parts
                            .get(*part_idx)
                            .map(|part| {
                                !part.has_effective_local_checksum_for_file(
                                    &file.local_checksum,
                                    &file.remote_checksum,
                                )
                            })
                            .unwrap_or(true)
                    })
            })
            .unwrap_or(false);

        if file_missing || part_missing {
            file_ids.insert(file.id);
        }
    }

    for (mod_idx, addon) in tree.mods.iter().enumerate() {
        if !addon.local_checksum.trim().is_empty() {
            continue;
        }
        if let Some(mod_node) = tree.mod_nodes.get(mod_idx) {
            for file_idx in &mod_node.files {
                if let Some(file) = tree.files.get(*file_idx) {
                    file_ids.insert(file.id);
                }
            }
        }
    }

    for (repo_idx, repo) in tree.repositories.iter().enumerate() {
        if !repo.local_checksum.trim().is_empty() {
            continue;
        }
        if let Some(repo_node) = tree.repo_nodes.get(repo_idx) {
            for mod_idx in &repo_node.mods {
                if let Some(mod_node) = tree.mod_nodes.get(*mod_idx) {
                    for file_idx in &mod_node.files {
                        if let Some(file) = tree.files.get(*file_idx) {
                            file_ids.insert(file.id);
                        }
                    }
                }
            }
        }
    }

    file_ids
}

pub(crate) struct TreeHashReadiness {
    pub(crate) ready_file_ids: HashSet<u64>,
    pub(crate) incomplete_files: Vec<String>,
}

pub(crate) fn partition_tree_hash_ready_files(
    tree: &Tree,
    file_ids: &HashSet<u64>,
) -> TreeHashReadiness {
    let mut ready_file_ids = HashSet::new();
    let mut incomplete_files = Vec::new();

    for file_id in file_ids {
        let Some(&file_idx) = tree.file_id_to_index.get(file_id) else {
            continue;
        };
        let Some(file) = tree.files.get(file_idx) else {
            continue;
        };

        if local_tree_hash_file_is_incomplete(tree, file_idx, file) {
            incomplete_files.push(file.name.clone());
        } else {
            ready_file_ids.insert(*file_id);
        }
    }

    incomplete_files.sort_unstable();
    incomplete_files.dedup();

    TreeHashReadiness {
        ready_file_ids,
        incomplete_files,
    }
}

fn local_tree_hash_file_is_incomplete(tree: &Tree, file_idx: usize, file: &FoxyModFile) -> bool {
    let local_path = file.local_path.trim();
    if local_path.is_empty() {
        return false;
    }

    let Ok(metadata) = std::fs::metadata(local_path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    let expected_size = expected_tree_hash_file_size(tree, file_idx, file);
    expected_size > 0 && metadata.len() < expected_size
}

fn expected_tree_hash_file_size(tree: &Tree, file_idx: usize, file: &FoxyModFile) -> u64 {
    let part_extent = tree
        .file_nodes
        .get(file_idx)
        .map(|file_node| {
            file_node
                .parts
                .iter()
                .filter_map(|part_idx| tree.parts.get(*part_idx))
                .map(|part| part.remote_start.saturating_add(part.remote_length))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);

    file.length.max(part_extent)
}

pub(super) fn content_hash_baseline_missing(tree: &Tree) -> bool {
    if tree.repositories.is_empty() || tree.mods.is_empty() || tree.files.is_empty() {
        return true;
    }
    let repositories_missing = tree
        .repositories
        .iter()
        .all(|repo| repo.local_content_hash.trim().is_empty());
    let addons_missing = tree
        .mods
        .iter()
        .all(|addon| addon.local_content_hash.trim().is_empty());
    let files_missing = tree
        .files
        .iter()
        .all(|file| file.local_content_hash.trim().is_empty());
    repositories_missing && addons_missing && files_missing
}

pub(super) fn content_hash_baseline_ready(tree: &Tree) -> bool {
    if tree.repositories.is_empty() || tree.mods.is_empty() || tree.files.is_empty() {
        return false;
    }

    tree.repositories
        .iter()
        .all(|repo| !repo.local_content_hash.trim().is_empty())
        && tree
            .mods
            .iter()
            .all(|addon| !addon.local_content_hash.trim().is_empty())
        && tree
            .files
            .iter()
            .all(|file| !file.local_content_hash.trim().is_empty())
}

#[cfg(test)]
pub(crate) struct EligibilityBatchResult {
    pub(crate) candidates: Vec<(i64, String)>,
    pub(crate) fast_rejected: usize,
}

#[cfg(test)]
pub(crate) async fn batch_eligible_repos(
    db: &FoxyDb,
    normalized_urls: &[String],
) -> EligibilityBatchResult {
    if normalized_urls.is_empty() {
        return EligibilityBatchResult {
            candidates: Vec::new(),
            fast_rejected: 0,
        };
    }

    let placeholders = vec!["?"; normalized_urls.len()].join(", ");
    let sql = format!(
        "SELECT id, remote_url, remote_checksum FROM repositories \
         WHERE remote_url IN ({placeholders})"
    );
    let values: Vec<DbValue> = normalized_urls.iter().map(DbValue::from).collect();
    let rows: Vec<(i64, String, String)> = match db.query_all(&sql, values).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.get_i64("id").ok()?,
                    row.get_string("remote_url").ok()?,
                    row.get_string("remote_checksum").ok()?,
                ))
            })
            .collect(),
        Err(err) => {
            warn!("Failed to batch-load repositories for eligibility: {}", err);
            return EligibilityBatchResult {
                candidates: Vec::new(),
                fast_rejected: 0,
            };
        }
    };

    let mut candidates = Vec::new();
    let mut fast_rejected = 0;
    for (id, url, remote_checksum) in rows {
        if remote_checksum.trim().is_empty() {
            fast_rejected += 1;
        } else {
            candidates.push((id, url));
        }
    }

    EligibilityBatchResult {
        candidates,
        fast_rejected,
    }
}

pub(crate) async fn launch_quick_scan_repo_startup_eligibility(
    context: Arc<FoxyContext>,
    repo_url: &str,
) -> StartupQuickScanEligibility {
    let Some(preflight) = quick_scan_preflight_combined(context, repo_url).await else {
        return StartupQuickScanEligibility::Ineligible;
    };

    if !preflight.remote_ready {
        debug!(
            "Startup quick scan skipped for {}: remote checksum metadata not ready",
            repo_url
        );
        return StartupQuickScanEligibility::Ineligible;
    }

    match preflight.bootstrap_plan {
        QuickScanBootstrapPlan::None => StartupQuickScanEligibility::Prevalidated,
        QuickScanBootstrapPlan::RefreshContentBaseline => {
            debug!(
                "Startup quick scan queued for {}: local content-hash baseline needs refresh",
                repo_url
            );
            StartupQuickScanEligibility::NeedsBootstrap
        }
        QuickScanBootstrapPlan::InitializeTreeAndRefreshContent => {
            debug!(
                "Startup quick scan skipped for {}: tree/content baseline is not initialized",
                repo_url
            );
            StartupQuickScanEligibility::Ineligible
        }
        QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums => {
            debug!(
                "Startup quick scan skipped for {}: local tree checksums need repair",
                repo_url
            );
            StartupQuickScanEligibility::Ineligible
        }
    }
}

/// Deep eligibility check using consolidated JOIN queries.
/// Takes repo_id directly (already known from [batch_eligible_repos]), skipping per-repo URL lookup.
#[cfg(test)]
pub(crate) async fn launch_quick_scan_repo_eligible_joined(
    db: &FoxyDb,
    repo_id: i64,
    repo_url: &str,
) -> bool {
    let purpose = format!("eligibility check {}", repo_url);

    match remote_checksum_state_ready_joined(db, repo_id, &purpose).await {
        Some(true) => {}
        Some(false) => {
            debug!(
                "Startup quick scan skipped for {}: remote checksum metadata not ready (joined)",
                repo_url
            );
            return false;
        }
        None => return false,
    }

    match content_hash_baseline_ready_joined(db, repo_id, &purpose).await {
        Some(true) => true,
        Some(false) => {
            debug!(
                "Startup quick scan skipped for {}: local content-hash baseline not initialized (joined)",
                repo_url
            );
            false
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::model_tree::{FileNode, ModNode, RepositoryNode};
    use crate::core::models::modification_file_part::FoxyModFilePart;
    use crate::core::models::repository::{FoxyMode, FoxyRepository};

    fn tree_with_one_file(local_path: String, file_length: u64, part_extent: u64) -> Tree {
        let mut file_id_to_index = HashMap::new();
        file_id_to_index.insert(1, 0);

        Tree {
            files: vec![FoxyModFile {
                id: 1,
                name: "test.pbo".to_string(),
                local_path,
                length: file_length,
                ..Default::default()
            }],
            parts: vec![FoxyModFilePart {
                id: 1,
                file_id: 1,
                remote_start: 0,
                remote_length: part_extent,
                ..Default::default()
            }],
            file_nodes: vec![FileNode {
                file_idx: 0,
                parts: vec![0],
            }],
            file_id_to_index,
            ..Default::default()
        }
    }

    #[test]
    fn tree_hash_readiness_defers_existing_short_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.pbo");
        std::fs::write(&path, b"short").expect("write partial file");
        let tree = tree_with_one_file(path.to_string_lossy().to_string(), 10, 10);
        let file_ids = HashSet::from([1]);

        let readiness = partition_tree_hash_ready_files(&tree, &file_ids);

        assert!(readiness.ready_file_ids.is_empty());
        assert_eq!(readiness.incomplete_files, vec!["test.pbo"]);
    }

    #[test]
    fn tree_hash_readiness_allows_missing_file_to_report_update() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("missing.pbo");
        let tree = tree_with_one_file(path.to_string_lossy().to_string(), 10, 10);
        let file_ids = HashSet::from([1]);

        let readiness = partition_tree_hash_ready_files(&tree, &file_ids);

        assert_eq!(readiness.ready_file_ids, file_ids);
        assert!(readiness.incomplete_files.is_empty());
    }

    #[test]
    fn tree_hash_readiness_allows_file_with_expected_size() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.pbo");
        std::fs::write(&path, vec![0u8; 12]).expect("write complete file");
        let tree = tree_with_one_file(path.to_string_lossy().to_string(), 10, 12);
        let file_ids = HashSet::from([1]);

        let readiness = partition_tree_hash_ready_files(&tree, &file_ids);

        assert_eq!(readiness.ready_file_ids, file_ids);
        assert!(readiness.incomplete_files.is_empty());
    }

    // ── checksum/content helpers ────────────────────────────────────────

    fn repo(local_checksum: &str, local_content_hash: &str) -> FoxyRepository {
        FoxyRepository {
            id: 1,
            name: "Repo".to_string(),
            remote_url: "https://example.invalid/repo/".to_string(),
            local_path: String::new(),
            image: String::new(),
            local_checksum: local_checksum.to_string(),
            local_content_hash: local_content_hash.to_string(),
            remote_checksum: String::new(),
            foxy_mode: FoxyMode::default(),
        }
    }

    /// Single repo/addon/file/part tree with explicit checksum + content-hash
    /// values for each level. Node graphs are intentionally omitted since the
    /// checksum/content helpers only inspect the flat vectors.
    #[allow(clippy::too_many_arguments)]
    fn checksum_tree(
        repo_tree: &str,
        repo_content: &str,
        mod_tree: &str,
        mod_content: &str,
        file_tree: &str,
        file_content: &str,
        part_tree: &str,
    ) -> Tree {
        Tree {
            repositories: vec![repo(repo_tree, repo_content)],
            mods: vec![FoxyMod {
                id: 1,
                name: "@a".to_string(),
                local_checksum: mod_tree.to_string(),
                local_content_hash: mod_content.to_string(),
                ..Default::default()
            }],
            files: vec![FoxyModFile {
                id: 10,
                name: "a.pbo".to_string(),
                local_checksum: file_tree.to_string(),
                local_content_hash: file_content.to_string(),
                ..Default::default()
            }],
            parts: vec![FoxyModFilePart {
                id: 100,
                file_id: 10,
                local_checksum: part_tree.to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn tree_local_checksums_missing_on_empty_tree() {
        assert!(tree_local_checksums_missing(&Tree::default()));
    }

    #[test]
    fn tree_local_checksums_missing_false_when_all_present() {
        let tree = checksum_tree("R", "RC", "M", "MC", "F", "FC", "P");
        assert!(!tree_local_checksums_missing(&tree));
    }

    #[test]
    fn tree_local_checksums_missing_detects_each_level() {
        assert!(tree_local_checksums_missing(&checksum_tree(
            "", "RC", "M", "MC", "F", "FC", "P"
        )));
        assert!(tree_local_checksums_missing(&checksum_tree(
            "R", "RC", "", "MC", "F", "FC", "P"
        )));
        assert!(tree_local_checksums_missing(&checksum_tree(
            "R", "RC", "M", "MC", "", "FC", "P"
        )));
        assert!(tree_local_checksums_missing(&checksum_tree(
            "R", "RC", "M", "MC", "F", "FC", ""
        )));
    }

    #[test]
    fn tree_local_checksums_missing_treats_whitespace_as_empty() {
        let tree = checksum_tree("   ", "RC", "M", "MC", "F", "FC", "P");
        assert!(tree_local_checksums_missing(&tree));
    }

    #[test]
    fn tree_local_checksums_baseline_missing_when_all_empty() {
        let tree = checksum_tree("", "", "", "", "", "", "");
        assert!(tree_local_checksums_baseline_missing(&tree));
    }

    #[test]
    fn tree_local_checksums_baseline_missing_false_when_any_present() {
        // A single populated file checksum means the baseline is not fully empty.
        let tree = checksum_tree("", "", "", "", "F", "", "");
        assert!(!tree_local_checksums_baseline_missing(&tree));
    }

    #[test]
    fn tree_local_checksums_baseline_missing_on_empty_tree() {
        assert!(tree_local_checksums_baseline_missing(&Tree::default()));
    }

    #[test]
    fn content_hash_baseline_missing_on_empty_tree() {
        assert!(content_hash_baseline_missing(&Tree::default()));
    }

    #[test]
    fn content_hash_baseline_missing_when_all_content_hashes_empty() {
        // Tree checksums present but content hashes entirely absent.
        let tree = checksum_tree("R", "", "M", "", "F", "", "P");
        assert!(content_hash_baseline_missing(&tree));
    }

    #[test]
    fn content_hash_baseline_missing_false_when_any_content_present() {
        let tree = checksum_tree("R", "", "M", "", "F", "FC", "P");
        assert!(!content_hash_baseline_missing(&tree));
    }

    #[test]
    fn content_hash_baseline_ready_when_all_content_present() {
        let tree = checksum_tree("R", "RC", "M", "MC", "F", "FC", "P");
        assert!(content_hash_baseline_ready(&tree));
    }

    #[test]
    fn content_hash_baseline_ready_false_when_any_missing() {
        let tree = checksum_tree("R", "RC", "M", "", "F", "FC", "P");
        assert!(!content_hash_baseline_ready(&tree));
    }

    #[test]
    fn content_hash_baseline_ready_false_on_empty_tree() {
        assert!(!content_hash_baseline_ready(&Tree::default()));
    }

    // ── collect_files_with_missing_local_tree_hashes ────────────────────

    /// `(file_id, file_checksum, [part_checksums])`.
    type FileSpec<'a> = (u64, &'a str, Vec<&'a str>);
    /// `(mod_checksum, [FileSpec])`.
    type ModSpec<'a> = (&'a str, Vec<FileSpec<'a>>);

    /// Builds a repo→addon→file→part tree from per-addon specs with a fully
    /// wired node graph.
    fn node_tree(repo_checksum: &str, mods: Vec<ModSpec<'_>>) -> Tree {
        let mut tree = Tree {
            repositories: vec![repo(repo_checksum, "")],
            ..Default::default()
        };
        let mut repo_mod_indices = Vec::new();
        for (mod_idx, (mod_checksum, files)) in mods.into_iter().enumerate() {
            repo_mod_indices.push(mod_idx);
            tree.mods.push(FoxyMod {
                id: mod_idx as u64 + 1,
                name: format!("@m{mod_idx}"),
                local_checksum: mod_checksum.to_string(),
                ..Default::default()
            });
            let mut file_indices = Vec::new();
            for (file_id, file_checksum, part_checksums) in files {
                let file_idx = tree.files.len();
                file_indices.push(file_idx);
                tree.files.push(FoxyModFile {
                    id: file_id,
                    name: format!("f{file_id}"),
                    local_checksum: file_checksum.to_string(),
                    ..Default::default()
                });
                let mut part_indices = Vec::new();
                for part_checksum in part_checksums {
                    let part_idx = tree.parts.len();
                    part_indices.push(part_idx);
                    tree.parts.push(FoxyModFilePart {
                        id: part_idx as u64 + 1,
                        file_id,
                        local_checksum: part_checksum.to_string(),
                        ..Default::default()
                    });
                }
                tree.file_nodes.push(FileNode {
                    file_idx,
                    parts: part_indices,
                });
            }
            tree.mod_nodes.push(ModNode {
                mod_idx,
                files: file_indices,
            });
        }
        tree.repo_nodes.push(RepositoryNode {
            repo_idx: 0,
            mods: repo_mod_indices,
        });
        tree
    }

    #[test]
    fn collect_missing_tree_hashes_empty_when_complete() {
        let tree = node_tree("R", vec![("M", vec![(10, "F", vec!["P"])])]);
        assert!(collect_files_with_missing_local_tree_hashes(&tree).is_empty());
    }

    #[test]
    fn collect_missing_tree_hashes_includes_file_with_empty_checksum() {
        let tree = node_tree("R", vec![("M", vec![(10, "", vec!["P"])])]);
        assert_eq!(
            collect_files_with_missing_local_tree_hashes(&tree),
            HashSet::from([10])
        );
    }

    #[test]
    fn collect_missing_tree_hashes_includes_file_with_empty_part_checksum() {
        let tree = node_tree("R", vec![("M", vec![(10, "F", vec![""])])]);
        assert_eq!(
            collect_files_with_missing_local_tree_hashes(&tree),
            HashSet::from([10])
        );
    }

    #[test]
    fn collect_missing_tree_hashes_accepts_derived_clean_part_checksum() {
        let mut tree = node_tree("R", vec![("M", vec![(10, "F", vec![""])])]);
        tree.files[0].remote_checksum = "F".to_string();
        tree.parts[0].remote_checksum = "P".to_string();

        assert!(collect_files_with_missing_local_tree_hashes(&tree).is_empty());
        assert!(!tree_local_checksums_missing(&tree));
        assert!(!tree_local_checksums_baseline_missing(&tree));
    }

    #[test]
    fn collect_missing_tree_hashes_rejects_dirty_file_with_missing_part_checksum() {
        let mut tree = node_tree("R", vec![("M", vec![(10, "OLD", vec![""])])]);
        tree.files[0].remote_checksum = "NEW".to_string();
        tree.parts[0].remote_checksum = "P".to_string();

        assert_eq!(
            collect_files_with_missing_local_tree_hashes(&tree),
            HashSet::from([10])
        );
        assert!(tree_local_checksums_missing(&tree));
    }

    #[test]
    fn collect_missing_tree_hashes_skips_partless_file_with_checksum() {
        let tree = node_tree("R", vec![("M", vec![(10, "F", vec![])])]);
        assert!(collect_files_with_missing_local_tree_hashes(&tree).is_empty());
        assert!(!tree_local_checksums_missing(&tree));
    }

    #[test]
    fn collect_missing_tree_hashes_includes_partless_file_without_checksum() {
        let tree = node_tree("R", vec![("M", vec![(10, "", vec![])])]);
        assert_eq!(
            collect_files_with_missing_local_tree_hashes(&tree),
            HashSet::from([10])
        );
        assert!(tree_local_checksums_missing(&tree));
    }

    #[test]
    fn collect_missing_tree_hashes_rolls_up_missing_addon_checksum() {
        // The addon checksum is empty, so every file under it is flagged even
        // though the files themselves look complete.
        let tree = node_tree(
            "R",
            vec![("", vec![(10, "F", vec!["P"]), (11, "F2", vec!["P2"])])],
        );
        assert_eq!(
            collect_files_with_missing_local_tree_hashes(&tree),
            HashSet::from([10, 11])
        );
    }

    #[test]
    fn collect_missing_tree_hashes_rolls_up_missing_repo_checksum() {
        // Empty repository checksum flags all files across all addons.
        let tree = node_tree(
            "",
            vec![
                ("M", vec![(10, "F", vec!["P"])]),
                ("M2", vec![(20, "F2", vec!["P2"])]),
            ],
        );
        assert_eq!(
            collect_files_with_missing_local_tree_hashes(&tree),
            HashSet::from([10, 20])
        );
    }
}
