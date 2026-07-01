use super::RepositoryServerContextAction;
use crate::ui::app::Foxy;
use crate::ui::context_menu::{ContextMenuItem, attach_context_menu};
use crate::ui::i18n::{tr, tr_fmt};
use crate::ui::types::{RepositorySelection, ServerOnlineStatus};
use crate::ui::views::galley_cache;
use eframe::egui::{self, Align2, Button, CornerRadius, RichText, Sense, TextStyle, Ui, Vec2};
use log::info;
use std::time::{Duration, Instant};

const SERVER_ROW_INNER_HEIGHT: f32 = 42.0;
const SERVER_ROW_SPACING: f32 = 6.0;
const SERVER_ROW_HEIGHT: f32 = SERVER_ROW_INNER_HEIGHT + SERVER_ROW_SPACING;

impl Foxy {
    pub(super) fn repository_server_min_section_height(&self, ui: &Ui, selected_idx: usize) -> f32 {
        let server_count = self.repository_view_state.repositories[selected_idx]
            .servers
            .len();
        let heading_height =
            ui.text_style_height(&TextStyle::Heading) + ui.spacing().item_spacing.y;

        if server_count == 0 {
            return heading_height + ui.text_style_height(&TextStyle::Body);
        }

        heading_height + server_count.min(2) as f32 * SERVER_ROW_HEIGHT
    }

    pub(super) fn repository_server_full_section_height(
        &self,
        ui: &Ui,
        selected_idx: usize,
    ) -> f32 {
        let server_count = self.repository_view_state.repositories[selected_idx]
            .servers
            .len();
        let heading_height =
            ui.text_style_height(&TextStyle::Heading) + ui.spacing().item_spacing.y;

        if server_count == 0 {
            return heading_height + ui.text_style_height(&TextStyle::Body);
        }

        heading_height + server_count as f32 * SERVER_ROW_HEIGHT
    }

    pub(super) fn repository_launch_join_area_height(&self) -> f32 {
        let launch_join_font_size = self
            .settings_view_state
            .font_sizes
            .repository_view
            .launch_join_buttons as f32;
        12.0 + Self::adaptive_button_height(launch_join_font_size, 50.0) + 4.0
    }

