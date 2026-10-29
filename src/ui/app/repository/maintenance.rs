use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tokio::runtime::Runtime;

use crate::core::tasks::purge_repository::{
    purge_addon_by_local_path, purge_repository_by_url, purge_repository_db_only_by_url,
    purge_repository_db_only_by_url_and_path,
};
use crate::ui::app::{AddonDeleteResult, Foxy, RepositoryDbWipeResult};
use crate::ui::types::{Repository, sanitize_user_path};

/// Why deleting a repository with "Delete files" keeps its files on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteFilesKept {
    SharedFolder,
    SameAddressElsewhere,
}

impl Foxy {
    /// The file purge is keyed by address, so it is skipped while another entry
    /// still uses the deleted repository's address.
    pub(crate) fn delete_files_kept_reason(
        repositories: &[Repository],
        repo_idx: usize,
    ) -> Option<DeleteFilesKept> {
        let repo = repositories.get(repo_idx)?;
        let url = Self::normalize_repo_url(&repo.address);
        let instance = Self::repo_instance_key(&repo.address, &repo.path);
        let mut same_address = false;
        for (idx, other) in repositories.iter().enumerate() {
            if idx == repo_idx || Self::normalize_repo_url(&other.address) != url {
                continue;
            }
            if Self::repo_instance_key(&other.address, &other.path) == instance {
                return Some(DeleteFilesKept::SharedFolder);
            }
            same_address = true;
        }
        same_address.then_some(DeleteFilesKept::SameAddressElsewhere)
    }

    pub fn delete_files_kept_note(&self, repo_idx: usize) -> Option<String> {
        Self::delete_files_kept_reason(&self.repository_view_state.repositories, repo_idx).map(
            |reason| match reason {
                DeleteFilesKept::SharedFolder => self.t(
                    "Files will not be deleted because another repository in your list uses the same address and folder. Only this entry is removed.",
                ),
                DeleteFilesKept::SameAddressElsewhere => self.t(
                    "Files will not be deleted because another repository in your list uses the same address. Only this folder's records are removed, so delete its files yourself if you no longer need them.",
                ),
            },
        )
    }

