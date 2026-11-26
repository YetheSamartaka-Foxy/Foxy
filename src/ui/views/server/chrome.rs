use crate::ui::app::Foxy;
use eframe::egui::{
    self, Color32, CornerRadius, Label, Margin, Response, RichText, Sense, Stroke, Ui,
};

const SECTION_GAP: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

impl Foxy {
    /// Draws one workspace section as a card. A section that renders nothing (for example a
    /// capability-gated one) leaves no empty card behind.
    pub(super) fn server_section<R>(
        &mut self,
        ui: &mut Ui,
        add: impl FnOnce(&mut Self, &mut Ui) -> R,
    ) -> R {
        let mut prepared = self.server_card_frame().begin(ui);
        let inner = add(self, &mut prepared.content_ui);
        if prepared.content_ui.min_rect().height() >= 1.0 {
            let width = prepared.content_ui.max_rect().width();
            prepared.content_ui.set_min_width(width);
            prepared.end(ui);
            ui.add_space(SECTION_GAP);
        }
        inner
    }

    pub(super) fn server_card_frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.color_card_bg())
            .stroke(Stroke::new(1.0, self.color_widget_bg()))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(14, 12))
    }

    /// Secondary explanatory text under a heading or next to a control.
    pub(super) fn server_note(&self, ui: &mut Ui, text: impl Into<String>) -> Response {
        ui.add(Label::new(RichText::new(text.into()).color(self.color_text_dim())).wrap())
    }

    pub(super) fn server_tone_color(&self, tone: Tone) -> Color32 {
        match tone {
            Tone::Neutral => self.color_text_dim(),
            Tone::Info => self.color_debug(),
            Tone::Success => self.color_success(),
            Tone::Warning => self.color_warn(),
            Tone::Danger => self.color_error(),
        }
    }

    /// A compact status badge. The text always carries the meaning; color only reinforces it.
    pub(super) fn server_status_pill(&self, ui: &mut Ui, text: &str, tone: Tone) -> Response {
        let color = self.server_tone_color(tone);
        egui::Frame::new()
            .fill(Self::blend_color(self.color_card_bg(), color, 0.16))
            .stroke(Stroke::new(1.0, color))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(8, 2))
            .show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 0.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let height = ui.text_style_height(&egui::TextStyle::Body);
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, height), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 4.0, color);
                    ui.add(Label::new(RichText::new(text).color(color).strong()).extend())
                })
                .inner
            })
            .inner
    }

    /// A tinted message strip for page-level status, warnings and errors. `actions` draws
    /// trailing buttons on the same row.
    pub(super) fn server_callout(
        &self,
        ui: &mut Ui,
        tone: Tone,
        message: &str,
        actions: impl FnOnce(&mut Ui),
    ) -> Response {
        let color = self.server_tone_color(tone);
        let mut label = None;
        egui::Frame::new()
            .fill(Self::blend_color(self.color_card_bg(), color, 0.12))
            .stroke(Stroke::new(
                1.0,
                Self::blend_color(self.color_card_bg(), color, 0.6),
            ))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(4.0, 18.0), Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(2), color);
                    label = Some(ui.add(
                        Label::new(RichText::new(message).color(self.color_text_normal())).wrap(),
                    ));
                    actions(ui);
                });
            });
        ui.add_space(6.0);
        label.expect("callout label is always drawn")
    }

    /// Label/value rows aligned in two columns.
    pub(super) fn server_facts(&self, ui: &mut Ui, id: &str, rows: &[(String, String)]) {
        let dim = self.color_text_dim();
        egui::Grid::new(("server-facts", id))
            .num_columns(2)
            .min_row_height(ui.text_style_height(&egui::TextStyle::Body))
            .spacing(egui::vec2(18.0, 5.0))
            .show(ui, |ui| {
                for (label, value) in rows {
                    ui.label(RichText::new(label).color(dim));
                    ui.add(Label::new(value.as_str()).wrap());
                    ui.end_row();
                }
            });
    }
}

pub(super) fn yes_no(value: bool) -> String {
    crate::ui::i18n::tr(if value { "Yes" } else { "No" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_section_leaves_no_card_or_gap() {
        let ctx = egui::Context::default();
        let mut app = Foxy::new_headless(&ctx);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let start = ui.next_widget_position().y;
                app.server_section(ui, |_, _| ());
                assert_eq!(ui.next_widget_position().y, start);
                app.server_section(ui, |_, ui| {
                    ui.label("content");
                });
                assert!(ui.next_widget_position().y > start + SECTION_GAP);
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn yes_no_uses_words() {
        assert_eq!(yes_no(true), "Yes");
        assert_eq!(yes_no(false), "No");
    }
}
