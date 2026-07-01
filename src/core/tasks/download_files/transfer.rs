use crate::core::models::context::FoxyContext;
use crate::core::models::download_target_file::DownloadTargetFile;
use anyhow::anyhow;
use log::{debug, trace};
use rand::{RngExt, rng};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

use super::SharedRollbackSession;
use super::bandwidth::AdaptiveBandwidthLimiter;
use super::metrics::{DownloadMetrics, DownloadSchedulerState};
use super::range_scheduler::{
    download_large_file_with_range_queue, range_part_meta_path, remove_range_meta,
};
use super::{ATTEMPT_DELAY_MS, ATTEMPT_LIMIT, BUFFERED_WRITE_CAPACITY, LARGE_FILE_THRESHOLD};

pub(super) const DOWNLOAD_CANCELLED_MESSAGE: &str = "download cancelled";

/// Transfer-layer stats returned after a file download completes.
/// Consumed by the batching layer to populate `FileMetric`.
pub(super) struct TransferStats {
    pub(super) split_count: usize,
    pub(super) disk_write_time: Duration,
    pub(super) disk_write_count: usize,
    pub(super) promote_time: Duration,
}

pub(super) fn jitter_delay(attempt: usize) -> Duration {
    let base = ATTEMPT_DELAY_MS * (1 << attempt).min(8); // exponential backoff capped at ~3.5s
    let jitter: u64 = rng().random_range(0..base / 2);
    Duration::from_millis(base + jitter)
}

pub(super) fn cancellation_requested(cancel_rx: &watch::Receiver<bool>) -> bool {
    *cancel_rx.borrow()
}

pub(super) fn download_cancelled_error() -> anyhow::Error {
    anyhow!(DOWNLOAD_CANCELLED_MESSAGE)
}

pub(super) fn is_download_cancelled_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string() == DOWNLOAD_CANCELLED_MESSAGE)
}

pub(super) fn ensure_not_cancelled(cancel_rx: &watch::Receiver<bool>) -> anyhow::Result<()> {
    if cancellation_requested(cancel_rx) {
        return Err(download_cancelled_error());
    }
    Ok(())
}

pub(super) async fn wait_for_download_resume(
    download_pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    while *download_pause_rx.borrow() {
        ensure_not_cancelled(cancel_rx)?;
        tokio::select! {
            changed = download_pause_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            changed = cancel_rx.changed() => {
                if changed.is_err() || cancellation_requested(cancel_rx) {
                    return Err(download_cancelled_error());
                }
            }
        }
    }
    ensure_not_cancelled(cancel_rx)
}

