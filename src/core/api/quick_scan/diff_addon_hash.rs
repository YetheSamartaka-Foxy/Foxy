use super::super::fs_watcher::normalize_path_for_match;
use super::super::*;
use super::file_state::{AddonFolderState, probe_addon_folder_state};
use super::persistent_cache::{
    AddonRootFingerprint, PersistentAddonHashEntry, addon_root_fingerprint, now_unix_ms,
    persistent_addon_fingerprint_is_current, persistent_addon_fingerprint_matches,
    persistent_addon_fingerprint_mismatch_reasons,
};
use super::shared_cache::QuickScanSharedCache;
use crate::core::utils::format::sanitize_log_path_str;
use log::{debug, info};

const MISSING_ADDON_PATH_SAMPLE_LIMIT: usize = 8;

#[derive(Clone)]
struct AddonHashWork {
    path_key: String,
    local_path: String,
    fingerprint: AddonRootFingerprint,
}

pub(super) struct AddonHashResult {
    pub addon_state_by_mod_id: HashMap<i64, AddonFolderState>,
    pub deep_scan_mod_ids: HashSet<i64>,
    pub mods_with_tree_mismatch: HashSet<i64>,
    pub mods_with_missing_path: HashSet<i64>,
    pub addon_hash_timings: Vec<(String, Duration, &'static str)>,
    pub addon_hash_elapsed: Duration,
    pub addon_hash_concurrency: usize,
    pub addon_hash_hits_shared_memory: usize,
    pub addon_hash_hits_persistent: usize,
    pub addon_hash_calculated: usize,
    pub persistent_cache_entry_count: usize,
    pub enabled_addons: usize,
    pub phase1_addon_content_mismatch_count: usize,
    pub missing_addon_path_samples: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn resolve_addon_hashes(
    mods: &[FoxyMod],
    mod_enabled_overrides: Option<&HashMap<String, bool>>,
    force_fresh_addon_hash: bool,
    shared_cache: Option<&Arc<Mutex<QuickScanSharedCache>>>,
) -> AddonHashResult {
    let addon_hash_stage_started = Instant::now();
    let mut addon_hash_timings: Vec<(String, Duration, &'static str)> = Vec::new();
    let mut addon_hash_hits_shared_memory = 0usize;
    let mut addon_hash_hits_persistent = 0usize;
    let mut addon_hash_calculated = 0usize;
    let persistent_cache_entry_count = if let Some(shared) = shared_cache {
        match shared.lock() {
            Ok(guard) => guard.persistent_addon_hash_by_path.len(),
            Err(poisoned) => poisoned.into_inner().persistent_addon_hash_by_path.len(),
        }
    } else {
        0
    };

    info!(
        "Addon hash resolution started: mods={} force_fresh={} persistent_cache_entries={}",
        mods.len(),
        force_fresh_addon_hash,
        persistent_cache_entry_count
    );

    let mut local_addon_state_cache: HashMap<String, AddonFolderState> = HashMap::new();
    let mut mod_path_key_by_id: HashMap<i64, String> = HashMap::new();
    let mut enabled_addons = 0usize;
    let mut missing_addon_path_samples = Vec::new();
    let mut unresolved_addon_hash_work: Vec<AddonHashWork> = Vec::new();
    let mut unresolved_seen_path_keys: HashSet<String> = HashSet::new();
    let mut persistent_miss_debug_samples = 0usize;

    for m in mods {
        let is_enabled = mod_enabled_overrides
            .and_then(|overrides| overrides.get(&m.name.to_lowercase()).copied())
            .unwrap_or(m.enabled);
        if !is_enabled {
            continue;
        }
        enabled_addons += 1;

        let mod_id = m.id as i64;
        let path_key = normalize_path_for_match(&m.local_path);
        mod_path_key_by_id.insert(mod_id, path_key.clone());

        if local_addon_state_cache.contains_key(&path_key) {
            continue;
        }

        let started = Instant::now();
        let mut state_from_cache: Option<AddonFolderState> = None;
        if !force_fresh_addon_hash && let Some(shared) = shared_cache {
            let cached = match shared.lock() {
                Ok(guard) => guard.addon_state_by_path.get(&path_key).cloned(),
                Err(poisoned) => poisoned
                    .into_inner()
                    .addon_state_by_path
                    .get(&path_key)
                    .cloned(),
            };
            if let Some(state) = cached {
                addon_hash_hits_shared_memory += 1;
                state_from_cache = Some(state);
                addon_hash_timings.push((path_key.clone(), started.elapsed(), "shared_memory"));
            }
        }
        if let Some(state) = state_from_cache {
            local_addon_state_cache.insert(path_key, state);
            continue;
        }

        let fingerprint = addon_root_fingerprint(&m.local_path);
        if !fingerprint.exists || !fingerprint.is_dir {
            if missing_addon_path_samples.len() < MISSING_ADDON_PATH_SAMPLE_LIMIT {
                missing_addon_path_samples.push(m.local_path.clone());
            }
            let missing_state = AddonFolderState::default();
            local_addon_state_cache.insert(path_key.clone(), missing_state.clone());
            if let Some(shared) = shared_cache {
                match shared.lock() {
                    Ok(mut guard) => {
                        guard
                            .addon_state_by_path
                            .insert(path_key.clone(), missing_state.clone());
                    }
                    Err(poisoned) => {
                        let mut guard = poisoned.into_inner();
                        guard
                            .addon_state_by_path
                            .insert(path_key.clone(), missing_state.clone());
                    }
                }
            }
            addon_hash_timings.push((path_key, started.elapsed(), "missing_or_not_dir"));
            continue;
        }

        let mut persistent_hit = false;
        if !force_fresh_addon_hash && let Some(shared) = shared_cache {
            let cached = match shared.lock() {
                Ok(mut guard) => {
                    let existing_entry =
                        guard.persistent_addon_hash_by_path.get(&path_key).cloned();
                    if let Some(entry) = existing_entry.as_ref()
                        && !persistent_addon_fingerprint_matches(entry, &fingerprint)
                        && persistent_miss_debug_samples < 8
                    {
                        persistent_miss_debug_samples += 1;
                        let reasons =
                            persistent_addon_fingerprint_mismatch_reasons(entry, &fingerprint);
                        debug!(
                            "Persistent addon hash cache miss: path={} reasons={}",
                            sanitize_log_path_str(&path_key),
                            reasons.join(",")
                        );
                    }
                    let cached_entry = existing_entry
                        .filter(|entry| persistent_addon_fingerprint_matches(entry, &fingerprint));
                    if let Some(entry) = cached_entry {
                        let state = AddonFolderState {
                            exists: true,
                            content_hash: entry.content_hash.clone(),
                        };
                        if !persistent_addon_fingerprint_is_current(
                            &entry.fingerprint,
                            &fingerprint,
                        ) {
                            guard.persistent_addon_hash_by_path.insert(
                                path_key.clone(),
                                PersistentAddonHashEntry {
                                    fingerprint: fingerprint.clone(),
                                    content_hash: entry.content_hash.clone(),
                                    updated_unix_ms: now_unix_ms(),
                                },
                            );
                            guard.persistent_dirty = true;
                        }
                        guard
                            .addon_state_by_path
                            .insert(path_key.clone(), state.clone());
                        Some(state)
                    } else {
                        None
                    }
                }
                Err(poisoned) => {
                    let mut guard = poisoned.into_inner();
                    let existing_entry =
                        guard.persistent_addon_hash_by_path.get(&path_key).cloned();
                    if let Some(entry) = existing_entry.as_ref()
                        && !persistent_addon_fingerprint_matches(entry, &fingerprint)
                        && persistent_miss_debug_samples < 8
                    {
                        persistent_miss_debug_samples += 1;
                        let reasons =
                            persistent_addon_fingerprint_mismatch_reasons(entry, &fingerprint);
                        debug!(
                            "Persistent addon hash cache miss: path={} reasons={}",
                            sanitize_log_path_str(&path_key),
                            reasons.join(",")
                        );
                    }
                    let cached_entry = existing_entry
                        .filter(|entry| persistent_addon_fingerprint_matches(entry, &fingerprint));
                    if let Some(entry) = cached_entry {
                        let state = AddonFolderState {
                            exists: true,
                            content_hash: entry.content_hash.clone(),
                        };
                        if !persistent_addon_fingerprint_is_current(
                            &entry.fingerprint,
                            &fingerprint,
                        ) {
                            guard.persistent_addon_hash_by_path.insert(
                                path_key.clone(),
                                PersistentAddonHashEntry {
                                    fingerprint: fingerprint.clone(),
                                    content_hash: entry.content_hash.clone(),
                                    updated_unix_ms: now_unix_ms(),
                                },
                            );
                            guard.persistent_dirty = true;
                        }
                        guard
                            .addon_state_by_path
                            .insert(path_key.clone(), state.clone());
                        Some(state)
                    } else {
                        None
                    }
                }
            };
            if let Some(state) = cached {
                addon_hash_hits_persistent += 1;
                persistent_hit = true;
                local_addon_state_cache.insert(path_key.clone(), state);
                addon_hash_timings.push((path_key.clone(), started.elapsed(), "persistent_cache"));
            }
        }
        if persistent_hit {
            continue;
        }

        if unresolved_seen_path_keys.insert(path_key.clone()) {
            unresolved_addon_hash_work.push(AddonHashWork {
                path_key,
                local_path: m.local_path.clone(),
                fingerprint,
            });
        }
    }

    let addon_hash_concurrency = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(1)
        .min(unresolved_addon_hash_work.len().max(1));

    if !unresolved_addon_hash_work.is_empty() {
        let semaphore = Arc::new(Semaphore::new(addon_hash_concurrency));
        let mut join_set: JoinSet<(AddonHashWork, AddonFolderState, Duration)> = JoinSet::new();

        for work in unresolved_addon_hash_work {
            let sem = semaphore.clone();
            join_set.spawn(async move {
                let _permit = sem.acquire_owned().await.ok();
                let started = Instant::now();
                let state = match tokio::task::spawn_blocking({
                    let local_path = work.local_path.clone();
                    move || probe_addon_folder_state(&local_path)
                })
                .await
                {
                    Ok(s) => s,
                    Err(err) => {
                        warn!(
                            "Addon folder probe task panicked for {}: {}",
                            work.local_path, err
                        );
                        Default::default()
                    }
                };
                (work, state, started.elapsed())
            });
        }

        while let Some(joined) = join_set.join_next().await {
            if let Ok((work, state, elapsed)) = joined {
                addon_hash_calculated += 1;
                debug!(
                    "Addon hash computed: path={} elapsed={:.2?} content_hash_prefix={}",
                    work.path_key,
                    elapsed,
                    &state.content_hash[..state.content_hash.len().min(12)]
                );
                addon_hash_timings.push((work.path_key.clone(), elapsed, "computed"));
                local_addon_state_cache.insert(work.path_key.clone(), state.clone());
                if let Some(shared) = shared_cache {
                    match shared.lock() {
                        Ok(mut guard) => {
                            guard
                                .addon_state_by_path
                                .insert(work.path_key.clone(), state.clone());
                            let updated_entry = PersistentAddonHashEntry {
                                fingerprint: work.fingerprint.clone(),
                                content_hash: state.content_hash.clone(),
                                updated_unix_ms: now_unix_ms(),
                            };
                            let changed = guard
                                .persistent_addon_hash_by_path
                                .get(&work.path_key)
                                .map(|existing| {
                                    !persistent_addon_fingerprint_is_current(
                                        &existing.fingerprint,
                                        &updated_entry.fingerprint,
                                    ) || existing.content_hash != updated_entry.content_hash
                                })
                                .unwrap_or(true);
                            if changed {
                                guard
                                    .persistent_addon_hash_by_path
                                    .insert(work.path_key.clone(), updated_entry);
                                guard.persistent_dirty = true;
                            }
                        }
                        Err(poisoned) => {
                            let mut guard = poisoned.into_inner();
                            guard
                                .addon_state_by_path
                                .insert(work.path_key.clone(), state.clone());
                            let updated_entry = PersistentAddonHashEntry {
                                fingerprint: work.fingerprint.clone(),
                                content_hash: state.content_hash.clone(),
                                updated_unix_ms: now_unix_ms(),
                            };
                            let changed = guard
                                .persistent_addon_hash_by_path
                                .get(&work.path_key)
                                .map(|existing| {
                                    !persistent_addon_fingerprint_is_current(
                                        &existing.fingerprint,
                                        &updated_entry.fingerprint,
                                    ) || existing.content_hash != updated_entry.content_hash
                                })
                                .unwrap_or(true);
                            if changed {
                                guard
                                    .persistent_addon_hash_by_path
                                    .insert(work.path_key.clone(), updated_entry);
                                guard.persistent_dirty = true;
                            }
                        }
                    }
                }
            }
        }
    }

    // Build per-mod state map from cache
    let mut addon_state_by_mod_id: HashMap<i64, AddonFolderState> = HashMap::new();
    for (mod_id, path_key) in &mod_path_key_by_id {
        let state = local_addon_state_cache
            .get(path_key)
            .cloned()
            .unwrap_or_default();
        addon_state_by_mod_id.insert(*mod_id, state);
    }

    // Identify mods requiring deeper inspection
    let mut mods_with_tree_mismatch: HashSet<i64> = HashSet::new();
    let mut mods_with_missing_path: HashSet<i64> = HashSet::new();
    let mut deep_scan_mod_ids: HashSet<i64> = HashSet::new();
    let mut phase1_addon_content_mismatch_count = 0usize;
    for m in mods {
        let is_enabled = mod_enabled_overrides
            .and_then(|overrides| overrides.get(&m.name.to_lowercase()).copied())
            .unwrap_or(m.enabled);
        if !is_enabled {
            continue;
        }
        let mod_id = m.id as i64;
        let addon_state = addon_state_by_mod_id
            .get(&mod_id)
            .cloned()
            .unwrap_or_default();
        let addon_content_mismatch =
            m.local_content_hash.is_empty() || m.local_content_hash != addon_state.content_hash;
        if addon_content_mismatch {
            phase1_addon_content_mismatch_count += 1;
            if addon_state.exists {
                deep_scan_mod_ids.insert(mod_id);
            }
        }
        if m.local_checksum != m.remote_checksum {
            mods_with_tree_mismatch.insert(mod_id);
        }
        let mod_path = m.local_path.trim();
        if mod_path.is_empty() || !addon_state.exists {
            mods_with_missing_path.insert(mod_id);
        }
    }

    let addon_hash_elapsed = addon_hash_stage_started.elapsed();

    if !addon_hash_timings.is_empty() {
        let mut sorted_timings = addon_hash_timings.clone();
        sorted_timings.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let top_n = sorted_timings.len().min(5);
        for (path, duration, source) in &sorted_timings[..top_n] {
            if *duration > Duration::from_millis(100) {
                info!(
                    "Addon hash slow path: path={} elapsed={:.2?} source={}",
                    sanitize_log_path_str(path),
                    duration,
                    source
                );
            }
        }
    }

    info!(
        "Addon hash resolution completed: elapsed={:.2?} enabled_addons={} computed={} shared_memory_hits={} persistent_hits={} missing_or_not_dir={} content_mismatches={} deep_scan_required={}",
        addon_hash_elapsed,
        enabled_addons,
        addon_hash_calculated,
        addon_hash_hits_shared_memory,
        addon_hash_hits_persistent,
        mods_with_missing_path.len(),
        phase1_addon_content_mismatch_count,
        deep_scan_mod_ids.len()
    );

    AddonHashResult {
        addon_state_by_mod_id,
        deep_scan_mod_ids,
        mods_with_tree_mismatch,
        mods_with_missing_path,
        addon_hash_timings,
        addon_hash_elapsed,
        addon_hash_concurrency,
        addon_hash_hits_shared_memory,
        addon_hash_hits_persistent,
        addon_hash_calculated,
        persistent_cache_entry_count,
        enabled_addons,
        phase1_addon_content_mismatch_count,
        missing_addon_path_samples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mod(local_path: String) -> FoxyMod {
        FoxyMod {
            id: 1,
            name: "@addon".to_string(),
            local_path,
            enabled: true,
            local_checksum: "TREE".to_string(),
            remote_checksum: "TREE".to_string(),
            local_content_hash: "STALE_CACHE_HASH".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn force_fresh_bypasses_persistent_addon_cache() {
        let temp = tempfile::tempdir().expect("temp dir");
        let addon_path = temp.path().join("@addon");
        std::fs::create_dir(&addon_path).expect("create addon dir");
        std::fs::write(addon_path.join("file.txt"), b"fresh content").expect("write addon file");
        let local_path = addon_path.to_string_lossy().to_string();
        let path_key = normalize_path_for_match(&local_path);
        let fingerprint = addon_root_fingerprint(&local_path);

        let mut shared = QuickScanSharedCache::default();
        shared.persistent_addon_hash_by_path.insert(
            path_key,
            PersistentAddonHashEntry {
                fingerprint,
                content_hash: "STALE_CACHE_HASH".to_string(),
                updated_unix_ms: now_unix_ms(),
            },
        );
        let shared = Arc::new(Mutex::new(shared));

        let result = resolve_addon_hashes(&[test_mod(local_path)], None, true, Some(&shared)).await;

        assert_eq!(result.addon_hash_hits_persistent, 0);
        assert_eq!(result.addon_hash_calculated, 1);
        assert_ne!(
            result
                .addon_state_by_mod_id
                .get(&1)
                .expect("addon state")
                .content_hash,
            "STALE_CACHE_HASH"
        );
    }
}
