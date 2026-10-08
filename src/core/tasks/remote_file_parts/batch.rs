use super::types::{FilePartsPayload, PartRow};
use super::validation::{local_file_matches_part_layout, log_suspicious_manifest_paths};
use crate::core::db::{DbErr, DbTxn, DbValue, FoxyDb};
use crate::core::models::context::{DeferredPartInsert, FoxyContext};
use crate::core::models::download_patch_file::delete_download_patch_files_by_file_ids;
use crate::core::models::download_patch_op::delete_download_patch_ops_for_files;
use crate::core::models::modification_file::FoxyModFile;
use crate::core::models::modification_file_part::{
    FoxyModFilePart, SUBFILE_COLUMNS, part_display_path, part_storage_path,
};
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::tasks::db_turso::{SUBFILES_INDEX_CREATE_SQL, SUBFILES_INDEX_NAMES};
use crate::core::tasks::delta_patch::{persist_patch_plan, plan_file_patch};
use crate::core::tasks::init_database::{sqlite_labeled_write_scope, sqlite_perf_snapshot};
use log::{debug, error, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn force_full_file_download(force_download_targets: bool, force_full_downloads: bool) -> bool {
    force_download_targets || force_full_downloads
}

const FILE_PART_UPSERT_PARAMS_PER_ROW: usize = 6;
const FILE_PART_INSERT_WITH_LOCAL_PARAMS_PER_ROW: usize = 9;
const DOWNLOAD_FILE_TARGET_UPSERT_PARAMS_PER_ROW: usize = 4;

/// A subfile (file part) row staged for the bulk remote-metadata upsert.
struct PartUpsertRow {
    file_id: i64,
    path: String,
    remote_length: i64,
    remote_start: i64,
    remote_checksum: String,
    data_order: i64,
}

/// A download_target_file row staged for the bulk queue rebuild.
struct DownloadTargetRow {
    file_id: i64,
    download_remote_url: String,
    download_local_path: String,
    size: i64,
}

fn file_part_upsert_chunk_size() -> usize {
    // Turso writes are fastest in small multi-row statements (see
    crate::core::tasks::init_database::bulk_write_rows_for(FILE_PART_UPSERT_PARAMS_PER_ROW)
}

/// Build the bulk subfile upsert SQL for `rows` rows. The local_* columns use
/// inline defaults (0, 0, '') so they consume no bind variables.
///
/// When `fresh` is true (a whole-wipe force-redownload / first-download load into
/// a globally-empty, index-deferred `subfiles` table - see
/// `after_turso_regression_analysis5.md` P0-b), the statement is a plain `INSERT`:
/// the table is empty so no conflict can occur, the unique index is dropped, and
/// dropping the `ON CONFLICT` arm lets the row append touch only the rowid PK.
/// Otherwise it is `INSERT … ON CONFLICT (file_id, path) DO UPDATE` that refreshes
/// only remote metadata, preserving any existing local checksums.
fn file_part_upsert_sql(rows: usize, fresh: bool) -> String {
    let placeholders = vec!["(?, ?, 0, 0, ?, ?, '', ?, ?)"; rows].join(", ");
    let insert = format!(
        "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) VALUES {placeholders}"
    );
    if fresh {
        insert
    } else {
        format!(
            "{insert} ON CONFLICT (file_id, path) DO UPDATE SET remote_length = excluded.remote_length, remote_start = excluded.remote_start, remote_checksum = excluded.remote_checksum, data_order = excluded.data_order"
        )
    }
}

fn file_part_upsert_values(chunk: &[PartUpsertRow]) -> Vec<DbValue> {
    let mut values = Vec::with_capacity(chunk.len() * FILE_PART_UPSERT_PARAMS_PER_ROW);
    for row in chunk {
        values.push(row.file_id.into());
        values.push(row.path.clone().into());
        values.push(row.remote_length.into());
        values.push(row.remote_start.into());
        values.push(row.remote_checksum.clone().into());
        values.push(row.data_order.into());
    }
    values
}

fn file_part_insert_with_local_chunk_size() -> usize {
    crate::core::tasks::init_database::bulk_write_rows_for(
        FILE_PART_INSERT_WITH_LOCAL_PARAMS_PER_ROW,
    )
}

fn file_part_insert_with_local_sql(rows: usize, fresh: bool) -> String {
    let placeholders = vec!["(?, ?, ?, ?, ?, ?, ?, ?, ?)"; rows].join(", ");
    let insert = format!(
        "INSERT INTO subfiles \
         (file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) \
         VALUES {placeholders}"
    );
    if fresh {
        insert
    } else {
        format!(
            "{insert} \
         ON CONFLICT (file_id, path) DO UPDATE SET \
            local_length = excluded.local_length, \
            local_start = excluded.local_start, \
            remote_length = excluded.remote_length, \
            remote_start = excluded.remote_start, \
            local_checksum = excluded.local_checksum, \
            remote_checksum = excluded.remote_checksum, \
            data_order = excluded.data_order"
        )
    }
}

fn file_part_insert_with_local_ref_values(chunk: &[&FoxyModFilePart]) -> Vec<DbValue> {
    let mut values = Vec::with_capacity(chunk.len() * FILE_PART_INSERT_WITH_LOCAL_PARAMS_PER_ROW);
    for part in chunk {
        values.push((part.file_id as i64).into());
        values.push(part.path.clone().into());
        values.push((part.local_length as i64).into());
        values.push((part.local_start as i64).into());
        values.push((part.remote_length as i64).into());
        values.push((part.remote_start as i64).into());
        values.push(part.local_checksum.clone().into());
        values.push(part.remote_checksum.clone().into());
        values.push(part.data_order.into());
    }
    values
}

fn download_file_target_upsert_chunk_size() -> usize {
    crate::core::tasks::init_database::bulk_write_rows_for(
        DOWNLOAD_FILE_TARGET_UPSERT_PARAMS_PER_ROW,
    )
}

fn download_file_target_upsert_sql(rows: usize) -> String {
    let placeholders = vec!["(?, ?, ?, ?, 0, 0)"; rows].join(", ");
    format!(
        "INSERT INTO download_target_file (file_id, download_remote_url, download_local_path, size, download_total, download_cycle) VALUES {} ON CONFLICT (file_id) DO UPDATE SET download_remote_url = excluded.download_remote_url, download_local_path = excluded.download_local_path, size = excluded.size, download_total = excluded.download_total, download_cycle = excluded.download_cycle",
        placeholders
    )
}

fn download_file_target_upsert_values(chunk: &[DownloadTargetRow]) -> Vec<DbValue> {
    let mut values = Vec::with_capacity(chunk.len() * DOWNLOAD_FILE_TARGET_UPSERT_PARAMS_PER_ROW);
    for row in chunk {
        values.push(row.file_id.into());
        values.push(row.download_remote_url.clone().into());
        values.push(row.download_local_path.clone().into());
        values.push(row.size.into());
    }
    values
}

fn should_prepare_download_work(
    queue_download_targets: bool,
    patch_plan_metadata_refresh: bool,
) -> bool {
    queue_download_targets || patch_plan_metadata_refresh
}

/// Load existing subfile rows for the given file ids, keyed by `(file_id, path)`.
/// Chunked under the SQLite bind limit.
async fn load_parts_by_file_ids(
    db: &FoxyDb,
    file_ids: &[i64],
) -> Result<HashMap<(i64, String), FoxyModFilePart>, DbErr> {
    let mut map = HashMap::new();
    let chunk_size = crate::core::tasks::init_database::read_chunk_ids();
    for ids in file_ids.chunks(chunk_size) {
        let placeholders = vec!["?"; ids.len()].join(", ");
        let sql =
            format!("SELECT {SUBFILE_COLUMNS} FROM subfiles WHERE file_id IN ({placeholders})");
        let values: Vec<DbValue> = ids.iter().copied().map(DbValue::from).collect();
        for row in db.query_all(&sql, values).await? {
            let part = FoxyModFilePart::from_row(&row)?;
            map.insert((part.file_id as i64, part.path.clone()), part);
        }
    }
    Ok(map)
}

/// Clear stale delta patch plans for a batch of files in two bulk transactions
/// instead of two per file. Callers collect the file IDs that proved
/// unpatchable during planning and flush them once at the end.
async fn clear_stale_patch_plans(context: Arc<FoxyContext>, file_ids: &[i64], reason: &str) {
    if file_ids.is_empty() {
        return;
    }
    if let Err(err) = delete_download_patch_ops_for_files(context.clone(), file_ids).await {
        warn!(
            "Failed to bulk-clear stale delta patch ops for {} files (reason={}): {}",
            file_ids.len(),
            reason,
            err
        );
    }
    if let Err(err) = delete_download_patch_files_by_file_ids(context, file_ids).await {
        warn!(
            "Failed to bulk-clear stale delta patch rows for {} files (reason={}): {}",
            file_ids.len(),
            reason,
            err
        );
    }
}

pub(crate) async fn flush_pending_patch_clears(context: Arc<FoxyContext>) {
    let mut file_ids = context.take_pending_patch_clear_ids();
    if file_ids.is_empty() {
        return;
    }
    file_ids.sort_unstable();
    file_ids.dedup();
    info!(
        "Bulk-clearing stale delta patch plans for {} files",
        file_ids.len()
    );
    clear_stale_patch_plans(context, &file_ids, "unpatchable file").await;
}

pub(crate) async fn flush_pending_download_targets(context: Arc<FoxyContext>) {
    let pending = context.take_pending_download_targets();
    if pending.is_empty() {
        return;
    }
    let rows: Vec<DownloadTargetRow> = pending
        .into_iter()
        .map(|row| DownloadTargetRow {
            file_id: row.file_id,
            download_remote_url: row.download_remote_url,
            download_local_path: row.download_local_path,
            size: row.size,
        })
        .collect();
    let chunk_size = download_file_target_upsert_chunk_size();
    info!(
        "Download target rebuild batch: file_targets={} file_insert_chunks={}",
        rows.len(),
        rows.len().div_ceil(chunk_size)
    );
    let db = context.db();
    let rows = Arc::new(rows);
    if let Err(e) = db
        .transaction("download target rebuild", |txn| {
            let rows = rows.clone();
            Box::pin(async move {
                for chunk in rows.chunks(chunk_size) {
                    let sql = download_file_target_upsert_sql(chunk.len());
                    txn.execute(&sql, download_file_target_upsert_values(chunk))
                        .await?;
                }
                Ok(())
            })
        })
        .await
    {
        warn!("Failed to upsert download file targets: {}", e);
    }
}

fn stale_subfile_ids(
    refreshed_parts: &HashMap<(i64, String), FoxyModFilePart>,
    desired_part_ids_by_file: &HashMap<i64, HashSet<i64>>,
    manifest_part_paths_by_file: &HashMap<i64, Vec<String>>,
) -> Vec<i64> {
    let mut stale = Vec::new();
    for ((file_id, _path), part) in refreshed_parts {
        // Only prune files whose current manifest describes a part layout.
        if manifest_part_paths_by_file
            .get(file_id)
            .is_none_or(|paths| paths.is_empty())
        {
            continue;
        }
        if let Some(desired) = desired_part_ids_by_file.get(file_id)
            && !desired.contains(&(part.id as i64))
        {
            stale.push(part.id as i64);
        }
    }
    stale
}

/// Local state a rebuilt part row must keep so the delta planner can still
/// copy the bytes already on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CarriedPartLocalState {
    id: u64,
    local_checksum: String,
    local_length: u64,
    local_start: u64,
}

