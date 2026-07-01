use super::enums::{AppUpdateMode, HashIoProfilePreference, UiRendererPreference};
use super::repository::{ActiveUpdateSession, UpdateSummaryNotice};
use super::scheduling::ScheduledJob;
use crate::ui::fonts::FontSizes;
use crate::ui::palette::PaletteColors;
use crate::ui::theme::Theme;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct SettingsViewState {
    pub debug_mode: bool,
    pub show_debug_windows: bool,
    #[serde(default)]
    pub show_activity_log: bool,
    #[serde(default)]
    pub show_memory_diagnostics_icon: bool,
    #[serde(default)]
    pub show_fps_counter: bool,
    /// Globally hide the repository banner image in the repository and space views.
    #[serde(default)]
    pub hide_repository_image: bool,
    pub close_after_launch: bool,
    #[serde(default)]
    pub hide_to_tray_after_launch: bool,
    #[serde(default = "default_auto_recheck_on_launch")]
    pub auto_recheck_on_launch: bool,
    #[serde(default = "default_auto_quick_scan_on_launch")]
    pub auto_quick_scan_on_launch: bool,
    #[serde(default)]
    pub auto_backup_on_update: bool,
    #[serde(default = "default_apply_repo_json_client_parameters")]
    pub apply_repo_json_client_parameters: bool,
    #[serde(default = "default_apply_repo_json_dlc_content")]
    pub apply_repo_json_dlc_content: bool,
    #[serde(default = "default_warn_editor_external_addons")]
    pub warn_editor_external_addons: bool,
    #[serde(default = "default_enable_editor_mission_list")]
    pub enable_editor_mission_list: bool,
    #[serde(default = "default_enable_server_list")]
    pub enable_server_list: bool,
    #[serde(default = "default_check_server_addons_before_join")]
    pub check_server_addons_before_join: bool,
    #[serde(default = "default_check_ts3_running_before_join")]
    pub check_ts3_running_before_join: bool,
    #[serde(default = "default_check_steam_running_before_launch")]
    pub check_steam_running_before_launch: bool,
    #[serde(default = "default_backup_keep_latest_per_addon")]
    pub backup_keep_latest_per_addon: u32,
    #[serde(default = "default_backup_max_age_days")]
    pub backup_max_age_days: Option<u32>,
    pub current_tab: String,
    pub arma3_directory: String,
    #[serde(default)]
    pub arma3_profiles_directory: String,
    #[serde(default)]
    pub steam_directory: String,
    #[serde(default)]
    pub teamspeak3_directory: String,
    pub temp_directory: String,
    #[serde(default)]
    pub backup_directory: String,
    #[serde(default = "default_download_speed_limit_mbps")]
    pub download_speed_limit_mbps: Option<u32>,
    #[serde(default)]
    pub hash_io_profile: HashIoProfilePreference,
    #[serde(default)]
    pub ui_renderer: UiRendererPreference,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default)]
    pub locale_preference_migrated: bool,
    #[serde(default)]
    pub additional_folders: Vec<String>,
    #[serde(default)]
    pub additional_folder_aliases: HashMap<String, String>,
    #[serde(default)]
    pub cleanup_folders: Vec<(String, bool)>,
    #[serde(default)]
    pub font_sizes: FontSizes,
    /// Global UI scale as a percentage applied to the egui zoom factor.
    /// 100 means the platform's native scale; range is clamped to 5..=500.
    #[serde(default = "default_ui_scale_percent")]
    pub ui_scale_percent: u16,
    /// Pending UI scale edited by the slider; applied to `ui_scale_percent`
    /// only when the user clicks Apply. Not persisted.
    #[serde(skip)]
    pub ui_scale_percent_draft: u16,
    #[serde(default)]
    pub update_view_font_hierarchy_migrated: bool,
    #[serde(default)]
    pub palette_colors: PaletteColors,
    /// User-created theme snapshots persisted with the rest of the settings.
    #[serde(default)]
    pub saved_themes: Vec<Theme>,
    /// Selected saved theme in the customization view. Not persisted.
    #[serde(skip)]
    pub selected_saved_theme: Option<usize>,
    /// Name editor for creating or updating a saved theme. Not persisted.
    #[serde(skip)]
    pub saved_theme_name_draft: String,
    /// Whether the add-theme modal is open. Not persisted.
    #[serde(skip)]
    pub show_add_theme_modal: bool,
    /// Name editor used by the add-theme modal. Not persisted.
    #[serde(skip)]
    pub new_theme_name_draft: String,
    /// Requests initial keyboard focus for the add-theme name field.
    #[serde(skip)]
    pub focus_new_theme_name: bool,
    #[serde(default)]
    pub update_summary_notices: Vec<UpdateSummaryNotice>,
    #[serde(default)]
    pub active_update_sessions: Vec<ActiveUpdateSession>,
    /// Whether to check for updates from a custom server or GitHub Releases.
    #[serde(default)]
    pub app_update_mode: AppUpdateMode,
    /// Base URL of the update source (e.g. "http://myserver.com/foxy/")
    #[serde(default)]
    pub app_update_url: String,
    /// GitHub repository slug (e.g. "owner/repo"). Used when mode == GitHub.
    #[serde(default = "default_app_update_github_repo")]
    pub app_update_github_repo: String,
    /// True when the update source mode was explicitly changed by the user.
    #[serde(default)]
    pub app_update_mode_user_override: bool,
    /// True when the update source URL is explicitly set by the user in Settings.
    #[serde(default)]
    pub app_update_url_user_override: bool,
    /// Whether to auto-check for app updates on launch.
    #[serde(default = "default_app_update_auto_check")]
    pub app_update_auto_check: bool,
    /// Interval in minutes between auto-checks (0 = only on launch).
    #[serde(default = "default_app_update_check_interval")]
    pub app_update_check_interval_minutes: u32,
    /// Maps plugin file path to BLAKE3 hash of the last installed version.
    #[serde(default)]
    pub ts3_installed_plugin_hashes: HashMap<String, String>,
    /// Whether the Swifty migration wizard has been offered to the user.
    #[serde(default)]
    pub swifty_migration_offered: bool,
    /// User-defined scheduled jobs (Settings -> Scheduling). Each runs an opt-in
    /// recheck/download pipeline and optional post-action while Foxy is open.
    #[serde(default)]
    pub scheduled_jobs: Vec<ScheduledJob>,
    #[serde(skip)]
    pub additional_folders_filter: String,
    #[serde(skip)]
    pub cleanup_folders_filter: String,
    #[serde(skip)]
    pub language_filter: String,
}

