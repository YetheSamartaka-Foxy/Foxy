use crate::ui::app::Foxy;
use crate::ui::types::RepositoryViewState;

impl Foxy {
    pub fn reset_repositories(&mut self) {
        self.repository_view_state = RepositoryViewState::default();
        self.repository_spaces.clear();
        self.repository_visual_folders.clear();
        self.selected_repository_space_id = None;
        self.selected_repository_visual_folder_id = None;
        self.repository_space_detail_filter.clear();
        self.repository_space_detail_filter_space_id = None;
        self.repository_space_selector_state = None;
        self.repository_space_settings_state = None;
        self.pending_repository_space_bulk_action = None;
        self.repository_space_bulk_progress = None;
        self.pending_repository_duplicate_add = None;
        self.repository_space_sync_queue.clear();
        self.repository_visual_folder_sync_queue.clear();
        self.pending_update_cache.clear();
        self.mod_diff_cache.clear();
        self.update_ready_repo = None;
        self.pending_repository_space_delete_id = None;
        self.pending_repository_visual_folder_edit = None;
        self.pending_repository_visual_folder_delete = None;
        self.bump_repository_list_data_version();
        self.bump_repository_spaces_version();
        self.bump_repository_visual_folders_version();
        let _ = std::fs::remove_file(Self::get_repositories_path());
        let _ = std::fs::remove_file(Self::get_repository_spaces_path());
        let _ = std::fs::remove_file(Self::get_repository_visual_folders_path());
    }
}
