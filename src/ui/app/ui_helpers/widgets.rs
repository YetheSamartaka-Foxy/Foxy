use std::time::{Duration, Instant};

use egui::{Align2, CornerRadius, Frame, Id, Margin, RichText};

use crate::ui::app::{Foxy, UiToastKind, UiToastState};

impl Foxy {
    fn show_toast(&mut self, message: String, kind: UiToastKind) {
        let duration = match kind {
            UiToastKind::Success => Duration::from_millis(2200),
            UiToastKind::Error => Duration::from_millis(3200),
        };
        self.ui_toast = Some(UiToastState {
            message,
            kind,
            shown_at: Instant::now(),
            duration,
        });
        self.needs_repaint = true;
    }

    /// Render a small circled-i help icon that reveals `help_text` on hover.
    ///
    /// Used to annotate filter inputs throughout the app with an explanation of
    /// the supported filter criteria, so the pattern stays visually consistent.
    pub(crate) fn filter_help_icon(&self, ui: &mut egui::Ui, help_text: &str) {
        Self::filter_help_icon_colored(ui, self.color_text_dim(), help_text);
    }

    /// `&self`-free variant of [`filter_help_icon`](Self::filter_help_icon) for
    /// call sites that still hold a borrow of `self` (e.g. an addon row's
    /// `&mut bool`) when the filter row is rendered. The caller supplies the
    /// already-resolved dim text color.
    pub(crate) fn filter_help_icon_colored(
        ui: &mut egui::Ui,
        color: egui::Color32,
        help_text: &str,
    ) {
        let response = ui.add(
            egui::Label::new(RichText::new("\u{24D8}").color(color)).sense(egui::Sense::hover()),
        );
        if response.hovered() {
            ui.ctx()
                .output_mut(|o| o.cursor_icon = egui::CursorIcon::Help);
        }
        response.on_hover_text(help_text);
    }

    pub(crate) fn show_success_toast(&mut self, message: impl Into<String>) {
        self.show_toast(message.into(), UiToastKind::Success);
    }

    pub(crate) fn show_error_toast(&mut self, message: impl Into<String>) {
        self.show_toast(message.into(), UiToastKind::Error);
    }

    pub(in crate::ui::app) fn render_ui_toast(&mut self, ctx: &egui::Context) {
        const FADE_OUT_DURATION: Duration = Duration::from_millis(220);

        let Some(toast) = self.ui_toast.as_ref() else {
            return;
        };

        // Read elapsed through the driver-controlled virtual clock so the
        // agent-gui `clock advance` command can fire toast expiry on demand;
        // with no driver running this is exactly `toast.shown_at.elapsed()`.
        let elapsed = crate::ui::app::agent_support::virtual_elapsed(toast.shown_at);
        if elapsed >= toast.duration {
            self.ui_toast = None;
            return;
        }

        let remaining = toast.duration.saturating_sub(elapsed);
        let alpha_factor = if remaining < FADE_OUT_DURATION {
            remaining.as_secs_f32() / FADE_OUT_DURATION.as_secs_f32()
        } else {
            1.0
        };

        let (fill, stroke_base) = match toast.kind {
            UiToastKind::Success => (self.color_success_muted(), self.color_success()),
            UiToastKind::Error => (self.color_widget_bg(), self.color_text_error()),
        };
        let stroke = Self::blend_color(stroke_base, fill, 0.25);
        let text_color = self.color_text_normal();

        egui::Area::new(Id::new("ui_toast"))
            .order(egui::Order::Foreground)
            .anchor(
                Align2::CENTER_BOTTOM,
                egui::vec2(0.0, -(self.footer_bar_height() + 18.0)),
            )
            .interactable(false)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(Self::color_with_alpha(fill, alpha_factor))
                    .stroke(egui::Stroke::new(
                        1.0,
                        Self::color_with_alpha(stroke, alpha_factor),
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&toast.message)
                                .color(Self::color_with_alpha(text_color, alpha_factor))
                                .strong(),
                        );
                    });
            });

        ctx.request_repaint_after(Duration::from_millis(16));
    }
}
