use std::sync::mpsc::TryRecvError as StdTryRecvError;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tokio::sync::broadcast::error::TryRecvError;

use crate::core::api::{ProgressEvent, SyncMode};
use crate::ui::app::{Foxy, RepositoryCheckCompletionState, RepositoryDbWipeCompletionState};
use crate::ui::types::{DownloadSummary, DownloadTelemetrySample, RepoState};

impl Foxy {
    fn record_progress_event(&mut self, evt: &ProgressEvent) {
        const PROGRESS_EVENT_HISTORY_LIMIT: usize = 256;

        if matches!(evt, ProgressEvent::Diff { .. }) {
            return;
        }

        if self.progress_events.len() >= PROGRESS_EVENT_HISTORY_LIMIT {
            self.progress_events.pop_front();
        }
        self.progress_events.push_back(evt.clone());
    }

    pub(in crate::ui::app) fn clear_progress_event_history(&mut self) {
        self.progress_events.clear();
        if self.progress_events.capacity() > 256 {
            self.progress_events.shrink_to(256);
        }
    }

    pub(crate) fn repository_sync_active(&self) -> bool {
        self.syncing_repository.is_some()
            || self.current_sync_mode.is_some()
            || self
                .backend_worker
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
    }

    pub(in crate::ui::app) fn poll_finished_backend_worker(&mut self) {
        let Some(handle) = self.backend_worker.as_ref() else {
            return;
        };
        if !handle.is_finished() {
            return;
        }

        if let Some(handle) = self.backend_worker.take() {
            if handle.join().is_err() {
                warn!("Repository sync worker panicked during shutdown");
            } else {
                debug!("Cleaned up finished repository sync worker");
            }
            self.needs_repaint = true;
        }
    }

