use super::super::logging::request_background_repaint;
use super::super::sync_pipeline::summary::{PipelineSummary, StageEntry};
use super::super::*;
use super::diff::quick_local_change_diff;
use super::local_path_preflight::{
    format_local_path_mismatch_message, log_local_path_availability,
    summarize_local_path_availability, suspect_local_path_mismatch,
};
use super::pending_updates::{
    apply_patch_plan_estimates_to_pending_updates, persist_pending_updates,
    refresh_patch_plan_metadata_for_pending_updates,
};
use super::persistent_cache::{load_persistent_addon_hash_cache, save_persistent_addon_hash_cache};
use super::readiness::{
    StartupQuickScanEligibility, launch_quick_scan_repo_startup_eligibility,
    repo_has_cached_pending_update,
};
use super::shared_cache::QuickScanSharedCache;
use crate::core::db::{DbValue, params};
use crate::core::models::repository::{REPOSITORY_COLUMNS, repository_from_row};
use crate::core::utils::fetch_json::fetch_json;

const STARTUP_REMOTE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StartupRepositoryInstance {
    pub repo_url: String,
    pub local_path: String,
}

fn normalize_startup_repositories(
    repositories: Vec<StartupRepositoryInstance>,
) -> Vec<StartupRepositoryInstance> {
    let mut seen = HashSet::new();
    repositories
        .into_iter()
        .filter_map(|repository| {
            if repository.repo_url.trim().is_empty() || repository.local_path.trim().is_empty() {
                return None;
            }
            let normalized = StartupRepositoryInstance {
                repo_url: if repository.repo_url.ends_with('/') {
                    repository.repo_url
                } else {
                    format!("{}/", repository.repo_url)
                },
                local_path: normalize_instance_path(&repository.local_path),
            };
            seen.insert(normalized.clone()).then_some(normalized)
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct StartupQuickScanPlan {
    pub eligible_repositories: Vec<StartupRepositoryInstance>,
    pub prevalidated_repositories: Vec<StartupRepositoryInstance>,
    pub remote_changed_repositories: Vec<StartupRepositoryInstance>,
}

pub async fn recalculate_hashes_for_addon_by_name(
    repository_url: &str,
    addon_name: &str,
) -> Result<bool> {
    ensure_logger();
    // DATABASE_URL is set once at startup in main.rs to avoid unsafe env::set_var
    // race conditions in multi-threaded context.

    let addon_name = addon_name.trim();
    if addon_name.is_empty() {
        warn!("Addon hash recalculation ignored: addon name is empty");
        return Ok(false);
    }

    let normalized_repo_url = if repository_url.ends_with('/') {
        repository_url.to_string()
    } else {
        format!("{}/", repository_url)
    };

    let context = create_context_with_recheck_level(RecheckLevel::DEFAULT).await;
    let tree = Tree::load(context.clone(), &normalized_repo_url).await?;
    if tree.repositories.is_empty() {
        warn!(
            "Addon hash recalculation skipped: repository {} not found in local database",
            normalized_repo_url
        );
        return Ok(false);
    }

    let target_mod_indices: Vec<usize> = tree
        .mods
        .iter()
        .enumerate()
        .filter_map(|(idx, m)| {
            if m.name.eq_ignore_ascii_case(addon_name) {
                Some(idx)
            } else {
                None
            }
        })
        .collect();

    if target_mod_indices.is_empty() {
        warn!(
            "Addon hash recalculation skipped: addon {} not found for repository {}",
            addon_name, normalized_repo_url
        );
        return Ok(false);
    }

    let mut target_file_ids: HashSet<u64> = HashSet::new();
    for mod_idx in target_mod_indices {
        if let Some(mod_node) = tree.mod_nodes.get(mod_idx) {
            for file_idx in &mod_node.files {
                if let Some(file) = tree.files.get(*file_idx) {
                    target_file_ids.insert(file.id);
                }
            }
        }
    }

    if target_file_ids.is_empty() {
        warn!(
            "Addon hash recalculation skipped: addon {} has no files in repository {}",
            addon_name, normalized_repo_url
        );
        return Ok(false);
    }

    info!(
        "Recalculating hashes for addon {} in repository {} (files={})",
        addon_name,
        normalized_repo_url,
        target_file_ids.len()
    );
    calculate_hashes_for_files(context, &normalized_repo_url, &target_file_ids, None, false).await;
    Ok(true)
}

/// Returns the subset of `repo_urls` that have a row in the `repositories` table,
/// meaning they have been checked/synced at least once before. Skips repos that
/// have never been initialized so auto-recheck does not trigger a fresh first
/// check on launch.
pub fn filter_repo_urls_with_db_entry(repo_urls: Vec<String>) -> HashSet<String> {
    if repo_urls.is_empty() {
        return HashSet::new();
    }

    let mut seen = HashSet::new();
    let mut normalized_unique: Vec<String> = Vec::new();
    for repo_url in repo_urls {
        if repo_url.trim().is_empty() {
            continue;
        }
        let normalized = if repo_url.ends_with('/') {
            repo_url
        } else {
            format!("{}/", repo_url)
        };
        if seen.insert(normalized.clone()) {
            normalized_unique.push(normalized);
        }
    }
    if normalized_unique.is_empty() {
        return HashSet::new();
    }

    let rt = match Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            warn!(
                "Failed to build runtime for startup recheck DB-entry filter: {}",
                err
            );
            return HashSet::new();
        }
    };

    rt.block_on(async move {
        ensure_logger();
        let context = create_context().await;
        if normalized_unique.is_empty() {
            return HashSet::new();
        }
        let placeholders = vec!["?"; normalized_unique.len()].join(", ");
        let sql =
            format!("SELECT remote_url FROM repositories WHERE remote_url IN ({placeholders})");
        let values: Vec<DbValue> = normalized_unique.into_iter().map(DbValue::from).collect();
        let rows: Vec<String> = match context.db().query_all(&sql, values).await {
            Ok(rows) => rows
                .iter()
                .filter_map(|row| row.get_string("remote_url").ok())
                .collect(),
            Err(err) => {
                warn!(
                    "Failed to query repositories for startup recheck DB-entry filter: {}",
                    err
                );
                return HashSet::new();
            }
        };
        rows.into_iter().collect()
    })
}

pub fn plan_startup_quick_scan_repos(
    repositories: Vec<StartupRepositoryInstance>,
) -> StartupQuickScanPlan {
    if repositories.is_empty() {
        return StartupQuickScanPlan::default();
    }

    // Repository identity is `(remote_url, local_path)`. Only collapse exact
    // duplicates; the same URL installed in another folder is independent.
    let normalized_unique = normalize_startup_repositories(repositories);
    if normalized_unique.is_empty() {
        return StartupQuickScanPlan::default();
    }

    let rt = match Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            warn!(
                "Failed to build runtime for startup quick scan filtering: {}",
                err
            );
            return StartupQuickScanPlan::default();
        }
    };

    rt.block_on(async move {
        ensure_logger();
        // DATABASE_URL is set once at startup in main.rs to avoid unsafe env::set_var
        // race conditions in multi-threaded context.

        let context = create_context().await;
        let remote_changed_set =
            startup_remote_changed_repositories(context.clone(), &normalized_unique).await;
        if !remote_changed_set.is_empty() {
            info!(
                "Startup remote checksum probe found {} changed repositories",
                remote_changed_set.len()
            );
        }
        let quick_scan_candidates = normalized_unique
            .iter()
            .filter(|repository| !remote_changed_set.contains(*repository))
            .cloned()
            .collect::<Vec<_>>();
        if quick_scan_candidates.is_empty() {
            return StartupQuickScanPlan {
                eligible_repositories: Vec::new(),
                prevalidated_repositories: Vec::new(),
                remote_changed_repositories: normalized_unique
                    .into_iter()
                    .filter(|repository| remote_changed_set.contains(repository))
                    .collect(),
            };
        }

        // Tier 1: batch gate - single query loads all repos, rejects those with
        // empty remote_checksum or local_content_hash in Rust.
        let mut join_set: JoinSet<(StartupRepositoryInstance, StartupQuickScanEligibility)> =
            JoinSet::new();
        for repository in quick_scan_candidates.iter().cloned() {
            let context = Arc::new(
                (*context)
                    .clone()
                    .with_target_local_path(repository.local_path.clone()),
            );
            join_set.spawn(async move {
                let eligibility =
                    launch_quick_scan_repo_startup_eligibility(context, &repository.repo_url).await;
                (repository, eligibility)
            });
        }

        let mut eligible_set = HashSet::new();
        let mut prevalidated_set = HashSet::new();
        while let Some(joined) = join_set.join_next().await {
            if let Ok((repository, eligibility)) = joined {
                match eligibility {
                    StartupQuickScanEligibility::Ineligible => {}
                    StartupQuickScanEligibility::NeedsBootstrap => {
                        eligible_set.insert(repository);
                    }
                    StartupQuickScanEligibility::Prevalidated => {
                        prevalidated_set.insert(repository.clone());
                        eligible_set.insert(repository);
                    }
                }
            }
        }

        // Preserve original insertion order
        let eligible_repositories: Vec<StartupRepositoryInstance> = quick_scan_candidates
            .into_iter()
            .filter(|repository| eligible_set.contains(repository))
            .collect();
        let prevalidated_repositories = eligible_repositories
            .iter()
            .filter(|repository| prevalidated_set.contains(*repository))
            .cloned()
            .collect();
        let remote_changed_repositories = normalized_unique
            .into_iter()
            .filter(|repository| remote_changed_set.contains(repository))
            .collect();
        StartupQuickScanPlan {
            eligible_repositories,
            prevalidated_repositories,
            remote_changed_repositories,
        }
    })
}

