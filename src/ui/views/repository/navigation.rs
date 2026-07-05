use crate::ui::app::{Foxy, RepositoryListRow, RepositoryListSection};
use crate::ui::types::{FoxyView, RepositorySelection, RepositorySettingsTab};
use eframe::egui::{self, Id, Key, Modifiers};
use log::info;

impl Foxy {
    pub(super) fn is_repository_list_section_collapsed(
        &self,
        section: RepositoryListSection,
    ) -> bool {
        match section {
            RepositoryListSection::Spaces => self.repository_view_state.repository_spaces_collapsed,
            RepositoryListSection::Repositories => {
                self.repository_view_state.repositories_collapsed
            }
        }
    }

    fn set_repository_list_section_collapsed(
        &mut self,
        section: RepositoryListSection,
        collapsed: bool,
    ) {
        match section {
            RepositoryListSection::Spaces => {
                self.repository_view_state.repository_spaces_collapsed = collapsed;
            }
            RepositoryListSection::Repositories => {
                self.repository_view_state.repositories_collapsed = collapsed;
            }
        }
    }

    pub(super) fn toggle_repository_list_section_collapsed(
        &mut self,
        section: RepositoryListSection,
    ) {
        let collapsed = !self.is_repository_list_section_collapsed(section);
        self.set_repository_list_section_collapsed(section, collapsed);
    }

    fn keyboard_navigable_repository_rows(&self) -> Vec<usize> {
        self.repository_list_cache
            .rows
            .iter()
            .enumerate()
            .filter_map(|(row_idx, row)| match row {
                RepositoryListRow::SpaceHeader(_)
                | RepositoryListRow::FolderHeader(_)
                | RepositoryListRow::Repository { .. } => Some(row_idx),
                RepositoryListRow::SectionLabel(_) => None,
            })
            .collect()
    }

    fn selected_repository_row_index(&self) -> Option<usize> {
        self.repository_list_cache
            .rows
            .iter()
            .enumerate()
            .find_map(|(row_idx, row)| match row {
                RepositoryListRow::Repository { repo_idx, .. } => {
                    (self.repository_view_state.selected_repository == Some(*repo_idx))
                        .then_some(row_idx)
                }
                RepositoryListRow::FolderHeader(folder_idx) => {
                    let folder = self.repository_visual_folders.get(*folder_idx)?;
                    (self.selected_repository_visual_folder_id.as_deref()
                        == Some(folder.id.as_str()))
                    .then_some(row_idx)
                }
                RepositoryListRow::SpaceHeader(space_idx) => {
                    let space = self.repository_spaces.get(*space_idx)?;
                    (self.selected_repository_space_id.as_deref() == Some(space.id.as_str()))
                        .then_some(row_idx)
                }
                RepositoryListRow::SectionLabel(_) => None,
            })
    }

    pub(super) fn select_repository_list_row_from_cache(&mut self, row_idx: usize) -> bool {
        let Some(row) = self.repository_list_cache.rows.get(row_idx).copied() else {
            return false;
        };

        match row {
            RepositoryListRow::Repository { repo_idx, .. } => {
                if self
                    .repository_view_state
                    .repositories
                    .get(repo_idx)
                    .is_none()
                {
                    return false;
                }
                self.repository_view_state.selected_repository = Some(repo_idx);
                self.selected_repository_space_id = None;
                self.selected_repository_visual_folder_id = None;
                self.clear_completed_repository_check_banner_for_repo_change(Some(repo_idx));
                self.pending_mission_duplicate = None;
                self.pending_mission_delete = None;
                self.pending_mission_remove_dependencies = None;
                self.editor_mission_search.clear();
                self.editor_mission_folder.clear();
                self.editor_mission_terrain_filter.clear();
                true
            }
            RepositoryListRow::SpaceHeader(space_idx) => {
                let Some(space) = self.repository_spaces.get(space_idx) else {
                    return false;
                };
                self.selected_repository_space_id = Some(space.id.clone());
                self.selected_repository_visual_folder_id = None;
                self.repository_view_state.selected_repository = None;
                self.repository_selection = None;
                self.clear_completed_repository_check_banner_for_repo_change(None);
                self.pending_mission_duplicate = None;
                self.pending_mission_delete = None;
                self.pending_mission_remove_dependencies = None;
                self.editor_mission_search.clear();
                self.editor_mission_folder.clear();
                self.editor_mission_terrain_filter.clear();
                true
            }
            RepositoryListRow::FolderHeader(folder_idx) => {
                let Some(folder) = self.repository_visual_folders.get(folder_idx) else {
                    return false;
                };
                self.selected_repository_visual_folder_id = Some(folder.id.clone());
                self.selected_repository_space_id = None;
                self.repository_view_state.selected_repository = None;
                self.repository_selection = None;
                self.clear_completed_repository_check_banner_for_repo_change(None);
                self.pending_mission_duplicate = None;
                self.pending_mission_delete = None;
                self.pending_mission_remove_dependencies = None;
                self.editor_mission_search.clear();
                self.editor_mission_folder.clear();
                self.editor_mission_terrain_filter.clear();
                true
            }
            RepositoryListRow::SectionLabel(_) => false,
        }
    }

