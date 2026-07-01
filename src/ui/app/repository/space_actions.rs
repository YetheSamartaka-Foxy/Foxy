use std::collections::HashSet;

use log::{info, warn};

use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::types::{
    BulkActionEntry, RepoState, RepositorySpaceBulkAction, RepositorySpaceBulkMode,
};

impl Foxy {
    pub fn detach_repository_from_space(&mut self, repo_idx: usize) -> bool {
        let Some(repo) = self.repository_view_state.repositories.get_mut(repo_idx) else {
            return false;
        };

        if repo.repository_space_id.is_none() {
            return false;
        }

        repo.repository_space_id = None;
        repo.repository_space_entry_address = None;
        self.save_repositories();
        true
    }

    pub(in crate::ui::app) fn collect_repository_space_sync_targets(
        &self,
        space_id: &str,
    ) -> Vec<usize> {
        let Some(space) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
        else {
            return Vec::new();
        };

        let mut ordered_entry_urls = Vec::new();
        for required in [true, false] {
            for entry in &space.entries {
                if entry.required == required {
                    ordered_entry_urls.push(Self::normalize_repo_url(&entry.address));
                }
            }
        }

        let mut seen = HashSet::new();
        let mut ordered_targets = Vec::new();
        for entry_url in &ordered_entry_urls {
            for (idx, repo) in self.repository_view_state.repositories.iter().enumerate() {
                if repo.repository_space_id.as_deref() != Some(space_id) || seen.contains(&idx) {
                    continue;
                }
                if Self::normalize_repo_url(&repo.address) == *entry_url {
                    seen.insert(idx);
                    ordered_targets.push(idx);
                }
            }
        }

        for (idx, repo) in self.repository_view_state.repositories.iter().enumerate() {
            if repo.repository_space_id.as_deref() == Some(space_id) && seen.insert(idx) {
                ordered_targets.push(idx);
            }
        }

        ordered_targets
    }

    pub fn queue_repository_space_sync(&mut self, space_id: &str, mode: SyncMode) -> usize {
        let targets = self.collect_repository_space_sync_targets(space_id);
        self.queue_selected_repository_space_sync(space_id, mode, &targets)
    }

    pub fn queue_selected_repository_space_sync(
        &mut self,
        space_id: &str,
        mode: SyncMode,
        selected_repo_indices: &[usize],
    ) -> usize {
        let selected: HashSet<usize> = selected_repo_indices.iter().copied().collect();
        if selected.is_empty() {
            self.repository_space_sync_queue.clear();
            return 0;
        }

        let ordered_targets = self
            .collect_repository_space_sync_targets(space_id)
            .into_iter()
            .filter(|repo_idx| selected.contains(repo_idx))
            .collect::<Vec<_>>();

        self.repository_space_sync_queue.clear();
        for repo_idx in ordered_targets {
            let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
                continue;
            };
            if repo.repository_space_id.as_deref() != Some(space_id)
                || repo.address.trim().is_empty()
                || repo.path.trim().is_empty()
            {
                continue;
            }
            self.repository_space_sync_queue
                .push_back((space_id.to_string(), repo_idx, mode));
        }

