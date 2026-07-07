use super::format_layout::local_file_matches_part_layout;
use super::part_hashes::PartSpanSource;
use super::persistence::{
    calculate_hash_from_items, persist_file_checksums, persist_mod_checksums,
    persist_part_checksums, persist_repository_checksums,
};
use super::propagation::{update_mod_hashes_for_mods, update_repository_hashes_from_mods};
use super::scheduling::{
    build_file_hash_jobs, hash_cpu_budget, hash_scheduler_limits, log_addon_hash_metrics,
    missing_local_hash_pass_is_noop, recalculate_parts_for_jobs_with_profile,
};
use super::*;
use crate::core::tasks::remote_file_parts::{
    flush_deferred_part_inserts, flush_deferred_part_inserts_with_local_state,
    persist_part_local_state_by_file_path,
};
use crate::ui::types::HashIoProfilePreference;

pub(crate) async fn calculate_hashes(
    context: Arc<FoxyContext>,
    repository_url: &str,
    progress_tx: Option<&Sender<ProgressEvent>>,
) {
    let _ = calculate_hashes_with_tree(context, repository_url, None, progress_tx).await;
}

pub(crate) async fn calculate_hashes_with_profile(
    context: Arc<FoxyContext>,
    repository_url: &str,
    progress_tx: Option<&Sender<ProgressEvent>>,
    hash_io_profile: HashIoProfilePreference,
) {
    let _ = calculate_hashes_with_tree_and_profile(
        context,
        repository_url,
        None,
        progress_tx,
        hash_io_profile,
    )
    .await;
}

/// Runs the full hash pipeline and returns the computed tree for reuse by callers
/// (e.g. content-hash refresh), avoiding a redundant Tree::load.
pub(crate) async fn calculate_hashes_with_tree(
    context: Arc<FoxyContext>,
    repository_url: &str,
    preloaded_tree: Option<Tree>,
    progress_tx: Option<&Sender<ProgressEvent>>,
) -> Option<Tree> {
    calculate_hashes_with_tree_and_profile(
        context,
        repository_url,
        preloaded_tree,
        progress_tx,
        HashIoProfilePreference::Auto,
    )
    .await
}

pub(crate) enum HashCalculationResult {
    Completed(Box<Tree>),
    Cancelled,
    Failed,
}

fn derived_clean_part_indices(data_tree: &Tree) -> HashSet<usize> {
    let mut clean_indices = HashSet::new();
    for file_node in &data_tree.file_nodes {
        let Some(file) = data_tree.files.get(file_node.file_idx) else {
            continue;
        };
        if file_node.parts.is_empty() {
            continue;
        }
        let mut parts = Vec::with_capacity(file_node.parts.len());
        let mut all_parts_match = true;
        for &part_idx in &file_node.parts {
            let Some(part) = data_tree.parts.get(part_idx) else {
                all_parts_match = false;
                break;
            };
            if part.local_checksum.is_empty()
                || part.local_checksum != part.remote_checksum
                || part.local_length != part.remote_length
                || part.local_start != part.remote_start
            {
                all_parts_match = false;
                break;
            }
            parts.push(part.clone());
        }
        if all_parts_match && local_file_matches_part_layout(&file.local_path, file.length, &parts)
        {
            clean_indices.extend(file_node.parts.iter().copied());
        }
    }
    clean_indices
}

/// The background flush issues plain conflict-free `INSERT`s, so it is only safe
/// when the buffered rows were staged during a fresh `subfiles` load. Gate on the
/// buffer marker, not tree provenance, since a preloaded tree says nothing about
/// whether the deferred rows are conflict-free.
fn should_insert_part_metadata_in_background(context: &FoxyContext) -> bool {
    context.deferred_part_count() > 0 && context.deferred_part_inserts_are_fresh_load()
}

