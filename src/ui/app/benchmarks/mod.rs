//! Recording of user-triggered repository actions into saved benchmarks.
//!
//! An entry point arms a capture right before it starts a sync; the capture
//! samples the live progress state once a second; when the sync reports, the
//! capture becomes a draft the user can save from a modal. Saving slices the
//! log, describes the machine and writes the folder on a worker thread.

mod prompt;
mod save;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use eframe::egui;
use log::info;

use crate::core::api::{ProgressEvent, SyncMode};
use crate::core::benchmarks::{
    BenchmarkKind, BenchmarkMetrics, BenchmarkOutcome, BenchmarkRecord, BenchmarkRepository,
    BenchmarkSample, BenchmarkStageMark,
};
use crate::ui::app::Foxy;

pub use save::BenchmarkWorkerResult;

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Set by a user-facing entry point just before it starts the sync.
#[derive(Clone, Debug)]
pub struct BenchmarkArm {
    pub kind: BenchmarkKind,
    pub addons: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct BenchmarkCapture {
    pub kind: BenchmarkKind,
    pub addons: Vec<String>,
    pub repository: BenchmarkRepository,
    pub started: Instant,
    pub started_wall: DateTime<Local>,
    pub samples: Vec<BenchmarkSample>,
    pub stage_marks: Vec<BenchmarkStageMark>,
    pub last_sample_at: Option<Instant>,
    pub last_cpu_percent: f64,
    pub last_disk_write_bps: f64,
    pub last_telemetry_memory: u64,
}

/// A finished capture waiting for the user's decision in the save modal.
#[derive(Clone, Debug)]
pub struct BenchmarkDraft {
    pub record: BenchmarkRecord,
    pub frame_start: DateTime<Local>,
    pub frame_end: DateTime<Local>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkSort {
    Newest,
    Oldest,
    Longest,
    Shortest,
    Name,
    Repository,
    Kind,
}

impl BenchmarkSort {
    pub const ALL: [BenchmarkSort; 7] = [
        BenchmarkSort::Newest,
        BenchmarkSort::Oldest,
        BenchmarkSort::Longest,
        BenchmarkSort::Shortest,
        BenchmarkSort::Name,
        BenchmarkSort::Repository,
        BenchmarkSort::Kind,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BenchmarkSort::Newest => "Newest first",
            BenchmarkSort::Oldest => "Oldest first",
            BenchmarkSort::Longest => "Longest first",
            BenchmarkSort::Shortest => "Shortest first",
            BenchmarkSort::Name => "Name",
            BenchmarkSort::Repository => "Repository",
            BenchmarkSort::Kind => "Kind",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchmarkOutcomeFilter {
    All,
    Success,
    Failed,
}

/// Export renders one chart at a time in a window, waits a few frames for
/// it to lay out, asks the viewport for a screenshot and crops the chart out
/// of it; one chart per pass so every chart fits on screen.
pub struct BenchmarkExportJob {
    pub id: String,
    pub dest: std::path::PathBuf,
    pub chart_index: usize,
    pub chart_count: usize,
    pub frames_rendered: u32,
    pub screenshot_requested: bool,
    pub chart_rect: Option<(String, egui::Rect)>,
    pub images: Vec<crate::core::benchmarks::export::ExportImage>,
}

pub struct BenchmarksViewState {
    pub records: Vec<BenchmarkRecord>,
    pub loaded: bool,
    pub search: String,
    pub sort: BenchmarkSort,
    pub kind_filter: Option<BenchmarkKind>,
    pub repository_filter: Option<String>,
    pub outcome_filter: BenchmarkOutcomeFilter,
    pub favourites_only: bool,
    pub show_hidden: bool,
    pub expanded: HashSet<String>,
    /// Up to two ids picked for the comparison view, in pick order.
    pub selected: Vec<String>,
    /// Compare the older start as A regardless of pick order.
    pub compare_by_date: bool,
    pub compare_open: bool,
    pub pending_remove: Option<String>,
    pub focused_row: Option<String>,
    pub focus_request: Option<String>,
    pub log_cache: HashMap<String, String>,
    pub notes_drafts: HashMap<String, String>,
    pub export: Option<BenchmarkExportJob>,
}

impl Default for BenchmarksViewState {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            loaded: false,
            search: String::new(),
            sort: BenchmarkSort::Newest,
            kind_filter: None,
            repository_filter: None,
            outcome_filter: BenchmarkOutcomeFilter::All,
            favourites_only: false,
            show_hidden: false,
            expanded: HashSet::new(),
            selected: Vec::new(),
            compare_by_date: true,
            compare_open: false,
            pending_remove: None,
            focused_row: None,
            focus_request: None,
            log_cache: HashMap::new(),
            notes_drafts: HashMap::new(),
            export: None,
        }
    }
}

impl BenchmarksViewState {
    pub fn record(&self, id: &str) -> Option<&BenchmarkRecord> {
        self.records.iter().find(|record| record.id == id)
    }

