use crate::ui::app::Foxy;
use crate::ui::types::sanitize_user_path;
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, Button, CursorIcon, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui,
    Vec2,
};
use log::info;
use rfd::FileDialog;

impl Foxy {
    pub fn render_repository_space_settings_view(
        &mut self,
        ui: &mut Ui,
        _frame: &mut eframe::Frame,
    ) {
        if self.repository_space_settings_state.is_none() {
            if let Some(space_id) = self.selected_repository_space_id.clone() {
                self.open_repository_space_settings(&space_id);
            } else {
                self.restore_last_view_or_default();
                return;
            }
        }

        let Some(mut settings) = self.repository_space_settings_state.clone() else {
            self.restore_last_view_or_default();
            return;
        };

        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == settings.space_id)
            .cloned()
        else {
            self.repository_space_settings_state = None;
            self.restore_last_view_or_default();
            return;
        };

        let mut save = false;
        let mut close_with_save = false;
        let mut cancel = false;
        let mut delete = false;

        let settings_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        let settings_frame = Frame::NONE.inner_margin(settings_margin);
        settings_frame.show(ui, |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let close_icon_size = self
                            .settings_view_state
                            .font_sizes
                            .repository_settings_view
                            .close_icon as f32;
                        ui.heading(
                            RichText::new(self.t("Repository space settings")).size(
                                self.settings_view_state
                                    .font_sizes
                                    .repository_settings_view
                                    .page_title as f32,
                            ),
                        );

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let close_button = ui.add_sized(
                                Self::modal_icon_button_size(close_icon_size),
                                Button::new(
                                    RichText::new("X")
                                        .color(self.color_text_normal())
                                        .size(close_icon_size),
                                )
                                .fill(self.color_main_bg()),
                            );
                            if close_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if close_button.clicked() {
                                close_with_save = true;
                            }
                        });
                    });
                    ui.label(self.t_fmt(
                        "Repository space settings - {name}",
                        &[(
                            "name",
                            Self::truncate_display_name(
                                Self::repository_space_display_name(&space),
                                50,
                            ),
                        )],
                    ));
                    ui.separator();

                    Frame::NONE
                        .fill(self.color_card_bg())
                        .stroke(egui::Stroke::new(1.0, self.color_text_gray()))
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(Margin::same(15))
                        .show(ui, |ui| {
                            ui.label(self.t("Address"));
                            let address_input = ui.add(
                                TextEdit::singleline(&mut settings.source_address_buffer)
                                    .desired_width(ui.available_width())
                                    .char_limit(500),
                            );
                            if address_input.hovered() {
                                ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                            }

                            ui.add_space(8.0);
                            ui.label(self.t("Local name"));
                            let name_input = ui.add(
                                TextEdit::singleline(&mut settings.local_name_buffer)
                                    .desired_width(ui.available_width())
                                    .char_limit(100),
                            );
                            if name_input.hovered() {
                                ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                            }

                            ui.add_space(8.0);
                            ui.label(self.t("Shared path"));
                            ui.horizontal(|ui| {
                                let path_input = ui.add(
                                    TextEdit::singleline(&mut settings.shared_path_buffer)
                                        .desired_width((ui.available_width() - 110.0).max(120.0)),
                                );
                                if path_input.hovered() {
                                    ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                                }
                                let browse_btn = ui.button(self.t("Browse"));
                                if browse_btn.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if browse_btn.clicked()
                                    && let Some(dir) =
                                        crate::ui::app::agent_support::pick_folder(|| {
                                            FileDialog::new().pick_folder()
                                        })
                                {
                                    settings.shared_path_buffer = dir.display().to_string();
                                }
                            });

                            if let Some(error) = &settings.error {
                                ui.add_space(8.0);
                                ui.colored_label(self.color_text_error(), error);
                            }

                            ui.separator();
                            ui.horizontal(|ui| {
                                let save_btn = ui.button(self.t("Save"));
                                if save_btn.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if save_btn.clicked() {
                                    save = true;
                                }

                                let cancel_btn = ui.button(self.t("Cancel"));
                                if cancel_btn.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if cancel_btn.clicked() {
                                    cancel = true;
                                }

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let delete_btn = ui.button(self.t("Delete repository space"));
                                    if delete_btn.hovered() {
                                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                    }
                                    if delete_btn.clicked() {
                                        delete = true;
                                    }
                                });
                            });
                        });
                });
            });
        });

        if delete {
            self.pending_repository_space_delete_id = Some(settings.space_id.clone());
            self.repository_space_settings_state = None;
            self.restore_last_view_or_default();
            return;
        }

        if cancel {
            self.repository_space_settings_state = None;
            self.restore_last_view_or_default();
            return;
        }

        let current_local_name = space
            .local_name_override
            .as_deref()
            .unwrap_or(space.name.as_str());
        let source_changed = settings.source_address_buffer != space.source_address;
        let local_name_changed = settings.local_name_buffer.trim() != current_local_name.trim();
        let shared_path_changed =
            sanitize_user_path(&settings.shared_path_buffer) != space.shared_path;

        let mut save_succeeded = false;
        if save || close_with_save {
            if source_changed {
                if let Err(err) = self.set_repository_space_source_address(
                    &settings.space_id,
                    &settings.source_address_buffer,
                ) {
                    settings.error = Some(err);
                    self.show_error_toast(self.t("Failed to save repository space settings"));
                } else {
                    if local_name_changed {
                        self.set_repository_space_local_name(
                            &settings.space_id,
                            settings.local_name_buffer.clone(),
                        );
                    }
                    if shared_path_changed {
                        self.set_repository_space_shared_path(
                            &settings.space_id,
                            settings.shared_path_buffer.clone(),
                        );
                    }
                    settings.error = None;
                    save_succeeded = true;
                }
            } else {
                if local_name_changed {
                    self.set_repository_space_local_name(
                        &settings.space_id,
                        settings.local_name_buffer.clone(),
                    );
                }
                if shared_path_changed {
                    self.set_repository_space_shared_path(
                        &settings.space_id,
                        settings.shared_path_buffer.clone(),
                    );
                }
                settings.error = None;
                save_succeeded = true;
            }

            if save_succeeded {
                self.show_success_toast(self.t("Repository space settings saved"));
            }
        }

        if close_with_save && save_succeeded {
            self.repository_space_settings_state = None;
            self.restore_last_view_or_default();
            return;
        }

        self.repository_space_settings_state = Some(settings);
    }

    #[allow(dead_code)]
    fn render_repository_space_selector_fullscreen(&mut self, ui: &mut Ui) {
        let Some(mut selector) = self.repository_space_selector_state.clone() else {
            return;
        };

        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == selector.space_id)
            .cloned()
        else {
            self.repository_space_selector_state = None;
            return;
        };

        let mut close = false;
        let mut apply_path = false;
        let mut refresh_scan = false;
        let mut move_selected = false;
        let mut add_entry_action: Option<(String, String)> = None;

        let selector_margin = Margin {
            left: 15,
            right: 15,
            top: 10,
            bottom: 10,
        };
        let selector_frame = Frame::NONE.inner_margin(selector_margin);
        selector_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(self.t_fmt(
                    "Repository space selector - {name}",
                    &[(
                        "name",
                        Self::repository_space_display_name(&space).to_string(),
                    )],
                ));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let close_button = ui.add_sized(
                        Vec2::new(30.0, 30.0),
                        Button::new(RichText::new("X").color(self.color_text_normal()))
                            .fill(self.color_main_bg()),
                    );
                    if close_button.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if close_button.clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            ui.label(self.t("Select repositories from this space"));
            ui.separator();

            ui.horizontal(|ui| {
                ui.label(self.t("Shared path"));
            });
            ui.horizontal(|ui| {
                let edit = ui.add(
                    TextEdit::singleline(&mut selector.path_buffer)
                        .desired_width((ui.available_width() - 210.0).max(160.0)),
                );
                if edit.hovered() {
                    ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                }
                let browse_btn = ui.button(self.t("Browse"));
                if browse_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if browse_btn.clicked()
                    && let Some(dir) = crate::ui::app::agent_support::pick_folder(|| {
                        FileDialog::new().pick_folder()
                    })
                {
                    selector.path_buffer = dir.display().to_string();
                }
                let apply_btn = ui.button(self.t("Apply path"));
                if apply_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if apply_btn.clicked() {
                    apply_path = true;
                }
            });

            if let Some(error) = &selector.error {
                ui.colored_label(self.color_text_error(), error);
            }

            ui.separator();
            ui.label(RichText::new(self.t("Available repositories")).strong());
            self.space_selector_entry_galleys.ensure(
                space.entries.len(),
                2,
                galley_cache::fingerprint((
                    space.id.as_str(),
                    space
                        .entries
                        .iter()
                        .map(|entry| (entry.name.as_str(), entry.address.as_str(), entry.required))
                        .collect::<Vec<_>>(),
                )),
                galley_cache::fingerprint((
                    self.color_text_normal().to_array(),
                    self.color_text_dim().to_array(),
                    self.color_primary_accent().to_array(),
                )),
            );
            ScrollArea::vertical()
                .id_salt(("space_selector_entries", &space.id))
                .max_height(240.0)
                .show_rows(
                    ui,
                    Self::repository_space_selector_entry_row_height(),
                    space.entries.len(),
                    |ui, row_range| {
                        ui.set_min_width(ui.available_width());
                        for entry_idx in row_range {
                            ui.push_id(entry_idx, |ui| {
                                let entry = &space.entries[entry_idx];
                                self.render_repository_space_selector_entry_card(
                                    ui,
                                    entry_idx,
                                    &space,
                                    entry,
                                    &mut add_entry_action,
                                );
                            });
                        }
                    },
                );

            ui.separator();
            ui.horizontal(|ui| {
                let scan_btn = ui.button(self.t("Scan existing repositories"));
                if scan_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if scan_btn.clicked() {
                    refresh_scan = true;
                }
                let move_btn = ui.button(self.t("Move selected repositories"));
                if move_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if move_btn.clicked() {
                    move_selected = true;
                }
            });

            if selector.candidates.is_empty() {
                ScrollArea::vertical()
                    .id_salt(("space_selector_scan_candidates", &space.id))
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.label(self.t("No matching existing repositories found"));
                    });
            } else {
                self.space_selector_candidate_galleys.ensure(
                    selector.candidates.len(),
                    1,
                    galley_cache::fingerprint((
                        space.id.as_str(),
                        selector
                            .candidates
                            .iter()
                            .filter_map(|candidate| {
                                self.repository_view_state
                                    .repositories
                                    .get(candidate.repo_index)
                                    .map(|repo| {
                                        (
                                            candidate.repo_index,
                                            repo.name.as_str(),
                                            repo.address.as_str(),
                                        )
                                    })
                            })
                            .collect::<Vec<_>>(),
                    )),
                    galley_cache::fingerprint((
                        self.color_text_normal().to_array(),
                        self.color_text_dim().to_array(),
                    )),
                );
                ScrollArea::vertical()
                    .id_salt(("space_selector_scan_candidates", &space.id))
                    .max_height(160.0)
                    .show_rows(
                        ui,
                        Self::repository_space_candidate_row_height(),
                        selector.candidates.len(),
                        |ui, row_range| {
                            for candidate_idx in row_range {
                                ui.push_id(candidate_idx, |ui| {
                                    let candidate = &mut selector.candidates[candidate_idx];
                                    self.render_repository_space_candidate_row(
                                        ui,
                                        candidate_idx,
                                        false,
                                        candidate,
                                    );
                                });
                            }
                        },
                    );
            }

            ui.separator();
            let required_ok = self.repository_space_required_entries_satisfied(&space.id);
            if !required_ok {
                ui.colored_label(
                    self.color_text_error(),
                    self.t("Required repositories must be added at least once"),
                );
            }

            ui.horizontal(|ui| {
                let done_btn = ui.add_enabled(required_ok, Button::new(self.t("Done")));
                if done_btn.hovered() && required_ok {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if done_btn.clicked() {
                    close = true;
                }
                let cancel_btn = ui.button(self.t("Cancel"));
                if cancel_btn.hovered() {
                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                }
                if cancel_btn.clicked() {
                    close = true;
                }
            });
        });

        if apply_path {
            let path = selector.path_buffer.trim().to_string();
            self.set_repository_space_shared_path(&space.id, path);
            selector.error = None;
        }

        if refresh_scan {
            selector.candidates = self.scan_repository_space_candidates(&space.id);
            selector.last_scan_result_count = Some(selector.candidates.len());
            if selector.candidates.is_empty() {
                self.show_success_toast(self.t("No matching existing repositories found"));
            }
        }

        if move_selected {
            let moved =
                self.apply_repository_space_scan_candidates(&space.id, &selector.candidates);
            selector.candidates = self.scan_repository_space_candidates(&space.id);
            selector.last_scan_result_count = Some(selector.candidates.len());
            selector.error = None;
            info!(
                "Moved {} repositories under repository space {}",
                moved,
                Self::repository_space_display_name(&space)
            );
            if moved > 0 {
                self.show_success_toast(self.t_fmt(
                    "Added {count} repositories to repository space.",
                    &[("count", moved.to_string())],
                ));
            } else {
                self.show_success_toast(self.t("No repositories were moved to repository space."));
            }
        }

        if let Some((entry_address, entry_name)) = add_entry_action {
            self.add_repository_from_space_entry(&space.id, &entry_address, &entry_name, ui.ctx());
            selector.error = None;
        }

        if close {
            self.repository_space_selector_state = None;
        } else {
            self.repository_space_selector_state = Some(selector);
        }
    }
}