async fn promote_part_file(
    rollback_session: Option<SharedRollbackSession>,
    file_id: u64,
    part_path: &str,
    target_path: &str,
) -> anyhow::Result<()> {
    let part_path = Path::new(part_path);
    let target_path = Path::new(target_path);
    if let Some(session) = rollback_session {
        let mut rollback = session.lock().await;
        rollback.promote_file(file_id, part_path, target_path).await
    } else {
        #[cfg(target_os = "windows")]
        if target_path.exists() {
            tokio::fs::remove_file(target_path).await?;
        }
        tokio::fs::rename(part_path, target_path).await?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_file_simple(
    context: Arc<FoxyContext>,
    download_target: &DownloadTargetFile,
    url: &str,
    path: &str,
    limiter: Arc<AdaptiveBandwidthLimiter>,
    mut download_pause_rx: watch::Receiver<bool>,
    mut cancel_rx: watch::Receiver<bool>,
    rollback_session: Option<SharedRollbackSession>,
    metrics: Arc<DownloadMetrics>,
) -> anyhow::Result<TransferStats> {
    ensure_not_cancelled(&cancel_rx)?;
    let parent_path = Path::new(path);
    if let Some(parent) = parent_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let part_path = format!("{}.foxy.part", path);
    let total_size = download_target.size;
    let mut bytes_received = 0_u64;
    let mut attempt = 0u8;
    let mut disk_write_time = Duration::ZERO;
    let mut disk_write_count = 0usize;

    // A resume sidecar marks the part file as a ranged pre-allocation: its
    // length does not reflect sequential progress, so it cannot be appended
    // to. Discard both and start fresh.
    let meta_path = range_part_meta_path(path);
    if tokio::fs::metadata(&meta_path).await.is_ok() {
        match tokio::fs::remove_file(&part_path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
        remove_range_meta(&meta_path).await;
        download_target.download_total.store(0, Ordering::SeqCst);
        download_target.download_cycle.store(0, Ordering::SeqCst);
    }

    // Check for existing .part file from a previous session to resume from
    if let Ok(meta) = tokio::fs::metadata(&part_path).await {
        let part_size = meta.len();
        if part_size > 0 && part_size < total_size as u64 {
            bytes_received = part_size;
            download_target
                .download_total
                .store(bytes_received as usize, Ordering::SeqCst);
            download_target.download_cycle.store(0, Ordering::SeqCst);
            trace!(
                "Resuming simple download {} from {} bytes (part file exists)",
                path, bytes_received
            );
        } else if part_size == total_size as u64 {
            ensure_not_cancelled(&cancel_rx)?;
            let promote_started = std::time::Instant::now();
            promote_part_file(rollback_session, download_target.file_id, &part_path, path).await?;
            download_target
                .download_total
                .store(total_size, Ordering::SeqCst);
            download_target.download_cycle.store(0, Ordering::SeqCst);
            return Ok(TransferStats {
                split_count: 1,
                disk_write_time,
                disk_write_count,
                promote_time: promote_started.elapsed(),
            });
        } else if part_size > total_size as u64 {
            tokio::fs::remove_file(&part_path).await?;
        }
    }

    loop {
        ensure_not_cancelled(&cancel_rx)?;
        // On first attempt or retry, open a (possibly Range) request.
        // Use identity encoding - binary PBO/PAA payloads are incompressible
        // and compression would make Range byte offsets ambiguous.
        let resp = if bytes_received == 0 {
            context
                .client
                .get(url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send()
                .await?
        } else {
            context
                .client
                .get(url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .header("Range", format!("bytes={}-", bytes_received))
                .send()
                .await?
        };
        if !resp.status().is_success() {
            anyhow::bail!("GET request for {} failed: {}", path, resp.status());
        }

        // If we asked for a Range but got 200 (not 206), the server doesn't
        // support resume - discard the partial data and start fresh.
        if bytes_received > 0 && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            trace!(
                "Server returned {} instead of 206 for resume of {} - restarting from scratch",
                resp.status(),
                path
            );
            bytes_received = 0;
            download_target.download_total.store(0, Ordering::SeqCst);
            download_target.download_cycle.store(0, Ordering::SeqCst);
        }

        // On first attempt (or after resume reset) create the part file; on resume, append
        let file = if bytes_received == 0 {
            tokio::fs::File::create(&part_path).await?
        } else {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .await?
        };
        let mut writer = tokio::io::BufWriter::with_capacity(BUFFERED_WRITE_CAPACITY, file);
        let mut response = resp;
        let mut stream_failed = false;

        loop {
            if *download_pause_rx.borrow() {
                wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx).await?;
            }
            ensure_not_cancelled(&cancel_rx)?;

            let chunk_result = timeout(Duration::from_secs(10), response.chunk()).await;
            match chunk_result {
                Ok(Ok(Some(bytes))) => {
                    attempt = 0;
                    let n = bytes.len();
                    bytes_received += n as u64;
                    limiter.acquire_and_record(n).await;
                    metrics.record_bytes(n as u64);
                    download_target
                        .download_cycle
                        .fetch_add(n, Ordering::Relaxed);
                    download_target
                        .download_total
                        .fetch_add(n, Ordering::Relaxed);
                    let write_started = std::time::Instant::now();
                    writer.write_all(&bytes).await?;
                    disk_write_time = disk_write_time.saturating_add(write_started.elapsed());
                    disk_write_count = disk_write_count.saturating_add(1);
                }
                Ok(Ok(None)) => break, // stream complete
                Ok(Err(e)) => {
                    debug!("Simple download error for {}: {}", path, e);
                    stream_failed = true;
                    break;
                }
                Err(_) => {
                    debug!("Simple download timeout for {}", path);
                    stream_failed = true;
                    break;
                }
            }
        }

        writer.flush().await?;

        if !stream_failed {
            // Stream ended normally - verify size and promote .part file.
            // sync_all() is intentionally omitted: the .part + rename pattern
            // provides crash safety (incomplete .part files are cleaned on restart),
            // and skipping fsync per file avoids significant I/O latency.
            writer.shutdown().await?;
            if bytes_received != total_size as u64 {
                anyhow::bail!(
                    "Download size mismatch for {}: expected {} bytes, received {}",
                    path,
                    total_size,
                    bytes_received
                );
            }
            // Atomically promote part file to final path
            ensure_not_cancelled(&cancel_rx)?;
            let promote_started = std::time::Instant::now();
            promote_part_file(rollback_session, download_target.file_id, &part_path, path).await?;
            return Ok(TransferStats {
                split_count: 1,
                disk_write_time,
                disk_write_count,
                promote_time: promote_started.elapsed(),
            });
        }

        // Stream failed - retry with resume
        if attempt >= ATTEMPT_LIMIT {
            anyhow::bail!(
                "Exceeded {} retry attempts for simple download: {}",
                ATTEMPT_LIMIT,
                path
            );
        }
        let delay = jitter_delay(attempt as usize);
        trace!(
            "Retrying simple download {} in {:?} (received {} bytes)",
            path, delay, bytes_received
        );
        sleep(delay).await;
        attempt += 1;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn download_file_ranges(
    context: Arc<FoxyContext>,
    download_target: &DownloadTargetFile,
    url: &str,
    path: &str,
    limiter: Arc<AdaptiveBandwidthLimiter>,
    download_pause_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
    rollback_session: Option<SharedRollbackSession>,
    supports_range: bool,
    metrics: Arc<DownloadMetrics>,
    scheduler: Arc<DownloadSchedulerState>,
) -> anyhow::Result<TransferStats> {
    ensure_not_cancelled(&cancel_rx)?;
    let parent_path = Path::new(path);

    if let Some(parent) = parent_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let total_size = download_target.size;

    // Small files - and any file when the server lacks range support - use the
    // appendable sequential path, which resumes from the .foxy.part length.
    if !supports_range || total_size <= LARGE_FILE_THRESHOLD {
        return download_file_simple(
            context,
            download_target,
            url,
            path,
            limiter,
            download_pause_rx,
            cancel_rx,
            rollback_session,
            metrics,
        )
        .await;
    }

    // Large files use the resumable range work queue: parallel range requests
    // with per-file concurrency that grows as the global queue drains, and
    // completed chunks tracked in a .foxy.part.meta sidecar so interrupted
    // downloads resume in the next session.
    let split_count = total_size
        .div_ceil(scheduler.limits.range_chunk_target)
        .max(1);
    let part_path = format!("{}.foxy.part", path);
    let bytes = download_large_file_with_range_queue(
        context,
        download_target.file_id,
        Arc::clone(&download_target.download_remote_url),
        Arc::clone(&download_target.download_local_path),
        total_size,
        download_target.download_total.clone(),
        download_target.download_cycle.clone(),
        limiter,
        scheduler,
        metrics,
        download_pause_rx,
        cancel_rx.clone(),
    )
    .await?;

    if bytes != total_size as u64 {
        anyhow::bail!(
            "Download size mismatch for {}: expected {} bytes, received {}",
            path,
            total_size,
            bytes
        );
    }

    ensure_not_cancelled(&cancel_rx)?;
    let promote_started = std::time::Instant::now();
    promote_part_file(rollback_session, download_target.file_id, &part_path, path).await?;
    Ok(TransferStats {
        split_count,
        disk_write_time: Duration::ZERO,
        disk_write_count: 0,
        promote_time: promote_started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_download_cancelled_error() {
        let err = download_cancelled_error();
        assert!(is_download_cancelled_error(&err));
    }

    #[test]
    fn does_not_classify_unrelated_error_as_cancelled() {
        let err = anyhow!("network timeout");
        assert!(!is_download_cancelled_error(&err));
    }

    #[test]
    fn classifies_context_wrapped_download_cancelled_error() {
        let err = download_cancelled_error().context("waiting for resume");
        assert!(is_download_cancelled_error(&err));
    }

    // ── download_cancelled_error ───────────────────────────────────────

    #[test]
    fn download_cancelled_error_message_matches_constant() {
        let err = download_cancelled_error();
        assert_eq!(err.to_string(), DOWNLOAD_CANCELLED_MESSAGE);
    }

    // ── ensure_not_cancelled ───────────────────────────────────────────

    #[test]
    fn ensure_not_cancelled_ok_when_not_cancelled() {
        let (_tx, rx) = watch::channel(false);
        assert!(ensure_not_cancelled(&rx).is_ok());
    }

    #[test]
    fn ensure_not_cancelled_err_when_cancelled() {
        let (_tx, rx) = watch::channel(true);
        let result = ensure_not_cancelled(&rx);
        assert!(result.is_err());
        assert!(is_download_cancelled_error(&result.unwrap_err()));
    }

    // ── cancellation_requested ─────────────────────────────────────────

    #[test]
    fn cancellation_requested_false_when_not_set() {
        let (_tx, rx) = watch::channel(false);
        assert!(!cancellation_requested(&rx));
    }

    #[test]
    fn cancellation_requested_true_when_set() {
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        assert!(cancellation_requested(&rx));
    }
}
