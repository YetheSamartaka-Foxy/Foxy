use crate::core::utils::addon_backup;

#[derive(Clone, Debug)]
pub struct AddonBackupTaskStatus {
    pub repo_index: usize,
    pub status_text: String,
}

#[derive(Clone, Copy, Debug)]
pub enum AddonBackupTaskAction {
    Backup,
    Restore,
}

#[derive(Clone, Debug)]
pub struct AddonBackupTaskResult {
    pub repo_index: usize,
    pub action: AddonBackupTaskAction,
    pub addon_name: String,
    pub success: bool,
    pub content_hash: Option<String>,
    pub error_message: Option<String>,
    pub trigger_recheck: bool,
}

#[derive(Clone, Debug)]
pub struct AddonBackupNotice {
    pub repo_index: usize,
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct AddonBackupRestoreState {
    pub repo_index: usize,
    pub addon_name: String,
    pub addon_path: Option<String>,
    pub backups: Vec<addon_backup::AddonBackupRecord>,
    pub selected_backup_index: usize,
}

#[derive(Clone, Debug)]
pub struct BackupManagerNotice {
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Debug)]
pub enum BackupManagerConfirmAction {
    DeleteBackup(addon_backup::AddonBackupRecord),
    DeleteAddonGroup {
        addon_name: String,
        backup_count: usize,
    },
    RunCleanup,
}
