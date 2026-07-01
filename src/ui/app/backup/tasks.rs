use std::path::{Path, PathBuf};

use log::warn;

use crate::core::utils::{addon_backup, app_paths};
use crate::ui::app::{
    AddonBackupTaskAction, AddonBackupTaskResult, AddonBackupTaskStatus, BackupManagerNotice, Foxy,
};
use crate::ui::i18n::fmt_bytes;
use crate::ui::types::Repository;

impl Foxy {
    pub(crate) fn configured_backup_directory(&self) -> Option<PathBuf> {
        let trimmed = self.settings_view_state.backup_directory.trim();
        if trimmed.is_empty() {
            Some(app_paths::foxy_backups_dir())
        } else {
            Some(PathBuf::from(trimmed))
        }
    }

    pub fn refresh_backup_manager_inventory(&mut self) -> bool {
        let Some(backup_root) = self.configured_backup_directory() else {
            let message = self.t("Addon backup directory is not configured.");
            self.backup_manager_notice = Some(BackupManagerNotice {
                success: false,
                message: message.clone(),
            });
            warn!("{}", message);
            return false;
        };

        let _ = backup_root;
        self.backup_manager_loaded = false;
        self.backup_inventory_refresh_requested = true;
        self.needs_repaint = true;
        true
    }

    pub fn backup_cleanup_policy(&self) -> addon_backup::BackupCleanupPolicy {
        addon_backup::BackupCleanupPolicy {
            keep_latest_per_addon: (self.settings_view_state.backup_keep_latest_per_addon > 0)
                .then_some(self.settings_view_state.backup_keep_latest_per_addon as usize),
            max_age_days: self
                .settings_view_state
                .backup_max_age_days
                .filter(|days| *days > 0)
                .map(u64::from),
        }
    }

