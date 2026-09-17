use std::path::PathBuf;

use chrono::{DateTime, Local};
use log::{info, warn};

use super::BenchmarkDraft;
use crate::core::benchmarks::export::ExportImage;
use crate::core::benchmarks::log_slice::{
    FRAME_SLACK_AFTER_MS, FRAME_SLACK_BEFORE_MS, STARTUP_WINDOW_MS, gather_log_lines,
    owned_sol_lines, parse_pipeline_summary, parse_sol_lines, slice_lines,
};
use crate::core::benchmarks::{BenchmarkRecord, index, store};
use crate::core::utils::app_paths;
use crate::ui::app::Foxy;

/// Roughly twenty minutes at 1 Hz; longer runs are thinned, which keeps a
/// record file well under a megabyte.
pub(super) const MAX_SAVED_SAMPLES: usize = 1200;

pub enum BenchmarkWorkerResult {
    Saved(Box<BenchmarkRecord>),
    SaveFailed(String),
    Exported(PathBuf),
    ExportFailed(String),
}

pub(super) fn run_blocking<F: std::future::Future>(future: F) -> Option<F::Output> {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => Some(runtime.block_on(future)),
        Err(err) => {
            warn!("Benchmark worker could not create a runtime: {err}");
            None
        }
    }
}

fn process_start_wall() -> DateTime<Local> {
    let elapsed = crate::core::api::process_start_elapsed();
    Local::now() - chrono::Duration::from_std(elapsed).unwrap_or_default()
}

fn describe_machine(record: &mut BenchmarkRecord) {
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, System};
    let mut system = System::new();
    system.refresh_cpu_list(CpuRefreshKind::nothing());
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    record.machine.os = format!(
        "{} {}",
        System::name().unwrap_or_default(),
        System::os_version().unwrap_or_default()
    )
    .trim()
    .to_owned();
    record.machine.cpu = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_owned())
        .unwrap_or_default();
    record.machine.cpu_cores = System::physical_core_count().unwrap_or_else(|| system.cpus().len());
    record.machine.cpu_threads = system.cpus().len();
    record.machine.total_memory_bytes = system.total_memory();
    record.machine.repository_storage_class =
        crate::core::tasks::calculate_hashes::detect_storage_class_for_path(
            &record.repository.local_path,
        )
        .to_string();
}

/// Everything the save does off the UI thread: slice the log, describe the
/// machine, write the folder, index the row.
fn save_draft_blocking(mut draft: BenchmarkDraft) -> Result<BenchmarkRecord, String> {
    let start_wall = process_start_wall();
    let startup_from = start_wall.timestamp_millis();
    let lines = gather_log_lines(&app_paths::foxy_logs_dir(), startup_from);
    let slice = slice_lines(
        lines.iter().map(String::as_str),
        (startup_from - 1_000, startup_from + STARTUP_WINDOW_MS),
        (
            draft.frame_start.timestamp_millis() - FRAME_SLACK_BEFORE_MS,
            draft.frame_end.timestamp_millis() + FRAME_SLACK_AFTER_MS,
        ),
    );
    let frame = slice.frame.iter().map(String::as_str);
    if let Some(summary) = parse_pipeline_summary(frame.clone()) {
        draft.record.stages = summary.stages;
        draft.record.operation_id = Some(summary.operation_id);
    }
    draft.record.sol =
        owned_sol_lines(parse_sol_lines(frame), draft.record.operation_id.as_deref());
    draft.record.log_line_count = slice.line_count();
    draft.record.log_file = (slice.line_count() > 0).then(|| store::LOG_FILE.to_owned());
    describe_machine(&mut draft.record);

    let header = format!(
        "Foxy benchmark {} ({} {}) {} .. {}",
        draft.record.id,
        draft.record.kind.label(),
        draft.record.repository.name,
        draft.record.started_at_local,
        draft.record.finished_at_local
    );
    let log_text = draft
        .record
        .log_file
        .is_some()
        .then(|| slice.render(&header));
    store::save_record(&draft.record, log_text.as_deref()).map_err(|err| format!("{err:#}"))?;
    if let Some(Err(err)) = run_blocking(index::upsert_record(&draft.record)) {
        warn!("Benchmark {} saved but not indexed: {err}", draft.record.id);
    }
    info!(
        "Benchmark saved: id={} log_lines={} stages={} sol={}",
        draft.record.id,
        draft.record.log_line_count,
        draft.record.stages.len(),
        draft.record.sol.len()
    );
    Ok(draft.record)
}

