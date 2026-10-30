use eframe::egui;
use log::warn;

use crate::core::api::SyncMode;
use crate::ui::app::{Foxy, LinkedTargetPreviewResult, PendingLinkedTargetSync};
use crate::ui::types::{Repository, sanitize_user_path};

impl Foxy {
    pub(crate) fn start_linked_target_preview(
        &mut self,
        repo: &Repository,
        intent: PendingLinkedTargetSync,
    ) {
        let address = repo.address.clone();
        let path = sanitize_user_path(&repo.path);
        let shared_root = repo.repository_space_id.as_deref().and_then(|space_id| {
            self.repository_spaces
                .iter()
                .find(|space| space.id == space_id)
                .map(|space| sanitize_user_path(&space.shared_path))
                .filter(|shared| {
                    !shared.trim().is_empty()
                        && crate::core::utils::content_hash::normalize_path(shared)
                            != crate::core::utils::content_hash::normalize_path(&path)
                })
        });
        let space_dir = Self::get_game_space_directory();
        self.pending_link_target_sync = Some(intent);
        self.pending_link_target_previews = None;
        let tx = self.link_target_preview_tx.clone();
        std::thread::spawn(move || {
            let result = crate::core::api::background_runtime()
                .map(|runtime| {
                    runtime.block_on(
                        crate::core::api::check_contracts::preview_linked_targets_in_space(
                            &path,
                            shared_root.as_deref(),
                            &space_dir,
                        ),
                    )
                })
                .unwrap_or_else(|| Err("Background runtime is unavailable".to_owned()));
            let _ = tx.send(LinkedTargetPreviewResult {
                repo_address: address,
                repo_path: path,
                result,
            });
        });
    }