    pub(in crate::ui::app) fn poll_repository_db_wipe_results(&mut self) {
        loop {
            match self.repository_db_wipe_rx.try_recv() {
                Ok(result) => {
                    self.pending_repository_db_wipes
                        .remove(&result.repository_url);
                    self.pending_repository_force_redownloads
                        .remove(&result.repository_url);
                    self.pending_repository_db_wipe_started_at
                        .remove(&result.repository_url);
                    let force_redownload_after_purge = result.force_redownload_after_purge;
                    self.completed_repository_db_wipe_banner = (!force_redownload_after_purge
                        || result.result.is_err())
                    .then_some(RepositoryDbWipeCompletionState {
                        repository_url: result.repository_url.clone(),
                        success: result.result.is_ok(),
                        elapsed: result.elapsed,
                        error_message: result.result.as_ref().err().cloned(),
                    });

                    match result.result {
                        Ok(()) => {
                            info!(
                                "Repository database entries wiped for repository {} in {:.2?}",
                                result.repository_name, result.elapsed
                            );
                            let summary_notice_count =
                                self.settings_view_state.update_summary_notices.len();
                            self.repo_db_reset_pending_recheck
                                .insert(result.repository_url.clone());
                            self.clear_mod_diff_cache();
                            self.clear_progress_event_history();
                            self.update_ready_repo = None;
                            self.clear_repo_state_for_url(
                                &result.repository_url,
                                &result.local_path,
                            );
                            self.quick_scan_pending.remove(&result.repository_url);
                            self.pending_quick_scan_urls.remove(&result.repository_url);
                            self.pending_quick_scan_prevalidated_urls
                                .remove(&result.repository_url);
                            self.pending_quick_scan_force_fresh_addon_hash_urls
                                .remove(&result.repository_url);
                            self.clear_pending_update_cache_for_url(
                                &result.repository_url,
                                &result.local_path,
                            );
                            self.settings_view_state
                                .update_summary_notices
                                .retain(|notice| notice.repository_url != result.repository_url);
                            if self.settings_view_state.update_summary_notices.len()
                                != summary_notice_count
                            {
                                self.save_settings();
                            }

                            if force_redownload_after_purge {
                                if let Some(repo_idx) =
                                    self.repository_view_state.repositories.iter().position(
                                        |repo| {
                                            Self::normalize_repo_url(&repo.address)
                                                == result.repository_url
                                        },
                                    )
                                {
                                    info!(
                                        "Repository reset complete, starting forced download for {}",
                                        result.repository_name
                                    );
                                    self.repository_view_state.selected_repository = Some(repo_idx);
                                    self.start_core_sync_with_selected_mod_states(
                                        repo_idx,
                                        SyncMode::Download,
                                        None,
                                        true,
                                    );
                                } else {
                                    warn!(
                                        "Forced download skipped after purge: repository {} is no longer configured",
                                        result.repository_name
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            log::error!(
                                "Failed to wipe repository database entries for {} after {:.2?}: {}",
                                result.repository_name,
                                result.elapsed,
                                err
                            );
                        }
                    }

                    self.needs_repaint = true;
                }
                Err(StdTryRecvError::Empty) => break,
                Err(StdTryRecvError::Disconnected) => {
                    warn!("Repository database wipe result channel disconnected");
                    break;
                }
            }
        }
    }

    /// Drain completed global database wipe results.
    pub(in crate::ui::app) fn poll_database_wipe_result(&mut self) {
        loop {
            match self.database_wipe_rx.try_recv() {
                Ok(Ok(())) => {
                    self.show_success_toast(self.t("Database wiped successfully"));
                    self.needs_repaint = true;
                }
                Ok(Err(err)) => {
                    self.show_error_toast(self.t("Failed to wipe database") + &format!(": {err}"));
                    self.needs_repaint = true;
                }
                Err(StdTryRecvError::Empty) => break,
                Err(StdTryRecvError::Disconnected) => {
                    warn!("Database wipe result channel disconnected");
                    break;
                }
            }
        }
    }

    pub(in crate::ui::app) fn poll_addon_delete_results(&mut self) {
        loop {
            match self.addon_delete_result_rx.try_recv() {
                Ok(result) => {
                    let delete_key = Self::normalize_path_for_addon_match(&result.addon_path);
                    self.pending_addon_deletes.remove(&delete_key);

                    match result.outcome {
                        Ok(deleted_rows) => {
                            info!(
                                "Addon {} deleted from storage and database references removed ({})",
                                result.addon_name, deleted_rows
                            );
                            let mut repository_data_changed = false;
                            for repo in &mut self.repository_view_state.repositories {
                                let before = repo.external_addons.len();
                                repo.external_addons.retain(|(_, _, path)| {
                                    Self::normalize_path_for_addon_match(path) != delete_key
                                });
                                repository_data_changed |= repo.external_addons.len() != before;

                                for profile in &mut repo.profiles {
                                    let before = profile.external_addons.len();
                                    profile.external_addons.retain(|(_, _, path)| {
                                        Self::normalize_path_for_addon_match(path) != delete_key
                                    });
                                    repository_data_changed |=
                                        profile.external_addons.len() != before;
                                }
                            }
                            if repository_data_changed {
                                self.save_repositories();
                            }
                            self.invalidate_addon_inventory_cache();
                            self.show_success_toast(
                                self.t_fmt("Addon deleted: {name}", &[("name", result.addon_name)]),
                            );
                        }
                        Err(err) => {
                            warn!("Addon delete failed for {}: {}", result.addon_name, err);
                            self.show_error_toast(self.t_fmt(
                                "Failed to delete addon {name}: {error}",
                                &[("name", result.addon_name), ("error", err)],
                            ));
                        }
                    }
                    self.needs_repaint = true;
                }
                Err(StdTryRecvError::Empty) => break,
                Err(StdTryRecvError::Disconnected) => break,
            }
        }
    }

    pub fn poll_backend_progress(&mut self) {
        const MAX_PROGRESS_EVENTS_PER_POLL: usize = 512;
        let mut processed_events = 0usize;

        loop {
            if processed_events >= MAX_PROGRESS_EVENTS_PER_POLL {
                self.needs_repaint = true;
                break;
            }

            // Limit the mutable borrow of backend_progress_rx to this block.
            let evt = {
                let Some(rx) = self.backend_progress_rx.as_mut() else {
                    break;
                };
                match rx.try_recv() {
                    Ok(evt) => Some(evt),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Closed) => {
                        let was_active =
                            self.syncing_repository.is_some() || self.current_sync_mode.is_some();
                        if was_active {
                            warn!("Core progress channel closed unexpectedly");
                        } else {
                            debug!("Core progress channel closed");
                        }
                        self.backend_progress_rx = None;
                        self.syncing_repository = None;
                        self.current_sync_mode = None;
                        self.refresh_repository_space_bulk_current_repo();
                        self.download_pause_tx = None;
                        self.download_paused = false;
                        self.cancel_tx = None;
                        break;
                    }
                    Err(TryRecvError::Lagged(skipped)) => {
                        warn!("Core progress receiver lagged; skipped {} events", skipped);
                        continue;
                    }
                }
            };

            let Some(evt) = evt else { continue };
            processed_events += 1;

            match &evt {
                ProgressEvent::Diff { mods } => {
                    if let Some(repo_idx) = self.syncing_repository
                        && let Some((repo_address, repo_path)) = self
                            .repository_view_state
                            .repositories
                            .get(repo_idx)
                            .map(|repo| (repo.address.clone(), repo.path.clone()))
                    {
                        self.cache_pending_updates_for_url(&repo_address, &repo_path, mods.clone());
                    }
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        let has_updates = mods.iter().any(|m| m.needs_update);
                        if has_updates {
                            self.set_download_mod_diff_cache_preserving_finished(mods.clone());
                            let (mods_updated, files_updated, parts_updated) =
                                Self::summarize_pending_mod_updates(mods);
                            if let Some(summary) = self.download_summary.as_mut() {
                                summary.mods_updated = summary.mods_updated.max(mods_updated);
                                summary.files_updated = summary.files_updated.max(files_updated);
                                summary.parts_updated = summary.parts_updated.max(parts_updated);
                            } else {
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
                            }
                        } else {
                            debug!(
                                "Ignoring clean post-download diff in UI to preserve final update summary display"
                            );
                        }
                    } else {
                        // Don't let a background recheck of another repository
                        // overwrite the diff currently displayed for a repo whose
                        // download just finished (keeps its summary/cards intact).
                        let preserve_completed_download = self.download_finished
                            && self.download_finished_repo.is_some()
                            && self.syncing_repository != self.download_finished_repo;
                        if !preserve_completed_download {
                            self.set_mod_diff_cache(mods.clone());
                        }
                    }
                    self.needs_repaint = true;
                }
                ProgressEvent::SiblingPropagation { repo_urls } => {
                    for repo_url in repo_urls {
                        let normalized_url = Self::normalize_repo_url(repo_url);
                        self.pending_quick_scan_urls.remove(&normalized_url);
                        self.pending_quick_scan_prevalidated_urls
                            .remove(&normalized_url);
                        self.pending_quick_scan_force_fresh_addon_hash_urls
                            .remove(&normalized_url);
                        self.quick_scan_pending.remove(&normalized_url);
                        self.deferred_fs_scan.remove(&normalized_url);

                        // Propagation can touch every folder instance of the URL,
                        // so refresh each instance's status independently.
                        let instances: Vec<(usize, String, String)> = self
                            .repository_view_state
                            .repositories
                            .iter()
                            .enumerate()
                            .filter(|(_, repo)| {
                                Self::normalize_repo_url(&repo.address) == normalized_url
                            })
                            .map(|(idx, repo)| (idx, repo.address.clone(), repo.path.clone()))
                            .collect();
                        for (repo_idx, repo_address, repo_path) in instances {
                            self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                            if self.update_ready_repo == Some(repo_idx) {
                                self.update_ready_repo = None;
                                self.clear_mod_diff_cache();
                                self.update_modal_open = false;
                            }
                            self.set_repo_state_for_address(
                                &repo_address,
                                &repo_path,
                                RepoState::Synced,
                            );
                        }
                    }
                    self.needs_repaint = true;
                }
                ProgressEvent::DownloadPlan {
                    files_total,
                    planned_bytes,
                    full_bytes,
                    patch_files,
                } => {
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        if let Some(summary) = self.download_summary.as_mut() {
                            summary.files_updated = summary.files_updated.max(*files_total);
                            summary.planned_transfer_bytes = *planned_bytes;
                            summary.full_download_bytes = *full_bytes;
                            summary.patch_savings_bytes = full_bytes.saturating_sub(*planned_bytes);
                            summary.patched_files = *patch_files;
                        } else {
                            self.download_summary = Some(DownloadSummary {
                                mods_updated: 0,
                                files_updated: *files_total,
                                parts_updated: 0,
                                downloaded_bytes: 0,
                                planned_transfer_bytes: *planned_bytes,
                                full_download_bytes: *full_bytes,
                                patch_savings_bytes: full_bytes.saturating_sub(*planned_bytes),
                                patched_files: *patch_files,
                                download_stage_duration: Duration::from_secs(0),
                                cumulative_hash_duration: Duration::from_secs(0),
                                after_download_hash_duration: Duration::from_secs(0),
                                hash_stage_duration: Duration::from_secs(0),
                                total_duration: Duration::from_secs(0),
                                avg_speed_bps: 0.0,
                                telemetry_samples: Vec::new(),
                            });
                        }
                        self.needs_repaint = true;
                    }
                }
                ProgressEvent::DownloadTelemetry {
                    elapsed_ms,
                    download_bps,
                    disk_write_bps,
                    cpu_percent,
                    memory_bytes,
                } => {
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        // Snapshot live running totals so the in-progress download
                        // summary card reflects real values instead of staying at
                        // zero until the final `Finished` event recomputes them.
                        let downloaded_bytes = self.total_downloaded_bytes;
                        let stage_elapsed = self.download_stage_started_at.map(|t| t.elapsed());
                        if let Some(summary) = self.download_summary.as_mut() {
                            summary.push_telemetry_sample(DownloadTelemetrySample {
                                elapsed_ms: *elapsed_ms,
                                download_bps: *download_bps,
                                disk_write_bps: *disk_write_bps,
                                hash_files_per_sec: 0.0,
                                cpu_percent: *cpu_percent,
                                memory_bytes: *memory_bytes,
                            });
                            summary.downloaded_bytes = downloaded_bytes;
                            if let Some(elapsed) = stage_elapsed {
                                summary.download_stage_duration = elapsed;
                                let stage_secs = elapsed.as_secs_f64();
                                if stage_secs > 0.0 {
                                    summary.avg_speed_bps = downloaded_bytes as f64 / stage_secs;
                                }
                            }
                        }
                        self.needs_repaint = true;
                    }
                }
                ProgressEvent::HashTelemetry {
                    elapsed_ms,
                    files_per_sec,
                } => {
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        if let Some(summary) = self.download_summary.as_mut() {
                            summary.push_telemetry_sample(DownloadTelemetrySample {
                                elapsed_ms: *elapsed_ms,
                                download_bps: 0.0,
                                disk_write_bps: 0.0,
                                hash_files_per_sec: *files_per_sec,
                                cpu_percent: 0.0,
                                memory_bytes: 0,
                            });
                        }
                        self.needs_repaint = true;
                    }
                }
                ProgressEvent::HashSummary {
                    cumulative_hash_ms,
                    after_download_hash_ms,
                } => {
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        let cumulative_hash_duration = Duration::from_millis(*cumulative_hash_ms);
                        let after_download_hash_duration =
                            Duration::from_millis(*after_download_hash_ms);
                        self.cumulative_hash_duration =
                            cumulative_hash_duration.saturating_sub(after_download_hash_duration);
                        self.hash_stage_duration = Some(after_download_hash_duration);
                        if let Some(summary) = self.download_summary.as_mut() {
                            summary.cumulative_hash_duration = cumulative_hash_duration;
                            summary.after_download_hash_duration = after_download_hash_duration;
                            summary.hash_stage_duration = after_download_hash_duration;
                        }
                        self.needs_repaint = true;
                    }
                }
                ProgressEvent::DownloadMod {
                    mod_name,
                    percent,
                    files_done,
                    files_total,
                    bytes_done,
                    bytes_total,
                } => {
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        let now = Instant::now();
                        if self.download_stage_started_at.is_none() {
                            self.download_stage_started_at = Some(now);
                        }
                        if self.download_started_at.is_none() {
                            self.download_started_at = Some(now);
                        }
                    }
                    let prev_rank = self.mod_download_sort_rank(mod_name);
                    let (
                        prev_percent,
                        prev_files_done,
                        prev_files_total,
                        prev_bytes_done,
                        prev_bytes_total,
                    ) = self
                        .mod_download_progress
                        .get(mod_name)
                        .copied()
                        .unwrap_or((0.0, 0, 0, 0, 0));

