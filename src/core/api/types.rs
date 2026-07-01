use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub source: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileDiffSummary {
    pub name: String,
    pub needs_update: bool,
    pub total_bytes: u64,
    pub changed_parts: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModDiffSummary {
    pub name: String,
    pub needs_update: bool,
    pub total_bytes: u64,
    pub files: Vec<FileDiffSummary>,
}

#[derive(Clone, Debug)]
pub struct QuickScanResult {
    pub repo_url: String,
    /// Download folder of the scanned repository instance, normalized for
    /// identity comparison. The same `repo_url` can be installed in several
    /// folders; results are routed back to the matching instance by
    /// `(repo_url, local_path)`. Empty when the repository has no folder yet.
    pub local_path: String,
    pub mods: Vec<ModDiffSummary>,
    /// When `true` the scan was skipped because it cannot safely produce a
    /// clean/update decision yet (e.g. metadata not ready or local paths do not
    /// match the expected remote tree).
    /// Consumers should treat this as "status unknown" rather than "clean".
    pub skipped: bool,
}

#[derive(Clone, Debug)]
pub struct FsChangeEvent {
    pub repo_urls: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum ProgressEvent {
    Stage {
        label: String,
        percent: f32,
    },
    RecheckHashProgress {
        checked_files: usize,
        total_files: usize,
        checked_parts: usize,
        total_parts: usize,
    },
    DownloadMod {
        mod_name: String,
        files_done: usize,
        files_total: usize,
        bytes_done: u64,
        bytes_total: u64,
        percent: f32,
    },
    DownloadPlan {
        files_total: usize,
        planned_bytes: u64,
        full_bytes: u64,
        patch_files: usize,
    },
    DownloadTelemetry {
        elapsed_ms: u64,
        download_bps: f64,
        disk_write_bps: f64,
        cpu_percent: f64,
        memory_bytes: u64,
    },
    HashTelemetry {
        elapsed_ms: u64,
        files_per_sec: f64,
    },
    HashSummary {
        cumulative_hash_ms: u64,
        after_download_hash_ms: u64,
    },
    Diff {
        mods: Vec<ModDiffSummary>,
    },
    SiblingPropagation {
        repo_urls: Vec<String>,
    },
    RepositoryFoxyMode {
        is_foxy: bool,
        app_update_url: Option<String>,
    },
    Finished,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncMode {
    RemoteRefreshOnly,
    QuickCheckOnly,
    RecheckOnly,
    RecheckIntegrity,
    Download,
}

pub struct RepositorySyncOptions {
    pub operation_id: String,
    /// Build and persist the final download queue, but stop before backup or transfer.
    pub prepare_download_plan: bool,
    pub repository_space_shared_path: Option<String>,
    pub auto_backup_directory: Option<String>,
    pub rollback_temp_directory: Option<String>,
    pub download_speed_limit_mbps: Option<u32>,
    pub recent_local_path_reset: bool,
    pub force_redownload: bool,
    pub allow_suspect_full_redownload: bool,
    pub download_pause_rx: watch::Receiver<bool>,
    pub cancel_rx: watch::Receiver<bool>,
    pub hash_algorithm_preference: crate::ui::types::HashAlgorithmPreference,
    pub hash_io_profile: crate::ui::types::HashIoProfilePreference,
}
