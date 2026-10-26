use super::{
    BANNER_CORNER_RADIUS, RepositoryActionBanner, RepositoryActionBannerAction,
    RepositoryBannerResponse, RepositoryCheckCompletionBanner, RepositoryCheckStatusBanner,
    RepositoryUiAction,
};
use crate::core::api::SyncMode;
use crate::ui::app::{
    ActivityBar, ActivityGlyph, BarMotion, Foxy, QuickScanProgressState, VerificationMap,
    paint_border_beam, transfer_progress,
};
use crate::ui::i18n::fmt_bytes;
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
        let (title, glyph) = match mode {
            SyncMode::RemoteRefreshOnly => (
                self.t("Remote data recheck in progress"),
                ActivityGlyph::Signal,
            ),
            SyncMode::QuickCheckOnly => (
                self.t("Quick local check in progress"),
                ActivityGlyph::Radar,
            ),
            SyncMode::RecheckOnly => (
                self.t("Repository recheck in progress"),
                ActivityGlyph::Radar,
            ),
            SyncMode::RecheckIntegrity => (
                self.t("Integrity recheck in progress"),
                ActivityGlyph::Shield,
            ),
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
            let counter = self.t_fmt(
                "Calculating file hashes ({checked}/{total})",
                &[("checked", checked_text), ("total", total_text)],
            );
            let remaining = self
                .recheck_hash_byte_counter
                .map(|(checked, total)| total.saturating_sub(checked));
            match self.recheck_hash_estimate {
                Some((estimated_remaining, bytes_per_sec)) if bytes_per_sec > 0 => {
                    let remaining_bytes = remaining.unwrap_or(estimated_remaining);
                    let eta = Self::format_hash_eta(remaining_bytes, bytes_per_sec);
                    format!(
                        "{counter} - {}",
                        self.t_fmt(
                            "{size} at {rate}/s, about {eta} remaining",
                            &[
                                ("size", fmt_bytes(remaining_bytes)),
                                ("rate", fmt_bytes(bytes_per_sec)),
                                ("eta", eta),
                            ],
                        )
                    )
                }
                _ => counter,
            }
        } else if let Some(stage) = &self.recheck_stage_label {
            self.translate_repository_check_stage(stage)
        } else {
            self.t("Rechecking repository...")
        };

        let elapsed = self
            .sync_started_at
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let progress = self.recheck_progress_fraction();

        Some(RepositoryCheckStatusBanner {
            title,
            detail,
            hint: self.repository_check_cycle_message(mode, elapsed),
            progress,
            elapsed_seconds: elapsed.as_secs(),
            cancellable: true,
            glyph,
            verification: true,
        })
    }

    pub(super) fn active_quick_scan_banner(
        &self,
        repo_index: usize,
    ) -> Option<RepositoryCheckStatusBanner> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        let key = Self::repo_instance_key(&repo.address, &repo.path);
        if !self.active_quick_scan_instance_keys.contains(&key) {
            return None;
        }

        let state = self.quick_scan_progress_by_instance.get(&key);
        let detail = if let Some(state) = state {
            if let Some((checked, total)) = state.hash_counter {
                let (checked_text, total_text) = if let Some((checked_parts, total_parts)) =
                    state.hash_part_counter
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
            } else if let Some(stage) = &state.stage_label {
                self.translate_repository_check_stage(stage)
            } else {
                self.t("Quick local check")
            }
        } else {
            self.t("Quick local check")
        };

        let elapsed = state
            .map(|state| state.started_at.elapsed())
            .unwrap_or_default();
        let progress = state.and_then(quick_scan_progress);

        Some(RepositoryCheckStatusBanner {
            title: self.t("Quick local check in progress"),
            detail,
            hint: self.repository_check_cycle_message(SyncMode::QuickCheckOnly, elapsed),
            progress,
            elapsed_seconds: elapsed.as_secs(),
            cancellable: false,
            glyph: ActivityGlyph::Radar,
            verification: true,
        })
    }

    /// Shown while startup database maintenance still holds the database.
    pub(super) fn active_database_maintenance_banner(&self) -> Option<RepositoryCheckStatusBanner> {
        let elapsed = crate::core::tasks::db_turso::db_startup_compaction_elapsed()?;
        Some(RepositoryCheckStatusBanner {
            title: self.t("Optimizing database"),
            detail: self.t(
                "One-time database maintenance is running. Checks and updates continue automatically when it finishes.",
            ),
            hint: self.t("This may take a few minutes"),
            progress: None,
            elapsed_seconds: elapsed.as_secs(),
            cancellable: false,
            glyph: ActivityGlyph::Gear,
            verification: false,
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
            cancellable: true,
            glyph: if force_redownload {
                ActivityGlyph::Download
            } else {
                ActivityGlyph::Gear
            },
            verification: false,
        })
    }

    /// Progress of the work running on a repository (`None` while its end is
    /// unknown), and whether it moves or a paused download holds it.
    pub(super) fn repository_operation_activity(
        &self,
        repo_index: usize,
    ) -> (Option<f32>, BarMotion) {
        if self.syncing_repository != Some(repo_index) {
            let progress = self
                .repository_view_state
                .repositories
                .get(repo_index)
                .and_then(|repo| {
                    let key = Self::repo_instance_key(&repo.address, &repo.path);
                    self.quick_scan_progress_by_instance.get(&key)
                })
                .and_then(quick_scan_progress);
            return (progress, BarMotion::Forward);
        }
        if self.current_sync_mode == Some(SyncMode::Download) {
            let motion = if self.download_paused {
                BarMotion::Held
            } else {
                BarMotion::Forward
            };
            let fraction = self
                .download_progress
                .as_ref()
                .map_or(0.0, |(_, fraction)| *fraction);
            return (transfer_progress(fraction, motion), motion);
        }
        (self.recheck_progress_fraction(), BarMotion::Forward)
    }

    pub(super) fn active_repository_row_operation_tooltip(
        &self,
        repo_index: usize,
    ) -> Option<String> {
        if self.syncing_repository != Some(repo_index) {
            let repo = self.repository_view_state.repositories.get(repo_index)?;
            let key = Self::repo_instance_key(&repo.address, &repo.path);
            if self.active_quick_scan_instance_keys.contains(&key) {
                let banner = self.active_quick_scan_banner(repo_index)?;
                return Some(if banner.detail == banner.title {
                    banner.title
                } else {
                    format!("{}\n{}", banner.title, banner.detail)
                });
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_repository_message_banner(
        &self,
        ui: &mut Ui,
        title: &str,
        detail: &str,
        stroke_color: Color32,
        action_button: Option<(&str, Color32)>,
        dismiss_label: Option<&str>,
        activity: Option<ActivityBar>,
        export_logs: bool,
    ) -> egui::InnerResponse<RepositoryBannerResponse> {
        let banner_font_size = self
            .settings_view_state
            .font_sizes
            .repository_view
            .status_banner as f32;
        let detail_font_size = (banner_font_size - 3.0).max(14.0);
        let status_banner_fill = self.color_widget_bg();
        let status_banner_text = self.color_text_normal();
        let mut response = RepositoryBannerResponse::None;
        let running = activity.is_some();

        let frame = Frame::NONE
            .fill(status_banner_fill)
            .stroke(egui::Stroke::new(1.0, stroke_color))
            .corner_radius(CornerRadius::same(BANNER_CORNER_RADIUS))
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
                        if export_logs {
                            if dismiss_label.is_some() || action_button.is_some() {
                                ui.add_space(8.0);
                            }
                            ui.push_id("banner_export_logs", |ui| {
                                let export_button = ui
                                    .add(
                                        Button::new(
                                            RichText::new(self.export_logs_button_text())
                                                .size((detail_font_size - 1.0).max(13.0)),
                                        )
                                        .fill(self.color_main_bg()),
                                    )
                                    .on_hover_text(self.export_logs_tooltip());
                                if export_button.hovered() {
                                    ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                                }
                                if export_button.clicked() {
                                    response = RepositoryBannerResponse::ExportLogsClicked;
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
                if let Some(activity) = activity {
                    ui.add_space(8.0);
                    ui.add(activity);
                }
            });
        if running {
            paint_border_beam(
                ui,
                frame.response.rect.shrink(0.5),
                f32::from(BANNER_CORNER_RADIUS),
                stroke_color,
            );
        }

        egui::InnerResponse::new(response, frame.response)
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
            let (progress, motion) = self.repository_operation_activity(repo_index);
            return Some(RepositoryActionBanner {
                title,
                detail,
                stroke_color: self.color_primary_accent(),
                button_label: self.t("Display update view"),
                button_fill: self.color_action_info(),
                action: RepositoryActionBannerAction::UpdateView,
                activity: Some(
                    ActivityBar::new(progress)
                        .fill(self.color_primary_accent())
                        .motion(motion)
                        .height(VerificationMap::HEIGHT),
                ),
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
                activity: None,
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
            activity: None,
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
            failed: !banner.success,
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
            failed: !banner.success,
        })
    }
}

impl Foxy {
    /// "M min" or "S s" for the hash benchmark ETA; coarse on purpose, the
    /// rate is a sample and the number is a hint, not a countdown.
    pub(crate) fn format_hash_eta(remaining_bytes: u64, bytes_per_sec: u64) -> String {
        let seconds = remaining_bytes.div_ceil(bytes_per_sec.max(1));
        if seconds >= 90 {
            format!("{} min", seconds.div_ceil(60))
        } else {
            format!("{seconds} s")
        }
    }

    /// Fraction of the running hash pass, for the check banner and the
    /// benchmark progress sample.
    pub(crate) fn recheck_hash_progress_fraction(&self) -> Option<f32> {
        hash_progress_fraction(
            self.recheck_hash_byte_counter,
            self.recheck_hash_part_counter,
            self.recheck_hash_counter,
        )
    }

    pub(crate) fn floored_recheck_hash_fraction(&self) -> Option<f32> {
        self.recheck_hash_progress_fraction()
            .map(|fraction| above_floor(self.recheck_progress_floor, fraction))
    }

    /// The check banner's bar: the hash pass over what the stages before it
    /// left, else the stage percent, never below the highest value already shown.
    pub(crate) fn recheck_progress_fraction(&self) -> Option<f32> {
        held_progress(
            self.floored_recheck_hash_fraction().or_else(|| {
                self.recheck_stage_percent
                    .map(|percent| percent.clamp(0.0, 1.0))
            }),
            self.recheck_progress_peak,
        )
    }
}

/// Parts when the scan reports them, else files, else the stage percent.
fn quick_scan_progress(state: &QuickScanProgressState) -> Option<f32> {
    let ratio = |done: usize, total: usize| {
        (total > 0).then(|| (done as f32 / total as f32).clamp(0.0, 1.0))
    };
    if let Some((checked_parts, total_parts)) = state.hash_part_counter {
        ratio(checked_parts, total_parts)
    } else if let Some((checked, total)) = state.hash_counter {
        ratio(checked, total)
    } else {
        state.stage_percent.map(|percent| percent.clamp(0.0, 1.0))
    }
}

fn above_floor(floor: Option<f32>, fraction: f32) -> f32 {
    let floor = floor.unwrap_or(0.0).clamp(0.0, 1.0);
    floor + (1.0 - floor) * fraction.clamp(0.0, 1.0)
}

fn held_progress(current: Option<f32>, peak: Option<f32>) -> Option<f32> {
    match (current, peak) {
        (Some(current), Some(peak)) => Some(current.max(peak)),
        (current, peak) => current.or(peak),
    }
}

/// Bytes when the hasher reports them, else parts, else files: heavy
/// archives hash first, so a parts count runs far ahead of the work done.
fn hash_progress_fraction(
    bytes: Option<(u64, u64)>,
    parts: Option<(usize, usize)>,
    files: Option<(usize, usize)>,
) -> Option<f32> {
    let ratio =
        |done: f64, total: f64| (total > 0.0).then(|| (done / total).clamp(0.0, 1.0) as f32);
    bytes
        .and_then(|(done, total)| ratio(done as f64, total as f64))
        .or_else(|| parts.and_then(|(done, total)| ratio(done as f64, total as f64)))
        .or_else(|| files.and_then(|(done, total)| ratio(done as f64, total as f64)))
}

#[cfg(test)]
mod hash_eta_tests {
    use super::Foxy;

    #[test]
    fn hash_eta_reports_minutes_for_long_runs_and_seconds_for_short_ones() {
        assert_eq!(Foxy::format_hash_eta(86_700_000_000, 107_000_000), "14 min");
        assert_eq!(Foxy::format_hash_eta(50_000_000, 100_000_000), "1 s");
        assert_eq!(Foxy::format_hash_eta(1_000, 0), "17 min");
    }

    #[test]
    fn the_hash_pass_fills_the_bar_from_where_the_stages_left_it() {
        use super::above_floor;
        assert_eq!(above_floor(Some(0.2), 0.0), 0.2);
        assert_eq!(above_floor(Some(0.2), 0.5), 0.6);
        assert_eq!(above_floor(Some(0.2), 1.0), 1.0);
        assert_eq!(above_floor(None, 0.25), 0.25);
    }

    #[test]
    fn check_progress_never_falls_below_the_hash_peak() {
        use super::held_progress;
        assert_eq!(held_progress(Some(0.86), Some(0.9998)), Some(0.9998));
        assert_eq!(held_progress(Some(0.2), Some(0.9998)), Some(0.9998));
        assert_eq!(held_progress(Some(1.0), Some(0.9998)), Some(1.0));
        assert_eq!(held_progress(Some(0.3), None), Some(0.3));
        assert_eq!(held_progress(None, Some(0.5)), Some(0.5));
        assert_eq!(held_progress(None, None), None);
    }

    #[test]
    fn hash_progress_prefers_bytes_then_parts_then_files() {
        use super::hash_progress_fraction;
        assert_eq!(
            hash_progress_fraction(Some((25, 100)), Some((90, 100)), Some((1, 10))),
            Some(0.25)
        );
        assert_eq!(
            hash_progress_fraction(Some((0, 0)), Some((90, 100)), Some((1, 10))),
            Some(0.9)
        );
        assert_eq!(hash_progress_fraction(None, None, Some((1, 10))), Some(0.1));
        assert_eq!(
            hash_progress_fraction(Some((150, 100)), None, None),
            Some(1.0)
        );
        assert_eq!(hash_progress_fraction(None, Some((0, 0)), None), None);
    }

    #[test]
    fn quick_scan_progress_prefers_parts_then_files_then_the_stage() {
        use super::quick_scan_progress;
        let mut state = crate::ui::app::QuickScanProgressState::new(std::time::Instant::now());
        assert_eq!(quick_scan_progress(&state), None);
        state.stage_percent = Some(0.3);
        assert_eq!(quick_scan_progress(&state), Some(0.3));
        state.hash_counter = Some((1, 4));
        assert_eq!(quick_scan_progress(&state), Some(0.25));
        state.hash_part_counter = Some((3, 4));
        assert_eq!(quick_scan_progress(&state), Some(0.75));
        state.hash_part_counter = Some((0, 0));
        assert_eq!(quick_scan_progress(&state), None);
    }
}
