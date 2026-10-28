use std::time::{Duration, Instant};

use eframe::egui;

use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::types::RepoInstanceKey;

pub(crate) struct PendingDownloadRetry {
    target: RepoInstanceKey,
    space_id: Option<String>,
    visual_folder_id: Option<String>,
    scheduled_job_id: Option<String>,
    due: Instant,
    grouped: bool,
}

impl Foxy {
    pub(crate) fn queue_failed_download_retry(&mut self, repo_idx: usize) -> bool {
        if !self.settings_view_state.retry_failed_downloads || self.close_requested_at.is_some() {
            return false;
        }
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return false;
        };
        let key = Self::repo_instance_key(&repo.address, &repo.path);
        let attempts = self
            .download_retry_attempts
            .get(&key)
            .copied()
            .unwrap_or_default();
        let grouped = self.sync_is_bulk
            || self.scheduler_active_run.is_some()
            || self.repository_space_bulk_progress.is_some()
            || !self.repository_visual_folder_sync_queue.is_empty();
        if grouped && attempts >= 1 {
            self.download_retry_attempts.remove(&key);
            return false;
        }
        self.download_retry_attempts.insert(key, attempts + 1);
        let target = RepoInstanceKey {
            remote_url: repo.address.clone(),
            local_path: repo.path.clone(),
        };
        let normalized_url = Self::normalize_repo_url(&target.remote_url);
        let space_id = self
            .repository_space_bulk_progress
            .as_ref()
            .and_then(|progress| {
                (repo.repository_space_id.as_deref() == Some(progress.space_id.as_str())
                    && progress.target_repo_urls.contains(&normalized_url))
                .then(|| progress.space_id.clone())
            });
        let scheduled_job_id = self.scheduled_retry_owner(&target);
        self.pending_download_retry = Some(PendingDownloadRetry {
            target,
            space_id,
            visual_folder_id: self.sync_visual_folder_id.clone(),
            scheduled_job_id,
            due: Instant::now() + Duration::from_secs(30),
            grouped,
        });
        true
    }

    pub(crate) fn cancel_pending_download_retry(&mut self) {
        if let Some(retry) = self.pending_download_retry.take() {
            self.finish_download_retry_owner(&retry, false);
            self.clear_download_retry_attempt(&retry.target);
        }
    }

    fn clear_download_retry_attempt(&mut self, target: &RepoInstanceKey) {
        self.download_retry_attempts
            .remove(&Self::repo_instance_key(
                &target.remote_url,
                &target.local_path,
            ));
    }

    fn finish_download_retry_owner(&mut self, retry: &PendingDownloadRetry, succeeded: bool) {
        let target_url = Self::normalize_repo_url(&retry.target.remote_url);
        if let Some(space_id) = retry.space_id.as_deref() {
            let repo_idx = self.repo_index_by_instance(
                &target_url,
                &Self::repo_instance_path_key(&retry.target.local_path),
            );
            if let Some(repo_idx) = repo_idx {
                self.record_repository_space_bulk_completion(
                    Some(repo_idx),
                    Some(SyncMode::Download),
                    succeeded,
                    false,
                );
            }
            if let Some(progress) =
                self.repository_space_bulk_progress
                    .as_mut()
                    .filter(|progress| {
                        progress.space_id == space_id
                            && progress.target_repo_urls.contains(&target_url)
                            && !progress.completed_repo_urls.contains(&target_url)
                    })
            {
                progress.record(&target_url, succeeded, false);
            }
        }
        if let Some(job_id) = retry.scheduled_job_id.as_deref() {
            self.record_scheduled_retry_completion(&retry.target, job_id, succeeded);
        }
    }

    pub(super) fn service_download_retry(&mut self, ctx: &egui::Context) {
        if self.pending_download_retry.is_none() {
            return;
        }
        if !self.settings_view_state.retry_failed_downloads {
            self.cancel_pending_download_retry();
            return;
        }
        let Some(retry) = self.pending_download_retry.as_ref() else {
            return;
        };
        let remaining = retry
            .due
            .saturating_duration_since(Instant::now())
            .as_secs();
        let mut stop = false;
        egui::Window::new(self.t("Waiting to retry download"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(self.t_fmt(
                    "Temporary download failure. Retrying in {seconds} seconds.",
                    &[("seconds", remaining.to_string())],
                ));
                stop = ui.button(self.t("Cancel retry")).clicked();
            });
        ctx.request_repaint_after(Duration::from_millis(250));
        if stop {
            self.cancel_sync_and_queued_runs();
            return;
        }
        if remaining > 0
            || self.repository_sync_active()
            || self.is_direct_download_running()
            || self
                .backend_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            || self.database_updates_blocked()
            || self.pending_game_space_switch.is_some()
        {
            return;
        }
        let Some(retry) = self.pending_download_retry.take() else {
            return;
        };
        let idx = self
            .repo_index_by_instance(
                &Self::normalize_repo_url(&retry.target.remote_url),
                &Self::repo_instance_path_key(&retry.target.local_path),
            )
            .filter(|&idx| {
                let repo = &self.repository_view_state.repositories[idx];
                retry
                    .space_id
                    .as_deref()
                    .is_none_or(|space_id| repo.repository_space_id.as_deref() == Some(space_id))
                    && retry.visual_folder_id.as_ref().is_none_or(|folder_id| {
                        let key = Self::repo_instance_key(
                            &retry.target.remote_url,
                            &retry.target.local_path,
                        );
                        self.repository_visual_folders.iter().any(|folder| {
                            folder.id == *folder_id && folder.repository_keys.contains(&key)
                        })
                    })
                    && retry.scheduled_job_id.as_ref().is_none_or(|job_id| {
                        self.scheduled_retry_owner(&retry.target).as_ref() == Some(job_id)
                    })
            });
        if let Some(idx) = idx {
            self.start_core_sync(idx, SyncMode::Download);
            if self.syncing_repository == Some(idx) {
                self.sync_is_bulk = retry.grouped;
                self.sync_visual_folder_id = retry.visual_folder_id.clone();
                return;
            }
        }
        self.finish_download_retry_owner(&retry, false);
        self.clear_download_retry_attempt(&retry.target);
    }
}
