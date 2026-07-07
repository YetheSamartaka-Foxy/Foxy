use crate::ui::app::{Foxy, SettingsFolderRemovalConfirmAction};
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::search_filter::MultiEntryFilter;
use crate::ui::types::{
    additional_folder_alias_key, path_is_inside_onedrive, sanitize_additional_folder_alias,
};
use eframe::egui::{
    self, Align2, Button, CornerRadius, CursorIcon, Frame, Label, Margin, RichText, ScrollArea,
    TextEdit, Ui, Vec2,
};
use log::{info, warn};
use rfd::FileDialog;

use super::render_wrapped_info_row;

impl Foxy {
    pub(crate) fn render_additional_search_folders(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        ui.vertical(|ui| {
            render_wrapped_info_row(
                ui,
                horizontal_padding,
                RichText::new(format!(
                    "{} {}",
                    '\u{2139}',
                    tr("Here you can add folders from which other external addons will be searched from. Clicking on Delete won't delete actual folders but it will unregister their source to this app.")
                ))
                .italics()
                .color(self.color_text_dim()),
            );
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(tr("Filter:"));
                ui.add_space(horizontal_padding);
                let text_edit_width = ui.available_width() - 2.0 * horizontal_padding;
                ui.add(
                    TextEdit::singleline(
                        &mut self
                            .edited_game_space_settings_mut()
                            .additional_folders_filter,
                    )
                    .desired_width(text_edit_width),
                );
                ui.add_space(horizontal_padding);
            });

            ui.separator();

            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                let button_width = ui.available_width() - 2.0 * horizontal_padding;
                let add_button = ui.add_sized(
                    Vec2::new(button_width, 30.0),
                    Button::new(tr("Add new folder"))
                        .fill(self.color_widget_bg()),
                ).on_hover_text(tr("Register a directory where Foxy will search for external addons."));

                if add_button.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }

