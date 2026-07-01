use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError as StdTryRecvError;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, info, warn};

use crate::core::api::{self, SyncMode};
use crate::ui::app::{Foxy, StartupQuickScanFilterResult};
use crate::ui::types::{RepoState, sanitize_user_path};

const FS_WATCH_DOWNLOAD_GRACE_MS: u64 = 3_000;

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

impl Foxy {
    pub(in crate::ui::app) fn suppress_fs_watch_for_active_download(&self) {
        self.fs_watch_suppressed_until_ms
            .store(u64::MAX, Ordering::Relaxed);
    }

    pub(in crate::ui::app) fn suppress_fs_watch_after_download(&self) {
        self.fs_watch_suppressed_until_ms.store(
            unix_time_millis().saturating_add(FS_WATCH_DOWNLOAD_GRACE_MS),
            Ordering::Relaxed,
        );
    }

    pub fn queue_startup_rechecks(&mut self) {
        self.startup_recheck_queue.clear();
        let mut candidates: Vec<(String, String, String)> = Vec::new();
        let mut skipped_incomplete = 0usize;
        let mut skipped_quick_scan = 0usize;
        let startup_quick_scan_active = !self.settings_view_state.debug_mode
            && self.settings_view_state.auto_quick_scan_on_launch;
        for repo in &self.repository_view_state.repositories {
            if !self.repo_auto_recheck_on_launch(repo) {
                continue;
            }
            if startup_quick_scan_active && self.repo_auto_quick_scan_on_launch(repo) {
                skipped_quick_scan += 1;
                continue;
            }
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                debug!(
                    "Skipping startup recheck for repo {} due to incomplete configuration",
                    repo.name
                );
                skipped_incomplete += 1;
                continue;
            }
            let normalized_url = Self::normalize_repo_url(&repo.address);
            candidates.push((
                repo.address.clone(),
                normalized_url,
                sanitize_user_path(&repo.path),
            ));
        }

        if candidates.is_empty() {
            info!(
                "Startup rechecks queued: 0 (incomplete_skipped={} quick_scan_skipped={})",
                skipped_incomplete, skipped_quick_scan
            );
            return;
        }

        let candidate_urls: Vec<String> = candidates
            .iter()
            .map(|(_, normalized, _)| normalized.clone())
            .collect();
        let existing_urls = api::filter_repo_urls_with_db_entry(candidate_urls);

