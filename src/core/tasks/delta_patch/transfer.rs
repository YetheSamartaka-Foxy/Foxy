use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::DownloadPatchFile;
use crate::core::models::download_patch_op::{DownloadPatchOp, update_download_patch_op_progress};
use crate::core::tasks::download_files::{AdaptiveBandwidthLimiter, DownloadMetrics};
use crate::core::utils::content_hash::FlexHasher;
use crate::core::utils::file_io::{read_at, write_at};
use crate::core::utils::http_range::validate_content_range_header;
use anyhow::{Context, anyhow};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use log::{debug, info, warn};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::watch;

use super::types::{
    COPY_BUFFER_SIZE, CopySourcePreflightStats, PATCH_CHUNK_TIMEOUT, PATCH_DOWNLOAD_MAX_RETRIES,
    PATCH_PREFLIGHT_COPY_SAMPLE_OPS, PatchArtifact, PatchOpType, checksum_matches,
    sampled_copy_op_indices,
};

fn cancellation_requested(cancel_rx: &watch::Receiver<bool>) -> bool {
    *cancel_rx.borrow()
}

fn ensure_not_cancelled(cancel_rx: &watch::Receiver<bool>) -> anyhow::Result<()> {
    if cancellation_requested(cancel_rx) {
        anyhow::bail!("download cancelled");
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
                    anyhow::bail!("download cancelled");
                }
            }
        }
    }
    ensure_not_cancelled(cancel_rx)
}

async fn request_exact_range(
    context: Arc<FoxyContext>,
    remote_url: &str,
    start: u64,
    end: u64,
) -> anyhow::Result<reqwest::Response> {
    let response = context
        .client
        .get(remote_url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .header(reqwest::header::RANGE, format!("bytes={}-{}", start, end))
        .send()
        .await
        .with_context(|| format!("range request {}-{} failed", start, end))?;

    if response.status() == reqwest::StatusCode::OK {
        return Err(anyhow!(
            "range request was ignored by server (HTTP 200 returned instead of 206)"
        ));
    }

    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(anyhow!(
            "range request {}-{} failed with HTTP {}",
            start,
            end,
            response.status()
        ));
    }

    validate_content_range_header(&response, start, end)?;
    Ok(response)
}

pub(super) async fn hash_file_segment(
    file: &mut tokio::fs::File,
    start: u64,
    length: u64,
    buffer: &mut Vec<u8>,
    expected_checksum: &str,
) -> anyhow::Result<String> {
    file.seek(SeekFrom::Start(start))
        .await
        .with_context(|| format!("failed to seek file segment start {}", start))?;

    buffer.resize(COPY_BUFFER_SIZE, 0);
    let mut hasher = FlexHasher::from_checksum(expected_checksum);
    let mut remaining = length;
    while remaining > 0 {
        let read_len = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..read_len])
            .await
            .with_context(|| {
                format!(
                    "failed to read file segment {} (remaining {})",
                    start, remaining
                )
            })?;
        hasher.update(&buffer[..read_len]);
        remaining -= read_len as u64;
    }
    Ok(hasher.finalize_hex())
}

pub(super) async fn copy_range_with_hash(
    source: &mut tokio::fs::File,
    source_start: u64,
    target: &mut tokio::io::BufWriter<tokio::fs::File>,
    target_start: u64,
    length: u64,
    buffer: &mut Vec<u8>,
    expected_checksum: &str,
) -> anyhow::Result<String> {
    source
        .seek(SeekFrom::Start(source_start))
        .await
        .with_context(|| format!("failed to seek source to {}", source_start))?;
    target
        .seek(SeekFrom::Start(target_start))
        .await
        .with_context(|| format!("failed to seek target to {}", target_start))?;

    buffer.resize(COPY_BUFFER_SIZE, 0);
    let mut hasher = FlexHasher::from_checksum(expected_checksum);
    let mut remaining = length;
    while remaining > 0 {
        let read_len = remaining.min(buffer.len() as u64) as usize;
        source
            .read_exact(&mut buffer[..read_len])
            .await
            .with_context(|| {
                format!(
                    "failed to read source range {}..{}",
                    source_start,
                    source_start.saturating_add(length)
                )
            })?;
        hasher.update(&buffer[..read_len]);
        target
            .write_all(&buffer[..read_len])
            .await
            .with_context(|| {
                format!(
                    "failed to write target range {}..{}",
                    target_start,
                    target_start.saturating_add(length)
                )
            })?;
        remaining -= read_len as u64;
    }
    Ok(hasher.finalize_hex())
}

