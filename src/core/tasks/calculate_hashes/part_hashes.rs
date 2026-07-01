use super::pbo_layout::{
    LocalPartSpan, has_pbo_part_markers, map_local_part_spans, parse_local_pbo_layout,
};
use super::*;

#[derive(Clone, Debug, Default)]
pub(super) struct PartHashMetrics {
    pub(super) total_elapsed: std::time::Duration,
    pub(super) metadata_elapsed: std::time::Duration,
    pub(super) layout_elapsed: std::time::Duration,
    pub(super) layout_parse_elapsed: std::time::Duration,
    pub(super) layout_map_elapsed: std::time::Duration,
    pub(super) semaphore_wait_elapsed: std::time::Duration,
    pub(super) blocking_hash_elapsed: std::time::Duration,
    pub(super) estimated_bytes: u64,
    pub(super) hashed_bytes: u64,
    pub(super) layout_files: usize,
    pub(super) remote_span_files: usize,
    pub(super) layout_entries: usize,
    pub(super) layout_entry_payload_bytes: u64,
    pub(super) mapped_parts: usize,
    pub(super) fallback_parts: usize,
}

pub(super) struct PartHashCalculation {
    pub(super) parts: Vec<FoxyModFilePart>,
    pub(super) metrics: PartHashMetrics,
}

#[derive(Clone)]
pub(super) struct PartHashProgress {
    parts_done: Arc<AtomicUsize>,
    total_parts: usize,
    files_done: Arc<AtomicUsize>,
    total_files: usize,
    progress_tx: Sender<ProgressEvent>,
}

impl PartHashProgress {
    pub(super) fn new(
        parts_done: Arc<AtomicUsize>,
        total_parts: usize,
        files_done: Arc<AtomicUsize>,
        total_files: usize,
        progress_tx: Sender<ProgressEvent>,
    ) -> Self {
        Self {
            parts_done,
            total_parts,
            files_done,
            total_files,
            progress_tx,
        }
    }

    pub(super) fn mark_parts_done(&self, count: usize) {
        if count == 0 {
            return;
        }

        const PART_PROGRESS_INTERVAL: usize = 512;
        let checked_parts = self
            .parts_done
            .fetch_add(count, Ordering::Relaxed)
            .saturating_add(count)
            .min(self.total_parts);
        let previous_parts = checked_parts.saturating_sub(count);
        let crossed_interval =
            checked_parts / PART_PROGRESS_INTERVAL != previous_parts / PART_PROGRESS_INTERVAL;

        if crossed_interval || checked_parts == self.total_parts {
            self.emit(checked_parts);
        }
    }

