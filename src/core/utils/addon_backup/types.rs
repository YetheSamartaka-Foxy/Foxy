use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct AddonBackupRecord {
    pub addon_name: String,
    pub content_hash: String,
    pub folder_name: String,
    pub path: PathBuf,
    pub created_at_unix_secs: u64,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BackupCleanupPolicy {
    pub keep_latest_per_addon: Option<usize>,
    pub max_age_days: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BackupCleanupReport {
    pub deleted_backups: usize,
    pub freed_bytes: u64,
}
