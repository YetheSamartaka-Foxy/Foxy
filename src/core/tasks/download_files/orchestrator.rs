use crate::core::api::{FileDiffSummary, ModDiffSummary, ProgressEvent, send_progress_event};
use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::load_download_patch_file;
use crate::core::models::download_target_file::{
    DownloadProgressUpdate, DownloadTargetFile, DownloadTargetWithModName,
    fetch_all_download_targets_with_mod_and_name, save_download_target_file,
    update_download_target_progress_batch,
};
use crate::core::utils::app_paths::foxy_large_payload_dir;
use crate::core::utils::format::sanitize_log_path;
use anyhow::anyhow;
use futures::StreamExt;
use futures::future::join_all;
use futures::stream::FuturesUnordered;
use log::{error, info, warn};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::broadcast::Sender;
use tokio::sync::{Semaphore, mpsc, watch};

use super::BYTES_PER_MEGABIT;
use super::DownloadResourceLimits;
use super::SharedRollbackSession;
use super::bandwidth::AdaptiveBandwidthLimiter;
use super::batching::{DownloadModCompletion, ModDownloadBatch, process_mod_batch};
use super::metrics::{
    DownloadMetrics, DownloadModOutcome, DownloadRunReport, DownloadSchedulerState,
};
use super::progress::{download_progress_percent, start_progress_ticker};
use super::range_scheduler::{RangePartMeta, range_part_meta_path};
use super::transfer::cancellation_requested;
use crate::core::utils::resource_profile::{ResourcePressure, ResourceProfile};
use crate::core::utils::speed_of_light::{SolLight, sol_line};

/// Disk space safety margin (500 MB) to avoid filling the drive completely.
const DISK_SPACE_MARGIN_BYTES: u64 = 500 * 1024 * 1024;
/// Maximum retries for network connectivity pre-check.
const CONNECTIVITY_CHECK_MAX_RETRIES: u32 = 3;
/// Base delay for connectivity check retries.
const CONNECTIVITY_CHECK_BASE_DELAY_MS: u64 = 1000;
const PROGRESS_CHECKPOINT_NORMAL_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
const PROGRESS_CHECKPOINT_PRESSURE_DELAY: std::time::Duration = std::time::Duration::from_secs(15);
const PROGRESS_CHECKPOINT_NORMAL_BYTES: usize = 8 * 1024 * 1024;
const PROGRESS_CHECKPOINT_PRESSURE_BYTES: usize = 32 * 1024 * 1024;
const PROGRESS_CHECKPOINT_SLOW_WRITE_MS: u128 = 250;
const PROGRESS_CHECKPOINT_RECOVERY_FLUSHES: usize = 3;

fn download_limits_for_profile(resource_profile: ResourceProfile) -> DownloadResourceLimits {
    match resource_profile.pressure {
        ResourcePressure::Normal => DownloadResourceLimits::normal(),
        ResourcePressure::Constrained => DownloadResourceLimits::constrained(),
        ResourcePressure::Severe => DownloadResourceLimits::severe(),
    }
}

fn download_display_name(path: &str) -> String {
    path.rsplit(['\\', '/'])
        .find(|name| !name.is_empty())
        .unwrap_or(path)
        .to_owned()
}

pub(crate) fn build_download_estimate_diffs(
    targets: &[DownloadTargetWithModName],
) -> Vec<ModDiffSummary> {
    let mut mod_indices = HashMap::<String, usize>::new();
    let mut mods = Vec::<ModDiffSummary>::new();

    for target in targets {
        let mod_idx = *mod_indices
            .entry(target.mod_name.clone())
            .or_insert_with(|| {
                let idx = mods.len();
                mods.push(ModDiffSummary {
                    name: target.mod_name.clone(),
                    needs_update: true,
                    total_bytes: 0,
                    files: Vec::new(),
                });
                idx
            });

        let estimate = target
            .download
            .expected_download_bytes
            .min(target.download.size) as u64;
        let mod_summary = &mut mods[mod_idx];
        mod_summary.total_bytes = mod_summary.total_bytes.saturating_add(estimate);
        mod_summary.files.push(FileDiffSummary {
            name: download_display_name(target.download.download_local_path.as_ref()),
            needs_update: true,
            total_bytes: estimate,
            changed_parts: 0,
        });
    }

    mods
}

pub(crate) async fn apply_download_plan_bytes(
    context: Arc<FoxyContext>,
    targets: &mut [DownloadTargetWithModName],
) -> (HashSet<u64>, u64, u64) {
    let patch_plans = join_all(targets.iter().map(|target| {
        let context = context.clone();
        let file_id = target.download.file_id;
        async move {
            let planned = match load_download_patch_file(context, file_id as i64).await {
                Ok(Some(row)) if !row.status.eq_ignore_ascii_case("fallback_full") => {
                    Some(row.planned_download_bytes as usize)
                }
                _ => None,
            };
            (file_id, planned)
        }
    }))
    .await;
    let patch_plan_by_file: HashMap<u64, usize> = patch_plans
        .into_iter()
        .filter_map(|(file_id, planned)| planned.map(|bytes| (file_id, bytes)))
        .collect();

    let mut patchable_file_ids = HashSet::new();
    let mut planned_bytes = 0u64;
    let mut full_bytes = 0u64;
    for target in targets {
        if let Some(planned) = patch_plan_by_file.get(&target.download.file_id).copied() {
            let capped = planned.min(target.download.size);
            target.download.expected_download_bytes = capped;
            if capped < target.download.size {
                patchable_file_ids.insert(target.download.file_id);
            }
        }
        planned_bytes =
            planned_bytes.saturating_add(target.download.expected_download_bytes as u64);
        full_bytes = full_bytes.saturating_add(target.download.size as u64);
    }

    (patchable_file_ids, planned_bytes, full_bytes)
}

