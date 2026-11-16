use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddonBackupRecord {
    pub addon_name: String,
    pub content_hash: String,
    pub folder_name: String,
    pub path: PathBuf,
    pub created_at_unix_secs: u64,
    pub created_at_unix_nanos: u128,
    pub size_bytes: u64,
    pub origins: Vec<AddonBackupOrigin>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddonBackupOrigin {
    pub game_space_id: String,
    pub repository_url: String,
    pub local_path: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackupCleanupPolicy {
    pub keep_latest_per_addon: Option<usize>,
    pub max_age_days: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct BackupCleanupReport {
    pub deleted_backups: usize,
    pub freed_bytes: u64,
    pub deleted_folders: Vec<String>,
    pub failed_folders: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct BackupCleanupPreview {
    pub policy: BackupCleanupPolicy,
    pub inventory: Vec<AddonBackupRecord>,
    pub deletions: Vec<AddonBackupRecord>,
    pub final_recovery_copies: Vec<AddonBackupRecord>,
}