    /// Render the "Servers" section in the repository detail view (cards only, no launch buttons).
    pub(super) fn render_repository_servers_section(
        &mut self,
        ui: &mut Ui,
        selected_idx: usize,
        repo_name: &str,
        max_section_height: Option<f32>,
    ) {
        let section_start_y = ui.cursor().min.y;
        ui.heading(tr("Servers"));

        let repo = &self.repository_view_state.repositories[selected_idx];
        let effective = {
            let snapshot_started_at = Instant::now();
            let eff = Self::build_effective_repository_snapshot(repo);
            let snapshot_elapsed = snapshot_started_at.elapsed();
            if snapshot_elapsed > Duration::from_millis(2) {
                info!(
                    "Building effective repository snapshot took {:.2?} for {}",
                    snapshot_elapsed, repo_name
                );
            }
            eff
        };

        let servers = repo.servers.clone();
        let statuses: Vec<ServerOnlineStatus> =
            servers.iter().map(|s| self.get_server_status(s)).collect();
        let mut new_selection = self.repository_selection.clone();
        let join_preflight_active =
            self.pending_join_preflight.is_some() || self.pending_join_preflight_query.is_some();

        // Cache each server name's shaped galley: this list culls off-screen
        // rows with `is_rect_visible`, so without a cache scrolling re-shapes
        // every newly revealed name. Keyed on the server names (rebuilds when the
        // list changes); the short, locale/player-dependent status badge stays a
        // live `painter.text`.
        let name_font = TextStyle::Body.resolve(ui.style());
        let name_text_color = self.color_text_normal();
        let servers_generation = galley_cache::fingerprint((
            selected_idx,
            servers.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ));
        self.server_row_galleys.ensure(
            servers.len(),
            1,
            servers_generation,
            galley_cache::fingerprint(name_font.size.to_bits()),
        );

        if servers.is_empty() {
            ui.label(tr("No servers found for this repository."));
        } else {
            let section_header_height = ui.cursor().min.y - section_start_y;
            let content_height = servers.len() as f32 * SERVER_ROW_HEIGHT;
            let list_height_budget = max_section_height
                .map(|height| (height - section_header_height).max(SERVER_ROW_INNER_HEIGHT))
                .unwrap_or(content_height);
            let list_height = content_height.min(list_height_budget);

            egui::ScrollArea::vertical()
                .id_salt(("repository_servers_list", selected_idx))
                .max_height(list_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for (i, (server, status)) in servers.iter().zip(&statuses).enumerate() {
                        let is_selected = matches!(
                            &self.repository_selection,
                            Some(RepositorySelection::Server(idx)) if *idx == i
                        );
                        let refreshing = self.is_server_refresh_indicator_active(server);
                        let (
                            status_text,
                            row_fill,
                            row_stroke,
                            badge_fill,
                            badge_stroke,
                            badge_width,
                            clickable,
                        ) = match status {
                            ServerOnlineStatus::Offline => (
                                tr("OFFLINE"),
                                self.color_server_offline_bg(),
                                self.color_text_gray(),
                                self.color_error(),
                                self.color_text_error(),
                                112.0,
                                false,
                            ),
                            ServerOnlineStatus::Online { players } => (
                                tr_fmt(
                                    "ONLINE: {players} players",
                                    &[("players", players.to_string())],
                                ),
                                self.color_widget_bg(),
                                self.color_text_gray(),
                                self.color_checkbox_enabled(),
                                self.color_widget_bg(),
                                172.0,
                                true,
                            ),
                        };

                        let row_size = Vec2::new(ui.available_width(), SERVER_ROW_INNER_HEIGHT);
                        let (rect, response) = ui.allocate_exact_size(row_size, Sense::click());
                        let row_fill = if is_selected {
                            if response.hovered() && clickable {
                                self.color_server_selected_bg_hover()
                            } else {
                                self.color_server_selected_bg()
                            }
                        } else if response.hovered() && clickable {
                            self.color_widget_bg_active()
                        } else {
                            row_fill
                        };
                        let row_stroke = if is_selected {
                            self.color_server_selected_stroke()
                        } else if response.hovered() && clickable {
                            self.color_primary_accent_hover()
                        } else {
                            row_stroke
                        };

                        if ui.is_rect_visible(rect) {
                            let painter = ui.painter();
                            let row_corner = CornerRadius::same(4);
                            let row_stroke_width = if is_selected { 2.5 } else { 1.0 };
                            painter.rect_filled(rect, row_corner, row_fill);
                            painter.rect_stroke(
                                rect,
                                row_corner,
                                egui::Stroke::new(row_stroke_width, row_stroke),
                                egui::StrokeKind::Inside,
                            );

                            let horizontal_padding = 14.0;
                            let badge_rect = egui::Rect::from_center_size(
                                egui::pos2(
                                    rect.right() - horizontal_padding - (badge_width * 0.5),
                                    rect.center().y,
                                ),
                                Vec2::new(badge_width, 28.0),
                            );
                            painter.rect_filled(badge_rect, CornerRadius::same(6), badge_fill);
                            painter.rect_stroke(
                                badge_rect,
                                CornerRadius::same(6),
                                egui::Stroke::new(1.0, badge_stroke),
                                egui::StrokeKind::Inside,
                            );

                        let spinner_rect = refreshing.then(|| {
                            egui::Rect::from_center_size(
                                egui::pos2(badge_rect.left() - 22.0, rect.center().y),
                                Vec2::splat(16.0),
                            )
                        });
                        let text_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + horizontal_padding, rect.top()),
                            egui::pos2(
                                spinner_rect
                                    .map(|spinner| spinner.left() - 12.0)
                                    .unwrap_or_else(|| badge_rect.left() - 14.0),
                                rect.bottom(),
                            ),
                        );
                        painter.text(
                            badge_rect.center(),
                            Align2::CENTER_CENTER,
                            &status_text,
                            TextStyle::Button.resolve(ui.style()),
                            name_text_color,
                        );
                        let name_galley = galley_cache::lazy_galley(
                            ui,
                            self.server_row_galleys.slot(i, 0),
                            name_font.clone(),
                            || server.name.clone(),
                        );
                        galley_cache::paint_anchored(
                            ui,
                            egui::pos2(text_rect.left(), rect.center().y),
                            Align2::LEFT_CENTER,
                            name_galley,
                            name_text_color,
                            Some(text_rect),
                        );
                        if let Some(spinner_rect) = spinner_rect {
                            ui.ctx().request_repaint_after(Duration::from_millis(16));
                            egui::Spinner::new().size(16.0).paint_at(ui, spinner_rect);
                        }
                    }

                    if response.clicked() && clickable {
                        new_selection = Some(RepositorySelection::Server(i));
                        self.force_refresh_server_status(server);
                    }
                    if response.hovered() && clickable {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }

                    let mut context_items = vec![ContextMenuItem::new(
                        RepositoryServerContextAction::RefreshStatus,
                        tr("Refresh server status"),
                    )];
                    if matches!(status, ServerOnlineStatus::Online { .. })
                        && !join_preflight_active
                    {
                        context_items.push(
                            ContextMenuItem::new(RepositoryServerContextAction::Join, tr("Join"))
                                .separator_before(),
                        );
                    }

                    let mut context_action = None;
                    attach_context_menu(&response, &context_items, &mut context_action);
                    match context_action {
                        Some(RepositoryServerContextAction::RefreshStatus) => {
                            new_selection = Some(RepositorySelection::Server(i));
                            info!(
                                "Manual server status refresh requested for {}:{} in repository {}",
                                server.address, server.port, repo_name
                            );
                            self.force_refresh_server_status(server);
                        }
                        Some(RepositoryServerContextAction::Join) => {
                            new_selection = Some(RepositorySelection::Server(i));
                            self.try_join_repository_server(
                                ui.ctx(),
                                &effective,
                                server,
                                repo_name,
                            );
                        }
                        None => {}
                    }
                    ui.add_space(SERVER_ROW_SPACING);
                    }
                });
        }

        self.repository_selection = new_selection;
    }

    /// Render the context-sensitive Launch/Join/Launch Editor buttons at the bottom.
    pub(super) fn render_launch_join_buttons(
        &mut self,
        ui: &mut Ui,
        selected_idx: usize,
        repo_name: &str,
    ) {
        let repo = &self.repository_view_state.repositories[selected_idx];
        let effective = Self::build_effective_repository_snapshot(repo);
        let servers = repo.servers.clone();
        let join_preflight_active =
            self.pending_join_preflight.is_some() || self.pending_join_preflight_query.is_some();

        ui.add_space(12.0);

        ui.scope_builder(
            egui::UiBuilder::new().id(egui::Id::new("launch_join_area")),
            |ui| {
                ui.horizontal(|ui| {
                    let launch_join_font_size = self
                        .settings_view_state
                        .font_sizes
                        .repository_view
                        .launch_join_buttons as f32;
                    let button_width =
                        ((ui.available_width() - ui.spacing().item_spacing.x) * 0.5).max(0.0);
                    let bs = Vec2::new(
                        button_width,
                        Self::adaptive_button_height(launch_join_font_size, 50.0),
                    );

                    // Left button: "Launch" (always available)
                    let launch_button = Button::new(
                        RichText::new(tr("Launch"))
                            .size(launch_join_font_size)
                            .color(if join_preflight_active {
                                self.color_text_gray()
                            } else {
                                self.color_text_normal()
                            }),
                    );
                    let launch_btn = if join_preflight_active {
                        ui.add_enabled(
                            false,
                            launch_button.fill(self.color_widget_bg()).min_size(bs),
                        )
                    } else {
                        self.add_sized_primary_button(ui, bs, launch_button, true)
                    };
                    if launch_btn.clicked() {
                        self.present_launch_preflight(ui.ctx(), &effective, repo_name);
                    }
                    if launch_btn.hovered() {
                        if join_preflight_active {
                            ui.ctx()
                                .output_mut(|o| o.cursor_icon = egui::CursorIcon::NotAllowed);
                        } else {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                    }

                    // Right button: context-sensitive
                    let current_selection = self.repository_selection.clone();
                    match &current_selection {
                        Some(RepositorySelection::Server(idx)) => {
                            let join_ok = *idx < servers.len() && !join_preflight_active;
                            let join_fg = if join_ok {
                                self.color_text_normal()
                            } else {
                                self.color_text_gray()
                            };
                            let join_button = Button::new(
                                RichText::new(tr("Join"))
                                    .size(launch_join_font_size)
                                    .color(join_fg),
                            );
                            let join_btn = if join_ok {
                                self.add_sized_primary_button(ui, bs, join_button, true)
                            } else {
                                ui.add_enabled(
                                    false,
                                    join_button.fill(self.color_widget_bg()).min_size(bs),
                                )
                            };
                            if join_btn.clicked() && join_ok {
                                let srv = &servers[*idx];
                                self.try_join_repository_server(
                                    ui.ctx(),
                                    &effective,
                                    srv,
                                    repo_name,
                                );
                            }
                            if join_btn.hovered() {
                                if join_ok {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                } else {
                                    ui.ctx().output_mut(|o| {
                                        o.cursor_icon = egui::CursorIcon::NotAllowed
                                    });
                                }
                            }
                        }

                        Some(RepositorySelection::Mission(idx)) => {
                            let profile_name = self
                                .resolve_arma3_profile_for_repo(selected_idx)
                                .unwrap_or_default();
                            let missions = self.get_or_scan_missions(&profile_name);
                            let valid = *idx < missions.len();

                            let btn_fg = if valid {
                                self.color_text_normal()
                            } else {
                                self.color_text_gray()
                            };

                            let editor_button = Button::new(
                                RichText::new(tr("Launch Editor"))
                                    .size(launch_join_font_size)
                                    .color(btn_fg),
                            );
                            let editor_btn = if valid {
                                self.add_sized_primary_button(ui, bs, editor_button, true)
                            } else {
                                ui.add_enabled(
                                    false,
                                    editor_button.fill(self.color_widget_bg()).min_size(bs),
                                )
                            };
                            if editor_btn.clicked() && valid {
                                let mission = missions[*idx].clone();
                                self.request_editor_mission_launch(
                                    ui.ctx(),
                                    &effective,
                                    &mission,
                                    selected_idx,
                                    repo_name,
                                );
                            }
                            if editor_btn.hovered() {
                                if valid {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                } else {
                                    ui.ctx().output_mut(|o| {
                                        o.cursor_icon = egui::CursorIcon::NotAllowed
                                    });
                                }
                            }
                        }

                        None => {
                            // No selection - show greyed out "Join" button
                            let join_btn = ui.add_enabled(
                                false,
                                Button::new(
                                    RichText::new(tr("Join"))
                                        .size(launch_join_font_size)
                                        .color(self.color_text_gray()),
                                )
                                .fill(self.color_widget_bg())
                                .min_size(bs),
                            );
                            if join_btn.hovered() {
                                ui.ctx()
                                    .output_mut(|o| o.cursor_icon = egui::CursorIcon::NotAllowed);
                            }
                        }
                    }
                });
            },
        );
        ui.add_space(4.0);
    }
}
