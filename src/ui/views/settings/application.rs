use crate::core::utils::app_paths;
use crate::ui::app::Foxy;
use eframe::egui::{ScrollArea, Ui};

use super::app_paths::ApplicationPathChangeFlags;

impl Foxy {
    pub(super) fn render_application_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;
        let browse_button_width = 70.0;
        let default_backup_path = app_paths::foxy_backups_dir().display().to_string();
        let default_temp_path = app_paths::foxy_data_dir().display().to_string();
        let mut path_change_flags = ApplicationPathChangeFlags {
            settings: false,
            addons: false,
        };

        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical(|ui| {
                self.render_application_settings_general(
                    ui,
                    horizontal_padding,
                    &mut path_change_flags.settings,
                );

                self.render_application_settings_paths(
                    ui,
                    horizontal_padding,
                    browse_button_width,
                    &default_backup_path,
                    &default_temp_path,
                    &mut path_change_flags,
                );

                self.render_application_settings_updates(
                    ui,
                    horizontal_padding,
                    &mut path_change_flags.settings,
                );
            });
        });

        self.render_application_settings_wipe_db_confirmation(ui);
        self.render_application_settings_reset_confirmation(ui);

        if path_change_flags.settings {
            self.save_settings();
            if path_change_flags.addons {
                self.invalidate_addon_inventory_cache();
            }
            if !ui.ctx().egui_wants_keyboard_input() {
                self.show_success_toast(self.t("Settings saved"));
            }
        }
    }
}
