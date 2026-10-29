use eframe::egui;

use crate::core::api;
use crate::ui::app::Foxy;

pub(crate) type UpdateRecoveryResult = Result<Vec<(String, String)>, String>;

impl Foxy {
    pub(super) fn poll_update_recovery(&mut self) {
        let result = self
            .startup_recovery_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Recovery worker stopped".to_string()))
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = result {
            self.startup_recovery_rx = None;
            match result {
                Ok(instances) => self.interrupted_update_instances.extend(
                    instances
                        .into_iter()
                        .map(|(url, path)| Self::repo_instance_key(&url, &path)),
                ),
                Err(err) => {
                    log::error!("Update recovery remains incomplete: {err}");
                    self.interrupted_update_instances.extend(crate::core::tasks::download_files::UpdateRollbackSession::recovery_notices(&Self::get_game_space_directory())
                        .into_iter().map(|(url, path)| Self::repo_instance_key(&url, &path)));
                    self.show_error_toast(self.t("Update recovery is incomplete. Restore access to the affected files and restart Foxy before updating."));
                }
            }
        }
    }
    pub(crate) fn note_successful_remote_check(&mut self, url: &str, path: &str) {
        let Some(idx) = self.repo_index_by_instance(
            &Self::normalize_repo_url(url),
            &Self::repo_instance_path_key(path),
        ) else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        self.repository_view_state.repositories[idx].last_remote_check_at = Some(now);
        self.save_repositories();
    }

    pub(crate) fn render_repository_recovery_status(&mut self, ui: &mut egui::Ui, idx: usize) {
        let Some(repo) = self.repository_view_state.repositories.get(idx).cloned() else {
            return;
        };
        if let Some((address, path, repository_empty, addons)) = &self.empty_publication_status
            && *address == Self::normalize_repo_url(&repo.address)
            && *path == Self::repo_instance_path_key(&repo.path)
        {
            if *repository_empty {
                ui.label(
                    self.t("This repository has no published content. Local files are retained."),
                );
            } else {
                ui.label(self.t_fmt(
                    "No published content in {addons}. Local files are retained.",
                    &[("addons", addons.join(", "))],
                ));
            }
            if ui
                .add_enabled(
                    !self.retained_files_cleanup_in_flight,
                    egui::Button::new(self.t("Review removal of retained files")),
                )
                .clicked()
            {
                self.start_retained_file_preview(&repo.address, &repo.path);
            }
        }
        if self
            .interrupted_update_instances
            .contains(&Self::repo_instance_key(&repo.address, &repo.path))
        {
            ui.label(self.t("An update was interrupted. Check or resume it before treating this installation as verified."));
        }
        if self.is_repository_paused(&repo) {
            ui.horizontal_wrapped(|ui| {
                ui.label(self.t("Update paused. Resume to check the latest plan and continue."));
                if ui.button(self.t("Resume")).clicked() {
                    self.resume_paused_repository(idx);
                }
            });
        }
        if self.startup_probe_unknown.iter().any(|unknown| {
            Self::repo_instance_key(&unknown.repo_url, &unknown.local_path)
                == Self::repo_instance_key(&repo.address, &repo.path)
        }) {
            ui.label(self.t("Server not checked. Local status uses cached metadata."));
        }
    }