    fn emit(&self, checked_parts: usize) {
        let _ = self.progress_tx.send(ProgressEvent::RecheckHashProgress {
            checked_files: self
                .files_done
                .load(Ordering::Relaxed)
                .min(self.total_files),
            total_files: self.total_files,
            checked_parts,
            total_parts: self.total_parts,
        });
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PartSpanSource {
    #[default]
    DetectLocalLayout,
    RemoteLayout,
}

pub(super) async fn calculate_part_hashes(
    parts: Vec<FoxyModFilePart>,
    file_path: &str,
    semaphore: Arc<Semaphore>,
    span_source: PartSpanSource,
    progress: Option<PartHashProgress>,
) -> PartHashCalculation {
    let pbo_name = Path::new(file_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(file_path);
    let total_parts = parts.len();
    let started_at = Instant::now();
    let mut metrics = PartHashMetrics {
        estimated_bytes: parts.iter().map(|part| part.remote_length).sum(),
        ..Default::default()
    };
    debug!("Calculating hashes for all files inside {}.", pbo_name);

    if total_parts == 0 {
        debug!(
            "Calculating hashes for all files inside {} finished - 0 files took 0.00 seconds.",
            pbo_name
        );
        metrics.total_elapsed = started_at.elapsed();
        return PartHashCalculation {
            parts: Vec::new(),
            metrics,
        };
    }

    let metadata_started = Instant::now();
    match tokio::fs::metadata(file_path).await {
        Ok(meta) => {
            let actual_size = meta.len();
            let expected_size = parts
                .iter()
                .map(|p| p.remote_start + p.remote_length)
                .max()
                .unwrap_or(0);
            if expected_size > 0 && actual_size < expected_size {
                info!(
                    "File {} is shorter than the remote layout (disk={}B expected>={}B); hashing readable local parts for delta planning",
                    pbo_name, actual_size, expected_size
                );
            }
        }
        Err(_) => {
            debug!("File {} does not exist, skipping hash", file_path);
            metrics.metadata_elapsed = metadata_started.elapsed();
            metrics.total_elapsed = started_at.elapsed();
            if let Some(progress) = &progress {
                progress.mark_parts_done(total_parts);
            }
            return PartHashCalculation {
                parts: parts
                    .into_iter()
                    .map(|mut part| {
                        part.local_checksum = String::new();
                        part.local_length = 0;
                        part.local_start = 0;
                        part
                    })
                    .collect(),
                metrics,
            };
        }
    }
    metrics.metadata_elapsed = metadata_started.elapsed();

    // Detect PBO layout for local span remapping before opening the file
    let layout_started = Instant::now();
    let has_pbo_markers = has_pbo_part_markers(&parts);
    let local_span_overrides = if span_source == PartSpanSource::DetectLocalLayout
        && has_pbo_markers
    {
        metrics.layout_files = 1;
        let parser_path = file_path.to_string();
        let parser_started = Instant::now();
        match tokio::task::spawn_blocking(move || parse_local_pbo_layout(&parser_path)).await {
            Ok(Ok(layout)) => {
                metrics.layout_parse_elapsed = parser_started.elapsed();
                metrics.layout_entries = layout.entry_count;
                metrics.layout_entry_payload_bytes = layout.entry_payload_bytes;
                let header_len = layout.header.length;
                let end_start = layout.end.start;
                let end_len = layout.end.length;
                let map_started = Instant::now();
                let spans = map_local_part_spans(&parts, layout);
                metrics.layout_map_elapsed = map_started.elapsed();
                let mapped = spans.iter().filter(|span| span.is_some()).count();
                let fallback = total_parts.saturating_sub(mapped);
                metrics.mapped_parts = mapped;
                metrics.fallback_parts = fallback;
                debug!(
                    "Local PBO span remap for {}: mapped_parts={} fallback_parts={} header_len={} end_start={} end_len={}",
                    pbo_name, mapped, fallback, header_len, end_start, end_len
                );
                Arc::new(spans)
            }
            Ok(Err(err)) => {
                metrics.layout_parse_elapsed = parser_started.elapsed();
                warn!(
                    "Local PBO span remap failed for {}: {}. Falling back to remote offsets.",
                    pbo_name, err
                );
                metrics.fallback_parts = total_parts;
                Arc::new(vec![None; total_parts])
            }
            Err(err) => {
                metrics.layout_parse_elapsed = parser_started.elapsed();
                warn!(
                    "Local PBO span remap task failed for {}: {}. Falling back to remote offsets.",
                    pbo_name, err
                );
                metrics.fallback_parts = total_parts;
                Arc::new(vec![None; total_parts])
            }
        }
    } else {
        if span_source == PartSpanSource::RemoteLayout && has_pbo_markers {
            metrics.remote_span_files = 1;
            debug!(
                "Using remote part spans for freshly materialized download {} (parts={})",
                pbo_name, total_parts
            );
        }
        Arc::new(vec![None; total_parts])
    };
    metrics.layout_elapsed = layout_started.elapsed();
    if metrics.layout_files > 0 && (total_parts >= 64 || metrics.layout_elapsed.as_millis() >= 100)
    {
        info!(
            "PBO layout metrics: file={} parts={} entries={} entry_payload_bytes={} mapped_parts={} fallback_parts={} parse={:.3}s map={:.3}s total={:.3}s",
            pbo_name,
            total_parts,
            metrics.layout_entries,
            metrics.layout_entry_payload_bytes,
            metrics.mapped_parts,
            metrics.fallback_parts,
            metrics.layout_parse_elapsed.as_secs_f64(),
            metrics.layout_map_elapsed.as_secs_f64(),
            metrics.layout_elapsed.as_secs_f64()
        );
    }

    // Attach original index to preserve order after offset-sorted processing
    let mut indexed_parts: Vec<(usize, FoxyModFilePart)> = parts.into_iter().enumerate().collect();

    // Sort parts by effective start offset for sequential disk I/O.
    // This dramatically improves read-ahead prefetch and reduces random seeks.
    let span_overrides = local_span_overrides.clone();
    indexed_parts.sort_by_key(|(idx, part)| {
        span_overrides
            .get(*idx)
            .and_then(|span| *span)
            .map(|s| s.start)
            .unwrap_or(part.remote_start)
    });

    let file_path_owned = file_path.to_string();

    // Acquire one semaphore permit for the entire file - sequential processing
    // uses a single blocking thread instead of one per part.
    let semaphore_started = Instant::now();
    let Ok(_permit) = semaphore.acquire().await else {
        warn!("Hash semaphore closed, skipping file: {}", file_path);
        metrics.semaphore_wait_elapsed = semaphore_started.elapsed();
        metrics.total_elapsed = started_at.elapsed();
        if let Some(progress) = &progress {
            progress.mark_parts_done(total_parts);
        }
        return PartHashCalculation {
            parts: indexed_parts.into_iter().map(|(_, part)| part).collect(),
            metrics,
        };
    };
    metrics.semaphore_wait_elapsed = semaphore_started.elapsed();

    // Open the file once and hash all parts sequentially within a single
    // spawn_blocking task.  This eliminates N redundant open/close syscalls
    // (one per part) and lets the OS read-ahead prefetcher work efficiently.
    let blocking_started = Instant::now();
    let blocking_progress = progress.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut file = match std::fs::File::open(&file_path_owned) {
            Ok(f) => f,
            Err(e) => {
                warn!("Failed to open file {}: {}", file_path_owned, e);
                let total_part_count = indexed_parts.len();
                if let Some(progress) = &blocking_progress {
                    progress.mark_parts_done(total_part_count);
                }
                return indexed_parts;
            }
        };

        // Fixed-size buffer reused across all parts - caps memory regardless of part size
        const HASH_BUF_SIZE: usize = 64 * 1024;
        let mut buf = vec![0u8; HASH_BUF_SIZE];
        // Track consecutive read failures; after too many, skip remaining parts
        // to avoid log spam and wasted I/O on corrupted/truncated files.
        const MAX_READ_FAILURES: usize = 3;
        let mut consecutive_read_failures: usize = 0;
        let total_part_count = indexed_parts.len();

        for (idx, part) in &mut indexed_parts {
            // Bail early if the file is consistently unreadable
            if consecutive_read_failures >= MAX_READ_FAILURES {
                part.local_checksum = String::new();
                part.local_length = 0;
                part.local_start = 0;
                if let Some(progress) = &blocking_progress {
                    progress.mark_parts_done(1);
                }
                continue;
            }

            let chosen_span =
                span_overrides
                    .get(*idx)
                    .and_then(|span| *span)
                    .unwrap_or(LocalPartSpan {
                        start: part.remote_start,
                        length: part.remote_length,
                    });

            let total_len = match usize::try_from(chosen_span.length) {
                Ok(len) => len,
                Err(_) => {
                    warn!(
                        "Part length does not fit usize for {}: {}",
                        file_path_owned, chosen_span.length
                    );
                    part.local_checksum = String::new();
                    part.local_length = 0;
                    part.local_start = 0;
                    if let Some(progress) = &blocking_progress {
                        progress.mark_parts_done(1);
                    }
                    continue;
                }
            };

            if let Err(e) = file.seek(std::io::SeekFrom::Start(chosen_span.start)) {
                if consecutive_read_failures == 0 {
                    warn!("Seek failed for {}: {}", file_path_owned, e);
                }
                part.local_checksum = String::new();
                part.local_length = 0;
                part.local_start = 0;
                consecutive_read_failures += 1;
                if let Some(progress) = &blocking_progress {
                    progress.mark_parts_done(1);
                }
                continue;
            }

            let mut hasher = FlexHasher::from_checksum(&part.remote_checksum);
            let mut remaining = total_len;
            let mut read_ok = true;

            while remaining > 0 {
                let chunk = remaining.min(HASH_BUF_SIZE);
                if let Err(e) = file.read_exact(&mut buf[..chunk]) {
                    if consecutive_read_failures == 0 {
                        warn!("Read failed for {}: {}", file_path_owned, e);
                    }
                    read_ok = false;
                    break;
                }
                hasher.update(&buf[..chunk]);
                remaining -= chunk;
            }

            let local_checksum = hasher.finalize_hex();

            if !read_ok {
                part.local_checksum = String::new();
                part.local_length = 0;
                part.local_start = 0;
                consecutive_read_failures += 1;
                if consecutive_read_failures == MAX_READ_FAILURES {
                    let remaining_parts = total_part_count.saturating_sub(*idx + 1);
                    warn!(
                        "Read failures exceeded limit ({}) for {}; skipping {} remaining parts",
                        MAX_READ_FAILURES, file_path_owned, remaining_parts
                    );
                }
                if let Some(progress) = &blocking_progress {
                    progress.mark_parts_done(1);
                }
                continue;
            }

            consecutive_read_failures = 0;
            part.local_checksum = local_checksum;
            part.local_length = chosen_span.length;
            part.local_start = chosen_span.start;
            if let Some(progress) = &blocking_progress {
                progress.mark_parts_done(1);
            }
        }

        indexed_parts
    })
    .await;
    metrics.blocking_hash_elapsed = blocking_started.elapsed();
    // _permit is dropped here, releasing the semaphore slot

    let mut final_parts = match result {
        Ok(parts) => parts,
        Err(e) => {
            error!("Part hashing task panicked for {}: {}", pbo_name, e);
            metrics.total_elapsed = started_at.elapsed();
            return PartHashCalculation {
                parts: Vec::new(),
                metrics,
            };
        }
    };

    // Restore original order
    final_parts.sort_by_key(|(idx, _)| *idx);

    debug!(
        "Calculating hashes for all files inside {} finished - {} files took {:.2} seconds.",
        pbo_name,
        total_parts,
        started_at.elapsed().as_secs_f64()
    );

    let parts: Vec<_> = final_parts.into_iter().map(|(_, part)| part).collect();
    metrics.hashed_bytes = parts.iter().map(|part| part.local_length).sum();
    metrics.total_elapsed = started_at.elapsed();
    PartHashCalculation { parts, metrics }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn shorter_local_file_hashes_readable_parts_for_delta_planning() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abcdefgh").unwrap();

        let parts = vec![
            FoxyModFilePart {
                remote_length: 4,
                remote_start: 0,
                remote_checksum: "E2FC714C4727EE9395F324CD2E7F331F".to_string(),
                data_order: 0,
                ..Default::default()
            },
            FoxyModFilePart {
                remote_length: 4,
                remote_start: 8,
                remote_checksum: "81DC9BDB52D04DC20036DBD8313ED055".to_string(),
                data_order: 1,
                ..Default::default()
            },
        ];

        let result = calculate_part_hashes(
            parts,
            file.path().to_str().unwrap(),
            Arc::new(Semaphore::new(1)),
            PartSpanSource::RemoteLayout,
            None,
        )
        .await;

        assert_eq!(
            result.parts[0].local_checksum,
            "E2FC714C4727EE9395F324CD2E7F331F"
        );
        assert_eq!(result.parts[0].local_start, 0);
        assert_eq!(result.parts[0].local_length, 4);
        assert!(result.parts[1].local_checksum.is_empty());
        assert_eq!(result.parts[1].local_length, 0);
    }

