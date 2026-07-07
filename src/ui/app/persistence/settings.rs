use std::fs;

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
        let mut can_persist_detected_directories = false;
        match crate::core::game::spaces::read_merged_settings_value(
            &Self::get_app_settings_path(),
            &Self::get_game_settings_path(),
        ) {
            Ok(Some(settings_json)) => {
                let app_update_mode_was_saved = settings_json.get("app_update_mode").is_some();
                let app_update_mode_override_was_saved =
                    settings_json.get("app_update_mode_user_override").is_some();
                let default_settings_json = match serde_json::to_value(SettingsViewState::default())
                {
                    Ok(value) => value,
                    Err(err) => {
                        log::error!("Failed to serialize default settings: {}", err);
                        return;
                    }
                };
                let settings_json = crate::core::game::spaces::merge_value_over_defaults(
                    default_settings_json,
                    settings_json,
                );
                let mut settings = match serde_json::from_value::<SettingsViewState>(settings_json)
                {
                    Ok(settings) => settings,
                    Err(err) => {
                        log::error!("Failed to parse settings: {}", err);
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
                debug!("Loaded app_settings.json and game_settings.json");
                let module = crate::core::game::registry().active();
                let install_dir = module.install_dir_from_settings(&self.settings_view_state);
                if install_dir.trim().is_empty() {
                    info!(
                        "{} directory is not configured in settings",
                        module.display_name()
                    );
                } else {
                    info!("{} directory: {}", module.display_name(), install_dir);
                }
                can_persist_detected_directories = true;
            }
            Ok(None) => {
                info!("Settings files not found, using default settings");
                can_persist_detected_directories = true;
            }
            Err(err) => {
                error!("Failed to load settings: {}", err);
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
        // Detection only ever fills the active module's own install field so a
        // non-Arma space never gets its game path written into the Arma 3
        // setting (or vice versa).
        let active_module = crate::core::game::registry().active();
        let is_arma3_space = active_module.id() == crate::core::game::arma3::ARMA3_GAME_ID;
        let active_install_setting_id = active_module
            .settings_schema()
            .install_dir_setting()
            .map(|setting| setting.id);
        if can_persist_detected_directories
            && active_module
                .install_dir_from_settings(&self.settings_view_state)
                .trim()
                .is_empty()
        {
            if let Some(path) =
                active_module.detect_install_dir(&crate::core::game::GameDetectCtx {
                    steam_directory: &self.settings_view_state.steam_directory,
                })
            {
                let path = path.display().to_string();
                if path_is_inside_onedrive(&path) {
                    debug!("Auto-detected game directory is inside OneDrive and was not persisted");
                } else {
                    match active_install_setting_id {
                        Some("arma3_directory") => {
                            self.settings_view_state.arma3_directory = path;
                            info!(
                                "Auto-detected Arma 3 directory: {}",
                                self.settings_view_state.arma3_directory
                            );
                            detected_settings_changed = true;
                        }
                        Some("twwh3_directory") => {
                            self.settings_view_state.twwh3_directory = path;
                            info!(
                                "Auto-detected Total War: WARHAMMER III directory: {}",
                                self.settings_view_state.twwh3_directory
                            );
                            detected_settings_changed = true;
                        }
                        Some("reforger_directory") => {
                            self.settings_view_state.reforger_directory = path;
                            info!(
                                "Auto-detected Arma Reforger directory: {}",
                                self.settings_view_state.reforger_directory
                            );
                            detected_settings_changed = true;
                        }
                        _ => {}
                    }
                }
            } else {
                debug!(
                    "Active game install directory is empty and auto-detection did not find an install"
                );
            }
        }
        if can_persist_detected_directories
            && is_arma3_space
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
        let _ = fs::remove_file(Self::get_app_settings_path());
        let _ = fs::remove_file(Self::get_game_settings_path());
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