pub(super) async fn preflight_copy_sources(
    local_target_path: &Path,
    patch_ops: &[DownloadPatchOp],
) -> anyhow::Result<CopySourcePreflightStats> {
    let mut stats = CopySourcePreflightStats::default();
    let copy_indices: Vec<usize> = patch_ops
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| (PatchOpType::CopyLocal.matches(op)).then_some(idx))
        .collect();

    if copy_indices.is_empty() {
        return Ok(stats);
    }

    stats.copy_ops_total = copy_indices.len();
    stats.copy_bytes_total = copy_indices
        .iter()
        .map(|idx| patch_ops[*idx].length)
        .sum::<u64>();

    let sampled_indices = sampled_copy_op_indices(&copy_indices, PATCH_PREFLIGHT_COPY_SAMPLE_OPS);
    if sampled_indices.is_empty() {
        return Ok(stats);
    }

    let local_meta = fs::metadata(local_target_path).await.with_context(|| {
        format!(
            "failed to stat local patch source file {}",
            local_target_path.display()
        )
    })?;
    let local_len = local_meta.len();

    let mut local_file = OpenOptions::new()
        .read(true)
        .open(local_target_path)
        .await
        .with_context(|| {
            format!(
                "failed to open local patch source file {}",
                local_target_path.display()
            )
        })?;

    let mut io_buf = Vec::new();
    for idx in sampled_indices {
        let op = &patch_ops[idx];
        stats.checked_ops = stats.checked_ops.saturating_add(1);
        stats.checked_bytes = stats.checked_bytes.saturating_add(op.length);

        let source_start = match op.source_start {
            Some(value) => value,
            None => {
                stats.mismatch_ops = stats.mismatch_ops.saturating_add(1);
                stats.mismatch_bytes = stats.mismatch_bytes.saturating_add(op.length);
                continue;
            }
        };
        let source_checksum = match op.source_checksum.as_ref() {
            Some(value) => value,
            None => {
                stats.mismatch_ops = stats.mismatch_ops.saturating_add(1);
                stats.mismatch_bytes = stats.mismatch_bytes.saturating_add(op.length);
                continue;
            }
        };

        let source_end = match source_start.checked_add(op.length) {
            Some(value) => value,
            None => {
                stats.mismatch_ops = stats.mismatch_ops.saturating_add(1);
                stats.mismatch_bytes = stats.mismatch_bytes.saturating_add(op.length);
                warn!(
                    "Delta preflight source overflow: file_id={} op={} source_start={} length={}",
                    op.file_id, op.data_order, source_start, op.length
                );
                continue;
            }
        };
        if source_end > local_len {
            stats.mismatch_ops = stats.mismatch_ops.saturating_add(1);
            stats.mismatch_bytes = stats.mismatch_bytes.saturating_add(op.length);
            warn!(
                "Delta preflight source out of bounds: file_id={} op={} source_start={} length={} local_len={}",
                op.file_id, op.data_order, source_start, op.length, local_len
            );
            continue;
        }

        let actual = match hash_file_segment(
            &mut local_file,
            source_start,
            op.length,
            &mut io_buf,
            source_checksum,
        )
        .await
        {
            Ok(value) => value,
            Err(err) => {
                stats.mismatch_ops = stats.mismatch_ops.saturating_add(1);
                stats.mismatch_bytes = stats.mismatch_bytes.saturating_add(op.length);
                warn!(
                    "Delta preflight source read failed: file_id={} op={} source_start={} length={} error={}",
                    op.file_id, op.data_order, source_start, op.length, err
                );
                continue;
            }
        };

        if !checksum_matches(source_checksum, &actual) {
            stats.mismatch_ops = stats.mismatch_ops.saturating_add(1);
            stats.mismatch_bytes = stats.mismatch_bytes.saturating_add(op.length);
            warn!(
                "Delta preflight source checksum mismatch: file_id={} op={} source_start={} length={} expected={} actual={}",
                op.file_id, op.data_order, source_start, op.length, source_checksum, actual
            );
        }
    }

    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn download_range_to_output(
    context: Arc<FoxyContext>,
    remote_url: &str,
    dest_start: u64,
    length: u64,
    target_checksum: &str,
    output_file: &mut tokio::io::BufWriter<tokio::fs::File>,
    download_pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    if length == 0 {
        return Ok(());
    }
    let range_end = dest_start
        .checked_add(length)
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| anyhow!("range overflow while downloading fallback segment"))?;

    let mut last_error: Option<anyhow::Error> = None;
    for attempt in 0..=PATCH_DOWNLOAD_MAX_RETRIES {
        if attempt > 0 {
            let delay = Duration::from_millis(200 * (1 << (attempt - 1).min(3)));
            warn!(
                "Retrying fallback range download {}-{} (attempt {}/{})",
                dest_start,
                range_end,
                attempt + 1,
                PATCH_DOWNLOAD_MAX_RETRIES + 1
            );
            tokio::time::sleep(delay).await;
        }

        match download_range_to_output_once(
            context.clone(),
            remote_url,
            dest_start,
            length,
            target_checksum,
            output_file,
            download_pause_rx,
            cancel_rx,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(err) => {
                warn!(
                    "Fallback range download {}-{} attempt {} failed: {}",
                    dest_start,
                    range_end,
                    attempt + 1,
                    err
                );
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("fallback range download failed after retries")))
}

