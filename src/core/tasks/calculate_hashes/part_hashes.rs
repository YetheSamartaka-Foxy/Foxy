use super::direct_read::{DirectReadPlan, DirectReader};
use super::format_layout::{
    map_local_part_spans, parse_local_content_layout_from, remote_parts_format_id,
};
use super::*;
use foxy_formats::LocalPartSpan;
use std::io::BufRead;

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
    /// 1 when the file was read around the cache.
    pub(super) direct_files: usize,
    /// 1 when a non-cached read failed and the file was read through the cache.
    pub(super) direct_fallback_files: usize,
}

pub(super) struct PartHashCalculation {
    pub(super) parts: Vec<FoxyModFilePart>,
    pub(super) metrics: PartHashMetrics,
    /// Sampled fingerprint built from the bytes the hash pass read, re-reading
    /// only samples the parts did not cover. `None` when the file could not be
    /// read or the pass was cancelled before it finished.
    pub(super) content_hash: Option<String>,
}

/// Shared counters of one hash run, advanced by every file's hasher.
#[derive(Clone)]
pub(super) struct HashRunCounters {
    pub(super) files_done: Arc<AtomicUsize>,
    pub(super) total_files: usize,
    pub(super) parts_done: Arc<AtomicUsize>,
    pub(super) total_parts: usize,
    pub(super) bytes_done: Arc<AtomicU64>,
    pub(super) total_bytes: u64,
}

impl HashRunCounters {
    pub(super) fn event(&self) -> ProgressEvent {
        ProgressEvent::RecheckHashProgress {
            checked_files: self
                .files_done
                .load(Ordering::Relaxed)
                .min(self.total_files),
            total_files: self.total_files,
            checked_parts: self
                .parts_done
                .load(Ordering::Relaxed)
                .min(self.total_parts),
            total_parts: self.total_parts,
            checked_bytes: self
                .bytes_done
                .load(Ordering::Relaxed)
                .min(self.total_bytes),
            total_bytes: self.total_bytes,
        }
    }
}

#[derive(Clone)]
pub(super) struct PartHashProgress {
    counters: HashRunCounters,
    progress_tx: Sender<ProgressEvent>,
}

impl PartHashProgress {
    pub(super) fn new(counters: HashRunCounters, progress_tx: Sender<ProgressEvent>) -> Self {
        Self {
            counters,
            progress_tx,
        }
    }

    /// `bytes` is the estimated size of the parts, so a file of a few huge
    /// parts still moves a byte-weighted bar between part intervals.
    pub(super) fn mark_parts_done(&self, count: usize, bytes: u64) {
        if count == 0 && bytes == 0 {
            return;
        }

        const PART_PROGRESS_INTERVAL: usize = 512;
        const BYTE_PROGRESS_INTERVAL: u64 = 64 * 1024 * 1024;
        let counters = &self.counters;
        let checked_parts = counters
            .parts_done
            .fetch_add(count, Ordering::Relaxed)
            .saturating_add(count)
            .min(counters.total_parts);
        let previous_parts = checked_parts.saturating_sub(count);
        let checked_bytes = counters
            .bytes_done
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes)
            .min(counters.total_bytes);
        let previous_bytes = checked_bytes.saturating_sub(bytes);
        let crossed_parts =
            checked_parts / PART_PROGRESS_INTERVAL != previous_parts / PART_PROGRESS_INTERVAL;
        let crossed_bytes =
            checked_bytes / BYTE_PROGRESS_INTERVAL != previous_bytes / BYTE_PROGRESS_INTERVAL;

        if crossed_parts || crossed_bytes || checked_parts == counters.total_parts {
            let _ = self.progress_tx.send(counters.event());
        }
    }
}

pub(super) const HASH_READER_CAPACITY: usize = 4 * 1024 * 1024;
/// On a rotational disk the two hash workers take turns on one head, so each
/// larger request pays for its seek with more data.
pub(super) const ROTATIONAL_HASH_READER_CAPACITY: usize = 128 * 1024 * 1024;

