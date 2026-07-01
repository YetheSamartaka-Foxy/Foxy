use super::part_hashes::PartSpanSource;
use super::pbo_layout::local_file_matches_part_layout;
use super::persistence::{
    CleanPartMark, calculate_hash_from_items, persist_file_checksums, persist_mod_checksums,
    persist_part_checksums, persist_repository_checksums,
};
use super::propagation::{
    collect_repo_indices_for_mods, update_mod_hashes_for_mods, update_repository_hashes_for_repos,
};
use super::scheduling::{
    AddonHashMetrics, build_file_hash_jobs, collect_addon_hash_metrics, hash_cpu_budget,
    hash_scheduler_limits, missing_local_hash_pass_is_noop,
    recalculate_parts_for_jobs_with_profile,
};
use super::*;
use crate::core::tasks::remote_file_parts::flush_deferred_part_inserts_with_local_state;
use crate::ui::types::HashIoProfilePreference;

#[derive(Clone, Debug, Default)]
pub(crate) struct HashPhaseTimings {
    pub(crate) hash_wall: std::time::Duration,
    pub(crate) apply_part_hashes: std::time::Duration,
    pub(crate) part_checksum_persist: std::time::Duration,
    pub(crate) file_rollup: std::time::Duration,
    pub(crate) file_rollup_persist: std::time::Duration,
    pub(crate) addon_rollup: std::time::Duration,
    pub(crate) addon_rollup_persist: std::time::Duration,
    pub(crate) repository_rollup: std::time::Duration,
    pub(crate) repository_rollup_persist: std::time::Duration,
    pub(crate) clean_part_mark_files: usize,
    pub(crate) clean_part_mark_parts: usize,
    pub(crate) fallback_part_update_parts: usize,
}

