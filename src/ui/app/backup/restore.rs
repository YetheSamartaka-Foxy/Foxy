use log::{info, warn};

use crate::core::api::SyncMode;
use crate::core::utils::addon_backup;
use crate::ui::app::{
    AddonBackupRestoreConsent, AddonBackupRestoreState, AddonBackupTaskAction,
    AddonBackupTaskResult, AddonBackupTaskStatus, BackupOriginConfirmation, Foxy,
};

impl Foxy {
    pub fn open_addon_backup_restore_selector(
        &mut self,
        repo_index: usize,
        addon_name: &str,
        addon_path: Option<&str>,
    ) -> bool {
        let Some(repo) = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .cloned()
        else {
            warn!(
                "Addon restore selector ignored: invalid repository index {}",
                repo_index
            );
            return false;
        };

        let Some(backup_root) = self.configured_backup_directory() else {
            let message = self.t("Addon backup directory is not configured.");
            self.set_addon_backup_notice(repo_index, false, message.clone());
            warn!("{}", message);
            return false;
        };

        let destination_path = match self.resolve_repo_addon_path(&repo, addon_name, addon_path) {
            Ok(path) => path,
            Err(message) => {
                self.set_addon_backup_notice(repo_index, false, message.clone());
                warn!("{}", message);
                return false;
            }
        };

        let mut backups = match addon_backup::list_addon_backups(&backup_root, addon_name) {
            Ok(backups) => backups,
            Err(err) => {
                let message = format!(
                    "Failed to load addon backups for {}: {}",
                    addon_name.trim(),
                    err
                );
                self.set_addon_backup_notice(repo_index, false, message.clone());
                warn!("{}", message);
                return false;
            }
        };

        let expected_origin = addon_backup::AddonBackupOrigin {
            game_space_id: crate::core::game::spaces::active_game_space().space_id,
            repository_url: repo.address.clone(),
            local_path: repo.path.clone(),
        };
        backups.sort_by_key(|backup| !backup.origins.contains(&expected_origin));

        if backups.is_empty() {
            let message = self.t_fmt(
                "No addon backups were found for {name}.",
                &[("name", addon_name.trim().to_string())],
            );
            self.set_addon_backup_notice(repo_index, false, message.clone());
            info!("{}", message);
            return false;
        }

        self.addon_backup_restore_state = Some(AddonBackupRestoreState {
            repo_index,
            repository_url: repo.address.clone(),
            repository_path: repo.path.clone(),
            addon_name: addon_name.trim().to_string(),
            addon_path: addon_path.map(str::to_string),
            destination_path,
            backups,
            selected_backup_index: 0,
            origin_consent: None,
            pending_origin_confirmation: None,
            retry_arbitration: false,
            allow_retry: false,
        });
        self.addon_backup_notice = None;
        self.needs_repaint = true;
        true
    }