/// How one file's parts are located and read.
#[derive(Clone, Copy)]
pub(super) struct PartReadOptions<'a> {
    pub(super) span_source: PartSpanSource,
    /// The active game's declared container formats.
    pub(super) game_formats: &'a [&'static str],
    pub(super) reader_capacity: usize,
    /// Open with the sequential-scan hint, meant for rotational storage.
    pub(super) sequential_scan: bool,
    /// Read around the cache instead, when the file is long enough.
    pub(super) direct: Option<&'a DirectReadPlan>,
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
    read: PartReadOptions<'_>,
    progress: Option<PartHashProgress>,
    cancel: Option<watch::Receiver<bool>>,
) -> PartHashCalculation {
    let PartReadOptions {
        span_source,
        game_formats,
        reader_capacity,
        sequential_scan,
        direct,
    } = read;
    let direct = direct.cloned();
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
            content_hash: None,
        };
    }

    let metadata_started = Instant::now();
    let file_metadata = match tokio::fs::metadata(file_path).await {
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
            meta
        }
        Err(_) => {
            debug!("File {} does not exist, skipping hash", file_path);
            metrics.metadata_elapsed = metadata_started.elapsed();
            metrics.total_elapsed = started_at.elapsed();
            if let Some(progress) = &progress {
                progress.mark_parts_done(total_parts, metrics.estimated_bytes);
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
                content_hash: None,
            };
        }
    };
    metrics.metadata_elapsed = metadata_started.elapsed();

    let remote_format_id = remote_parts_format_id(file_path, &parts, game_formats);
    let layout_format = match (span_source, remote_format_id) {
        (PartSpanSource::DetectLocalLayout, Some(format_id)) => Some(format_id),
        _ => None,
    };
    if span_source == PartSpanSource::RemoteLayout && remote_format_id.is_some() {
        metrics.remote_span_files = 1;
        debug!(
            "Using remote part spans for freshly materialized download {} (parts={})",
            pbo_name, total_parts
        );
    }

    // Attach original index to preserve order after offset-sorted processing
    let mut indexed_parts: Vec<(usize, FoxyModFilePart)> = parts.into_iter().enumerate().collect();

    let file_path_owned = file_path.to_string();
    let file_name = pbo_name.to_string();

    // Acquire one semaphore permit for the entire file - sequential processing
    // uses a single blocking thread instead of one per part.
    let semaphore_started = Instant::now();
    let Ok(_permit) = semaphore.acquire().await else {
        warn!("Hash semaphore closed, skipping file: {}", file_path);
        metrics.semaphore_wait_elapsed = semaphore_started.elapsed();
        metrics.total_elapsed = started_at.elapsed();
        if let Some(progress) = &progress {
            progress.mark_parts_done(total_parts, metrics.estimated_bytes);
        }
        return PartHashCalculation {
            parts: indexed_parts.into_iter().map(|(_, part)| part).collect(),
            metrics,
            content_hash: None,
        };
    };
    metrics.semaphore_wait_elapsed = semaphore_started.elapsed();

    // Open the file once and hash all parts sequentially within a single
    // spawn_blocking task.  This eliminates N redundant open/close syscalls
    // (one per part) and lets the OS read-ahead prefetcher work efficiently.
    let blocking_started = Instant::now();
    let blocking_progress = progress.clone();
    let estimated_bytes = metrics.estimated_bytes;
    let result = tokio::task::spawn_blocking(move || {
        let mut layout_metrics = LayoutMetrics::default();
        let file_len = file_metadata.len();
        let direct_reader = direct
            .filter(|plan| file_len >= plan.min_len)
            .and_then(|plan| DirectReader::open(&file_path_owned, file_len, &plan));
        let open_cached = || {
            HashReader::open_cached(&file_path_owned, file_len, reader_capacity, sequential_scan)
        };
        let mut reader = match direct_reader {
            Some(reader) => HashReader::Direct(reader),
            None => match open_cached() {
                Ok(reader) => reader,
                Err(e) => {
                    warn!("Failed to open file {}: {}", file_path_owned, e);
                    let total_part_count = indexed_parts.len();
                    if let Some(progress) = &blocking_progress {
                        progress.mark_parts_done(total_part_count, estimated_bytes);
                    }
                    return (indexed_parts, None, layout_metrics);
                }
            },
        };
        layout_metrics.direct = matches!(reader, HashReader::Direct(_));
        let mut span_overrides = match layout_format {
            Some(format_id) => resolve_local_spans(
                format_id,
                &mut reader,
                file_len,
                &indexed_parts,
                &file_name,
                &mut layout_metrics,
            ),
            None => vec![None; indexed_parts.len()],
        };
        // A disk or filter that refuses non-cached reads must not turn into
        // parts placed at remote offsets, so the layout is read again.
        if let Some(format_id) = layout_format
            && reader.direct_failed()
            && let Ok(cached) = open_cached()
        {
            warn!(
                "Non-cached read failed for {}; reading it through the cache",
                file_name
            );
            reader = cached;
            layout_metrics = LayoutMetrics {
                direct: true,
                direct_fallback: true,
                ..Default::default()
            };
            span_overrides = resolve_local_spans(
                format_id,
                &mut reader,
                file_len,
                &indexed_parts,
                &file_name,
                &mut layout_metrics,
            );
        }
        // Sort parts by effective start offset for sequential disk I/O.
        indexed_parts.sort_by_key(|(idx, part)| {
            span_overrides
                .get(*idx)
                .and_then(|span| *span)
                .map(|s| s.start)
                .unwrap_or(part.remote_start)
        });
        let mut reader_pos: Option<u64> = if layout_metrics.attempted {
            reader.stream_position().ok()
        } else {
            Some(0)
        };
        // Timed per file rather than per part: a 562-entry PBO would otherwise
        // charge the profiler more events than the hashing does work.
        let profiled = crate::core::utils::profiling::FsTimer::start();

        let mut fingerprint = crate::core::utils::content_hash::FingerprintTap::new(&file_metadata);
        // Track consecutive read failures; after too many, skip remaining parts
        // to avoid log spam and wasted I/O on corrupted/truncated files.
        const MAX_READ_FAILURES: usize = 3;
        let mut consecutive_read_failures: usize = 0;
        let total_part_count = indexed_parts.len();
        let mut cancelled = false;

        for (idx, part) in &mut indexed_parts {
            // A multi-gigabyte PBO would otherwise keep the disk busy for
            // its whole length after the user asked to stop.
            if cancel.as_ref().is_some_and(|rx| *rx.borrow()) {
                cancelled = true;
                part.local_checksum = String::new();
                part.local_length = 0;
                part.local_start = 0;
                if let Some(progress) = &blocking_progress {
                    progress.mark_parts_done(1, part.remote_length);
                }
                continue;
            }
            // Bail early if the file is consistently unreadable
            if consecutive_read_failures >= MAX_READ_FAILURES {
                part.local_checksum = String::new();
                part.local_length = 0;
                part.local_start = 0;
                if let Some(progress) = &blocking_progress {
                    progress.mark_parts_done(1, part.remote_length);
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
                        progress.mark_parts_done(1, part.remote_length);
                    }
                    continue;
                }
            };

            let mut hashed = hash_span(
                &mut reader,
                &mut reader_pos,
                chosen_span,
                total_len,
                &part.remote_checksum,
                &mut fingerprint,
            );
            if hashed.is_err()
                && reader.direct_failed()
                && let Ok(cached) = open_cached()
            {
                warn!(
                    "Non-cached read failed for {}; reading it through the cache",
                    file_name
                );
                reader = cached;
                reader_pos = None;
                layout_metrics.direct_fallback = true;
                hashed = hash_span(
                    &mut reader,
                    &mut reader_pos,
                    chosen_span,
                    total_len,
                    &part.remote_checksum,
                    &mut fingerprint,
                );
            }

            let local_checksum = match hashed {
                Ok(checksum) => checksum,
                Err(e) => {
                    if consecutive_read_failures == 0 {
                        warn!("Read failed for {}: {}", file_path_owned, e);
                    }
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
                        progress.mark_parts_done(1, part.remote_length);
                    }
                    continue;
                }
            };

            consecutive_read_failures = 0;
            part.local_checksum = local_checksum;
            part.local_length = chosen_span.length;
            part.local_start = chosen_span.start;
            if let Some(progress) = &blocking_progress {
                progress.mark_parts_done(1, part.remote_length);
            }
        }

        profiled.stop(
            "hash_read",
            indexed_parts
                .iter()
                .map(|(_, part)| part.local_length)
                .sum::<u64>(),
        );
        let content_hash = if cancelled || consecutive_read_failures >= MAX_READ_FAILURES {
            None
        } else {
            fingerprint
                .finish()
                .or_else(|| reader.sampled_fingerprint(&file_path_owned, &file_metadata))
        };
        (indexed_parts, content_hash, layout_metrics)
    })
    .await;
    let blocking_elapsed = blocking_started.elapsed();
    // _permit is dropped here, releasing the semaphore slot

    let (mut final_parts, content_hash, layout_metrics) = match result {
        Ok(parts) => parts,
        Err(e) => {
            error!("Part hashing task panicked for {}: {}", pbo_name, e);
            metrics.blocking_hash_elapsed = blocking_elapsed;
            metrics.total_elapsed = started_at.elapsed();
            return PartHashCalculation {
                parts: Vec::new(),
                metrics,
                content_hash: None,
            };
        }
    };
    metrics.direct_files = usize::from(layout_metrics.direct);
    metrics.direct_fallback_files = usize::from(layout_metrics.direct_fallback);
    metrics.layout_files = usize::from(layout_metrics.attempted);
    metrics.layout_parse_elapsed = layout_metrics.parse_elapsed;
    metrics.layout_map_elapsed = layout_metrics.map_elapsed;
    metrics.layout_elapsed = layout_metrics.parse_elapsed + layout_metrics.map_elapsed;
    metrics.layout_entries = layout_metrics.entries;
    metrics.layout_entry_payload_bytes = layout_metrics.entry_payload_bytes;
    metrics.mapped_parts = layout_metrics.mapped_parts;
    metrics.fallback_parts = layout_metrics.fallback_parts;
    metrics.blocking_hash_elapsed = blocking_elapsed.saturating_sub(metrics.layout_elapsed);
    if metrics.layout_files > 0 && (total_parts >= 64 || metrics.layout_elapsed.as_millis() >= 100)
    {
        info!(
            "Content layout metrics: file={} parts={} entries={} entry_payload_bytes={} mapped_parts={} fallback_parts={} parse={:.3}s map={:.3}s total={:.3}s",
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
    PartHashCalculation {
        parts,
        metrics,
        content_hash,
    }
}

/// The reader one file's parts are hashed through.
enum HashReader {
    Cached(std::io::BufReader<std::fs::File>),
    Direct(DirectReader),
}

impl HashReader {
    fn open_cached(
        path: &str,
        file_len: u64,
        capacity: usize,
        sequential_scan: bool,
    ) -> std::io::Result<Self> {
        let file = if sequential_scan {
            crate::core::utils::content_hash::open_for_sequential_read(path)?
        } else {
            std::fs::File::open(path)?
        };
        // A small file never fills a large buffer, so it does not allocate one.
        let capacity = usize::try_from(file_len)
            .unwrap_or(usize::MAX)
            .clamp(8 * 1024, capacity.max(8 * 1024));
        Ok(Self::Cached(std::io::BufReader::with_capacity(
            capacity, file,
        )))
    }

    fn direct_failed(&self) -> bool {
        matches!(self, Self::Direct(reader) if reader.failed())
    }

    /// The sampled fingerprint read directly, for a file whose parts left a
    /// sample unread. The direct reader would fetch a whole block per sample.
    fn sampled_fingerprint(&mut self, path: &str, metadata: &std::fs::Metadata) -> Option<String> {
        match self {
            Self::Cached(reader) => {
                crate::core::utils::content_hash::fast_file_content_hash_from_buffered(
                    reader, metadata,
                )
                .ok()
            }
            Self::Direct(_) => {
                let mut file = std::fs::File::open(path).ok()?;
                crate::core::utils::content_hash::fast_file_content_hash_from_reader(
                    &mut file, metadata,
                )
                .ok()
            }
        }
    }
}

impl Read for HashReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Cached(reader) => reader.read(out),
            Self::Direct(reader) => reader.read(out),
        }
    }
}