/// Clean up stale temp files from previous delta patching crashes and validate
/// that download target files on disk match the expected state in the database.
async fn pre_download_cleanup(targets: &[DownloadTargetWithModName]) {
    // Collect unique parent directories from download targets to scan for temp files
    let mut parent_dirs: HashSet<&Path> = HashSet::new();
    for target in targets {
        if let Some(parent) = Path::new(target.download.download_local_path.as_ref()).parent() {
            parent_dirs.insert(parent);
        }
    }

    let mut cleaned_tmp = 0u64;
    let mut cleaned_bak = 0u64;
    let current_part_paths: HashSet<PathBuf> = targets
        .iter()
        .map(|target| part_path_for_target(&target.download.download_local_path))
        .collect();
    let current_meta_paths: HashSet<PathBuf> = targets
        .iter()
        .map(|target| PathBuf::from(range_part_meta_path(&target.download.download_local_path)))
        .collect();

    for dir in &parent_dirs {
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.ends_with(".foxy.tmp")
                || name.ends_with(".foxy.bak")
                || name.ends_with(".foxy.part.meta.tmp")
            {
                let path = entry.path();
                if let Ok(meta) = tokio::fs::metadata(&path).await {
                    let size = meta.len();
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => {
                            if name.ends_with(".foxy.tmp") {
                                cleaned_tmp += size;
                            } else {
                                cleaned_bak += size;
                            }
                        }
                        Err(err) => {
                            warn!(
                                "Failed to clean stale temp file {}: {}",
                                path.display(),
                                err
                            );
                        }
                    }
                }
            } else if name.ends_with(".foxy.part") || name.ends_with(".foxy.part.meta") {
                let path = entry.path();
                if current_part_paths.contains(&path) || current_meta_paths.contains(&path) {
                    continue;
                }
                if let Ok(meta) = tokio::fs::metadata(&path).await {
                    let size = meta.len();
                    match tokio::fs::remove_file(&path).await {
                        Ok(()) => {
                            info!(
                                "Cleaned stale non-queued part file: {}",
                                sanitize_log_path(&path)
                            );
                        }
                        Err(err) => {
                            warn!(
                                "Failed to clean stale non-queued part file {}: {}",
                                sanitize_log_path(&path),
                                err
                            );
                        }
                    }
                    cleaned_bak += size;
                }
            }
        }
    }

    // Clean stale patch artifacts in the patches temp directory, but preserve
    // artifacts for files that are about to be downloaded (they were just created
    // by the delta patch planning step during manifest update).
    let current_file_ids: HashSet<u64> = targets.iter().map(|t| t.download.file_id).collect();
    let patches_dir = foxy_large_payload_dir();
    if let Ok(mut entries) = tokio::fs::read_dir(&patches_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if name.starts_with("file_")
                && (name.ends_with(".patch.json") || name.ends_with(".patch.bin"))
            {
                // Extract file_id from filename like "file_1234.patch.json"
                let is_current = name
                    .strip_prefix("file_")
                    .and_then(|rest| rest.split('.').next())
                    .and_then(|id_str| id_str.parse::<u64>().ok())
                    .is_some_and(|id| current_file_ids.contains(&id));
                if is_current {
                    continue;
                }
                let path = entry.path();
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {
                        info!("Cleaned stale patch artifact: {}", sanitize_log_path(&path));
                    }
                    Err(err) => {
                        warn!(
                            "Failed to clean stale patch artifact {}: {}",
                            sanitize_log_path(&path),
                            err
                        );
                    }
                }
            }
        }
    }

    if cleaned_tmp > 0 || cleaned_bak > 0 {
        info!(
            "Startup cleanup: removed {:.1} MB temp files and {:.1} MB backup/stale files",
            cleaned_tmp as f64 / (1024.0 * 1024.0),
            cleaned_bak as f64 / (1024.0 * 1024.0),
        );
    }
}

fn part_path_for_target(path: &str) -> PathBuf {
    PathBuf::from(format!("{}.foxy.part", path))
}

fn staged_temp_paths_for_target(path: &str) -> [PathBuf; 3] {
    [
        part_path_for_target(path),
        PathBuf::from(format!("{}.foxy.tmp", path)),
        PathBuf::from(range_part_meta_path(path)),
    ]
}

fn should_checkpoint_download_progress(
    current: usize,
    last_persisted: usize,
    file_size: usize,
    dirty_threshold: usize,
) -> bool {
    if current == last_persisted {
        return false;
    }
    current >= file_size || current.saturating_sub(last_persisted) >= dirty_threshold
}

async fn cleanup_staged_temp_files(targets: &[DownloadTargetFile]) -> (usize, usize) {
    let mut staged_paths = HashSet::new();
    for target in targets {
        staged_paths.extend(staged_temp_paths_for_target(&target.download_local_path));
    }

    let mut removed = 0usize;
    let mut failed = 0usize;
    for path in staged_paths {
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                removed += 1;
                info!(
                    "Cleaned cancelled download temp file: {}",
                    sanitize_log_path(&path)
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                failed += 1;
                warn!(
                    "Failed to clean cancelled download temp file {}: {}",
                    sanitize_log_path(&path),
                    err
                );
            }
        }
    }

    (removed, failed)
}

