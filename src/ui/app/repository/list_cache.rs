use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::mpsc::TryRecvError as StdTryRecvError;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{debug, error, info, warn};
use rand::{RngExt, distr::Alphanumeric, rng};

use crate::core::api::ModDiffSummary;
use crate::core::models::pending_update::{
    clear_pending_update_payload_for_path, load_pending_update_payload_for_path,
};
use crate::core::models::repository::is_repository_foxy;
use crate::ui::app::{
    CachedUpdateLoadOutcome, CachedUpdateLoadResult, Foxy, StartupPendingUpdateRestoreRecord,
    StartupPendingUpdateRestoreRequest,
};
use crate::ui::types::{RepoState, Repository, normalize_loaded_repositories};

impl Foxy {
    pub(in crate::ui::app) fn bump_repository_list_data_version(&mut self) {
        self.repository_list_data_version = self.repository_list_data_version.wrapping_add(1);
        if self.repository_list_data_version == 0 {
            self.repository_list_data_version = 1;
        }
    }

    pub(in crate::ui::app) fn bump_repository_spaces_version(&mut self) {
        self.repository_spaces_version = self.repository_spaces_version.wrapping_add(1);
        if self.repository_spaces_version == 0 {
            self.repository_spaces_version = 1;
        }
    }

    pub(in crate::ui::app) fn bump_repository_visual_folders_version(&mut self) {
        self.repository_visual_folders_version =
            self.repository_visual_folders_version.wrapping_add(1);
        if self.repository_visual_folders_version == 0 {
            self.repository_visual_folders_version = 1;
        }
    }

    pub fn load_repositories(&mut self) {
        let path = Self::get_repositories_path();
        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<Vec<Repository>>(&json) {
                Ok(mut repos) => {
                    normalize_loaded_repositories(&mut repos);

                    if !self.settings_view_state.debug_mode {
                        let before = repos.len();
                        repos.retain(|repo| !Self::is_generated_debug_repository(repo));
                        let removed = before - repos.len();
                        if removed > 0 {
                            warn!(
                                "Filtered {} synthetic debug repositories loaded from repositories.json",
                                removed
                            );
                        }
                    }

                    self.repository_view_state.repositories = repos;
                    self.bump_repository_list_data_version();
                    debug!(
                        "Loaded repositories.json with {} repositories",
                        self.repository_view_state.repositories.len()
                    );
                }
                Err(err) => {
                    error!("Failed to parse repositories.json: {}", err);
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => {
                info!("repositories.json not found, using empty repository list");
                self.repository_view_state.repositories.clear();
                self.bump_repository_list_data_version();
            }
            Err(err) => {
                error!("Failed to read repositories.json: {}", err);
            }
        }
    }

    pub fn save_repositories(&mut self) {
        self.bump_repository_list_data_version();
        self.mark_repositories_dirty();
    }

    /// Clear persisted pending-update cache for a repository. The DB write
    /// runs on a background thread to avoid blocking the UI draw loop with
    /// runtime creation and synchronous I/O.
    pub fn clear_cached_pending_update(&self, repo_index: usize) {
        if let Some(repo) = self.repository_view_state.repositories.get(repo_index) {
            let normalized_url = Self::normalize_repo_url(&repo.address);
            let local_path = repo.path.clone();
            let repo_name = repo.name.clone();
            std::thread::spawn(move || {
                let Some(rt) = crate::core::api::background_runtime() else {
                    log::error!(
                        "Shared background runtime unavailable for clearing pending updates"
                    );
                    return;
                };
                if let Err(err) = rt.block_on(clear_pending_update_payload_for_path(
                    &normalized_url,
                    &local_path,
                )) {
                    log::error!(
                        "Failed to clear cached pending updates for {}: {}",
                        repo_name,
                        err
                    );
                }
            });
        }
    }

    /// Canonicalize a download folder for instance-scoped status keys. Matches
    /// the core (`content_hash::normalize_path`) so the UI agrees with the
    /// `local_path` carried on quick-scan results and pending-update rows.
    pub(crate) fn repo_instance_path_key(local_path: &str) -> String {
        crate::core::utils::content_hash::normalize_path(local_path)
    }

    /// Composite key for the per-instance status maps (`repo_states`,
    /// `pending_update_cache`). The same remote URL installed in different
    /// folders is tracked independently, so an empty new-folder install no
    /// longer inherits a sibling's "complete" status.
    pub(crate) fn repo_instance_key(repository_url: &str, local_path: &str) -> String {
        format!(
            "{}\u{1f}{}",
            Self::normalize_repo_url(repository_url),
            Self::repo_instance_path_key(local_path)
        )
    }

    pub fn repo_state_for_address(&self, repository_url: &str, local_path: &str) -> RepoState {
        self.repo_states
            .get(&Self::repo_instance_key(repository_url, local_path))
            .copied()
            .unwrap_or(RepoState::Unknown)
    }

    /// State-filter tag describing whether the repository instance has been
    /// downloaded. A repository whose sync state is still
    /// [`RepoState::Unknown`] (never checked/downloaded) counts as "not
    /// installed"; any other state counts as "installed".
    pub fn repo_installed_state_tag(&self, repository_url: &str, local_path: &str) -> &'static str {
        if matches!(
            self.repo_state_for_address(repository_url, local_path),
            RepoState::Unknown
        ) {
            crate::ui::search_filter::STATE_KEYWORD_NOT_INSTALLED
        } else {
            crate::ui::search_filter::STATE_KEYWORD_INSTALLED
        }
    }

