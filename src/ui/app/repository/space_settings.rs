use std::collections::HashMap;
use std::fs;

use log::{debug, error, info, warn};

use crate::ui::app::Foxy;
use crate::ui::types::{RepositorySpace, sanitize_repository_spaces_paths, sanitize_user_path};

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
