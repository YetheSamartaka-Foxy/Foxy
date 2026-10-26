mod backup_inventory;
mod queue;
mod repositories;
mod settings;

use std::path::{Path, PathBuf};

use crate::core::utils::app_paths;
use crate::ui::app::Foxy;

impl Foxy {
    /// A configuration file exists but cannot be used. Keep a copy and tell the
    /// user, because the next save would replace it with whatever did load.
    pub(crate) fn report_unreadable_config(&mut self, path: &Path) {
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(copy) = crate::core::utils::fs_safety::preserve_unreadable_file(path) else {
            return;
        };
        let copy = copy
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        log::error!("Kept a copy of unreadable {} as {}", file, copy);
        let message = self.t_fmt(
            "{file} could not be read. Foxy kept a copy named {copy} so it can be restored.",
            &[("file", file), ("copy", copy)],
        );
        self.show_error_toast(message);
    }

    /// Settings are merged from two files; keep whichever of them exists.
    pub(crate) fn report_unreadable_settings(&mut self) {
        for path in [
            Self::get_app_settings_path(),
            Self::get_game_settings_path(),
        ] {
            if path.exists() {
                self.report_unreadable_config(&path);
            }
        }
    }

    /// App-global data root (logs, window state, app settings).
    pub fn get_config_directory() -> PathBuf {
        app_paths::foxy_data_dir()
    }

    /// Active game space directory (repositories, spaces, game settings, DB).
    pub fn get_game_space_directory() -> PathBuf {
        crate::core::game::spaces::active_game_space_dir()
    }

    pub fn get_app_settings_path() -> PathBuf {
        Self::get_config_directory().join(crate::core::game::spaces::APP_SETTINGS_FILE)
    }

    pub fn get_game_settings_path() -> PathBuf {
        Self::get_game_space_directory().join(crate::core::game::spaces::GAME_SETTINGS_FILE)
    }

    pub fn get_repositories_path() -> PathBuf {
        let mut config_dir = Self::get_game_space_directory();
        config_dir.push("repositories.json");
        config_dir
    }

    pub fn get_repository_spaces_path() -> PathBuf {
        let mut config_dir = Self::get_game_space_directory();
        config_dir.push("repository_spaces.json");
        config_dir
    }

    pub fn get_repository_visual_folders_path() -> PathBuf {
        let mut config_dir = Self::get_game_space_directory();
        config_dir.push("repository_visual_folders.json");
        config_dir
    }

    pub fn get_window_state_path() -> PathBuf {
        let mut config_dir = Self::get_config_directory();
        config_dir.push("window_state.json");
        config_dir
    }
}
