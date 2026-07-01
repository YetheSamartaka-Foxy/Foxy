use crate::core::utils::file_io::write_at;
use crate::core::utils::http_range::validate_content_range_header;
use anyhow::anyhow;
use log::{debug, info, trace, warn};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, watch};
use tokio::time::{sleep, timeout};

use super::bandwidth::AdaptiveBandwidthLimiter;
use super::metrics::{DownloadMetrics, DownloadSchedulerState, RangeMetric};
use super::transfer::{
    download_cancelled_error, ensure_not_cancelled, jitter_delay, wait_for_download_resume,
};
use super::{ATTEMPT_LIMIT, BUFFERED_WRITE_CAPACITY};

/// How often a worker parked above the current per-file range cap re-checks
/// whether the cap has grown (queue drained) or work remains.
const WORKER_GATE_RECHECK: Duration = Duration::from_millis(250);

const RANGE_PART_META_VERSION: u32 = 1;

/// Sidecar path holding completed-chunk state for a ranged `.foxy.part` file.
pub(super) fn range_part_meta_path(path: &str) -> String {
    format!("{}.foxy.part.meta", path)
}

/// Persistent record of which chunks of a ranged download already completed.
///
/// Ranged downloads pre-allocate the full `.foxy.part` file and fill it out of
/// order, so the part length alone cannot say how much real data it holds.
/// This sidecar (`<file>.foxy.part.meta`) is updated after each chunk fully
/// hits disk, which makes ranged downloads resumable across app restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct RangePartMeta {
    version: u32,
    pub(super) file_size: u64,
    chunk_size: u64,
    completed: Vec<u32>,
}

impl RangePartMeta {
    fn new(file_size: u64, chunk_size: u64) -> Self {
        Self {
            version: RANGE_PART_META_VERSION,
            file_size,
            chunk_size,
            completed: Vec::new(),
        }
    }

    fn chunk_count(&self) -> u64 {
        chunk_count_for(self.file_size, self.chunk_size)
    }

    /// Parse and validate sidecar bytes. Returns `None` for any malformed or
    /// inconsistent state so callers fall back to a fresh download.
    pub(super) fn parse(bytes: &[u8]) -> Option<Self> {
        let mut meta: Self = serde_json::from_slice(bytes).ok()?;
        if meta.version != RANGE_PART_META_VERSION || meta.file_size == 0 || meta.chunk_size == 0 {
            return None;
        }
        let chunk_count = meta.chunk_count();
        let unique: BTreeSet<u32> = meta.completed.iter().copied().collect();
        if unique.iter().any(|idx| u64::from(*idx) >= chunk_count) {
            return None;
        }
        meta.completed = unique.into_iter().collect();
        Some(meta)
    }

    /// Synchronously load and validate a sidecar file (for blocking contexts).
    pub(super) fn load_sync(meta_path: &str) -> Option<Self> {
        let bytes = std::fs::read(meta_path).ok()?;
        Self::parse(&bytes)
    }

    fn completed_set(&self) -> HashSet<u32> {
        self.completed.iter().copied().collect()
    }

    /// Total bytes covered by completed chunks (last chunk may be short).
    pub(super) fn completed_bytes(&self) -> u64 {
        self.completed
            .iter()
            .map(|idx| chunk_len_at(u64::from(*idx), self.file_size, self.chunk_size))
            .sum()
    }

