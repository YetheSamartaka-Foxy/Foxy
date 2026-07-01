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
    OpenLocalPath,
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
        /// Optional name override from the add-repository dialog (empty = derive
        /// from address).
        name: String,
        /// Optional local path override from the add-repository dialog (empty =
        /// no chosen folder).
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

#[derive(Clone, Debug)]
pub struct PendingMissionEditorLaunchWarningState {
    pub repo_idx: usize,
    pub repo_name: String,
    pub effective_repository: Repository,
    pub mission: crate::core::arma3_missions::EditorMission,
    pub external_addons: Vec<String>,
    /// Enabled external addons whose files are missing, so the editor launch
    /// would silently drop them. Surfaced as a warning regardless of the
    /// general "warn about editor external addons" setting.
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
    #[serde(rename = "appUpdateUrl", alias = "app_update_url", default)]
    pub app_update_url: String,
    #[serde(default)]
    pub entries: Vec<RepositorySpaceManifestEntry>,
}

#[derive(Debug)]
pub struct StartupQuickScanFilterResult {
    pub requested_repositories: usize,
    pub eligible_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub prevalidated_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
    pub remote_changed_repositories: Vec<crate::core::api::StartupRepositoryInstance>,
}

#[derive(Debug)]
pub struct StartupPendingUpdateRestoreRequest {
    pub repo_index: usize,
    pub repo_name: String,
    pub repo_url: String,
    /// Download folder of this repository instance; the cached pending-update row
    /// is keyed by `(repo_url, local_path)`.
    pub repo_path: String,
    pub verify_with_quick_scan: bool,
}

#[derive(Debug)]
pub struct StartupPendingUpdateRestoreRecord {
    pub repo_index: usize,
    pub repo_url: String,
    pub state: RepoState,
    pub mods: Option<Vec<ModDiffSummary>>,
    pub verify_with_quick_scan: bool,
    pub is_foxy: Option<bool>,
}

#[derive(Debug)]
pub struct RepositoryDbWipeResult {
    pub repository_url: String,
    /// Download folder of the wiped instance; empty for a URL-wide wipe (e.g.
    /// repository deletion). Used to clear the matching `(url, local_path)`
    /// status entry.
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

/// Prompt shown after a sync/download when an updated TS3 plugin is detected.
#[derive(Clone, Debug)]
pub struct Ts3PluginUpdatePrompt {
    pub plugin_path: std::path::PathBuf,
    pub addon_name: String,
    pub file_hash: String,
}

#[derive(Debug)]
pub struct DecodedImagePayload {
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub struct ImageLoadResult {
    pub checksum_hex: String,
    pub is_icon: bool,
    pub payload: Result<DecodedImagePayload, String>,
}

/// Successfully fetched repo.json (and, for FoxyMode repos, foxy_addons.json)
/// payload produced by the background repository-metadata fetch worker.
#[derive(Debug)]
pub struct RepoMetadataPayload {
    pub repo_json: serde_json::Value,
    pub addon_manifest: serde_json::Value,
}

/// Result of a background repository-metadata refresh, delivered to the UI
/// thread for application. Network I/O happens off the UI thread so a slow or
/// unreachable server can no longer freeze the app.
#[derive(Debug)]
pub struct RepoMetadataFetchResult {
    /// Index the refresh was dispatched for; re-validated against `repo_address`
    /// on apply since the list may have changed while the fetch was in flight.
    pub repo_index: usize,
    pub repo_address: String,
    pub repo_name: String,
    pub apply_client_parameters: bool,
    pub apply_dlc_content: bool,
    pub outcome: Result<RepoMetadataPayload, String>,
}

/// Result of a background addon hash recalculation, applied on the UI thread.
/// Hashing reads and digests every file in the addon, so it runs off the UI
/// thread to keep the draw loop responsive.
#[derive(Debug)]
pub struct AddonHashRecalcResult {
    pub repo_url: String,
    pub addon_name: String,
    pub repo_name: String,
    /// `Ok(true)` = hashes recalculated, `Ok(false)` = addon not found,
    /// `Err` = recalculation failed.
    pub outcome: Result<bool, String>,
}

/// Result of an addon delete request from repository settings.
#[derive(Debug)]
pub struct AddonDeleteResult {
    pub addon_name: String,
    pub addon_path: String,
    pub outcome: Result<usize, String>,
}

/// Disposition of a background cached pending-update load. Mirrors the branches
/// of the previous synchronous loader so UI-thread state transitions are
/// preserved exactly.
#[derive(Debug)]
pub enum CachedUpdateLoadOutcome {
    /// Cached payload present and still has updates.
    Pending(Vec<ModDiffSummary>),
    /// Cached payload present but no updates remain (stale; the worker cleared
    /// the database row).
    Synced,
    /// No cached payload exists in the database.
    NoPayload,
    /// Cached payload could not be parsed (corrupt); state resets to Unknown.
    Corrupt,
    /// The database read itself failed.
    ReadError(String),
}

/// Result of a background load of a repository's cached pending-update payload
/// from SQLite, applied on the UI thread. Reading runs off the UI thread so a
/// busy database (e.g. mid-sync) can no longer stall the draw loop.
#[derive(Debug)]
pub struct CachedUpdateLoadResult {
    pub repo_url: String,
    /// Download folder this load targeted; results are routed back to the
    /// matching `(repo_url, local_path)` instance.
    pub local_path: String,
    /// When the payload still has updates, open the repository update modal once
    /// the result is applied (used by the pending-updates banner action).
    pub open_modal_when_pending: bool,
    pub outcome: CachedUpdateLoadOutcome,
}

/// Repository-space manifest fetched off the UI thread, ready to be merged into
/// app state on the UI thread by `apply_fetched_repository_space`.
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
    pub entries: Vec<RepositorySpaceEntry>,
}

/// What to do on the UI thread once a background repository-space manifest
/// fetch completes.
#[derive(Debug)]
pub enum RepositorySpaceImportContinuation {
    /// Triggered from the add-repository dialog. If no manifest is found, fall
    /// back to adding the input as a plain repository, applying the optional
    /// name/path overrides (empty = derive name from address / no chosen folder).
    AddRepositoryDialog {
        address_input: String,
        name: String,
        path: String,
    },
    /// Triggered from the Swifty migration wizard. Continues the import with the
    /// snapshot of selected Swifty repositories taken when Import was clicked.
    SwiftyMigration { selected: Vec<SwiftyDetectedRepo> },
}

/// Result of a background repository-space manifest fetch, delivered to the UI
/// thread for application. The previous timeout-less `reqwest::blocking::get`
/// here could hang the UI for many seconds against a dead server.
#[derive(Debug)]
pub struct RepositorySpaceImportResult {
    pub continuation: RepositorySpaceImportContinuation,
    /// `Ok(Some(space))` = manifest found, `Ok(None)` = no manifest at any
    /// candidate URL, `Err` = fetch/parse failure.
    pub outcome: Result<Option<FetchedRepositorySpace>, String>,
}

/// Cached aggregated backup view data to avoid per-frame clone/sort/group.
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

/// Pre-computed Color32 values for the current palette, avoiding per-frame
/// `RgbColor::to_color32()` conversions and blend computations.
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
