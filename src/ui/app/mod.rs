pub mod agent_driver;
pub mod agent_support;
mod backup;
mod diagnostics;
mod downloads;
mod init;
mod persistence;
mod repository;
mod runtime;
mod scheduling;
mod state;
mod ui_helpers;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::time::{Duration, Instant};

use super::i18n::I18n;
use super::memory::ProcessVirtualMemoryMap;
use super::palette;
use super::tray::TrayManager;
use crate::core::api::{
    self, ModDiffSummary, ProgressEvent, QuickScanProgressEvent, QuickScanResult, SyncMode,
};
use crate::core::utils::addon_backup;
use tokio::sync::broadcast::Receiver as BroadcastReceiver;
use tokio::sync::watch;

use super::types::*;
use agent_driver::AgentGuiRuntime;

pub use scheduling::{PendingPostAction, ScheduledJobRun};
pub use state::*;

/// Last logged display metrics: (monitor resolution, app resolution, scale percent).
pub type DisplayMetricsSnapshot = (Option<[i32; 2]>, Option<[i32; 2]>, i32);

#[derive(Clone, Debug)]
pub(crate) struct QuickScanProgressState {
    pub(crate) started_at: Instant,
    pub(crate) stage_label: Option<String>,
    pub(crate) stage_percent: Option<f32>,
    pub(crate) hash_counter: Option<(usize, usize)>,
    pub(crate) hash_part_counter: Option<(usize, usize)>,
    pub(crate) last_repaint: Option<Instant>,
}

impl QuickScanProgressState {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            stage_label: None,
            stage_percent: None,
            hash_counter: None,
            hash_part_counter: None,
            last_repaint: None,
        }
    }
}

