use egui::{
    Button, Color32, CornerRadius, Frame, Label, Margin, Response, RichText, Sense, Stroke, Ui,
};

use crate::ui::app::Foxy;
use crate::ui::palette;

impl Foxy {
    /// Modal chrome for prompts that need the user's attention before anything
    /// else: the regular modal frame with a heavy danger-colored border.
    pub(crate) fn danger_modal_chrome(&self, ctx: &egui::Context) -> Frame {
        self.modal_window_chrome(ctx)
            .stroke(Stroke::new(2.0, self.color_error()))
    }

    pub(crate) fn danger_modal_title(&self, title: String) -> RichText {
        RichText::new(title).color(self.color_error()).strong()
    }

    /// Tinted, bordered panel with a warning glyph beside the key message.
    pub(crate) fn render_danger_callout(&self, ui: &mut Ui, message: String) {
        let danger = self.color_error();
        let glyph_size = egui::TextStyle::Body.resolve(ui.style()).size * 2.4;
        Frame::new()
            .fill(Self::blend_color(self.color_card_bg(), danger, 0.14))
            .stroke(Stroke::new(1.0, danger))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let row_layout = if crate::ui::i18n::is_rtl() {
                    egui::Layout::right_to_left(egui::Align::TOP)
                } else {
                    egui::Layout::left_to_right(egui::Align::TOP)
                };
                ui.with_layout(row_layout, |ui| {
                    Self::paint_danger_glyph(ui, glyph_size, danger);
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(message)
                                    .color(self.color_text_normal())
                                    .strong(),
                            )
                            .wrap(),
                        );
                    });
                });
            });
    }

    /// Full-width destructive-colored button for the action a prompt recommends.
    /// It takes keyboard focus while nothing else holds it, so Enter confirms it.
    pub(crate) fn add_danger_primary_button(
        &self,
        ui: &mut Ui,
        label: String,
        enabled: bool,
    ) -> Response {
        let body_size = egui::TextStyle::Body.resolve(ui.style()).size;
        let response = ui
            .vertical_centered_justified(|ui| {
                ui.add_enabled(
                    enabled,
                    Button::new(RichText::new(label).strong().color(palette::ON_STRONG_FILL))
                        .fill(self.color_action_destructive())
                        .stroke(Stroke::new(1.0, self.color_error()))
                        .corner_radius(CornerRadius::same(6))
                        .min_size(egui::vec2(
                            0.0,
                            Self::adaptive_button_height(body_size, 40.0),
                        )),
                )
            })
            .inner;
        if enabled && ui.ctx().memory(|memory| memory.focused().is_none()) {
            response.request_focus();
        }
        if enabled && response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        response
    }

    /// Frameless, dimmed button for the discouraged alternative in a prompt.
    pub(crate) fn add_secondary_prompt_button(&self, ui: &mut Ui, label: String) -> Response {
        let response =
            ui.add(Button::new(RichText::new(label).color(self.color_text_dim())).frame(false));
        if response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        response
    }

    fn paint_danger_glyph(ui: &mut Ui, size: f32, color: Color32) {
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), Sense::hover());
        let painter = ui.painter();
        painter.add(egui::Shape::convex_polygon(
            vec![rect.center_top(), rect.right_bottom(), rect.left_bottom()],
            color,
            Stroke::NONE,
        ));
        painter.text(
            rect.center_bottom() - egui::vec2(0.0, size * 0.08),
            egui::Align2::CENTER_BOTTOM,
            "!",
            egui::FontId::proportional(size * 0.62),
            palette::ON_STRONG_FILL,
        );
    }
}