                if add_button.clicked()
                    && let Some(folder) =
                        crate::ui::app::agent_support::pick_folder(|| FileDialog::new().pick_folder())
                {
                        let path_str = folder.display().to_string();
                        if path_is_inside_onedrive(&path_str) {
                            warn!("Rejected additional folder inside OneDrive: {}", path_str);
                            self.show_error_toast(self.t("This path is inside a OneDrive folder. OneDrive sync can cause file access conflicts. Please choose a different location."));
                        } else {
                            info!("Added additional search folder {}", folder.display());
                            self.edited_game_space_settings_mut()
                                .additional_folders
                                .push(path_str);
                            self.save_edited_game_space_settings();
                            if self.editing_active_game_space() {
                                self.invalidate_addon_inventory_cache();
                            }
                        }
                    }
                ui.add_space(horizontal_padding);
            });
            ui.separator();

            ScrollArea::vertical().show(ui, |ui| {
                let multi_filter = MultiEntryFilter::parse(
                    &self.edited_game_space_settings().additional_folders_filter,
                );
                let folder_count = self.edited_game_space_settings().additional_folders.len();

                for i in 0..folder_count {
                    let folder = self.edited_game_space_settings().additional_folders[i].clone();
                    let folder_alias_key = additional_folder_alias_key(&folder);
                    let mut alias_value = self
                        .edited_game_space_settings()
                        .additional_folder_aliases
                        .get(&folder_alias_key)
                        .cloned()
                        .unwrap_or_default();
                    if !multi_filter.matches_any(&[folder.as_str(), alias_value.as_str()]) {
                        continue;
                    }

                    ui.horizontal(|ui| {
                        ui.add_space(horizontal_padding);

                        let card_frame = Frame {
                            fill: self.color_card_bg(),
                            stroke: egui::Stroke::new(1.0, self.color_text_gray()),
                            corner_radius: eframe::egui::CornerRadius::same(5),
                            inner_margin: Margin::same(5),
                            ..Default::default()
                        };

                        let card_width = ui.available_width() - 2.0 * horizontal_padding;
                        card_frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.add_sized(
                                        Vec2::new(card_width - 50.0, 30.0),
                                        Label::new(
                                            RichText::new(folder.clone())
                                                .color(self.color_text_normal()),
                                        ),
                                    );

                                    ui.horizontal(|ui| {
                                        ui.label(tr("Alias (optional):"));
                                        let alias_response = ui.add(
                                            TextEdit::singleline(&mut alias_value)
                                                .hint_text(tr("Alias"))
                                                .desired_width((card_width - 220.0).max(120.0)),
                                        );

                                        if alias_response.changed() {
                                            let sanitized_alias =
                                                sanitize_additional_folder_alias(&alias_value);
                                            let alias_changed = if sanitized_alias.is_empty() {
                                                self.edited_game_space_settings_mut()
                                                    .additional_folder_aliases
                                                    .remove(&folder_alias_key)
                                                    .is_some()
                                            } else {
                                                let settings =
                                                    self.edited_game_space_settings_mut();
                                                let current_alias = settings
                                                    .additional_folder_aliases
                                                    .get(&folder_alias_key)
                                                    .map(String::as_str);
                                                if current_alias != Some(sanitized_alias.as_str())
                                                {
                                                    settings.additional_folder_aliases.insert(
                                                        folder_alias_key.clone(),
                                                        sanitized_alias,
                                                    );
                                                    true
                                                } else {
                                                    false
                                                }
                                            };
                                            if alias_changed {
                                                self.save_edited_game_space_settings();
                                                if self.editing_active_game_space() {
                                                    self.invalidate_addon_inventory_cache();
                                                }
                                            }
                                        }
                                    });
                                });

                                let delete_button = ui.add_sized(
                                    Vec2::new(30.0, 30.0),
                                    Button::new("X").fill(self.color_text_error()),
                                ).on_hover_text(tr("Remove this folder from the additional search folders list. The actual folder is not deleted."));

                                if delete_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }

                                if delete_button.clicked() {
                                    self.pending_settings_folder_removal =
                                        Some(SettingsFolderRemovalConfirmAction::AdditionalSearchFolder {
                                            folder: folder.clone(),
                                        });
                                }
                            });
                        });

                        ui.add_space(horizontal_padding);
                    });

                    ui.add_space(10.0);
                }

            });
        });
    }

    pub(super) fn render_cleanup_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr("Here are addons that are not used by any repositories and can be safely deleted")
                );
                ui.label(RichText::new(info_text).italics().color(self.color_text_dim()));
            });
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(tr("Filter:"));
                ui.add_space(horizontal_padding);
                let text_edit_width = ui.available_width() - 2.0 * horizontal_padding;
                ui.add(
                    TextEdit::singleline(&mut self.settings_view_state.cleanup_folders_filter)
                        .desired_width(text_edit_width),
                );
                ui.add_space(horizontal_padding);
            });
            ui.separator();

            ScrollArea::vertical().show(ui, |ui| {
                let multi_filter =
                    MultiEntryFilter::parse(&self.settings_view_state.cleanup_folders_filter);
                let cleanup_folders = self.settings_view_state.cleanup_folders.clone();

                for (folder, _) in cleanup_folders {
                    if !multi_filter.matches_any(&[folder.as_str()]) {
                        continue;
                    }

                    ui.horizontal(|ui| {
                        ui.add_space(horizontal_padding);

                        let card_frame = Frame {
                            fill: self.color_card_bg(),
                            stroke: egui::Stroke::new(1.0, self.color_text_gray()),
                            corner_radius: eframe::egui::CornerRadius::same(5),
                            inner_margin: Margin::same(5),
                            ..Default::default()
                        };

                        let card_width = ui.available_width() - 2.0 * horizontal_padding;
                        card_frame.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    Vec2::new(card_width - 50.0, 30.0),
                                    eframe::egui::Label::new(
                                        RichText::new(folder.clone())
                                            .color(self.color_text_normal()),
                                    ),
                                );

                                let delete_button = ui.add_sized(
                                    Vec2::new(30.0, 30.0),
                                    Button::new("X").fill(self.color_text_error()),
                                ).on_hover_text(tr("Delete this unused addon folder from disk."));

                                if delete_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }

                                if delete_button.clicked() {
                                    self.pending_settings_folder_removal =
                                        Some(SettingsFolderRemovalConfirmAction::CleanupFolder {
                                            folder: folder.clone(),
                                        });
                                }
                            });
                        });

                        ui.add_space(horizontal_padding);
                    });

                    ui.add_space(10.0);
                }

            });
        });
    }

    pub(crate) fn render_settings_folder_removal_confirmation(&mut self, ui: &mut Ui) {
        let Some(action) = self.pending_settings_folder_removal.clone() else {
            return;
        };

        let (message, confirm_label) = match &action {
            SettingsFolderRemovalConfirmAction::AdditionalSearchFolder { folder } => (
                tr_fmt(
                    "Remove {path} from additional search folders?",
                    &[("path", folder.clone())],
                ),
                tr("Remove folder"),
            ),
            SettingsFolderRemovalConfirmAction::CleanupFolder { folder } => (
                tr_fmt(
                    "Remove {path} from cleanup folders?",
                    &[("path", folder.clone())],
                ),
                tr("Remove folder"),
            ),
        };

        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(tr("Confirm Folder Removal"))
            .frame(
                egui::Frame::window(&ui.ctx().global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(620.0)
            .show(ui.ctx(), |ui| {
                ui.label(message);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let confirm_btn =
                        ui.add(Button::new(confirm_label).fill(self.color_action_destructive()));
                    if confirm_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if confirm_btn.clicked() {
                        confirm = true;
                    }

                    let cancel_btn = ui.button(tr("Cancel"));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }
                });
            });

        if cancel {
            self.pending_settings_folder_removal = None;
            return;
        }

        if !confirm {
            return;
        }

        self.pending_settings_folder_removal = None;
        match action {
            SettingsFolderRemovalConfirmAction::AdditionalSearchFolder { folder } => {
                let settings = self.edited_game_space_settings_mut();
                let removed = if let Some(index) = settings
                    .additional_folders
                    .iter()
                    .position(|existing| existing == &folder)
                {
                    let removed_folder = settings.additional_folders.remove(index);
                    settings
                        .additional_folder_aliases
                        .remove(&additional_folder_alias_key(&removed_folder));
                    true
                } else {
                    false
                };
                if removed {
                    info!("Removed additional search folder {}", folder);
                    self.save_edited_game_space_settings();
                    if self.editing_active_game_space() {
                        self.invalidate_addon_inventory_cache();
                    }
                }
            }
            SettingsFolderRemovalConfirmAction::CleanupFolder { folder } => {
                if let Some(index) = self
                    .settings_view_state
                    .cleanup_folders
                    .iter()
                    .position(|(existing, _)| existing == &folder)
                {
                    info!("Removed cleanup folder {}", folder);
                    self.settings_view_state.cleanup_folders.remove(index);
                    self.save_settings();
                }
            }
        }
    }

    pub(super) fn render_direct_download_settings(&mut self, ui: &mut Ui) {
        let horizontal_padding = 15.0;
        self.initialize_direct_download_destination_if_empty();

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr("Download repositories, addons, or files directly from URL without database sync.")
                );
                ui.label(RichText::new(info_text).italics().color(self.color_text_dim()));
            });
            ui.separator();

            ui.horizontal(|ui| {
                ui.add_space(horizontal_padding);
                let open_button = ui.add_sized(
                    Vec2::new(ui.available_width() - 2.0 * horizontal_padding, 32.0),
                    Button::new(tr("Direct download")).fill(self.color_main_bg()),
                ).on_hover_text(tr("Open the direct download window to download a repository, addon, or file by URL without database sync."));
                if open_button.hovered() {
                    ui.ctx()
                        .output_mut(Foxy::set_pointing_cursor_output);
                }
                if open_button.clicked() {
                    self.show_direct_download_screen = true;
                    self.direct_download_error = None;
                }
                ui.add_space(horizontal_padding);
            });
            ui.separator();

            if let Some(session) = self.direct_download_session.clone() {
                let status_label = if session.is_running() {
                    tr_fmt(
                        "Direct download in progress: {done}/{total} files",
                        &[
                            ("done", session.files_done.to_string()),
                            ("total", session.files_total.to_string()),
                        ],
                    )
                } else if session.finished_successfully() {
                    tr("Direct download finished")
                } else {
                    tr("Direct download failed")
                };

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(
                        RichText::new(status_label)
                            .color(if session.finished_successfully() {
                                self.color_text_normal()
                            } else if session.is_running() {
                                self.color_text_dim()
                            } else {
                                self.color_text_error()
                            }),
                    );
                    ui.add_space(horizontal_padding);
                });

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(
                        RichText::new(tr_fmt(
                            "Source: {url}",
                            &[("url", session.source_url.clone())],
                        ))
                        .color(self.color_text_dim()),
                    );
                    ui.add_space(horizontal_padding);
                });

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(
                        RichText::new(tr_fmt(
                            "Destination: {path}",
                            &[("path", session.destination_folder.clone())],
                        ))
                        .color(self.color_text_dim()),
                    );
                    ui.add_space(horizontal_padding);
                });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    let show_update_button = ui.add_sized(
                        Vec2::new(ui.available_width() - 2.0 * horizontal_padding, 32.0),
                        Button::new(tr("Display update view")).fill(self.color_main_bg()),
                    ).on_hover_text(tr("Open the update progress view for the currently active direct download."));
                    if show_update_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if show_update_button.clicked() {
                        self.direct_download_update_view = true;
                        self.update_modal_open = true;
                        self.needs_repaint = true;
                    }
                    ui.add_space(horizontal_padding);
                });

                if let Some(error) = &session.error_message {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(horizontal_padding);
                        ui.colored_label(self.color_text_error(), error);
                        ui.add_space(horizontal_padding);
                    });
                }
            } else {
                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);
                    ui.label(RichText::new(tr("No direct download yet.")).color(self.color_text_dim()));
                    ui.add_space(horizontal_padding);
                });
            }
        });

        self.render_direct_download_screen(ui.ctx());
    }

    fn render_direct_download_screen(&mut self, ctx: &egui::Context) {
        if !self.show_direct_download_screen {
            return;
        }

        let mut open = self.show_direct_download_screen;
        let mut request_close = false;
        let mut start_download = false;

        egui::Window::new(tr("Direct download"))
            .open(&mut open)
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_text_normal()))
                    .corner_radius(eframe::egui::CornerRadius::same(10)),
            )
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .default_height(340.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(tr("Download URL"));
                let address_edit = ui.add(
                    TextEdit::singleline(&mut self.direct_download_url_input)
                        .desired_width(ui.available_width()),
                );
                if address_edit.hovered() {
                    ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                }

                ui.add_space(8.0);
                ui.label(tr("Destination folder"));
                ui.horizontal(|ui| {
                    let folder_edit = ui.add(
                        TextEdit::singleline(&mut self.direct_download_destination_input)
                            .desired_width((ui.available_width() - 215.0).max(120.0)),
                    );
                    if folder_edit.hovered() {
                        ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                    }
                    let browse_button = ui.button(tr("Browse"));
                    if browse_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if browse_button.clicked()
                        && let Some(folder) = crate::ui::app::agent_support::pick_folder(|| {
                            FileDialog::new().pick_folder()
                        })
                    {
                        self.direct_download_destination_input = folder.display().to_string();
                    }
                    let open_button = ui.button(tr("Open folder"));
                    if open_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if open_button.clicked() && !self.open_direct_download_destination_folder() {
                        self.show_error_toast(
                            self.t("Failed to open direct download destination folder."),
                        );
                    }
                });

                ui.add_space(8.0);
                let use_global_checkbox = Self::ui_state_checkbox(
                    ui,
                    &mut self.direct_download_use_global_speed_limit,
                    tr("Use global speed limit"),
                );
                if use_global_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }

                if self.direct_download_use_global_speed_limit {
                    let inherited = match self.settings_view_state.download_speed_limit_mbps {
                        Some(limit) if limit > 0 => format!("{} {}", limit, tr("Mbps")),
                        _ => tr("Unlimited"),
                    };
                    ui.label(
                        RichText::new(tr_fmt(
                            "Inherited global limit: {limit}",
                            &[("limit", inherited)],
                        ))
                        .color(self.color_text_dim()),
                    );
                } else {
                    let unlimited_checkbox = Self::ui_state_checkbox(
                        ui,
                        &mut self.direct_download_override_speed_unlimited,
                        tr("Unlimited"),
                    );
                    if unlimited_checkbox.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if !self.direct_download_override_speed_unlimited {
                        self.direct_download_override_speed_limit_mbps =
                            self.direct_download_override_speed_limit_mbps.max(1);
                        let speed_input = ui.add(
                            egui::DragValue::new(
                                &mut self.direct_download_override_speed_limit_mbps,
                            )
                            .range(1..=u32::MAX)
                            .suffix(format!(" {}", tr("Mbps"))),
                        );
                        if speed_input.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                    }
                }

                if let Some(error) = &self.direct_download_error {
                    ui.add_space(8.0);
                    ui.colored_label(self.color_text_error(), error);
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let download_button = ui.button(tr("Direct download"));
                    if download_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if download_button.clicked() {
                        start_download = true;
                    }

                    let close_button = ui.button(tr("Cancel"));
                    if close_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if close_button.clicked() {
                        request_close = true;
                    }
                });
            });

        if request_close {
            open = false;
        }
        self.show_direct_download_screen = open;

        if start_download && self.start_direct_download() {
            self.show_direct_download_screen = false;
        }
    }

    #[cfg(target_os = "windows")]
    fn open_direct_download_destination_folder(&self) -> bool {
        let destination = self.direct_download_destination_input.trim();
        if destination.is_empty() {
            return false;
        }

        let destination_path = std::path::PathBuf::from(destination);
        if let Err(err) = std::fs::create_dir_all(&destination_path) {
            warn!(
                "Failed to create direct download destination folder: {}",
                err
            );
            return false;
        }

        if let Err(err) = std::process::Command::new("explorer")
            .arg(destination_path.as_os_str())
            .spawn()
        {
            warn!("Failed to open direct download destination folder: {}", err);
            return false;
        }
        true
    }

    #[cfg(not(target_os = "windows"))]
    fn open_direct_download_destination_folder(&self) -> bool {
        let destination = self.direct_download_destination_input.trim();
        if destination.is_empty() {
            return false;
        }

        let destination_path = std::path::PathBuf::from(destination);
        if let Err(err) = std::fs::create_dir_all(&destination_path) {
            warn!(
                "Failed to create direct download destination folder: {}",
                err
            );
            return false;
        }

        if let Err(err) = crate::core::utils::platform::open_with_default_app(&destination_path) {
            warn!("Failed to open direct download destination folder: {}", err);
            return false;
        }
        true
    }
}
