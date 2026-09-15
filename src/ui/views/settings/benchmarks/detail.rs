//! Expanded view of one benchmark: headline tiles, grouped metadata and
//! metrics, stage bars, charts, notes and the log slice.

use eframe::egui::{self, RichText, ScrollArea, TextEdit, Ui};

use super::charts::{ChartTheme, bar_chart, fmt_duration, line_chart};
use super::series::{charts_for, cpu_scale, cpu_title, stage_rows};
use super::widgets::{panel, text_scale};
use crate::core::benchmarks::{BenchmarkOutcome, BenchmarkRecord, SolDetailValue};
use crate::ui::app::Foxy;
use crate::ui::i18n::fmt_bytes;
use crate::ui::palette;

const SOL_BAR_WIDTH: f32 = 90.0;
const SOL_BAR_HEIGHT: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricUnit {
    Bytes,
    BytesPerSec,
    Seconds,
    Count,
    Percent,
}

pub struct MetricRow {
    pub label: &'static str,
    pub unit: MetricUnit,
    pub value: f64,
    pub lower_is_better: bool,
}

pub fn fmt_metric(unit: MetricUnit, value: f64) -> String {
    match unit {
        MetricUnit::Bytes => fmt_bytes(value.max(0.0) as u64),
        MetricUnit::BytesPerSec => format!("{}/s", fmt_bytes(value.max(0.0) as u64)),
        MetricUnit::Seconds => fmt_duration(value),
        MetricUnit::Count => format!("{value:.0}"),
        MetricUnit::Percent => format!("{value:.1}%"),
    }
}

pub fn metric_rows(record: &BenchmarkRecord) -> Vec<MetricRow> {
    let m = &record.metrics;
    let row = |label, unit, value: f64, lower_is_better| MetricRow {
        label,
        unit,
        value,
        lower_is_better,
    };
    vec![
        row("Elapsed", MetricUnit::Seconds, record.elapsed_secs(), true),
        row(
            "Download stage",
            MetricUnit::Seconds,
            m.download_stage_ms as f64 / 1000.0,
            true,
        ),
        row(
            "Hash stage",
            MetricUnit::Seconds,
            m.hash_stage_ms as f64 / 1000.0,
            true,
        ),
        row(
            "Downloaded",
            MetricUnit::Bytes,
            m.downloaded_bytes as f64,
            true,
        ),
        row(
            "Planned transfer",
            MetricUnit::Bytes,
            m.planned_transfer_bytes as f64,
            true,
        ),
        row(
            "Full download size",
            MetricUnit::Bytes,
            m.full_download_bytes as f64,
            true,
        ),
        row(
            "Patch savings",
            MetricUnit::Bytes,
            m.patch_savings_bytes as f64,
            false,
        ),
        row(
            "Patched files",
            MetricUnit::Count,
            m.patched_files as f64,
            false,
        ),
        row(
            "Mods updated",
            MetricUnit::Count,
            m.mods_updated as f64,
            false,
        ),
        row(
            "Files updated",
            MetricUnit::Count,
            m.files_updated as f64,
            false,
        ),
        row(
            "Parts updated",
            MetricUnit::Count,
            m.parts_updated as f64,
            false,
        ),
        row(
            "Average download speed",
            MetricUnit::BytesPerSec,
            m.avg_download_bps,
            false,
        ),
        row(
            "Peak download speed",
            MetricUnit::BytesPerSec,
            m.peak_download_bps,
            false,
        ),
        row(
            "Files checked",
            MetricUnit::Count,
            m.hash_files_total as f64,
            false,
        ),
        row(
            "Parts checked",
            MetricUnit::Count,
            m.hash_parts_total as f64,
            false,
        ),
        row(
            "Pending updates found",
            MetricUnit::Count,
            m.pending_updates as f64,
            false,
        ),
        row(
            "Peak memory",
            MetricUnit::Bytes,
            m.peak_memory_bytes as f64,
            true,
        ),
        row(
            "Average CPU",
            MetricUnit::Percent,
            m.avg_cpu_percent * cpu_scale(record),
            true,
        ),
    ]
}

