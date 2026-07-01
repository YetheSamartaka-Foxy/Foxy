use super::AddonContextAction;
use crate::ui::app::{AddonDestructiveConfirmAction, Foxy};
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::i18n::{locale_compare, tr, tr_fmt};
use crate::ui::search_filter::MultiEntryFilter;
use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui,
    Vec2,
};
use log::warn;
use std::collections::HashMap;

impl Foxy {
    #[allow(dead_code)]
    pub(super) fn render_repository_addons_list(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        label: &str,
        addons: &mut [(String, bool)],
        filter: &mut String,
        addon_state_filter: &mut String,
    ) {
        let repo_path = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .map(|repo| repo.path.clone())
            .unwrap_or_default();
        let repo_path_lower = repo_path.trim().to_lowercase();
        let backup_configured = self.configured_backup_directory().is_some();

        let all_addons = self.get_or_generate_all_addons();
        let mut addon_paths: HashMap<String, Vec<String>> = HashMap::new();
        for (addon_name, path, _origin, _size_bytes) in all_addons {
            addon_paths
                .entry(addon_name.to_lowercase())
                .or_default()
                .push(path.clone());
        }

        addons.sort_by(|a, b| locale_compare(&a.0, &b.0));
        let horizontal_padding = 15.0;
        let enabled_card_fill = self.color_addon_row_enabled_bg();
        let disabled_card_fill = self.color_addon_row_disabled_bg();
        let mut ui_state_changed = false;
        let mut repo_data_changed = false;
        let mut addon_context_action: Option<(String, Option<String>, AddonContextAction)> = None;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr_fmt(
                        "Here you can enable/disable {kind} for this repository.",
                        &[("kind", tr(label))],
                    )
                );
                ui.label(
                    RichText::new(info_text)
                        .italics()
                        .color(self.color_text_dim()),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(50.0);

                    let disable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Disable all")));
                    if disable_all_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if disable_all_button.clicked() {
                        for (_, enabled) in addons.iter_mut() {
                            *enabled = false;
                        }
                        repo_data_changed = true;
                    }

                    ui.add_space(5.0);

                    let enable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Enable all")));
                    if enable_all_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if enable_all_button.clicked() {
                        for (_, enabled) in addons.iter_mut() {
                            *enabled = true;
                        }
                        repo_data_changed = true;
                    }
                });
            });
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(tr("Filter:"));
                self.filter_help_icon(ui, &tr("addon_filter_help"));
                ui.add_space(horizontal_padding);

                let text_edit_width = ui.available_width() - 150.0 - 2.0 * horizontal_padding;
                let filter_edit =
                    ui.add(TextEdit::singleline(filter).desired_width(text_edit_width));
                if filter_edit.changed() {
                    ui_state_changed = true;
                }
                if filter_edit.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(10.0);

                let combo_box_response = egui::ComboBox::from_label("")
                    .selected_text(match addon_state_filter.as_str() {
                        "Enabled" => tr("Enabled"),
                        "Disabled" => tr("Disabled"),
                        _ => tr("All"),
                    })
                    .show_ui(ui, |ui| {
                        let response_all =
                            ui.selectable_label(addon_state_filter == "All", tr("All"));
                        if response_all.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if response_all.clicked() {
                            *addon_state_filter = "All".to_string();
                            ui_state_changed = true;
                        }

                        let response_enabled =
                            ui.selectable_label(addon_state_filter == "Enabled", tr("Enabled"));
                        if response_enabled.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if response_enabled.clicked() {
                            *addon_state_filter = "Enabled".to_string();
                            ui_state_changed = true;
                        }

                        let response_disabled =
                            ui.selectable_label(addon_state_filter == "Disabled", tr("Disabled"));
                        if response_disabled.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if response_disabled.clicked() {
                            *addon_state_filter = "Disabled".to_string();
                            ui_state_changed = true;
                        }
                    });

                if combo_box_response.response.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            });
            ui.separator();

            let multi_filter = MultiEntryFilter::parse(filter);
            let filtered_indices: Vec<usize> = addons
                .iter()
                .enumerate()
                .filter_map(|(index, (name, enabled))| {
                    let matches_text_filter = multi_filter.matches_any(&[name.as_str()]);
                    let matches_state_filter = match addon_state_filter.as_str() {
                        "Enabled" => *enabled,
                        "Disabled" => !*enabled,
                        _ => true,
                    };

                    (matches_text_filter && matches_state_filter).then_some(index)
                })
                .collect();

            if addons.is_empty() {
                let empty_message = match label {
                    "optional addons" => {
                        tr("This repository does not provide any optional addons.")
                    }
                    _ => tr("This repository does not provide any addons in this section."),
                };
                ui.add_space(8.0);
                ui.label(
                    RichText::new(empty_message)
                        .color(self.color_text_dim())
                        .italics(),
                );
                return;
            }

            if filtered_indices.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("No addons match the current filters."))
                        .color(self.color_text_dim())
                        .italics(),
                );
                return;
            }

            let row_height = 66.0;

            ScrollArea::vertical()
                .id_salt(("repository_addons_list", repo_index, label))
                .show_rows(ui, row_height, filtered_indices.len(), |ui, row_range| {
                    for filtered_index in row_range {
                        let addon_index = filtered_indices[filtered_index];
                        let (name, enabled) = &mut addons[addon_index];

                        ui.horizontal(|ui| {
                            ui.add_space(horizontal_padding);

                            let addon_directory_path =
                                addon_paths.get(&name.to_lowercase()).and_then(|paths| {
                                    if repo_path_lower.is_empty() {
                                        return paths.first().cloned();
                                    }
                                    paths
                                        .iter()
                                        .find(|path| {
                                            path.to_lowercase().starts_with(&repo_path_lower)
                                        })
                                        .cloned()
                                        .or_else(|| paths.first().cloned())
                                });
                            let path_text = addon_directory_path
                                .clone()
                                .unwrap_or_else(|| tr("(path not found)"));

                            let card_fill = if *enabled {
                                enabled_card_fill
                            } else {
                                disabled_card_fill
                            };
                            let text_color = if *enabled {
                                self.color_text_normal()
                            } else {
                                self.color_text_gray()
                            };

                            let card_frame = Frame {
                                fill: card_fill,
                                stroke: egui::Stroke::new(1.0, self.color_text_gray()),
                                corner_radius: CornerRadius::same(5),
                                inner_margin: Margin::same(5),
                                ..Default::default()
                            };

                            let card_width = ui.available_width() - 2.0 * horizontal_padding;
                            let was_enabled = *enabled;
                            let card_response = card_frame.show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            Vec2::new(card_width - 50.0, 30.0),
                                            eframe::egui::Label::new(
                                                RichText::new(name.as_str()).color(text_color),
                                            ),
                                        );

                                        let chk = Self::ui_state_checkbox(ui, enabled, "");
                                        if chk.changed() {
                                            repo_data_changed = true;
                                        }
                                        if chk.hovered() {
                                            ui.ctx().output_mut(|o| {
                                                Foxy::set_pointing_cursor_output(o)
                                            });
                                        }
                                    });

                                    ui.label(
                                        RichText::new(path_text)
                                            .color(if *enabled {
                                                self.color_text_gray()
                                            } else {
                                                self.color_text_dim()
                                            })
                                            .size(
                                                self.settings_view_state
                                                    .font_sizes
                                                    .repository_settings_view
                                                    .addon_path
                                                    as f32,
                                            ),
                                    );
                                });
                            });
                            let context_response =
                                card_response.response.interact(egui::Sense::click());
                            if context_response.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if context_response.clicked() && *enabled == was_enabled {
                                *enabled = !*enabled;
                                repo_data_changed = true;
                            }
                            let mut context_action: Option<AddonContextAction> = None;
                            attach_context_menu(
                                &context_response,
                                &[
                                    ContextMenuItem::new(
                                        AddonContextAction::OpenDirectory,
                                        tr("Open addon directory"),
                                    )
                                    .disabled_if(addon_directory_path.is_none()),
                                    ContextMenuItem::new(
                                        AddonContextAction::Backup,
                                        tr("Manual addon backup"),
                                    )
                                    .disabled_if(
                                        addon_directory_path.is_none() || !backup_configured,
                                    )
                                    .separator_before(),
                                    ContextMenuItem::new(
                                        AddonContextAction::RestoreBackup,
                                        tr("Restore addon backup"),
                                    )
                                    .disabled_if(!backup_configured)
                                    .separator_before(),
                                    ContextMenuItem::new(
                                        AddonContextAction::RecheckIntegrity,
                                        tr("Recheck addon integrity"),
                                    )
                                    .separator_before(),
                                    ContextMenuItem::new(
                                        AddonContextAction::StandaloneDownload,
                                        tr("Standalone download"),
                                    )
                                    .separator_before(),
                                    ContextMenuItem::new(
                                        AddonContextAction::ForceRedownload,
                                        tr("Force redownload addon"),
                                    )
                                    .separator_before()
                                    .danger(),
                                    ContextMenuItem::new(
                                        AddonContextAction::Delete,
                                        tr("Delete addon"),
                                    )
                                    .disabled_if(addon_directory_path.is_none())
                                    .separator_before()
                                    .danger(),
                                ],
                                &mut context_action,
                            );
                            if let Some(action) = context_action {
                                addon_context_action =
                                    Some((name.clone(), addon_directory_path.clone(), action));
                            }

                            ui.add_space(horizontal_padding);
                        });
                    }
                }); // End ScrollArea

            if repo_data_changed {
                if let Some(repo) = self.repository_view_state.repositories.get_mut(repo_index) {
                    if let Some(selected_name) = &repo.selected_profile {
                        if let Some(p) = repo.profiles.iter_mut().find(|p| &p.name == selected_name)
                        {
                            match label {
                                "addons" => p.addons = addons.to_vec(),
                                "optional addons" => p.optional_addons = addons.to_vec(),
                                _ => {}
                            }
                        }
                    } else {
                        match label {
                            "addons" => repo.addons = addons.to_vec(),
                            "optional addons" => repo.optional_addons = addons.to_vec(),
                            _ => {}
                        }
                    }
                    self.save_repositories();
                }
            } else if ui_state_changed {
                // UI-only filter controls should not persist repository data.
            }

            if let Some((addon_name, addon_directory_path, action)) = addon_context_action {
                match action {
                    AddonContextAction::OpenDirectory => {
                        if let Some(path) = addon_directory_path {
                            if !self.open_addon_directory(&addon_name, &path) {
                                warn!("Failed to open addon directory for {}", addon_name);
                                self.show_error_toast(self.t("Failed to open addon directory."));
                            }
                        } else {
                            warn!(
                                "Open addon directory skipped: path not found for {}",
                                addon_name
                            );
                        }
                    }
                    AddonContextAction::Backup => {
                        if !self.start_manual_addon_backup(
                            repo_index,
                            &addon_name,
                            addon_directory_path.as_deref(),
                        ) {
                            warn!("Manual addon backup failed for {}", addon_name);
                        }
                    }
                    AddonContextAction::RestoreBackup => {
                        if !self.open_addon_backup_restore_selector(
                            repo_index,
                            &addon_name,
                            addon_directory_path.as_deref(),
                        ) {
                            warn!("Addon backup restore selection failed for {}", addon_name);
                        }
                    }
                    AddonContextAction::RecheckIntegrity => {
                        if !self.recalculate_addon_hashes(repo_index, &addon_name) {
                            warn!("Manual addon hash recalculation failed for {}", addon_name);
                        }
                    }
                    AddonContextAction::StandaloneDownload => {
                        if !self.standalone_download_addon(repo_index, &addon_name) {
                            warn!("Standalone download failed for addon {}", addon_name);
                        }
                    }
                    AddonContextAction::ForceRedownload => {
                        self.pending_addon_destructive_confirmation =
                            Some(AddonDestructiveConfirmAction::ForceRedownload {
                                repo_idx: repo_index,
                                addon_name,
                                addon_path: addon_directory_path,
                            });
                    }
                    AddonContextAction::Delete => {
                        if let Some(path) = addon_directory_path {
                            self.pending_addon_destructive_confirmation =
                                Some(AddonDestructiveConfirmAction::Delete {
                                    addon_name,
                                    addon_path: path,
                                });
                        }
                    }
                }
            }
        });
    }
}