/// Carry the local state of one file's previous part rows onto its rebuilt rows.
///
/// `old_parts` must already carry the derived clean state of the previous file.
/// A clean file stores no part local checksums, so once its remote checksum
/// changes that state is lost unless `materialize_existing` writes it onto the
/// rows that keep their key. A row whose key changed (the manifest inserted or
/// removed an entry before it) takes the state of an unclaimed old row with the
/// same part path.
fn carried_part_local_state(
    old_parts: &[FoxyModFilePart],
    current_parts: &[&FoxyModFilePart],
    materialize_existing: bool,
) -> Vec<CarriedPartLocalState> {
    let has_local = |part: &FoxyModFilePart| !part.local_checksum.trim().is_empty();
    let old_by_key: HashMap<&str, usize> = old_parts
        .iter()
        .enumerate()
        .map(|(idx, part)| (part.path.as_str(), idx))
        .collect();
    let mut claimed = vec![false; old_parts.len()];
    let mut carried = Vec::new();
    let mut rekeyed = Vec::new();

    for current in current_parts {
        let Some(&idx) = old_by_key.get(current.path.as_str()) else {
            rekeyed.push(*current);
            continue;
        };
        claimed[idx] = true;
        let old = &old_parts[idx];
        if materialize_existing
            && has_local(old)
            && (current.local_checksum != old.local_checksum
                || current.local_length != old.local_length
                || current.local_start != old.local_start)
        {
            carried.push(CarriedPartLocalState {
                id: current.id,
                local_checksum: old.local_checksum.clone(),
                local_length: old.local_length,
                local_start: old.local_start,
            });
        }
    }

    let mut unclaimed_by_display_path: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut unclaimed: Vec<usize> = (0..old_parts.len())
        .filter(|idx| !claimed[*idx] && has_local(&old_parts[*idx]))
        .collect();
    unclaimed.sort_by_key(|idx| old_parts[*idx].data_order);
    for idx in unclaimed {
        unclaimed_by_display_path
            .entry(part_display_path(&old_parts[idx].path))
            .or_default()
            .push(idx);
    }
    rekeyed.sort_by_key(|part| part.data_order);
    for current in rekeyed {
        if has_local(current) {
            continue;
        }
        let Some(candidates) = unclaimed_by_display_path.get_mut(part_display_path(&current.path))
        else {
            continue;
        };
        if candidates.is_empty() {
            continue;
        }
        let old = &old_parts[candidates.remove(0)];
        carried.push(CarriedPartLocalState {
            id: current.id,
            local_checksum: old.local_checksum.clone(),
            local_length: old.local_length,
            local_start: old.local_start,
        });
    }
    carried
}

async fn persist_carried_part_local_state(db: &FoxyDb, states: Vec<CarriedPartLocalState>) {
    let chunk_size = crate::core::tasks::init_database::bulk_write_rows_for(4);
    let states = Arc::new(states);
    if let Err(e) = db
        .transaction("carry part local state", |txn| {
            let states = states.clone();
            Box::pin(async move {
                for chunk in states.chunks(chunk_size) {
                    let values_rows = vec!["(?, ?, ?, ?)"; chunk.len()].join(", ");
                    let sql = format!(
                        "WITH v(id, local_checksum, local_length, local_start) AS (VALUES {values_rows}) \
                         UPDATE subfiles SET local_checksum = v.local_checksum, \
                         local_length = v.local_length, local_start = v.local_start \
                         FROM v WHERE subfiles.id = v.id"
                    );
                    let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 4);
                    for state in chunk {
                        values.push((state.id as i64).into());
                        values.push(state.local_checksum.clone().into());
                        values.push((state.local_length as i64).into());
                        values.push((state.local_start as i64).into());
                    }
                    txn.execute(&sql, values).await?;
                }
                Ok(())
            })
        })
        .await
    {
        warn!("Failed to carry part local state across manifest update: {}", e);
    }
}