    pub(in crate::ui::app) fn set_repo_state_for_address(
        &mut self,
        repository_url: &str,
        local_path: &str,
        state: RepoState,
    ) {
        let key = Self::repo_instance_key(repository_url, local_path);
        if self.repo_states.get(&key) != Some(&state) {
            self.repo_states_version = self.repo_states_version.wrapping_add(1);
        }
        self.repo_states.insert(key, state);
    }

    pub fn repo_foxy_mode_for_address(&self, repository_url: &str) -> Option<bool> {
        self.repo_foxy_modes
            .get(&Self::normalize_repo_url(repository_url))
            .copied()
    }

    pub(in crate::ui::app) fn set_repo_foxy_mode_for_address(
        &mut self,
        repository_url: &str,
        is_foxy: bool,
    ) {
        self.repo_foxy_modes
            .insert(Self::normalize_repo_url(repository_url), is_foxy);
    }

    pub(in crate::ui::app) fn cache_pending_updates_for_url(
        &mut self,
        repository_url: &str,
        local_path: &str,
        mods: Vec<ModDiffSummary>,
    ) {
        let normalized_url = Self::normalize_repo_url(repository_url);
        let instance_key = Self::repo_instance_key(repository_url, local_path);
        let filtered: Vec<ModDiffSummary> = mods.into_iter().filter(|m| m.needs_update).collect();
        if filtered.is_empty() {
            self.pending_update_cache.remove(&instance_key);
            self.clear_active_update_session_for_url(&normalized_url);
        } else {
            self.pending_update_cache.insert(instance_key, filtered);
        }
    }

    pub(in crate::ui::app) fn clear_pending_update_cache_for_url(
        &mut self,
        repository_url: &str,
        local_path: &str,
    ) {
        let instance_key = Self::repo_instance_key(repository_url, local_path);
        self.pending_update_cache.remove(&instance_key);
    }

    pub(crate) fn active_update_session_mods_for_url(
        &self,
        repository_url: &str,
    ) -> Option<Vec<ModDiffSummary>> {
        let normalized_url = Self::normalize_repo_url(repository_url);
        self.settings_view_state
            .active_update_sessions
            .iter()
            .find(|session| session.repository_url == normalized_url)
            .map(|session| session.mods.clone())
    }