/// Validate that download target files on disk are consistent with database state.
/// Resets download progress for files that are missing or have unexpected sizes.
///
/// Uses `spawn_blocking` to avoid blocking the tokio runtime with synchronous
/// filesystem metadata calls, which can stall other async tasks when many
/// download targets need validation.
async fn validate_download_targets(targets: &mut [DownloadTargetWithModName]) {
    // Collect paths + DB progress into a Vec so we can send them into spawn_blocking.
    let checks: Vec<(String, usize, usize)> = targets
        .iter()
        .map(|t| {
            (
                t.download.download_local_path.to_string(),
                t.download.size,
                t.download.download_total.load(Ordering::Relaxed),
            )
        })
        .collect();

    let decisions: Vec<DownloadProgressReconcile> = tokio::task::spawn_blocking(move || {
        checks
            .iter()
            .map(|(path, size, db_downloaded)| {
                let part_path = format!("{}.foxy.part", path);
                let part_len = std::fs::metadata(&part_path)
                    .ok()
                    .map(|meta| meta.len() as usize);
                // A valid resume sidecar records which chunks of a ranged
                // download already completed; its byte count is the trusted
                // progress for full-length pre-allocated part files.
                let sidecar_completed = RangePartMeta::load_sync(&range_part_meta_path(path))
                    .filter(|meta| meta.file_size == *size as u64)
                    .map(|meta| meta.completed_bytes() as usize);
                reconcile_download_progress(*db_downloaded, *size, part_len, sidecar_completed)
            })
            .collect()
    })
    .await
    .unwrap_or_else(|_| vec![DownloadProgressReconcile::default(); targets.len()]);

    let mut reset_count = 0usize;
    let mut resumed_count = 0usize;
    let mut removed_invalid_part_count = 0usize;
    for (target, decision) in targets.iter_mut().zip(decisions.iter()) {
        if decision.remove_part {
            let part_path = part_path_for_target(&target.download.download_local_path);
            match tokio::fs::remove_file(&part_path).await {
                Ok(()) => {
                    removed_invalid_part_count += 1;
                    info!(
                        "Removed invalid resumable part file: {}",
                        sanitize_log_path(&part_path)
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    warn!(
                        "Failed to remove invalid resumable part file {}: {}",
                        sanitize_log_path(&part_path),
                        err
                    );
                }
            }
            let meta_path =
                PathBuf::from(range_part_meta_path(&target.download.download_local_path));
            match tokio::fs::remove_file(&meta_path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    warn!(
                        "Failed to remove invalid resume sidecar {}: {}",
                        sanitize_log_path(&meta_path),
                        err
                    );
                }
            }
        }

        let previous = target.download.download_total.load(Ordering::Relaxed);
        if previous != decision.download_total {
            target
                .download
                .download_total
                .store(decision.download_total, Ordering::SeqCst);
            target.download.download_cycle.store(0, Ordering::SeqCst);
            if decision.download_total == 0 {
                reset_count += 1;
            } else {
                resumed_count += 1;
            }
        }
    }

    if resumed_count > 0 || reset_count > 0 || removed_invalid_part_count > 0 {
        info!(
            "Startup validation: resumed_progress={} reset_progress={} removed_invalid_part_files={}",
            resumed_count, reset_count, removed_invalid_part_count
        );
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DownloadProgressReconcile {
    download_total: usize,
    remove_part: bool,
}

fn reconcile_download_progress(
    db_downloaded: usize,
    file_size: usize,
    part_len: Option<usize>,
    sidecar_completed: Option<usize>,
) -> DownloadProgressReconcile {
    let Some(part_len) = part_len else {
        return DownloadProgressReconcile {
            download_total: 0,
            remove_part: false,
        };
    };

    if part_len == 0 {
        return DownloadProgressReconcile {
            download_total: 0,
            remove_part: true,
        };
    }

    // Ranged downloads pre-allocate the full file and record completed chunks
    // in a sidecar; trust the sidecar byte count over part length or DB state.
    if part_len == file_size
        && let Some(completed) = sidecar_completed
    {
        return DownloadProgressReconcile {
            download_total: completed.min(file_size),
            remove_part: false,
        };
    }

    if part_len < file_size {
        // A sidecar with a short part is inconsistent (ranged parts are always
        // full length) - discard. Without a sidecar this is an append-style
        // part and its length is the resume offset.
        if sidecar_completed.is_some() {
            return DownloadProgressReconcile {
                download_total: 0,
                remove_part: true,
            };
        }
        return DownloadProgressReconcile {
            download_total: part_len,
            remove_part: false,
        };
    }

    if part_len == file_size && db_downloaded >= file_size {
        return DownloadProgressReconcile {
            download_total: file_size,
            remove_part: false,
        };
    }

    DownloadProgressReconcile {
        download_total: 0,
        remove_part: true,
    }
}

/// Check that enough disk space is available for the planned downloads.
/// Returns Ok(()) if sufficient, or an error message if not.
fn check_disk_space(targets: &[DownloadTargetWithModName]) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }

    let total_needed: u64 = targets
        .iter()
        .map(|t| t.download.expected_download_bytes as u64)
        .sum();

    // Find the first valid download target path to check disk space on its drive
    let check_path = targets
        .iter()
        .filter_map(|t| {
            Path::new(t.download.download_local_path.as_ref())
                .parent()
                .filter(|p| p.exists())
        })
        .next();

    let Some(check_path) = check_path else {
        return Ok(()); // Can't determine path, proceed optimistically
    };

    match fs4::available_space(check_path) {
        Ok(available) => {
            let needed_with_margin = total_needed.saturating_add(DISK_SPACE_MARGIN_BYTES);
            if available < needed_with_margin {
                Err(format!(
                    "Insufficient disk space: need {:.1} GB ({:.1} GB + {:.0} MB margin) but only {:.1} GB available on {}",
                    needed_with_margin as f64 / (1024.0 * 1024.0 * 1024.0),
                    total_needed as f64 / (1024.0 * 1024.0 * 1024.0),
                    DISK_SPACE_MARGIN_BYTES as f64 / (1024.0 * 1024.0),
                    available as f64 / (1024.0 * 1024.0 * 1024.0),
                    check_path.display()
                ))
            } else {
                info!(
                    "Disk space check passed: need {:.1} GB, available {:.1} GB",
                    needed_with_margin as f64 / (1024.0 * 1024.0 * 1024.0),
                    available as f64 / (1024.0 * 1024.0 * 1024.0),
                );
                Ok(())
            }
        }
        Err(err) => {
            warn!("Could not check disk space: {}", err);
            Ok(()) // Proceed optimistically if we can't check
        }
    }
}