impl std::io::BufRead for HashReader {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        match self {
            Self::Cached(reader) => reader.fill_buf(),
            Self::Direct(reader) => reader.fill_buf(),
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            Self::Cached(reader) => reader.consume(amount),
            Self::Direct(reader) => reader.consume(amount),
        }
    }
}

impl Seek for HashReader {
    fn seek(&mut self, from: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::Cached(reader) => reader.seek(from),
            Self::Direct(reader) => reader.seek(from),
        }
    }

    fn seek_relative(&mut self, offset: i64) -> std::io::Result<()> {
        match self {
            Self::Cached(reader) => reader.seek_relative(offset),
            Self::Direct(reader) => reader.seek_relative(offset),
        }
    }
}

/// Seeks to `span` and hashes its `len` bytes straight out of the reader's
/// buffer, recording them in the fingerprint tap. Returns the part checksum.
fn hash_span(
    reader: &mut HashReader,
    reader_pos: &mut Option<u64>,
    span: LocalPartSpan,
    len: usize,
    remote_checksum: &str,
    fingerprint: &mut crate::core::utils::content_hash::FingerprintTap,
) -> std::io::Result<String> {
    let seeked = match *reader_pos {
        Some(pos) if pos == span.start => Ok(()),
        Some(pos) => match i64::try_from(span.start as i128 - pos as i128) {
            Ok(delta) => reader.seek_relative(delta),
            Err(_) => reader
                .seek(std::io::SeekFrom::Start(span.start))
                .map(|_| ()),
        },
        None => reader
            .seek(std::io::SeekFrom::Start(span.start))
            .map(|_| ()),
    };
    *reader_pos = None;
    seeked?;
    let mut hasher = FlexHasher::from_checksum(remote_checksum);
    let mut remaining = len;
    while remaining > 0 {
        let available = match reader.fill_buf()? {
            [] => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            available => available,
        };
        let chunk = remaining.min(available.len());
        hasher.update(&available[..chunk]);
        fingerprint.observe(span.start + (len - remaining) as u64, &available[..chunk]);
        reader.consume(chunk);
        remaining -= chunk;
    }
    *reader_pos = Some(span.start.saturating_add(span.length));
    Ok(hasher.finalize_hex())
}

