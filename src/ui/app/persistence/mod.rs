mod backup_inventory;
mod paused_repositories;
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
        let copy = crate::core::utils::fs_safety::preserve_unreadable_file(path);
        self.unreadable_config_replacements
            .insert(path.to_path_buf(), copy.clone());
        let Some(copy) = copy else {
            let message = self.t_fmt(
                "{file} could not be read, and Foxy could not preserve a backup. Saving over it is blocked.",
                &[("file", file)],
            );
            self.show_error_toast(message);
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

    pub(crate) fn confirm_replace_unreadable_config(&mut self) {
        let current = [
            Self::get_app_settings_path(),
            Self::get_game_settings_path(),
            Self::get_repositories_path(),
            Self::get_repository_spaces_path(),
            Self::get_repository_visual_folders_path(),
            Self::get_game_space_directory().join("paused_repositories.json"),
        ];
        let spaces = self
            .unreadable_config_replacements
            .get(&Self::get_repository_spaces_path())
            .is_some_and(Option::is_some);
        let folders = self
            .unreadable_config_replacements
            .get(&Self::get_repository_visual_folders_path())
            .is_some_and(Option::is_some);
        self.unreadable_config_replacements
            .retain(|path, backup| backup.is_none() || !current.contains(path));
        self.show_unreadable_config_replacement = false;
        if spaces {
            self.save_repository_spaces();
        }
        if folders {
            self.save_repository_visual_folders();
        }
        self.needs_repaint = true;
    }

    pub(crate) fn cancel_replace_unreadable_config(&mut self) {
        self.show_unreadable_config_replacement = false;
        self.close_requested_at = None;
        self.close_worker_wait_started_at = None;
        self.needs_repaint = true;
    }

    pub(crate) fn render_unreadable_config_replacement(&mut self, ctx: &eframe::egui::Context) {
        if !self.show_unreadable_config_replacement {
            return;
        }
        let space_dir = Self::get_game_space_directory();
        let app_path = Self::get_app_settings_path();
        let mut replacements = self
            .unreadable_config_replacements
            .iter()
            .filter(|(path, _)| path.parent() == Some(space_dir.as_path()) || **path == app_path)
            .map(|(path, backup)| (path.clone(), backup.clone()))
            .collect::<Vec<_>>();
        replacements.sort_by(|left, right| left.0.cmp(&right.0));
        eframe::egui::Window::new(self.t("Replace unreadable configuration?"))
            .collapsible(false)
            .resizable(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, eframe::egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let can_replace = replacements.iter().any(|(_, backup)| backup.is_some());
                let backup_failed = replacements.iter().any(|(_, backup)| backup.is_none());
                if backup_failed && can_replace {
                    ui.label(self.t("Foxy preserved copies of some unreadable files. Files without a backup remain blocked."));
                } else if backup_failed {
                    ui.label(self.t("Foxy could not preserve a backup. Saving over this file is blocked."));
                } else {
                    ui.label(self.t("Foxy preserved a copy of the unreadable file. Saving will replace its current contents."));
                }
                for (path, backup) in &replacements {
                    let file = path.file_name().unwrap_or_default().to_string_lossy();
                    if let Some(backup) = backup {
                        ui.label(format!("{file}: {}", backup.file_name().unwrap_or_default().to_string_lossy()));
                    } else {
                        ui.label(file);
                    }
                }
                ui.horizontal(|ui| {
                    if backup_failed && ui.button(self.t("Retry")).clicked() {
                        let paths: Vec<_> = self.unreadable_config_replacements.iter()
                            .filter(|(_, backup)| backup.is_none()).map(|(path, _)| path.clone()).collect();
                        for path in paths {
                            if let Some(backup) = crate::core::utils::fs_safety::preserve_unreadable_file(&path) {
                                self.unreadable_config_replacements.insert(path, Some(backup));
                            }
                        }
                    }
                    if can_replace && ui.button(self.t("Replace and continue")).clicked() {
                        self.confirm_replace_unreadable_config();
                    }
                    if ui.button(self.t("Cancel")).clicked() {
                        self.cancel_replace_unreadable_config();
                    }
                });
            });
    }

    pub(crate) fn persistence_path_is_confirmed_for_replacement(&self, path: &Path) -> bool {
        !self.unreadable_config_replacements.contains_key(path)
    }

    pub(crate) fn settings_persistence_is_confirmed(&self) -> bool {
        self.persistence_path_is_confirmed_for_replacement(&Self::get_app_settings_path())
            && self.persistence_path_is_confirmed_for_replacement(&Self::get_game_settings_path())
    }

    pub(crate) fn repositories_persistence_is_confirmed(&self) -> bool {
        self.persistence_path_is_confirmed_for_replacement(&Self::get_repositories_path())
    }

    pub(crate) fn show_replacement_confirmation_if_needed(&mut self) {
        if self.unreadable_config_replacements.iter().any(|(path, _)| {
            path == &Self::get_app_settings_path()
                || path == &Self::get_game_settings_path()
                || path == &Self::get_repositories_path()
        }) {
            self.show_unreadable_config_replacement = true;
        }
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
