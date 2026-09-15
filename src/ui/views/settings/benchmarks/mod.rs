//! Settings tab listing saved benchmarks: search, filters, sort, per-row
//! actions, expandable detail with charts, and a two-way comparison.

mod charts;
mod compare;
mod detail;
mod export;
mod series;
mod widgets;

use eframe::egui::{
    self, Button, CornerRadius, Layout, Margin, Rect, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui, Vec2,
};
use log::info;

use crate::core::benchmarks::{BenchmarkKind, BenchmarkOutcome, BenchmarkRecord};
use crate::ui::app::Foxy;
use crate::ui::app::benchmarks::{BenchmarkOutcomeFilter, BenchmarkSort};
use crate::ui::i18n::{fmt_bytes, tr, tr_fmt};
use crate::ui::palette;
use detail::fmt_sol;
use widgets::{CARD_RADIUS, text_scale};

const ROW_HEADER_HEIGHT: f32 = 28.0;
const ICON_BUTTON_SIZE: f32 = 26.0;
const SELECTION_CHIP_WIDTH: f32 = 220.0;

enum RowAction {
    ToggleExpand,
    ToggleFavourite,
    ToggleHidden,
    ToggleSelect,
    Export,
    Remove,
    ConfirmRemove,
    CancelRemove,
}

