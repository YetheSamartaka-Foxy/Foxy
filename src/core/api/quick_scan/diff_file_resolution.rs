use super::super::fs_watcher::normalize_path_for_match;
use super::super::*;
use super::db_helpers::{
    PartChangeStats, load_changed_part_stats_by_file_ids, load_patch_download_bytes_by_file_ids,
    refresh_files_by_ids,
};
use super::diff_addon_hash::AddonHashResult;
use super::file_state::{LocalFileState, resolve_local_file_state};
use super::shared_cache::QuickScanSharedCache;
use super::unexpected_files::collect_unexpected_local_files_for_mod;
use crate::core::db::DbValue;

pub(super) struct DiffComputeResult {
    pub diffs: Vec<ModDiffSummary>,
    pub files_needing_tree_verify: HashSet<u64>,
    pub clean_hash_updates: Vec<FoxyMod>,
    pub addons_needing_tree_hash: Vec<String>,
    pub addons_content_mismatch: Vec<String>,
    pub addons_with_updates: usize,
    pub file_fallback_elapsed: Duration,
    pub tree_part_stats_load_elapsed: Duration,
    pub deep_scan_files_total: usize,
    pub checksum_mismatch_files: usize,
    pub missing_files: usize,
    pub size_mismatch_files: usize,
    pub content_mismatch_files: usize,
    pub unexpected_files: usize,
    pub addons_with_unexpected_files: usize,
}

fn inferred_patch_bytes_from_part_stats(
    stats: PartChangeStats,
    file_length: u64,
    file_tree_mismatch: bool,
) -> u64 {
    if stats.total_parts == 0 {
        return file_length;
    }

    let known_delta_bytes = stats
        .changed_bytes
        .saturating_add(stats.missing_bytes)
        .min(file_length);

    if file_tree_mismatch {
        return known_delta_bytes;
    }

    if stats.changed_parts > 0 || stats.missing_local_checksums > 0 {
        known_delta_bytes
    } else {
        file_length
    }
}

fn addon_needs_update_from_file_diff(has_expected_file_diffs: bool) -> bool {
    has_expected_file_diffs
}

