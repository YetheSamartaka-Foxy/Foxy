use super::super::*;
use super::content_hash::{
    persist_mod_content_hashes, refresh_content_hashes_for_scoped_tree,
    refresh_content_hashes_for_tree, refresh_content_hashes_when_tree_matches,
};
use super::diff_addon_hash::resolve_addon_hashes;
use super::diff_file_resolution::compute_file_diffs;
use super::local_path_preflight::{
    format_local_path_mismatch_message, log_local_path_availability,
    summarize_local_path_availability, suspect_local_path_mismatch,
};
use super::readiness::{
    QuickScanBootstrapPlan, QuickScanPreflightResult, collect_files_with_missing_local_tree_hashes,
    content_hash_baseline_missing, content_hash_baseline_ready, partition_tree_hash_ready_files,
    quick_scan_preflight_for_local_check, tree_local_checksums_missing,
};
use super::shared_cache::QuickScanSharedCache;
use crate::core::db::{DbValue, params};
use crate::core::models::modification::ADDON_COLUMNS;
use crate::core::tasks::calculate_hashes::{
    finalize_repository_content_hashes_from_mods, finalize_repository_hashes_from_mods,
};
use crate::core::utils::format::sanitize_log_path_str;
use crate::core::utils::speed_of_light::{SolLight, sol_line};
use std::collections::HashSet as StdHashSet;
use std::sync::{Mutex as StdMutex, OnceLock};

static ZERO_HIT_CACHE_WARNING_REPOS: OnceLock<StdMutex<StdHashSet<String>>> = OnceLock::new();

