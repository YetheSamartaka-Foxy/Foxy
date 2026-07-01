use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::{info, warn};
use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use tokio::sync::watch;

use crate::core::api::{self, ModDiffSummary, SyncMode};
use crate::ui::app::{AddonHashRecalcResult, Foxy};
use crate::ui::types::{DownloadSummary, RepoState, Repository, sanitize_user_path};

impl Foxy {
    fn resolve_selected_mod_states(
        repo: &Repository,
        selected_mod_states_override: Option<Vec<(String, bool)>>,
    ) -> Vec<(String, bool)> {
        if let Some(selected_mod_states) = selected_mod_states_override {
            return selected_mod_states;
        }

        let mut effective = repo.clone();
        if let Some(profile_name) = &repo.selected_profile
            && let Some(profile) = repo.profiles.iter().find(|p| &p.name == profile_name)
        {
            Self::apply_profile_to_repository(&mut effective, profile);
        }

        effective
            .addons
            .iter()
            .chain(effective.optional_addons.iter())
            .map(|(name, enabled)| (name.clone(), *enabled))
            .collect()
    }

    pub(in crate::ui::app) fn start_core_sync_with_selected_mod_states(
        &mut self,
        repo_idx: usize,
        mode: SyncMode,
        selected_mod_states_override: Option<Vec<(String, bool)>>,
        force_redownload: bool,
    ) {
        self.start_core_sync_internal(
            repo_idx,
            mode,
            selected_mod_states_override,
            force_redownload,
            false,
        );
    }

