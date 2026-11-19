use crate::core::server::ServerInstanceId;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerPage;
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

impl Foxy {
    pub(super) fn render_server_sidebar(&mut self, ui: &mut Ui, compact: bool) {
        let busy = self.server_view.busy() || self.pending_game_space_switch.is_some();
        let dirty = self.server_view.dirty();
        let can_select =
            !dirty && !self.server_view.controls_busy() && self.pending_game_space_switch.is_none();
        if !can_select {
            self.server_view.pending_sidebar_selection = None;
        } else if !busy && let Some(id) = self.server_view.pending_sidebar_selection.take() {
            self.select_server_from_sidebar(&id);
        }
        if compact {
            ui.horizontal_wrapped(|ui| {
                ui.label(crate::core::game::spaces::active_game_space().display_name);
                if ui
                    .button(tr("Switch"))
                    .on_hover_text(tr("Switch game space"))
                    .clicked()
                {
                    self.open_game_spaces_view();
                }
                self.render_server_add_button(ui, !busy && !dirty, compact);
                self.render_server_reload_button(ui, !busy && !dirty);
            });
        } else {
            self.render_game_space_header(ui);
            self.render_server_add_button(ui, !busy && !dirty, compact);
        }
        let label = tr("Filter servers");
        ui.add(
            egui::TextEdit::singleline(&mut self.server_view.instance_filter)
                .hint_text(&label)
                .desired_width(ui.available_width()),
        )
        .accessible_name(label);
        if !compact {
            self.render_server_reload_button(ui, !busy && !dirty);
        }

        let search = self.server_view.instance_filter.trim().to_lowercase();
        let rows = self
            .server_view
            .catalog
            .as_ref()
            .map(|catalog| {
                catalog
                    .instances
                    .iter()
                    .filter(|instance| {
                        search.is_empty()
                            || instance.display_name.to_lowercase().contains(&search)
                            || instance.id.as_str().contains(&search)
                    })
                    .map(|instance| (instance.id.clone(), instance.display_name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if rows.is_empty() {
            let label = if !self.server_view.loaded {
                "Reading or saving server drafts..."
            } else if self
                .server_view
                .catalog
                .as_ref()
                .is_some_and(|catalog| !catalog.instances.is_empty())
            {
                "No servers match the filter."
            } else {
                "No server drafts in this game space."
            };
            ui.label(tr(label));
            return;
        }

        let mut selected = None;
        let mut focus_target = None;
        let mut responses = Vec::with_capacity(rows.len());
        let mut area = egui::ScrollArea::vertical().id_salt("server-instance-list");
        if compact {
            area = area.max_height(68.0);
        }
        area.show(ui, |ui| {
            ui.add_enabled_ui(can_select, |ui| {
                for (index, (id, name)) in rows.iter().enumerate() {
                    let current = self.server_view.selected.as_ref() == Some(id);
                    let response = ui
                        .push_id(("server-row", id.as_str()), |ui| {
                            let phase = self
                                .observed_server_status(id)
                                .map(|status| super::lifecycle::runtime_phase_label(status.phase))
                                .unwrap_or_else(|| tr("Unknown"));
                            let response = ui
                                .add(
                                    egui::Button::selectable(current, format!("{name}\n{phase}"))
                                        .min_size(egui::vec2(ui.available_width(), 34.0))
                                        .wrap(),
                                )
                                .on_hover_text(id.as_str())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            Self::focus_outline(ui, &response);
                            response
                        })
                        .inner;
                    if response.clicked() {
                        response.request_focus();
                        selected = Some(index);
                    }
                    if response.enabled() && ui.memory(|memory| memory.has_focus(response.id)) {
                        ui.memory_mut(|memory| {
                            memory.set_focus_lock_filter(
                                response.id,
                                egui::EventFilter {
                                    vertical_arrows: true,
                                    ..Default::default()
                                },
                            )
                        });
                        let offset = ui.input_mut(|input| {
                            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                                1
                            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                                -1
                            } else {
                                0
                            }
                        });
                        if offset != 0 {
                            let index = index.saturating_add_signed(offset).min(rows.len() - 1);
                            selected = Some(index);
                            focus_target = Some(index);
                        }
                    }
                    responses.push(response);
                }
            });
        });
        if let Some(index) = selected {
            let id = &rows[index].0;
            if busy {
                // The passive worker finishes against its captured instance before selection changes.
                self.server_view.pending_sidebar_selection = Some(id.clone());
            } else {
                self.select_server_from_sidebar(id);
            }
        }
        if let Some(index) = focus_target {
            let response = &responses[index];
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
            response.request_focus();
            response.scroll_to_me(None);
            ui.ctx().request_repaint();
        }
    }

    fn select_server_from_sidebar(&mut self, id: &ServerInstanceId) {
        if (self.server_view.selected.as_ref() == Some(id) || self.server_view.select(id))
            && self.server_view.page == ServerPage::Add
        {
            self.server_view.page = ServerPage::Overview;
        }
    }

    fn render_server_add_button(&mut self, ui: &mut Ui, editable: bool, compact: bool) {
        let mut button = egui::Button::new(tr("Add server"));
        if !compact {
            let font = self
                .settings_view_state
                .font_sizes
                .repository_view
                .add_repository_button as f32;
            button = egui::Button::new(egui::RichText::new(tr("Add server")).size(font))
                .min_size(egui::vec2(
                    ui.available_width(),
                    Self::adaptive_button_height(font, 50.0),
                ))
                .wrap();
        }
        let add = ui.add_enabled(editable, button);
        Self::focus_outline(ui, &add);
        if self.server_view.pending_focus == Some("server-add-trigger") && editable {
            add.request_focus();
            self.server_view.pending_focus = None;
        }
        if add.clicked() {
            self.server_view.page = ServerPage::Add;
            self.server_view.focus_add_name = true;
        }
    }

    fn render_server_reload_button(&mut self, ui: &mut Ui, editable: bool) {
        let reload = ui.add_enabled(editable, egui::Button::new(tr("Reload drafts")));
        Self::focus_outline(ui, &reload);
        if reload.clicked() {
            self.start_server_draft_task(None);
        }
    }
}
