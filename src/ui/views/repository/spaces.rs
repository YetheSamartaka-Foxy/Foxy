use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::i18n::fmt_bytes;
use crate::ui::search_filter::MultiEntryFilter;
use crate::ui::types::{FoxyView, RepositorySpaceBulkMode};
use crate::ui::views::galley_cache;
use eframe::egui::{
    self, Align, CursorIcon, Layout, Margin, RichText, ScrollArea, TextEdit, TextStyle, Ui,
};
use log::info;

use super::RepositoryBannerResponse;

impl Foxy {
    pub(super) fn render_repository_space_detail(&mut self, ui: &mut Ui, space_id: &str) {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
            .cloned()
        else {
            self.selected_repository_space_id = None;
            ui.vertical_centered_justified(|ui| {
                ui.heading(self.t("Selected repository space"));
                ui.label(self.t("No repository space selected"));
            });
            return;
        };

        let mut selector_state = self
            .repository_space_selector_state
            .clone()
            .filter(|state| state.space_id == space.id)
            .unwrap_or_else(|| {
                let candidates = self.scan_repository_space_candidates(&space.id);
                crate::ui::app::RepositorySpaceSelectorState {
                    space_id: space.id.clone(),
                    path_buffer: space.shared_path.clone(),
                    candidates,
                    last_scan_result_count: None,
                    error: None,
                }
            });
        if self.repository_space_detail_filter_space_id.as_deref() != Some(space.id.as_str()) {
            self.repository_space_detail_filter.clear();
            self.repository_space_detail_filter_space_id = Some(space.id.clone());
        }
        let detail_filter = MultiEntryFilter::parse(&self.repository_space_detail_filter);
        let has_detail_filter = !detail_filter.is_empty();
        let filtered_entry_indices: Vec<usize> = if has_detail_filter {
            space
                .entries
                .iter()
                .enumerate()
                .filter_map(|(entry_idx, entry)| {
                    // Entries are repositories already attached to this space;
                    // they install into the space's shared download folder.
                    let installed_tag =
                        self.repo_installed_state_tag(&entry.address, &space.shared_path);
                    detail_filter
                        .matches_with_tags(
                            &[entry.name.as_str(), entry.address.as_str()],
                            &[
                                installed_tag,
                                crate::ui::search_filter::STATE_KEYWORD_ATTACHED,
                            ],
                        )
                        .then_some(entry_idx)
                })
                .collect()
        } else {
            (0..space.entries.len()).collect()
        };
        let filtered_candidate_indices: Vec<usize> = if has_detail_filter {
            selector_state
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(candidate_idx, candidate)| {
                    let repo = self
                        .repository_view_state
                        .repositories
                        .get(candidate.repo_index)?;
                    // Candidates are repositories not yet attached to this space.
                    let installed_tag = self.repo_installed_state_tag(&repo.address, &repo.path);
                    detail_filter
                        .matches_with_tags(
                            &[repo.name.as_str(), repo.address.as_str()],
                            &[
                                installed_tag,
                                crate::ui::search_filter::STATE_KEYWORD_DETACHED,
                            ],
                        )
                        .then_some(candidate_idx)
                })
                .collect()
        } else {
            (0..selector_state.candidates.len()).collect()
        };
        let mut open_settings = false;
        let mut add_entry_action: Option<(String, String)> = None;
        let mut jump_to_repository: Option<usize> = None;
        let mut detach_repo_idx: Option<usize> = None;
        let mut refresh_scan = false;
        let mut move_selected = false;
        let mut quick_check_all_in_space = false;
        let mut open_recheck_all_modal = false;
        let mut open_update_all_modal = false;
        let mut dismiss_completed_bulk_progress = false;
        self.ensure_repository_addon_size_cache_loaded();