    fn serialize(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

fn chunk_count_for(file_size: u64, chunk_size: u64) -> u64 {
    if chunk_size == 0 {
        return 0;
    }
    file_size.div_ceil(chunk_size)
}

fn chunk_len_at(index: u64, file_size: u64, chunk_size: u64) -> u64 {
    let start = index.saturating_mul(chunk_size);
    chunk_size.min(file_size.saturating_sub(start))
}

/// Chunk indices fully covered by a sequential prefix of `part_len` bytes.
/// Used to adopt an append-style part file from a previous simple download.
fn prefix_completed_chunks(part_len: u64, file_size: u64, chunk_size: u64) -> Vec<u32> {
    if chunk_size == 0 || part_len == 0 {
        return Vec::new();
    }
    let full_chunks =
        (part_len.min(file_size) / chunk_size).min(chunk_count_for(file_size, chunk_size));
    (0..full_chunks).map(|idx| idx as u32).collect()
}

/// How an existing `.foxy.part` (and optional sidecar) should be handled when
/// a ranged download starts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PartInitPlan {
    /// No usable prior state - truncate and download everything.
    Fresh,
    /// Resume a ranged part: these chunk indices are already on disk.
    ResumeChunks(Vec<u32>),
    /// Every byte is already on disk (previous session finished the transfer
    /// but did not promote). Skip the download; later hashing verifies it.
    AlreadyComplete,
}

/// Decide resume strategy. Returns the plan plus the chunk grid to use -
/// resuming must reuse the grid recorded in the sidecar, not the current one.
fn plan_part_init(
    part_len: Option<u64>,
    meta: Option<&RangePartMeta>,
    file_size: u64,
    chunk_target: u64,
    download_total: u64,
) -> (PartInitPlan, u64) {
    let meta = meta.filter(|m| m.file_size == file_size);
    match part_len {
        Some(len) if len == file_size => {
            if let Some(meta) = meta {
                if meta.completed_bytes() >= file_size {
                    (PartInitPlan::AlreadyComplete, meta.chunk_size)
                } else {
                    (
                        PartInitPlan::ResumeChunks(meta.completed.clone()),
                        meta.chunk_size,
                    )
                }
            } else if download_total >= file_size {
                // Append-style download finished but was never promoted.
                (PartInitPlan::AlreadyComplete, chunk_target)
            } else {
                // Full-length part without completion records is a ranged
                // pre-allocation with unknown contents - cannot be trusted.
                (PartInitPlan::Fresh, chunk_target)
            }
        }
        Some(len) if len < file_size => {
            if meta.is_some() {
                // Ranged parts are always full length; a short part with a
                // sidecar is inconsistent state.
                (PartInitPlan::Fresh, chunk_target)
            } else {
                // Adopt the sequential prefix from a previous append-style
                // (simple) download; only the boundary chunk is re-fetched.
                (
                    PartInitPlan::ResumeChunks(prefix_completed_chunks(
                        len,
                        file_size,
                        chunk_target,
                    )),
                    chunk_target,
                )
            }
        }
        _ => (PartInitPlan::Fresh, chunk_target),
    }
}

/// A single HTTP range request job for a portion of a file.
struct RangeJob {
    /// Index of this chunk in the fixed chunk grid.
    index: u32,
    /// Byte offset where this chunk starts in the file.
    start: u64,
    /// Byte offset where this chunk ends (inclusive).
    end: u64,
}

/// Build range jobs for the chunks not present in `completed`.
fn missing_range_jobs(total_size: u64, chunk_size: u64, completed: &HashSet<u32>) -> Vec<RangeJob> {
    if chunk_size == 0 {
        return Vec::new();
    }
    let mut jobs = Vec::new();
    let mut offset = 0_u64;
    let mut index = 0_u32;
    while offset < total_size {
        let end = (offset + chunk_size - 1).min(total_size - 1);
        if !completed.contains(&index) {
            jobs.push(RangeJob {
                index,
                start: offset,
                end,
            });
        }
        offset = end + 1;
        index += 1;
    }
    jobs
}

/// Serialized writer for the resume sidecar. Chunk completions are recorded
/// best-effort: a failed sidecar write degrades resume granularity, never the
/// download itself.
struct RangeMetaWriter {
    meta_path: String,
    state: Mutex<RangePartMeta>,
}

impl RangeMetaWriter {
    async fn persist_locked(meta_path: &str, meta: &RangePartMeta) -> std::io::Result<()> {
        let tmp_path = format!("{}.tmp", meta_path);
        tokio::fs::write(&tmp_path, meta.serialize()).await?;
        tokio::fs::rename(&tmp_path, meta_path).await
    }