    pub fn delete_backup_manager_record(
        &mut self,
        record: &addon_backup::AddonBackupRecord,
    ) -> bool {
        match addon_backup::delete_addon_backup(record) {
            Ok(()) => {
                let message = self.t_fmt(
                    "Deleted addon backup {name} ({hash}).",
                    &[
                        ("name", record.addon_name.clone()),
                        ("hash", record.content_hash.clone()),
                    ],
                );
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: true,
                    message,
                });
                self.invalidate_backup_manager_inventory();
                self.refresh_backup_manager_inventory();
                true
            }
            Err(err) => {
                let message = self.t_fmt(
                    "Failed to delete addon backup {name}: {error}",
                    &[
                        ("name", record.folder_name.clone()),
                        ("error", err.to_string()),
                    ],
                );
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: false,
                    message: message.clone(),
                });
                warn!("{}", message);
                false
            }
        }
    }

    pub fn delete_backup_manager_addon_group(&mut self, addon_name: &str) -> bool {
        let Some(backup_root) = self.configured_backup_directory() else {
            let message = self.t("Addon backup directory is not configured.");
            self.backup_manager_notice = Some(BackupManagerNotice {
                success: false,
                message: message.clone(),
            });
            warn!("{}", message);
            return false;
        };

        match addon_backup::delete_addon_backups(&backup_root, addon_name) {
            Ok(deleted_backups) => {
                let message = self.i18n.tr_plural_fmt(
                    "Deleted {count} addon backups for {name}.",
                    deleted_backups as u64,
                    &[("name", addon_name.to_string())],
                );
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: true,
                    message,
                });
                self.invalidate_backup_manager_inventory();
                self.refresh_backup_manager_inventory();
                true
            }
            Err(err) => {
                let message = self.t_fmt(
                    "Failed to delete addon backups for {name}: {error}",
                    &[("name", addon_name.to_string()), ("error", err.to_string())],
                );
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: false,
                    message: message.clone(),
                });
                warn!("{}", message);
                false
            }
        }
    }

    pub fn run_backup_manager_cleanup(&mut self) -> bool {
        let Some(backup_root) = self.configured_backup_directory() else {
            let message = self.t("Addon backup directory is not configured.");
            self.backup_manager_notice = Some(BackupManagerNotice {
                success: false,
                message: message.clone(),
            });
            warn!("{}", message);
            return false;
        };

        let policy = self.backup_cleanup_policy();
        if policy.keep_latest_per_addon.is_none() && policy.max_age_days.is_none() {
            let message = self.t("Configure at least one cleanup rule before running cleanup.");
            self.backup_manager_notice = Some(BackupManagerNotice {
                success: false,
                message: message.clone(),
            });
            warn!("{}", message);
            return false;
        }

        match addon_backup::cleanup_addon_backups(&backup_root, policy) {
            Ok(report) => {
                let message = self.i18n.tr_plural_fmt(
                    "Backup cleanup removed {count} backups and freed {size}.",
                    report.deleted_backups as u64,
                    &[("size", fmt_bytes(report.freed_bytes))],
                );
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: true,
                    message,
                });
                self.invalidate_backup_manager_inventory();
                self.refresh_backup_manager_inventory();
                true
            }
            Err(err) => {
                let message = self.t_fmt(
                    "Backup cleanup failed: {error}",
                    &[("error", err.to_string())],
                );
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: false,
                    message: message.clone(),
                });
                warn!("{}", message);
                false
            }
        }
    }

    pub fn has_addon_backup_task_running(&self) -> bool {
        self.addon_backup_worker.is_some()
    }

    pub(in crate::ui::app) fn resolve_repo_addon_path(
        &self,
        repo: &Repository,
        addon_name: &str,
        preferred_path: Option<&str>,
    ) -> Result<PathBuf, String> {
        let addon_name = addon_name.trim();
        if addon_name.is_empty() {
            return Err("Addon name is empty".to_string());
        }

        let repo_path = repo.path.trim();
        if repo_path.is_empty() {
            return Err(format!(
                "Repository {} does not have a local path configured",
                repo.name
            ));
        }

        let fallback = Path::new(repo_path).join(addon_name);
        let candidate = preferred_path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| Self::is_safe_addon_path(repo_path, path))
            .unwrap_or(fallback);

        if !Self::is_safe_addon_path(repo_path, &candidate) {
            return Err(format!(
                "Resolved addon path is outside repository root for {}",
                addon_name
            ));
        }

        Ok(candidate)
    }

    pub fn start_manual_addon_backup(
        &mut self,
        repo_index: usize,
        addon_name: &str,
        addon_path: Option<&str>,
    ) -> bool {
        if self.repository_sync_active()
            || self.is_direct_download_running()
            || self.has_addon_backup_task_running()
        {
            warn!("Manual addon backup ignored: background work is currently active");
            return false;
        }

        let Some(repo) = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .cloned()
        else {
            warn!(
                "Manual addon backup ignored: invalid repository index {}",
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

        let addon_path = match self.resolve_repo_addon_path(&repo, addon_name, addon_path) {
            Ok(path) => path,
            Err(message) => {
                self.set_addon_backup_notice(repo_index, false, message.clone());
                warn!("{}", message);
                return false;
            }
        };

        let addon_name_owned = addon_name.trim().to_string();
        self.addon_backup_restore_state = None;
        self.addon_backup_notice = None;
        self.addon_backup_status = Some(AddonBackupTaskStatus {
            repo_index,
            status_text: self.t_fmt(
                "Backing up addon {name}...",
                &[("name", addon_name_owned.clone())],
            ),
        });
        let task_tx = self.addon_backup_task_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        self.addon_backup_worker = Some(std::thread::spawn(move || {
            let result = addon_backup::backup_addon(&backup_root, &addon_path)
                .map(|record| AddonBackupTaskResult {
                    repo_index,
                    action: AddonBackupTaskAction::Backup,
                    addon_name: record.addon_name,
                    success: true,
                    content_hash: Some(record.content_hash),
                    error_message: None,
                    trigger_recheck: false,
                })
                .unwrap_or_else(|err| AddonBackupTaskResult {
                    repo_index,
                    action: AddonBackupTaskAction::Backup,
                    addon_name: addon_name_owned,
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

    pub(in crate::ui::app) fn poll_addon_backup_results(&mut self) {
        while let Ok(result) = self.addon_backup_task_rx.try_recv() {
            self.addon_backup_status = None;
            if let Some(ref handle) = self.addon_backup_worker
                && handle.is_finished()
                && let Some(h) = self.addon_backup_worker.take()
            {
                let _ = h.join();
            }

            let message = if result.success {
                let key = match result.action {
                    AddonBackupTaskAction::Backup => "Addon backup saved: {name} ({hash})",
                    AddonBackupTaskAction::Restore => "Addon backup restored: {name} ({hash})",
                };
                self.t_fmt(
                    key,
                    &[
                        ("name", result.addon_name.clone()),
                        ("hash", result.content_hash.clone().unwrap_or_default()),
                    ],
                )
            } else {
                let key = match result.action {
                    AddonBackupTaskAction::Backup => "Failed to back up addon {name}: {error}",
                    AddonBackupTaskAction::Restore => "Failed to restore addon {name}: {error}",
                };
                self.t_fmt(
                    key,
                    &[
                        ("name", result.addon_name.clone()),
                        (
                            "error",
                            result
                                .error_message
                                .clone()
                                .unwrap_or_else(|| self.t("Unknown error")),
                        ),
                    ],
                )
            };

            self.set_addon_backup_notice(result.repo_index, result.success, message);
            if result.success {
                self.invalidate_backup_manager_inventory();
            }

            if result.success && result.trigger_recheck {
                self.open_update_after_sync = false;
                self.clear_mod_diff_cache();
                self.update_ready_repo = None;
                if let Some((repo_address, repo_path)) = self
                    .repository_view_state
                    .repositories
                    .get(result.repo_index)
                    .map(|repo| (repo.address.clone(), repo.path.clone()))
                {
                    self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                }
                self.schedule_post_restore_repo_verification(result.repo_index);
            }
        }
    }
}
