use super::enums::HashAlgorithmPreference;
use crate::core::api::ModDiffSummary;
use crate::core::arma3_missions::EditorMission;
use crate::ui::i18n::tr;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, Default)]
pub struct RepositoryServer {
    pub name: String,
    pub address: String,
    pub port: String,
    pub password: String,
    pub battle_eye: bool,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct RepositoryProfile {
    pub name: String,
    pub csla: bool,
    pub ef: bool,
    pub gm: bool,
    pub rf: bool,
    pub spe: bool,
    pub vn: bool,
    pub ws: bool,
    pub skip_intro: bool,
    pub no_splash: bool,
    pub world_empty: bool,
    pub load_mission_to_memory: bool,
    pub enable_ht: bool,
    pub huge_pages: bool,
    pub no_logs: bool,
    #[serde(default)]
    pub include_steam_addons: bool,
    pub additional_params: String,
    #[serde(default)]
    pub addons: Vec<(String, bool)>,
    #[serde(default)]
    pub optional_addons: Vec<(String, bool)>,
    #[serde(default)]
    pub optional_addon_favorites: Vec<String>,
    #[serde(default)]
    pub optional_addon_client_side: Vec<String>,
    #[serde(default)]
    pub external_addons: Vec<(String, bool, String)>,
    #[serde(default)]
    pub external_addon_favorites: Vec<String>,
    #[serde(default)]
    pub external_addon_client_side: Vec<String>,
}

impl Default for RepositoryProfile {
    fn default() -> Self {
        Self {
            name: tr("New Profile"),
            csla: false,
            ef: false,
            gm: false,
            rf: false,
            spe: false,
            vn: false,
            ws: false,
            skip_intro: false,
            no_splash: false,
            world_empty: false,
            load_mission_to_memory: false,
            enable_ht: false,
            huge_pages: false,
            no_logs: false,
            include_steam_addons: false,
            additional_params: String::new(),
            addons: Vec::new(),
            optional_addons: Vec::new(),
            optional_addon_favorites: Vec::new(),
            optional_addon_client_side: Vec::new(),
            external_addons: Vec::new(),
            external_addon_favorites: Vec::new(),
            external_addon_client_side: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct Repository {
    #[serde(default)]
    pub profiles: Vec<RepositoryProfile>,
    #[serde(default)]
    pub selected_profile: Option<String>,

    pub name: String,
    pub address: String,
    pub path: String,
    #[serde(default)]
    pub auto_recheck_on_launch: Option<bool>,
    #[serde(default)]
    pub auto_quick_scan_on_launch: Option<bool>,
    #[serde(default)]
    pub auto_backup_on_update: Option<bool>,
    #[serde(default)]
    pub apply_repo_json_client_parameters: Option<bool>,
    #[serde(default)]
    pub apply_repo_json_dlc_content: Option<bool>,
    #[serde(default)]
    pub warn_editor_external_addons: Option<bool>,
    #[serde(default)]
    pub enable_editor_mission_list: Option<bool>,
    #[serde(default)]
    pub enable_server_list: Option<bool>,
    #[serde(default)]
    pub check_server_addons_before_join: Option<bool>,
    #[serde(default)]
    pub check_ts3_running_before_join: Option<bool>,
    #[serde(default)]
    pub check_steam_running_before_launch: Option<bool>,
    /// Per-repository override for hiding the repository banner image.
    /// None = use the global setting.
    #[serde(default)]
    pub hide_repo_image: Option<bool>,

    pub csla: bool,
    pub ef: bool,
    pub gm: bool,
    pub rf: bool,
    pub spe: bool,
    pub vn: bool,
    pub ws: bool,

    pub skip_intro: bool,
    pub no_splash: bool,
    pub world_empty: bool,
    pub load_mission_to_memory: bool,
    pub enable_ht: bool,
    pub huge_pages: bool,
    pub no_logs: bool,
    #[serde(default)]
    pub include_steam_addons: bool,

    pub additional_params: String,
    #[serde(default)]
    pub addons: Vec<(String, bool)>,
    #[serde(default)]
    pub optional_addons: Vec<(String, bool)>,
    #[serde(default)]
    pub optional_addon_favorites: Vec<String>,
    #[serde(default)]
    pub optional_addon_client_side: Vec<String>,
    #[serde(default)]
    pub remote_client_side_addons: Vec<String>,
    #[serde(default)]
    pub external_addons: Vec<(String, bool, String)>,
    #[serde(default)]
    pub external_addon_favorites: Vec<String>,
    #[serde(default)]
    pub external_addon_client_side: Vec<String>,
    #[serde(default)]
    pub servers: Vec<RepositoryServer>,

    /// The Arma 3 player profile name to use for this repository.
    /// None = use the auto-detected active profile (or default).
    #[serde(default)]
    pub arma3_profile: Option<String>,

    #[serde(default)]
    pub icon_image_path: String,
    #[serde(default)]
    pub icon_image_checksum: String,
    #[serde(default)]
    pub repo_image_path: String,
    #[serde(default)]
    pub repo_image_checksum: String,
    #[serde(default)]
    pub app_update_url: String,
    #[serde(default)]
    pub repository_space_id: Option<String>,
    #[serde(default)]
    pub repository_space_entry_address: Option<String>,
    #[serde(default)]
    pub hash_algorithm_preference: HashAlgorithmPreference,
}

impl Default for Repository {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            selected_profile: None,

            name: tr("New Repository"),
            address: String::new(),
            path: String::new(),
            auto_recheck_on_launch: None,
            auto_quick_scan_on_launch: None,
            auto_backup_on_update: None,
            apply_repo_json_client_parameters: None,
            apply_repo_json_dlc_content: None,
            warn_editor_external_addons: None,
            enable_editor_mission_list: None,
            enable_server_list: None,
            check_server_addons_before_join: None,
            check_ts3_running_before_join: None,
            check_steam_running_before_launch: None,
            hide_repo_image: None,

            csla: false,
            ef: false,
            gm: false,
            rf: false,
            spe: false,
            vn: false,
            ws: false,

            skip_intro: false,
            no_splash: false,
            world_empty: false,
            load_mission_to_memory: false,
            enable_ht: false,
            huge_pages: false,
            no_logs: false,
            include_steam_addons: false,

            additional_params: String::new(),
            addons: Vec::new(),
            optional_addons: Vec::new(),
            optional_addon_favorites: Vec::new(),
            optional_addon_client_side: Vec::new(),
            remote_client_side_addons: Vec::new(),
            external_addons: Vec::new(),
            external_addon_favorites: Vec::new(),
            external_addon_client_side: Vec::new(),
            servers: Vec::new(),
            arma3_profile: None,

            icon_image_path: String::new(),
            icon_image_checksum: String::new(),
            repo_image_path: String::new(),
            repo_image_checksum: String::new(),
            app_update_url: String::new(),
            repository_space_id: None,
            repository_space_entry_address: None,
            hash_algorithm_preference: HashAlgorithmPreference::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, Default)]
pub struct RepositoryViewState {
    pub selected_repository: Option<usize>,
    #[serde(skip)]
    pub repository_filter: String,
    #[serde(skip)]
    pub repository_spaces_collapsed: bool,
    #[serde(skip)]
    pub repositories_collapsed: bool,
    #[serde(default)]
    pub repositories: Vec<Repository>,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct DownloadTelemetrySample {
    pub elapsed_ms: u64,
    pub download_bps: f64,
    pub disk_write_bps: f64,
    pub hash_files_per_sec: f64,
    #[serde(default)]
    pub cpu_percent: f64,
    #[serde(default)]
    pub memory_bytes: u64,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct DownloadSummary {
    pub mods_updated: usize,
    pub files_updated: usize,
    pub parts_updated: usize,
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub planned_transfer_bytes: u64,
    #[serde(default)]
    pub full_download_bytes: u64,
    #[serde(default)]
    pub patch_savings_bytes: u64,
    #[serde(default)]
    pub patched_files: usize,
    pub download_stage_duration: Duration,
    #[serde(default)]
    pub cumulative_hash_duration: Duration,
    #[serde(default)]
    pub after_download_hash_duration: Duration,
    pub hash_stage_duration: Duration,
    pub total_duration: Duration,
    pub avg_speed_bps: f64,
    #[serde(default)]
    pub telemetry_samples: Vec<DownloadTelemetrySample>,
}

impl DownloadSummary {
    /// Returns `true` when at least one mod, file, or part was actually updated.
    /// A summary with all-zero counts is a no-op download and should not be
    /// persisted as an update notice.
    pub fn has_meaningful_content(&self) -> bool {
        self.mods_updated > 0 || self.files_updated > 0 || self.parts_updated > 0
    }

    pub fn planned_or_downloaded_bytes(&self) -> u64 {
        if self.planned_transfer_bytes > 0 {
            self.planned_transfer_bytes
        } else {
            self.downloaded_bytes
        }
    }

    pub fn cumulative_or_after_download_hash_duration(&self) -> Duration {
        if self.cumulative_hash_duration > Duration::ZERO {
            self.cumulative_hash_duration
        } else if self.after_download_hash_duration > Duration::ZERO {
            self.after_download_hash_duration
        } else {
            self.hash_stage_duration
        }
    }

    pub fn after_download_or_legacy_hash_duration(&self) -> Duration {
        if self.after_download_hash_duration > Duration::ZERO {
            self.after_download_hash_duration
        } else {
            self.hash_stage_duration
        }
    }

    pub fn push_telemetry_sample(&mut self, sample: DownloadTelemetrySample) {
        const MAX_TELEMETRY_SAMPLES: usize = 180;

        if self.telemetry_samples.len() >= MAX_TELEMETRY_SAMPLES {
            let remove_count = self.telemetry_samples.len() + 1 - MAX_TELEMETRY_SAMPLES;
            self.telemetry_samples.drain(0..remove_count);
        }
        self.telemetry_samples.push(sample);
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct UpdateSummaryNotice {
    pub repository_url: String,
    #[serde(default)]
    pub pending_ack_count: u32,
    pub summary: DownloadSummary,
    // Snapshot of the per-mod diff at the time the download completed, so the
    // update summary modal can restore the correct "Mods to be updated" list
    // per repository. Older persisted notices without this field fall back to
    // an empty list.
    #[serde(default)]
    pub mods: Vec<ModDiffSummary>,
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ActiveUpdateSession {
    pub repository_url: String,
    pub session_id: String,
    pub mods: Vec<ModDiffSummary>,
}

/// What the user has selected in the repository view - either a server or a mission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositorySelection {
    Server(usize),
    Mission(usize),
}

/// Cached mission list for the currently viewed repository.
#[derive(Debug, Clone)]
pub struct CachedMissionList {
    pub profile_name: String,
    pub missions: Vec<EditorMission>,
    pub scanned_at: Instant,
}
