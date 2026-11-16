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
    #[serde(default)]
    pub change_kind: FileDiffKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDiffKind {
    Added,
    #[default]
    Modified,
    Deleted,
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

    pub local_path: String,
    pub mods: Vec<ModDiffSummary>,

    pub skipped: bool,
}

#[derive(Clone, Debug)]
pub struct QuickScanProgressEvent {
    pub repo_url: String,
    pub local_path: String,
    pub event: ProgressEvent,
}

#[derive(Clone, Debug)]
pub struct FsChangeEvent {
    pub repo_urls: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum ProgressEvent {
    UpdatePhase(crate::core::utils::update_estimate::UpdatePhase),
    HashEvidence {
        state: Arc<crate::core::tasks::calculate_hashes::HashEvidenceState>,
    },
    VerificationBasis {
        full: bool,
        remote_fresh: bool,
    },
    LocalDrift {
        addons: Vec<String>,
    },
    CleanupPlan {
        files: Vec<String>,
    },
    EmptyPublication {
        repository_empty: bool,
        addons: Vec<String>,
    },
    Stage {
        label: String,
        percent: f32,
    },
    RecheckHashProgress {
        checked_files: usize,
        total_files: usize,
        checked_parts: usize,
        total_parts: usize,

        checked_bytes: u64,
        total_bytes: u64,
    },
    DownloadMod {
        mod_name: String,
        attempt: usize,
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

    HashEstimate {
        remaining_bytes: u64,
        bytes_per_sec: u64,
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

    DiskSpaceShortfall(crate::core::utils::disk_space::DiskSpaceShortfall),
    RetryableDownloadFailure,
    SharedVersionWarning {
        repositories: Vec<String>,
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

#[derive(Clone)]
pub struct RepositorySyncOptions {
    pub operation_id: String,

    pub prepare_download_plan: bool,
    pub repository_space_shared_path: Option<String>,
    pub repository_space_members: Vec<(String, String)>,
    pub auto_backup_directory: Option<String>,
    pub backup_local_drift: bool,
    pub enrolled_link_targets: Vec<LinkedTargetPreview>,
    pub interactive_hash_checks: bool,
    pub rollback_temp_directory: Option<String>,
    pub download_speed_limit_mbps: Option<u32>,
    pub recent_local_path_reset: bool,

    pub discard_prepared_queue: bool,
    pub force_redownload: bool,

    pub force_full_downloads: bool,
    pub allow_suspect_full_redownload: bool,
    pub download_pause_rx: watch::Receiver<bool>,
    pub cancel_rx: watch::Receiver<bool>,
    pub hash_algorithm_preference: crate::ui::types::HashAlgorithmPreference,
    pub hash_io_profile: crate::ui::types::HashIoProfilePreference,
    /// Restore files the verified-hash record proves unchanged when the database
    /// holds no state for them, instead of reading them again.
    pub trust_verified_hashes: bool,
    /// Durable addon selection is distinct from transient per-run overrides; never persist the latter.
    pub persisted_addon_selection: Option<Vec<(String, bool)>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_diff_summary_defaults_missing_change_kind_to_modified() {
        let summary: FileDiffSummary = serde_json::from_str(
            r#"{"name":"addons/main.pbo","needs_update":true,"total_bytes":12,"changed_parts":1}"#,
        )
        .unwrap();

        assert_eq!(summary.change_kind, FileDiffKind::Modified);
    }
}