#[allow(clippy::too_many_arguments)]
async fn download_range_to_output_once(
    context: Arc<FoxyContext>,
    remote_url: &str,
    dest_start: u64,
    length: u64,
    target_checksum: &str,
    output_file: &mut tokio::io::BufWriter<tokio::fs::File>,
    download_pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    if length == 0 {
        return Ok(());
    }
    let range_end = dest_start + length - 1;
    let mut response = request_exact_range(context, remote_url, dest_start, range_end).await?;

    // Seek to correct position - critical for retries after partial writes
    output_file
        .seek(SeekFrom::Start(dest_start))
        .await
        .with_context(|| format!("failed to seek output file to {}", dest_start))?;

    let mut hasher = FlexHasher::from_checksum(target_checksum);
    let mut written = 0_u64;
    while let Some(chunk) = {
        wait_for_download_resume(download_pause_rx, cancel_rx).await?;
        tokio::time::timeout(PATCH_CHUNK_TIMEOUT, response.chunk())
            .await
            .map_err(|_| anyhow!("delta fallback range chunk read timed out"))?
            .context("failed to read range chunk")?
    } {
        output_file
            .write_all(&chunk)
            .await
            .context("failed to write fallback range chunk")?;
        hasher.update(&chunk);
        written = written.saturating_add(chunk.len() as u64);
    }

    if written != length {
        return Err(anyhow!(
            "fallback range download length mismatch: expected {}, got {}",
            length,
            written
        ));
    }

    let actual_checksum = hasher.finalize_hex();
    if !checksum_matches(target_checksum, &actual_checksum) {
        return Err(anyhow!(
            "fallback range checksum mismatch (expected {}, got {})",
            target_checksum,
            actual_checksum
        ));
    }

    Ok(())
}

