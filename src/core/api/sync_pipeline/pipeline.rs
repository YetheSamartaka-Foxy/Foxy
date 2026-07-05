use super::super::quick_scan::{
    apply_download_target_estimates_to_pending_updates,
    apply_patch_plan_estimates_to_pending_updates, collect_files_with_missing_local_tree_hashes,
    collect_repo_download_targets, collect_unexpected_files_for_repo_mods,
    delete_unexpected_local_files, format_local_path_mismatch_message, log_addon_path_disk_state,
    log_local_path_availability, pending_update_mod_scope, persist_pending_updates,
    quick_local_change_diff, refresh_content_hashes_for_scoped_tree,
    refresh_content_hashes_for_tree, refresh_content_hashes_when_tree_matches,
    refresh_patch_plan_metadata_for_pending_updates, summarize_local_path_availability,
    suspect_local_path_mismatch, tree_local_checksums_baseline_missing,
    tree_local_checksums_missing,
};
use super::super::*;
use super::backup::backup_pending_addons_for_download;
use super::hashing::{
    HashTotalSummary, SqlitePerfRunGuard, render_aggregated_addon_hash_metrics,
    render_hash_total_summary, run_incremental_hash_batch,
};
use super::summary::{PipelineSummary, StageEntry};
use crate::core::api::FileDiffKind;
use crate::core::db::{DbValue, FoxyDb, params};
use crate::core::models::download_target_file::fetch_all_download_targets_with_mod_and_name;
use crate::core::models::modification::ADDON_COLUMNS;
use crate::core::models::pending_update::fetch_pending_update_for_context;
use crate::core::models::repository::load_repository_by_remote_url_and_local_path;
use crate::core::tasks::calculate_hashes::{
    AddonHashMetrics, HashCalculationResult, HashPhaseTimings, RepositoryHashContext,
    calculate_hashes_for_files_in_tree_with_profile, calculate_hashes_for_files_with_profile,
    calculate_hashes_with_profile, calculate_hashes_with_tree_and_profile_cancellable,
    finalize_repository_hashes_from_mods, finalize_repository_hashes_from_tree,
    pre_propagate_sibling_checksums, propagate_checksums_to_siblings,
};
use crate::core::tasks::download_files::{
    DownloadRunReport, UpdateRollbackSession, apply_download_plan_bytes,
    build_download_estimate_diffs, download_files,
};
use crate::core::tasks::purge_repository::purge_repository_instance;
use crate::core::tasks::remote_file_parts::flush_deferred_part_inserts;
use crate::core::tasks::remote_repository::{probe_remote_repository_checksum, remote_repository};
use crate::core::tasks::truncate_download_targets::truncate_all_download_tables;
use crate::core::utils::app_paths;
use crate::core::utils::format::{sanitize_log_path_str, sanitize_log_url};
use crate::ui::types::HashIoProfilePreference;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

// Byte size is the main signal for useful overlapped hashing. The threshold is
// kept large enough to avoid paying a DB persist cycle for every small
// completion burst, but small enough that hashing starts overlapping the
// download soon after the first mods finish rather than idling until a quarter
// gigabyte has piled up.
const INCREMENTAL_HASH_MIN_FILES: usize = 32;
const INCREMENTAL_HASH_MIN_BYTES: u64 = 128 * 1024 * 1024;
const AUTO_REBENCHMARK_DOWNLOAD_PERCENT_STEP: u64 = 10;
const AUTO_REBENCHMARK_DOWNLOAD_PERCENT_DENOMINATOR: u64 = 100;
const PATCH_PLAN_TINY_FILE_THRESHOLD_BYTES: i64 = 64 * 1024;
const SUSPECT_MISSING_ADDON_RATIO_NUMERATOR: usize = 9;
const SUSPECT_MISSING_ADDON_RATIO_DENOMINATOR: usize = 10;
const SUSPECT_MISSING_ADDON_MIN_ENABLED: usize = 5;
const SUSPECT_PARTIAL_MISSING_ADDON_RATIO_NUMERATOR: usize = 1;
const SUSPECT_PARTIAL_MISSING_ADDON_RATIO_DENOMINATOR: usize = 2;
const SUSPECT_PARTIAL_MISSING_ADDON_MIN_MISSING: usize = 20;
const SUSPECT_MISSING_ADDON_SAMPLE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoRebenchmarkMilestone {
    percent: u64,
    threshold_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct AutoRebenchmarkSchedule {
    estimated_total_bytes: u64,
    next_percent: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MissingAddonPathSummary {
    enabled_addons: usize,
    missing_addons: usize,
    empty_repo_root: bool,
    sample_paths: Vec<String>,
}

impl AutoRebenchmarkSchedule {
    fn new(estimated_total_bytes: u64) -> Option<Self> {
        (estimated_total_bytes > 0).then_some(Self {
            estimated_total_bytes,
            next_percent: AUTO_REBENCHMARK_DOWNLOAD_PERCENT_STEP,
        })
    }

    fn next_milestone(&self) -> Option<AutoRebenchmarkMilestone> {
        (self.next_percent < AUTO_REBENCHMARK_DOWNLOAD_PERCENT_DENOMINATOR).then(|| {
            AutoRebenchmarkMilestone {
                percent: self.next_percent,
                threshold_bytes: self.threshold_for_percent(self.next_percent),
            }
        })
    }

    fn take_crossed_milestone(
        &mut self,
        completed_download_bytes: u64,
    ) -> Option<AutoRebenchmarkMilestone> {
        let milestone = self.next_milestone()?;
        if completed_download_bytes < milestone.threshold_bytes {
            return None;
        }

        self.advance_past(completed_download_bytes);
        Some(milestone)
    }

    fn advance_past(&mut self, completed_download_bytes: u64) {
        while let Some(milestone) = self.next_milestone() {
            if milestone.threshold_bytes > completed_download_bytes {
                break;
            }
            self.next_percent = self
                .next_percent
                .saturating_add(AUTO_REBENCHMARK_DOWNLOAD_PERCENT_STEP);
        }
    }

    fn threshold_for_percent(&self, percent: u64) -> u64 {
        self.estimated_total_bytes
            .saturating_mul(percent)
            .div_ceil(AUTO_REBENCHMARK_DOWNLOAD_PERCENT_DENOMINATOR)
            .max(1)
    }
}

fn missing_addon_ratio_is_suspect(summary: &MissingAddonPathSummary) -> bool {
    summary.enabled_addons >= SUSPECT_MISSING_ADDON_MIN_ENABLED
        && summary
            .missing_addons
            .saturating_mul(SUSPECT_MISSING_ADDON_RATIO_DENOMINATOR)
            >= summary
                .enabled_addons
                .saturating_mul(SUSPECT_MISSING_ADDON_RATIO_NUMERATOR)
}

fn partial_missing_addon_ratio_is_suspect(summary: &MissingAddonPathSummary) -> bool {
    summary.missing_addons >= SUSPECT_PARTIAL_MISSING_ADDON_MIN_MISSING
        && summary
            .missing_addons
            .saturating_mul(SUSPECT_PARTIAL_MISSING_ADDON_RATIO_DENOMINATOR)
            >= summary
                .enabled_addons
                .saturating_mul(SUSPECT_PARTIAL_MISSING_ADDON_RATIO_NUMERATOR)
}

fn suspect_full_redownload_guard_applies(
    summary: &MissingAddonPathSummary,
    recent_local_path_reset: bool,
    repo_already_complete: bool,
    allow_suspect_full_redownload: bool,
) -> bool {
    !allow_suspect_full_redownload
        && !summary.empty_repo_root
        && (recent_local_path_reset || repo_already_complete)
        && (missing_addon_ratio_is_suspect(summary)
            || partial_missing_addon_ratio_is_suspect(summary))
}

fn local_path_mismatch_guard_applies(mode: SyncMode) -> bool {
    matches!(mode, SyncMode::RecheckOnly | SyncMode::RemoteRefreshOnly)
}

fn should_build_download_plan(mode: SyncMode, prepare_download_plan: bool) -> bool {
    mode == SyncMode::Download || prepare_download_plan
}

fn should_refresh_delta_plan_after_quick_verify(
    builds_download_plan: bool,
    has_pending_updates: bool,
    delta_plan_estimate_refreshed: bool,
) -> bool {
    !builds_download_plan && has_pending_updates && !delta_plan_estimate_refreshed
}

fn quick_verify_already_eligible(cached_pending_scope: Option<&HashSet<String>>) -> bool {
    cached_pending_scope.is_some_and(|scope| !scope.is_empty())
}

fn should_defer_remote_metadata_part_inserts(mode: SyncMode, force_redownload: bool) -> bool {
    !force_redownload
        && matches!(
            mode,
            SyncMode::Download
                | SyncMode::RecheckIntegrity
                | SyncMode::RecheckOnly
                | SyncMode::RemoteRefreshOnly
        )
}

fn should_queue_download_targets_during_remote_metadata(
    mode: SyncMode,
    force_redownload: bool,
) -> bool {
    mode == SyncMode::Download && force_redownload
}

/// True when this repository instance has file rows whose tree hash is missing
/// (`local_checksum` NULL/empty) AND ZERO part rows (`subfiles`). This is a
/// structurally broken state: the tree-hash rollup derives every file's
/// `local_checksum` from its parts, so with no parts the checksums can never be
/// filled and every quick scan falsely reports all addons as needing
/// re-download - a loop a local-only `QuickCheckOnly` can never break (it has no
/// parts to roll up and never fetches remote metadata to rebuild them). It
/// arises when a metadata rebuild deferred the manifest parts but the flush
/// never completed (an interrupted force-redownload, or the earlier
/// deferral-flush gap).
///
/// The missing-tree-hash condition is what distinguishes this corruption from a
/// legitimate whole-file manifest (which also has zero parts but whose files DO
/// carry tree hashes - see `remote_state_complete_accepts_whole_file_manifest_without_parts`):
/// without it we would needlessly re-escalate every healthy part-free repo on
/// every quick check. Scoped to the (url, local_path) instance, so a healthy
/// sibling sharing the same URL in a repository space is unaffected. On query
/// error we return false (never escalate on a transient failure).
async fn repository_has_files_but_no_parts(db: &FoxyDb, repository_id: i64) -> bool {
    let has_unhashed_files = match db
        .query_all(
            "SELECT 1 FROM repository_addons ra \
             JOIN addon_files af ON af.addon_id = ra.addon_id \
             JOIN files f ON f.id = af.file_id \
             WHERE ra.repository_id = ? \
               AND (f.local_checksum IS NULL OR f.local_checksum = '') LIMIT 1",
            params![repository_id],
        )
        .await
    {
        Ok(rows) => !rows.is_empty(),
        Err(e) => {
            warn!("Could not check repository file presence for part-rebuild escalation: {e}");
            return false;
        }
    };
    if !has_unhashed_files {
        return false;
    }
    let has_parts = match db
        .query_all(
            "SELECT 1 FROM subfiles sf \
             JOIN addon_files af ON af.file_id = sf.file_id \
             JOIN repository_addons ra ON ra.addon_id = af.addon_id \
             WHERE ra.repository_id = ? LIMIT 1",
            params![repository_id],
        )
        .await
    {
        Ok(rows) => !rows.is_empty(),
        Err(e) => {
            warn!("Could not check repository part presence for part-rebuild escalation: {e}");
            return false;
        }
    };
    !has_parts
}

fn format_compact_elapsed_duration(duration: std::time::Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{} ms", duration.as_millis())
    } else if duration.as_secs() < 10 {
        format!("{:.2} s", duration.as_secs_f64())
    } else {
        format!("{:.1} s", duration.as_secs_f64())
    }
}

async fn collect_missing_addon_path_summary(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mod_enabled_overrides: &HashMap<String, bool>,
) -> Option<MissingAddonPathSummary> {
    let repo = match load_repository_by_remote_url(context.clone(), repo_url).await {
        Ok(repo) => repo,
        Err(err) => {
            warn!(
                "Skipping suspect full-redownload guard for repo={}: failed to load repository: {}",
                repo_url, err
            );
            return None;
        }
    };

    let db = context.db();
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
                "Skipping suspect full-redownload guard for repo={}: failed to load repository addons: {}",
                repo_url, err
            );
            return None;
        }
    };
    mod_ids.sort_unstable();
    mod_ids.dedup();
    if mod_ids.is_empty() {
        return Some(MissingAddonPathSummary {
            enabled_addons: 0,
            missing_addons: 0,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        });
    }

    let chunk_size = read_chunk_ids();
    let mut mods = Vec::new();
    for chunk in mod_ids.chunks(chunk_size) {
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
                warn!(
                    "Skipping suspect full-redownload guard for repo={}: failed to load addons: {}",
                    repo_url, err
                );
                return None;
            }
        }
    }

    let mut enabled_addons = 0usize;
    let mut missing_addons = 0usize;
    let mut sample_paths = Vec::new();
    let empty_repo_root = {
        let root = Path::new(repo.local_path.trim());
        root.is_dir()
            && root
                .read_dir()
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false)
    };
    for addon in mods {
        let is_enabled = mod_enabled_overrides
            .get(&addon.name.to_lowercase())
            .copied()
            .unwrap_or(addon.enabled);
        if !is_enabled {
            continue;
        }

        enabled_addons += 1;
        let local_path = addon.local_path.trim();
        if local_path.is_empty() || !Path::new(local_path).is_dir() {
            missing_addons += 1;
            if sample_paths.len() < SUSPECT_MISSING_ADDON_SAMPLE_LIMIT {
                sample_paths.push(local_path.to_string());
            }
        }
    }

    Some(MissingAddonPathSummary {
        enabled_addons,
        missing_addons,
        empty_repo_root,
        sample_paths,
    })
}

fn format_suspect_full_redownload_message(
    local_path: &str,
    summary: &MissingAddonPathSummary,
) -> String {
    let sample = summary
        .sample_paths
        .iter()
        .filter(|path| !path.is_empty())
        .take(3)
        .map(|path| sanitize_log_path_str(path))
        .collect::<Vec<_>>()
        .join("; ");
    let sample_suffix = if sample.is_empty() {
        String::new()
    } else {
        format!(" Sample missing addon paths: {sample}.")
    };

    format!(
        "Update paused: {}/{} enabled addon folders are missing under the configured repository path ({}). Verify that the local path points to the folder that directly contains the @addon folders, or use Force redownload if this full download is intentional.{}",
        summary.missing_addons,
        summary.enabled_addons,
        sanitize_log_path_str(local_path),
        sample_suffix
    )
}

fn render_final_update_report(
    operation_id: &str,
    repository_url: &str,
    elapsed: Duration,
    download_report: &DownloadRunReport,
    hash_total_summary: &HashTotalSummary<'_>,
    addon_hash_metrics: &[AddonHashMetrics],
    sqlite_perf_guard: &SqlitePerfRunGuard,
) -> String {
    let mut lines = Vec::new();
    lines.push("============================================================".to_owned());
    lines.push("FOXY UPDATE FINAL REPORT".to_owned());
    lines.push(format!(
        "operation={} repo={} elapsed={:.2}s",
        operation_id,
        sanitize_log_url(repository_url),
        elapsed.as_secs_f64()
    ));
    lines.push("============================================================".to_owned());
    lines.push(download_report.render());
    lines.push(render_hash_total_summary(
        hash_total_summary,
        addon_hash_metrics,
    ));
    let addon_hash_summary = render_aggregated_addon_hash_metrics(addon_hash_metrics);
    if !addon_hash_summary.is_empty() {
        lines.push(addon_hash_summary);
    }
    lines.push(sqlite_perf_guard.render_summary());
    lines.push("============================================================".to_owned());
    lines.push("END FOXY UPDATE FINAL REPORT".to_owned());
    lines.push("============================================================".to_owned());
    lines.join("\n")
}

/// Resolves once the user cancels the sync or the sender is dropped.
async fn sync_cancel_requested(cancel_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancel_rx.borrow() {
            return;
        }
        if cancel_rx.changed().await.is_err() {
            return;
        }
    }
}

