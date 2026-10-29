use std::collections::HashSet;

use log::{info, warn};

use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::types::{
    BulkActionEntry, RepoInstanceKey, RepoState, Repository, RepositorySpace,
    RepositorySpaceBulkAction, RepositorySpaceBulkMode, RepositoryVisualFolder,
};

#[derive(Debug, Clone)]
pub(crate) struct QueuedRepositorySpaceSync {
    pub space_id: String,
    pub target: RepoInstanceKey,
    pub mode: SyncMode,
}

#[derive(Debug, Clone)]
pub(crate) struct QueuedRepositoryVisualFolderSync {
    pub folder_id: String,
    pub target: RepoInstanceKey,
    pub mode: SyncMode,
}

fn repo_key(repo: &Repository) -> RepoInstanceKey {
    RepoInstanceKey {
        remote_url: repo.address.clone(),
        local_path: repo.path.clone(),
    }
}

fn queued_target_matches(repo: &Repository, target: &RepoInstanceKey) -> bool {
    queued_identity_matches(&repo.address, &repo.path, target)
}

fn queued_identity_matches(address: &str, path: &str, target: &RepoInstanceKey) -> bool {
    Foxy::normalize_repo_url(address) == Foxy::normalize_repo_url(&target.remote_url)
        && Foxy::repo_instance_path_key(path) == Foxy::repo_instance_path_key(&target.local_path)
}

fn queued_target_remains_in_folder(
    folder: &RepositoryVisualFolder,
    target: &RepoInstanceKey,
) -> bool {
    folder.repository_keys.contains(&Foxy::repo_instance_key(
        &target.remote_url,
        &target.local_path,
    ))
}