async fn startup_remote_changed_repositories(
    context: Arc<FoxyContext>,
    repositories: &[StartupRepositoryInstance],
) -> HashSet<StartupRepositoryInstance> {
    if repositories.is_empty() {
        return HashSet::new();
    }

    let normalized_urls = repositories
        .iter()
        .map(|repository| repository.repo_url.clone())
        .collect::<Vec<_>>();
    if normalized_urls.is_empty() {
        return HashSet::new();
    }
    let placeholders = vec!["?"; normalized_urls.len()].join(", ");
    let sql = format!(
        "SELECT remote_url, local_path, local_checksum \
         FROM repositories WHERE remote_url IN ({placeholders})"
    );
    let values: Vec<DbValue> = normalized_urls.into_iter().map(DbValue::from).collect();
    let rows: Vec<(String, String, String)> = match context.db().query_all(&sql, values).await {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| {
                Some((
                    row.get_string("remote_url").ok()?,
                    row.get_string("local_path").ok()?,
                    row.get_string("local_checksum").ok()?,
                ))
            })
            .collect(),
        Err(err) => {
            warn!(
                "Failed to load repositories for startup remote checksum probe: {}",
                err
            );
            return HashSet::new();
        }
    };

    let mut join_set: JoinSet<(StartupRepositoryInstance, bool)> = JoinSet::new();
    for repository in repositories {
        let Some((_, _, local_checksum)) = rows.iter().find(|(repo_url, local_path, _)| {
            repo_url == &repository.repo_url
                && normalize_instance_path(local_path) == repository.local_path
        }) else {
            continue;
        };
        let repository = repository.clone();
        let repo_url = repository.repo_url.clone();
        let local_checksum = local_checksum.clone();
        let context = context.clone();
        join_set.spawn(async move {
            let repo_json_url = format!("{}repo.json", repo_url);
            let remote_result = tokio::time::timeout(
                STARTUP_REMOTE_CHECK_TIMEOUT,
                fetch_json(context, &repo_json_url),
            )
            .await;
            let remote_checksum = match remote_result {
                Ok(Ok(data)) => data
                    .get("checksum")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                Ok(Err(err)) => {
                    debug!(
                        "Startup remote checksum probe skipped for {}: {}",
                        repo_url, err
                    );
                    return (repository, false);
                }
                Err(_) => {
                    debug!(
                        "Startup remote checksum probe timed out for {} after {:?}",
                        repo_url, STARTUP_REMOTE_CHECK_TIMEOUT
                    );
                    return (repository, false);
                }
            };
            let local_checksum = local_checksum.trim();
            // Only compare checksums of the same algorithm/length. A FoxyMode/Hybrid
            // repo stores a BLAKE3 (64 hex) local_checksum while repo.json may carry a SHA-1
            // (40 hex) value; comparing across algorithms can never match and would flag a
            // false "remote changed" on every launch. When the lengths differ, the repo.json
            // checksum is not authoritative for this repo, so we do not treat it as changed.
            if !local_checksum.is_empty() && remote_checksum.len() != local_checksum.len() {
                debug!(
                    "Startup remote checksum probe skipped for {}: checksum algorithm mismatch (local len={}, remote len={})",
                    repo_url,
                    local_checksum.len(),
                    remote_checksum.len()
                );
                return (repository, false);
            }
            let changed =
                !remote_checksum.is_empty() && !remote_checksum.eq_ignore_ascii_case(local_checksum);
            if changed {
                info!(
                    "Startup remote checksum changed for repo={} local_checksum={} remote_checksum={}",
                    repo_url, local_checksum, remote_checksum
                );
            }
            (repository, changed)
        });
    }

    let mut changed = HashSet::new();
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok((repository, true)) => {
                changed.insert(repository);
            }
            Ok((_repo_url, false)) => {}
            Err(err) => {
                warn!("Startup remote checksum probe task failed: {}", err);
            }
        }
    }
    changed
}