    fn move_repository_list_keyboard_selection(&mut self, step: isize) -> bool {
        let navigable = self.keyboard_navigable_repository_rows();
        if navigable.is_empty() {
            return false;
        }

        let current_row = self.selected_repository_row_index();
        let current_pos = current_row.and_then(|row| navigable.iter().position(|idx| *idx == row));
        let target_pos = match (current_pos, step.cmp(&0)) {
            (Some(pos), std::cmp::Ordering::Less) => pos.saturating_sub(1),
            (Some(pos), std::cmp::Ordering::Greater) => {
                (pos + 1).min(navigable.len().saturating_sub(1))
            }
            (Some(pos), std::cmp::Ordering::Equal) => pos,
            (None, std::cmp::Ordering::Less) => navigable.len().saturating_sub(1),
            (None, std::cmp::Ordering::Greater) | (None, std::cmp::Ordering::Equal) => 0,
        };

        self.select_repository_list_row_from_cache(navigable[target_pos])
    }

    fn move_selected_server_keyboard(&mut self, step: isize) -> bool {
        let Some(repo_idx) = self.repository_view_state.selected_repository else {
            return false;
        };
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return false;
        };
        if repo.servers.is_empty() {
            return false;
        }

        let current_server = match &self.repository_selection {
            Some(RepositorySelection::Server(idx)) => Some(*idx),
            _ => None,
        };

        let last_idx = repo.servers.len().saturating_sub(1);
        let next_idx = match (current_server, step.cmp(&0)) {
            (Some(idx), std::cmp::Ordering::Less) => idx.saturating_sub(1),
            (Some(idx), std::cmp::Ordering::Greater) => (idx + 1).min(last_idx),
            (Some(idx), std::cmp::Ordering::Equal) => idx,
            (None, std::cmp::Ordering::Less) => last_idx,
            (None, std::cmp::Ordering::Greater) | (None, std::cmp::Ordering::Equal) => 0,
        };

        self.repository_selection = Some(RepositorySelection::Server(next_idx));
        true
    }

    fn open_selected_repository_settings(&mut self) -> bool {
        let Some(repo_idx) = self.repository_view_state.selected_repository else {
            return false;
        };
        self.selected_repository_for_settings = Some(repo_idx);
        self.current_repository_settings_tab = RepositorySettingsTab::Configuration;
        self.last_view = self.current_view;
        self.current_view = FoxyView::RepositorySettings;
        self.preload_repository_settings_addon_caches(repo_idx);
        true
    }

    pub(super) fn handle_repository_view_keyboard_navigation(&mut self, ctx: &egui::Context) {
        if self.show_add_repository_modal
            || self.pending_repository_context_confirmation.is_some()
            || self.pending_repository_space_delete_id.is_some()
            || self.pending_repository_visual_folder_edit.is_some()
            || self.pending_repository_visual_folder_delete.is_some()
            || self.pending_repository_space_bulk_action.is_some()
            || self.pending_repository_duplicate_add.is_some()
            || self.pending_mission_duplicate.is_some()
            || self.pending_mission_delete.is_some()
            || self.pending_mission_remove_dependencies.is_some()
            || self.repository_space_selector_state.is_some()
        {
            return;
        }

        let filter_focused =
            ctx.memory(|memory| memory.has_focus(Id::new("repository_filter_input")));
        if filter_focused {
            return;
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::SHIFT, Key::Tab)) {
            self.move_repository_list_keyboard_selection(-1);
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Tab)) {
            self.move_repository_list_keyboard_selection(1);
            return;
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowUp)) {
            self.move_repository_list_keyboard_selection(-1);
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowDown)) {
            self.move_repository_list_keyboard_selection(1);
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowLeft)) {
            self.move_selected_server_keyboard(-1);
            return;
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowRight)) {
            self.move_selected_server_keyboard(1);
            return;
        }

        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter)) {
            if let Some(space_id) = self.selected_repository_space_id.clone() {
                self.open_repository_space_selector(space_id);
                return;
            }
            if self.open_selected_repository_settings() {
                info!("Opened repository settings from Enter key");
            }
        }
    }
}