fn is_stale_tree_only_file_diff(
    exists: bool,
    size_ok: bool,
    file_content_mismatch: bool,
    file_tree_mismatch: bool,
    planned_patch_bytes: u64,
    file_length: u64,
) -> bool {
    exists
        && size_ok
        && !file_content_mismatch
        && file_tree_mismatch
        && planned_patch_bytes == 0
        && file_length > 0
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn compute_file_diffs(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mods: &[FoxyMod],
    mod_enabled_overrides: Option<&HashMap<String, bool>>,
    addon_hash: &AddonHashResult,
    progress_tx: Option<&Sender<ProgressEvent>>,
    shared_cache: Option<&Arc<Mutex<QuickScanSharedCache>>>,
) -> Option<DiffComputeResult> {
    let db = context.db();
    let chunk_size = read_chunk_ids();

    // Determine which mods need file metadata
    let mut mods_requiring_file_metadata: HashSet<i64> = HashSet::new();
    mods_requiring_file_metadata.extend(addon_hash.deep_scan_mod_ids.iter().copied());
    mods_requiring_file_metadata.extend(addon_hash.mods_with_tree_mismatch.iter().copied());
    mods_requiring_file_metadata.extend(addon_hash.mods_with_missing_path.iter().copied());

    let mut required_mod_ids: Vec<i64> = mods_requiring_file_metadata.into_iter().collect();
    required_mod_ids.sort_unstable();
    required_mod_ids.dedup();

    let mut mod_file_links: Vec<(i64, i64)> = Vec::new();
    let mut idx = 0usize;
    while idx < required_mod_ids.len() {
        let end = (idx + chunk_size).min(required_mod_ids.len());
        let chunk = &required_mod_ids[idx..end];
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
                    "Failed to load mod files for quick scan {}: {}",
                    repo_url, err
                );
                return None;
            }
        }
        idx = end;
    }

    let mut file_ids: Vec<i64> = mod_file_links.iter().map(|(_, file_id)| *file_id).collect();
    file_ids.sort_unstable();
    file_ids.dedup();

    let files_by_id = if file_ids.is_empty() {
        HashMap::new()
    } else {
        match refresh_files_by_ids(&db, &file_ids, chunk_size).await {
            Some(map) => map,
            None => return None,
        }
    };
    let build_files_by_mod = |links: &[(i64, i64)],
                              file_map: &HashMap<i64, FoxyModFile>|
     -> HashMap<i64, Vec<FoxyModFile>> {
        let mut files_by_mod: HashMap<i64, Vec<FoxyModFile>> = HashMap::new();
        for (addon_id, file_id) in links {
            if let Some(file) = file_map.get(file_id).cloned() {
                files_by_mod.entry(*addon_id).or_default().push(file);
            }
        }
        for files in files_by_mod.values_mut() {
            files.sort_by_key(|f| f.data_order);
        }
        files_by_mod
    };
    let files_by_mod = build_files_by_mod(&mod_file_links, &files_by_id);
    let mut deep_scan_mod_ids = addon_hash.deep_scan_mod_ids.clone();
    deep_scan_mod_ids.retain(|mod_id| {
        files_by_mod
            .get(mod_id)
            .map(|entries| !entries.is_empty())
            .unwrap_or(false)
    });

    let deep_scan_files_total: usize = deep_scan_mod_ids
        .iter()
        .map(|mod_id| {
            files_by_mod
                .get(mod_id)
                .map(|entries| entries.len())
                .unwrap_or(0)
        })
        .sum();

    let mut stats_file_ids: Vec<i64> = Vec::new();
    for (mod_id, files) in &files_by_mod {
        let needs_deep_stats = deep_scan_mod_ids.contains(mod_id);
        for file in files {
            if needs_deep_stats || file.local_checksum != file.remote_checksum {
                stats_file_ids.push(file.id as i64);
            }
        }
    }
    stats_file_ids.sort_unstable();
    stats_file_ids.dedup();

    let patch_download_bytes_by_file_id = if stats_file_ids.is_empty() {
        HashMap::new()
    } else {
        load_patch_download_bytes_by_file_ids(&db, &stats_file_ids, chunk_size).await
    };
    let tree_part_stats_started = Instant::now();
    let changed_part_stats_by_file_id = if stats_file_ids.is_empty() {
        HashMap::new()
    } else {
        load_changed_part_stats_by_file_ids(&db, &stats_file_ids, chunk_size).await
    };
    let tree_part_stats_load_elapsed = tree_part_stats_started.elapsed();

    let mut local_file_state_cache: HashMap<String, LocalFileState> = HashMap::new();
    let mut checked_files = 0usize;
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::RecheckHashProgress {
            checked_files,
            total_files: deep_scan_files_total,
            checked_parts: checked_files,
            total_parts: deep_scan_files_total,
        });
    }

    let mut diffs = Vec::new();
    let mut checksum_mismatch_files = 0usize;
    let mut missing_files = 0usize;
    let mut size_mismatch_files = 0usize;
    let mut content_mismatch_files = 0usize;
    let mut unexpected_files = 0usize;
    let mut addons_with_unexpected_files = 0usize;
    let mut addons_content_mismatch = Vec::new();
    let mut addons_needing_tree_hash = Vec::new();
    let mut addons_with_updates = 0usize;
    let mut files_needing_tree_verify: HashSet<u64> = HashSet::new();
    let mut clean_hash_updates: Vec<FoxyMod> = Vec::new();
    let mut file_fallback_elapsed = Duration::default();

    for m in mods {
        let is_enabled = mod_enabled_overrides
            .and_then(|overrides| overrides.get(&m.name.to_lowercase()).copied())
            .unwrap_or(m.enabled);
        if !is_enabled {
            continue;
        }

        let mod_id = m.id as i64;
        let mut files = files_by_mod.get(&mod_id).cloned().unwrap_or_default();
        let addon_state = addon_hash
            .addon_state_by_mod_id
            .get(&mod_id)
            .cloned()
            .unwrap_or_default();
        let addon_current_content_hash = addon_state.content_hash.clone();
        let addon_content_mismatch =
            m.local_content_hash.is_empty() || m.local_content_hash != addon_current_content_hash;
        if addon_content_mismatch {
            addons_content_mismatch.push(format!(
                "{}(stored={} current={})",
                m.name, m.local_content_hash, addon_current_content_hash
            ));
        }

        let mod_path = m.local_path.trim();
        let mod_missing = mod_path.is_empty() || !addon_state.exists;
        let addon_tree_mismatch = m.local_checksum != m.remote_checksum;

        if files.is_empty() {
            if mod_missing {
                diffs.push(ModDiffSummary {
                    name: m.name.clone(),
                    needs_update: true,
                    total_bytes: 0,
                    files: Vec::new(),
                });
                addons_with_updates += 1;
            }
            continue;
        }

        let expected_bytes: u64 = files.iter().map(|f| f.length).sum();

        if mod_missing {
            let mod_files: Vec<FileDiffSummary> = files
                .iter()
                .map(|f| FileDiffSummary {
                    name: f.name.clone(),
                    needs_update: true,
                    total_bytes: f.length,
                    changed_parts: 0,
                })
                .collect();
            diffs.push(ModDiffSummary {
                name: m.name.clone(),
                needs_update: true,
                total_bytes: expected_bytes,
                files: mod_files,
            });
            addons_with_updates += 1;
            missing_files += files.len();
            continue;
        }

        let deep_scan_addon = deep_scan_mod_ids.contains(&mod_id);
        let mut mod_files = Vec::new();
        let mut total_bytes = 0u64;

        if deep_scan_addon {
            let fallback_addon_started = Instant::now();
            for f in files.drain(..) {
                let file_state = resolve_local_file_state(
                    &mut local_file_state_cache,
                    shared_cache,
                    &f.local_path,
                    f.length,
                )
                .await;
                let exists = file_state.exists;
                let size_ok = exists && file_state.length == f.length;
                let content_hash = if size_ok {
                    file_state.content_hash.clone()
                } else {
                    String::new()
                };
                checked_files += 1;
                if let Some(tx) = progress_tx {
                    let _ = tx.send(ProgressEvent::RecheckHashProgress {
                        checked_files,
                        total_files: deep_scan_files_total,
                        checked_parts: checked_files,
                        total_parts: deep_scan_files_total,
                    });
                }
                let file_content_mismatch =
                    f.local_content_hash.is_empty() || f.local_content_hash != content_hash;
                if file_content_mismatch {
                    content_mismatch_files += 1;
                }

                if !exists {
                    missing_files += 1;
                } else if !size_ok {
                    size_mismatch_files += 1;
                }

                let file_tree_mismatch = f.local_checksum != f.remote_checksum;
                let mismatch = !exists || !size_ok || file_tree_mismatch || file_content_mismatch;

                if mismatch {
                    debug!(
                        "File mismatch detected during quick scan: repo={} addon={} file={} local_tree_checksum={} remote_tree_checksum={} local_content_hash={} current_content_hash={} exists={} size_ok={} bytes={}",
                        repo_url,
                        m.name,
                        f.name,
                        f.local_checksum,
                        f.remote_checksum,
                        f.local_content_hash,
                        content_hash,
                        exists,
                        size_ok,
                        f.length
                    );
                    if file_tree_mismatch {
                        checksum_mismatch_files += 1;
                    }
                    if exists && (file_content_mismatch || file_tree_mismatch) {
                        files_needing_tree_verify.insert(f.id);
                    }
                    let part_stats = changed_part_stats_by_file_id
                        .get(&(f.id as i64))
                        .copied()
                        .unwrap_or_default();
                    let inferred_patch_bytes = inferred_patch_bytes_from_part_stats(
                        part_stats,
                        f.length,
                        file_tree_mismatch,
                    );
                    let patch_hint = patch_download_bytes_by_file_id.get(&(f.id as i64)).copied();
                    let planned_patch_bytes = if exists {
                        patch_hint.unwrap_or(inferred_patch_bytes).min(f.length)
                    } else {
                        f.length
                    };
                    if is_stale_tree_only_file_diff(
                        exists,
                        size_ok,
                        file_content_mismatch,
                        file_tree_mismatch,
                        planned_patch_bytes,
                        f.length,
                    ) {
                        debug!(
                            "Skipping stale tree-only file diff with no planned transfer: repo={} addon={} file={} local_tree_checksum={} remote_tree_checksum={}",
                            repo_url, m.name, f.name, f.local_checksum, f.remote_checksum
                        );
                        continue;
                    }
                    if patch_hint.is_none() && inferred_patch_bytes < f.length {
                        debug!(
                            "Quick scan inferred delta-size from part checksums: repo={} addon={} file={} changed_parts={} changed_bytes={} full_bytes={}",
                            repo_url,
                            m.name,
                            f.name,
                            part_stats.changed_parts,
                            inferred_patch_bytes,
                            f.length
                        );
                    }
                    if planned_patch_bytes < f.length {
                        debug!(
                            "Quick scan delta-size hint: repo={} addon={} file={} planned_bytes={} full_bytes={}",
                            repo_url, m.name, f.name, planned_patch_bytes, f.length
                        );
                    }
                    total_bytes += planned_patch_bytes;
                    mod_files.push(FileDiffSummary {
                        name: f.name.clone(),
                        needs_update: true,
                        total_bytes: planned_patch_bytes,
                        changed_parts: part_stats.changed_parts,
                    });
                }
            }
            file_fallback_elapsed += fallback_addon_started.elapsed();
        } else if addon_tree_mismatch {
            for f in files.drain(..) {
                let file_tree_mismatch = f.local_checksum != f.remote_checksum;
                if !file_tree_mismatch {
                    continue;
                }

                checksum_mismatch_files += 1;
                let part_stats = changed_part_stats_by_file_id
                    .get(&(f.id as i64))
                    .copied()
                    .unwrap_or_default();
                let inferred_patch_bytes =
                    inferred_patch_bytes_from_part_stats(part_stats, f.length, file_tree_mismatch);
                let patch_hint = patch_download_bytes_by_file_id.get(&(f.id as i64)).copied();
                let planned_patch_bytes = patch_hint.unwrap_or(inferred_patch_bytes).min(f.length);
                if planned_patch_bytes == 0 && f.length > 0 {
                    debug!(
                        "Skipping tree-only file diff with no planned transfer: repo={} addon={} file={} local_tree_checksum={} remote_tree_checksum={}",
                        repo_url, m.name, f.name, f.local_checksum, f.remote_checksum
                    );
                    continue;
                }

                if patch_hint.is_none() && inferred_patch_bytes < f.length {
                    debug!(
                        "Quick scan inferred delta-size from part checksums: repo={} addon={} file={} changed_parts={} changed_bytes={} full_bytes={}",
                        repo_url,
                        m.name,
                        f.name,
                        part_stats.changed_parts,
                        inferred_patch_bytes,
                        f.length
                    );
                }
                if planned_patch_bytes < f.length {
                    debug!(
                        "Quick scan delta-size hint: repo={} addon={} file={} planned_bytes={} full_bytes={}",
                        repo_url, m.name, f.name, planned_patch_bytes, f.length
                    );
                }

                total_bytes += planned_patch_bytes;
                mod_files.push(FileDiffSummary {
                    name: f.name.clone(),
                    needs_update: true,
                    total_bytes: planned_patch_bytes,
                    changed_parts: part_stats.changed_parts,
                });
            }
        }

        let unexpected_local_files =
            if addon_content_mismatch && mod_files.is_empty() && !addon_tree_mismatch {
                let expected_local_paths: HashSet<String> = files_by_mod
                    .get(&mod_id)
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|f| normalize_path_for_match(&f.local_path))
                            .collect()
                    })
                    .unwrap_or_default();
                if expected_local_paths.is_empty() {
                    Vec::new()
                } else {
                    collect_unexpected_local_files_for_mod(mod_path, &expected_local_paths)
                }
            } else {
                Vec::new()
            };
        if !unexpected_local_files.is_empty() {
            unexpected_files += unexpected_local_files.len();
            addons_with_unexpected_files += 1;
            info!(
                "Quick scan found {} unexpected local files for repo={} addon={}",
                unexpected_local_files.len(),
                repo_url,
                m.name
            );
        }

        let addon_needs_update = addon_needs_update_from_file_diff(!mod_files.is_empty());
        if addon_needs_update && total_bytes == 0 && addon_tree_mismatch {
            total_bytes = expected_bytes;
        }

        if addon_needs_update {
            if addon_content_mismatch {
                addons_needing_tree_hash.push(m.name.clone());
            }
            debug!(
                "Addon mismatch detected during quick scan: repo={} addon={} local_tree_checksum={} remote_tree_checksum={} stored_content_hash={} current_content_hash={} mismatched_files={} unexpected_files={} expected_bytes={} deep_scan={}",
                repo_url,
                m.name,
                m.local_checksum,
                m.remote_checksum,
                m.local_content_hash,
                addon_current_content_hash,
                mod_files.len(),
                unexpected_local_files.len(),
                expected_bytes,
                deep_scan_addon
            );
            diffs.push(ModDiffSummary {
                name: m.name.clone(),
                needs_update: true,
                total_bytes,
                files: mod_files,
            });
            addons_with_updates += 1;
        } else if (addon_content_mismatch && !addon_current_content_hash.is_empty())
            || addon_tree_mismatch
        {
            let mut updated = m.clone();
            if addon_content_mismatch && !addon_current_content_hash.is_empty() {
                updated.local_content_hash = addon_current_content_hash.clone();
            }
            if addon_tree_mismatch {
                updated.local_checksum = updated.remote_checksum.clone();
            }
            clean_hash_updates.push(updated);
        }
    }

    Some(DiffComputeResult {
        diffs,
        files_needing_tree_verify,
        clean_hash_updates,
        addons_needing_tree_hash,
        addons_content_mismatch,
        addons_with_updates,
        file_fallback_elapsed,
        tree_part_stats_load_elapsed,
        deep_scan_files_total,
        checksum_mismatch_files,
        missing_files,
        size_mismatch_files,
        content_mismatch_files,
        unexpected_files,
        addons_with_unexpected_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inferred_patch_bytes_uses_zero_when_tree_mismatch_has_complete_matching_parts() {
        let stats = PartChangeStats {
            changed_parts: 0,
            changed_bytes: 0,
            missing_bytes: 0,
            total_parts: 3,
            missing_local_checksums: 0,
        };

        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, true), 0);
    }

    #[test]
    fn inferred_patch_bytes_uses_full_size_when_tree_state_is_stale() {
        let stats = PartChangeStats {
            changed_parts: 0,
            changed_bytes: 0,
            missing_bytes: 0,
            total_parts: 3,
            missing_local_checksums: 0,
        };

        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, false), 100);
    }

    #[test]
    fn inferred_patch_bytes_uses_missing_bytes_for_tree_mismatch() {
        let stats = PartChangeStats {
            changed_parts: 0,
            changed_bytes: 0,
            missing_bytes: 10,
            total_parts: 3,
            missing_local_checksums: 1,
        };

        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, true), 10);
    }

    #[test]
    fn inferred_patch_bytes_includes_missing_part_bytes() {
        let stats = PartChangeStats {
            changed_parts: 1,
            changed_bytes: 25,
            missing_bytes: 10,
            total_parts: 3,
            missing_local_checksums: 1,
        };

        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, true), 35);
    }

    #[test]
    fn inferred_patch_bytes_uses_changed_bytes_for_known_changed_parts() {
        let stats = PartChangeStats {
            changed_parts: 2,
            changed_bytes: 25,
            missing_bytes: 0,
            total_parts: 4,
            missing_local_checksums: 0,
        };

        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, true), 25);
    }

    #[test]
    fn addon_update_decision_ignores_unexpected_files_without_expected_diffs() {
        assert!(!addon_needs_update_from_file_diff(false));
    }

    #[test]
    fn addon_update_decision_keeps_expected_file_diffs() {
        assert!(addon_needs_update_from_file_diff(true));
    }

    #[test]
    fn stale_tree_only_file_diff_requires_no_transfer() {
        assert!(is_stale_tree_only_file_diff(
            true, true, false, true, 0, 100
        ));
        assert!(!is_stale_tree_only_file_diff(
            true, true, true, true, 0, 100
        ));
        assert!(!is_stale_tree_only_file_diff(
            true, true, false, true, 25, 100
        ));
        assert!(!is_stale_tree_only_file_diff(true, true, false, true, 0, 0));
    }

    // ── inferred_patch_bytes_from_part_stats: additional ────────────────

    #[test]
    fn inferred_patch_bytes_no_parts_returns_full_size() {
        let stats = PartChangeStats {
            changed_parts: 0,
            changed_bytes: 0,
            missing_bytes: 0,
            total_parts: 0,
            missing_local_checksums: 0,
        };
        // With no recorded parts we cannot reason about a delta, so plan for the
        // full file in both tree-mismatch and non-mismatch cases.
        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 500, true), 500);
        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 500, false), 500);
    }

    #[test]
    fn inferred_patch_bytes_caps_known_delta_at_file_length() {
        let stats = PartChangeStats {
            changed_parts: 4,
            changed_bytes: 300,
            missing_bytes: 300,
            total_parts: 4,
            missing_local_checksums: 0,
        };
        // changed + missing = 600 but the file is only 500 bytes.
        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 500, true), 500);
    }

    #[test]
    fn inferred_patch_bytes_missing_local_checksum_only_uses_delta() {
        let stats = PartChangeStats {
            changed_parts: 0,
            changed_bytes: 0,
            missing_bytes: 40,
            total_parts: 5,
            missing_local_checksums: 1,
        };
        // No tree mismatch, but missing_local_checksums > 0 still implies a delta.
        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, false), 40);
    }

    #[test]
    fn inferred_patch_bytes_clean_parts_no_mismatch_uses_full_size() {
        let stats = PartChangeStats {
            changed_parts: 0,
            changed_bytes: 0,
            missing_bytes: 0,
            total_parts: 5,
            missing_local_checksums: 0,
        };
        assert_eq!(inferred_patch_bytes_from_part_stats(stats, 100, false), 100);
    }

    // ── addon_needs_update_from_file_diff: full truth table ─────────────

    #[test]
    fn addon_update_decision_full_truth_table() {
        assert!(!addon_needs_update_from_file_diff(false));
        assert!(addon_needs_update_from_file_diff(true));
    }

    // ── is_stale_tree_only_file_diff: additional ────────────────────────

    #[test]
    fn stale_tree_only_false_when_file_missing() {
        assert!(!is_stale_tree_only_file_diff(
            false, false, false, true, 0, 100
        ));
    }

    #[test]
    fn stale_tree_only_false_when_size_mismatch() {
        assert!(!is_stale_tree_only_file_diff(
            true, false, false, true, 0, 100
        ));
    }

    #[test]
    fn stale_tree_only_false_without_tree_mismatch() {
        assert!(!is_stale_tree_only_file_diff(
            true, true, false, false, 0, 100
        ));
    }
}