    pub(crate) fn render_repository_check_time(&self, ui: &mut egui::Ui, idx: usize) {
        let Some(repo) = self.repository_view_state.repositories.get(idx) else {
            return;
        };
        let last_remote = repo
            .last_remote_check_at
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds as i64, 0))
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| self.t("Unknown"));
        let remote_label = self.t_fmt(
            "Last successful server check: {time}",
            &[("time", last_remote.clone())],
        );
        let response = ui.add(
            egui::Label::new(
                egui::RichText::new(format!("\u{21bb}  {last_remote}"))
                    .color(self.color_text_dim()),
            )
            .sense(egui::Sense::focusable_noninteractive()),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &remote_label));
        let tooltip = if response.has_focus() {
            ui.painter().rect_stroke(
                response.rect.expand(2.0),
                egui::CornerRadius::same(2),
                ui.visuals().selection.stroke,
                egui::StrokeKind::Outside,
            );
            egui::Tooltip::for_widget(&response)
        } else {
            egui::Tooltip::for_enabled(&response)
        };
        tooltip.show(|ui| {
            ui.strong(remote_label);
            ui.separator();
            self.render_repository_check_details(ui, repo);
        });
    }

    fn render_repository_check_details(
        &self,
        ui: &mut egui::Ui,
        repo: &crate::ui::types::Repository,
    ) {
        let last_check = repo
            .last_check_at
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds as i64, 0))
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| self.t("Never"));
        ui.label(self.t_fmt("Last completed check: {time}", &[("time", last_check)]));
        if repo.last_check_at.is_some() {
            let local_basis = if repo.last_check_full {
                self.t("Full local read")
            } else {
                self.t("Selected local files")
            };
            let remote_basis = if repo.last_check_remote_fresh {
                self.t("Fresh remote metadata")
            } else {
                self.t("Cached remote metadata")
            };
            ui.label(self.t_fmt(
                "Last check basis: {local}; {remote}",
                &[("local", local_basis), ("remote", remote_basis)],
            ));
        }
        if !repo.last_check_remote_fresh {
            ui.label(self.t("Remote metadata may be out of date; local status uses cached data."));
        }
        let last_full = repo
            .last_full_verification_at
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds as i64, 0))
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| self.t("Never"));
        ui.label(self.t_fmt(
            "Last full integrity verification: {time}",
            &[("time", last_full)],
        ));
    }

    fn start_retained_file_preview(&mut self, repo_url: &str, local_path: &str) {
        if self.retained_files_cleanup_in_flight
            || self.database_updates_blocked()
            || self.repository_sync_active()
            || self.quick_scan_worker.is_some()
            || self.is_direct_download_running()
            || !self.pending_repository_db_wipes.is_empty()
            || !self.pending_addon_deletes.is_empty()
            || self.scheduler_active_run.is_some()
            || self.scheduler_pending_post_action.is_some()
        {
            self.show_error_toast(
                self.t("Finish active checks and file operations before reviewing retained files."),
            );
            return;
        }

        self.retained_files_cleanup_in_flight = true;
        let repo_url = repo_url.to_string();
        let local_path = crate::ui::types::sanitize_user_path(local_path);
        let tx = self.retained_file_preview_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let result = crate::core::api::background_runtime()
                .map(|runtime| {
                    runtime.block_on(async {
                        let files =
                            api::preview_retained_empty_files(&repo_url, &local_path).await?;
                        let linked_targets = api::preview_linked_targets(&local_path).await?;
                        Ok((files, linked_targets))
                    })
                })
                .unwrap_or_else(|| Err("Background runtime is unavailable".to_owned()));
            if tx
                .send(crate::ui::app::RetainedFilePreviewResult {
                    repo_url,
                    local_path,
                    result,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });
    }

    pub(crate) fn poll_retained_file_task_results(&mut self) {
        loop {
            match self.retained_file_preview_rx.try_recv() {
                Ok(result) => {
                    self.retained_files_cleanup_in_flight = false;
                    match result.result {
                        Ok((files, _)) if files.is_empty() => {
                            self.show_success_toast(
                                self.t("No retained files are available for removal."),
                            );
                        }
                        Ok((files, linked_targets)) => {
                            self.pending_retained_file_removal =
                                Some(crate::ui::app::PendingRetainedFileRemoval {
                                    repo_url: result.repo_url,
                                    local_path: result.local_path,
                                    files,
                                    linked_targets,
                                });
                        }
                        Err(error) => self.show_error_toast(error),
                    }
                    self.needs_repaint = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.retained_files_cleanup_in_flight = false;
                    break;
                }
            }
        }

        loop {
            match self.retained_file_removal_rx.try_recv() {
                Ok(result) => {
                    self.retained_files_cleanup_in_flight = false;
                    match result.result {
                        Ok(count) => {
                            let clears_empty_status = self
                                .empty_publication_status
                                .as_ref()
                                .is_some_and(|(url, path, _, _)| {
                                    *url == Self::normalize_repo_url(&result.repo_url)
                                        && *path == Self::repo_instance_path_key(&result.local_path)
                                });
                            if clears_empty_status {
                                self.empty_publication_status = None;
                            }
                            self.show_success_toast(self.t_fmt(
                                "Removed {count} retained files",
                                &[("count", count.to_string())],
                            ));
                        }
                        Err(error) => self.show_error_toast(error),
                    }
                    self.needs_repaint = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.retained_files_cleanup_in_flight = false;
                    break;
                }
            }
        }
    }

    pub(crate) fn render_retained_file_removal_confirmation(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_retained_file_removal.clone() else {
            return;
        };
        let mut open = true;
        let mut remove = false;
        let mut cancel = false;
        egui::Window::new(self.t("Review retained file removal"))
            .id(egui::Id::unique("retained-file-removal"))
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(self.t("Only the listed files will be removed. Local folders and other repository records will be kept."));
                ui.label(self.t("For linked files, the resolved target is shown. Confirming approves these exact targets for this removal only."));
                ui.separator();
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for file in &pending.files {
                        ui.push_id(file.local_path.as_str(), |ui| {
                            ui.label(self.t_fmt(
                                "{addon}: {path}",
                                &[("addon", file.addon_name.clone()), ("path", file.local_path.clone())],
                            ));
                            if file.resolved_path != file.local_path {
                                let matching_links = pending
                                    .linked_targets
                                    .iter()
                                    .filter(|link| {
                                        std::path::Path::new(&file.resolved_path)
                                            .starts_with(&link.target_path)
                                    })
                                    .collect::<Vec<_>>();
                                if !matching_links.is_empty() {
                                    ui.label(self.t_fmt(
                                        "Shared target: {path}",
                                        &[("path", file.resolved_path.clone())],
                                    ));
                                    let mut affected = matching_links
                                        .iter()
                                        .flat_map(|link| link.affected_repositories.iter().cloned())
                                        .collect::<Vec<_>>();
                                    affected.sort();
                                    affected.dedup();
                                    for link in matching_links {
                                        ui.label(self.t_fmt(
                                            "Link: {path}",
                                            &[("path", link.link_path.clone())],
                                        ));
                                    }
                                    if affected.is_empty() {
                                        ui.label(self.t("No other configured installation was found for this target."));
                                    } else {
                                        ui.label(self.t_fmt(
                                            "Affected installations: {repositories}",
                                            &[("repositories", affected.join(", "))],
                                        ));
                                    }
                                }
                            }
                            if file.shared {
                                ui.label(self.t("This file is shared with another repository and cannot be removed."));
                            }
                        });
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.retained_files_cleanup_in_flight
                                && pending.files.iter().any(|file| !file.shared),
                            egui::Button::new(self.t("Remove listed files")),
                        )
                        .clicked()
                    {
                        remove = true;
                    }
                    if ui.button(self.t("Keep files")).clicked() {
                        cancel = true;
                    }
                });
            });

        if remove {
            if self.repository_sync_active()
                || self.database_updates_blocked()
                || self.quick_scan_worker.is_some()
                || self.is_direct_download_running()
                || !self.pending_repository_db_wipes.is_empty()
                || !self.pending_addon_deletes.is_empty()
                || self.scheduler_active_run.is_some()
                || self.scheduler_pending_post_action.is_some()
                || self.retained_files_cleanup_in_flight
            {
                self.show_error_toast(
                    self.t(
                        "Finish active checks and file operations before removing retained files.",
                    ),
                );
                self.pending_retained_file_removal = None;
                return;
            }
            let approved = pending
                .files
                .iter()
                .filter(|file| !file.shared)
                .cloned()
                .collect::<Vec<_>>();
            if approved.is_empty() {
                self.show_error_toast(
                    self.t("No unshared retained files are available for removal."),
                );
                self.pending_retained_file_removal = None;
                return;
            }
            self.retained_files_cleanup_in_flight = true;
            self.pending_retained_file_removal = None;
            let tx = self.retained_file_removal_tx.clone();
            let repaint_ctx = self.repaint_ctx.clone();
            std::thread::spawn(move || {
                let result = crate::core::api::background_runtime()
                    .map(|runtime| {
                        runtime.block_on(api::remove_retained_empty_files(
                            pending.repo_url.clone(),
                            pending.local_path.clone(),
                            approved,
                        ))
                    })
                    .unwrap_or_else(|| Err("Background runtime is unavailable".to_owned()));
                if tx
                    .send(crate::ui::app::RetainedFileRemovalResult {
                        repo_url: pending.repo_url,
                        local_path: pending.local_path,
                        result,
                    })
                    .is_ok()
                {
                    Self::request_background_repaint(repaint_ctx.as_ref());
                }
            });
        } else if cancel || !open {
            self.pending_retained_file_removal = None;
        }
    }
}
