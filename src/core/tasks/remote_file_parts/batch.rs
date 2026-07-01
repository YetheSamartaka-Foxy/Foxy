use super::types::{FilePartsPayload, PartRow};
use super::validation::{local_file_matches_part_layout, log_suspicious_manifest_paths};
use crate::core::db::{DbErr, DbValue, FoxyDb};
use crate::core::models::context::{DeferredPartInsert, FoxyContext};
use crate::core::models::download_patch_file::delete_download_patch_files_by_file_ids;
use crate::core::models::download_patch_op::delete_download_patch_ops_for_files;
use crate::core::models::modification_file::FoxyModFile;
use crate::core::models::modification_file_part::{
    FoxyModFilePart, SUBFILE_COLUMNS, part_display_path, part_storage_path,
};
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::tasks::delta_patch::{persist_patch_plan, plan_file_patch};
use crate::core::tasks::init_database::{sqlite_labeled_write_scope, sqlite_perf_snapshot};
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    // `bulk_write_chunk_rows`). Cap by the bind-variable ceiling for safety, but
    // the tuned ~256-row default is far below it.
    crate::core::tasks::init_database::bulk_write_chunk_rows()
        .min(
            (crate::core::tasks::init_database::sqlite_variable_limit()
                / FILE_PART_UPSERT_PARAMS_PER_ROW)
                .saturating_sub(1),
        )
        .max(1)
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
    crate::core::tasks::init_database::bulk_write_chunk_rows()
        .min(
            (crate::core::tasks::init_database::sqlite_variable_limit()
                / FILE_PART_INSERT_WITH_LOCAL_PARAMS_PER_ROW)
                .saturating_sub(1),
        )
        .max(1)
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
    (crate::core::tasks::init_database::sqlite_variable_limit()
        / DOWNLOAD_FILE_TARGET_UPSERT_PARAMS_PER_ROW)
        .saturating_sub(1)
        .max(1)
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
    let chunk_size = crate::core::tasks::init_database::SQLITE_MAX_VARIABLES
        .saturating_sub(4)
        .max(1);
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

    for part in &all_part_rows {
        let needs_upsert = match existing_parts.get(&(part.file_id, part.path.clone())) {
            Some(existing) => {
                force_parts_recheck
                    || existing.remote_length != part.length as u64
                    || existing.remote_start != part.start as u64
                    || existing.remote_checksum != part.remote_checksum
            }
            None => true,
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
    let refreshed_parts: HashMap<(i64, String), FoxyModFilePart> =
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
            let local_file_ready = std::fs::metadata(&file.local_path)
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
            let local_file_present = std::fs::metadata(&file.local_path)
                .map(|meta| meta.is_file())
                .unwrap_or(false);
            if context.force_download_targets {
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
        "Remote file parts batch timings: files={} part_rows={} upsert_rows={} planned_patches={} prefetch={:.3}s upsert={:.3}s reload={:.3}s plan_loop={:.3}s total={:.3}s",
        file_ids.len(),
        all_part_rows.len(),
        upsert_row_count,
        planned_patch_files,
        prefetch_elapsed.as_secs_f64(),
        upsert_elapsed.as_secs_f64(),
        reload_elapsed.as_secs_f64(),
        plan_loop_elapsed.as_secs_f64(),
        batch_started.elapsed().as_secs_f64(),
    );

    // Flush all stale patch-plan clears in two bulk transactions rather than two
    // transactions per file (the dominant SQLite write cost when a whole repo of
    // files is missing and none are patchable).
    if !patch_clear_file_ids.is_empty() {
        patch_clear_file_ids.sort_unstable();
        patch_clear_file_ids.dedup();
        info!(
            "Bulk-clearing stale delta patch plans for {} files",
            patch_clear_file_ids.len()
        );
        clear_stale_patch_plans(context.clone(), &patch_clear_file_ids, "unpatchable file").await;
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
        let chunk_size = download_file_target_upsert_chunk_size();
        info!(
            "Download target rebuild batch: file_targets={} file_insert_chunks={} skipped_part_targets={}",
            download_file_models.len(),
            download_file_models.len().div_ceil(chunk_size),
            skipped_part_target_rows
        );
        let download_file_models = Arc::new(download_file_models);
        if let Err(e) = db
            .transaction("download target rebuild", |txn| {
                let download_file_models = download_file_models.clone();
                Box::pin(async move {
                    for chunk in download_file_models.chunks(chunk_size) {
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
}

/// Flush every part row staged by the deferred-insert path (A++,
/// after_turso_regression_analysis7.md) in one background transaction. Runs
/// concurrently with the download; the pipeline awaits the spawning task before the
/// incremental hasher loads its tree, so the rows are present by the time any reader
/// needs them. A plain conflict-free `INSERT` (the deferred load always targets a
/// globally-empty `subfiles` table - same `fresh=true` SQL as the inline fast path).
/// No-op when nothing was deferred.
pub(crate) async fn flush_deferred_part_inserts(context: Arc<FoxyContext>) {
    let rows = context.take_deferred_parts();
    if rows.is_empty() {
        return;
    }
    let total = rows.len();
    let db = context.db();
    let chunk_size = file_part_upsert_chunk_size();
    let started = Instant::now();
    let rows = Arc::new(rows);
    if let Err(e) = db
        .transaction("deferred file parts insert", |txn| {
            let rows = rows.clone();
            Box::pin(async move {
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
            })
        })
        .await
    {
        warn!("Failed to flush deferred file part inserts: {}", e);
    }
    info!(
        "Deferred file part insert flushed {} rows in {:.2?}",
        total,
        started.elapsed()
    );
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
    let txn = match db.begin().await {
        Ok(txn) => txn,
        Err(err) => {
            warn!(
                "Failed to begin deferred file part insert with local state: {}",
                err
            );
            return false;
        }
    };

    let mut insert_elapsed = Duration::ZERO;
    let mut insert_batch_max_elapsed = Duration::ZERO;
    let mut insert_batches = 0usize;
    let mut insert_rows_affected = 0u64;
    for chunk in ordered_parts.chunks(chunk_size) {
        let sql = file_part_insert_with_local_sql(chunk.len(), true);
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

    let commit_started = Instant::now();
    if let Err(err) = txn.commit().await {
        warn!(
            "Failed to commit deferred file part inserts with local state: {}",
            err
        );
        return false;
    }
    let commit_elapsed = commit_started.elapsed();
    let sqlite_delta = sqlite_perf_snapshot().delta_since(sqlite_baseline);
    let _ = context.take_deferred_parts();
    context.set_defer_part_inserts(false);
    info!(
        "Deferred file part insert with local state flushed {} rows in {:.2?}",
        total,
        started.elapsed()
    );
    info!(
        "Deferred file part insert metrics: strategy=coalesced_sorted_fresh_plain_insert_live_indexes rows={} insert_batch_size={} insert_batches={} insert_rows_affected={} sqlite_retries={} sqlite_backoff_ms={} sqlite_write_time_ms={:.1} sort={:.3}s insert={:.3}s insert_batch_max={:.3}s commit={:.3}s total={:.3}s",
        total,
        chunk_size,
        insert_batches,
        insert_rows_affected,
        sqlite_delta.lock_retries,
        sqlite_delta.lock_backoff_ms_total,
        sqlite_delta.db_write_time_ms(),
        sort_elapsed.as_secs_f64(),
        insert_elapsed.as_secs_f64(),
        insert_batch_max_elapsed.as_secs_f64(),
        commit_elapsed.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;

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

    #[test]
    fn download_target_raw_sql_uses_bulk_variable_limit() {
        assert_eq!(download_file_target_upsert_chunk_size(), 8_190);
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
}