impl Foxy {
    pub(super) fn render_benchmarks_settings(&mut self, ui: &mut Ui, card_height: f32) {
        self.ensure_benchmarks_loaded();
        self.render_benchmarks_enabled_toggle(ui);
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        if self.benchmarks_view.compare_open {
            ScrollArea::vertical()
                .id_salt("benchmarks_compare_scroll")
                .max_height(card_height)
                .show(ui, |ui| self.render_benchmark_compare(ui));
            return;
        }
        self.render_benchmarks_toolbar(ui);
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        let ids = self.benchmarks_view.visible_ids();
        if ids.is_empty() {
            self.render_benchmarks_empty_state(ui, card_height);
            return;
        }
        let mut action: Option<(String, RowAction)> = None;
        ScrollArea::vertical()
            .id_salt("benchmarks_list_scroll")
            .max_height(card_height)
            .show(ui, |ui| {
                for id in &ids {
                    if let Some(row_action) = self.render_benchmark_row(ui, id) {
                        action = Some((id.clone(), row_action));
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some((id, row_action)) = action {
            self.apply_benchmark_row_action(&id, row_action);
        }
    }

    fn render_benchmarks_enabled_toggle(&mut self, ui: &mut Ui) {
        let scale = text_scale(ui);
        let mut enabled = self.settings_view_state.benchmarks_enabled;
        ui.horizontal_wrapped(|ui| {
            let checkbox = Self::ui_state_checkbox(ui, &mut enabled, tr("Enable benchmarks"))
                .on_hover_text(tr("After a recheck, update or redownload you started, offer to save the run as a benchmark with its metrics, charts and log slice. Saved benchmarks are listed in the Benchmarks tab. Also turns on extended diagnostics logging while enabled."));
            if checkbox.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if checkbox.changed() {
                self.apply_benchmarks_enabled(enabled);
                self.save_settings();
                self.show_success_toast(self.t("Settings saved"));
            }
            ui.label(
                RichText::new(tr(
                    "Offers to save a benchmark after each recheck, update or redownload you start.",
                ))
                .size(scale.small)
                .color(self.color_text_dim()),
            );
        });
    }

    fn render_benchmarks_empty_state(&mut self, ui: &mut Ui, card_height: f32) {
        let scale = text_scale(ui);
        let no_records = self.benchmarks_view.records.is_empty();
        let enabled = self.settings_view_state.benchmarks_enabled;
        ui.add_space((card_height * 0.22).max(24.0));
        ui.vertical_centered(|ui| {
            let (title, hint) = if !no_records {
                (
                    self.t("No benchmarks match the current filters."),
                    String::new(),
                )
            } else if enabled {
                (
                    self.t("No benchmarks saved yet."),
                    self.t("Run a recheck or update and choose Save benchmark when it finishes."),
                )
            } else {
                (
                    self.t("No benchmarks saved yet."),
                    self.t("Enable Benchmarks to be offered a save after rechecks and updates."),
                )
            };
            ui.label(RichText::new(title).size(scale.large).strong());
            if !hint.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(hint).color(self.color_text_dim()));
            }
        });
    }

    fn render_benchmarks_toolbar(&mut self, ui: &mut Ui) {
        let scale = text_scale(ui);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                TextEdit::singleline(&mut self.benchmarks_view.search)
                    .hint_text(tr("Search benchmarks"))
                    .desired_width(220.0),
            );
            egui::ComboBox::from_id_salt("benchmarks_sort")
                .selected_text(tr(self.benchmarks_view.sort.label()))
                .show_ui(ui, |ui| {
                    for sort in BenchmarkSort::ALL {
                        ui.selectable_value(&mut self.benchmarks_view.sort, sort, tr(sort.label()));
                    }
                });
            let kind_text = self
                .benchmarks_view
                .kind_filter
                .map_or_else(|| tr("All actions"), |kind| tr(kind.label()));
            egui::ComboBox::from_id_salt("benchmarks_kind")
                .selected_text(kind_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.benchmarks_view.kind_filter,
                        None,
                        tr("All actions"),
                    );
                    for kind in BenchmarkKind::ALL {
                        ui.selectable_value(
                            &mut self.benchmarks_view.kind_filter,
                            Some(kind),
                            tr(kind.label()),
                        );
                    }
                });
            let repositories = self.benchmarks_view.repositories();
            let repo_text = self
                .benchmarks_view
                .repository_filter
                .as_ref()
                .and_then(|url| repositories.iter().find(|(known, _)| known == url))
                .map_or_else(|| tr("All repositories"), |(_, name)| name.clone());
            egui::ComboBox::from_id_salt("benchmarks_repo")
                .selected_text(repo_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.benchmarks_view.repository_filter,
                        None,
                        tr("All repositories"),
                    );
                    for (url, name) in &repositories {
                        ui.selectable_value(
                            &mut self.benchmarks_view.repository_filter,
                            Some(url.clone()),
                            name,
                        );
                    }
                });
            let outcome_text = match self.benchmarks_view.outcome_filter {
                BenchmarkOutcomeFilter::All => tr("All outcomes"),
                BenchmarkOutcomeFilter::Success => tr("Success"),
                BenchmarkOutcomeFilter::Failed => tr("Failed"),
            };
            egui::ComboBox::from_id_salt("benchmarks_outcome")
                .selected_text(outcome_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.benchmarks_view.outcome_filter,
                        BenchmarkOutcomeFilter::All,
                        tr("All outcomes"),
                    );
                    ui.selectable_value(
                        &mut self.benchmarks_view.outcome_filter,
                        BenchmarkOutcomeFilter::Success,
                        tr("Success"),
                    );
                    ui.selectable_value(
                        &mut self.benchmarks_view.outcome_filter,
                        BenchmarkOutcomeFilter::Failed,
                        tr("Failed"),
                    );
                });
            Self::ui_state_checkbox(
                ui,
                &mut self.benchmarks_view.favourites_only,
                tr("Favourites only"),
            );
            Self::ui_state_checkbox(ui, &mut self.benchmarks_view.show_hidden, tr("Show hidden"));
        });
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let selected: Vec<(String, String)> = self
                .benchmarks_view
                .selected
                .iter()
                .filter_map(|id| {
                    self.benchmarks_view
                        .record(id)
                        .map(|record| (id.clone(), record.name.clone()))
                })
                .collect();
            let compare_text = tr_fmt(
                "Compare selected ({count}/2)",
                &[("count", selected.len().to_string())],
            );
            let compare = if selected.len() == 2 {
                self.add_sized_primary_button(
                    ui,
                    Vec2::new(0.0, 24.0),
                    Button::new(RichText::new(compare_text).strong()),
                    true,
                )
            } else {
                ui.add_enabled(
                    false,
                    Button::new(compare_text).min_size(Vec2::new(0.0, 24.0)),
                )
            };
            if compare.clicked() {
                self.benchmarks_view.compare_open = true;
            }
            for (index, (id, name)) in selected.iter().enumerate() {
                let letter = if index == 0 { "A" } else { "B" };
                self.benchmark_letter_tag(ui, letter, palette::BENCHMARK_SERIES[index]);
                ui.scope(|ui| {
                    ui.set_max_width(SELECTION_CHIP_WIDTH.min(ui.available_width()));
                    ui.add(
                        egui::Label::new(RichText::new(name).size(scale.small))
                            .truncate()
                            .selectable(false),
                    )
                    .on_hover_text(name);
                });
                if ui
                    .add(Button::new(RichText::new("\u{00D7}").size(scale.body)).frame(false))
                    .on_hover_text(tr("Clear selection"))
                    .clicked()
                {
                    self.benchmarks_view.toggle_selected(id);
                }
            }
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("Open benchmarks folder")).clicked() {
                    let dir = crate::core::benchmarks::store::benchmarks_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    if let Err(err) = open_directory(&dir) {
                        log::warn!("Failed to open benchmarks folder: {err}");
                        self.show_error_toast(tr("Failed to open benchmarks folder."));
                    }
                }
                if ui.button(tr("Refresh")).clicked() {
                    self.reload_benchmarks();
                }
                let count = self.benchmarks_view.records.len();
                let hidden = self
                    .benchmarks_view
                    .records
                    .iter()
                    .filter(|record| record.hidden)
                    .count();
                let mut summary = tr_fmt("{count} saved", &[("count", count.to_string())]);
                if hidden > 0 {
                    summary.push_str(&format!(
                        "  \u{00B7}  {}",
                        tr_fmt("{count} hidden", &[("count", hidden.to_string())])
                    ));
                }
                ui.label(
                    RichText::new(summary)
                        .size(scale.small)
                        .color(self.color_text_dim()),
                );
            });
        });
    }

    fn render_benchmark_row(&mut self, ui: &mut Ui, id: &str) -> Option<RowAction> {
        let record = self.benchmarks_view.record(id).cloned()?;
        let expanded = self.benchmarks_view.expanded.contains(id);
        let selected = self
            .benchmarks_view
            .selected
            .iter()
            .any(|known| known == id);
        let pending_remove = self.benchmarks_view.pending_remove.as_deref() == Some(id);
        let scale = text_scale(ui);
        let dim = self.color_text_dim();
        let mut action = None;
        let mut frame = egui::Frame::NONE
            .corner_radius(CornerRadius::same(CARD_RADIUS))
            .inner_margin(Margin::symmetric(12, 8))
            .begin(ui);
        let header_focused;
        {
            let ui = &mut frame.content_ui;
            ui.set_width(ui.available_width());
            // Registered before the header widgets so the buttons drawn on
            // top of it keep priority for their own clicks.
            let header_rect = Rect::from_min_size(
                ui.cursor().min,
                Vec2::new(ui.available_width(), ROW_HEADER_HEIGHT),
            );
            let header = ui.interact(
                header_rect,
                ui.make_persistent_id(("benchmark_row_header", id)),
                Sense::click(),
            );
            header_focused = header.has_focus();
            if header.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if header.clicked() {
                action = Some(RowAction::ToggleExpand);
            }
            ui.allocate_ui_with_layout(
                header_rect.size(),
                Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if let Some(button_action) =
                        self.render_benchmark_row_buttons(ui, &record, selected, pending_remove)
                    {
                        action = Some(button_action);
                    }
                    ui.with_layout(Layout::left_to_right(egui::Align::Center), |ui| {
                        let arrow = if expanded { "\u{25BC}" } else { "\u{25B6}" };
                        ui.label(RichText::new(arrow).size(scale.small).color(dim));
                        self.benchmark_outcome_dot(ui, &record.outcome);
                        if record.favourite {
                            ui.label(RichText::new("\u{2605}").color(self.color_warn()));
                        }
                        ui.add(
                            egui::Label::new(RichText::new(&record.name).strong())
                                .truncate()
                                .selectable(false),
                        )
                        .on_hover_text(self.benchmark_row_hover(&record));
                    });
                },
            );
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                self.benchmark_kind_badge(ui, record.kind);
                let mut meta = vec![
                    record.repository.name.clone(),
                    record.started_at_local.clone(),
                ];
                if !record.machine.repository_storage_class.is_empty() {
                    meta.push(record.machine.repository_storage_class.clone());
                }
                if !record.build.version.is_empty() {
                    meta.push(format!("{} {}", record.build.version, record.build.commit));
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(meta.join("  \u{00B7}  "))
                            .size(scale.small)
                            .color(dim),
                    )
                    .truncate()
                    .selectable(false),
                );
                if record.outcome != BenchmarkOutcome::Success {
                    ui.label(self.outcome_text(&record.outcome).size(scale.small));
                }
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if expanded {
                        return;
                    }
                    if record.hidden {
                        self.benchmark_stat_chip(ui, self.t("Hidden"), "\u{1F6AB}");
                    }
                    if let Some(sol) = record.headline_sol().and_then(|summary| summary.sol) {
                        self.benchmark_stat_chip(ui, self.t("SoL"), fmt_sol(sol));
                    }
                    if record.kind.transfers_files() && record.metrics.downloaded_bytes > 0 {
                        self.benchmark_stat_chip(
                            ui,
                            self.t("downloaded"),
                            fmt_bytes(record.metrics.downloaded_bytes),
                        );
                    }
                    if let Some((rate, unit)) = record.headline_rate() {
                        let text = if unit == "B/s" {
                            format!("{}/s", fmt_bytes(rate as u64))
                        } else {
                            format!("{rate:.0} {unit}")
                        };
                        self.benchmark_stat_chip(ui, self.t("avg"), text);
                    }
                    if record.metrics.hash_files_total > 0 && !record.kind.transfers_files() {
                        self.benchmark_stat_chip(
                            ui,
                            self.t("files"),
                            record.metrics.hash_files_total.to_string(),
                        );
                    }
                    self.benchmark_stat_chip(
                        ui,
                        self.t("elapsed"),
                        format!("{:.1} s", record.elapsed_secs()),
                    );
                });
            });
            let notes = record.notes.trim();
            if !notes.is_empty() && !expanded {
                ui.add(
                    egui::Label::new(
                        RichText::new(notes.lines().next().unwrap_or_default())
                            .size(scale.small)
                            .italics()
                            .color(dim),
                    )
                    .truncate()
                    .selectable(false),
                );
            }
            if expanded {
                self.render_benchmark_detail(ui, id);
            }
        }
        let rect = frame.content_ui.min_rect() + frame.frame.inner_margin;
        let hovered = !expanded && ui.rect_contains_pointer(rect);
        let fill = if selected {
            self.color_server_selected_bg()
        } else if hovered {
            self.color_widget_bg()
        } else {
            self.color_main_bg()
        };
        let stroke = if selected || header_focused {
            Stroke::new(1.25, self.color_primary_accent())
        } else {
            Stroke::NONE
        };
        frame.frame = frame.frame.fill(fill).stroke(stroke);
        frame.end(ui);
        action
    }

    fn render_benchmark_row_buttons(
        &self,
        ui: &mut Ui,
        record: &BenchmarkRecord,
        selected: bool,
        pending_remove: bool,
    ) -> Option<RowAction> {
        let mut action = None;
        let icon_button =
            |icon: RichText| Button::new(icon).min_size(Vec2::splat(ICON_BUTTON_SIZE));
        if pending_remove {
            if ui
                .add(
                    Button::new(self.t("Confirm remove"))
                        .fill(self.color_action_destructive())
                        .min_size(Vec2::new(0.0, ICON_BUTTON_SIZE)),
                )
                .clicked()
            {
                action = Some(RowAction::ConfirmRemove);
            }
            if ui
                .add(Button::new(self.t("Cancel")).min_size(Vec2::new(0.0, ICON_BUTTON_SIZE)))
                .clicked()
            {
                action = Some(RowAction::CancelRemove);
            }
            return action;
        }
        if ui
            .add(icon_button(RichText::new("\u{1F5D1}")))
            .on_hover_text(self.t("Remove benchmark"))
            .clicked()
        {
            action = Some(RowAction::Remove);
        }
        if ui
            .add_enabled(
                self.benchmarks_view.export.is_none(),
                Button::new(format!("\u{1F4E6} {}", self.t("Export")))
                    .min_size(Vec2::new(0.0, ICON_BUTTON_SIZE)),
            )
            .on_hover_text(self.t("Export benchmark to ZIP"))
            .clicked()
        {
            action = Some(RowAction::Export);
        }
        let (hide_icon, hide_hover) = if record.hidden {
            ("\u{1F441}", self.t("Unhide benchmark"))
        } else {
            ("\u{1F6AB}", self.t("Hide benchmark"))
        };
        if ui
            .add(icon_button(RichText::new(hide_icon)))
            .on_hover_text(hide_hover)
            .clicked()
        {
            action = Some(RowAction::ToggleHidden);
        }
        let star = if record.favourite {
            RichText::new("\u{2605}").color(self.color_warn())
        } else {
            RichText::new("\u{2606}")
        };
        if ui
            .add(icon_button(star))
            .on_hover_text(self.t("Favourite"))
            .clicked()
        {
            action = Some(RowAction::ToggleFavourite);
        }
        ui.add_space(6.0);
        let mut select = selected;
        if Self::ui_state_checkbox(ui, &mut select, self.t("Compare"))
            .on_hover_text(self.t("Select two benchmarks to compare them."))
            .changed()
        {
            action = Some(RowAction::ToggleSelect);
        }
        action
    }

    fn benchmark_row_hover(&self, record: &BenchmarkRecord) -> String {
        let mut text = format!("{}\n{}", record.repository.url, record.id);
        if !record.notes.trim().is_empty() {
            text.push('\n');
            text.push_str(record.notes.trim());
        }
        text
    }

    fn apply_benchmark_row_action(&mut self, id: &str, action: RowAction) {
        match action {
            RowAction::ToggleExpand => {
                if !self.benchmarks_view.expanded.remove(id) {
                    self.benchmarks_view.expanded.insert(id.to_owned());
                }
            }
            RowAction::ToggleFavourite => {
                if let Some(record) = self.benchmarks_view.record_mut(id) {
                    record.favourite = !record.favourite;
                }
                self.persist_benchmark_record(id);
            }
            RowAction::ToggleHidden => {
                if let Some(record) = self.benchmarks_view.record_mut(id) {
                    record.hidden = !record.hidden;
                }
                self.persist_benchmark_record(id);
            }
            RowAction::ToggleSelect => self.benchmarks_view.toggle_selected(id),
            RowAction::Export => self.start_benchmark_export(id),
            RowAction::Remove => self.benchmarks_view.pending_remove = Some(id.to_owned()),
            RowAction::CancelRemove => self.benchmarks_view.pending_remove = None,
            RowAction::ConfirmRemove => {
                info!("Removing benchmark {id}");
                self.remove_benchmark(id);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn open_directory(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("explorer")
        .arg(path.as_os_str())
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(not(target_os = "windows"))]
fn open_directory(path: &std::path::Path) -> Result<(), String> {
    crate::core::utils::platform::open_with_default_app(path)
}