impl HashPhaseTimings {
    pub(crate) fn merge(&mut self, other: &Self) {
        self.hash_wall += other.hash_wall;
        self.apply_part_hashes += other.apply_part_hashes;
        self.part_checksum_persist += other.part_checksum_persist;
        self.file_rollup += other.file_rollup;
        self.file_rollup_persist += other.file_rollup_persist;
        self.addon_rollup += other.addon_rollup;
        self.addon_rollup_persist += other.addon_rollup_persist;
        self.repository_rollup += other.repository_rollup;
        self.repository_rollup_persist += other.repository_rollup_persist;
        self.clean_part_mark_files += other.clean_part_mark_files;
        self.clean_part_mark_parts += other.clean_part_mark_parts;
        self.fallback_part_update_parts += other.fallback_part_update_parts;
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FileHashBatchResult {
    pub(crate) requested_file_ids: HashSet<u64>,
    pub(crate) processed_file_ids: HashSet<u64>,
    pub(crate) updated_file_ids: HashSet<u64>,
    pub(crate) profile_decision: Option<super::scheduling::HashProfileDecision>,
    pub(crate) addon_metrics: Vec<AddonHashMetrics>,
    pub(crate) phase_timings: HashPhaseTimings,
}

impl FileHashBatchResult {
    pub(crate) fn processed(&self) -> bool {
        !self.processed_file_ids.is_empty()
    }
}

fn clean_part_mark_for_file(data_tree: &Tree, file_idx: usize) -> Option<CleanPartMark> {
    let file = data_tree.files.get(file_idx)?;
    let file_node = data_tree.file_nodes.get(file_idx)?;
    let parts: Vec<FoxyModFilePart> = file_node
        .parts
        .iter()
        .filter_map(|&part_idx| data_tree.parts.get(part_idx).cloned())
        .collect();
    if parts.is_empty() {
        return None;
    }

    let all_parts_verified_against_remote = parts.iter().all(|part| {
        part.id > 0
            && part.file_id == file.id
            && !part.remote_checksum.is_empty()
            && !part.local_checksum.is_empty()
            && part.local_checksum == part.remote_checksum
            && part.local_length == part.remote_length
            && part.local_start == part.remote_start
    });
    if !all_parts_verified_against_remote {
        return None;
    }

    local_file_matches_part_layout(&file.local_path, file.length, &parts).then_some(CleanPartMark {
        file_id: file.id,
        part_count: parts.len(),
    })
}

pub(crate) async fn calculate_hashes_for_files(
    context: Arc<FoxyContext>,
    repository_url: &str,
    file_ids: &HashSet<u64>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    force_rehash: bool,
) -> FileHashBatchResult {
    calculate_hashes_for_files_with_profile(
        context,
        repository_url,
        file_ids,
        progress_tx,
        force_rehash,
        HashIoProfilePreference::Auto,
    )
    .await
}

pub(crate) async fn calculate_hashes_for_files_with_profile(
    context: Arc<FoxyContext>,
    repository_url: &str,
    file_ids: &HashSet<u64>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    force_rehash: bool,
    hash_io_profile: HashIoProfilePreference,
) -> FileHashBatchResult {
    calculate_hashes_for_files_with_profile_and_sticky_auto(
        context,
        repository_url,
        file_ids,
        progress_tx,
        force_rehash,
        hash_io_profile,
        None,
        false,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn calculate_hashes_for_files_with_profile_and_sticky_auto(
    context: Arc<FoxyContext>,
    repository_url: &str,
    file_ids: &HashSet<u64>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    force_rehash: bool,
    hash_io_profile: HashIoProfilePreference,
    sticky_auto_profile: Option<HashIoProfilePreference>,
    freshly_downloaded_files: bool,
    clean_part_mark_downloaded_files: bool,
) -> FileHashBatchResult {
    let mut data_tree: Tree =
        match Tree::load_for_files(context.clone(), repository_url, file_ids).await {
            Ok(tree) => tree,
            Err(err) => {
                warn!("Failed to load tree for partial hash: {}", err);
                return FileHashBatchResult {
                    requested_file_ids: file_ids.clone(),
                    ..Default::default()
                };
            }
        };

    calculate_hashes_for_files_in_tree_with_profile_and_sticky_auto(
        context,
        &mut data_tree,
        file_ids,
        progress_tx,
        force_rehash,
        hash_io_profile,
        sticky_auto_profile,
        freshly_downloaded_files,
        clean_part_mark_downloaded_files,
    )
    .await
}

pub(crate) async fn calculate_hashes_for_files_in_tree_with_profile(
    context: Arc<FoxyContext>,
    data_tree: &mut Tree,
    file_ids: &HashSet<u64>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    force_rehash: bool,
    hash_io_profile: HashIoProfilePreference,
) -> FileHashBatchResult {
    calculate_hashes_for_files_in_tree_with_profile_and_sticky_auto(
        context,
        data_tree,
        file_ids,
        progress_tx,
        force_rehash,
        hash_io_profile,
        None,
        false,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn calculate_hashes_for_files_in_tree_with_profile_and_sticky_auto(
    context: Arc<FoxyContext>,
    data_tree: &mut Tree,
    file_ids: &HashSet<u64>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    force_rehash: bool,
    hash_io_profile: HashIoProfilePreference,
    sticky_auto_profile: Option<HashIoProfilePreference>,
    freshly_downloaded_files: bool,
    clean_part_mark_downloaded_files: bool,
) -> FileHashBatchResult {
    let requested_file_ids = file_ids.clone();
    if file_ids.is_empty() {
        return FileHashBatchResult::default();
    }
    let total_start = Instant::now();
    let mut phase_timings = HashPhaseTimings::default();

    let db = context.db();

    let mut file_indices = Vec::new();
    for file_id in file_ids {
        if let Some(&idx) = data_tree.file_id_to_index.get(file_id) {
            file_indices.push(idx);
        }
    }
    file_indices.sort_unstable();
    file_indices.dedup();
    if file_indices.is_empty() {
        return FileHashBatchResult {
            requested_file_ids,
            ..Default::default()
        };
    }
    if missing_local_hash_pass_is_noop(data_tree, &file_indices) {
        info!(
            "Partial hash pass not scheduled: all {} requested target files are missing locally and no local checksum state needs clearing",
            file_indices.len()
        );
        return FileHashBatchResult {
            requested_file_ids,
            ..Default::default()
        };
    }

    if !force_rehash {
        // Safety net: filter out files where all parts AND the file-level hash
        // already match their remote checksums, avoiding redundant disk I/O when
        // multiple code paths request hashing for the same files within a single
        // sync session.  We must check BOTH part-level and file-level checksums:
        // after a manifest update that changes the part layout (e.g. fewer parts),
        // all remaining parts may already match remote but the file-level hash
        // still needs recomputation from the new set of parts.
        let pre_filter_count = file_indices.len();
        file_indices.retain(|&file_idx| {
            let Some(file_node) = data_tree.file_nodes.get(file_idx) else {
                return true;
            };
            let has_parts = !file_node.parts.is_empty();
            let all_parts_match_remote = has_parts
                && file_node.parts.iter().all(|&part_idx| {
                    data_tree.parts.get(part_idx).is_some_and(|p| {
                        !p.local_checksum.is_empty() && p.local_checksum == p.remote_checksum
                    })
                });
            let file_hash_matches = data_tree.files.get(file_idx).is_some_and(|f| {
                !f.local_checksum.is_empty() && f.local_checksum == f.remote_checksum
            });
            !(all_parts_match_remote && file_hash_matches)
        });
        let skipped = pre_filter_count - file_indices.len();
        if skipped > 0 {
            info!(
                "Hash dedup safety net: skipped {} already-hashed files out of {} requested",
                skipped, pre_filter_count
            );
        }
        if file_indices.is_empty() {
            info!(
                "All {} requested files already have local checksums, nothing to hash",
                pre_filter_count
            );
            return FileHashBatchResult {
                requested_file_ids,
                ..Default::default()
            };
        }
    }
    let processed_file_ids: HashSet<u64> = file_indices
        .iter()
        .filter_map(|&file_idx| data_tree.files.get(file_idx).map(|file| file.id))
        .collect();

    let file_index_set: HashSet<usize> = file_indices.iter().copied().collect();
    let mut mod_indices: HashSet<usize> = HashSet::new();
    for (mod_idx, mod_node) in data_tree.mod_nodes.iter().enumerate() {
        if mod_node
            .files
            .iter()
            .any(|fidx| file_index_set.contains(fidx))
        {
            mod_indices.insert(mod_idx);
        }
    }
    let repo_indices = collect_repo_indices_for_mods(data_tree, &mod_indices);

    let mut updated_part_indices: HashSet<usize> = HashSet::new();
    let hash_jobs = build_file_hash_jobs(
        data_tree,
        &file_indices,
        if freshly_downloaded_files {
            PartSpanSource::RemoteLayout
        } else {
            PartSpanSource::DetectLocalLayout
        },
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
    let (hash_results, profile_decision, _cancelled) = recalculate_parts_for_jobs_with_profile(
        hash_jobs,
        hash_io_profile,
        sticky_auto_profile,
        progress_tx,
        total_files,
        None,
    )
    .await;
    phase_timings.hash_wall += hash_started.elapsed();
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
                "Partial hash pass skipped {} missing local files out of {} ({}%). Repository may not be fully downloaded yet.",
                missing_hash_files, total_files, missing_percent
            );
        } else {
            info!(
                "Partial hash pass skipped {} missing local files out of {} ({}%)",
                missing_hash_files, total_files, missing_percent
            );
        }
    }
    let addon_metrics = collect_addon_hash_metrics("partial_hash", data_tree, &hash_results);

    // --- Apply hash results to data_tree ---
    let apply_parts_started = Instant::now();
    for file_result in hash_results {
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
    phase_timings.apply_part_hashes += apply_parts_started.elapsed();

    // --- Phase 2: Persist part hashes (batched with progress) ---
    let (clean_part_marks, clean_part_indices): (Vec<CleanPartMark>, HashSet<usize>) =
        if clean_part_mark_downloaded_files && freshly_downloaded_files {
            let marks: Vec<CleanPartMark> = file_indices
                .iter()
                .filter_map(|&file_idx| clean_part_mark_for_file(data_tree, file_idx))
                .collect();
            let clean_file_ids: HashSet<u64> = marks.iter().map(|mark| mark.file_id).collect();
            let indices: HashSet<usize> = file_indices
                .iter()
                .filter_map(|&file_idx| data_tree.file_nodes.get(file_idx))
                .flat_map(|file_node| file_node.parts.iter().copied())
                .filter(|&part_idx| {
                    data_tree
                        .parts
                        .get(part_idx)
                        .is_some_and(|part| clean_file_ids.contains(&part.file_id))
                })
                .collect();
            (marks, indices)
        } else {
            (Vec::new(), HashSet::new())
        };
    let clean_part_count: usize = clean_part_marks.iter().map(|mark| mark.part_count).sum();
    let fallback_part_indices: HashSet<usize> = updated_part_indices
        .difference(&clean_part_indices)
        .copied()
        .collect();
    let fallback_part_count = fallback_part_indices.len();
    let part_count = clean_part_count.saturating_add(fallback_part_count);
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Saving parts 0/{}", part_count),
            percent: 0.50,
        });
    }
    let persist_started = Instant::now();
    let mut part_updates: Vec<_> = updated_part_indices
        .iter()
        .filter(|&&part_idx| fallback_part_indices.contains(&part_idx))
        .filter_map(|&part_idx| data_tree.parts.get(part_idx).cloned())
        .collect();
    // Sort by PK so the UPDATE walks the subfiles B-tree sequentially,
    // keeping pages in cache instead of thrashing on random access.
    part_updates.sort_by_key(|p| p.id);
    let mut parts_persisted = 0;
    // The metadata rebuild deferred this repository's brand-new part rows (fresh,
    // empty-`subfiles` load): they live only in the in-memory tree and were never
    // written to `subfiles`. Persist them here with the local hash state computed in
    // Phase 1, exactly like the full `calculate_hashes` pipeline does. Without this,
    // the targeted tree-hash init left the repository with files whose checksums are
    // populated but with zero part rows; the next quick scan reloads a part-less tree
    // and Phase 3 wipes every `local_checksum` (the `has_parts == false` branch),
    // falsely flagging every addon for re-download.
    //
    // Scope this to the post-rebuild bootstrap (`!freshly_downloaded_files`): the
    // bootstrap hashes the whole set of just-deferred files in one pass, so the tree
    // covers the entire deferred buffer that the flush drains. Per-batch incremental
    // download hashing covers only a subset of the deferred files at a time, so it
    // must keep the inline persist path and let the download flow drain the buffer.
    if context.deferred_part_count() > 0 && !freshly_downloaded_files {
        let total_deferred_parts = data_tree.parts.len();
        info!(
            "Persisting {} deferred manifest parts with local hash state in one coalesced sorted insert pass",
            total_deferred_parts
        );
        let mut next_progress_log = PERSIST_LOG_INTERVAL;
        if !flush_deferred_part_inserts_with_local_state(
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
            error!(
                "Failed to persist deferred manifest parts with local hash state during targeted tree-hash init; subfiles remain empty and a re-download may be falsely flagged"
            );
        }
    } else {
        if !clean_part_marks.is_empty() {
            info!(
                "Phase 2 derived clean part state: files={} parts={} fallback_parts={} db_part_updates=skipped",
                clean_part_marks.len(),
                clean_part_count,
                part_updates.len()
            );
        }
        if clean_part_count > 0 {
            parts_persisted += clean_part_count;
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
    let committed_clean_files = clean_part_marks.len();
    let committed_clean_parts = clean_part_count;
    info!(
        "Phase 2 (persist parts) completed in {:.2}s ({} parts, clean_mark_committed=derived, clean_mark_files={}, clean_mark_parts={}, fallback_parts={})",
        persist_started.elapsed().as_secs_f64(),
        part_count,
        committed_clean_files,
        committed_clean_parts,
        part_updates.len()
    );
    phase_timings.part_checksum_persist += persist_started.elapsed();
    phase_timings.clean_part_mark_files += committed_clean_files;
    phase_timings.clean_part_mark_parts += committed_clean_parts;
    phase_timings.fallback_part_update_parts += part_updates.len();

    // --- Phase 3: Update file hashes from parts ---
    let file_count = file_indices.len();
    let mut updated_file_ids: HashSet<u64> = HashSet::new();
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Updating files 0/{}", file_count),
            percent: 0.72,
        });
    }
    let file_rollup_started = Instant::now();
    for file_idx in &file_indices {
        if let Some(file) = data_tree.files.get_mut(*file_idx) {
            let old_checksum = file.local_checksum.clone();
            let file_node = &data_tree.file_nodes[*file_idx];
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
                file.local_checksum = String::new();
            } else if all_match {
                file.local_checksum = file.remote_checksum.clone();
            } else if parts_match && !layout_matches {
                file.local_checksum = String::new();
            } else {
                file.local_checksum = new_checksum.to_uppercase();
            }
            if file.local_checksum != old_checksum {
                updated_file_ids.insert(file.id);
            }
        }
    }
    info!(
        "Phase 3 (roll up file hashes from parts) completed in {:.3}s ({} files, {} changed)",
        file_rollup_started.elapsed().as_secs_f64(),
        file_count,
        updated_file_ids.len()
    );
    phase_timings.file_rollup += file_rollup_started.elapsed();

