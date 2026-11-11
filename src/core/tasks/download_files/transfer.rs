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
    download_large_file_with_range_queue, range_chunk_size_for, range_part_meta_path,
    remove_range_meta,
};
use super::request::{cancellable, download_response};
use super::{ATTEMPT_DELAY_MS, ATTEMPT_LIMIT, BUFFERED_WRITE_CAPACITY, LARGE_FILE_THRESHOLD};
use crate::core::utils::http_range::validate_content_range_header;

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
    let base = ATTEMPT_DELAY_MS * (1 << attempt.min(3));
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
        if crate::core::utils::profiling::fs::exists(target_path) {
            let profiled = crate::core::utils::profiling::FsTimer::start();
            tokio::fs::remove_file(target_path).await?;
            profiled.stop("remove", 0);
        }
        let profiled = crate::core::utils::profiling::FsTimer::start();
        tokio::fs::rename(part_path, target_path).await?;
        profiled.stop("rename", 0);
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
    if crate::core::utils::profiling::fs::metadata_async(&meta_path)
        .await
        .is_ok()
    {
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
    if let Ok(meta) = crate::core::utils::profiling::fs::metadata_async(&part_path).await {
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
        let range = (bytes_received > 0).then_some((bytes_received, None));
        let resp = download_response(
            &context.client,
            url,
            range,
            &mut download_pause_rx,
            &mut cancel_rx,
            &metrics,
        )
        .await?;
        if !resp.status().is_success() {
            anyhow::bail!("GET request failed: {}", resp.status());
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
        if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            validate_content_range_header(
                &resp,
                bytes_received,
                (total_size as u64).saturating_sub(1),
            )?;
        }

        // On first attempt (or after resume reset) create the part file; on resume, append
        let profiled = crate::core::utils::profiling::FsTimer::start();
        let file = if bytes_received == 0 {
            tokio::fs::File::create(&part_path).await?
        } else {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&part_path)
                .await?
        };
        profiled.stop("create", 0);
        let mut writer = tokio::io::BufWriter::with_capacity(BUFFERED_WRITE_CAPACITY, file);
        let mut response = resp;
        loop {
            if *download_pause_rx.borrow() {
                wait_for_download_resume(&mut download_pause_rx, &mut cancel_rx).await?;
            }
            ensure_not_cancelled(&cancel_rx)?;

            let chunk_result = cancellable(
                &mut cancel_rx,
                timeout(Duration::from_secs(10), response.chunk()),
            )
            .await?;
            match chunk_result {
                Ok(Ok(Some(bytes))) => {
                    attempt = 0;
                    let n = bytes.len();
                    if n as u64 > (total_size as u64).saturating_sub(bytes_received) {
                        anyhow::bail!("Download response exceeds expected file size");
                    }
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
                    let profiled = crate::core::utils::profiling::FsTimer::start();
                    writer.write_all(&bytes).await?;
                    profiled.stop("write", n as u64);
                    crate::core::utils::file_io::note_written(n as u64);
                    disk_write_time = disk_write_time.saturating_add(write_started.elapsed());
                    disk_write_count = disk_write_count.saturating_add(1);
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    debug!("Simple download error for {}: {}", path, e);
                    break;
                }
                Err(_) => {
                    debug!("Simple download timeout for {}", path);
                    break;
                }
            }
        }

        let profiled = crate::core::utils::profiling::FsTimer::start();
        let flushed = writer.flush().await;
        profiled.stop("flush", 1);
        flushed?;

        if bytes_received == total_size as u64 {
            // sync_all() is intentionally omitted: the .part + rename pattern
            // provides crash safety (incomplete .part files are cleaned on restart),
            // and skipping fsync per file avoids significant I/O latency.
            writer.shutdown().await?;
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
        cancellable(&mut cancel_rx, sleep(delay)).await?;
        attempt += 1;
        metrics
            .counters
            .range_retries
            .fetch_add(1, Ordering::Relaxed);
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
        .div_ceil(range_chunk_size_for(
            total_size as u64,
            scheduler.limits.max_ranges_per_file,
            scheduler.limits.min_range_chunk as u64,
            scheduler.limits.range_chunk_target as u64,
        ) as usize)
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
    fn retry_delay_stays_bounded_after_many_failures() {
        for attempt in [0, 3, 31, 32, 49, usize::MAX] {
            let delay = jitter_delay(attempt);
            assert!(delay >= Duration::from_millis(ATTEMPT_DELAY_MS));
            assert!(delay < Duration::from_millis(ATTEMPT_DELAY_MS * 12));
        }
    }

    #[tokio::test]
    async fn missing_http_status_survives_simple_and_ranged_transfers() {
        use crate::core::tasks::download_files::failure::is_missing_remote_file;
        use std::sync::atomic::AtomicUsize;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/missing.bin", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let context = Arc::new(FoxyContext::new(
            crate::core::tasks::db_turso::build_test_database().await,
            reqwest::Client::new(),
        ));
        let (_pause_tx, pause_rx) = watch::channel(false);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        for size in [4, LARGE_FILE_THRESHOLD + 1] {
            let path = directory
                .path()
                .join(format!("missing-{size}.bin"))
                .to_string_lossy()
                .to_string();
            let target = DownloadTargetFile {
                file_id: 1,
                download_remote_url: Arc::from(url.as_str()),
                download_local_path: Arc::from(path.as_str()),
                size,
                expected_download_bytes: size,
                download_total: Arc::new(AtomicUsize::new(0)),
                download_cycle: Arc::new(AtomicUsize::new(0)),
            };
            let error = tokio::time::timeout(
                Duration::from_secs(5),
                download_file_ranges(
                    context.clone(),
                    &target,
                    &url,
                    &path,
                    Arc::new(AdaptiveBandwidthLimiter::from_mbps(None)),
                    pause_rx.clone(),
                    cancel_rx.clone(),
                    None,
                    true,
                    Arc::new(DownloadMetrics::new()),
                    Arc::new(DownloadSchedulerState::new(
                        super::super::DownloadResourceLimits::normal(),
                    )),
                ),
            )
            .await
            .unwrap()
            .err()
            .unwrap();
            assert!(is_missing_remote_file(&error));
            assert!(!is_download_cancelled_error(&error));
        }
        server.abort();
    }

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
