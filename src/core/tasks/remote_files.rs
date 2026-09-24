use crate::core::db::{DbValue, FoxyDb, params};
use crate::core::models::context::FoxyContext;
use crate::core::models::modification::FoxyMod;
use crate::core::models::modification_file::{FILE_COLUMNS, FoxyModFile};
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::models::repository::FoxyRepository;
use crate::core::tasks::init_database::{
    DB_WRITE_PERMITS, SQLITE_MAX_VARIABLES, read_chunk_ids, sqlite_perf_snapshot,
};
use crate::core::tasks::remote_file_parts::{
    FilePartData, FilePartsPayload, remote_file_parts_batch,
};
use crate::core::utils::fetch_json::{FetchJsonTiming, fetch_manifest_json_timed};
use log::{debug, error, info, warn};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

/// Typed manifest for both mod.srf (PascalCase) and foxy_addon.json (camelCase).
/// Uses serde aliases so one struct deserializes either format.
#[derive(Deserialize)]
struct ModManifest {
    #[serde(alias = "Files")]
    files: Vec<ManifestFile>,
}

#[derive(Deserialize, Clone)]
struct ManifestFile {
    #[serde(alias = "Path")]
    path: String,
    #[serde(alias = "Checksum")]
    checksum: String,
    #[serde(alias = "Length")]
    length: i64,
    #[serde(alias = "Parts", default)]
    parts: Vec<ManifestPart>,
}

#[derive(Deserialize, Clone)]
struct ManifestPart {
    #[serde(alias = "Path")]
    path: String,
    #[serde(alias = "Checksum")]
    checksum: String,
    #[serde(alias = "Start")]
    start: i64,
    #[serde(alias = "Length")]
    length: i64,
}

fn join_path(base: &str, child: &str) -> String {
    if base.ends_with('/') || base.ends_with('\\') {
        format!("{}{}", base, child)
    } else {
        format!("{}/{}", base, child)
    }
}

fn part_task_limit() -> usize {
    let cpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // This path fans into DB writes in remote_file_parts_batch; keep it conservative.
    let lock_retries = sqlite_perf_snapshot().lock_retries;
    let pressure_divisor = if lock_retries >= 128 {
        4
    } else if lock_retries >= 48 {
        2
    } else {
        1
    };
    let suggested = cpu.saturating_mul(2) / pressure_divisor;
    let ceiling = (*DB_WRITE_PERMITS).saturating_mul(12).clamp(12, 48);
    suggested.clamp(8, ceiling)
}

/// Split files into sub-batches targeting `max_parts` parts per batch.
/// Each batch gets at least `min_files` files (to avoid single-file batches
/// that would underutilize the DB transaction). Files are assigned greedily:
/// accumulate into the current batch until the part budget is exceeded.
fn build_parts_sub_batches(
    files: &[FilePartsPayload],
    max_parts: usize,
    min_files: usize,
) -> Vec<Vec<FilePartsPayload>> {
    if files.is_empty() {
        return vec![];
    }
    let mut batches: Vec<Vec<FilePartsPayload>> = Vec::new();
    let mut current_batch: Vec<FilePartsPayload> = Vec::new();
    let mut current_parts: usize = 0;

    for file in files {
        let file_parts = file.parts.len();
        // Start new batch if we'd exceed the budget AND have enough files
        if !current_batch.is_empty()
            && current_parts + file_parts > max_parts
            && current_batch.len() >= min_files
        {
            batches.push(std::mem::take(&mut current_batch));
            current_parts = 0;
        }
        current_parts += file_parts;
        current_batch.push(file.clone());
    }
    if !current_batch.is_empty() {
        batches.push(current_batch);
    }
    batches
}

fn normalize_local_path_key(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path)
}

fn file_identity_key(remote_path: &str, local_path: &str) -> String {
    format!("{}|{}", remote_path, normalize_local_path_key(local_path))
}

struct StagedFileKey {
    name: String,
    remote_checksum: String,
    length: i64,
    remote_path: String,
    local_path: String,
    parts: Vec<ManifestPart>,
    data_order: i64,
}

pub(crate) struct StagedModFiles {
    mod_parent: Arc<FoxyMod>,
    file_keys: Vec<StagedFileKey>,
    http_download_duration: std::time::Duration,
    http_response_bytes: usize,
    http_parse_duration: std::time::Duration,
    manifest_cached: bool,
    mod_start: Instant,
}

