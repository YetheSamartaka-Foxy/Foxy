use crate::core::game::{DirectorySetting, GameModule};
use crate::ui::app::Foxy;
use crate::ui::i18n::tr;
use crate::ui::types::{SettingsViewState, path_is_inside_onedrive};
use eframe::egui::{Button, RichText, ScrollArea, TextEdit, TextStyle, Ui, Vec2};
use log::{info, warn};
use rfd::FileDialog;

use super::render_wrapped_info_row;

fn directory_field_mut<'a>(
    settings: &'a mut SettingsViewState,
    id: &str,
) -> Option<&'a mut String> {
    match id {
        "arma3_directory" => Some(&mut settings.arma3_directory),
        "twwh3_directory" => Some(&mut settings.twwh3_directory),
        "reforger_directory" => Some(&mut settings.reforger_directory),
        "teamspeak3_directory" => Some(&mut settings.teamspeak3_directory),
        "arma3_profiles_directory" => Some(&mut settings.arma3_profiles_directory),
        _ => None,
    }
}

fn toggle_field_mut<'a>(settings: &'a mut SettingsViewState, id: &str) -> Option<&'a mut bool> {
    match id {
        "apply_repo_json_client_parameters" => {
            Some(&mut settings.apply_repo_json_client_parameters)
        }
        "apply_repo_json_dlc_content" => Some(&mut settings.apply_repo_json_dlc_content),
        "warn_editor_external_addons" => Some(&mut settings.warn_editor_external_addons),
        "enable_editor_mission_list" => Some(&mut settings.enable_editor_mission_list),
        "enable_server_list" => Some(&mut settings.enable_server_list),
        "check_server_addons_before_join" => Some(&mut settings.check_server_addons_before_join),
        "check_ts3_running_before_join" => Some(&mut settings.check_ts3_running_before_join),
        "check_steam_running_before_launch" => {
            Some(&mut settings.check_steam_running_before_launch)
        }
        _ => None,
    }
}

struct DirectoryBinding {
    onedrive_check: bool,
    invalidates_addons: bool,
    refreshes_profiles: bool,
    validate_error: Option<&'static str>,
    detect_hover: &'static str,
    detect_error: &'static str,
}

fn directory_binding(id: &str) -> DirectoryBinding {
    match id {
        "arma3_directory" => DirectoryBinding {
            onedrive_check: true,
            invalidates_addons: true,
            refreshes_profiles: false,
            validate_error: Some(
                "Arma 3 executable was not found in the selected directory. Make sure this is your Arma 3 installation folder.",
            ),
            detect_hover: "Automatically detect the Arma 3 installation directory using Steam library metadata.",
            detect_error: "Could not auto-detect Arma 3 directory.",
        },
        "teamspeak3_directory" => DirectoryBinding {
            onedrive_check: false,
            invalidates_addons: false,
            refreshes_profiles: false,
            validate_error: Some(
                "TeamSpeak 3 client was not found in the selected directory. Make sure this is your TeamSpeak 3 installation folder.",
            ),
            detect_hover: "Automatically detect the TeamSpeak 3 installation directory.",
            detect_error: "Could not auto-detect TeamSpeak 3 directory.",
        },
        "twwh3_directory" => DirectoryBinding {
            onedrive_check: true,
            invalidates_addons: false,
            refreshes_profiles: false,
            validate_error: Some(
                "Warhammer3.exe was not found in the selected directory. Make sure this is your Total War: WARHAMMER III installation folder.",
            ),
            detect_hover: "Automatically detect the Total War: WARHAMMER III installation directory using Steam library metadata.",
            detect_error: "Could not auto-detect Total War: WARHAMMER III directory.",
        },
        "reforger_directory" => DirectoryBinding {
            onedrive_check: true,
            invalidates_addons: false,
            refreshes_profiles: false,
            validate_error: Some(
                "ArmaReforgerSteam.exe was not found in the selected directory. Make sure this is your Arma Reforger installation folder.",
            ),
            detect_hover: "Automatically detect the Arma Reforger installation directory using Steam library metadata.",
            detect_error: "Could not auto-detect Arma Reforger directory.",
        },
        "arma3_profiles_directory" => DirectoryBinding {
            onedrive_check: true,
            invalidates_addons: false,
            refreshes_profiles: true,
            validate_error: None,
            detect_hover: "",
            detect_error: "",
        },
        _ => DirectoryBinding {
            onedrive_check: true,
            invalidates_addons: false,
            refreshes_profiles: false,
            validate_error: None,
            detect_hover: "",
            detect_error: "",
        },
    }
}

