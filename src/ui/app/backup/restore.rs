use log::{info, warn};

use crate::core::api::SyncMode;
use crate::core::utils::addon_backup;
use crate::ui::app::{
    AddonBackupRestoreState, AddonBackupTaskAction, AddonBackupTaskResult, AddonBackupTaskStatus,
    Foxy,
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

        if let Err(message) = self.resolve_repo_addon_path(&repo, addon_name, addon_path) {
            self.set_addon_backup_notice(repo_index, false, message.clone());
            warn!("{}", message);
            return false;
        }

        let backups = match addon_backup::list_addon_backups(&backup_root, addon_name) {
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
            addon_name: addon_name.trim().to_string(),
            addon_path: addon_path.map(str::to_string),
            backups,
            selected_backup_index: 0,
        });
        self.addon_backup_notice = None;
        self.needs_repaint = true;
        true
    }

    pub fn start_manual_addon_restore(&mut self, restore_state: AddonBackupRestoreState) -> bool {
        if self.repository_sync_active()
            || self.is_direct_download_running()
            || self.has_addon_backup_task_running()
        {
            warn!("Manual addon restore ignored: background work is currently active");
            return false;
        }

        let Some(repo) = self
            .repository_view_state
            .repositories
            .get(restore_state.repo_index)
            .cloned()
        else {
            warn!(
                "Manual addon restore ignored: invalid repository index {}",
                restore_state.repo_index
            );
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

        let repo_index = restore_state.repo_index;
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