    pub(crate) fn ensure_active_update_session_for_url(
        &mut self,
        repository_url: &str,
        mods: &[ModDiffSummary],
    ) {
        if mods.is_empty() {
            return;
        }

        let normalized_url = Self::normalize_repo_url(repository_url);
        if self
            .settings_view_state
            .active_update_sessions
            .iter()
            .any(|session| session.repository_url == normalized_url)
        {
            return;
        }

        let session_id = update_session_id();
        self.settings_view_state.active_update_sessions.push(
            crate::ui::types::ActiveUpdateSession {
                repository_url: normalized_url.clone(),
                session_id: session_id.clone(),
                mods: mods.to_vec(),
            },
        );
        self.save_settings();
        info!(
            "Created active update session: repo={} session={} mods={}",
            normalized_url,
            session_id,
            mods.iter().filter(|m| m.needs_update).count()
        );
    }

    pub(crate) fn clear_active_update_session_for_url(&mut self, repository_url: &str) {
        let normalized_url = Self::normalize_repo_url(repository_url);
        let previous_len = self.settings_view_state.active_update_sessions.len();
        self.settings_view_state
            .active_update_sessions
            .retain(|session| session.repository_url != normalized_url);
        if self.settings_view_state.active_update_sessions.len() != previous_len {
            self.save_settings();
        }
    }

    pub fn pending_update_count_for_address(
        &self,
        repository_url: &str,
        local_path: &str,
    ) -> usize {
        let instance_key = Self::repo_instance_key(repository_url, local_path);
        self.pending_update_cache
            .get(&instance_key)
            .map(|mods| mods.len())
            .unwrap_or_default()
    }

    pub(crate) fn apply_pending_update_cache_for_repo(&mut self, repo_index: usize) -> bool {
        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            return false;
        };

        let normalized_url = Self::normalize_repo_url(&repo.address);
        let instance_key = Self::repo_instance_key(&repo.address, &repo.path);
        let Some(mods) = self.pending_update_cache.get(&instance_key).cloned() else {
            return false;
        };

        if mods.is_empty() {
            return false;
        }