        ScrollArea::vertical()
            .id_salt(("space_detail_page", space_id))
            .show(ui, |ui| {
                self.render_repository_banner_image(
                    ui,
                    &space.repo_image_checksum,
                    self.settings_view_state.hide_repository_image,
                );

                let mut header = egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
                header.fill = self.color_primary_accent();
                header.inner_margin = Margin::same(10);
                header.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let toolbar_icon_size = self
                            .settings_view_state
                            .font_sizes
                            .repository_view
                            .toolbar_icons as f32;
                        let toolbar_btn_width =
                            Self::toolbar_icon_button_size(toolbar_icon_size).x;
                        let toolbar_count = 4.0;
                        let toolbar_total = toolbar_count * toolbar_btn_width
                            + (toolbar_count - 1.0) * ui.spacing().item_spacing.x;
                        let heading_max_width = (ui.available_width()
                            - toolbar_total
                            - ui.spacing().item_spacing.x)
                            .max(0.0);

                        ui.scope(|ui| {
                            ui.set_max_width(heading_max_width);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(
                                        Self::repository_space_display_name(&space),
                                    )
                                    .text_style(TextStyle::Heading),
                                )
                                .truncate(),
                            );
                        });

                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            let is_syncing_anything = self.syncing_repository.is_some();
                            let space_bulk_in_progress = self
                                .repository_space_bulk_progress
                                .as_ref()
                                .is_some_and(|p| {
                                    p.space_id == space.id
                                        && p.completed_count < p.total_count
                                });
                            let buttons_disabled = is_syncing_anything || space_bulk_in_progress;
                            let quick_check_disabled =
                                self.current_sync_mode == Some(SyncMode::QuickCheckOnly);
                            let quick_check_enabled = !buttons_disabled && !quick_check_disabled;
                            let op_in_progress = if space_bulk_in_progress {
                                self.t("Operation in progress")
                            } else if let Some(repo) = self
                                .syncing_repository
                                .and_then(|idx| {
                                    self.repository_view_state.repositories.get(idx)
                                })
                            {
                                self.t_fmt(
                                    "Operation in progress: {repo}",
                                    &[("repo", repo.name.clone())],
                                )
                            } else {
                                self.t("Operation in progress")
                            };
                            let settings_button = Self::repository_toolbar_icon_button(
                                ui,
                                "\u{2699}",
                                toolbar_icon_size,
                                self.t("Repository space settings").as_str(),
                                true,
                                None,
                            );
                            if settings_button.hovered() {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if settings_button.clicked() {
                                open_settings = true;
                            }
                            let recheck_button = Self::repository_toolbar_icon_button(
                                ui,
                                "\u{21bb}",
                                toolbar_icon_size,
                                self.t("Recheck all repositories").as_str(),
                                !buttons_disabled,
                                if buttons_disabled {
                                    Some(op_in_progress.as_str())
                                } else {
                                    None
                                },
                            );
                            if recheck_button.hovered() && !buttons_disabled {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if recheck_button.clicked() {
                                open_recheck_all_modal = true;
                            }

                            let quick_check_button = Self::repository_toolbar_icon_button(
                                ui,
                                "\u{1F4DA}",
                                toolbar_icon_size,
                                self.t("Quick local check.\nIt checks local files for integrity issues or changes.").as_str(),
                                quick_check_enabled,
                                if !quick_check_enabled {
                                    Some(op_in_progress.as_str())
                                } else {
                                    None
                                },
                            );
                            if quick_check_button.hovered() && quick_check_enabled {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if quick_check_button.clicked() {
                                quick_check_all_in_space = true;
                            }

                            let update_all_button = Self::repository_toolbar_icon_button(
                                ui,
                                "\u{2B07}",
                                toolbar_icon_size,
                                self.t("Update all repositories").as_str(),
                                !buttons_disabled,
                                if buttons_disabled {
                                    Some(op_in_progress.as_str())
                                } else {
                                    None
                                },
                            );
                            if update_all_button.hovered() && !buttons_disabled {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if update_all_button.clicked() {
                                open_update_all_modal = true;
                            }
                        });
                    });
                });

                ui.add_space(8.0);
                let mut info_block =
                    egui::containers::Frame::side_top_panel(&ui.ctx().global_style());
                info_block.fill = self.color_widget_bg();
                info_block.corner_radius = egui::CornerRadius::same(8);
                info_block.inner_margin = Margin::same(10);
                info_block.show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(self.t_fmt(
                        "Shared path: {path}",
                        &[("path", space.shared_path.clone())],
                    ));
                    let installed_count = self
                        .repository_view_state
                        .repositories
                        .iter()
                        .filter(|repo| repo.repository_space_id.as_deref() == Some(space.id.as_str()))
                        .count();
                    ui.label(format!(
                        "{} - {}",
                        self.t_fmt(
                            "Total size: {size}",
                            &[("size", fmt_bytes(self.repository_space_remote_size_bytes(&space.id)))],
                        ),
                        self.t_fmt(
                            "Showing {shown} / {total} repositories",
                            &[
                                ("shown", installed_count.to_string()),
                                ("total", space.entries.len().to_string()),
                            ],
                        ),
                    ));
                    if let Some(progress) = self
                        .repository_space_bulk_progress
                        .as_ref()
                        .filter(|progress| progress.space_id == space.id)
                        .filter(|progress| progress.completed_count < progress.total_count)
                    {
                        let active_count = if progress.current_repo_name.is_some() {
                            (progress.completed_count + 1).min(progress.total_count)
                        } else {
                            progress.completed_count
                        };
                        let detail = match progress.mode {
                            RepositorySpaceBulkMode::RecheckAll => self.t_fmt(
                                "Rechecking {done}/{total} - {repo}",
                                &[
                                    ("done", active_count.to_string()),
                                    ("total", progress.total_count.to_string()),
                                    (
                                        "repo",
                                        progress
                                            .current_repo_name
                                            .clone()
                                            .unwrap_or_else(|| self.t("Preparing")),
                                    ),
                                ],
                            ),
                            RepositorySpaceBulkMode::UpdateAll => self.t_fmt(
                                "Updating {done}/{total} - {repo}",
                                &[
                                    ("done", active_count.to_string()),
                                    ("total", progress.total_count.to_string()),
                                    (
                                        "repo",
                                        progress
                                            .current_repo_name
                                            .clone()
                                            .unwrap_or_else(|| self.t("Preparing")),
                                    ),
                                ],
                            ),
                        };
                        ui.colored_label(
                            self.color_primary_accent(),
                            RichText::new(detail).strong(),
                        );
                    }
                });

                if let Some(progress) = self
                    .repository_space_bulk_progress
                    .as_ref()
                    .filter(|progress| progress.space_id == space.id)
                    .filter(|progress| progress.completed_count >= progress.total_count)
                {
                    ui.add_space(8.0);
                    let (title, detail) = match progress.mode {
                        RepositorySpaceBulkMode::RecheckAll => (
                            self.t("Recheck completed"),
                            self.t_fmt(
                                "Recheck complete: {up_to_date} up to date, {updates} updates available, {failed} failed",
                                &[
                                    ("up_to_date", progress.up_to_date_count.to_string()),
                                    ("updates", progress.updates_available_count.to_string()),
                                    ("failed", progress.failed_count.to_string()),
                                ],
                            ),
                        ),
                        RepositorySpaceBulkMode::UpdateAll => (
                            self.t("Update complete"),
                            self.t_fmt(
                                "Update complete: {updated} updated, {failed} failed",
                                &[
                                    ("updated", progress.succeeded_count.to_string()),
                                    ("failed", progress.failed_count.to_string()),
                                ],
                            ),
                        ),
                    };
                    let stroke_color = if progress.failed_count > 0 {
                        self.color_warn()
                    } else {
                        self.color_success_muted()
                    };
                    let dismiss_label = self.t("Dismiss");
                    if self.render_repository_message_banner(
                        ui,
                        title.as_str(),
                        detail.as_str(),
                        stroke_color,
                        None,
                        Some(dismiss_label.as_str()),
                    ) == RepositoryBannerResponse::DismissClicked
                    {
                        dismiss_completed_bulk_progress = true;
                    }
                }

                ui.add_space(8.0);
                let detail_filter_hint = self.t("Filter by name or address");
                let detail_filter_help = self.t("repository_filter_help");
                ui.horizontal(|ui| {
                    ui.label(self.t("Filter:"));
                    let filter_input = ui.add(
                        TextEdit::singleline(&mut self.repository_space_detail_filter)
                            .hint_text(detail_filter_hint)
                            .desired_width((ui.available_width() - 150.0).max(200.0)),
                    );
                    if filter_input.hovered() {
                        ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                    }
                    ui.add_space(6.0);
                    self.filter_help_icon(ui, &detail_filter_help);
                });
                if has_detail_filter {
                    ui.label(self.t_fmt(
                        "Showing {shown} / {total} repositories",
                        &[
                            ("shown", filtered_entry_indices.len().to_string()),
                            ("total", space.entries.len().to_string()),
                        ],
                    ));
                }
                ui.add_space(8.0);
                ui.heading(self.t("Available repositories"));
                let remaining_detail_height = ui.available_height().max(0.0);
                let candidate_info_lines = 1.0
                    + if selector_state.last_scan_result_count.is_some() {
                        1.0
                    } else {
                        0.0
                    }
                    + if has_detail_filter { 1.0 } else { 0.0 };
                let candidate_controls_height = 62.0 + candidate_info_lines * 18.0;
                let candidate_list_height = if filtered_candidate_indices.is_empty() {
                    72.0
                } else {
                    ((filtered_candidate_indices.len() as f32)
                        * Self::repository_space_candidate_row_height()
                        + 8.0)
                        .clamp(72.0, 180.0)
                };
                let reserved_bottom_height = candidate_controls_height + candidate_list_height + 12.0;
                let available_list_height =
                    (remaining_detail_height - reserved_bottom_height).max(220.0);
                ScrollArea::vertical()
                    .id_salt(("space_detail_entries", space_id))
                    .max_height(available_list_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for (list_idx, entry_idx) in filtered_entry_indices.iter().enumerate() {
                            ui.push_id(*entry_idx, |ui| {
                                let entry = &space.entries[*entry_idx];
                                self.render_repository_space_detail_entry_card(
                                    ui,
                                    &space,
                                    entry,
                                    &mut add_entry_action,
                                    &mut jump_to_repository,
                                    &mut detach_repo_idx,
                                );
                                if list_idx + 1 < filtered_entry_indices.len() {
                                    ui.add_space(6.0);
                                }
                            });
                        }
                    });

                ui.separator();
                ui.vertical(|ui| {
                    ui.set_min_width(ui.available_width());
                    ui.heading(self.t("Matching existing repositories"));
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
                    if let Some(scan_count) = selector_state.last_scan_result_count {
                        if scan_count == 0 {
                            ui.colored_label(
                                self.color_text_dim(),
                                self.t("No unassigned repositories found matching this space's entries."),
                            );
                        } else {
                            ui.colored_label(
                                self.color_text_dim(),
                                self.t_fmt(
                                    "Scan complete: found {count} matching repositories.",
                                    &[("count", scan_count.to_string())],
                                ),
                            );
                        }
                    }
                    if has_detail_filter {
                        ui.label(self.t_fmt(
                            "Showing {shown} / {total} repositories",
                            &[
                                ("shown", filtered_candidate_indices.len().to_string()),
                                ("total", selector_state.candidates.len().to_string()),
                            ],
                        ));
                    }
                    if filtered_candidate_indices.is_empty() {
                        ScrollArea::vertical()
                            .id_salt(("space_detail_scan_candidates", &space.id))
                            .max_height(180.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                if selector_state.last_scan_result_count == Some(0)
                                    && !has_detail_filter
                                {
                                    ui.label(self.t(
                                        "No unassigned repositories found matching this space's entries.",
                                    ));
                                } else {
                                    ui.label(self.t("No matching existing repositories found"));
                                }
                            });
                    } else {
                        self.space_detail_candidate_galleys.ensure(
                            filtered_candidate_indices.len(),
                            1,
                            galley_cache::fingerprint((
                                space.id.as_str(),
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
                            .id_salt(("space_detail_scan_candidates", &space.id))
                            .max_height(180.0)
                            .auto_shrink([false, false])
                            .show_rows(
                                ui,
                                Self::repository_space_candidate_row_height(),
                                filtered_candidate_indices.len(),
                                |ui, row_range| {
                                    ui.set_min_width(ui.available_width());
                                    for filtered_idx in row_range {
                                        let candidate_idx =
                                            filtered_candidate_indices[filtered_idx];
                                        ui.push_id(candidate_idx, |ui| {
                                            let candidate =
                                                &mut selector_state.candidates[candidate_idx];
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
                });
                if let Some(error) = &selector_state.error {
                    ui.colored_label(self.color_text_error(), error);
                }

                if !self.repository_space_required_entries_satisfied(&space.id) {
                    ui.colored_label(
                        self.color_text_error(),
                        self.t("Required repositories must be added at least once"),
                    );
                }
            });

        if refresh_scan {
            selector_state.candidates = self.scan_repository_space_candidates(&space.id);
            selector_state.last_scan_result_count = Some(selector_state.candidates.len());
            if selector_state.candidates.is_empty() {
                self.show_success_toast(self.t("No matching existing repositories found"));
            }
        }

        if move_selected {
            let moved =
                self.apply_repository_space_scan_candidates(&space.id, &selector_state.candidates);
            selector_state.candidates = self.scan_repository_space_candidates(&space.id);
            selector_state.last_scan_result_count = Some(selector_state.candidates.len());
            selector_state.error = None;
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

        if open_recheck_all_modal {
            self.pending_repository_space_bulk_action = self
                .build_repository_space_bulk_action(&space.id, RepositorySpaceBulkMode::RecheckAll);
        }
        if quick_check_all_in_space {
            let queued = self.queue_repository_space_sync(&space.id, SyncMode::QuickCheckOnly);
            info!(
                "Queued quick local check for {} repositories in repository space {}",
                queued,
                Self::repository_space_display_name(&space)
            );
        }
        if open_update_all_modal {
            self.pending_repository_space_bulk_action = self
                .build_repository_space_bulk_action(&space.id, RepositorySpaceBulkMode::UpdateAll);
        }

        if open_settings {
            self.open_repository_space_settings(&space.id);
            self.last_view = self.current_view;
            self.current_view = FoxyView::RepositorySpaceSettings;
        }
        if let Some((entry_address, entry_name)) = add_entry_action {
            self.add_repository_from_space_entry(&space.id, &entry_address, &entry_name, ui.ctx());
        }
        if let Some(repo_idx) = detach_repo_idx {
            if self.detach_repository_from_space(repo_idx) {
                info!(
                    "Detached repository from space {}",
                    Self::repository_space_display_name(&space)
                );
            }
            selector_state.candidates = self.scan_repository_space_candidates(&space.id);
            selector_state.last_scan_result_count = Some(selector_state.candidates.len());
        }
        if let Some(repo_idx) = jump_to_repository {
            self.selected_repository_space_id = None;
            self.repository_view_state.selected_repository = Some(repo_idx);
            self.clear_completed_repository_check_banner_for_repo_change(Some(repo_idx));
            self.clear_mod_diff_cache();
            self.load_cached_updates_for_repo(repo_idx);
        }
        if dismiss_completed_bulk_progress {
            self.repository_space_bulk_progress = None;
        }

        self.repository_space_selector_state = Some(selector_state);
    }
}
