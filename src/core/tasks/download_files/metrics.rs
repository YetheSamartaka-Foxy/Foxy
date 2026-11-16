use crate::core::api::ProgressEvent;
use log::debug;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::DownloadResourceLimits;

pub(crate) struct DownloadMetrics {
    started_at: Instant,
    pub(super) counters: DownloadCounters,
    file_events: Mutex<Vec<FileMetric>>,
    range_events: Mutex<Vec<RangeMetric>>,
    phase_events: Mutex<Vec<PhaseMetric>>,

    destinations: Mutex<Vec<String>>,

    series: Mutex<Vec<SeriesPoint>>,
    patch_stages: Mutex<Vec<PatchStageMetric>>,
    sampler_stop: AtomicBool,
    sampler_wake: tokio::sync::Notify,
    patch: PatchCounters,
}

struct PatchCounters {
    first_start_ns: AtomicU64,
    last_end_ns: AtomicU64,
    attempts: AtomicUsize,
    successes: AtomicUsize,
    fallbacks: AtomicUsize,
    cancellations: AtomicUsize,
    requests: AtomicUsize,
    retries: AtomicUsize,
    useful_output_bytes: AtomicU64,
    unique_insert_bytes: AtomicU64,
    actual_received_bytes: AtomicU64,
    source_copy_bytes: AtomicU64,
    staging_bytes: AtomicU64,
    planning_ns: AtomicU64,
    fetch_ns: AtomicU64,
    apply_ns: AtomicU64,
    promote_ns: AtomicU64,
    verify_ns: AtomicU64,
    finalize_ns: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PatchSummary {
    pub(crate) start_offset_ns: u64,
    pub(crate) end_offset_ns: u64,
    pub(crate) attempts: usize,
    pub(crate) successes: usize,
    pub(crate) fallbacks: usize,
    pub(crate) cancellations: usize,
    pub(crate) requests: usize,
    pub(crate) retries: usize,
    pub(crate) useful_output_bytes: u64,
    pub(crate) unique_insert_bytes: u64,
    pub(crate) actual_received_bytes: u64,
    pub(crate) source_copy_bytes: u64,
    pub(crate) staging_bytes: u64,
    pub(crate) planning_ns: u64,
    pub(crate) fetch_ns: u64,
    pub(crate) apply_ns: u64,
    pub(crate) promote_ns: u64,
    pub(crate) verify_ns: u64,
    pub(crate) finalize_ns: u64,
    pub(crate) verify_promote_ns: u64,
}

impl PatchSummary {
    pub(crate) fn makespan(&self) -> Duration {
        Duration::from_nanos(self.end_offset_ns.saturating_sub(self.start_offset_ns))
    }

    pub(crate) fn outcome(&self) -> &'static str {
        if self.cancellations > 0 {
            "cancelled"
        } else if self.successes == 0 && self.fallbacks > 0 {
            "fallback"
        } else if self.fallbacks > 0 {
            "completed_with_fallback"
        } else {
            "completed"
        }
    }
}

pub(crate) struct PatchAttemptTelemetry {
    metrics: Arc<DownloadMetrics>,
    file_id: u64,
    stage: &'static str,
    stage_started: Instant,
    outcome: PatchAttemptOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchStageMetric {
    pub(crate) file_id: u64,
    pub(crate) stage: &'static str,
    pub(crate) start_offset_ns: u64,
    pub(crate) end_offset_ns: u64,
    pub(crate) outcome: &'static str,
}

#[derive(Clone, Copy)]
enum PatchAttemptOutcome {
    Fallback,
    Success,
    Cancelled,
}

impl PatchAttemptTelemetry {
    pub(crate) fn set_work(
        &self,
        useful_output_bytes: u64,
        unique_insert_bytes: u64,
        source_copy_bytes: u64,
    ) {
        self.metrics
            .patch
            .useful_output_bytes
            .fetch_add(useful_output_bytes, Ordering::Relaxed);
        self.metrics
            .patch
            .unique_insert_bytes
            .fetch_add(unique_insert_bytes, Ordering::Relaxed);
        self.metrics
            .patch
            .source_copy_bytes
            .fetch_add(source_copy_bytes, Ordering::Relaxed);
        self.metrics.patch.staging_bytes.fetch_add(
            useful_output_bytes.saturating_add(unique_insert_bytes),
            Ordering::Relaxed,
        );
    }