    /// Write the current state to disk (atomic replace via temp + rename).
    async fn persist(&self) {
        let state = self.state.lock().await;
        if let Err(err) = Self::persist_locked(&self.meta_path, &state).await {
            warn!(
                "Failed to persist range resume sidecar {}: {}",
                self.meta_path, err
            );
        }
    }

    async fn mark_complete(&self, index: u32) {
        let mut state = self.state.lock().await;
        if !state.completed.contains(&index) {
            state.completed.push(index);
        }
        if let Err(err) = Self::persist_locked(&self.meta_path, &state).await {
            warn!(
                "Failed to persist range resume sidecar {}: {}",
                self.meta_path, err
            );
        }
    }
}

/// Per-file state shared across all range workers downloading that file.
struct RangeFileState {
    file_id: u64,
    url: Arc<str>,
    path: Arc<str>,
    part_file: Arc<std::fs::File>,
    completed_ranges: AtomicUsize,
    completed_bytes: AtomicUsize,
    meta_writer: Arc<RangeMetaWriter>,
}

fn prepare_part_file(
    part_path: &str,
    total_size: u64,
    fresh: bool,
) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if fresh {
        options.truncate(true);
    }
    let file = options.open(part_path)?;
    // Pre-allocate (fresh) or extend an adopted shorter part to full length.
    file.set_len(total_size)?;
    Ok(file)
}