async fn await_deferred_part_metadata_insert(
    handle: &mut Option<tokio::task::JoinHandle<bool>>,
    repository_url: &str,
) -> bool {
    let Some(handle) = handle.take() else {
        return true;
    };
    match handle.await {
        Ok(true) => true,
        Ok(false) => {
            error!(
                "Deferred manifest part metadata insert failed for repo {}",
                repository_url
            );
            false
        }
        Err(err) => {
            error!(
                "Deferred manifest part metadata insert task failed for repo {}: {}",
                repository_url, err
            );
            false
        }
    }
}

pub(crate) async fn calculate_hashes_with_tree_and_profile(
    context: Arc<FoxyContext>,
    repository_url: &str,
    preloaded_tree: Option<Tree>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    hash_io_profile: HashIoProfilePreference,
) -> Option<Tree> {
    match calculate_hashes_with_tree_and_profile_cancellable(
        context,
        repository_url,
        preloaded_tree,
        progress_tx,
        hash_io_profile,
        None,
    )
    .await
    {
        HashCalculationResult::Completed(tree) => Some(*tree),
        HashCalculationResult::Cancelled | HashCalculationResult::Failed => None,
    }
}

pub(crate) async fn calculate_hashes_with_tree_and_profile_cancellable(
    context: Arc<FoxyContext>,
    repository_url: &str,
    preloaded_tree: Option<Tree>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    hash_io_profile: HashIoProfilePreference,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> HashCalculationResult {
    let total_start = Instant::now();
    let db = context.db();

    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled before tree load for repo {}",
            repository_url
        );
        return HashCalculationResult::Cancelled;
    }

    let mut data_tree: Tree = match preloaded_tree {
        Some(tree) => tree,
        None => match Tree::load(context.clone(), repository_url).await {
            Ok(tree) => tree,
            Err(err) => {
                error!(
                    "Failed to load tree for hash calculation (repo={}): {:#}",
                    repository_url, err
                );
                return HashCalculationResult::Failed;
            }
        },
    };

    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled after tree load for repo {}",
            repository_url
        );
        return HashCalculationResult::Cancelled;
    }

    let all_file_indices: Vec<usize> = data_tree
        .file_nodes
        .iter()
        .map(|node| node.file_idx)
        .collect();
    if missing_local_hash_pass_is_noop(&data_tree, &all_file_indices) {
        info!(
            "Hash pass not scheduled for repo {}: all {} target files are missing locally and no local checksum state needs clearing",
            repository_url,
            all_file_indices.len()
        );
        return HashCalculationResult::Completed(Box::new(data_tree));
    }
    let mut deferred_part_metadata_insert_handle = if should_insert_part_metadata_in_background(
        &context,
    ) {
        info!(
            "Starting deferred manifest part metadata insert in background before hashing: rows={}",
            context.deferred_part_count()
        );
        let flush_context = context.clone();
        context.set_defer_part_inserts(false);
        Some(tokio::spawn(async move {
            flush_deferred_part_inserts(flush_context).await
        }))
    } else {
        None
    };
    let hash_jobs = build_file_hash_jobs(
        &data_tree,
        &all_file_indices,
        PartSpanSource::DetectLocalLayout,
    );
    let total_parts: usize = hash_jobs.iter().map(|job| job.indexed_parts.len()).sum();
    let single_part_files = hash_jobs
        .iter()
        .filter(|job| job.indexed_parts.len() == 1)
        .count();
    let heavy_files = hash_jobs
        .iter()
        .filter(|job| job.indexed_parts.len() >= 64)
        .count();
    let avg_parts = if hash_jobs.is_empty() {
        0.0
    } else {
        total_parts as f64 / hash_jobs.len() as f64
    };
    let total_files = hash_jobs.len();
    let limits = hash_scheduler_limits(
        total_files,
        total_parts,
        HashIoProfilePreference::Aggressive,
    );
    info!(
        "Hashing {} files (parts={}) requested_profile={} with initial file_concurrency={} global_part_concurrency={} (cpu_budget={})",
        total_files,
        total_parts,
        hash_io_profile,
        limits.file_concurrency,
        limits.global_part_concurrency,
        hash_cpu_budget()
    );
    info!(
        "Hash scheduler profile: files_single_part={} files_heavy(>=64parts)={} avg_parts_per_file={:.2}",
        single_part_files, heavy_files, avg_parts
    );

    // --- Phase 1: Hash file parts ---
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::RecheckHashProgress {
            checked_files: 0,
            total_files,
            checked_parts: 0,
            total_parts,
        });
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Hashing 0/{} files", total_files),
            percent: 0.20,
        });
    }
    let hash_started = Instant::now();
    let (hash_results, profile_decision, cancelled) = recalculate_parts_for_jobs_with_profile(
        hash_jobs,
        hash_io_profile,
        None,
        progress_tx,
        total_files,
        cancel_rx,
    )
    .await;
    if cancelled || cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled during part hashing for repo {}",
            repository_url
        );
        await_deferred_part_metadata_insert(
            &mut deferred_part_metadata_insert_handle,
            repository_url,
        )
        .await;
        return HashCalculationResult::Cancelled;
    }
    info!(
        "Hash profile decision: requested={} selected={} reason={} benchmark_files={} benchmark_bytes={} benchmark_elapsed={:.2}s",
        profile_decision.requested,
        profile_decision.selected,
        profile_decision.reason,
        profile_decision.benchmarked_files,
        profile_decision.benchmarked_bytes,
        profile_decision.benchmark_elapsed.as_secs_f64()
    );
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Hash profile: {}", profile_decision.selected),
            percent: 0.86,
        });
    }
    info!(
        "Phase 1 (part hashing) completed in {:.2}s",
        hash_started.elapsed().as_secs_f64()
    );
    let missing_hash_files = hash_results
        .iter()
        .filter(|result| result.missing_file)
        .count();
    if missing_hash_files > 0 {
        let missing_percent = missing_hash_files.saturating_mul(100) / total_files.max(1);
        if missing_percent >= 50 {
            warn!(
                "Hash pass skipped {} missing local files out of {} for repo {} ({}%). Repository may not be fully downloaded yet.",
                missing_hash_files, total_files, repository_url, missing_percent
            );
        } else {
            info!(
                "Hash pass skipped {} missing local files out of {} for repo {} ({}%)",
                missing_hash_files, total_files, repository_url, missing_percent
            );
        }
    }
    log_addon_hash_metrics("full_hash", &data_tree, &hash_results);

    // Log slowest files and distribution (ISSUE_07)
    {
        use std::time::Duration;
        let mut file_timings: Vec<(&str, Duration, usize)> = hash_results
            .iter()
            .map(|r| (r.file_path.as_str(), r.elapsed, r.parts_count))
            .collect();
        file_timings.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let slow_count = file_timings.len().min(10);
        if slow_count > 0 && file_timings[0].1 > Duration::from_millis(500) {
            for (path, elapsed, parts) in &file_timings[..slow_count] {
                if *elapsed > Duration::from_millis(500) {
                    info!(
                        "Hash slow file: path={} elapsed={:.2?} parts={}",
                        path, elapsed, parts
                    );
                }
            }
        }
        let under_100ms = file_timings
            .iter()
            .filter(|t| t.1 < Duration::from_millis(100))
            .count();
        let under_1s = file_timings
            .iter()
            .filter(|t| t.1 < Duration::from_secs(1))
            .count();
        let over_1s = file_timings
            .iter()
            .filter(|t| t.1 >= Duration::from_secs(1))
            .count();
        let over_5s = file_timings
            .iter()
            .filter(|t| t.1 >= Duration::from_secs(5))
            .count();
        info!(
            "Hash timing distribution: total_files={} <100ms={} <1s={} >=1s={} >=5s={}",
            file_timings.len(),
            under_100ms,
            under_1s,
            over_1s,
            over_5s
        );
    }

    // --- Apply hash results to data_tree ---
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled before applying part hashes for repo {}",
            repository_url
        );
        await_deferred_part_metadata_insert(
            &mut deferred_part_metadata_insert_handle,
            repository_url,
        )
        .await;
        return HashCalculationResult::Cancelled;
    }
    let apply_parts_started = Instant::now();
    let mut updated_part_indices: HashSet<usize> = HashSet::new();
    let mut whole_file_checksums_by_file_idx: HashMap<usize, String> = HashMap::new();
    for file_result in hash_results {
        if let Some(checksum) = file_result.whole_file_checksum {
            whole_file_checksums_by_file_idx.insert(file_result.file_idx, checksum);
        }
        for (part_idx, updated_part) in file_result.updated_parts {
            if let Some(dest) = data_tree.parts.get_mut(part_idx) {
                *dest = updated_part;
                updated_part_indices.insert(part_idx);
            }
        }
    }
    info!(
        "Phase 1 (apply part hashes) completed in {:.3}s ({} updated parts)",
        apply_parts_started.elapsed().as_secs_f64(),
        updated_part_indices.len()
    );

    // --- Phase 2: Persist part hashes (batched transactions) ---
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled before persisting part hashes for repo {}",
            repository_url
        );
        await_deferred_part_metadata_insert(
            &mut deferred_part_metadata_insert_handle,
            repository_url,
        )
        .await;
        return HashCalculationResult::Cancelled;
    }
    let derived_clean_indices = if deferred_part_metadata_insert_handle.is_some() {
        derived_clean_part_indices(&data_tree)
    } else {
        HashSet::new()
    };
    let fallback_part_indices: HashSet<usize> = updated_part_indices
        .difference(&derived_clean_indices)
        .copied()
        .collect();
    let mut part_updates: Vec<_> = fallback_part_indices
        .iter()
        .filter_map(|&idx| data_tree.parts.get(idx).cloned())
        .collect();
    // Sort by PK so the UPDATE walks the subfiles B-tree sequentially,
    // keeping pages in cache instead of thrashing on random access.
    part_updates.sort_by_key(|p| p.id);
    let derived_clean_part_count = derived_clean_indices.len();
    let part_count = derived_clean_part_count.saturating_add(part_updates.len());
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Saving parts 0/{}", part_count),
            percent: 0.50,
        });
    }
    let persist_started = Instant::now();
    let mut parts_persisted = 0;
    if deferred_part_metadata_insert_handle.is_some() {
        if derived_clean_part_count > 0 {
            parts_persisted += derived_clean_part_count;
            info!(
                "Phase 2 derived clean part state: parts={} fallback_parts={} db_part_updates=skipped",
                derived_clean_part_count,
                part_updates.len()
            );
            info!(
                "Phase 2 progress: {}/{} parts accepted as derived clean state",
                parts_persisted, part_count
            );
            if let Some(tx) = progress_tx {
                let pct = 0.50 + 0.20 * (parts_persisted as f32 / part_count.max(1) as f32);
                let _ = tx.send(ProgressEvent::Stage {
                    label: format!("Saving parts {}/{}", parts_persisted, part_count),
                    percent: pct,
                });
            }
        }
        if !await_deferred_part_metadata_insert(
            &mut deferred_part_metadata_insert_handle,
            repository_url,
        )
        .await
        {
            return HashCalculationResult::Failed;
        }
        if !part_updates.is_empty()
            && !persist_part_local_state_by_file_path(&db, &part_updates, |persisted_chunk| {
                parts_persisted += persisted_chunk;
                if parts_persisted % PERSIST_LOG_INTERVAL == 0 || parts_persisted == part_count {
                    info!(
                        "Phase 2 progress: {}/{} fallback parts persisted",
                        parts_persisted, part_count
                    );
                }
                if let Some(tx) = progress_tx {
                    let pct = 0.50 + 0.20 * (parts_persisted as f32 / part_count.max(1) as f32);
                    let _ = tx.send(ProgressEvent::Stage {
                        label: format!("Saving parts {}/{}", parts_persisted, part_count),
                        percent: pct,
                    });
                }
            })
            .await
        {
            return HashCalculationResult::Failed;
        }
    } else if context.deferred_part_count() > 0 {
        info!(
            "Persisting {} deferred manifest parts with local hash state in one coalesced sorted insert pass",
            data_tree.parts.len()
        );
        let total_deferred_parts = data_tree.parts.len();
        let mut next_progress_log = PERSIST_LOG_INTERVAL;
        if flush_deferred_part_inserts_with_local_state(
            context.clone(),
            &data_tree.parts,
            |persisted_chunk| {
                parts_persisted += persisted_chunk;
                if parts_persisted >= next_progress_log || parts_persisted == total_deferred_parts {
                    info!(
                        "Phase 2 progress: {}/{} deferred parts persisted",
                        parts_persisted, total_deferred_parts
                    );
                    next_progress_log += PERSIST_LOG_INTERVAL;
                }
                if let Some(tx) = progress_tx {
                    let pct =
                        0.50 + 0.20 * (parts_persisted as f32 / total_deferred_parts.max(1) as f32);
                    let _ = tx.send(ProgressEvent::Stage {
                        label: format!("Saving parts {}/{}", parts_persisted, total_deferred_parts),
                        percent: pct,
                    });
                }
            },
        )
        .await
        {
        } else {
            error!(
                "Failed to persist deferred manifest parts with local hash state for repo {}",
                repository_url
            );
            return HashCalculationResult::Failed;
        }
    } else {
        persist_part_checksums(&db, &part_updates, |persisted_chunk| {
            parts_persisted += persisted_chunk;
            if parts_persisted % PERSIST_LOG_INTERVAL == 0 || parts_persisted == part_count {
                info!(
                    "Phase 2 progress: {}/{} parts persisted",
                    parts_persisted, part_count
                );
            }
            if let Some(tx) = progress_tx {
                let pct = 0.50 + 0.20 * (parts_persisted as f32 / part_count.max(1) as f32);
                let _ = tx.send(ProgressEvent::Stage {
                    label: format!("Saving parts {}/{}", parts_persisted, part_count),
                    percent: pct,
                });
            }
        })
        .await;
    }
    info!(
        "Phase 2 (persist parts) completed in {:.2}s ({} parts)",
        persist_started.elapsed().as_secs_f64(),
        part_count
    );

    // --- Phase 3: Update file hashes from parts (batched transactions) ---
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled before file hash rollup for repo {}",
            repository_url
        );
        return HashCalculationResult::Cancelled;
    }
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Updating files 0/{}", data_tree.files.len()),
            percent: 0.72,
        });
    }
    let file_rollup_started = Instant::now();
    let mut updated_file_indices: Vec<usize> = Vec::new();
    for file_node in &data_tree.file_nodes {
        if let Some(file) = data_tree.files.get_mut(file_node.file_idx) {
            let old_checksum = file.local_checksum.clone();

            let mut file_parts: Vec<FoxyModFilePart> = file_node
                .parts
                .iter()
                .filter_map(|&part_idx| data_tree.parts.get(part_idx).cloned())
                .collect();
            file_parts.sort_by_key(|p| p.data_order);

            let has_parts = !file_parts.is_empty();
            let parts_match = file_parts
                .iter()
                .all(|p| !p.local_checksum.is_empty() && p.local_checksum == p.remote_checksum);
            let layout_matches = has_parts
                && local_file_matches_part_layout(&file.local_path, file.length, &file_parts);
            let mut new_checksum = calculate_hash_from_items(&mut file_parts);
            let all_match = parts_match && layout_matches;

            if new_checksum != file.remote_checksum && all_match {
                warn!("Fixing legacy checksum mismatch for file: {}", file.name);
                new_checksum = file.remote_checksum.clone();
            }

            if !has_parts {
                if let Some(checksum) = whole_file_checksums_by_file_idx.get(&file_node.file_idx) {
                    file.local_checksum = checksum.clone();
                } else if !old_checksum.is_empty() && old_checksum == file.remote_checksum {
                    file.local_checksum = old_checksum.clone();
                } else {
                    file.local_checksum = String::new();
                }
            } else if all_match {
                file.local_checksum = file.remote_checksum.clone();
            } else if parts_match && !layout_matches {
                file.local_checksum = String::new();
            } else {
                file.local_checksum = new_checksum.to_uppercase();
            }

            if file.local_checksum != old_checksum {
                updated_file_indices.push(file_node.file_idx);
            }
        }
    }
    info!(
        "Phase 3 (roll up file hashes from parts) completed in {:.3}s ({} files, {} changed)",
        file_rollup_started.elapsed().as_secs_f64(),
        data_tree.file_nodes.len(),
        updated_file_indices.len()
    );

    let file_persist_started = Instant::now();
    let file_updates: Vec<_> = updated_file_indices
        .iter()
        .filter_map(|&idx| data_tree.files.get(idx).cloned())
        .collect();
    let file_count = file_updates.len();
    let mut files_persisted = 0;
    persist_file_checksums(&db, &file_updates, |persisted_chunk| {
        files_persisted += persisted_chunk;
        if files_persisted % PERSIST_LOG_INTERVAL == 0 || files_persisted == file_count {
            info!(
                "Phase 3 progress: {}/{} files persisted",
                files_persisted, file_count
            );
        }
        if let Some(tx) = progress_tx {
            let pct = 0.72 + 0.10 * (files_persisted as f32 / file_count.max(1) as f32);
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Saving files {}/{}", files_persisted, file_count),
                percent: pct,
            });
        }
    })
    .await;
    info!(
        "Phase 3 (persist files) completed in {:.2}s ({} files)",
        file_persist_started.elapsed().as_secs_f64(),
        file_count
    );

    // --- Phase 4: Update addon(mod) hashes from files (batched transactions) ---
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled before addon hash rollup for repo {}",
            repository_url
        );
        return HashCalculationResult::Cancelled;
    }
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Updating addons 0/{}", data_tree.mods.len()),
            percent: 0.84,
        });
    }
    let mod_rollup_started = Instant::now();
    let updated_mod_indices = update_mod_hashes_for_mods(&mut data_tree, None);
    info!(
        "Phase 4 (roll up addon hashes from files) completed in {:.3}s ({} addons changed)",
        mod_rollup_started.elapsed().as_secs_f64(),
        updated_mod_indices.len()
    );
    let mod_updates: Vec<_> = updated_mod_indices
        .iter()
        .filter_map(|&idx| data_tree.mods.get(idx).cloned())
        .collect();
    let mod_count = mod_updates.len();
    let mod_persist_started = Instant::now();
    let mut mods_persisted = 0;
    persist_mod_checksums(&db, &mod_updates, |persisted_chunk| {
        mods_persisted += persisted_chunk;
        if mods_persisted % 500 == 0 || mods_persisted == mod_count {
            info!(
                "Phase 4 progress: {}/{} mods persisted",
                mods_persisted, mod_count
            );
        }
        if let Some(tx) = progress_tx {
            let pct = 0.84 + 0.06 * (mods_persisted as f32 / mod_count.max(1) as f32);
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Saving addons {}/{}", mods_persisted, mod_count),
                percent: pct,
            });
        }
    })
    .await;
    info!(
        "Phase 4 (persist mods) completed in {:.2}s ({} mods)",
        mod_persist_started.elapsed().as_secs_f64(),
        mod_count
    );

    // --- Phase 5: Update repository hashes based on addons(mods) (batched transactions) ---
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        info!(
            "Hash calculation cancelled before repository hash rollup for repo {}",
            repository_url
        );
        return HashCalculationResult::Cancelled;
    }
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: "Updating repositories".into(),
            percent: 0.92,
        });
    }
    let repo_rollup_started = Instant::now();
    update_repository_hashes_from_mods(&mut data_tree);
    info!(
        "Phase 5 (roll up repository hashes from addons) completed in {:.3}s ({} repositories)",
        repo_rollup_started.elapsed().as_secs_f64(),
        data_tree.repositories.len()
    );
    let repo_persist_started = Instant::now();
    let repo_updates: Vec<_> = data_tree.repositories.clone();
    persist_repository_checksums(&db, &repo_updates, |_| {}).await;
    info!(
        "Phase 5 (persist repos) completed in {:.2}s",
        repo_persist_started.elapsed().as_secs_f64()
    );
    info!(
        "Total hash recalculation completed in {:.2}s",
        total_start.elapsed().as_secs_f64()
    );
    HashCalculationResult::Completed(Box::new(data_tree))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::context::DeferredPartInsert;
    use crate::core::models::model_tree::{FileNode, Tree};

    /// Fresh rows stay eligible for the background metadata insert after
    /// `fresh_subfiles_load` is cleared: the buffer's own marker gates it.
    #[tokio::test]
    async fn background_part_metadata_insert_selected_for_fresh_buffered_rows() {
        let handle = crate::core::tasks::db_turso::build_test_database().await;
        let context = Arc::new(FoxyContext::new(handle, reqwest::Client::new()));

        context.set_fresh_subfiles_load(true);
        context.set_defer_part_inserts(true);
        context.buffer_deferred_parts(vec![DeferredPartInsert {
            file_id: 1,
            path: "p0".to_owned(),
            remote_length: 1,
            remote_start: 0,
            remote_checksum: "r0".to_owned(),
            data_order: 0,
        }]);
        context.set_fresh_subfiles_load(false);

        assert!(should_insert_part_metadata_in_background(&context));
    }

    #[tokio::test]
    async fn background_part_metadata_insert_rejected_for_non_fresh_rows() {
        let handle = crate::core::tasks::db_turso::build_test_database().await;
        let context = Arc::new(FoxyContext::new(handle, reqwest::Client::new()));

        context.set_defer_part_inserts(true);
        context.buffer_deferred_parts(vec![DeferredPartInsert {
            file_id: 1,
            path: "p0".to_owned(),
            remote_length: 1,
            remote_start: 0,
            remote_checksum: "r0".to_owned(),
            data_order: 0,
        }]);

        assert!(!should_insert_part_metadata_in_background(&context));
    }

    #[test]
    fn derived_clean_part_indices_accepts_clean_layout() {
        let file_path = std::env::temp_dir().join(format!(
            "foxy-derived-clean-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&file_path, b"abcd").unwrap();

        let mut tree = Tree::default();
        tree.files.push(FoxyModFile {
            id: 10,
            local_path: file_path.to_string_lossy().to_string(),
            length: 4,
            ..Default::default()
        });
        tree.parts.push(FoxyModFilePart {
            id: 1,
            file_id: 10,
            path: "part0".to_owned(),
            remote_checksum: "A".to_owned(),
            local_checksum: "A".to_owned(),
            remote_length: 4,
            local_length: 4,
            remote_start: 0,
            local_start: 0,
            data_order: 0,
        });
        tree.file_nodes.push(FileNode {
            file_idx: 0,
            parts: vec![0],
        });

        assert_eq!(derived_clean_part_indices(&tree), HashSet::from([0]));
        let _ = std::fs::remove_file(file_path);
    }

    #[test]
    fn derived_clean_part_indices_rejects_dirty_part() {
        let file_path = std::env::temp_dir().join(format!(
            "foxy-derived-dirty-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&file_path, b"abcd").unwrap();

        let mut tree = Tree::default();
        tree.files.push(FoxyModFile {
            id: 10,
            local_path: file_path.to_string_lossy().to_string(),
            length: 4,
            ..Default::default()
        });
        tree.parts.push(FoxyModFilePart {
            id: 1,
            file_id: 10,
            path: "part0".to_owned(),
            remote_checksum: "A".to_owned(),
            local_checksum: "B".to_owned(),
            remote_length: 4,
            local_length: 4,
            remote_start: 0,
            local_start: 0,
            data_order: 0,
        });
        tree.file_nodes.push(FileNode {
            file_idx: 0,
            parts: vec![0],
        });

        assert!(derived_clean_part_indices(&tree).is_empty());
        let _ = std::fs::remove_file(file_path);
    }
}