impl Foxy {
    pub(crate) fn spawn_benchmark_save(&mut self, draft: BenchmarkDraft) {
        let tx = self.benchmark_channels.save_tx.clone();
        let repaint = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let result = match save_draft_blocking(draft) {
                Ok(record) => BenchmarkWorkerResult::Saved(Box::new(record)),
                Err(error) => BenchmarkWorkerResult::SaveFailed(error),
            };
            if tx.send(result).is_ok() {
                Self::request_background_repaint(repaint.as_ref());
            }
        });
    }

    pub(crate) fn poll_benchmark_save_results(&mut self) {
        while let Ok(result) = self.benchmark_channels.save_rx.try_recv() {
            match result {
                BenchmarkWorkerResult::Saved(record) => {
                    self.benchmarks_view.forget(&record.id);
                    self.benchmarks_view.records.insert(0, *record);
                    self.show_success_toast(self.t("Benchmark saved."));
                }
                BenchmarkWorkerResult::SaveFailed(error) => {
                    warn!("Benchmark save failed: {error}");
                    self.show_error_toast(self.t("Failed to save benchmark."));
                }
                BenchmarkWorkerResult::Exported(dest) => {
                    let name = dest
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.show_success_toast(
                        self.t_fmt("Benchmark exported to {path}.", &[("path", name)]),
                    );
                }
                BenchmarkWorkerResult::ExportFailed(error) => {
                    warn!("Benchmark export failed: {error}");
                    self.show_error_toast(self.t("Failed to export benchmark."));
                }
            }
            self.needs_repaint = true;
        }
    }

    /// Persist an edited record (flags, name, notes) and refresh its index row.
    pub(crate) fn persist_benchmark_record(&mut self, id: &str) {
        let Some(record) = self.benchmarks_view.record(id).cloned() else {
            return;
        };
        std::thread::spawn(move || {
            if let Err(err) = store::write_record(&record) {
                warn!("Failed to write benchmark {}: {err:#}", record.id);
                return;
            }
            if let Some(Err(err)) = run_blocking(index::upsert_record(&record)) {
                warn!("Failed to index benchmark {}: {err}", record.id);
            }
        });
    }

    pub(crate) fn remove_benchmark(&mut self, id: &str) {
        self.benchmarks_view.forget(id);
        let id = id.to_owned();
        std::thread::spawn(move || {
            if let Err(err) = store::delete_record(&id) {
                warn!("Failed to remove benchmark {id}: {err:#}");
            }
            if let Some(Err(err)) = run_blocking(index::delete_record(&id)) {
                warn!("Failed to drop benchmark index row {id}: {err}");
            }
            info!("Benchmark removed: {id}");
        });
    }

    pub(crate) fn benchmark_log_text(&mut self, id: &str) -> Option<String> {
        if let Some(text) = self.benchmarks_view.log_cache.get(id) {
            return Some(text.clone());
        }
        let record = self.benchmarks_view.record(id)?;
        let text = store::read_log(record)?;
        self.benchmarks_view
            .log_cache
            .insert(id.to_owned(), text.clone());
        Some(text)
    }

    pub(crate) fn write_benchmark_export(
        &mut self,
        id: &str,
        dest: PathBuf,
        images: Vec<ExportImage>,
    ) {
        let Some(record) = self.benchmarks_view.record(id).cloned() else {
            return;
        };
        let log_text = store::read_log(&record);
        let best = crate::core::benchmarks::best_comparable(&record, &self.benchmarks_view.records);
        let tx = self.benchmark_channels.save_tx.clone();
        let repaint = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let result = match crate::core::benchmarks::export::write_zip(
                &dest,
                &record,
                best.as_ref(),
                log_text.as_deref(),
                &images,
            ) {
                Ok(()) => {
                    info!(
                        "Benchmark exported: id={} images={}",
                        record.id,
                        images.len()
                    );
                    BenchmarkWorkerResult::Exported(dest)
                }
                Err(err) => BenchmarkWorkerResult::ExportFailed(format!("{err:#}")),
            };
            if tx.send(result).is_ok() {
                Self::request_background_repaint(repaint.as_ref());
            }
        });
    }
}