        let mut queued = 0usize;
        let mut skipped_no_db = 0usize;
        for (address, normalized_url, path) in candidates {
            if !existing_urls.contains(&normalized_url) {
                debug!(
                    "Skipping startup recheck for {}: no prior database entry",
                    address
                );
                skipped_no_db += 1;
                continue;
            }
            self.startup_recheck_queue
                .push_back((address, path, SyncMode::RecheckOnly));
            queued += 1;
        }
        info!(
            "Startup rechecks queued: {} (incomplete_skipped={} no_db_entry_skipped={} quick_scan_skipped={})",
            queued, skipped_incomplete, skipped_no_db, skipped_quick_scan
        );
    }

    fn queue_startup_remote_refreshes(
        &mut self,
        repositories: Vec<api::StartupRepositoryInstance>,
    ) {
        let mut queued = 0usize;
        for repository in repositories {
            let normalized_url = Self::normalize_repo_url(&repository.repo_url);
            let local_path_key = Self::repo_instance_path_key(&repository.local_path);
            let Some(repo) = self.repository_view_state.repositories.iter().find(|repo| {
                Self::normalize_repo_url(&repo.address) == normalized_url
                    && Self::repo_instance_path_key(&repo.path) == local_path_key
            }) else {
                warn!(
                    "Startup remote refresh ignored for unknown repository {}",
                    normalized_url
                );
                continue;
            };
            if repo.path.trim().is_empty() {
                warn!(
                    "Startup remote refresh ignored for repository {} due to missing local path",
                    repo.name
                );
                continue;
            }
            let already_queued = self.startup_recheck_queue.iter().any(|(address, path, _)| {
                Self::normalize_repo_url(address) == normalized_url
                    && Self::repo_instance_path_key(path) == local_path_key
            });
            if already_queued {
                continue;
            }
            self.startup_recheck_queue.push_back((
                repo.address.clone(),
                sanitize_user_path(&repo.path),
                SyncMode::RemoteRefreshOnly,
            ));
            queued += 1;
        }
        if queued > 0 {
            info!("Startup remote refreshes queued: {}", queued);
        }
    }

    pub fn start_quick_local_scan(&mut self) {
        let mut requested_repositories = Vec::new();
        for repo in &self.repository_view_state.repositories {
            if !self.repo_auto_quick_scan_on_launch(repo) {
                continue;
            }
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                continue;
            }
            requested_repositories.push(api::StartupRepositoryInstance {
                repo_url: repo.address.clone(),
                local_path: repo.path.clone(),
            });
        }

        self.spawn_startup_quick_scan_filter(requested_repositories);
    }

    fn spawn_startup_quick_scan_filter(
        &mut self,
        requested_repositories: Vec<api::StartupRepositoryInstance>,
    ) {
        let mut normalized_requested_repositories = Vec::new();
        let mut seen = HashSet::new();
        for repository in requested_repositories {
            if repository.repo_url.trim().is_empty() || repository.local_path.trim().is_empty() {
                continue;
            }
            let normalized = api::StartupRepositoryInstance {
                repo_url: Self::normalize_repo_url(&repository.repo_url),
                local_path: Self::repo_instance_path_key(&repository.local_path),
            };
            if seen.insert(normalized.clone()) {
                normalized_requested_repositories.push(normalized);
            }
        }

        let requested = normalized_requested_repositories.len();
        if requested == 0 {
            info!("Scheduling startup quick scan for 0 of 0 repositories");
            return;
        }

        if self.startup_quick_scan_filter_worker.is_some() {
            info!(
                "Startup quick scan eligibility worker already active; queueing {} repositories without waiting",
                requested
            );
            self.queue_quick_scan_for_urls_with_flags(
                normalized_requested_repositories
                    .into_iter()
                    .map(|repository| repository.repo_url)
                    .collect(),
                false,
                false,
            );
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<StartupQuickScanFilterResult>();
        self.startup_quick_scan_filter_rx = Some(rx);
        let repaint_ctx = self.repaint_ctx.clone();
        info!(
            "Starting startup quick scan eligibility worker for {} repositories",
            requested
        );
        self.startup_quick_scan_filter_worker = Some(std::thread::spawn(move || {
            let requested_repositories = normalized_requested_repositories;
            let mut plan = api::plan_startup_quick_scan_repos(requested_repositories.clone());
            if requested > 0
                && plan.eligible_repositories.is_empty()
                && plan.remote_changed_repositories.is_empty()
            {
                info!(
                    "Startup quick scan eligibility returned 0 of {} repositories; falling back to requested set",
                    requested
                );
                plan.eligible_repositories = requested_repositories;
            }
            if tx
                .send(StartupQuickScanFilterResult {
                    requested_repositories: requested,
                    eligible_repositories: plan.eligible_repositories,
                    prevalidated_repositories: plan.prevalidated_repositories,
                    remote_changed_repositories: plan.remote_changed_repositories,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        }));
    }

    pub(in crate::ui::app) fn poll_startup_quick_scan_filter_results(&mut self) {
        let mut result = None;
        let mut disconnected = false;
        if let Some(rx) = self.startup_quick_scan_filter_rx.as_ref() {
            match rx.try_recv() {
                Ok(payload) => result = Some(payload),
                Err(StdTryRecvError::Empty) => {}
                Err(StdTryRecvError::Disconnected) => disconnected = true,
            }
        }

        if let Some(payload) = result {
            if !payload.remote_changed_repositories.is_empty() {
                info!(
                    "Scheduling startup remote refresh for {} repositories with changed remote checksums",
                    payload.remote_changed_repositories.len()
                );
                self.queue_startup_remote_refreshes(payload.remote_changed_repositories);
            }
            info!(
                "Scheduling startup quick scan for {} of {} repositories",
                payload.eligible_repositories.len(),
                payload.requested_repositories
            );
            let prevalidated_repositories: HashSet<api::StartupRepositoryInstance> =
                payload.prevalidated_repositories.into_iter().collect();
            let eligible_repositories = payload.eligible_repositories;
            if self.syncing_repository.is_none() && self.quick_scan_worker.is_none() {
                self.quick_scan_worker = Some(api::spawn_quick_local_scan_instances(
                    eligible_repositories,
                    prevalidated_repositories,
                    HashSet::new(),
                    self.quick_scan_tx.clone(),
                    self.repaint_ctx.clone(),
                ));
            } else {
                self.queue_quick_scan_for_urls_with_flags(
                    eligible_repositories
                        .into_iter()
                        .map(|repository| repository.repo_url)
                        .collect(),
                    false,
                    false,
                );
            }
            self.startup_quick_scan_filter_rx = None;
            self.needs_repaint = true;
        } else if disconnected {
            warn!("Startup quick scan eligibility worker disconnected before delivering results");
            self.startup_quick_scan_filter_rx = None;
        }

        let finished = self
            .startup_quick_scan_filter_worker
            .as_ref()
            .map(|worker| worker.is_finished())
            .unwrap_or(false);
        if finished {
            self.startup_quick_scan_filter_worker = None;
        }
    }

    pub fn queue_quick_scan_for_urls(&mut self, repo_urls: Vec<String>) {
        self.queue_quick_scan_for_urls_with_flags(repo_urls, false, false);
    }

    pub fn queue_quick_scan_for_urls_from_fs(&mut self, repo_urls: Vec<String>) {
        self.queue_quick_scan_for_urls_with_flags(repo_urls, false, true);
    }

    pub(in crate::ui::app) fn queue_quick_scan_for_urls_with_flags(
        &mut self,
        repo_urls: Vec<String>,
        prevalidated: bool,
        force_fresh_addon_hash: bool,
    ) {
        let mut newly_added = 0usize;
        for repo_url in repo_urls {
            if repo_url.trim().is_empty() {
                continue;
            }
            let normalized = Self::normalize_repo_url(&repo_url);
            if self.pending_quick_scan_urls.insert(normalized.clone()) {
                newly_added += 1;
            }
            if prevalidated {
                self.pending_quick_scan_prevalidated_urls
                    .insert(normalized.clone());
            } else {
                self.pending_quick_scan_prevalidated_urls
                    .remove(&normalized);
            }
            if force_fresh_addon_hash {
                self.pending_quick_scan_force_fresh_addon_hash_urls
                    .insert(normalized);
            }
        }

        if self.syncing_repository.is_some() {
            if newly_added > 0 {
                debug!(
                    "Deferring {} quick-scan repositories because sync is active",
                    newly_added
                );
            }
            return;
        }

        if self.quick_scan_worker.is_some() {
            if newly_added > 0 {
                debug!(
                    "Queued {} repositories for quick scan while worker is active",
                    newly_added
                );
            }
            return;
        }

        if self.pending_quick_scan_urls.is_empty() {
            return;
        }

        let repo_urls: Vec<String> = self.pending_quick_scan_urls.drain().collect();
        let prevalidated_repo_urls: HashSet<String> = repo_urls
            .iter()
            .filter(|url| self.pending_quick_scan_prevalidated_urls.remove(*url))
            .cloned()
            .collect();
        let force_fresh_addon_hash_repo_urls: HashSet<String> = repo_urls
            .iter()
            .filter(|url| {
                self.pending_quick_scan_force_fresh_addon_hash_urls
                    .remove(*url)
            })
            .cloned()
            .collect();
        info!(
            "Starting quick scan worker for {} repositories",
            repo_urls.len()
        );
        self.quick_scan_worker = Some(api::spawn_quick_local_scan(
            repo_urls,
            prevalidated_repo_urls,
            force_fresh_addon_hash_repo_urls,
            self.quick_scan_tx.clone(),
            self.repaint_ctx.clone(),
        ));
    }

    pub fn poll_quick_scan_results(&mut self) {
        while let Ok(result) = self.quick_scan_rx.try_recv() {
            if self.syncing_repository.is_some() {
                let deferred_repo_url = result.repo_url;
                self.pending_quick_scan_urls
                    .insert(deferred_repo_url.clone());
                self.pending_quick_scan_prevalidated_urls
                    .remove(&deferred_repo_url);
                debug!("Deferring quick scan result processing while sync is active");
                continue;
            }

            let local_path_key = Self::repo_instance_path_key(&result.local_path);
            let Some(idx) = self.repo_index_by_url_and_path(&result.repo_url, &local_path_key)
            else {
                warn!(
                    "Quick scan result received for unknown repository url {} (folder {:?})",
                    result.repo_url, result.local_path
                );
                continue;
            };
            let repo_path = self.repository_view_state.repositories[idx].path.clone();

            // When the scan was skipped (e.g. fresh DB, metadata not ready,
            // or local path mismatch), keep the repo in Unknown state rather
            // than marking it Synced.
            if result.skipped {
                info!(
                    "Quick scan skipped for repo {} (status remains unknown)",
                    self.repository_view_state.repositories[idx].name
                );
                self.quick_scan_pending.remove(&result.repo_url);
                self.needs_repaint = true;
                continue;
            }

            let has_updates = result.mods.iter().any(|m| m.needs_update);
            if !has_updates {
                info!(
                    "Quick scan clean for repo {}",
                    self.repository_view_state.repositories[idx].name
                );
                self.quick_scan_pending.remove(&result.repo_url);
                let repo_address = self.repository_view_state.repositories[idx].address.clone();
                self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                if self.update_ready_repo == Some(idx) {
                    self.update_ready_repo = None;
                    self.clear_mod_diff_cache();
                }
                if self
                    .completed_repository_check_banner
                    .as_ref()
                    .is_some_and(|banner| banner.repo_index == idx)
                {
                    self.completed_repository_check_banner = None;
                }
                self.set_repo_state_for_address(&repo_address, &repo_path, RepoState::Synced);
                self.clear_cached_pending_update(idx);
                self.needs_repaint = true;
                continue;
            }

            info!(
                "Quick scan pending update for repo {} (mods={})",
                self.repository_view_state.repositories[idx].name,
                result.mods.iter().filter(|m| m.needs_update).count()
            );
            let repo_address = self.repository_view_state.repositories[idx].address.clone();
            self.cache_pending_updates_for_url(&repo_address, &repo_path, result.mods.clone());
            self.set_repo_state_for_address(&repo_address, &repo_path, RepoState::PendingUpdate);
            self.quick_scan_pending.remove(&result.repo_url);

            if self.update_ready_repo.is_none() {
                self.update_ready_repo = Some(idx);
            }

            if self.repository_view_state.selected_repository == Some(idx) {
                self.set_mod_diff_cache(result.mods.clone());
                self.update_ready_repo = Some(idx);
            }

            self.needs_repaint = true;
        }

        let quick_scan_finished = match self.quick_scan_worker.as_ref() {
            Some(handle) => handle.is_finished(),
            None => false,
        };
        if quick_scan_finished {
            info!("Quick scan worker finished");
            self.quick_scan_worker = None;
            if self.syncing_repository.is_none() && !self.pending_quick_scan_urls.is_empty() {
                let repo_urls: Vec<String> = self.pending_quick_scan_urls.drain().collect();
                let prevalidated_repo_urls: HashSet<String> = repo_urls
                    .iter()
                    .filter(|url| self.pending_quick_scan_prevalidated_urls.remove(*url))
                    .cloned()
                    .collect();
                let force_fresh_addon_hash_repo_urls: HashSet<String> = repo_urls
                    .iter()
                    .filter(|url| {
                        self.pending_quick_scan_force_fresh_addon_hash_urls
                            .remove(*url)
                    })
                    .cloned()
                    .collect();
                info!(
                    "Restarting quick scan worker for {} queued repositories",
                    repo_urls.len()
                );
                self.quick_scan_worker = Some(api::spawn_quick_local_scan(
                    repo_urls,
                    prevalidated_repo_urls,
                    force_fresh_addon_hash_repo_urls,
                    self.quick_scan_tx.clone(),
                    self.repaint_ctx.clone(),
                ));
            }
        }
    }

    pub fn start_fs_watcher(&mut self) {
        if self.fs_watch_worker.is_some() {
            debug!("Filesystem watcher is already running");
            return;
        }

        let mut watch_paths = HashSet::new();
        for repo in &self.repository_view_state.repositories {
            let folder = repo.path.trim();
            if folder.is_empty() {
                continue;
            }
            watch_paths.insert(folder.to_string());
        }

        if watch_paths.is_empty() {
            info!("Filesystem watcher not started: no repository folders configured");
            return;
        }

        info!(
            "Starting filesystem watcher for {} paths",
            watch_paths.len()
        );
        self.fs_watch_worker = Some(api::spawn_repo_fs_watcher(
            watch_paths.into_iter().collect(),
            self.fs_watch_suppressed_until_ms.clone(),
            self.fs_watch_tx.clone(),
            self.repaint_ctx.clone(),
        ));
    }

    /// Keep only the filesystem-watch repository URLs whose auto quick-scan
    /// setting is enabled, honoring the per-repository override and the global
    /// default via [`Self::repo_auto_quick_scan_on_launch`].
    ///
    /// The watcher observes every configured repository folder regardless of
    /// settings, so this is the authoritative gate that keeps a disabled
    /// auto quick-scan from being re-triggered by on-disk changes. Without it
    /// the setting only governed the one-shot scan at startup and the watcher
    /// kept scanning anyway.
    fn fs_watch_repo_urls_with_quick_scan_enabled(&self, repo_urls: Vec<String>) -> Vec<String> {
        repo_urls
            .into_iter()
            .filter(|repo_url| {
                let normalized = Self::normalize_repo_url(repo_url);
                self.repository_view_state
                    .repositories
                    .iter()
                    .find(|repo| Self::normalize_repo_url(&repo.address) == normalized)
                    .is_some_and(|repo| self.repo_auto_quick_scan_on_launch(repo))
            })
            .collect()
    }

    pub fn poll_fs_watch_results(&mut self) {
        while let Ok(event) = self.fs_watch_rx.try_recv() {
            if event.repo_urls.is_empty() {
                continue;
            }

            // The watcher reports changes for every configured repository, but
            // automatic quick scans must respect the auto quick-scan setting
            // (global default and per-repository override). Drop repositories
            // whose quick scan is disabled before scheduling any work, so the
            // setting holds for change-triggered scans and not just the
            // one-shot scan at startup.
            let repo_urls = self.fs_watch_repo_urls_with_quick_scan_enabled(event.repo_urls);
            if repo_urls.is_empty() {
                continue;
            }

            // Drop repositories whose purge / forced-redownload is still running.
            // The purge deletes local files on a background runtime while holding a
            // large DB write transaction - and those very deletions are what
            // tripped this watcher event. Starting a quick scan now races the
            // purge's transaction against the same database from a second runtime,
            // which can wedge it indefinitely. The forced download is kicked off
            // explicitly once the purge result is consumed, so skipping here loses
            // nothing.
            let repo_urls: Vec<String> = repo_urls
                .into_iter()
                .filter(|repo_url| {
                    let normalized = Self::normalize_repo_url(repo_url);
                    if self.pending_repository_db_wipes.contains(&normalized) {
                        debug!(
                            "Ignoring filesystem-triggered quick scan for {} during repository purge/redownload",
                            normalized
                        );
                        false
                    } else {
                        true
                    }
                })
                .collect();
            if repo_urls.is_empty() {
                continue;
            }

            if self.syncing_repository.is_some() {
                if self.current_sync_mode == Some(SyncMode::Download) {
                    debug!(
                        "Ignoring filesystem-triggered quick scan for {} repositories during download",
                        repo_urls.len()
                    );
                    continue;
                }
                debug!(
                    "Deferring filesystem-triggered quick scan for {} repositories during sync",
                    repo_urls.len()
                );
                for url in repo_urls {
                    self.deferred_fs_scan.insert(url);
                }
                continue;
            }

            debug!(
                "Filesystem watcher queued quick scan for {} repositories",
                repo_urls.len()
            );
            self.queue_quick_scan_for_urls_from_fs(repo_urls);
        }
    }

    pub fn process_startup_rechecks(&mut self) {
        if self.syncing_repository.is_some() || self.quick_scan_worker.is_some() {
            return;
        }
        while let Some((address, path, mode)) = self.startup_recheck_queue.pop_front() {
            let repo_index = self
                .repository_view_state
                .repositories
                .iter()
                .position(|repo| repo.address == address && sanitize_user_path(&repo.path) == path);
            let Some(idx) = repo_index else {
                warn!("Startup recheck target repository no longer exists");
                continue;
            };
            if mode == SyncMode::RecheckOnly
                && !self.repo_auto_recheck_on_launch(&self.repository_view_state.repositories[idx])
            {
                continue;
            }
            // A download that just completed already fully synced this repo;
            // starting a recheck now would also wipe the finished-download
            // summary still shown in the update modal.
            if self.download_finished && self.download_finished_repo == Some(idx) {
                info!(
                    "Skipping startup {:?} for repository {}: download just completed",
                    mode, self.repository_view_state.repositories[idx].name
                );
                continue;
            }
            self.start_core_sync(idx, mode);
            if self.syncing_repository.is_some() {
                info!(
                    "Started startup {:?} for repository {}",
                    mode, self.repository_view_state.repositories[idx].name
                );
                break;
            }
        }
    }
}