/// Download delta patch insert-ops concurrently into non-overlapping blob offsets.
///
/// Each InsertRemote op targets a unique `(blob_offset, length)` region, so they
/// can safely be written in parallel using random-access writes. This function
/// limits concurrency to `max_concurrent` tasks.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn download_patch_blob_ranges_parallel(
    context: Arc<FoxyContext>,
    artifact: &PatchArtifact,
    patch_file: &DownloadPatchFile,
    patch_ops: &mut [DownloadPatchOp],
    download_pause_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
    max_concurrent: usize,
    rate_limiter: Arc<AdaptiveBandwidthLimiter>,
    metrics: Arc<DownloadMetrics>,
) -> anyhow::Result<()> {
    let blob_path = Path::new(&patch_file.patch_blob_path);
    if !blob_path.exists() {
        return Err(anyhow!(
            "patch blob file does not exist: {}",
            blob_path.display()
        ));
    }

    // Collect indices of InsertRemote ops for parallel processing
    let insert_indices: Vec<usize> = patch_ops
        .iter()
        .enumerate()
        .filter(|(_, op)| PatchOpType::from_str(&op.op_type) == Some(PatchOpType::InsertRemote))
        .map(|(i, _)| i)
        .collect();

    if insert_indices.is_empty() {
        return Ok(());
    }

    // Open the blob file for random-access writing (std::fs::File for write_at)
    let blob_file = Arc::new(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(blob_path)
            .with_context(|| {
                format!(
                    "failed to open blob for parallel write: {}",
                    blob_path.display()
                )
            })?,
    );

    let blob_download_started = std::time::Instant::now();
    let concurrency_semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

    // Snapshot op data needed per task (avoids borrowing patch_ops across spawn)
    struct InsertOpSnapshot {
        index: usize,
        file_id: i64,
        data_order: i64,
        dest_start: u64,
        length: u64,
        blob_offset: u64,
        target_checksum: String,
    }

    let snapshots: Vec<InsertOpSnapshot> = insert_indices
        .iter()
        .filter_map(|&i| {
            let op = &patch_ops[i];
            let blob_offset = op.blob_offset?;
            Some(InsertOpSnapshot {
                index: i,
                file_id: op.file_id as i64,
                data_order: op.data_order,
                dest_start: op.dest_start,
                length: op.length,
                blob_offset,
                target_checksum: op.target_checksum.clone(),
            })
        })
        .collect();

    let remote_url: Arc<str> = Arc::from(artifact.remote_url.as_str());
    let mut tasks = FuturesUnordered::new();

    for snap in &snapshots {
        let ctx = context.clone();
        let sem = concurrency_semaphore.clone();
        let blob = blob_file.clone();
        let url = remote_url.clone();
        let pause_rx = download_pause_rx.clone();
        let cancel_rx = cancel_rx.clone();
        let limiter = rate_limiter.clone();
        let task_metrics = metrics.clone();
        let file_id = snap.file_id;
        let data_order = snap.data_order;
        let dest_start = snap.dest_start;
        let length = snap.length;
        let blob_offset = snap.blob_offset;
        let target_checksum = snap.target_checksum.clone();
        let index = snap.index;

        tasks.push(tokio::spawn(async move {
            let _permit = sem
                .acquire_owned()
                .await
                .map_err(|_| anyhow!("patch concurrency semaphore closed"))?;

            download_single_insert_op(
                ctx,
                &url,
                blob,
                file_id,
                data_order,
                dest_start,
                length,
                blob_offset,
                &target_checksum,
                pause_rx,
                cancel_rx,
                limiter,
                task_metrics,
            )
            .await
            .map(|bytes| (index, bytes, data_order))
        }));
    }

    let mut total_bytes = 0u64;
    let mut ops_completed = 0usize;
    let mut first_error: Option<anyhow::Error> = None;

    while let Some(result) = tasks.next().await {
        match result {
            Ok(Ok((index, bytes, _data_order))) => {
                // Update the op in place with completion state
                patch_ops[index].downloaded_bytes = patch_ops[index].length;
                total_bytes += bytes;
                ops_completed += 1;
            }
            Ok(Err(err)) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
            Err(join_err) => {
                if first_error.is_none() {
                    first_error = Some(anyhow!("patch insert task panicked: {}", join_err));
                }
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    // Batch-persist all completed op progress
    for snap in &snapshots {
        let _ = update_download_patch_op_progress(
            context.clone(),
            snap.file_id,
            snap.data_order,
            patch_ops[snap.index].downloaded_bytes,
            patch_ops[snap.index].retry_count,
        )
        .await;
    }

    if ops_completed > 0 {
        let elapsed = blob_download_started.elapsed();
        let speed = if elapsed.as_secs_f64() > 0.0 {
            (total_bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
        } else {
            0.0
        };
        info!(
            "Parallel delta blob download: file_id={} ops={} bytes={} elapsed={:.2?} speed={:.2} MB/s",
            artifact.file_id, ops_completed, total_bytes, elapsed, speed
        );
    }

    Ok(())
}

/// Download a single InsertRemote op into the blob file at the correct offset.
#[allow(clippy::too_many_arguments)]
async fn download_single_insert_op(
    context: Arc<FoxyContext>,
    remote_url: &str,
    blob_file: Arc<std::fs::File>,
    file_id: i64,
    data_order: i64,
    dest_start: u64,
    length: u64,
    blob_offset: u64,
    target_checksum: &str,
    mut pause_rx: watch::Receiver<bool>,
    mut cancel_rx: watch::Receiver<bool>,
    rate_limiter: Arc<AdaptiveBandwidthLimiter>,
    metrics: Arc<DownloadMetrics>,
) -> anyhow::Result<u64> {
    let request_start = dest_start;
    let request_end = dest_start
        .checked_add(length)
        .and_then(|v| v.checked_sub(1))
        .ok_or_else(|| anyhow!("insert op range overflow"))?;

    let mut retry_count = 0u32;
    let mut downloaded = 0u64;

    while downloaded < length {
        wait_for_download_resume(&mut pause_rx, &mut cancel_rx).await?;

        let range_start = request_start.saturating_add(downloaded);
        let response = match request_exact_range(
            context.clone(),
            remote_url,
            range_start,
            request_end,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                retry_count += 1;
                if retry_count > PATCH_DOWNLOAD_MAX_RETRIES {
                    return Err(err)
                        .context(format!("insert op {} exceeded retry limit", data_order));
                }
                warn!(
                    "Delta parallel range request failed: file_id={} op={} retries={} error={}",
                    file_id, data_order, retry_count, err
                );
                continue;
            }
        };

        let mut resp = response;
        loop {
            wait_for_download_resume(&mut pause_rx, &mut cancel_rx).await?;
            let chunk = match tokio::time::timeout(PATCH_CHUNK_TIMEOUT, resp.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => return Err(err).context("failed to read parallel blob chunk"),
                Err(_) => {
                    retry_count += 1;
                    if retry_count > PATCH_DOWNLOAD_MAX_RETRIES {
                        return Err(anyhow!(
                            "delta parallel blob chunk timed out after {} retries",
                            PATCH_DOWNLOAD_MAX_RETRIES
                        ));
                    }
                    warn!(
                        "Delta parallel blob chunk timed out: file_id={} op={} downloaded={}/{} retries={}",
                        file_id, data_order, downloaded, length, retry_count
                    );
                    break;
                }
            };
            let n = chunk.len();
            rate_limiter.acquire_and_record(n).await;
            metrics.record_bytes(n as u64);
            let write_offset = blob_offset.saturating_add(downloaded);
            let chunk_data = chunk.to_vec();
            let file = blob_file.clone();
            tokio::task::spawn_blocking(move || write_at(&file, write_offset, &chunk_data))
                .await??;
            downloaded += n as u64;
            retry_count = 0;
            if downloaded > length {
                return Err(anyhow!(
                    "parallel insert op {} wrote beyond planned length",
                    data_order
                ));
            }
        }
    }

    // Verify checksum by reading back from blob
    let blob_file_verify = blob_file.clone();
    let verify_offset = blob_offset;
    let verify_length = length as usize;
    let checksum_expected = target_checksum.to_string();
    let actual_checksum = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let mut hasher = FlexHasher::from_checksum(&checksum_expected);
        let mut buf = vec![0u8; (64 * 1024).min(verify_length)];
        let mut remaining = verify_length;
        let mut offset = verify_offset;
        while remaining > 0 {
            let to_read = remaining.min(buf.len());
            read_at(&blob_file_verify, offset, &mut buf[..to_read])?;
            hasher.update(&buf[..to_read]);
            offset += to_read as u64;
            remaining -= to_read;
        }
        Ok(hasher.finalize_hex())
    })
    .await??;

    if !checksum_matches(target_checksum, &actual_checksum) {
        return Err(anyhow!(
            "parallel insert op {} checksum mismatch (expected {}, got {})",
            data_order,
            target_checksum,
            actual_checksum
        ));
    }

    debug!(
        "Parallel delta insert op complete: file_id={} op={} bytes={}",
        file_id, data_order, downloaded
    );

    Ok(downloaded)
}
