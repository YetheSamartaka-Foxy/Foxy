use crate::core::api::SyncMode;
use crate::ui::app::{BarMotion, Foxy, StepTrack, paint_border_flare, rise_effect};
use crate::ui::search_filter::MultiEntryFilter;
use crate::ui::types::{FoxyView, RepositorySpaceBulkMode};
use crate::ui::views::game_spaces::overview::CARD_GAP;
use eframe::egui::{
    self, Align, CursorIcon, Layout, Margin, RichText, ScrollArea, TextEdit, TextStyle, Ui,
};
use log::info;

use super::{BANNER_CORNER_RADIUS, BANNER_FLARE_SECONDS, RepositoryBannerResponse};

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
        let mut refresh_from_server = false;
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
                        let toolbar_count = 5.0;
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
                            let refresh_enabled = !space.source_address.trim().is_empty()
                                && !self.repository_space_refresh_in_flight();
                            let refresh_button = Self::repository_toolbar_icon_button(
                                ui,
                                "\u{1F310}",
                                toolbar_icon_size,
                                self.t("Refresh from server").as_str(),
                                refresh_enabled,
                                None,
                            );
                            if refresh_button.hovered() && refresh_enabled {
                                ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                            }
                            if refresh_button.clicked() {
                                refresh_from_server = true;
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


                ui.add_space(CARD_GAP);
                self.render_repository_space_stat_tiles(ui, &space);

                if !self.repository_space_required_entries_satisfied(&space.id) {
                    ui.add_space(CARD_GAP);
                    self.render_repository_message_banner(
                        ui,
                        self.t("Required repositories missing").as_str(),
                        self.t("Required repositories must be added at least once").as_str(),
                        self.color_warn(),
                        None,
                        None,
                        None,
                    );
                }

                if let Some(progress) = self
                    .repository_space_bulk_progress
                    .clone()
                    .filter(|progress| progress.space_id == space.id)
                    .filter(|progress| progress.completed_count < progress.total_count)
                {
                    ui.add_space(CARD_GAP);
                    let active_count = if progress.current_repo_name.is_some() {
                        (progress.completed_count + 1).min(progress.total_count)
                    } else {
                        progress.completed_count
                    };
                    let current_repo = progress
                        .current_repo_name
                        .clone()
                        .unwrap_or_else(|| self.t("Preparing"));
                    let detail = match progress.mode {
                        RepositorySpaceBulkMode::RecheckAll => self.t_fmt(
                            "Rechecking {done}/{total} - {repo}",
                            &[
                                ("done", active_count.to_string()),
                                ("total", progress.total_count.to_string()),
                                ("repo", current_repo),
                            ],
                        ),
                        RepositorySpaceBulkMode::UpdateAll => self.t_fmt(
                            "Updating {done}/{total} - {repo}",
                            &[
                                ("done", active_count.to_string()),
                                ("total", progress.total_count.to_string()),
                                ("repo", current_repo),
                            ],
                        ),
                    };
                    let (current, motion) = self
                        .syncing_repository
                        .map_or((None, BarMotion::Forward), |index| {
                            self.repository_operation_activity(index)
                        });
                    self.overview_card(ui, |this, ui| {
                        ui.colored_label(
                            this.color_primary_accent(),
                            RichText::new(detail).strong(),
                        );
                        ui.add_space(4.0);
                        ui.add(
                            StepTrack::new(
                                progress.completed_count,
                                progress.total_count,
                                this.color_primary_accent(),
                                this.color_card_bg(),
                            )
                            .running(current, motion),
                        );
                    });
                }

                let finished_bulk = self
                    .repository_space_bulk_progress
                    .as_ref()
                    .filter(|progress| progress.space_id == space.id)
                    .filter(|progress| progress.completed_count >= progress.total_count);
                let bulk_finished_flare = rise_effect(
                    ui,
                    egui::Id::new(("space-bulk-finished", space.id.as_str())),
                    finished_bulk.is_some(),
                    BANNER_FLARE_SECONDS,
                );
                if let Some(progress) = finished_bulk {
                    ui.add_space(CARD_GAP);
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
                    let banner = self.render_repository_message_banner(
                        ui,
                        title.as_str(),
                        detail.as_str(),
                        stroke_color,
                        None,
                        Some(dismiss_label.as_str()),
                        None,
                    );
                    if let Some(flare) = bulk_finished_flare {
                        paint_border_flare(
                            ui,
                            banner.response.rect.shrink(0.5),
                            f32::from(BANNER_CORNER_RADIUS),
                            stroke_color,
                            flare,
                        );
                    }
                    if banner.inner == RepositoryBannerResponse::DismissClicked {
                        dismiss_completed_bulk_progress = true;
                    }
                }

                ui.add_space(CARD_GAP);
                let detail_filter_hint = self.t("Filter by name or address");
                let detail_filter_help = self.t("repository_filter_help");
                self.overview_card(ui, |this, ui| {
                    this.overview_section_title(ui, &this.t("Available repositories"));
                    this.overview_folder_row(ui, &space.shared_path, "repository space shared");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let help_width = ui.text_style_height(&TextStyle::Body)
                            + ui.spacing().item_spacing.x;
                        let filter_input = ui.add(
                            TextEdit::singleline(&mut this.repository_space_detail_filter)
                                .hint_text(detail_filter_hint)
                                .desired_width((ui.available_width() - help_width).max(120.0)),
                        );
                        if filter_input.hovered() {
                            ui.ctx().output_mut(|o| o.cursor_icon = CursorIcon::Text);
                        }
                        this.filter_help_icon(ui, &detail_filter_help);
                    });
                    if has_detail_filter {
                        ui.label(
                            RichText::new(this.t_fmt(
                                "Showing {shown} / {total} repositories",
                                &[
                                    ("shown", filtered_entry_indices.len().to_string()),
                                    ("total", space.entries.len().to_string()),
                                ],
                            ))
                            .small()
                            .color(this.color_text_dim()),
                        );
                    }
                    ui.add_space(8.0);
                    if filtered_entry_indices.is_empty() {
                        ui.label(
                            RichText::new(this.t("No repositories match the current filter."))
                                .italics()
                                .color(this.color_text_dim()),
                        );
                    }
                    for (list_idx, entry_idx) in filtered_entry_indices.iter().enumerate() {
                        ui.push_id(*entry_idx, |ui| {
                            this.render_repository_space_detail_entry_card(
                                ui,
                                &space,
                                &space.entries[*entry_idx],
                                &mut add_entry_action,
                                &mut jump_to_repository,
                                &mut detach_repo_idx,
                            );
                        });
                        if list_idx + 1 < filtered_entry_indices.len() {
                            ui.add_space(6.0);
                        }
                    }
                });

                ui.add_space(CARD_GAP);
                self.overview_card(ui, |this, ui| {
                    let actions = this.render_repository_space_matching_section(
                        ui,
                        &space.id,
                        &mut selector_state,
                        &filtered_candidate_indices,
                        has_detail_filter,
                    );
                    refresh_scan = actions.scan;
                    move_selected = actions.move_selected;
                });
                ui.add_space(CARD_GAP);
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

        if refresh_from_server {
            info!(
                "Refreshing repository space {} from the server",
                Self::repository_space_display_name(&space)
            );
            self.refresh_repository_space_from_server(&space.id);
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
