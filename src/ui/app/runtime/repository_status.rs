use eframe::egui;

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
            let checked = repo
                .last_remote_check_at
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds as i64, 0))
                .map(|date| {
                    date.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| self.t("Unknown"));
            ui.label(self.t_fmt("Last successful server check: {time}", &[("time", checked)]));
        }
    }
}
