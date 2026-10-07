//! Small presentational pieces shared by the benchmark list, detail and
//! comparison: kind badges, outcome dots, stat tiles and section cards.

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, TextStyle, Ui};

use crate::core::benchmarks::{BenchmarkKind, BenchmarkOutcome};
use crate::ui::app::Foxy;

pub const CARD_RADIUS: u8 = 6;

/// Font sizes derived from the body text style so the tab follows the app's
/// text scale instead of hardcoding pixel sizes.
pub struct TextScale {
    pub body: f32,
    pub small: f32,
    pub large: f32,
}

pub fn text_scale(ui: &Ui) -> TextScale {
    let body = TextStyle::Body.resolve(ui.style()).size;
    TextScale {
        body,
        small: (body * 0.85).max(10.0),
        large: body * 1.45,
    }
}

/// A rounded panel used for every grouped block of the tab.
pub fn panel<R>(ui: &mut Ui, fill: Color32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

impl Foxy {
    pub(super) fn benchmark_section_title(&self, ui: &mut Ui, text: impl Into<String>) {
        let scale = text_scale(ui);
        ui.label(
            RichText::new(text.into().to_uppercase())
                .size(scale.small)
                .strong()
                .color(self.color_text_dim()),
        );
        ui.add_space(2.0);
    }

    /// Pill naming the action of a benchmark.
    pub(crate) fn benchmark_kind_badge(&self, ui: &mut Ui, kind: BenchmarkKind) {
        let scale = text_scale(ui);
        let fill = if kind.transfers_files() {
            self.color_primary_accent()
        } else {
            self.color_server_selected_bg()
        };
        egui::Frame::NONE
            .fill(fill)
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(self.t(kind.label()))
                        .size(scale.small)
                        .strong()
                        .color(self.color_text_normal()),
                );
            });
    }

    /// Coloured dot summarising the outcome, with the message on hover.
    pub(crate) fn benchmark_outcome_dot(&self, ui: &mut Ui, outcome: &BenchmarkOutcome) {
        let (color, hover) = match outcome {
            BenchmarkOutcome::Success => (self.color_success(), self.t("Success")),
            BenchmarkOutcome::Failed { message } => (
                self.color_error(),
                format!("{}: {}", self.t("Failed"), message),
            ),
            BenchmarkOutcome::Cancelled => (self.color_text_dim(), self.t("Operation cancelled")),
        };
        let (rect, response) =
            ui.allocate_exact_size(egui::Vec2::splat(12.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.5, color);
        response.on_hover_text(hover);
    }

    /// Big number with a small caption, for the headline figures of a run.
    pub(super) fn benchmark_stat_tile(
        &self,
        ui: &mut Ui,
        label: impl Into<String>,
        value: impl Into<String>,
        accent: Option<Color32>,
    ) {
        let scale = text_scale(ui);
        egui::Frame::NONE
            .fill(self.color_card_bg())
            .corner_radius(CornerRadius::same(CARD_RADIUS))
            .inner_margin(Margin::symmetric(14, 8))
            .show(ui, |ui| {
                ui.set_min_width(118.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(value.into())
                            .size(scale.large)
                            .strong()
                            .color(accent.unwrap_or_else(|| self.color_text_normal())),
                    );
                    ui.label(
                        RichText::new(label.into())
                            .size(scale.small)
                            .color(self.color_text_dim()),
                    );
                });
            });
    }

    /// Compact `value label` chip used in list rows.
    pub(crate) fn benchmark_stat_chip(
        &self,
        ui: &mut Ui,
        label: impl Into<String>,
        value: impl Into<String>,
    ) {
        let scale = text_scale(ui);
        egui::Frame::NONE
            .fill(self.color_card_bg())
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(RichText::new(value.into()).size(scale.small).strong());
                ui.label(
                    RichText::new(label.into())
                        .size(scale.small)
                        .color(self.color_text_dim()),
                );
            });
    }

    /// Letter tag used to mark record A or B of a comparison.
    pub(super) fn benchmark_letter_tag(&self, ui: &mut Ui, letter: &str, color: Color32) {
        let scale = text_scale(ui);
        egui::Frame::NONE
            .fill(color)
            .corner_radius(CornerRadius::same(4))
            .inner_margin(Margin::symmetric(7, 1))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(letter)
                        .size(scale.body)
                        .strong()
                        .color(self.color_card_bg()),
                );
            });
    }
}