#[derive(Clone)]
pub(crate) struct FileUpsertResult {
    files: HashMap<String, FoxyModFile>,
    previous: HashMap<String, FoxyModFile>,
}

fn file_present_with_expected_len(path: &str, expected_len: u64) -> bool {
    crate::core::utils::profiling::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() == expected_len)
        .unwrap_or(false)
}

#[derive(Clone, Copy, Debug, Default)]
struct FileRemoteGraphState {
    part_count: i64,
    missing_part_remote_checksums: i64,
}

impl FileRemoteGraphState {
    fn complete(self) -> bool {
        self.part_count > 0 && self.missing_part_remote_checksums == 0
    }
}

async fn load_file_remote_graph_states(
    db: &FoxyDb,
    file_ids: &HashSet<i64>,
) -> HashMap<i64, FileRemoteGraphState> {
    let mut states: HashMap<i64, FileRemoteGraphState> = file_ids
        .iter()
        .map(|id| (*id, FileRemoteGraphState::default()))
        .collect();
    if file_ids.is_empty() {
        return states;
    }

    let mut ids: Vec<i64> = file_ids.iter().copied().collect();
    ids.sort_unstable();
    let chunk_size = read_chunk_ids();
    let mut idx = 0usize;
    while idx < ids.len() {
        let end = (idx + chunk_size).min(ids.len());
        let chunk = &ids[idx..end];
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            r#"SELECT
                   f.id AS file_id,
                   COUNT(sf.id) AS part_count,
                   SUM(CASE WHEN sf.id IS NOT NULL AND sf.remote_checksum = '' THEN 1 ELSE 0 END) AS missing_part_remote_checksums
               FROM files f
               LEFT JOIN subfiles sf ON sf.file_id = f.id
               WHERE f.id IN ({})
               GROUP BY f.id"#,
            placeholders
        );
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();

        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    let Ok(file_id) = row.get_i64("file_id") else {
                        continue;
                    };
                    states.insert(
                        file_id,
                        FileRemoteGraphState {
                            part_count: row.get_i64("part_count").unwrap_or(0),
                            missing_part_remote_checksums: row
                                .get_i64("missing_part_remote_checksums")
                                .unwrap_or(0),
                        },
                    );
                }
            }
            Err(err) => {
                warn!("Failed to load file remote graph states: {}", err);
            }
        }
        idx = end;
    }

    states
}

/// Load existing file rows for the given remote paths (chunked under the SQLite
/// bind limit). Returns every matching [`FoxyModFile`]; callers filter by identity.
async fn load_files_by_remote_paths(db: &FoxyDb, remote_paths: &[String]) -> Vec<FoxyModFile> {
    let mut out = Vec::new();
    let chunk_size = read_chunk_ids();
    let mut idx = 0usize;
    while idx < remote_paths.len() {
        let end = (idx + chunk_size).min(remote_paths.len());
        let slice = &remote_paths[idx..end];
        let placeholders = vec!["?"; slice.len()].join(", ");
        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE remote_path IN ({placeholders})");
        let values: Vec<DbValue> = slice.iter().map(|p| p.clone().into()).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    match FoxyModFile::from_row(&row) {
                        Ok(f) => out.push(f),
                        Err(err) => warn!("Failed to read file row: {}", err),
                    }
                }
            }
            Err(err) => {
                warn!("Failed to load existing file models batch: {}", err);
            }
        }
        idx = end;
    }
    out
}

