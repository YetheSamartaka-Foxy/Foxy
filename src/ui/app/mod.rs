mod agent_caption;
pub mod agent_driver;
pub mod agent_support;
mod backup;
pub mod benchmarks;
pub mod debug_modals;
mod diagnostics;
mod downloads;
mod init;
mod persistence;
pub(crate) mod profile_backups;
mod repository;
mod runtime;
mod scheduling;
pub(crate) mod server;
mod state;
mod ui_helpers;
pub(crate) use ui_helpers::activity::{
    ActivityBar, ActivityGlyph, ActivityIcon, BarMotion, ProgressOrb, StepTrack, VerificationMap,
    paint_area_under, paint_bar_label, paint_border_beam, paint_border_flare, paint_live_marker,
    rise_effect, transfer_progress,
};
#[cfg(test)]
pub(crate) use ui_helpers::spinner::PROGRESS_FRAME_INTERVAL;
pub(crate) use ui_helpers::spinner::{
    PacedSpinner, WindowPresence, progress_frame_interval, progress_frame_interval_override,
    request_frame_after, set_progress_frame_rate, set_window_presence, window_presence,
};

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

pub use downloads::UpdateModalDiskSpaceProbe;
pub(crate) use downloads::direct_download_managed_paths_problem;
pub use repository::game_space_overview::{RepositoryGroupSummary, TeamSpeakSummary};
pub(crate) use runtime::DatabaseLaunchContinuation;
pub(crate) use runtime::LaunchRequest;
pub(crate) use runtime::client_workers::{
    admit_database_section, current_worker_result, reap_worker,
};
pub use scheduling::{HeldScheduledJob, PendingPostAction, ScheduledJobRun};
pub use state::*;

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
    pub(crate) interrupted_update_instances: HashSet<String>,
    pub(crate) startup_recovery_rx:
        Option<std::sync::mpsc::Receiver<runtime::repository_status::UpdateRecoveryResult>>,
    pub(crate) pending_download_retry: Option<runtime::download_retry::PendingDownloadRetry>,
    pub(crate) download_retry_attempts: HashMap<String, u32>,
    pub(crate) sync_retryable_failure: bool,
    pub(crate) sync_is_bulk: bool,
    pub(crate) sync_visual_folder_id: Option<String>,
    pub(crate) paused_repository_instances: HashSet<String>,
    pub(crate) pending_game_space_switch_save_error: Option<String>,
    pub(crate) pending_game_space_switch_discard_save: bool,
    pub(crate) unreadable_config_replacements:
        HashMap<std::path::PathBuf, Option<std::path::PathBuf>>,
    pub(crate) show_unreadable_config_replacement: bool,
    pub(crate) pending_update_launch: Option<PendingUpdateLaunch>,
    pub(crate) scheduling_job_change_prompt: Option<scheduling::ScheduledJobChangePrompt>,
    pub app_icon: Option<egui::TextureHandle>,
    pub tfr_logo: Option<egui::TextureHandle>,
    pub default_repo_image: Option<egui::TextureHandle>,
    pub game_logo_textures: crate::ui::game_logos::GameLogoTextures,
    pub(crate) repaint_ctx: Option<egui::Context>,
    pub(crate) agent_gui: Option<AgentGuiRuntime>,
    pub current_view: FoxyView,
    pub(crate) launch_mode_override: Option<crate::core::app_mode::AppMode>,
    pub(crate) mode_navigation: runtime::AppModeNavigation,
    pub(crate) server_view: server::ServerViewState,
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
    pub(crate) game_space_overview: repository::game_space_overview::GameSpaceOverviewState,
    pub repository_space_detail_filter: String,
    pub repository_space_detail_filter_space_id: Option<String>,
    pub show_add_repository_modal: bool,
    pub add_repository_input_address: String,

    pub add_repository_input_name: String,

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

    pub wipe_db_include_benchmarks: bool,
    pub show_wipe_repo_db_confirmation: bool,
    pub benchmark_armed: Option<benchmarks::BenchmarkArm>,
    pub benchmark_capture: Option<benchmarks::BenchmarkCapture>,
    pub benchmark_prompt: Option<benchmarks::BenchmarkDraft>,
    pub benchmarks_view: benchmarks::BenchmarksViewState,
    pub benchmark_channels: benchmarks::BenchmarkChannels,
    pub pending_renderer_fallback_notice: bool,

    pub pending_db_schema_wipe: Option<crate::core::tasks::db_schema_version::DbSchemaWipePrompt>,
    pub(crate) show_db_schema_wipe_prompt: bool,
    pub(crate) pending_database_launch: Option<runtime::DatabaseLaunchContinuation>,

    pub db_schema_wipe_recheck_all: bool,
    pub recheck_all_after_database_wipe: bool,
    pub(crate) database_wipe_in_flight: bool,
    /// Turso forbids concurrent processes on one database; when another Foxy owns it, this window may only close.
    pub db_lock_conflict: Option<crate::core::tasks::db_process_lock::LockOutcome>,
    /// Warn when a drive Foxy writes through falls below the free-space floor; filling mid-sync can leave partial state.
    pub pending_low_space_notice: bool,
    /// Store startup storage findings until the user dismisses or acknowledges them.
    pub storage_compat_notice: Option<runtime::StorageCompatNotice>,

    pub repository_selection: Option<RepositorySelection>,

    pub detected_arma3_profiles: Vec<crate::core::arma3_profiles::Arma3Profile>,
    pub(crate) profile_backup_state: profile_backups::ProfileBackupState,

    pub detected_active_arma3_profile: Option<String>,

    pub pending_arma3_profile_action: Option<crate::ui::views::settings::Arma3ProfileAction>,

    pub cached_missions: Option<CachedMissionList>,
    /// Mission scans run off-thread because profiles with many missions can delay first paint.
    pub(crate) mission_scan_rx: Option<StdReceiver<MissionScanDelivery>>,
    /// Retained until the handoff joins it. Dropping the handle would detach the scan.
    pub(crate) mission_scan_worker: Option<std::thread::JoinHandle<()>>,

    pub(crate) mission_scan_in_flight: Option<String>,
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
    repository_settings_addon_preload_worker: Option<(u64, std::thread::JoinHandle<()>)>,
    pub(crate) repository_addon_size_load_rx: StdReceiver<RepositoryAddonSizeLoadResult>,
    pub(crate) repository_addon_size_load_tx: StdSender<RepositoryAddonSizeLoadResult>,
    pub(crate) repository_addon_size_load_pending: bool,
    pub(crate) repository_addon_size_load_worker: Option<std::thread::JoinHandle<()>>,
    pub repository_addon_size_bytes_by_repo_and_addon: HashMap<(String, String), u64>,

    pub selected_repository_for_settings: Option<usize>,
    pub current_repository_settings_tab: RepositorySettingsTab,
    pub current_help_tab: HelpTab,
    pub help_search_query: String,
    pub current_about_tab: AboutTab,
    pub addons_filter: String,
    pub addons_search_files: bool,
    pub addons_bulk_filtered_only: bool,
    pub optional_addons_filter: String,
    pub external_addons_filter: String,
    pub external_addons_origin_filter: String,
    pub external_addons_group_by_origin: bool,
    pub optional_addons_search_files: bool,
    pub external_addons_search_files: bool,
    pub optional_addons_bulk_filtered_only: bool,
    pub external_addons_bulk_filtered_only: bool,
    pub addon_state_filter: String,
    pub addon_favorites_only_filter: bool,
    pub addon_client_side_only_filter: bool,
    pub cached_icons: HashMap<String, egui::TextureHandle>,
    pub cached_repo_images: HashMap<String, egui::TextureHandle>,
    pub server_statuses: HashMap<(String, String), ServerStatusCache>,
    pub server_refresh_indicator_until: HashMap<(String, String), Instant>,
    pub pending_server_queries: HashSet<(String, String)>,
    pub(crate) server_query_generation: runtime::ServerQueryGeneration,
    /// Closed while a game-space handoff is draining client workers.
    pub(crate) client_db_admission: runtime::DbAdmission,
    pub(crate) client_worker_handoff_active: bool,
    pub(crate) mode_ownership: runtime::ownership::ModeOwnership,
    pub pending_queries: Vec<std::thread::JoinHandle<()>>,
    pub(crate) server_updates: StdReceiver<runtime::ServerQueryResult>,
    pub(crate) updates_sender: StdSender<runtime::ServerQueryResult>,
    pub join_preflight_worker: Option<std::thread::JoinHandle<()>>,
    pub join_preflight_result_rx: StdReceiver<JoinPreflightQueryResult>,
    pub join_preflight_result_tx: StdSender<JoinPreflightQueryResult>,
    image_result_rx: StdReceiver<ImageLoadResult>,
    image_result_tx: StdSender<ImageLoadResult>,
    pending_image_jobs: HashSet<(String, bool)>,
    /// Capture the cache directory at dispatch so a late image fetch cannot write into the next space.
    image_workers: Vec<std::thread::JoinHandle<()>>,
    repo_metadata_result_rx: StdReceiver<RepoMetadataFetchResult>,
    repo_metadata_result_tx: StdSender<RepoMetadataFetchResult>,

    pending_repo_metadata_jobs: HashSet<String>,
    repo_metadata_workers: Vec<std::thread::JoinHandle<()>>,

    repository_space_import_result_rx: StdReceiver<RepositorySpaceImportResult>,
    repository_space_import_result_tx: StdSender<RepositorySpaceImportResult>,

    pub(crate) repository_space_import_in_flight: bool,

    pub(crate) repository_space_freshness_rx: Option<
        StdReceiver<crate::ui::app::repository::space_freshness::RepositorySpaceFreshnessResult>,
    >,
    repository_space_freshness_worker: Option<std::thread::JoinHandle<()>>,

    pub(crate) repository_space_last_refresh: Option<Instant>,

    pub(crate) periodic_remote_check_last_run: Option<Instant>,
    pub(crate) dispatching_background_sync: bool,
    pub(crate) repository_space_remote_changes:
        crate::ui::app::repository::space_freshness::RepositorySpaceRemoteChanges,

    addon_hash_recalc_result_rx: StdReceiver<AddonHashRecalcResult>,
    addon_hash_recalc_result_tx: StdSender<AddonHashRecalcResult>,

    addon_hash_recalc_in_flight: bool,
    addon_hash_recalc_worker: Option<std::thread::JoinHandle<()>>,

    addon_delete_result_rx: StdReceiver<AddonDeleteResult>,
    addon_delete_result_tx: StdSender<AddonDeleteResult>,
    pending_addon_deletes: HashSet<String>,
    maintenance_workers: Vec<std::thread::JoinHandle<()>>,

    addon_force_redownload_result_rx: StdReceiver<AddonForceRedownloadProbeResult>,
    addon_force_redownload_result_tx: StdSender<AddonForceRedownloadProbeResult>,

    pending_addon_force_redownloads: HashSet<String>,
    force_redownload_workers: Vec<std::thread::JoinHandle<()>>,

    cached_update_load_result_rx: StdReceiver<CachedUpdateLoadResult>,
    cached_update_load_result_tx: StdSender<CachedUpdateLoadResult>,

    pending_cached_update_loads: HashSet<String>,
    cached_update_load_workers: Vec<std::thread::JoinHandle<()>>,
    pub quick_scan_rx: StdReceiver<QuickScanResult>,
    pub quick_scan_tx: StdSender<QuickScanResult>,
    pub quick_scan_progress_rx: StdReceiver<QuickScanProgressEvent>,
    pub quick_scan_progress_tx: StdSender<QuickScanProgressEvent>,
    pub quick_scan_worker: Option<std::thread::JoinHandle<()>>,
    startup_quick_scan_filter_rx: Option<StdReceiver<StartupQuickScanFilterResult>>,
    startup_quick_scan_filter_worker: Option<std::thread::JoinHandle<()>>,

    startup_probe_unknown: Vec<crate::core::api::StartupRepositoryInstance>,
    startup_probe_retry_armed_at: Option<Instant>,
    startup_probe_retries_completed: u32,
    startup_probe_retry_rx: Option<StdReceiver<crate::core::api::StartupProbeRetryPlan>>,
    startup_probe_retry_worker: Option<std::thread::JoinHandle<()>>,
    pub fs_watch_rx: StdReceiver<api::FsChangeEvent>,
    pub fs_watch_tx: StdSender<api::FsChangeEvent>,
    pub fs_watch_worker: Option<std::thread::JoinHandle<()>>,
    pub fs_watch_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Retry an empty watcher only after its index changes or the retry interval, not every frame.
    pub fs_watch_idle_exit: Arc<std::sync::atomic::AtomicBool>,
    pub fs_watch_idle_retry_at: Option<Instant>,
    pub fs_watch_suppressed_until_ms: Arc<AtomicU64>,
    /// Restart the watcher when configured folders change.
    pub fs_watch_signature: Option<String>,
    /// Set when the watcher's repository/addon index may be stale (a sync
    /// created or changed addon rows it was spawned without).
    pub fs_watch_index_dirty: bool,
    pub fs_watch_observed_repositories_revision: u64,
    /// Rebuild a prepared download queue when the watcher marks its repository folder changed.
    pub fs_changed_since_prepare: HashSet<String>,
    /// Back off after repeated clean watcher scans because unrelated disk walks can emit events continuously.
    pub fs_watch_clean_scan_streak: u32,
    pub fs_watch_backoff_until: Option<Instant>,
    /// Scan backoff events together when the window closes.
    pub fs_watch_backoff_urls: HashSet<String>,
    /// Track watcher-triggered scans so their outcomes update the clean-scan streak.
    pub fs_watch_scan_urls: HashSet<String>,
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

    database_wipe_rx: StdReceiver<DatabaseWipeDelivery>,
    pub(crate) database_wipe_tx: StdSender<DatabaseWipeDelivery>,
    pub(crate) database_wipe_worker: Option<std::thread::JoinHandle<()>>,
    addon_backup_task_rx: StdReceiver<AddonBackupTaskResult>,
    addon_backup_task_tx: StdSender<AddonBackupTaskResult>,
    pub addon_backup_worker: Option<std::thread::JoinHandle<()>>,
    pending_update_clear_workers: Vec<std::thread::JoinHandle<()>>,
    overview_worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) ts3_plugin_scan_worker: Option<std::thread::JoinHandle<()>>,
    repository_space_import_worker: Option<std::thread::JoinHandle<()>>,
    benchmark_workers: Vec<std::thread::JoinHandle<()>>,
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
    backup_cleanup_task_rx: StdReceiver<BackupCleanupTaskResult>,
    backup_cleanup_task_tx: StdSender<BackupCleanupTaskResult>,
    pub backup_cleanup_worker: Option<std::thread::JoinHandle<()>>,
    pub backup_cleanup_in_flight: bool,
    pub sync_started_at: Option<Instant>,
    pub frame_cost: crate::ui::app::runtime::frame_cost::FrameCostTotals,
    pub frame_cost_at_sync_start: Option<crate::ui::app::runtime::frame_cost::FrameCostTotals>,
    pub new_profile_name: String,
    pub show_add_profile_window: bool,
    pub show_rename_profile_window: bool,
    pub pending_profile_confirm_action: Option<ProfileConfirmAction>,
    pub pending_settings_reset_confirmation: bool,
    pub show_direct_download_screen: bool,
    pub direct_download_url_input: String,
    pub direct_download_destination_input: String,
    pub(crate) pending_direct_download_collisions:
        Option<(Vec<std::path::PathBuf>, std::sync::mpsc::Sender<bool>)>,
    pub direct_download_use_global_speed_limit: bool,
    pub direct_download_override_speed_unlimited: bool,
    pub direct_download_override_speed_limit_mbps: u32,
    pub direct_download_error: Option<String>,
    pub direct_download_session: Option<DirectDownloadSession>,
    direct_download_progress_rx: Option<StdReceiver<DirectDownloadProgressEvent>>,
    pub direct_download_worker: Option<std::thread::JoinHandle<()>>,
    pub direct_download_update_view: bool,

    pub ts3_plugin_update_prompt: Option<Ts3PluginUpdatePrompt>,

    pub ts3_plugin_cache: Option<Vec<crate::core::ts3_plugin::Ts3PluginStatus>>,
    pub ts3_plugin_scan_rx: Option<StdReceiver<Ts3PluginScanResult>>,
    pub ts3_plugin_scanning: bool,
    pub ts3_running_cache: Option<bool>,

    pub ts3_plugin_scan_prompt_on_update: bool,

    pub ts3_plugin_scan_requeued: bool,
    pub(crate) prelaunch_process_checks: repository::prelaunch_checks::PrelaunchProcessChecks,
    pub(crate) launch_action: runtime::LaunchActionState,
    pub(crate) pending_installer_path: Option<std::path::PathBuf>,
    pub(crate) pending_installer_error: Option<String>,
    pub(crate) installer_owner_check: runtime::InstallerOwnerCheck,
    pub(crate) shutdown_after_close: bool,
    pub(crate) configuration_recovery_error: Option<String>,

    pub backend_progress_rx: Option<BroadcastReceiver<ProgressEvent>>,
    pub backend_worker: Option<std::thread::JoinHandle<()>>,
    startup_pending_restore_rx: Option<StdReceiver<StartupPendingUpdateRestoreBatch>>,
    startup_pending_restore_worker: Option<std::thread::JoinHandle<()>>,
    pub startup_repository_layout_logged: bool,
    pub startup_recheck_queue: VecDeque<(String, String, SyncMode)>,
    pub repository_space_sync_queue: VecDeque<repository::space_actions::QueuedRepositorySpaceSync>,
    pub repository_visual_folder_sync_queue:
        VecDeque<repository::space_actions::QueuedRepositoryVisualFolderSync>,
    pub addon_hash_recalc_queue: VecDeque<(String, String)>,

    pub scheduler_active_run: Option<ScheduledJobRun>,

    pub scheduler_pending_post_action: Option<PendingPostAction>,
    pub(crate) scheduler_deferred_post_actions: Vec<PendingPostAction>,

    pub scheduler_held_jobs: HashMap<String, HeldScheduledJob>,

    pub scheduler_last_tick_at: Option<chrono::DateTime<chrono::Local>>,

    pub addon_selection_baseline: HashMap<String, u64>,

    pub pending_addon_selection_checks: HashSet<String>,

    pub scheduling_editor: Option<ScheduleJobDraft>,
    pub syncing_repository: Option<usize>,

    pub pending_repo_metadata_refresh: Vec<usize>,
    pub pending_update_cache: HashMap<String, Vec<ModDiffSummary>>,
    pub mod_diff_cache: Vec<ModDiffSummary>,
    pub progress_events: VecDeque<ProgressEvent>,
    pub(crate) hash_evidence_state:
        Option<Arc<crate::core::tasks::calculate_hashes::HashEvidenceState>>,
    hash_evidence_retry_tx: StdSender<Result<bool, String>>,
    hash_evidence_retry_rx: StdReceiver<Result<bool, String>>,
    pub(crate) hash_evidence_retry_in_flight: bool,
    pub(crate) hash_evidence_notice_open: bool,
    pub(crate) hash_evidence_notice_seen: (usize, usize, bool),
    pub(crate) hash_stall_acknowledged: HashSet<String>,
    pub update_modal_open: bool,
    pub update_ready_repo: Option<usize>,
    pub local_drift_addons: Vec<String>,
    pub backup_local_drift_requested: bool,
    pub(crate) empty_publication_status: Option<(String, String, bool, Vec<String>)>,
    pub enrolled_link_targets: Vec<api::LinkedTargetPreview>,
    pub(crate) cleanup_plan_files: Vec<String>,
    pub(crate) pending_link_target_sync: Option<PendingLinkedTargetSync>,
    pub(crate) pending_link_target_previews: Option<Vec<api::LinkedTargetPreview>>,
    pub(crate) link_target_preview_bypass_once: bool,
    link_target_preview_tx: StdSender<LinkedTargetPreviewResult>,
    link_target_preview_rx: StdReceiver<LinkedTargetPreviewResult>,
    retained_file_preview_tx: StdSender<RetainedFilePreviewResult>,
    retained_file_preview_rx: StdReceiver<RetainedFilePreviewResult>,
    retained_file_removal_tx: StdSender<RetainedFileRemovalResult>,
    retained_file_removal_rx: StdReceiver<RetainedFileRemovalResult>,
    pub(crate) retained_files_cleanup_in_flight: bool,
    pub(crate) pending_retained_file_removal: Option<PendingRetainedFileRemoval>,
    pub current_sync_mode: Option<SyncMode>,
    pub(crate) active_sync_selection: Option<Vec<(String, bool)>>,
    pub(crate) active_sync_modifiers: Option<(bool, bool, bool, bool)>,
    pub(crate) proxy_change_prompt: Option<ProxyChangePrompt>,
    pub(crate) game_space_switch_prepare_rx:
        Option<StdReceiver<Result<crate::core::tasks::db_process_lock::ReservedClaim, String>>>,
    pub(crate) game_space_switch_prepare_worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) game_space_switch_open_error: Option<String>,
    pub(crate) legacy_migration_review_files: Vec<String>,
    pub(crate) settings_save_error: Option<String>,
    pub(crate) pending_integrity_recheck_prompt: Option<(String, String)>,
    pub(crate) integrity_recheck_after_settlement: Option<(String, String)>,
    pub download_progress: Option<(String, f32)>,
    pub download_finished: bool,
    pub(crate) download_transfer_attempt: u32,
    pub download_finished_repo: Option<usize>,
    pub download_summary: Option<DownloadSummary>,
    /// Keep the last space-shortfall result so the update modal can explain the refused download.
    pub download_disk_space_shortfall:
        Option<(usize, crate::core::utils::disk_space::DiskSpaceShortfall)>,
    pub update_modal_disk_space_probe: Option<UpdateModalDiskSpaceProbe>,
    pub open_update_after_sync: bool,
    pub needs_repaint: bool,
    pub mod_download_progress: HashMap<String, (f32, usize, usize, u64, u64)>,
    pub mod_download_attempts: HashMap<String, usize>,
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
    pub(crate) completion_estimate: downloads::CompletionEstimateState,
    pub download_pause_tx: Option<watch::Sender<bool>>,
    pub download_paused: bool,
    pub cancel_tx: Option<watch::Sender<bool>>,
    pub recheck_stage_label: Option<String>,
    pub recheck_stage_percent: Option<f32>,
    pub recheck_hash_counter: Option<(usize, usize)>,
    pub recheck_hash_part_counter: Option<(usize, usize)>,
    pub recheck_hash_byte_counter: Option<(u64, u64)>,
    /// Track the highest hash fraction so later stages with lower percentages do not move the bar backward.
    pub recheck_progress_peak: Option<f32>,
    /// Continue from the progress reached before hashing so the bar never falls back to zero.
    pub recheck_progress_floor: Option<f32>,

    pub recheck_hash_estimate: Option<(u64, u64)>,
    pub last_hash_progress_repaint: Option<Instant>,
    pub download_hash_sample_at: Option<Instant>,
    pub download_hash_sample_files: usize,
    pub download_hash_sample_parts: usize,
    pub completed_repository_check_banner: Option<RepositoryCheckCompletionState>,
    pub completed_repository_db_wipe_banner: Option<RepositoryDbWipeCompletionState>,
    pub repo_states: HashMap<String, RepoState>,
    /// Invalidate repository-list filter results whenever sync state changes.
    pub repo_states_version: u64,
    pub repo_foxy_modes: HashMap<String, bool>,
    pub pending_repository_context_confirmation: Option<RepositoryContextConfirmAction>,
    pub pending_repository_space_delete_id: Option<String>,
    pub pending_repository_visual_folder_edit: Option<RepositoryVisualFolderEditState>,
    pub pending_repository_visual_folder_delete: Option<RepositoryVisualFolderDeleteState>,
    pub startup_frame_rendered: bool,
    pub startup_tasks_started: bool,
    /// Build the startup storage summary off-thread because it can take hundreds of milliseconds.
    pub(crate) startup_diagnostics_rx: Option<StdReceiver<api::StartupDiagnosticsReport>>,

    pub(crate) startup_first_frame_at: Option<Duration>,

    pub(crate) startup_quick_scan_requested: usize,

    pub(crate) startup_sync: Option<runtime::StartupSyncTracker>,
    pub close_requested_at: Option<Instant>,
    /// Set while a close waits for a cancelled sync to roll back its files.
    pub close_worker_wait_started_at: Option<Instant>,
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

    pub fps_ema: f32,

    pub frame_intervals_ms: VecDeque<f32>,
    pub memory_diagnostics_history: VecDeque<MemoryDiagnosticsSample>,
    pub memory_diagnostics_pinned_baseline: Option<MemoryDiagnosticsSample>,
    pub memory_diagnostics_last_sample_at: Option<Instant>,
    pub memory_diagnostics_last_logged_stage_key: Option<String>,
    pub memory_diagnostics_process_map: Option<ProcessVirtualMemoryMap>,
    pub memory_diagnostics_last_process_map_at: Option<Instant>,
    pub tracked_icon_texture_bytes: HashMap<String, usize>,
    pub tracked_repo_image_texture_bytes: HashMap<String, usize>,
    pub app_icon_texture_bytes: usize,
    pub tfr_logo_texture_bytes: usize,
    pub default_repo_image_texture_bytes: usize,
    pub last_applied_palette: Option<palette::PaletteColors>,
    pub cached_color32: Option<CachedPaletteColor32>,
    /// Drop cached galleys when the font atlas changes; their UVs no longer reference the current atlas.
    pub last_font_image_size: [usize; 2],
    pub last_font_pixels_per_point: f32,
    pub last_saved_window_state: Option<WindowState>,

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
    /// Show the launch update prompt for this session only; it returns next launch until Foxy is updated.
    pub pending_app_update_prompt: bool,
    /// Armed by the launch update check so only that check can raise the
    /// prompt; a manual re-check from settings must not reopen it.
    pub app_update_prompt_armed: bool,
    /// Debug-modal previews skip their real side effects.
    pub debug_modal_previews: Vec<debug_modals::DebugModal>,

    pub swifty_migration_state: crate::ui::views::swifty_migration::types::SwiftyMigrationState,

    pub game_spaces_view_state: crate::ui::views::game_spaces::GameSpacesViewState,
    pub game_space_settings_view_state:
        crate::ui::views::game_spaces::settings::GameSpaceSettingsViewState,

    pub pending_game_space_switch: Option<crate::core::game::spaces::GameSpaceEntry>,
    /// Record when a space-switch request becomes visible in the target state.
    pub game_space_switch_requested_at: Option<std::time::Instant>,

    pub workshop_view_state: crate::ui::views::workshop::WorkshopViewState,
    pub(crate) addon_workshop_lookups: Vec<StdReceiver<Option<String>>>,
    /// Run Workshop downloads and folder copies off the frame loop.
    pub workshop_task_rx:
        Option<std::sync::mpsc::Receiver<crate::ui::views::workshop::tasks::WorkshopTaskOutcome>>,
    pub workshop_task_worker: Option<std::thread::JoinHandle<()>>,
    pub(crate) workshop_task_generation: u64,
}
