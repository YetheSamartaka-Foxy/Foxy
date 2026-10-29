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
    pub repository_url: String,
    pub repository_path: String,
    pub addon_name: String,
    pub addon_path: Option<String>,
    pub destination_path: std::path::PathBuf,
    pub backups: Vec<addon_backup::AddonBackupRecord>,
    pub selected_backup_index: usize,
    pub origin_consent: Option<AddonBackupRestoreConsent>,
    pub pending_origin_confirmation: Option<BackupOriginConfirmation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddonBackupRestoreConsent {
    pub game_space_id: String,
    pub repository_url: String,
    pub repository_path: String,
    pub destination_path: std::path::PathBuf,
    pub backup_path: std::path::PathBuf,
    pub content_hash: String,
    pub origins: Vec<addon_backup::AddonBackupOrigin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupOriginConfirmation {
    CrossOrigin,
    UnknownLegacyOrigin,
}

#[derive(Clone, Debug)]
pub struct BackupManagerNotice {
    pub success: bool,
    pub message: String,
}

#[derive(Debug)]
pub enum BackupCleanupTaskResult {
    Preview(Result<addon_backup::BackupCleanupPreview, String>),
    Cleanup(Result<addon_backup::BackupCleanupReport, String>),
}

#[derive(Clone, Debug)]
pub enum BackupManagerConfirmAction {
    DeleteBackup(addon_backup::AddonBackupRecord),
    DeleteAddonGroup {
        addon_name: String,
        backup_count: usize,
    },
    RunCleanup {
        preview: addon_backup::BackupCleanupPreview,
        include_final_recovery_copies: bool,
    },
}