struct QuickScanWorkerRepoOutcome {
    result: QuickScanResult,
    stage: StageEntry,
}

/// Canonicalize a download folder so quick-scan instance identity matches the
/// repository row, the pending-update key, and the UI's `repository.path`.
fn normalize_instance_path(local_path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(local_path)
}

/// Normalized download folders of every repository instance stored for `repo_url`.
/// Two installs of the same URL (independent, or repository-space entries) each
/// have their own row, so they are scanned independently.
async fn load_repository_instance_paths(context: &FoxyContext, repo_url: &str) -> Vec<String> {
    match context
        .db()
        .query_all(
            "SELECT local_path FROM repositories WHERE remote_url = ?",
            params![repo_url],
        )
        .await
    {
        Ok(rows) => {
            let mut seen = HashSet::new();
            rows.iter()
                .filter_map(|row| row.get_string("local_path").ok())
                .map(|path| normalize_instance_path(&path))
                .filter(|path| seen.insert(path.clone()))
                .collect()
        }
        Err(err) => {
            warn!(
                "Failed to load repository instances for quick scan {}: {}",
                repo_url, err
            );
            Vec::new()
        }
    }
}

async fn run_quick_scan_worker_repo(
    base_context: Arc<FoxyContext>,
    shared_cache: Arc<Mutex<QuickScanSharedCache>>,
    normalized_repo_url: String,
    target_local_path: String,
    already_eligible: bool,
    force_fresh_addon_hash: bool,
) -> QuickScanWorkerRepoOutcome {
    let repo_started_at = Instant::now();
    // Scope every DB lookup, tree load, eligibility check and pending-update
    // write to this specific folder instance so two installs of the same URL do
    // not bleed status into each other.
    let context = Arc::new(
        (*base_context)
            .clone()
            .with_target_local_path(target_local_path.clone()),
    );
    let instance_rows: Vec<_> = match context
        .db()
        .query_all(
            &format!("SELECT {REPOSITORY_COLUMNS} FROM repositories WHERE remote_url = ?"),
            params![&normalized_repo_url],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| repository_from_row(row).ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    let instance_key = normalize_instance_path(&target_local_path);
    let repo_name = instance_rows
        .iter()
        .find(|repo| normalize_instance_path(&repo.local_path) == instance_key)
        .or_else(|| instance_rows.first())
        .map(|repo| repo.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| normalized_repo_url.clone());

    info!(
        "Quick scan worker started: repo={} folder={:?}",
        repo_name, target_local_path
    );

    let has_cached =
        repo_has_cached_pending_update(&context.db(), &normalized_repo_url, &target_local_path)
            .await;
    if force_fresh_addon_hash {
        debug!(
            "Quick scan forcing fresh addon-hash probing for repo={}",
            normalized_repo_url
        );
    }
    let eligibility = if already_eligible {
        StartupQuickScanEligibility::Prevalidated
    } else {
        launch_quick_scan_repo_startup_eligibility(context.clone(), &normalized_repo_url).await
    };
    let can_produce_result = !matches!(eligibility, StartupQuickScanEligibility::Ineligible);

    let addon_states_before = match shared_cache.lock() {
        Ok(guard) => guard.addon_state_by_path.len(),
        Err(poisoned) => poisoned.into_inner().addon_state_by_path.len(),
    };

    let mut mods = quick_local_change_diff(
        context.clone(),
        &normalized_repo_url,
        None,
        None,
        None,
        true,
        already_eligible,
        force_fresh_addon_hash,
        Some(&shared_cache),
    )
    .await;

    let addon_states_after = match shared_cache.lock() {
        Ok(guard) => guard.addon_state_by_path.len(),
        Err(poisoned) => poisoned.into_inner().addon_state_by_path.len(),
    };
    let new_addon_states = addon_states_after.saturating_sub(addon_states_before);
    let mut has_updates = mods.iter().any(|m| m.needs_update);

    let skipped = !can_produce_result && mods.is_empty();
    if has_updates
        && already_eligible
        && let Ok(tree) = Tree::load(context.clone(), &normalized_repo_url).await
    {
        let availability = summarize_local_path_availability(&tree);
        log_local_path_availability(&repo_name, &availability);
        if suspect_local_path_mismatch(&availability) {
            let message = format_local_path_mismatch_message(&repo_name, &availability);
            warn!("{message}");
            return QuickScanWorkerRepoOutcome {
                result: QuickScanResult {
                    repo_url: normalized_repo_url,
                    local_path: target_local_path,
                    mods: Vec::new(),
                    skipped: true,
                },
                stage: StageEntry::new(&repo_name, repo_started_at.elapsed())
                    .with("skipped", true)
                    .with("reason", "local_path_mismatch")
                    .with("had_cached", has_cached),
            };
        }
    }
    if has_updates {
        let pending_mod_names: HashSet<String> = mods
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.name.to_lowercase())
            .collect();
        let refreshed_files = refresh_patch_plan_metadata_for_pending_updates(
            context.clone(),
            &normalized_repo_url,
            Some(&pending_mod_names),
        )
        .await;
        if refreshed_files > 0
            && let Some(adjusted_mods) = apply_patch_plan_estimates_to_pending_updates(
                context.clone(),
                &normalized_repo_url,
                &mods,
            )
            .await
        {
            mods = adjusted_mods;
            has_updates = mods.iter().any(|m| m.needs_update);
        }
        info!(
            "Quick scan pending update: repo={} mods_with_updates={} cached={}",
            normalized_repo_url,
            mods.iter().filter(|m| m.needs_update).count(),
            has_cached
        );
        persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
    } else if skipped {
        info!(
            "Quick scan skipped: repo={} remote checksum metadata is not ready",
            normalized_repo_url
        );
    } else {
        if has_cached
            && let Err(err) =
                clear_pending_update_for_context(context.clone(), &normalized_repo_url).await
        {
            warn!(
                "Failed to clear cached updates after clean quick scan for {}: {}",
                normalized_repo_url, err
            );
        }
        info!(
            "Quick scan clean: repo={} cached={}",
            normalized_repo_url, has_cached
        );
    }

    let mods_total = mods.len();
    let mods_with_updates = mods.iter().filter(|m| m.needs_update).count();
    let result = QuickScanResult {
        repo_url: normalized_repo_url,
        local_path: target_local_path,
        mods,
        skipped,
    };
    let stage = StageEntry::new(&repo_name, repo_started_at.elapsed())
        .with("has_updates", has_updates)
        .with("mods_with_updates", mods_with_updates)
        .with("mods_total", mods_total)
        .with("had_cached", has_cached)
        .with("new_addon_states", new_addon_states);
    info!(
        "Quick scan worker finished: repo={} elapsed={:.2?}",
        repo_name,
        repo_started_at.elapsed()
    );

    QuickScanWorkerRepoOutcome { result, stage }
}