fn default_locale() -> String {
    "system".to_string()
}

fn default_download_speed_limit_mbps() -> Option<u32> {
    None
}

fn default_backup_keep_latest_per_addon() -> u32 {
    5
}

/// Default global UI scale percentage (100% = native platform scale).
pub const DEFAULT_UI_SCALE_PERCENT: u16 = 100;
/// Smallest selectable UI scale percentage.
pub const MIN_UI_SCALE_PERCENT: u16 = 25;
/// Largest selectable UI scale percentage.
pub const MAX_UI_SCALE_PERCENT: u16 = 500;

fn default_ui_scale_percent() -> u16 {
    DEFAULT_UI_SCALE_PERCENT
}

fn default_auto_recheck_on_launch() -> bool {
    true
}

fn default_auto_quick_scan_on_launch() -> bool {
    true
}

fn default_apply_repo_json_client_parameters() -> bool {
    true
}

fn default_apply_repo_json_dlc_content() -> bool {
    true
}

fn default_warn_editor_external_addons() -> bool {
    true
}

fn default_enable_editor_mission_list() -> bool {
    true
}

fn default_enable_server_list() -> bool {
    true
}

fn default_check_server_addons_before_join() -> bool {
    true
}

fn default_check_ts3_running_before_join() -> bool {
    true
}