    pub fn record_mut(&mut self, id: &str) -> Option<&mut BenchmarkRecord> {
        self.records.iter_mut().find(|record| record.id == id)
    }

    /// Ids of the records that pass the search, filters and sort, in order.
    pub fn visible_ids(&self) -> Vec<String> {
        let needle = self.search.trim().to_lowercase();
        let mut visible: Vec<&BenchmarkRecord> = self
            .records
            .iter()
            .filter(|record| self.show_hidden || !record.hidden)
            .filter(|record| !self.favourites_only || record.favourite)
            .filter(|record| self.kind_filter.is_none_or(|kind| record.kind == kind))
            .filter(|record| {
                self.repository_filter
                    .as_deref()
                    .is_none_or(|url| record.repository.url == url)
            })
            .filter(|record| match self.outcome_filter {
                BenchmarkOutcomeFilter::All => true,
                BenchmarkOutcomeFilter::Success => record.outcome == BenchmarkOutcome::Success,
                BenchmarkOutcomeFilter::Failed => {
                    matches!(record.outcome, BenchmarkOutcome::Failed { .. })
                }
            })
            .filter(|record| needle.is_empty() || record.search_haystack().contains(&needle))
            .collect();
        match self.sort {
            BenchmarkSort::Newest => visible.sort_by_key(|r| std::cmp::Reverse(r.started_at)),
            BenchmarkSort::Oldest => visible.sort_by_key(|r| r.started_at),
            BenchmarkSort::Longest => visible.sort_by_key(|r| std::cmp::Reverse(r.elapsed_ms)),
            BenchmarkSort::Shortest => visible.sort_by_key(|r| r.elapsed_ms),
            BenchmarkSort::Name => visible.sort_by_key(|r| r.name.to_lowercase()),
            BenchmarkSort::Repository => visible.sort_by(|a, b| {
                a.repository
                    .name
                    .to_lowercase()
                    .cmp(&b.repository.name.to_lowercase())
                    .then(b.started_at.cmp(&a.started_at))
            }),
            BenchmarkSort::Kind => {
                visible.sort_by(|a, b| a.kind.cmp(&b.kind).then(b.started_at.cmp(&a.started_at)))
            }
        }
        visible
            .into_iter()
            .map(|record| record.id.clone())
            .collect()
    }

    /// Distinct repositories among all records, for the repository filter.
    pub fn repositories(&self) -> Vec<(String, String)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for record in &self.records {
            if seen.insert(record.repository.url.clone()) {
                out.push((
                    record.repository.url.clone(),
                    record.repository.name.clone(),
                ));
            }
        }
        out.sort_by_key(|(_, name)| name.to_lowercase());
        out
    }

    pub fn toggle_selected(&mut self, id: &str) {
        if let Some(pos) = self.selected.iter().position(|known| known == id) {
            self.selected.remove(pos);
        } else {
            if self.selected.len() >= 2 {
                self.selected.remove(0);
            }
            self.selected.push(id.to_owned());
        }
        if self.selected.len() < 2 {
            self.compare_open = false;
        }
    }

    /// Selected ids as A then B.
    pub fn compare_order(&self) -> Vec<String> {
        let picked: Vec<(&str, i64)> = self
            .selected
            .iter()
            .filter_map(|id| {
                self.record(id)
                    .map(|record| (id.as_str(), record.started_at))
            })
            .collect();
        compare_order(&picked, self.compare_by_date)
    }

    /// Swap A and B. Date order cannot express a swap, so it switches to pick
    /// order with the current pair reversed.
    pub fn swap_compare_order(&mut self) {
        let mut order = self.compare_order();
        order.reverse();
        self.selected = order;
        self.compare_by_date = false;
    }