pub(crate) async fn upsert_file_rows_batch(
    db: &FoxyDb,
    staged_mods: &[StagedModFiles],
) -> FileUpsertResult {
    let mut desired_file_keys: HashSet<String> = HashSet::new();
    let mut unique_rows: HashMap<String, &StagedFileKey> = HashMap::new();
    let mut remote_paths: Vec<String> = Vec::new();
    for staged in staged_mods {
        for row in &staged.file_keys {
            let key = file_identity_key(&row.remote_path, &row.local_path);
            if unique_rows.insert(key.clone(), row).is_none() {
                desired_file_keys.insert(key);
                remote_paths.push(row.remote_path.clone());
            }
        }
    }

    let mut previous: HashMap<String, FoxyModFile> = HashMap::new();
    let mut ignored_existing_path_mismatches = 0usize;
    if !remote_paths.is_empty() {
        for file in load_files_by_remote_paths(db, &remote_paths).await {
            let key = file_identity_key(&file.remote_path, &file.local_path);
            if desired_file_keys.contains(&key) {
                previous.insert(key, file);
            } else {
                ignored_existing_path_mismatches += 1;
            }
        }
    }
    if ignored_existing_path_mismatches > 0 {
        info!(
            "Ignored {} existing file row(s) with matching remote paths but different local paths during batched file upsert",
            ignored_existing_path_mismatches
        );
    }

    let has_new_files = unique_rows.keys().any(|key| !previous.contains_key(key));
    let rows: Vec<&StagedFileKey> = unique_rows.into_values().collect();
    let file_chunk_size = crate::core::tasks::init_database::bulk_write_rows_for(8);
    for chunk in rows.chunks(file_chunk_size) {
        let placeholders = vec!["(?, ?, ?, ?, ?, ?, ?, ?)"; chunk.len()].join(", ");
        let sql = format!(
            "INSERT INTO files \
             (name, remote_path, local_path, remote_checksum, local_checksum, \
              local_content_hash, length, data_order) \
             VALUES {placeholders} \
             ON CONFLICT(name, remote_path, local_path) DO UPDATE SET \
                remote_checksum = excluded.remote_checksum, \
                length = excluded.length, \
                data_order = excluded.data_order, \
                local_path = excluded.local_path"
        );
        let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 8);
        for row in chunk {
            let key = file_identity_key(&row.remote_path, &row.local_path);
            let (local_checksum, local_content_hash) = previous
                .get(&key)
                .map(|existing| {
                    (
                        existing.local_checksum.clone(),
                        existing.local_content_hash.clone(),
                    )
                })
                .unwrap_or_default();
            values.push(row.name.clone().into());
            values.push(row.remote_path.clone().into());
            values.push(row.local_path.clone().into());
            values.push(row.remote_checksum.clone().into());
            values.push(local_checksum.into());
            values.push(local_content_hash.into());
            values.push(row.length.into());
            values.push(row.data_order.into());
        }
        if let Err(e) = db.execute_retry("file upsert", &sql, values).await {
            warn!("Failed to upsert batched files: {}", e);
        }
    }

    let files = if remote_paths.is_empty() {
        HashMap::new()
    } else if has_new_files {
        let mut files: HashMap<String, FoxyModFile> = HashMap::new();
        for file in load_files_by_remote_paths(db, &remote_paths).await {
            let key = file_identity_key(&file.remote_path, &file.local_path);
            if desired_file_keys.contains(&key) {
                files.insert(key, file);
            }
        }
        files
    } else {
        let mut files = previous.clone();
        for row in &rows {
            let key = file_identity_key(&row.remote_path, &row.local_path);
            if let Some(file) = files.get_mut(&key) {
                file.remote_checksum = row.remote_checksum.clone();
                file.length = row.length as u64;
                file.local_path = row.local_path.clone();
                file.data_order = row.data_order;
            }
        }
        files
    };
    FileUpsertResult { files, previous }
}

async fn reconcile_addon_file_links(
    context: Arc<FoxyContext>,
    mod_parent: Arc<FoxyMod>,
    desired_file_ids: HashSet<i64>,
    can_prune_stale: bool,
) {
    let mod_id = mod_parent.id as i64;
    if !can_prune_stale {
        warn!(
            "Skipping stale addon_files cleanup for {} due to unresolved file ids",
            mod_parent.remote_path
        );
    }

    // Phase 1: Stage addon_file links for a single cross-mod flush after the
    // metadata fan-out. Inserting per mod was 96 nine-row statements.
    if !desired_file_ids.is_empty() {
        context.buffer_addon_file_links(
            desired_file_ids
                .iter()
                .copied()
                .map(|file_id| (mod_id, file_id)),
        );
    }

    // Phase 2: Prune stale addon_file links. A whole-wipe force-redownload has
    // no leftover links, so skip the per-mod SELECT/DELETE.
    if can_prune_stale
        && !context.is_fresh_subfiles_load()
        && let Err(e) = context
            .db()
            .transaction("prune addon_files", |txn| {
                let desired_file_ids = desired_file_ids.clone();
                Box::pin(async move {
                    if desired_file_ids.is_empty() {
                        txn.execute(
                            "DELETE FROM addon_files WHERE addon_id = ?",
                            params![mod_id],
                        )
                        .await?;
                    } else {
                        let existing_links = txn
                            .query_all(
                                "SELECT file_id FROM addon_files WHERE addon_id = ?",
                                params![mod_id],
                            )
                            .await?;
                        let stale_file_ids: Vec<i64> = existing_links
                            .iter()
                            .filter_map(|row| row.get_i64("file_id").ok())
                            .filter(|id| !desired_file_ids.contains(id))
                            .collect();

                        if !stale_file_ids.is_empty() {
                            let delete_chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(4).max(1);
                            for chunk in stale_file_ids.chunks(delete_chunk_size) {
                                let placeholders = vec!["?"; chunk.len()].join(", ");
                                let sql = format!(
                                    "DELETE FROM addon_files \
                                     WHERE addon_id = ? AND file_id IN ({placeholders})"
                                );
                                let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() + 1);
                                values.push(mod_id.into());
                                for id in chunk {
                                    values.push((*id).into());
                                }
                                txn.execute(&sql, values).await?;
                            }
                        }
                    }
                    Ok(())
                })
            })
            .await
    {
        warn!(
            "Failed to prune stale addon_files for {}: {}",
            mod_parent.remote_path, e
        );
    }
}