/// Batch version that handles all files of a mod in a handful of queries instead of per-file round trips.
pub(crate) async fn remote_file_parts_batch(
    context: Arc<FoxyContext>,
    files: Vec<FilePartsPayload>,
) {
    if files.is_empty() {
        return;
    }

    let db = context.db();
    let force_parts_recheck = context.recheck_level >= RecheckLevel::FILE_PART;
    // Phase timing (plan.md §4 follow-up): the metadata rebuild's DB work was
    // opaque in logs, so split prefetch / upsert / reload / delta-planning here to
    // tell DB cost apart from the caller's HTTP fetch time.
    let batch_started = Instant::now();
    let mut upsert_elapsed = Duration::ZERO;
    let mut reload_elapsed = Duration::ZERO;
    let mut upsert_row_count = 0usize;

    // Parse parts upfront
    let mut all_part_rows: Vec<PartRow> = Vec::new();
    let mut order_keys: HashMap<i64, Vec<String>> = HashMap::new();
    let mut file_by_id: HashMap<i64, FoxyModFile> = HashMap::new();
    let mut previous_file_by_id: HashMap<i64, FoxyModFile> = HashMap::new();

    for payload in files.iter() {
        file_by_id.insert(payload.file.id as i64, payload.file.clone());
        if let Some(previous_file) = payload.previous_file.as_ref() {
            previous_file_by_id.insert(payload.file.id as i64, previous_file.clone());
        }
        order_keys.entry(payload.file.id as i64).or_default();

        if payload.parts.is_empty() {
            debug!(
                "File has no manifest parts metadata; falling back to whole-file handling: file_id={} path={}",
                payload.file.id, payload.file.remote_path
            );
            continue;
        }

        let mut parsed_rows = Vec::with_capacity(payload.parts.len());

        for part_data in &payload.parts {
            order_keys
                .entry(payload.file.id as i64)
                .or_default()
                .push(part_storage_path(&part_data.path, part_data.data_order));

            let row = PartRow {
                file_id: payload.file.id as i64,
                path: part_storage_path(&part_data.path, part_data.data_order),
                display_path: part_data.path.clone(),
                remote_checksum: part_data.checksum.clone(),
                length: part_data.length,
                start: part_data.start,
                data_order: part_data.data_order,
            };
            parsed_rows.push(row.clone());
            all_part_rows.push(row);
        }

        log_suspicious_manifest_paths(&payload.file, &parsed_rows);
    }

    if file_by_id.is_empty() {
        return;
    }

    let file_ids: Vec<i64> = file_by_id.keys().copied().collect();
    info!(
        "Remote file parts batch started: files={} part_rows={} queue_download_targets={} patch_plan_metadata_refresh={} force_download_targets={} defer_part_inserts={} fresh_subfiles_load={}",
        file_ids.len(),
        all_part_rows.len(),
        context.queue_download_targets,
        context.patch_plan_metadata_refresh,
        context.force_download_targets,
        context.should_defer_part_inserts(),
        context.is_fresh_subfiles_load()
    );

    // Prefetch existing parts for all files in one query
    let prefetch_started = Instant::now();
    let existing_parts: HashMap<(i64, String), FoxyModFilePart> =
        match load_parts_by_file_ids(&db, &file_ids).await {
            Ok(parts) => parts,
            Err(e) => {
                warn!("Failed to prefetch file parts batch: {}", e);
                HashMap::new()
            }
        };
    let prefetch_elapsed = prefetch_started.elapsed();
    let mut old_parts_by_file: HashMap<i64, Vec<FoxyModFilePart>> = HashMap::new();
    for ((file_id, _path), part) in &existing_parts {
        let part = if let Some(previous_file) = previous_file_by_id.get(file_id) {
            part.clone().with_derived_clean_local_state(
                &previous_file.local_checksum,
                &previous_file.remote_checksum,
            )
        } else {
            part.clone()
        };
        old_parts_by_file.entry(*file_id).or_default().push(part);
    }
    let mut upsert_models: Vec<PartUpsertRow> = Vec::new();
    let mut files_with_new_part_rows: HashSet<i64> = HashSet::new();

    for part in &all_part_rows {
        let needs_upsert = match existing_parts.get(&(part.file_id, part.path.clone())) {
            Some(existing) => {
                force_parts_recheck
                    || existing.remote_length != part.length as u64
                    || existing.remote_start != part.start as u64
                    || existing.remote_checksum != part.remote_checksum
            }
            None => {
                files_with_new_part_rows.insert(part.file_id);
                true
            }
        };

        if needs_upsert {
            upsert_models.push(PartUpsertRow {
                file_id: part.file_id,
                path: part.path.clone(),
                remote_length: part.length,
                remote_start: part.start,
                remote_checksum: part.remote_checksum.clone(),
                data_order: part.data_order,
            });
        }
    }

    let has_part_upserts = !upsert_models.is_empty();
    // A++ (after_turso_regression_analysis7.md): on a force-redownload into a
    // globally-empty `subfiles` table, buffer the brand-new part rows for one
    // background INSERT overlapped with the download instead of writing them inline
    // on `remote_repository`'s critical path (~22s). Safe because the download queue
    // is file-level (built below from the manifest, identical via the no-parts
    // branch when `refreshed_parts` is empty) and, after Step 3b, nothing before the
    // download reads `subfiles`. Gated on `is_fresh_subfiles_load` so it only applies
    // to the conflict-free whole-wipe load, never an incremental upsert.
    let defer_parts =
        has_part_upserts && context.should_defer_part_inserts() && context.is_fresh_subfiles_load();
    if defer_parts {
        upsert_row_count = upsert_models.len();
        info!(
            "File part metadata deferred for background insert: files={} rows={}",
            file_ids.len(),
            upsert_models.len()
        );
        context.buffer_deferred_parts(
            upsert_models
                .iter()
                .map(|row| DeferredPartInsert {
                    file_id: row.file_id,
                    path: row.path.clone(),
                    remote_length: row.remote_length,
                    remote_start: row.remote_start,
                    remote_checksum: row.remote_checksum.clone(),
                    data_order: row.data_order,
                })
                .collect(),
        );
    }
    if has_part_upserts && !defer_parts {
        // Use raw SQL with 6 bound params per row (file_id, path, remote_length, remote_start,
        // remote_checksum, data_order). The local_* columns use inline defaults (0, 0, '') so
        // they don't consume bind variables. ON CONFLICT updates only remote metadata (4 cols),
        // preserving any existing local checksums from prior hash runs.
        // Keep all insert chunks in one writer window. SQLite serializes writes even in WAL mode,
        // so committing every small chunk creates lock handoff churn on part-heavy manifests.
        let chunk_size = file_part_upsert_chunk_size();
        // P0-b: on a whole-wipe force-redownload the rebuild emptied subfiles and
        // dropped its indexes, so this load is a conflict-free plain INSERT into a
        // rowid-PK-only table (4→1 B-trees/row). The metadata rebuild rebuilds the
        // indexes once after the parallel fan-out completes.
        let fresh = context.is_fresh_subfiles_load();
        upsert_row_count = upsert_models.len();
        info!(
            "File part metadata upsert batch: files={} rows={} raw_insert_chunks={} fresh={}",
            file_ids.len(),
            upsert_models.len(),
            upsert_models.len().div_ceil(chunk_size),
            fresh
        );
        let upsert_models = Arc::new(upsert_models);
        let upsert_started = Instant::now();
        if let Err(e) = db
            .transaction("file parts upsert", |txn| {
                let upsert_models = upsert_models.clone();
                Box::pin(async move {
                    for chunk in upsert_models.chunks(chunk_size) {
                        let sql = file_part_upsert_sql(chunk.len(), fresh);
                        txn.execute(&sql, file_part_upsert_values(chunk)).await?;
                    }
                    Ok(())
                })
            })
            .await
        {
            warn!("Failed to upsert file parts batch: {}", e);
        }
        upsert_elapsed = upsert_started.elapsed();
    }

    // Refresh parts for all files -- skip re-read when nothing was upserted
    // since existing_parts already has the correct IDs and data. When the insert
    // was deferred (A++) the rows do not exist yet, so leave `refreshed_parts`
    // empty: the target build below falls to the file-level (no-parts) branch,
    // producing the same file-level download targets, and the deferred parts are
    // inserted in the background before the hasher needs them.
    let mut refreshed_parts: HashMap<(i64, String), FoxyModFilePart> =
        if !has_part_upserts || defer_parts {
            existing_parts
        } else {
            let reload_started = Instant::now();
            let reloaded = match load_parts_by_file_ids(&db, &file_ids).await {
                Ok(parts) => parts,
                Err(e) => {
                    warn!("Failed to reload file parts batch: {}", e);
                    return;
                }
            };
            reload_elapsed = reload_started.elapsed();
            reloaded
        };

    let mut desired_part_ids_by_file: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut can_prune_stale_parts = true;
    for (file_id, order) in &order_keys {
        let desired_for_file = desired_part_ids_by_file.entry(*file_id).or_default();
        for path in order {
            if let Some(part) = refreshed_parts.get(&(*file_id, path.clone())) {
                desired_for_file.insert(part.id as i64);
            } else {
                can_prune_stale_parts = false;
            }
        }
    }
    // Parts carry their file relation in subfiles.file_id. Prune stale rows
    // directly so all part readers observe the current manifest layout without
    // maintaining the former file_subfiles junction table.
    let stale_subfile_ids: Vec<i64> = if can_prune_stale_parts {
        stale_subfile_ids(&refreshed_parts, &desired_part_ids_by_file, &order_keys)
    } else {
        Vec::new()
    };

    // Delete stale subfile rows from the subfiles table so that Tree::load
    // (which queries by file_id directly) does not pick up orphaned parts.
    if !stale_subfile_ids.is_empty() {
        let chunk_size = crate::core::tasks::init_database::SQLITE_MAX_VARIABLES
            .saturating_sub(4)
            .max(1);
        for chunk in stale_subfile_ids.chunks(chunk_size) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!("DELETE FROM subfiles WHERE id IN ({placeholders})");
            let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
            if let Err(e) = db
                .execute_retry("stale subfile cleanup", &sql, values)
                .await
            {
                warn!("Failed to delete stale subfile rows: {}", e);
            }
        }
        info!(
            "Pruned {} stale subfile rows after manifest update",
            stale_subfile_ids.len()
        );
    }

    let materialize_file_ids: HashSet<i64> = previous_file_by_id
        .iter()
        .filter(|(file_id, previous)| {
            FoxyModFilePart::file_checksums_are_clean(
                &previous.local_checksum,
                &previous.remote_checksum,
            ) && file_by_id
                .get(*file_id)
                .is_some_and(|file| file.remote_checksum != previous.remote_checksum)
        })
        .map(|(file_id, _)| *file_id)
        .collect();
    let carry_file_ids: HashSet<i64> = materialize_file_ids
        .iter()
        .chain(files_with_new_part_rows.iter())
        .copied()
        .filter(|file_id| old_parts_by_file.contains_key(file_id))
        .collect();
    let mut current_parts_by_file: HashMap<i64, Vec<&FoxyModFilePart>> = HashMap::new();
    if !carry_file_ids.is_empty() {
        for ((file_id, _path), part) in &refreshed_parts {
            if carry_file_ids.contains(file_id)
                && desired_part_ids_by_file
                    .get(file_id)
                    .is_some_and(|desired| desired.contains(&(part.id as i64)))
            {
                current_parts_by_file
                    .entry(*file_id)
                    .or_default()
                    .push(part);
            }
        }
    }
    let mut carried_states = Vec::new();
    let mut carried_files = 0usize;
    for (file_id, current_parts) in &current_parts_by_file {
        let Some(old_parts) = old_parts_by_file.get(file_id) else {
            continue;
        };
        let materialize_existing = materialize_file_ids.contains(file_id);
        let carried = carried_part_local_state(old_parts, current_parts, materialize_existing);
        if !carried.is_empty() {
            carried_files += 1;
            carried_states.extend(carried);
        }
    }
    drop(current_parts_by_file);
    if !carried_states.is_empty() {
        let by_id: HashMap<u64, &CarriedPartLocalState> = carried_states
            .iter()
            .map(|state| (state.id, state))
            .collect();
        for part in refreshed_parts.values_mut() {
            if let Some(state) = by_id.get(&part.id) {
                part.local_checksum = state.local_checksum.clone();
                part.local_length = state.local_length;
                part.local_start = state.local_start;
            }
        }
        info!(
            "Carried local part state across manifest update: files={} parts={}",
            carried_files,
            carried_states.len()
        );
        persist_carried_part_local_state(&db, carried_states).await;
    }

    let prepare_download_work = should_prepare_download_work(
        context.queue_download_targets,
        context.patch_plan_metadata_refresh,
    );
    if !prepare_download_work {
        info!(
            "Metadata-only file parts batch completed without patch planning or download target rebuild: files={} part_rows={} upsert_rows={} prefetch={:.3}s upsert={:.3}s reload={:.3}s total={:.3}s",
            file_ids.len(),
            all_part_rows.len(),
            upsert_row_count,
            prefetch_elapsed.as_secs_f64(),
            upsert_elapsed.as_secs_f64(),
            reload_elapsed.as_secs_f64(),
            batch_started.elapsed().as_secs_f64(),
        );
        return;
    }

    // Build delta plans whenever local part hashes are available. Download target
    // queueing is optional, but check/update UI estimates also use patch plans.
    let mut download_file_models = Vec::new();
    let mut skipped_part_target_rows = 0usize;
    let mut planned_patch_files = 0usize;
    // Files whose stale patch plan must be cleared. Collected here and flushed in
    // two bulk transactions after the loop instead of two transactions per file.
    let mut patch_clear_file_ids: Vec<i64> = Vec::new();

    let plan_loop_started = Instant::now();
    for (file_id, order) in order_keys {
        let Some(file) = file_by_id.get(&file_id) else {
            continue;
        };
        let mut parts_for_file = Vec::new();
        for path in order {
            if let Some(part) = refreshed_parts.get(&(file_id, path.clone())) {
                parts_for_file.push(
                    part.clone().with_derived_clean_local_state(
                        &file.local_checksum,
                        &file.remote_checksum,
                    ),
                );
            }
        }

        if parts_for_file.is_empty() {
            let local_file_ready = crate::core::utils::profiling::fs::metadata(&file.local_path)
                .map(|meta| meta.is_file() && meta.len() == file.length)
                .unwrap_or(false);
            let needs_file_download = context.force_download_targets
                || !local_file_ready
                || file.remote_checksum != file.local_checksum;
            if needs_file_download {
                patch_clear_file_ids.push(file.id as i64);
            }
            if needs_file_download && context.queue_download_targets {
                debug!(
                    "File queued for full download without manifest parts: file_id={} path={} local_file_ready={} file_tree_match={}",
                    file.id,
                    file.remote_path,
                    local_file_ready,
                    file.remote_checksum == file.local_checksum
                );
                download_file_models.push(DownloadTargetRow {
                    file_id: file.id as i64,
                    download_remote_url: file.remote_path.clone(),
                    download_local_path: file.local_path.clone(),
                    size: file.length as i64,
                });
            }
            continue;
        }

        let layout_matches = local_file_matches_part_layout(file, &parts_for_file);
        let mismatched_parts: Vec<&FoxyModFilePart> = parts_for_file
            .iter()
            .filter(|p| p.remote_checksum != p.local_checksum)
            .collect();
        let total_parts = parts_for_file.len();
        let changed_parts = mismatched_parts.len();
        let changed_parts_percent = if total_parts == 0 {
            0.0
        } else {
            (changed_parts as f64 * 100.0) / total_parts as f64
        };
        let total_parts_bytes: u64 = parts_for_file.iter().map(|p| p.remote_length).sum();
        let changed_parts_bytes: u64 = mismatched_parts.iter().map(|p| p.remote_length).sum();
        let changed_bytes_percent = if total_parts_bytes == 0 {
            0.0
        } else {
            (changed_parts_bytes as f64 * 100.0) / total_parts_bytes as f64
        };
        let has_any_mismatch = !mismatched_parts.is_empty();
        let needs_file_download = context.force_download_targets
            || !layout_matches
            || has_any_mismatch
            || file.remote_checksum != file.local_checksum;
        if needs_file_download {
            let file_tree_match = file.remote_checksum == file.local_checksum;
            debug!(
                "File queued for download: file_id={} path={} layout_matches={} file_tree_match={} changed_parts={}/{} ({:.2}%) changed_bytes={}/{} ({:.2}%)",
                file.id,
                file.remote_path,
                layout_matches,
                file_tree_match,
                changed_parts,
                total_parts,
                changed_parts_percent,
                changed_parts_bytes,
                total_parts_bytes,
                changed_bytes_percent
            );
            if !mismatched_parts.is_empty() {
                let max_logged = 12usize;
                for part in mismatched_parts.iter().take(max_logged) {
                    debug!(
                        "Part mismatch: file_id={} order={} path={} start={} length={} local_checksum={} remote_checksum={}",
                        file.id,
                        part.data_order,
                        part_display_path(&part.path),
                        part.remote_start,
                        part.remote_length,
                        part.local_checksum,
                        part.remote_checksum
                    );
                }
                if mismatched_parts.len() > max_logged {
                    debug!(
                        "Part mismatch logging truncated for file_id={} ({} additional parts omitted)",
                        file.id,
                        mismatched_parts.len() - max_logged
                    );
                }
            }
            // Missing-file fast path: a file with no local copy on disk can never
            // be patched (delta planning requires the current local file) and
            // always needs a full download. Skip plan_file_patch entirely and just
            // mark its stale plan for clearing. The metadata probe only runs for
            // files already proven to need a download, so clean files pay nothing.
            let local_file_present = crate::core::utils::profiling::fs::metadata(&file.local_path)
                .map(|meta| meta.is_file())
                .unwrap_or(false);
            if force_full_file_download(
                context.force_download_targets,
                context.force_full_downloads,
            ) {
                patch_clear_file_ids.push(file.id as i64);
            } else if !local_file_present {
                debug!(
                    "Skipping delta planning for missing local file: file_id={} path={}",
                    file.id, file.remote_path
                );
                patch_clear_file_ids.push(file.id as i64);
            } else {
                let old_parts_snapshot =
                    old_parts_by_file.get(&file_id).cloned().unwrap_or_default();
                match plan_file_patch(file, &parts_for_file, &old_parts_snapshot) {
                    Ok(Some(plan)) => {
                        if let Err(err) = persist_patch_plan(context.clone(), &plan).await {
                            warn!(
                                "Failed to persist delta patch plan for file_id={} path={}: {}",
                                file.id, file.remote_path, err
                            );
                        } else {
                            planned_patch_files += 1;
                        }
                    }
                    Ok(None) => {
                        patch_clear_file_ids.push(file.id as i64);
                    }
                    Err(err) => {
                        patch_clear_file_ids.push(file.id as i64);
                        warn!(
                            "Failed to build delta patch plan for file_id={} path={}: {}",
                            file.id, file.remote_path, err
                        );
                    }
                }
            }
            if context.queue_download_targets {
                download_file_models.push(DownloadTargetRow {
                    file_id: file.id as i64,
                    download_remote_url: file.remote_path.clone(),
                    download_local_path: file.local_path.clone(),
                    size: file.length as i64,
                });
                // The active downloader consumes file-level targets and delta patch
                // plans separately. Do not persist one unused queue row per changed
                // part for part-heavy PBOs.
                skipped_part_target_rows += changed_parts;
            }
        }
    }

    let plan_loop_elapsed = plan_loop_started.elapsed();
    info!(
        "Remote file parts batch timings: files={} part_rows={} upsert_rows={} planned_patches={} skipped_part_targets={} prefetch={:.3}s upsert={:.3}s reload={:.3}s plan_loop={:.3}s total={:.3}s",
        file_ids.len(),
        all_part_rows.len(),
        upsert_row_count,
        planned_patch_files,
        skipped_part_target_rows,
        prefetch_elapsed.as_secs_f64(),
        upsert_elapsed.as_secs_f64(),
        reload_elapsed.as_secs_f64(),
        plan_loop_elapsed.as_secs_f64(),
        batch_started.elapsed().as_secs_f64(),
    );

    // Flush all stale patch-plan clears in two bulk transactions after the
    // metadata fan-out rather than two transactions per mod. A force-redownload
    // purge already emptied these tables, so skip the no-op deletes.
    if !patch_clear_file_ids.is_empty() && !context.force_download_targets {
        context.buffer_patch_clear_ids(patch_clear_file_ids);
    }

    if !context.queue_download_targets {
        debug!(
            "Skipping download target rebuild during check-only metadata refresh (files={}, patch_plans={})",
            file_by_id.len(),
            planned_patch_files
        );
        return;
    }

    if !download_file_models.is_empty() {
        context.buffer_download_targets(download_file_models.into_iter().map(|row| {
            crate::core::models::context::PendingDownloadTarget {
                file_id: row.file_id,
                download_remote_url: row.download_remote_url,
                download_local_path: row.download_local_path,
                size: row.size,
            }
        }));
    }
}