    fn start_core_sync_internal(
        &mut self,
        repo_idx: usize,
        mode: SyncMode,
        selected_mod_states_override: Option<Vec<(String, bool)>>,
        force_redownload: bool,
        prepare_download_plan: bool,
    ) {
        if self.syncing_repository.is_some() {
            warn!("Sync request ignored: another repository sync is already in progress");
            return;
        }
        if self.is_direct_download_running() {
            warn!("Sync request ignored: direct download is currently active");
            return;
        }
        if let Some(repo) = self
            .repository_view_state
            .repositories
            .get(repo_idx)
            .cloned()
        {
            if repo.address.trim().is_empty() || repo.path.trim().is_empty() {
                warn!(
                    "Sync request ignored for repository {} due to incomplete configuration",
                    repo.name
                );
                const INCOMPLETE_CONFIG_TOAST_THROTTLE: Duration = Duration::from_secs(4);
                let now = Instant::now();
                let should_toast = self
                    .last_incomplete_config_sync_toast_at
                    .is_none_or(|prev| {
                        now.duration_since(prev) >= INCOMPLETE_CONFIG_TOAST_THROTTLE
                    });
                if should_toast {
                    let message = self.t_fmt(
                        "Sync ignored for {name}: repository URL or local path is not configured.",
                        &[("name", repo.name.clone())],
                    );
                    self.show_error_toast(message);
                    self.last_incomplete_config_sync_toast_at = Some(now);
                }
                return;
            }
            info!(
                "Starting repository sync: repo={} mode={:?}",
                repo.name, mode
            );
            let normalized_repo_url = Self::normalize_repo_url(&repo.address);
            if self
                .pending_repository_db_wipes
                .contains(&normalized_repo_url)
            {
                warn!(
                    "Sync request ignored for repository {}: database wipe is still in progress",
                    repo.name
                );
                return;
            }
            let recent_local_path_reset = self
                .repo_db_reset_pending_recheck
                .contains(&normalized_repo_url);
            let (tx, rx) = broadcast::channel(4096);
            let (download_pause_tx, download_pause_rx) = watch::channel(false);
            let (cancel_tx, cancel_rx) = watch::channel(false);
            self.backend_progress_rx = Some(rx);
            self.cancel_tx = Some(cancel_tx);
            self.clear_progress_event_history();
            // A background sync (e.g. a startup recheck of a *different*
            // repository) must not wipe the just-finished download summary that
            // the update modal is still showing. Preserve that display state
            // unless this sync is a new download or targets the same repo.
            let preserve_completed_download = mode != SyncMode::Download
                && self.download_finished
                && self.download_finished_repo.is_some()
                && self.download_finished_repo != Some(repo_idx);
            if mode != SyncMode::Download && !preserve_completed_download {
                self.clear_mod_diff_cache();
            }
            self.direct_download_update_view = false;
            self.syncing_repository = Some(repo_idx);
            self.current_sync_mode = Some(mode);
            if mode == SyncMode::Download {
                self.suppress_fs_watch_for_active_download();
            }
            if !preserve_completed_download {
                self.download_progress = None;
                self.download_finished = false;
                self.download_finished_repo = None;
            }
            self.recheck_stage_label = Self::initial_recheck_stage_label(mode).map(str::to_owned);
            self.recheck_stage_percent = Self::initial_recheck_stage_label(mode).map(|_| 0.05);
            self.recheck_hash_counter = None;
            self.recheck_hash_part_counter = None;
            self.last_hash_progress_repaint = None;
            self.download_hash_sample_at = None;
            self.download_hash_sample_files = 0;
            self.download_hash_sample_parts = 0;
            self.completed_repository_check_banner = None;
            if self
                .completed_repository_db_wipe_banner
                .as_ref()
                .is_some_and(|banner| banner.repository_url == normalized_repo_url)
            {
                self.completed_repository_db_wipe_banner = None;
            }
            self.needs_repaint = true;
            if !self.mod_download_progress.is_empty() {
                self.mod_download_progress.clear();
                self.invalidate_update_modal_sort_cache();
            }
            if mode == SyncMode::Download {
                self.set_repo_state_for_address(&repo.address, &repo.path, RepoState::Updating);
                let instance_key = Self::repo_instance_key(&repo.address, &repo.path);
                let current_pending_source: Vec<ModDiffSummary> = self
                    .pending_update_cache
                    .get(&instance_key)
                    .cloned()
                    .or_else(|| {
                        if self.update_ready_repo == Some(repo_idx) {
                            Some(self.mod_diff_cache.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                let summary_source = self
                    .active_update_session_mods_for_url(&normalized_repo_url)
                    .unwrap_or_else(|| current_pending_source.clone());
                if !summary_source.is_empty() {
                    self.ensure_active_update_session_for_url(
                        &normalized_repo_url,
                        &summary_source,
                    );
                }
                let (mods_updated, files_updated, parts_updated) =
                    Self::summarize_pending_mod_updates(&summary_source);
                self.download_summary = Some(DownloadSummary {
                    mods_updated,
                    files_updated,
                    parts_updated,
                    downloaded_bytes: 0,
                    planned_transfer_bytes: 0,
                    full_download_bytes: 0,
                    patch_savings_bytes: 0,
                    patched_files: 0,
                    download_stage_duration: Duration::from_secs(0),
                    cumulative_hash_duration: Duration::from_secs(0),
                    after_download_hash_duration: Duration::from_secs(0),
                    hash_stage_duration: Duration::from_secs(0),
                    total_duration: Duration::from_secs(0),
                    avg_speed_bps: 0.0,
                    telemetry_samples: Vec::new(),
                });
                self.download_started_at = None;
                self.download_stage_started_at = None;
                self.hash_stage_started_at = None;
                self.download_stage_duration = None;
                self.hash_stage_duration = None;
                self.cumulative_hash_duration = Duration::ZERO;
                self.download_speed_bps = 0.0;
                self.download_speed_sample_at = None;
                self.download_speed_sample_bytes = 0;
                self.total_downloaded_bytes = 0;
                self.download_eta_remaining = None;
                self.download_eta_updated_at = None;
                self.download_pause_tx = Some(download_pause_tx);
                self.download_paused = false;
            } else {
                if !preserve_completed_download {
                    self.download_summary = None;
                }
                self.download_started_at = None;
                self.download_stage_started_at = None;
                self.hash_stage_started_at = None;
                self.download_stage_duration = None;
                self.hash_stage_duration = None;
                self.cumulative_hash_duration = Duration::ZERO;
                self.download_speed_bps = 0.0;
                self.download_speed_sample_at = None;
                self.download_speed_sample_bytes = 0;
                self.total_downloaded_bytes = 0;
                self.download_eta_remaining = None;
                self.download_eta_updated_at = None;
                self.download_pause_tx = None;
                self.download_paused = false;
            }
            self.memory_diagnostics_last_logged_stage_key = None;
            self.sync_started_at = Some(Instant::now());
            self.capture_memory_diagnostics_snapshot(
                format!("sync-start {:?} {}", mode, repo.name),
                true,
            );
            let download_speed_limit_mbps = self
                .settings_view_state
                .download_speed_limit_mbps
                .filter(|limit| *limit > 0);
            let auto_backup_directory =
                if mode == SyncMode::Download && self.repo_auto_backup_on_update(&repo) {
                    Some(sanitize_user_path(
                        &self.settings_view_state.backup_directory,
                    ))
                } else {
                    None
                };
            let rollback_temp_directory = (mode == SyncMode::Download)
                .then(|| sanitize_user_path(&self.effective_temp_directory()));
            let selected_mod_states =
                Self::resolve_selected_mod_states(&repo, selected_mod_states_override);
            let repository_space_shared_path = repo.repository_space_id.as_deref().and_then(|id| {
                self.repository_spaces
                    .iter()
                    .find(|space| space.id == id)
                    .map(|space| sanitize_user_path(&space.shared_path))
                    .filter(|path| {
                        !path.trim().is_empty()
                            && crate::core::utils::content_hash::normalize_path(path)
                                != crate::core::utils::content_hash::normalize_path(&repo.path)
                    })
            });
            self.backend_worker = Some(api::spawn_repository_sync(
                repo.address.clone(),
                sanitize_user_path(&repo.path),
                selected_mod_states,
                tx,
                mode,
                api::RepositorySyncOptions {
                    operation_id: api::next_operation_id("repo-sync"),
                    prepare_download_plan,
                    repository_space_shared_path,
                    auto_backup_directory,
                    rollback_temp_directory,
                    download_speed_limit_mbps,
                    recent_local_path_reset,
                    force_redownload,
                    allow_suspect_full_redownload: force_redownload,
                    download_pause_rx,
                    cancel_rx,
                    hash_algorithm_preference: repo.hash_algorithm_preference,
                    hash_io_profile: self.settings_view_state.hash_io_profile,
                },
                self.repaint_ctx.clone(),
            ));
        } else {
            warn!(
                "Sync request ignored: invalid repository index {}",
                repo_idx
            );
        }
    }

    pub fn start_core_sync(&mut self, repo_idx: usize, mode: SyncMode) {
        self.start_core_sync_with_selected_mod_states(repo_idx, mode, None, false);
    }

    pub(crate) fn prepare_update_confirmation(&mut self, repo_idx: usize) {
        self.update_modal_open = false;
        self.open_update_after_sync = true;
        self.start_core_sync_internal(repo_idx, SyncMode::RecheckOnly, None, false, true);
        if self.syncing_repository.is_none() {
            self.open_update_after_sync = false;
        }
    }

    /// Manual remote recheck that also builds the final download plan (queue +
    /// exact patch-byte estimate) while it refreshes remote metadata. Preparing
    /// the plan here lets a later "Update ready" click open the confirmation
    /// modal instantly and lets the download reuse the prepared queue, instead
    /// of running a second redundant recheck. Does not auto-open the modal - it
    /// only refreshes status and leaves the queue ready for review.
    pub(crate) fn start_remote_recheck_with_plan(&mut self, repo_idx: usize) {
        self.open_update_after_sync = false;
        self.start_core_sync_internal(repo_idx, SyncMode::RemoteRefreshOnly, None, false, true);
    }

    pub fn standalone_download_addon(&mut self, repo_idx: usize, addon_name: &str) -> bool {
        self.standalone_download_addons(repo_idx, &[addon_name.to_owned()])
    }

    pub fn standalone_download_addons(&mut self, repo_idx: usize, addon_names: &[String]) -> bool {
        if self.repository_sync_active() || self.is_direct_download_running() {
            warn!("Standalone addon download ignored: sync worker is currently active");
            return false;
        }

        let requested_addons = addon_names
            .iter()
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if requested_addons.is_empty() {
            warn!("Standalone addon download ignored: no addon names were provided");
            return false;
        }

        let repo = match self.repository_view_state.repositories.get(repo_idx) {
            Some(repo) => repo.clone(),
            None => {
                warn!(
                    "Standalone addon download ignored: invalid repository index {}",
                    repo_idx
                );
                return false;
            }
        };

        let mut selected_mod_states = Self::resolve_selected_mod_states(&repo, None);
        let mut found_targets = Vec::new();
        for (name, enabled) in &mut selected_mod_states {
            let matches_target = requested_addons
                .iter()
                .any(|addon_name| name.eq_ignore_ascii_case(addon_name));
            *enabled = matches_target;
            if matches_target {
                found_targets.push(name.clone());
            }
        }

        if found_targets.is_empty() {
            warn!(
                "Standalone addon download ignored: addons {:?} are not available in repository {} local/optional addon list",
                requested_addons, repo.name
            );
            return false;
        }

        let selected_count = selected_mod_states
            .iter()
            .filter(|(_, enabled)| *enabled)
            .count();
        info!(
            "Standalone addon download requested: repo={} url={} addons={:?} selected_mod_count={}",
            repo.name,
            Self::normalize_repo_url(&repo.address),
            found_targets,
            selected_count
        );
        for (name, enabled) in &selected_mod_states {
            if *enabled {
                info!(
                    "Standalone addon download selected target: repo={} addon={}",
                    repo.name, name
                );
            }
        }

        self.repository_view_state.selected_repository = Some(repo_idx);
        self.selected_repository_space_id = None;
        self.clear_completed_repository_check_banner_for_repo_change(Some(repo_idx));
        self.update_modal_open = true;
        self.open_update_after_sync = false;
        self.start_core_sync_with_selected_mod_states(
            repo_idx,
            SyncMode::Download,
            Some(selected_mod_states),
            false,
        );

        self.syncing_repository.is_some()
    }

    pub fn force_redownload_repository(&mut self, repo_idx: usize) {
        if self.repository_sync_active() || self.is_direct_download_running() {
            warn!("Force redownload ignored: sync worker is currently active");
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
                    "Force redownload ignored: invalid repository index {}",
                    repo_idx
                );
                return;
            }
        };
        info!("Force redownload requested for repository {}", repo.name);

        let normalized_url = Self::normalize_repo_url(&repo.address);
        if self.pending_repository_db_wipes.contains(&normalized_url) {
            warn!(
                "Force redownload ignored: repository purge is still in progress for {}",
                repo.name
            );
            return;
        }
        self.repo_db_reset_pending_recheck.remove(&normalized_url);
        self.clear_mod_diff_cache();
        self.clear_progress_event_history();
        self.syncing_repository = None;
        self.current_sync_mode = None;
        self.update_ready_repo = None;
        self.clear_pending_update_cache_for_url(&repo.address, &repo.path);
        // Clear any stale update summary notice - the repo was just purged
        // and will be re-downloaded from scratch.
        self.acknowledge_update_summary_for_repo(repo_idx);
        self.repository_view_state.selected_repository = Some(repo_idx);
        self.clear_completed_repository_check_banner_for_repo_change(Some(repo_idx));
        self.download_progress = None;
        self.download_finished = false;
        self.download_finished_repo = None;
        self.update_modal_open = false;
        self.open_update_after_sync = false;
        self.needs_repaint = true;

        self.start_core_sync_with_selected_mod_states(repo_idx, SyncMode::Download, None, true);
    }

    pub(in crate::ui::app) fn normalize_path_for_addon_match(path: &str) -> String {
        crate::core::utils::content_hash::normalize_path(path)
    }

    pub(in crate::ui::app) fn is_safe_addon_path(base_path: &str, addon_path: &Path) -> bool {
        if base_path.trim().is_empty() {
            return false;
        }
        let base_normalized = Self::normalize_path_for_addon_match(base_path.trim());
        let addon_normalized = Self::normalize_path_for_addon_match(&addon_path.to_string_lossy());
        let prefix = format!("{}/", base_normalized);
        addon_normalized.starts_with(&prefix)
    }

    pub fn recalculate_addon_hashes(&mut self, repo_idx: usize, addon_name: &str) -> bool {
        let addon_name = addon_name.trim();
        if addon_name.is_empty() {
            warn!("Addon hash recalculation ignored: addon name is empty");
            return false;
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
                    "Addon hash recalculation ignored: invalid repository index {}",
                    repo_idx
                );
                return false;
            }
        };

        let normalized_url = Self::normalize_repo_url(&repo.address);
        let same_action_running = self.current_sync_mode == Some(SyncMode::RecheckOnly)
            && self
                .syncing_repository
                .and_then(|idx| self.repository_view_state.repositories.get(idx))
                .map(|active_repo| Self::normalize_repo_url(&active_repo.address) == normalized_url)
                .unwrap_or(false);
        if same_action_running {
            warn!(
                "Addon hash recalculation ignored: action is already running for repository {}",
                repo.name
            );
            return false;
        }

        // Defer until any blocking work ahead of this addon finishes: an active
        // sync, a direct download, or a recalculation already in flight. The
        // queue is drained by `process_addon_hash_recalc_queue`.
        if self.repository_sync_active()
            || self.is_direct_download_running()
            || self.addon_hash_recalc_in_flight
        {
            self.queue_addon_hash_recalc(normalized_url, addon_name, &repo.name);
            return true;
        }

        // Hashing reads and digests every file in the addon, which can take
        // seconds for large addons; run it on a background thread so the UI
        // never blocks. The result is applied by `poll_addon_hash_recalc_results`,
        // which then starts the recheck sync.
        info!(
            "Starting background addon hash recalculation for {} in {}",
            addon_name, repo.name
        );
        let tx = self.addon_hash_recalc_result_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        let worker_url = normalized_url;
        let worker_addon = addon_name.to_string();
        let worker_repo_name = repo.name.clone();
        self.addon_hash_recalc_in_flight = true;
        self.needs_repaint = true;
        std::thread::spawn(move || {
            let outcome = match Runtime::new() {
                Ok(rt) => rt
                    .block_on(api::recalculate_hashes_for_addon_by_name(
                        &worker_url,
                        &worker_addon,
                    ))
                    .map_err(|err| err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            if tx
                .send(AddonHashRecalcResult {
                    repo_url: worker_url,
                    addon_name: worker_addon,
                    repo_name: worker_repo_name,
                    outcome,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });
        true
    }

    /// Queue an addon hash recalculation to run once the work ahead of it
    /// finishes, de-duplicating against entries already queued for the same
    /// addon in the same repository.
    fn queue_addon_hash_recalc(
        &mut self,
        normalized_url: String,
        addon_name: &str,
        repo_name: &str,
    ) {
        let already_queued =
            self.addon_hash_recalc_queue
                .iter()
                .any(|(queued_url, queued_addon)| {
                    queued_url == &normalized_url && queued_addon.eq_ignore_ascii_case(addon_name)
                });
        if already_queued {
            info!(
                "Addon hash recalculation already queued for {} in {}",
                addon_name, repo_name
            );
            return;
        }
        self.addon_hash_recalc_queue
            .push_back((normalized_url, addon_name.to_string()));
        info!(
            "Queued addon hash recalculation for {} in {}",
            addon_name, repo_name
        );
        self.needs_repaint = true;
    }

    /// Drain completed background addon hash recalculations and apply them on
    /// the UI thread. On success a recheck sync is started for the repository.
    pub(in crate::ui::app) fn poll_addon_hash_recalc_results(&mut self) {
        while let Ok(result) = self.addon_hash_recalc_result_rx.try_recv() {
            self.addon_hash_recalc_in_flight = false;
            let AddonHashRecalcResult {
                repo_url,
                addon_name,
                repo_name,
                outcome,
            } = result;
            match outcome {
                Ok(true) => {
                    info!(
                        "Addon hash recalculation complete for {} in {}",
                        addon_name, repo_name
                    );
                    // The list may have changed while the worker ran; re-resolve
                    // the index by URL before kicking off the recheck sync.
                    match self.repo_index_by_normalized_url(&repo_url) {
                        Some(repo_idx) => {
                            self.open_update_after_sync = false;
                            self.start_core_sync(repo_idx, SyncMode::RecheckOnly);
                        }
                        None => warn!(
                            "Addon hash recalculation finished for {} but repository {} no longer exists",
                            addon_name, repo_url
                        ),
                    }
                }
                Ok(false) => warn!(
                    "Addon hash recalculation skipped: addon {} not found in repository {}",
                    addon_name, repo_name
                ),
                Err(err) => log::error!(
                    "Failed to recalculate addon hashes for {} in {}: {}",
                    addon_name,
                    repo_name,
                    err
                ),
            }
            self.needs_repaint = true;
        }
    }

    pub fn force_redownload_addon(
        &mut self,
        repo_idx: usize,
        addon_name: &str,
        addon_path: Option<&str>,
    ) -> bool {
        if self.repository_sync_active() || self.is_direct_download_running() {
            warn!("Addon force redownload ignored: sync worker is currently active");
            return false;
        }

        let addon_name = addon_name.trim();
        if addon_name.is_empty() {
            warn!("Addon force redownload ignored: addon name is empty");
            return false;
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
                    "Addon force redownload ignored: invalid repository index {}",
                    repo_idx
                );
                return false;
            }
        };

        let repo_path = repo.path.trim();
        if repo_path.is_empty() {
            warn!(
                "Addon force redownload ignored: repository {} has no local path",
                repo.name
            );
            return false;
        }

        let fallback_path = Path::new(repo_path).join(addon_name);
        let requested_path = addon_path
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
        let target_path = if let Some(path) = requested_path {
            if Self::is_safe_addon_path(repo_path, &path) {
                path
            } else {
                warn!(
                    "Resolved addon path is outside repository root for {}; falling back to repo-local path",
                    addon_name
                );
                fallback_path
            }
        } else {
            fallback_path
        };

        if !Self::is_safe_addon_path(repo_path, &target_path) {
            warn!(
                "Addon force redownload ignored: unsafe addon path for {} in {}",
                addon_name, repo.name
            );
            return false;
        }

        if target_path.exists() {
            if target_path.is_dir() {
                if let Err(err) = fs::remove_dir_all(&target_path) {
                    warn!(
                        "Failed to remove addon directory for {} in {}: {}",
                        addon_name, repo.name, err
                    );
                    return false;
                }
                info!(
                    "Removed addon directory for {} in {} before recheck",
                    addon_name, repo.name
                );
            } else {
                warn!(
                    "Addon force redownload ignored: target path is not a directory for {} in {}",
                    addon_name, repo.name
                );
                return false;
            }
        } else {
            info!(
                "Addon directory already missing for {} in {}; continuing with recheck",
                addon_name, repo.name
            );
        }

        self.update_modal_open = false;
        self.prepare_update_confirmation(repo_idx);
        self.syncing_repository.is_some()
    }
}