async fn wait_for_download_resume(
    download_pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> bool {
    while *download_pause_rx.borrow() {
        if *cancel_rx.borrow() {
            return false;
        }
        tokio::select! {
            changed = download_pause_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = cancel_rx.changed() => {
                if changed.is_err() || *cancel_rx.borrow() {
                    return false;
                }
            }
        }
    }
    !*cancel_rx.borrow()
}

async fn estimate_download_queue_bytes(context: Arc<FoxyContext>, file_ids: &HashSet<u64>) -> u64 {
    let db = context.db();
    let chunk_size = read_chunk_ids();
    let mut total = 0u64;
    let mut ids: Vec<i64> = file_ids.iter().map(|file_id| *file_id as i64).collect();
    ids.sort_unstable();
    for chunk in ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql =
            format!("SELECT size FROM download_target_file WHERE file_id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                total = total.saturating_add(
                    rows.iter()
                        .map(|row| row.get_i64("size").unwrap_or(0).max(0) as u64)
                        .sum::<u64>(),
                );
            }
            Err(err) => {
                warn!("Failed to estimate download queue bytes: {}", err);
                return 0;
            }
        }
    }
    total
}

async fn existing_download_targets_are_tiny(
    context: Arc<FoxyContext>,
    file_ids: &HashSet<u64>,
) -> bool {
    if file_ids.is_empty() {
        return false;
    }

    let db = context.db();
    let chunk_size = read_chunk_ids();
    let mut checked = 0usize;
    let mut ids: Vec<i64> = file_ids.iter().map(|file_id| *file_id as i64).collect();
    ids.sort_unstable();

    for chunk in ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql =
            format!("SELECT size FROM download_target_file WHERE file_id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(rows) => {
                for row in rows {
                    checked += 1;
                    if row.get_i64("size").unwrap_or(0) > PATCH_PLAN_TINY_FILE_THRESHOLD_BYTES {
                        return false;
                    }
                }
            }
            Err(err) => {
                warn!(
                    "Failed to inspect existing download target sizes for patch refresh guard: {}",
                    err
                );
                return false;
            }
        }
    }

    checked == file_ids.len()
}

async fn invalidate_force_redownload_hash_baseline(
    context: Arc<FoxyContext>,
    file_ids: &HashSet<u64>,
) -> Result<u64, crate::core::db::DbErr> {
    if file_ids.is_empty() {
        return Ok(0);
    }

    let db = context.db();
    let chunk_size = SQLITE_MAX_VARIABLES.saturating_sub(10).max(1);
    let mut ids: Vec<i64> = file_ids.iter().map(|file_id| *file_id as i64).collect();
    ids.sort_unstable();

    let mut affected = 0u64;
    for chunk in ids.chunks(chunk_size) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        affected = affected.saturating_add(
            db.execute_retry(
                "invalidate force-redownload file baseline",
                &format!(
                    "UPDATE files \
                     SET local_checksum = '', local_content_hash = '' \
                     WHERE id IN ({placeholders}) \
                       AND (local_checksum != '' OR local_content_hash != '')"
                ),
                values,
            )
            .await?,
        );

        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        affected = affected.saturating_add(
            db.execute_retry(
                "invalidate force-redownload part baseline",
                &format!(
                    "UPDATE subfiles \
                     SET local_checksum = '', local_length = 0, local_start = 0 \
                     WHERE file_id IN ({placeholders}) \
                       AND (local_checksum != '' OR local_length != 0 OR local_start != 0)"
                ),
                values,
            )
            .await?,
        );
    }

    Ok(affected)
}

fn should_rebenchmark_auto_profile(
    requested_profile: HashIoProfilePreference,
    rebenchmark_pending: bool,
    completed_download_bytes: u64,
    threshold_bytes: u64,
) -> bool {
    requested_profile == HashIoProfilePreference::Auto
        && !rebenchmark_pending
        && threshold_bytes > 0
        && completed_download_bytes >= threshold_bytes
}

async fn reconcile_target_repository_local_checksum(
    context: Arc<FoxyContext>,
    repo_url: &str,
) -> Result<u64, crate::core::db::DbErr> {
    let repository = load_repository_by_remote_url(context.clone(), repo_url).await?;
    context
        .db()
        .execute(
            r#"UPDATE repositories
               SET local_checksum = remote_checksum
               WHERE id = ?
                 AND remote_checksum != ''
                 AND local_checksum != remote_checksum"#,
            params![repository.id as i64],
        )
        .await
}

/// Decide whether the confirmation-prepared download queue can be reused for a
/// `Download` run instead of re-running the full prepare pipeline (remote
/// refresh, hash bootstrap, quick verify and queue rebuild).
///
/// Reuse is safe only when all of the following hold:
/// - a genuine pending update is recorded for this instance (`remote_checksum`
///   is non-empty and differs from `local_checksum`),
/// - a confirmation preflight left a pending-update payload behind,
/// - a prepared download queue already exists for this repository instance, and
/// - a cheap `repo.json` re-probe shows the remote repository checksum is
///   unchanged since the queue was built.
///
/// Patch-first execution and the final hash pass still re-validate every file,
/// so a stale or incomplete queue degrades to a retry, never silent corruption.
async fn can_reuse_prepared_download_queue(
    context: Arc<FoxyContext>,
    repo_url: &str,
    hash_algorithm_preference: crate::ui::types::HashAlgorithmPreference,
) -> bool {
    let repo = match load_repository_by_remote_url(context.clone(), repo_url).await {
        Ok(repo) => repo,
        Err(_) => return false,
    };
    // Cheap gate first so we only pay for the network probe when reuse is otherwise
    // plausible: a genuine pending update must be recorded for this instance.
    if !pending_update_recorded(&repo.remote_checksum, &repo.local_checksum) {
        return false;
    }
    // A confirmation preflight must have left a pending payload behind.
    if !matches!(
        fetch_pending_update_for_context(context.clone(), repo_url).await,
        Ok(Some(_))
    ) {
        return false;
    }
    // ...and a prepared download queue for this repository instance.
    let (file_ids, _) = collect_repo_download_targets(context.clone(), repo_url, None).await;
    if file_ids.is_empty() {
        return false;
    }
    // Confirm the remote repository has not changed since the queue was built.
    let probed =
        probe_remote_repository_checksum(context, repo_url, hash_algorithm_preference).await;
    prepared_queue_reuse_is_safe(
        &repo.remote_checksum,
        &repo.local_checksum,
        probed.as_deref(),
    )
}

/// Whether a genuine pending update is recorded for an instance: the remote
/// checksum is known and differs from the verified local checksum.
fn pending_update_recorded(remote_checksum: &str, local_checksum: &str) -> bool {
    let remote = remote_checksum.trim();
    !remote.is_empty() && !remote.eq_ignore_ascii_case(local_checksum.trim())
}

/// Pure reuse decision: the prepared queue is safe to reuse only when a genuine
/// pending update is recorded AND a fresh `repo.json` probe shows the remote
/// repository checksum is unchanged since the queue was built.
fn prepared_queue_reuse_is_safe(
    stored_remote_checksum: &str,
    stored_local_checksum: &str,
    probed_remote_checksum: Option<&str>,
) -> bool {
    if !pending_update_recorded(stored_remote_checksum, stored_local_checksum) {
        return false;
    }
    matches!(
        probed_remote_checksum,
        Some(probed) if probed.trim().eq_ignore_ascii_case(stored_remote_checksum.trim())
    )
}

/// Load the confirmation-prepared pending-update diff (the exact queue the
/// preflight emitted) so a reused download run can show and back up the same
/// addons without recomputing them.
async fn load_pending_update_mods(
    context: Arc<FoxyContext>,
    repo_url: &str,
) -> Vec<ModDiffSummary> {
    match fetch_pending_update_for_context(context, repo_url).await {
        Ok(Some(payload)) => serde_json::from_str(&payload).unwrap_or_else(|err| {
            warn!(
                "Failed to parse prepared pending updates for reuse repo={}: {}",
                repo_url, err
            );
            Vec::new()
        }),
        _ => Vec::new(),
    }
}

