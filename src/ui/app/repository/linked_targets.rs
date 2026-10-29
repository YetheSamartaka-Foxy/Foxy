use eframe::egui;
use log::warn;

use crate::core::api::{self, SyncMode};
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
        self.pending_link_target_sync = Some(intent);
        self.pending_link_target_previews = None;
        let tx = self.link_target_preview_tx.clone();
        std::thread::spawn(move || {
            let result = crate::core::api::background_runtime()
                .map(|runtime| runtime.block_on(api::preview_linked_targets(&path)))
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
                    self.pending_link_target_sync = None;
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
                    self.pending_link_target_sync = None;
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
        egui::Window::new(self.t("Review linked addon targets"))
            .id(egui::Id::unique("linked-target-enrollment"))
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(self.t("This update may write outside the repository folder. Review and enroll each shared target for this update."));
                ui.separator();
                for (index, preview) in previews.iter().enumerate() {
                    ui.push_id(("linked-target-preview", index), |ui| {
                        ui.label(self.t_fmt("Link: {path}", &[("path", preview.link_path.clone())]));
                        ui.label(self.t_fmt("Shared target: {path}", &[("path", preview.target_path.clone())]));
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
                    if ui.button(self.t("Enroll targets and continue update")).clicked() {
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
            self.pending_link_target_sync = None;
            self.pending_link_target_previews = None;
        }
    }

    fn resume_linked_target_sync(&mut self, intent: PendingLinkedTargetSync) {
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
            self.show_error_toast(self.t("The repository changed before the update could start."));
            return;
        };
        self.start_core_sync_internal(
            repo_index,
            intent.mode,
            intent.selected_mod_states,
            intent.force_redownload,
            intent.prepare_download_plan,
            intent.force_full_downloads,
        );
        self.link_target_preview_bypass_once = false;
        self.enrolled_link_targets.clear();
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
