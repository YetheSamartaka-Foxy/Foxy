use crate::core::steam;
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::path_is_inside_onedrive;
use eframe::egui::{Button, RichText, TextEdit, TextStyle, Ui, Vec2};
use log::{info, warn};
use rfd::FileDialog;

use super::render_wrapped_info_row;

fn path_action_button_width(ui: &Ui, label: &str, min_width: f32) -> f32 {
    let text_width = ui
        .painter()
        .layout_no_wrap(
            label.to_owned(),
            TextStyle::Button.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size()
        .x;
    (text_width + ui.spacing().button_padding.x * 2.0 + 8.0).max(min_width)
}

pub(super) struct ApplicationPathChangeFlags {
    pub settings: bool,
    pub addons: bool,
}

impl Foxy {
    /// Steam, temporary, and addon backup directory path settings + browse
    /// dialogs. Game-specific directories live on the game-space settings tab.
    pub(super) fn render_application_settings_paths(
        &mut self,
        ui: &mut Ui,
        horizontal_padding: f32,
        browse_button_width: f32,
        default_backup_path: &str,
        default_temp_path: &str,
        change_flags: &mut ApplicationPathChangeFlags,
    ) {
        // Steam Directory
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Steam Directory"));
            ui.add_space(horizontal_padding);
        });
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let browse_label = tr("Browse");
            let auto_detect_label = tr("Auto-detect");
            let browse_button_width = path_action_button_width(ui, &browse_label, browse_button_width);
            let auto_detect_button_width =
                path_action_button_width(ui, &auto_detect_label, browse_button_width);
            let text_edit_width = (ui.available_width()
                - 2.0 * horizontal_padding
                - browse_button_width
                - auto_detect_button_width
                - 2.0 * ui.spacing().item_spacing.x)
                .max(0.0);
            let steam_edit = ui.add(
                TextEdit::singleline(&mut self.settings_view_state.steam_directory)
                    .desired_width(text_edit_width),
            );
            if steam_edit.changed() {
                change_flags.settings = true;
                change_flags.addons = true;
            }
            if steam_edit.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let folder_button = ui.add_sized(
                Vec2::new(browse_button_width, 24.0),
                Button::new(browse_label),
            );
            if folder_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if folder_button.clicked()
                && let Some(folder) =
                    crate::ui::app::agent_support::pick_folder(|| FileDialog::new().pick_folder())
            {
                let path_str = folder.display().to_string();
                if path_is_inside_onedrive(&path_str) {
                    warn!("Rejected Steam directory inside OneDrive: {}", path_str);
                    self.show_error_toast(self.t("This path is inside a OneDrive folder. OneDrive sync can cause file access conflicts. Please choose a different location."));
                } else {
                    self.settings_view_state.steam_directory = path_str;
                    info!("Updated Steam directory from settings");
                    change_flags.settings = true;
                    change_flags.addons = true;
                }
            }

            let auto_detect_button = ui.add_sized(
                Vec2::new(auto_detect_button_width, 24.0),
                Button::new(auto_detect_label),
            ).on_hover_text(tr("Automatically detect the Steam installation directory using the Windows registry."));
            if auto_detect_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if auto_detect_button.clicked() {
                if let Some(folder) = steam::detect_steam_install_directory() {
                    self.settings_view_state.steam_directory = folder.display().to_string();
                    info!("Auto-detected Steam directory from registry");
                    change_flags.settings = true;
                    change_flags.addons = true;
                } else {
                    warn!("Failed to auto-detect Steam directory");
                    self.show_error_toast(self.t("Could not auto-detect Steam directory."));
                }
            }

            ui.add_space(horizontal_padding);
        });
        ui.separator();

        // Temporary Directory
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Temporary Directory"));
            ui.add_space(horizontal_padding);
        });
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let browse_label = tr("Browse");
            let browse_button_width = path_action_button_width(ui, &browse_label, browse_button_width);
            let text_edit_width = (ui.available_width()
                - 2.0 * horizontal_padding
                - browse_button_width
                - ui.spacing().item_spacing.x)
                .max(0.0);
            let temp_dir_edit = ui.add(
                TextEdit::singleline(&mut self.settings_view_state.temp_directory)
                    .hint_text(default_temp_path)
                    .desired_width(text_edit_width),
            );
            if temp_dir_edit.changed() {
                change_flags.settings = true;
            }
            if temp_dir_edit.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let folder_button = ui.add_sized(
                Vec2::new(browse_button_width, 24.0),
                Button::new(browse_label),
            );
            if folder_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if folder_button.clicked()
                && let Some(folder) =
                    crate::ui::app::agent_support::pick_folder(|| FileDialog::new().pick_folder())
            {
                let path_str = folder.display().to_string();
                if path_is_inside_onedrive(&path_str) {
                    warn!(
                        "Rejected temporary directory inside OneDrive: {}",
                        path_str
                    );
                    self.show_error_toast(self.t("This path is inside a OneDrive folder. OneDrive sync can cause file access conflicts. Please choose a different location."));
                } else {
                    self.settings_view_state.temp_directory = path_str;
                    info!("Updated temporary directory from settings");
                    change_flags.settings = true;
                }
            }

            ui.add_space(horizontal_padding);
        });
        render_wrapped_info_row(
            ui,
            horizontal_padding,
            RichText::new(tr(
                "Used as temporary storage for updater cache, metadata, and intermediate files. If empty, Foxy uses the app config directory (for example %APPDATA%\\Foxy on Windows or ~/.config/Foxy on Linux).",
            ))
            .italics()
            .color(self.color_text_dim()),
        );
        ui.separator();

        // Addon Backup Directory
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr("Addon Backup Directory"));
            ui.add_space(horizontal_padding);
        });
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let browse_label = tr("Browse");
            let browse_button_width = path_action_button_width(ui, &browse_label, browse_button_width);
            let text_edit_width = (ui.available_width()
                - 2.0 * horizontal_padding
                - browse_button_width
                - ui.spacing().item_spacing.x)
                .max(0.0);
            let backup_dir_edit = ui.add(
                TextEdit::singleline(&mut self.settings_view_state.backup_directory)
                    .hint_text(default_backup_path)
                    .desired_width(text_edit_width),
            );
            if backup_dir_edit.changed() {
                self.invalidate_backup_manager_inventory();
                change_flags.settings = true;
            }
            if backup_dir_edit.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }

            let folder_button = ui.add_sized(
                Vec2::new(browse_button_width, 24.0),
                Button::new(browse_label),
            );
            if folder_button.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            if folder_button.clicked()
                && let Some(folder) =
                    crate::ui::app::agent_support::pick_folder(|| FileDialog::new().pick_folder())
            {
                let path_str = folder.display().to_string();
                if path_is_inside_onedrive(&path_str) {
                    warn!(
                        "Rejected backup directory inside OneDrive: {}",
                        path_str
                    );
                    self.show_error_toast(self.t("This path is inside a OneDrive folder. OneDrive sync can cause file access conflicts. Please choose a different location."));
                } else {
                    self.settings_view_state.backup_directory = path_str;
                    self.invalidate_backup_manager_inventory();
                    info!("Updated addon backup directory from settings");
                    change_flags.settings = true;
                }
            }

            ui.add_space(horizontal_padding);
        });
        render_wrapped_info_row(
            ui,
            horizontal_padding,
            RichText::new(self.t_fmt(
                "Backups are stored here as addon folders prefixed with their local content hash. If empty, Foxy uses {path}. Automatic update backups and manual addon restore both use this location.",
                &[("path", default_backup_path.to_string())],
            ))
            .italics()
            .color(self.color_text_dim()),
        );
        ui.separator();
    }

    pub(super) fn refresh_detected_arma3_profiles(&mut self) {
        let custom_profiles_dir = self.settings_view_state.arma3_profiles_directory.trim();
        let custom_profiles_dir = if custom_profiles_dir.is_empty() {
            None
        } else {
            Some(std::path::Path::new(custom_profiles_dir))
        };
        self.detected_arma3_profiles =
            crate::core::arma3_profiles::detect_all_profiles(custom_profiles_dir);
        self.detected_active_arma3_profile =
            crate::core::arma3_profiles::detect_active_profile(&self.detected_arma3_profiles);
    }
}