async fn run_repository_pipeline(
    repository_url: String,
    local_path: String,
    mod_enabled_overrides: HashMap<String, bool>,
    progress_tx: Sender<ProgressEvent>,
    mut mode: SyncMode,
    options: RepositorySyncOptions,
) {
    let RepositorySyncOptions {
        operation_id,
        prepare_download_plan,
        repository_space_shared_path,
        auto_backup_directory,
        rollback_temp_directory,
        download_speed_limit_mbps,
        recent_local_path_reset,
        force_redownload,
        allow_suspect_full_redownload,
        mut download_pause_rx,
        mut cancel_rx,
        hash_algorithm_preference,
        hash_io_profile,
    } = options;
    let mut builds_download_plan = should_build_download_plan(mode, prepare_download_plan);
    macro_rules! emit_progress {
        ($event:expr) => {
            send_progress_event(&progress_tx, $event, &operation_id)
        };
    }
    let overall_start = std::time::Instant::now();
    ensure_logger();
    info!(
        "Starting sync: op={} mode={:?} repo={} path={}",
        operation_id,
        mode,
        sanitize_log_url(&repository_url),
        sanitize_log_path_str(&local_path)
    );
    // DATABASE_URL is set once at startup in main.rs to avoid unsafe env::set_var
    // race conditions in multi-threaded context.

    let mut stage = std::time::Instant::now();

    send_progress_event(
        &progress_tx,
        ProgressEvent::Stage {
            label: "Preparing".into(),
            percent: 0.05,
        },
        &operation_id,
    );

    // Normalize remote URL once so all stages use the same key that matches DB storage (trailing slash)
    let normalized_repo_url = if repository_url.ends_with('/') {
        repository_url.clone()
    } else {
        format!("{}/", repository_url)
    };
    let mut summary = PipelineSummary::new(
        operation_id.clone(),
        format!("{:?}", mode),
        &normalized_repo_url,
        overall_start,
    );
    let mut sqlite_perf_guard =
        SqlitePerfRunGuard::start(normalized_repo_url.clone(), mode, overall_start);

    // Surface startup DB maintenance while context creation is blocked.
    if crate::core::tasks::db_turso::db_startup_compaction_active() {
        send_progress_event(
            &progress_tx,
            ProgressEvent::Stage {
                label: "Optimizing database".into(),
                percent: 0.05,
            },
            &operation_id,
        );
    }
    let base_context = tokio::select! {
        context = create_context_with_recheck_level(RecheckLevel::DEFAULT) => context,
        _ = sync_cancel_requested(&mut cancel_rx) => {
            info!(
                "Sync cancelled while waiting for database availability: op={} repo={}",
                operation_id,
                sanitize_log_url(&repository_url)
            );
            send_progress_event(&progress_tx, ProgressEvent::Cancelled, &operation_id);
            return;
        }
    };
    let mut context = Arc::new(
        base_context
            .as_ref()
            .clone()
            .with_download_target_queueing(builds_download_plan)
            .with_force_download_targets(mode == SyncMode::Download && force_redownload)
            .with_target_local_path(local_path.clone())
            .with_repository_space_shared_path(repository_space_shared_path.clone()),
    );
    summary.push(StageEntry::new("create_context", stage.elapsed()));
    stage = std::time::Instant::now();

    // Self-heal a part-less repository. A repo whose files exist on disk but
    // whose `subfiles` (parts) were lost - e.g. an interrupted force-redownload
    // or the deferred-part flush gap - can never be repaired by a local-only
    // QuickCheckOnly: the tree-hash rollup reads from parts, so with zero parts
    // every file checksum stays NULL and every scan falsely re-flags all addons
    // for download forever. Escalate it to a forced (RecheckLevel::REPOSITORY)
    // RemoteRefreshOnly: that re-fetches the manifest, rebuilds the parts (the
    // "complete" gate treats `part_count == 0` as a valid whole-file manifest,
    // so without forcing it the unchanged-graph skip would never rebuild them),
    // and hashes the existing on-disk files - no file content is re-downloaded.
    if mode == SyncMode::QuickCheckOnly && !force_redownload {
        let part_less = match load_repository_by_remote_url_and_local_path(
            context.clone(),
            &normalized_repo_url,
            &local_path,
        )
        .await
        {
            Ok(repo) => repository_has_files_but_no_parts(&context.db(), repo.id as i64).await,
            Err(_) => false,
        };
        if part_less {
            warn!(
                "Repository {} has file rows but zero part rows (subfiles); a local quick check cannot rebuild its tree hashes. Escalating QuickCheckOnly to a forced RemoteRefreshOnly to rebuild parts from remote metadata and re-hash the on-disk files (no content re-download).",
                sanitize_log_url(&normalized_repo_url)
            );
            mode = SyncMode::RemoteRefreshOnly;
            builds_download_plan = should_build_download_plan(mode, prepare_download_plan);
            context = Arc::new(
                create_context_with_recheck_level(RecheckLevel::REPOSITORY)
                    .await
                    .as_ref()
                    .clone()
                    .with_download_target_queueing(builds_download_plan)
                    .with_target_local_path(local_path.clone())
                    .with_repository_space_shared_path(repository_space_shared_path.clone()),
            );
            summary.push(
                StageEntry::new("part_rebuild_escalation", stage.elapsed())
                    .with("reason", "files_without_parts"),
            );
            stage = std::time::Instant::now();
        }
    }

    if mode == SyncMode::Download && force_redownload {
        send_progress_event(
            &progress_tx,
            ProgressEvent::Stage {
                label: "Purging local repository".into(),
                percent: 0.06,
            },
            &operation_id,
        );
        match purge_repository_instance(context.clone(), &normalized_repo_url, &local_path).await {
            Ok(()) => {
                summary.push(StageEntry::new("force_redownload_purge", stage.elapsed()));
                stage = std::time::Instant::now();
            }
            Err(err) => {
                let message = format!(
                    "Failed to purge repository before force redownload for repo {}: {}",
                    sanitize_log_url(&normalized_repo_url),
                    err
                );
                error!("{}", message);
                summary.log_table("failed-force-redownload-purge");
                emit_progress!(ProgressEvent::Failed(message));
                return;
            }
        }
    }

    let mut download_file_ids: HashSet<u64> = HashSet::new();
    let mut download_mod_ids: HashSet<u64> = HashSet::new();
    let mut quick_update_mod_names: HashSet<String> = HashSet::new();
    let mut delta_plan_estimate_refreshed = false;
    // The download-path mod diff. Normally produced by the post-remote quick
    // verify; for a reused confirmation queue it is loaded from the prepared
    // pending-update payload instead.
    let mut mods: Vec<ModDiffSummary> = Vec::new();
    let cached_pending_scope = pending_update_mod_scope(&context.db(), &normalized_repo_url).await;

    if *cancel_rx.borrow() {
        info!(
            "Sync cancelled before main pipeline: op={} repo={}",
            operation_id,
            sanitize_log_url(&normalized_repo_url)
        );
        send_progress_event(&progress_tx, ProgressEvent::Cancelled, &operation_id);
        return;
    }

    // Fast path: a Download that immediately follows a confirmation preflight can
    // reuse the queue that preflight already built, as long as the remote
    // repository checksum and local-path identity are unchanged. This skips the
    // redundant remote refresh, hash bootstrap, quick verify and queue rebuild
    // (the second ~7s pass) and goes straight to backup + transfer.
    let mut reuse_prepared_queue = mode == SyncMode::Download
        && !force_redownload
        && !recent_local_path_reset
        && can_reuse_prepared_download_queue(
            context.clone(),
            &normalized_repo_url,
            hash_algorithm_preference,
        )
        .await;
    if reuse_prepared_queue {
        mods = load_pending_update_mods(context.clone(), &normalized_repo_url).await;
        if mods.iter().any(|m| m.needs_update) {
            info!(
                "Reusing confirmation-prepared download queue for repo={} (remote checksum unchanged; skipping remote refresh, hash bootstrap and queue rebuild)",
                normalized_repo_url
            );
            emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
            summary.push(StageEntry::new("reuse_prepared_queue", stage.elapsed()));
            stage = std::time::Instant::now();
        } else {
            warn!(
                "Prepared queue payload had no pending updates for repo={}; falling back to full prepare",
                normalized_repo_url
            );
            reuse_prepared_queue = false;
            mods.clear();
        }
    }

    if builds_download_plan && !reuse_prepared_queue {
        let quick_verify_start = std::time::Instant::now();
        send_progress_event(
            &progress_tx,
            ProgressEvent::Stage {
                label: "Quick local verify".into(),
                percent: 0.10,
            },
            &operation_id,
        );
        if let Some(scope) = cached_pending_scope.as_ref() {
            info!(
                "Scoping quick local verify to cached pending addons for repo={} addons={}",
                normalized_repo_url,
                scope.len()
            );
        }

        let mut quick_diffs = quick_local_change_diff(
            context.clone(),
            &normalized_repo_url,
            cached_pending_scope.as_ref(),
            Some(&mod_enabled_overrides),
            Some(&progress_tx),
            true,
            quick_verify_already_eligible(cached_pending_scope.as_ref()),
            false,
            None,
        )
        .await;
        if cached_pending_scope.is_some() && !quick_diffs.iter().any(|m| m.needs_update) {
            info!(
                "Scoped quick local verify found no updates for repo={}, falling back to full-repo quick verify",
                normalized_repo_url
            );
            quick_diffs = quick_local_change_diff(
                context.clone(),
                &normalized_repo_url,
                None,
                Some(&mod_enabled_overrides),
                Some(&progress_tx),
                true,
                false,
                false,
                None,
            )
            .await;
        }
        let has_quick_updates = quick_diffs.iter().any(|m| m.needs_update);
        quick_update_mod_names = quick_diffs
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.name.to_lowercase())
            .collect();
        persist_pending_updates(context.clone(), &normalized_repo_url, &quick_diffs).await;
        info!(
            "Quick local verify finished in {:.2?} (updates={})",
            quick_verify_start.elapsed(),
            has_quick_updates
        );
        summary.push(
            StageEntry::new("quick_local_verify", quick_verify_start.elapsed())
                .with("mods", quick_diffs.len())
                .with("mods_with_updates", quick_update_mod_names.len())
                .with("scoped", cached_pending_scope.is_some()),
        );

        let (file_ids, _) =
            collect_repo_download_targets(context.clone(), &normalized_repo_url, None).await;
        if file_ids.is_empty() {
            info!(
                "No cached download targets for repo={}, rebuilding from remote recheck",
                normalized_repo_url
            );
        } else if !force_redownload && !has_quick_updates {
            info!(
                "Quick verify found no updates for repo={}, skipping download and clearing cached queue",
                normalized_repo_url
            );
            if let Err(err) = truncate_all_download_tables(context.clone()).await {
                warn!(
                    "Failed to clear cached download queue after clean quick verify: {}",
                    err
                );
            }
            summary.push(
                StageEntry::new("quick_verify_only", overall_start.elapsed())
                    .with("updates", false),
            );
            summary.log_table("early-exit-clean");
            emit_progress!(ProgressEvent::Diff { mods: quick_diffs });
            emit_progress!(ProgressEvent::Finished);
            return;
        } else {
            // Local quick verification changed checksum state in DB, so stale cached targets can be
            // inaccurate. Rebuild queue from remote recheck for correctness.
            info!(
                "Cached targets exist for repo={} but quick verify found updates; forcing queue rebuild",
                normalized_repo_url
            );
        }
    }

    if mode == SyncMode::RecheckIntegrity {
        if *cancel_rx.borrow() {
            emit_progress!(ProgressEvent::Cancelled);
            return;
        }
        let integrity_start = std::time::Instant::now();
        let mut integrity_stage = std::time::Instant::now();

        // Phase 1: Full remote metadata fetch
        emit_progress!(ProgressEvent::Stage {
            label: "Fetching repository metadata".into(),
            percent: 0.05,
        });
        let repo_metadata = remote_repository(
            context.clone(),
            &normalized_repo_url,
            Some(&local_path),
            Some(&mod_enabled_overrides),
            true, // force_refresh - always fetch remote for integrity recheck
            hash_algorithm_preference,
        )
        .await;
        if let Some(metadata) = &repo_metadata {
            emit_progress!(ProgressEvent::RepositoryFoxyMode {
                is_foxy: metadata.foxy_mode.is_foxy(),
                app_update_url: metadata.app_update_url.clone(),
            });
        }
        summary.push(StageEntry::new(
            "remote_metadata_fetch",
            integrity_stage.elapsed(),
        ));
        integrity_stage = std::time::Instant::now();

        // Phase 1.5: Pre-propagate sibling checksums to avoid redundant hashing
        emit_progress!(ProgressEvent::Stage {
            label: "Pre-propagating sibling checksums".into(),
            percent: 0.15,
        });
        pre_propagate_sibling_checksums(context.clone(), &normalized_repo_url).await;
        summary.push(StageEntry::new(
            "pre_propagate_checksums",
            integrity_stage.elapsed(),
        ));
        integrity_stage = std::time::Instant::now();

        // Phase 2: Full local hash recalculation
        emit_progress!(ProgressEvent::Stage {
            label: "Recalculating file hashes".into(),
            percent: 0.20,
        });
        calculate_hashes_with_profile(
            context.clone(),
            &normalized_repo_url,
            Some(&progress_tx),
            hash_io_profile,
        )
        .await;
        summary.push(StageEntry::new(
            "hash_recalculation",
            integrity_stage.elapsed(),
        ));
        integrity_stage = std::time::Instant::now();

        emit_progress!(ProgressEvent::Stage {
            label: "Refreshing content hashes".into(),
            percent: 0.60,
        });
        let _ =
            refresh_content_hashes_when_tree_matches(context.clone(), &normalized_repo_url, None)
                .await;
        summary.push(StageEntry::new(
            "content_hash_refresh",
            integrity_stage.elapsed(),
        ));
        integrity_stage = std::time::Instant::now();

        emit_progress!(ProgressEvent::Stage {
            label: "Propagating checksums to sibling repositories".into(),
            percent: 0.65,
        });
        let propagated_sibling_urls =
            propagate_checksums_to_siblings(context.clone(), &normalized_repo_url).await;
        if !propagated_sibling_urls.is_empty() {
            emit_progress!(ProgressEvent::SiblingPropagation {
                repo_urls: propagated_sibling_urls,
            });
        }
        summary.push(StageEntry::new(
            "propagate_to_siblings",
            integrity_stage.elapsed(),
        ));
        integrity_stage = std::time::Instant::now();

        info!(
            "Integrity recheck hash recalculation finished in {:.2?}",
            integrity_start.elapsed()
        );

        if *cancel_rx.borrow() {
            emit_progress!(ProgressEvent::Cancelled);
            return;
        }

        // Phase 3: Build update status
        emit_progress!(ProgressEvent::Stage {
            label: "Building update status".into(),
            percent: 0.85,
        });
        let mods = quick_local_change_diff(
            context.clone(),
            &normalized_repo_url,
            None,
            Some(&mod_enabled_overrides),
            Some(&progress_tx),
            false,
            true, // already_eligible: tree hashes + content baseline were just recalculated
            true, // force_fresh_addon_hash: ensure addon content hash reflects current disk state
            None,
        )
        .await;
        emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
        persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
        let mods_with_updates = mods.iter().filter(|m| m.needs_update).count();
        summary.push(
            StageEntry::new("build_update_status", integrity_stage.elapsed())
                .with("mods", mods.len())
                .with("mods_with_updates", mods_with_updates),
        );
        summary.log_table("completed");

        emit_progress!(ProgressEvent::Stage {
            label: "Done".into(),
            percent: 1.0,
        });
        emit_progress!(ProgressEvent::Finished);
        return;
    }

    if mode == SyncMode::QuickCheckOnly {
        emit_progress!(ProgressEvent::Stage {
            label: "Quick local check".into(),
            percent: 0.20,
        });
        let quick_check_start = std::time::Instant::now();
        let quick_diff_start = std::time::Instant::now();
        let mods = quick_local_change_diff(
            context.clone(),
            &normalized_repo_url,
            None,
            Some(&mod_enabled_overrides),
            Some(&progress_tx),
            true,
            false,
            false,
            None,
        )
        .await;
        let quick_diff_elapsed = quick_diff_start.elapsed();
        let emit_diff_started = std::time::Instant::now();
        emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
        let emit_diff_elapsed = emit_diff_started.elapsed();
        let pending_persist_started = std::time::Instant::now();
        persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
        let pending_persist_elapsed = pending_persist_started.elapsed();
        let quick_check_elapsed = quick_check_start.elapsed();
        let quick_check_compact = format_compact_elapsed_duration(quick_check_elapsed);
        info!("Quick local check finished in {}", quick_check_compact);
        info!(
            "Quick local check finished in {:.2?} (updates={})",
            quick_check_elapsed,
            mods.iter().any(|m| m.needs_update)
        );
        info!(
            "Quick local check breakdown: repo={} total={:.2?} quick_diff={:.2?} emit_diff={:.2?} pending_persist={:.2?} mods={} mods_with_updates={}",
            normalized_repo_url,
            quick_check_elapsed,
            quick_diff_elapsed,
            emit_diff_elapsed,
            pending_persist_elapsed,
            mods.len(),
            mods.iter().filter(|m| m.needs_update).count()
        );
        summary.push(
            StageEntry::new("quick_local_check", quick_check_elapsed)
                .with("mods", mods.len())
                .with(
                    "mods_with_updates",
                    mods.iter().filter(|m| m.needs_update).count(),
                ),
        );
        summary.log_table("completed");
        emit_progress!(ProgressEvent::Stage {
            label: format!("Quick local check finished in {}", quick_check_compact),
            percent: 0.95,
        });
        emit_progress!(ProgressEvent::Stage {
            label: "Done".into(),
            percent: 1.0,
        });
        emit_progress!(ProgressEvent::Finished);
        return;
    }

    // Produce mod-level diff (needs_update + size) scoped to the current repository only
    let emit_diff = |tree: &Tree, mod_filter: Option<&HashSet<u64>>| -> Vec<ModDiffSummary> {
        let mut repo_mod_indices: Vec<usize> = tree
            .repo_nodes
            .iter()
            .flat_map(|repo_node| repo_node.mods.iter().copied())
            .collect();
        repo_mod_indices.sort_unstable();
        repo_mod_indices.dedup();

        let mut mods = Vec::new();
        let mut mod_count = 0usize;
        let mut file_count = 0usize;
        let mut part_count = 0usize;

        for mod_idx in repo_mod_indices {
            let Some(m) = tree.mods.get(mod_idx) else {
                continue;
            };
            let is_enabled = mod_enabled_overrides
                .get(&m.name.to_lowercase())
                .copied()
                .unwrap_or(m.enabled);
            if !is_enabled {
                continue;
            }
            if let Some(filter) = mod_filter
                && !filter.contains(&m.id)
            {
                continue;
            }

            let mut files = Vec::new();
            if let Some(node) = tree.mod_nodes.get(mod_idx) {
                for &file_idx in &node.files {
                    if let Some(f) = tree.files.get(file_idx) {
                        file_count += 1;
                        let mut changed_parts = 0usize;
                        if let Some(fnode) = tree.file_nodes.get(file_idx) {
                            part_count += fnode.parts.len();
                            for &part_idx in &fnode.parts {
                                if let Some(p) = tree.parts.get(part_idx)
                                    && p.local_checksum != p.remote_checksum
                                {
                                    changed_parts += 1;
                                }
                            }
                        }
                        let file_needs_update =
                            f.local_checksum != f.remote_checksum || changed_parts > 0;
                        if file_needs_update {
                            files.push(FileDiffSummary {
                                name: f.name.clone(),
                                needs_update: true,
                                total_bytes: f.length,
                                changed_parts,
                                change_kind: if f.local_checksum.is_empty() {
                                    FileDiffKind::Added
                                } else {
                                    FileDiffKind::Modified
                                },
                            });
                        }
                    }
                }
            }

            let total_bytes: u64 = files.iter().map(|f| f.total_bytes).sum();
            let needs_update = !files.is_empty() || m.local_checksum != m.remote_checksum;
            if needs_update {
                info!(
                    "Mod mismatch detected after recheck: repo={} mod={} local_checksum={} remote_checksum={} mismatched_files={} total_file_bytes={}",
                    normalized_repo_url,
                    m.name,
                    m.local_checksum,
                    m.remote_checksum,
                    files.len(),
                    total_bytes
                );
            }
            mods.push(ModDiffSummary {
                name: m.name.clone(),
                needs_update,
                total_bytes,
                files,
            });
            mod_count += 1;
        }

        emit_progress!(ProgressEvent::Diff { mods: mods.clone() });

        info!(
            "Recheck stats: mods={}, files={}, parts={}, elapsed_total={:.2?}",
            mod_count,
            file_count,
            part_count,
            overall_start.elapsed(),
        );

        mods
    };

    // Ensure download queues are clean for this repository run. Ordinary
    // check-only modes suppress target creation; confirmation preflight is the
    // explicit exception and leaves its freshly rebuilt queue for review.
    // When reusing a confirmation-prepared queue, this is exactly the queue we
    // intend to execute, so it must NOT be truncated here.
    if !reuse_prepared_queue {
        if let Err(e) = truncate_all_download_tables(context.clone()).await {
            warn!("Failed to clear download queue before sync: {}", e);
        } else {
            info!("Cleared download queue before sync");
        }
    }

    // Keep download path incremental: remote refresh + quick diff + targeted hash updates only.

    if *cancel_rx.borrow() {
        info!(
            "Sync cancelled before remote recheck for repo={}",
            normalized_repo_url
        );
        emit_progress!(ProgressEvent::Cancelled);
        return;
    }

    // Recheck against remote. Skipped entirely when reusing a confirmation-prepared
    // queue: the repo.json re-probe already confirmed the remote is unchanged.
    let mut remote_recheck_elapsed = None;
    let repo_metadata = if reuse_prepared_queue {
        None
    } else {
        let recheck_start = std::time::Instant::now();
        let remote_metadata_queues_download_targets =
            should_queue_download_targets_during_remote_metadata(mode, force_redownload);
        let remote_refresh_context = if builds_download_plan
            && !force_redownload
            && !quick_update_mod_names.is_empty()
        {
            info!(
                "Scoping any needed remote metadata refresh to {} pending addons during download queue rebuild",
                quick_update_mod_names.len()
            );
            Arc::new(
                context
                    .as_ref()
                    .clone()
                    .with_forced_mod_refreshes(quick_update_mod_names.clone())
                    .with_download_target_queueing(remote_metadata_queues_download_targets),
            )
        } else {
            Arc::new(
                context
                    .as_ref()
                    .clone()
                    .with_download_target_queueing(remote_metadata_queues_download_targets),
            )
        };

        // A++ (after_turso_regression_analysis7.md): on a force-redownload, let the
        // per-mod part insert buffer its brand-new rows for one background INSERT
        // overlapped with the download instead of writing the 66k rows inline on the
        // critical path. The flag is an `Arc<AtomicBool>` shared across context
        // clones, so the per-mod fan-out spawned inside `remote_repository` sees it.
        let defer_remote_metadata_part_inserts =
            should_defer_remote_metadata_part_inserts(mode, force_redownload);
        info!(
            "Remote metadata refresh options: mode={:?} builds_download_plan={} force_redownload={} queue_download_targets={} defer_part_inserts={}",
            mode,
            builds_download_plan,
            force_redownload,
            remote_metadata_queues_download_targets,
            force_redownload || defer_remote_metadata_part_inserts
        );
        if force_redownload || defer_remote_metadata_part_inserts {
            context.set_defer_part_inserts(true);
        }
        let repo_metadata = remote_repository(
            remote_refresh_context,
            &normalized_repo_url,
            Some(&local_path),
            Some(&mod_enabled_overrides),
            force_redownload,
            hash_algorithm_preference,
        )
        .await;
        let recheck_elapsed = recheck_start.elapsed();
        remote_recheck_elapsed = Some(recheck_elapsed);
        info!(
            "Remote data recheck finished in {}",
            format_compact_elapsed_duration(recheck_elapsed)
        );
        info!("Recheck finished in {:.2?}", recheck_elapsed);
        summary.push(StageEntry::new("remote_repository", stage.elapsed()));
        stage = std::time::Instant::now();
        repo_metadata
    };

    if let Some(metadata) = &repo_metadata {
        info!(
            "Remote repository gate result: repo={} skipped_clean={} remote_graph_complete={} remote_graph_fetched={}",
            normalized_repo_url,
            metadata.skipped,
            metadata.remote_graph_complete,
            metadata.remote_graph_fetched
        );
        emit_progress!(ProgressEvent::RepositoryFoxyMode {
            is_foxy: metadata.foxy_mode.is_foxy(),
            app_update_url: metadata.app_update_url.clone(),
        });
    }

    let recheck_completed_label = if mode == SyncMode::RemoteRefreshOnly {
        remote_recheck_elapsed
            .map(|elapsed| {
                format!(
                    "Remote data recheck finished in {}",
                    format_compact_elapsed_duration(elapsed)
                )
            })
            .unwrap_or_else(|| "Remote data recheck finished".into())
    } else {
        "Recheck completed".into()
    };
    emit_progress!(ProgressEvent::Stage {
        label: recheck_completed_label,
        percent: 0.25,
    });

    // Fast path: when RemoteRefreshOnly and remote said checksums already match,
    // skip the expensive local verification and return immediately.
    if mode == SyncMode::RemoteRefreshOnly
        && let Some(metadata) = &repo_metadata
        && metadata.skipped
    {
        info!(
            "Remote refresh skipped (checksums match) for repo={}; finishing early",
            normalized_repo_url
        );
        context.set_defer_part_inserts(false);
        summary.push(
            StageEntry::new("remote_refresh_skip", overall_start.elapsed())
                .with("reason", "checksums_match"),
        );
        summary.log_table("early-exit-skip");
        emit_progress!(ProgressEvent::Stage {
            label: "Done".into(),
            percent: 1.0,
        });
        emit_progress!(ProgressEvent::Finished);
        return;
    }

    // When the repository is already complete (has linked addons, remote state,
    // and non-empty checksums), we can skip the expensive Tree::load + hash
    // bootstrap check and go straight to the quick scan with `already_eligible`.
    // Reusing the prepared queue means the tree is already as initialized as the
    // preflight left it; treat the repo as complete so the bootstrap pass is skipped.
    let repo_already_complete = reuse_prepared_queue
        || repo_metadata
            .as_ref()
            .is_some_and(|m| m.repository_complete);

    // Pre-propagate checksums from sibling repositories that share the same local
    // file paths. This lets the hashing phase skip files that were already hashed
    // by a previously-synced sibling, saving significant I/O and CPU time.
    // Note: remote_repository() also calls pre-propagation for the first-sync case
    // (empty checksums), but the metadata rebuild may have created new records since
    // then. This call covers those; the early bail-out makes it cheap when redundant.
    if !reuse_prepared_queue {
        emit_progress!(ProgressEvent::Stage {
            label: "Pre-propagating sibling checksums".into(),
            percent: 0.27,
        });
        pre_propagate_sibling_checksums(context.clone(), &normalized_repo_url).await;
    }

    // First run after fresh remote metadata can have empty local tree checksums.
    // Initialize them once so subsequent quick checks can rely on persisted tree state.
    // Load the tree once and reuse it for both the check and the hashing phase,
    // avoiding redundant 622K-row subfiles queries.
    // When the repo is already complete, skip this entirely - tree hashes are already
    // initialized and the quick scan preflight would just confirm that.
    let mut full_tree_hash_bootstrap = false;
    let mut targeted_tree_hash_init = false;
    let mut bootstrap_tree_for_content_hash: Option<Tree> = None;
    let scoped_tree_bootstrap = builds_download_plan && !quick_update_mod_names.is_empty();
    let bootstrap_tree_result = if !repo_already_complete {
        if scoped_tree_bootstrap {
            Tree::load_for_mod_names(
                context.clone(),
                &normalized_repo_url,
                &quick_update_mod_names,
            )
            .await
        } else {
            Tree::load(context.clone(), &normalized_repo_url).await
        }
    } else {
        Ok(Tree::default())
    };
    if !repo_already_complete && let Ok(mut tree) = bootstrap_tree_result {
        if local_path_mismatch_guard_applies(mode) {
            let repo_label = tree
                .repositories
                .first()
                .map(|repo| repo.name.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&normalized_repo_url);
            let availability = summarize_local_path_availability(&tree);
            log_local_path_availability(repo_label, &availability);
            if suspect_local_path_mismatch(&availability) {
                let message = format_local_path_mismatch_message(repo_label, &availability);
                error!("{message}");
                summary.push(
                    StageEntry::new("local_path_preflight", stage.elapsed())
                        .with("root_exists", availability.root_exists)
                        .with("expected_addons", availability.expected_addons)
                        .with("existing_addons", availability.existing_addons)
                        .with("missing_addon_dirs", availability.missing_addon_dirs)
                        .with("expected_files", availability.expected_files)
                        .with("existing_files", availability.existing_files)
                        .with("missing_files", availability.missing_files),
                );
                summary.log_table("failed-local-path-mismatch");
                emit_progress!(ProgressEvent::Failed(message));
                return;
            }
        }

        if force_redownload {
            // a#7 Step 1 / §3: skip the local tree-hash baseline init on a
            // force-redownload. Every file re-downloads unconditionally and
            // `force_redownload_hash_invalidate` clears all local checksums, so any
            // baseline we hash here is discarded and recomputed during the download.
            // On a missing baseline (after a schema wipe or first sync over a
            // pre-populated dir) this branch otherwise hashes all on-disk files
            // (~13.5s on TFR_40K) purely to throw the result away.
            info!(
                "Skipping tree hash bootstrap for force-redownload repo={} (baseline discarded; rehashed during download)",
                normalized_repo_url
            );
            summary.push(
                StageEntry::new("tree_hash_bootstrap", stage.elapsed())
                    .with("type", "skipped")
                    .with("reason", "force_redownload"),
            );
            stage = std::time::Instant::now();
        } else if tree_local_checksums_baseline_missing(&tree) {
            info!(
                "Local tree hash baseline is empty for repo {}, running full tree hash initialization",
                normalized_repo_url
            );
            emit_progress!(ProgressEvent::Stage {
                label: "Initializing local tree hashes".into(),
                percent: 0.30,
            });
            let bootstrap_tree = if scoped_tree_bootstrap {
                let file_ids = tree.files.iter().map(|file| file.id).collect();
                let _ = calculate_hashes_for_files_with_profile(
                    context.clone(),
                    &normalized_repo_url,
                    &file_ids,
                    Some(&progress_tx),
                    false,
                    hash_io_profile,
                )
                .await;
                if *cancel_rx.borrow() {
                    info!(
                        "Sync cancelled during scoped full tree hash bootstrap for repo={}",
                        normalized_repo_url
                    );
                    summary.push(
                        StageEntry::new("tree_hash_bootstrap", stage.elapsed())
                            .with("type", "scoped")
                            .with("cancelled", true),
                    );
                    summary.log_table("cancelled");
                    emit_progress!(ProgressEvent::Cancelled);
                    return;
                }
                Tree::load_for_mod_names(
                    context.clone(),
                    &normalized_repo_url,
                    &quick_update_mod_names,
                )
                .await
                .ok()
            } else {
                match calculate_hashes_with_tree_and_profile_cancellable(
                    context.clone(),
                    &normalized_repo_url,
                    Some(tree),
                    Some(&progress_tx),
                    hash_io_profile,
                    Some(&cancel_rx),
                )
                .await
                {
                    HashCalculationResult::Completed(tree) => Some(*tree),
                    HashCalculationResult::Cancelled => {
                        info!(
                            "Sync cancelled during full tree hash bootstrap for repo={}",
                            normalized_repo_url
                        );
                        summary.push(
                            StageEntry::new("tree_hash_bootstrap", stage.elapsed())
                                .with("type", "full")
                                .with("cancelled", true),
                        );
                        summary.log_table("cancelled");
                        emit_progress!(ProgressEvent::Cancelled);
                        return;
                    }
                    HashCalculationResult::Failed => None,
                }
            };
            full_tree_hash_bootstrap = true;
            summary
                .push(StageEntry::new("tree_hash_bootstrap", stage.elapsed()).with("type", "full"));
            stage = std::time::Instant::now();
            // Reuse the computed tree for content-hash refresh to avoid a redundant Tree::load
            bootstrap_tree_for_content_hash = bootstrap_tree;
        } else if tree_local_checksums_missing(&tree) {
            let missing_file_ids = collect_files_with_missing_local_tree_hashes(&tree);
            if missing_file_ids.is_empty() {
                info!(
                    "Local tree hashes are partially missing for repo {}, but no file scope was resolved for targeted bootstrap",
                    normalized_repo_url
                );
            } else {
                info!(
                    "Local tree hashes are partially missing for repo {}, running targeted tree hash initialization for {} files",
                    normalized_repo_url,
                    missing_file_ids.len()
                );
                emit_progress!(ProgressEvent::Stage {
                    label: format!(
                        "Initializing missing tree hashes ({})",
                        missing_file_ids.len()
                    ),
                    percent: 0.30,
                });
                if scoped_tree_bootstrap {
                    let _ = calculate_hashes_for_files_with_profile(
                        context.clone(),
                        &normalized_repo_url,
                        &missing_file_ids,
                        Some(&progress_tx),
                        false,
                        hash_io_profile,
                    )
                    .await;
                    if *cancel_rx.borrow() {
                        info!(
                            "Sync cancelled during scoped targeted tree hash bootstrap for repo={}",
                            normalized_repo_url
                        );
                        summary.push(
                            StageEntry::new("tree_hash_bootstrap", stage.elapsed())
                                .with("type", "targeted")
                                .with("cancelled", true),
                        );
                        summary.log_table("cancelled");
                        emit_progress!(ProgressEvent::Cancelled);
                        return;
                    }
                    if let Ok(refreshed_tree) = Tree::load_for_mod_names(
                        context.clone(),
                        &normalized_repo_url,
                        &quick_update_mod_names,
                    )
                    .await
                    {
                        tree = refreshed_tree;
                    }
                } else {
                    let _ = calculate_hashes_for_files_in_tree_with_profile(
                        context.clone(),
                        &mut tree,
                        &missing_file_ids,
                        Some(&progress_tx),
                        false,
                        hash_io_profile,
                    )
                    .await;
                    if *cancel_rx.borrow() {
                        info!(
                            "Sync cancelled during targeted tree hash bootstrap for repo={}",
                            normalized_repo_url
                        );
                        summary.push(
                            StageEntry::new("tree_hash_bootstrap", stage.elapsed())
                                .with("type", "targeted")
                                .with("cancelled", true),
                        );
                        summary.log_table("cancelled");
                        emit_progress!(ProgressEvent::Cancelled);
                        return;
                    }
                }
                targeted_tree_hash_init = true;
                summary.push(
                    StageEntry::new("tree_hash_bootstrap", stage.elapsed())
                        .with("type", "targeted")
                        .with("files", missing_file_ids.len()),
                );
                stage = std::time::Instant::now();
                bootstrap_tree_for_content_hash = Some(tree);
            }
        }
    }

    // Refresh content-hash baseline after any tree hash initialization so the
    // quick scan bootstrap finds content hashes already present and skips both
    // tree AND content hash re-computation.
    if targeted_tree_hash_init {
        if scoped_tree_bootstrap {
            if let Some(tree) = bootstrap_tree_for_content_hash.take() {
                let _ = refresh_content_hashes_for_scoped_tree(
                    context.clone(),
                    &normalized_repo_url,
                    &tree,
                )
                .await;
            }
        } else {
            let _ = refresh_content_hashes_when_tree_matches(
                context.clone(),
                &normalized_repo_url,
                bootstrap_tree_for_content_hash.take(),
            )
            .await;
        }
    }

    if full_tree_hash_bootstrap {
        if scoped_tree_bootstrap {
            if let Some(tree) = bootstrap_tree_for_content_hash.take() {
                let _ = refresh_content_hashes_for_scoped_tree(
                    context.clone(),
                    &normalized_repo_url,
                    &tree,
                )
                .await;
            }
        } else {
            let _ = refresh_content_hashes_when_tree_matches(
                context.clone(),
                &normalized_repo_url,
                bootstrap_tree_for_content_hash.take(),
            )
            .await;
        }
    }

    if !builds_download_plan
        && (mode == SyncMode::RecheckOnly || mode == SyncMode::RemoteRefreshOnly)
        && full_tree_hash_bootstrap
    {
        info!(
            "Full tree hash bootstrap completed for repo={}; running post-bootstrap quick verify to build delta-aware pending updates",
            normalized_repo_url
        );
        let quick_post_bootstrap_started = Instant::now();
        if let Some(scope) = cached_pending_scope.as_ref() {
            info!(
                "Scoping post-bootstrap quick verify to cached pending addons for repo={} addons={}",
                normalized_repo_url,
                scope.len()
            );
        }
        let mut mods = quick_local_change_diff(
            context.clone(),
            &normalized_repo_url,
            cached_pending_scope.as_ref(),
            Some(&mod_enabled_overrides),
            Some(&progress_tx),
            false,
            true, // already_eligible: tree hashes + content baseline were just initialized
            false,
            None,
        )
        .await;
        let mut has_bootstrap_updates = mods.iter().any(|m| m.needs_update);
        if should_refresh_delta_plan_after_quick_verify(
            builds_download_plan,
            has_bootstrap_updates,
            delta_plan_estimate_refreshed,
        ) {
            emit_progress!(ProgressEvent::Stage {
                label: "Preparing delta estimate".into(),
                percent: 0.82,
            });
            let pending_mod_names: HashSet<String> = mods
                .iter()
                .filter(|m| m.needs_update)
                .map(|m| m.name.to_lowercase())
                .collect();
            let estimate_started = Instant::now();
            let refreshed_files = refresh_patch_plan_metadata_for_pending_updates(
                context.clone(),
                &normalized_repo_url,
                Some(&pending_mod_names),
            )
            .await;
            delta_plan_estimate_refreshed = refreshed_files > 0;
            summary.push(
                StageEntry::new("delta_plan_refresh", estimate_started.elapsed())
                    .with("files_considered", refreshed_files)
                    .with("mods", pending_mod_names.len()),
            );
        }
        if delta_plan_estimate_refreshed
            && let Some(adjusted_mods) = apply_patch_plan_estimates_to_pending_updates(
                context.clone(),
                &normalized_repo_url,
                &mods,
            )
            .await
        {
            mods = adjusted_mods;
        }
        has_bootstrap_updates = mods.iter().any(|m| m.needs_update);
        emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
        persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
        info!(
            "Post-bootstrap quick verify finished in {:.2?} (updates={})",
            quick_post_bootstrap_started.elapsed(),
            has_bootstrap_updates
        );
        if !has_bootstrap_updates
            && let Ok(rows_affected) =
                reconcile_target_repository_local_checksum(context.clone(), &normalized_repo_url)
                    .await
            && rows_affected > 0
        {
            info!(
                "Reconciled stale repo-level checksum for {} after bootstrap",
                normalized_repo_url
            );
        }
        let stage_name = if mode == SyncMode::RemoteRefreshOnly {
            "remote_refresh_only_total"
        } else {
            "recheck_only_total"
        };
        summary.push(StageEntry::new(stage_name, overall_start.elapsed()));
        summary.log_table("early-exit-bootstrap");
        emit_progress!(ProgressEvent::Finished);
        return;
    }

    // After remote metadata refresh creates/updates DB entries, run a local quick verification
    // against disk so recheck reflects actual local state and avoids false redownload prompts.
    let quick_post_remote_start = std::time::Instant::now();
    // Reusing the prepared queue keeps the prepared `mods` payload; re-running the
    // quick verify here would re-hash and overwrite it for no benefit.
    if !reuse_prepared_queue {
        if force_redownload {
            // a#7 Step 3b: on a force-redownload the verdict is trivially "everything
            // updates" and the actual queue is driven by `download_target_file` (built
            // in remote_repository), not by this diff. The `mods` payload here is only
            // cosmetic (UI diff + pending_updates) and is overwritten from the hashed
            // tree post-download. So skip the local-disk hashing + `subfiles` tree read
            // (the last pre-download `subfiles` reader, ~5.8s of wasted work) and
            // synthesize the payload directly from the freshly persisted download
            // targets. This also removes the only pre-download dependency on persisted
            // parts, which is what lets the part insert be deferred (A++).
            match fetch_all_download_targets_with_mod_and_name(context.clone()).await {
                Ok(targets) => {
                    mods = build_download_estimate_diffs(&targets);
                }
                Err(err) => {
                    warn!(
                        "Force-redownload diff synthesis could not load download targets for repo={}: {}",
                        normalized_repo_url, err
                    );
                }
            }
        } else {
            let quick_verify_mod_filter = if builds_download_plan
                && !quick_update_mod_names.is_empty()
            {
                Some(&quick_update_mod_names)
            } else if !builds_download_plan {
                if let Some(scope) = cached_pending_scope.as_ref() {
                    info!(
                        "Scoping post-remote quick verify to cached pending addons for repo={} addons={}",
                        normalized_repo_url,
                        scope.len()
                    );
                }
                cached_pending_scope.as_ref()
            } else {
                None
            };
            mods = quick_local_change_diff(
                context.clone(),
                &normalized_repo_url,
                quick_verify_mod_filter,
                Some(&mod_enabled_overrides),
                Some(&progress_tx),
                !builds_download_plan,
                targeted_tree_hash_init || repo_already_complete, // skip bootstrap if tree is initialized or repo is already complete
                false,
                None,
            )
            .await;
        }
    }
    let has_pending_updates = mods.iter().any(|m| m.needs_update);
    if should_refresh_delta_plan_after_quick_verify(
        builds_download_plan,
        has_pending_updates,
        delta_plan_estimate_refreshed,
    ) {
        emit_progress!(ProgressEvent::Stage {
            label: "Preparing delta estimate".into(),
            percent: 0.82,
        });
        let pending_mod_names: HashSet<String> = mods
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.name.to_lowercase())
            .collect();
        let estimate_started = Instant::now();
        let refreshed_files = refresh_patch_plan_metadata_for_pending_updates(
            context.clone(),
            &normalized_repo_url,
            Some(&pending_mod_names),
        )
        .await;
        delta_plan_estimate_refreshed = refreshed_files > 0;
        summary.push(
            StageEntry::new("delta_plan_refresh", estimate_started.elapsed())
                .with("files_considered", refreshed_files)
                .with("mods", pending_mod_names.len()),
        );
    }
    if delta_plan_estimate_refreshed
        && let Some(adjusted_mods) = apply_patch_plan_estimates_to_pending_updates(
            context.clone(),
            &normalized_repo_url,
            &mods,
        )
        .await
    {
        mods = adjusted_mods;
    }
    emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
    persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
    let has_pending_updates = mods.iter().any(|m| m.needs_update);
    let post_remote_elapsed = quick_post_remote_start.elapsed();
    info!(
        "Quick local check finished in {}",
        format_compact_elapsed_duration(post_remote_elapsed)
    );
    info!(
        "Post-remote quick verify finished in {:.2?} (updates={})",
        post_remote_elapsed, has_pending_updates
    );

    // Reconcile stale repo-level checksum: when the quick scan confirms all addons
    // are up-to-date, ensure the repository's local_checksum equals remote_checksum.
    // This can drift when hash recomputation updates file/mod checksums but does not
    // roll up to the repository level, causing the fast-path early exit to miss on
    // every subsequent startup.
    if !has_pending_updates {
        match reconcile_target_repository_local_checksum(context.clone(), &normalized_repo_url)
            .await
        {
            Ok(rows_affected) if rows_affected > 0 => {
                info!(
                    "Reconciled stale repo-level checksum for {} (set local = remote)",
                    normalized_repo_url
                );
            }
            Err(err) => {
                warn!(
                    "Failed to reconcile repo-level checksum for {}: {}",
                    normalized_repo_url, err
                );
            }
            _ => {}
        }
    }

    summary.push(
        StageEntry::new("post_remote_quick_verify", stage.elapsed())
            .with("mods_checked", mods.len())
            .with(
                "mods_with_updates",
                mods.iter().filter(|m| m.needs_update).count(),
            ),
    );
    stage = std::time::Instant::now();

    if builds_download_plan {
        let mut pending_mod_names: HashSet<String> = mods
            .iter()
            .filter(|m| m.needs_update)
            .map(|m| m.name.to_lowercase())
            .collect();
        // Unexpected-file cleanup already ran during the confirmation preflight;
        // skip it when reusing that prepared queue.
        if !reuse_prepared_queue && !pending_mod_names.is_empty() {
            let unexpected_by_mod = collect_unexpected_files_for_repo_mods(
                context.clone(),
                &normalized_repo_url,
                &pending_mod_names,
                Some(&mod_enabled_overrides),
            )
            .await;
            let unexpected_total: usize = unexpected_by_mod.values().map(Vec::len).sum();
            if unexpected_total > 0 {
                info!(
                    "Detected {} unexpected local files across {} addons for repo={}",
                    unexpected_total,
                    unexpected_by_mod.len(),
                    normalized_repo_url
                );
                emit_progress!(ProgressEvent::Stage {
                    label: "Cleaning unexpected local files".into(),
                    percent: 0.82,
                });
                let (deleted_unexpected, failed_unexpected) =
                    delete_unexpected_local_files(&unexpected_by_mod).await;
                info!(
                    "Unexpected local file cleanup finished for repo={} (deleted={} failed={})",
                    normalized_repo_url, deleted_unexpected, failed_unexpected
                );
                summary.push(
                    StageEntry::new("unexpected_file_cleanup", stage.elapsed())
                        .with("deleted", deleted_unexpected)
                        .with("failed", failed_unexpected),
                );
                stage = std::time::Instant::now();
                if deleted_unexpected > 0 {
                    mods = quick_local_change_diff(
                        context.clone(),
                        &normalized_repo_url,
                        Some(&pending_mod_names),
                        Some(&mod_enabled_overrides),
                        Some(&progress_tx),
                        false,
                        true, // already_eligible: tree state is established from earlier phases
                        false,
                        None,
                    )
                    .await;
                    emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
                    persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
                    pending_mod_names = mods
                        .iter()
                        .filter(|m| m.needs_update)
                        .map(|m| m.name.to_lowercase())
                        .collect();
                }
            }
        }

        // Path/layout guards and the patch-plan queue rebuild already ran during
        // the confirmation preflight; reusing its queue skips them. A force
        // redownload must still rebuild the queue even when quick verify found no
        // pending updates, so only the guards stay scoped to non-empty pending
        // mods.
        if !reuse_prepared_queue && (force_redownload || !pending_mod_names.is_empty()) {
            if !pending_mod_names.is_empty()
                && let Some(missing_summary) = collect_missing_addon_path_summary(
                    context.clone(),
                    &normalized_repo_url,
                    &mod_enabled_overrides,
                )
                .await
            {
                if missing_summary.missing_addons > 0 {
                    let samples = missing_summary
                        .sample_paths
                        .iter()
                        .filter(|path| !path.is_empty())
                        .map(|path| sanitize_log_path_str(path))
                        .collect::<Vec<_>>()
                        .join("; ");
                    info!(
                        "Pre-download addon path check: repo={} root={} enabled_addons={} missing_or_not_dir={} samples=[{}]",
                        normalized_repo_url,
                        sanitize_log_path_str(&local_path),
                        missing_summary.enabled_addons,
                        missing_summary.missing_addons,
                        samples
                    );
                    // In-depth on-disk layout snapshot before a download that
                    // would (re)fetch whole addons, so the actual paths/contents
                    // are captured whether the redownload proceeds or is blocked
                    // by the suspect guard below.
                    log_addon_path_disk_state(
                        &normalized_repo_url,
                        &local_path,
                        &missing_summary.sample_paths,
                    );
                }

                if suspect_full_redownload_guard_applies(
                    &missing_summary,
                    recent_local_path_reset,
                    repo_already_complete,
                    allow_suspect_full_redownload,
                ) {
                    let message =
                        format_suspect_full_redownload_message(&local_path, &missing_summary);
                    error!("{}", message);
                    summary.push(
                        StageEntry::new("suspect_full_redownload_guard", stage.elapsed())
                            .with("enabled_addons", missing_summary.enabled_addons)
                            .with("missing_addons", missing_summary.missing_addons)
                            .with("recent_local_path_reset", recent_local_path_reset)
                            .with("repo_already_complete", repo_already_complete),
                    );
                    summary.log_table("failed-suspect-full-redownload");
                    emit_progress!(ProgressEvent::Failed(message));
                    return;
                }

                // Layout/path-mismatch guard: every enabled addon folder is
                // present (none counted missing above), but if their expected
                // files do not resolve while real content sits on disk, then
                // "downloading the missing files" would be a near-full
                // redownload of content the user already has. Probe only when no
                // addon dir is missing, so a genuine fresh download into empty
                // folders is never penalised, and let an explicit force bypass
                // it.
                if !allow_suspect_full_redownload
                    && missing_summary.missing_addons == 0
                    && let Ok(tree) = Tree::load_for_mod_names(
                        context.clone(),
                        &normalized_repo_url,
                        &pending_mod_names,
                    )
                    .await
                {
                    let availability = summarize_local_path_availability(&tree);
                    if availability.layout_mismatch_suspected() {
                        let repo_label = tree
                            .repositories
                            .first()
                            .map(|repo| repo.name.as_str())
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or(&normalized_repo_url);
                        log_local_path_availability(repo_label, &availability);
                        let message = format_local_path_mismatch_message(repo_label, &availability);
                        error!("{message}");
                        summary.push(
                            StageEntry::new("layout_mismatch_guard", stage.elapsed())
                                .with("expected_files", availability.expected_files)
                                .with("existing_files", availability.existing_files)
                                .with(
                                    "addons_with_disk_content_unresolved",
                                    availability.addons_with_disk_content_unresolved,
                                ),
                        );
                        summary.log_table("failed-layout-mismatch");
                        emit_progress!(ProgressEvent::Failed(message));
                        return;
                    }
                }
            }

            let existing_targets = if force_redownload {
                HashSet::new()
            } else {
                collect_repo_download_targets(
                    context.clone(),
                    &normalized_repo_url,
                    Some(&pending_mod_names),
                )
                .await
                .0
            };
            let rebuilt_files = if force_redownload {
                // a#6 Step 3 / P2: on a force-redownload the local files are deleted, so
                // there are no patch sources and patch planning is a no-op (every measured
                // run shows planned_patches=0). The part metadata and the
                // `download_target_file` rows were already (re)built during
                // `remote_repository`; `refresh_patch_plan_metadata_for_pending_updates`
                // here only re-reads all ~66k `subfiles` rows and re-runs
                // `remote_file_parts_batch` over them to produce identical data - pure
                // redundant work on the critical path (~5s). Skip it. The download queue is
                // rebuilt independently below from the existing download targets via
                // `collect_repo_download_targets`.
                0
            } else if existing_download_targets_are_tiny(context.clone(), &existing_targets).await {
                info!(
                    "Skipping patch-plan metadata refresh for repo={} because existing scoped download targets are tiny files (files={})",
                    normalized_repo_url,
                    existing_targets.len()
                );
                0
            } else {
                refresh_patch_plan_metadata_for_pending_updates(
                    context.clone(),
                    &normalized_repo_url,
                    Some(&pending_mod_names),
                )
                .await
            };
            summary.push(
                StageEntry::new("existing_graph_queue_rebuild", stage.elapsed())
                    .with("files_considered", rebuilt_files)
                    .with("mods", pending_mod_names.len()),
            );
            stage = std::time::Instant::now();
        }

        let (file_ids, mod_ids) = collect_repo_download_targets(
            context.clone(),
            &normalized_repo_url,
            if force_redownload {
                None
            } else {
                Some(&pending_mod_names)
            },
        )
        .await;
        download_file_ids = file_ids;
        download_mod_ids = mod_ids;
        summary.push(
            StageEntry::new("download_queue_build", stage.elapsed())
                .with("files", download_file_ids.len())
                .with("mods", download_mod_ids.len()),
        );
        if force_redownload && !download_file_ids.is_empty() {
            match invalidate_force_redownload_hash_baseline(context.clone(), &download_file_ids)
                .await
            {
                Ok(rows) => {
                    info!(
                        "Force redownload hash baseline invalidated: repo={} files={} rows={}",
                        normalized_repo_url,
                        download_file_ids.len(),
                        rows
                    );
                    summary.push(
                        StageEntry::new("force_redownload_hash_invalidate", stage.elapsed())
                            .with("files", download_file_ids.len())
                            .with("rows", rows),
                    );
                    stage = std::time::Instant::now();
                }
                Err(err) => {
                    let message = format!(
                        "Failed to invalidate hash baseline before force redownload for repo {}: {}",
                        normalized_repo_url, err
                    );
                    error!("{}", message);
                    summary.log_table("failed-force-redownload-hash-invalidate");
                    emit_progress!(ProgressEvent::Failed(message));
                    return;
                }
            }
        }

        if !force_redownload && !download_file_ids.is_empty() {
            if let Some(adjusted_mods) = apply_download_target_estimates_to_pending_updates(
                context.clone(),
                &normalized_repo_url,
                Some(&pending_mod_names),
            )
            .await
            {
                mods = adjusted_mods;
                emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
                persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
            } else if let Some(adjusted_mods) = apply_patch_plan_estimates_to_pending_updates(
                context.clone(),
                &normalized_repo_url,
                &mods,
            )
            .await
            {
                mods = adjusted_mods;
                emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
                persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
            }
        }

        if prepare_download_plan && !download_file_ids.is_empty() {
            match fetch_all_download_targets_with_mod_and_name(context.clone()).await {
                Ok(mut targets) => {
                    targets.retain(|target| download_file_ids.contains(&target.download.file_id));
                    let (patchable_file_ids, planned_bytes, full_bytes) =
                        apply_download_plan_bytes(context.clone(), &mut targets).await;
                    mods = build_download_estimate_diffs(&targets);
                    info!(
                        "Final confirmation plan prepared: repo={} mods={} files={} patch_files={} planned_transfer_bytes={} full_bytes={}",
                        normalized_repo_url,
                        mods.len(),
                        targets.len(),
                        patchable_file_ids.len(),
                        planned_bytes,
                        full_bytes
                    );
                    emit_progress!(ProgressEvent::Diff { mods: mods.clone() });
                    persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
                    summary.push(
                        StageEntry::new("confirmation_download_plan", stage.elapsed())
                            .with("mods", mods.len())
                            .with("files", targets.len())
                            .with("planned_bytes", planned_bytes),
                    );
                }
                Err(err) => {
                    let message = format!(
                        "Failed to load the final download plan for repo {}: {}",
                        normalized_repo_url, err
                    );
                    error!("{}", message);
                    summary.log_table("failed-confirmation-plan");
                    emit_progress!(ProgressEvent::Failed(message));
                    return;
                }
            }
        }

        // Queue may still contain stale targets when quick verification proves files are already valid.
        if !force_redownload && !mods.iter().any(|m| m.needs_update) {
            if let Err(err) = truncate_all_download_tables(context.clone()).await {
                warn!(
                    "Failed to clear download queue after clean post-remote quick verify: {}",
                    err
                );
            }
            summary.push(StageEntry::new(
                "download_skip_after_quick_verify",
                overall_start.elapsed(),
            ));
            summary.log_table("early-exit-clean");
            emit_progress!(ProgressEvent::Finished);
            return;
        }

        if download_file_ids.is_empty() {
            let pending_mods = mods.iter().filter(|m| m.needs_update).count();
            let message = format!(
                "Unable to build a download queue for repo {} despite {} pending addon updates (possible undeletable unexpected local files)",
                normalized_repo_url, pending_mods
            );
            error!("{}", message);
            summary.log_table("failed-empty-queue");
            emit_progress!(ProgressEvent::Failed(message));
            return;
        }

        if prepare_download_plan {
            summary.push(StageEntry::new(
                "prepare_download_total",
                overall_start.elapsed(),
            ));
            summary.log_table("prepared-download-confirmation");
            emit_progress!(ProgressEvent::Finished);
            return;
        }

        if let Some(backup_root) = auto_backup_directory.as_deref()
            && let Err(err) = backup_pending_addons_for_download(
                backup_root,
                &local_path,
                &mods,
                &progress_tx,
                &operation_id,
            )
        {
            let message = format!("Automatic addon backup failed: {}", err);
            error!("{}", message);
            summary.log_table("failed-backup");
            emit_progress!(ProgressEvent::Failed(message));
            return;
        }
    }

    if mode == SyncMode::RecheckOnly || mode == SyncMode::RemoteRefreshOnly {
        let stage_name = if mode == SyncMode::RemoteRefreshOnly {
            "remote_refresh_only_total"
        } else {
            "recheck_only_total"
        };
        summary.push(StageEntry::new(stage_name, overall_start.elapsed()));
        summary.log_table("completed");
        emit_progress!(ProgressEvent::Finished);
        return;
    }

    if !wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx).await {
        info!(
            "Sync cancelled before download phase for repo={}",
            normalized_repo_url
        );
        emit_progress!(ProgressEvent::Cancelled);
        return;
    }

    if *cancel_rx.borrow() {
        info!(
            "Sync cancelled before download phase for repo={}",
            normalized_repo_url
        );
        emit_progress!(ProgressEvent::Cancelled);
        return;
    }

    // Download files
    let download_start = std::time::Instant::now();
    let mut hashed_download_file_ids: HashSet<u64> = HashSet::new();
    let mut incremental_hash_duration = Duration::ZERO;
    let mut incremental_hash_tree_context: Option<RepositoryHashContext> = None;
    let mut sticky_auto_hash_profile: Option<HashIoProfilePreference> = None;
    let mut addon_hash_metrics: Vec<AddonHashMetrics> = Vec::new();
    let mut hash_phase_timings = HashPhaseTimings::default();
    let mut hash_tree_loads = 0usize;
    let rollback_root = rollback_temp_directory
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(app_paths::foxy_data_dir);
    if let Err(err) =
        UpdateRollbackSession::cleanup_stale_sessions(&rollback_root, Some(&progress_tx)).await
    {
        warn!(
            "Rollback stale-session cleanup failed for repo={}: {}",
            normalized_repo_url, err
        );
    }
    let rollback_session =
        match UpdateRollbackSession::new(&rollback_root, &normalized_repo_url).await {
            Ok(session) => Some(Arc::new(Mutex::new(session))),
            Err(err) => {
                let message = format!("Failed to prepare update rollback: {}", err);
                error!("{}", message);
                summary.log_table("failed-rollback-prepare");
                emit_progress!(ProgressEvent::Failed(message));
                return;
            }
        };
    let mut cancel_watch = cancel_rx.clone();
    let allowed_ids_for_download = if download_file_ids.is_empty() {
        None
    } else {
        Some(download_file_ids.clone())
    };
    let estimated_download_bytes =
        estimate_download_queue_bytes(context.clone(), &download_file_ids).await;
    let mut auto_rebenchmark_schedule = AutoRebenchmarkSchedule::new(estimated_download_bytes);
    if hash_io_profile == HashIoProfilePreference::Auto
        && let Some(first_milestone) = auto_rebenchmark_schedule
            .as_ref()
            .and_then(AutoRebenchmarkSchedule::next_milestone)
    {
        info!(
            "Auto hash profile re-benchmarks scheduled every {}% of download bytes: first_percent={} first_threshold_bytes={} estimated_total_bytes={}",
            AUTO_REBENCHMARK_DOWNLOAD_PERCENT_STEP,
            first_milestone.percent,
            first_milestone.threshold_bytes,
            estimated_download_bytes
        );
    }
    // A++ (after_turso_regression_analysis7.md): on a force-redownload the part
    // insert was deferred (buffered by the per-mod batch), so kick it off now as one
    // background transaction overlapped with the download. The incremental hash worker
    // awaits this handle before its first tree load, so the rows are guaranteed present
    // by the time any reader needs them. No-op when nothing was deferred.
    let deferred_part_insert_handle: Option<tokio::task::JoinHandle<bool>> = if force_redownload
        && context.should_defer_part_inserts()
    {
        let flush_context = context.clone();
        let handle = tokio::spawn(async move { flush_deferred_part_inserts(flush_context).await });
        // Close the defer window now that the buffer has been handed to the flush
        // task; any later writes in this session use the normal inline path.
        context.set_defer_part_inserts(false);
        Some(handle)
    } else {
        None
    };
    // The worker blocks on the deferred insert (~22s) before its first tree load, so
    // the channel must hold the completions that arrive during that window without
    // spilling them to the (post-download) final hash stage. 2048 comfortably covers
    // the ~10% of files that finish in the first ~22s of a multi-minute download.
    let mod_completion_channel_bound = if deferred_part_insert_handle.is_some() {
        2048
    } else {
        128
    };
    let (mod_completion_tx, mut mod_completion_rx) =
        mpsc::channel::<DownloadModCompletion>(mod_completion_channel_bound);
    let incremental_hash_context = context.clone();
    let incremental_hash_repo_url = normalized_repo_url.clone();
    let incremental_hash_progress = progress_tx.clone();
    // Shared origin for every telemetry lane on the speed graph. The download
    // worker sets it once real transfer begins (after pre-download validation);
    // the hash lanes read it so they sit on the same timeline as the download
    // lane instead of trailing it by the remote-refresh/prep gap. Falls back to
    // `overall_start` only if a hash sample somehow precedes the download start.
    let telemetry_epoch: Arc<std::sync::OnceLock<std::time::Instant>> =
        Arc::new(std::sync::OnceLock::new());
    let incremental_hash_started_at = overall_start;
    let hash_telemetry_epoch = telemetry_epoch.clone();
    let incremental_hash_worker = tokio::spawn(async move {
        // A++: ensure the deferred part insert has completed before the first tree
        // load (the only `subfiles` reader on the force path). Completions that arrive
        // during this wait buffer in the widened channel rather than spilling.
        if let Some(handle) = deferred_part_insert_handle {
            match handle.await {
                Ok(true) => {}
                Ok(false) => {
                    warn!("Deferred part insert task failed before incremental hashing");
                }
                Err(err) => {
                    warn!("Deferred part insert task failed before incremental hashing: {err}");
                }
            }
        }
        let mut hashed_file_ids: HashSet<u64> = HashSet::new();
        let mut pending_file_ids: HashSet<u64> = HashSet::new();
        let mut pending_bytes = 0u64;
        let mut completed_download_bytes = 0u64;
        let mut hash_duration = Duration::ZERO;
        let mut hash_context: Option<RepositoryHashContext> = None;
        let mut tree_loads = 0usize;
        let mut sticky_auto_profile: Option<HashIoProfilePreference> = None;
        let mut addon_hash_metrics: Vec<AddonHashMetrics> = Vec::new();
        let mut hash_phase_timings = HashPhaseTimings::default();
        let mut auto_rebenchmark_pending: Option<AutoRebenchmarkMilestone> = None;
        while let Some(completion) = mod_completion_rx.recv().await {
            if !completion.success || completion.file_ids.is_empty() {
                continue;
            }
            completed_download_bytes = completed_download_bytes.saturating_add(completion.bytes);
            let crossed_milestone = auto_rebenchmark_schedule.as_mut().and_then(|schedule| {
                schedule.next_milestone().and_then(|milestone| {
                    should_rebenchmark_auto_profile(
                        hash_io_profile,
                        auto_rebenchmark_pending.is_some(),
                        completed_download_bytes,
                        milestone.threshold_bytes,
                    )
                    .then(|| {
                        schedule
                            .take_crossed_milestone(completed_download_bytes)
                            .unwrap_or(milestone)
                    })
                })
            });
            if let Some(milestone) = crossed_milestone {
                if let Some(previous_profile) = sticky_auto_profile.take() {
                    info!(
                        "Auto hash profile re-benchmark milestone reached: percent={} completed_bytes={} threshold_bytes={} previous_profile={} next_eligible_batch_will_rebenchmark=true",
                        milestone.percent,
                        completed_download_bytes,
                        milestone.threshold_bytes,
                        previous_profile
                    );
                } else {
                    auto_rebenchmark_pending = Some(milestone);
                    info!(
                        "Auto hash profile re-benchmark milestone reached before a sticky profile was selected: percent={} completed_bytes={} threshold_bytes={} rebenchmark_pending=true",
                        milestone.percent, completed_download_bytes, milestone.threshold_bytes
                    );
                }
            }
            let file_ids: HashSet<u64> = completion
                .file_ids
                .iter()
                .filter(|file_id| {
                    !hashed_file_ids.contains(file_id) && !pending_file_ids.contains(file_id)
                })
                .copied()
                .collect();
            if file_ids.is_empty() {
                continue;
            }
            pending_file_ids.extend(file_ids);
            pending_bytes = pending_bytes.saturating_add(completion.bytes);
            let pending_mod_label = completion.mod_name.clone();
            if pending_file_ids.len() < INCREMENTAL_HASH_MIN_FILES
                && pending_bytes < INCREMENTAL_HASH_MIN_BYTES
            {
                continue;
            }

            let file_ids = std::mem::take(&mut pending_file_ids);
            let batch_bytes = pending_bytes;
            pending_bytes = 0;
            info!(
                "Starting incremental hash for completed download batch: repo={} mod_id={} mod={} files={} bytes={}",
                incremental_hash_repo_url,
                completion.mod_id,
                pending_mod_label,
                file_ids.len(),
                batch_bytes
            );
            let _ = incremental_hash_progress.send(ProgressEvent::Stage {
                label: format!("Hashing downloaded files ({})", pending_mod_label),
                percent: 0.86,
            });
            let batch_start = std::time::Instant::now();
            let hash_result = run_incremental_hash_batch(
                incremental_hash_context.clone(),
                &incremental_hash_repo_url,
                &mut hash_context,
                &file_ids,
                &mut hashed_file_ids,
                &mut hash_duration,
                &mut tree_loads,
                Some(&incremental_hash_progress),
                hash_io_profile,
                &mut sticky_auto_profile,
                &mut addon_hash_metrics,
                0.86,
                force_redownload,
            )
            .await;
            hash_phase_timings.merge(&hash_result.phase_timings);
            let batch_elapsed = batch_start.elapsed();
            let hashed_files = hash_result.processed_file_ids.len();
            if hashed_files > 0 && batch_elapsed.as_secs_f64() > 0.0 {
                let _ = incremental_hash_progress.send(ProgressEvent::HashTelemetry {
                    elapsed_ms: hash_telemetry_epoch
                        .get()
                        .copied()
                        .unwrap_or(incremental_hash_started_at)
                        .elapsed()
                        .as_millis() as u64,
                    files_per_sec: hashed_files as f64 / batch_elapsed.as_secs_f64(),
                });
                // Surface the running incremental hash total so the in-progress
                // download summary shows a live "Cumulative hash" value instead of
                // 0ms until the final summary lands. After-download hashing has not
                // started yet, so its share is still zero here.
                let _ = incremental_hash_progress.send(ProgressEvent::HashSummary {
                    cumulative_hash_ms: hash_duration.as_millis() as u64,
                    after_download_hash_ms: 0,
                });
            }
            if let Some(milestone) = auto_rebenchmark_pending
                && let Some(previous_profile) = sticky_auto_profile.take()
            {
                auto_rebenchmark_pending = None;
                info!(
                    "Auto hash profile re-benchmark armed after initial sticky selection: percent={} completed_bytes={} threshold_bytes={} previous_profile={} next_eligible_batch_will_rebenchmark=true",
                    milestone.percent,
                    completed_download_bytes,
                    milestone.threshold_bytes,
                    previous_profile
                );
            }
        }
        if !pending_file_ids.is_empty() {
            let file_ids = std::mem::take(&mut pending_file_ids);
            info!(
                "Flushing final incremental hash batch after download: repo={} files={} bytes={}",
                incremental_hash_repo_url,
                file_ids.len(),
                pending_bytes
            );
            let batch_start = std::time::Instant::now();
            let hash_result = run_incremental_hash_batch(
                incremental_hash_context.clone(),
                &incremental_hash_repo_url,
                &mut hash_context,
                &file_ids,
                &mut hashed_file_ids,
                &mut hash_duration,
                &mut tree_loads,
                Some(&incremental_hash_progress),
                hash_io_profile,
                &mut sticky_auto_profile,
                &mut addon_hash_metrics,
                0.86,
                force_redownload,
            )
            .await;
            hash_phase_timings.merge(&hash_result.phase_timings);
            let batch_elapsed = batch_start.elapsed();
            let hashed_files = hash_result.processed_file_ids.len();
            if hashed_files > 0 && batch_elapsed.as_secs_f64() > 0.0 {
                let _ = incremental_hash_progress.send(ProgressEvent::HashTelemetry {
                    elapsed_ms: hash_telemetry_epoch
                        .get()
                        .copied()
                        .unwrap_or(incremental_hash_started_at)
                        .elapsed()
                        .as_millis() as u64,
                    files_per_sec: hashed_files as f64 / batch_elapsed.as_secs_f64(),
                });
                let _ = incremental_hash_progress.send(ProgressEvent::HashSummary {
                    cumulative_hash_ms: hash_duration.as_millis() as u64,
                    after_download_hash_ms: 0,
                });
            }
        }
        (
            hashed_file_ids,
            hash_duration,
            hash_context,
            tree_loads,
            sticky_auto_profile,
            addon_hash_metrics,
            hash_phase_timings,
        )
    });
    let mut download_worker = tokio::spawn(download_files(
        context.clone(),
        Some(progress_tx.clone()),
        download_speed_limit_mbps,
        download_pause_rx.clone(),
        cancel_rx.clone(),
        rollback_session.clone(),
        Some(mod_completion_tx),
        allowed_ids_for_download,
        operation_id.clone(),
        telemetry_epoch.clone(),
    ));
    let mut cancelled_during_download = false;
    let download_result = loop {
        tokio::select! {
            _ = cancel_watch.changed(), if !cancelled_during_download => {
                if *cancel_watch.borrow() {
                    info!("Sync cancelled during download phase for repo={}", normalized_repo_url);
                    cancelled_during_download = true;
                    emit_progress!(ProgressEvent::Stage {
                        label: "Cancelling...".into(),
                        percent: 0.84,
                    });
                }
            }
            join_res = &mut download_worker => {
                break match join_res {
                    Ok(result) => result,
                    Err(err) => Err(anyhow::anyhow!("download worker task failed: {}", err)),
                };
            }
        }
    };

    if cancelled_during_download || *cancel_rx.borrow() {
        incremental_hash_worker.abort();
        info!(
            "Sync cancelled after download phase for repo={}",
            normalized_repo_url
        );
        emit_progress!(ProgressEvent::Stage {
            label: "Reverting changes".into(),
            percent: 0.0,
        });
        if let Some(session) = rollback_session.as_ref() {
            let mut rollback = session.lock().await;
            if let Err(err) = rollback.restore_all(Some(progress_tx.clone())).await {
                let message = format!("Failed to revert cancelled update: {}", err);
                error!("{}", message);
                summary.log_table("failed-rollback");
                emit_progress!(ProgressEvent::Failed(message));
                return;
            }
        }
        summary.log_table("cancelled");
        emit_progress!(ProgressEvent::Cancelled);
        return;
    }

    let download_report: DownloadRunReport = match download_result {
        Ok(report) => report,
        Err(err) => {
            incremental_hash_worker.abort();
            let message = format!("Download failed: {}", err);
            error!("{}", message);
            if let Some(session) = rollback_session.as_ref() {
                emit_progress!(ProgressEvent::Stage {
                    label: "Reverting changes".into(),
                    percent: 0.0,
                });
                let mut rollback = session.lock().await;
                if let Err(rollback_err) = rollback.restore_all(Some(progress_tx.clone())).await {
                    warn!(
                        "Rollback after download failure failed for repo={}: {}",
                        normalized_repo_url, rollback_err
                    );
                }
            }
            summary.log_table("failed-download");
            emit_progress!(ProgressEvent::Failed(message));
            return;
        }
    };

    match incremental_hash_worker.await {
        Ok((
            hashed_ids,
            hash_duration,
            hash_context,
            tree_load_count,
            selected_profile,
            collected_addon_hash_metrics,
            collected_hash_phase_timings,
        )) => {
            hashed_download_file_ids = hashed_ids;
            incremental_hash_duration = hash_duration;
            incremental_hash_tree_context = hash_context;
            hash_tree_loads = tree_load_count;
            sticky_auto_hash_profile = selected_profile;
            addon_hash_metrics = collected_addon_hash_metrics;
            hash_phase_timings = collected_hash_phase_timings;
        }
        Err(err) => {
            warn!(
                "Incremental hash worker failed for repo={}: {}. Final hash stage will process remaining files.",
                normalized_repo_url, err
            );
        }
    }
    info!(
        "Incremental hash tree loads during download stage for repo={}: {} hashed_files={} hash_time={:.2}s",
        normalized_repo_url,
        hash_tree_loads,
        hashed_download_file_ids.len(),
        incremental_hash_duration.as_secs_f64()
    );
    info!(
        "Download stage finished in {:.2?}",
        download_start.elapsed()
    );
    emit_progress!(ProgressEvent::Stage {
        label: format!("Download {:.1}s", download_start.elapsed().as_secs_f32()),
        percent: 0.85,
    });

    if !wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx).await {
        info!(
            "Sync cancelled before hash finalization for repo={}",
            normalized_repo_url
        );
        emit_progress!(ProgressEvent::Stage {
            label: "Reverting changes".into(),
            percent: 0.0,
        });
        if let Some(session) = rollback_session.as_ref() {
            let mut rollback = session.lock().await;
            if let Err(err) = rollback.restore_all(Some(progress_tx.clone())).await {
                let message = format!("Failed to revert cancelled update: {}", err);
                error!("{}", message);
                summary.log_table("failed-rollback");
                emit_progress!(ProgressEvent::Failed(message));
                return;
            }
        }
        summary.log_table("cancelled");
        emit_progress!(ProgressEvent::Cancelled);
        return;
    }

    if *cancel_rx.borrow() {
        info!(
            "Sync cancelled before hash finalization for repo={}",
            normalized_repo_url
        );
        emit_progress!(ProgressEvent::Stage {
            label: "Reverting changes".into(),
            percent: 0.0,
        });
        if let Some(session) = rollback_session.as_ref() {
            let mut rollback = session.lock().await;
            if let Err(err) = rollback.restore_all(Some(progress_tx.clone())).await {
                let message = format!("Failed to revert cancelled update: {}", err);
                error!("{}", message);
                summary.log_table("failed-rollback");
                emit_progress!(ProgressEvent::Failed(message));
                return;
            }
        }
        summary.log_table("cancelled");
        emit_progress!(ProgressEvent::Cancelled);
        return;
    }

    // Recalculate hashes
    let hash_start = std::time::Instant::now();
    emit_progress!(ProgressEvent::Stage {
        label: "Hashing...".into(),
        percent: 0.90,
    });
    let remaining_file_ids: HashSet<u64> = download_file_ids
        .difference(&hashed_download_file_ids)
        .copied()
        .collect();
    if remaining_file_ids.is_empty() {
        info!(
            "All downloaded files were already hashed incrementally for repo={}",
            normalized_repo_url
        );
    } else {
        info!(
            "Finalizing hash stage for repo={} with {} files not hashed incrementally",
            normalized_repo_url,
            remaining_file_ids.len()
        );
        let hashed_before = hashed_download_file_ids.len();
        let final_hash_batch_start = std::time::Instant::now();
        let final_hash_result = run_incremental_hash_batch(
            context.clone(),
            &normalized_repo_url,
            &mut incremental_hash_tree_context,
            &remaining_file_ids,
            &mut hashed_download_file_ids,
            &mut incremental_hash_duration,
            &mut hash_tree_loads,
            Some(&progress_tx),
            hash_io_profile,
            &mut sticky_auto_hash_profile,
            &mut addon_hash_metrics,
            0.91,
            force_redownload,
        )
        .await;
        hash_phase_timings.merge(&final_hash_result.phase_timings);
        let final_hash_batch_elapsed = final_hash_batch_start.elapsed();
        let final_hash_files = final_hash_result.processed_file_ids.len();
        if final_hash_files > 0 && final_hash_batch_elapsed.as_secs_f64() > 0.0 {
            emit_progress!(ProgressEvent::HashTelemetry {
                elapsed_ms: telemetry_epoch
                    .get()
                    .copied()
                    .unwrap_or(overall_start)
                    .elapsed()
                    .as_millis() as u64,
                files_per_sec: final_hash_files as f64 / final_hash_batch_elapsed.as_secs_f64(),
            });
        }
        if hashed_download_file_ids.len() == hashed_before {
            warn!(
                "Final hash stage returned no updates for repo={} (remaining_files={})",
                normalized_repo_url,
                remaining_file_ids.len()
            );
        }
    }
    info!(
        "Total incremental hash tree loads for repo={} before final consistency pass: {}",
        normalized_repo_url, hash_tree_loads
    );
    let hash_work_performed =
        incremental_hash_tree_context.is_some() || !addon_hash_metrics.is_empty();
    let finalized_hashes = if !hash_work_performed {
        info!(
            "Skipping repository hash finalization for repo={} because all requested files were already verified",
            normalized_repo_url
        );
        true
    } else if let Some(hash_context) = incremental_hash_tree_context.as_mut() {
        let db = context.db();
        finalize_repository_hashes_from_tree(&db, &mut hash_context.tree, &normalized_repo_url)
            .await
    } else {
        finalize_repository_hashes_from_mods(context.clone(), &normalized_repo_url).await
    };
    if !finalized_hashes {
        warn!(
            "Failed to finalize repository hash rollup for repo={}",
            normalized_repo_url
        );
    }
    info!("Hash stage finished in {:.2?}", hash_start.elapsed());
    if let Some(hash_context) = incremental_hash_tree_context.as_ref() {
        let _ = refresh_content_hashes_for_tree(
            context.clone(),
            &normalized_repo_url,
            &hash_context.tree,
        )
        .await;
    } else if hash_work_performed {
        let _ =
            refresh_content_hashes_when_tree_matches(context.clone(), &normalized_repo_url, None)
                .await;
    } else if !download_file_ids.is_empty() {
        match Tree::load_for_files(context.clone(), &normalized_repo_url, &download_file_ids).await
        {
            Ok(scoped_tree) => {
                let _ = refresh_content_hashes_for_scoped_tree(
                    context.clone(),
                    &normalized_repo_url,
                    &scoped_tree,
                )
                .await;
            }
            Err(err) => {
                warn!(
                    "Failed to load scoped tree for verified download content-hash refresh repo={}: {}",
                    normalized_repo_url, err
                );
            }
        }
    }

    // Propagate checksums to sibling repositories sharing the same addon paths,
    // so subsequent syncs in the same space skip already-downloaded files.
    let propagated_sibling_urls =
        propagate_checksums_to_siblings(context.clone(), &normalized_repo_url).await;
    if !propagated_sibling_urls.is_empty() {
        emit_progress!(ProgressEvent::SiblingPropagation {
            repo_urls: propagated_sibling_urls,
        });
    }

    let total_hash_duration = incremental_hash_duration + hash_start.elapsed();
    emit_progress!(ProgressEvent::HashSummary {
        cumulative_hash_ms: total_hash_duration.as_millis() as u64,
        after_download_hash_ms: hash_start.elapsed().as_millis() as u64,
    });
    let total_download_bytes = estimated_download_bytes;
    let selected_hash_profile = sticky_auto_hash_profile
        .map(|profile| profile.to_string())
        .unwrap_or_else(|| hash_io_profile.to_string());
    let critical_tail_after_download = hash_start.elapsed();
    let overlapped_with_download = total_hash_duration
        .checked_sub(critical_tail_after_download)
        .unwrap_or_default();
    let hash_total_summary = HashTotalSummary {
        repo: &normalized_repo_url,
        files: download_file_ids.len(),
        bytes: total_download_bytes,
        incremental_files: hashed_download_file_ids.len(),
        remaining_files: remaining_file_ids.len(),
        tree_loads: hash_tree_loads,
        finalized: finalized_hashes,
        total_elapsed: total_hash_duration,
        incremental_elapsed: incremental_hash_duration,
        finalize_elapsed: hash_start.elapsed(),
        selected_profile: &selected_hash_profile,
        phase_timings: hash_phase_timings.clone(),
        critical_tail_after_download,
        overlapped_with_download,
    };
    emit_progress!(ProgressEvent::Stage {
        label: format!("Hash {:.1}s", total_hash_duration.as_secs_f32()),
        percent: 0.95,
    });

    // Emit a fresh diff after hashes so UI can update downloaded states immediately
    if download_file_ids.is_empty() {
        info!(
            "Skipping post-download diff refresh for repo={} because there were no downloaded file IDs",
            normalized_repo_url
        );
    } else if let Some(hash_context) = incremental_hash_tree_context.as_ref() {
        let diff_filter = if download_mod_ids.is_empty() {
            None
        } else {
            Some(&download_mod_ids)
        };
        let mods = emit_diff(&hash_context.tree, diff_filter);
        persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
    } else if let Ok(tree_after_hash) = Tree::load(context.clone(), &normalized_repo_url).await {
        let diff_filter = if download_mod_ids.is_empty() {
            None
        } else {
            Some(&download_mod_ids)
        };
        let mods = emit_diff(&tree_after_hash, diff_filter);
        persist_pending_updates(context.clone(), &normalized_repo_url, &mods).await;
    } else {
        warn!("Failed to build diff tree after hash stage");
    }

    // Only clear queued downloads after a successful full download cycle
    if let Err(err) = truncate_all_download_tables(context.clone()).await {
        warn!("Failed to clear download queue after sync: {}", err);
    } else {
        info!("Cleared download queue after sync");
    }
    if let Some(session) = rollback_session.as_ref() {
        let mut rollback = session.lock().await;
        let touched_files = rollback.touched_file_ids().len();
        if let Err(err) = rollback.commit().await {
            let message = format!("Failed to commit update rollback cleanup: {}", err);
            error!("{}", message);
            summary.log_table("failed-rollback-commit");
            emit_progress!(ProgressEvent::Failed(message));
            return;
        }
        info!(
            "Committed update rollback session for repo={} touched_files={}",
            normalized_repo_url, touched_files
        );
    }
    summary.push(
        StageEntry::new("download", download_start.elapsed())
            .with("files", download_file_ids.len())
            .with("mods", download_mod_ids.len())
            .with("incremental_hashes", hashed_download_file_ids.len())
            .with("tree_loads", hash_tree_loads)
            .with(
                "incremental_hash_time",
                format!("{:.2}s", incremental_hash_duration.as_secs_f64()),
            ),
    );
    summary.push(
        StageEntry::new("hash_finalize", hash_start.elapsed())
            .with("remaining_files", remaining_file_ids.len()),
    );
    summary.log_table("completed");
    emit_progress!(ProgressEvent::Stage {
        label: "Done".into(),
        percent: 1.0,
    });
    emit_progress!(ProgressEvent::Finished);
    info!(
        "{}",
        render_final_update_report(
            &operation_id,
            &normalized_repo_url,
            overall_start.elapsed(),
            &download_report,
            &hash_total_summary,
            &addon_hash_metrics,
            &sqlite_perf_guard,
        )
    );
    sqlite_perf_guard.mark_final_report_logged();
}