    pub(crate) fn poll_linked_target_preview_results(&mut self) {
        loop {
            let result = match self.link_target_preview_rx.try_recv() {
                Ok(result) => result,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if let Some(intent) = self.pending_link_target_sync.take() {
                        self.settle_linked_target_bulk(&intent, None);
                    }
                    self.show_error_toast(
                        self.t("Linked-folder inspection stopped. Retry the update."),
                    );
                    break;
                }
            };
            let matches_pending = self
                .pending_link_target_sync
                .as_ref()
                .is_some_and(|pending| {
                    Self::normalize_repo_url(&pending.repo_address)
                        == Self::normalize_repo_url(&result.repo_address)
                        && Self::repo_instance_path_key(&pending.repo_path)
                            == Self::repo_instance_path_key(&result.repo_path)
                });
            if !matches_pending {
                continue;
            }
            match result.result {
                Ok(previews) if previews.is_empty() => {
                    if let Some(intent) = self.pending_link_target_sync.take() {
                        self.link_target_preview_bypass_once = true;
                        self.resume_linked_target_sync(intent);
                    }
                }
                Ok(previews) => {
                    self.pending_link_target_previews = Some(previews);
                    self.needs_repaint = true;
                }
                Err(error) => {
                    warn!("Linked-folder inspection failed: {error}");
                    if let Some(intent) = self.pending_link_target_sync.take() {
                        self.settle_linked_target_bulk(&intent, None);
                    }
                    self.pending_link_target_previews = None;
                    self.show_error_toast(error);
                }
            }
        }
    }

    pub(crate) fn render_link_target_enrollment_prompt(&mut self, ctx: &egui::Context) {
        let Some(previews) = self.pending_link_target_previews.clone() else {
            return;
        };
        let mut open = true;
        let mut approve = false;
        let mut cancel = false;
        egui::Window::new(self.t("Review update targets"))
            .id(egui::Id::unique("linked-target-enrollment"))
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                if previews.iter().any(|preview| !preview.storage_changed) {
                    ui.label(self.t("This update may write outside the repository folder. Review and approve each listed target before continuing."));
                }
                if previews.iter().any(|preview| preview.storage_changed) {
                    ui.label(self.t("At least one target uses a different storage device than before. Review the affected path before writing."));
                }
                ui.separator();
                for (index, preview) in previews.iter().enumerate() {
                    ui.push_id(("linked-target-preview", index), |ui| {
                        if preview.storage_changed {
                            ui.label(self.t_fmt("Storage device changed at: {path}", &[("path", preview.target_path.clone())]));
                        } else {
                            ui.label(self.t_fmt("Link: {path}", &[("path", preview.link_path.clone())]));
                        }
                        if !preview.storage_changed {
                            ui.label(self.t_fmt("Shared target: {path}", &[("path", preview.target_path.clone())]));
                        }
                        if preview.affected_repositories.is_empty() {
                            ui.label(self.t("No other configured installation was found for this target."));
                        } else {
                            ui.label(self.t_fmt(
                                "Affected installations: {repositories}",
                                &[("repositories", preview.affected_repositories.join(", "))],
                            ));
                        }
                    });
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    if ui.button(self.t("Approve targets and continue update")).clicked() {
                        approve = true;
                    }
                    if ui.button(self.t("Cancel update")).clicked() {
                        cancel = true;
                    }
                });
            });
        if approve {
            if let Some(intent) = self.pending_link_target_sync.take() {
                self.pending_link_target_previews = None;
                self.enrolled_link_targets = previews;
                self.link_target_preview_bypass_once = true;
                self.resume_linked_target_sync(intent);
            }
        } else if cancel || !open {
            if let Some(intent) = self.pending_link_target_sync.take() {
                self.settle_linked_target_bulk(&intent, None);
            }
            self.pending_link_target_previews = None;
        }
    }

    fn resume_linked_target_sync(&mut self, mut intent: PendingLinkedTargetSync) {
        let Some(repo_index) = self
            .repository_view_state
            .repositories
            .iter()
            .position(|repo| {
                Self::normalize_repo_url(&repo.address)
                    == Self::normalize_repo_url(&intent.repo_address)
                    && Self::repo_instance_path_key(&repo.path)
                        == Self::repo_instance_path_key(&intent.repo_path)
            })
        else {
            self.enrolled_link_targets.clear();
            self.link_target_preview_bypass_once = false;
            self.settle_linked_target_bulk(&intent, None);
            self.show_error_toast(self.t("The repository changed before the update could start."));
            return;
        };
        let bulk_target_matches = self
            .repository_space_bulk_progress
            .as_ref()
            .and_then(|progress| progress.pending_target.as_ref())
            .is_some_and(|target| {
                Self::normalize_repo_url(&target.remote_url)
                    == Self::normalize_repo_url(&intent.repo_address)
                    && Self::repo_instance_path_key(&target.local_path)
                        == Self::repo_instance_path_key(&intent.repo_path)
            });
        if bulk_target_matches
            && self
                .repository_space_bulk_progress
                .as_ref()
                .is_some_and(|progress| {
                    self.repository_view_state.repositories[repo_index]
                        .repository_space_id
                        .as_deref()
                        != Some(progress.space_id.as_str())
                })
        {
            self.settle_linked_target_bulk(&intent, None);
            self.show_error_toast(self.t("The repository changed before the update could start."));
            return;
        }
        self.start_core_sync_internal(
            repo_index,
            intent.mode,
            intent.selected_mod_states.take(),
            intent.force_redownload,
            intent.prepare_download_plan,
            intent.force_full_downloads,
        );
        self.settle_linked_target_bulk(&intent, Some(repo_index));
        self.link_target_preview_bypass_once = false;
        self.enrolled_link_targets.clear();
    }

    fn settle_linked_target_bulk(
        &mut self,
        intent: &PendingLinkedTargetSync,
        started_repo_index: Option<usize>,
    ) {
        let target_matches = self
            .repository_space_bulk_progress
            .as_ref()
            .and_then(|progress| progress.pending_target.as_ref())
            .is_some_and(|target| {
                Self::normalize_repo_url(&target.remote_url)
                    == Self::normalize_repo_url(&intent.repo_address)
                    && Self::repo_instance_path_key(&target.local_path)
                        == Self::repo_instance_path_key(&intent.repo_path)
            });
        if !target_matches {
            return;
        }
        if started_repo_index.is_some_and(|index| self.syncing_repository == Some(index)) {
            if let Some(progress) = self.repository_space_bulk_progress.as_mut() {
                progress.active_target = progress.pending_target.take();
            }
            self.sync_is_bulk = true;
            self.refresh_repository_space_bulk_current_repo();
        } else {
            self.settle_pending_repository_space_target();
        }
    }

    pub(crate) fn maybe_start_linked_target_preview(
        &mut self,
        repo: &Repository,
        mode: SyncMode,
        selected_mod_states: Option<Vec<(String, bool)>>,
        force_redownload: bool,
        prepare_download_plan: bool,
        force_full_downloads: bool,
    ) -> bool {
        if mode != SyncMode::Download {
            return false;
        }
        if self.link_target_preview_bypass_once {
            self.link_target_preview_bypass_once = false;
            return false;
        }
        if !self.enrolled_link_targets.is_empty() {
            return false;
        }
        if self.pending_link_target_sync.is_some() {
            return true;
        }
        self.start_linked_target_preview(
            repo,
            PendingLinkedTargetSync {
                repo_address: repo.address.clone(),
                repo_path: sanitize_user_path(&repo.path),
                mode,
                selected_mod_states,
                force_redownload,
                prepare_download_plan,
                force_full_downloads,
            },
        );
        true
    }
}
