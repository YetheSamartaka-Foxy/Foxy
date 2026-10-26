use eframe::egui::{self, Button, RichText, Ui, Vec2};
use log::{info, warn};

use crate::ui::app::Foxy;

impl Foxy {
    pub(crate) fn render_export_logs_button(&mut self, ui: &mut Ui) {
        let font_size = egui::TextStyle::Button.resolve(ui.style()).size;
        self.render_sized_export_logs_button(ui, Vec2::ZERO, font_size);
    }

    pub(crate) fn render_sized_export_logs_button(
        &mut self,
        ui: &mut Ui,
        min_size: Vec2,
        font_size: f32,
    ) {
        let response = ui
            .add(
                Button::new(RichText::new(self.export_logs_button_text()).size(font_size))
                    .fill(self.color_main_bg())
                    .min_size(min_size)
                    .wrap(),
            )
            .on_hover_text(self.export_logs_tooltip());
        if response.hovered() {
            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
        }
        if response.clicked() {
            self.on_export_logs_to_zip();
        }
    }

    pub(crate) fn export_logs_button_text(&self) -> String {
        format!("\u{1F4E6}  {}", self.t("Export logs to ZIP"))
    }

    pub(crate) fn export_logs_tooltip(&self) -> String {
        self.t("Package Foxy log files into a timestamped ZIP archive for sharing with support.")
    }

    pub(crate) fn on_export_logs_to_zip(&mut self) {
        info!("Export logs to ZIP triggered");
        match self.export_logs_to_zip() {
            Ok(true) => self.show_success_toast(self.t("Logs exported successfully.")),
            Ok(false) => {}
            Err(err) => {
                warn!("Failed to export logs to ZIP: {}", err);
                self.show_error_toast(self.t("Failed to export logs."));
            }
        }
    }
}