/// Download a large file using a pool of range workers pulling from a shared queue.
///
/// The file is decomposed into fixed-size chunks. A pool of workers pulls chunks
/// from the queue, acquires a global range permit, downloads, and writes to the
/// correct offset. Workers above the current fair-share cap stay parked, so when
/// the global queue drains (tail of the run, or a single-file download) the
/// remaining files automatically ramp up to more parallel connections.
///
/// Completed chunks are recorded in a `.foxy.part.meta` sidecar after their data
/// is on disk, so an interrupted ranged download resumes in the next session.
#[allow(clippy::too_many_arguments)]
pub(super) async fn download_large_file_with_range_queue(
    context: Arc<crate::core::models::context::FoxyContext>,
    file_id: u64,
    url: Arc<str>,
    path: Arc<str>,
    total_size: usize,
    download_total: Arc<AtomicUsize>,
    download_cycle: Arc<AtomicUsize>,
    limiter: Arc<AdaptiveBandwidthLimiter>,
    scheduler: Arc<DownloadSchedulerState>,
    metrics: Arc<DownloadMetrics>,
    download_pause_rx: watch::Receiver<bool>,
    cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<u64> {
    let chunk_target = scheduler.limits.range_chunk_target as u64;
    let part_path = format!("{}.foxy.part", path);
    let meta_path = range_part_meta_path(&path);

    // Inspect prior part/sidecar state off the async runtime.
    let (plan, chunk_size) = {
        let part_path = part_path.clone();
        let meta_path = meta_path.clone();
        let download_total_now = download_total.load(Ordering::SeqCst) as u64;
        let file_size = total_size as u64;
        tokio::task::spawn_blocking(move || {
            let part_len = std::fs::metadata(&part_path).ok().map(|meta| meta.len());
            let meta = RangePartMeta::load_sync(&meta_path);
            plan_part_init(
                part_len,
                meta.as_ref(),
                file_size,
                chunk_target,
                download_total_now,
            )
        })
        .await?
    };

    if plan == PartInitPlan::AlreadyComplete {
        info!(
            "Range download already complete on disk, skipping transfer: file_id={} path={}",
            file_id, path
        );
        download_total.store(total_size, Ordering::SeqCst);
        download_cycle.store(0, Ordering::SeqCst);
        remove_range_meta(&meta_path).await;
        return Ok(total_size as u64);
    }

    let (completed, fresh) = match plan {
        PartInitPlan::ResumeChunks(chunks) => (chunks, false),
        _ => (Vec::new(), true),
    };

    let mut resume_meta = RangePartMeta::new(total_size as u64, chunk_size);
    resume_meta.completed = completed.clone();
    let resumed_bytes = resume_meta.completed_bytes();
    let completed_set = resume_meta.completed_set();
    let total_ranges = chunk_count_for(total_size as u64, chunk_size) as usize;

    let jobs = missing_range_jobs(total_size as u64, chunk_size, &completed_set);

    // Reset progress counters to verified on-disk state. Mirrors the simple
    // path, which re-stores progress at every resume/restart boundary.
    download_total.store(resumed_bytes as usize, Ordering::SeqCst);
    download_cycle.store(0, Ordering::SeqCst);

    let part_file = tokio::task::spawn_blocking({
        let part_path = part_path.clone();
        let file_size = total_size as u64;
        move || prepare_part_file(&part_path, file_size, fresh)
    })
    .await??;

    let meta_writer = Arc::new(RangeMetaWriter {
        meta_path: meta_path.clone(),
        state: Mutex::new(resume_meta),
    });
    // Persist the initial sidecar so a crash during the first chunks already
    // has a valid grid recorded (fresh start) or adopted prefix state.
    meta_writer.persist().await;

    if !completed_set.is_empty() {
        info!(
            "Range queue resume: file_id={} path={} resumed_chunks={}/{} resumed_bytes={}",
            file_id,
            path,
            completed_set.len(),
            total_ranges,
            resumed_bytes
        );
    }

    let state = Arc::new(RangeFileState {
        file_id,
        url: url.clone(),
        path: path.clone(),
        part_file: Arc::new(part_file),
        completed_ranges: AtomicUsize::new(completed_set.len()),
        completed_bytes: AtomicUsize::new(resumed_bytes as usize),
        meta_writer: meta_writer.clone(),
    });

    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));

    // Spawn up to the per-file ceiling; workers above the current fair-share
    // cap stay parked until queue pressure drops and the cap grows.
    let worker_count = scheduler
        .limits
        .max_ranges_per_file
        .min(total_ranges.saturating_sub(completed_set.len()))
        .max(1);

    debug!(
        "Range queue: file_id={} size={} chunks={} resumed={} workers={} chunk_size={} cap_now={}",
        file_id,
        total_size,
        total_ranges,
        completed_set.len(),
        worker_count,
        chunk_size,
        scheduler.current_per_file_range_cap()
    );

    let mut workers = futures::stream::FuturesUnordered::new();
    for worker_id in 0..worker_count {
        let ctx = context.clone();
        let limiter = limiter.clone();
        let scheduler = scheduler.clone();
        let metrics = metrics.clone();
        let queue = queue.clone();
        let state = state.clone();
        let download_total = download_total.clone();
        let download_cycle = download_cycle.clone();
        let pause_rx = download_pause_rx.clone();
        let cancel_rx = cancel_rx.clone();

        workers.push(tokio::spawn(async move {
            range_worker(
                worker_id,
                ctx,
                limiter,
                scheduler,
                metrics,
                queue,
                state,
                download_total,
                download_cycle,
                pause_rx,
                cancel_rx,
            )
            .await
        }));
    }

    use futures::StreamExt;
    let mut first_error: Option<anyhow::Error> = None;
    while let Some(result) = workers.next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
                // Drain the queue so other workers exit quickly
                queue.lock().await.clear();
            }
            Err(join_err) => {
                if first_error.is_none() {
                    first_error = Some(anyhow!("range worker panicked: {}", join_err));
                }
                queue.lock().await.clear();
            }
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    let completed = state.completed_ranges.load(Ordering::Relaxed);
    if completed != total_ranges {
        anyhow::bail!(
            "Not all range chunks completed for {}: {}/{}",
            path,
            completed,
            total_ranges
        );
    }

    let total_bytes = state.completed_bytes.load(Ordering::Relaxed) as u64;
    if total_bytes != total_size as u64 {
        anyhow::bail!(
            "Range queue size mismatch for {}: expected {} bytes, received {}",
            path,
            total_size,
            total_bytes
        );
    }

    // Drop the shared file handle before rename
    drop(state);

    // The transfer is complete; the sidecar has served its purpose.
    remove_range_meta(&meta_path).await;

    Ok(total_bytes)
}

