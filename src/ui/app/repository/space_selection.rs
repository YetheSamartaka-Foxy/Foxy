use std::collections::{HashMap, HashSet};

use log::info;

use crate::ui::app::{Foxy, RepositorySpaceScanCandidate, RepositorySpaceSelectorState};

impl Foxy {
    pub fn open_repository_space_selector(&mut self, space_id: String) {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
        else {
            return;
        };

        let candidates = self.scan_repository_space_candidates(&space.id);
        self.repository_space_selector_state = Some(RepositorySpaceSelectorState {
            space_id: space.id.clone(),
            path_buffer: space.shared_path.clone(),
            candidates,
            last_scan_result_count: None,
            error: None,
        });
    }

    pub fn scan_repository_space_candidates(
        &self,
        space_id: &str,
    ) -> Vec<RepositorySpaceScanCandidate> {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
        else {
            return Vec::new();
        };

        let entry_urls: HashSet<String> = space
            .entries
            .iter()
            .map(|entry| Self::normalize_repo_url(&entry.address))
            .collect();

        let mut candidates = Vec::new();
        for (repo_index, repo) in self.repository_view_state.repositories.iter().enumerate() {
            let normalized_repo = Self::normalize_repo_url(&repo.address);
            if !entry_urls.contains(&normalized_repo) {
                continue;
            }
            if repo.repository_space_id.as_deref() == Some(space_id) {
                continue;
            }
            candidates.push(RepositorySpaceScanCandidate {
                repo_index,
                checked: false,
            });
        }
        info!(
            "Scanned repository space candidates for {}: found {}",
            Self::repository_space_display_name(space),
            candidates.len()
        );
        candidates
    }

    pub fn apply_repository_space_scan_candidates(
        &mut self,
        space_id: &str,
        candidates: &[RepositorySpaceScanCandidate],
    ) -> usize {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
        else {
            return 0;
        };

        let mut entries_by_address: HashMap<String, String> = HashMap::new();
        for entry in &space.entries {
            entries_by_address.insert(
                Self::normalize_repo_url(&entry.address),
                entry.address.clone(),
            );
        }

        let mut moved = 0usize;
        let mut changed = false;
        for candidate in candidates {
            if !candidate.checked {
                continue;
            }

            let Some(repo) = self
                .repository_view_state
                .repositories
                .get_mut(candidate.repo_index)
            else {
                continue;
            };

            let normalized_repo = Self::normalize_repo_url(&repo.address);
            if !entries_by_address.contains_key(&normalized_repo) {
                continue;
            }

            repo.repository_space_id = Some(space_id.to_string());
            repo.repository_space_entry_address = entries_by_address.get(&normalized_repo).cloned();
            if repo.path.trim().is_empty() {
                repo.path = space.shared_path.clone();
            }
            moved += 1;
            changed = true;
        }

        if changed {
            self.save_repositories();
        }

        moved
    }
}
