use crate::core::models::context::FoxyContext;
use crate::core::models::download_patch_file::DownloadPatchFile;
use crate::core::models::download_patch_op::{DownloadPatchOp, update_download_patch_op_progress};
use crate::core::tasks::download_files::{AdaptiveBandwidthLimiter, DownloadMetrics};
use crate::core::utils::content_hash::FlexHasher;
use crate::core::utils::file_io::write_at;
use crate::core::utils::http_range::validate_content_range_header;
use anyhow::{Context, anyhow};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use log::{debug, info, warn};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::watch;

use super::types::{
    COPY_BUFFER_SIZE, CopySourcePreflightStats, INSERT_RUN_MAX_BYTES, InsertRun,
    PATCH_CHUNK_TIMEOUT, PATCH_DOWNLOAD_MAX_RETRIES, PATCH_PREFLIGHT_COPY_SAMPLE_OPS,
    PatchArtifact, PatchOpType, checksum_matches, insert_run_gap_budget, plan_insert_runs,
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
    output_file: Arc<std::fs::File>,
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
            output_file.clone(),
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

/// Network chunks are buffered up to this size before one positional write,
/// so a fallback range does not cost a blocking-pool hop per TCP segment.
const RANGE_WRITE_BUFFER_SIZE: usize = 1024 * 1024;

async fn write_buffered_range(
    output_file: &Arc<std::fs::File>,
    offset: u64,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    let file = output_file.clone();
    tokio::task::spawn_blocking(move || write_at(&file, offset, &data))
        .await
        .context("failed to join fallback range write")?
        .context("failed to write fallback range chunk")
}

#[allow(clippy::too_many_arguments)]
async fn download_range_to_output_once(
    context: Arc<FoxyContext>,
    remote_url: &str,
    dest_start: u64,
    length: u64,
    target_checksum: &str,
    output_file: Arc<std::fs::File>,
    download_pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<()> {
    if length == 0 {
        return Ok(());
    }
    let range_end = dest_start + length - 1;
    let mut response = request_exact_range(context, remote_url, dest_start, range_end).await?;

    let mut hasher = FlexHasher::from_checksum(target_checksum);
    let mut written = 0_u64;
    let mut pending: Vec<u8> = Vec::with_capacity(RANGE_WRITE_BUFFER_SIZE);
    let mut pending_offset = dest_start;
    while let Some(chunk) = {
        wait_for_download_resume(download_pause_rx, cancel_rx).await?;
        tokio::time::timeout(PATCH_CHUNK_TIMEOUT, response.chunk())
            .await
            .map_err(|_| anyhow!("delta fallback range chunk read timed out"))?
            .context("failed to read range chunk")?
    } {
        hasher.update(&chunk);
        written = written.saturating_add(chunk.len() as u64);
        if written > length {
            return Err(anyhow!(
                "fallback range download length mismatch: expected {}, got at least {}",
                length,
                written
            ));
        }
        pending.extend_from_slice(&chunk);
        if pending.len() >= RANGE_WRITE_BUFFER_SIZE {
            let flushed = std::mem::take(&mut pending);
            let flushed_len = flushed.len() as u64;
            write_buffered_range(&output_file, pending_offset, flushed).await?;
            pending_offset = pending_offset.saturating_add(flushed_len);
        }
    }
    write_buffered_range(&output_file, pending_offset, pending).await?;

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

    let gap_budget = insert_run_gap_budget(metrics.peak_network_bps());
    let runs = plan_insert_runs(patch_ops, gap_budget, INSERT_RUN_MAX_BYTES);
    if runs.is_empty() {
        return Ok(());
    }
    let insert_ops: usize = runs.iter().map(|run| run.op_indices.len()).sum();

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

    let ops_shared: Arc<Vec<InsertOpSnapshot>> = Arc::new(
        patch_ops
            .iter()
            .map(|op| InsertOpSnapshot {
                file_id: op.file_id as i64,
                data_order: op.data_order,
                dest_start: op.dest_start,
                length: op.length,
                blob_offset: op.blob_offset.unwrap_or(0),
                target_checksum: op.target_checksum.clone(),
            })
            .collect(),
    );

    let remote_url: Arc<str> = Arc::from(artifact.remote_url.as_str());
    let mut tasks = FuturesUnordered::new();

    for run in runs {
        let ctx = context.clone();
        let sem = concurrency_semaphore.clone();
        let blob = blob_file.clone();
        let url = remote_url.clone();
        let pause_rx = download_pause_rx.clone();
        let cancel_rx = cancel_rx.clone();
        let limiter = rate_limiter.clone();
        let task_metrics = metrics.clone();
        let ops = ops_shared.clone();

        tasks.push(tokio::spawn(async move {
            let _permit = sem
                .acquire_owned()
                .await
                .map_err(|_| anyhow!("patch concurrency semaphore closed"))?;

            download_insert_run(
                ctx,
                &url,
                blob,
                &ops,
                &run,
                pause_rx,
                cancel_rx,
                limiter,
                task_metrics,
            )
            .await
            .map(|bytes| (run, bytes))
        }));
    }

    let mut total_bytes = 0u64;
    let mut gap_bytes = 0u64;
    let mut ops_completed = 0usize;
    let mut runs_completed = 0usize;
    let mut first_error: Option<anyhow::Error> = None;

    while let Some(result) = tasks.next().await {
        match result {
            Ok(Ok((run, bytes))) => {
                for &index in &run.op_indices {
                    patch_ops[index].downloaded_bytes = patch_ops[index].length;
                }
                gap_bytes += run.request_len().saturating_sub(bytes);
                total_bytes += bytes;
                ops_completed += run.op_indices.len();
                runs_completed += 1;
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
    for op in patch_ops
        .iter()
        .filter(|op| PatchOpType::from_str(&op.op_type) == Some(PatchOpType::InsertRemote))
    {
        let _ = update_download_patch_op_progress(
            context.clone(),
            op.file_id as i64,
            op.data_order,
            op.downloaded_bytes,
            op.retry_count,
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
            "Parallel delta blob download: file_id={} ops={}/{} requests={} bytes={} gap_bytes={} gap_budget={} elapsed={:.2?} speed={:.2} MB/s",
            artifact.file_id,
            ops_completed,
            insert_ops,
            runs_completed,
            total_bytes,
            gap_bytes,
            gap_budget,
            elapsed,
            speed
        );
    }

    Ok(())
}

/// The fields of an op the insert-run downloader needs, snapshotted so the
/// spawned tasks do not borrow `patch_ops`.
struct InsertOpSnapshot {
    file_id: i64,
    data_order: i64,
    dest_start: u64,
    length: u64,
    blob_offset: u64,
    target_checksum: String,
}

/// Output bytes buffered per op before one positional write into the blob.
const INSERT_RUN_WRITE_BUFFER: usize = 1024 * 1024;

/// Fetch one insert run with a single range request, splitting the stream
/// into its ops as it arrives: each op's bytes are hashed and written at its
/// blob offset, bytes between ops are dropped. A mismatch or a stalled chunk
/// restarts from the op that was in flight. Returns the op bytes written.
#[allow(clippy::too_many_arguments)]
async fn download_insert_run(
    context: Arc<FoxyContext>,
    remote_url: &str,
    blob_file: Arc<std::fs::File>,
    ops: &[InsertOpSnapshot],
    run: &InsertRun,
    mut pause_rx: watch::Receiver<bool>,
    mut cancel_rx: watch::Receiver<bool>,
    rate_limiter: Arc<AdaptiveBandwidthLimiter>,
    metrics: Arc<DownloadMetrics>,
) -> anyhow::Result<u64> {
    let file_id = run
        .op_indices
        .first()
        .map(|&idx| ops[idx].file_id)
        .unwrap_or_default();
    let mut retry_count = 0u32;
    let mut next_op = 0usize;
    let mut written_total = 0u64;

    while next_op < run.op_indices.len() {
        wait_for_download_resume(&mut pause_rx, &mut cancel_rx).await?;
        let first = &ops[run.op_indices[next_op]];
        let response = match request_exact_range(
            context.clone(),
            remote_url,
            first.dest_start,
            run.request_end,
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                retry_count += 1;
                if retry_count > PATCH_DOWNLOAD_MAX_RETRIES {
                    return Err(err).context(format!(
                        "insert run starting at op {} exceeded retry limit",
                        first.data_order
                    ));
                }
                warn!(
                    "Delta insert run request failed: file_id={} first_op={} ops={} retries={} error={}",
                    file_id,
                    first.data_order,
                    run.op_indices.len() - next_op,
                    retry_count,
                    err
                );
                continue;
            }
        };

        let mut resp = response;
        let mut stream_pos = first.dest_start;
        let mut op_cursor = next_op;
        // Progress within the op at `op_cursor`.
        let mut op_done = 0u64;
        let mut hasher = FlexHasher::from_checksum(&first.target_checksum);
        let mut pending: Vec<u8> = Vec::with_capacity(INSERT_RUN_WRITE_BUFFER);
        let mut pending_at = first.blob_offset;
        let mut written_this_attempt = 0u64;
        let mut stalled = false;

        loop {
            wait_for_download_resume(&mut pause_rx, &mut cancel_rx).await?;
            let chunk = match tokio::time::timeout(PATCH_CHUNK_TIMEOUT, resp.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => return Err(err).context("failed to read insert run chunk"),
                Err(_) => {
                    retry_count += 1;
                    if retry_count > PATCH_DOWNLOAD_MAX_RETRIES {
                        return Err(anyhow!(
                            "delta insert run chunk timed out after {} retries",
                            PATCH_DOWNLOAD_MAX_RETRIES
                        ));
                    }
                    warn!(
                        "Delta insert run chunk timed out: file_id={} op={} retries={}",
                        file_id, ops[run.op_indices[op_cursor]].data_order, retry_count
                    );
                    stalled = true;
                    break;
                }
            };
            rate_limiter.acquire_and_record(chunk.len()).await;

            let mut slice: &[u8] = &chunk;
            while !slice.is_empty() && op_cursor < run.op_indices.len() {
                let op = &ops[run.op_indices[op_cursor]];
                if stream_pos < op.dest_start {
                    // Gap before the next op: copy-op bytes the plan already has.
                    let skip = (op.dest_start - stream_pos).min(slice.len() as u64) as usize;
                    slice = &slice[skip..];
                    stream_pos += skip as u64;
                    continue;
                }
                let take = (op.length - op_done).min(slice.len() as u64) as usize;
                let bytes = &slice[..take];
                hasher.update(bytes);
                if pending.len() + take > INSERT_RUN_WRITE_BUFFER && !pending.is_empty() {
                    flush_insert_pending(&blob_file, &mut pending, &mut pending_at).await?;
                }
                pending.extend_from_slice(bytes);
                metrics.record_bytes(take as u64);
                written_this_attempt += take as u64;
                op_done += take as u64;
                stream_pos += take as u64;
                slice = &slice[take..];

                if op_done == op.length {
                    flush_insert_pending(&blob_file, &mut pending, &mut pending_at).await?;
                    let actual =
                        std::mem::replace(&mut hasher, FlexHasher::new_md5()).finalize_hex();
                    if !checksum_matches(&op.target_checksum, &actual) {
                        return Err(anyhow!(
                            "insert op {} checksum mismatch (expected {}, got {})",
                            op.data_order,
                            op.target_checksum,
                            actual
                        ));
                    }
                    debug!(
                        "Delta insert op complete: file_id={} op={} bytes={}",
                        file_id, op.data_order, op.length
                    );
                    op_cursor += 1;
                    op_done = 0;
                    if let Some(&idx) = run.op_indices.get(op_cursor) {
                        hasher = FlexHasher::from_checksum(&ops[idx].target_checksum);
                        pending_at = ops[idx].blob_offset;
                    }
                }
            }
            if op_cursor >= run.op_indices.len() {
                break;
            }
        }

        if stalled {
            // Bytes of the op in flight are rewritten on the retry; the
            // completed ops before it stay.
            written_total += written_this_attempt - op_done;
            next_op = op_cursor;
            continue;
        }
        if op_cursor < run.op_indices.len() {
            return Err(anyhow!(
                "insert run ended early: op {} received {}/{} bytes",
                ops[run.op_indices[op_cursor]].data_order,
                op_done,
                ops[run.op_indices[op_cursor]].length
            ));
        }
        written_total += written_this_attempt;
        next_op = op_cursor;
        retry_count = 0;
    }

    Ok(written_total)
}

async fn flush_insert_pending(
    blob_file: &Arc<std::fs::File>,
    pending: &mut Vec<u8>,
    pending_at: &mut u64,
) -> anyhow::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let data = std::mem::take(pending);
    let at = *pending_at;
    *pending_at += data.len() as u64;
    let file = blob_file.clone();
    tokio::task::spawn_blocking(move || write_at(&file, at, &data)).await??;
    Ok(())
}