pub struct Foxy {
    pub app_icon: Option<egui::TextureHandle>,
    pub default_repo_image: Option<egui::TextureHandle>,
    pub(crate) repaint_ctx: Option<egui::Context>,
    pub(crate) agent_gui: Option<AgentGuiRuntime>,
    pub current_view: FoxyView,
    pub last_view: FoxyView,
    pub main_view_state: MainViewState,
    pub repository_view_state: RepositoryViewState,
    pub repository_list_cache: RepositoryListCache,
    pub repository_list_data_version: u64,
    pub drag_source_repo_index: Option<usize>,
    pub drag_drop_target_index: Option<usize>,
    pub drag_drop_target_visual_folder_id: Option<String>,
    pub repository_spaces_version: u64,
    pub repository_spaces: Vec<RepositorySpace>,
    pub repository_visual_folders_version: u64,
    pub repository_visual_folders: Vec<RepositoryVisualFolder>,
    pub selected_repository_space_id: Option<String>,
    pub selected_repository_visual_folder_id: Option<String>,
    pub repository_space_detail_filter: String,
    pub repository_space_detail_filter_space_id: Option<String>,
    pub show_add_repository_modal: bool,
    pub add_repository_input_address: String,
    /// Optional repository name entered in the add-repository dialog. Empty
    /// falls back to a name derived from the address.
    pub add_repository_input_name: String,
    /// Optional local download path entered in the add-repository dialog. Empty
    /// leaves the repository without a chosen folder (resolved later).
    pub add_repository_input_path: String,
    pub add_repository_input_error: Option<String>,
    pub pending_repository_duplicate_add: Option<PendingRepositoryDuplicateAddState>,
    pub pending_mission_duplicate: Option<PendingMissionDuplicateState>,
    pub pending_mission_delete: Option<PendingMissionDeleteState>,
    pub pending_mission_remove_dependencies: Option<PendingMissionRemoveDependenciesState>,
    pub pending_mission_editor_launch_warning: Option<PendingMissionEditorLaunchWarningState>,
    pub pending_addon_destructive_confirmation: Option<AddonDestructiveConfirmAction>,
    pub pending_settings_folder_removal: Option<SettingsFolderRemovalConfirmAction>,
    pub pending_join_preflight: Option<PendingJoinPreflightState>,
    pub pending_join_preflight_query: Option<PendingJoinPreflightQuery>,
    /// A Join request waiting on a background server-status (A2S) query so the
    /// DNS/UDP round-trip never blocks the UI thread.
    pub pending_join_status_query: Option<PendingJoinStatusQuery>,
    pub editor_mission_search: String,
    pub editor_mission_folder: String,
    pub editor_mission_show_folders: bool,
    pub editor_mission_terrain_filter: String,
    pub repository_space_selector_state: Option<RepositorySpaceSelectorState>,
    pub repository_space_settings_state: Option<RepositorySpaceSettingsState>,
    pub pending_repository_space_bulk_action: Option<RepositorySpaceBulkAction>,
    pub repository_space_bulk_progress: Option<RepositorySpaceBulkProgress>,
    pub settings_view_state: SettingsViewState,
    pub i18n: I18n,
    pub launch_debug_mode: bool,
    pub show_debug_windows: bool,
    pub show_delete_confirmation: bool,
    pub delete_repository_delete_files: bool,
    pub show_force_redownload_confirmation: bool,
    pub show_wipe_db_confirmation: bool,
    pub show_wipe_repo_db_confirmation: bool,
    pub pending_renderer_fallback_notice: bool,
    /// Set when the local database schema is older than the schema this binary
    /// ships and the user must be prompted to wipe-and-continue (or dismiss and
    /// keep the old data at their own risk). Driven by `db_schema_version`.
    pub pending_db_schema_wipe: Option<crate::core::tasks::db_schema_version::DbSchemaWipePrompt>,
    /// Unified selection: either a server or an editor mission in the repository view.
    pub repository_selection: Option<RepositorySelection>,
    /// Cached list of detected Arma 3 profiles.
    pub detected_arma3_profiles: Vec<crate::core::arma3_profiles::Arma3Profile>,
    /// The auto-detected "currently active" Arma 3 profile name.
    pub detected_active_arma3_profile: Option<String>,
    /// Pending rename/clone/delete action from the Arma 3 profile manager
    /// in the application settings, awaiting confirmation in a modal.
    pub pending_arma3_profile_action: Option<crate::ui::views::settings::Arma3ProfileAction>,
    /// Cached editor missions for the currently viewed repository.
    pub cached_missions: Option<CachedMissionList>,
    pub previous_debug_mode: bool,
    pub stored_settings: Option<SettingsViewState>,
    pub stored_repositories: Option<RepositoryViewState>,
    pub cached_all_addons: Option<Vec<AddonInventoryEntry>>,
    pub addon_inventory_generation: u64,
    pub addon_inventory_view_cache: AddonInventoryViewCache,
    pub repository_addons_list_cache: RepositoryAddonListCache,
    pub repository_optional_addons_list_cache: RepositoryAddonListCache,
    pub repository_external_addons_list_cache: RepositoryExternalAddonsListCache,
    pub mission_row_galleys: MissionRowGalleyCache,
    /// Per-row galley caches for the remaining virtualized / visibility-culled
    /// list views, so scrolling does not re-shape newly revealed rows. See
    /// [`ListGalleyCache`] and [`crate::ui::views::galley_cache`].
    pub activity_log_galleys: ListGalleyCache,
    pub repository_list_galleys: ListGalleyCache,
    pub update_detail_file_galleys: ListGalleyCache,
    pub bulk_action_entry_galleys: ListGalleyCache,
    pub space_selector_entry_galleys: ListGalleyCache,
    pub space_selector_candidate_galleys: ListGalleyCache,
    pub space_detail_candidate_galleys: ListGalleyCache,
    pub server_row_galleys: ListGalleyCache,
    repository_settings_addon_preload_rx: StdReceiver<RepositorySettingsAddonPreloadResult>,
    repository_settings_addon_preload_tx: StdSender<RepositorySettingsAddonPreloadResult>,
    repository_settings_addon_preload_worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) repository_addon_size_load_rx: StdReceiver<RepositoryAddonSizeLoadResult>,
    pub(crate) repository_addon_size_load_tx: StdSender<RepositoryAddonSizeLoadResult>,
    pub(crate) repository_addon_size_load_pending: bool,
    pub repository_addon_size_bytes_by_repo_and_addon: HashMap<(String, String), u64>,
    // For repository settings
    pub selected_repository_for_settings: Option<usize>,
    pub current_repository_settings_tab: RepositorySettingsTab,
    pub current_help_tab: HelpTab,
    pub current_about_tab: AboutTab,
    pub addons_filter: String,
    pub addons_search_files: bool,
    pub optional_addons_filter: String,
    pub external_addons_filter: String,
    pub external_addons_origin_filter: String,
    pub external_addons_group_by_origin: bool,
    pub optional_addons_search_files: bool,
    pub external_addons_search_files: bool,
    pub addon_state_filter: String,
    pub addon_favorites_only_filter: bool,
    pub addon_client_side_only_filter: bool,
    pub cached_icons: HashMap<String, egui::TextureHandle>,
    pub cached_repo_images: HashMap<String, egui::TextureHandle>,
    pub server_statuses: HashMap<(String, String), ServerStatusCache>,
    pub server_refresh_indicator_until: HashMap<(String, String), Instant>,
    pub pending_server_queries: HashSet<(String, String)>,
    pub pending_queries: Vec<std::thread::JoinHandle<()>>,
    pub server_updates: StdReceiver<(String, String, ServerOnlineStatus)>,
    pub updates_sender: StdSender<(String, String, ServerOnlineStatus)>,
    pub join_preflight_cache: HashMap<(String, u16), JoinPreflightCacheEntry>,
    pub join_preflight_worker: Option<std::thread::JoinHandle<()>>,
    pub join_preflight_result_rx: StdReceiver<JoinPreflightQueryResult>,
    pub join_preflight_result_tx: StdSender<JoinPreflightQueryResult>,
    image_result_rx: StdReceiver<ImageLoadResult>,
    image_result_tx: StdSender<ImageLoadResult>,
    pending_image_jobs: HashSet<(String, bool)>,
    repo_metadata_result_rx: StdReceiver<RepoMetadataFetchResult>,
    repo_metadata_result_tx: StdSender<RepoMetadataFetchResult>,
    /// Repository addresses with an in-flight background metadata fetch, to
    /// avoid dispatching duplicate concurrent fetches for the same repo.
    pending_repo_metadata_jobs: HashSet<String>,
    /// Background repository-space manifest import (network fetch off the UI
    /// thread). Results are applied by `poll_repository_space_import_results`.
    repository_space_import_result_rx: StdReceiver<RepositorySpaceImportResult>,
    repository_space_import_result_tx: StdSender<RepositorySpaceImportResult>,
    /// True while a repository-space manifest fetch is in flight, to disable
    /// duplicate dispatches (e.g. repeated dialog submits) while it runs.
    /// Read from view code to show progress; only mutated within app modules.
    pub(crate) repository_space_import_in_flight: bool,
    /// Background addon hash recalculation (file hashing off the UI thread).
    /// Results are applied by `poll_addon_hash_recalc_results`.
    addon_hash_recalc_result_rx: StdReceiver<AddonHashRecalcResult>,
    addon_hash_recalc_result_tx: StdSender<AddonHashRecalcResult>,
    /// True while an addon hash recalculation worker is running.
    addon_hash_recalc_in_flight: bool,
    /// Background addon deletion from repository settings.
    addon_delete_result_rx: StdReceiver<AddonDeleteResult>,
    addon_delete_result_tx: StdSender<AddonDeleteResult>,
    pending_addon_deletes: HashSet<String>,
    /// Background load of a repository's cached pending-update payload from the
    /// database. Results are applied by `poll_cached_update_load_results`.
    cached_update_load_result_rx: StdReceiver<CachedUpdateLoadResult>,
    cached_update_load_result_tx: StdSender<CachedUpdateLoadResult>,
    /// Repository URLs with an in-flight cached pending-update load, to avoid
    /// dispatching duplicate concurrent reads for the same repository.
    pending_cached_update_loads: HashSet<String>,
    pub quick_scan_rx: StdReceiver<QuickScanResult>,
    pub quick_scan_tx: StdSender<QuickScanResult>,
    pub quick_scan_progress_rx: StdReceiver<QuickScanProgressEvent>,
    pub quick_scan_progress_tx: StdSender<QuickScanProgressEvent>,
    pub quick_scan_worker: Option<std::thread::JoinHandle<()>>,
    startup_quick_scan_filter_rx: Option<StdReceiver<StartupQuickScanFilterResult>>,
    startup_quick_scan_filter_worker: Option<std::thread::JoinHandle<()>>,
    pub fs_watch_rx: StdReceiver<api::FsChangeEvent>,
    pub fs_watch_tx: StdSender<api::FsChangeEvent>,
    pub fs_watch_worker: Option<std::thread::JoinHandle<()>>,
    pub fs_watch_suppressed_until_ms: Arc<AtomicU64>,
    pub deferred_fs_scan: HashSet<String>,
    pub pending_quick_scan_urls: HashSet<String>,
    pub pending_quick_scan_prevalidated_urls: HashSet<String>,
    pub pending_quick_scan_force_fresh_addon_hash_urls: HashSet<String>,
    pub quick_scan_pending: HashSet<String>,
    pub active_quick_scan_instance_keys: HashSet<String>,
    pub quick_scan_progress_by_instance: HashMap<String, QuickScanProgressState>,
    pub repo_db_reset_pending_recheck: HashSet<String>,
    pending_repository_db_wipes: HashSet<String>,
    pending_repository_force_redownloads: HashSet<String>,
    pending_repository_db_wipe_started_at: HashMap<String, Instant>,
    repository_db_wipe_rx: StdReceiver<RepositoryDbWipeResult>,
    repository_db_wipe_tx: StdSender<RepositoryDbWipeResult>,
    /// Completion channel for the global settings database wipe.
    database_wipe_rx: StdReceiver<Result<(), String>>,
    pub(crate) database_wipe_tx: StdSender<Result<(), String>>,
    addon_backup_task_rx: StdReceiver<AddonBackupTaskResult>,
    addon_backup_task_tx: StdSender<AddonBackupTaskResult>,
    pub addon_backup_worker: Option<std::thread::JoinHandle<()>>,
    pub addon_backup_status: Option<AddonBackupTaskStatus>,
    pub addon_backup_notice: Option<AddonBackupNotice>,
    pub addon_backup_restore_state: Option<AddonBackupRestoreState>,
    pub backup_manager_records: Vec<addon_backup::AddonBackupRecord>,
    pub backup_manager_records_version: u64,
    pub backup_manager_loaded: bool,
    pub backup_manager_filter: String,
    pub backup_manager_view_cache: Option<BackupManagerViewCache>,
    pub backup_manager_notice: Option<BackupManagerNotice>,
    pub backup_manager_confirm_action: Option<BackupManagerConfirmAction>,
    pub sync_started_at: Option<Instant>,
    pub new_profile_name: String,
    pub show_add_profile_window: bool,
    pub show_rename_profile_window: bool,
    pub pending_profile_confirm_action: Option<ProfileConfirmAction>,
    pub pending_settings_reset_confirmation: bool,
    pub show_direct_download_screen: bool,
    pub direct_download_url_input: String,
    pub direct_download_destination_input: String,
    pub direct_download_use_global_speed_limit: bool,
    pub direct_download_override_speed_unlimited: bool,
    pub direct_download_override_speed_limit_mbps: u32,
    pub direct_download_error: Option<String>,
    pub direct_download_session: Option<DirectDownloadSession>,
    direct_download_progress_rx: Option<StdReceiver<DirectDownloadProgressEvent>>,
    pub direct_download_worker: Option<std::thread::JoinHandle<()>>,
    pub direct_download_update_view: bool,
    // TS3 plugin update prompt (shown after download when plugin was updated)
    pub ts3_plugin_update_prompt: Option<Ts3PluginUpdatePrompt>,
    // TS3 plugin background scan state
    pub ts3_plugin_cache: Option<Vec<crate::core::ts3_plugin::Ts3PluginInfo>>,
    pub ts3_plugin_scan_rx:
        Option<StdReceiver<(Vec<crate::core::ts3_plugin::Ts3PluginInfo>, bool)>>,
    pub ts3_plugin_scanning: bool,
    pub ts3_running_cache: Option<bool>,
    /// Throttle marker for re-checking TeamSpeak- and Steam-running state while
    /// the join/launch preflight modal is open, so those warnings auto-clear
    /// once TS3 or Steam starts.
    pub prelaunch_recheck_at: Option<std::time::Instant>,
    // Core sync
    pub backend_progress_rx: Option<BroadcastReceiver<ProgressEvent>>,
    pub backend_worker: Option<std::thread::JoinHandle<()>>,
    startup_pending_restore_rx: Option<StdReceiver<Vec<StartupPendingUpdateRestoreRecord>>>,
    startup_pending_restore_worker: Option<std::thread::JoinHandle<()>>,
    pub startup_recheck_queue: VecDeque<(String, String, SyncMode)>,
    pub repository_space_sync_queue: VecDeque<(String, usize, SyncMode)>,
    pub repository_visual_folder_sync_queue: VecDeque<(usize, SyncMode)>,
    pub addon_hash_recalc_queue: VecDeque<(String, String)>,
    /// The scheduled job currently executing its recheck/download pipeline, if
    /// any. Runtime-only; never persisted. See `src/ui/app/scheduling/`.
    pub scheduler_active_run: Option<ScheduledJobRun>,
    /// A pending post-action (close app / shut down PC) waiting out its
    /// cancellable countdown before it fires. Runtime-only.
    pub scheduler_pending_post_action: Option<PendingPostAction>,
    /// Open editor draft for the Scheduling tab (Add/Edit). Runtime-only.
    pub scheduling_editor: Option<ScheduleJobDraft>,
    pub syncing_repository: Option<usize>,
    /// Repositories that need a repo.json metadata refresh on the next frame.
    pub pending_repo_metadata_refresh: Vec<usize>,
    pub pending_update_cache: HashMap<String, Vec<ModDiffSummary>>,
    pub mod_diff_cache: Vec<ModDiffSummary>,
    pub progress_events: VecDeque<ProgressEvent>,
    pub update_modal_open: bool,
    pub update_ready_repo: Option<usize>,
    pub current_sync_mode: Option<SyncMode>,
    pub download_progress: Option<(String, f32)>,
    pub download_finished: bool,
    pub download_finished_repo: Option<usize>,
    pub download_summary: Option<DownloadSummary>,
    pub open_update_after_sync: bool,
    pub needs_repaint: bool,
    pub mod_download_progress: HashMap<String, (f32, usize, usize, u64, u64)>,
    pub download_started_at: Option<Instant>,
    pub download_stage_started_at: Option<Instant>,
    pub hash_stage_started_at: Option<Instant>,
    pub download_stage_duration: Option<Duration>,
    pub hash_stage_duration: Option<Duration>,
    pub cumulative_hash_duration: Duration,
    pub download_speed_bps: f64,
    pub download_speed_sample_at: Option<Instant>,
    pub download_speed_sample_bytes: u64,
    pub total_downloaded_bytes: u64,
    pub download_eta_remaining: Option<Duration>,
    pub download_eta_updated_at: Option<Instant>,
    pub download_pause_tx: Option<watch::Sender<bool>>,
    pub download_paused: bool,
    pub cancel_tx: Option<watch::Sender<bool>>,
    pub recheck_stage_label: Option<String>,
    pub recheck_stage_percent: Option<f32>,
    pub recheck_hash_counter: Option<(usize, usize)>,
    pub recheck_hash_part_counter: Option<(usize, usize)>,
    pub last_hash_progress_repaint: Option<Instant>,
    pub download_hash_sample_at: Option<Instant>,
    pub download_hash_sample_files: usize,
    pub download_hash_sample_parts: usize,
    pub completed_repository_check_banner: Option<RepositoryCheckCompletionState>,
    pub completed_repository_db_wipe_banner: Option<RepositoryDbWipeCompletionState>,
    pub repo_states: HashMap<String, RepoState>,
    /// Bumped whenever a repository's sync state changes, so the repository list
    /// filter cache knows to rebuild when filtering by installed/not-installed
    /// state.
    pub repo_states_version: u64,
    pub repo_foxy_modes: HashMap<String, bool>,
    pub pending_repository_context_confirmation: Option<RepositoryContextConfirmAction>,
    pub pending_repository_space_delete_id: Option<String>,
    pub pending_repository_visual_folder_edit: Option<RepositoryVisualFolderEditState>,
    pub pending_repository_visual_folder_delete: Option<RepositoryVisualFolderDeleteState>,
    pub startup_frame_rendered: bool,
    pub startup_tasks_started: bool,
    pub close_requested_at: Option<Instant>,
    pub update_modal_sorted_mod_indices: Vec<usize>,
    pub update_modal_mod_name_lowers: Vec<String>,
    pub update_modal_sort_generation: u64,
    pub update_modal_sorted_generation: u64,
    pub update_modal_sort_last_progress_invalidation: Option<Instant>,
    pub activity_log_cache: Vec<api::LogEntry>,
    pub activity_log_generation: u64,
    pub activity_log_last_poll_at: Option<Instant>,
    pub(crate) activity_log_filter_error: bool,
    pub(crate) activity_log_filter_warn: bool,
    pub(crate) activity_log_filter_info: bool,
    pub(crate) activity_log_filter_debug: bool,
    pub(crate) activity_log_filter_trace: bool,
    pub(crate) activity_log_search: String,
    ui_toast: Option<UiToastState>,
    pub(crate) editor_launch_cooldown_until: Option<Instant>,
    pub(crate) last_incomplete_config_sync_toast_at: Option<Instant>,
    pub show_memory_diagnostics_window: bool,
    /// Smoothed frames-per-second estimate driving the optional on-screen FPS
    /// counter. Runtime-only; not persisted.
    pub fps_ema: f32,
    pub memory_diagnostics_history: VecDeque<MemoryDiagnosticsSample>,
    pub memory_diagnostics_pinned_baseline: Option<MemoryDiagnosticsSample>,
    pub memory_diagnostics_last_sample_at: Option<Instant>,
    pub memory_diagnostics_last_logged_stage_key: Option<String>,
    pub memory_diagnostics_process_map: Option<ProcessVirtualMemoryMap>,
    pub memory_diagnostics_last_process_map_at: Option<Instant>,
    pub tracked_icon_texture_bytes: HashMap<String, usize>,
    pub tracked_repo_image_texture_bytes: HashMap<String, usize>,
    pub app_icon_texture_bytes: usize,
    pub default_repo_image_texture_bytes: usize,
    pub last_applied_palette: Option<palette::PaletteColors>,
    pub cached_color32: Option<CachedPaletteColor32>,
    /// Size of egui's font atlas the last time the persistent galley caches were
    /// checked. A change signals an atlas recreation/growth that invalidates any
    /// `Arc<Galley>` held across frames, so we drop those caches the frame it moves.
    pub last_font_image_size: [usize; 2],
    pub last_saved_window_state: Option<WindowState>,
    /// Used to log monitor/app resolutions at startup and whenever they change.
    pub last_logged_display_metrics: Option<DisplayMetricsSnapshot>,
    pub tray_manager: Option<TrayManager>,
    pub hidden_to_tray: bool,
    persistence_request_tx: StdSender<PersistenceRequest>,
    persistence_result_rx: StdReceiver<PersistenceResult>,
    pub settings_dirty: bool,
    settings_revision: u64,
    settings_last_mutated_at: Option<Instant>,
    settings_save_in_flight_revision: Option<u64>,
    settings_completed_revision: u64,
    pub repositories_dirty: bool,
    repositories_revision: u64,
    repositories_last_mutated_at: Option<Instant>,
    repositories_save_in_flight_revision: Option<u64>,
    repositories_completed_revision: u64,
    pub backup_inventory_refresh_requested: bool,
    backup_inventory_refresh_in_progress: bool,
    backup_inventory_request_id: u64,
    backup_inventory_in_flight_request_id: Option<u64>,
    // App update system
    pub app_update_status: crate::core::tasks::app_update::UpdateCheckStatus,
    pub app_update_event_rx: Option<StdReceiver<crate::core::tasks::app_update::AppUpdateEvent>>,
    pub app_update_download_rx: Option<StdReceiver<crate::core::tasks::app_update::AppUpdateEvent>>,
    pub app_update_changelog_rx:
        Option<StdReceiver<crate::core::tasks::app_update::AppUpdateEvent>>,
    pub app_update_changelog_tx: Option<StdSender<crate::core::tasks::app_update::AppUpdateEvent>>,
    pub app_update_last_check: Option<Instant>,
    pub app_update_changelogs: Vec<crate::core::tasks::app_update::ChangelogVersion>,
    pub app_update_changelog_loading: HashSet<String>,
    pub app_update_changelogs_requested: bool,
    // Swifty migration
    pub swifty_migration_state: crate::ui::views::swifty_migration::types::SwiftyMigrationState,
}
