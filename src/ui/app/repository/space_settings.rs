use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;

use log::{debug, error, info, warn};
use rand::{RngExt, distr::Alphanumeric, rng};

use crate::ui::app::Foxy;
use crate::ui::app::RepositoryVisualFolderEditState;
use crate::ui::types::{
    RepositorySpace, RepositoryVisualFolder, default_repository_visual_folder_color,
    sanitize_repository_spaces_paths, sanitize_user_path,
};

impl Foxy {
    pub fn load_repository_spaces(&mut self) {
        let path = Self::get_repository_spaces_path();
        match fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<Vec<RepositorySpace>>(&json) {
                Ok(mut spaces) => {
                    sanitize_repository_spaces_paths(&mut spaces);
                    self.repository_spaces = spaces;
                    self.bump_repository_spaces_version();
                    debug!(
                        "Loaded repository_spaces.json with {} repository spaces",
                        self.repository_spaces.len()
                    );
                }
                Err(err) => {
                    error!("Failed to parse repository_spaces.json: {}", err);
                }
            },
            Err(_) => {
                info!("repository_spaces.json not found, using empty repository space list");
                self.repository_spaces.clear();
                self.bump_repository_spaces_version();
            }
        }
    }

    pub fn save_repository_spaces(&mut self) {
        self.bump_repository_spaces_version();
        if self.settings_view_state.debug_mode {
            warn!("Skipping repository_spaces.json save while debug mode is active");
            return;
        }

        let path = Self::get_repository_spaces_path();
        let mut spaces = self.repository_spaces.clone();
        sanitize_repository_spaces_paths(&mut spaces);
        match serde_json::to_string_pretty(&spaces) {
            Ok(json) => {
                if let Err(err) =
                    crate::core::utils::fs_safety::atomic_write(&path, json.as_bytes())
                {
                    error!("Failed to write repository_spaces.json: {}", err);
                } else {
                    debug!(
                        "Saved repository_spaces.json with {} repository spaces",
                        spaces.len()
                    );
                }
            }
            Err(err) => error!("Failed to serialize repository spaces: {}", err),
        }
    }

    pub fn load_repository_visual_folders(&mut self) {
        let path = Self::get_repository_visual_folders_path();
        match fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<Vec<RepositoryVisualFolder>>(&json) {
                Ok(mut folders) => {
                    sanitize_repository_visual_folders(&mut folders);
                    self.repository_visual_folders = folders;
                    self.prune_repository_visual_folders(false);
                    self.bump_repository_visual_folders_version();
                    debug!(
                        "Loaded repository_visual_folders.json with {} folders",
                        self.repository_visual_folders.len()
                    );
                }
                Err(err) => {
                    error!("Failed to parse repository_visual_folders.json: {}", err);
                }
            },
            Err(_) => {
                info!("repository_visual_folders.json not found, using empty visual folder list");
                self.repository_visual_folders.clear();
                self.bump_repository_visual_folders_version();
            }
        }
    }

    pub fn save_repository_visual_folders(&mut self) {
        self.prune_repository_visual_folders(false);
        self.bump_repository_visual_folders_version();
        if self.settings_view_state.debug_mode {
            warn!("Skipping repository_visual_folders.json save while debug mode is active");
            return;
        }

        let path = Self::get_repository_visual_folders_path();
        let mut folders = self.repository_visual_folders.clone();
        sanitize_repository_visual_folders(&mut folders);
        match serde_json::to_string_pretty(&folders) {
            Ok(json) => {
                if let Err(err) =
                    crate::core::utils::fs_safety::atomic_write(&path, json.as_bytes())
                {
                    error!("Failed to write repository_visual_folders.json: {}", err);
                } else {
                    debug!(
                        "Saved repository_visual_folders.json with {} folders",
                        folders.len()
                    );
                }
            }
            Err(err) => error!("Failed to serialize repository visual folders: {}", err),
        }
    }

    pub fn repository_visual_folder_key_for_repo(&self, repo_idx: usize) -> Option<String> {
        let repo = self.repository_view_state.repositories.get(repo_idx)?;
        Some(Self::repo_instance_key(&repo.address, &repo.path))
    }

    pub fn repository_visual_folder_for_repo(
        &self,
        repo_idx: usize,
    ) -> Option<&RepositoryVisualFolder> {
        let repo_key = self.repository_visual_folder_key_for_repo(repo_idx)?;
        self.repository_visual_folders
            .iter()
            .find(|folder| folder.repository_keys.iter().any(|key| key == &repo_key))
    }

    pub fn open_create_repository_visual_folder(&mut self, repository_space_id: Option<String>) {
        self.pending_repository_visual_folder_edit = Some(RepositoryVisualFolderEditState {
            folder_id: None,
            repository_space_id,
            name_buffer: self.t("New folder"),
            color_rgb: default_repository_visual_folder_color(),
            error: None,
        });
    }

    pub fn open_edit_repository_visual_folder(&mut self, folder_id: &str) {
        let Some(folder) = self
            .repository_visual_folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .cloned()
        else {
            return;
        };
        self.pending_repository_visual_folder_edit = Some(RepositoryVisualFolderEditState {
            folder_id: Some(folder.id),
            repository_space_id: folder.repository_space_id,
            name_buffer: folder.name,
            color_rgb: folder.color_rgb,
            error: None,
        });
    }

    pub fn apply_repository_visual_folder_edit(&mut self) -> bool {
        let Some(mut edit) = self.pending_repository_visual_folder_edit.clone() else {
            return false;
        };
        let name = edit.name_buffer.trim().to_string();
        if name.is_empty() {
            edit.error = Some(self.t("Folder name is required."));
            self.pending_repository_visual_folder_edit = Some(edit);
            return false;
        }

        if let Some(folder_id) = edit.folder_id.as_deref() {
            if let Some(folder) = self
                .repository_visual_folders
                .iter_mut()
                .find(|folder| folder.id == folder_id)
            {
                folder.name = name;
                folder.color_rgb = edit.color_rgb;
            }
        } else {
            let folder = RepositoryVisualFolder {
                id: Self::new_repository_visual_folder_id(),
                name,
                repository_space_id: edit.repository_space_id,
                color_rgb: edit.color_rgb,
                collapsed: false,
                repository_keys: Vec::new(),
            };
            self.repository_visual_folders.push(folder);
        }

        self.pending_repository_visual_folder_edit = None;
        self.save_repository_visual_folders();
        true
    }

    pub fn set_repository_visual_folder_collapsed(
        &mut self,
        folder_id: &str,
        collapsed: bool,
    ) -> bool {
        let Some(folder) = self
            .repository_visual_folders
            .iter_mut()
            .find(|folder| folder.id == folder_id)
        else {
            return false;
        };
        if folder.collapsed == collapsed {
            return false;
        }
        folder.collapsed = collapsed;
        self.save_repository_visual_folders();
        true
    }

    pub fn assign_repository_to_visual_folder(&mut self, repo_idx: usize, folder_id: &str) -> bool {
        let Some(repo_key) = self.repository_visual_folder_key_for_repo(repo_idx) else {
            return false;
        };
        let Some(target_scope) = self
            .repository_visual_folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .map(|folder| folder.repository_space_id.clone())
        else {
            return false;
        };
        let repo_scope = self
            .repository_view_state
            .repositories
            .get(repo_idx)
            .and_then(|repo| repo.repository_space_id.clone());
        if repo_scope != target_scope {
            return false;
        }

        for folder in &mut self.repository_visual_folders {
            folder.repository_keys.retain(|key| key != &repo_key);
        }
        if let Some(folder) = self
            .repository_visual_folders
            .iter_mut()
            .find(|folder| folder.id == folder_id)
            && !folder.repository_keys.iter().any(|key| key == &repo_key)
        {
            folder.repository_keys.push(repo_key);
            self.save_repository_visual_folders();
            return true;
        }
        self.save_repository_visual_folders();
        true
    }

    pub fn remove_repository_from_visual_folder(&mut self, repo_idx: usize) -> bool {
        let Some(repo_key) = self.repository_visual_folder_key_for_repo(repo_idx) else {
            return false;
        };
        let mut changed = false;
        for folder in &mut self.repository_visual_folders {
            let before = folder.repository_keys.len();
            folder.repository_keys.retain(|key| key != &repo_key);
            changed |= folder.repository_keys.len() != before;
        }
        if changed {
            self.save_repository_visual_folders();
        }
        changed
    }

    pub fn delete_repository_visual_folder(&mut self, folder_id: &str, delete_repositories: bool) {
        let Some(folder_idx) = self
            .repository_visual_folders
            .iter()
            .position(|folder| folder.id == folder_id)
        else {
            return;
        };
        let removed = self.repository_visual_folders.remove(folder_idx);
        self.pending_repository_visual_folder_delete = None;
        if self.selected_repository_visual_folder_id.as_deref() == Some(folder_id) {
            self.selected_repository_visual_folder_id = None;
        }
        if delete_repositories {
            let keys: HashSet<String> = removed.repository_keys.into_iter().collect();
            let mut indices = self
                .repository_view_state
                .repositories
                .iter()
                .enumerate()
                .filter_map(|(idx, repo)| {
                    keys.contains(&Self::repo_instance_key(&repo.address, &repo.path))
                        .then_some(idx)
                })
                .collect::<Vec<_>>();
            indices.sort_unstable_by(|left, right| right.cmp(left));
            for idx in indices {
                self.delete_repository_by_index(idx, false);
            }
        }
        self.save_repository_visual_folders();
        let removed_message =
            self.t_fmt("Folder removed: {name}", &[("name", removed.name.clone())]);
        self.show_success_toast(removed_message);
    }

    pub(in crate::ui::app) fn prune_repository_visual_folders(&mut self, save_if_changed: bool) {
        let valid_space_ids: HashSet<String> = self
            .repository_spaces
            .iter()
            .map(|space| space.id.clone())
            .collect();
        let valid_repo_keys: HashSet<String> = self
            .repository_view_state
            .repositories
            .iter()
            .map(|repo| Self::repo_instance_key(&repo.address, &repo.path))
            .collect();
        let before_folders = self.repository_visual_folders.len();
        let mut changed = false;
        self.repository_visual_folders.retain(|folder| {
            folder
                .repository_space_id
                .as_ref()
                .is_none_or(|space_id| valid_space_ids.contains(space_id))
        });
        changed |= self.repository_visual_folders.len() != before_folders;
        for folder in &mut self.repository_visual_folders {
            let before_keys = folder.repository_keys.len();
            folder
                .repository_keys
                .retain(|key| valid_repo_keys.contains(key));
            changed |= folder.repository_keys.len() != before_keys;
        }
        if changed && save_if_changed {
            self.save_repository_visual_folders();
        }
    }

    fn new_repository_visual_folder_id() -> String {
        let suffix: String = rng()
            .sample_iter(&Alphanumeric)
            .take(12)
            .map(char::from)
            .collect();
        format!("folder-{suffix}")
    }

    pub fn reconcile_repository_space_paths(&mut self) {
        let space_paths: HashMap<String, String> = self
            .repository_spaces
            .iter()
            .map(|space| (space.id.clone(), space.shared_path.clone()))
            .collect();

        let mut changed = false;
        for repo in &mut self.repository_view_state.repositories {
            let Some(space_id) = repo.repository_space_id.clone() else {
                continue;
            };

            if let Some(shared_path) = space_paths.get(&space_id) {
                if repo.path.trim().is_empty() && !shared_path.trim().is_empty() {
                    repo.path = shared_path.clone();
                    changed = true;
                }
            } else {
                repo.repository_space_id = None;
                repo.repository_space_entry_address = None;
                changed = true;
            }
        }

        if changed {
            self.save_repositories();
            info!("Reconciled repository space bindings with repository paths");
        }
    }

    pub fn set_repository_space_shared_path(&mut self, space_id: &str, path: String) {
        let path = sanitize_user_path(&path);
        let mut space_changed = false;
        let old_path = self
            .repository_spaces
            .iter()
            .find(|s| s.id == space_id)
            .map(|space| space.shared_path.clone())
            .unwrap_or_default();
        if let Some(space) = self.repository_spaces.iter_mut().find(|s| s.id == space_id)
            && space.shared_path != path
        {
            space.shared_path = path.clone();
            space_changed = true;
        }

        let mut repo_changed = false;
        for repo in &mut self.repository_view_state.repositories {
            if repo.repository_space_id.as_deref() == Some(space_id)
                && (repo.path.trim().is_empty() || repo.path == old_path)
                && repo.path != path
            {
                repo.path = path.clone();
                repo_changed = true;
            }
        }

        if space_changed {
            self.save_repository_spaces();
        }
        if repo_changed {
            self.save_repositories();
        }
    }

    pub fn open_repository_space_settings(&mut self, space_id: &str) {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
        else {
            return;
        };

        self.repository_space_settings_state = Some(crate::ui::app::RepositorySpaceSettingsState {
            space_id: space.id.clone(),
            source_address_buffer: space.source_address.clone(),
            local_name_buffer: space
                .local_name_override
                .clone()
                .unwrap_or_else(|| space.name.clone()),
            shared_path_buffer: space.shared_path.clone(),
            error: None,
        });
    }

    pub fn set_repository_space_source_address(
        &mut self,
        space_id: &str,
        address_input: &str,
    ) -> Result<(), String> {
        let candidate = Self::repository_space_manifest_candidates(address_input)
            .into_iter()
            .next()
            .ok_or_else(|| "Address is required".to_string())?;

        let Some(space) = self
            .repository_spaces
            .iter_mut()
            .find(|space| space.id == space_id)
        else {
            return Err("Repository space was not found".to_string());
        };

        space.source_address = candidate.clone();
        space.source_base_url = Self::repository_space_base_url(&candidate);
        self.save_repository_spaces();
        Ok(())
    }

    pub fn repository_space_entry_install_count(
        &self,
        space_id: &str,
        entry_address: &str,
    ) -> usize {
        let normalized_entry = Self::normalize_repo_url(entry_address);
        self.repository_view_state
            .repositories
            .iter()
            .filter(|repo| {
                repo.repository_space_id.as_deref() == Some(space_id)
                    && Self::normalize_repo_url(&repo.address) == normalized_entry
            })
            .count()
    }

    pub fn repository_space_required_entries_satisfied(&self, space_id: &str) -> bool {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
        else {
            return true;
        };

        space.entries.iter().all(|entry| {
            !entry.required
                || self.repository_space_entry_install_count(space_id, &entry.address) > 0
        })
    }

    pub fn repository_space_name_by_id(&self, space_id: &str) -> Option<&str> {
        self.repository_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(Self::repository_space_display_name)
    }

    pub fn repository_space_display_name(space: &RepositorySpace) -> &str {
        space
            .local_name_override
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(space.name.as_str())
    }

    pub fn set_repository_space_local_name(&mut self, space_id: &str, name: String) {
        if let Some(space) = self
            .repository_spaces
            .iter_mut()
            .find(|space| space.id == space_id)
        {
            let trimmed = name.trim();
            space.local_name_override = if trimmed.is_empty() || trimmed == space.name.trim() {
                None
            } else {
                Some(trimmed.to_string())
            };
            self.save_repository_spaces();
        }
    }

    pub fn set_repository_space_collapsed(&mut self, space_id: &str, collapsed: bool) -> bool {
        let Some(space) = self
            .repository_spaces
            .iter_mut()
            .find(|space| space.id == space_id)
        else {
            return false;
        };

        if space.collapsed == collapsed {
            return false;
        }

        space.collapsed = collapsed;
        self.save_repository_spaces();
        true
    }
}

fn sanitize_repository_visual_folders(folders: &mut Vec<RepositoryVisualFolder>) {
    let mut seen_ids = HashSet::new();
    folders.retain_mut(|folder| {
        folder.id = folder.id.trim().to_string();
        folder.name = folder.name.trim().to_string();
        if folder.id.is_empty() || folder.name.is_empty() || !seen_ids.insert(folder.id.clone()) {
            return false;
        }
        if let Some(space_id) = folder.repository_space_id.as_mut() {
            *space_id = space_id.trim().to_string();
            if space_id.is_empty() {
                folder.repository_space_id = None;
            }
        }
        let mut seen_keys = HashSet::new();
        folder.repository_keys.retain(|key| {
            let trimmed = key.trim();
            !trimmed.is_empty() && seen_keys.insert(key.clone())
        });
        true
    });
}