    // ── PartHashProgress::mark_parts_done ───────────────────────────────

    fn progress_with_channel(
        total_parts: usize,
        total_files: usize,
    ) -> (
        PartHashProgress,
        tokio::sync::broadcast::Receiver<ProgressEvent>,
    ) {
        let (tx, rx) = tokio::sync::broadcast::channel(64);
        let progress = PartHashProgress::new(
            Arc::new(AtomicUsize::new(0)),
            total_parts,
            Arc::new(AtomicUsize::new(0)),
            total_files,
            tx,
        );
        (progress, rx)
    }

    fn recv_checked_parts(
        rx: &mut tokio::sync::broadcast::Receiver<ProgressEvent>,
    ) -> Option<usize> {
        match rx.try_recv() {
            Ok(ProgressEvent::RecheckHashProgress { checked_parts, .. }) => Some(checked_parts),
            _ => None,
        }
    }

    #[test]
    fn mark_parts_done_zero_count_does_not_emit() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mark_parts_done_below_interval_does_not_emit() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(100);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mark_parts_done_crossing_interval_emits() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(512);
        assert_eq!(recv_checked_parts(&mut rx), Some(512));
    }

    #[test]
    fn mark_parts_done_reaching_total_emits_even_below_interval() {
        let (progress, mut rx) = progress_with_channel(100, 4);
        progress.mark_parts_done(100);
        assert_eq!(recv_checked_parts(&mut rx), Some(100));
    }

    #[test]
    fn mark_parts_done_clamps_reported_parts_to_total() {
        let (progress, mut rx) = progress_with_channel(10, 2);
        progress.mark_parts_done(50);
        assert_eq!(recv_checked_parts(&mut rx), Some(10));
    }

    #[test]
    fn mark_parts_done_accumulates_across_calls() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(300);
        assert!(rx.try_recv().is_err());
        progress.mark_parts_done(300);
        // 600 crosses the 512 boundary.
        assert_eq!(recv_checked_parts(&mut rx), Some(600));
    }

    // ── calculate_part_hashes edge cases ────────────────────────────────

    #[tokio::test]
    async fn calculate_part_hashes_empty_parts_returns_empty() {
        let result = calculate_part_hashes(
            Vec::new(),
            "ignored",
            Arc::new(Semaphore::new(1)),
            PartSpanSource::RemoteLayout,
            None,
        )
        .await;
        assert!(result.parts.is_empty());
        assert_eq!(result.metrics.estimated_bytes, 0);
    }

    #[tokio::test]
    async fn calculate_part_hashes_missing_file_clears_local_fields() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.pbo");
        let parts = vec![FoxyModFilePart {
            remote_length: 4,
            remote_start: 0,
            remote_checksum: "E2FC714C4727EE9395F324CD2E7F331F".to_string(),
            local_checksum: "STALE".to_string(),
            local_length: 4,
            local_start: 0,
            data_order: 0,
            ..Default::default()
        }];

        let result = calculate_part_hashes(
            parts,
            missing.to_str().unwrap(),
            Arc::new(Semaphore::new(1)),
            PartSpanSource::RemoteLayout,
            None,
        )
        .await;

        assert_eq!(result.parts.len(), 1);
        assert!(result.parts[0].local_checksum.is_empty());
        assert_eq!(result.parts[0].local_length, 0);
        assert_eq!(result.parts[0].local_start, 0);
    }

    #[tokio::test]
    async fn calculate_part_hashes_matching_single_part_records_local_state() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abcd").unwrap();
        let parts = vec![FoxyModFilePart {
            remote_length: 4,
            remote_start: 0,
            remote_checksum: blake3::hash(b"abcd").to_hex().to_uppercase(),
            data_order: 0,
            ..Default::default()
        }];

        let result = calculate_part_hashes(
            parts,
            file.path().to_str().unwrap(),
            Arc::new(Semaphore::new(1)),
            PartSpanSource::RemoteLayout,
            None,
        )
        .await;

        assert_eq!(result.parts.len(), 1);
        assert_eq!(result.parts[0].local_length, 4);
        assert_eq!(result.parts[0].local_start, 0);
        assert!(!result.parts[0].local_checksum.is_empty());
        assert_eq!(result.metrics.hashed_bytes, 4);
    }
}
