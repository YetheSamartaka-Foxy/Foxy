use crate::core::api::ProgressEvent;
use log::debug;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::DownloadResourceLimits;

/// Lightweight download telemetry collected during a single download run.
///
/// Designed to be shared via `Arc<DownloadMetrics>` across the orchestrator,
/// batching, and transfer layers. All counters are lock-free atomics; event
/// vectors use a `Mutex` that is held only for the short push.
pub(crate) struct DownloadMetrics {
    started_at: Instant,
    pub(super) counters: DownloadCounters,
    file_events: Mutex<Vec<FileMetric>>,
    range_events: Mutex<Vec<RangeMetric>>,
    phase_events: Mutex<Vec<PhaseMetric>>,
    sampler_stop: AtomicBool,
}

#[derive(Clone, Debug)]
pub(crate) struct DownloadModOutcome {
    pub(crate) mod_id: u64,
    pub(crate) mod_name: String,
    pub(crate) success: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DownloadRunReport {
    pub(crate) total: String,
    pub(crate) addon_summaries: Vec<String>,
}

impl DownloadRunReport {
    pub(crate) fn render(&self) -> String {
        let mut lines = Vec::new();
        lines.push("-- DOWNLOAD REPORT --".to_owned());
        if !self.addon_summaries.is_empty() {
            lines.push("".to_owned());
            lines.push("ADDONS".to_owned());
            lines.extend(self.addon_summaries.iter().cloned());
        }
        if !self.total.is_empty() {
            lines.push("".to_owned());
            lines.push(self.total.clone());
        }
        lines.push("-- END DOWNLOAD REPORT --".to_owned());
        lines.join("\n")
    }
}

/// Lock-free counters polled by the throughput sampler.
pub(super) struct DownloadCounters {
    pub(super) active_files: AtomicUsize,
    pub(super) active_ranges: AtomicUsize,
    pub(super) bytes_transferred: AtomicU64,
    /// Best network throughput observed in any single 1-second sampler window
    /// (bytes/sec). Serves as the demonstrated link capacity ("light") for the
    /// end-of-run SOL ratio when no bandwidth limiter is configured.
    pub(super) peak_network_bps: AtomicU64,
    pub(super) disk_bytes_written: AtomicU64,
    pub(super) files_completed: AtomicUsize,
    pub(super) range_retries: AtomicUsize,
    pub(super) db_checkpoint_ms: AtomicU64,
    pub(super) db_checkpoint_batches: AtomicUsize,
    pub(super) db_checkpoint_rows: AtomicUsize,
    pub(super) db_checkpoint_statements: AtomicUsize,
}

/// Per-file telemetry recorded when a file download completes or fails.
pub(super) struct FileMetric {
    pub(super) file_id: u64,
    pub(super) mod_id: u64,
    pub(super) size: usize,
    pub(super) expected_network_bytes: usize,
    pub(super) method: &'static str,
    pub(super) split_count: usize,
    pub(super) permit_wait: Duration,
    pub(super) first_byte_latency: Option<Duration>,
    pub(super) transfer_time: Duration,
    pub(super) promote_time: Duration,
    pub(super) disk_write_time: Duration,
    pub(super) disk_write_count: usize,
    pub(super) retries: usize,
    pub(super) avg_mbps: f64,
}

/// Per-range telemetry recorded when a single HTTP range request completes.
pub(super) struct RangeMetric {
    pub(super) file_id: u64,
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) request_latency: Duration,
    pub(super) transfer_time: Duration,
    pub(super) write_time: Duration,
    pub(super) write_count: usize,
    pub(super) retries: usize,
    pub(super) bytes: u64,
}

/// Phase timing for top-level orchestrator steps.
struct PhaseMetric {
    name: &'static str,
    elapsed: Duration,
}

/// RAII guard that records phase duration on drop.
pub(super) struct PhaseGuard<'a> {
    metrics: &'a DownloadMetrics,
    name: &'static str,
    started: Instant,
}

impl Drop for PhaseGuard<'_> {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        if let Ok(mut phases) = self.metrics.phase_events.lock() {
            phases.push(PhaseMetric {
                name: self.name,
                elapsed,
            });
        }
    }
}