fn validate_directory(module: &dyn GameModule, id: &str, folder: &std::path::Path) -> bool {
    match id {
        "arma3_directory" | "twwh3_directory" | "reforger_directory" => {
            module.validate_install_dir(folder)
        }
        "teamspeak3_directory" => {
            crate::core::ts3_plugin::teamspeak_client_exe_in(folder).is_some()
        }
        _ => true,
    }
}

fn detect_directory(
    module: &dyn GameModule,
    id: &str,
    steam_directory: &str,
) -> Option<std::path::PathBuf> {
    match id {
        "arma3_directory" | "twwh3_directory" | "reforger_directory" => {
            module.detect_install_dir(&crate::core::game::GameDetectCtx { steam_directory })
        }
        "teamspeak3_directory" => crate::core::ts3_plugin::detect_teamspeak_directory(),
        _ => None,
    }
}

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

#[derive(Default)]
struct GameSettingsChangeFlags {
    settings: bool,
    addons: bool,
    profiles: bool,
}

impl Foxy {
    pub(crate) fn render_game_space_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;
        let browse_button_width = 70.0;
        let mut change_flags = GameSettingsChangeFlags::default();
        let space = self.game_space_settings_target();
        let is_active_space = self.editing_active_game_space();
        let Some(module) = crate::core::game::registry().get(&space.game_id) else {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(self.t_fmt("Unknown game {game_id}", &[("game_id", space.game_id)]))
                    .color(self.color_text_error()),
            );
            return;
        };
        let schema = module.settings_schema();

        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical(|ui| {
                for directory in &schema.directories {
                    if directory.id == "arma3_profiles_directory" {
                        continue;
                    }
                    self.render_game_space_directory_setting(
                        ui,
                        module,
                        directory,
                        horizontal_padding,
                        browse_button_width,
                        &mut change_flags,
                    );
                    ui.separator();
                }

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    let row_width = (ui.available_width() - 2.0 * horizontal_padding).max(0.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(row_width, ui.spacing().interact_size.y),
                        eframe::egui::Layout::top_down(eframe::egui::Align::Min),
                        |ui| {
                            ui.set_width(row_width);
                            ui.horizontal_wrapped(|ui| {
                                for toggle in &schema.toggles {
                                    let Some(field) = toggle_field_mut(
                                        self.edited_game_space_settings_mut(),
                                        toggle.id,
                                    ) else {
                                        warn!(
                                            "Game settings schema toggle {} has no binding; skipping",
                                            toggle.id
                                        );
                                        continue;
                                    };
                                    Self::render_wrapped_settings_checkbox(
                                        ui,
                                        true,
                                        field,
                                        tr(toggle.label),
                                        Some(tr(toggle.help)),
                                        row_width,
                                        &mut change_flags.settings,
                                    );
                                }
                            });
                        },
                    );
                    ui.add_space(horizontal_padding);
                });
                ui.separator();
            });
        });

        if change_flags.settings {
            self.save_edited_game_space_settings();
            if is_active_space {
                if change_flags.addons {
                    self.invalidate_addon_inventory_cache();
                }
                if change_flags.profiles {
                    self.refresh_detected_arma3_profiles();
                }
            }
            if !ui.ctx().egui_wants_keyboard_input() {
                self.show_success_toast(self.t("Settings saved"));
            }
        }
    }

    pub(crate) fn render_game_space_profile_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;
        let browse_button_width = 70.0;
        let mut change_flags = GameSettingsChangeFlags::default();
        let space = self.game_space_settings_target();
        let is_active_space = self.editing_active_game_space();
        let Some(module) = crate::core::game::registry().get(&space.game_id) else {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(self.t_fmt("Unknown game {game_id}", &[("game_id", space.game_id)]))
                    .color(self.color_text_error()),
            );
            return;
        };
        let schema = module.settings_schema();
        let Some(directory) = schema
            .directories
            .iter()
            .find(|directory| directory.id == "arma3_profiles_directory")
        else {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(tr("Profiles are not available for this game space."))
                    .italics()
                    .color(self.color_text_dim()),
            );
            return;
        };

        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical(|ui| {
                self.render_game_space_directory_setting(
                    ui,
                    module,
                    directory,
                    horizontal_padding,
                    browse_button_width,
                    &mut change_flags,
                );
                ui.separator();

                if is_active_space {
                    self.render_arma3_profile_management(ui, horizontal_padding);
                } else {
                    render_wrapped_info_row(
                        ui,
                        horizontal_padding,
                        RichText::new(tr("Open this game space to manage Arma 3 profiles."))
                            .italics()
                            .color(self.color_text_dim()),
                    );
                }
            });
        });

        if change_flags.settings {
            self.save_edited_game_space_settings();
            if is_active_space && change_flags.profiles {
                self.refresh_detected_arma3_profiles();
            }
            if !ui.ctx().egui_wants_keyboard_input() {
                self.show_success_toast(self.t("Settings saved"));
            }
        }
    }

    fn render_game_space_directory_setting(
        &mut self,
        ui: &mut Ui,
        module: &'static dyn GameModule,
        setting: &DirectorySetting,
        horizontal_padding: f32,
        browse_button_width: f32,
        change_flags: &mut GameSettingsChangeFlags,
    ) {
        if directory_field_mut(self.edited_game_space_settings_mut(), setting.id).is_none() {
            warn!(
                "Game settings schema directory {} has no binding; skipping",
                setting.id
            );
            return;
        }
        let binding = directory_binding(setting.id);
        let mark_changed = |flags: &mut GameSettingsChangeFlags| {
            flags.settings = true;
            flags.addons |= binding.invalidates_addons;
            flags.profiles |= binding.refreshes_profiles;
        };

        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            ui.label(tr(setting.label));
            ui.add_space(horizontal_padding);
        });
        ui.horizontal(|ui| {
            ui.add_space(horizontal_padding);
            let browse_label = tr("Browse");
            let auto_detect_label = tr("Auto-detect");
            let browse_button_width =
                path_action_button_width(ui, &browse_label, browse_button_width);
            let auto_detect_button_width = setting
                .auto_detect
                .then(|| path_action_button_width(ui, &auto_detect_label, browse_button_width));
            let text_edit_width = (ui.available_width()
                - 2.0 * horizontal_padding
                - browse_button_width
                - auto_detect_button_width.unwrap_or(0.0)
                - if setting.auto_detect { 2.0 } else { 1.0 } * ui.spacing().item_spacing.x)
                .max(0.0);

            let field = directory_field_mut(self.edited_game_space_settings_mut(), setting.id)
                .expect("binding checked above");
            let directory_edit = ui.add(TextEdit::singleline(field).desired_width(text_edit_width));
            if directory_edit.changed() {
                mark_changed(change_flags);
            }
            if directory_edit.hovered() {
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
                if binding.onedrive_check && path_is_inside_onedrive(&path_str) {
                    warn!(
                        "Rejected {} inside OneDrive: {}",
                        setting.id, path_str
                    );
                    self.show_error_toast(self.t("This path is inside a OneDrive folder. OneDrive sync can cause file access conflicts. Please choose a different location."));
                } else {
                    let valid = validate_directory(module, setting.id, &folder);
                    if let Some(field) =
                        directory_field_mut(self.edited_game_space_settings_mut(), setting.id)
                    {
                        *field = path_str;
                    }
                    info!("Updated {} from settings", setting.id);
                    mark_changed(change_flags);
                    if !valid && let Some(validate_error) = binding.validate_error {
                        warn!(
                            "Selected directory failed validation for {}: {}",
                            setting.id,
                            folder.display()
                        );
                        self.show_error_toast(self.t(validate_error));
                    }
                }
            }

            if setting.auto_detect {
                let auto_detect_button = ui
                    .add_sized(
                        Vec2::new(auto_detect_button_width.unwrap_or(browse_button_width), 24.0),
                        Button::new(auto_detect_label),
                    )
                    .on_hover_text(tr(binding.detect_hover));
                if auto_detect_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if auto_detect_button.clicked() {
                    // Steam directory is app-global, so detection always reads
                    // the live settings even when editing another space.
                    let detected = detect_directory(
                        module,
                        setting.id,
                        &self.settings_view_state.steam_directory,
                    );
                    if let Some(folder) = detected {
                        let path_str = folder.display().to_string();
                        if binding.onedrive_check && path_is_inside_onedrive(&path_str) {
                            warn!("Rejected auto-detected {} inside OneDrive", setting.id);
                            self.show_error_toast(self.t("This path is inside a OneDrive folder. OneDrive sync can cause file access conflicts. Please choose a different location."));
                        } else {
                            if let Some(field) =
                                directory_field_mut(self.edited_game_space_settings_mut(), setting.id)
                            {
                                *field = path_str;
                            }
                            info!("Auto-detected {} from settings", setting.id);
                            mark_changed(change_flags);
                        }
                    } else {
                        warn!("Failed to auto-detect {}", setting.id);
                        self.show_error_toast(self.t(binding.detect_error));
                    }
                }
            }
            ui.add_space(horizontal_padding);
        });
        if let Some(help) = setting.help {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(tr(help))
                    .italics()
                    .color(self.color_text_dim()),
            );
        }
    }
}