    pub fn forget(&mut self, id: &str) {
        self.records.retain(|record| record.id != id);
        self.expanded.remove(id);
        self.selected.retain(|known| known != id);
        self.log_cache.remove(id);
        self.notes_drafts.remove(id);
        if self.selected.len() < 2 {
            self.compare_open = false;
        }
        if self.pending_remove.as_deref() == Some(id) {
            self.pending_remove = None;
        }
    }
}

/// `picked` is (id, started_at) in pick order; `by_date` puts the older start
/// first, keeping pick order for equal starts.
fn compare_order(picked: &[(&str, i64)], by_date: bool) -> Vec<String> {
    let mut order = picked.to_vec();
    if by_date {
        order.sort_by_key(|(_, started_at)| *started_at);
    }
    order.into_iter().map(|(id, _)| id.to_owned()).collect()
}

pub struct BenchmarkChannels {
    pub save_tx: StdSender<BenchmarkWorkerResult>,
    pub save_rx: StdReceiver<BenchmarkWorkerResult>,
}

impl Default for BenchmarkChannels {
    fn default() -> Self {
        let (save_tx, save_rx) = std::sync::mpsc::channel();
        Self { save_tx, save_rx }
    }
}

impl Foxy {
    /// Mark the next sync start as a benchmarkable user action. Ignored while
    /// the benchmarks setting is off, so entry points call it unconditionally.
    pub(crate) fn arm_benchmark(&mut self, kind: BenchmarkKind, addons: Vec<String>) {
        if !self.settings_view_state.benchmarks_enabled {
            return;
        }
        self.benchmark_armed = Some(BenchmarkArm { kind, addons });
    }

    /// Called by the sync starter right before it spawns the worker, with the
    /// arm it took at entry so a refused start cannot leak it to a later sync.
    pub(crate) fn begin_benchmark_capture(&mut self, arm: Option<BenchmarkArm>, repo_idx: usize) {
        let Some(arm) = arm else {
            return;
        };
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return;
        };
        let space_id = repo.repository_space_id.clone();
        self.benchmark_capture = Some(BenchmarkCapture {
            kind: arm.kind,
            addons: arm.addons,
            repository: BenchmarkRepository {
                name: repo.name.clone(),
                url: Self::normalize_repo_url(&repo.address),
                local_path: repo.path.clone(),
                space_id,
            },
            started: Instant::now(),
            started_wall: Local::now(),
            samples: Vec::new(),
            stage_marks: Vec::new(),
            last_sample_at: None,
            last_cpu_percent: 0.0,
            last_disk_write_bps: 0.0,
            last_telemetry_memory: 0,
        });
        info!(
            "Benchmark capture started: kind={:?}",
            self.benchmark_capture.as_ref().map(|c| c.kind)
        );
    }

    /// Feed a progress event into the running capture.
    pub(crate) fn benchmark_observe_event(&mut self, event: &ProgressEvent) {
        let Some(capture) = self.benchmark_capture.as_mut() else {
            return;
        };
        match event {
            ProgressEvent::Stage { label, .. } => {
                let t_ms = capture.started.elapsed().as_millis() as u64;
                if capture
                    .stage_marks
                    .last()
                    .is_none_or(|mark| mark.label != *label)
                {
                    capture.stage_marks.push(BenchmarkStageMark {
                        t_ms,
                        label: label.clone(),
                    });
                }
            }
            ProgressEvent::DownloadTelemetry {
                cpu_percent,
                disk_write_bps,
                memory_bytes,
                ..
            } => {
                capture.last_cpu_percent = *cpu_percent;
                capture.last_disk_write_bps = *disk_write_bps;
                capture.last_telemetry_memory = *memory_bytes;
            }
            _ => {}
        }
    }

    /// Once-a-second sample of the live progress state while a capture runs.
    pub(crate) fn tick_benchmark_capture(&mut self, ctx: &egui::Context) {
        let Some(capture) = self.benchmark_capture.as_ref() else {
            return;
        };
        let now = Instant::now();
        if capture
            .last_sample_at
            .is_some_and(|at| now.duration_since(at) < SAMPLE_INTERVAL)
        {
            ctx.request_repaint_after(SAMPLE_INTERVAL);
            return;
        }
        let memory_bytes = crate::ui::memory::sample_process_memory()
            .baseline_bytes()
            .unwrap_or(capture.last_telemetry_memory);
        let (hash_files_done, hash_files_total) = self
            .recheck_hash_counter
            .map(|(done, total)| (done as u64, total as u64))
            .unwrap_or((0, 0));
        let (hash_parts_done, hash_parts_total) = self
            .recheck_hash_part_counter
            .map(|(done, total)| (done as u64, total as u64))
            .unwrap_or((0, 0));
        let progress_percent = self
            .download_progress
            .as_ref()
            .map(|(_, percent)| *percent)
            .or_else(|| self.recheck_progress_fraction())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
            * 100.0;
        let sample = BenchmarkSample {
            t_ms: capture.started.elapsed().as_millis() as u64,
            downloaded_bytes: self.total_downloaded_bytes,
            download_bps: self.download_speed_bps,
            disk_write_bps: capture.last_disk_write_bps,
            cpu_percent: capture.last_cpu_percent,
            memory_bytes,
            hash_files_done,
            hash_files_total,
            hash_parts_done,
            hash_parts_total,
            progress_percent,
        };
        if let Some(capture) = self.benchmark_capture.as_mut() {
            capture.samples.push(sample);
            capture.last_sample_at = Some(now);
        }
        ctx.request_repaint_after(SAMPLE_INTERVAL);
    }