impl DownloadMetrics {
    pub(super) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            counters: DownloadCounters {
                active_files: AtomicUsize::new(0),
                active_ranges: AtomicUsize::new(0),
                bytes_transferred: AtomicU64::new(0),
                peak_network_bps: AtomicU64::new(0),
                disk_bytes_written: AtomicU64::new(0),
                files_completed: AtomicUsize::new(0),
                range_retries: AtomicUsize::new(0),
                db_checkpoint_ms: AtomicU64::new(0),
                db_checkpoint_batches: AtomicUsize::new(0),
                db_checkpoint_rows: AtomicUsize::new(0),
                db_checkpoint_statements: AtomicUsize::new(0),
            },
            file_events: Mutex::new(Vec::new()),
            range_events: Mutex::new(Vec::new()),
            phase_events: Mutex::new(Vec::new()),
            sampler_stop: AtomicBool::new(false),
        }
    }

    /// Start timing a named phase. Duration is recorded when the guard drops.
    pub(super) fn phase(&self, name: &'static str) -> PhaseGuard<'_> {
        PhaseGuard {
            metrics: self,
            name,
            started: Instant::now(),
        }
    }

    /// Record a completed (or failed) file download.
    pub(super) fn record_file(&self, metric: FileMetric) {
        if metric.split_count <= 1 && metric.disk_write_time > Duration::ZERO {
            self.counters
                .disk_bytes_written
                .fetch_add(metric.size as u64, Ordering::Relaxed);
        }
        if let Ok(mut files) = self.file_events.lock() {
            files.push(metric);
        }
    }

    /// Record a completed range request.
    pub(super) fn record_range(&self, metric: RangeMetric) {
        if metric.write_time > Duration::ZERO {
            self.counters
                .disk_bytes_written
                .fetch_add(metric.bytes, Ordering::Relaxed);
        }
        if let Ok(mut ranges) = self.range_events.lock() {
            ranges.push(metric);
        }
    }

    /// Record bytes transferred (called from transfer layer per chunk).
    pub(crate) fn record_bytes(&self, n: u64) {
        self.counters
            .bytes_transferred
            .fetch_add(n, Ordering::Relaxed);
    }

    /// Spawn a background task that logs a throughput sample every second.
    /// Active regardless of whether a bandwidth limiter is configured.
    ///
    /// `telemetry_epoch` anchors the `elapsed_ms` reported in each
    /// `DownloadTelemetry` event. It is captured by the caller at the moment
    /// real download work begins (after pre-download validation) so the speed
    /// graph starts at the download's own t=0 rather than including the prep
    /// phases, and so the hash lane can share the same origin.
    pub(super) fn spawn_sampler(
        self: &std::sync::Arc<Self>,
        progress_tx: Option<tokio::sync::broadcast::Sender<ProgressEvent>>,
        telemetry_epoch: Instant,
    ) -> JoinHandle<()> {
        let me = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let process_pid = sysinfo::get_current_pid().ok();
            let mut system = System::new();
            if let Some(pid) = process_pid {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    true,
                    ProcessRefreshKind::nothing().with_cpu().with_memory(),
                );
            }
            let mut last_bytes = 0_u64;
            let mut last_disk_bytes = 0_u64;
            loop {
                interval.tick().await;
                if me.sampler_stop.load(Ordering::Relaxed) {
                    break;
                }
                let bytes = me.counters.bytes_transferred.load(Ordering::Relaxed);
                let delta = bytes.saturating_sub(last_bytes);
                last_bytes = bytes;
                me.counters
                    .peak_network_bps
                    .fetch_max(delta, Ordering::Relaxed);
                let disk_bytes = me.counters.disk_bytes_written.load(Ordering::Relaxed);
                let disk_delta = disk_bytes.saturating_sub(last_disk_bytes);
                last_disk_bytes = disk_bytes;
                let speed_mbps = delta as f64 / (1024.0 * 1024.0);
                let disk_mbps = disk_delta as f64 / (1024.0 * 1024.0);
                let active_files = me.counters.active_files.load(Ordering::Relaxed);
                let active_ranges = me.counters.active_ranges.load(Ordering::Relaxed);
                let completed = me.counters.files_completed.load(Ordering::Relaxed);
                let (cpu_percent, memory_bytes) = if let Some(pid) = process_pid {
                    system.refresh_processes_specifics(
                        ProcessesToUpdate::Some(&[pid]),
                        true,
                        ProcessRefreshKind::nothing().with_cpu().with_memory(),
                    );
                    system.process(pid).map_or((0.0, 0), |process| {
                        (process.cpu_usage() as f64, process.memory())
                    })
                } else {
                    (0.0, 0)
                };
                debug!(
                    "Download sample: speed={:.2} MB/s disk={:.2} MB/s cpu={:.1}% memory={} active_files={} active_ranges={} completed_files={}",
                    speed_mbps,
                    disk_mbps,
                    cpu_percent,
                    format_bytes(memory_bytes),
                    active_files,
                    active_ranges,
                    completed
                );
                if let Some(tx) = progress_tx.as_ref() {
                    let _ = tx.send(ProgressEvent::DownloadTelemetry {
                        elapsed_ms: telemetry_epoch.elapsed().as_millis() as u64,
                        download_bps: delta as f64,
                        disk_write_bps: disk_delta as f64,
                        cpu_percent,
                        memory_bytes,
                    });
                }
            }
        })
    }

    /// Signal the sampler task to stop.
    pub(super) fn stop_sampler(&self) {
        self.sampler_stop.store(true, Ordering::Relaxed);
    }

    /// Build a structured summary of the entire download run.
    pub(crate) fn build_report(&self, mod_outcomes: &[DownloadModOutcome]) -> DownloadRunReport {
        DownloadRunReport {
            total: self.render_summary(),
            addon_summaries: mod_outcomes
                .iter()
                .map(|outcome| self.render_mod_summary(outcome))
                .collect(),
        }
    }

    fn render_summary(&self) -> String {
        let total_elapsed = self.started_at.elapsed();
        let total_bytes = self.counters.bytes_transferred.load(Ordering::Relaxed);
        let total_files = self.counters.files_completed.load(Ordering::Relaxed);
        let total_retries = self.counters.range_retries.load(Ordering::Relaxed);
        let db_ms = self.counters.db_checkpoint_ms.load(Ordering::Relaxed);
        let db_batches = self.counters.db_checkpoint_batches.load(Ordering::Relaxed);
        let db_rows = self.counters.db_checkpoint_rows.load(Ordering::Relaxed);
        let db_statements = self
            .counters
            .db_checkpoint_statements
            .load(Ordering::Relaxed);
        let avg_mbps = mbps(total_bytes, total_elapsed);

        let mut lines = Vec::new();
        lines.push("TOTAL DOWNLOAD".to_owned());
        lines.push(format!(
            "total: files={} bytes={} elapsed={} avg={:.2} MB/s retries={}",
            total_files,
            format_bytes(total_bytes),
            format_duration(total_elapsed),
            avg_mbps,
            total_retries
        ));
        if db_batches > 0 {
            lines.push(format!(
                "db: checkpoint_batches={} rows={} statements={} total={} avg_batch={:.1}ms rows_per_batch={:.1} statements_per_batch={:.1}",
                db_batches,
                db_rows,
                db_statements,
                format_millis(db_ms),
                db_ms as f64 / db_batches as f64,
                db_rows as f64 / db_batches as f64,
                db_statements as f64 / db_batches as f64
            ));
        }

        if let Ok(phases) = self.phase_events.lock()
            && !phases.is_empty()
        {
            lines.push("phases:".to_owned());
            for phase in phases.iter() {
                lines.push(format!(
                    "  {:<28} {}",
                    phase.name,
                    format_duration(phase.elapsed)
                ));
            }
        }

        if let Ok(files) = self.file_events.lock()
            && !files.is_empty()
        {
            let stats = FileStats::from_files(&files);
            lines.push(format!(
                "files: count={} bytes={} network_bytes={} methods={} retries={} split_files={} max_splits={}",
                files.len(),
                format_bytes(stats.bytes),
                format_bytes(stats.network_bytes),
                stats.methods,
                stats.retries,
                stats.split_files,
                stats.max_splits
            ));
            lines.push(format!(
                "network: avg={:.2} MB/s p50_file={:.2} MB/s p95_file={:.2} MB/s permit_wait={} promote={}",
                stats.avg_speed_mbps,
                stats.p50_speed_mbps,
                stats.p95_speed_mbps,
                format_duration(stats.permit_wait),
                format_duration(stats.promote_time)
            ));
            lines.push(format!(
                "disk: file_write_ops={} file_write_time={} avg={:.2} MB/s p50={:.2} MB/s p95={:.2} MB/s",
                stats.disk_write_count,
                format_duration(stats.disk_write_time),
                stats.disk_avg_mbps,
                stats.disk_p50_mbps,
                stats.disk_p95_mbps
            ));
        }

        if let Ok(ranges) = self.range_events.lock()
            && !ranges.is_empty()
        {
            let stats = RangeStats::from_ranges(&ranges);
            lines.push(format!(
                "ranges: count={} bytes={} retries={} max_range={}",
                ranges.len(),
                format_bytes(stats.bytes),
                stats.retries,
                format_bytes(stats.max_range_bytes)
            ));
            lines.push(format!(
                "ranges network: p50_latency={:.1}ms p95_latency={:.1}ms transfer_avg={:.2} MB/s",
                stats.p50_request_latency_ms, stats.p95_request_latency_ms, stats.transfer_avg_mbps
            ));
            lines.push(format!(
                "ranges disk: write_ops={} write_time={} avg={:.2} MB/s p50={:.2} MB/s p95={:.2} MB/s",
                stats.write_count,
                format_duration(stats.write_time),
                stats.write_avg_mbps,
                stats.write_p50_mbps,
                stats.write_p95_mbps
            ));
        }

        lines.join("\n")
    }

    fn render_mod_summary(&self, outcome: &DownloadModOutcome) -> String {
        let files = match self.file_events.lock() {
            Ok(files) => files,
            Err(_) => return String::new(),
        };
        let mod_files: Vec<&FileMetric> = files
            .iter()
            .filter(|file| file.mod_id == outcome.mod_id)
            .collect();
        if mod_files.is_empty() {
            return format!(
                "addon: mod_id={} name={} success={}\n  files: count=0",
                outcome.mod_id, outcome.mod_name, outcome.success
            );
        }

        let file_stats = FileStats::from_file_refs(&mod_files);
        let range_stats = self
            .range_events
            .lock()
            .ok()
            .map(|ranges| {
                let file_ids: std::collections::HashSet<u64> =
                    mod_files.iter().map(|file| file.file_id).collect();
                let mod_ranges: Vec<&RangeMetric> = ranges
                    .iter()
                    .filter(|range| file_ids.contains(&range.file_id))
                    .collect();
                RangeStats::from_range_refs(&mod_ranges)
            })
            .unwrap_or_default();
        let total_disk_time = file_stats
            .disk_write_time
            .saturating_add(range_stats.write_time);
        let total_disk_bytes = file_stats.disk_bytes.saturating_add(range_stats.bytes);

        let mut lines = Vec::new();
        lines.push(format!(
            "addon: mod_id={} name={} success={}",
            outcome.mod_id, outcome.mod_name, outcome.success
        ));
        lines.push(format!(
            "files: count={} bytes={} network_bytes={} methods={} retries={} ranges={} range_retries={}",
            mod_files.len(),
            format_bytes(file_stats.bytes),
            format_bytes(file_stats.network_bytes),
            file_stats.methods,
            file_stats.retries,
            range_stats.count,
            range_stats.retries
        ));
        lines.push(format!(
            "network: avg={:.2} MB/s p50_file={:.2} MB/s p95_file={:.2} MB/s permit_wait={}",
            file_stats.avg_speed_mbps,
            file_stats.p50_speed_mbps,
            file_stats.p95_speed_mbps,
            format_duration(file_stats.permit_wait)
        ));
        lines.push(format!(
            "disk: write_ops={} write_time={} avg={:.2} MB/s promote={}",
            file_stats
                .disk_write_count
                .saturating_add(range_stats.write_count),
            format_duration(total_disk_time),
            mbps(total_disk_bytes, total_disk_time),
            format_duration(file_stats.promote_time)
        ));
        indent_lines(&lines.join("\n"), "  ")
    }
}
#[derive(Default)]
struct FileStats {
    bytes: u64,
    network_bytes: u64,
    methods: String,
    avg_speed_mbps: f64,
    p50_speed_mbps: f64,
    p95_speed_mbps: f64,
    retries: usize,
    split_files: usize,
    max_splits: usize,
    permit_wait: Duration,
    promote_time: Duration,
    disk_write_time: Duration,
    disk_write_count: usize,
    disk_bytes: u64,
    disk_avg_mbps: f64,
    disk_p50_mbps: f64,
    disk_p95_mbps: f64,
}