/// Flush every part row staged by the deferred-insert path (A++,
/// after_turso_regression_analysis7.md) in one background transaction. Runs
/// concurrently with the download; the pipeline awaits the spawning task before the
/// incremental hasher loads its tree, so the rows are present by the time any reader
/// needs them. A plain conflict-free `INSERT` (the deferred load always targets a
/// globally-empty `subfiles` table - same `fresh=true` SQL as the inline fast path).
/// No-op when nothing was deferred.
pub(crate) async fn flush_deferred_part_inserts(context: Arc<FoxyContext>) -> bool {
    let (mut rows, streamed) = context.take_unpersisted_deferred_parts();
    context.set_deferred_part_inserts_fresh_load(false);
    if rows.is_empty() {
        if streamed > 0 {
            info!(
                "Deferred file part insert skipped: all {streamed} rows were streamed during the fetch"
            );
        }
        return true;
    }
    // Both subfiles indexes lead with file_id, and the buffer arrives in mod
    // completion order, so inserting unsorted walks the index B-trees at random.
    rows.sort_unstable_by_key(|row| (row.file_id, row.data_order));
    let total = rows.len();
    let db = context.db();
    let chunk_size = file_part_upsert_chunk_size();
    let started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();
    let rows = Arc::new(rows);
    let mut insert_batches = 0usize;
    let mut insert_rows_affected = 0u64;
    let file_ids = Arc::new(distinct_file_ids(&rows));
    if let Err(e) = db
        .bulk_insert_transaction("deferred file parts insert", |txn| {
            let rows = rows.clone();
            let file_ids = file_ids.clone();
            Box::pin(async move {
                insert_fresh_part_rows(txn, &rows, chunk_size).await?;
                require_parent_rows(txn, "files", &file_ids).await
            })
        })
        .await
    {
        warn!("Failed to flush deferred file part inserts: {}", e);
        return false;
    }
    for chunk in rows.chunks(chunk_size) {
        insert_batches += 1;
        insert_rows_affected = insert_rows_affected.saturating_add(chunk.len() as u64);
    }
    let sqlite_delta = sqlite_perf_snapshot().delta_since(sqlite_baseline);
    info!(
        "Deferred file part insert flushed {} rows in {:.2?}",
        total,
        started.elapsed()
    );
    info!(
        "Deferred file part insert metrics: strategy=fresh_remote_metadata_live_indexes rows={} streamed_rows={} insert_batch_size={} insert_batches={} insert_rows_affected={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} total={:.3}s",
        total,
        streamed,
        chunk_size,
        insert_batches,
        insert_rows_affected,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        started.elapsed().as_secs_f64()
    );
    true
}