    pub fn start_manual_addon_restore(&mut self, restore_state: AddonBackupRestoreState) -> bool {
        if self.pending_retry_matches_instance(
            &restore_state.repository_url,
            &restore_state.repository_path,
        ) && !restore_state.retry_arbitration
        {
            let mut pending = restore_state;
            pending.retry_arbitration = true;
            self.addon_backup_restore_state = Some(pending);
            self.needs_repaint = true;
            return true;
        }
        if self.repository_sync_active()
            || self.is_direct_download_running()
            || self.has_addon_backup_task_running()
            || self.backup_cleanup_in_flight
        {
            warn!("Manual addon restore ignored: background work is currently active");
            self.addon_backup_restore_state = Some(restore_state);
            self.show_error_toast(
                self.t("Wait for the current update or backup task to finish before restoring."),
            );
            return false;
        }

        let Some((repo_index, repo)) = self
            .repository_view_state
            .repositories
            .iter()
            .enumerate()
            .find(|(_, repo)| {
                repo.address == restore_state.repository_url
                    && repo.path == restore_state.repository_path
            })
            .map(|(index, repo)| (index, repo.clone()))
        else {
            self.addon_backup_restore_state = None;
            warn!("Manual addon restore ignored: repository instance changed or was removed");
            return false;
        };

        let destination = match self.resolve_repo_addon_path(
            &repo,
            &restore_state.addon_name,
            restore_state.addon_path.as_deref(),
        ) {
            Ok(path) => path,
            Err(message) => {
                self.set_addon_backup_notice(restore_state.repo_index, false, message.clone());
                warn!("{}", message);
                return false;
            }
        };
        if destination != restore_state.destination_path {
            self.addon_backup_restore_state = None;
            let message = self.t(
                "The restore destination changed. Reopen the backup selector and review it again.",
            );
            self.set_addon_backup_notice(repo_index, false, message.clone());
            warn!("{}", message);
            return false;
        }

        if restore_state.retry_arbitration {
            self.addon_backup_restore_state = Some(restore_state);
            return true;
        }

        let Some(selected_backup) = restore_state
            .backups
            .get(restore_state.selected_backup_index)
            .cloned()
        else {
            let message = self.t("The selected addon backup is no longer available.");
            self.set_addon_backup_notice(restore_state.repo_index, false, message.clone());
            warn!("{}", message);
            return false;
        };

        let Some(backup_root) = self.configured_backup_directory() else {
            self.addon_backup_restore_state = None;
            return false;
        };
        let current_backups =
            match addon_backup::list_addon_backups(&backup_root, &restore_state.addon_name) {
                Ok(backups) => backups,
                Err(err) => {
                    self.addon_backup_restore_state = None;
                    let message = self.t_fmt(
                        "Failed to load addon backups for {name}: {error}",
                        &[
                            ("name", restore_state.addon_name.clone()),
                            ("error", err.to_string()),
                        ],
                    );
                    self.set_addon_backup_notice(repo_index, false, message.clone());
                    warn!("{}", message);
                    return false;
                }
            };
        let Some(current_backup) = current_backups
            .iter()
            .find(|backup| backup.path == selected_backup.path)
        else {
            self.addon_backup_restore_state = None;
            let message = self.t("The selected addon backup is no longer available.");
            self.set_addon_backup_notice(repo_index, false, message.clone());
            warn!("{}", message);
            return false;
        };
        if current_backup != &selected_backup {
            self.addon_backup_restore_state = None;
            let message = self.t(
                "The selected addon backup changed. Reopen the backup selector and review it again.",
            );
            self.set_addon_backup_notice(repo_index, false, message.clone());
            warn!("{}", message);
            return false;
        }
        let selected_backup = current_backup.clone();

        let expected = addon_backup::AddonBackupOrigin {
            game_space_id: crate::core::game::spaces::active_game_space().space_id,
            repository_url: repo.address.clone(),
            local_path: repo.path.clone(),
        };
        let confirmation = if selected_backup.origins.is_empty() {
            Some(BackupOriginConfirmation::UnknownLegacyOrigin)
        } else if selected_backup.origins.contains(&expected) {
            None
        } else {
            Some(BackupOriginConfirmation::CrossOrigin)
        };
        if let Some(confirmation) = confirmation {
            let consent = AddonBackupRestoreConsent {
                game_space_id: expected.game_space_id.clone(),
                repository_url: repo.address.clone(),
                repository_path: repo.path.clone(),
                destination_path: destination.clone(),
                backup_path: selected_backup.path.clone(),
                content_hash: selected_backup.content_hash.clone(),
                origins: selected_backup.origins.clone(),
            };
            if restore_state.origin_consent.as_ref() != Some(&consent) {
                let mut pending = restore_state;
                pending.origin_consent = None;
                pending.pending_origin_confirmation = Some(confirmation);
                pending.repo_index = repo_index;
                self.addon_backup_restore_state = Some(pending);
                self.needs_repaint = true;
                return true;
            }
        }

        let addon_name = restore_state.addon_name.clone();
        self.addon_backup_restore_state = None;
        self.addon_backup_notice = None;
        self.addon_backup_status = Some(AddonBackupTaskStatus {
            repo_index,
            status_text: self.t_fmt(
                "Restoring addon backup {name}...",
                &[("name", addon_name.clone())],
            ),
        });
        let task_tx = self.addon_backup_task_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        self.addon_backup_worker = Some(std::thread::spawn(move || {
            let result = addon_backup::restore_addon_backup(&selected_backup, &destination)
                .map(|_| AddonBackupTaskResult {
                    repo_index,
                    action: AddonBackupTaskAction::Restore,
                    addon_name: addon_name.clone(),
                    success: true,
                    content_hash: Some(selected_backup.content_hash),
                    error_message: None,
                    trigger_recheck: true,
                })
                .unwrap_or_else(|err| AddonBackupTaskResult {
                    repo_index,
                    action: AddonBackupTaskAction::Restore,
                    addon_name,
                    success: false,
                    content_hash: None,
                    error_message: Some(err.to_string()),
                    trigger_recheck: false,
                });
            if task_tx.send(result).is_ok() {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        }));
        self.needs_repaint = true;
        true
    }

    pub fn render_pending_addon_backup_origin_confirmation(&mut self, ctx: &eframe::egui::Context) {
        let Some(state) = self
            .addon_backup_restore_state
            .clone()
            .filter(|state| state.pending_origin_confirmation.is_some())
        else {
            return;
        };
        let Some(backup) = state.backups.get(state.selected_backup_index) else {
            self.addon_backup_restore_state = None;
            return;
        };
        let Some((repo_index, repo)) = self
            .repository_view_state
            .repositories
            .iter()
            .enumerate()
            .find(|(_, repo)| {
                repo.address == state.repository_url && repo.path == state.repository_path
            })
        else {
            self.addon_backup_restore_state = None;
            return;
        };
        let destination = self
            .resolve_repo_addon_path(repo, &state.addon_name, state.addon_path.as_deref())
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| repo.path.clone());
        if destination != state.destination_path.display().to_string() {
            self.addon_backup_restore_state = None;
            let message = self.t(
                "The restore destination changed. Reopen the backup selector and review it again.",
            );
            self.set_addon_backup_notice(repo_index, false, message.clone());
            return;
        }
        let source = if backup.origins.is_empty() {
            self.t("Unknown legacy backup source")
        } else {
            backup
                .origins
                .iter()
                .map(|origin| format!("{} ({})", origin.repository_url, origin.local_path))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let title = match state.pending_origin_confirmation.as_ref() {
            Some(BackupOriginConfirmation::CrossOrigin) => {
                self.t("Confirm cross-repository restore")
            }
            Some(BackupOriginConfirmation::UnknownLegacyOrigin) => {
                self.t("Confirm legacy backup restore")
            }
            None => return,
        };
        let message = match state.pending_origin_confirmation.as_ref() {
            Some(BackupOriginConfirmation::CrossOrigin) => self.t("This backup comes from another repository. Review the source and destination before restoring."),
            Some(BackupOriginConfirmation::UnknownLegacyOrigin) => self.t("This legacy backup has no recorded source. Review the destination before restoring."),
            None => return,
        };
        let default_activation = ctx.memory(|memory| memory.focused().is_none());
        let (confirm_key, cancel_key) = ctx.input_mut(|input| {
            (
                default_activation && input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        let mut confirmed = confirm_key;
        let mut cancelled = cancel_key;
        egui::Window::new(title)
            .frame(self.modal_window_chrome(ctx))
            .title_frame(self.modal_window_chrome(ctx))
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(message);
                ui.label(self.t_fmt("Source: {source}", &[("source", source.clone())]));
                ui.label(self.t_fmt(
                    "Destination: {destination}",
                    &[("destination", destination.clone())],
                ));
                ui.horizontal(|ui| {
                    if ui.button(self.t("Restore to this destination")).clicked() {
                        confirmed = true;
                    }
                    if ui.button(self.t("Cancel")).clicked() {
                        cancelled = true;
                    }
                });
            });
        if confirmed {
            let mut resumed = state;
            let Some(backup) = resumed.backups.get(resumed.selected_backup_index).cloned() else {
                self.addon_backup_restore_state = None;
                return;
            };
            resumed.origin_consent = Some(AddonBackupRestoreConsent {
                game_space_id: crate::core::game::spaces::active_game_space().space_id,
                repository_url: resumed.repository_url.clone(),
                repository_path: resumed.repository_path.clone(),
                destination_path: resumed.destination_path.clone(),
                backup_path: backup.path,
                content_hash: backup.content_hash,
                origins: backup.origins,
            });
            resumed.pending_origin_confirmation = None;
            self.addon_backup_restore_state = Some(resumed.clone());
            self.start_manual_addon_restore(resumed);
        } else if cancelled {
            self.addon_backup_restore_state = None;
        }
    }

    pub(in crate::ui::app) fn schedule_post_restore_repo_verification(
        &mut self,
        repo_index: usize,
    ) {
        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            warn!(
                "Post-restore verification skipped: invalid repository index {}",
                repo_index
            );
            return;
        };

        if self.syncing_repository.is_some() {
            info!(
                "Queueing post-restore quick scan for repo {} while sync is active",
                repo.name
            );
            self.queue_quick_scan_for_urls_with_flags(vec![repo.address.clone()], false, true);
            return;
        }

        self.start_core_sync(repo_index, SyncMode::RecheckOnly);
    }
}