impl FileStats {
    fn from_files(files: &[FileMetric]) -> Self {
        let refs: Vec<&FileMetric> = files.iter().collect();
        Self::from_file_refs(&refs)
    }

    fn from_file_refs(files: &[&FileMetric]) -> Self {
        let mut stats = Self::default();
        let mut speeds = Vec::with_capacity(files.len());
        let mut disk_speeds = Vec::new();
        let mut method_counts = std::collections::BTreeMap::<&'static str, usize>::new();
        let mut transfer_time = Duration::ZERO;

        for file in files {
            stats.bytes = stats.bytes.saturating_add(file.size as u64);
            stats.network_bytes = stats
                .network_bytes
                .saturating_add(file.expected_network_bytes as u64);
            stats.retries = stats.retries.saturating_add(file.retries);
            stats.max_splits = stats.max_splits.max(file.split_count);
            if file.split_count > 1 {
                stats.split_files += 1;
            }
            stats.permit_wait = stats.permit_wait.saturating_add(file.permit_wait);
            stats.promote_time = stats.promote_time.saturating_add(file.promote_time);
            stats.disk_write_time = stats.disk_write_time.saturating_add(file.disk_write_time);
            stats.disk_write_count = stats.disk_write_count.saturating_add(file.disk_write_count);
            transfer_time = transfer_time.saturating_add(file.transfer_time);
            speeds.push(file.avg_mbps);
            if file.disk_write_time > Duration::ZERO {
                stats.disk_bytes = stats.disk_bytes.saturating_add(file.size as u64);
                disk_speeds.push(mbps(file.size as u64, file.disk_write_time));
            }
            *method_counts.entry(file.method).or_default() += 1;
            let _ = file.first_byte_latency;
        }

        speeds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        disk_speeds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        stats.p50_speed_mbps = percentile(&speeds, 50);
        stats.p95_speed_mbps = percentile(&speeds, 95);
        stats.disk_p50_mbps = percentile(&disk_speeds, 50);
        stats.disk_p95_mbps = percentile(&disk_speeds, 95);
        stats.avg_speed_mbps = mbps(stats.network_bytes, transfer_time);
        stats.disk_avg_mbps = mbps(stats.disk_bytes, stats.disk_write_time);
        stats.methods = method_counts
            .into_iter()
            .map(|(method, count)| format!("{method}={count}"))
            .collect::<Vec<_>>()
            .join(",");
        stats
    }
}