/// Commit the part rows one group of manifests appended to the deferred buffer
/// from `first_row` on, with that group's addon file links, under the part
/// stream lock. Links land only with their parts because the completeness
/// checks count files through `addon_files` and probe for just one part row.
/// A failed group turns the stream off; the regular flushes take over.
pub(crate) async fn persist_streamed_part_group(context: &Arc<FoxyContext>, first_row: usize) {
    if !context.should_stream_part_inserts() {
        return;
    }
    let prepare_started = Instant::now();
    let mut links = context.take_pending_addon_file_links();
    if context.deferred_parts_persisted() != first_row {
        warn!(
            "Streamed part insert stopped: {} rows committed but the group starts at row {}",
            context.deferred_parts_persisted(),
            first_row
        );
        context.set_stream_part_inserts(false);
        context.buffer_addon_file_links(links);
        return;
    }
    let mut rows = context.deferred_parts_from(first_row);
    if rows.is_empty() && links.is_empty() {
        return;
    }
    let group_rows = rows.len();
    rows.sort_unstable_by_key(|row| (row.file_id, row.data_order));
    links.sort_unstable();
    links.dedup();
    let started = Instant::now();
    let chunk_size = file_part_upsert_chunk_size();
    let link_chunk_size = crate::core::tasks::init_database::bulk_write_rows_for(2);
    let mut file_ids: Vec<i64> = rows
        .iter()
        .map(|row| row.file_id)
        .chain(links.iter().map(|(_, file_id)| *file_id))
        .collect();
    file_ids.sort_unstable();
    file_ids.dedup();
    let mut addon_ids: Vec<i64> = links.iter().map(|(addon_id, _)| *addon_id).collect();
    addon_ids.sort_unstable();
    addon_ids.dedup();
    let parents = Arc::new((file_ids, addon_ids));
    let rows = Arc::new(rows);
    let shared_links = Arc::new(links);
    let prepare = prepare_started.elapsed();
    let insert_nanos = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let result = context
        .db()
        .bulk_insert_transaction("streamed file parts insert", |txn| {
            let rows = rows.clone();
            let links = shared_links.clone();
            let parents = parents.clone();
            let insert_nanos = insert_nanos.clone();
            Box::pin(async move {
                let insert_started = Instant::now();
                insert_fresh_part_rows(txn, &rows, chunk_size).await?;
                insert_nanos.store(
                    insert_started.elapsed().as_nanos() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                for chunk in links.chunks(link_chunk_size) {
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
                    txn.execute(&sql, values).await?;
                }
                let (file_ids, addon_ids) = parents.as_ref();
                require_parent_rows(txn, "files", file_ids).await?;
                require_parent_rows(txn, "addons", addon_ids).await
            })
        })
        .await;
    match result {
        Ok(()) => {
            context.set_deferred_parts_persisted(first_row + group_rows);
            info!(
                "Streamed part group committed: rows={} links={} total_rows={} elapsed={:.3}s prepare={:.3}s insert={:.3}s",
                group_rows,
                shared_links.len(),
                first_row + group_rows,
                started.elapsed().as_secs_f64(),
                prepare.as_secs_f64(),
                insert_nanos.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
            );
        }
        Err(err) => {
            warn!("Streamed part insert failed, falling back to the deferred insert: {err}");
            context.set_stream_part_inserts(false);
            context.buffer_addon_file_links(shared_links.iter().copied());
        }
    }
}

async fn insert_fresh_part_rows(
    txn: &DbTxn<'_>,
    rows: &[DeferredPartInsert],
    chunk_size: usize,
) -> Result<(), DbErr> {
    for chunk in rows.chunks(chunk_size) {
        let sql = file_part_upsert_sql(chunk.len(), true);
        let mut values: Vec<DbValue> =
            Vec::with_capacity(chunk.len() * FILE_PART_UPSERT_PARAMS_PER_ROW);
        for row in chunk {
            values.push(row.file_id.into());
            values.push(row.path.clone().into());
            values.push(row.remote_length.into());
            values.push(row.remote_start.into());
            values.push(row.remote_checksum.clone().into());
            values.push(row.data_order.into());
        }
        txn.execute(&sql, values).await?;
    }
    Ok(())
}

fn distinct_file_ids(rows: &[DeferredPartInsert]) -> Vec<i64> {
    let mut ids: Vec<i64> = rows.iter().map(|row| row.file_id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Stands in for the foreign keys a bulk insert runs without: a purge that
/// ran since the rows were staged fails the transaction instead of leaving orphans.
async fn require_parent_rows(txn: &DbTxn<'_>, table: &str, ids: &[i64]) -> Result<(), DbErr> {
    let found = count_existing_ids(txn, table, ids).await?;
    if found != ids.len() {
        return Err(DbErr::Custom(format!(
            "{} of {} {table} rows referenced by the insert are gone",
            ids.len() - found,
            ids.len()
        )));
    }
    Ok(())
}

async fn count_existing_ids(txn: &DbTxn<'_>, table: &str, ids: &[i64]) -> Result<usize, DbErr> {
    let mut found = 0usize;
    for chunk in ids.chunks(crate::core::tasks::init_database::read_chunk_ids()) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let row = txn
            .query_one(
                &format!("SELECT COUNT(*) AS c FROM {table} WHERE id IN ({placeholders})"),
                chunk.iter().map(|id| DbValue::from(*id)).collect(),
            )
            .await?;
        found += row.map(|row| row.get_i64("c")).transpose()?.unwrap_or(0) as usize;
    }
    Ok(found)
}

pub(crate) async fn persist_part_local_state_by_file_path<F>(
    db: &FoxyDb,
    parts: &[FoxyModFilePart],
    mut on_rows_persisted: F,
) -> bool
where
    F: FnMut(usize),
{
    if parts.is_empty() {
        return true;
    }

    let chunk_size = file_part_insert_with_local_chunk_size();
    let started = Instant::now();
    let sqlite_baseline = sqlite_perf_snapshot();
    let _write_scope = sqlite_labeled_write_scope("persist part local state by file path");
    let txn = match db.begin().await {
        Ok(txn) => txn,
        Err(err) => {
            error!("Failed to begin part local-state upsert: {}", err);
            return false;
        }
    };

    let mut upsert_elapsed = Duration::ZERO;
    let mut upsert_batch_max_elapsed = Duration::ZERO;
    let mut upsert_batches = 0usize;
    let mut upsert_rows_affected = 0u64;
    for chunk in parts.chunks(chunk_size) {
        let sql = file_part_insert_with_local_sql(chunk.len(), false);
        let chunk_refs: Vec<&FoxyModFilePart> = chunk.iter().collect();
        let upsert_started = Instant::now();
        match txn
            .execute(&sql, file_part_insert_with_local_ref_values(&chunk_refs))
            .await
        {
            Ok(affected) => {
                let elapsed = upsert_started.elapsed();
                upsert_elapsed += elapsed;
                upsert_batch_max_elapsed = upsert_batch_max_elapsed.max(elapsed);
                upsert_batches += 1;
                upsert_rows_affected = upsert_rows_affected.saturating_add(affected);
                on_rows_persisted(chunk.len());
            }
            Err(err) => {
                let _ = txn.rollback().await;
                error!("Failed to persist part local state by file path: {}", err);
                return false;
            }
        }
    }

    let commit_started = Instant::now();
    if let Err(err) = txn.commit().await {
        error!("Failed to commit part local-state upsert: {}", err);
        return false;
    }
    let commit_elapsed = commit_started.elapsed();
    let sqlite_delta = sqlite_perf_snapshot().delta_since(sqlite_baseline);
    info!(
        "Part local-state persistence metrics: strategy=upsert_by_file_path rows={} upsert_batch_size={} upsert_batches={} upsert_rows_affected={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} upsert={:.3}s upsert_batch_max={:.3}s commit={:.3}s total={:.3}s",
        parts.len(),
        chunk_size,
        upsert_batches,
        upsert_rows_affected,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        upsert_elapsed.as_secs_f64(),
        upsert_batch_max_elapsed.as_secs_f64(),
        commit_elapsed.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    true
}

pub(crate) async fn flush_deferred_part_inserts_with_local_state<F>(
    context: Arc<FoxyContext>,
    parts: &[FoxyModFilePart],
    mut on_rows_persisted: F,
) -> bool
where
    F: FnMut(usize),
{
    if context.deferred_part_count() == 0 {
        return false;
    }
    if parts.is_empty() {
        let _ = context.take_deferred_parts();
        context.set_deferred_part_inserts_fresh_load(false);
        return false;
    }

    let total = parts.len();
    let db = context.db();
    let chunk_size = file_part_insert_with_local_chunk_size();
    let started = Instant::now();
    let sort_started = Instant::now();
    let mut ordered_parts: Vec<&FoxyModFilePart> = parts.iter().collect();
    ordered_parts.sort_by_key(|part| (part.file_id, part.data_order));
    let sort_elapsed = sort_started.elapsed();

    let sqlite_baseline = sqlite_perf_snapshot();
    let _write_scope = sqlite_labeled_write_scope("deferred file parts insert with local state");
    let fresh_requested = context.deferred_part_inserts_are_fresh_load();
    let txn = match if fresh_requested {
        db.begin_exclusive().await
    } else {
        db.begin().await
    } {
        Ok(txn) => txn,
        Err(err) => {
            warn!(
                "Failed to begin deferred file part insert with local state: {}",
                err
            );
            return false;
        }
    };

    let mut strategy = if fresh_requested {
        "fresh_plain_insert_rebuild_indexes"
    } else {
        "coalesced_sorted_upsert_live_indexes"
    };
    let mut drop_indexes_elapsed = Duration::ZERO;
    let mut rebuild_indexes_elapsed = Duration::ZERO;
    let mut indexes_deferred = false;
    if fresh_requested {
        let row_count = match txn
            .query_all("SELECT COUNT(*) AS c FROM subfiles", Vec::new())
            .await
            .and_then(|rows| {
                rows.into_iter()
                    .next()
                    .ok_or_else(|| DbErr::Custom("COUNT(*) returned no rows".to_owned()))
                    .and_then(|row| row.get_i64("c"))
            }) {
            Ok(row_count) => row_count,
            Err(err) => {
                let _ = txn.rollback().await;
                error!(
                    "Failed to verify empty subfiles table before deferred fresh insert: {}",
                    err
                );
                return false;
            }
        };

        if row_count == 0 {
            let drop_started = Instant::now();
            for name in SUBFILES_INDEX_NAMES {
                if let Err(err) = txn
                    .execute(&format!("DROP INDEX IF EXISTS {name}"), Vec::new())
                    .await
                {
                    let _ = txn.rollback().await;
                    error!(
                        "Failed to drop subfiles indexes before deferred fresh insert: {}",
                        err
                    );
                    return false;
                }
            }
            drop_indexes_elapsed = drop_started.elapsed();
            indexes_deferred = true;
        } else {
            strategy = "fresh_requested_but_subfiles_nonempty_upsert_live_indexes";
            warn!(
                "Fresh deferred subfiles insert requested but table is not empty (rows={}); keeping live indexes and using upsert",
                row_count
            );
        }
    }

    let mut insert_elapsed = Duration::ZERO;
    let mut insert_batch_max_elapsed = Duration::ZERO;
    let mut insert_batches = 0usize;
    let mut insert_rows_affected = 0u64;
    for chunk in ordered_parts.chunks(chunk_size) {
        let sql = file_part_insert_with_local_sql(chunk.len(), indexes_deferred);
        let insert_started = Instant::now();
        match txn
            .execute(&sql, file_part_insert_with_local_ref_values(chunk))
            .await
        {
            Ok(affected) => {
                let elapsed = insert_started.elapsed();
                insert_elapsed += elapsed;
                insert_batch_max_elapsed = insert_batch_max_elapsed.max(elapsed);
                insert_batches += 1;
                insert_rows_affected = insert_rows_affected.saturating_add(affected);
                on_rows_persisted(chunk.len());
            }
            Err(err) => {
                let _ = txn.rollback().await;
                warn!(
                    "Failed to flush deferred file part inserts with local state: {}",
                    err
                );
                return false;
            }
        }
    }

    if indexes_deferred {
        let rebuild_started = Instant::now();
        for sql in SUBFILES_INDEX_CREATE_SQL {
            if let Err(err) = txn.execute(sql, Vec::new()).await {
                let _ = txn.rollback().await;
                error!(
                    "Failed to rebuild subfiles indexes after deferred fresh insert: {}",
                    err
                );
                return false;
            }
        }
        rebuild_indexes_elapsed = rebuild_started.elapsed();
    }

    let commit_started = Instant::now();
    if let Err(err) = txn.commit().await {
        error!(
            "Failed to commit deferred file part inserts with local state: {}",
            err
        );
        return false;
    }
    let commit_elapsed = commit_started.elapsed();
    let sqlite_delta = sqlite_perf_snapshot().delta_since(sqlite_baseline);
    let _ = context.take_deferred_parts();
    context.set_defer_part_inserts(false);
    context.set_fresh_subfiles_load(false);
    context.set_deferred_part_inserts_fresh_load(false);
    info!(
        "Deferred file part insert with local state flushed {} rows in {:.2?}",
        total,
        started.elapsed()
    );
    info!(
        "Deferred file part insert metrics: strategy={} rows={} insert_batch_size={} insert_batches={} insert_rows_affected={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} sort={:.3}s drop_indexes={:.3}s insert={:.3}s insert_batch_max={:.3}s rebuild_indexes={:.3}s commit={:.3}s total={:.3}s",
        strategy,
        total,
        chunk_size,
        insert_batches,
        insert_rows_affected,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        sort_elapsed.as_secs_f64(),
        drop_indexes_elapsed.as_secs_f64(),
        insert_elapsed.as_secs_f64(),
        insert_batch_max_elapsed.as_secs_f64(),
        rebuild_indexes_elapsed.as_secs_f64(),
        commit_elapsed.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;

    #[test]
    fn full_file_control_keeps_patch_planning_disabled_for_present_files() {
        assert!(!force_full_file_download(false, false));
        assert!(force_full_file_download(false, true));
        assert!(force_full_file_download(true, false));
    }

    // ── part upsert batch sizing ───────────────────────────────────────

    #[test]
    fn part_upsert_uses_tuned_small_chunk() {
        // Turso writes are fastest in small statements (bench_fresh_insert_vs_upsert):
        // the tuned ~256-row chunk replaces the old bind-variable-maxed 5 460.
        assert_eq!(
            file_part_upsert_chunk_size(),
            crate::core::tasks::init_database::bulk_write_chunk_rows()
        );
        assert_eq!(file_part_upsert_chunk_size(), 256);
        // Stays safely under the bind-variable ceiling (256 × 6 params ≪ 32 766).
        assert!(file_part_upsert_chunk_size() * FILE_PART_UPSERT_PARAMS_PER_ROW < 32_766);
    }

    #[test]
    fn part_upsert_statement_binds_remote_metadata_only() {
        let sql = file_part_upsert_sql(1, false);
        let values = file_part_upsert_values(&[PartUpsertRow {
            file_id: 10,
            path: "addons/ace_main.pbo".to_owned(),
            remote_length: 20,
            remote_start: 30,
            remote_checksum: "remote".to_owned(),
            data_order: 2,
        }]);

        assert_eq!(sql.matches('?').count(), 6);
        assert_eq!(values.len(), 6);
        assert!(sql.contains("VALUES (?, ?, 0, 0, ?, ?, '', ?, ?)"));
        assert!(sql.contains("ON CONFLICT (file_id, path)"));
        assert!(!sql.contains("local_checksum = excluded"));
        assert!(!sql.contains("local_length = excluded"));
        assert!(!sql.contains("local_start = excluded"));
    }

    #[test]
    fn part_upsert_fresh_mode_is_plain_insert_without_on_conflict() {
        // P0-b: the index-deferred fresh load drops the ON CONFLICT arm so the
        // append touches only the rowid PK. Same bind shape, no conflict clause.
        let sql = file_part_upsert_sql(2, true);
        assert_eq!(sql.matches('?').count(), 12);
        assert!(sql.contains("VALUES (?, ?, 0, 0, ?, ?, '', ?, ?)"));
        assert!(!sql.contains("ON CONFLICT"));
        assert!(!sql.contains("DO UPDATE"));
    }

    #[test]
    fn part_insert_with_local_incremental_mode_keeps_upsert() {
        let sql = file_part_insert_with_local_sql(1, false);

        assert_eq!(sql.matches('?').count(), 9);
        assert!(sql.contains("ON CONFLICT (file_id, path)"));
        assert!(sql.contains("local_checksum = excluded.local_checksum"));
        assert!(sql.contains("local_length = excluded.local_length"));
        assert!(sql.contains("local_start = excluded.local_start"));
    }

    #[test]
    fn part_insert_with_local_fresh_mode_is_plain_insert() {
        let sql = file_part_insert_with_local_sql(2, true);

        assert_eq!(sql.matches('?').count(), 18);
        assert!(sql.contains("VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"));
        assert!(!sql.contains("ON CONFLICT"));
        assert!(!sql.contains("DO UPDATE"));
    }

    #[test]
    fn metadata_only_batches_skip_download_work() {
        assert!(!should_prepare_download_work(false, false));
        assert!(should_prepare_download_work(true, false));
        assert!(should_prepare_download_work(false, true));
    }

    /// Benchmark (`FOXY_BENCH_MANIFEST_DIR=<manifest_cache dir> cargo test --release
    /// bench_streamed_groups_real_manifests -- --ignored --nocapture`). The record
    /// case's two streamed groups on real cached manifests, against one flush.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_streamed_groups_real_manifests() {
        let Ok(dir) = std::env::var("FOXY_BENCH_MANIFEST_DIR") else {
            println!("[bench streamed-groups] FOXY_BENCH_MANIFEST_DIR is not set");
            return;
        };
        let mut rows = Vec::new();
        let mut file_id = 0i64;
        let mut bodies: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "body"))
            .collect();
        bodies.sort();
        for body in bodies {
            let manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(body).unwrap()).unwrap();
            for file in manifest["files"].as_array().into_iter().flatten() {
                file_id += 1;
                for (order, part) in file["parts"].as_array().into_iter().flatten().enumerate() {
                    rows.push(DeferredPartInsert {
                        file_id,
                        path: crate::core::models::modification_file_part::part_storage_path(
                            part["path"].as_str().unwrap_or_default(),
                            order as i64,
                        ),
                        remote_length: part["length"].as_i64().unwrap_or_default(),
                        remote_start: part["start"].as_i64().unwrap_or_default(),
                        remote_checksum: part["checksum"].as_str().unwrap_or_default().to_owned(),
                        data_order: order as i64,
                    });
                }
            }
        }
        let split = rows.len() * 55 / 100;
        for round in 0..2 {
            for (streamed, after_drop) in [(true, false), (true, true), (false, true)] {
                let handle = crate::core::tasks::db_turso::build_test_database().await;
                let db = FoxyDb::from_handle(handle.clone());
                db.execute(
                    "INSERT INTO addons (id, name, remote_path, local_path, required) \
                     VALUES (1, 'a', 'rp', 'lp', 1)",
                    Vec::new(),
                )
                .await
                .unwrap();
                for id in 1..=file_id {
                    db.execute(
                        "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                        vec![
                            id.into(),
                            format!("f{id}").into(),
                            format!("rp{id}").into(),
                            format!("lp{id}").into(),
                        ],
                    )
                    .await
                    .unwrap();
                }
                let context = Arc::new(FoxyContext::new(handle, reqwest::Client::new()));
                context.set_fresh_subfiles_load(true);
                context.set_defer_part_inserts(true);
                context.set_stream_part_inserts(streamed);
                if after_drop {
                    context.buffer_deferred_parts(rows.clone());
                    assert!(flush_deferred_part_inserts(context.clone()).await);
                    db.execute("DROP TABLE IF EXISTS subfiles", Vec::new())
                        .await
                        .unwrap();
                    db.execute(
                        crate::core::tasks::db_turso::SUBFILES_CREATE_TABLE,
                        Vec::new(),
                    )
                    .await
                    .unwrap();
                    for sql in SUBFILES_INDEX_CREATE_SQL {
                        db.execute(sql, Vec::new()).await.unwrap();
                    }
                }
                let started = Instant::now();
                if streamed {
                    context.buffer_deferred_parts(rows[..split].to_vec());
                    context.buffer_addon_file_links(
                        (1..=rows[split - 1].file_id).map(|file| (1, file)),
                    );
                    persist_streamed_part_group(&context, 0).await;
                    let first = started.elapsed().as_secs_f64();
                    context.buffer_deferred_parts(rows[split..].to_vec());
                    context.buffer_addon_file_links(
                        (rows[split].file_id..=file_id).map(|file| (1, file)),
                    );
                    persist_streamed_part_group(&context, split).await;
                    assert_eq!(context.deferred_parts_persisted(), rows.len());
                    println!(
                        "[bench streamed-groups] streamed after_drop={after_drop} round={round} first={first:.3}s total={:.3}s",
                        started.elapsed().as_secs_f64()
                    );
                } else {
                    context.buffer_deferred_parts(rows.clone());
                    assert!(flush_deferred_part_inserts(context).await);
                    println!(
                        "[bench streamed-groups] flush after_drop={after_drop} round={round} total={:.3}s",
                        started.elapsed().as_secs_f64()
                    );
                }
            }
        }
    }

    /// Benchmark (`cargo test --release bench_deferred_flush_seam -- --ignored --nocapture`).
    /// The deferred flush through the app's seam, with the record case's row
    /// count and shape, to compare against the raw engine insert in
    /// `bench_subfiles_checksum_encoding`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "perf benchmark; run manually with --ignored --nocapture"]
    async fn bench_deferred_flush_seam() {
        const ROWS: usize = 433_248;
        const FILES: usize = 3_738;
        let per_file = (ROWS / FILES) as i64;
        for round in 0..2 {
            let handle = crate::core::tasks::db_turso::build_test_database().await;
            let db = FoxyDb::from_handle(handle.clone());
            for file in 1..=FILES as i64 {
                db.execute(
                    "INSERT INTO files (id, name, remote_path, local_path) VALUES (?, ?, ?, ?)",
                    vec![
                        file.into(),
                        format!("f{file}").into(),
                        format!("rp{file}").into(),
                        format!("lp{file}").into(),
                    ],
                )
                .await
                .unwrap();
            }
            let context = Arc::new(FoxyContext::new(handle, reqwest::Client::new()));
            context.buffer_deferred_parts(
                (1..=FILES as i64)
                    .flat_map(|file| {
                        (0..per_file).map(move |order| DeferredPartInsert {
                            file_id: file,
                            path: format!("a3/data_f/lod/texture_{order:05}_co.paa\u{1F}{order}"),
                            remote_length: 65_536,
                            remote_start: order * 65_536,
                            remote_checksum: format!(
                                "{:064X}",
                                (file as u128) << 64 | order as u128
                            ),
                            data_order: order,
                        })
                    })
                    .collect(),
            );
            let build_started = Instant::now();
            let snapshot = context.deferred_parts_snapshot();
            let mut built = 0usize;
            for chunk in snapshot.chunks(file_part_upsert_chunk_size()) {
                let sql = file_part_upsert_sql(chunk.len(), true);
                let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 6);
                for row in chunk {
                    values.push(row.file_id.into());
                    values.push(row.path.clone().into());
                    values.push(row.remote_length.into());
                    values.push(row.remote_start.into());
                    values.push(row.remote_checksum.clone().into());
                    values.push(row.data_order.into());
                }
                let turso_values: Vec<turso::Value> =
                    values.into_iter().map(DbValue::into_turso_value).collect();
                built += sql.len() + turso_values.len();
            }
            println!(
                "[bench deferred-flush-seam] build_only={:.3}s ({built})",
                build_started.elapsed().as_secs_f64()
            );
            drop(snapshot);
            let started = Instant::now();
            assert!(flush_deferred_part_inserts(context).await);
            println!(
                "[bench deferred-flush-seam] round={round} total={:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
    }

    fn deferred_row(file_id: i64, order: i64) -> DeferredPartInsert {
        DeferredPartInsert {
            file_id,
            path: format!("p{order}"),
            remote_length: 1,
            remote_start: order,
            remote_checksum: format!("r{file_id}_{order}"),
            data_order: order,
        }
    }

    async fn count(db: &FoxyDb, table: &str) -> i64 {
        db.query_one(&format!("SELECT COUNT(*) AS c FROM {table}"), Vec::new())
            .await
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap()
    }

    async fn streamed_context() -> (FoxyDb, Arc<FoxyContext>) {
        let handle = crate::core::tasks::db_turso::build_test_database().await;
        let db = FoxyDb::from_handle(handle.clone());
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required) \
             VALUES (10, 'a', 'rp', 'lp', 1)",
            Vec::new(),
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path) VALUES \
             (1, 'f1', 'rp1', 'lp1'), (2, 'f2', 'rp2', 'lp2')",
            Vec::new(),
        )
        .await
        .unwrap();
        let context = Arc::new(FoxyContext::new(handle, reqwest::Client::new()));
        context.set_fresh_subfiles_load(true);
        context.set_defer_part_inserts(true);
        context.set_stream_part_inserts(true);
        (db, context)
    }

    #[tokio::test]
    async fn a_streamed_group_commits_its_parts_with_its_links_and_the_flush_skips_them() {
        let (db, context) = streamed_context().await;
        context.buffer_deferred_parts(vec![deferred_row(1, 1), deferred_row(1, 0)]);
        context.buffer_addon_file_links([(10, 1)]);
        persist_streamed_part_group(&context, 0).await;
        assert_eq!(context.deferred_parts_persisted(), 2);
        assert_eq!(count(&db, "subfiles").await, 2);
        assert_eq!(count(&db, "addon_files").await, 1);
        assert!(context.take_pending_addon_file_links().is_empty());

        context.buffer_deferred_parts(vec![deferred_row(2, 0)]);
        context.set_stream_part_inserts(false);
        persist_streamed_part_group(&context, 2).await;
        assert_eq!(count(&db, "subfiles").await, 2);

        assert!(flush_deferred_part_inserts(context.clone()).await);
        assert_eq!(count(&db, "subfiles").await, 3);
        assert_eq!(context.deferred_part_count(), 0);
        assert_eq!(context.deferred_parts_persisted(), 0);
    }

    #[tokio::test]
    async fn a_group_whose_file_was_purged_rolls_back_instead_of_leaving_orphans() {
        let (db, context) = streamed_context().await;
        context.buffer_deferred_parts(vec![deferred_row(1, 0), deferred_row(3, 0)]);
        context.buffer_addon_file_links([(10, 1), (10, 3)]);
        persist_streamed_part_group(&context, 0).await;
        assert!(!context.should_stream_part_inserts());
        assert_eq!(context.deferred_parts_persisted(), 0);
        assert_eq!(count(&db, "subfiles").await, 0);
        assert_eq!(count(&db, "addon_files").await, 0);
        assert_eq!(context.take_pending_addon_file_links().len(), 2);
    }

    #[tokio::test]
    async fn a_flush_whose_file_was_purged_inserts_nothing_and_pools_no_bulk_connection() {
        let (db, context) = streamed_context().await;
        context.set_stream_part_inserts(false);
        context.buffer_deferred_parts(vec![deferred_row(1, 0), deferred_row(3, 0)]);
        assert!(!flush_deferred_part_inserts(context.clone()).await);
        assert_eq!(count(&db, "subfiles").await, 0);

        context.buffer_deferred_parts(vec![deferred_row(1, 0), deferred_row(2, 0)]);
        assert!(flush_deferred_part_inserts(context).await);
        assert_eq!(count(&db, "subfiles").await, 2);
        let cache = db
            .query_one("PRAGMA cache_size", Vec::new())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(cache.values[0], DbValue::Int(-16384)));
    }

    #[tokio::test]
    async fn a_group_that_does_not_follow_the_committed_rows_stops_the_stream() {
        let (db, context) = streamed_context().await;
        context.buffer_deferred_parts(vec![deferred_row(1, 0), deferred_row(2, 0)]);
        context.buffer_addon_file_links([(10, 2)]);
        persist_streamed_part_group(&context, 1).await;
        assert!(!context.should_stream_part_inserts());
        assert_eq!(context.deferred_parts_persisted(), 0);
        assert_eq!(count(&db, "subfiles").await, 0);
        assert_eq!(count(&db, "addon_files").await, 0);
        assert_eq!(context.take_pending_addon_file_links(), vec![(10, 2)]);
    }

    #[tokio::test]
    async fn deferred_local_state_fresh_flush_rebuilds_unique_index() {
        let handle = crate::core::tasks::db_turso::build_test_database().await;
        let db = FoxyDb::from_handle(handle.clone());
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path, length, data_order) \
             VALUES (1, 'f', 'remote/f', 'local/f', 2, 0)",
            Vec::new(),
        )
        .await
        .unwrap();

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
        assert!(context.deferred_part_inserts_are_fresh_load());

        for name in SUBFILES_INDEX_NAMES {
            db.execute(&format!("DROP INDEX IF EXISTS {name}"), Vec::new())
                .await
                .unwrap();
        }

        let parts = vec![
            FoxyModFilePart {
                file_id: 1,
                path: "p0".to_owned(),
                remote_length: 1,
                local_length: 1,
                remote_start: 0,
                local_start: 0,
                remote_checksum: "r0".to_owned(),
                local_checksum: "r0".to_owned(),
                data_order: 0,
                ..FoxyModFilePart::default()
            },
            FoxyModFilePart {
                file_id: 1,
                path: "p1".to_owned(),
                remote_length: 1,
                local_length: 1,
                remote_start: 1,
                local_start: 1,
                remote_checksum: "r1".to_owned(),
                local_checksum: "r1".to_owned(),
                data_order: 1,
                ..FoxyModFilePart::default()
            },
        ];
        let mut persisted = 0usize;

        assert!(
            flush_deferred_part_inserts_with_local_state(context.clone(), &parts, |rows| {
                persisted += rows;
            })
            .await
        );
        assert_eq!(persisted, 2);
        assert_eq!(context.deferred_part_count(), 0);
        assert!(!context.should_defer_part_inserts());
        assert!(!context.is_fresh_subfiles_load());
        assert!(!context.deferred_part_inserts_are_fresh_load());

        let row = db
            .query_one("SELECT COUNT(*) AS c FROM subfiles", Vec::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("c").unwrap(), 2);

        let duplicate = db
            .execute(
                "INSERT INTO subfiles \
                 (file_id, path, local_length, local_start, remote_length, remote_start, \
                  local_checksum, remote_checksum, data_order) \
                 VALUES (1, 'p0', 1, 0, 1, 0, 'r0', 'r0', 99)",
                Vec::new(),
            )
            .await;
        assert!(
            duplicate.is_err(),
            "rebuilt idx_subfiles_file_id_path must reject duplicate rows"
        );
    }

    #[test]
    fn download_target_upsert_uses_tuned_small_chunk() {
        assert_eq!(
            download_file_target_upsert_chunk_size(),
            crate::core::tasks::init_database::bulk_write_chunk_rows()
        );
        assert!(
            download_file_target_upsert_chunk_size() * DOWNLOAD_FILE_TARGET_UPSERT_PARAMS_PER_ROW
                < 32_766
        );
    }

    #[test]
    fn download_target_statement_binds_queue_metadata_only() {
        let sql = download_file_target_upsert_sql(1);
        let values = download_file_target_upsert_values(&[DownloadTargetRow {
            file_id: 10,
            download_remote_url: "remote".to_owned(),
            download_local_path: "local".to_owned(),
            size: 20,
        }]);

        assert_eq!(sql.matches('?').count(), 4);
        assert_eq!(values.len(), 4);
        assert!(sql.contains("VALUES (?, ?, ?, ?, 0, 0)"));
        assert!(sql.contains("ON CONFLICT (file_id)"));
    }

    fn test_part(id: u64, file_id: u64, path: &str) -> FoxyModFilePart {
        FoxyModFilePart {
            id,
            file_id,
            path: path.to_owned(),
            ..FoxyModFilePart::default()
        }
    }

    #[test]
    fn stale_subfile_ids_prunes_parts_missing_from_manifest_layout() {
        let refreshed_parts = HashMap::from([
            ((10, "current".to_owned()), test_part(20, 10, "current")),
            ((10, "stale".to_owned()), test_part(21, 10, "stale")),
        ]);
        let desired = HashMap::from([(10, HashSet::from([20]))]);
        let manifest_paths = HashMap::from([(10, vec!["current".to_owned()])]);

        assert_eq!(
            stale_subfile_ids(&refreshed_parts, &desired, &manifest_paths),
            vec![21]
        );
    }

    #[test]
    fn stale_subfile_ids_keeps_files_without_manifest_part_layout() {
        let refreshed_parts =
            HashMap::from([((10, "legacy".to_owned()), test_part(21, 10, "legacy"))]);
        let desired = HashMap::from([(10, HashSet::new())]);
        let manifest_paths = HashMap::from([(10, Vec::new())]);

        assert!(stale_subfile_ids(&refreshed_parts, &desired, &manifest_paths).is_empty());
    }

    #[test]
    fn part_upsert_old_9_col_batch_size_was_110() {
        // Verify old batch size for comparison
        let params_per_row = 9usize;
        let chunk_size = (SQLITE_MAX_VARIABLES / params_per_row)
            .saturating_sub(1)
            .max(1);
        assert_eq!(chunk_size, 110);
    }

    #[test]
    fn part_upsert_batch_improvement_is_50_percent() {
        let old_batch = (SQLITE_MAX_VARIABLES / 9).saturating_sub(1).max(1);
        let new_batch = (SQLITE_MAX_VARIABLES / 6).saturating_sub(1).max(1);
        let improvement_pct = ((new_batch as f64 - old_batch as f64) / old_batch as f64) * 100.0;
        assert!(
            improvement_pct >= 49.0,
            "batch size improvement should be ~50%, got {:.1}%",
            improvement_pct
        );
    }

    // ── SQL placeholder generation ─────────────────────────────────────

    #[test]
    fn raw_upsert_placeholder_for_single_row() {
        let sql = file_part_upsert_sql(1, false);
        // 6 bound params for the one row
        assert_eq!(sql.matches('?').count(), 6);
        // local_length and local_start are inline 0
        assert!(sql.contains("0, 0"));
        // local_checksum is inline empty string
        assert!(sql.contains("''"));
    }

    #[test]
    fn raw_upsert_placeholder_for_three_rows() {
        let sql = file_part_upsert_sql(3, false);
        // 3 rows * 6 params = 18
        assert_eq!(sql.matches('?').count(), 18);
    }

    #[test]
    fn raw_upsert_sql_has_on_conflict_clause() {
        let sql = file_part_upsert_sql(1, false);
        assert!(sql.contains("ON CONFLICT (file_id, path)"));
        assert!(sql.contains("DO UPDATE SET"));
        // Should NOT update local_* columns
        assert!(!sql.contains("local_checksum = excluded"));
        assert!(!sql.contains("local_length = excluded"));
        assert!(!sql.contains("local_start = excluded"));
    }

    // ── download target batch sizing ───────────────────────────────────

    #[test]
    fn stale_subfile_ids_keeps_all_when_every_part_is_desired() {
        let refreshed_parts = HashMap::from([
            ((10, "a".to_owned()), test_part(20, 10, "a")),
            ((10, "b".to_owned()), test_part(21, 10, "b")),
        ]);
        let desired = HashMap::from([(10, HashSet::from([20, 21]))]);
        let manifest_paths = HashMap::from([(10, vec!["a".to_owned(), "b".to_owned()])]);

        assert!(stale_subfile_ids(&refreshed_parts, &desired, &manifest_paths).is_empty());
    }

    #[test]
    fn stale_subfile_ids_prunes_across_multiple_files() {
        let refreshed_parts = HashMap::from([
            ((10, "keep".to_owned()), test_part(20, 10, "keep")),
            ((10, "drop".to_owned()), test_part(21, 10, "drop")),
            ((11, "drop2".to_owned()), test_part(31, 11, "drop2")),
        ]);
        let desired = HashMap::from([(10, HashSet::from([20])), (11, HashSet::new())]);
        let manifest_paths = HashMap::from([
            (10, vec!["keep".to_owned()]),
            (11, vec!["other".to_owned()]),
        ]);

        let mut stale = stale_subfile_ids(&refreshed_parts, &desired, &manifest_paths);
        stale.sort_unstable();
        assert_eq!(stale, vec![21, 31]);
    }

    #[test]
    fn stale_subfile_ids_ignores_files_absent_from_manifest_map() {
        // A file with parts in the DB but no entry in the manifest map at all
        // must not be pruned (we only prune files whose manifest has a layout).
        let refreshed_parts =
            HashMap::from([((10, "orphan".to_owned()), test_part(20, 10, "orphan"))]);
        let desired = HashMap::from([(10, HashSet::new())]);
        let manifest_paths: HashMap<i64, Vec<String>> = HashMap::new();

        assert!(stale_subfile_ids(&refreshed_parts, &desired, &manifest_paths).is_empty());
    }

    fn manifest_part(
        id: u64,
        display_path: &str,
        order: i64,
        start: u64,
        length: u64,
        checksum: &str,
    ) -> FoxyModFilePart {
        FoxyModFilePart {
            id,
            file_id: 10,
            path: part_storage_path(display_path, order),
            remote_start: start,
            remote_length: length,
            remote_checksum: checksum.to_owned(),
            data_order: order,
            ..FoxyModFilePart::default()
        }
    }

    fn with_local(
        mut part: FoxyModFilePart,
        start: u64,
        length: u64,
        checksum: &str,
    ) -> FoxyModFilePart {
        part.local_start = start;
        part.local_length = length;
        part.local_checksum = checksum.to_owned();
        part
    }

    fn clean_old_parts(parts: Vec<FoxyModFilePart>) -> Vec<FoxyModFilePart> {
        parts
            .into_iter()
            .map(|part| part.with_derived_clean_local_state("OLD", "OLD"))
            .collect()
    }

    #[test]
    fn carried_state_materializes_clean_parts_that_keep_their_key() {
        let old = clean_old_parts(vec![
            manifest_part(1, "$$HEADER$$", 0, 0, 100, "H1"),
            manifest_part(2, "config.bin", 1, 100, 50, "C1"),
        ]);
        let header = manifest_part(1, "$$HEADER$$", 0, 0, 100, "H2");
        let config = manifest_part(2, "config.bin", 1, 100, 50, "C1");

        let carried = carried_part_local_state(&old, &[&header, &config], true);

        assert_eq!(
            carried,
            vec![
                CarriedPartLocalState {
                    id: 1,
                    local_checksum: "H1".to_owned(),
                    local_length: 100,
                    local_start: 0,
                },
                CarriedPartLocalState {
                    id: 2,
                    local_checksum: "C1".to_owned(),
                    local_length: 50,
                    local_start: 100,
                },
            ]
        );
    }

    #[test]
    fn carried_state_follows_part_path_when_an_inserted_entry_shifts_the_keys() {
        let old = clean_old_parts(vec![
            manifest_part(1, "$$HEADER$$", 0, 0, 100, "H1"),
            manifest_part(2, "config.bin", 1, 100, 50, "C1"),
            manifest_part(3, "$$END$$", 2, 150, 21, "E1"),
        ]);
        let header = manifest_part(1, "$$HEADER$$", 0, 0, 120, "H2");
        let added = manifest_part(4, "added.sqf", 1, 120, 10, "A2");
        let config = manifest_part(5, "config.bin", 2, 130, 50, "C1");
        let end = manifest_part(6, "$$END$$", 3, 180, 21, "E2");

        let carried = carried_part_local_state(&old, &[&end, &config, &added, &header], true);

        let by_id: HashMap<u64, &CarriedPartLocalState> =
            carried.iter().map(|state| (state.id, state)).collect();
        assert_eq!(by_id.len(), 3);
        assert_eq!(by_id[&1].local_checksum, "H1");
        assert_eq!(
            (
                by_id[&5].local_checksum.as_str(),
                by_id[&5].local_start,
                by_id[&5].local_length
            ),
            ("C1", 100, 50)
        );
        assert_eq!(
            (by_id[&6].local_checksum.as_str(), by_id[&6].local_start),
            ("E1", 150)
        );
        assert!(!by_id.contains_key(&4));
    }

    #[test]
    fn carried_state_leaves_stored_local_state_of_a_dirty_file_alone() {
        let old = vec![with_local(
            manifest_part(1, "config.bin", 0, 0, 50, "C1"),
            0,
            48,
            "LOCAL",
        )];
        let current = with_local(
            manifest_part(1, "config.bin", 0, 0, 50, "C2"),
            0,
            48,
            "LOCAL",
        );

        assert!(carried_part_local_state(&old, &[&current], false).is_empty());
    }

    #[test]
    fn carried_state_gives_a_rekeyed_row_the_stored_state_of_a_dirty_file() {
        let old = vec![with_local(
            manifest_part(1, "config.bin", 0, 0, 50, "C1"),
            0,
            48,
            "LOCAL",
        )];
        let moved = manifest_part(2, "config.bin", 1, 10, 50, "C2");

        assert_eq!(
            carried_part_local_state(&old, &[&moved], false),
            vec![CarriedPartLocalState {
                id: 2,
                local_checksum: "LOCAL".to_owned(),
                local_length: 48,
                local_start: 0,
            }]
        );
    }

    #[test]
    fn carried_state_skips_parts_without_any_local_state() {
        let old = vec![manifest_part(1, "config.bin", 0, 0, 50, "C1")];
        let moved = manifest_part(2, "config.bin", 1, 10, 50, "C2");
        let kept = manifest_part(1, "config.bin", 0, 0, 50, "C2");

        assert!(carried_part_local_state(&old, &[&moved], true).is_empty());
        assert!(carried_part_local_state(&old, &[&kept], true).is_empty());
    }

    #[tokio::test]
    async fn manifest_update_of_a_clean_file_keeps_a_delta_plan_possible() {
        let dir = tempfile::tempdir().unwrap();
        let local_path = dir.path().join("f.pbo");
        let mut bytes = vec![b'H'; 4];
        bytes.extend(vec![b'B'; 1000]);
        bytes.extend(vec![b'E'; 2]);
        std::fs::write(&local_path, &bytes).unwrap();
        let local_path = local_path.to_string_lossy().replace('\\', "/");

        let handle = crate::core::tasks::db_turso::build_test_database().await;
        let db = FoxyDb::from_handle(handle.clone());
        db.execute(
            "INSERT INTO files (id, name, remote_path, local_path, remote_checksum, \
             local_checksum, length, data_order) VALUES (1, 'f.pbo', 'remote/f.pbo', ?, \
             'OLD', 'OLD', 1006, 0)",
            vec![local_path.clone().into()],
        )
        .await
        .unwrap();
        db.execute(
            "INSERT INTO subfiles (file_id, path, local_length, local_start, remote_length, \
             remote_start, local_checksum, remote_checksum, data_order) VALUES \
             (1, ?, 0, 0, 4, 0, '', 'H1', 0), (1, ?, 0, 0, 1000, 4, '', 'B1', 1), \
             (1, ?, 0, 0, 2, 1004, '', 'E1', 2)",
            vec![
                part_storage_path("$$HEADER$$", 0).into(),
                part_storage_path("body.bin", 1).into(),
                part_storage_path("$$END$$", 2).into(),
            ],
        )
        .await
        .unwrap();

        let previous = FoxyModFile {
            id: 1,
            name: "f.pbo".to_owned(),
            remote_path: "remote/f.pbo".to_owned(),
            local_path: local_path.clone(),
            remote_checksum: "OLD".to_owned(),
            local_checksum: "OLD".to_owned(),
            length: 1006,
            ..FoxyModFile::default()
        };
        let file = FoxyModFile {
            remote_checksum: "NEW".to_owned(),
            length: 1032,
            ..previous.clone()
        };
        let new_part = |path: &str, start: i64, length: i64, checksum: &str, order: i64| {
            super::super::types::FilePartData {
                path: path.to_owned(),
                checksum: checksum.to_owned(),
                start,
                length,
                data_order: order,
            }
        };
        let context = Arc::new(FoxyContext::new(handle, reqwest::Client::new()));
        remote_file_parts_batch(
            context,
            vec![FilePartsPayload {
                file: file.clone(),
                previous_file: Some(previous),
                parts: vec![
                    new_part("$$HEADER$$", 0, 10, "H2", 0),
                    new_part("added.sqf", 10, 20, "A2", 1),
                    new_part("body.bin", 30, 1000, "B1", 2),
                    new_part("$$END$$", 1030, 2, "E2", 3),
                ],
            }],
        )
        .await;

        let parts: Vec<FoxyModFilePart> = load_parts_by_file_ids(&db, &[1])
            .await
            .unwrap()
            .into_values()
            .collect();
        assert_eq!(parts.len(), 4);
        let body = parts
            .iter()
            .find(|part| part_display_path(&part.path) == "body.bin")
            .unwrap();
        assert_eq!(
            (
                body.local_checksum.as_str(),
                body.local_start,
                body.local_length
            ),
            ("B1", 4, 1000)
        );

        let plan = plan_file_patch(&file, &parts, &parts).unwrap().unwrap();
        assert_eq!(plan.planned_copy_bytes, 1000);
        assert_eq!(plan.planned_download_bytes, 32);
    }

    #[test]
    fn carried_state_claims_each_old_part_once_for_duplicate_paths() {
        let old = clean_old_parts(vec![
            manifest_part(1, "dup.paa", 0, 0, 10, "D1"),
            manifest_part(2, "dup.paa", 1, 10, 10, "D2"),
        ]);
        let first = manifest_part(3, "dup.paa", 2, 0, 10, "N1");
        let second = manifest_part(4, "dup.paa", 3, 10, 10, "N2");
        let third = manifest_part(5, "dup.paa", 4, 20, 10, "N3");

        let carried = carried_part_local_state(&old, &[&third, &second, &first], true);

        let by_id: HashMap<u64, &str> = carried
            .iter()
            .map(|state| (state.id, state.local_checksum.as_str()))
            .collect();
        assert_eq!(by_id, HashMap::from([(3, "D1"), (4, "D2")]));
    }
}