        let queued = self.repository_space_sync_queue.len();
        if queued > 0 {
            self.process_repository_space_sync_queue();
        }
        queued
    }

    fn sync_mode_for_bulk_mode(mode: RepositorySpaceBulkMode) -> SyncMode {
        match mode {
            RepositorySpaceBulkMode::RecheckAll => SyncMode::RemoteRefreshOnly,
            RepositorySpaceBulkMode::UpdateAll => SyncMode::Download,
        }
    }

    pub fn build_repository_space_bulk_action(
        &self,
        space_id: &str,
        mode: RepositorySpaceBulkMode,
    ) -> Option<RepositorySpaceBulkAction> {
        let space = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)?
            .clone();

        let required_urls: HashSet<String> = space
            .entries
            .iter()
            .filter(|entry| entry.required)
            .map(|entry| Self::normalize_repo_url(&entry.address))
            .collect();

        let entries = self
            .collect_repository_space_sync_targets(space_id)
            .into_iter()
            .filter_map(|repo_index| {
                let repo = self.repository_view_state.repositories.get(repo_index)?;
                let normalized_repo_url = Self::normalize_repo_url(&repo.address);
                let state = self.repo_state_for_address(&repo.address, &repo.path);
                let default_selected = match mode {
                    RepositorySpaceBulkMode::RecheckAll => true,
                    RepositorySpaceBulkMode::UpdateAll => state == RepoState::PendingUpdate,
                };
                Some(BulkActionEntry {
                    repo_index,
                    repo_name: repo.name.clone(),
                    current_state: state,
                    selected: default_selected,
                    required: required_urls.contains(&normalized_repo_url),
                })
            })
            .collect();

        Some(RepositorySpaceBulkAction {
            space_id: space.id.clone(),
            space_name: Self::repository_space_display_name(&space).to_string(),
            mode,
            entries,
        })
    }

    pub fn start_repository_space_bulk_action(
        &mut self,
        space_id: &str,
        mode: RepositorySpaceBulkMode,
        selected_repo_indices: &[usize],
    ) -> usize {
        let queued = self.queue_selected_repository_space_sync(
            space_id,
            Self::sync_mode_for_bulk_mode(mode),
            selected_repo_indices,
        );

        if queued == 0 {
            self.repository_space_bulk_progress = None;
            return 0;
        }

        let target_repo_urls: HashSet<String> = self
            .repository_space_sync_queue
            .iter()
            .filter(|(queued_space_id, _, queued_mode)| {
                queued_space_id == space_id && *queued_mode == Self::sync_mode_for_bulk_mode(mode)
            })
            .filter_map(|(_, idx, _)| self.repository_view_state.repositories.get(*idx))
            .map(|repo| Self::normalize_repo_url(&repo.address))
            .collect();
        let total_count = target_repo_urls.len();
        self.repository_space_bulk_progress = Some(crate::ui::types::RepositorySpaceBulkProgress {
            space_id: space_id.to_string(),
            mode,
            total_count,
            completed_count: 0,
            succeeded_count: 0,
            failed_count: 0,
            updates_available_count: 0,
            up_to_date_count: 0,
            current_repo_name: None,
            target_repo_urls,
            completed_repo_urls: HashSet::new(),
        });
        self.refresh_repository_space_bulk_current_repo();
        queued
    }

    pub(in crate::ui::app) fn refresh_repository_space_bulk_current_repo(&mut self) {
        let current_repo_name = self
            .syncing_repository
            .and_then(|repo_idx| self.repository_view_state.repositories.get(repo_idx))
            .map(|repo| repo.name.clone());
        if let Some(progress) = self.repository_space_bulk_progress.as_mut() {
            progress.current_repo_name = current_repo_name;
        }
    }

    pub(in crate::ui::app) fn record_repository_space_bulk_completion(
        &mut self,
        repo_idx: Option<usize>,
        mode: Option<SyncMode>,
        finished_successfully: bool,
        had_updates: bool,
    ) {
        let Some(repo_idx) = repo_idx else {
            return;
        };
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return;
        };
        let repo_url = Self::normalize_repo_url(&repo.address);

        let completion_summary: Option<(RepositorySpaceBulkMode, usize, usize, usize, usize)> = {
            let Some(progress) = self.repository_space_bulk_progress.as_mut() else {
                return;
            };
            if progress.completed_repo_urls.contains(&repo_url) {
                return;
            }
            if !progress.target_repo_urls.contains(&repo_url) {
                return;
            }

            let expected_mode = match progress.mode {
                RepositorySpaceBulkMode::RecheckAll => SyncMode::RemoteRefreshOnly,
                RepositorySpaceBulkMode::UpdateAll => SyncMode::Download,
            };
            if mode != Some(expected_mode) {
                return;
            }

            progress.completed_repo_urls.insert(repo_url);
            progress.completed_count += 1;
            if finished_successfully {
                progress.succeeded_count += 1;
                if progress.mode == RepositorySpaceBulkMode::RecheckAll {
                    if had_updates {
                        progress.updates_available_count += 1;
                    } else {
                        progress.up_to_date_count += 1;
                    }
                }
            } else {
                progress.failed_count += 1;
            }

            if progress.completed_count >= progress.total_count {
                progress.current_repo_name = None;
                Some((
                    progress.mode,
                    progress.up_to_date_count,
                    progress.updates_available_count,
                    progress.succeeded_count,
                    progress.failed_count,
                ))
            } else {
                None
            }
        };
        if let Some((
            bulk_mode,
            up_to_date_count,
            updates_available_count,
            succeeded_count,
            failed_count,
        )) = completion_summary
        {
            let message = match bulk_mode {
                RepositorySpaceBulkMode::RecheckAll => self.t_fmt(
                    "Recheck complete: {up_to_date} up to date, {updates} updates available, {failed} failed",
                    &[
                        ("up_to_date", up_to_date_count.to_string()),
                        ("updates", updates_available_count.to_string()),
                        ("failed", failed_count.to_string()),
                    ],
                ),
                RepositorySpaceBulkMode::UpdateAll => self.t_fmt(
                    "Update complete: {updated} updated, {failed} failed",
                    &[
                        ("updated", succeeded_count.to_string()),
                        ("failed", failed_count.to_string()),
                    ],
                ),
            };
            if failed_count > 0 {
                self.show_error_toast(message);
            } else {
                self.show_success_toast(message);
            }
        }
    }

    pub(in crate::ui::app) fn process_repository_space_sync_queue(&mut self) {
        if self.repository_sync_active() || self.is_direct_download_running() {
            return;
        }

        while let Some((space_id, repo_idx, mode)) = self.repository_space_sync_queue.pop_front() {
            let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
                continue;
            };
            if repo.repository_space_id.as_deref() != Some(space_id.as_str()) {
                continue;
            }
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                warn!(
                    "Skipping queued repository-space sync for {} due to incomplete configuration",
                    repo.name
                );
                continue;
            }

            let repo_name = repo.name.clone();
            self.start_core_sync(repo_idx, mode);
            if self.syncing_repository == Some(repo_idx) {
                self.refresh_repository_space_bulk_current_repo();
                info!(
                    "Started queued repository-space sync: {} mode={:?}",
                    repo_name, mode
                );
                break;
            }
        }
    }

    pub(in crate::ui::app) fn process_addon_hash_recalc_queue(&mut self) {
        if self.repository_sync_active()
            || self.is_direct_download_running()
            || self.addon_hash_recalc_in_flight
        {
            return;
        }

        while let Some((repo_url, addon_name)) = self.addon_hash_recalc_queue.pop_front() {
            let Some(repo_idx) = self.repo_index_by_normalized_url(&repo_url) else {
                warn!(
                    "Skipping queued addon hash recalculation for {}: repository {} no longer exists",
                    addon_name, repo_url
                );
                continue;
            };

            let repo_name = self.repository_view_state.repositories[repo_idx]
                .name
                .clone();
            if self.recalculate_addon_hashes(repo_idx, &addon_name) {
                info!(
                    "Started queued addon hash recalculation for {} in {}",
                    addon_name, repo_name
                );
                break;
            }
        }
    }
}