/// Remove a resume sidecar, ignoring not-found.
pub(super) async fn remove_range_meta(meta_path: &str) {
    match tokio::fs::remove_file(meta_path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!(
            "Failed to remove range resume sidecar {}: {}",
            meta_path, err
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn range_worker(
    worker_id: usize,
    context: Arc<crate::core::models::context::FoxyContext>,
    limiter: Arc<AdaptiveBandwidthLimiter>,
    scheduler: Arc<DownloadSchedulerState>,
    metrics: Arc<DownloadMetrics>,
    queue: Arc<Mutex<VecDeque<RangeJob>>>,
    state: Arc<RangeFileState>,
    download_total: Arc<AtomicUsize>,
    download_cycle: Arc<AtomicUsize>,
    mut pause_rx: watch::Receiver<bool>,
    mut cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        wait_for_download_resume(&mut pause_rx, &mut cancel_rx).await?;

        // Fair-share gate: workers above the current per-file cap park until
        // the global queue drains and the cap grows (tail ramp-up).
        if worker_id >= scheduler.current_per_file_range_cap() {
            if queue.lock().await.is_empty() {
                return Ok(());
            }
            tokio::select! {
                _ = sleep(WORKER_GATE_RECHECK) => {}
                changed = cancel_rx.changed() => {
                    if changed.is_err() {
                        return Err(download_cancelled_error());
                    }
                    ensure_not_cancelled(&cancel_rx)?;
                }
            }
            continue;
        }

        let Some(job) = queue.lock().await.pop_front() else {
            return Ok(());
        };

        ensure_not_cancelled(&cancel_rx)?;

        // Acquire global range permit
        let _permit = scheduler
            .range_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("range semaphore closed"))?;
        metrics
            .counters
            .active_ranges
            .fetch_add(1, Ordering::Relaxed);

        let result = download_one_range(
            &context,
            &limiter,
            &metrics,
            &state,
            &download_total,
            &download_cycle,
            &job,
            &mut pause_rx,
            &mut cancel_rx,
        )
        .await;

        metrics
            .counters
            .active_ranges
            .fetch_sub(1, Ordering::Relaxed);

        match result {
            Ok(bytes) => {
                state.completed_ranges.fetch_add(1, Ordering::Relaxed);
                state
                    .completed_bytes
                    .fetch_add(bytes as usize, Ordering::Relaxed);
                // Record after the chunk's data is fully written so the
                // sidecar never claims bytes that are not on disk.
                state.meta_writer.mark_complete(job.index).await;
            }
            Err(err) => {
                // On failure, clear the queue to stop other workers
                queue.lock().await.clear();
                return Err(err);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_one_range(
    context: &Arc<crate::core::models::context::FoxyContext>,
    limiter: &Arc<AdaptiveBandwidthLimiter>,
    metrics: &Arc<DownloadMetrics>,
    state: &Arc<RangeFileState>,
    download_total: &Arc<AtomicUsize>,
    download_cycle: &Arc<AtomicUsize>,
    job: &RangeJob,
    pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> anyhow::Result<u64> {
    let start = job.start;
    let end = job.end;
    let expected_bytes = end - start + 1;
    let request_started = Instant::now();

    let resp = context
        .client
        .get(&*state.url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .header("Range", format!("bytes={}-{}", start, end))
        .send()
        .await?;

    let request_latency = request_started.elapsed();

    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!(
            "Range request {}-{} returned {} (expected 206)",
            start,
            end,
            resp.status()
        );
    }

    if let Err(err) = validate_content_range_header(&resp, start, end) {
        warn!(
            "Content-Range validation failed for {} range {}-{}: {}",
            state.path, start, end, err
        );
        anyhow::bail!(
            "Content-Range validation failed for range {}-{}: {}",
            start,
            end,
            err
        );
    }

    let mut response = resp;
    let mut write_buf = Vec::with_capacity(BUFFERED_WRITE_CAPACITY);
    let mut buf_offset = start;
    let mut bytes_received = 0_u64;
    let mut attempt = 0u8;
    let mut io_total_time = Duration::ZERO;
    let mut io_write_count = 0usize;
    let transfer_started = Instant::now();

    loop {
        ensure_not_cancelled(cancel_rx)?;
        if *pause_rx.borrow() {
            wait_for_download_resume(pause_rx, cancel_rx).await?;
        }

        let chunk_result = timeout(Duration::from_secs(10), response.chunk()).await;

        let needs_retry = match chunk_result {
            Ok(Ok(Some(bytes))) => {
                attempt = 0;
                let n = bytes.len();
                bytes_received += n as u64;
                limiter.acquire_and_record(n).await;
                metrics.record_bytes(n as u64);
                download_cycle.fetch_add(n, Ordering::Relaxed);
                download_total.fetch_add(n, Ordering::Relaxed);

                write_buf.extend_from_slice(&bytes);
                if write_buf.len() >= BUFFERED_WRITE_CAPACITY {
                    let flush_offset = buf_offset;
                    buf_offset += write_buf.len() as u64;
                    let flush_data: Vec<u8> = std::mem::take(&mut write_buf);
                    let file = state.part_file.clone();
                    let write_started = Instant::now();
                    tokio::task::spawn_blocking(move || write_at(&file, flush_offset, &flush_data))
                        .await??;
                    io_total_time += write_started.elapsed();
                    io_write_count += 1;
                }
                false
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                debug!("Range worker error {}-{}: {}", start, end, e);
                true
            }
            Err(_) => {
                debug!("Range worker timeout {}-{}", start, end);
                true
            }
        };

        if needs_retry {
            if attempt >= ATTEMPT_LIMIT {
                anyhow::bail!("Exceeded max retries for range {}-{}", start, end);
            }
            let delay = jitter_delay(attempt as usize);
            trace!("Retrying range {}-{} in {:?}", start, end, delay);
            sleep(delay).await;
            attempt += 1;
            metrics
                .counters
                .range_retries
                .fetch_add(1, Ordering::Relaxed);

            // Flush buffer before reconnect
            if !write_buf.is_empty() {
                let flush_offset = buf_offset;
                buf_offset += write_buf.len() as u64;
                let flush_data: Vec<u8> = std::mem::take(&mut write_buf);
                let file = state.part_file.clone();
                let write_started = Instant::now();
                tokio::task::spawn_blocking(move || write_at(&file, flush_offset, &flush_data))
                    .await??;
                io_total_time += write_started.elapsed();
                io_write_count += 1;
            }

            // Reconnect from where we left off
            let resume_start = start + bytes_received;
            if resume_start > end {
                break;
            }
            ensure_not_cancelled(cancel_rx)?;
            let reconnect_resp = context
                .client
                .get(&*state.url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .header("Range", format!("bytes={}-{}", resume_start, end))
                .send()
                .await?;
            if reconnect_resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                anyhow::bail!(
                    "Reconnect range {}-{} returned {} (expected 206)",
                    resume_start,
                    end,
                    reconnect_resp.status()
                );
            }
            response = reconnect_resp;
        }
    }

    // Flush remaining buffer
    if !write_buf.is_empty() {
        let file = state.part_file.clone();
        let offset = buf_offset;
        let write_started = Instant::now();
        tokio::task::spawn_blocking(move || write_at(&file, offset, &write_buf)).await??;
        io_total_time += write_started.elapsed();
        io_write_count += 1;
    }

    if bytes_received != expected_bytes {
        anyhow::bail!(
            "Range {}-{} size mismatch: expected {} bytes, received {}",
            start,
            end,
            expected_bytes,
            bytes_received
        );
    }

    let transfer_time = transfer_started.elapsed();
    metrics.record_range(RangeMetric {
        file_id: state.file_id,
        start,
        end,
        request_latency,
        transfer_time,
        write_time: io_total_time,
        write_count: io_write_count,
        retries: attempt as usize,
        bytes: bytes_received,
    });

    Ok(bytes_received)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full chunk grid with nothing completed (test convenience).
    fn build_range_jobs(total_size: u64, chunk_size: u64) -> Vec<RangeJob> {
        missing_range_jobs(total_size, chunk_size, &HashSet::new())
    }

    #[test]
    fn build_range_jobs_single_chunk() {
        let jobs = build_range_jobs(100, 200);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].index, 0);
        assert_eq!(jobs[0].start, 0);
        assert_eq!(jobs[0].end, 99);
    }

    #[test]
    fn build_range_jobs_exact_split() {
        let jobs = build_range_jobs(200, 100);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].start, 0);
        assert_eq!(jobs[0].end, 99);
        assert_eq!(jobs[1].start, 100);
        assert_eq!(jobs[1].end, 199);
    }

    #[test]
    fn build_range_jobs_remainder() {
        let jobs = build_range_jobs(250, 100);
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].start, 0);
        assert_eq!(jobs[0].end, 99);
        assert_eq!(jobs[1].start, 100);
        assert_eq!(jobs[1].end, 199);
        assert_eq!(jobs[2].start, 200);
        assert_eq!(jobs[2].end, 249);
    }

    #[test]
    fn build_range_jobs_zero_size() {
        let jobs = build_range_jobs(0, 100);
        assert!(jobs.is_empty());
    }

    #[test]
    fn build_range_jobs_one_byte() {
        let jobs = build_range_jobs(1, 100);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].start, 0);
        assert_eq!(jobs[0].end, 0);
    }

    #[test]
    fn build_range_jobs_large_file() {
        let size = 500 * 1024 * 1024; // 500 MB
        let chunk = 8 * 1024 * 1024; // 8 MB
        let jobs = build_range_jobs(size, chunk);
        // 500 / 8 = 62.5 → 63 chunks
        assert_eq!(jobs.len(), 63);
        // Verify contiguity and index ordering
        for i in 1..jobs.len() {
            assert_eq!(jobs[i].start, jobs[i - 1].end + 1);
            assert_eq!(jobs[i].index, jobs[i - 1].index + 1);
        }
        assert_eq!(jobs.last().unwrap().end, size - 1);
    }

    #[test]
    fn missing_range_jobs_skips_completed_chunks() {
        let completed: HashSet<u32> = [0, 2].into_iter().collect();
        let jobs = missing_range_jobs(250, 100, &completed);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].index, 1);
        assert_eq!(jobs[0].start, 100);
        assert_eq!(jobs[0].end, 199);
    }

    // ── RangePartMeta ──────────────────────────────────────────────────

    #[test]
    fn range_part_meta_roundtrip() {
        let mut meta = RangePartMeta::new(250, 100);
        meta.completed = vec![2, 0];
        let parsed = RangePartMeta::parse(&meta.serialize()).expect("valid sidecar");
        assert_eq!(parsed.file_size, 250);
        assert_eq!(parsed.chunk_size, 100);
        // Parse normalizes ordering and dedupes
        assert_eq!(parsed.completed, vec![0, 2]);
    }

    #[test]
    fn range_part_meta_rejects_bad_version() {
        let mut meta = RangePartMeta::new(250, 100);
        meta.version = 99;
        assert!(RangePartMeta::parse(&meta.serialize()).is_none());
    }

    #[test]
    fn range_part_meta_rejects_out_of_range_index() {
        let mut meta = RangePartMeta::new(250, 100);
        meta.completed = vec![3]; // grid has chunks 0..=2
        assert!(RangePartMeta::parse(&meta.serialize()).is_none());
    }

    #[test]
    fn range_part_meta_rejects_garbage() {
        assert!(RangePartMeta::parse(b"not json").is_none());
        assert!(RangePartMeta::parse(b"{}").is_none());
    }

    #[test]
    fn range_part_meta_completed_bytes_counts_short_last_chunk() {
        let mut meta = RangePartMeta::new(250, 100);
        meta.completed = vec![0, 2];
        // Chunk 0 is 100 bytes, chunk 2 is the 50-byte tail.
        assert_eq!(meta.completed_bytes(), 150);
    }

    // ── prefix_completed_chunks ────────────────────────────────────────

    #[test]
    fn prefix_chunks_cover_only_full_chunks() {
        assert_eq!(prefix_completed_chunks(0, 1000, 100), Vec::<u32>::new());
        assert_eq!(prefix_completed_chunks(99, 1000, 100), Vec::<u32>::new());
        assert_eq!(prefix_completed_chunks(100, 1000, 100), vec![0]);
        assert_eq!(prefix_completed_chunks(250, 1000, 100), vec![0, 1]);
    }

    // ── plan_part_init ─────────────────────────────────────────────────

    #[test]
    fn plan_fresh_when_no_part_file() {
        let (plan, chunk) = plan_part_init(None, None, 1000, 100, 0);
        assert_eq!(plan, PartInitPlan::Fresh);
        assert_eq!(chunk, 100);
    }

    #[test]
    fn plan_resumes_from_sidecar_with_its_own_grid() {
        let mut meta = RangePartMeta::new(1000, 250);
        meta.completed = vec![0, 3];
        let (plan, chunk) = plan_part_init(Some(1000), Some(&meta), 1000, 100, 0);
        assert_eq!(plan, PartInitPlan::ResumeChunks(vec![0, 3]));
        // Resume must reuse the sidecar's chunk grid, not the current target.
        assert_eq!(chunk, 250);
    }

    #[test]
    fn plan_complete_when_sidecar_covers_all_bytes() {
        let mut meta = RangePartMeta::new(1000, 250);
        meta.completed = vec![0, 1, 2, 3];
        let (plan, _) = plan_part_init(Some(1000), Some(&meta), 1000, 100, 0);
        assert_eq!(plan, PartInitPlan::AlreadyComplete);
    }

    #[test]
    fn plan_distrusts_full_length_part_without_sidecar() {
        let (plan, _) = plan_part_init(Some(1000), None, 1000, 100, 400);
        assert_eq!(plan, PartInitPlan::Fresh);
    }

    #[test]
    fn plan_complete_for_unpromoted_append_download() {
        let (plan, _) = plan_part_init(Some(1000), None, 1000, 100, 1000);
        assert_eq!(plan, PartInitPlan::AlreadyComplete);
    }

    #[test]
    fn plan_adopts_append_prefix_without_sidecar() {
        let (plan, chunk) = plan_part_init(Some(450), None, 1000, 100, 450);
        assert_eq!(plan, PartInitPlan::ResumeChunks(vec![0, 1, 2, 3]));
        assert_eq!(chunk, 100);
    }

    #[test]
    fn plan_fresh_for_short_part_with_sidecar() {
        let meta = RangePartMeta::new(1000, 100);
        let (plan, _) = plan_part_init(Some(450), Some(&meta), 1000, 100, 0);
        assert_eq!(plan, PartInitPlan::Fresh);
    }

    #[test]
    fn plan_fresh_when_sidecar_size_mismatches() {
        let mut meta = RangePartMeta::new(500, 100);
        meta.completed = vec![0];
        let (plan, chunk) = plan_part_init(Some(1000), Some(&meta), 1000, 100, 0);
        assert_eq!(plan, PartInitPlan::Fresh);
        assert_eq!(chunk, 100);
    }

    #[test]
    fn plan_fresh_for_oversized_part() {
        let (plan, _) = plan_part_init(Some(2000), None, 1000, 100, 0);
        assert_eq!(plan, PartInitPlan::Fresh);
    }

    #[test]
    fn meta_path_appends_suffix() {
        assert_eq!(
            range_part_meta_path("C:\\mods\\@a\\addons\\x.pbo"),
            "C:\\mods\\@a\\addons\\x.pbo.foxy.part.meta"
        );
    }
}
