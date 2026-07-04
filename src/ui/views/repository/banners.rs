use super::{
    RepositoryActionBanner, RepositoryActionBannerAction, RepositoryBannerResponse,
    RepositoryCheckCompletionBanner, RepositoryCheckStatusBanner, RepositoryUiAction,
};
use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::types::RepoState;
use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, RichText, Ui,
};

impl Foxy {
    pub(super) fn repository_toolbar_icon_button(
        ui: &mut Ui,
        icon: &str,
        icon_size: f32,
        tooltip: &str,
        enabled: bool,
        disabled_reason: Option<&str>,
    ) -> egui::Response {
        let (rect, _) = ui.allocate_exact_size(
            Self::toolbar_icon_button_size(icon_size),
            egui::Sense::hover(),
        );
        let sense = if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let response = ui.interact(rect, ui.id().with(icon), sense);

        if ui.is_rect_visible(rect) {
            let visuals = if enabled {
                ui.style().interact(&response)
            } else {
                &ui.visuals().widgets.inactive
            };

            let (bg_color, fg_color, stroke) = if enabled {
                (visuals.bg_fill, visuals.fg_stroke.color, visuals.bg_stroke)
            } else {
                let dim = |c: Color32| -> Color32 {
                    let a = (c.a() as f32 * 0.3).round().clamp(0.0, 255.0) as u8;
                    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
                };
                (
                    dim(visuals.bg_fill),
                    dim(visuals.fg_stroke.color),
                    egui::Stroke::new(visuals.bg_stroke.width, dim(visuals.bg_stroke.color)),
                )
            };

            ui.painter()
                .rect_filled(rect, CornerRadius::same(4), bg_color);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(4),
                stroke,
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                icon,
                egui::FontId::proportional(icon_size),
                fg_color,
            );
        }

        if !enabled && response.hovered() {
            ui.ctx()
                .output_mut(|o| o.cursor_icon = CursorIcon::NotAllowed);
        }

