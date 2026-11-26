use crate::core::server::ServerInstanceId;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerPage;
use crate::ui::i18n::{is_rtl, tr};
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
            });
        } else {
            self.render_game_space_header(ui);
            self.render_server_add_button(ui, !busy && !dirty, compact);
        }
        ui.add_space(4.0);
        let reload_size = Self::toolbar_icon_button_size(16.0);
        let label = tr("Filter servers");
        if is_rtl() {
            ui.horizontal(|ui| {
                self.render_server_reload_button(ui, !busy && !dirty, reload_size);
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_view.instance_filter)
                        .hint_text(&label)
                        .desired_width(ui.available_width()),
                )
                .accessible_name(label);
            });
        } else {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_view.instance_filter)
                        .hint_text(&label)
                        .desired_width(
                            ui.available_width() - reload_size.x - ui.spacing().item_spacing.x,
                        ),
                )
                .accessible_name(label);
                self.render_server_reload_button(ui, !busy && !dirty, reload_size);
            });
        }
        ui.add_space(4.0);

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
            self.server_note(ui, tr(label));
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
                            ui.spacing_mut().button_padding = egui::vec2(10.0, 4.0);
                            let (phase, tone) = self.server_runtime_badge(id);
                            let mut job = egui::text::LayoutJob::default();
                            let body = egui::TextStyle::Body.resolve(ui.style());
                            job.append(
                                name,
                                0.0,
                                egui::TextFormat::simple(body.clone(), self.color_text_normal()),
                            );
                            job.append(
                                &format!("\n{phase}"),
                                0.0,
                                egui::TextFormat::simple(
                                    egui::FontId::new(body.size * 0.86, body.family),
                                    self.color_text_dim(),
                                ),
                            );
                            let response = ui
                                .add(
                                    egui::Button::selectable(current, egui::WidgetText::from(job))
                                        .min_size(egui::vec2(ui.available_width(), 42.0))
                                        .wrap(),
                                )
                                .on_hover_text(id.as_str())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            let edge = if is_rtl() {
                                egui::Rect::from_min_max(
                                    response.rect.right_top() - egui::vec2(3.0, 0.0),
                                    response.rect.right_bottom(),
                                )
                            } else {
                                egui::Rect::from_min_max(
                                    response.rect.left_top(),
                                    response.rect.left_bottom() + egui::vec2(3.0, 0.0),
                                )
                            };
                            ui.painter().rect_filled(
                                edge,
                                egui::CornerRadius::same(1),
                                self.server_tone_color(tone),
                            );
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

    fn render_server_reload_button(&mut self, ui: &mut Ui, editable: bool, size: egui::Vec2) {
        let label = tr("Reload drafts");
        let reload = ui
            .add_enabled(
                editable,
                egui::Button::new(egui::RichText::new("\u{21bb}").size(16.0)).min_size(size),
            )
            .accessible_name(&label)
            .on_hover_text(&label)
            .on_disabled_hover_text(&label);
        Self::focus_outline(ui, &reload);
        if reload.clicked() {
            self.start_server_draft_task(None);
        }
    }
}