    let file_persist_started = Instant::now();
    let file_updates: Vec<_> = file_indices
        .iter()
        .filter_map(|&file_idx| data_tree.files.get(file_idx))
        .filter(|file| updated_file_ids.contains(&file.id))
        .cloned()
        .collect();
    let file_persist_count = file_updates.len();
    let mut files_persisted = 0;
    persist_file_checksums(&db, &file_updates, |persisted_chunk| {
        files_persisted += persisted_chunk;
        if files_persisted % PERSIST_LOG_INTERVAL == 0 || files_persisted == file_persist_count {
            info!(
                "Phase 3 progress: {}/{} files persisted",
                files_persisted, file_persist_count
            );
        }
        if let Some(tx) = progress_tx {
            let pct = 0.72 + 0.10 * (files_persisted as f32 / file_persist_count.max(1) as f32);
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Saving files {}/{}", files_persisted, file_persist_count),
                percent: pct,
            });
        }
    })
    .await;
    info!(
        "Phase 3 (persist files) completed in {:.2}s ({} files)",
        file_persist_started.elapsed().as_secs_f64(),
        file_persist_count
    );
    phase_timings.file_rollup_persist += file_persist_started.elapsed();

    // --- Phase 4: Update addon(mod) hashes from files ---
    let mod_count = mod_indices.len();
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Updating addons 0/{}", mod_count),
            percent: 0.84,
        });
    }
    let mod_rollup_started = Instant::now();
    let updated_mod_indices = update_mod_hashes_for_mods(data_tree, Some(&mod_indices));
    info!(
        "Phase 4 (roll up addon hashes from files) completed in {:.3}s ({} addons)",
        mod_rollup_started.elapsed().as_secs_f64(),
        mod_count
    );
    phase_timings.addon_rollup += mod_rollup_started.elapsed();
    let mod_persist_started = Instant::now();
    let mod_updates: Vec<_> = updated_mod_indices
        .iter()
        .filter_map(|&mod_idx| data_tree.mods.get(mod_idx).cloned())
        .collect();
    let mod_persist_count = mod_updates.len();
    let mut mods_persisted = 0;
    persist_mod_checksums(&db, &mod_updates, |persisted_chunk| {
        mods_persisted += persisted_chunk;
        if mods_persisted % 500 == 0 || mods_persisted == mod_persist_count {
            info!(
                "Phase 4 progress: {}/{} mods persisted",
                mods_persisted, mod_persist_count
            );
        }
        if let Some(tx) = progress_tx {
            let pct = 0.84 + 0.06 * (mods_persisted as f32 / mod_persist_count.max(1) as f32);
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Saving addons {}/{}", mods_persisted, mod_persist_count),
                percent: pct,
            });
        }
    })
    .await;
    info!(
        "Phase 4 (persist mods) completed in {:.2}s ({} mods)",
        mod_persist_started.elapsed().as_secs_f64(),
        mod_persist_count
    );
    phase_timings.addon_rollup_persist += mod_persist_started.elapsed();

    // --- Phase 5: Update repository hashes based on addons(mods) ---
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: format!("Updating repositories 0/{}", repo_indices.len()),
            percent: 0.92,
        });
    }
    let repo_rollup_started = Instant::now();
    update_repository_hashes_for_repos(data_tree, &repo_indices);
    info!(
        "Phase 5 (roll up repository hashes from addons) completed in {:.3}s ({} repositories)",
        repo_rollup_started.elapsed().as_secs_f64(),
        repo_indices.len()
    );
    phase_timings.repository_rollup += repo_rollup_started.elapsed();
    let repo_persist_started = Instant::now();
    let repo_updates: Vec<_> = repo_indices
        .iter()
        .filter_map(|&repo_idx| data_tree.repositories.get(repo_idx).cloned())
        .collect();
    let repo_count = repo_updates.len();
    let mut repos_persisted = 0usize;
    persist_repository_checksums(&db, &repo_updates, |persisted_chunk| {
        repos_persisted += persisted_chunk;
        if repos_persisted == repo_count || repos_persisted.is_multiple_of(PERSIST_LOG_INTERVAL) {
            info!(
                "Phase 5 progress: {}/{} repositories persisted",
                repos_persisted, repo_count
            );
        }
        if let Some(tx) = progress_tx {
            let pct = 0.92 + 0.06 * (repos_persisted as f32 / repo_count.max(1) as f32);
            let _ = tx.send(ProgressEvent::Stage {
                label: format!("Saving repositories {}/{}", repos_persisted, repo_count),
                percent: pct,
            });
        }
    })
    .await;
    info!(
        "Phase 5 (persist repos) completed in {:.2}s ({} repositories)",
        repo_persist_started.elapsed().as_secs_f64(),
        repo_count
    );
    phase_timings.repository_rollup_persist += repo_persist_started.elapsed();
    info!(
        "Total partial hash recalculation completed in {:.2}s",
        total_start.elapsed().as_secs_f64()
    );

    FileHashBatchResult {
        requested_file_ids,
        processed_file_ids,
        updated_file_ids,
        profile_decision: Some(profile_decision),
        addon_metrics,
        phase_timings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::model_tree::FileNode;
    use std::io::Write;

    fn tree_with_one_file(local_path: String, parts: Vec<FoxyModFilePart>) -> Tree {
        let part_indices: Vec<usize> = (0..parts.len()).collect();
        Tree {
            files: vec![FoxyModFile {
                id: 10,
                local_path,
                length: 4,
                ..Default::default()
            }],
            parts,
            file_nodes: vec![FileNode {
                file_idx: 0,
                parts: part_indices,
            }],
            file_id_to_index: HashMap::from([(10, 0)]),
            ..Default::default()
        }
    }

    fn verified_part() -> FoxyModFilePart {
        FoxyModFilePart {
            id: 1,
            file_id: 10,
            remote_length: 4,
            local_length: 4,
            remote_start: 0,
            local_start: 0,
            remote_checksum: "ABCD".to_string(),
            local_checksum: "ABCD".to_string(),
            data_order: 0,
            ..Default::default()
        }
    }

    #[test]
    fn clean_part_mark_requires_verified_remote_layout() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"test").unwrap();
        let tree = tree_with_one_file(
            file.path().to_string_lossy().into_owned(),
            vec![verified_part()],
        );

        let mark = clean_part_mark_for_file(&tree, 0).expect("clean file should be markable");
        assert_eq!(mark.file_id, 10);
        assert_eq!(mark.part_count, 1);
    }

    #[test]
    fn clean_part_mark_rejects_checksum_mismatch() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"test").unwrap();
        let mut part = verified_part();
        part.local_checksum = "DIFFERENT".to_string();
        let tree = tree_with_one_file(file.path().to_string_lossy().into_owned(), vec![part]);

        assert!(clean_part_mark_for_file(&tree, 0).is_none());
    }

    #[test]
    fn clean_part_mark_rejects_local_span_mismatch() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"test").unwrap();
        let mut part = verified_part();
        part.local_start = 1;
        let tree = tree_with_one_file(file.path().to_string_lossy().into_owned(), vec![part]);

        assert!(clean_part_mark_for_file(&tree, 0).is_none());
    }
}
