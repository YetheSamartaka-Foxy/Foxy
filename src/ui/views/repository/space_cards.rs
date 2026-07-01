use crate::ui::app::Foxy;
use crate::ui::types::{RepositorySpace, RepositorySpaceEntry};
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, Button, CornerRadius, Frame, Label, Layout, Margin, RichText, Sense, Ui, Vec2,
};

impl Foxy {
    pub(super) fn render_repository_space_detail_entry_card(
        &mut self,
        ui: &mut Ui,
        space: &RepositorySpace,
        entry: &RepositorySpaceEntry,
        add_entry_action: &mut Option<(String, String)>,
        jump_to_repository: &mut Option<usize>,
        detach_repo_idx: &mut Option<usize>,
    ) {
        let installed_count = self.repository_space_entry_install_count(&space.id, &entry.address);
        let name_with_address = self.t_fmt(
            "{name} ({address})",
            &[
                ("name", entry.name.clone()),
                ("address", entry.address.clone()),
            ],
        );

        let card_width = ui.available_width();
        ui.allocate_ui_with_layout(
            Vec2::new(card_width, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_width(card_width);
                Frame::NONE
                    .fill(self.color_card_bg())
                    .stroke(egui::Stroke::new(1.0, self.color_widget_bg()))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let add_button_size = Vec2::new(60.0, 24.0);
                        let add_button_gap = 8.0;
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            let add_btn = ui
                                .add_sized(add_button_size, Button::new(self.t("Add")).truncate());
                            if add_btn.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if add_btn.clicked() {
                                *add_entry_action =
                                    Some((entry.address.clone(), entry.name.clone()));
                            }

                            ui.add_space(add_button_gap);

                            let content_width = ui.available_width().max(0.0);
                            ui.allocate_ui_with_layout(
                                Vec2::new(content_width, 0.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.add(
                                        Label::new(RichText::new(&name_with_address).strong())
                                            .truncate(),
                                    );
                                    if installed_count > 0 {
                                        ui.add_space(4.0);
                                        Frame::NONE
                                            .fill(self.color_main_bg())
                                            .stroke(egui::Stroke::new(1.0, self.color_widget_bg()))
                                            .corner_radius(CornerRadius::same(6))
                                            .inner_margin(Margin::symmetric(8, 4))
                                            .show(ui, |ui| {
                                                ui.horizontal_wrapped(|ui| {
                                                    let installed_label = format!(
                                                        "{} ({})",
                                                        self.t("Installed in app:"),
                                                        installed_count
                                                    );
                                                    ui.colored_label(
                                                        self.color_text_dim(),
                                                        installed_label,
                                                    );
                                                    let normalized_entry =
                                                        Self::normalize_repo_url(&entry.address);
                                                    let installed_repos: Vec<(usize, String)> =
                                                        self.repository_view_state
                                                            .repositories
                                                            .iter()
                                                            .enumerate()
                                                            .filter(|(_, repo)| {
                                                                repo.repository_space_id.as_deref()
                                                                    == Some(&space.id)
                                                                    && Self::normalize_repo_url(
                                                                        &repo.address,
                                                                    ) == normalized_entry
                                                            })
                                                            .map(|(idx, repo)| {
                                                                (idx, repo.name.clone())
                                                            })
                                                            .collect();
                                                    for (idx, repo_name) in installed_repos {
                                                        ui.push_id(idx, |ui| {
                                                            let link = ui.link(repo_name);
                                                            if link.hovered() {
                                                                ui.ctx().output_mut(
                                                                    Foxy::set_pointing_cursor_output,
                                                                );
                                                            }
                                                            if link.clicked() {
                                                                *jump_to_repository = Some(idx);
                                                            }

                                                            let detach_btn =
                                                                ui.small_button(self.t("Detach"));
                                                            if detach_btn.hovered() {
                                                                ui.ctx().output_mut(
                                                                    Foxy::set_pointing_cursor_output,
                                                                );
                                                            }
                                                            if detach_btn.clicked() {
                                                                *detach_repo_idx = Some(idx);
                                                            }
                                                        });
                                                    }
                                                });
                                            });
                                    }
                                },
                            );
                        });
                    });
            },
        );
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