pub fn spawn_quick_local_scan(
    repo_urls: Vec<String>,
    prevalidated_repo_urls: HashSet<String>,
    force_fresh_addon_hash_repo_urls: HashSet<String>,
    result_tx: StdSender<QuickScanResult>,
    repaint_ctx: Option<egui::Context>,
) -> std::thread::JoinHandle<()> {
    let repositories = repo_urls
        .into_iter()
        .map(|repo_url| StartupRepositoryInstance {
            repo_url,
            local_path: String::new(),
        })
        .collect();
    let prevalidated_repositories = prevalidated_repo_urls
        .into_iter()
        .map(|repo_url| StartupRepositoryInstance {
            repo_url,
            local_path: String::new(),
        })
        .collect();
    let force_fresh_addon_hash_repositories = force_fresh_addon_hash_repo_urls
        .into_iter()
        .map(|repo_url| StartupRepositoryInstance {
            repo_url,
            local_path: String::new(),
        })
        .collect();
    spawn_quick_local_scan_instances(
        repositories,
        prevalidated_repositories,
        force_fresh_addon_hash_repositories,
        result_tx,
        repaint_ctx,
    )
}

pub fn spawn_quick_local_scan_instances(
    repositories: Vec<StartupRepositoryInstance>,
    prevalidated_repositories: HashSet<StartupRepositoryInstance>,
    force_fresh_addon_hash_repositories: HashSet<StartupRepositoryInstance>,
    result_tx: StdSender<QuickScanResult>,
    repaint_ctx: Option<egui::Context>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        info!(
            "Quick scan worker spawned for {} repositories (prevalidated={} force_fresh_addon_hash={})",
            repositories.len(),
            prevalidated_repositories.len(),
            force_fresh_addon_hash_repositories.len()
        );
        let rt = match Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(err) => {
                error!("Failed to build runtime for quick scan: {}", err);
                return;
            }
        };
        rt.block_on(async move {
            ensure_logger();
            // DATABASE_URL is set once at startup in main.rs to avoid unsafe env::set_var
            // race conditions in multi-threaded context.

            let context = create_context().await;
            let shared_cache: Arc<Mutex<QuickScanSharedCache>> =
                Arc::new(Mutex::new(QuickScanSharedCache::default()));
            let persistent_addon_cache = load_persistent_addon_hash_cache();
            if !persistent_addon_cache.is_empty() {
                info!(
                    "Loaded quick-scan addon hash cache entries={}",
                    persistent_addon_cache.len()
                );
                match shared_cache.lock() {
                    Ok(mut guard) => {
                        guard.persistent_addon_hash_by_path = persistent_addon_cache;
                    }
                    Err(poisoned) => {
                        let mut guard = poisoned.into_inner();
                        guard.persistent_addon_hash_by_path = persistent_addon_cache;
                    }
                }
            }
            let worker_started_at = Instant::now();
            let operation_id = next_operation_id("quick-scan");
            let repo_total = repositories.len();
            let mut summary = PipelineSummary::new(
                operation_id.clone(),
                "QuickScan",
                format!("{} repositories", repo_total),
                worker_started_at,
            );

            let mut join_set: JoinSet<QuickScanWorkerRepoOutcome> = JoinSet::new();
            let mut scheduled_repositories = HashSet::new();
            for repository in repositories {
                let normalized_repo_url = if repository.repo_url.ends_with('/') {
                    repository.repo_url
                } else {
                    format!("{}/", repository.repo_url)
                };
                let normalized_repository = StartupRepositoryInstance {
                    repo_url: normalized_repo_url.clone(),
                    local_path: normalize_instance_path(&repository.local_path),
                };
                if !scheduled_repositories.insert(normalized_repository.clone()) {
                    continue;
                }
                let url_wide_target = StartupRepositoryInstance {
                    repo_url: normalized_repo_url.clone(),
                    local_path: String::new(),
                };
                let already_eligible = prevalidated_repositories
                    .contains(&normalized_repository)
                    || prevalidated_repositories.contains(&url_wide_target);
                let force_fresh_addon_hash = force_fresh_addon_hash_repositories
                    .contains(&normalized_repository)
                    || force_fresh_addon_hash_repositories.contains(&url_wide_target);
                // Expand the URL into every folder instance and scan each one
                // independently. A URL with no DB row yet (never synced) still
                // gets a single empty-folder pass so it reports a result.
                let mut instance_paths = if normalized_repository.local_path.is_empty() {
                    load_repository_instance_paths(context.as_ref(), &normalized_repo_url).await
                } else {
                    vec![normalized_repository.local_path]
                };
                if instance_paths.is_empty() {
                    instance_paths.push(String::new());
                }
                for target_local_path in instance_paths {
                    join_set.spawn(run_quick_scan_worker_repo(
                        context.clone(),
                        shared_cache.clone(),
                        normalized_repo_url.clone(),
                        target_local_path,
                        already_eligible,
                        force_fresh_addon_hash,
                    ));
                }
            }

            while let Some(joined) = join_set.join_next().await {
                match joined {
                    Ok(outcome) => {
                        if result_tx.send(outcome.result).is_ok() {
                            request_background_repaint(repaint_ctx.as_ref());
                        }
                        summary.push(outcome.stage);
                    }
                    Err(err) => {
                        warn!("Quick scan worker repo task failed: {}", err);
                    }
                }
            }

            let (should_persist_addon_cache, addon_cache_entries, total_addon_states) = match shared_cache.lock() {
                Ok(guard) => (
                    guard.persistent_dirty,
                    guard.persistent_addon_hash_by_path.clone(),
                    guard.addon_state_by_path.len(),
                ),
                Err(poisoned) => {
                    let guard = poisoned.into_inner();
                    (
                        guard.persistent_dirty,
                        guard.persistent_addon_hash_by_path.clone(),
                        guard.addon_state_by_path.len(),
                    )
                }
            };

            info!(
                "Quick scan cache summary: cached_addon_states={} persistent_entries={} persistent_dirty={}",
                total_addon_states, addon_cache_entries.len(), should_persist_addon_cache
            );
            if should_persist_addon_cache {
                save_persistent_addon_hash_cache(&addon_cache_entries);
                info!(
                    "Saved quick-scan addon hash cache entries={}",
                    addon_cache_entries.len()
                );
            }
            summary.log_table("completed");
            info!(
                "Quick scan worker finished: op={} repositories={} elapsed={:.2?}",
                operation_id,
                repo_total,
                worker_started_at.elapsed()
            );
            info!(
                "Quick scan ready {:.2?} after app start",
                super::super::logging::PROCESS_START.elapsed()
            );
        });
    })
}