    /// Turn the running capture into a draft when the sync reports. Cancelled
    /// runs are dropped; anything else is offered for saving.
    pub(crate) fn benchmark_finish_capture(
        &mut self,
        event: &ProgressEvent,
        mode: Option<SyncMode>,
        pending_updates: usize,
    ) {
        let Some(mut capture) = self.benchmark_capture.take() else {
            return;
        };
        let outcome = match event {
            ProgressEvent::Finished => BenchmarkOutcome::Success,
            ProgressEvent::Failed(message) => BenchmarkOutcome::Failed {
                message: message.clone(),
            },
            _ => {
                info!("Benchmark capture dropped: the action was cancelled");
                return;
            }
        };
        let elapsed = capture.started.elapsed();
        let finished_wall = Local::now();
        let memory_bytes = crate::ui::memory::sample_process_memory()
            .baseline_bytes()
            .unwrap_or(0);
        capture.samples.push(BenchmarkSample {
            t_ms: elapsed.as_millis() as u64,
            downloaded_bytes: self.total_downloaded_bytes,
            download_bps: 0.0,
            disk_write_bps: 0.0,
            cpu_percent: capture.last_cpu_percent,
            memory_bytes,
            hash_files_done: capture.samples.last().map_or(0, |s| s.hash_files_done),
            hash_files_total: capture.samples.last().map_or(0, |s| s.hash_files_total),
            hash_parts_done: capture.samples.last().map_or(0, |s| s.hash_parts_done),
            hash_parts_total: capture.samples.last().map_or(0, |s| s.hash_parts_total),
            progress_percent: 100.0,
        });

        let mut metrics = BenchmarkMetrics {
            downloaded_bytes: self.total_downloaded_bytes,
            pending_updates: if mode == Some(SyncMode::Download) {
                0
            } else {
                pending_updates as u64
            },
            ..BenchmarkMetrics::default()
        };
        if let Some(summary) = self.download_summary.as_ref() {
            metrics.downloaded_bytes = metrics.downloaded_bytes.max(summary.downloaded_bytes);
            metrics.planned_transfer_bytes = summary.planned_transfer_bytes;
            metrics.full_download_bytes = summary.full_download_bytes;
            metrics.patch_savings_bytes = summary.patch_savings_bytes;
            metrics.patched_files = summary.patched_files as u64;
            metrics.mods_updated = summary.mods_updated as u64;
            metrics.files_updated = summary.files_updated as u64;
            metrics.parts_updated = summary.parts_updated as u64;
            metrics.download_stage_ms = summary.download_stage_duration.as_millis() as u64;
            metrics.hash_stage_ms = summary.hash_stage_duration.as_millis() as u64;
            metrics.cumulative_hash_ms = summary.cumulative_hash_duration.as_millis() as u64;
            metrics.avg_download_bps = summary.avg_speed_bps;
        }
        metrics.peak_download_bps = capture
            .samples
            .iter()
            .map(|s| s.download_bps)
            .fold(0.0, f64::max);
        metrics.peak_memory_bytes = capture
            .samples
            .iter()
            .map(|s| s.memory_bytes)
            .max()
            .unwrap_or(0);
        let cpu_samples: Vec<f64> = capture
            .samples
            .iter()
            .map(|s| s.cpu_percent)
            .filter(|cpu| *cpu > 0.0)
            .collect();
        if !cpu_samples.is_empty() {
            metrics.avg_cpu_percent = cpu_samples.iter().sum::<f64>() / cpu_samples.len() as f64;
        }
        metrics.hash_files_total = capture
            .samples
            .iter()
            .map(|s| s.hash_files_total)
            .max()
            .unwrap_or(0);
        metrics.hash_parts_total = capture
            .samples
            .iter()
            .map(|s| s.hash_parts_total)
            .max()
            .unwrap_or(0);

        let default_name = format!(
            "{} {} {}",
            capture.kind.label(),
            capture.repository.name,
            capture.started_wall.format("%Y-%m-%d %H:%M")
        );
        let record = BenchmarkRecord {
            id: crate::core::benchmarks::store::new_benchmark_id(
                capture.kind.slug(),
                finished_wall,
            ),
            record_version: crate::core::benchmarks::BENCHMARK_RECORD_VERSION,
            name: default_name,
            notes: String::new(),
            kind: capture.kind,
            started_at: capture.started_wall.timestamp(),
            started_at_local: capture.started_wall.format("%Y-%m-%d %H:%M:%S").to_string(),
            finished_at_local: finished_wall.format("%Y-%m-%d %H:%M:%S").to_string(),
            elapsed_ms: elapsed.as_millis() as u64,
            outcome,
            operation_id: None,
            repository: capture.repository.clone(),
            addons: capture.addons.clone(),
            favourite: false,
            hidden: false,
            build: crate::core::benchmarks::BenchmarkBuild {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                commit: crate::build_info::GIT_HASH.to_owned(),
                kind: crate::build_info::build_kind().to_owned(),
            },
            machine: crate::core::benchmarks::BenchmarkMachine {
                hash_io_profile: self.settings_view_state.hash_io_profile.to_string(),
                download_speed_limit_mbps: self.settings_view_state.download_speed_limit_mbps,
                extended_diagnostics: self.settings_view_state.extended_diagnostics_logging,
                ..Default::default()
            },
            metrics,
            stages: Vec::new(),
            sol: Vec::new(),
            samples: crate::core::benchmarks::downsample_samples(
                &capture.samples,
                save::MAX_SAVED_SAMPLES,
            ),
            stage_marks: capture.stage_marks.clone(),
            log_file: None,
            log_line_count: 0,
        };
        info!(
            "Benchmark capture finished: kind={:?} elapsed={:.2}s samples={} outcome={}",
            record.kind,
            record.elapsed_secs(),
            record.samples.len(),
            record.outcome.slug()
        );
        self.benchmark_prompt = Some(BenchmarkDraft {
            record,
            frame_start: capture.started_wall,
            frame_end: finished_wall,
        });
        self.needs_repaint = true;
    }

