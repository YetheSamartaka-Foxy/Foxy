//! Side-by-side comparison of two benchmarks: header cards, a metric diff
//! table with proportion bars, stage bars and overlaid charts.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Ui, Vec2};

use super::charts::{bar_chart, line_chart};
use super::detail::{fmt_metric, fmt_sol, metric_rows};
use super::series::{charts_for, stage_rows};
use super::widgets::{panel, text_scale};
use crate::core::benchmarks::BenchmarkRecord;
use crate::ui::app::Foxy;
use crate::ui::palette;

/// Signed change from `a` to `b` as a percentage of `a`; `None` when `a` is
/// zero (no meaningful baseline).
pub fn percent_change(a: f64, b: f64) -> Option<f64> {
    (a.abs() > f64::EPSILON).then(|| (b - a) / a * 100.0)
}

/// Two stacked bars showing `a` and `b` relative to the larger of them.
fn proportion_bars(ui: &mut Ui, a: f64, b: f64, colors: [Color32; 2], track: Color32) {
    const WIDTH: f32 = 110.0;
    const BAR: f32 = 5.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(WIDTH, BAR * 2.0 + 3.0), Sense::hover());
    let max = a.max(b).max(f64::EPSILON);
    let painter = ui.painter();
    for (index, (value, color)) in [(a, colors[0]), (b, colors[1])].into_iter().enumerate() {
        let top = rect.min.y + index as f32 * (BAR + 3.0);
        let track_rect = Rect::from_min_size(Pos2::new(rect.min.x, top), Vec2::new(WIDTH, BAR));
        painter.rect_filled(track_rect, 2.0, track);
        let width = ((value / max) as f32 * WIDTH).max(if value > 0.0 { 2.0 } else { 0.0 });
        if width > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(track_rect.min, Vec2::new(width, BAR)),
                2.0,
                color,
            );
        }
    }
}