        self.update_ready_repo = Some(repo_index);
        if let Some(session_mods) = self.active_update_session_mods_for_url(&normalized_url) {
            self.mod_diff_cache = merge_download_mod_diffs_preserving_finished(
                session_mods,
                mods,
                &mut self.mod_download_progress,
            );
            self.invalidate_update_modal_sort_cache();
        } else {
            self.set_mod_diff_cache(mods);
        }
        true
    }

    pub(crate) fn invalidate_update_modal_sort_cache(&mut self) {
        self.update_modal_sort_generation = self.update_modal_sort_generation.wrapping_add(1);
    }

    pub(crate) fn clear_mod_diff_cache(&mut self) {
        if self.mod_diff_cache.is_empty() {
            return;
        }
        self.mod_diff_cache.clear();
        self.invalidate_update_modal_sort_cache();
    }

    pub(crate) fn set_mod_diff_cache(&mut self, mods: Vec<ModDiffSummary>) {
        self.mod_diff_cache = mods;
        self.invalidate_update_modal_sort_cache();
    }

    pub(crate) fn set_download_mod_diff_cache_preserving_finished(
        &mut self,
        active_mods: Vec<ModDiffSummary>,
    ) {
        let merged = merge_download_mod_diffs_preserving_finished(
            std::mem::take(&mut self.mod_diff_cache),
            active_mods,
            &mut self.mod_download_progress,
        );
        self.mod_diff_cache = merged;
        self.invalidate_update_modal_sort_cache();
    }

    pub(crate) fn summarize_pending_mod_updates(mods: &[ModDiffSummary]) -> (usize, usize, usize) {
        let mut mods_updated = 0usize;
        let mut files_updated = 0usize;
        let mut parts_updated = 0usize;
        for m in mods {
            if !m.needs_update {
                continue;
            }
            mods_updated += 1;
            files_updated += m.files.len();
            parts_updated += m.files.iter().map(|f| f.changed_parts).sum::<usize>();
        }
        (mods_updated, files_updated, parts_updated)
    }

    pub(crate) fn download_sort_rank(percent: Option<f32>) -> u8 {
        match percent {
            Some(pct) if pct < 1.0 => 0,
            Some(pct) if (pct - 1.0).abs() < f32::EPSILON => 2,
            Some(_) => 1,
            None => 1,
        }
    }

    pub(crate) fn mod_download_sort_rank(&self, mod_name: &str) -> u8 {
        let pct = self
            .mod_download_progress
            .get(mod_name)
            .map(|(percent, ..)| *percent);
        Self::download_sort_rank(pct)
    }

    pub fn restore_pending_updates(&mut self) {
        if self.startup_pending_restore_worker.is_some() {
            debug!("Startup pending-update restore worker is already running");
            return;
        }

        let requests: Vec<StartupPendingUpdateRestoreRequest> = self
            .repository_view_state
            .repositories
            .iter()
            .enumerate()
            .map(|(repo_index, repo)| StartupPendingUpdateRestoreRequest {
                repo_index,
                repo_name: repo.name.clone(),
                repo_url: Self::normalize_repo_url(&repo.address),
                repo_path: repo.path.clone(),
                verify_with_quick_scan: self.repo_auto_quick_scan_on_launch(repo),
            })
            .collect();

        let (tx, rx) = std::sync::mpsc::channel::<Vec<StartupPendingUpdateRestoreRecord>>();
        self.startup_pending_restore_rx = Some(rx);
        let repaint_ctx = self.repaint_ctx.clone();
        info!(
            "Starting startup pending-update restore worker for {} repositories",
            requests.len()
        );
        self.startup_pending_restore_worker = Some(std::thread::spawn(move || {
            let Some(runtime) = crate::core::api::background_runtime() else {
                log::error!("Shared background runtime unavailable for cached updates");
                return;
            };

            let mut restored = Vec::with_capacity(requests.len());
            for request in requests {
                let is_foxy =
                    runtime.block_on(is_repository_foxy(&request.repo_url, &request.repo_path));
                match runtime.block_on(load_pending_update_payload_for_path(
                    &request.repo_url,
                    &request.repo_path,
                )) {
                    Ok(Some(payload)) => {
                        match serde_json::from_str::<Vec<ModDiffSummary>>(&payload) {
                            Ok(mods) => {
                                if mods.iter().any(|m| m.needs_update) {
                                    info!(
                                        "Startup pending update cached for repo {} (mods={})",
                                        request.repo_name,
                                        mods.iter().filter(|m| m.needs_update).count()
                                    );
                                    restored.push(StartupPendingUpdateRestoreRecord {
                                        repo_index: request.repo_index,
                                        repo_url: request.repo_url,
                                        state: RepoState::PendingUpdate,
                                        mods: Some(mods),
                                        verify_with_quick_scan: request.verify_with_quick_scan,
                                        is_foxy,
                                    });
                                } else {
                                    if let Err(err) =
                                        runtime.block_on(clear_pending_update_payload_for_path(
                                            &request.repo_url,
                                            &request.repo_path,
                                        ))
                                    {
                                        log::error!(
                                            "Failed to clear stale cached pending updates for {}: {}",
                                            request.repo_name,
                                            err
                                        );
                                    }
                                    restored.push(StartupPendingUpdateRestoreRecord {
                                        repo_index: request.repo_index,
                                        repo_url: request.repo_url,
                                        state: RepoState::Synced,
                                        mods: None,
                                        verify_with_quick_scan: false,
                                        is_foxy,
                                    });
                                }
                            }
                            Err(err) => {
                                log::error!(
                                    "Failed to parse cached pending updates for {}: {}",
                                    request.repo_name,
                                    err
                                );
                                if let Err(clear_err) =
                                    runtime.block_on(clear_pending_update_payload_for_path(
                                        &request.repo_url,
                                        &request.repo_path,
                                    ))
                                {
                                    log::error!(
                                        "Failed to clear invalid cached updates for {}: {}",
                                        request.repo_name,
                                        clear_err
                                    );
                                }
                                restored.push(StartupPendingUpdateRestoreRecord {
                                    repo_index: request.repo_index,
                                    repo_url: request.repo_url,
                                    state: RepoState::Unknown,
                                    mods: None,
                                    verify_with_quick_scan: false,
                                    is_foxy,
                                });
                            }
                        }
                    }
                    Ok(None) => restored.push(StartupPendingUpdateRestoreRecord {
                        repo_index: request.repo_index,
                        repo_url: request.repo_url,
                        state: RepoState::Unknown,
                        mods: None,
                        verify_with_quick_scan: false,
                        is_foxy,
                    }),
                    Err(err) => {
                        log::error!(
                            "Failed to read cached pending updates for {}: {}",
                            request.repo_name,
                            err
                        );
                        restored.push(StartupPendingUpdateRestoreRecord {
                            repo_index: request.repo_index,
                            repo_url: request.repo_url,
                            state: RepoState::Unknown,
                            mods: None,
                            verify_with_quick_scan: false,
                            is_foxy,
                        });
                    }
                }
            }

            if tx.send(restored).is_ok() {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        }));
    }

    pub(in crate::ui::app) fn poll_restore_pending_updates(&mut self) {
        let mut restored = None;
        let mut disconnected = false;
        if let Some(rx) = self.startup_pending_restore_rx.as_ref() {
            match rx.try_recv() {
                Ok(payload) => restored = Some(payload),
                Err(StdTryRecvError::Empty) => {}
                Err(StdTryRecvError::Disconnected) => disconnected = true,
            }
        }

        if let Some(records) = restored {
            let mut verify_urls = Vec::new();
            let mut first_pending: Option<(usize, Vec<ModDiffSummary>)> = None;
            for record in records {
                let Some((repo_address, repo_path)) = self
                    .repository_view_state
                    .repositories
                    .get(record.repo_index)
                    .map(|repo| (repo.address.clone(), repo.path.clone()))
                else {
                    continue;
                };
                let normalized_url = Self::normalize_repo_url(&repo_address);
                if normalized_url != record.repo_url {
                    continue;
                }

                let state =
                    if record.state == RepoState::PendingUpdate && record.verify_with_quick_scan {
                        RepoState::Unknown
                    } else {
                        record.state
                    };
                self.set_repo_state_for_address(&repo_address, &repo_path, state);
                if let Some(is_foxy) = record.is_foxy {
                    self.set_repo_foxy_mode_for_address(&repo_address, is_foxy);
                }
                if record.state == RepoState::PendingUpdate {
                    if record.verify_with_quick_scan {
                        self.quick_scan_pending.insert(normalized_url.clone());
                        verify_urls.push(normalized_url);
                    } else {
                        if let Some(mods) = record.mods.clone() {
                            self.cache_pending_updates_for_url(
                                &repo_address,
                                &repo_path,
                                mods.clone(),
                            );
                        }
                        if self.update_ready_repo.is_none()
                            && first_pending.is_none()
                            && let Some(mods) = record.mods
                        {
                            first_pending = Some((record.repo_index, mods));
                        }
                    }
                } else {
                    self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                }
            }

            let repo_instances: Vec<(String, String)> = self
                .repository_view_state
                .repositories
                .iter()
                .map(|repo| (repo.address.clone(), repo.path.clone()))
                .collect();
            for (repo_address, repo_path) in repo_instances {
                let key = Self::repo_instance_key(&repo_address, &repo_path);
                if !self.repo_states.contains_key(&key) {
                    self.set_repo_state_for_address(&repo_address, &repo_path, RepoState::Unknown);
                }
            }

            if self.update_ready_repo.is_none()
                && let Some((idx, mods)) = first_pending
            {
                self.update_ready_repo = Some(idx);
                self.set_mod_diff_cache(mods);
            }

            if !verify_urls.is_empty() {
                info!(
                    "Startup quick scan verify queued for {} repos",
                    verify_urls.len()
                );
                self.queue_quick_scan_for_urls_with_flags(verify_urls, false, false);
            }

            self.startup_pending_restore_rx = None;
            self.needs_repaint = true;
        } else if disconnected {
            warn!("Startup pending-update restore worker disconnected before delivering results");
            self.startup_pending_restore_rx = None;
        }

        let finished = self
            .startup_pending_restore_worker
            .as_ref()
            .map(|worker| worker.is_finished())
            .unwrap_or(false);
        if finished {
            self.startup_pending_restore_worker = None;
        }
    }

    /// Find a repository's index by its normalized URL. Used to re-resolve a
    /// repository after a background task completes, since the list may have
    /// changed while the task was in flight.
    pub(crate) fn repo_index_by_normalized_url(&self, normalized_url: &str) -> Option<usize> {
        self.repository_view_state
            .repositories
            .iter()
            .position(|repo| Self::normalize_repo_url(&repo.address) == normalized_url)
    }

    /// Resolve a repository instance by both its normalized URL and download
    /// folder. `local_path_key` must already be normalized via
    /// [`Self::repo_instance_path_key`]. Falls back to a URL-only match when no
    /// instance shares the folder (e.g. a result that predates a folder change),
    /// so a single-instance repository keeps working.
    pub(crate) fn repo_index_by_url_and_path(
        &self,
        normalized_url: &str,
        local_path_key: &str,
    ) -> Option<usize> {
        self.repository_view_state
            .repositories
            .iter()
            .position(|repo| {
                Self::normalize_repo_url(&repo.address) == normalized_url
                    && Self::repo_instance_path_key(&repo.path) == local_path_key
            })
            .or_else(|| self.repo_index_by_normalized_url(normalized_url))
    }

    pub fn load_cached_updates_for_repo(&mut self, repo_index: usize) {
        self.dispatch_cached_update_load(repo_index, false);
    }

    /// Like [`Self::load_cached_updates_for_repo`], but once a background load
    /// finds pending updates it also opens the repository update modal. Used by
    /// the pending-updates banner action so a slow database read never blocks.
    pub fn load_cached_updates_for_repo_and_open_modal(&mut self, repo_index: usize) {
        self.dispatch_cached_update_load(repo_index, true);
    }

    /// Resolve a repository's pending-update state. In-memory fast paths run
    /// synchronously; a database miss is loaded on a background thread (see
    /// [`Self::poll_cached_update_load_results`]) so the SQLite read never
    /// blocks the UI draw loop.
    fn dispatch_cached_update_load(&mut self, repo_index: usize, open_modal_when_pending: bool) {
        let repo = match self
            .repository_view_state
            .repositories
            .get(repo_index)
            .cloned()
        {
            Some(r) => r,
            None => return,
        };

        let normalized_url = Self::normalize_repo_url(&repo.address);
        let repo_path = repo.path.clone();
        if self.quick_scan_pending.contains(&normalized_url) {
            self.set_repo_state_for_address(&repo.address, &repo_path, RepoState::Unknown);
            return;
        }

        // Fast path: the pending updates are already cached in memory.
        if self.apply_pending_update_cache_for_repo(repo_index) {
            self.set_repo_state_for_address(&repo.address, &repo_path, RepoState::PendingUpdate);
            if open_modal_when_pending {
                self.open_pending_update_modal_for_repo(repo_index);
            }
            return;
        }

        // A load for this repository is already in flight; avoid a duplicate read.
        if !self
            .pending_cached_update_loads
            .insert(normalized_url.clone())
        {
            return;
        }

        let tx = self.cached_update_load_result_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        let worker_url = normalized_url;
        let worker_path = repo_path;
        std::thread::spawn(move || {
            let outcome = load_cached_update_outcome(&worker_url, &worker_path);
            if tx
                .send(CachedUpdateLoadResult {
                    repo_url: worker_url,
                    local_path: worker_path,
                    open_modal_when_pending,
                    outcome,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });
    }

    /// Open the repository update modal for `repo_index` if it currently has
    /// pending updates ready in `mod_diff_cache`. The download plan (queue +
    /// exact estimate) is prepared up front by the remote recheck, so this
    /// opens the modal directly instead of running a second redundant recheck.
    pub(crate) fn open_pending_update_modal_for_repo(&mut self, repo_index: usize) {
        let has_updates = self.update_ready_repo == Some(repo_index)
            && self.mod_diff_cache.iter().any(|m| m.needs_update);
        let repo_name = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .map(|repo| repo.name.clone())
            .unwrap_or_default();
        if has_updates {
            self.direct_download_update_view = false;
            self.update_modal_open = true;
            info!("Opened pending update modal for {}", repo_name);
        } else {
            warn!(
                "Pending updates requested for {} but no pending payload was available",
                repo_name
            );
        }
    }

    /// Drain completed background cached pending-update loads and apply them on
    /// the UI thread, preserving the original loader's state transitions.
    pub(in crate::ui::app) fn poll_cached_update_load_results(&mut self) {
        while let Ok(result) = self.cached_update_load_result_rx.try_recv() {
            let CachedUpdateLoadResult {
                repo_url,
                local_path,
                open_modal_when_pending,
                outcome,
            } = result;
            self.pending_cached_update_loads.remove(&repo_url);

            // The list may have changed while the load was in flight; re-resolve
            // by URL + folder and discard the result if the repository is gone.
            let local_path_key = Self::repo_instance_path_key(&local_path);
            let Some(repo_index) = self.repo_index_by_url_and_path(&repo_url, &local_path_key)
            else {
                continue;
            };
            let (repo_address, repo_path) = {
                let repo = &self.repository_view_state.repositories[repo_index];
                (repo.address.clone(), repo.path.clone())
            };

            match outcome {
                CachedUpdateLoadOutcome::Pending(mods) => {
                    self.cache_pending_updates_for_url(&repo_address, &repo_path, mods);
                    if self.apply_pending_update_cache_for_repo(repo_index) {
                        self.set_repo_state_for_address(
                            &repo_address,
                            &repo_path,
                            RepoState::PendingUpdate,
                        );
                    }
                    if open_modal_when_pending {
                        self.open_pending_update_modal_for_repo(repo_index);
                    }
                }
                CachedUpdateLoadOutcome::Synced => {
                    if self.update_ready_repo == Some(repo_index) {
                        self.update_ready_repo = None;
                        self.clear_mod_diff_cache();
                    }
                    self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                    self.set_repo_state_for_address(&repo_address, &repo_path, RepoState::Synced);
                }
                CachedUpdateLoadOutcome::NoPayload => {
                    self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                    if self.update_ready_repo == Some(repo_index) {
                        self.update_ready_repo = None;
                        self.clear_mod_diff_cache();
                    }
                    let key = Self::repo_instance_key(&repo_address, &repo_path);
                    if !self.repo_states.contains_key(&key) {
                        self.set_repo_state_for_address(
                            &repo_address,
                            &repo_path,
                            RepoState::Unknown,
                        );
                    }
                }
                CachedUpdateLoadOutcome::Corrupt => {
                    self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                    self.set_repo_state_for_address(&repo_address, &repo_path, RepoState::Unknown);
                }
                CachedUpdateLoadOutcome::ReadError(err) => {
                    log::error!(
                        "Failed to read cached pending updates for {}: {}",
                        self.repository_view_state.repositories[repo_index].name,
                        err
                    );
                    let key = Self::repo_instance_key(&repo_address, &repo_path);
                    if !self.repo_states.contains_key(&key) {
                        self.set_repo_state_for_address(
                            &repo_address,
                            &repo_path,
                            RepoState::Unknown,
                        );
                    }
                }
            }
            self.needs_repaint = true;
        }
    }
}

fn update_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let suffix: String = rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();
    format!("{millis}-{suffix}")
}

pub(crate) fn merge_download_mod_diffs_preserving_finished(
    previous: Vec<ModDiffSummary>,
    active: Vec<ModDiffSummary>,
    progress: &mut HashMap<String, (f32, usize, usize, u64, u64)>,
) -> Vec<ModDiffSummary> {
    if previous.is_empty() {
        return active;
    }

    let mut active_by_name = active
        .into_iter()
        .map(|m| (m.name.to_lowercase(), m))
        .collect::<HashMap<_, _>>();
    let mut merged = Vec::with_capacity(previous.len().max(active_by_name.len()));

    for mut old in previous {
        let key = old.name.to_lowercase();
        if let Some(new) = active_by_name.remove(&key) {
            merged.push(new);
        } else if old.needs_update {
            let files_total = old.files.len();
            let bytes_total = old.total_bytes;
            old.needs_update = false;
            progress.insert(
                old.name.clone(),
                (1.0, files_total, files_total, bytes_total, bytes_total),
            );
            merged.push(old);
        } else {
            merged.push(old);
        }
    }

    merged.extend(active_by_name.into_values());
    merged
}

/// Worker-thread body for [`Foxy::dispatch_cached_update_load`]: load a
/// repository's cached pending-update payload from the database and classify
/// the result. Runs off the UI thread.
fn load_cached_update_outcome(normalized_url: &str, local_path: &str) -> CachedUpdateLoadOutcome {
    let Some(rt) = crate::core::api::background_runtime() else {
        return CachedUpdateLoadOutcome::ReadError(
            "shared background runtime unavailable".to_string(),
        );
    };
    let payload = match rt.block_on(load_pending_update_payload_for_path(
        normalized_url,
        local_path,
    )) {
        Ok(Some(payload)) => payload,
        Ok(None) => return CachedUpdateLoadOutcome::NoPayload,
        Err(err) => return CachedUpdateLoadOutcome::ReadError(err.to_string()),
    };
    let mods = match serde_json::from_str::<Vec<ModDiffSummary>>(&payload) {
        Ok(mods) => mods,
        Err(err) => {
            log::error!(
                "Failed to parse cached pending updates for {}: {}",
                normalized_url,
                err
            );
            return CachedUpdateLoadOutcome::Corrupt;
        }
    };
    if mods.iter().any(|m| m.needs_update) {
        CachedUpdateLoadOutcome::Pending(mods)
    } else {
        // Stale "no updates" payload: clear the row here so the UI thread never has to.
        let _ = rt.block_on(clear_pending_update_payload_for_path(
            normalized_url,
            local_path,
        ));
        CachedUpdateLoadOutcome::Synced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::api::{FileDiffSummary, ModDiffSummary};

    fn mod_summary(name: &str, needs_update: bool, files: usize) -> ModDiffSummary {
        use crate::core::api::FileDiffKind;

        ModDiffSummary {
            name: name.to_string(),
            needs_update,
            total_bytes: files as u64 * 100,
            files: (0..files)
                .map(|idx| FileDiffSummary {
                    name: format!("file_{idx}.pbo"),
                    needs_update,
                    total_bytes: 100,
                    changed_parts: 1,
                    change_kind: FileDiffKind::Modified,
                })
                .collect(),
        }
    }

    #[test]
    fn download_diff_merge_marks_previous_missing_active_mods_finished() {
        let previous = vec![
            mod_summary("@done", true, 2),
            mod_summary("@remaining", true, 3),
        ];
        let active = vec![mod_summary("@remaining", true, 3)];
        let mut progress = HashMap::new();

        let merged = merge_download_mod_diffs_preserving_finished(previous, active, &mut progress);

        assert_eq!(merged.len(), 2);
        assert!(!merged[0].needs_update);
        assert!(merged[1].needs_update);
        assert_eq!(progress.get("@done"), Some(&(1.0, 2, 2, 200, 200)));
    }
}