    pub(crate) fn planning_finished(&mut self) {
        self.transition_to("fetch");
    }

    pub(crate) fn fetch_finished(&mut self) {
        self.transition_to("apply");
    }

    pub(crate) fn apply_finished(&mut self) {
        self.transition_to("promote");
    }

    pub(crate) fn promote_finished(&mut self) {
        self.transition_to("verify");
    }

    pub(crate) fn verify_finished(&mut self) {
        self.transition_to("finalize");
    }

    pub(crate) fn success(&mut self) {
        self.outcome = PatchAttemptOutcome::Success;
    }

    pub(crate) fn cancelled(&mut self) {
        self.outcome = PatchAttemptOutcome::Cancelled;
    }

    fn transition_to(&mut self, next: &'static str) {
        let ended = self.record_stage("completed");
        self.stage = next;
        self.stage_started = ended;
    }

    fn record_stage(&self, outcome: &'static str) -> Instant {
        let ended = Instant::now();
        let elapsed_ns = ended.duration_since(self.stage_started).as_nanos() as u64;
        match self.stage {
            "planning" => &self.metrics.patch.planning_ns,
            "fetch" => &self.metrics.patch.fetch_ns,
            "apply" => &self.metrics.patch.apply_ns,
            "promote" => &self.metrics.patch.promote_ns,
            "verify" => &self.metrics.patch.verify_ns,
            "finalize" => &self.metrics.patch.finalize_ns,
            _ => unreachable!("unknown patch stage"),
        }
        .fetch_add(elapsed_ns, Ordering::Relaxed);
        if let Ok(mut stages) = self.metrics.patch_stages.lock() {
            stages.push(PatchStageMetric {
                file_id: self.file_id,
                stage: self.stage,
                start_offset_ns: self
                    .stage_started
                    .duration_since(self.metrics.started_at)
                    .as_nanos() as u64,
                end_offset_ns: ended.duration_since(self.metrics.started_at).as_nanos() as u64,
                outcome,
            });
        }
        ended
    }
}

impl Drop for PatchAttemptTelemetry {
    fn drop(&mut self) {
        let outcome = match self.outcome {
            PatchAttemptOutcome::Fallback => "fallback",
            PatchAttemptOutcome::Success => "completed",
            PatchAttemptOutcome::Cancelled => "cancelled",
        };
        let ended = self.record_stage(outcome);
        self.metrics.patch.last_end_ns.fetch_max(
            ended.duration_since(self.metrics.started_at).as_nanos() as u64,
            Ordering::Relaxed,
        );
        match self.outcome {
            PatchAttemptOutcome::Fallback => &self.metrics.patch.fallbacks,
            PatchAttemptOutcome::Success => &self.metrics.patch.successes,
            PatchAttemptOutcome::Cancelled => &self.metrics.patch.cancellations,
        }
        .fetch_add(1, Ordering::Relaxed);
    }
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

pub(super) struct DownloadCounters {
    pub(super) active_files: AtomicUsize,
    pub(super) active_ranges: AtomicUsize,
    pub(super) bytes_transferred: AtomicU64,
    /// Use the best qualified sample as path capacity when no limiter sets a ceiling.
    pub(super) peak_network_bps: AtomicU64,

