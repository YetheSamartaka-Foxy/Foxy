use crate::ui::app::Foxy;
use crate::ui::types::{FoxyView, Repository, RepositorySettingsTab};

impl Foxy {
    pub(crate) fn add_blank_repository(&mut self, name: &str, path: &str) {
        let path = if path.trim().is_empty() {
            // Address-less collections still need distinct URL/path instance keys.
            crate::core::game::spaces::active_game_space_dir()
                .join("local-repositories")
                .join(format!("{:032x}", rand::random::<u128>()))
                .to_string_lossy()
                .into_owned()
        } else {
            path.trim().to_string()
        };
        let repository = Repository {
            name: if name.trim().is_empty() {
                self.t("Local repository")
            } else {
                name.trim().to_string()
            },
            path,
            ..Repository::default()
        };
        self.repository_view_state.repositories.push(repository);
        let index = self.repository_view_state.repositories.len() - 1;
        self.repository_view_state.selected_repository = Some(index);
        self.selected_repository_space_id = None;
        self.selected_repository_visual_folder_id = None;
        self.clear_completed_repository_check_banner_for_repo_change(Some(index));
        self.save_repositories();
        self.selected_repository_for_settings = Some(index);
        self.current_repository_settings_tab = RepositorySettingsTab::ExternalAddons;
        self.last_view = self.current_view;
        self.current_view = FoxyView::RepositorySettings;
        self.invalidate_addon_inventory_cache();
        self.preload_repository_settings_addon_caches(index);
    }
}
