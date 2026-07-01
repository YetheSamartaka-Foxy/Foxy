mod backup_inventory;
mod queue;
mod repositories;
mod settings;

use std::path::PathBuf;

use crate::core::utils::app_paths;
use crate::ui::app::Foxy;

impl Foxy {
    pub fn get_config_directory() -> PathBuf {
        app_paths::foxy_data_dir()
    }

    pub fn get_settings_path() -> PathBuf {
        let mut config_dir = Self::get_config_directory();
        config_dir.push("settings.json");
        config_dir
    }

    pub fn get_repositories_path() -> PathBuf {
        let mut config_dir = Self::get_config_directory();
        config_dir.push("repositories.json");
        config_dir
    }

    pub fn get_repository_spaces_path() -> PathBuf {
        let mut config_dir = Self::get_config_directory();
        config_dir.push("repository_spaces.json");
        config_dir
    }

    pub fn get_window_state_path() -> PathBuf {
        let mut config_dir = Self::get_config_directory();
        config_dir.push("window_state.json");
        config_dir
    }
}