    pub(super) peak_window_ns: AtomicU64,
    /// Patched bytes come from local copies and are not reread by the hash.
    pub(super) patched_full_bytes: AtomicU64,
    pub(super) files_completed: AtomicUsize,
    pub(super) range_retries: AtomicUsize,
    pub(super) db_checkpoint_ms: AtomicU64,
    pub(super) db_checkpoint_batches: AtomicUsize,
    pub(super) db_checkpoint_rows: AtomicUsize,
    pub(super) db_checkpoint_statements: AtomicUsize,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SeriesPoint {
    pub(crate) end_offset: Duration,
    pub(crate) elapsed: Duration,
    pub(crate) network_delta: u64,
    pub(crate) active_files: usize,
    pub(crate) active_ranges: usize,
}

impl SeriesPoint {
    fn bps(&self) -> u64 {
        let secs = self.elapsed.as_secs_f64();
        if secs <= 0.0 {
            0
        } else {
            (self.network_delta as f64 / secs).round() as u64
        }
    }
}

const PLATEAU_FRACTION: f64 = 0.9;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RampTail {
    pub(crate) ramp: Duration,
    pub(crate) plateau: Duration,
    pub(crate) tail: Duration,
    pub(crate) ramp_deficit_bytes: u64,
    pub(crate) tail_deficit_bytes: u64,
    pub(crate) ramp_windows: usize,
    pub(crate) reached: bool,
}

pub(crate) fn ramp_tail_profile(
    series: &[SeriesPoint],
    peak_bps: u64,
    total: Duration,
) -> RampTail {
    if peak_bps == 0 || series.is_empty() {
        return RampTail::default();
    }
    let threshold = (peak_bps as f64 * PLATEAU_FRACTION) as u64;
    let at_plateau = |point: &SeriesPoint| point.bps() >= threshold;
    let Some(first) = series.iter().position(at_plateau) else {
        return RampTail::default();
    };
    let last = series.iter().rposition(at_plateau).unwrap_or(first);
    let deficit = |point: &SeriesPoint| {
        let expected = (peak_bps as f64 * point.elapsed.as_secs_f64()) as u64;
        expected.saturating_sub(point.network_delta)
    };
    let ramp_end = series[first]
        .end_offset
        .saturating_sub(series[first].elapsed);
    let tail_start = series[last].end_offset.min(total);
    RampTail {
        ramp: ramp_end,
        plateau: tail_start.saturating_sub(ramp_end),
        tail: total.saturating_sub(tail_start),
        ramp_deficit_bytes: series[..first].iter().map(deficit).sum(),
        tail_deficit_bytes: series[last + 1..].iter().map(deficit).sum(),
        ramp_windows: first,
        reached: true,
    }
}

pub(super) const PEAK_MIN_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RateSample {
    pub(super) at: Instant,
    pub(super) elapsed: Duration,
    pub(super) network_delta: u64,
    pub(super) disk_delta: u64,
    pub(super) network_bps: u64,
    pub(super) disk_bps: u64,
}

impl RateSample {
    pub(super) fn peak_eligible(&self) -> bool {
        self.elapsed >= PEAK_MIN_INTERVAL && self.network_delta > 0
    }
}

pub(super) struct RateSampler {
    last_at: Instant,
    last_network_bytes: u64,
    last_disk_bytes: u64,
}

impl RateSampler {
    pub(super) fn new(epoch: Instant) -> Self {
        Self {
            last_at: epoch,
            last_network_bytes: 0,
            last_disk_bytes: 0,
        }
    }

    pub(super) fn sample(
        &mut self,
        network_bytes: u64,
        disk_bytes: u64,
        now: Instant,
    ) -> RateSample {
        let elapsed = now.saturating_duration_since(self.last_at);
        let network_delta = network_bytes.saturating_sub(self.last_network_bytes);
        let disk_delta = disk_bytes.saturating_sub(self.last_disk_bytes);
        self.last_at = now;
        self.last_network_bytes = network_bytes;
        self.last_disk_bytes = disk_bytes;
        RateSample {
            at: now,
            elapsed,
            network_delta,
            disk_delta,
            network_bps: interval_rate(network_delta, elapsed),
            disk_bps: interval_rate(disk_delta, elapsed),
        }
    }
}

pub(super) fn interval_rate(delta_bytes: u64, elapsed: Duration) -> u64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 || delta_bytes == 0 {
        return 0;
    }
    (delta_bytes as f64 / secs).round() as u64
}

struct PhaseMetric {
    name: &'static str,
    elapsed: Duration,
}

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
    pub(crate) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            counters: DownloadCounters {
                active_files: AtomicUsize::new(0),
                active_ranges: AtomicUsize::new(0),
                bytes_transferred: AtomicU64::new(0),
                peak_network_bps: AtomicU64::new(0),
                peak_window_ns: AtomicU64::new(0),
                patched_full_bytes: AtomicU64::new(0),
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
            destinations: Mutex::new(Vec::new()),
            series: Mutex::new(Vec::new()),
            patch_stages: Mutex::new(Vec::new()),
            sampler_stop: AtomicBool::new(false),
            sampler_wake: tokio::sync::Notify::new(),
            patch: PatchCounters {
                first_start_ns: AtomicU64::new(u64::MAX),
                last_end_ns: AtomicU64::new(0),
                attempts: AtomicUsize::new(0),
                successes: AtomicUsize::new(0),
                fallbacks: AtomicUsize::new(0),
                cancellations: AtomicUsize::new(0),
                requests: AtomicUsize::new(0),
                retries: AtomicUsize::new(0),
                useful_output_bytes: AtomicU64::new(0),
                unique_insert_bytes: AtomicU64::new(0),
                actual_received_bytes: AtomicU64::new(0),
                source_copy_bytes: AtomicU64::new(0),
                staging_bytes: AtomicU64::new(0),
                planning_ns: AtomicU64::new(0),
                fetch_ns: AtomicU64::new(0),
                apply_ns: AtomicU64::new(0),
                promote_ns: AtomicU64::new(0),
                verify_ns: AtomicU64::new(0),
                finalize_ns: AtomicU64::new(0),
            },
        }
    }

