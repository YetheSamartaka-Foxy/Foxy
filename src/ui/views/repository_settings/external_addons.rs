use super::ExternalAddonContextAction;
use crate::ui::app::{AddonDestructiveConfirmAction, Foxy};
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::i18n::{locale_compare, tr, tr_fmt};
use crate::ui::search_filter::MultiEntryFilter;
use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui,
    Vec2,
};
use log::warn;
use std::collections::{BTreeMap, HashMap, HashSet};

impl Foxy {
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_repository_external_addons_list(
        &mut self,
        ui: &mut Ui,
        repo_index: usize,
        external_addons: &mut [(String, bool, String)],
        filter: &mut String,
        origin_filter: &mut String,
        group_by_origin: &mut bool,
        addon_state_filter: &mut String,
    ) {
        if repo_index >= self.repository_view_state.repositories.len() {
            return;
        }

        let local_names = {
            let repo = &self.repository_view_state.repositories[repo_index];
            repo.addons
                .iter()
                .map(|(n, _)| n.to_lowercase())
                .chain(repo.optional_addons.iter().map(|(n, _)| n.to_lowercase()))
                .collect::<std::collections::HashSet<_>>()
        };

        let all_addons = self.get_or_generate_all_addons();

        let mut external_candidates: Vec<(String, String, String)> = all_addons
            .iter()
            .filter(|(addon_name, _path, _origin, _size_bytes)| {
                let lower = addon_name.to_lowercase();
                !local_names.contains(&lower)
            })
            .map(|(addon_name, path, origin, _size_bytes)| {
                (addon_name.clone(), path.clone(), origin.clone())
            })
            .collect();

        external_candidates.sort_by(|a, b| locale_compare(&a.0, &b.0));
        let workshop_root = self.normalized_steam_workshop_root_path();

        let enabled_card_fill = self.color_addon_row_enabled_bg();
        let disabled_card_fill = self.color_addon_row_disabled_bg();
        let color_text_normal = self.color_text_normal();
        let color_text_gray = self.color_text_gray();
        let color_text_dim = self.color_text_dim();

        let include_steam = {
            let repo = &mut self.repository_view_state.repositories[repo_index];
            if let Some(sel) = &repo.selected_profile {
                repo.profiles
                    .iter_mut()
                    .find(|p| &p.name == sel)
                    .map(|p| &mut p.include_steam_addons)
                    .unwrap_or(&mut repo.include_steam_addons)
            } else {
                &mut repo.include_steam_addons
            }
        };
        if !*include_steam {
            external_candidates.retain(|(_, path, _)| {
                !Foxy::is_steam_workshop_path_with_root(path, workshop_root.as_deref())
            });
        }

        if origin_filter.is_empty() {
            *origin_filter = "All".to_string();
        }

        // Build a path-based enabled lookup so that previously saved entries
        // match even when the display name changed (e.g. numeric workshop ID →
        // human-readable name from !Workshop symlinks).
        let mut enabled_by_path: HashMap<String, bool> = HashMap::new();
        for (_, enabled, p) in external_addons.iter() {
            enabled_by_path.insert(p.clone(), *enabled);
        }

        // Merge: use discovered candidates as the source of truth for names,
        // and fall back to saved enabled state by path.
        let mut external_map: HashMap<String, (String, bool)> = HashMap::new();
        for (addon_name, path, _) in &external_candidates {
            let enabled = enabled_by_path.get(path).copied().unwrap_or(false);
            external_map
                .entry(path.clone())
                .or_insert_with(|| (addon_name.clone(), enabled));
        }

        let origin_by_path: HashMap<String, String> = external_candidates
            .iter()
            .map(|(_, path, origin)| (path.clone(), origin.clone()))
            .collect();
        let mut origin_options: Vec<String> = external_candidates
            .iter()
            .map(|(_, _, origin)| origin.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        origin_options.sort_by(|a, b| locale_compare(a, b));

        let mut external_entries: Vec<(String, String, String, bool)> = external_map
            .into_iter()
            .map(|(path, (addon_name, enabled))| {
                let origin = origin_by_path
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| tr("Unknown origin"));
                (addon_name, path, origin, enabled)
            })
            .collect();
        external_entries.sort_by(|a, b| {
            locale_compare(&a.0, &b.0)
                .then_with(|| locale_compare(&a.2, &b.2))
                .then_with(|| a.1.cmp(&b.1))
        });

        let horizontal_padding = 15.0;
        let mut ui_state_changed = false;
        let mut repo_data_changed = false;
        let mut external_addon_context_action: Option<(
            String,
            String,
            ExternalAddonContextAction,
        )> = None;

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let info_text = format!(
                    "{} {}",
                    '\u{2139}',
                    tr("Here you can enable/disable external addons for this repository.\nThey come from all known paths except this repo's local/optional addons.")
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(info_text)
                        .italics()
                        .color(color_text_dim),
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(50.0);
                    let refresh_icon_size = self
                        .settings_view_state
                        .font_sizes
                        .repository_settings_view
                        .refresh_icon as f32;
                    let recheck_button = ui.add_sized(
                        Self::toolbar_icon_button_size(refresh_icon_size),
                        Button::new(RichText::new("\u{21bb}").size(refresh_icon_size)),
                    );

                    if recheck_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }

                    if recheck_button.clicked() {
                        self.cached_all_addons = None;
                    }

                    let disable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Disable all")));
                    if disable_all_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if disable_all_button.clicked() {
                        for (_, _, _, enabled) in &mut external_entries {
                            *enabled = false;
                        }
                        repo_data_changed = true;
                    }

                    ui.add_space(5.0);

                    let enable_all_button =
                        ui.add_sized(Vec2::new(120.0, 30.0), Button::new(tr("Enable all")));
                    if enable_all_button.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if enable_all_button.clicked() {
                        for (_, _, _, enabled) in &mut external_entries {
                            *enabled = true;
                        }
                        repo_data_changed = true;
                    }
                });
            });
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(tr("Filter:"));
                Foxy::filter_help_icon_colored(ui, color_text_dim, &tr("addon_filter_help"));
                ui.add_space(horizontal_padding);

                let filter_edit = ui.add(
                    TextEdit::singleline(filter).desired_width(ui.available_width()),
                );
                if filter_edit.changed() {
                    ui_state_changed = true;
                }
                if filter_edit.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
            });

            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                let include_steam_addons_checkbox =
                    ui.checkbox(include_steam, tr("Include Steam Addons"));
                if include_steam_addons_checkbox.changed() {
                    repo_data_changed = true;
                }
                if include_steam_addons_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                ui.label(tr("Origin:"));
                ui.add_space(6.0);
                let origin_combo = egui::ComboBox::from_id_salt("external_addon_origin_filter")
                    .selected_text(if origin_filter == "All" {
                        tr("All origins")
                    } else {
                        tr(origin_filter)
                    })
                    .show_ui(ui, |ui| {
                        let response_all =
                            ui.selectable_label(origin_filter == "All", tr("All origins"));
                        if response_all.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if response_all.clicked() {
                            *origin_filter = "All".to_string();
                            ui_state_changed = true;
                        }

                        for origin in &origin_options {
                            let response = ui.selectable_label(origin_filter == origin, tr(origin));
                            if response.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response.clicked() {
                                *origin_filter = origin.clone();
                                ui_state_changed = true;
                            }
                        }
                    });
                if origin_combo.response.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                let group_by_origin_checkbox =
                    ui.checkbox(group_by_origin, tr("Group by origin"));
                if group_by_origin_checkbox.changed() {
                    ui_state_changed = true;
                }
                if group_by_origin_checkbox.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                ui.add_space(16.0);

                ui.label(tr("State:"));
                ui.add_space(6.0);
                let combo_box_response =
                    egui::ComboBox::from_id_salt("external_addon_state_filter")
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

                            let response_enabled = ui
                                .selectable_label(addon_state_filter == "Enabled", tr("Enabled"));
                            if response_enabled.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response_enabled.clicked() {
                                *addon_state_filter = "Enabled".to_string();
                                ui_state_changed = true;
                            }

                            let response_disabled = ui.selectable_label(
                                addon_state_filter == "Disabled",
                                tr("Disabled"),
                            );
                            if response_disabled.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if response_disabled.clicked() {
                                *addon_state_filter = "Disabled".to_string();
                                ui_state_changed = true;
                            }
                        });

                if combo_box_response.response.hovered() {
                    ui.ctx()
                        .output_mut(Foxy::set_pointing_cursor_output);
                }
            });
            ui.separator();

            let multi_filter = MultiEntryFilter::parse(filter);
            let filtered_indices: Vec<usize> = external_entries
                .iter()
                .enumerate()
                .filter_map(|(index, (addon_name, path, origin, is_enabled))| {
                    let matches_text_filter = multi_filter.matches_any(&[
                        addon_name.as_str(),
                        path.as_str(),
                        origin.as_str(),
                    ]);
                    let matches_origin_filter =
                        origin_filter == "All" || origin.as_str() == origin_filter.as_str();
                    let matches_state_filter = match addon_state_filter.as_str() {
                        "Enabled" => *is_enabled,
                        "Disabled" => !*is_enabled,
                        _ => true,
                    };

                    (matches_text_filter && matches_origin_filter && matches_state_filter)
                        .then_some(index)
                })
                .collect();

            if external_entries.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("No external addons were found. Shared addons are unavailable, no external addon folder is configured, or no Steam addons were detected."))
                        .color(color_text_dim)
                        .italics(),
                );
                return;
            }

            if filtered_indices.is_empty() {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr("No external addons match the current filters."))
                        .color(color_text_dim)
                        .italics(),
                );
                return;
            }

            let render_entry = |ui: &mut Ui,
                                entry_index: usize,
                                external_entries: &mut Vec<(String, String, String, bool)>,
                                repo_data_changed: &mut bool,
                                external_addon_context_action: &mut Option<(
                String,
                String,
                ExternalAddonContextAction,
            )>| {
                let (addon_name, path, origin, is_enabled) = &mut external_entries[entry_index];

                ui.horizontal(|ui| {
                    ui.add_space(horizontal_padding);

                    let card_fill = if *is_enabled {
                        enabled_card_fill
                    } else {
                        disabled_card_fill
                    };
                    let text_color = if *is_enabled {
                        color_text_normal
                    } else {
                        color_text_gray
                    };

                    let card_frame = Frame {
                        fill: card_fill,
                        stroke: egui::Stroke::new(1.0, color_text_gray),
                        corner_radius: CornerRadius::same(5),
                        inner_margin: Margin::same(5),
                        ..Default::default()
                    };

                    let card_width = ui.available_width() - 2.0 * horizontal_padding;
                    let was_enabled = *is_enabled;
                    let card_response = card_frame.show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    Vec2::new(card_width - 50.0, 30.0),
                                    eframe::egui::Label::new(
                                        RichText::new(addon_name.as_str()).color(text_color),
                                    ),
                                );
                                let chk = Self::ui_state_checkbox(ui, is_enabled, "");
                                if chk.changed() {
                                    *repo_data_changed = true;
                                }
                                if chk.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                            });

                            ui.label(
                                RichText::new(path.as_str())
                                    .color(if *is_enabled {
                                        color_text_gray
                                    } else {
                                        color_text_dim
                                    })
                                    .size(
                                        self.settings_view_state
                                            .font_sizes
                                            .repository_settings_view
                                            .addon_path as f32,
                                    ),
                            );
                            if !*group_by_origin {
                                ui.label(
                                    RichText::new(tr_fmt(
                                        "Origin: {origin}",
                                        &[("origin", tr(origin))],
                                    ))
                                    .color(color_text_dim)
                                    .size(
                                        self.settings_view_state
                                            .font_sizes
                                            .repository_settings_view
                                            .addon_path as f32,
                                    ),
                                );
                            }
                        });
                    });
                    let context_response = card_response.response.interact(egui::Sense::click());
                    if context_response.hovered() {
                        ui.ctx()
                            .output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if context_response.clicked() && *is_enabled == was_enabled {
                        *is_enabled = !*is_enabled;
                        *repo_data_changed = true;
                    }
                    let mut context_action: Option<ExternalAddonContextAction> = None;
                    attach_context_menu(
                        &context_response,
                        &[
                            ContextMenuItem::new(
                                ExternalAddonContextAction::OpenDirectory,
                                tr("Open addon directory"),
                            ),
                            ContextMenuItem::new(
                                ExternalAddonContextAction::Delete,
                                tr("Delete addon"),
                            )
                            .separator_before()
                            .danger(),
                        ],
                        &mut context_action,
                    );
                    if let Some(action) = context_action {
                        *external_addon_context_action =
                            Some((addon_name.clone(), path.clone(), action));
                    }

                    ui.add_space(horizontal_padding);
                });
            };

            if *group_by_origin {
                let mut grouped_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                for entry_index in filtered_indices {
                    let origin = external_entries[entry_index].2.clone();
                    grouped_indices.entry(origin).or_default().push(entry_index);
                }

                ScrollArea::vertical().show(ui, |ui| {
                    for (origin, indices) in grouped_indices {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(horizontal_padding);
                            ui.heading(tr(&origin));
                        });
                        ui.add_space(4.0);

                        for entry_index in indices {
                            render_entry(
                                ui,
                                entry_index,
                                &mut external_entries,
                                &mut repo_data_changed,
                                &mut external_addon_context_action,
                            );
                        }
                    }
                });
            } else {
                let row_height = 66.0;

                ScrollArea::vertical().id_salt(("repository_external_addons_list", repo_index)).show_rows(
                    ui,
                    row_height,
                    filtered_indices.len(),
                    |ui, row_range| {
                        for filtered_index in row_range {
                            let entry_index = filtered_indices[filtered_index];
                            render_entry(
                                ui,
                                entry_index,
                                &mut external_entries,
                                &mut repo_data_changed,
                                &mut external_addon_context_action,
                            );
                        }
                    },
                );
            }
        });

        if repo_data_changed {
            let new_external_addons: Vec<(String, bool, String)> = external_entries
                .into_iter()
                .filter_map(|(addon_name, path, _origin, enabled)| {
                    enabled.then_some((addon_name, true, path))
                })
                .collect();

            if let Some(repo_mut) = self.repository_view_state.repositories.get_mut(repo_index) {
                if let Some(selected_name) = &repo_mut.selected_profile {
                    if let Some(p) = repo_mut
                        .profiles
                        .iter_mut()
                        .find(|p| &p.name == selected_name)
                    {
                        p.external_addons = new_external_addons;
                    }
                } else {
                    repo_mut.external_addons = new_external_addons;
                }
                self.save_repositories();
            }
        } else if ui_state_changed {
            // UI-only filter controls should not persist repository data.
        }

        if let Some((addon_name, addon_directory_path, action)) = external_addon_context_action {
            match action {
                ExternalAddonContextAction::OpenDirectory => {
                    if !self.open_addon_directory(&addon_name, &addon_directory_path) {
                        warn!("Failed to open addon directory for {}", addon_name);
                        self.show_error_toast(self.t("Failed to open addon directory."));
                    }
                }
                ExternalAddonContextAction::Delete => {
                    self.pending_addon_destructive_confirmation =
                        Some(AddonDestructiveConfirmAction::Delete {
                            addon_name,
                            addon_path: addon_directory_path,
                        });
                }
            }
        }
    }
}