                    // Core retries can emit transient lower snapshots for an addon.
                    // Keep UI progress monotonic to avoid visual flicker/backtracking.
                    let merged_files_total = prev_files_total.max(*files_total);
                    let merged_files_done =
                        prev_files_done.max((*files_done).min(merged_files_total));
                    let merged_bytes_total = prev_bytes_total.max(*bytes_total);
                    let merged_bytes_done =
                        prev_bytes_done.max((*bytes_done).min(merged_bytes_total));
                    let merged_percent = {
                        let pct_from_event = (*percent).clamp(0.0, 1.0);
                        let pct_from_bytes = if merged_bytes_total == 0 {
                            1.0
                        } else {
                            (merged_bytes_done as f32 / merged_bytes_total as f32).min(1.0)
                        };
                        prev_percent.max(pct_from_event).max(pct_from_bytes)
                    };

                    self.mod_download_progress.insert(
                        mod_name.clone(),
                        (
                            merged_percent,
                            merged_files_done,
                            merged_files_total,
                            merged_bytes_done,
                            merged_bytes_total,
                        ),
                    );
                    let new_rank = Self::download_sort_rank(Some(merged_percent));
                    if prev_rank != new_rank {
                        self.invalidate_update_modal_sort_cache();
                        self.update_modal_sort_last_progress_invalidation = Some(Instant::now());
                    } else if merged_percent != prev_percent {
                        const SORT_THROTTLE: Duration = Duration::from_secs(2);
                        let should_invalidate = self
                            .update_modal_sort_last_progress_invalidation
                            .map(|t| t.elapsed() >= SORT_THROTTLE)
                            .unwrap_or(true);
                        if should_invalidate {
                            self.invalidate_update_modal_sort_cache();
                            self.update_modal_sort_last_progress_invalidation =
                                Some(Instant::now());
                        }
                    }
                    self.total_downloaded_bytes = self
                        .total_downloaded_bytes
                        .saturating_add(merged_bytes_done.saturating_sub(prev_bytes_done));
                    self.recheck_hash_counter = None;
                    self.recheck_hash_part_counter = None;
                    self.update_download_speed();
                    self.needs_repaint = true;
                }
                ProgressEvent::RecheckHashProgress {
                    checked_files,
                    total_files,
                    checked_parts,
                    total_parts,
                } if self.current_sync_mode == Some(SyncMode::Download) => {
                    if self.download_transfer_progress_active() {
                        continue;
                    }
                    self.recheck_hash_counter = Some((*checked_files, *total_files));
                    self.recheck_hash_part_counter = Some((*checked_parts, *total_parts));
                    self.needs_repaint = true;
                }
                ProgressEvent::RecheckHashProgress {
                    checked_files,
                    total_files,
                    checked_parts,
                    total_parts,
                } if matches!(
                    self.current_sync_mode,
                    Some(
                        SyncMode::RemoteRefreshOnly
                            | SyncMode::QuickCheckOnly
                            | SyncMode::RecheckOnly
                            | SyncMode::RecheckIntegrity
                    )
                ) =>
                {
                    self.recheck_hash_counter = Some((*checked_files, *total_files));
                    self.recheck_hash_part_counter = Some((*checked_parts, *total_parts));
                    // Throttle repaints for hash progress to avoid overwhelming
                    // the renderer during heavy operations (thousands of events).
                    let now = Instant::now();
                    let should_repaint = *checked_files == *total_files
                        || *checked_parts == *total_parts
                        || self
                            .last_hash_progress_repaint
                            .is_none_or(|last| now.duration_since(last).as_millis() >= 250);
                    if should_repaint {
                        self.last_hash_progress_repaint = Some(now);
                        self.needs_repaint = true;
                    }
                }
                ProgressEvent::RepositoryFoxyMode {
                    is_foxy,
                    app_update_url,
                } => {
                    if let Some(idx) = self.syncing_repository
                        && let Some(repo_address) = self
                            .repository_view_state
                            .repositories
                            .get(idx)
                            .map(|repo| repo.address.clone())
                    {
                        self.set_repo_foxy_mode_for_address(&repo_address, *is_foxy);
                        if self.set_repo_app_update_url_for_address(
                            &repo_address,
                            app_update_url.as_deref(),
                        ) {
                            self.needs_repaint = true;
                        }
                    }
                }
                ProgressEvent::Finished | ProgressEvent::Failed(_) | ProgressEvent::Cancelled => {
                    let finished_successfully = matches!(evt, ProgressEvent::Finished);
                    let was_cancelled = matches!(evt, ProgressEvent::Cancelled);
                    let last_repo = self.syncing_repository;
                    let last_mode = self.current_sync_mode;
                    let sync_elapsed = self.sync_started_at.map(|started_at| started_at.elapsed());
                    let mut update_count = self
                        .mod_diff_cache
                        .iter()
                        .filter(|m| m.needs_update)
                        .count();
                    if let Some(idx) = last_repo
                        && let Some(repo) = self.repository_view_state.repositories.get(idx)
                    {
                        let instance_key = Self::repo_instance_key(&repo.address, &repo.path);
                        if self.pending_update_cache.contains_key(&instance_key) {
                            update_count =
                                self.pending_update_count_for_address(&repo.address, &repo.path);
                        }
                    }
                    let had_updates = last_mode != Some(SyncMode::Download) && update_count > 0;
                    self.syncing_repository = None;
                    self.current_sync_mode = None;
                    // A sync is what first writes a repository's addon rows, and
                    // the watcher indexes those only when it starts.
                    if finished_successfully {
                        self.mark_fs_watch_index_dirty();
                    }
                    if last_mode == Some(SyncMode::Download) {
                        self.suppress_fs_watch_after_download();
                    }
                    self.refresh_repository_space_bulk_current_repo();
                    if last_mode == Some(SyncMode::Download) {
                        if was_cancelled {
                            self.download_progress = None;
                            self.download_finished = false;
                            self.download_finished_repo = None;
                            self.download_summary = None;
                            self.update_modal_open = false;
                        } else if finished_successfully {
                            self.invalidate_addon_inventory_cache();
                            self.download_progress = Some(("Finished".to_string(), 1.0));
                            self.download_finished = true;
                            self.download_finished_repo = last_repo;
                            self.needs_repaint = true;
                            let now = Instant::now();
                            let download_stage_duration = self
                                .download_stage_duration
                                .or_else(|| {
                                    if let (Some(start), Some(hash_start)) =
                                        (self.download_stage_started_at, self.hash_stage_started_at)
                                    {
                                        Some(hash_start.duration_since(start))
                                    } else {
                                        self.download_stage_started_at
                                            .map(|start| now.duration_since(start))
                                    }
                                })
                                .unwrap_or_else(|| Duration::from_secs(0));
                            let hash_stage_duration = self
                                .hash_stage_duration
                                .or_else(|| {
                                    self.hash_stage_started_at
                                        .map(|start| now.duration_since(start))
                                })
                                .unwrap_or_else(|| Duration::from_secs(0));
                            let (cumulative_hash_duration, after_download_hash_duration) = self
                                .download_summary
                                .as_ref()
                                .map(|summary| {
                                    (
                                        summary.cumulative_hash_duration,
                                        summary.after_download_hash_duration,
                                    )
                                })
                                .filter(|(cumulative, after_download)| {
                                    *cumulative > Duration::ZERO || *after_download > Duration::ZERO
                                })
                                .unwrap_or_else(|| {
                                    (
                                        self.cumulative_hash_duration + hash_stage_duration,
                                        hash_stage_duration,
                                    )
                                });
                            let total_duration =
                                download_stage_duration + after_download_hash_duration;
                            let bytes_downloaded = self.total_downloaded_bytes;
                            let avg_speed_bps = if download_stage_duration.as_secs_f64() > 0.0 {
                                bytes_downloaded as f64 / download_stage_duration.as_secs_f64()
                            } else if total_duration.as_secs_f64() > 0.0 {
                                bytes_downloaded as f64 / total_duration.as_secs_f64()
                            } else {
                                0.0
                            };
                            if let Some(summary) = self.download_summary.as_mut() {
                                summary.downloaded_bytes = bytes_downloaded;
                                summary.download_stage_duration = download_stage_duration;
                                summary.cumulative_hash_duration = cumulative_hash_duration;
                                summary.after_download_hash_duration = after_download_hash_duration;
                                summary.hash_stage_duration = after_download_hash_duration;
                                summary.total_duration = total_duration;
                                summary.avg_speed_bps = avg_speed_bps;
                            } else {
                                self.download_summary = Some(DownloadSummary {
                                    mods_updated: 0,
                                    files_updated: 0,
                                    parts_updated: 0,
                                    downloaded_bytes: bytes_downloaded,
                                    planned_transfer_bytes: 0,
                                    full_download_bytes: 0,
                                    patch_savings_bytes: 0,
                                    patched_files: 0,
                                    download_stage_duration,
                                    cumulative_hash_duration,
                                    after_download_hash_duration,
                                    hash_stage_duration: after_download_hash_duration,
                                    total_duration,
                                    avg_speed_bps,
                                    telemetry_samples: Vec::new(),
                                });
                            }
                            if let Some(repo_idx) = last_repo {
                                self.register_update_summary_notice_for_repo(repo_idx);
                                self.check_ts3_plugin_updates_for_repo(repo_idx);
                            }
                        } else {
                            self.download_finished = false;
                            self.download_finished_repo = None;
                        }
                    }
                    if last_mode == Some(SyncMode::RecheckOnly)
                        || last_mode == Some(SyncMode::RemoteRefreshOnly)
                        || last_mode == Some(SyncMode::QuickCheckOnly)
                        || last_mode == Some(SyncMode::RecheckIntegrity)
                    {
                        self.completed_repository_check_banner = if was_cancelled {
                            if last_repo == self.repository_view_state.selected_repository {
                                last_repo.zip(last_mode).map(|(repo_index, mode)| {
                                    RepositoryCheckCompletionState {
                                        repo_index,
                                        mode,
                                        success: false,
                                        had_updates: false,
                                        update_count: 0,
                                        elapsed: sync_elapsed,
                                        error_message: Some(self.t("Operation cancelled")),
                                    }
                                })
                            } else {
                                None
                            }
                        } else if finished_successfully || matches!(evt, ProgressEvent::Failed(_)) {
                            if last_repo == self.repository_view_state.selected_repository {
                                last_repo.zip(last_mode).map(|(repo_index, mode)| {
                                    RepositoryCheckCompletionState {
                                        repo_index,
                                        mode,
                                        success: finished_successfully,
                                        had_updates,
                                        update_count,
                                        elapsed: sync_elapsed,
                                        error_message: match &evt {
                                            ProgressEvent::Failed(message) => Some(message.clone()),
                                            _ => None,
                                        },
                                    }
                                })
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        self.update_ready_repo = if had_updates { last_repo } else { None };
                        if had_updates
                            && let Some(idx) = last_repo
                            && self.repository_view_state.selected_repository == Some(idx)
                        {
                            self.apply_pending_update_cache_for_repo(idx);
                        }
                        if finished_successfully && !had_updates {
                            if let Some(idx) = last_repo {
                                if let Some((repo_address, repo_path)) = self
                                    .repository_view_state
                                    .repositories
                                    .get(idx)
                                    .map(|repo| (repo.address.clone(), repo.path.clone()))
                                {
                                    self.clear_pending_update_cache_for_url(
                                        &repo_address,
                                        &repo_path,
                                    );
                                }
                                self.clear_cached_pending_update(idx);
                            }
                            self.open_update_after_sync = false;
                        } else if (last_mode == Some(SyncMode::RecheckOnly)
                            || last_mode == Some(SyncMode::RemoteRefreshOnly)
                            || last_mode == Some(SyncMode::QuickCheckOnly))
                            && self.open_update_after_sync
                            && had_updates
                        {
                            self.direct_download_update_view = false;
                            self.update_modal_open = true;
                            self.open_update_after_sync = false;
                        } else {
                            self.open_update_after_sync = false;
                        }
                    } else if last_mode == Some(SyncMode::Download) {
                        self.completed_repository_check_banner = None;
                        self.update_ready_repo = if finished_successfully || update_count == 0 {
                            None
                        } else {
                            last_repo
                        };
                        if finished_successfully && let Some(idx) = last_repo {
                            if let Some((repo_address, repo_path)) = self
                                .repository_view_state
                                .repositories
                                .get(idx)
                                .map(|repo| (repo.address.clone(), repo.path.clone()))
                            {
                                let normalized_url = Self::normalize_repo_url(&repo_address);
                                self.repo_db_reset_pending_recheck.remove(&normalized_url);
                                self.clear_active_update_session_for_url(&repo_address);
                                self.clear_pending_update_cache_for_url(&repo_address, &repo_path);
                                // The download fully synced this repository, so a
                                // queued startup recheck is redundant - and running
                                // it would reset the finished-download summary the
                                // update modal is still displaying.
                                let queue_len = self.startup_recheck_queue.len();
                                self.startup_recheck_queue.retain(|(address, _, _)| {
                                    Self::normalize_repo_url(address) != normalized_url
                                });
                                if self.startup_recheck_queue.len() != queue_len {
                                    info!(
                                        "Dropped queued startup recheck for {} after successful download",
                                        normalized_url
                                    );
                                }
                            }
                            self.clear_cached_pending_update(idx);
                        }
                    }
                    if !finished_successfully {
                        if was_cancelled {
                            info!("Repository sync was cancelled by user");
                        } else if let ProgressEvent::Failed(message) = &evt {
                            log::error!("Repository sync failed: {}", message);
                        }
                        self.open_update_after_sync = false;
                    }
                    if let Some(ref handle) = self.backend_worker
                        && handle.is_finished()
                        && let Some(h) = self.backend_worker.take()
                    {
                        let _ = h.join();
                    }
                    self.backend_progress_rx = None;
                    if finished_successfully {
                        if let Some(idx) = last_repo
                            && let Some(repo_address) = self
                                .repository_view_state
                                .repositories
                                .get(idx)
                                .map(|repo| repo.address.clone())
                        {
                            self.repo_db_reset_pending_recheck
                                .remove(&Self::normalize_repo_url(&repo_address));
                        }

                        self.maybe_auto_fill_app_update_url_from_metadata();

                        // Queue a repo.json metadata refresh for modes that talk to the server.
                        if let Some(idx) = last_repo
                            && matches!(
                                last_mode,
                                Some(
                                    SyncMode::RemoteRefreshOnly
                                        | SyncMode::RecheckOnly
                                        | SyncMode::Download
                                )
                            )
                        {
                            self.pending_repo_metadata_refresh.push(idx);
                        }
                    }

                    if let Some(idx) = last_repo
                        && let Some(repo) = self.repository_view_state.repositories.get(idx)
                    {
                        let repo_name = repo.name.clone();
                        let address = repo.address.clone();
                        let path = repo.path.clone();
                        self.clear_quick_scan_instance_active(&address, &path);
                        if finished_successfully {
                            if last_mode == Some(SyncMode::Download) {
                                self.set_repo_state_for_address(&address, &path, RepoState::Synced);
                            } else if had_updates {
                                self.set_repo_state_for_address(
                                    &address,
                                    &path,
                                    RepoState::PendingUpdate,
                                );
                            } else {
                                self.set_repo_state_for_address(&address, &path, RepoState::Synced);
                            }
                        } else if last_mode == Some(SyncMode::Download) && update_count > 0 {
                            self.set_repo_state_for_address(
                                &address,
                                &path,
                                RepoState::PendingUpdate,
                            );
                        } else {
                            self.set_repo_state_for_address(&address, &path, RepoState::Unknown);
                        }
                        info!(
                            "Repository sync finished: repo={} success={} mode={:?}",
                            repo_name, finished_successfully, last_mode
                        );
                        if let Some(start_time) = self.sync_started_at {
                            info!(
                                "Total repository sync duration: {:.2}s",
                                start_time.elapsed().as_secs_f64()
                            );
                            self.sync_started_at = None;
                        }
                        self.capture_memory_diagnostics_snapshot(
                            format!(
                                "sync-finish {:?} {} success={}",
                                last_mode.unwrap_or(SyncMode::RecheckOnly),
                                repo_name,
                                finished_successfully
                            ),
                            true,
                        );
                    }
                    self.record_repository_space_bulk_completion(
                        last_repo,
                        last_mode,
                        finished_successfully,
                        had_updates,
                    );
                    self.record_scheduled_job_completion(
                        last_repo,
                        last_mode,
                        finished_successfully,
                        had_updates,
                    );
                    if !self.mod_download_progress.is_empty() {
                        self.mod_download_progress.clear();
                        self.invalidate_update_modal_sort_cache();
                    }
                    self.download_speed_bps = 0.0;
                    self.download_speed_sample_at = None;
                    self.download_speed_sample_bytes = 0;
                    self.total_downloaded_bytes = 0;
                    self.download_started_at = None;
                    self.download_stage_started_at = None;
                    self.hash_stage_started_at = None;
                    self.download_stage_duration = None;
                    self.hash_stage_duration = None;
                    self.cumulative_hash_duration = Duration::ZERO;
                    self.download_eta_remaining = None;
                    self.download_eta_updated_at = None;
                    self.download_pause_tx = None;
                    self.download_paused = false;
                    self.download_hash_sample_at = None;
                    self.download_hash_sample_files = 0;
                    self.download_hash_sample_parts = 0;
                    self.cancel_tx = None;
                    self.recheck_stage_label = None;
                    self.recheck_stage_percent = None;
                    self.recheck_hash_counter = None;
                    self.recheck_hash_part_counter = None;
                    self.memory_diagnostics_last_logged_stage_key = None;
                    if self.syncing_repository.is_none() && !self.deferred_fs_scan.is_empty() {
                        let repo_urls: Vec<String> = self.deferred_fs_scan.drain().collect();
                        self.queue_quick_scan_for_urls_from_fs(repo_urls);
                    }
                    if self.syncing_repository.is_none()
                        && self.quick_scan_worker.is_none()
                        && !self.pending_quick_scan_urls.is_empty()
                    {
                        let repo_urls: Vec<String> = self.pending_quick_scan_urls.drain().collect();
                        self.queue_quick_scan_for_urls(repo_urls);
                    }
                    self.needs_repaint = true;
                }
                ProgressEvent::Stage { label, percent } => {
                    if self.current_sync_mode == Some(SyncMode::Download) {
                        let hash_stage_label = Self::stage_label_is_download_hashing(label)
                            || Self::stage_label_uses_hash_counter(label);
                        if hash_stage_label && self.download_transfer_progress_active() {
                            continue;
                        }

                        let display_label = match label.as_str() {
                            "Cancelling..." => self.t("Cancelling..."),
                            "Reverting changes" => self.t("Reverting changes"),
                            _ => label.clone(),
                        };
                        let now = Instant::now();
                        if label.starts_with("Download 0/") {
                            if self.download_stage_started_at.is_none() {
                                self.download_stage_started_at = Some(now);
                            }
                            if self.download_started_at.is_none() {
                                self.download_started_at = Some(now);
                            }
                            self.recheck_hash_counter = None;
                            self.recheck_hash_part_counter = None;
                        } else if label == "Hashing..." {
                            if self.hash_stage_started_at.is_none() {
                                self.hash_stage_started_at = Some(now);
                            }
                            if self.download_stage_duration.is_none()
                                && let Some(start) = self.download_stage_started_at
                            {
                                self.download_stage_duration = Some(now.duration_since(start));
                            }
                        } else if label.starts_with("Hash ") {
                            let has_backend_hash_summary =
                                self.download_summary.as_ref().is_some_and(|summary| {
                                    summary.cumulative_hash_duration > Duration::ZERO
                                        || summary.after_download_hash_duration > Duration::ZERO
                                });
                            if has_backend_hash_summary {
                                // The core emits `Hash {total}s` for display after sending
                                // authoritative cumulative/after-download hash durations.
                            } else if let Some(parsed_duration) =
                                Self::parse_hash_stage_duration_label(label)
                            {
                                self.hash_stage_duration = Some(parsed_duration);
                            } else if self.hash_stage_duration.is_none()
                                && let Some(start) = self.hash_stage_started_at
                            {
                                self.hash_stage_duration = Some(now.duration_since(start));
                            }
                        }
                        self.download_progress = Some((display_label, *percent));
                        self.download_finished = false;
                        self.download_finished_repo = None;
                        self.needs_repaint = true;
                    } else if self.current_sync_mode == Some(SyncMode::RemoteRefreshOnly)
                        || self.current_sync_mode == Some(SyncMode::RecheckOnly)
                        || self.current_sync_mode == Some(SyncMode::QuickCheckOnly)
                        || self.current_sync_mode == Some(SyncMode::RecheckIntegrity)
                    {
                        let stage_key = Self::memory_diagnostics_stage_key(label);
                        if self.memory_diagnostics_last_logged_stage_key.as_deref()
                            != Some(stage_key.as_str())
                        {
                            self.memory_diagnostics_last_logged_stage_key = Some(stage_key.clone());
                            self.capture_memory_diagnostics_snapshot(
                                format!("recheck-stage {stage_key}"),
                                true,
                            );
                        }
                        self.recheck_stage_label = Some(label.clone());
                        self.recheck_stage_percent = Some(*percent);
                        if !Self::stage_label_uses_hash_counter(label) {
                            self.recheck_hash_counter = None;
                            self.recheck_hash_part_counter = None;
                        }
                        self.needs_repaint = true;
                    }
                }
                _ => {}
            }
            self.record_progress_event(&evt);
        }
    }

    fn parse_hash_stage_duration_label(label: &str) -> Option<Duration> {
        let seconds_text = label.strip_prefix("Hash ")?.strip_suffix('s')?.trim();
        let seconds = seconds_text.parse::<f32>().ok()?;
        Some(Duration::from_secs_f32(seconds.max(0.0)))
    }

    fn download_transfer_progress_active(&self) -> bool {
        self.current_sync_mode == Some(SyncMode::Download)
            && self.download_stage_duration.is_none()
            && self.mod_download_progress.values().any(
                |(percent, files_done, files_total, bytes_done, bytes_total)| {
                    *percent < 1.0
                        || (*files_total > 0 && *files_done < *files_total)
                        || (*bytes_total > 0 && *bytes_done < *bytes_total)
                },
            )
    }

    pub(in crate::ui::app) fn initial_recheck_stage_label(mode: SyncMode) -> Option<&'static str> {
        match mode {
            SyncMode::RemoteRefreshOnly => Some("Starting remote data recheck"),
            SyncMode::QuickCheckOnly => Some("Starting quick local check"),
            SyncMode::RecheckOnly => Some("Starting repository recheck"),
            SyncMode::RecheckIntegrity => Some("Starting integrity recheck"),
            SyncMode::Download => None,
        }
    }

    pub(in crate::ui::app) fn stage_label_uses_hash_counter(label: &str) -> bool {
        // Only actual hashing stages should keep the hash counter visible.
        // Persistence stages ("Saving parts X/Y", "Updating files X/Y", etc.)
        // carry their own progress in the label and should clear the counter.
        label == "Calculating file hashes"
            || label == "Recalculating file hashes"
            || Self::hashing_files_stage_has_progress(label)
    }

    fn stage_label_is_download_hashing(label: &str) -> bool {
        label == "Hashing..." || label.starts_with("Hashing downloaded files")
    }

    fn hashing_files_stage_has_progress(label: &str) -> bool {
        let Some(progress) = label
            .strip_prefix("Hashing ")
            .and_then(|value| value.strip_suffix(" files"))
        else {
            return false;
        };
        let Some((checked, total)) = progress.split_once('/') else {
            return false;
        };
        checked.parse::<usize>().is_ok() && total.parse::<usize>().is_ok()
    }
}