fn repository_space_sync_targets(
    space: &RepositorySpace,
    repositories: &[Repository],
    folders: &[RepositoryVisualFolder],
) -> Vec<usize> {
    let mut targets: Vec<usize> = repositories
        .iter()
        .enumerate()
        .filter_map(|(idx, repo)| {
            (repo.repository_space_id.as_deref() == Some(space.id.as_str())).then_some(idx)
        })
        .collect();
    targets.sort_by_cached_key(|idx| {
        let repo = &repositories[*idx];
        let key = Foxy::repo_instance_key(&repo.address, &repo.path);
        folders
            .iter()
            .position(|folder| {
                folder.repository_space_id.as_deref() == Some(space.id.as_str())
                    && folder.repository_keys.contains(&key)
            })
            .unwrap_or(folders.len())
    });
    targets
}

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

        repository_space_sync_targets(
            space,
            &self.repository_view_state.repositories,
            &self.repository_visual_folders,
        )
    }

    pub fn queue_repository_space_sync(&mut self, space_id: &str, mode: SyncMode) -> usize {
        let targets = self.collect_repository_space_sync_targets(space_id);
        self.queue_selected_repository_space_sync(space_id, mode, &targets)
    }

    pub fn collect_repository_visual_folder_sync_targets(&self, folder_id: &str) -> Vec<usize> {
        let Some(folder) = self
            .repository_visual_folders
            .iter()
            .find(|folder| folder.id == folder_id)
        else {
            return Vec::new();
        };
        let folder_keys: HashSet<&str> =
            folder.repository_keys.iter().map(String::as_str).collect();
        self.repository_view_state
            .repositories
            .iter()
            .enumerate()
            .filter_map(|(idx, repo)| {
                let key = Self::repo_instance_key(&repo.address, &repo.path);
                folder_keys.contains(key.as_str()).then_some(idx)
            })
            .collect()
    }

    pub fn queue_repository_visual_folder_sync(
        &mut self,
        folder_id: &str,
        mode: SyncMode,
    ) -> usize {
        if self.block_database_update_request() {
            return 0;
        }
        let targets = self.collect_repository_visual_folder_sync_targets(folder_id);
        self.repository_visual_folder_sync_queue.clear();
        for repo_idx in targets {
            let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
                continue;
            };
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                continue;
            }
            let Some(folder) = self
                .repository_visual_folders
                .iter()
                .find(|folder| folder.id == folder_id)
            else {
                continue;
            };
            self.repository_visual_folder_sync_queue
                .push_back(QueuedRepositoryVisualFolderSync {
                    folder_id: folder.id.clone(),
                    target: repo_key(repo),
                    mode,
                });
        }

        let queued = self.repository_visual_folder_sync_queue.len();
        if queued > 0 {
            self.process_repository_visual_folder_sync_queue();
        }
        queued
    }

    pub fn queue_selected_repository_space_sync(
        &mut self,
        space_id: &str,
        mode: SyncMode,
        selected_repo_indices: &[usize],
    ) -> usize {
        let queued =
            self.enqueue_selected_repository_space_sync(space_id, mode, selected_repo_indices);
        if queued > 0 {
            self.process_repository_space_sync_queue();
        }
        queued
    }

    fn enqueue_selected_repository_space_sync(
        &mut self,
        space_id: &str,
        mode: SyncMode,
        selected_repo_indices: &[usize],
    ) -> usize {
        if self.block_database_update_request() {
            return 0;
        }
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
                .push_back(QueuedRepositorySpaceSync {
                    space_id: space_id.to_string(),
                    target: repo_key(repo),
                    mode,
                });
        }

        self.repository_space_sync_queue.len()
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
            .map(|entry| Self::repo_url_match_key(&entry.address))
            .collect();

        let entries = self
            .collect_repository_space_sync_targets(space_id)
            .into_iter()
            .filter_map(|repo_index| {
                let repo = self.repository_view_state.repositories.get(repo_index)?;
                let normalized_repo_url = Self::repo_url_match_key(&repo.address);
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
        // Processing starts the first repository and pops it from the queue, so
        // the targets are read from the queue before it runs.
        let queued = self.enqueue_selected_repository_space_sync(
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
            .filter(|queued| {
                queued.space_id == space_id && queued.mode == Self::sync_mode_for_bulk_mode(mode)
            })
            .filter_map(|queued| {
                self.repository_view_state
                    .repositories
                    .iter()
                    .find(|repo| queued_target_matches(repo, &queued.target))
            })
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
        self.process_repository_space_sync_queue();
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
        let repo_space_id = repo.repository_space_id.clone();

        let completion_summary: Option<(RepositorySpaceBulkMode, usize, usize, usize, usize)> = {
            let Some(progress) = self.repository_space_bulk_progress.as_mut() else {
                return;
            };
            // A same-URL copy outside the space is a different instance.
            if repo_space_id.as_deref() != Some(progress.space_id.as_str()) {
                return;
            }
            if mode != Some(Self::sync_mode_for_bulk_mode(progress.mode)) {
                return;
            }
            if !progress.record(&repo_url, finished_successfully, had_updates) {
                return;
            }

            if progress.is_finished() {
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
        if self.pending_download_retry.is_some()
            || self.database_updates_blocked()
            || self.repository_sync_active()
            || self.is_direct_download_running()
            || self.retained_files_cleanup_in_flight
            || self.pending_retained_file_removal.is_some()
        {
            return;
        }

        while let Some(queued) = self.repository_space_sync_queue.pop_front() {
            let space_id = queued.space_id;
            let mode = queued.mode;
            let Some(repo_idx) = self
                .repository_view_state
                .repositories
                .iter()
                .position(|repo| queued_target_matches(repo, &queued.target))
            else {
                self.skip_changed_repository_space_target(&space_id, &queued.target);
                continue;
            };
            let repo = &self.repository_view_state.repositories[repo_idx];
            let repo_url = Self::normalize_repo_url(&repo.address);
            if repo.repository_space_id.as_deref() != Some(space_id.as_str()) {
                // Detached since it was queued: no longer part of the run.
                if let Some(progress) = self
                    .repository_space_bulk_progress
                    .as_mut()
                    .filter(|progress| progress.space_id == space_id)
                {
                    progress.remove_target(&repo_url);
                }
                self.finish_repository_space_bulk_if_done();
                continue;
            }
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                warn!(
                    "Skipping queued repository-space sync for {} due to incomplete configuration",
                    repo.name
                );
                self.record_repository_space_bulk_completion(
                    Some(repo_idx),
                    Some(mode),
                    false,
                    false,
                );
                continue;
            }

            let repo_name = repo.name.clone();
            self.start_core_sync(repo_idx, mode);
            if self.syncing_repository == Some(repo_idx) {
                self.sync_is_bulk = true;
                self.refresh_repository_space_bulk_current_repo();
                info!(
                    "Started queued repository-space sync: {} mode={:?}",
                    repo_name, mode
                );
                break;
            }
            // Refused to start (unwritable folder, pending purge): count it, or
            // the run never reaches its total and the space stays locked.
            warn!(
                "Queued repository-space sync for {} could not start; counting it as failed",
                repo_name
            );
            self.record_repository_space_bulk_completion(Some(repo_idx), Some(mode), false, false);
        }
    }

    fn skip_changed_repository_space_target(&mut self, space_id: &str, target: &RepoInstanceKey) {
        if let Some(progress) = self
            .repository_space_bulk_progress
            .as_mut()
            .filter(|progress| progress.space_id == space_id)
        {
            progress.remove_target(&Self::normalize_repo_url(&target.remote_url));
        }
        self.show_error_toast(
            self.t("Queued repository skipped because its address or folder changed."),
        );
        self.finish_repository_space_bulk_if_done();
    }

    /// Clear a run whose remaining members were all taken out of it.
    fn finish_repository_space_bulk_if_done(&mut self) {
        if self
            .repository_space_bulk_progress
            .as_ref()
            .is_some_and(|progress| progress.total_count == 0 && progress.failed_count == 0)
        {
            self.repository_space_bulk_progress = None;
        }
    }

    /// The user's Cancel stops the run the cancelled sync belongs to, not just
    /// that one repository: the folder and space queues are dropped and an
    /// active scheduled job ends without its post-action.
    pub(in crate::ui::app) fn drop_queued_repository_runs(&mut self) {
        let dropped_folder = self.repository_visual_folder_sync_queue.len();
        self.repository_visual_folder_sync_queue.clear();
        let dropped_space: Vec<_> = self.repository_space_sync_queue.drain(..).collect();
        for queued in &dropped_space {
            let repo_url = Self::normalize_repo_url(&queued.target.remote_url);
            if let Some(progress) = self
                .repository_space_bulk_progress
                .as_mut()
                .filter(|progress| progress.space_id == queued.space_id)
            {
                progress.remove_target(&repo_url);
            }
        }
        self.finish_repository_space_bulk_if_done();
        let cancelled_job = self.cancel_scheduled_job_run();
        if dropped_folder > 0 || !dropped_space.is_empty() || cancelled_job {
            info!(
                "Cancel stopped queued work: folder_entries={} space_entries={} scheduled_job={}",
                dropped_folder,
                dropped_space.len(),
                cancelled_job
            );
        }
    }

    pub(in crate::ui::app) fn process_repository_visual_folder_sync_queue(&mut self) {
        if self.pending_download_retry.is_some()
            || self.database_updates_blocked()
            || self.repository_sync_active()
            || self.is_direct_download_running()
        {
            return;
        }

        while let Some(queued) = self.repository_visual_folder_sync_queue.pop_front() {
            let Some(repo_idx) = self
                .repository_view_state
                .repositories
                .iter()
                .position(|repo| queued_target_matches(repo, &queued.target))
            else {
                self.show_error_toast(
                    self.t("Queued repository skipped because its address or folder changed."),
                );
                continue;
            };
            let still_in_folder = self.repository_visual_folders.iter().any(|folder| {
                folder.id == queued.folder_id
                    && queued_target_remains_in_folder(folder, &queued.target)
            });
            if !still_in_folder {
                self.show_error_toast(
                    self.t("Queued repository skipped because it left the selected folder."),
                );
                continue;
            }
            let repo = &self.repository_view_state.repositories[repo_idx];
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                warn!(
                    "Skipping queued visual-folder sync for {} due to incomplete configuration",
                    repo.name
                );
                continue;
            }

            let repo_name = repo.name.clone();
            self.start_core_sync(repo_idx, queued.mode);
            if self.syncing_repository == Some(repo_idx) {
                self.sync_is_bulk = true;
                self.sync_visual_folder_id = Some(queued.folder_id.clone());
                info!(
                    "Started queued visual-folder sync: {} mode={:?}",
                    repo_name, queued.mode
                );
                break;
            }
        }
    }

    pub(in crate::ui::app) fn process_addon_hash_recalc_queue(&mut self) {
        if self.database_updates_blocked()
            || self.repository_sync_active()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_identity_matches_only_the_original_address_and_path() {
        let target = RepoInstanceKey {
            remote_url: "https://repo.example.invalid/pack".into(),
            local_path: "R:/Mods/Pack".into(),
        };
        assert!(queued_identity_matches(
            "https://repo.example.invalid/pack/",
            "r:/mods/pack",
            &target,
        ));
        assert!(!queued_identity_matches(
            "https://repo.example.invalid/other/",
            "R:/Mods/Pack",
            &target,
        ));
        assert!(!queued_identity_matches(
            "https://repo.example.invalid/pack/",
            "R:/Mods/Other",
            &target,
        ));
    }

    #[test]
    fn visual_folder_queue_member_must_still_be_in_original_folder() {
        let target = RepoInstanceKey {
            remote_url: "https://repo.example.invalid/pack/".into(),
            local_path: "R:/Mods/Pack".into(),
        };
        let mut folder = RepositoryVisualFolder {
            id: "folder".into(),
            name: "Folder".into(),
            repository_space_id: None,
            color_rgb: [0, 0, 0],
            collapsed: false,
            repository_keys: vec![Foxy::repo_instance_key(
                &target.remote_url,
                &target.local_path,
            )],
        };
        assert!(queued_target_remains_in_folder(&folder, &target));
        folder.repository_keys.clear();
        assert!(!queued_target_remains_in_folder(&folder, &target));
    }

    fn space() -> RepositorySpace {
        serde_json::from_value(serde_json::json!({
            "id": "space-alpha",
            "name": "SpaceAlpha",
            "entries": [
                { "name": "RepoAlpha", "address": "https://repo.example.invalid:8080/alpha/", "required": true },
                { "name": "RepoBeta", "address": "https://repo.example.invalid:8080/beta/", "required": false }
            ]
        }))
        .expect("valid space fixture")
    }

    fn repository(address: &str, space_id: Option<&str>) -> Repository {
        Repository {
            address: address.to_string(),
            path: "R:/Mods/Shared".to_string(),
            repository_space_id: space_id.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn space_sync_follows_user_order_instead_of_manifest_or_required_order() {
        let mut space = space();
        let mut repositories = vec![
            repository(&space.entries[0].address, Some(&space.id)),
            repository(&space.entries[1].address, Some(&space.id)),
        ];
        let moved = repositories.remove(1);
        repositories.insert(0, moved);

        assert_eq!(
            repository_space_sync_targets(&space, &repositories, &[]),
            vec![0, 1]
        );
        space.entries.reverse();
        space.entries[0].required = true;
        assert_eq!(
            repository_space_sync_targets(&space, &repositories, &[]),
            vec![0, 1]
        );
    }

    #[test]
    fn space_sync_keeps_members_missing_from_manifest_in_user_order() {
        let space = space();
        let repositories = vec![
            repository("https://repo.example.invalid:8080/extra/", Some(&space.id)),
            repository(&space.entries[0].address, None),
            repository(&space.entries[1].address, Some("space-beta")),
            repository(&space.entries[1].address, Some(&space.id)),
            repository(&space.entries[0].address, Some(&space.id)),
        ];

        assert_eq!(
            repository_space_sync_targets(&space, &repositories, &[]),
            vec![0, 3, 4]
        );
        assert!(repository_space_sync_targets(&space, &[], &[]).is_empty());
        assert!(repository_space_sync_targets(&space, &repositories[1..3], &[]).is_empty());
    }

    #[test]
    fn space_sync_follows_folder_order_then_ungrouped_repositories() {
        let space = space();
        let mut repositories = vec![
            repository(&space.entries[0].address, Some(&space.id)),
            repository(&space.entries[1].address, Some(&space.id)),
            repository(&space.entries[0].address, Some(&space.id)),
            repository("https://repo.example.invalid:8080/extra/", Some(&space.id)),
        ];
        repositories[2].path = "R:/Mods/Other".to_string();
        let folder = |id: &str, space_id: &str, indices: &[usize], collapsed: bool| {
            serde_json::from_value::<RepositoryVisualFolder>(serde_json::json!({
                "id": id,
                "name": id,
                "repository_space_id": space_id,
                "collapsed": collapsed,
                "repository_keys": indices.iter().map(|idx| {
                    let repo = &repositories[*idx];
                    Foxy::repo_instance_key(&repo.address, &repo.path)
                }).collect::<Vec<_>>()
            }))
            .expect("valid folder fixture")
        };
        let folders = vec![
            folder("unrelated", "space-beta", &[0], false),
            folder("first", &space.id, &[3, 2], true),
            folder("second", &space.id, &[1], false),
        ];

        assert_eq!(
            repository_space_sync_targets(&space, &repositories, &folders),
            vec![2, 3, 1, 0]
        );
    }
}
