use std::fs;
use std::io::ErrorKind;

use log::{debug, error, info};

use crate::core::steam;
use crate::ui::app::Foxy;
use crate::ui::i18n::{migrate_locale_preference, normalize_language};
use crate::ui::types::{
    MAX_UI_SCALE_PERCENT, MIN_UI_SCALE_PERCENT, Repository, SettingsViewState,
    normalize_settings_launch_behavior, path_is_inside_onedrive, sanitize_settings_paths,
};

impl Foxy {
    pub fn load_settings(&mut self) {
        let settings_path = Self::get_settings_path();
        let mut can_persist_detected_directories = false;
        match fs::read_to_string(&settings_path) {
            Ok(json_string) => match serde_json::from_str::<serde_json::Value>(&json_string) {
                Ok(settings_json) => {
                    let app_update_mode_was_saved = settings_json.get("app_update_mode").is_some();
                    let app_update_mode_override_was_saved =
                        settings_json.get("app_update_mode_user_override").is_some();
                    let mut settings =
                        match serde_json::from_value::<SettingsViewState>(settings_json) {
                            Ok(settings) => settings,
                            Err(err) => {
                                log::error!("Failed to parse settings.json: {}", err);
                                return;
                            }
                        };
                    if app_update_mode_was_saved && !app_update_mode_override_was_saved {
                        settings.app_update_mode_user_override = true;
                    }
                    self.settings_view_state = settings;
                    (
                        self.settings_view_state.locale,
                        self.settings_view_state.locale_preference_migrated,
                    ) = migrate_locale_preference(
                        &self.settings_view_state.locale,
                        self.settings_view_state.locale_preference_migrated,
                    );
                    if self.settings_view_state.download_speed_limit_mbps == Some(0) {
                        self.settings_view_state.download_speed_limit_mbps = Some(1);
                    }
                    if self.settings_view_state.backup_max_age_days == Some(0) {
                        self.settings_view_state.backup_max_age_days = None;
                    }
                    self.settings_view_state.app_update_url =
                        self.settings_view_state.app_update_url.trim().to_string();
                    if self.settings_view_state.app_update_url.is_empty() {
                        self.settings_view_state.app_update_url_user_override = false;
                    }
                    sanitize_settings_paths(&mut self.settings_view_state);
                    normalize_settings_launch_behavior(&mut self.settings_view_state);
                    for notice in &mut self.settings_view_state.update_summary_notices {
                        notice.repository_url = Self::normalize_repo_url(&notice.repository_url);
                        if notice.pending_ack_count == 0 {
                            notice.pending_ack_count = 1;
                        }
                    }
                    for session in &mut self.settings_view_state.active_update_sessions {
                        session.repository_url = Self::normalize_repo_url(&session.repository_url);
                    }
                    self.settings_view_state
                        .active_update_sessions
                        .retain(|session| !session.mods.is_empty());
                    // Discard stale notices that carry no meaningful update info
                    // (e.g. leftover from a no-op download after a DB wipe).
                    self.settings_view_state
                        .update_summary_notices
                        .retain(|notice| notice.summary.has_meaningful_content());
                    if !self.settings_view_state.update_view_font_hierarchy_migrated {
                        self.settings_view_state
                            .font_sizes
                            .update_view
                            .migrate_heading_hierarchy();
                        self.settings_view_state.update_view_font_hierarchy_migrated = true;
                        info!("Migrated update-view font heading hierarchy to new defaults");
                    }
                    self.settings_view_state.font_sizes.clamp_to_limits();
                    self.settings_view_state.ui_scale_percent = self
                        .settings_view_state
                        .ui_scale_percent
                        .clamp(MIN_UI_SCALE_PERCENT, MAX_UI_SCALE_PERCENT);
                    self.settings_view_state.ui_scale_percent_draft =
                        self.settings_view_state.ui_scale_percent;
                    self.i18n.set_language(&self.settings_view_state.locale);
                    debug!("Loaded settings.json");
                    if self.settings_view_state.arma3_directory.trim().is_empty() {
                        info!("Arma 3 directory is not configured in settings");
                    } else {
                        info!(
                            "Arma 3 directory: {}",
                            self.settings_view_state.arma3_directory
                        );
                    }
                    can_persist_detected_directories = true;
                }
                Err(err) => log::error!("Failed to parse settings.json: {}", err),
            },
            Err(err) if err.kind() == ErrorKind::NotFound => {
                info!("settings.json not found, using default settings");
                can_persist_detected_directories = true;
            }
            Err(err) => {
                error!("Failed to read settings.json: {}", err);
            }
        }

        let mut detected_settings_changed = false;
        if can_persist_detected_directories
            && self.settings_view_state.steam_directory.trim().is_empty()
        {
            if let Some(path) = steam::detect_steam_install_directory() {
                self.settings_view_state.steam_directory = path.display().to_string();
                info!(
                    "Auto-detected Steam directory: {}",
                    self.settings_view_state.steam_directory
                );
                detected_settings_changed = true;
            } else {
                debug!("Steam directory is empty and auto-detection did not find an install");
            }
        }
        if can_persist_detected_directories
            && self.settings_view_state.arma3_directory.trim().is_empty()
        {
            if let Some(path) =
                steam::detect_arma3_install_directory(&self.settings_view_state.steam_directory)
            {
                let path = path.display().to_string();
                if path_is_inside_onedrive(&path) {
                    debug!(
                        "Auto-detected Arma 3 directory is inside OneDrive and was not persisted"
                    );
                } else {
                    self.settings_view_state.arma3_directory = path;
                    info!(
                        "Auto-detected Arma 3 directory: {}",
                        self.settings_view_state.arma3_directory
                    );
                    detected_settings_changed = true;
                }
            } else {
                debug!("Arma 3 directory is empty and auto-detection did not find an install");
            }
        }
        if can_persist_detected_directories
            && self
                .settings_view_state
                .teamspeak3_directory
                .trim()
                .is_empty()
        {
            if let Some(path) = crate::core::ts3_plugin::detect_teamspeak_directory() {
                self.settings_view_state.teamspeak3_directory = path.display().to_string();
                info!(
                    "Auto-detected TeamSpeak 3 directory: {}",
                    self.settings_view_state.teamspeak3_directory
                );
                detected_settings_changed = true;
            } else {
                debug!("TeamSpeak 3 directory is empty and auto-detection did not find an install");
            }
        }
        if detected_settings_changed {
            self.save_settings();
        }

        self.sanitize_settings_debug_artifacts();
        self.sync_debug_runtime_state();
        if !self.settings_view_state.show_memory_diagnostics_icon {
            self.show_memory_diagnostics_window = false;
        }
    }

