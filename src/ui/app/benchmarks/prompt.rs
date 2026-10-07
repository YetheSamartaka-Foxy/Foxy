use eframe::egui::{self, RichText, TextEdit};
use log::info;

use crate::core::benchmarks::BenchmarkOutcome;
use crate::ui::app::Foxy;
use crate::ui::i18n::fmt_bytes;

impl Foxy {
    /// "Save this run as a benchmark?" modal shown after a benchmarked action.
    pub(crate) fn render_benchmark_save_prompt(&mut self, ctx: &egui::Context) {
        let Some(draft) = self.benchmark_prompt.as_ref() else {
            return;
        };
        let record = draft.record.clone();
        let mut save = false;
        let mut discard = false;
        let mut name = record.name.clone();
        let mut notes = record.notes.clone();
        egui::Window::new(self.t("Save benchmark"))
            .frame(self.modal_window_chrome(ctx))
            .title_frame(self.modal_window_chrome(ctx))
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(self.t(
                    "The action just finished. Save it as a benchmark with its log and metrics?",
                ));
                ui.add_space(8.0);
                egui::Grid::new("benchmark_prompt_summary")
                    .num_columns(2)
                    .spacing([16.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new(self.t("Action")).color(self.color_text_dim()));
                        ui.label(self.t(record.kind.label()));
                        ui.end_row();
                        ui.label(RichText::new(self.t("Repository")).color(self.color_text_dim()));
                        ui.label(&record.repository.name);
                        ui.end_row();
                        ui.label(RichText::new(self.t("Elapsed")).color(self.color_text_dim()));
                        ui.label(format!("{:.2} s", record.elapsed_secs()));
                        ui.end_row();
                        ui.label(RichText::new(self.t("Outcome")).color(self.color_text_dim()));
                        match &record.outcome {
                            BenchmarkOutcome::Success => {
                                ui.label(
                                    RichText::new(self.t("Success")).color(self.color_success()),
                                );
                            }
                            BenchmarkOutcome::Failed { message } => {
                                ui.label(
                                    RichText::new(format!("{}: {}", self.t("Failed"), message))
                                        .color(self.color_text_error()),
                                );
                            }
                            BenchmarkOutcome::Cancelled => {
                                ui.label(self.t("Operation cancelled"));
                            }
                        }
                        ui.end_row();
                        if record.kind.transfers_files() {
                            ui.label(
                                RichText::new(self.t("Downloaded")).color(self.color_text_dim()),
                            );
                            ui.label(format!(
                                "{} @ {}/s",
                                fmt_bytes(record.metrics.downloaded_bytes),
                                fmt_bytes(record.metrics.avg_download_bps as u64)
                            ));
                            ui.end_row();
                        } else if record.metrics.hash_files_total > 0 {
                            ui.label(
                                RichText::new(self.t("Files checked")).color(self.color_text_dim()),
                            );
                            ui.label(record.metrics.hash_files_total.to_string());
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.label(self.t("Name"));
                ui.add(TextEdit::singleline(&mut name).desired_width(f32::INFINITY));
                ui.label(self.t("Notes"));
                ui.add(
                    TextEdit::multiline(&mut notes)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let save_button = ui.add_enabled(
                        !name.trim().is_empty(),
                        egui::Button::new(self.t("Save benchmark")),
                    );
                    if save_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if save_button.clicked() {
                        save = true;
                    }
                    let discard_button = ui.button(self.t("Discard"));
                    if discard_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if discard_button.clicked() {
                        discard = true;
                    }
                });
            });
        if let Some(draft) = self.benchmark_prompt.as_mut() {
            draft.record.name = name;
            draft.record.notes = notes;
        }
        if save && let Some(mut draft) = self.benchmark_prompt.take() {
            draft.record.name = draft.record.name.trim().to_owned();
            info!("Benchmark save requested: {}", draft.record.id);
            self.spawn_benchmark_save(draft);
        } else if discard {
            info!("Benchmark discarded by user");
            self.benchmark_prompt = None;
        }
    }
}