        if enabled {
            response.on_hover_text(tooltip)
        } else if let Some(reason) = disabled_reason {
            response.on_hover_text(reason)
        } else {
            response.on_hover_text(tooltip)
        }
    }

    pub(super) fn active_repository_check_banner(
        &self,
        repo_index: usize,
    ) -> Option<RepositoryCheckStatusBanner> {
        if self.syncing_repository != Some(repo_index) {
            return None;
        }

        let mode = self.current_sync_mode?;
        let title = match mode {
            SyncMode::RemoteRefreshOnly => self.t("Remote data recheck in progress"),
            SyncMode::QuickCheckOnly => self.t("Quick local check in progress"),
            SyncMode::RecheckOnly => self.t("Repository recheck in progress"),
            SyncMode::RecheckIntegrity => self.t("Integrity recheck in progress"),
            SyncMode::Download => return None,
        };

        let detail = if let Some((checked, total)) = self.recheck_hash_counter {
            let (checked_text, total_text) = if let Some((checked_parts, total_parts)) =
                self.recheck_hash_part_counter
                && total_parts > 0
                && (checked_parts, total_parts) != (checked, total)
            {
                (
                    format!("{checked}/{total}, {checked_parts}"),
                    total_parts.to_string(),
                )
            } else {
                (checked.to_string(), total.to_string())
            };
            self.t_fmt(
                "Calculating file hashes ({checked}/{total})",
                &[("checked", checked_text), ("total", total_text)],
            )
        } else if let Some(stage) = &self.recheck_stage_label {
            self.translate_repository_check_stage(stage)
        } else {
            self.t("Rechecking repository...")
        };

        let elapsed = self
            .sync_started_at
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let progress = if let Some((checked_parts, total_parts)) = self.recheck_hash_part_counter {
            if total_parts > 0 {
                Some((checked_parts as f32 / total_parts as f32).clamp(0.0, 1.0))
            } else {
                None
            }
        } else if let Some((checked, total)) = self.recheck_hash_counter {
            if total > 0 {
                Some((checked as f32 / total as f32).clamp(0.0, 1.0))
            } else {
                None
            }
        } else {
            self.recheck_stage_percent
                .map(|percent| percent.clamp(0.0, 1.0))
        };

        Some(RepositoryCheckStatusBanner {
            title,
            detail,
            hint: self.repository_check_cycle_message(mode, elapsed),
            progress,
            elapsed_seconds: elapsed.as_secs(),
        })
    }

    pub(super) fn active_repository_db_wipe_banner(
        &self,
        repo_index: usize,
    ) -> Option<RepositoryCheckStatusBanner> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if !self.is_repository_db_wipe_pending(&repo.address) {
            return None;
        }

        let elapsed = self
            .repository_db_wipe_elapsed(&repo.address)
            .unwrap_or_default();
        let force_redownload = self.is_repository_force_redownload_pending(&repo.address);
        Some(RepositoryCheckStatusBanner {
            title: if force_redownload {
                self.t("Force redownload repository")
            } else {
                self.t("Repository database wipe in progress")
            },
            detail: if force_redownload {
                self.t_fmt(
                    "Force redownload {name}?\nThis will remove local files and re-download the repository.",
                    &[("name", repo.name.clone())],
                )
            } else {
                self.t("This only clears cached metadata for this repository.")
            },
            hint: if force_redownload {
                self.t("Updating...")
            } else {
                self.t("Run a remote data recheck afterward to rebuild metadata")
            },
            progress: None,
            elapsed_seconds: elapsed.as_secs(),
        })
    }

    pub(super) fn active_repository_row_operation_tooltip(
        &self,
        repo_index: usize,
    ) -> Option<String> {
        if self.syncing_repository != Some(repo_index) {
            let repo = self.repository_view_state.repositories.get(repo_index)?;
            let key = Self::repo_instance_key(&repo.address, &repo.path);
            if self.active_quick_scan_instance_keys.contains(&key) {
                return Some(format!(
                    "{}\n{}",
                    self.t("Quick local check in progress"),
                    self.t("Quick local verify")
                ));
            }
            return None;
        }

        let mode = self.current_sync_mode?;
        match mode {
            SyncMode::Download => {
                let title = if self.download_paused {
                    self.t("Download paused")
                } else {
                    self.t("Updating...")
                };
                let detail = self.active_repository_download_stage_detail();
                Some(if detail == title {
                    title
                } else {
                    format!("{title}\n{detail}")
                })
            }
            SyncMode::RemoteRefreshOnly
            | SyncMode::QuickCheckOnly
            | SyncMode::RecheckOnly
            | SyncMode::RecheckIntegrity => {
                let banner = self.active_repository_check_banner(repo_index)?;
                Some(if banner.detail == banner.title {
                    banner.title
                } else {
                    format!("{}\n{}", banner.title, banner.detail)
                })
            }
        }
    }

    pub(super) fn render_repository_message_banner(
        &self,
        ui: &mut Ui,
        title: &str,
        detail: &str,
        stroke_color: Color32,
        action_button: Option<(&str, Color32)>,
        dismiss_label: Option<&str>,
    ) -> RepositoryBannerResponse {
        let banner_font_size = self
            .settings_view_state
            .font_sizes
            .repository_view
            .status_banner as f32;
        let detail_font_size = (banner_font_size - 3.0).max(14.0);
        let status_banner_fill = self.color_widget_bg();
        let status_banner_text = self.color_text_normal();
        let mut response = RepositoryBannerResponse::None;

        Frame::NONE
            .fill(status_banner_fill)
            .stroke(egui::Stroke::new(1.0, stroke_color))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).size(banner_font_size).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Keep the buttons' geometry stable across hover/press.
                        // egui derives a button's inner margin from
                        // `button_padding - bg_stroke.width`, and the default
                        // `bg_stroke` width differs per state (0 inactive, 1
                        // hovered/active). That made the dismiss button shrink by
                        // 1px per side on hover and shifted the neighbouring action
                        // button. We paint our own constant borders, so pin the
                        // stroke width across the interactive states.
                        let widgets = &mut ui.visuals_mut().widgets;
                        widgets.inactive.bg_stroke.width = 1.0;
                        widgets.hovered.bg_stroke.width = 1.0;
                        widgets.active.bg_stroke.width = 1.0;
                        // In a right-to-left layout the first widget sits at the
                        // far right, so render the dismiss control first to keep
                        // it pinned to the edge with the action button to its left.
                        if let Some(dismiss_label) = dismiss_label {
                            ui.push_id("banner_dismiss", |ui| {
                                let dismiss_button = ui.add(
                                    Button::new(
                                        RichText::new(dismiss_label)
                                            .size((detail_font_size - 1.0).max(13.0)),
                                    )
                                    .stroke(egui::Stroke::new(1.0, stroke_color)),
                                );
                                if dismiss_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if dismiss_button.clicked() {
                                    response = RepositoryBannerResponse::DismissClicked;
                                }
                            });
                            if action_button.is_some() {
                                ui.add_space(8.0);
                            }
                        }
                        if let Some((button_label, button_fill)) = action_button {
                            ui.push_id("banner_action", |ui| {
                                let action_button = ui.add(
                                    Button::new(
                                        RichText::new(button_label)
                                            .size((detail_font_size - 1.0).max(13.0)),
                                    )
                                    .fill(button_fill),
                                );
                                if action_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if action_button.clicked() {
                                    response = RepositoryBannerResponse::ActionClicked;
                                }
                            });
                        }
                    });
                });
                ui.add_space(6.0);
                ui.label(
                    RichText::new(detail)
                        .size(detail_font_size)
                        .color(status_banner_text),
                );
            });

        response
    }

    pub(super) fn repository_action_banner(
        &self,
        repo_index: usize,
        repo_state: RepoState,
    ) -> Option<RepositoryActionBanner> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        let normalized_repo_url = Self::normalize_repo_url(&repo.address);

        if self.syncing_repository == Some(repo_index)
            && self.current_sync_mode == Some(SyncMode::Download)
        {
            let title = if self.download_paused {
                self.t("Download paused")
            } else {
                self.t("Updating...")
            };
            let detail = self.active_repository_download_stage_detail();
            return Some(RepositoryActionBanner {
                title,
                detail,
                stroke_color: self.color_primary_accent(),
                button_label: self.t("Display update view"),
                button_fill: self.color_action_info(),
                action: RepositoryActionBannerAction::UpdateView,
            });
        }

        if let Some(notice) =
            self.settings_view_state
                .update_summary_notices
                .iter()
                .find(|notice| {
                    notice.repository_url == normalized_repo_url
                        && notice.summary.has_meaningful_content()
                })
        {
            // `pending_ack_count` is how many completed downloads for this repo
            // have not been reviewed yet. Surface that count only when several
            // updates have stacked up; for the common single update the bare
            // label is clearer and the per-update totals live in `detail`.
            let unreviewed_count = notice.pending_ack_count.max(1);
            let button_label = if unreviewed_count > 1 {
                self.t_fmt(
                    "Show update summary ({count})",
                    &[("count", unreviewed_count.to_string())],
                )
            } else {
                self.t("Show update summary")
            };
            let detail = self.t_fmt(
                "Updated: {mods} mods, {files} files, {parts} parts",
                &[
                    ("mods", notice.summary.mods_updated.to_string()),
                    ("files", notice.summary.files_updated.to_string()),
                    ("parts", notice.summary.parts_updated.to_string()),
                ],
            );
            return Some(RepositoryActionBanner {
                title: self.t("Download Summary"),
                detail,
                stroke_color: self.color_success_muted(),
                button_label,
                button_fill: self.color_success_muted(),
                action: RepositoryActionBannerAction::UpdateSummary,
            });
        }

        let has_cached_updates = self.mod_diff_cache.iter().any(|m| m.needs_update);
        let ready_from_live_cache = (self.syncing_repository == Some(repo_index)
            || !self.mod_diff_cache.is_empty())
            && self.update_ready_repo == Some(repo_index)
            && has_cached_updates;
        let show_pending_button = ready_from_live_cache || repo_state == RepoState::PendingUpdate;
        if !show_pending_button {
            return None;
        }

        let update_count = if self.update_ready_repo == Some(repo_index) {
            self.mod_diff_cache
                .iter()
                .filter(|m| m.needs_update)
                .count()
        } else {
            self.pending_update_count_for_address(&repo.address, &repo.path)
        };
        let detail = if update_count > 0 {
            self.i18n
                .tr_plural("Updates found in {count} addons", update_count as u64)
        } else {
            self.t("Updates available - recheck for details")
        };

        Some(RepositoryActionBanner {
            title: self.t("Quick local check finished"),
            detail,
            stroke_color: self.color_warn(),
            button_label: self.t("Update ready - click here"),
            button_fill: self.color_action_destructive(),
            action: RepositoryActionBannerAction::PendingUpdate,
        })
    }

    pub(super) fn queue_open_pending_update_action(
        open_pending_update_action: &mut Option<RepositoryUiAction>,
        repo_index: usize,
    ) {
        *open_pending_update_action = Some(Box::new(move |app| {
            // Fast path: pending updates are already available in memory.
            let has_updates_in_memory = (app.update_ready_repo == Some(repo_index)
                && app.mod_diff_cache.iter().any(|m| m.needs_update))
                || app.apply_pending_update_cache_for_repo(repo_index);
            if has_updates_in_memory {
                app.open_pending_update_modal_for_repo(repo_index);
            } else {
                // Not cached in memory; load the payload off the UI thread and
                // open the modal once it arrives (if it still has updates).
                app.load_cached_updates_for_repo_and_open_modal(repo_index);
            }
        }));
    }

    pub(super) fn completed_repository_check_banner(
        &self,
        repo_index: usize,
    ) -> Option<RepositoryCheckCompletionBanner> {
        let banner = self.completed_repository_check_banner.as_ref()?;
        if banner.repo_index != repo_index || self.syncing_repository.is_some() {
            return None;
        }

        let title = self.repository_check_completion_title(banner);
        let detail = if banner.success {
            if banner.had_updates {
                self.i18n.tr_plural(
                    "Updates found in {count} addons",
                    banner.update_count as u64,
                )
            } else {
                self.t("No updates found")
            }
        } else {
            banner
                .error_message
                .clone()
                .unwrap_or_else(|| self.t("Review the activity log for details"))
        };

        let stroke_color = if !banner.success {
            self.color_error()
        } else if banner.had_updates {
            self.color_warn()
        } else {
            self.color_primary_accent()
        };

        Some(RepositoryCheckCompletionBanner {
            title,
            detail,
            stroke_color,
            show_pending_action: banner.success && banner.had_updates,
        })
    }

    pub(super) fn completed_repository_db_wipe_banner(
        &self,
        repo_index: usize,
    ) -> Option<RepositoryCheckCompletionBanner> {
        let banner = self.completed_repository_db_wipe_banner.as_ref()?;
        if self.syncing_repository.is_some() {
            return None;
        }

        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if Self::normalize_repo_url(&repo.address) != banner.repository_url {
            return None;
        }

        let title = if banner.success {
            self.t_fmt(
                "Repository database wipe finished in {duration}",
                &[(
                    "duration",
                    Self::format_compact_elapsed_duration(banner.elapsed),
                )],
            )
        } else {
            self.t("Repository database wipe failed")
        };

        let detail = if banner.success {
            self.t("Database entries cleared. Run a remote data recheck to rebuild metadata.")
        } else {
            banner
                .error_message
                .clone()
                .unwrap_or_else(|| self.t("Review the activity log for details"))
        };

        Some(RepositoryCheckCompletionBanner {
            title,
            detail,
            stroke_color: if banner.success {
                self.color_primary_accent()
            } else {
                self.color_error()
            },
            show_pending_action: false,
        })
    }
}