    pub fn save_settings(&mut self) {
        self.mark_settings_dirty();
    }

    pub fn reset_settings(&mut self) {
        self.settings_view_state = SettingsViewState::default();
        self.show_memory_diagnostics_window = false;
        self.i18n
            .set_language(&normalize_language(&self.settings_view_state.locale));
        let _ = fs::remove_file(Self::get_settings_path());
    }

    pub fn repo_auto_recheck_on_launch(&self, repo: &Repository) -> bool {
        match repo.auto_recheck_on_launch {
            Some(value) => value,
            None => self.settings_view_state.auto_recheck_on_launch,
        }
    }

    pub fn repo_auto_quick_scan_on_launch(&self, repo: &Repository) -> bool {
        match repo.auto_quick_scan_on_launch {
            Some(value) => value,
            None => self.settings_view_state.auto_quick_scan_on_launch,
        }
    }

    pub fn repo_auto_backup_on_update(&self, repo: &Repository) -> bool {
        match repo.auto_backup_on_update {
            Some(value) => value,
            None => self.settings_view_state.auto_backup_on_update,
        }
    }

    pub fn repo_apply_repo_json_client_parameters(&self, repo: &Repository) -> bool {
        match repo.apply_repo_json_client_parameters {
            Some(value) => value,
            None => self.settings_view_state.apply_repo_json_client_parameters,
        }
    }

    pub fn repo_apply_repo_json_dlc_content(&self, repo: &Repository) -> bool {
        match repo.apply_repo_json_dlc_content {
            Some(value) => value,
            None => self.settings_view_state.apply_repo_json_dlc_content,
        }
    }

    pub fn repo_warn_editor_external_addons(&self, repo: &Repository) -> bool {
        match repo.warn_editor_external_addons {
            Some(value) => value,
            None => self.settings_view_state.warn_editor_external_addons,
        }
    }

    pub fn repo_enable_editor_mission_list(&self, repo: &Repository) -> bool {
        match repo.enable_editor_mission_list {
            Some(value) => value,
            None => self.settings_view_state.enable_editor_mission_list,
        }
    }

    pub fn repo_enable_server_list(&self, repo: &Repository) -> bool {
        match repo.enable_server_list {
            Some(value) => value,
            None => self.settings_view_state.enable_server_list,
        }
    }

    pub fn repo_check_server_addons_before_join(&self, repo: &Repository) -> bool {
        match repo.check_server_addons_before_join {
            Some(value) => value,
            None => self.settings_view_state.check_server_addons_before_join,
        }
    }

    pub fn repo_check_ts3_running_before_join(&self, repo: &Repository) -> bool {
        match repo.check_ts3_running_before_join {
            Some(value) => value,
            None => self.settings_view_state.check_ts3_running_before_join,
        }
    }

    pub fn repo_check_steam_running_before_launch(&self, repo: &Repository) -> bool {
        match repo.check_steam_running_before_launch {
            Some(value) => value,
            None => self.settings_view_state.check_steam_running_before_launch,
        }
    }
}
