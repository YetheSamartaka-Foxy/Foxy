use super::super::fs_watcher::normalize_path_for_match;
use super::super::logging::CONTENT_HASH_PERSIST_LOG_INTERVAL;
use super::super::*;
use crate::core::db::{DbValue, FoxyDb};
use crate::core::tasks::init_database::bulk_write_rows_for;
use crate::core::utils::format::sanitize_log_path_str;

// Do not include creation time: it changes on copies/restores while content does not.
pub(super) fn calculate_fast_file_content_hash(path: &str) -> Result<String, std::io::Error> {
    const SAMPLE_CHUNK_BYTES: usize = 16 * 1024;
    const SAMPLE_SLOTS: u64 = 8;

    let metadata = std::fs::metadata(path)?;
    let file_len = metadata.len();
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"FOXY_FILE_CONTENT_HASH_V2");
    hasher.update(&file_len.to_le_bytes());
    hasher.update(&modified_ns.to_le_bytes());

    if file_len == 0 {
        return Ok(crate::core::utils::content_hash::blake3_hex(hasher));
    }

    let mut file = std::fs::File::open(path)?;
    let sample_chunk = SAMPLE_CHUNK_BYTES as u64;
    let mut sample_buf = vec![0u8; SAMPLE_CHUNK_BYTES];

    // For smaller files, hash entire content. For larger files, hash evenly spaced samples.
    if file_len <= sample_chunk.saturating_mul(SAMPLE_SLOTS) {
        loop {
            let read = std::io::Read::read(&mut file, &mut sample_buf)?;
            if read == 0 {
                break;
            }
            hasher.update(&(read as u64).to_le_bytes());
            hasher.update(&sample_buf[..read]);
        }
        return Ok(crate::core::utils::content_hash::blake3_hex(hasher));
    }

    let max_offset = file_len.saturating_sub(sample_chunk);
    let mut last_offset = u64::MAX;
    for slot in 0..SAMPLE_SLOTS {
        let offset = if SAMPLE_SLOTS <= 1 {
            0
        } else {
            max_offset.saturating_mul(slot) / (SAMPLE_SLOTS - 1)
        };
        if offset == last_offset {
            continue;
        }
        last_offset = offset;
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset))?;
        let read = std::io::Read::read(&mut file, &mut sample_buf)?;
        hasher.update(&offset.to_le_bytes());
        hasher.update(&(read as u64).to_le_bytes());
        hasher.update(&sample_buf[..read]);
    }

    Ok(crate::core::utils::content_hash::blake3_hex(hasher))
}

pub(super) fn calculate_fast_addon_folder_content_hash(
    path: &str,
) -> Result<String, std::io::Error> {
    crate::core::utils::content_hash::calculate_addon_folder_content_hash(Path::new(path))
}

fn calculate_compound_content_hash(ordered_hashes: &[(i64, String)]) -> String {
    if ordered_hashes.is_empty() {
        return String::new();
    }
    let mut values = ordered_hashes.to_vec();
    values.sort_by_key(|(order, _)| *order);
    let mut hasher = blake3::Hasher::new();
    for (_, value) in values {
        hasher.update(value.as_bytes());
    }
    crate::core::utils::content_hash::blake3_hex(hasher)
}