pub fn spawn_repository_sync(
    repository_url: String,
    local_path: String,
    selected_mod_states: Vec<(String, bool)>,
    progress_tx: Sender<ProgressEvent>,
    mode: SyncMode,
    options: RepositorySyncOptions,
    repaint_ctx: Option<egui::Context>,
) -> std::thread::JoinHandle<()> {
    if let Some(repaint_ctx) = repaint_ctx {
        let mut repaint_rx = progress_tx.subscribe();
        std::thread::spawn(move || {
            const REPAINT_THROTTLE: Duration = Duration::from_millis(16);
            let mut last_repaint = Instant::now() - REPAINT_THROTTLE;

            loop {
                match repaint_rx.blocking_recv() {
                    Ok(_) => {
                        let now = Instant::now();
                        let elapsed = now.duration_since(last_repaint);
                        if elapsed >= REPAINT_THROTTLE {
                            repaint_ctx.request_repaint();
                            last_repaint = now;
                        } else {
                            repaint_ctx.request_repaint_after(REPAINT_THROTTLE - elapsed);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    std::thread::spawn(move || {
        let operation_id = options.operation_id.clone();
        let mod_enabled_overrides: HashMap<String, bool> = selected_mod_states
            .into_iter()
            .map(|(name, enabled)| (name.to_lowercase(), enabled))
            .collect();
        info!("Spawning repository sync worker for mode {:?}", mode);
        let rt = match Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(err) => {
                error!("Failed to build tokio runtime for repository sync: {}", err);
                send_progress_event(
                    &progress_tx,
                    ProgressEvent::Failed(format!("Failed to initialize sync runtime: {}", err)),
                    &operation_id,
                );
                return;
            }
        };
        rt.block_on(run_repository_pipeline(
            repository_url,
            local_path,
            mod_enabled_overrides,
            progress_tx.clone(),
            mode,
            options,
        ));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{FoxyDb, params};

    #[tokio::test]
    async fn checksum_reconciliation_updates_only_target_repository_instance() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        fdb.execute(
            "INSERT INTO repositories \
                 (id, name, remote_url, local_path, local_checksum, remote_checksum) \
             VALUES \
                 (1, 'Shared', 'https://example.invalid/repo/', 'C:/shared', 'SHARED_OLD', 'REMOTE'), \
                 (2, 'Standalone', 'https://example.invalid/repo/', 'D:/standalone', 'TARGET_OLD', 'REMOTE')",
            params![],
        )
        .await
        .expect("insert repository instances");

        let context = Arc::new(
            FoxyContext::new(db.clone(), reqwest::Client::new())
                .with_target_local_path("D:/standalone"),
        );
        assert_eq!(
            reconcile_target_repository_local_checksum(context, "https://example.invalid/repo/")
                .await
                .expect("reconcile target checksum"),
            1
        );

        let shared = fdb
            .query_one(
                "SELECT local_checksum FROM repositories WHERE id = 1",
                params![],
            )
            .await
            .expect("load shared instance")
            .expect("shared instance missing");
        let standalone = fdb
            .query_one(
                "SELECT local_checksum FROM repositories WHERE id = 2",
                params![],
            )
            .await
            .expect("load standalone instance")
            .expect("standalone instance missing");
        assert_eq!(shared.get_string("local_checksum").unwrap(), "SHARED_OLD");
        assert_eq!(standalone.get_string("local_checksum").unwrap(), "REMOTE");
    }

    #[tokio::test]
    async fn part_less_repository_detection_is_scoped_to_repository_instance() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        // Two repository instances of the same URL in different folders.
        fdb.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES \
                 (1, 'Healthy', 'https://example.invalid/repo/', 'C:/space'), \
                 (2, 'PartLess', 'https://example.invalid/repo/', 'D:/standalone'), \
                 (3, 'WholeFile', 'https://example.invalid/repo/', 'E:/wholefile')",
            params![],
        )
        .await
        .expect("insert repositories");
        fdb.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required) VALUES \
                 (10, 'a1', 'rp1', 'lp1', 1), (20, 'a2', 'rp2', 'lp2', 1), (30, 'a3', 'rp3', 'lp3', 1)",
            params![],
        )
        .await
        .expect("insert addons");
        // file 100/200 have NULL tree hash; file 300 carries a tree hash (a
        // legitimate whole-file manifest entry).
        fdb.execute(
            "INSERT INTO files (id, name, remote_path, local_path, local_checksum) VALUES \
                 (100, 'f1', 'frp1', 'flp1', ''), (200, 'f2', 'frp2', 'flp2', ''), \
                 (300, 'f3', 'frp3', 'flp3', 'TREEHASH')",
            params![],
        )
        .await
        .expect("insert files");
        fdb.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 10), (2, 20), (3, 30)",
            params![],
        )
        .await
        .expect("insert repository_addons");
        fdb.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (10, 100), (20, 200), (30, 300)",
            params![],
        )
        .await
        .expect("insert addon_files");
        // Only the healthy instance (repo 1) has a part row; the standalone
        // instance (repo 2) has unhashed files but zero parts.
        fdb.execute(
            "INSERT INTO subfiles (file_id, path, remote_length, remote_start, remote_checksum, data_order) \
                 VALUES (100, 'p1', 1, 0, 'c1', 0)",
            params![],
        )
        .await
        .expect("insert subfile");

        assert!(
            !repository_has_files_but_no_parts(&fdb, 1).await,
            "healthy instance has parts and must not be flagged"
        );
        assert!(
            repository_has_files_but_no_parts(&fdb, 2).await,
            "standalone instance has unhashed files but no parts and must be flagged"
        );
        assert!(
            !repository_has_files_but_no_parts(&fdb, 3).await,
            "legitimate whole-file manifest (tree hash present, no parts) must not be flagged"
        );
    }

    #[tokio::test]
    async fn repository_with_no_files_is_not_flagged_as_part_less() {
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = FoxyDb::from_turso(db.clone());
        fdb.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES \
                 (1, 'Empty', 'https://example.invalid/repo/', 'C:/empty')",
            params![],
        )
        .await
        .expect("insert repository");
        // No addons/files linked: an empty repo is not the part-less corruption.
        assert!(!repository_has_files_but_no_parts(&fdb, 1).await);
    }

    #[test]
    fn pending_update_recorded_requires_known_differing_remote() {
        // No remote checksum known yet → not a recorded pending update.
        assert!(!pending_update_recorded("", "LOCAL"));
        assert!(!pending_update_recorded("   ", "LOCAL"));
        // Remote equals local (case/whitespace-insensitive) → already up to date.
        assert!(!pending_update_recorded("ABC", "abc"));
        assert!(!pending_update_recorded(" ABC ", "abc"));
        // Remote differs from local → genuine pending update.
        assert!(pending_update_recorded("ABC", "DEF"));
        assert!(pending_update_recorded("ABC", ""));
    }

    #[test]
    fn prepared_queue_reuse_requires_pending_update_and_unchanged_remote() {
        // No pending update → never reuse, regardless of probe.
        assert!(!prepared_queue_reuse_is_safe("ABC", "ABC", Some("ABC")));
        assert!(!prepared_queue_reuse_is_safe("", "", Some("")));
        // Pending update but the probe could not be obtained → do not reuse.
        assert!(!prepared_queue_reuse_is_safe("REMOTE", "LOCAL", None));
        // Pending update but the remote changed since the queue was built → rebuild.
        assert!(!prepared_queue_reuse_is_safe(
            "REMOTE",
            "LOCAL",
            Some("REMOTE_NEW")
        ));
        // Pending update and the probe matches the stored remote → safe to reuse.
        assert!(prepared_queue_reuse_is_safe(
            "REMOTE",
            "LOCAL",
            Some("REMOTE")
        ));
        // Match is case- and whitespace-insensitive like the rest of the gate.
        assert!(prepared_queue_reuse_is_safe(
            "REMOTE",
            "LOCAL",
            Some(" remote ")
        ));
    }

    #[test]
    fn auto_rebenchmark_waits_for_threshold() {
        assert!(!should_rebenchmark_auto_profile(
            HashIoProfilePreference::Auto,
            false,
            99,
            100
        ));
        assert!(should_rebenchmark_auto_profile(
            HashIoProfilePreference::Auto,
            false,
            100,
            100
        ));
    }

    #[test]
    fn auto_rebenchmark_requires_auto_without_pending_work() {
        assert!(!should_rebenchmark_auto_profile(
            HashIoProfilePreference::Auto,
            true,
            200,
            100
        ));
        assert!(!should_rebenchmark_auto_profile(
            HashIoProfilePreference::Balanced,
            false,
            200,
            100
        ));
        assert!(!should_rebenchmark_auto_profile(
            HashIoProfilePreference::Auto,
            false,
            200,
            0
        ));
    }

    #[test]
    fn auto_rebenchmark_schedule_uses_ten_percent_milestones() {
        let mut schedule = AutoRebenchmarkSchedule::new(1_000).unwrap();

        assert_eq!(
            schedule.next_milestone(),
            Some(AutoRebenchmarkMilestone {
                percent: 10,
                threshold_bytes: 100,
            })
        );
        assert_eq!(schedule.take_crossed_milestone(99), None);
        assert_eq!(
            schedule.take_crossed_milestone(100),
            Some(AutoRebenchmarkMilestone {
                percent: 10,
                threshold_bytes: 100,
            })
        );
        assert_eq!(
            schedule.next_milestone(),
            Some(AutoRebenchmarkMilestone {
                percent: 20,
                threshold_bytes: 200,
            })
        );
    }

    #[test]
    fn auto_rebenchmark_schedule_skips_milestones_already_crossed_by_a_completion_jump() {
        let mut schedule = AutoRebenchmarkSchedule::new(1_000).unwrap();

        assert_eq!(
            schedule.take_crossed_milestone(450),
            Some(AutoRebenchmarkMilestone {
                percent: 10,
                threshold_bytes: 100,
            })
        );
        assert_eq!(
            schedule.next_milestone(),
            Some(AutoRebenchmarkMilestone {
                percent: 50,
                threshold_bytes: 500,
            })
        );
    }

    #[test]
    fn auto_rebenchmark_schedule_stops_before_download_completion() {
        let mut schedule = AutoRebenchmarkSchedule::new(10).unwrap();

        assert_eq!(schedule.take_crossed_milestone(10).unwrap().percent, 10);
        assert_eq!(schedule.next_milestone(), None);
    }

    #[test]
    fn confirmation_preflight_builds_a_download_plan_without_downloading() {
        assert!(should_build_download_plan(SyncMode::RecheckOnly, true));
        assert!(!should_build_download_plan(SyncMode::RecheckOnly, false));
        assert!(should_build_download_plan(SyncMode::Download, false));
    }

    #[test]
    fn check_only_delta_refresh_waits_for_pending_updates() {
        assert!(!should_refresh_delta_plan_after_quick_verify(
            false, false, false
        ));
        assert!(should_refresh_delta_plan_after_quick_verify(
            false, true, false
        ));
    }

    #[test]
    fn delta_refresh_skips_download_plan_or_already_refreshed_paths() {
        assert!(!should_refresh_delta_plan_after_quick_verify(
            true, true, false
        ));
        assert!(!should_refresh_delta_plan_after_quick_verify(
            false, true, true
        ));
    }

    #[test]
    fn cached_pending_scope_prevalidates_quick_verify() {
        let mut scope = HashSet::new();
        scope.insert("@ace3".to_string());

        assert!(quick_verify_already_eligible(Some(&scope)));
    }

    #[test]
    fn missing_or_empty_pending_scope_keeps_quick_verify_preflight() {
        let empty_scope = HashSet::new();

        assert!(!quick_verify_already_eligible(None));
        assert!(!quick_verify_already_eligible(Some(&empty_scope)));
    }

    #[test]
    fn remote_metadata_rechecks_defer_fresh_part_inserts_until_tree_load() {
        assert!(should_defer_remote_metadata_part_inserts(
            SyncMode::RemoteRefreshOnly,
            false
        ));
        assert!(should_defer_remote_metadata_part_inserts(
            SyncMode::RecheckOnly,
            false
        ));
        assert!(should_defer_remote_metadata_part_inserts(
            SyncMode::Download,
            false
        ));
    }

    #[test]
    fn part_insert_deferral_skips_quick_check_and_force_redownload() {
        assert!(!should_defer_remote_metadata_part_inserts(
            SyncMode::QuickCheckOnly,
            false
        ));
        assert!(!should_defer_remote_metadata_part_inserts(
            SyncMode::RemoteRefreshOnly,
            true
        ));
    }

    #[test]
    fn only_force_redownload_queues_targets_during_remote_metadata() {
        assert!(should_queue_download_targets_during_remote_metadata(
            SyncMode::Download,
            true
        ));
        assert!(!should_queue_download_targets_during_remote_metadata(
            SyncMode::RemoteRefreshOnly,
            false
        ));
        assert!(!should_queue_download_targets_during_remote_metadata(
            SyncMode::RemoteRefreshOnly,
            true
        ));
        assert!(!should_queue_download_targets_during_remote_metadata(
            SyncMode::Download,
            false
        ));
    }

    #[test]
    fn missing_addon_ratio_is_suspect_at_ninety_percent() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 10,
            missing_addons: 9,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };

        assert!(missing_addon_ratio_is_suspect(&summary));
    }

    #[test]
    fn missing_addon_ratio_ignores_small_repositories() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 4,
            missing_addons: 4,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };

        assert!(!missing_addon_ratio_is_suspect(&summary));
    }

    #[test]
    fn partial_missing_addon_ratio_catches_large_partial_redownloads() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 96,
            missing_addons: 51,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };

        assert!(!missing_addon_ratio_is_suspect(&summary));
        assert!(partial_missing_addon_ratio_is_suspect(&summary));
        assert!(suspect_full_redownload_guard_applies(
            &summary, false, true, false
        ));
    }

    #[test]
    fn partial_missing_addon_ratio_ignores_small_partial_updates() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 30,
            missing_addons: 10,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };

        assert!(!partial_missing_addon_ratio_is_suspect(&summary));
    }

    #[test]
    fn suspect_guard_requires_existing_state_or_recent_path_reset() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 20,
            missing_addons: 20,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };

        assert!(!suspect_full_redownload_guard_applies(
            &summary, false, false, false
        ));
        assert!(suspect_full_redownload_guard_applies(
            &summary, true, false, false
        ));
        assert!(suspect_full_redownload_guard_applies(
            &summary, false, true, false
        ));
        assert!(!suspect_full_redownload_guard_applies(
            &summary, true, true, true
        ));
    }

    // ── local_path_mismatch_guard_applies ───────────────────────────────

    #[test]
    fn suspect_guard_allows_empty_root_as_fresh_download_destination() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 20,
            missing_addons: 20,
            empty_repo_root: true,
            sample_paths: Vec::new(),
        };

        assert!(!suspect_full_redownload_guard_applies(
            &summary, true, true, false
        ));
    }

    #[test]
    fn local_path_mismatch_guard_applies_for_check_only_modes() {
        assert!(local_path_mismatch_guard_applies(SyncMode::RecheckOnly));
        assert!(local_path_mismatch_guard_applies(
            SyncMode::RemoteRefreshOnly
        ));
    }

    #[test]
    fn local_path_mismatch_guard_skips_download_and_other_modes() {
        assert!(!local_path_mismatch_guard_applies(SyncMode::Download));
        assert!(!local_path_mismatch_guard_applies(SyncMode::QuickCheckOnly));
        assert!(!local_path_mismatch_guard_applies(
            SyncMode::RecheckIntegrity
        ));
    }

    // ── format_suspect_full_redownload_message ──────────────────────────

    #[test]
    fn format_suspect_message_includes_missing_counts() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 10,
            missing_addons: 9,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };
        let message = format_suspect_full_redownload_message("C:/repo", &summary);
        assert!(message.contains("9/10 enabled addon folders are missing"));
        assert!(message.contains("Force redownload"));
    }

    #[test]
    fn format_suspect_message_without_samples_omits_sample_suffix() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 10,
            missing_addons: 9,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };
        let message = format_suspect_full_redownload_message("C:/repo", &summary);
        assert!(!message.contains("Sample missing addon paths"));
    }

    #[test]
    fn format_suspect_message_with_samples_lists_them() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 10,
            missing_addons: 9,
            empty_repo_root: false,
            sample_paths: vec!["C:/repo/@ace".to_string(), String::new()],
        };
        let message = format_suspect_full_redownload_message("C:/repo", &summary);
        assert!(message.contains("Sample missing addon paths:"));
        assert!(message.contains("@ace"));
    }

    // ── AutoRebenchmarkSchedule edges ───────────────────────────────────

    #[test]
    fn auto_rebenchmark_schedule_zero_total_is_none() {
        assert!(AutoRebenchmarkSchedule::new(0).is_none());
    }

    #[test]
    fn auto_rebenchmark_threshold_rounds_up_and_floors_at_one() {
        let schedule = AutoRebenchmarkSchedule::new(1_000).unwrap();
        assert_eq!(schedule.threshold_for_percent(10), 100);
        assert_eq!(schedule.threshold_for_percent(25), 250);

        // Tiny totals still produce a threshold of at least one byte.
        let tiny = AutoRebenchmarkSchedule::new(5).unwrap();
        assert_eq!(tiny.threshold_for_percent(10), 1);
    }

    #[test]
    fn auto_rebenchmark_advance_past_skips_multiple_milestones() {
        let mut schedule = AutoRebenchmarkSchedule::new(1_000).unwrap();
        // Jumping straight past 30% should leave 40% as the next milestone.
        assert_eq!(schedule.take_crossed_milestone(350).unwrap().percent, 10);
        assert_eq!(schedule.next_milestone().unwrap().percent, 40);
    }

    // ── suspect ratio boundaries ────────────────────────────────────────

    #[test]
    fn missing_addon_ratio_below_ninety_percent_is_not_suspect() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 10,
            missing_addons: 8,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };
        assert!(!missing_addon_ratio_is_suspect(&summary));
    }

    #[test]
    fn partial_missing_requires_minimum_missing_count() {
        let just_below = MissingAddonPathSummary {
            enabled_addons: 30,
            missing_addons: 19,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };
        let at_minimum = MissingAddonPathSummary {
            enabled_addons: 30,
            missing_addons: 20,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };
        assert!(!partial_missing_addon_ratio_is_suspect(&just_below));
        assert!(partial_missing_addon_ratio_is_suspect(&at_minimum));
    }

    #[test]
    fn suspect_guard_respects_allow_override() {
        let summary = MissingAddonPathSummary {
            enabled_addons: 10,
            missing_addons: 10,
            empty_repo_root: false,
            sample_paths: Vec::new(),
        };
        // Would be suspect, but the explicit force-redownload override disables it.
        assert!(suspect_full_redownload_guard_applies(
            &summary, true, false, false
        ));
        assert!(!suspect_full_redownload_guard_applies(
            &summary, true, false, true
        ));
    }
}