/// The figures worth a headline tile for this record: caption key, value
/// text and whether it is a rate (accent coloured).
/// A speed-of-light ratio as a percentage; 100% is running at the light.
pub fn fmt_sol(sol: f64) -> String {
    format!("{:.0}%", (sol.clamp(0.0, 1.0) * 100.0).round())
}

pub fn headline_stats(record: &BenchmarkRecord) -> Vec<(&'static str, String, bool)> {
    let m = &record.metrics;
    let mut stats = vec![("Elapsed", format!("{:.1} s", record.elapsed_secs()), false)];
    if record.kind.transfers_files() && m.avg_download_bps > 0.0 {
        stats.push((
            "Average download speed",
            format!("{}/s", fmt_bytes(m.avg_download_bps as u64)),
            true,
        ));
    }
    if m.downloaded_bytes > 0 {
        stats.push(("Downloaded", fmt_bytes(m.downloaded_bytes), false));
    }
    if m.patch_savings_bytes > 0 {
        stats.push(("Patch savings", fmt_bytes(m.patch_savings_bytes), false));
    }
    if m.hash_files_total > 0 {
        stats.push(("Files checked", m.hash_files_total.to_string(), false));
        let secs = record.elapsed_secs();
        if secs > 0.0 && !record.kind.transfers_files() {
            stats.push((
                "Files per second",
                format!("{:.0}", m.hash_files_total as f64 / secs),
                true,
            ));
        }
    }
    if m.pending_updates > 0 {
        stats.push((
            "Pending updates found",
            m.pending_updates.to_string(),
            false,
        ));
    }
    if let Some(sol) = record.headline_sol().and_then(|summary| summary.sol) {
        stats.push(("Speed of light", fmt_sol(sol), true));
    }
    if m.peak_memory_bytes > 0 {
        stats.push(("Peak memory", fmt_bytes(m.peak_memory_bytes), false));
    }
    if m.avg_cpu_percent > 0.0 {
        stats.push((
            cpu_title(record),
            format!("{:.1}%", m.avg_cpu_percent * cpu_scale(record)),
            false,
        ));
    }
    stats
}

impl Foxy {
    pub(super) fn benchmark_chart_theme(&self) -> ChartTheme {
        // Opaque on purpose: the export crops the chart out of a screenshot
        // and a translucent fill would let the page behind bleed through.
        ChartTheme {
            background: self.color_card_bg().to_opaque(),
            plot: self.color_main_bg().to_opaque(),
            grid: palette::BENCHMARK_CHART_GRID,
            axis: palette::BENCHMARK_CHART_AXIS,
            text: self.color_text_normal(),
            muted: self.color_text_dim(),
        }
    }

    pub(super) fn outcome_text(&self, outcome: &BenchmarkOutcome) -> RichText {
        match outcome {
            BenchmarkOutcome::Success => {
                RichText::new(self.t("Success")).color(self.color_success())
            }
            BenchmarkOutcome::Failed { message } => {
                RichText::new(format!("{}: {}", self.t("Failed"), message))
                    .color(self.color_error())
            }
            BenchmarkOutcome::Cancelled => RichText::new(self.t("Operation cancelled")),
        }
    }

