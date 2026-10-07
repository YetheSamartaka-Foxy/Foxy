mod backup_inventory;
mod queue;
mod repositories;
mod settings;

use std::path::PathBuf;

use crate::core::utils::app_paths;
use crate::ui::app::Foxy;

impl Foxy {
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