pub(crate) async fn flush_pending_addon_file_links(context: Arc<FoxyContext>) {
    let mut links = context.take_pending_addon_file_links();
    if links.is_empty() {
        return;
    }
    links.sort_unstable();
    links.dedup();
    let db = context.db();
    let chunk_size = crate::core::tasks::init_database::bulk_write_rows_for(2);
    for chunk in links.chunks(chunk_size) {
        let placeholders = vec!["(?, ?)"; chunk.len()].join(", ");
        let sql = format!(
            "INSERT INTO addon_files (addon_id, file_id) VALUES {placeholders} \
             ON CONFLICT(addon_id, file_id) DO NOTHING"
        );
        let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 2);
        for (addon_id, file_id) in chunk {
            values.push((*addon_id).into());
            values.push((*file_id).into());
        }
        if let Err(e) = db.execute_retry("addon_files insert", &sql, values).await {
            warn!("Failed to insert batched addon_files: {}", e);
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ModRecheckStats {
    pub mod_path: String,
    pub files: usize,
    pub parts: usize,
    pub bytes: u64,
    pub mod_concurrency_limit: usize,
    pub duration: std::time::Duration,
    /// Time spent downloading the manifest body over HTTP
    pub http_download_duration: std::time::Duration,
    /// Size of the manifest response body in bytes
    pub http_response_bytes: usize,
    /// Time spent parsing the manifest JSON (BOM strip + serde_json)
    pub http_parse_duration: std::time::Duration,
    /// The server confirmed the cached manifest; nothing was downloaded.
    pub manifest_cached: bool,
    /// Time spent upserting file records for this mod
    pub file_upsert_duration: std::time::Duration,
    /// Time spent in remote_file_parts_batch (parts upsert + reconcile + download targets)
    pub parts_persist_duration: std::time::Duration,
}

fn empty_recheck_stats(
    mod_parent: &FoxyMod,
    http_timing: FetchJsonTiming,
    duration: std::time::Duration,
) -> ModRecheckStats {
    ModRecheckStats {
        mod_path: mod_parent.remote_path.clone(),
        files: 0,
        parts: 0,
        bytes: 0,
        mod_concurrency_limit: 1,
        duration,
        http_download_duration: http_timing.download,
        http_response_bytes: http_timing.response_bytes,
        http_parse_duration: http_timing.parse,
        manifest_cached: http_timing.cached,
        file_upsert_duration: std::time::Duration::ZERO,
        parts_persist_duration: std::time::Duration::ZERO,
    }
}

fn zero_fetch_timing() -> FetchJsonTiming {
    FetchJsonTiming {
        download: std::time::Duration::ZERO,
        parse: std::time::Duration::ZERO,
        response_bytes: 0,
        cached: false,
    }
}

pub(crate) async fn fetch_mod_file_manifest(
    context: Arc<FoxyContext>,
    repository_parent: Arc<FoxyRepository>,
    mod_parent: Arc<FoxyMod>,
) -> Result<StagedModFiles, ModRecheckStats> {
    let mod_start = Instant::now();
    let is_foxy_mode = repository_parent.foxy_mode.is_foxy();

    let files_metadata_url = if is_foxy_mode {
        format!("{}/foxy_addon.json", mod_parent.remote_path)
    } else {
        format!("{}/mod.srf", mod_parent.remote_path)
    };
    debug!("Loading mod files metadata from: {}", files_metadata_url);

    let (files_data, http_timing) =
        match fetch_manifest_json_timed(context.clone(), &files_metadata_url).await {
            Ok(r) => r,
            Err(e) if is_foxy_mode => {
                // FoxyMode failed - try falling back to mod.srf (HybridMode safety net)
                let fallback_url = format!("{}/mod.srf", mod_parent.remote_path);
                warn!(
                    "foxy_addon.json fetch failed for {}, trying mod.srf fallback: {}",
                    mod_parent.remote_path, e
                );
                match fetch_manifest_json_timed(context.clone(), &fallback_url).await {
                    Ok(r) => r,
                    Err(e2) => {
                        error!(
                            "Unable to fetch mod files metadata (foxy + fallback): {} : {}",
                            files_metadata_url, e2
                        );
                        return Err(empty_recheck_stats(
                            &mod_parent,
                            zero_fetch_timing(),
                            mod_start.elapsed(),
                        ));
                    }
                }
            }
            Err(e) => {
                error!(
                    "Unable to fetch mod files metadata: {} : {}",
                    files_metadata_url, e
                );
                return Err(empty_recheck_stats(
                    &mod_parent,
                    zero_fetch_timing(),
                    mod_start.elapsed(),
                ));
            }
        };

    let manifest_parse_start = Instant::now();
    let manifest: ModManifest = match serde_json::from_value(files_data) {
        Ok(m) => m,
        Err(e) => {
            warn!("Failed to deserialize mod manifest: {}", e);
            return Err(empty_recheck_stats(
                &mod_parent,
                http_timing,
                mod_start.elapsed(),
            ));
        }
    };
    // Total parse = JSON-to-Value (in fetch_json) + Value-to-ModManifest
    let total_parse_duration = http_timing.parse + manifest_parse_start.elapsed();

    let file_keys: Vec<StagedFileKey> = manifest
        .files
        .into_iter()
        .enumerate()
        .map(|(index, mf)| {
            let file_name = mf.path.replace('\\', "/");
            let remote_path = join_path(&mod_parent.remote_path, &file_name);
            let local_path = join_path(&mod_parent.local_path, &file_name);
            StagedFileKey {
                name: file_name,
                remote_checksum: mf.checksum,
                length: mf.length,
                remote_path,
                local_path,
                parts: mf.parts,
                data_order: index as i64,
            }
        })
        .collect();

    Ok(StagedModFiles {
        mod_parent,
        file_keys,
        http_download_duration: http_timing.download,
        http_response_bytes: http_timing.response_bytes,
        http_parse_duration: total_parse_duration,
        manifest_cached: http_timing.cached,
        mod_start,
    })
}

pub(crate) async fn apply_mod_file_rows(
    context: Arc<FoxyContext>,
    staged: StagedModFiles,
    upsert: &FileUpsertResult,
) -> ModRecheckStats {
    let StagedModFiles {
        mod_parent,
        file_keys,
        http_download_duration,
        http_response_bytes,
        http_parse_duration,
        manifest_cached,
        mod_start,
    } = staged;
    let mut all_rows_with_json = Vec::new();
    let mut desired_file_ids: HashSet<i64> = HashSet::new();
    let mut can_prune_file_links = true;
    let mut total_bytes: u64 = 0;
    let mut total_parts: usize = 0;

    if !file_keys.is_empty() {
        let db = context.db();
        let manifest_file_ids: HashSet<i64> = file_keys
            .iter()
            .filter_map(|row| {
                let key = file_identity_key(&row.remote_path, &row.local_path);
                upsert.files.get(&key).map(|file| file.id as i64)
            })
            .collect();
        let file_graph_states = load_file_remote_graph_states(&db, &manifest_file_ids).await;

        for row in file_keys {
            let key = file_identity_key(&row.remote_path, &row.local_path);
            if let Some(file) = upsert.files.get(&key) {
                desired_file_ids.insert(file.id as i64);

                total_bytes = total_bytes.saturating_add(row.length as u64);
                total_parts += row.parts.len();
                let previous = upsert.previous.get(&key);
                let remote_graph_unchanged = previous.is_some_and(|existing| {
                    existing.remote_checksum == file.remote_checksum
                        && existing.length == file.length
                        && normalize_local_path_key(&existing.local_path)
                            == normalize_local_path_key(&file.local_path)
                });
                let graph_complete = file_graph_states
                    .get(&(file.id as i64))
                    .copied()
                    .unwrap_or_default()
                    .complete();
                let local_file_ready =
                    file_present_with_expected_len(&file.local_path, file.length);
                if remote_graph_unchanged
                    && graph_complete
                    && local_file_ready
                    && context.recheck_level < RecheckLevel::FILE
                {
                    debug!(
                        "Keeping existing remote file graph: {} (parts={}, local_checksum_match={})",
                        file.remote_path,
                        file_graph_states
                            .get(&(file.id as i64))
                            .map(|state| state.part_count)
                            .unwrap_or(0),
                        file.remote_checksum == file.local_checksum
                    );
                    continue;
                }
                all_rows_with_json.push((file.clone(), previous.cloned(), row.parts));
            } else {
                warn!("File record missing after upsert: {}", row.remote_path);
                can_prune_file_links = false;
            }
        }
    }

    reconcile_addon_file_links(
        context.clone(),
        mod_parent.clone(),
        desired_file_ids,
        can_prune_file_links,
    )
    .await;

    let parts_start = Instant::now();
    let total_files = all_rows_with_json.len();
    let part_limit = part_task_limit();
    let semaphore = Arc::new(Semaphore::new(part_limit));

    // Perform recheck in bulk to minimize DB round-trips while still guarding SQLite with a semaphore
    let mut needs_parts: Vec<FilePartsPayload> = Vec::new();
    for (file, previous_file, parts) in all_rows_with_json {
        let local_file_ready = file_present_with_expected_len(&file.local_path, file.length);
        if file.remote_checksum == file.local_checksum
            && local_file_ready
            && context.recheck_level < RecheckLevel::FILE
        {
            debug!("Up-to-date: File: {}", file.remote_path);
            continue;
        }

        debug!("Recheck needed: File: {}", file.remote_path);
        needs_parts.push(FilePartsPayload {
            file,
            previous_file,
            parts: parts
                .into_iter()
                .enumerate()
                .map(|(idx, p)| FilePartData {
                    path: p.path.replace('\\', "/"),
                    checksum: p.checksum,
                    start: p.start,
                    length: p.length,
                    data_order: idx as i64,
                })
                .collect(),
        });
    }

    // Split large mods into sub-batches and process concurrently. We split by
    // part count (not just file count) so a mod like @csa38 (76 files, 460K parts)
    // fans into multiple concurrent batches instead of just 2.
    const PARTS_PER_BATCH: usize = 30_000;
    const FILES_PER_BATCH_MIN: usize = 4;

    let sub_batches = build_parts_sub_batches(&needs_parts, PARTS_PER_BATCH, FILES_PER_BATCH_MIN);
    let sub_batch_count = sub_batches.len();

    if sub_batch_count <= 1 {
        if !needs_parts.is_empty() {
            let _permit = semaphore.clone().acquire_owned().await.ok();
            remote_file_parts_batch(context.clone(), needs_parts).await;
        }
    } else {
        let mut handles = Vec::with_capacity(sub_batch_count);
        for sub_batch in sub_batches {
            let ctx = context.clone();
            let sem = semaphore.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.ok();
                remote_file_parts_batch(ctx, sub_batch).await;
            }));
        }
        for handle in handles {
            let _ = handle.await;
        }
    }

    let parts_persist_duration = parts_start.elapsed();

    ModRecheckStats {
        mod_path: mod_parent.remote_path.clone(),
        files: total_files,
        parts: total_parts,
        bytes: total_bytes,
        mod_concurrency_limit: sub_batch_count,
        duration: mod_start.elapsed(),
        http_download_duration,
        http_response_bytes,
        http_parse_duration,
        manifest_cached,
        file_upsert_duration: std::time::Duration::ZERO,
        parts_persist_duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── join_path ───────────────────────────────────────────────────────

    #[test]
    fn join_path_trailing_forward_slash() {
        assert_eq!(
            join_path("https://repo.com/mod/", "file.pbo"),
            "https://repo.com/mod/file.pbo"
        );
    }

    #[test]
    fn join_path_no_trailing_slash() {
        assert_eq!(
            join_path("https://repo.com/mod", "file.pbo"),
            "https://repo.com/mod/file.pbo"
        );
    }

    #[test]
    fn join_path_trailing_backslash() {
        assert_eq!(
            join_path("C:\\mods\\ace\\", "addons\\ace_main.pbo"),
            "C:\\mods\\ace\\addons\\ace_main.pbo"
        );
    }

    // ── normalize_local_path_key ────────────────────────────────────────

    #[test]
    fn normalize_local_path_key_delegates_to_content_hash() {
        let result = normalize_local_path_key("C:\\mods\\ace/");
        assert!(!result.ends_with('/'));
        assert!(!result.contains('\\'));
    }

    // ── file_present_with_expected_len ──────────────────────────────────

    #[test]
    fn file_identity_key_keeps_same_remote_under_different_local_roots_separate() {
        let remote_path = "https://repo.example/@ace/addons/ace_main.pbo";
        let old_key = file_identity_key(remote_path, "R:/Mods/MainRepo/@ace/addons/ace_main.pbo");
        let new_key = file_identity_key(remote_path, "R:/Mods/other/@ace/addons/ace_main.pbo");

        assert_ne!(old_key, new_key);
    }

    #[test]
    fn file_identity_key_normalizes_equivalent_local_paths() {
        let remote_path = "https://repo.example/@ace/addons/ace_main.pbo";
        let forward = file_identity_key(remote_path, "R:/Mods/Repo/@ace/addons/ace_main.pbo/");
        let backward = file_identity_key(remote_path, "R:\\Mods\\Repo\\@ace\\addons\\ace_main.pbo");

        assert_eq!(forward, backward);
    }

    #[test]
    fn file_present_with_expected_len_correct() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.bin");
        std::fs::write(&file, b"hello").unwrap();
        assert!(file_present_with_expected_len(&file.to_string_lossy(), 5));
    }

    #[test]
    fn file_present_with_expected_len_wrong_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.bin");
        std::fs::write(&file, b"hello").unwrap();
        assert!(!file_present_with_expected_len(
            &file.to_string_lossy(),
            999
        ));
    }

    #[test]
    fn file_present_with_expected_len_missing_file() {
        assert!(!file_present_with_expected_len("/nonexistent/file.bin", 0));
    }

    #[test]
    fn file_present_with_expected_len_directory_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!file_present_with_expected_len(
            &dir.path().to_string_lossy(),
            0
        ));
    }

    #[test]
    fn file_remote_graph_state_complete_requires_parts() {
        assert!(
            FileRemoteGraphState {
                part_count: 1,
                missing_part_remote_checksums: 0,
            }
            .complete()
        );
        assert!(
            !FileRemoteGraphState {
                part_count: 0,
                missing_part_remote_checksums: 0,
            }
            .complete()
        );
    }

    #[test]
    fn file_remote_graph_state_incomplete_with_missing_part_checksum() {
        assert!(
            !FileRemoteGraphState {
                part_count: 4,
                missing_part_remote_checksums: 1,
            }
            .complete()
        );
    }

    // ── ModManifest deserialization ─────────────────────────────────────

    #[test]
    fn deserialize_camel_case_manifest() {
        let data = json!({
            "files": [
                {
                    "path": "addons/ace_main.pbo",
                    "checksum": "ABC123",
                    "length": 1024,
                    "parts": [
                        {"path": "addons/ace_main.pbo.zsync.0", "checksum": "DEF", "start": 0, "length": 512}
                    ]
                }
            ]
        });
        let manifest: ModManifest = serde_json::from_value(data).unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].path, "addons/ace_main.pbo");
        assert_eq!(manifest.files[0].checksum, "ABC123");
        assert_eq!(manifest.files[0].length, 1024);
        assert_eq!(manifest.files[0].parts.len(), 1);
        assert_eq!(manifest.files[0].parts[0].start, 0);
    }

    #[test]
    fn deserialize_pascal_case_manifest() {
        let data = json!({
            "Files": [
                {
                    "Path": "addons/ace_main.pbo",
                    "Checksum": "ABC123",
                    "Length": 2048,
                    "Parts": []
                }
            ]
        });
        let manifest: ModManifest = serde_json::from_value(data).unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert_eq!(manifest.files[0].length, 2048);
    }

    #[test]
    fn deserialize_manifest_no_parts_defaults_empty() {
        let data = json!({
            "files": [
                {"path": "test.pbo", "checksum": "X", "length": 100}
            ]
        });
        let manifest: ModManifest = serde_json::from_value(data).unwrap();
        assert!(manifest.files[0].parts.is_empty());
    }

    #[test]
    fn deserialize_manifest_multiple_files() {
        let data = json!({
            "files": [
                {"path": "a.pbo", "checksum": "A", "length": 10},
                {"path": "b.pbo", "checksum": "B", "length": 20},
                {"path": "c.pbo", "checksum": "C", "length": 30}
            ]
        });
        let manifest: ModManifest = serde_json::from_value(data).unwrap();
        assert_eq!(manifest.files.len(), 3);
    }

    // ── part_task_limit ─────────────────────────────────────────────────

    #[test]
    fn part_task_limit_within_bounds() {
        let limit = part_task_limit();
        assert!(limit >= 8, "limit should be at least 8, got {}", limit);
    }

    // ── sub-batch splitting for large mods ─────────────────────────────

    // ── build_parts_sub_batches ─────────────────────────────────────────

    use crate::core::tasks::remote_file_parts::FilePartData;

    fn make_payload(parts_count: usize) -> FilePartsPayload {
        FilePartsPayload {
            file: FoxyModFile::default(),
            previous_file: None,
            parts: (0..parts_count)
                .map(|i| FilePartData {
                    path: String::new(),
                    checksum: String::new(),
                    start: 0,
                    length: 0,
                    data_order: i as i64,
                })
                .collect(),
        }
    }

    #[test]
    fn sub_batch_empty_returns_empty() {
        let result = build_parts_sub_batches(&[], 30_000, 4);
        assert!(result.is_empty());
    }

    #[test]
    fn sub_batch_small_mod_stays_single() {
        let files: Vec<_> = (0..10).map(|_| make_payload(100)).collect();
        let result = build_parts_sub_batches(&files, 30_000, 4);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 10);
    }

    #[test]
    fn sub_batch_splits_by_part_count() {
        // 4 files with 10K parts each = 40K total, budget=30K → 2 batches
        let files: Vec<_> = (0..4).map(|_| make_payload(10_000)).collect();
        let result = build_parts_sub_batches(&files, 30_000, 1);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn sub_batch_csa38_like_splits_into_many() {
        // Simulate @csa38: 76 files, ~6K parts each = ~460K total
        let files: Vec<_> = (0..76).map(|_| make_payload(6_060)).collect();
        let result = build_parts_sub_batches(&files, 30_000, 4);
        // ~460K parts / 30K budget ≈ 15-16 batches
        assert!(
            result.len() >= 10,
            "expected >=10 batches for 460K parts, got {}",
            result.len()
        );
    }

    #[test]
    fn sub_batch_respects_min_files() {
        // 2 files with 50K parts each, min_files=4
        // Should NOT split because each batch must have >=4 files
        let files: Vec<_> = (0..2).map(|_| make_payload(50_000)).collect();
        let result = build_parts_sub_batches(&files, 30_000, 4);
        assert_eq!(result.len(), 1);
    }

    // ── reconcile chunking ─────────────────────────────────────────────

    #[test]
    fn reconcile_addon_file_insert_uses_tuned_write_chunk() {
        let chunk_size = crate::core::tasks::init_database::bulk_write_rows_for(2);
        assert_eq!(
            chunk_size,
            crate::core::tasks::init_database::bulk_write_chunk_rows()
        );
    }

    fn staged_file(mod_name: &str, file_name: &str, checksum: &str) -> StagedModFiles {
        let remote_path = format!("https://repo.example/{mod_name}/{file_name}");
        let local_path = format!("C:/mods/{mod_name}/{file_name}");
        StagedModFiles {
            mod_parent: Arc::new(FoxyMod {
                name: mod_name.to_string(),
                remote_path: format!("https://repo.example/{mod_name}"),
                local_path: format!("C:/mods/{mod_name}"),
                ..Default::default()
            }),
            file_keys: vec![StagedFileKey {
                name: file_name.to_string(),
                remote_checksum: checksum.to_string(),
                length: 10,
                remote_path,
                local_path,
                parts: Vec::new(),
                data_order: 0,
            }],
            http_download_duration: std::time::Duration::ZERO,
            http_response_bytes: 0,
            http_parse_duration: std::time::Duration::ZERO,
            manifest_cached: false,
            mod_start: Instant::now(),
        }
    }

    #[tokio::test]
    async fn batched_file_upsert_maps_ids_across_mods() {
        use crate::core::tasks::db_turso::build_test_database;
        let db = FoxyDb::from_turso(build_test_database().await);
        let staged = vec![
            staged_file("@one", "a.pbo", "aa"),
            staged_file("@two", "b.pbo", "bb"),
        ];
        let result = upsert_file_rows_batch(&db, &staged).await;
        assert_eq!(result.files.len(), 2);
        for row in staged.iter().flat_map(|s| s.file_keys.iter()) {
            let key = file_identity_key(&row.remote_path, &row.local_path);
            let file = result
                .files
                .get(&key)
                .expect("batched upsert must map identity to a row id");
            assert!(file.id > 0);
            assert_eq!(file.remote_checksum, row.remote_checksum);
            assert_eq!(file.name, row.name);
        }
    }
}
