use crate::core::api::ProgressEvent;
use crate::core::models::context::FoxyContext;
use crate::core::models::download_target_file::DownloadTargetFile;
use crate::core::tasks::delta_patch::try_patch_first;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use log::{debug, error, info, warn};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::Sender;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::sleep;

use super::SharedRollbackSession;
use super::bandwidth::AdaptiveBandwidthLimiter;
use super::metrics::{DownloadMetrics, DownloadSchedulerState, FileMetric};
use super::progress::{ModProgressEntry, summarize_mod_progress};
use super::transfer::{
    TransferStats, cancellation_requested, download_cancelled_error, download_file_ranges,
    is_download_cancelled_error, wait_for_download_resume,
};
use super::{LARGE_FILE_THRESHOLD, MAX_FILE_RETRIES};

/// Files at or below this size are classified as "tiny" and skip patch plan
/// checks and per-file progress DB writes, reducing overhead for the many
/// small metadata/signature files typical in mod repositories.
const TINY_FILE_THRESHOLD: usize = 64 * 1024;
const MOD_PROGRESS_TICK_INTERVAL: Duration = Duration::from_millis(500);

pub(super) struct ModDownloadBatch {
    pub(super) mod_id: u64,
    pub(super) mod_name: String,
    pub(super) files: Vec<DownloadTargetFile>,
    pub(super) total_size: usize,
    /// File IDs that have a delta patch plan available.
    /// Files not in this set skip `try_patch_first` entirely.
    pub(super) patchable_file_ids: HashSet<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadModCompletion {
    pub(crate) mod_id: u64,
    pub(crate) mod_name: String,
    pub(crate) file_ids: HashSet<u64>,
    pub(crate) bytes: u64,
    pub(crate) success: bool,
}

#[allow(clippy::too_many_arguments)]
async fn download_single_file(
    context: Arc<FoxyContext>,
    file: DownloadTargetFile,
    rate_limiter: Arc<AdaptiveBandwidthLimiter>,
    large_file_permits: Arc<Semaphore>,
    small_file_permits: Arc<Semaphore>,
    completed: Arc<AtomicUsize>,
    mod_id: u64,
    mod_name: Arc<str>,
    mod_files_done: Arc<AtomicUsize>,
    mod_bytes_done: Arc<AtomicUsize>,
    mut download_pause_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
    rollback_session: Option<SharedRollbackSession>,
    supports_range: bool,
    metrics: Arc<DownloadMetrics>,
    has_patch_plan: bool,
    scheduler: Arc<DownloadSchedulerState>,
    download_completion_tx: Option<mpsc::Sender<DownloadModCompletion>>,
) -> Result<(), anyhow::Error> {
    let (permit_pool, permit_type) = if file.size > LARGE_FILE_THRESHOLD {
        (Arc::clone(&large_file_permits), "large")
    } else {
        (Arc::clone(&small_file_permits), "small")
    };

    let mut permit_cancel_rx = cancel_rx.clone();
    wait_for_download_resume(&mut download_pause_rx, &mut permit_cancel_rx).await?;
    if cancellation_requested(&cancel_rx) {
        return Err(download_cancelled_error());
    }

    let is_large = file.size > LARGE_FILE_THRESHOLD;
    let permit_wait_started = std::time::Instant::now();
    let _permit = match permit_pool.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => {
            error!(
                "Download semaphore closed for mod {} file {}, skipping",
                mod_id, file.download_remote_url
            );
            return Err(anyhow::anyhow!("Download semaphore closed"));
        }
    };
    let permit_wait = permit_wait_started.elapsed();
    metrics
        .counters
        .active_files
        .fetch_add(1, Ordering::Relaxed);
    if is_large {
        scheduler.active_large_files.fetch_add(1, Ordering::Relaxed);
        // Saturating decrement - avoids wrapping to usize::MAX on retries
        // (a retried file has already been dequeued on the first attempt).
        let _ =
            scheduler
                .queued_large_files
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_sub(1));
    }
    let expected_transfer_bytes = file.expected_download_bytes.min(file.size);
    let file_download_started = std::time::Instant::now();

    debug!(
        "Downloading file: file_id={} mod_id={} url={} path={} expected={} bytes full={} bytes permit={}",
        file.file_id,
        mod_id,
        &file.download_remote_url,
        &file.download_local_path,
        expected_transfer_bytes,
        file.size,
        permit_type
    );

    let (result, download_method): (Result<Option<TransferStats>, anyhow::Error>, &str) =
        if has_patch_plan {
            match try_patch_first(
                context.clone(),
                &file,
                download_pause_rx.clone(),
                cancel_rx.clone(),
                rollback_session.clone(),
                rate_limiter.clone(),
                metrics.clone(),
            )
            .await
            {
                Ok(true) => {
                    file.download_total
                        .store(expected_transfer_bytes, Ordering::SeqCst);
                    file.download_cycle
                        .store(expected_transfer_bytes, Ordering::SeqCst);
                    (Ok(None), "delta_patch")
                }
                Ok(false) => {
                    let r = download_file_ranges(
                        context.clone(),
                        &file,
                        &file.download_remote_url,
                        &file.download_local_path,
                        rate_limiter.clone(),
                        download_pause_rx,
                        cancel_rx.clone(),
                        rollback_session,
                        supports_range,
                        metrics.clone(),
                        scheduler.clone(),
                    )
                    .await;
                    (r.map(Some), "full_download")
                }
                Err(err) if cancellation_requested(&cancel_rx) => (Err(err), "cancelled"),
                Err(err) => {
                    warn!(
                        "Patch-first attempt failed unexpectedly: file_id={} mod_id={} file={} error={}",
                        file.file_id, mod_id, &file.download_remote_url, err
                    );
                    let r = download_file_ranges(
                        context.clone(),
                        &file,
                        &file.download_remote_url,
                        &file.download_local_path,
                        rate_limiter.clone(),
                        download_pause_rx,
                        cancel_rx.clone(),
                        rollback_session,
                        supports_range,
                        metrics.clone(),
                        scheduler.clone(),
                    )
                    .await;
                    (r.map(Some), "full_after_patch_error")
                }
            }
        } else {
            let r = download_file_ranges(
                context.clone(),
                &file,
                &file.download_remote_url,
                &file.download_local_path,
                rate_limiter.clone(),
                download_pause_rx,
                cancel_rx.clone(),
                rollback_session,
                supports_range,
                metrics.clone(),
                scheduler.clone(),
            )
            .await;
            (r.map(Some), "full_download")
        };

    metrics
        .counters
        .active_files
        .fetch_sub(1, Ordering::Relaxed);
    if is_large {
        scheduler.active_large_files.fetch_sub(1, Ordering::Relaxed);
    }

    match &result {
        Ok(transfer_stats) => {
            let file_elapsed = file_download_started.elapsed();
            let actual_bytes = file.download_total.load(Ordering::SeqCst);
            let speed_mbps = if file_elapsed.as_secs_f64() > 0.0 {
                (actual_bytes as f64 / (1024.0 * 1024.0)) / file_elapsed.as_secs_f64()
            } else {
                0.0
            };
            let split_count = transfer_stats.as_ref().map(|s| s.split_count).unwrap_or(0);
            let disk_write_time = transfer_stats
                .as_ref()
                .map(|s| s.disk_write_time)
                .unwrap_or(Duration::ZERO);
            let disk_write_count = transfer_stats
                .as_ref()
                .map(|s| s.disk_write_count)
                .unwrap_or(0);
            let promote_time = transfer_stats
                .as_ref()
                .map(|s| s.promote_time)
                .unwrap_or(Duration::ZERO);
            metrics
                .counters
                .files_completed
                .fetch_add(1, Ordering::Relaxed);
            let actual_network_bytes = if download_method == "delta_patch" {
                expected_transfer_bytes
            } else {
                actual_bytes.min(file.size)
            };
            metrics.record_file(FileMetric {
                file_id: file.file_id,
                mod_id,
                size: file.size,
                expected_network_bytes: actual_network_bytes,
                method: download_method,
                split_count,
                permit_wait,
                first_byte_latency: None,
                transfer_time: file_elapsed,
                promote_time,
                disk_write_time,
                disk_write_count,
                retries: 0,
                avg_mbps: speed_mbps,
            });

            let is_tiny = file.size <= TINY_FILE_THRESHOLD;
            // Tiny files use debug-level logging to reduce log noise
            if is_tiny {
                debug!(
                    "Tiny file download completed: file_id={} mod_id={} size={} elapsed={:.2?} method={}",
                    file.file_id, mod_id, file.size, file_elapsed, download_method
                );
            } else {
                debug!(
                    "File download completed: file_id={} mod_id={} file_size={} actual_bytes={} elapsed={:.2?} speed={:.2} MB/s method={} permit={}",
                    file.file_id,
                    mod_id,
                    file.size,
                    actual_bytes,
                    file_elapsed,
                    speed_mbps,
                    download_method,
                    permit_type
                );
            }

            mod_files_done.fetch_add(1, Ordering::SeqCst);
            let file_network_bytes = file.download_total.load(Ordering::SeqCst).min(file.size);
            mod_bytes_done.fetch_add(file_network_bytes, Ordering::SeqCst);

            if let Some(tx) = download_completion_tx.as_ref() {
                let completion = DownloadModCompletion {
                    mod_id,
                    mod_name: mod_name.to_string(),
                    file_ids: [file.file_id].into_iter().collect(),
                    bytes: file.size as u64,
                    success: true,
                };
                if tx.try_send(completion).is_err() {
                    warn!(
                        "Incremental hash queue full or closed; file will be covered by final hash stage: file_id={} mod_id={}",
                        file.file_id, mod_id
                    );
                }
            }

            completed.fetch_add(1, Ordering::SeqCst);
        }
        Err(err) => {
            let file_elapsed = file_download_started.elapsed();
            if cancellation_requested(&cancel_rx) || is_download_cancelled_error(err) {
                info!(
                    "File download cancelled: file_id={} mod_id={} file={} elapsed={:.2?} method={}",
                    file.file_id, mod_id, &file.download_remote_url, file_elapsed, download_method
                );
            } else {
                error!(
                    "File download failed: file_id={} mod_id={} file={} elapsed={:.2?} method={} error={}",
                    file.file_id,
                    mod_id,
                    &file.download_remote_url,
                    file_elapsed,
                    download_method,
                    err
                );
            }
        }
    }

    result.map(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn process_mod_batch(
    mut batch: ModDownloadBatch,
    context: Arc<FoxyContext>,
    rate_limiter: Arc<AdaptiveBandwidthLimiter>,
    large_file_permits: Arc<Semaphore>,
    small_file_permits: Arc<Semaphore>,
    progress_tx: Option<Sender<ProgressEvent>>,
    completed: Arc<AtomicUsize>,
    slots_assigned: usize,
    mut download_pause_rx: watch::Receiver<bool>,
    mut cancel_rx: watch::Receiver<bool>,
    rollback_session: Option<SharedRollbackSession>,
    supports_range: bool,
    metrics: Arc<DownloadMetrics>,
    scheduler_state: Arc<DownloadSchedulerState>,
    download_completion_tx: Option<mpsc::Sender<DownloadModCompletion>>,
) -> (usize, ModDownloadBatch, bool) {
    info!(
        "Starting download for mod {} ({} files, {} bytes)",
        batch.mod_id,
        batch.files.len(),
        batch.total_size
    );

    let total_slots =
        scheduler_state.limits.max_large_files + scheduler_state.limits.max_small_files;
    let slot_budget = slots_assigned.clamp(1, total_slots.max(1));

    let mod_files_total = batch.files.len();
    let mod_bytes_total = batch.total_size;
    let mod_name_arc: Arc<str> = Arc::from(batch.mod_name.clone());

    // Extract lightweight progress entries (just size + download counter)
    // instead of cloning the entire DownloadTargetFile vec.
    let progress_entries: Arc<Vec<ModProgressEntry>> = Arc::new(
        batch
            .files
            .iter()
            .map(|f| ModProgressEntry {
                size: f.size,
                download_total: f.download_total.clone(),
            })
            .collect(),
    );
    let batch_files = batch.files.clone();
    let mut remaining = std::mem::take(&mut batch.files);
    batch.files = batch_files;
    let mut attempt = 0usize;

    let progress_stop = Arc::new(AtomicBool::new(false));
    let progress_handle = if let Some(tx) = progress_tx.clone() {
        let mod_name = mod_name_arc.clone();
        let entries = progress_entries.clone();
        let stop_signal = progress_stop.clone();
        Some(tokio::spawn(async move {
            let mut last_sent: Option<(usize, u64)> = None;
            loop {
                if stop_signal.load(Ordering::SeqCst) {
                    break;
                }
                let (files_done, bytes_done) = summarize_mod_progress(&entries);
                let snapshot = (files_done, bytes_done);
                if last_sent != Some(snapshot) {
                    let effective_total = mod_bytes_total.max(bytes_done as usize);
                    let percent = if effective_total == 0 {
                        1.0
                    } else {
                        (bytes_done as f32 / effective_total as f32).min(1.0)
                    };
                    let _ = tx.send(ProgressEvent::DownloadMod {
                        mod_name: mod_name.to_string(),
                        files_done,
                        files_total: mod_files_total,
                        bytes_done,
                        bytes_total: effective_total as u64,
                        percent,
                    });
                    last_sent = Some(snapshot);
                }
                sleep(MOD_PROGRESS_TICK_INTERVAL).await;
            }
            let (files_done, bytes_done) = summarize_mod_progress(&entries);
            let effective_total = mod_bytes_total.max(bytes_done as usize);
            let percent = if effective_total == 0 {
                1.0
            } else {
                (bytes_done as f32 / effective_total as f32).min(1.0)
            };
            let _ = tx.send(ProgressEvent::DownloadMod {
                mod_name: mod_name.to_string(),
                files_done,
                files_total: mod_files_total,
                bytes_done,
                bytes_total: effective_total as u64,
                percent,
            });
        }))
    } else {
        None
    };

    let mut total_retried_files = 0usize;
    let mut saw_cancelled_error = false;

    while !remaining.is_empty() && attempt < MAX_FILE_RETRIES && !cancellation_requested(&cancel_rx)
    {
        if wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx)
            .await
            .is_err()
        {
            break;
        }
        attempt += 1;

        let mut small_queue: VecDeque<DownloadTargetFile> = VecDeque::new();
        let mut large_queue: VecDeque<DownloadTargetFile> = VecDeque::new();
        for file in remaining.drain(..) {
            if file.size > LARGE_FILE_THRESHOLD {
                large_queue.push_back(file);
            } else {
                small_queue.push_back(file);
            }
        }

        let mut running_total = 0usize;
        let mut inflight: FuturesUnordered<_> = FuturesUnordered::new();

        let mut failed = Vec::new();

        let mod_files_done = Arc::new(AtomicUsize::new(0));
        let mod_bytes_done = Arc::new(AtomicUsize::new(0));

        let push_task =
            |inflight: &mut FuturesUnordered<_>,
             file: DownloadTargetFile,
             is_large: bool,
             context: Arc<FoxyContext>,
             limiter: Arc<AdaptiveBandwidthLimiter>,
             large: Arc<Semaphore>,
             small: Arc<Semaphore>,
             completed: Arc<AtomicUsize>,
             mod_id: u64,
             mod_name: Arc<str>,
             mod_files_done: Arc<AtomicUsize>,
             mod_bytes_done: Arc<AtomicUsize>,
             download_pause_rx: watch::Receiver<bool>,
             cancel_rx: watch::Receiver<bool>,
             rollback_session: Option<SharedRollbackSession>,
             supports_range: bool,
             metrics: Arc<DownloadMetrics>,
             has_patch_plan: bool,
             scheduler: Arc<DownloadSchedulerState>,
             download_completion_tx: Option<mpsc::Sender<DownloadModCompletion>>| {
                inflight.push(tokio::spawn(async move {
                    let res = download_single_file(
                        context,
                        file.clone(),
                        limiter,
                        large,
                        small,
                        completed,
                        mod_id,
                        mod_name,
                        mod_files_done,
                        mod_bytes_done,
                        download_pause_rx,
                        cancel_rx,
                        rollback_session,
                        supports_range,
                        metrics,
                        has_patch_plan,
                        scheduler,
                        download_completion_tx,
                    )
                    .await;
                    (res, is_large, file)
                }));
            };

        let ctx = context.clone();
        let limiter = rate_limiter.clone();
        let large_sem = large_file_permits.clone();
        let small_sem = small_file_permits.clone();

        // Kick off initial batch up to slot budget
        while running_total < slot_budget && !cancellation_requested(&cancel_rx) {
            if let Some(file) = large_queue.pop_front() {
                let patchable = batch.patchable_file_ids.contains(&file.file_id);
                push_task(
                    &mut inflight,
                    file,
                    true,
                    ctx.clone(),
                    limiter.clone(),
                    large_sem.clone(),
                    small_sem.clone(),
                    completed.clone(),
                    batch.mod_id,
                    mod_name_arc.clone(),
                    mod_files_done.clone(),
                    mod_bytes_done.clone(),
                    download_pause_rx.clone(),
                    cancel_rx.clone(),
                    rollback_session.clone(),
                    supports_range,
                    metrics.clone(),
                    patchable,
                    scheduler_state.clone(),
                    download_completion_tx.clone(),
                );
                running_total += 1;
                continue;
            }
            if let Some(file) = small_queue.pop_front() {
                let patchable = batch.patchable_file_ids.contains(&file.file_id);
                push_task(
                    &mut inflight,
                    file,
                    false,
                    ctx.clone(),
                    limiter.clone(),
                    large_sem.clone(),
                    small_sem.clone(),
                    completed.clone(),
                    batch.mod_id,
                    mod_name_arc.clone(),
                    mod_files_done.clone(),
                    mod_bytes_done.clone(),
                    download_pause_rx.clone(),
                    cancel_rx.clone(),
                    rollback_session.clone(),
                    supports_range,
                    metrics.clone(),
                    patchable,
                    scheduler_state.clone(),
                    download_completion_tx.clone(),
                );
                running_total += 1;
                continue;
            }
            break;
        }

        while let Some(res) = inflight.next().await {
            match res {
                Ok((Ok(()), _is_large, _file)) => {
                    running_total = running_total.saturating_sub(1);
                }
                Ok((Err(err), _is_large, file)) => {
                    running_total = running_total.saturating_sub(1);
                    if cancellation_requested(&cancel_rx) || is_download_cancelled_error(&err) {
                        saw_cancelled_error = true;
                        info!(
                            "Download attempt {} for mod {} file {} stopped by cancellation",
                            attempt, batch.mod_id, &file.download_remote_url
                        );
                    } else {
                        warn!(
                            "Download attempt {} for mod {} file {} failed: {}",
                            attempt, batch.mod_id, &file.download_remote_url, err
                        );
                    }
                    failed.push(file);
                }
                Err(join_err) => {
                    warn!(
                        "Download task join error for mod {}: {}",
                        batch.mod_id, join_err
                    );
                    running_total = running_total.saturating_sub(1);
                }
            }

            while running_total < slot_budget
                && !cancellation_requested(&cancel_rx)
                && !saw_cancelled_error
            {
                if let Some(file) = large_queue.pop_front() {
                    let patchable = batch.patchable_file_ids.contains(&file.file_id);
                    push_task(
                        &mut inflight,
                        file,
                        true,
                        ctx.clone(),
                        limiter.clone(),
                        large_sem.clone(),
                        small_sem.clone(),
                        completed.clone(),
                        batch.mod_id,
                        mod_name_arc.clone(),
                        mod_files_done.clone(),
                        mod_bytes_done.clone(),
                        download_pause_rx.clone(),
                        cancel_rx.clone(),
                        rollback_session.clone(),
                        supports_range,
                        metrics.clone(),
                        patchable,
                        scheduler_state.clone(),
                        download_completion_tx.clone(),
                    );
                    running_total += 1;
                    continue;
                }
                if let Some(file) = small_queue.pop_front() {
                    let patchable = batch.patchable_file_ids.contains(&file.file_id);
                    push_task(
                        &mut inflight,
                        file,
                        false,
                        ctx.clone(),
                        limiter.clone(),
                        large_sem.clone(),
                        small_sem.clone(),
                        completed.clone(),
                        batch.mod_id,
                        mod_name_arc.clone(),
                        mod_files_done.clone(),
                        mod_bytes_done.clone(),
                        download_pause_rx.clone(),
                        cancel_rx.clone(),
                        rollback_session.clone(),
                        supports_range,
                        metrics.clone(),
                        patchable,
                        scheduler_state.clone(),
                        download_completion_tx.clone(),
                    );
                    running_total += 1;
                    continue;
                }
                break;
            }
        }

        if cancellation_requested(&cancel_rx) || saw_cancelled_error {
            remaining.extend(large_queue);
            remaining.extend(small_queue);
            break;
        }

        if failed.is_empty() {
            if attempt > 1 {
                info!(
                    "Retry recovery: mod_id={} all files succeeded on attempt {} total_retried_files={}",
                    batch.mod_id, attempt, total_retried_files
                );
            }
            remaining.clear();
            break;
        }

        total_retried_files += failed.len();

        if attempt < MAX_FILE_RETRIES {
            let backoff_ms = 2000 * (1u64 << (attempt.saturating_sub(1)).min(3));
            warn!(
                "Retrying {} files for mod {} (attempt {}/{}) after {}ms",
                failed.len(),
                batch.mod_id,
                attempt + 1,
                MAX_FILE_RETRIES,
                backoff_ms
            );
            tokio::select! {
                _ = sleep(Duration::from_millis(backoff_ms)) => {}
                changed = cancel_rx.changed() => {
                    if changed.is_err() || cancellation_requested(&cancel_rx) {
                        remaining = failed;
                        break;
                    }
                }
            }
            // Reset per-file cycle counters so retried downloads don't double-count bytes
            for file in &failed {
                file.download_cycle.store(0, Ordering::SeqCst);
            }
            remaining = failed;
        } else {
            remaining = failed;
            break;
        }
    }

    let cancelled = cancellation_requested(&cancel_rx) || saw_cancelled_error;
    if !remaining.is_empty() && cancelled {
        info!(
            "Stopped download for mod {} with {} unfinished file(s) due to cancellation",
            batch.mod_id,
            remaining.len()
        );
    } else if !remaining.is_empty() {
        error!(
            "Failed to download {} files for mod {} after {} attempts",
            remaining.len(),
            batch.mod_id,
            MAX_FILE_RETRIES
        );
    }

    if let Some(handle) = progress_handle {
        progress_stop.store(true, Ordering::SeqCst);
        let _ = handle.await;
    }

    (slot_budget, batch, remaining.is_empty() && !cancelled)
}