impl Foxy {
    fn render_compare_header_card(
        &self,
        ui: &mut Ui,
        letter: &str,
        color: Color32,
        record: &BenchmarkRecord,
    ) {
        let scale = text_scale(ui);
        let dim = self.color_text_dim();
        panel(ui, self.color_main_bg(), |ui| {
            ui.horizontal(|ui| {
                self.benchmark_letter_tag(ui, letter, color);
                ui.add(
                    egui::Label::new(RichText::new(&record.name).strong().color(color)).truncate(),
                );
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                self.benchmark_kind_badge(ui, record.kind);
                self.benchmark_outcome_dot(ui, &record.outcome);
                ui.label(self.outcome_text(&record.outcome).size(scale.small));
                ui.label(
                    RichText::new(&record.started_at_local)
                        .size(scale.small)
                        .color(dim),
                );
            });
            ui.add_space(4.0);
            egui::Grid::new(("benchmark_compare_card", letter))
                .num_columns(2)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    for (label, value) in [
                        (self.t("Repository"), record.repository.name.clone()),
                        (
                            self.t("Build"),
                            format!("{} {}", record.build.version, record.build.commit),
                        ),
                        (
                            self.t("Repository storage"),
                            record.machine.repository_storage_class.clone(),
                        ),
                        (
                            self.t("Hash profile"),
                            record.machine.hash_io_profile.clone(),
                        ),
                        (
                            self.t("CPU"),
                            format!("{} x{}", record.machine.cpu, record.machine.cpu_cores),
                        ),
                    ] {
                        ui.label(RichText::new(label).size(scale.small).color(dim));
                        ui.add(egui::Label::new(RichText::new(value).size(scale.small)).truncate());
                        ui.end_row();
                    }
                });
        });
    }

    /// Speed-of-light ratio per operation of both records, so a build can be
    /// judged by how close it ran to the light rather than by raw seconds
    /// that a different repository or machine would skew.
    fn render_compare_sol(
        &self,
        ui: &mut Ui,
        a: &BenchmarkRecord,
        b: &BenchmarkRecord,
        colors: [Color32; 2],
    ) {
        let sol_a = a.sol_summaries();
        let sol_b = b.sol_summaries();
        if sol_a.is_empty() && sol_b.is_empty() {
            return;
        }
        let scale = text_scale(ui);
        let dim = self.color_text_dim();
        // (operation, sub-part) pairs of both records in first-seen order;
        // the main ratio has no sub-part name.
        let mut ops: Vec<(String, Option<&'static str>)> = Vec::new();
        for summary in sol_a.iter().chain(&sol_b) {
            let parts = std::iter::once(None).chain(summary.sub_parts.iter().map(|p| Some(p.name)));
            for part in parts {
                if !ops
                    .iter()
                    .any(|(op, known)| *op == summary.op && *known == part)
                {
                    ops.push((summary.op.clone(), part));
                }
            }
        }
        panel(ui, self.color_main_bg(), |ui| {
            self.benchmark_section_title(ui, self.t("Speed of light"));
            egui::Grid::new("benchmark_compare_sol")
                .num_columns(5)
                .striped(true)
                .spacing([18.0, 5.0])
                .min_col_width(60.0)
                .show(ui, |ui| {
                    let header = |ui: &mut Ui, text: String, color: Color32| {
                        ui.label(RichText::new(text).size(scale.small).strong().color(color));
                    };
                    header(ui, self.t("Operation"), dim);
                    header(ui, format!("A  {}", a.build.version), colors[0]);
                    header(ui, format!("B  {}", b.build.version), colors[1]);
                    header(ui, self.t("Change"), dim);
                    header(ui, self.t("Kind"), dim);
                    ui.end_row();
                    for (op, part) in &ops {
                        let find = |summaries: &[crate::core::benchmarks::SolOpSummary]| {
                            let summary = summaries.iter().find(|summary| summary.op == *op)?;
                            match part {
                                None => Some((
                                    summary.display_sol(),
                                    if summary.display_sol().is_some() {
                                        format!(
                                            "{} \u{00B7} {}",
                                            self.t(summary.metric_kind().label()),
                                            self.t(summary.light_src.label())
                                        )
                                    } else {
                                        format!(
                                            "{} \u{00B7} raw {}",
                                            self.t(summary.unavailable_reason()),
                                            summary.sol_raw.map_or_else(
                                                || "n/a".to_owned(),
                                                |raw| format!("{raw:.2}")
                                            )
                                        )
                                    },
                                )),
                                Some(name) => {
                                    summary.sub_parts.iter().find(|sub| sub.name == *name).map(
                                        |sub| {
                                            (
                                                sub.display_sol(),
                                                format!(
                                                    "{} \u{00B7} {}",
                                                    self.t(sub.metric_kind().label()),
                                                    self.t(sub.light_src.label())
                                                ),
                                            )
                                        },
                                    )
                                }
                            }
                        };
                        let (ratio_a, src_a) = find(&sol_a).unwrap_or((None, String::new()));
                        let (ratio_b, src_b) = find(&sol_b).unwrap_or((None, String::new()));
                        match part {
                            None => ui.label(RichText::new(op).strong()),
                            Some(name) => ui.label(
                                RichText::new(format!("{op} / {}", self.t(name)))
                                    .size(scale.small)
                                    .color(dim),
                            ),
                        };
                        self.sol_cell(ui, ratio_a, &src_a);
                        self.sol_cell(ui, ratio_b, &src_b);
                        match (ratio_a, ratio_b) {
                            (Some(ra), Some(rb)) => {
                                let points = (rb - ra) * 100.0;
                                if points.abs() < 0.5 {
                                    ui.label(RichText::new("0 pp").color(dim));
                                } else {
                                    let color = if points > 0.0 {
                                        self.color_success()
                                    } else {
                                        self.color_error()
                                    };
                                    ui.label(
                                        RichText::new(format!("{points:+.0} pp"))
                                            .strong()
                                            .color(color),
                                    )
                                    .on_hover_text(format!(
                                        "{} -> {}",
                                        fmt_sol(ra),
                                        fmt_sol(rb)
                                    ));
                                }
                            }
                            _ => {
                                ui.label(RichText::new("n/a").color(dim));
                            }
                        }
                        // The kind is text so a mismatch between the two
                        // records reads without a tooltip.
                        let kind_text = if src_a == src_b || src_b.is_empty() {
                            src_a.clone()
                        } else if src_a.is_empty() {
                            src_b.clone()
                        } else {
                            format!("A {src_a}; B {src_b}")
                        };
                        ui.label(RichText::new(kind_text).size(scale.small).color(dim));
                        ui.end_row();
                    }
                });
        });
        ui.add_space(8.0);
    }

    /// "Oldest as A" toggle and an A/B swap, shared by the list toolbar and
    /// the comparison header.
    pub(super) fn render_compare_order_controls(&mut self, ui: &mut Ui) {
        let can_swap = self.benchmarks_view.selected.len() == 2;
        if ui
            .add_enabled(can_swap, egui::Button::new(self.t("Swap A and B")))
            .on_hover_text(self.t("Exchange which benchmark is A and which is B"))
            .clicked()
        {
            self.benchmarks_view.swap_compare_order();
        }
        let label = self.t("Oldest as A");
        Self::ui_state_checkbox(ui, &mut self.benchmarks_view.compare_by_date, label)
            .on_hover_text(self.t(
                "When on, the older benchmark is always A. When off, A is the one selected first.",
            ));
    }

    pub(super) fn render_benchmark_compare(&mut self, ui: &mut Ui) {
        let order = self.benchmarks_view.compare_order();
        let [Some(a), Some(b)] = [0, 1].map(|index| {
            order
                .get(index)
                .and_then(|id| self.benchmarks_view.record(id))
                .cloned()
        }) else {
            self.benchmarks_view.compare_open = false;
            return;
        };
        let theme = self.benchmark_chart_theme();
        let dim = self.color_text_dim();
        let scale = text_scale(ui);
        let panel_bg = self.color_main_bg();
        let colors = [palette::BENCHMARK_SERIES[0], palette::BENCHMARK_SERIES[1]];
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(self.t("Compare benchmarks"))
                    .size(scale.large)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.t("Back to list")).clicked() {
                    self.benchmarks_view.compare_open = false;
                }
                self.render_compare_order_controls(ui);
            });
        });
        ui.add_space(6.0);
        ui.columns(2, |columns| {
            self.render_compare_header_card(&mut columns[0], "A", colors[0], &a);
            self.render_compare_header_card(&mut columns[1], "B", colors[1], &b);
        });
        ui.add_space(8.0);

        panel(ui, panel_bg, |ui| {
            self.benchmark_section_title(ui, self.t("Metrics"));
            egui::Grid::new("benchmark_compare_metrics")
                .num_columns(5)
                .striped(true)
                .spacing([18.0, 5.0])
                .min_col_width(60.0)
                .show(ui, |ui| {
                    let header = |ui: &mut Ui, text: String, color: Color32| {
                        ui.label(RichText::new(text).size(scale.small).strong().color(color));
                    };
                    header(ui, self.t("Metric"), dim);
                    header(ui, "A".to_owned(), colors[0]);
                    header(ui, "B".to_owned(), colors[1]);
                    header(ui, self.t("Change"), dim);
                    ui.label("");
                    ui.end_row();
                    for (ra, rb) in metric_rows(&a).into_iter().zip(metric_rows(&b)) {
                        if ra.value <= 0.0 && rb.value <= 0.0 {
                            continue;
                        }
                        ui.label(self.t(ra.label));
                        ui.label(fmt_metric(ra.unit, ra.value));
                        ui.label(fmt_metric(rb.unit, rb.value));
                        match percent_change(ra.value, rb.value) {
                            Some(change) if change.abs() >= 0.05 => {
                                let better = (change < 0.0) == ra.lower_is_better;
                                let color = if better {
                                    self.color_success()
                                } else {
                                    self.color_error()
                                };
                                ui.label(
                                    RichText::new(format!("{change:+.1}%"))
                                        .strong()
                                        .color(color),
                                );
                            }
                            Some(_) => {
                                ui.label(RichText::new("0.0%").color(dim));
                            }
                            None => {
                                ui.label(RichText::new("n/a").color(dim));
                            }
                        }
                        proportion_bars(ui, ra.value, rb.value, colors, theme.plot);
                        ui.end_row();
                    }
                });
        });
        ui.add_space(8.0);
        self.render_compare_sol(ui, &a, &b, colors);
        if !a.stages.is_empty() || !b.stages.is_empty() {
            bar_chart(
                ui,
                &self.t("Stage durations"),
                &stage_rows(&[&a, &b]),
                &[
                    (format!("A: {}", a.name), colors[0]),
                    (format!("B: {}", b.name), colors[1]),
                ],
                theme,
            );
            ui.add_space(6.0);
        }
        let charts_a = charts_for(&a, "A: ", &palette::BENCHMARK_SERIES);
        let charts_b = charts_for(&b, "B: ", &palette::BENCHMARK_SERIES_B);
        let mut keys: Vec<&str> = charts_a.iter().map(|def| def.key).collect();
        for def in &charts_b {
            if !keys.contains(&def.key) {
                keys.push(def.key);
            }
        }
        let mut merged = Vec::new();
        for key in keys {
            let def_a = charts_a.iter().find(|def| def.key == key);
            let def_b = charts_b.iter().find(|def| def.key == key);
            let Some(reference) = def_a.or(def_b) else {
                continue;
            };
            let mut series = Vec::new();
            if let Some(def) = def_a {
                series.extend(def.series.iter().cloned());
            }
            if let Some(def) = def_b {
                series.extend(def.series.iter().cloned());
            }
            merged.push((reference.title, reference.unit, series));
        }
        ui.columns(2, |columns| {
            for (index, (title, unit, series)) in merged.iter().enumerate() {
                let column = &mut columns[index % 2];
                line_chart(column, 200.0, &self.t(title), *unit, series, theme);
                column.add_space(6.0);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::percent_change;

    #[test]
    fn percent_change_handles_zero_baseline() {
        assert_eq!(percent_change(0.0, 5.0), None);
        assert_eq!(percent_change(10.0, 5.0), Some(-50.0));
        assert_eq!(percent_change(10.0, 15.0), Some(50.0));
    }
}
