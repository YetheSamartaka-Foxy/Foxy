use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::core::api::{ModDiffSummary, SyncMode};

use super::super::super::types::*;
use crate::ui::views::swifty_migration::types::SwiftyDetectedRepo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryListContextAction {
    Delete,
    WipeRepositoryDb,
    ForceRedownload,
    CloneWithSuffix,
    GoToRepositorySpace,
    RemoveFromVisualFolder,
    OpenLocalPath,
    QuickLocalCheck,
    RemoteRecheck,
    OpenSettings,
    MoveUp,
    MoveDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositoryContextConfirmAction {
    Delete(usize),
    WipeRepositoryDb(usize),
    ForceRedownload(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct ProxyChangePrompt {
    pub repository_url: String,
    pub repository_path: String,
    pub mode: crate::core::api::SyncMode,
    pub selected_mod_states: Vec<(String, bool)>,
    pub force_redownload: bool,
    pub prepare_download_plan: bool,
    pub force_full_downloads: bool,
    pub backup_local_drift: bool,
    pub restart_requested: bool,
}

#[derive(Clone, Debug)]
pub struct RepositoryVisualFolderEditState {
    pub folder_id: Option<String>,
    pub repository_space_id: Option<String>,
    pub name_buffer: String,
    pub color_rgb: [u8; 3],
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RepositoryVisualFolderDeleteState {
    pub folder_id: String,
    pub delete_repositories: bool,
}

#[derive(Clone, Debug)]
pub enum ProfileConfirmAction {
    Delete { profile_name: String },
    Reset { profile_name: Option<String> },
}

#[derive(Clone, Debug)]
pub enum AddonDestructiveConfirmAction {
    ForceRedownload {
        repo_idx: usize,
        addon_name: String,
        addon_path: Option<String>,
    },
    Delete {
        addon_name: String,
        addon_path: String,
    },
}

#[derive(Clone, Debug)]
pub enum SettingsFolderRemovalConfirmAction {
    AdditionalSearchFolder { folder: String },
    CleanupFolder { folder: String },
}

#[derive(Clone, Debug)]
pub struct RepositorySpaceScanCandidate {
    pub repo_index: usize,
    pub checked: bool,
}

#[derive(Clone, Debug)]
pub struct RepositorySpaceSelectorState {
    pub space_id: String,
    pub path_buffer: String,
    pub candidates: Vec<RepositorySpaceScanCandidate>,
    pub last_scan_result_count: Option<usize>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RepositorySpaceSettingsState {
    pub space_id: String,
    pub source_address_buffer: String,
    pub local_name_buffer: String,
    pub shared_path_buffer: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PendingRepositoryDuplicateAddAction {
    FromAddressInput {
        address_input: String,

        name: String,

        path: String,
    },
    FromSpaceEntry {
        space_id: String,
        entry_address: String,
        entry_name: String,
    },
}

#[derive(Clone, Debug)]
pub struct PendingRepositoryDuplicateAddState {
    pub normalized_url: String,
    pub action: PendingRepositoryDuplicateAddAction,
    pub existing_repos: Vec<(String, Option<String>)>,
    pub adding_to_space_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PendingMissionDuplicateState {
    pub repo_idx: usize,
    pub profile_name: String,
    pub mission: crate::core::arma3_missions::EditorMission,
    pub name_input: String,
    pub suggested_name: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PendingMissionDeleteState {
    pub repo_idx: usize,
    pub profile_name: String,
    pub mission: crate::core::arma3_missions::EditorMission,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PendingMissionRemoveDependenciesState {
    pub repo_idx: usize,
    pub mission: crate::core::arma3_missions::EditorMission,
    pub error: Option<String>,
}

/// An enabled external addon offered for one editor launch; `keep` false leaves it out of that launch only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorLaunchExternalAddon {
    pub name: String,
    pub path: String,
    pub keep: bool,
}

#[derive(Clone, Debug)]
pub struct PendingMissionEditorLaunchWarningState {
    pub repo_idx: usize,
    pub repo_name: String,
    pub effective_repository: Repository,
    pub mission: crate::core::arma3_missions::EditorMission,
    pub external_addons: Vec<EditorLaunchExternalAddon>,

    pub unavailable_enabled: Vec<super::JoinPreflightUnavailableAddon>,
}

#[derive(Clone, Debug)]
pub struct RepositoryCheckCompletionState {
    pub repo_index: usize,
    pub mode: SyncMode,
    pub success: bool,
    pub had_updates: bool,
    pub update_count: usize,
    pub elapsed: Option<Duration>,
    pub error_message: Option<String>,
}

#[derive(Debug)]
pub(crate) struct PendingLinkedTargetSync {
    pub repo_address: String,
    pub repo_path: String,
    pub mode: crate::core::api::SyncMode,
    pub selected_mod_states: Option<Vec<(String, bool)>>,
    pub force_redownload: bool,
    pub prepare_download_plan: bool,
    pub force_full_downloads: bool,
}

#[derive(Debug)]
pub(crate) struct LinkedTargetPreviewResult {
    pub repo_address: String,
    pub repo_path: String,
    pub result: Result<Vec<crate::core::api::LinkedTargetPreview>, String>,
}

#[derive(Debug)]
pub(crate) struct RetainedFilePreviewResult {
    pub repo_url: String,
    pub local_path: String,
    pub result: Result<
        (
            Vec<crate::core::api::RetainedFilePreview>,
            Vec<crate::core::api::LinkedTargetPreview>,
        ),
        String,
    >,
}

#[derive(Debug)]
pub(crate) struct RetainedFileRemovalResult {
    pub repo_url: String,
    pub local_path: String,
    pub result: Result<usize, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingRetainedFileRemoval {
    pub repo_url: String,
    pub local_path: String,
    pub files: Vec<crate::core::api::RetainedFilePreview>,
    pub linked_targets: Vec<crate::core::api::LinkedTargetPreview>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiToastKind {
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub struct UiToastState {
    pub message: String,
    pub kind: UiToastKind,
    pub shown_at: Instant,
    pub duration: Duration,
}

#[derive(Debug, Deserialize)]
pub struct RepositorySpaceManifestEntry {
    #[serde(rename = "Name", alias = "name")]
    pub name: String,
    #[serde(rename = "Address", alias = "address")]
    pub address: String,
    #[serde(rename = "Requiered", alias = "Required", alias = "required", default)]
    pub required: bool,
    #[serde(rename = "Description", alias = "description", default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RepositorySpaceManifest {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(rename = "image", alias = "repoImagePath", default)]
    pub image: String,
    #[serde(rename = "imageChecksum", alias = "repoImageChecksum", default)]
    pub image_checksum: String,
    #[serde(rename = "icon", alias = "iconImagePath", default)]
    pub icon: String,
    #[serde(rename = "iconChecksum", alias = "iconImageChecksum", default)]
    pub icon_checksum: String,
    /// Written by newer servers; absent from older and hand-written manifests.
    #[serde(rename = "spaceChecksum", default)]
    pub space_checksum: String,
    #[serde(rename = "appUpdateUrl", alias = "app_update_url", default)]
    pub app_update_url: String,
    #[serde(default)]
    pub entries: Vec<RepositorySpaceManifestEntry>,
}

#[derive(Debug)]
pub struct StartupQuickScanFilterResult {
    pub space_id: String,
    pub generation: u64,
    pub requested_repositories: usize,
    pub eligible_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub prevalidated_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub remote_changed_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub pending_update_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub remote_unknown_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub unchanged_ineligible_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
}

#[derive(Debug)]
pub struct StartupPendingUpdateRestoreRequest {
    pub repo_name: String,
    pub repo_url: String,
    /// Route pending-update state by repository URL and download folder.
    pub repo_path: String,
    pub verify_with_quick_scan: bool,
}

#[derive(Debug)]
pub struct StartupPendingUpdateRestoreRecord {
    /// Use URL and folder to re-resolve the repository if its list position changed.
    pub repo_path: String,
    pub repo_url: String,
    pub state: RepoState,
    pub mods: Option<Vec<ModDiffSummary>>,
    pub verify_with_quick_scan: bool,
    pub is_foxy: Option<bool>,
}

pub struct StartupPendingUpdateRestoreBatch {
    pub space_id: String,
    pub generation: u64,
    pub records: Vec<StartupPendingUpdateRestoreRecord>,
}

pub struct DatabaseWipeDelivery {
    pub space_id: String,
    pub generation: u64,
    pub outcome: Result<(), String>,
}

#[derive(Debug)]
pub struct RepositoryDbWipeResult {
    pub space_id: String,
    pub generation: u64,
    pub repository_url: String,
    /// An empty folder marks a URL-wide wipe; otherwise clear only the matching `(url, local_path)` status.
    pub local_path: String,
    pub repository_name: String,
    pub elapsed: Duration,
    pub result: Result<(), String>,
    pub force_redownload_after_purge: bool,
}

#[derive(Clone, Debug)]
pub struct RepositoryDbWipeCompletionState {
    pub repository_url: String,
    pub success: bool,
    pub elapsed: Duration,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Ts3PluginUpdatePrompt {
    pub plugin_path: std::path::PathBuf,
    pub addon_name: String,
    pub file_hash: String,
}

#[derive(Debug)]
pub struct Ts3PluginScanResult {
    pub space_id: String,
    pub generation: u64,
    pub statuses: Vec<crate::core::ts3_plugin::Ts3PluginStatus>,
    pub ts3_running: bool,

    pub prompt_on_update: bool,
}

#[derive(Debug)]
pub struct DecodedImagePayload {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub struct ImageLoadResult {
    pub space_id: String,
    pub generation: u64,
    pub checksum_hex: String,
    pub is_icon: bool,
    pub payload: Result<DecodedImagePayload, String>,
}

pub struct MissionScanDelivery {
    pub space_id: String,
    pub generation: u64,
    pub list: CachedMissionList,
}

#[derive(Debug)]
pub struct RepoMetadataPayload {
    pub repo_json: serde_json::Value,
    pub addon_manifest: serde_json::Value,
}

#[derive(Debug)]
pub struct RepoMetadataFetchResult {
    pub space_id: String,
    pub generation: u64,
    /// Revalidate the dispatched index against its address because the repository list may change during fetch.
    pub repo_index: usize,
    pub repo_address: String,
    pub repo_name: String,
    pub apply_client_parameters: bool,
    pub apply_dlc_content: bool,
    pub outcome: Result<RepoMetadataPayload, String>,
}

/// Apply addon hash results on the UI thread after hashing every file off-thread.
#[derive(Debug)]
pub struct AddonHashRecalcResult {
    pub space_id: String,
    pub generation: u64,
    pub repo_url: String,
    pub addon_name: String,
    pub repo_name: String,

    pub outcome: Result<bool, String>,
}

#[derive(Debug)]
pub struct AddonDeleteResult {
    pub space_id: String,
    pub generation: u64,
    pub addon_name: String,
    pub addon_path: String,
    pub outcome: Result<usize, String>,
}

/// Probe reachability off-thread and remove local files only after success.
#[derive(Debug)]
pub struct AddonForceRedownloadProbeResult {
    pub space_id: String,
    pub generation: u64,
    pub repo_address: String,
    pub repo_path: String,
    pub repo_name: String,
    pub addon_name: String,
    pub target_path: std::path::PathBuf,
    pub outcome: Result<(), String>,
}

#[derive(Debug)]
pub enum CachedUpdateLoadOutcome {
    Pending(Vec<ModDiffSummary>),
    NeedsLocalCheck,

    Synced,

    NoPayload,

    Corrupt,

    ReadError(String),
    /// If admission closed before the read, clear the pending flag without changing repository state.
    Skipped,
}

#[derive(Debug)]
pub struct CachedUpdateLoadResult {
    pub space_id: String,
    pub generation: u64,
    pub repo_url: String,
    /// Route results to the repository instance that owns the captured download folder.
    pub local_path: String,
    /// Open the update modal after applying results only when updates remain.
    pub open_modal_when_pending: bool,
    pub outcome: CachedUpdateLoadOutcome,
}

#[derive(Debug)]
pub struct FetchedRepositorySpace {
    pub source_address: String,
    pub source_base_url: String,
    pub space_id: String,
    pub manifest_name: String,
    pub icon_image_path: String,
    pub icon_image_checksum: String,
    pub repo_image_path: String,
    pub repo_image_checksum: String,
    pub app_update_url: String,
    /// Checksum of the published content, see `repository_space_manifest_checksum`.
    pub manifest_checksum: String,
    pub entries: Vec<RepositorySpaceEntry>,
}

#[derive(Debug)]
pub enum RepositorySpaceImportContinuation {
    /// If no space manifest exists, add the input as a plain repository with the optional name/path overrides.
    AddRepositoryDialog {
        address_input: String,
        name: String,
        path: String,
    },
    /// Continue Swifty import using the repository selection captured when Import was clicked.
    SwiftyMigration { selected: Vec<SwiftyDetectedRepo> },
}

/// Fetch repository-space manifests off-thread; blocking HTTP here could freeze the UI against an unreachable server.
#[derive(Debug)]
pub struct RepositorySpaceImportResult {
    pub space_id: String,
    pub generation: u64,
    pub continuation: RepositorySpaceImportContinuation,
    /// Distinguish a found manifest, no manifest at candidate URLs, and a fetch/parse error.
    pub outcome: Result<Option<FetchedRepositorySpace>, String>,
}

#[derive(Clone, Debug)]
pub struct BackupManagerViewCache {
    pub records_version: u64,
    pub filter: String,
    pub total_backups: usize,
    pub total_bytes: u64,
    pub addon_count: usize,
    pub grouped_backups: std::collections::BTreeMap<
        String,
        Vec<crate::core::utils::addon_backup::AddonBackupRecord>,
    >,
}

#[derive(Clone, Debug)]
pub struct CachedPaletteColor32 {
    pub primary_accent: egui::Color32,
    pub primary_accent_hover: egui::Color32,
    pub primary_accent_active: egui::Color32,
    pub widget_bg: egui::Color32,
    pub main_bg: egui::Color32,
    pub card_bg: egui::Color32,
    pub server_offline_bg: egui::Color32,
    pub server_selected_bg: egui::Color32,
    pub server_selected_bg_hover: egui::Color32,
    pub server_selected_stroke: egui::Color32,
    pub text_normal: egui::Color32,
    pub text_gray: egui::Color32,
    pub text_dim: egui::Color32,
    pub text_error: egui::Color32,
    pub error: egui::Color32,
    pub warn: egui::Color32,
    pub debug: egui::Color32,
    pub success: egui::Color32,
    pub success_muted: egui::Color32,
    pub action_info: egui::Color32,
    pub action_destructive: egui::Color32,
    pub widget_bg_hover: egui::Color32,
    pub widget_bg_active: egui::Color32,
}