    /// Charts of one record, or only the `only`th of them; returns the key
    /// and rect of every chart drawn so the export can crop them out of a
    /// screenshot. On screen the line charts sit in two columns; a single
    /// requested chart is drawn full width.
    pub(super) fn render_benchmark_charts(
        &self,
        ui: &mut Ui,
        record: &BenchmarkRecord,
        chart_height: f32,
        only: Option<usize>,
    ) -> Vec<(String, egui::Rect)> {
        let theme = self.benchmark_chart_theme();
        let defs = charts_for(record, "", &palette::BENCHMARK_SERIES);
        let has_stages = !record.stages.is_empty();
        let mut rects = Vec::new();
        let stage_legend = [(record.name.clone(), palette::BENCHMARK_SERIES[0])];
        if let Some(index) = only {
            let line_index = if has_stages {
                if index == 0 {
                    let rect = bar_chart(
                        ui,
                        &self.t("Stage durations"),
                        &stage_rows(&[record]),
                        &stage_legend,
                        theme,
                    );
                    return vec![("stages".to_owned(), rect)];
                }
                index - 1
            } else {
                index
            };
            if let Some(def) = defs.get(line_index) {
                let rect = line_chart(
                    ui,
                    chart_height,
                    &self.t(def.title),
                    def.unit,
                    &def.series,
                    theme,
                );
                rects.push((def.key.to_owned(), rect));
            }
            return rects;
        }
        if has_stages {
            let rect = bar_chart(
                ui,
                &self.t("Stage durations"),
                &stage_rows(&[record]),
                &stage_legend,
                theme,
            );
            rects.push(("stages".to_owned(), rect));
            ui.add_space(6.0);
        }
        if !defs.is_empty() {
            ui.columns(2, |columns| {
                for (index, def) in defs.iter().enumerate() {
                    let column = &mut columns[index % 2];
                    let rect = line_chart(
                        column,
                        chart_height,
                        &self.t(def.title),
                        def.unit,
                        &def.series,
                        theme,
                    );
                    rects.push((def.key.to_owned(), rect));
                    column.add_space(6.0);
                }
            });
        }
        if rects.is_empty() {
            ui.label(
                RichText::new(self.t("No samples were recorded for this benchmark."))
                    .color(self.color_text_dim()),
            );
        }
        rects
    }

    pub(super) fn benchmark_chart_count(record: &BenchmarkRecord) -> usize {
        usize::from(!record.stages.is_empty())
            + charts_for(record, "", &palette::BENCHMARK_SERIES).len()
    }