fn default_check_steam_running_before_launch() -> bool {
    true
}

fn default_backup_max_age_days() -> Option<u32> {
    None
}

fn default_app_update_check_interval() -> u32 {
    60
}

fn default_app_update_auto_check() -> bool {
    true
}

fn default_app_update_github_repo() -> String {
    "YetheSamartaka-Foxy/Foxy".to_string()
}

impl Default for SettingsViewState {
    fn default() -> Self {
        Self {
            debug_mode: false,
            show_debug_windows: false,
            show_activity_log: false,
            show_memory_diagnostics_icon: false,
            show_fps_counter: false,
            hide_repository_image: false,
            close_after_launch: true,
            hide_to_tray_after_launch: false,
            auto_recheck_on_launch: default_auto_recheck_on_launch(),
            auto_quick_scan_on_launch: default_auto_quick_scan_on_launch(),
            auto_backup_on_update: false,
            apply_repo_json_client_parameters: default_apply_repo_json_client_parameters(),
            apply_repo_json_dlc_content: default_apply_repo_json_dlc_content(),
            warn_editor_external_addons: default_warn_editor_external_addons(),
            enable_editor_mission_list: default_enable_editor_mission_list(),
            enable_server_list: default_enable_server_list(),
            check_server_addons_before_join: default_check_server_addons_before_join(),
            check_ts3_running_before_join: default_check_ts3_running_before_join(),
            check_steam_running_before_launch: default_check_steam_running_before_launch(),
            backup_keep_latest_per_addon: default_backup_keep_latest_per_addon(),
            backup_max_age_days: default_backup_max_age_days(),
            current_tab: "Application".to_string(),
            arma3_directory: String::new(),
            arma3_profiles_directory: String::new(),
            steam_directory: String::new(),
            teamspeak3_directory: String::new(),
            temp_directory: String::new(),
            backup_directory: String::new(),
            download_speed_limit_mbps: default_download_speed_limit_mbps(),
            hash_io_profile: HashIoProfilePreference::default(),
            ui_renderer: UiRendererPreference::default(),
            locale: default_locale(),
            locale_preference_migrated: true,
            additional_folders: Vec::new(),
            additional_folder_aliases: HashMap::new(),
            cleanup_folders: Vec::new(),
            font_sizes: FontSizes::default(),
            ui_scale_percent: default_ui_scale_percent(),
            ui_scale_percent_draft: default_ui_scale_percent(),
            update_view_font_hierarchy_migrated: true,
            palette_colors: PaletteColors::default(),
            saved_themes: Vec::new(),
            selected_saved_theme: None,
            saved_theme_name_draft: String::new(),
            show_add_theme_modal: false,
            new_theme_name_draft: String::new(),
            focus_new_theme_name: false,
            update_summary_notices: Vec::new(),
            active_update_sessions: Vec::new(),
            app_update_mode: AppUpdateMode::GitHub,
            app_update_url: String::new(),
            app_update_github_repo: default_app_update_github_repo(),
            app_update_mode_user_override: false,
            app_update_url_user_override: false,
            app_update_auto_check: default_app_update_auto_check(),
            app_update_check_interval_minutes: default_app_update_check_interval(),
            ts3_installed_plugin_hashes: HashMap::new(),
            swifty_migration_offered: false,
            scheduled_jobs: Vec::new(),
            additional_folders_filter: String::new(),
            cleanup_folders_filter: String::new(),
            language_filter: String::new(),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Default)]
pub struct WindowState {
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    #[serde(default)]
    pub size: Option<[f32; 2]>,
    #[serde(default)]
    pub maximized: bool,
}

pub struct MainViewState {
    pub use_window_decorations: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum ServerOnlineStatus {
    Offline,
    Online { players: u32 },
}

#[derive(Clone, Debug)]
pub struct ServerStatusCache {
    pub last_check: Instant,
    pub status: ServerOnlineStatus,
}