async fn persist_file_content_hashes(db: &FoxyDb, repo_url: &str, file_updates: &[FoxyModFile]) {
    let batch_size = bulk_write_rows_for(9);
    for (chunk_index, chunk) in file_updates
        .chunks(CONTENT_HASH_PERSIST_LOG_INTERVAL)
        .enumerate()
    {
        let chunk_rows = Arc::new(chunk.to_vec());
        let had_error = db
            .transaction("persist file content-hashes", move |txn| {
                let chunk_rows = Arc::clone(&chunk_rows);
                Box::pin(async move {
                    for batch in chunk_rows.chunks(batch_size) {
                        let placeholders =
                            vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO files (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) VALUES {placeholders} ON CONFLICT(id) DO UPDATE SET local_content_hash = excluded.local_content_hash WHERE files.local_content_hash IS NOT excluded.local_content_hash"
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 9);
                        for f in batch {
                            values.push((f.id as i64).into());
                            values.push(f.name.clone().into());
                            values.push(f.remote_path.clone().into());
                            values.push(f.local_path.clone().into());
                            values.push(f.local_checksum.clone().into());
                            values.push(f.remote_checksum.clone().into());
                            values.push(f.local_content_hash.clone().into());
                            values.push((f.length as i64).into());
                            values.push(f.data_order.into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                    Ok(())
                })
            })
            .await
            .is_err();

        info!(
            "Persisted file content-hash chunk for repo {} (chunk={} rows={} had_errors={})",
            repo_url,
            chunk_index + 1,
            chunk.len(),
            had_error
        );
    }
}

pub(super) async fn persist_mod_content_hashes(
    db: &FoxyDb,
    repo_url: &str,
    mod_updates: &[FoxyMod],
) {
    let batch_size = bulk_write_rows_for(12);
    for (chunk_index, chunk) in mod_updates
        .chunks(CONTENT_HASH_PERSIST_LOG_INTERVAL)
        .enumerate()
    {
        let chunk_rows = Arc::new(chunk.to_vec());
        let had_error = db
            .transaction("persist addon content-hashes", move |txn| {
                let chunk_rows = Arc::clone(&chunk_rows);
                Box::pin(async move {
                    for batch in chunk_rows.chunks(batch_size) {
                        let placeholders =
                            vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO addons (id, name, display_name, remote_path, local_path, client_side, enabled, local_checksum, remote_checksum, local_content_hash, required, data_order) VALUES {placeholders} ON CONFLICT(id) DO UPDATE SET local_checksum = excluded.local_checksum, local_content_hash = excluded.local_content_hash WHERE addons.local_checksum IS NOT excluded.local_checksum OR addons.local_content_hash IS NOT excluded.local_content_hash"
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 12);
                        for m in batch {
                            values.push((m.id as i64).into());
                            values.push(m.name.clone().into());
                            values.push(m.display_name.clone().into());
                            values.push(m.remote_path.clone().into());
                            values.push(m.local_path.clone().into());
                            values.push(m.client_side.into());
                            values.push(m.enabled.into());
                            values.push(m.local_checksum.clone().into());
                            values.push(m.remote_checksum.clone().into());
                            values.push(m.local_content_hash.clone().into());
                            values.push(m.required.into());
                            values.push(m.data_order.into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                    Ok(())
                })
            })
            .await
            .is_err();

        info!(
            "Persisted addon content-hash chunk for repo {} (chunk={} rows={} had_errors={})",
            repo_url,
            chunk_index + 1,
            chunk.len(),
            had_error
        );
    }
}

async fn persist_repo_content_hashes(db: &FoxyDb, repo_url: &str, repo_updates: &[FoxyRepository]) {
    let batch_size = bulk_write_rows_for(9);
    for (chunk_index, chunk) in repo_updates
        .chunks(CONTENT_HASH_PERSIST_LOG_INTERVAL)
        .enumerate()
    {
        let chunk_rows = Arc::new(chunk.to_vec());
        let had_error = db
            .transaction("persist repo content-hashes", move |txn| {
                let chunk_rows = Arc::clone(&chunk_rows);
                Box::pin(async move {
                    for batch in chunk_rows.chunks(batch_size) {
                        let placeholders =
                            vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
                        let sql = format!(
                            "INSERT INTO repositories (id, name, remote_url, local_path, image, local_checksum, remote_checksum, local_content_hash, foxy_mode) VALUES {placeholders} ON CONFLICT(id) DO UPDATE SET local_content_hash = excluded.local_content_hash WHERE repositories.local_content_hash IS NOT excluded.local_content_hash"
                        );
                        let mut values: Vec<DbValue> = Vec::with_capacity(batch.len() * 9);
                        for r in batch {
                            values.push((r.id as i64).into());
                            values.push(r.name.clone().into());
                            values.push(r.remote_url.clone().into());
                            values.push(r.local_path.clone().into());
                            values.push(r.image.clone().into());
                            values.push(r.local_checksum.clone().into());
                            values.push(r.remote_checksum.clone().into());
                            values.push(r.local_content_hash.clone().into());
                            values.push(r.foxy_mode.as_db_str().to_string().into());
                        }
                        txn.execute(&sql, values).await?;
                    }
                    Ok(())
                })
            })
            .await
            .is_err();

        info!(
            "Persisted repository content-hash chunk for repo {} (chunk={} rows={} had_errors={})",
            repo_url,
            chunk_index + 1,
            chunk.len(),
            had_error
        );
    }
}

pub(crate) async fn refresh_content_hashes_when_tree_matches(
    context: Arc<FoxyContext>,
    repo_url: &str,
    preloaded_tree: Option<Tree>,
) -> bool {
    let content_hash_started = Instant::now();
    let tree = match preloaded_tree {
        Some(tree) => tree,
        None => match Tree::load(context.clone(), repo_url).await {
            Ok(tree) => tree,
            Err(err) => {
                warn!(
                    "Failed to load tree for content-hash refresh {}: {}",
                    repo_url, err
                );
                return false;
            }
        },
    };

    refresh_content_hashes_for_tree_started(context, repo_url, &tree, content_hash_started, true)
        .await
}

pub(crate) async fn refresh_content_hashes_for_tree(
    context: Arc<FoxyContext>,
    repo_url: &str,
    tree: &Tree,
) -> bool {
    refresh_content_hashes_for_tree_started(context, repo_url, tree, Instant::now(), true).await
}

pub(crate) async fn refresh_content_hashes_for_scoped_tree(
    context: Arc<FoxyContext>,
    repo_url: &str,
    tree: &Tree,
) -> bool {
    refresh_content_hashes_for_tree_started(context, repo_url, tree, Instant::now(), false).await
}

async fn refresh_content_hashes_for_tree_started(
    context: Arc<FoxyContext>,
    repo_url: &str,
    tree: &Tree,
    content_hash_started: Instant,
    persist_repository_rollup: bool,
) -> bool {
    if tree.repositories.is_empty() || tree.mods.is_empty() || tree.files.is_empty() {
        return false;
    }

    // Skip content-hash computation for files/addons with tree-hash mismatches.
    // If a file's local_checksum differs from remote_checksum, the file on disk
    // doesn't match what the server expects.  Refreshing its content-hash would
    // "blind" the quick scan by making the baseline match the (wrong) disk state.
    // Leaving the content hash empty forces a deep scan that compares tree hashes.
    let file_ids_with_tree_mismatch: HashSet<u64> = tree
        .files
        .iter()
        .filter(|f| !f.local_checksum.is_empty() && f.local_checksum != f.remote_checksum)
        .map(|f| f.id)
        .collect();
    let addon_indices_with_tree_mismatch: HashSet<usize> = tree
        .mods
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.local_checksum.is_empty() && m.local_checksum != m.remote_checksum)
        .map(|(idx, _)| idx)
        .collect();
    if !file_ids_with_tree_mismatch.is_empty() || !addon_indices_with_tree_mismatch.is_empty() {
        info!(
            "Content-hash refresh skipping {} files and {} addons with tree-hash mismatches for repo {}",
            file_ids_with_tree_mismatch.len(),
            addon_indices_with_tree_mismatch.len(),
            repo_url
        );
    }

    let db = context.db();
    let cpu_budget = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_mul(9)
        .div_ceil(10)
        .max(1);
    let probe_path = tree
        .files
        .iter()
        .map(|file| file.local_path.trim())
        .find(|path| !path.is_empty())
        .or_else(|| {
            tree.repositories
                .iter()
                .map(|repo| repo.local_path.trim())
                .find(|path| !path.is_empty())
        })
        .unwrap_or_default();
    let storage_class =
        crate::core::tasks::calculate_hashes::detect_storage_class_for_path(probe_path);
    let storage_is_rotational = matches!(
        storage_class,
        crate::core::tasks::calculate_hashes::HashStorageClass::Hdd
            | crate::core::tasks::calculate_hashes::HashStorageClass::Removable
    );
    let file_concurrency = if storage_is_rotational {
        4
    } else {
        (cpu_budget.saturating_mul(6)).clamp(16, 256)
    };
    let semaphore = Arc::new(Semaphore::new(file_concurrency));
    let mut join_set: JoinSet<(u64, String)> = JoinSet::new();

    for file in &tree.files {
        let file_id = file.id;
        if file_ids_with_tree_mismatch.contains(&file_id) {
            continue;
        }
        let path = file.local_path.clone();
        let sem = semaphore.clone();
        join_set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            let hash = if path.trim().is_empty() {
                String::new()
            } else {
                tokio::task::spawn_blocking(move || calculate_fast_file_content_hash(&path))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default()
            };
            (file_id, hash)
        });
    }

    let file_hash_started = Instant::now();
    let mut file_content_hash_by_id: HashMap<u64, String> = HashMap::new();
    let mut file_hash_failures = 0usize;
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((file_id, hash)) => {
                if hash.is_empty() {
                    file_hash_failures += 1;
                }
                file_content_hash_by_id.insert(file_id, hash);
            }
            Err(_) => {
                file_hash_failures += 1;
            }
        }
    }
    let file_hash_elapsed = file_hash_started.elapsed();

    let mut files_with_content_hash = 0usize;
    let mut file_updates: Vec<FoxyModFile> = Vec::new();
    for file in &tree.files {
        let mut updated = file.clone();
        if file_ids_with_tree_mismatch.contains(&file.id) {
            // Tree mismatch - clear content hash so the quick scan forces deep inspection
            updated.local_content_hash = String::new();
        } else {
            let hash = file_content_hash_by_id
                .get(&file.id)
                .cloned()
                .unwrap_or_default();
            if !hash.is_empty() {
                files_with_content_hash += 1;
            }
            updated.local_content_hash = hash;
        }
        if updated.local_content_hash != file.local_content_hash {
            file_updates.push(updated);
        }
    }
    let file_persist_started = Instant::now();
    persist_file_content_hashes(&db, repo_url, &file_updates).await;
    let file_persist_elapsed = file_persist_started.elapsed();

    let addon_hash_started = Instant::now();
    let mut addon_content_hash_by_idx: HashMap<usize, String> = HashMap::new();
    let mut addons_with_content_hash = 0usize;
    let mut addon_hash_failures = 0usize;
    let mut addons_with_missing_expected_files = 0usize;

    let mut addon_paths_to_hash: HashMap<String, String> = HashMap::new();
    for addon_node in &tree.mod_nodes {
        let Some(addon) = tree.mods.get(addon_node.mod_idx) else {
            continue;
        };
        if addon_indices_with_tree_mismatch.contains(&addon_node.mod_idx) {
            continue;
        }
        let addon_path = addon.local_path.trim().to_string();
        if addon_path.is_empty() {
            continue;
        }
        addon_paths_to_hash
            .entry(normalize_path_for_match(&addon_path))
            .or_insert(addon_path);
    }
    let addon_concurrency = if storage_is_rotational {
        2
    } else {
        cpu_budget.clamp(2, 16)
    };
    let addon_semaphore = Arc::new(Semaphore::new(addon_concurrency));
    let mut addon_join_set: JoinSet<(String, Option<String>)> = JoinSet::new();
    for (path_key, addon_path) in addon_paths_to_hash {
        let sem = addon_semaphore.clone();
        addon_join_set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok();
            let addon_path_for_task = addon_path.clone();
            let hash = match tokio::task::spawn_blocking(move || {
                calculate_fast_addon_folder_content_hash(&addon_path_for_task)
            })
            .await
            {
                Ok(Ok(h)) => Some(h),
                Ok(Err(err)) => {
                    debug!(
                        "Content hash calculation failed for {}: {}",
                        addon_path, err
                    );
                    None
                }
                Err(err) => {
                    warn!(
                        "Content hash task panicked for {}: {}",
                        sanitize_log_path_str(&addon_path),
                        err
                    );
                    None
                }
            };
            (path_key, hash)
        });
    }
    let mut addon_hash_by_path: HashMap<String, String> = HashMap::new();
    while let Some(result) = addon_join_set.join_next().await {
        match result {
            Ok((path_key, hash)) => {
                if hash.is_none() {
                    addon_hash_failures += 1;
                }
                addon_hash_by_path.insert(path_key, hash.unwrap_or_default());
            }
            Err(_) => {
                addon_hash_failures += 1;
            }
        }
    }

    for addon_node in &tree.mod_nodes {
        let Some(addon) = tree.mods.get(addon_node.mod_idx) else {
            continue;
        };
        if addon_indices_with_tree_mismatch.contains(&addon_node.mod_idx) {
            addon_content_hash_by_idx.insert(addon_node.mod_idx, String::new());
            continue;
        }
        let addon_path = addon.local_path.trim().to_string();
        if addon_path.is_empty() {
            addon_content_hash_by_idx.insert(addon_node.mod_idx, String::new());
            continue;
        }

        let path_key = normalize_path_for_match(&addon_path);
        let addon_content_hash = addon_hash_by_path
            .get(&path_key)
            .cloned()
            .unwrap_or_default();

        // A folder content hash computed over a folder that is *missing* manifest
        // files still matches a baseline recorded from the same incomplete state,
        // permanently masking the missing files from the quick scan (which then
        // reports the addon clean and never schedules the files for download).
        // Refuse to bless an addon whose expected files are not all present on
        // disk - leaving its content hash empty forces the quick scan to deep-scan
        // it and surface the missing files for re-download, mirroring the
        // tree-hash-mismatch handling above. A file's content hash is only empty
        // when it could not be read from disk (a present file, even 0 bytes, hashes
        // to a non-empty value), so an empty hash reliably means "missing on disk".
        let all_expected_files_present = addon_node.files.iter().all(|&file_idx| {
            tree.files
                .get(file_idx)
                .map(|f| {
                    file_content_hash_by_id
                        .get(&f.id)
                        .is_some_and(|hash| !hash.is_empty())
                })
                .unwrap_or(false)
        });
        let addon_content_hash = if all_expected_files_present {
            addon_content_hash
        } else {
            addons_with_missing_expected_files += 1;
            String::new()
        };

        if !addon_content_hash.is_empty() {
            addons_with_content_hash += 1;
        }
        addon_content_hash_by_idx.insert(addon_node.mod_idx, addon_content_hash);
    }

    let addon_hash_elapsed = addon_hash_started.elapsed();

    if addons_with_missing_expected_files > 0 {
        info!(
            "Content-hash refresh withheld baseline for {} addon(s) with manifest files missing on disk for repo {}; quick scan will deep-scan them and report the missing files for download",
            addons_with_missing_expected_files, repo_url
        );
    }

    let mut mod_updates: Vec<FoxyMod> = Vec::new();
    for (addon_idx, addon) in tree.mods.iter().enumerate() {
        let hash = addon_content_hash_by_idx
            .get(&addon_idx)
            .cloned()
            .unwrap_or_default();
        if hash == addon.local_content_hash {
            continue;
        }
        let mut updated = addon.clone();
        updated.local_content_hash = hash;
        mod_updates.push(updated);
    }
    persist_mod_content_hashes(&db, repo_url, &mod_updates).await;

    let mut repos_with_content_hash = 0usize;
    if persist_repository_rollup {
        let mut repo_updates: Vec<FoxyRepository> = Vec::with_capacity(tree.repo_nodes.len());
        for repo_node in &tree.repo_nodes {
            let Some(repo) = tree.repositories.get(repo_node.repo_idx) else {
                continue;
            };

            let mut ordered_hashes: Vec<(i64, String)> = Vec::new();
            let mut all_addons_hashed = true;
            for addon_idx in &repo_node.mods {
                let Some(addon) = tree.mods.get(*addon_idx) else {
                    continue;
                };
                let Some(addon_hash) = addon_content_hash_by_idx.get(addon_idx).cloned() else {
                    all_addons_hashed = false;
                    break;
                };
                if addon_hash.is_empty() {
                    all_addons_hashed = false;
                    break;
                }
                ordered_hashes.push((addon.data_order, addon_hash));
            }
            let repo_content_hash = if all_addons_hashed {
                calculate_compound_content_hash(&ordered_hashes)
            } else {
                String::new()
            };
            if !repo_content_hash.is_empty() {
                repos_with_content_hash += 1;
            }
            if repo_content_hash == repo.local_content_hash {
                continue;
            }
            let mut updated = repo.clone();
            updated.local_content_hash = repo_content_hash;
            repo_updates.push(updated);
        }
        persist_repo_content_hashes(&db, repo_url, &repo_updates).await;
    }

    let total_elapsed = content_hash_started.elapsed();
    let total_files = tree.files.len();
    let total_addons = tree.mods.len();
    let file_failure_percent = file_hash_failures
        .saturating_mul(100)
        .checked_div(total_files)
        .unwrap_or(0);
    let addon_failure_percent = addon_hash_failures
        .saturating_mul(100)
        .checked_div(total_addons)
        .unwrap_or(0);
    info!(
        "Content-hash baseline refreshed: repo={} total_elapsed={:.2?} file_hash={:.2?} file_persist={:.2?} addon_hash={:.2?} repos_hashed={} addons_hashed={}/{} files_hashed={}/{} file_failures={} addon_failures={}",
        repo_url,
        total_elapsed,
        file_hash_elapsed,
        file_persist_elapsed,
        addon_hash_elapsed,
        repos_with_content_hash,
        addons_with_content_hash,
        total_addons,
        files_with_content_hash,
        total_files,
        file_hash_failures,
        addon_hash_failures
    );
    if file_failure_percent > 5 {
        warn!(
            "High content-hash failure rate for repo {}: file_failures={}/{} ({}%) addon_failures={}/{} ({}%)",
            repo_url,
            file_hash_failures,
            total_files,
            file_failure_percent,
            addon_hash_failures,
            total_addons,
            addon_failure_percent
        );
    }
    true
}