    fn render_benchmark_info_grid(
        &self,
        ui: &mut Ui,
        salt: (&str, &str),
        rows: Vec<(String, String)>,
    ) {
        let dim = self.color_text_dim();
        let scale = text_scale(ui);
        egui::Grid::new(salt)
            .num_columns(2)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for (label, value) in rows {
                    ui.label(RichText::new(label).size(scale.small).color(dim));
                    ui.add(egui::Label::new(RichText::new(value).size(scale.small)).truncate());
                    ui.end_row();
                }
            });
    }

    /// Fill colour of a speed-of-light bar: green near the light, amber
    /// with headroom, red when most of the time was not the work.
    pub(super) fn sol_color(&self, sol: f64) -> egui::Color32 {
        if sol >= 0.85 {
            self.color_success()
        } else if sol >= 0.5 {
            self.color_warn()
        } else {
            self.color_error()
        }
    }

    /// Percentage with a proportion bar; `None` renders as `n/a`.
    pub(super) fn sol_cell(&self, ui: &mut Ui, sol: Option<f64>, hover: &str) {
        let scale = text_scale(ui);
        ui.horizontal(|ui| {
            let Some(sol) = sol else {
                ui.label(
                    RichText::new("n/a")
                        .size(scale.small)
                        .color(self.color_text_dim()),
                )
                .on_hover_text(hover);
                return;
            };
            let (rect, response) = ui.allocate_exact_size(
                egui::Vec2::new(SOL_BAR_WIDTH, SOL_BAR_HEIGHT + 4.0),
                egui::Sense::hover(),
            );
            let track = egui::Rect::from_center_size(
                rect.center(),
                egui::Vec2::new(SOL_BAR_WIDTH, SOL_BAR_HEIGHT),
            );
            let painter = ui.painter();
            painter.rect_filled(track, 2.0, self.color_widget_bg());
            let fill = (sol.clamp(0.0, 1.0) as f32 * SOL_BAR_WIDTH).max(2.0);
            painter.rect_filled(
                egui::Rect::from_min_size(track.min, egui::Vec2::new(fill, SOL_BAR_HEIGHT)),
                2.0,
                self.sol_color(sol),
            );
            response.on_hover_text(hover);
            ui.label(
                RichText::new(fmt_sol(sol))
                    .size(scale.small)
                    .strong()
                    .color(self.sol_color(sol)),
            )
            .on_hover_text(hover);
        });
    }

    fn sol_hover(&self, light_src_label: &str, headroom: Option<f64>) -> String {
        let mut text = format!("{}: {}", self.t("Light source"), self.t(light_src_label));
        if let Some(headroom) = headroom {
            text.push('\n');
            text.push_str(&self.t_fmt(
                "Could be up to {factor}x faster before physics objects.",
                &[("factor", format!("{headroom:.2}"))],
            ));
        }
        text
    }

    fn fmt_sol_detail(&self, value: &SolDetailValue) -> String {
        match value {
            SolDetailValue::Count(count) => format!("{count:.0}"),
            SolDetailValue::Secs(secs) => fmt_duration(*secs),
            SolDetailValue::BytesPerSec(bps) => format!("{}/s", fmt_bytes(bps.max(0.0) as u64)),
            SolDetailValue::Percent(percent) => format!("{percent:.0}%"),
            SolDetailValue::Rate(rate, unit) => format!("{rate:.0} {}", self.t(unit)),
            SolDetailValue::Text(text) => text.clone(),
        }
    }

    /// One resource row of the speed-of-light table: its work, its light,
    /// the ratio and where the light came from.
    #[allow(clippy::too_many_arguments)]
    fn sol_part_row(
        &self,
        ui: &mut Ui,
        name: String,
        work_bytes: u64,
        light_bps: Option<f64>,
        sol: Option<f64>,
        light_src_label: &str,
        headroom: Option<f64>,
    ) {
        let scale = text_scale(ui);
        let dim = self.color_text_dim();
        let hover = self.sol_hover(light_src_label, headroom);
        ui.label(RichText::new(name).size(scale.small));
        ui.label(
            RichText::new(if work_bytes > 0 {
                fmt_bytes(work_bytes)
            } else {
                "n/a".to_owned()
            })
            .size(scale.small)
            .color(dim),
        );
        ui.label(
            RichText::new(light_bps.map_or_else(
                || "n/a".to_owned(),
                |bps| format!("{}/s", fmt_bytes(bps as u64)),
            ))
            .size(scale.small),
        );
        self.sol_cell(ui, sol, &hover);
        ui.label(
            RichText::new(self.t(light_src_label))
                .size(scale.small)
                .color(dim),
        );
        ui.end_row();
    }

    /// One block per operation: its convention category and totals, a row
    /// per resource it was judged against, and the category's own figures.
    fn render_benchmark_sol(&self, ui: &mut Ui, record: &BenchmarkRecord, id: &str) {
        let summaries = record.sol_summaries();
        if summaries.is_empty() {
            return;
        }
        let scale = text_scale(ui);
        let dim = self.color_text_dim();
        ui.add_space(6.0);
        panel(ui, self.color_card_bg(), |ui| {
            ui.horizontal(|ui| {
                self.benchmark_section_title(ui, self.t("Speed of light"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} {}",
                            self.t("Build"),
                            record.build.version,
                            record.build.commit
                        ))
                        .size(scale.small)
                        .color(dim),
                    );
                });
            });
            ui.label(
                RichText::new(self.t(
                    "How close each operation ran to the fastest time physics allows for its work; 100% is the speed of light.",
                ))
                .size(scale.small)
                .color(dim),
            );
            for summary in &summaries {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new(&summary.op).strong());
                    if let Some((code, name)) = summary.category() {
                        self.benchmark_stat_chip(ui, self.t(name), code);
                    }
                    let mut totals = vec![format!(
                        "{} {}",
                        summary.runs,
                        self.t(if summary.runs == 1 { "run" } else { "runs" })
                    )];
                    if summary.work_bytes > 0 {
                        totals.push(fmt_bytes(summary.work_bytes));
                    }
                    totals.push(fmt_duration(summary.actual_s));
                    if let Some(bps) = summary.actual_bps {
                        totals.push(format!("{}/s", fmt_bytes(bps as u64)));
                    }
                    ui.label(
                        RichText::new(totals.join("  \u{00B7}  "))
                            .size(scale.small)
                            .color(dim),
                    );
                });
                ui.add_space(2.0);
                egui::Grid::new(("benchmark_sol_grid", id, summary.op.as_str()))
                    .num_columns(5)
                    .spacing([14.0, 4.0])
                    .min_col_width(70.0)
                    .show(ui, |ui| {
                        for header in [
                            "Resource",
                            "Work",
                            "Light rate",
                            "Speed of light",
                            "Light source",
                        ] {
                            ui.label(
                                RichText::new(self.t(header))
                                    .size(scale.small)
                                    .strong()
                                    .color(dim),
                            );
                        }
                        ui.end_row();
                        self.sol_part_row(
                            ui,
                            self.t(summary.main_part_name()),
                            summary.work_bytes,
                            summary.light_bps,
                            summary.sol,
                            summary.light_src.label(),
                            summary.headroom(),
                        );
                        for part in &summary.sub_parts {
                            self.sol_part_row(
                                ui,
                                self.t(part.name),
                                part.work_bytes,
                                part.light_bps,
                                part.sol,
                                part.light_src.label(),
                                part.headroom(),
                            );
                        }
                    });
                if !summary.details.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::Vec2::new(6.0, 4.0);
                        for detail in &summary.details {
                            self.benchmark_stat_chip(
                                ui,
                                self.t(detail.label),
                                self.fmt_sol_detail(&detail.value),
                            );
                        }
                    });
                }
            }
            ui.add_space(6.0);
            egui::CollapsingHeader::new(RichText::new(self.t("Raw log lines")).size(scale.small))
                .id_salt(("benchmark_sol_raw", id))
                .show(ui, |ui| {
                    for sol in &record.sol {
                        let pairs: Vec<String> =
                            sol.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        ui.label(
                            RichText::new(pairs.join("  "))
                                .monospace()
                                .size(scale.small),
                        );
                    }
                });
        });
    }

    pub(crate) fn render_benchmark_detail(&mut self, ui: &mut Ui, id: &str) {
        let Some(record) = self.benchmarks_view.record(id).cloned() else {
            return;
        };
        let dim = self.color_text_dim();
        let scale = text_scale(ui);
        let panel_bg = self.color_card_bg();
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::Vec2::new(8.0, 8.0);
            for (label, value, is_rate) in headline_stats(&record) {
                let accent = is_rate.then(|| self.color_primary_accent());
                self.benchmark_stat_tile(ui, self.t(label), value, accent);
            }
        });
        ui.add_space(8.0);

        let yes_no = |flag: bool| {
            if flag { self.t("Yes") } else { self.t("No") }
        };
        let mut run_rows = vec![
            (self.t("Action"), self.t(record.kind.label())),
            (self.t("Started"), record.started_at_local.clone()),
            (self.t("Finished"), record.finished_at_local.clone()),
            (self.t("Repository"), record.repository.name.clone()),
            (self.t("Local path"), record.repository.local_path.clone()),
        ];
        if !record.addons.is_empty() {
            run_rows.push((self.t("Addons"), record.addons.join(", ")));
        }
        if let Some(op) = &record.operation_id {
            run_rows.push((self.t("Operation"), op.clone()));
        }
        let machine_rows = vec![
            (
                self.t("Build"),
                format!(
                    "{} ({}, {})",
                    record.build.version, record.build.commit, record.build.kind
                ),
            ),
            (self.t("System"), record.machine.os.clone()),
            (
                self.t("CPU"),
                format!("{} x{}", record.machine.cpu, record.machine.cpu_cores),
            ),
            (self.t("RAM"), fmt_bytes(record.machine.total_memory_bytes)),
            (
                self.t("Repository storage"),
                record.machine.repository_storage_class.clone(),
            ),
            (
                self.t("Hash profile"),
                record.machine.hash_io_profile.clone(),
            ),
            (
                self.t("Speed limit"),
                record
                    .machine
                    .download_speed_limit_mbps
                    .map(|limit| format!("{limit} Mb/s"))
                    .unwrap_or_else(|| self.t("Unlimited")),
            ),
            (
                self.t("Extended diagnostics"),
                yes_no(record.machine.extended_diagnostics),
            ),
        ];
        let metric_pairs: Vec<(String, String)> = metric_rows(&record)
            .into_iter()
            .filter(|metric| metric.value > 0.0 || metric.label == "Elapsed")
            .map(|metric| (self.t(metric.label), fmt_metric(metric.unit, metric.value)))
            .collect();

        ui.columns(3, |columns| {
            panel(&mut columns[0], panel_bg, |ui| {
                self.benchmark_section_title(ui, self.t("Run"));
                self.render_benchmark_info_grid(ui, ("benchmark_run", id), run_rows);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(self.t("Outcome"))
                            .size(scale.small)
                            .color(dim),
                    );
                    ui.label(self.outcome_text(&record.outcome).size(scale.small));
                });
            });
            panel(&mut columns[1], panel_bg, |ui| {
                self.benchmark_section_title(ui, self.t("Machine"));
                self.render_benchmark_info_grid(ui, ("benchmark_machine", id), machine_rows);
            });
            panel(&mut columns[2], panel_bg, |ui| {
                self.benchmark_section_title(ui, self.t("Results"));
                self.render_benchmark_info_grid(ui, ("benchmark_metrics", id), metric_pairs);
            });
        });

        self.render_benchmark_sol(ui, &record, id);
        ui.add_space(8.0);
        self.render_benchmark_charts(ui, &record, 190.0, None);

        ui.add_space(4.0);
        let notes_title = self.t("Notes");
        let notes_hint = self.t("Add notes about this run");
        let save_notes = self.t("Save notes");
        panel(ui, panel_bg, |ui| {
            self.benchmark_section_title(ui, notes_title);
            let draft = self
                .benchmarks_view
                .notes_drafts
                .entry(id.to_owned())
                .or_insert_with(|| record.notes.clone());
            ui.add(
                TextEdit::multiline(draft)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text(notes_hint),
            );
            let draft_text = draft.clone();
            let dirty = draft_text != record.notes;
            ui.add_space(4.0);
            if ui
                .add_enabled(dirty, egui::Button::new(save_notes))
                .clicked()
            {
                if let Some(record) = self.benchmarks_view.record_mut(id) {
                    record.notes = draft_text.trim().to_owned();
                }
                self.persist_benchmark_record(id);
            }
        });

        if record.log_file.is_some() {
            ui.add_space(6.0);
            let header = format!("{} ({})", self.t("Log"), record.log_line_count);
            egui::CollapsingHeader::new(header)
                .id_salt(("benchmark_log", id))
                .show(ui, |ui| {
                    let text = self.benchmark_log_text(id).unwrap_or_default();
                    panel(ui, panel_bg, |ui| {
                        ScrollArea::both()
                            .id_salt(("benchmark_log_scroll", id))
                            .max_height(280.0)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(text).monospace().size(scale.small),
                                    )
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                            });
                    });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_formatting_by_unit() {
        assert_eq!(fmt_metric(MetricUnit::Seconds, 1.5), "1.50 s");
        assert_eq!(fmt_metric(MetricUnit::Count, 12.0), "12");
        assert_eq!(fmt_metric(MetricUnit::Percent, 12.34), "12.3%");
        assert!(fmt_metric(MetricUnit::BytesPerSec, 1024.0).ends_with("/s"));
    }

    #[test]
    fn headline_stats_follow_the_action() {
        use crate::core::benchmarks::*;
        let mut record = BenchmarkRecord {
            id: "x".into(),
            record_version: 1,
            name: "x".into(),
            notes: String::new(),
            kind: BenchmarkKind::Recheck,
            started_at: 0,
            started_at_local: String::new(),
            finished_at_local: String::new(),
            elapsed_ms: 2_000,
            outcome: BenchmarkOutcome::Success,
            operation_id: None,
            repository: BenchmarkRepository::default(),
            addons: vec![],
            favourite: false,
            hidden: false,
            build: BenchmarkBuild::default(),
            machine: BenchmarkMachine::default(),
            metrics: BenchmarkMetrics::default(),
            stages: vec![],
            sol: vec![],
            samples: vec![],
            stage_marks: vec![],
            log_file: None,
            log_line_count: 0,
        };
        record.metrics.hash_files_total = 40;
        let labels: Vec<&str> = headline_stats(&record)
            .iter()
            .map(|(label, _, _)| *label)
            .collect();
        assert_eq!(labels, vec!["Elapsed", "Files checked", "Files per second"]);
        assert_eq!(headline_stats(&record)[2].1, "20");

        record.kind = crate::core::benchmarks::BenchmarkKind::Update;
        record.metrics.avg_download_bps = 1024.0;
        record.metrics.downloaded_bytes = 2048;
        let labels: Vec<&str> = headline_stats(&record)
            .iter()
            .map(|(label, _, _)| *label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "Elapsed",
                "Average download speed",
                "Downloaded",
                "Files checked"
            ]
        );
    }
}
