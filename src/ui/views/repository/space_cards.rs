use crate::ui::app::{Foxy, RepositorySpaceSelectorState};
use crate::ui::i18n::fmt_bytes;
use crate::ui::types::{RepoState, RepositorySpace, RepositorySpaceEntry};
use crate::ui::views::galley_cache;
use crate::ui::views::game_spaces::overview::StatTile;
use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Label, Layout, Margin, RichText, ScrollArea, Sense,
    Stroke, TextStyle, Ui, Vec2,
};

const ENTRY_ADD_BUTTON_SIZE: Vec2 = Vec2::new(72.0, 28.0);
const CANDIDATE_LIST_MAX_HEIGHT: f32 = 180.0;

#[derive(Default)]
pub(super) struct SpaceMatchingActions {
    pub(super) scan: bool,
    pub(super) move_selected: bool,
}

impl Foxy {
    pub(super) fn render_repository_space_stat_tiles(&self, ui: &mut Ui, space: &RepositorySpace) {
        let entry_added = |entry: &RepositorySpaceEntry| {
            self.repository_space_entry_install_count(&space.id, &entry.address) > 0
        };
        let added_count = space
            .entries
            .iter()
            .filter(|entry| entry_added(entry))
            .count();
        let required_total = space.entries.iter().filter(|entry| entry.required).count();
        let required_added = space
            .entries
            .iter()
            .filter(|entry| entry.required && entry_added(entry))
            .count();
        let updates_count = self
            .repository_view_state
            .repositories
            .iter()
            .filter(|repo| repo.repository_space_id.as_deref() == Some(space.id.as_str()))
            .filter(|repo| self.repo_state_for_repository(repo) == RepoState::PendingUpdate)
            .count();

        let mut tiles = vec![StatTile {
            value: format!("{added_count} / {}", space.entries.len()),
            label: self.t("Repositories added"),
        }];
        if required_total > 0 {
            tiles.push(StatTile {
                value: format!("{required_added} / {required_total}"),
                label: self.t("Required repositories"),
            });
        }
        tiles.push(StatTile {
            value: updates_count.to_string(),
            label: self.t("Updates available"),
        });
        tiles.push(StatTile {
            value: fmt_bytes(self.repository_space_remote_size_bytes(&space.id)),
            label: self.t("Total size"),
        });
        self.render_stat_tiles(ui, &tiles);
    }