/// Check whether the download server supports HTTP Range requests.
/// Sends a single HEAD request with a Range header; returns `true` if the server
/// responds with 206 Partial Content. This is called once per download session
/// to avoid redundant per-file probes that waste bandwidth and connections.
async fn check_range_support(context: &FoxyContext, sample_url: &str) -> bool {
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        context
            .client
            .head(sample_url)
            .header("Range", "bytes=0-0")
            .send(),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let supports = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            info!(
                "Range support check: {} (server returned {})",
                if supports {
                    "supported"
                } else {
                    "not supported"
                },
                resp.status()
            );
            supports
        }
        Ok(Err(e)) => {
            warn!("Range support check failed: {}, assuming not supported", e);
            false
        }
        Err(_) => {
            warn!("Range support check timed out, assuming not supported");
            false
        }
    }
}

/// Try a HEAD request to the download server to verify network connectivity.
/// Retries with backoff before giving up - avoids having every file individually
/// exhaust its retry budget when the network is simply down.
async fn check_network_connectivity(context: &FoxyContext, sample_url: &str) -> Result<(), String> {
    for attempt in 0..=CONNECTIVITY_CHECK_MAX_RETRIES {
        if attempt > 0 {
            let delay = CONNECTIVITY_CHECK_BASE_DELAY_MS * (1 << (attempt - 1).min(3));
            warn!(
                "Connectivity check retry {}/{} after {}ms",
                attempt + 1,
                CONNECTIVITY_CHECK_MAX_RETRIES + 1,
                delay
            );
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }

        match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            context.client.head(sample_url).send(),
        )
        .await
        {
            Ok(Ok(resp)) => {
                let status = resp.status();
                if status.is_success() {
                    info!(
                        "Network connectivity check passed: {} returned {}",
                        sample_url, status
                    );
                } else {
                    warn!(
                        "Network connectivity check: server reachable but returned {} for {} - downloads may fail",
                        status, sample_url
                    );
                }
                return Ok(());
            }
            Ok(Err(err)) => {
                warn!("Connectivity check attempt {} failed: {}", attempt + 1, err);
            }
            Err(_) => {
                warn!("Connectivity check attempt {} timed out", attempt + 1);
            }
        }
    }

    Err(format!(
        "Download server unreachable after {} attempts - check your network connection (tried: {})",
        CONNECTIVITY_CHECK_MAX_RETRIES + 1,
        sample_url
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_files(
    context: Arc<FoxyContext>,
    progress_tx: Option<Sender<ProgressEvent>>,
    download_speed_limit_mbps: Option<u32>,
    download_pause_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
    rollback_session: Option<SharedRollbackSession>,
    mod_completion_tx: Option<mpsc::Sender<DownloadModCompletion>>,
    allowed_file_ids: Option<HashSet<u64>>,
    operation_id: String,
    telemetry_epoch: Arc<std::sync::OnceLock<std::time::Instant>>,
) -> anyhow::Result<DownloadRunReport> {
    info!("Download worker started: op={}", operation_id);
    let resource_profile = ResourceProfile::sample();
    let resource_limits = download_limits_for_profile(resource_profile);
    info!(
        "Download resource profile: {}; limits large_files={} small_files={} ranges={} per_file_ranges={}..{} range_chunk={}",
        resource_profile.summary(),
        resource_limits.max_large_files,
        resource_limits.max_small_files,
        resource_limits.max_active_range_requests,
        resource_limits.min_ranges_per_file,
        resource_limits.max_ranges_per_file,
        resource_limits.range_chunk_target
    );
    let large_file_permits = Arc::new(Semaphore::new(resource_limits.max_large_files));
    let small_file_permits = Arc::new(Semaphore::new(resource_limits.max_small_files));
    let scheduler_state = Arc::new(DownloadSchedulerState::new(resource_limits));

    let rate_limiter = Arc::new(AdaptiveBandwidthLimiter::from_mbps(
        download_speed_limit_mbps,
    ));

    let metrics = Arc::new(DownloadMetrics::new());

    let mut targets = {
        let _phase = metrics.phase("fetch_download_targets");
        match fetch_all_download_targets_with_mod_and_name(context.clone()).await {
            Ok(t) => t,
            Err(e) => {
                error!("Failed to fetch download targets: {}", e);
                return Err(e.into());
            }
        }
    };

    // When the pipeline provides a scoped file-id set (e.g. from quick-scan
    // pending updates), drop any download-target rows that leaked in from
    // remote_repository() for mods that do not actually need updating.
    if let Some(ref allowed) = allowed_file_ids {
        let pre_filter = targets.len();
        targets.retain(|t| allowed.contains(&t.download.file_id));
        if targets.len() < pre_filter {
            info!(
                "Download target scope filter: kept {} of {} targets ({} dropped)",
                targets.len(),
                pre_filter,
                pre_filter - targets.len()
            );
        }
    }

    let download_stage_started = std::time::Instant::now();

    if targets.is_empty() {
        info!("No download targets found, skipping download stage.");
        return Ok(metrics.build_report(&[]));
    }

    // Startup cleanup: remove stale temp files and validate file state vs DB
    {
        let _phase = metrics.phase("pre_download_cleanup");
        pre_download_cleanup(&targets).await;
    }
    {
        let _phase = metrics.phase("validate_targets");
        validate_download_targets(&mut targets).await;
    }
    if cancellation_requested(&cancel_rx) {
        return Err(anyhow!("download cancelled"));
    }

    let (patchable_file_ids, patch_planned_bytes, full_bytes) = if context.force_download_targets {
        let full_bytes = targets
            .iter()
            .map(|target| target.download.size as u64)
            .sum();
        for target in &mut targets {
            target.download.expected_download_bytes = target.download.size;
        }
        (HashSet::new(), full_bytes, full_bytes)
    } else {
        let _phase = metrics.phase("load_patch_plans");
        apply_download_plan_bytes(context.clone(), &mut targets).await
    };
    info!(
        "Download queue delta planning: files={} patchable_files={} planned_transfer_bytes={} full_bytes={}",
        targets.len(),
        patchable_file_ids.len(),
        patch_planned_bytes,
        full_bytes
    );
    if let Some(tx) = progress_tx.as_ref() {
        let estimate_mods = build_download_estimate_diffs(&targets);
        let estimate_bytes: u64 = estimate_mods.iter().map(|m| m.total_bytes).sum();
        info!(
            "Download queue UI estimate refreshed: mods={} files={} estimated_transfer_bytes={}",
            estimate_mods.len(),
            targets.len(),
            estimate_bytes
        );
        send_progress_event(
            tx,
            ProgressEvent::DownloadPlan {
                files_total: targets.len(),
                planned_bytes: patch_planned_bytes,
                full_bytes,
                patch_files: patchable_file_ids.len(),
            },
            &operation_id,
        );
        send_progress_event(
            tx,
            ProgressEvent::Diff {
                mods: estimate_mods,
            },
            &operation_id,
        );
    }

    // Issue 14: Verify network connectivity before starting downloads
    {
        let _phase = metrics.phase("connectivity_check");
        if let Some(first_url) = targets
            .first()
            .map(|t| t.download.download_remote_url.clone())
            && let Err(net_err) = check_network_connectivity(&context, &first_url).await
        {
            error!("{}", net_err);
            if let Some(tx) = progress_tx.as_ref() {
                send_progress_event(tx, ProgressEvent::Failed(net_err.clone()), &operation_id);
            }
            return Err(anyhow!(net_err));
        }
    }

    // Check Range support once for the whole session to avoid per-file probes
    let supports_range = {
        let _phase = metrics.phase("range_support_check");
        if let Some(first_url) = targets
            .first()
            .map(|t| t.download.download_remote_url.clone())
        {
            check_range_support(&context, &first_url).await
        } else {
            false
        }
    };

    // Issue 09: Check available disk space before starting downloads
    {
        let _phase = metrics.phase("disk_space_check");
        if let Err(space_err) = check_disk_space(&targets) {
            error!("{}", space_err);
            if let Some(tx) = progress_tx.as_ref() {
                send_progress_event(tx, ProgressEvent::Failed(space_err.clone()), &operation_id);
            }
            return Err(anyhow!(space_err));
        }
    }
    if cancellation_requested(&cancel_rx) {
        return Err(anyhow!("download cancelled"));
    }

    // Collect lightweight references to all download targets for periodic progress persistence
    let all_download_refs: Vec<DownloadTargetFile> =
        targets.iter().map(|t| t.download.clone()).collect();

    let mut grouped: HashMap<u64, (String, Vec<DownloadTargetFile>)> = HashMap::new();
    for target in targets {
        grouped
            .entry(target.mod_id)
            .or_insert_with(|| (target.mod_name.clone(), Vec::new()))
            .1
            .push(target.download);
    }

    let mut batches: Vec<ModDownloadBatch> = grouped
        .into_iter()
        .map(|(mod_id, (mod_name, files))| {
            let total_size = files.iter().map(|f| f.expected_download_bytes).sum();
            let batch_patchable_file_ids = files
                .iter()
                .filter(|file| patchable_file_ids.contains(&file.file_id))
                .map(|file| file.file_id)
                .collect();
            ModDownloadBatch {
                mod_id,
                mod_name,
                files,
                total_size,
                patchable_file_ids: batch_patchable_file_ids,
            }
        })
        .collect();

    // Prioritize completing whole mods (largest first) before moving on.
    batches.sort_by_key(|batch| std::cmp::Reverse(batch.total_size));

    let total_files: usize = batches.iter().map(|b| b.files.len()).sum();
    let completed = Arc::new(AtomicUsize::new(0));

    if let Some(tx) = progress_tx.as_ref() {
        send_progress_event(
            tx,
            ProgressEvent::Stage {
                label: format!("Download 0/{}", total_files),
                percent: download_progress_percent(0, total_files),
            },
            &operation_id,
        );
    }

    let ticker = start_progress_ticker(
        progress_tx.clone(),
        completed.clone(),
        total_files,
        operation_id.clone(),
    );

    // Start always-on throughput sampler (independent of bandwidth limiter).
    // Anchor telemetry elapsed at the moment real download work begins - after
    // the pre-download validation phases above - so the speed graph starts at
    // the download's own t=0 instead of trailing the prep work. Share that epoch
    // with the hash worker so the download and hash lanes line up on one timeline.
    let telemetry_started_at = std::time::Instant::now();
    let _ = telemetry_epoch.set(telemetry_started_at);
    let sampler_handle = metrics.spawn_sampler(progress_tx.clone(), telemetry_started_at);

    // Periodic progress checkpoint: flush dirty download progress with adaptive spacing.
    // When SQLite reports contention or a slow checkpoint, progress persistence backs off
    // until several clean flushes complete. The final flush remains authoritative.
    let checkpoint_stop = Arc::new(AtomicBool::new(false));
    let checkpoint_handle = {
        let stop_signal = checkpoint_stop.clone();
        let ctx = context.clone();
        let refs = all_download_refs.clone();
        let checkpoint_metrics = metrics.clone();
        tokio::spawn(async move {
            let mut last_persisted: HashMap<u64, usize> =
                refs.iter().map(|f| (f.file_id, 0)).collect();
            let mut delay = PROGRESS_CHECKPOINT_NORMAL_DELAY;
            let mut dirty_threshold = PROGRESS_CHECKPOINT_NORMAL_BYTES;
            let mut clean_pressure_flushes = 0usize;
            loop {
                tokio::time::sleep(delay).await;
                if stop_signal.load(Ordering::SeqCst) {
                    break;
                }
                let mut updates = Vec::new();
                for file in &refs {
                    let current = file.download_total.load(Ordering::Relaxed);
                    let last = last_persisted.get(&file.file_id).copied().unwrap_or(0);
                    if should_checkpoint_download_progress(
                        current,
                        last,
                        file.size,
                        dirty_threshold,
                    ) {
                        updates.push(DownloadProgressUpdate {
                            file_id: file.file_id,
                            download_total: current,
                            download_cycle: file.download_cycle.load(Ordering::Relaxed),
                        });
                    }
                }
                if !updates.is_empty() {
                    let checkpoint_started = std::time::Instant::now();
                    let checkpoint_rows = updates.len();
                    let persist_result =
                        update_download_target_progress_batch(ctx.clone(), &updates).await;
                    let elapsed_ms = checkpoint_started.elapsed().as_millis() as u64;
                    checkpoint_metrics
                        .counters
                        .db_checkpoint_ms
                        .fetch_add(elapsed_ms, Ordering::Relaxed);
                    checkpoint_metrics
                        .counters
                        .db_checkpoint_batches
                        .fetch_add(1, Ordering::Relaxed);
                    checkpoint_metrics
                        .counters
                        .db_checkpoint_rows
                        .fetch_add(checkpoint_rows, Ordering::Relaxed);
                    match persist_result {
                        Ok(persisted) => {
                            for update in &updates {
                                last_persisted.insert(update.file_id, update.download_total);
                            }
                            checkpoint_metrics
                                .counters
                                .db_checkpoint_statements
                                .fetch_add(persisted.statements, Ordering::Relaxed);
                            let under_pressure = persisted.sqlite_delta.lock_retries > 0
                                || persisted.elapsed.as_millis()
                                    >= PROGRESS_CHECKPOINT_SLOW_WRITE_MS;
                            if under_pressure {
                                clean_pressure_flushes = 0;
                                if delay != PROGRESS_CHECKPOINT_PRESSURE_DELAY {
                                    info!(
                                        "Download progress checkpoint entering DB pressure mode: rows={} statements={} retries={} elapsed_ms={}",
                                        persisted.rows,
                                        persisted.statements,
                                        persisted.sqlite_delta.lock_retries,
                                        persisted.elapsed.as_millis()
                                    );
                                }
                                delay = PROGRESS_CHECKPOINT_PRESSURE_DELAY;
                                dirty_threshold = PROGRESS_CHECKPOINT_PRESSURE_BYTES;
                            } else if delay == PROGRESS_CHECKPOINT_PRESSURE_DELAY {
                                clean_pressure_flushes += 1;
                                if clean_pressure_flushes >= PROGRESS_CHECKPOINT_RECOVERY_FLUSHES {
                                    info!(
                                        "Download progress checkpoint leaving DB pressure mode after {} clean flushes",
                                        clean_pressure_flushes
                                    );
                                    delay = PROGRESS_CHECKPOINT_NORMAL_DELAY;
                                    dirty_threshold = PROGRESS_CHECKPOINT_NORMAL_BYTES;
                                    clean_pressure_flushes = 0;
                                }
                            }
                        }
                        Err(err) => {
                            warn!("Progress checkpoint failed: {}", err);
                            delay = PROGRESS_CHECKPOINT_PRESSURE_DELAY;
                            dirty_threshold = PROGRESS_CHECKPOINT_PRESSURE_BYTES;
                            clean_pressure_flushes = 0;
                        }
                    }
                }
            }
        })
    };

    // Start ALL mods concurrently. Each mod batch spawns its files immediately,
    // with the shared semaphores (large_file_permits / small_file_permits) acting
    // as the sole concurrency limiter. This avoids the old proportional slot-budget
    // system that starved mods with few large files.
    let mut active: FuturesUnordered<tokio::task::JoinHandle<(usize, ModDownloadBatch, bool)>> =
        FuturesUnordered::new();

    // Initialize queued large file count for adaptive split scheduling.
    let total_large_files: usize = batches
        .iter()
        .flat_map(|b| &b.files)
        .filter(|f| f.size > super::LARGE_FILE_THRESHOLD)
        .count();
    scheduler_state
        .queued_large_files
        .store(total_large_files, Ordering::Relaxed);

    for batch in batches {
        let file_count = batch.files.len();
        let ctx = context.clone();
        let limiter = rate_limiter.clone();
        let large_sem = large_file_permits.clone();
        let small_sem = small_file_permits.clone();
        let progress_tx_clone = progress_tx.clone();
        let mod_completion_tx_clone = mod_completion_tx.clone();
        let completed_clone = completed.clone();
        let pause_rx = download_pause_rx.clone();
        let cancel_rx_clone = cancel_rx.clone();
        let rollback_session_clone = rollback_session.clone();
        let metrics_clone = metrics.clone();
        let scheduler_clone = scheduler_state.clone();
        let handle = tokio::spawn(async move {
            process_mod_batch(
                batch,
                ctx,
                limiter,
                large_sem,
                small_sem,
                progress_tx_clone,
                completed_clone,
                file_count,
                pause_rx,
                cancel_rx_clone,
                rollback_session_clone,
                supports_range,
                metrics_clone,
                scheduler_clone,
                mod_completion_tx_clone,
            )
            .await
        });
        active.push(handle);
    }

    let mut mods_succeeded = 0usize;
    let mut mods_failed = 0usize;
    let mut mods_cancelled = 0usize;
    let mut mod_outcomes = Vec::new();

    while let Some(res) = active.next().await {
        match res {
            Ok((_slots_used, finished_batch, success)) => {
                if success {
                    mods_succeeded += 1;
                } else if cancellation_requested(&cancel_rx) {
                    mods_cancelled += 1;
                } else {
                    mods_failed += 1;
                }
                mod_outcomes.push(DownloadModOutcome {
                    mod_id: finished_batch.mod_id,
                    mod_name: finished_batch.mod_name.clone(),
                    success,
                });
                if cancellation_requested(&cancel_rx) && !success {
                    info!(
                        "Cancelled download for mod {} ({})",
                        finished_batch.mod_id, finished_batch.mod_name
                    );
                } else {
                    info!(
                        "Finished download for mod {} ({})",
                        finished_batch.mod_id, finished_batch.mod_name
                    );
                }
                if let Some(tx) = mod_completion_tx.as_ref() {
                    let completion = DownloadModCompletion {
                        mod_id: finished_batch.mod_id,
                        mod_name: finished_batch.mod_name.clone(),
                        file_ids: finished_batch
                            .files
                            .iter()
                            .map(|file| file.file_id)
                            .collect(),
                        bytes: finished_batch.total_size as u64,
                        success,
                    };
                    if tx.try_send(completion).is_err() {
                        warn!(
                            "Incremental hash queue full or closed; completed mod batch will be covered by final hash stage: mod_id={}",
                            finished_batch.mod_id
                        );
                    }
                }
            }
            Err(join_err) => {
                warn!("A mod download task failed to join: {}", join_err);
            }
        }
    }

    if let Some((handle, stop)) = ticker {
        stop.store(true, Ordering::SeqCst);
        let _ = handle.await;
    }

    // Stop the throughput sampler
    metrics.stop_sampler();
    let _ = sampler_handle.await;

    // Stop the checkpoint task and do a final progress flush (only files with progress)
    checkpoint_stop.store(true, Ordering::SeqCst);
    let _ = checkpoint_handle.await;
    {
        let _phase = metrics.phase("final_progress_flush");
        let final_updates: Vec<DownloadProgressUpdate> = all_download_refs
            .iter()
            .filter(|f| f.download_total.load(Ordering::Relaxed) > 0)
            .map(|f| DownloadProgressUpdate {
                file_id: f.file_id,
                download_total: f.download_total.load(Ordering::Relaxed),
                download_cycle: f.download_cycle.load(Ordering::Relaxed),
            })
            .collect();
        let final_rows = final_updates.len();
        let final_flush_started = std::time::Instant::now();
        match update_download_target_progress_batch(context.clone(), &final_updates).await {
            Ok(persisted) => {
                metrics
                    .counters
                    .db_checkpoint_statements
                    .fetch_add(persisted.statements, Ordering::Relaxed);
            }
            Err(err) => {
                warn!(
                    "Final progress flush failed, falling back to per-row save: {}",
                    err
                );
                for file in &all_download_refs {
                    let _ = save_download_target_file(context.clone(), file).await;
                }
            }
        }
        metrics.counters.db_checkpoint_ms.fetch_add(
            final_flush_started.elapsed().as_millis() as u64,
            Ordering::Relaxed,
        );
        metrics
            .counters
            .db_checkpoint_batches
            .fetch_add(1, Ordering::Relaxed);
        metrics
            .counters
            .db_checkpoint_rows
            .fetch_add(final_rows, Ordering::Relaxed);
    }

    let download_elapsed = download_stage_started.elapsed();
    let total_downloaded_bytes: u64 = all_download_refs
        .iter()
        .map(|f| f.download_total.load(Ordering::Relaxed) as u64)
        .sum();
    let total_expected_bytes: u64 = all_download_refs
        .iter()
        .map(|f| f.expected_download_bytes as u64)
        .sum();
    let total_full_bytes: u64 = all_download_refs.iter().map(|f| f.size as u64).sum();
    let delta_savings_bytes = total_full_bytes.saturating_sub(total_downloaded_bytes);
    let delta_savings_percent = delta_savings_bytes
        .saturating_mul(100)
        .checked_div(total_full_bytes)
        .unwrap_or(0);
    let avg_speed_mbps = if download_elapsed.as_secs_f64() > 0.0 {
        (total_downloaded_bytes as f64 / (1024.0 * 1024.0)) / download_elapsed.as_secs_f64()
    } else {
        0.0
    };
    info!(
        "Download stage completed: op={} elapsed={:.2?} files={} mods_succeeded={} mods_failed={} mods_cancelled={} bytes_transferred={} bytes_expected={} full_bytes={} delta_savings_bytes={} delta_savings_percent={}% avg_speed={:.2} MB/s",
        operation_id,
        download_elapsed,
        total_files,
        mods_succeeded,
        mods_failed,
        mods_cancelled,
        total_downloaded_bytes,
        total_expected_bytes,
        total_full_bytes,
        delta_savings_bytes,
        delta_savings_percent,
        avg_speed_mbps
    );

    // Speed-of-light accounting (see conventions/SPEED_OF_LIGHT.md, O1).
    // Light = configured limiter cap when set, otherwise the best 1-second
    // throughput sample demonstrated within this run.
    let wire_bytes = metrics
        .counters
        .bytes_transferred
        .load(std::sync::atomic::Ordering::Relaxed);
    let peak_bps = metrics
        .counters
        .peak_network_bps
        .load(std::sync::atomic::Ordering::Relaxed);
    let light = match download_speed_limit_mbps {
        Some(limit_mbps) if limit_mbps > 0 => {
            SolLight::LimiterCap(u64::from(limit_mbps).saturating_mul(BYTES_PER_MEGABIT))
        }
        _ if peak_bps > 0 => SolLight::PeakSample(peak_bps),
        _ => SolLight::SelfBaseline,
    };
    info!(
        "{}",
        sol_line(
            "download",
            wire_bytes,
            download_elapsed,
            &light,
            &[
                ("files", total_files.to_string()),
                ("peak_1s_bps", peak_bps.to_string()),
                ("delta_savings_percent", delta_savings_percent.to_string()),
            ],
        )
    );

    let report = metrics.build_report(&mod_outcomes);

    if cancellation_requested(&cancel_rx) {
        let (removed_temp_files, failed_temp_files) =
            cleanup_staged_temp_files(&all_download_refs).await;
        info!(
            "Cancelled download temp cleanup finished: removed={} failed={}",
            removed_temp_files, failed_temp_files
        );
        return Err(anyhow!("download cancelled"));
    }

    if mods_failed > 0 {
        return Err(anyhow!(
            "download failed for {} mod(s); {} mod(s) completed",
            mods_failed,
            mods_succeeded
        ));
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_temp_paths_use_target_path_suffixes() {
        let paths = staged_temp_paths_for_target("C:\\mods\\addon\\file.pbo");

        assert_eq!(
            paths[0],
            PathBuf::from("C:\\mods\\addon\\file.pbo.foxy.part")
        );
        assert_eq!(
            paths[1],
            PathBuf::from("C:\\mods\\addon\\file.pbo.foxy.tmp")
        );
        assert_eq!(
            paths[2],
            PathBuf::from("C:\\mods\\addon\\file.pbo.foxy.part.meta")
        );
    }

    #[test]
    fn completed_checkpoint_progress_is_dirty_only_once() {
        assert!(should_checkpoint_download_progress(100, 99, 100, 64));
        assert!(!should_checkpoint_download_progress(100, 100, 100, 64));
    }

    #[test]
    fn reconcile_progress_resumes_from_partial_part_file() {
        let decision = reconcile_download_progress(0, 1_000, Some(400), None);

        assert_eq!(
            decision,
            DownloadProgressReconcile {
                download_total: 400,
                remove_part: false,
            }
        );
    }

    #[test]
    fn reconcile_progress_removes_preallocated_part_without_sidecar_or_db_progress() {
        let decision = reconcile_download_progress(400, 1_000, Some(1_000), None);

        assert_eq!(
            decision,
            DownloadProgressReconcile {
                download_total: 0,
                remove_part: true,
            }
        );
    }

    #[test]
    fn reconcile_progress_keeps_complete_part_with_complete_db_progress() {
        let decision = reconcile_download_progress(1_000, 1_000, Some(1_000), None);

        assert_eq!(
            decision,
            DownloadProgressReconcile {
                download_total: 1_000,
                remove_part: false,
            }
        );
    }

    #[test]
    fn reconcile_progress_trusts_sidecar_for_preallocated_part() {
        let decision = reconcile_download_progress(0, 1_000, Some(1_000), Some(600));

        assert_eq!(
            decision,
            DownloadProgressReconcile {
                download_total: 600,
                remove_part: false,
            }
        );
    }

    #[test]
    fn reconcile_progress_caps_sidecar_progress_at_file_size() {
        let decision = reconcile_download_progress(0, 1_000, Some(1_000), Some(2_000));

        assert_eq!(
            decision,
            DownloadProgressReconcile {
                download_total: 1_000,
                remove_part: false,
            }
        );
    }

    #[test]
    fn reconcile_progress_removes_short_part_with_sidecar() {
        let decision = reconcile_download_progress(0, 1_000, Some(400), Some(300));

        assert_eq!(
            decision,
            DownloadProgressReconcile {
                download_total: 0,
                remove_part: true,
            }
        );
    }

    #[test]
    fn in_progress_checkpoint_requires_dirty_byte_threshold() {
        assert!(!should_checkpoint_download_progress(63, 0, 100, 64));
        assert!(should_checkpoint_download_progress(64, 0, 100, 64));
    }

    #[test]
    fn constrained_profile_reduces_download_concurrency() {
        let profile =
            ResourceProfile::from_memory(8 * 1024 * 1024 * 1024, 3 * 1024 * 1024 * 1024, 0);
        let limits = download_limits_for_profile(profile);

        assert_eq!(limits.max_large_files, 4);
        assert_eq!(limits.max_active_range_requests, 16);
    }

    #[test]
    fn download_estimate_diffs_use_expected_transfer_bytes() {
        let targets = vec![
            DownloadTargetWithModName {
                download: DownloadTargetFile {
                    file_id: 1,
                    download_remote_url: Arc::from("http://example.test/a.pbo"),
                    download_local_path: Arc::from("C:\\mods\\@optre\\addons\\a.pbo"),
                    size: 1_000,
                    expected_download_bytes: 250,
                    download_total: Arc::new(AtomicUsize::new(0)),
                    download_cycle: Arc::new(AtomicUsize::new(0)),
                },
                mod_id: 10,
                mod_name: "@optre".to_owned(),
            },
            DownloadTargetWithModName {
                download: DownloadTargetFile {
                    file_id: 2,
                    download_remote_url: Arc::from("http://example.test/b.pbo"),
                    download_local_path: Arc::from("C:\\mods\\@optre\\addons\\b.pbo"),
                    size: 2_000,
                    expected_download_bytes: 3_000,
                    download_total: Arc::new(AtomicUsize::new(0)),
                    download_cycle: Arc::new(AtomicUsize::new(0)),
                },
                mod_id: 10,
                mod_name: "@optre".to_owned(),
            },
        ];

        let mods = build_download_estimate_diffs(&targets);

        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "@optre");
        assert_eq!(mods[0].total_bytes, 2_250);
        assert_eq!(mods[0].files.len(), 2);
        assert_eq!(mods[0].files[0].name, "a.pbo");
        assert_eq!(mods[0].files[0].total_bytes, 250);
        assert_eq!(mods[0].files[1].total_bytes, 2_000);
    }
}