/// One async lock per repository so concurrent quick scans of the same repo
/// (startup worker + manual recheck's quick verify, for example) coalesce
/// instead of both bootstrapping and hashing the full file set in parallel -
/// on an HDD that doubles a multi-minute pass and thrashes the disk.
static QUICK_SCAN_REPO_LOCKS: OnceLock<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn quick_scan_repo_lock(repo_url: &str) -> Arc<tokio::sync::Mutex<()>> {
    let key = repo_url.trim().trim_end_matches('/').to_string();
    let locks = QUICK_SCAN_REPO_LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut guard = match locks.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .entry(key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn should_warn_persistent_zero_hits(repo_url: &str) -> bool {
    let warned_repos =
        ZERO_HIT_CACHE_WARNING_REPOS.get_or_init(|| StdMutex::new(StdHashSet::new()));
    match warned_repos.lock() {
        Ok(mut guard) => guard.insert(repo_url.to_string()),
        Err(poisoned) => poisoned.into_inner().insert(repo_url.to_string()),
    }
}

fn has_conclusive_file_presence_or_size_mismatch(
    missing_files: usize,
    size_mismatch_files: usize,
) -> bool {
    missing_files > 0 || size_mismatch_files > 0
}

/// Emit the canonical `SOL op=quick_scan` line (conventions/SPEED_OF_LIGHT.md, O4).
/// Quick scan work is stat-denominated, so no absolute byte light exists in-app;
/// the rate is trended against the machine's own best clean-run baseline.
#[allow(clippy::too_many_arguments)]
fn log_quick_scan_sol(
    repo_url: &str,
    elapsed: std::time::Duration,
    addons_total: usize,
    addons_hashed: usize,
    cache_hits_shared: usize,
    cache_hits_persistent: usize,
    deep_scan_files: usize,
    outcome: &str,
) {
    let addons_per_s = if elapsed.as_secs_f64() > 0.0 {
        addons_total as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    info!(
        "{}",
        sol_line(
            "quick_scan",
            0,
            elapsed,
            &SolLight::SelfBaseline,
            &[
                ("repo", repo_url.to_string()),
                ("addons_total", addons_total.to_string()),
                ("addons_hashed", addons_hashed.to_string()),
                ("cache_hits_shared", cache_hits_shared.to_string()),
                ("cache_hits_persistent", cache_hits_persistent.to_string()),
                ("deep_scan_files", deep_scan_files.to_string()),
                ("addons_per_s", format!("{:.1}", addons_per_s)),
                ("outcome", outcome.to_string()),
            ],
        )
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn quick_local_change_diff(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mod_name_filter: Option<&HashSet<String>>,
    mod_enabled_overrides: Option<&HashMap<String, bool>>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    auto_tree_verify_on_mismatch: bool,
    already_eligible: bool,
    force_fresh_addon_hash: bool,
    shared_cache: Option<&Arc<Mutex<QuickScanSharedCache>>>,
) -> Vec<ModDiffSummary> {
    let repo_lock = quick_scan_repo_lock(repo_url);
    let _in_flight_guard = match repo_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            info!(
                "Quick scan for repo {} is already in flight; waiting for it to finish before re-checking",
                repo_url
            );
            repo_lock.lock().await
        }
    };
    quick_local_change_diff_locked(
        context,
        repo_url,
        mod_name_filter,
        mod_enabled_overrides,
        progress_tx,
        auto_tree_verify_on_mismatch,
        already_eligible,
        force_fresh_addon_hash,
        shared_cache,
    )
    .await
}

/// Body of [`quick_local_change_diff`]; callers must hold the per-repo
/// in-flight lock. The tree-verify retry recurses here directly since the
/// outer entry point already holds the (non-reentrant) lock.
#[allow(clippy::too_many_arguments)]
async fn quick_local_change_diff_locked(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mod_name_filter: Option<&HashSet<String>>,
    mod_enabled_overrides: Option<&HashMap<String, bool>>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    auto_tree_verify_on_mismatch: bool,
    already_eligible: bool,
    force_fresh_addon_hash: bool,
    shared_cache: Option<&Arc<Mutex<QuickScanSharedCache>>>,
) -> Vec<ModDiffSummary> {
    let quick_scan_total_started = Instant::now();
    let preflight_started = Instant::now();
    let mut checksum_ready_check_elapsed = Duration::default();
    let mut bootstrap_check_elapsed = Duration::default();
    let mut file_fallback_elapsed = Duration::default();
    let mut tree_part_stats_load_elapsed = Duration::default();
    let mut tree_verify_elapsed = Duration::default();

    // Files whose tree hashes were computed by a bootstrap pass within this
    // scan invocation. A hash computed seconds ago cannot be stale, so the
    // targeted tree-hash verify (which force-rehashes every flagged file)
    // must skip them - otherwise a bootstrap whose result differs from remote
    // re-reads the entire repository a second time in the same scan.
    let mut bootstrap_hashed_all_files = false;
    let mut bootstrap_hashed_file_ids: HashSet<u64> = HashSet::new();

    // ── Phase 1: Preflight & Bootstrap ──────────────────────────────────
    if !already_eligible {
        let checksum_ready_started = Instant::now();
        let preflight = quick_scan_preflight_for_local_check(context.clone(), repo_url)
            .await
            .unwrap_or(QuickScanPreflightResult {
                remote_ready: false,
                bootstrap_plan: QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums,
            });
        checksum_ready_check_elapsed = checksum_ready_started.elapsed();

        if !preflight.remote_ready {
            info!(
                "Quick scan skipped for repo {}: remote checksum metadata is not ready yet",
                repo_url
            );
            return Vec::new();
        }

        let bootstrap_started = Instant::now();
        let bootstrap_plan = preflight.bootstrap_plan;
        match bootstrap_plan {
            QuickScanBootstrapPlan::None => {}
            QuickScanBootstrapPlan::RefreshContentBaseline => {
                info!(
                    "Quick scan bootstrap for repo {}: content hashes are partially missing, refreshing baseline from current state",
                    repo_url
                );
                if let Some(tx) = progress_tx {
                    let _ = tx.send(ProgressEvent::Stage {
                        label: "Refreshing content-hash baseline".into(),
                        percent: 0.10,
                    });
                }
                let _ =
                    refresh_content_hashes_when_tree_matches(context.clone(), repo_url, None).await;
            }
            QuickScanBootstrapPlan::InitializeTreeAndRefreshContent => {
                info!(
                    "Quick scan bootstrap for repo {}: content hashes missing, initializing tree hashes first",
                    repo_url
                );
                if let Some(tx) = progress_tx {
                    let _ = tx.send(ProgressEvent::Stage {
                        label: "Initializing tree hashes".into(),
                        percent: 0.10,
                    });
                }
                calculate_hashes(context.clone(), repo_url, progress_tx).await;
                bootstrap_hashed_all_files = true;
                let _ =
                    refresh_content_hashes_when_tree_matches(context.clone(), repo_url, None).await;
            }
            QuickScanBootstrapPlan::LoadTreeAndRepairMissingChecksums => {
                let scoped_bootstrap = mod_name_filter.is_some_and(|filter| !filter.is_empty());
                let tree_result = if let Some(filter) = mod_name_filter {
                    if filter.is_empty() {
                        Tree::load(context.clone(), repo_url).await
                    } else {
                        Tree::load_for_mod_names(context.clone(), repo_url, filter).await
                    }
                } else {
                    Tree::load(context.clone(), repo_url).await
                };
                if let Ok(tree) = tree_result {
                    let availability = summarize_local_path_availability(&tree);
                    log_local_path_availability(repo_url, &availability);
                    if suspect_local_path_mismatch(&availability) {
                        let message = format_local_path_mismatch_message(repo_url, &availability);
                        warn!("{message}");
                        if let Some(tx) = progress_tx {
                            let _ = tx.send(ProgressEvent::Failed(message));
                        }
                        return Vec::new();
                    }

                    if content_hash_baseline_missing(&tree) {
                        info!(
                            "Quick scan bootstrap for repo {}: content hashes missing, initializing tree hashes first",
                            repo_url
                        );
                        if let Some(tx) = progress_tx {
                            let _ = tx.send(ProgressEvent::Stage {
                                label: "Initializing tree hashes".into(),
                                percent: 0.10,
                            });
                        }
                        if scoped_bootstrap {
                            let file_ids = tree.files.iter().map(|file| file.id).collect();
                            let hashed = calculate_hashes_for_files(
                                context.clone(),
                                repo_url,
                                &file_ids,
                                progress_tx,
                                false,
                            )
                            .await;
                            bootstrap_hashed_file_ids.extend(hashed.processed_file_ids.iter());
                            if let Some(filter) = mod_name_filter
                                && let Ok(refreshed_tree) =
                                    Tree::load_for_mod_names(context.clone(), repo_url, filter)
                                        .await
                            {
                                let _ = refresh_content_hashes_for_scoped_tree(
                                    context.clone(),
                                    repo_url,
                                    &refreshed_tree,
                                )
                                .await;
                            }
                        } else {
                            calculate_hashes(context.clone(), repo_url, progress_tx).await;
                            bootstrap_hashed_all_files = true;
                            let _ = refresh_content_hashes_when_tree_matches(
                                context.clone(),
                                repo_url,
                                None,
                            )
                            .await;
                        }
                    } else if tree_local_checksums_missing(&tree) {
                        let missing_file_ids = collect_files_with_missing_local_tree_hashes(&tree);
                        if !missing_file_ids.is_empty() {
                            let readiness =
                                partition_tree_hash_ready_files(&tree, &missing_file_ids);
                            if !readiness.incomplete_files.is_empty() {
                                let preview = readiness
                                    .incomplete_files
                                    .iter()
                                    .take(5)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                info!(
                                    "Quick scan bootstrap for repo {}: {} local files appear smaller than expected (old version or partial write) [{}]",
                                    repo_url,
                                    readiness.incomplete_files.len(),
                                    preview
                                );
                                // Don't return early - hash the ready files and let the
                                // normal diff flow handle incomplete files.  Returning
                                // only the incomplete set would undercount the actual
                                // update when files are from an older version rather
                                // than a partially-written download.
                            }
                            if !readiness.ready_file_ids.is_empty() {
                                info!(
                                    "Quick scan bootstrap for repo {}: local tree hashes are partially missing, initializing {} files",
                                    repo_url,
                                    readiness.ready_file_ids.len()
                                );
                                if let Some(tx) = progress_tx {
                                    let _ = tx.send(ProgressEvent::Stage {
                                        label: format!(
                                            "Initializing missing tree hashes ({})",
                                            readiness.ready_file_ids.len()
                                        ),
                                        percent: 0.10,
                                    });
                                }
                                let hashed = calculate_hashes_for_files(
                                    context.clone(),
                                    repo_url,
                                    &readiness.ready_file_ids,
                                    progress_tx,
                                    false,
                                )
                                .await;
                                bootstrap_hashed_file_ids.extend(hashed.processed_file_ids.iter());
                                if scoped_bootstrap {
                                    if let Some(filter) = mod_name_filter
                                        && let Ok(refreshed_tree) = Tree::load_for_mod_names(
                                            context.clone(),
                                            repo_url,
                                            filter,
                                        )
                                        .await
                                    {
                                        let _ = refresh_content_hashes_for_scoped_tree(
                                            context.clone(),
                                            repo_url,
                                            &refreshed_tree,
                                        )
                                        .await;
                                    }
                                } else {
                                    let _ = refresh_content_hashes_when_tree_matches(
                                        context.clone(),
                                        repo_url,
                                        None,
                                    )
                                    .await;
                                }
                            }
                        }
                    } else if !content_hash_baseline_ready(&tree) {
                        info!(
                            "Quick scan bootstrap for repo {}: content hashes are partially missing, refreshing baseline from current tree state",
                            repo_url
                        );
                        if let Some(tx) = progress_tx {
                            let _ = tx.send(ProgressEvent::Stage {
                                label: "Refreshing content-hash baseline".into(),
                                percent: 0.10,
                            });
                        }
                        if scoped_bootstrap {
                            let _ = refresh_content_hashes_for_scoped_tree(
                                context.clone(),
                                repo_url,
                                &tree,
                            )
                            .await;
                        } else {
                            let _ =
                                refresh_content_hashes_for_tree(context.clone(), repo_url, &tree)
                                    .await;
                        }
                    }
                }
            }
        }
        bootstrap_check_elapsed = bootstrap_started.elapsed();
    }

    let preflight_elapsed = preflight_started.elapsed();

    // ── Phase 2: Database Load ──────────────────────────────────────────
    let db_load_started = Instant::now();
    let db = context.db();

    let repo_load_started = Instant::now();
    let repo = match load_repository_by_remote_url(context.clone(), repo_url).await {
        Ok(repo) => repo,
        Err(err) => {
            warn!(
                "Failed to load repository for quick scan {}: {}",
                repo_url, err
            );
            return Vec::new();
        }
    };
    let repo_load_elapsed = repo_load_started.elapsed();

    let display_name_started = Instant::now();
    crate::core::addon_metadata::regenerate_addon_display_names_for_repo(&db, repo_url).await;
    let display_name_elapsed = display_name_started.elapsed();

    let repo_addons_started = Instant::now();
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
                "Failed to load repo mods for quick scan {}: {}",
                repo_url, err
            );
            return Vec::new();
        }
    };
    let repo_addons_elapsed = repo_addons_started.elapsed();
    if mod_ids.is_empty() {
        info!(
            "Quick scan db-load timings: repo={} outcome=no_addons repo_load={:.2?} display_names={:.2?} repo_addons={:.2?} addon_rows=0.00ns total={:.2?}",
            repo_url,
            repo_load_elapsed,
            display_name_elapsed,
            repo_addons_elapsed,
            db_load_started.elapsed()
        );
        return Vec::new();
    }
    mod_ids.sort_unstable();
    mod_ids.dedup();

    let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);

    let addon_rows_started = Instant::now();
    let mut mods: Vec<FoxyMod> = Vec::new();
    let mut idx = 0usize;
    while idx < mod_ids.len() {
        let end = (idx + chunk_size).min(mod_ids.len());
        let chunk = &mod_ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT {ADDON_COLUMNS} FROM addons WHERE id IN ({placeholders}) \
             ORDER BY data_order ASC, id ASC"
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    if let Ok(m) = FoxyMod::from_row(&row) {
                        mods.push(m);
                    }
                }
            }
            Err(err) => {
                warn!("Failed to load mods for quick scan {}: {}", repo_url, err);
                return Vec::new();
            }
        }
        idx = end;
    }
    let addon_rows_elapsed = addon_rows_started.elapsed();

    if let Some(filter) = mod_name_filter
        && !filter.is_empty()
    {
        mods.retain(|m| filter.contains(&m.name.to_lowercase()));
        if mods.is_empty() {
            info!(
                "Quick scan db-load timings: repo={} outcome=filtered_empty repo_load={:.2?} display_names={:.2?} repo_addons={:.2?} addon_rows={:.2?} total={:.2?} mod_ids={} mods_loaded=0",
                repo_url,
                repo_load_elapsed,
                display_name_elapsed,
                repo_addons_elapsed,
                addon_rows_elapsed,
                db_load_started.elapsed(),
                mod_ids.len()
            );
            return Vec::new();
        }
    }

    let db_load_elapsed = db_load_started.elapsed();
    info!(
        "Quick scan db-load timings: repo={} outcome=loaded repo_load={:.2?} display_names={:.2?} repo_addons={:.2?} addon_rows={:.2?} total={:.2?} mod_ids={} mods_loaded={}",
        repo_url,
        repo_load_elapsed,
        display_name_elapsed,
        repo_addons_elapsed,
        addon_rows_elapsed,
        db_load_elapsed,
        mod_ids.len(),
        mods.len()
    );

    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: "Quick addon content hash check".into(),
            percent: 0.20,
        });
    }

    // ── Phase 3: Addon Hash Resolution ──────────────────────────────────
    let addon_hash = resolve_addon_hashes(
        &mods,
        mod_enabled_overrides,
        force_fresh_addon_hash,
        shared_cache,
    )
    .await;
    let addon_hash_elapsed = addon_hash.addon_hash_elapsed;

    if !addon_hash.mods_with_missing_path.is_empty() {
        let sample_paths = addon_hash
            .missing_addon_path_samples
            .iter()
            .map(|path| sanitize_log_path_str(path))
            .collect::<Vec<_>>()
            .join("; ");
        info!(
            "Quick scan missing addon directories: repo={} root={} missing_or_not_dir={} samples=[{}]",
            repo_url,
            sanitize_log_path_str(&repo.local_path),
            addon_hash.mods_with_missing_path.len(),
            sample_paths
        );
    }

    if !force_fresh_addon_hash
        && addon_hash.persistent_cache_entry_count > 0
        && addon_hash.addon_hash_hits_persistent == 0
        && addon_hash.addon_hash_calculated > 0
    {
        if should_warn_persistent_zero_hits(repo_url) {
            warn!(
                "Quick scan persistent addon hash cache produced zero hits for repo {} (entries={}, computed={}). Root fingerprint may be too volatile.",
                repo_url, addon_hash.persistent_cache_entry_count, addon_hash.addon_hash_calculated
            );
        } else {
            debug!(
                "Quick scan persistent addon hash cache produced zero hits again for repo {} (entries={}, computed={})",
                repo_url, addon_hash.persistent_cache_entry_count, addon_hash.addon_hash_calculated
            );
        }
    }

    let has_tree_or_path_mismatch = !addon_hash.mods_with_tree_mismatch.is_empty()
        || !addon_hash.mods_with_missing_path.is_empty();
    if addon_hash.deep_scan_mod_ids.is_empty() && !has_tree_or_path_mismatch {
        debug!(
            "Quick scan addon-hash scheduler: repo={} addons={} deep_scan_addons=0 deep_scan_files=0",
            repo_url, addon_hash.enabled_addons
        );
        debug!(
            "Quick scan addon-hash stage: repo={} elapsed={:.2?} concurrency={} computed={} shared_hits={} persistent_hits={}",
            repo_url,
            addon_hash_elapsed,
            addon_hash.addon_hash_concurrency,
            addon_hash.addon_hash_calculated,
            addon_hash.addon_hash_hits_shared_memory,
            addon_hash.addon_hash_hits_persistent
        );
        if !addon_hash.addon_hash_timings.is_empty() {
            let mut slowest = addon_hash.addon_hash_timings.clone();
            slowest.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            let preview = slowest
                .into_iter()
                .take(5)
                .map(|(path, elapsed, source)| format!("{} ({:.2?}, {})", path, elapsed, source))
                .collect::<Vec<_>>()
                .join("; ");
            debug!(
                "Quick scan slow addon paths: repo={} [{}]",
                repo_url, preview
            );
        }
        info!(
            "Quick scan summary: repo={} addons_total={} addons_updates=0 missing_files=0 size_mismatch_files=0 unexpected_files=0 unexpected_addons=0 tree_checksum_mismatch_files=0 content_mismatch_files=0 content_mismatch_addons=0",
            repo_url, addon_hash.enabled_addons
        );
        log_quick_scan_sol(
            repo_url,
            quick_scan_total_started.elapsed(),
            addon_hash.enabled_addons,
            addon_hash.addon_hash_calculated,
            addon_hash.addon_hash_hits_shared_memory,
            addon_hash.addon_hash_hits_persistent,
            0,
            "clean",
        );
        info!(
            "Quick scan timings: repo={} outcome=clean preflight={:.2?} checksum_ready_check={:.2?} bootstrap_check={:.2?} db_load={:.2?} addon_hash={:.2?} file_fallback={:.2?} tree_part_stats_load={:.2?} tree_verify={:.2?} total={:.2?} addons_total={} computed={} shared_hits={} persistent_hits={} deep_scan_files=0",
            repo_url,
            preflight_elapsed,
            checksum_ready_check_elapsed,
            bootstrap_check_elapsed,
            db_load_elapsed,
            addon_hash_elapsed,
            file_fallback_elapsed,
            tree_part_stats_load_elapsed,
            tree_verify_elapsed,
            quick_scan_total_started.elapsed(),
            addon_hash.enabled_addons,
            addon_hash.addon_hash_calculated,
            addon_hash.addon_hash_hits_shared_memory,
            addon_hash.addon_hash_hits_persistent
        );
        return Vec::new();
    }

    // ── Phase 4: File Resolution & Diff Computation ─────────────────────
    let diff_result = match compute_file_diffs(
        context.clone(),
        repo_url,
        &mods,
        mod_enabled_overrides,
        &addon_hash,
        progress_tx,
        shared_cache,
    )
    .await
    {
        Some(result) => result,
        None => return Vec::new(),
    };
    file_fallback_elapsed = diff_result.file_fallback_elapsed;
    tree_part_stats_load_elapsed = diff_result.tree_part_stats_load_elapsed;

    debug!(
        "Quick scan addon-hash scheduler: repo={} addons={} deep_scan_addons={} deep_scan_files={}",
        repo_url,
        addon_hash.enabled_addons,
        addon_hash.deep_scan_mod_ids.len(),
        diff_result.deep_scan_files_total
    );
    debug!(
        "Quick scan addon-hash stage: repo={} elapsed={:.2?} concurrency={} computed={} shared_hits={} persistent_hits={} content_mismatch_addons={}",
        repo_url,
        addon_hash_elapsed,
        addon_hash.addon_hash_concurrency,
        addon_hash.addon_hash_calculated,
        addon_hash.addon_hash_hits_shared_memory,
        addon_hash.addon_hash_hits_persistent,
        addon_hash.phase1_addon_content_mismatch_count
    );
    if !addon_hash.addon_hash_timings.is_empty() {
        let mut slowest = addon_hash.addon_hash_timings.clone();
        slowest.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let preview = slowest
            .into_iter()
            .take(5)
            .map(|(path, elapsed, source)| format!("{} ({:.2?}, {})", path, elapsed, source))
            .collect::<Vec<_>>()
            .join("; ");
        debug!(
            "Quick scan slow addon paths: repo={} [{}]",
            repo_url, preview
        );
    }

    // ── Phase 5: Finalization ───────────────────────────────────────────
    let diffs = diff_result.diffs;

    if !diff_result.clean_hash_updates.is_empty() {
        persist_mod_content_hashes(&db, repo_url, &diff_result.clean_hash_updates).await;
        finalize_repository_hashes_from_mods(context.clone(), repo_url).await;
        finalize_repository_content_hashes_from_mods(context.clone(), repo_url).await;
        info!(
            "Quick scan normalized addon content-hash baseline for repo {} (addons={})",
            repo_url,
            diff_result.clean_hash_updates.len()
        );
    }

    let mut addons_content_mismatch = diff_result.addons_content_mismatch;
    let mut addons_needing_tree_hash = diff_result.addons_needing_tree_hash;
    addons_content_mismatch.sort_unstable();
    addons_needing_tree_hash.sort_unstable();
    addons_needing_tree_hash.dedup();
    if !addons_needing_tree_hash.is_empty() {
        info!(
            "Quick scan content-hash mismatches detected for repo={} addons={} [{}]. Tree hash verification is recommended before download.",
            repo_url,
            addons_needing_tree_hash.len(),
            addons_needing_tree_hash.join(", ")
        );
        if let Some(tx) = progress_tx {
            let _ = tx.send(ProgressEvent::Stage {
                label: format!(
                    "Tree hash verify recommended for {} addons",
                    addons_needing_tree_hash.len()
                ),
                percent: 0.60,
            });
        }
    }

    info!(
        "Quick scan summary: repo={} addons_total={} addons_updates={} missing_files={} size_mismatch_files={} unexpected_files={} unexpected_addons={} tree_checksum_mismatch_files={} content_mismatch_files={} content_mismatch_addons={}",
        repo_url,
        addon_hash.enabled_addons,
        diff_result.addons_with_updates,
        diff_result.missing_files,
        diff_result.size_mismatch_files,
        diff_result.unexpected_files,
        diff_result.addons_with_unexpected_files,
        diff_result.checksum_mismatch_files,
        diff_result.content_mismatch_files,
        addons_content_mismatch.len()
    );
    log_quick_scan_sol(
        repo_url,
        quick_scan_total_started.elapsed(),
        addon_hash.enabled_addons,
        addon_hash.addon_hash_calculated,
        addon_hash.addon_hash_hits_shared_memory,
        addon_hash.addon_hash_hits_persistent,
        diff_result.deep_scan_files_total,
        if diff_result.addons_with_updates > 0 {
            "updates"
        } else {
            "clean_after_diff"
        },
    );

    // ── Phase 6: Optional Tree Verify ───────────────────────────────────
    // The verify exists to catch stale local tree hashes. Files hashed by this
    // scan's own bootstrap pass cannot be stale, so re-verifying them would
    // only repeat the same disk read and produce the same result; their diff
    // entries already reflect fresh hashes.
    let verify_targets: HashSet<u64> = if bootstrap_hashed_all_files {
        HashSet::new()
    } else {
        diff_result
            .files_needing_tree_verify
            .difference(&bootstrap_hashed_file_ids)
            .copied()
            .collect()
    };
    if auto_tree_verify_on_mismatch
        && !diff_result.files_needing_tree_verify.is_empty()
        && verify_targets.is_empty()
    {
        info!(
            "Quick scan skipping targeted tree-hash verify for repo={}: all {} flagged files were hashed by this scan's bootstrap",
            repo_url,
            diff_result.files_needing_tree_verify.len()
        );
    }
    if auto_tree_verify_on_mismatch && !verify_targets.is_empty() {
        let tree_verify_started = Instant::now();
        info!(
            "Quick scan triggering targeted tree-hash verify for repo={} files={}",
            repo_url,
            verify_targets.len()
        );
        if let Some(tx) = progress_tx {
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Verifying tree hashes for {} files", verify_targets.len()),
                percent: 0.70,
            });
        }
        let hashed = calculate_hashes_for_files(
            context.clone(),
            repo_url,
            &verify_targets,
            progress_tx,
            true,
        )
        .await;
        if !hashed.processed() {
            warn!(
                "Quick scan targeted tree-hash verify reported no updates for repo={} (files={})",
                repo_url,
                verify_targets.len()
            );
        }
        let _ = refresh_content_hashes_when_tree_matches(context.clone(), repo_url, None).await;
        tree_verify_elapsed = tree_verify_started.elapsed();
        info!(
            "Quick scan timings: repo={} outcome=retry_after_tree_verify preflight={:.2?} checksum_ready_check={:.2?} bootstrap_check={:.2?} db_load={:.2?} addon_hash={:.2?} file_fallback={:.2?} tree_part_stats_load={:.2?} tree_verify={:.2?} total_before_retry={:.2?} addons_total={} computed={} shared_hits={} persistent_hits={} deep_scan_files={}",
            repo_url,
            preflight_elapsed,
            checksum_ready_check_elapsed,
            bootstrap_check_elapsed,
            db_load_elapsed,
            addon_hash_elapsed,
            file_fallback_elapsed,
            tree_part_stats_load_elapsed,
            tree_verify_elapsed,
            quick_scan_total_started.elapsed(),
            addon_hash.enabled_addons,
            addon_hash.addon_hash_calculated,
            addon_hash.addon_hash_hits_shared_memory,
            addon_hash.addon_hash_hits_persistent,
            diff_result.deep_scan_files_total
        );
        if has_conclusive_file_presence_or_size_mismatch(
            diff_result.missing_files,
            diff_result.size_mismatch_files,
        ) {
            info!(
                "Quick scan returning pending update after tree verify for repo={}: missing_files={} size_mismatch_files={} updates={}",
                repo_url,
                diff_result.missing_files,
                diff_result.size_mismatch_files,
                diff_result.addons_with_updates
            );
            return diffs;
        }
        return Box::pin(quick_local_change_diff_locked(
            context,
            repo_url,
            mod_name_filter,
            mod_enabled_overrides,
            progress_tx,
            false,
            false,
            false,
            shared_cache,
        ))
        .await;
    }

    info!(
        "Quick scan timings: repo={} outcome={} preflight={:.2?} checksum_ready_check={:.2?} bootstrap_check={:.2?} db_load={:.2?} addon_hash={:.2?} file_fallback={:.2?} tree_part_stats_load={:.2?} tree_verify={:.2?} total={:.2?} addons_total={} computed={} shared_hits={} persistent_hits={} deep_scan_files={}",
        repo_url,
        if diff_result.addons_with_updates > 0 {
            "updates"
        } else {
            "clean_after_diff"
        },
        preflight_elapsed,
        checksum_ready_check_elapsed,
        bootstrap_check_elapsed,
        db_load_elapsed,
        addon_hash_elapsed,
        file_fallback_elapsed,
        tree_part_stats_load_elapsed,
        tree_verify_elapsed,
        quick_scan_total_started.elapsed(),
        addon_hash.enabled_addons,
        addon_hash.addon_hash_calculated,
        addon_hash.addon_hash_hits_shared_memory,
        addon_hash.addon_hash_hits_persistent,
        diff_result.deep_scan_files_total
    );

    diffs
}

#[cfg(test)]
mod tests {
    use super::has_conclusive_file_presence_or_size_mismatch;

    #[test]
    fn conclusive_mismatch_detects_missing_or_wrong_size_files() {
        assert!(has_conclusive_file_presence_or_size_mismatch(1, 0));
        assert!(has_conclusive_file_presence_or_size_mismatch(0, 1));
        assert!(has_conclusive_file_presence_or_size_mismatch(2, 3));
        assert!(!has_conclusive_file_presence_or_size_mismatch(0, 0));
    }
}