    /// Same look as `overview_chip`, but allocated at its exact size so it stays a
    /// pill inside a centered row instead of stretching to the row height.
    fn space_entry_chip(&self, ui: &mut Ui, text: &str, color: egui::Color32) {
        let galley = ui.painter().layout_no_wrap(
            text.to_string(),
            TextStyle::Small.resolve(ui.style()),
            color,
        );
        let (rect, _) =
            ui.allocate_exact_size(galley.size() + Vec2::new(16.0, 6.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect(
                rect,
                CornerRadius::same(10),
                self.color_main_bg(),
                Stroke::new(1.0, color),
                egui::StrokeKind::Inside,
            );
            painter.galley(rect.center() - galley.size() * 0.5, galley, color);
        }
    }

    fn repository_state_color(&self, state: RepoState) -> egui::Color32 {
        match state {
            RepoState::Synced => self.color_success(),
            RepoState::PendingUpdate => self.color_warn(),
            RepoState::Updating => self.color_primary_accent(),
            RepoState::Unknown => self.color_text_dim(),
        }
    }

    pub(super) fn render_repository_space_detail_entry_card(
        &mut self,
        ui: &mut Ui,
        space: &RepositorySpace,
        entry: &RepositorySpaceEntry,
        add_entry_action: &mut Option<(String, String)>,
        jump_to_repository: &mut Option<usize>,
        detach_repo_idx: &mut Option<usize>,
    ) {
        let normalized_entry = Self::repo_url_match_key(&entry.address);
        let instances: Vec<(usize, String, RepoState)> = self
            .repository_view_state
            .repositories
            .iter()
            .enumerate()
            .filter(|(_, repo)| {
                repo.repository_space_id.as_deref() == Some(space.id.as_str())
                    && Self::repo_url_match_key(&repo.address) == normalized_entry
            })
            .map(|(idx, repo)| (idx, repo.name.clone(), self.repo_state_for_repository(repo)))
            .collect();
        let added = !instances.is_empty();
        let missing_required = entry.required && !added;
        let text_color = self.color_text_normal();
        let dim_color = self.color_text_dim();
        let stroke_color = if missing_required {
            self.color_warn()
        } else {
            self.color_widget_bg()
        };

        Frame::NONE
            .fill(self.color_main_bg())
            .stroke(Stroke::new(1.0, stroke_color))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let header_height = (ui.text_style_height(&TextStyle::Body)
                    + ui.text_style_height(&TextStyle::Small)
                    + ui.spacing().item_spacing.y)
                    .max(ENTRY_ADD_BUTTON_SIZE.y);
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), header_height),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        let add_button = Button::new(self.t("Add")).truncate();
                        let add_btn = if added {
                            ui.add_sized(ENTRY_ADD_BUTTON_SIZE, add_button)
                        } else {
                            self.add_sized_primary_button(
                                ui,
                                ENTRY_ADD_BUTTON_SIZE,
                                add_button,
                                true,
                            )
                        };
                        if add_btn.hovered() {
                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                        }
                        if add_btn.clicked() {
                            *add_entry_action = Some((entry.address.clone(), entry.name.clone()));
                        }
                        ui.add_space(8.0);
                        if !added {
                            let not_added_color = if missing_required {
                                self.color_warn()
                            } else {
                                dim_color
                            };
                            self.space_entry_chip(ui, &self.t("Not added"), not_added_color);
                        }
                        if entry.required {
                            self.space_entry_chip(
                                ui,
                                &self.t("Required"),
                                self.color_primary_accent(),
                            );
                        } else {
                            self.space_entry_chip(ui, &self.t("Optional"), dim_color);
                        }
                        ui.add_space(8.0);
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            ui.add(
                                Label::new(RichText::new(&entry.name).strong().color(text_color))
                                    .truncate(),
                            );
                            ui.add(
                                Label::new(RichText::new(&entry.address).small().color(dim_color))
                                    .truncate(),
                            )
                            .on_hover_text(&entry.address);
                        });
                    },
                );

                if instances.is_empty() {
                    return;
                }
                ui.add_space(6.0);
                Frame::NONE
                    .fill(self.color_card_bg())
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (idx, repo_name, state) in instances {
                            ui.push_id(idx, |ui| {
                                ui.allocate_ui_with_layout(
                                    Vec2::new(ui.available_width(), ui.spacing().interact_size.y),
                                    Layout::right_to_left(Align::Center),
                                    |ui| {
                                        let detach_btn = ui.small_button(self.t("Detach"));
                                        if detach_btn.hovered() {
                                            ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                        }
                                        if detach_btn.clicked() {
                                            *detach_repo_idx = Some(idx);
                                        }
                                        ui.with_layout(
                                            Layout::left_to_right(Align::Center),
                                            |ui| {
                                                let state_label =
                                                    self.repository_state_label(state);
                                                let (dot_rect, dot_response) = ui
                                                    .allocate_exact_size(
                                                        Vec2::splat(12.0),
                                                        Sense::hover(),
                                                    );
                                                ui.painter().circle_filled(
                                                    dot_rect.center(),
                                                    4.0,
                                                    self.repository_state_color(state),
                                                );
                                                dot_response.on_hover_text(&state_label);
                                                let link = ui.link(repo_name);
                                                if link.hovered() {
                                                    ui.ctx().output_mut(
                                                        Foxy::set_pointing_cursor_output,
                                                    );
                                                }
                                                if link.clicked() {
                                                    *jump_to_repository = Some(idx);
                                                }
                                                ui.add(
                                                    Label::new(
                                                        RichText::new(state_label)
                                                            .small()
                                                            .color(dim_color),
                                                    )
                                                    .truncate(),
                                                );
                                            },
                                        );
                                    },
                                );
                            });
                        }
                    });
            });
    }

    pub(super) fn render_repository_space_matching_section(
        &mut self,
        ui: &mut Ui,
        space_id: &str,
        selector_state: &mut RepositorySpaceSelectorState,
        filtered_candidate_indices: &[usize],
        has_detail_filter: bool,
    ) -> SpaceMatchingActions {
        let mut actions = SpaceMatchingActions::default();
        let dim_color = self.color_text_dim();
        self.overview_section_title(ui, &self.t("Matching existing repositories"));
        ui.label(
            RichText::new(self.t(
                "Repositories added to Foxy outside this space that use one of its addresses. Select them and move them into this space.",
            ))
            .small()
            .color(dim_color),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let scan_btn = ui.button(self.t("Scan existing repositories"));
            if scan_btn.hovered() {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            actions.scan = scan_btn.clicked();

            let any_checked = selector_state
                .candidates
                .iter()
                .any(|candidate| candidate.checked);
            let move_btn = self.add_sized_primary_button(
                ui,
                Vec2::ZERO,
                Button::new(self.t("Move selected repositories")),
                any_checked,
            );
            if move_btn.hovered() && any_checked {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
            actions.move_selected = move_btn.clicked();
        });

        if let Some(scan_count) = selector_state.last_scan_result_count {
            let scan_text = if scan_count == 0 {
                self.t("No unassigned repositories found matching this space's entries.")
            } else {
                self.t_fmt(
                    "Scan complete: found {count} matching repositories.",
                    &[("count", scan_count.to_string())],
                )
            };
            ui.label(RichText::new(scan_text).small().color(dim_color));
        }
        if has_detail_filter && !selector_state.candidates.is_empty() {
            ui.label(
                RichText::new(self.t_fmt(
                    "Showing {shown} / {total} repositories",
                    &[
                        ("shown", filtered_candidate_indices.len().to_string()),
                        ("total", selector_state.candidates.len().to_string()),
                    ],
                ))
                .small()
                .color(dim_color),
            );
        }
        ui.add_space(6.0);

        if filtered_candidate_indices.is_empty() {
            if selector_state.last_scan_result_count != Some(0) || has_detail_filter {
                ui.label(
                    RichText::new(self.t("No matching existing repositories found"))
                        .italics()
                        .color(dim_color),
                );
            }
        } else {
            self.space_detail_candidate_galleys.ensure(
                filtered_candidate_indices.len(),
                1,
                galley_cache::fingerprint((
                    space_id,
                    self.repository_space_detail_filter.as_str(),
                    filtered_candidate_indices
                        .iter()
                        .filter_map(|candidate_idx| {
                            let candidate = selector_state.candidates.get(*candidate_idx)?;
                            self.repository_view_state
                                .repositories
                                .get(candidate.repo_index)
                                .map(|repo| {
                                    (
                                        *candidate_idx,
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
                .id_salt(("space_detail_scan_candidates", space_id))
                .max_height(CANDIDATE_LIST_MAX_HEIGHT)
                .auto_shrink([false, true])
                .show_rows(
                    ui,
                    Self::repository_space_candidate_row_height(),
                    filtered_candidate_indices.len(),
                    |ui, row_range| {
                        ui.set_min_width(ui.available_width());
                        for filtered_idx in row_range {
                            let candidate_idx = filtered_candidate_indices[filtered_idx];
                            ui.push_id(candidate_idx, |ui| {
                                let candidate = &mut selector_state.candidates[candidate_idx];
                                self.render_repository_space_candidate_row(
                                    ui,
                                    filtered_idx,
                                    true,
                                    candidate,
                                );
                            });
                        }
                    },
                );
        }

        if let Some(error) = &selector_state.error {
            ui.colored_label(self.color_text_error(), error);
        }
        actions
    }

    pub(super) fn render_repository_space_selector_entry_card(
        &mut self,
        ui: &mut Ui,
        row_slot: usize,
        space: &RepositorySpace,
        entry: &RepositorySpaceEntry,
        add_entry_action: &mut Option<(String, String)>,
    ) {
        let installed_count = self.repository_space_entry_install_count(&space.id, &entry.address);
        let marker = if entry.required {
            self.t("Required")
        } else {
            self.t("Optional")
        };
        let name_with_address = self.t_fmt(
            "{name} ({address})",
            &[
                ("name", entry.name.clone()),
                ("address", entry.address.clone()),
            ],
        );
        let state_text = self.t_fmt(
            "{required} - installed: {count}",
            &[("required", marker), ("count", installed_count.to_string())],
        );
        Frame::NONE
            .fill(self.color_card_bg())
            .stroke(egui::Stroke::new(1.0, self.color_widget_bg()))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::symmetric(10, 4))
            .show(ui, |ui| {
                let add_button_size = Vec2::new(60.0, 24.0);
                let add_button_gap = 8.0;
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    let add_btn =
                        ui.add_sized(add_button_size, Button::new(self.t("Add")).truncate());
                    if add_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if add_btn.clicked() {
                        *add_entry_action = Some((entry.address.clone(), entry.name.clone()));
                    }

                    ui.add_space(add_button_gap);

                    let content_width = ui.available_width().max(0.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(content_width, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            let name_text_color = self.color_text_normal();
                            let name_galley = galley_cache::lazy_galley_colored(
                                ui,
                                self.space_selector_entry_galleys.slot(row_slot, 0),
                                egui::TextStyle::Button.resolve(ui.style()),
                                name_text_color,
                                || name_with_address,
                            );
                            ui.add(Label::new(name_galley).truncate());
                            ui.add_space(4.0);
                            let state_text_color = if entry.required {
                                self.color_primary_accent()
                            } else {
                                self.color_text_dim()
                            };
                            let state_galley = galley_cache::lazy_galley_colored(
                                ui,
                                self.space_selector_entry_galleys.slot(row_slot, 1),
                                egui::TextStyle::Body.resolve(ui.style()),
                                state_text_color,
                                || state_text,
                            );
                            Frame::NONE
                                .fill(self.color_widget_bg())
                                .stroke(egui::Stroke::new(1.0, self.color_main_bg()))
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(Margin::symmetric(8, 4))
                                .show(ui, |ui| {
                                    ui.label(state_galley);
                                });
                        },
                    );
                });
            });
    }

    pub(super) fn render_repository_space_candidate_row(
        &mut self,
        ui: &mut Ui,
        row_slot: usize,
        detail_cache: bool,
        candidate: &mut crate::ui::app::RepositorySpaceScanCandidate,
    ) {
        if let Some(repo) = self
            .repository_view_state
            .repositories
            .get(candidate.repo_index)
        {
            let label = self.t_fmt(
                "{name} ({address})",
                &[
                    ("name", repo.name.clone()),
                    ("address", repo.address.clone()),
                ],
            );
            let text_color = if candidate.checked {
                self.color_text_normal()
            } else {
                self.color_text_dim()
            };
            let cache = if detail_cache {
                &mut self.space_detail_candidate_galleys
            } else {
                &mut self.space_selector_candidate_galleys
            };
            let galley = galley_cache::lazy_galley_colored(
                ui,
                cache.slot(row_slot, 0),
                egui::TextStyle::Button.resolve(ui.style()),
                text_color,
                || label,
            );
            let mut hovered = false;
            ui.horizontal(|ui| {
                let check = Self::ui_state_checkbox(ui, &mut candidate.checked, "");
                hovered |= check.hovered();
                let label = ui.add(Label::new(galley).sense(Sense::click()).truncate());
                if label.clicked() {
                    candidate.checked = !candidate.checked;
                }
                hovered |= label.hovered();
            });
            if hovered {
                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
            }
        }
    }
}