#[derive(Default)]
struct LayoutMetrics {
    direct: bool,
    /// A non-cached read failed and the file was read through the cache.
    direct_fallback: bool,
    attempted: bool,
    parse_elapsed: std::time::Duration,
    map_elapsed: std::time::Duration,
    entries: usize,
    entry_payload_bytes: u64,
    mapped_parts: usize,
    fallback_parts: usize,
}

/// Parses the archive layout through the hash reader, so the header read runs
/// straight on into the payload instead of costing a second open and a second
/// trip to the start of the file. `indexed_parts` must be in original order.
fn resolve_local_spans(
    format_id: &str,
    reader: &mut dyn foxy_formats::BufReadSeek,
    file_len: u64,
    indexed_parts: &[(usize, FoxyModFilePart)],
    file_name: &str,
    metrics: &mut LayoutMetrics,
) -> Vec<Option<LocalPartSpan>> {
    metrics.attempted = true;
    let total_parts = indexed_parts.len();
    let parse_started = Instant::now();
    let parsed = parse_local_content_layout_from(format_id, reader, file_len);
    metrics.parse_elapsed = parse_started.elapsed();
    match parsed {
        Ok(layout) => {
            metrics.entries = layout.entry_count;
            metrics.entry_payload_bytes = layout.entry_payload_bytes;
            let map_started = Instant::now();
            let spans = map_local_part_spans(indexed_parts.iter().map(|(_, part)| part), &layout);
            metrics.map_elapsed = map_started.elapsed();
            metrics.mapped_parts = spans.iter().filter(|span| span.is_some()).count();
            metrics.fallback_parts = total_parts.saturating_sub(metrics.mapped_parts);
            debug!(
                "Local {} span remap for {}: mapped_parts={} fallback_parts={} header_len={} end_start={} end_len={}",
                format_id,
                file_name,
                metrics.mapped_parts,
                metrics.fallback_parts,
                layout.header.length,
                layout.end.start,
                layout.end.length
            );
            spans
        }
        Err(err) => {
            warn!(
                "Local {} span remap failed for {}: {}. Falling back to remote offsets.",
                format_id, file_name, err
            );
            metrics.fallback_parts = total_parts;
            vec![None; total_parts]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pbo_entry(bytes: &mut Vec<u8>, name: &[u8], length: u32) {
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.extend_from_slice(&length.to_le_bytes());
    }

    #[tokio::test]
    async fn local_layout_is_parsed_through_the_hash_reader_and_every_part_matches() {
        let mut bytes = vec![0u8];
        bytes.extend_from_slice(b"sreV");
        bytes.extend_from_slice(&[0u8; 17]);
        pbo_entry(&mut bytes, b"a.txt", 5);
        pbo_entry(&mut bytes, b"b.txt", 3);
        pbo_entry(&mut bytes, b"", 0);
        let header_len = bytes.len();
        bytes.extend_from_slice(b"alphabet");
        bytes.extend_from_slice(b"TAIL");
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();

        let spans = [
            ("$$HEADER$$", 0, header_len),
            ("a.txt", header_len, 5),
            ("b.txt", header_len + 5, 3),
            ("$$END$$", header_len + 8, 4),
        ];
        let parts: Vec<FoxyModFilePart> = spans
            .iter()
            .enumerate()
            .map(|(order, (name, start, len))| FoxyModFilePart {
                path: crate::core::models::modification_file_part::part_storage_path(
                    name,
                    order as i64,
                ),
                // Remote offsets from another build of the archive; only the
                // local layout can place these parts.
                remote_start: *start as u64 + 100,
                remote_length: *len as u64,
                remote_checksum: blake3::hash(&bytes[*start..*start + *len])
                    .to_hex()
                    .to_uppercase(),
                data_order: order as i64,
                ..Default::default()
            })
            .collect();

        let direct = DirectReadPlan::new(64 * 1024, 2, 0, true);
        let mut failing = direct.clone();
        failing.fail_at = Some(0);
        let plans = if cfg!(windows) {
            vec![None, Some(direct), Some(failing)]
        } else {
            vec![None]
        };
        for plan in plans {
            let result = calculate_part_hashes(
                parts.clone(),
                file.path().to_str().unwrap(),
                Arc::new(Semaphore::new(1)),
                PartReadOptions {
                    span_source: PartSpanSource::DetectLocalLayout,
                    game_formats: &[foxy_formats::PBO_FORMAT_ID],
                    reader_capacity: HASH_READER_CAPACITY,
                    sequential_scan: false,
                    direct: plan.as_ref(),
                },
                None,
                None,
            )
            .await;

            assert_eq!(result.metrics.layout_files, 1);
            assert_eq!(result.metrics.mapped_parts, 4);
            assert_eq!(result.metrics.fallback_parts, 0);
            for (part, (name, start, len)) in result.parts.iter().zip(spans) {
                assert_eq!(part.local_start, start as u64, "{name}");
                assert_eq!(part.local_length, len as u64, "{name}");
                assert_eq!(part.local_checksum, part.remote_checksum, "{name}");
            }
            assert_eq!(
                result.content_hash.as_deref(),
                Some(
                    crate::core::utils::content_hash::fast_file_content_hash(
                        file.path().to_str().unwrap()
                    )
                    .unwrap()
                    .as_str()
                )
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn direct_reads_hash_every_part_and_the_fingerprint_like_cached_reads() {
        let bytes: Vec<u8> = (0..900_001usize).map(|i| (i * 7 + i / 13) as u8).collect();
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        // Out of order, one past the end, and a gap between 700_000 and 710_000.
        let spans = [
            (400_000usize, 300_000usize),
            (0, 131_072),
            (131_072, 268_928),
            (710_000, 190_001),
            (899_990, 20),
        ];
        let parts = || -> Vec<FoxyModFilePart> {
            spans
                .iter()
                .enumerate()
                .map(|(order, (start, len))| FoxyModFilePart {
                    remote_start: *start as u64,
                    remote_length: *len as u64,
                    remote_checksum: blake3::hash(&bytes[*start..(*start + *len).min(bytes.len())])
                        .to_hex()
                        .to_uppercase(),
                    data_order: order as i64,
                    ..Default::default()
                })
                .collect()
        };
        let hash_with = |direct: Option<DirectReadPlan>| {
            let parts = parts();
            let path = file.path().to_str().unwrap().to_owned();
            async move {
                calculate_part_hashes(
                    parts,
                    &path,
                    Arc::new(Semaphore::new(1)),
                    PartReadOptions {
                        span_source: PartSpanSource::RemoteLayout,
                        game_formats: &[],
                        reader_capacity: HASH_READER_CAPACITY,
                        sequential_scan: false,
                        direct: direct.as_ref(),
                    },
                    None,
                    None,
                )
                .await
            }
        };

        let cached = hash_with(None).await;
        for (depth, one_stream, fail_at) in [
            (1, false, None),
            (2, true, None),
            (4, false, None),
            (2, true, Some(0)),
            (2, false, Some(500_000)),
        ] {
            let mut plan = DirectReadPlan::new(64 * 1024, depth, 0, one_stream);
            plan.fail_at = fail_at;
            let direct = hash_with(Some(plan)).await;
            assert_eq!(direct.metrics.direct_files, 1);
            assert_eq!(
                direct.metrics.direct_fallback_files,
                usize::from(fail_at.is_some())
            );
            assert_eq!(direct.content_hash, cached.content_hash);
            for (direct_part, cached_part) in direct.parts.iter().zip(&cached.parts) {
                assert_eq!(direct_part.local_checksum, cached_part.local_checksum);
                assert_eq!(direct_part.local_start, cached_part.local_start);
                assert_eq!(direct_part.local_length, cached_part.local_length);
            }
        }
        assert!(
            cached.parts[..4]
                .iter()
                .all(|part| part.local_checksum == part.remote_checksum)
        );
        assert!(cached.parts[4].local_checksum.is_empty());
        assert_eq!(
            cached.content_hash.as_deref(),
            Some(
                crate::core::utils::content_hash::fast_file_content_hash(
                    file.path().to_str().unwrap()
                )
                .unwrap()
                .as_str()
            )
        );
    }

    #[tokio::test]
    async fn unreadable_local_layout_falls_back_to_remote_offsets() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"not a pbo at all").unwrap();
        let parts = vec![
            FoxyModFilePart {
                path: "$$HEADER$$".to_string(),
                remote_start: 0,
                remote_length: 4,
                remote_checksum: blake3::hash(b"not ").to_hex().to_uppercase(),
                data_order: 0,
                ..Default::default()
            },
            FoxyModFilePart {
                path: "$$END$$".to_string(),
                remote_start: 4,
                remote_length: 12,
                remote_checksum: blake3::hash(b"a pbo at all").to_hex().to_uppercase(),
                data_order: 1,
                ..Default::default()
            },
        ];

        let result = calculate_part_hashes(
            parts,
            file.path().to_str().unwrap(),
            Arc::new(Semaphore::new(1)),
            PartReadOptions {
                span_source: PartSpanSource::DetectLocalLayout,
                game_formats: &[foxy_formats::PBO_FORMAT_ID],
                reader_capacity: HASH_READER_CAPACITY,
                sequential_scan: false,
                direct: None,
            },
            None,
            None,
        )
        .await;

        assert_eq!(result.metrics.layout_files, 1);
        assert_eq!(result.metrics.fallback_parts, 2);
        for part in &result.parts {
            assert_eq!(part.local_checksum, part.remote_checksum);
        }
    }

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
            PartReadOptions {
                span_source: PartSpanSource::RemoteLayout,
                game_formats: &[],
                reader_capacity: HASH_READER_CAPACITY,
                sequential_scan: false,
                direct: None,
            },
            None,
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
            HashRunCounters {
                files_done: Arc::new(AtomicUsize::new(0)),
                total_files,
                parts_done: Arc::new(AtomicUsize::new(0)),
                total_parts,
                bytes_done: Arc::new(AtomicU64::new(0)),
                total_bytes: 1 << 40,
            },
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
        progress.mark_parts_done(0, 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mark_parts_done_below_interval_does_not_emit() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(100, 0);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mark_parts_done_crossing_interval_emits() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(512, 0);
        assert_eq!(recv_checked_parts(&mut rx), Some(512));
    }

    #[test]
    fn mark_parts_done_reaching_total_emits_even_below_interval() {
        let (progress, mut rx) = progress_with_channel(100, 4);
        progress.mark_parts_done(100, 0);
        assert_eq!(recv_checked_parts(&mut rx), Some(100));
    }

    #[test]
    fn mark_parts_done_clamps_reported_parts_to_total() {
        let (progress, mut rx) = progress_with_channel(10, 2);
        progress.mark_parts_done(50, 0);
        assert_eq!(recv_checked_parts(&mut rx), Some(10));
    }

    #[test]
    fn mark_parts_done_accumulates_across_calls() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(300, 0);
        assert!(rx.try_recv().is_err());
        progress.mark_parts_done(300, 0);
        // 600 crosses the 512 boundary.
        assert_eq!(recv_checked_parts(&mut rx), Some(600));
    }

    #[test]
    fn mark_parts_done_emits_on_byte_intervals_between_part_intervals() {
        let (progress, mut rx) = progress_with_channel(1000, 10);
        progress.mark_parts_done(1, 32 * 1024 * 1024);
        assert!(rx.try_recv().is_err());
        progress.mark_parts_done(1, 32 * 1024 * 1024);
        match rx.try_recv() {
            Ok(ProgressEvent::RecheckHashProgress {
                checked_parts,
                checked_bytes,
                ..
            }) => {
                assert_eq!(checked_parts, 2);
                assert_eq!(checked_bytes, 64 * 1024 * 1024);
            }
            other => panic!("expected a progress event, got {other:?}"),
        }
    }

    // ── calculate_part_hashes edge cases ────────────────────────────────

    #[tokio::test]
    async fn calculate_part_hashes_empty_parts_returns_empty() {
        let result = calculate_part_hashes(
            Vec::new(),
            "ignored",
            Arc::new(Semaphore::new(1)),
            PartReadOptions {
                span_source: PartSpanSource::RemoteLayout,
                game_formats: &[],
                reader_capacity: HASH_READER_CAPACITY,
                sequential_scan: false,
                direct: None,
            },
            None,
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
            PartReadOptions {
                span_source: PartSpanSource::RemoteLayout,
                game_formats: &[],
                reader_capacity: HASH_READER_CAPACITY,
                sequential_scan: false,
                direct: None,
            },
            None,
            None,
        )
        .await;

        assert_eq!(result.parts.len(), 1);
        assert!(result.parts[0].local_checksum.is_empty());
        assert_eq!(result.parts[0].local_length, 0);
        assert_eq!(result.parts[0].local_start, 0);
    }

    #[tokio::test]
    async fn calculate_part_hashes_cancelled_before_start_clears_parts_and_fingerprint() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abcd").unwrap();
        let parts = vec![FoxyModFilePart {
            remote_length: 4,
            remote_start: 0,
            remote_checksum: blake3::hash(b"abcd").to_hex().to_uppercase(),
            data_order: 0,
            ..Default::default()
        }];
        let (_cancel_tx, cancel_rx) = watch::channel(true);

        let result = calculate_part_hashes(
            parts,
            file.path().to_str().unwrap(),
            Arc::new(Semaphore::new(1)),
            PartReadOptions {
                span_source: PartSpanSource::RemoteLayout,
                game_formats: &[],
                reader_capacity: HASH_READER_CAPACITY,
                sequential_scan: false,
                direct: None,
            },
            None,
            Some(cancel_rx),
        )
        .await;

        assert_eq!(result.parts.len(), 1);
        assert!(result.parts[0].local_checksum.is_empty());
        assert!(result.content_hash.is_none());
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
            PartReadOptions {
                span_source: PartSpanSource::RemoteLayout,
                game_formats: &[],
                reader_capacity: HASH_READER_CAPACITY,
                sequential_scan: false,
                direct: None,
            },
            None,
            None,
        )
        .await;

        assert_eq!(result.parts.len(), 1);
        assert_eq!(result.parts[0].local_length, 4);
        assert_eq!(result.parts[0].local_start, 0);
        assert_eq!(
            result.content_hash.as_deref(),
            Some(
                crate::core::utils::content_hash::fast_file_content_hash(
                    file.path().to_str().unwrap()
                )
                .unwrap()
                .as_str()
            )
        );
        assert!(!result.parts[0].local_checksum.is_empty());
        assert_eq!(result.metrics.hashed_bytes, 4);
    }
}