#[cfg(test)]
mod tests {
    use super::{StartupRepositoryInstance, normalize_startup_repositories};

    #[test]
    fn startup_repositories_deduplicate_only_matching_url_and_path() {
        let repositories = normalize_startup_repositories(vec![
            StartupRepositoryInstance {
                repo_url: "https://example.test/repo".to_string(),
                local_path: "C:/mods/one".to_string(),
            },
            StartupRepositoryInstance {
                repo_url: "https://example.test/repo/".to_string(),
                local_path: "C:/mods/two/".to_string(),
            },
            StartupRepositoryInstance {
                repo_url: "https://example.test/repo/".to_string(),
                local_path: "C:/mods/one/".to_string(),
            },
        ]);

        assert_eq!(repositories.len(), 2);
        assert_eq!(repositories[0].local_path, "c:/mods/one");
        assert_eq!(repositories[1].local_path, "c:/mods/two");
    }

    #[test]
    fn startup_repositories_ignore_incomplete_instances() {
        let repositories = normalize_startup_repositories(vec![
            StartupRepositoryInstance {
                repo_url: String::new(),
                local_path: "C:/mods/one".to_string(),
            },
            StartupRepositoryInstance {
                repo_url: "https://example.test/repo/".to_string(),
                local_path: String::new(),
            },
        ]);

        assert!(repositories.is_empty());
    }
}