    pub fn delete_addon_from_storage_and_database(&mut self, addon_name: &str, addon_path: &str) {
        if self.repository_sync_active()
            || self.is_direct_download_running()
            || self.retained_files_cleanup_in_flight
            || self.pending_retained_file_removal.is_some()
        {
            warn!("Addon delete ignored: sync or download worker is currently active");
            self.show_error_toast(self.t("Operation cancelled"));
            return;
        }

        let addon_name = addon_name.trim();
        let addon_path = addon_path.trim();
        if addon_name.is_empty() || addon_path.is_empty() {
            warn!("Addon delete ignored: addon name or path is empty");
            self.show_error_toast(self.t("Operation cancelled"));
            return;
        }

        let delete_key = Self::normalize_path_for_addon_match(addon_path);
        if !self.pending_addon_deletes.insert(delete_key) {
            debug!("Addon delete skipped for {} (already running)", addon_name);
            return;
        }

        info!("Addon delete requested for {}", addon_name);
        self.needs_repaint = true;

        let addon_delete_result_tx = self.addon_delete_result_tx.clone();
        let thread_name = addon_name.to_string();
        let thread_path = addon_path.to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let purge_result = match Runtime::new() {
                Ok(rt) => rt
                    .block_on(purge_addon_by_local_path(&thread_path))
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };

            if let Err(err) = &purge_result {
                warn!("Addon delete worker failed for {}: {}", thread_name, err);
            }

            if addon_delete_result_tx
                .send(AddonDeleteResult {
                    addon_name: thread_name,
                    addon_path: thread_path,
                    outcome: purge_result,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            } else {
                warn!("Failed to report addon delete completion");
            }
        });
    }

    /// `forget_hash_record` also drops the folder's verified-hash record, so the
    /// next check reads every file; the user wipe does, a measurement may not.
    pub fn wipe_repository_database_entries(&mut self, repo_idx: usize, forget_hash_record: bool) {
        if self.repository_sync_active() || self.is_direct_download_running() {
            warn!("Repository database wipe ignored: sync worker is currently active");
            return;
        }

        let repo = match self
            .repository_view_state
            .repositories
            .get(repo_idx)
            .cloned()
        {
            Some(r) => r,
            None => {
                warn!(
                    "Repository database wipe ignored: invalid repository index {}",
                    repo_idx
                );
                return;
            }
        };

        // A scan finishing after the wipe would restore the verdict and hashes
        // the user just asked to drop.
        if self
            .active_quick_scan_instance_keys
            .contains(&Self::repo_instance_key(&repo.address, &repo.path))
        {
            warn!(
                "Repository database wipe ignored for {}: a quick local check is running",
                repo.name
            );
            self.show_error_toast(self.t("Quick local check in progress"));
            return;
        }

        let normalized_url = Self::normalize_repo_url(&repo.address);
        // Scope the wipe to this repository's own download folder so a sibling
        // repository that shares the same remote URL but a different folder keeps
        // its cached metadata and local hashes.
        self.wipe_repository_database_entries_by_url_and_path(
            &normalized_url,
            &repo.path,
            &repo.name,
            forget_hash_record,
        );
    }

    /// Address and folder identify the instance that running work writes to.
    /// Changing them mid-flight lands the work's results on the new identity
    /// and wipes the old folder's rows while the worker still writes them.
    pub fn repository_identity_locked(&self, repo_idx: usize) -> bool {
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return false;
        };
        self.syncing_repository == Some(repo_idx)
            || self
                .active_quick_scan_instance_keys
                .contains(&Self::repo_instance_key(&repo.address, &repo.path))
            || self.is_repository_db_wipe_pending(&repo.address)
    }

    pub fn is_repository_db_wipe_pending(&self, repository_url: &str) -> bool {
        let normalized_url = Self::normalize_repo_url(repository_url);
        self.pending_repository_db_wipes.contains(&normalized_url)
    }

    pub fn is_repository_force_redownload_pending(&self, repository_url: &str) -> bool {
        let normalized_url = Self::normalize_repo_url(repository_url);
        self.pending_repository_force_redownloads
            .contains(&normalized_url)
    }

    pub fn repository_db_wipe_elapsed(&self, repository_url: &str) -> Option<Duration> {
        let normalized_url = Self::normalize_repo_url(repository_url);
        self.pending_repository_db_wipe_started_at
            .get(&normalized_url)
            .map(|started| started.elapsed())
    }

    pub(in crate::ui::app) fn clear_repo_state_for_url(
        &mut self,
        repository_url: &str,
        local_path: &str,
    ) {
        let instance_key = Self::repo_instance_key(repository_url, local_path);
        if self.repo_states.remove(&instance_key).is_some() {
            self.repo_states_version = self.repo_states_version.wrapping_add(1);
        }
        self.pending_update_cache.remove(&instance_key);

        // Foxy mode is a property of the URL, shared by every folder instance, so
        // only forget it once no other configured repository still uses the URL.
        let url_key = Self::normalize_repo_url(repository_url);
        let url_still_used = self
            .repository_view_state
            .repositories
            .iter()
            .any(|repo| Self::normalize_repo_url(&repo.address) == url_key);
        if !url_still_used {
            self.repo_foxy_modes.remove(&url_key);
        }
    }

    pub fn wipe_repository_database_entries_by_url(
        &mut self,
        repository_url: &str,
        repo_name: &str,
    ) {
        let normalized_url = Self::normalize_repo_url(repository_url);

        if self.repo_db_reset_pending_recheck.remove(&normalized_url) {
            debug!(
                "Repository database wipe for {} is replacing a pending rebuild marker",
                repo_name
            );
        }

        if self.pending_repository_db_wipes.contains(&normalized_url) {
            debug!(
                "Repository database wipe skipped for {} (already running)",
                repo_name
            );
            return;
        }

        info!(
            "Repository database wipe requested for repository {}",
            repo_name
        );
        self.completed_repository_check_banner = None;
        self.completed_repository_db_wipe_banner = None;
        self.pending_repository_db_wipes
            .insert(normalized_url.clone());
        self.pending_repository_db_wipe_started_at
            .insert(normalized_url.clone(), Instant::now());
        self.needs_repaint = true;

        let repository_db_wipe_tx = self.repository_db_wipe_tx.clone();
        let thread_url = normalized_url.clone();
        let thread_name = repo_name.to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let started_at = Instant::now();
            let purge_result = match Runtime::new() {
                Ok(rt) => rt
                    .block_on(purge_repository_db_only_by_url(&thread_url))
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            let elapsed = started_at.elapsed();

            if purge_result.is_ok() {
                info!(
                    "Repository database wipe worker completed for {} in {:.2}s",
                    thread_name,
                    elapsed.as_secs_f64()
                );
            } else if let Err(err) = &purge_result {
                warn!(
                    "Repository database wipe worker failed for {} in {:.2}s: {}",
                    thread_name,
                    elapsed.as_secs_f64(),
                    err
                );
            }

            if repository_db_wipe_tx
                .send(RepositoryDbWipeResult {
                    repository_url: thread_url,
                    local_path: String::new(),
                    repository_name: thread_name,
                    elapsed,
                    result: purge_result,
                    force_redownload_after_purge: false,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            } else {
                warn!("Failed to report repository database wipe completion");
            }
        });
    }

    /// Wipe cached database entries for a single repository instance, identified
    /// by remote URL *and* local download folder. Use this instead of
    /// [`Self::wipe_repository_database_entries_by_url`] whenever a sibling
    /// repository may share the same URL under a different folder (independent
    /// installs, repository-space entries), since the URL-only purge would also
    /// destroy that sibling's cached metadata and computed local hashes.
    ///
    /// A blank `local_path` resolves to no instance, so the purge is a safe no-op
    /// (e.g. when a freshly added repository that never had a folder is given one).
    pub fn wipe_repository_database_entries_by_url_and_path(
        &mut self,
        repository_url: &str,
        local_path: &str,
        repo_name: &str,
        forget_hash_record: bool,
    ) {
        let normalized_url = Self::normalize_repo_url(repository_url);

        if self.repo_db_reset_pending_recheck.remove(&normalized_url) {
            debug!(
                "Repository database wipe for {} is replacing a pending rebuild marker",
                repo_name
            );
        }

        if self.pending_repository_db_wipes.contains(&normalized_url) {
            debug!(
                "Repository database wipe skipped for {} (already running)",
                repo_name
            );
            return;
        }

        info!(
            "Repository database wipe requested for repository {} (scoped to local folder)",
            repo_name
        );
        self.completed_repository_check_banner = None;
        self.completed_repository_db_wipe_banner = None;
        self.pending_repository_db_wipes
            .insert(normalized_url.clone());
        self.pending_repository_db_wipe_started_at
            .insert(normalized_url.clone(), Instant::now());
        self.needs_repaint = true;

        let repository_db_wipe_tx = self.repository_db_wipe_tx.clone();
        let thread_url = normalized_url.clone();
        let thread_path = sanitize_user_path(local_path);
        let thread_name = repo_name.to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let started_at = Instant::now();
            if forget_hash_record {
                crate::core::tasks::calculate_hashes::forget_verified_hashes_under(&thread_path);
            }
            let purge_result = match Runtime::new() {
                Ok(rt) => rt
                    .block_on(purge_repository_db_only_by_url_and_path(
                        &thread_url,
                        &thread_path,
                    ))
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            let elapsed = started_at.elapsed();

            if purge_result.is_ok() {
                info!(
                    "Repository database wipe worker completed for {} in {:.2}s",
                    thread_name,
                    elapsed.as_secs_f64()
                );
            } else if let Err(err) = &purge_result {
                warn!(
                    "Repository database wipe worker failed for {} in {:.2}s: {}",
                    thread_name,
                    elapsed.as_secs_f64(),
                    err
                );
            }

            if repository_db_wipe_tx
                .send(RepositoryDbWipeResult {
                    repository_url: thread_url,
                    local_path: thread_path,
                    repository_name: thread_name,
                    elapsed,
                    result: purge_result,
                    force_redownload_after_purge: false,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            } else {
                warn!("Failed to report repository database wipe completion");
            }
        });
    }

    fn next_cloned_repository_name(&self, original_name: &str) -> String {
        let trimmed = original_name.trim();
        let base_name = if trimmed.is_empty() {
            "Repository"
        } else {
            trimmed
        };

        let mut candidate = format!("{} Copy", base_name);
        let mut count = 2;
        while self
            .repository_view_state
            .repositories
            .iter()
            .any(|repo| repo.name.eq_ignore_ascii_case(&candidate))
        {
            candidate = format!("{} Copy {}", base_name, count);
            count += 1;
        }

        candidate
    }

    pub fn clone_repository_with_suffix(&mut self, repo_idx: usize) -> Option<usize> {
        let mut cloned = match self
            .repository_view_state
            .repositories
            .get(repo_idx)
            .cloned()
        {
            Some(repo) => repo,
            None => {
                warn!(
                    "Clone repository ignored: invalid repository index {}",
                    repo_idx
                );
                return None;
            }
        };

        cloned.name = self.next_cloned_repository_name(&cloned.name);
        let insert_idx = repo_idx + 1;
        self.repository_view_state
            .repositories
            .insert(insert_idx, cloned.clone());
        self.remap_repository_indices(|idx| {
            Some(Self::adjust_index_after_insertion(idx, insert_idx))
        });
        self.repository_view_state.selected_repository = Some(insert_idx);
        self.clear_completed_repository_check_banner_for_repo_change(Some(insert_idx));
        self.save_repositories();
        info!("Cloned repository as {}", cloned.name);

        Some(insert_idx)
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn open_editor_mission_folder(
        &self,
        mission_name: &str,
        mission_path: &Path,
    ) -> bool {
        if !mission_path.exists() {
            warn!(
                "Open editor mission folder ignored: path does not exist for {}: {}",
                mission_name,
                mission_path.display()
            );
            return false;
        }

        if !mission_path.is_dir() {
            warn!(
                "Open editor mission folder ignored: path is not a directory for {}: {}",
                mission_name,
                mission_path.display()
            );
            return false;
        }

        match std::process::Command::new("explorer")
            .arg(mission_path.as_os_str())
            .spawn()
        {
            Ok(_) => {
                info!("Opened editor mission folder for {}", mission_name);
                true
            }
            Err(err) => {
                warn!(
                    "Failed to open editor mission folder for {}: {}",
                    mission_name, err
                );
                false
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn open_editor_mission_folder(
        &self,
        mission_name: &str,
        mission_path: &Path,
    ) -> bool {
        if !mission_path.exists() {
            warn!(
                "Open editor mission folder ignored: path does not exist for {}: {}",
                mission_name,
                mission_path.display()
            );
            return false;
        }

        if !mission_path.is_dir() {
            warn!(
                "Open editor mission folder ignored: path is not a directory for {}: {}",
                mission_name,
                mission_path.display()
            );
            return false;
        }

        match crate::core::utils::platform::open_with_default_app(mission_path) {
            Ok(_) => {
                info!("Opened editor mission folder for {}", mission_name);
                true
            }
            Err(err) => {
                warn!(
                    "Failed to open editor mission folder for {}: {}",
                    mission_name, err
                );
                false
            }
        }
    }

    #[cfg(target_os = "windows")]
    pub fn open_repository_local_path(&self, repo_idx: usize) -> bool {
        let repo = match self.repository_view_state.repositories.get(repo_idx) {
            Some(repo) => repo,
            None => {
                warn!(
                    "Open local path ignored: invalid repository index {}",
                    repo_idx
                );
                return false;
            }
        };

        let path = repo.path.trim();
        if path.is_empty() {
            warn!(
                "Open local path ignored: repository {} has no local path",
                repo.name
            );
            return false;
        }

        let local_path = PathBuf::from(path);
        if !local_path.exists() {
            warn!(
                "Open local path ignored: repository path does not exist for {}: {}",
                repo.name, path
            );
            return false;
        }

        match std::process::Command::new("explorer")
            .arg(local_path.as_os_str())
            .spawn()
        {
            Ok(_) => {
                info!("Opened repository local path for {}", repo.name);
                true
            }
            Err(err) => {
                warn!(
                    "Failed to open repository local path for {}: {}",
                    repo.name, err
                );
                false
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn open_repository_local_path(&self, repo_idx: usize) -> bool {
        let repo = match self.repository_view_state.repositories.get(repo_idx) {
            Some(repo) => repo,
            None => {
                warn!(
                    "Open local path ignored: invalid repository index {}",
                    repo_idx
                );
                return false;
            }
        };

        let path = repo.path.trim();
        if path.is_empty() {
            warn!(
                "Open local path ignored: repository {} has no local path",
                repo.name
            );
            return false;
        }

        let local_path = PathBuf::from(path);
        if !local_path.exists() {
            warn!(
                "Open local path ignored: repository path does not exist for {}: {}",
                repo.name, path
            );
            return false;
        }

        match crate::core::utils::platform::open_with_default_app(&local_path) {
            Ok(_) => {
                info!("Opened repository local path for {}", repo.name);
                true
            }
            Err(err) => {
                warn!(
                    "Failed to open repository local path for {}: {}",
                    repo.name, err
                );
                false
            }
        }
    }

    #[cfg(target_os = "windows")]
    pub fn open_addon_directory(&self, addon_name: &str, addon_path: &str) -> bool {
        let path = addon_path.trim();
        if path.is_empty() {
            warn!(
                "Open addon directory ignored: addon {} has no resolved path",
                addon_name
            );
            return false;
        }

        let local_path = PathBuf::from(path);
        if !local_path.exists() {
            warn!(
                "Open addon directory ignored: path does not exist for addon {}",
                addon_name
            );
            return false;
        }

        if !local_path.is_dir() {
            warn!(
                "Open addon directory ignored: resolved path is not a directory for addon {}",
                addon_name
            );
            return false;
        }

        match std::process::Command::new("explorer")
            .arg(local_path.as_os_str())
            .spawn()
        {
            Ok(_) => {
                info!("Opened addon directory for {}", addon_name);
                true
            }
            Err(err) => {
                warn!("Failed to open addon directory for {}: {}", addon_name, err);
                false
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn open_addon_directory(&self, addon_name: &str, addon_path: &str) -> bool {
        let path = addon_path.trim();
        if path.is_empty() {
            warn!(
                "Open addon directory ignored: addon {} has no resolved path",
                addon_name
            );
            return false;
        }

        let local_path = PathBuf::from(path);
        if !local_path.exists() {
            warn!(
                "Open addon directory ignored: path does not exist for addon {}",
                addon_name
            );
            return false;
        }

        if !local_path.is_dir() {
            warn!(
                "Open addon directory ignored: resolved path is not a directory for addon {}",
                addon_name
            );
            return false;
        }

        match crate::core::utils::platform::open_with_default_app(&local_path) {
            Ok(_) => {
                info!("Opened addon directory for {}", addon_name);
                true
            }
            Err(err) => {
                warn!("Failed to open addon directory for {}: {}", addon_name, err);
                false
            }
        }
    }

    pub fn delete_repository_by_index(
        &mut self,
        repo_idx: usize,
        delete_local_files: bool,
    ) -> bool {
        if repo_idx >= self.repository_view_state.repositories.len() {
            warn!(
                "Delete repository ignored: invalid repository index {}",
                repo_idx
            );
            return false;
        }

        // The worker would otherwise keep hashing or downloading into a folder
        // nobody tracks any more, contending with the next sync for the disk.
        if self.syncing_repository == Some(repo_idx) {
            self.cancel_sync();
        }
        let files_kept =
            Self::delete_files_kept_reason(&self.repository_view_state.repositories, repo_idx);
        let removed = self.repository_view_state.repositories.remove(repo_idx);
        self.forget_startup_probe_unknown(&removed.address, &removed.path);
        let removed_visual_folder_key = Self::repo_instance_key(&removed.address, &removed.path);
        let mut visual_folders_changed = false;
        for folder in &mut self.repository_visual_folders {
            let before = folder.repository_keys.len();
            folder
                .repository_keys
                .retain(|key| key != &removed_visual_folder_key);
            visual_folders_changed |= folder.repository_keys.len() != before;
        }
        let normalized_url = Self::normalize_repo_url(&removed.address);
        self.clear_repo_state_for_url(&removed.address, &removed.path);
        let summary_notice_count = self.settings_view_state.update_summary_notices.len();
        self.settings_view_state
            .update_summary_notices
            .retain(|notice| notice.repository_url != normalized_url);
        if self.settings_view_state.update_summary_notices.len() != summary_notice_count {
            self.save_settings();
        }
        self.quick_scan_pending.remove(&normalized_url);
        self.pending_quick_scan_urls.remove(&normalized_url);
        self.pending_quick_scan_prevalidated_urls
            .remove(&normalized_url);
        self.pending_quick_scan_force_fresh_addon_hash_urls
            .remove(&normalized_url);
        self.repo_db_reset_pending_recheck.remove(&normalized_url);

        self.remap_repository_indices(|idx| Self::adjust_index_after_removal(Some(idx), repo_idx));
        self.clear_completed_repository_check_banner_for_repo_change(
            self.repository_view_state.selected_repository,
        );

        if let Some(progress) = self.repository_space_bulk_progress.as_mut()
            && removed.repository_space_id.as_deref() == Some(progress.space_id.as_str())
        {
            progress.remove_target(&normalized_url);
            if progress.total_count == 0 && progress.failed_count == 0 {
                self.repository_space_bulk_progress = None;
            }
        }

        self.pending_repository_context_confirmation = None;
        self.save_repositories();
        if visual_folders_changed {
            self.save_repository_visual_folders();
        }
        info!("Deleted repository {}", removed.name);
        if files_kept.is_none() {
            if delete_local_files {
                self.purge_deleted_repository_files_and_database(
                    &normalized_url,
                    &removed.name,
                    &removed.path,
                );
            } else {
                self.wipe_repository_database_entries_by_url(&normalized_url, &removed.name);
            }
        } else if files_kept == Some(DeleteFilesKept::SameAddressElsewhere) {
            // Another folder's copy keeps the URL's rows; this folder's own rows
            // would otherwise stay behind and be reused if the folder is re-added.
            info!(
                "Repository file purge skipped for {}: another configured repository uses the same URL; wiping this folder's database rows only",
                removed.name
            );
            self.wipe_repository_database_entries_by_url_and_path(
                &normalized_url,
                &removed.path,
                &removed.name,
                false,
            );
        } else {
            info!(
                "Repository purge skipped for {}: another configured repository uses the same URL and folder",
                removed.name
            );
        }
        let removed_message = self.t_fmt(
            "Repository removed: {name}",
            &[("name", removed.name.clone())],
        );
        self.show_success_toast(removed_message);
        true
    }

    fn purge_deleted_repository_files_and_database(
        &mut self,
        repository_url: &str,
        repo_name: &str,
        repo_path: &str,
    ) {
        let normalized_url = Self::normalize_repo_url(repository_url);

        if self.pending_repository_db_wipes.contains(&normalized_url) {
            debug!(
                "Repository file purge skipped for {} (already running)",
                repo_name
            );
            return;
        }

        info!(
            "Repository file and database purge requested for repository {}",
            repo_name
        );
        self.completed_repository_check_banner = None;
        self.completed_repository_db_wipe_banner = None;
        self.pending_repository_db_wipes
            .insert(normalized_url.clone());
        self.pending_repository_db_wipe_started_at
            .insert(normalized_url.clone(), Instant::now());
        self.needs_repaint = true;

        let thread_local_path = repo_path.to_string();
        let repo_path = if repo_path.trim().is_empty() {
            None
        } else {
            Some(sanitize_user_path(repo_path))
        };
        let repository_db_wipe_tx = self.repository_db_wipe_tx.clone();
        let thread_url = normalized_url;
        let thread_name = repo_name.to_string();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let started_at = Instant::now();
            let purge_result = match Runtime::new() {
                Ok(rt) => rt
                    .block_on(purge_repository_by_url(&thread_url, repo_path.as_deref()))
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            let elapsed = started_at.elapsed();

            if purge_result.is_ok() {
                info!(
                    "Repository file and database purge worker completed for {} in {:.2}s",
                    thread_name,
                    elapsed.as_secs_f64()
                );
            } else if let Err(err) = &purge_result {
                warn!(
                    "Repository file and database purge worker failed for {} in {:.2}s: {}",
                    thread_name,
                    elapsed.as_secs_f64(),
                    err
                );
            }

            if repository_db_wipe_tx
                .send(RepositoryDbWipeResult {
                    repository_url: thread_url,
                    local_path: thread_local_path,
                    repository_name: thread_name,
                    elapsed,
                    result: purge_result,
                    force_redownload_after_purge: false,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            } else {
                warn!("Failed to report repository file purge completion");
            }
        });
    }

    pub fn delete_repository_space_by_id(&mut self, space_id: &str) -> bool {
        let Some(space_idx) = self
            .repository_spaces
            .iter()
            .position(|space| space.id == space_id)
        else {
            warn!(
                "Delete repository space ignored: unknown repository space id {}",
                space_id
            );
            return false;
        };

        let removed = self.repository_spaces.remove(space_idx);
        for repo in &mut self.repository_view_state.repositories {
            if repo.repository_space_id.as_deref() == Some(space_id) {
                repo.repository_space_id = None;
                repo.repository_space_entry_address = None;
            }
        }

        if self.selected_repository_space_id.as_deref() == Some(space_id) {
            self.selected_repository_space_id = None;
        }

        if self
            .repository_space_selector_state
            .as_ref()
            .map(|selector| selector.space_id == space_id)
            .unwrap_or(false)
        {
            self.repository_space_selector_state = None;
        }
        if self
            .repository_space_settings_state
            .as_ref()
            .map(|settings| settings.space_id == space_id)
            .unwrap_or(false)
        {
            self.repository_space_settings_state = None;
        }
        if self
            .pending_repository_space_bulk_action
            .as_ref()
            .is_some_and(|action| action.space_id == space_id)
        {
            self.pending_repository_space_bulk_action = None;
        }
        if self
            .repository_space_bulk_progress
            .as_ref()
            .is_some_and(|progress| progress.space_id == space_id)
        {
            self.repository_space_bulk_progress = None;
        }
        self.repository_space_sync_queue
            .retain(|queued| queued.space_id != space_id);
        self.prune_repository_visual_folders(false);

        self.pending_repository_space_delete_id = None;
        self.save_repository_spaces();
        self.save_repository_visual_folders();
        self.save_repositories();
        info!("Deleted repository space {}", removed.name);
        let removed_message = self.t_fmt(
            "Repository space removed: {name}",
            &[("name", removed.name.clone())],
        );
        self.show_success_toast(removed_message);
        true
    }

    pub fn dismiss_completed_repository_check_banner(&mut self) {
        self.completed_repository_check_banner = None;
        self.completed_repository_db_wipe_banner = None;
    }

    pub fn clear_completed_repository_check_banner_for_repo_change(
        &mut self,
        next_repo: Option<usize>,
    ) {
        if self
            .completed_repository_check_banner
            .as_ref()
            .map(|banner| banner.repo_index)
            != next_repo
        {
            self.completed_repository_check_banner = None;
        }

        let next_repo_url = next_repo
            .and_then(|idx| self.repository_view_state.repositories.get(idx))
            .map(|repo| Self::normalize_repo_url(&repo.address));
        if self
            .completed_repository_db_wipe_banner
            .as_ref()
            .map(|banner| banner.repository_url.clone())
            != next_repo_url
        {
            self.completed_repository_db_wipe_banner = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(address: &str, path: &str) -> Repository {
        Repository {
            address: address.to_string(),
            path: path.to_string(),
            ..Repository::default()
        }
    }

    #[test]
    fn delete_files_is_kept_only_while_another_entry_uses_the_address() {
        let repositories = vec![
            repo("https://repo.example.invalid/main", "R:/Mods/Main"),
            repo("https://repo.example.invalid/main/", "R:/Mods/Copy"),
            repo("https://repo.example.invalid/alpha/", "R:/Mods/Main"),
            repo("https://repo.example.invalid/alpha/", "R:/Mods/Main"),
        ];
        assert_eq!(
            Foxy::delete_files_kept_reason(&repositories, 0),
            Some(DeleteFilesKept::SameAddressElsewhere)
        );
        assert_eq!(
            Foxy::delete_files_kept_reason(&repositories, 2),
            Some(DeleteFilesKept::SharedFolder)
        );
        assert_eq!(Foxy::delete_files_kept_reason(&repositories[2..3], 0), None);
        assert_eq!(Foxy::delete_files_kept_reason(&repositories, 9), None);
    }
}