#[derive(Default)]
struct RangeStats {
    count: usize,
    bytes: u64,
    retries: usize,
    p50_request_latency_ms: f64,
    p95_request_latency_ms: f64,
    transfer_avg_mbps: f64,
    write_time: Duration,
    write_count: usize,
    write_avg_mbps: f64,
    write_p50_mbps: f64,
    write_p95_mbps: f64,
    max_range_bytes: u64,
}

impl RangeStats {
    fn from_ranges(ranges: &[RangeMetric]) -> Self {
        let refs: Vec<&RangeMetric> = ranges.iter().collect();
        Self::from_range_refs(&refs)
    }

    fn from_range_refs(ranges: &[&RangeMetric]) -> Self {
        let mut stats = Self {
            count: ranges.len(),
            ..Self::default()
        };
        let mut latencies = Vec::with_capacity(ranges.len());
        let mut write_speeds = Vec::new();
        let mut transfer_time = Duration::ZERO;

        for range in ranges {
            stats.bytes = stats.bytes.saturating_add(range.bytes);
            stats.retries = stats.retries.saturating_add(range.retries);
            stats.write_time = stats.write_time.saturating_add(range.write_time);
            stats.write_count = stats.write_count.saturating_add(range.write_count);
            stats.max_range_bytes = stats.max_range_bytes.max(range.bytes);
            transfer_time = transfer_time.saturating_add(range.transfer_time);
            latencies.push(range.request_latency.as_secs_f64() * 1000.0);
            if range.write_time > Duration::ZERO {
                write_speeds.push(mbps(range.bytes, range.write_time));
            }
            let _ = (range.start, range.end);
        }

        latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        write_speeds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        stats.p50_request_latency_ms = percentile(&latencies, 50);
        stats.p95_request_latency_ms = percentile(&latencies, 95);
        stats.transfer_avg_mbps = mbps(stats.bytes, transfer_time);
        stats.write_avg_mbps = mbps(stats.bytes, stats.write_time);
        stats.write_p50_mbps = percentile(&write_speeds, 50);
        stats.write_p95_mbps = percentile(&write_speeds, 95);
        stats
    }
}