    pub(crate) fn ensure_benchmarks_loaded(&mut self) {
        if self.benchmarks_view.loaded {
            return;
        }
        self.benchmarks_view.records = crate::core::benchmarks::store::load_all_records();
        self.benchmarks_view.loaded = true;
        info!(
            "Loaded {} saved benchmarks",
            self.benchmarks_view.records.len()
        );
        let records = self.benchmarks_view.records.clone();
        std::thread::spawn(move || {
            save::run_blocking(async move {
                match crate::core::benchmarks::index::reconcile(&records).await {
                    Ok(removed) if removed > 0 => {
                        info!("Benchmark index reconciled: dropped {removed} stale rows")
                    }
                    Ok(_) => {}
                    Err(err) => log::warn!("Benchmark index reconcile failed: {err}"),
                }
            });
        });
    }

    pub(crate) fn reload_benchmarks(&mut self) {
        self.benchmarks_view.loaded = false;
        self.benchmarks_view.log_cache.clear();
        self.ensure_benchmarks_loaded();
    }

    /// Flips the setting and applies the extended diagnostics it may have
    /// switched on or off to the running logger.
    pub(crate) fn apply_benchmarks_enabled(&mut self, enabled: bool) {
        if self.settings_view_state.set_benchmarks_enabled(enabled) {
            let diagnostics = self.settings_view_state.extended_diagnostics_logging;
            log::info!(
                "Benchmarks {} extended diagnostics logging",
                if diagnostics { "enabled" } else { "disabled" }
            );
            crate::core::api::set_extended_diagnostics(diagnostics);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compare_order;

    #[test]
    fn compare_order_puts_older_first_by_date_and_keeps_pick_order_otherwise() {
        let picked = [("newer", 200), ("older", 100)];
        assert_eq!(compare_order(&picked, true), vec!["older", "newer"]);
        assert_eq!(compare_order(&picked, false), vec!["newer", "older"]);
        let tie = [("first", 100), ("second", 100)];
        assert_eq!(compare_order(&tie, true), vec!["first", "second"]);
        assert_eq!(compare_order(&[("only", 5)], true), vec!["only"]);
    }
}