    pub(crate) fn start_patch_attempt(self: &Arc<Self>, file_id: u64) -> PatchAttemptTelemetry {
        let started = Instant::now();
        self.patch.attempts.fetch_add(1, Ordering::Relaxed);
        self.patch.first_start_ns.fetch_min(
            started.duration_since(self.started_at).as_nanos() as u64,
            Ordering::Relaxed,
        );
        PatchAttemptTelemetry {
            metrics: Arc::clone(self),
            file_id,
            stage: "planning",
            stage_started: started,
            outcome: PatchAttemptOutcome::Fallback,
        }
    }

    pub(crate) fn record_patch_request(&self) {
        self.patch.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_patch_retry(&self) {
        self.patch.retries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_patch_received_bytes(&self, bytes: u64) {
        self.patch
            .actual_received_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn patch_summary(&self) -> Option<PatchSummary> {
        let attempts = self.patch.attempts.load(Ordering::Relaxed);
        let promote_ns = self.patch.promote_ns.load(Ordering::Relaxed);
        let verify_ns = self.patch.verify_ns.load(Ordering::Relaxed);
        let finalize_ns = self.patch.finalize_ns.load(Ordering::Relaxed);
        (attempts > 0).then(|| PatchSummary {
            start_offset_ns: self.patch.first_start_ns.load(Ordering::Relaxed),
            end_offset_ns: self.patch.last_end_ns.load(Ordering::Relaxed),
            attempts,
            successes: self.patch.successes.load(Ordering::Relaxed),
            fallbacks: self.patch.fallbacks.load(Ordering::Relaxed),
            cancellations: self.patch.cancellations.load(Ordering::Relaxed),
            requests: self.patch.requests.load(Ordering::Relaxed),
            retries: self.patch.retries.load(Ordering::Relaxed),
            useful_output_bytes: self.patch.useful_output_bytes.load(Ordering::Relaxed),
            unique_insert_bytes: self.patch.unique_insert_bytes.load(Ordering::Relaxed),
            actual_received_bytes: self.patch.actual_received_bytes.load(Ordering::Relaxed),
            source_copy_bytes: self.patch.source_copy_bytes.load(Ordering::Relaxed),
            staging_bytes: self.patch.staging_bytes.load(Ordering::Relaxed),
            planning_ns: self.patch.planning_ns.load(Ordering::Relaxed),
            fetch_ns: self.patch.fetch_ns.load(Ordering::Relaxed),
            apply_ns: self.patch.apply_ns.load(Ordering::Relaxed),
            promote_ns,
            verify_ns,
            finalize_ns,
            verify_promote_ns: promote_ns
                .saturating_add(verify_ns)
                .saturating_add(finalize_ns),
        })
    }

    pub(crate) fn patch_stages(&self) -> Vec<PatchStageMetric> {
        let mut stages = self
            .patch_stages
            .lock()
            .map_or_else(|_| Vec::new(), |stages| stages.clone());
        stages.sort_by_key(|stage| (stage.start_offset_ns, stage.file_id, stage.stage));
        stages
    }

    pub(super) fn record_destination(&self, description: String) {
        if let Ok(mut destinations) = self.destinations.lock() {
            destinations.push(description);
        }
    }

    pub(super) fn phase(&self, name: &'static str) -> PhaseGuard<'_> {
        PhaseGuard {
            metrics: self,
            name,
            started: Instant::now(),
        }
    }

    pub(super) fn record_file(&self, metric: FileMetric) {
        if metric.method == "delta_patch" {
            self.counters
                .patched_full_bytes
                .fetch_add(metric.size as u64, Ordering::Relaxed);
        }
        if let Ok(mut files) = self.file_events.lock() {
            files.push(metric);
        }
    }

    pub(super) fn record_range(&self, metric: RangeMetric) {
        if let Ok(mut ranges) = self.range_events.lock() {
            ranges.push(metric);
        }
    }

    pub(crate) fn peak_network_bps(&self) -> u64 {
        self.counters.peak_network_bps.load(Ordering::Relaxed)
    }

    pub(crate) fn peak_window(&self) -> Duration {
        Duration::from_nanos(self.counters.peak_window_ns.load(Ordering::Relaxed))
    }

    fn record_peak(&self, sample: &RateSample) {
        if !sample.peak_eligible() {
            return;
        }
        let bps = sample.network_bps;
        if self
            .counters
            .peak_network_bps
            .fetch_max(bps, Ordering::Relaxed)
            < bps
        {
            self.counters
                .peak_window_ns
                .store(sample.elapsed.as_nanos() as u64, Ordering::Relaxed);
        }
    }

    pub(crate) fn series(&self) -> Vec<SeriesPoint> {
        self.series
            .lock()
            .map(|series| series.clone())
            .unwrap_or_default()
    }

    pub(crate) fn record_bytes(&self, n: u64) {
        self.counters
            .bytes_transferred
            .fetch_add(n, Ordering::Relaxed);
    }

    /// Anchor telemetry after pre-download validation so download and hash lanes share the work timeline.
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
            let mut rates = RateSampler::new(telemetry_epoch);
            let written_at_start = crate::core::utils::file_io::bytes_written_total();
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = me.sampler_wake.notified() => {}
                }
                // Take a final sample on stop so the last partial interval is not lost.
                let stopping = me.sampler_stop.load(Ordering::Relaxed);
                let sample = rates.sample(
                    me.counters.bytes_transferred.load(Ordering::Relaxed),
                    crate::core::utils::file_io::bytes_written_total()
                        .saturating_sub(written_at_start),
                    Instant::now(),
                );
                if stopping && sample.network_delta == 0 && sample.disk_delta == 0 {
                    break;
                }
                me.record_peak(&sample);
                if let Ok(mut series) = me.series.lock() {
                    series.push(SeriesPoint {
                        end_offset: sample.at.saturating_duration_since(telemetry_epoch),
                        elapsed: sample.elapsed,
                        network_delta: sample.network_delta,
                        active_files: me.counters.active_files.load(Ordering::Relaxed),
                        active_ranges: me.counters.active_ranges.load(Ordering::Relaxed),
                    });
                }
                let speed_mbps = sample.network_bps as f64 / (1024.0 * 1024.0);
                let disk_mbps = sample.disk_bps as f64 / (1024.0 * 1024.0);
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

                let memory_bytes = crate::ui::memory::sample_process_memory()
                    .task_manager_memory_bytes()
                    .unwrap_or(memory_bytes);
                debug!(
                    "Download sample: speed={:.2} MB/s disk={:.2} MB/s interval_ms={} bytes={} cpu={:.1}% memory={} active_files={} active_ranges={} completed_files={}",
                    speed_mbps,
                    disk_mbps,
                    sample.elapsed.as_millis(),
                    sample.network_delta,
                    cpu_percent,
                    format_bytes(memory_bytes),
                    active_files,
                    active_ranges,
                    completed
                );
                if let Some(tx) = progress_tx.as_ref() {
                    let _ = tx.send(ProgressEvent::DownloadTelemetry {
                        elapsed_ms: sample
                            .at
                            .saturating_duration_since(telemetry_epoch)
                            .as_millis() as u64,
                        download_bps: sample.network_bps as f64,
                        disk_write_bps: sample.disk_bps as f64,
                        cpu_percent,
                        memory_bytes,
                    });
                }
                if stopping {
                    break;
                }
            }
        })
    }

    pub(super) fn stop_sampler(&self) {
        self.sampler_stop.store(true, Ordering::Relaxed);
        self.sampler_wake.notify_one();
    }

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

        if let Ok(destinations) = self.destinations.lock() {
            for destination in destinations.iter() {
                lines.push(format!("destination: {destination}"));
            }
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

pub(super) struct DownloadSchedulerState {
    pub(super) active_large_files: AtomicUsize,

    pub(super) queued_large_files: AtomicUsize,

    pub(super) range_permits: std::sync::Arc<Semaphore>,
    /// Cap disk-bound applies on rotational storage to limit concurrent seeks.
    pub(super) patch_apply_permits: std::sync::Arc<Semaphore>,

    pub(super) range_cap_changed: tokio::sync::Notify,
    pub(super) limits: DownloadResourceLimits,
}

impl DownloadSchedulerState {
    pub(super) fn new(limits: DownloadResourceLimits) -> Self {
        Self {
            active_large_files: AtomicUsize::new(0),
            queued_large_files: AtomicUsize::new(0),
            range_permits: std::sync::Arc::new(Semaphore::new(limits.max_active_range_requests)),
            patch_apply_permits: std::sync::Arc::new(Semaphore::new(limits.max_patch_applies)),
            range_cap_changed: tokio::sync::Notify::new(),
            limits,
        }
    }

    /// Divide range permits among active files only; queued files must not park workers behind idle capacity.
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
    fn patch_attempt_records_owned_monotonic_stage_spans() {
        let metrics = Arc::new(DownloadMetrics::new());
        {
            let mut attempt = metrics.start_patch_attempt(42);
            attempt.planning_finished();
            attempt.fetch_finished();
            attempt.apply_finished();
            attempt.promote_finished();
            attempt.verify_finished();
            attempt.success();
        }
        let stages = metrics.patch_stages();
        assert_eq!(
            stages.iter().map(|stage| stage.stage).collect::<Vec<_>>(),
            [
                "planning", "fetch", "apply", "promote", "verify", "finalize"
            ]
        );
        assert!(stages.iter().all(|stage| stage.file_id == 42));
        assert!(
            stages
                .windows(2)
                .all(|pair| pair[0].end_offset_ns <= pair[1].start_offset_ns)
        );
        assert!(stages.iter().all(|stage| stage.outcome == "completed"));
        let summary = metrics.patch_summary().expect("patch summary");
        assert_eq!(summary.attempts, 1);
        assert_eq!(summary.successes, 1);
        assert_eq!(summary.outcome(), "completed");
    }

    #[test]
    fn patch_attempt_records_the_stage_where_fallback_happened() {
        let metrics = Arc::new(DownloadMetrics::new());
        {
            let mut attempt = metrics.start_patch_attempt(7);
            attempt.planning_finished();
        }
        let stages = metrics.patch_stages();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].outcome, "completed");
        assert_eq!(stages[1].stage, "fetch");
        assert_eq!(stages[1].outcome, "fallback");
        assert_eq!(
            metrics.patch_summary().expect("patch summary").outcome(),
            "fallback"
        );
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

    fn point(end_s: f64, elapsed_s: f64, delta: u64) -> SeriesPoint {
        SeriesPoint {
            end_offset: Duration::from_secs_f64(end_s),
            elapsed: Duration::from_secs_f64(elapsed_s),
            network_delta: delta,
            active_files: 0,
            active_ranges: 0,
        }
    }

    #[test]
    fn ramp_tail_splits_the_wall_time_around_the_plateau() {
        let series = vec![
            point(1.0, 1.0, 10),
            point(2.0, 1.0, 50),
            point(3.0, 1.0, 95),
            point(4.0, 1.0, 100),
            point(5.0, 1.0, 92),
            point(5.5, 0.5, 10),
        ];
        let shape = ramp_tail_profile(&series, 100, Duration::from_secs_f64(5.5));
        assert!(shape.reached);
        assert_eq!(shape.ramp, Duration::from_secs(2));
        assert_eq!(shape.plateau, Duration::from_secs(3));
        assert_eq!(shape.tail, Duration::from_secs_f64(0.5));
        assert_eq!(shape.ramp_deficit_bytes, 90 + 50);
        assert_eq!(shape.tail_deficit_bytes, 40);
        assert_eq!(shape.ramp_windows, 2);
    }

    #[test]
    fn ramp_tail_is_empty_without_a_plateau_or_a_peak() {
        let series = vec![point(1.0, 1.0, 10), point(2.0, 1.0, 20)];
        assert_eq!(
            ramp_tail_profile(&series, 100, Duration::from_secs(2)),
            RampTail::default()
        );
        assert_eq!(
            ramp_tail_profile(&series, 0, Duration::from_secs(2)),
            RampTail::default()
        );
        assert_eq!(
            ramp_tail_profile(&[], 100, Duration::from_secs(2)),
            RampTail::default()
        );
    }

    #[test]
    fn ramp_tail_with_one_plateau_window() {
        let series = vec![point(1.0, 1.0, 100)];
        let shape = ramp_tail_profile(&series, 100, Duration::from_secs_f64(1.4));
        assert_eq!(shape.ramp, Duration::ZERO);
        assert_eq!(shape.plateau, Duration::from_secs(1));
        assert_eq!(shape.tail, Duration::from_secs_f64(0.4));
        assert_eq!(shape.ramp_deficit_bytes + shape.tail_deficit_bytes, 0);
    }

    #[test]
    fn stop_sampler_sets_flag() {
        let m = DownloadMetrics::new();
        assert!(!m.sampler_stop.load(Ordering::Relaxed));
        m.stop_sampler();
        assert!(m.sampler_stop.load(Ordering::Relaxed));
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn interval_rate_divides_by_measured_time() {
        assert_eq!(interval_rate(1_000_000, secs(1.0)), 1_000_000);
        assert_eq!(interval_rate(1_800_000, secs(1.8)), 1_000_000);
        assert_eq!(interval_rate(200_000, secs(0.2)), 1_000_000);
        assert_eq!(interval_rate(0, secs(1.0)), 0);
        assert_eq!(interval_rate(500, Duration::ZERO), 0);
    }

    #[test]
    fn delayed_tick_is_not_reported_as_a_faster_second() {
        let epoch = Instant::now();
        let mut sampler = RateSampler::new(epoch);

        let sample = sampler.sample(1_800_000, 0, epoch + secs(1.8));
        assert_eq!(sample.elapsed, secs(1.8));
        assert_eq!(sample.network_bps, 1_000_000);
        assert!(sample.peak_eligible());
    }

    #[test]
    fn immediate_tick_reports_zero_rate_and_no_peak() {
        let epoch = Instant::now();
        let mut sampler = RateSampler::new(epoch);
        sampler.sample(1_000, 0, epoch + secs(1.0));
        let burst = sampler.sample(2_000, 0, epoch + secs(1.0));
        assert_eq!(burst.elapsed, Duration::ZERO);
        assert_eq!(burst.network_delta, 1_000);
        assert_eq!(burst.network_bps, 0);
        assert!(!burst.peak_eligible());
    }

    #[test]
    fn burst_catch_up_ticks_split_bytes_over_their_own_intervals() {
        let epoch = Instant::now();
        let mut sampler = RateSampler::new(epoch);
        let late = sampler.sample(3_000_000, 0, epoch + secs(3.0));
        let catch_up = sampler.sample(3_050_000, 0, epoch + secs(3.05));
        assert_eq!(late.network_bps, 1_000_000);
        assert_eq!(catch_up.network_delta, 50_000);
        assert_eq!(catch_up.network_bps, 1_000_000);
        assert!(!catch_up.peak_eligible());
    }

    #[test]
    fn final_partial_interval_keeps_its_bytes_but_not_the_peak() {
        let epoch = Instant::now();
        let mut sampler = RateSampler::new(epoch);
        sampler.sample(1_000_000, 0, epoch + secs(1.0));
        let tail = sampler.sample(1_200_000, 0, epoch + secs(1.2));
        assert_eq!(tail.elapsed, secs(0.2));
        assert_eq!(tail.network_delta, 200_000);
        assert_eq!(tail.network_bps, 1_000_000);
        assert!(!tail.peak_eligible());
    }

    #[test]
    fn deltas_conserve_bytes_across_arbitrary_intervals() {
        let epoch = Instant::now();
        let mut sampler = RateSampler::new(epoch);
        let stream = [
            (0.7, 100_u64),
            (1.0, 250),
            (1.0, 250),
            (2.6, 900),
            (2.6, 900),
            (2.9, 1_000),
        ];
        let mut total = 0;
        let mut disk_total = 0;
        for (at, bytes) in stream {
            let sample = sampler.sample(bytes, bytes / 2, epoch + secs(at));
            total += sample.network_delta;
            disk_total += sample.disk_delta;
        }
        assert_eq!(total, 1_000);
        assert_eq!(disk_total, 500);
    }

    #[test]
    fn peak_records_the_widest_qualifying_window_rate() {
        let m = DownloadMetrics::new();
        let epoch = Instant::now();
        let mut sampler = RateSampler::new(epoch);
        m.record_peak(&sampler.sample(1_000_000, 0, epoch + secs(1.0)));

        m.record_peak(&sampler.sample(2_000_000, 0, epoch + secs(1.2)));
        m.record_peak(&sampler.sample(4_000_000, 0, epoch + secs(2.2)));
        assert_eq!(m.peak_network_bps(), 2_000_000);
        assert_eq!(m.peak_window(), secs(1.0));
    }

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