fn mbps(bytes: u64, elapsed: Duration) -> f64 {
    if elapsed.as_secs_f64() > 0.0 {
        (bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
    } else {
        0.0
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.2} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.2}s", duration.as_secs_f64())
    } else {
        format!("{:.1}ms", duration.as_secs_f64() * 1000.0)
    }
}

fn format_millis(ms: u64) -> String {
    format_duration(Duration::from_millis(ms))
}

fn indent_lines(text: &str, indent: &str) -> String {
    text.lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shared scheduler state used to compute adaptive split counts per file.
/// Created once in the orchestrator and passed through to the transfer layer.
pub(super) struct DownloadSchedulerState {
    /// Number of large files currently being downloaded.
    pub(super) active_large_files: AtomicUsize,
    /// Number of large files still waiting in the queue.
    pub(super) queued_large_files: AtomicUsize,
    /// Global semaphore capping total concurrent HTTP range requests.
    pub(super) range_permits: std::sync::Arc<Semaphore>,
    pub(super) limits: DownloadResourceLimits,
}

impl DownloadSchedulerState {
    pub(super) fn new(limits: DownloadResourceLimits) -> Self {
        Self {
            active_large_files: AtomicUsize::new(0),
            queued_large_files: AtomicUsize::new(0),
            range_permits: std::sync::Arc::new(Semaphore::new(limits.max_active_range_requests)),
            limits,
        }
    }

    /// Fair-share cap on concurrent ranges a single file may use right now.
    ///
    /// Divides the global range budget across large files that are actively
    /// transferring, clamped to the per-file floor/ceiling. Queued files do not
    /// own range capacity yet; counting them here parks workers behind capacity
    /// that is otherwise idle at the start of large batches and can keep the
    /// network well below line rate.
    pub(super) fn current_per_file_range_cap(&self) -> usize {
        let in_flight = self.active_large_files.load(Ordering::Relaxed).max(1);
        let fair_share = self.limits.max_active_range_requests / in_flight;
        fair_share.clamp(
            self.limits.min_ranges_per_file.max(1),
            self.limits.max_ranges_per_file.max(1),
        )
    }
}

fn percentile(sorted: &[f64], pct: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (pct * sorted.len() / 100).min(sorted.len() - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_empty_returns_zero() {
        assert_eq!(percentile(&[], 50), 0.0);
    }

    #[test]
    fn percentile_single_element() {
        assert_eq!(percentile(&[5.0], 50), 5.0);
        assert_eq!(percentile(&[5.0], 95), 5.0);
    }

    #[test]
    fn percentile_multiple_elements() {
        let data: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let p50 = percentile(&data, 50);
        assert!((p50 - 50.0).abs() < 2.0);
        let p95 = percentile(&data, 95);
        assert!((p95 - 95.0).abs() < 2.0);
    }

    #[test]
    fn counters_default_to_zero() {
        let m = DownloadMetrics::new();
        assert_eq!(m.counters.active_files.load(Ordering::Relaxed), 0);
        assert_eq!(m.counters.bytes_transferred.load(Ordering::Relaxed), 0);
        assert_eq!(m.counters.files_completed.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn record_bytes_increments() {
        let m = DownloadMetrics::new();
        m.record_bytes(100);
        m.record_bytes(200);
        assert_eq!(m.counters.bytes_transferred.load(Ordering::Relaxed), 300);
    }

    #[test]
    fn record_file_metric() {
        let m = DownloadMetrics::new();
        m.record_file(FileMetric {
            file_id: 1,
            mod_id: 2,
            size: 1000,
            expected_network_bytes: 800,
            method: "full_download",
            split_count: 1,
            permit_wait: Duration::ZERO,
            first_byte_latency: None,
            transfer_time: Duration::from_secs(1),
            promote_time: Duration::ZERO,
            disk_write_time: Duration::ZERO,
            disk_write_count: 0,
            retries: 0,
            avg_mbps: 0.8,
        });
        let files = m.file_events.lock().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_id, 1);
    }

    #[test]
    fn record_range_metric() {
        let m = DownloadMetrics::new();
        m.record_range(RangeMetric {
            file_id: 1,
            start: 0,
            end: 999,
            request_latency: Duration::from_millis(50),
            transfer_time: Duration::from_secs(1),
            write_time: Duration::from_millis(10),
            write_count: 1,
            retries: 0,
            bytes: 1000,
        });
        let ranges = m.range_events.lock().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].bytes, 1000);
    }

    #[test]
    fn phase_guard_records_duration() {
        let m = DownloadMetrics::new();
        {
            let _guard = m.phase("test_phase");
            std::thread::sleep(Duration::from_millis(10));
        }
        let phases = m.phase_events.lock().unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].name, "test_phase");
        assert!(phases[0].elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn stop_sampler_sets_flag() {
        let m = DownloadMetrics::new();
        assert!(!m.sampler_stop.load(Ordering::Relaxed));
        m.stop_sampler();
        assert!(m.sampler_stop.load(Ordering::Relaxed));
    }

    // ── current_per_file_range_cap ─────────────────────────────────────

    fn scheduler_with_large_files(active: usize, queued: usize) -> DownloadSchedulerState {
        let state = DownloadSchedulerState::new(DownloadResourceLimits::normal());
        state.active_large_files.store(active, Ordering::Relaxed);
        state.queued_large_files.store(queued, Ordering::Relaxed);
        state
    }

    #[test]
    fn range_cap_uses_floor_when_many_large_files() {
        let state = scheduler_with_large_files(24, 20);
        assert_eq!(
            state.current_per_file_range_cap(),
            DownloadResourceLimits::normal().min_ranges_per_file
        );
    }

    #[test]
    fn range_cap_uses_ceiling_for_single_file() {
        let state = scheduler_with_large_files(1, 0);
        assert_eq!(
            state.current_per_file_range_cap(),
            DownloadResourceLimits::normal().max_ranges_per_file
        );
    }

    #[test]
    fn range_cap_scales_with_queue_drain() {
        // Global permits are divided across active files only, then clamped to
        // the per-file ceiling.
        assert_eq!(
            scheduler_with_large_files(3, 3).current_per_file_range_cap(),
            32
        );
        assert_eq!(
            scheduler_with_large_files(2, 1).current_per_file_range_cap(),
            48
        );
    }

    #[test]
    fn range_cap_handles_zero_files() {
        let state = scheduler_with_large_files(0, 0);
        assert_eq!(
            state.current_per_file_range_cap(),
            DownloadResourceLimits::normal().max_ranges_per_file
        );
    }
}
