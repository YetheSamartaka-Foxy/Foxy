use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError as StdTryRecvError;
use std::time::{Duration, Instant};

use log::{info, warn};
use tokio::sync::watch;

use crate::core::api::SyncMode;
use crate::ui::app::{DirectDownloadProgressEvent, DirectDownloadSession, Foxy};
use crate::ui::types::DownloadSummary;

impl Foxy {
    pub fn is_direct_download_running(&self) -> bool {
        self.direct_download_session
            .as_ref()
            .map(|session| session.is_running())
            .unwrap_or(false)
    }

    pub fn effective_direct_download_speed_limit_mbps(&self) -> Option<u32> {
        if self.direct_download_use_global_speed_limit {
            self.settings_view_state
                .download_speed_limit_mbps
                .filter(|limit| *limit > 0)
        } else if self.direct_download_override_speed_unlimited {
            None
        } else {
            Some(self.direct_download_override_speed_limit_mbps.max(1))
        }
    }

    pub fn effective_temp_directory(&self) -> String {
        let configured = self.settings_view_state.temp_directory.trim();
        if configured.is_empty() {
            crate::core::utils::app_paths::foxy_data_dir()
                .display()
                .to_string()
        } else {
            configured.to_string()
        }
    }

    pub fn initialize_direct_download_destination_if_empty(&mut self) {
        if !self.direct_download_destination_input.trim().is_empty() {
            return;
        }
        self.direct_download_destination_input = self.effective_temp_directory();
    }

    pub fn start_direct_download(&mut self) -> bool {
        if self.repository_sync_active() {
            self.direct_download_error = Some(self.t("Another download is already running"));
            warn!("Direct download ignored: repository sync is currently active");
            return false;
        }
        if self.is_direct_download_running() {
            self.direct_download_error = Some(self.t("Direct download is already running"));
            warn!("Direct download ignored: another direct download is currently active");
            return false;
        }

        let source_url = self.direct_download_url_input.trim().to_string();
        if source_url.is_empty() {
            self.direct_download_error = Some(self.t("Address is required"));
            return false;
        }

        self.initialize_direct_download_destination_if_empty();
        let destination_folder = self.direct_download_destination_input.trim().to_string();
        if destination_folder.is_empty() {
            self.direct_download_error = Some(self.t("Destination folder is required"));
            return false;
        }

        let destination_path = PathBuf::from(&destination_folder);
        if !destination_path.exists()
            && let Err(err) = fs::create_dir_all(&destination_path)
        {
            self.direct_download_error = Some(format!(
                "{}: {}",
                self.t("Failed to create destination folder"),
                err
            ));
            return false;
        }
        if !destination_path.is_dir() {
            self.direct_download_error = Some(self.t("Destination path is not a folder"));
            return false;
        }

        let speed_limit_mbps = self.effective_direct_download_speed_limit_mbps();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel::<DirectDownloadProgressEvent>();
        let (download_pause_tx, download_pause_rx) = watch::channel(false);
        let started_at = Instant::now();

        self.direct_download_error = None;
        self.direct_download_progress_rx = Some(progress_rx);
        self.direct_download_session = Some(DirectDownloadSession {
            source_url: source_url.clone(),
            destination_folder: destination_folder.clone(),
            target_label: self.t("Direct download"),
            files_total: 0,
            files_done: 0,
            total_bytes: 0,
            downloaded_bytes: 0,
            finished_at: None,
            error_message: None,
        });

        self.download_progress = Some((self.t("Preparing direct download"), 0.0));
        self.download_finished = false;
        self.download_finished_repo = None;
        self.download_summary = Some(DownloadSummary {
            mods_updated: 1,
            files_updated: 0,
            parts_updated: 0,
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
        self.download_started_at = Some(started_at);
        self.download_stage_started_at = Some(started_at);
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
        self.recheck_stage_label = None;
        self.recheck_stage_percent = None;
        self.recheck_hash_counter = None;
        self.recheck_hash_part_counter = None;
        self.download_hash_sample_at = None;
        self.download_hash_sample_files = 0;
        self.download_hash_sample_parts = 0;
        if !self.mod_download_progress.is_empty() {
            self.mod_download_progress.clear();
            self.invalidate_update_modal_sort_cache();
        }
        self.current_sync_mode = None;
        self.syncing_repository = None;
        self.direct_download_update_view = true;
        self.update_modal_open = true;
        self.needs_repaint = true;
        let repaint_ctx = self.repaint_ctx.clone();

        self.direct_download_worker = Some(std::thread::spawn(move || {
            Self::run_direct_download_worker(
                source_url,
                destination_path,
                speed_limit_mbps,
                progress_tx,
                download_pause_rx,
                repaint_ctx,
            );
        }));

        info!("Started direct download");
        true
    }

    pub fn poll_direct_download_progress(&mut self) {
        loop {
            let evt = {
                let Some(rx) = self.direct_download_progress_rx.as_mut() else {
                    break;
                };
                match rx.try_recv() {
                    Ok(evt) => Some(evt),
                    Err(StdTryRecvError::Empty) => break,
                    Err(StdTryRecvError::Disconnected) => {
                        self.direct_download_progress_rx = None;
                        break;
                    }
                }
            };

            let Some(evt) = evt else { continue };
            match evt {
                DirectDownloadProgressEvent::PlanResolved {
                    target_label,
                    files_total,
                    total_bytes,
                } => {
                    if let Some(session) = self.direct_download_session.as_mut() {
                        session.target_label = target_label;
                        session.files_total = files_total;
                        session.total_bytes = total_bytes;
                    }
                    if let Some(summary) = self.download_summary.as_mut() {
                        summary.files_updated = files_total;
                        summary.planned_transfer_bytes = total_bytes;
                        summary.full_download_bytes = total_bytes;
                        summary.patch_savings_bytes = 0;
                        summary.patched_files = 0;
                    }
                    self.needs_repaint = true;
                }
                DirectDownloadProgressEvent::Progress {
                    label,
                    percent,
                    files_done,
                    files_total,
                    downloaded_bytes,
                    total_bytes,
                } => {
                    let now = Instant::now();
                    if self.download_stage_started_at.is_none() {
                        self.download_stage_started_at = Some(now);
                    }
                    if self.download_started_at.is_none() {
                        self.download_started_at = Some(now);
                    }

                    if let Some(session) = self.direct_download_session.as_mut() {
                        session.files_done = files_done;
                        session.files_total = files_total;
                        session.downloaded_bytes = downloaded_bytes;
                        if total_bytes > 0 {
                            session.total_bytes = total_bytes;
                        }
                    }

                    self.download_progress = Some((label, percent.clamp(0.0, 1.0)));
                    self.download_finished = false;
                    self.download_finished_repo = None;
                    self.total_downloaded_bytes = downloaded_bytes;
                    self.update_download_speed();
                    self.needs_repaint = true;
                }
                DirectDownloadProgressEvent::Finished {
                    error_message,
                    files_done,
                    files_total,
                    downloaded_bytes,
                    total_bytes,
                    elapsed,
                } => {
                    let finished_at = Instant::now();
                    let success = error_message.is_none();

                    if let Some(session) = self.direct_download_session.as_mut() {
                        session.files_done = files_done;
                        session.files_total = files_total;
                        session.downloaded_bytes = downloaded_bytes;
                        if total_bytes > 0 {
                            session.total_bytes = total_bytes;
                        }
                        session.finished_at = Some(finished_at);
                        session.error_message = error_message.clone();
                    }

                    if let Some(summary) = self.download_summary.as_mut() {
                        summary.files_updated = files_done;
                        summary.downloaded_bytes = downloaded_bytes;
                        summary.download_stage_duration = elapsed;
                        summary.cumulative_hash_duration = Duration::from_secs(0);
                        summary.after_download_hash_duration = Duration::from_secs(0);
                        summary.hash_stage_duration = Duration::from_secs(0);
                        summary.total_duration = elapsed;
                        summary.avg_speed_bps = if elapsed.as_secs_f64() > 0.0 {
                            downloaded_bytes as f64 / elapsed.as_secs_f64()
                        } else {
                            0.0
                        };
                    } else {
                        self.download_summary = Some(DownloadSummary {
                            mods_updated: 1,
                            files_updated: files_done,
                            parts_updated: 0,
                            downloaded_bytes,
                            planned_transfer_bytes: total_bytes,
                            full_download_bytes: total_bytes,
                            patch_savings_bytes: 0,
                            patched_files: 0,
                            download_stage_duration: elapsed,
                            cumulative_hash_duration: Duration::from_secs(0),
                            after_download_hash_duration: Duration::from_secs(0),
                            hash_stage_duration: Duration::from_secs(0),
                            total_duration: elapsed,
                            avg_speed_bps: if elapsed.as_secs_f64() > 0.0 {
                                downloaded_bytes as f64 / elapsed.as_secs_f64()
                            } else {
                                0.0
                            },
                            telemetry_samples: Vec::new(),
                        });
                    }

                    if success {
                        self.invalidate_addon_inventory_cache();
                        self.download_progress = Some(("Finished".to_string(), 1.0));
                    } else {
                        self.download_progress = Some((self.t("Download failed"), 1.0));
                        if let Some(message) = error_message {
                            self.direct_download_error = Some(message.clone());
                            log::error!("Direct download failed: {}", message);
                        }
                    }
                    self.download_finished = true;
                    self.download_finished_repo = None;
                    self.download_pause_tx = None;
                    self.download_paused = false;
                    self.download_eta_remaining = None;
                    self.download_eta_updated_at = None;
                    self.download_speed_sample_at = None;
                    self.download_speed_sample_bytes = self.total_downloaded_bytes;
                    self.download_speed_bps = 0.0;
                    self.direct_download_progress_rx = None;
                    if let Some(ref handle) = self.direct_download_worker
                        && handle.is_finished()
                        && let Some(h) = self.direct_download_worker.take()
                    {
                        let _ = h.join();
                    }
                    self.needs_repaint = true;
                }
            }
        }
    }

    pub fn cancel_direct_download(&mut self) {
        if !self.is_direct_download_running() {
            return;
        }
        info!("Cancelling direct download");
        // Drop the progress receiver and worker to signal cancellation
        self.direct_download_progress_rx = None;
        if let Some(handle) = self.direct_download_worker.take() {
            // The worker will naturally stop when its progress_tx send fails
            drop(handle);
        }
        // Clean up UI state
        let cancelled_message = self.t("Cancelled by user");
        if let Some(session) = self.direct_download_session.as_mut() {
            session.finished_at = Some(std::time::Instant::now());
            session.error_message = Some(cancelled_message);
        }
        self.download_progress = None;
        self.download_finished = false;
        self.download_finished_repo = None;
        self.download_pause_tx = None;
        self.download_paused = false;
        self.download_eta_remaining = None;
        self.download_eta_updated_at = None;
        self.download_speed_bps = 0.0;
        self.update_modal_open = false;
        self.direct_download_update_view = false;
        self.needs_repaint = true;
    }

    pub fn cancel_sync(&mut self) {
        let is_syncing = self.syncing_repository.is_some() || self.current_sync_mode.is_some();
        let direct_running = self.is_direct_download_running();
        if !is_syncing && !direct_running {
            return;
        }

        let mut cancel_already_requested = false;
        if let Some(tx) = self.cancel_tx.as_ref() {
            cancel_already_requested = *tx.borrow();
            let _ = tx.send(true);
        }

        if is_syncing {
            if cancel_already_requested {
                return;
            }
            if self.current_sync_mode == Some(SyncMode::Download) {
                self.download_progress = Some((self.t("Cancelling..."), 0.84));
                self.download_paused = false;
                self.needs_repaint = true;
            } else {
                self.recheck_stage_label = Some(self.t("Cancelling..."));
                self.recheck_stage_percent = self.recheck_stage_percent.or(Some(0.95));
                self.needs_repaint = true;
            }
            if let Some(repo_idx) = self.syncing_repository
                && let Some(repo) = self.repository_view_state.repositories.get(repo_idx)
            {
                info!(
                    "Cancelling sync for repository {} (mode={:?})",
                    repo.name, self.current_sync_mode
                );
            } else {
                info!("Cancelling active sync operation");
            }
        } else if direct_running {
            info!("Cancelling direct download");
        }
    }

    pub fn set_download_paused(&mut self, paused: bool) {
        let direct_running = self.is_direct_download_running();
        if self.current_sync_mode != Some(SyncMode::Download) && !direct_running {
            return;
        }
        if self.download_paused == paused {
            return;
        }

        let Some(tx) = self.download_pause_tx.as_ref() else {
            warn!("Download pause toggle ignored: pause channel unavailable");
            return;
        };

        if tx.send(paused).is_err() {
            warn!("Download pause toggle ignored: sync worker is no longer listening");
            return;
        }

        self.download_paused = paused;
        self.download_eta_remaining = None;
        self.download_eta_updated_at = None;
        self.download_speed_sample_at = None;
        self.download_speed_sample_bytes = self.total_downloaded_bytes;
        if paused {
            self.download_speed_bps = 0.0;
        }
        self.needs_repaint = true;

        if let Some(repo_idx) = self.syncing_repository
            && let Some(repo) = self.repository_view_state.repositories.get(repo_idx)
        {
            if paused {
                info!("Paused download for repository {}", repo.name);
            } else {
                info!("Resumed download for repository {}", repo.name);
            }
            return;
        }

        if direct_running {
            if paused {
                info!("Paused direct download");
            } else {
                info!("Resumed direct download");
            }
            return;
        }

        if paused {
            info!("Paused download");
        } else {
            info!("Resumed download");
        }
    }

    pub fn update_download_speed(&mut self) {
        if self.download_paused {
            return;
        }
        let now = Instant::now();
        let Some(sample_at) = self.download_speed_sample_at else {
            self.download_speed_sample_at = Some(now);
            self.download_speed_sample_bytes = self.total_downloaded_bytes;
            return;
        };
        let elapsed = now.duration_since(sample_at);
        if elapsed < Duration::from_millis(250) {
            return;
        }
        let delta_bytes = self
            .total_downloaded_bytes
            .saturating_sub(self.download_speed_sample_bytes);
        let secs = elapsed.as_secs_f64();
        if secs > 0.0 {
            let instant_bps = delta_bytes as f64 / secs;
            if self.download_speed_bps <= 0.0 {
                self.download_speed_bps = instant_bps;
            } else {
                let alpha = 0.2;
                self.download_speed_bps =
                    (alpha * instant_bps) + ((1.0 - alpha) * self.download_speed_bps);
            }
        }
        self.download_speed_sample_at = Some(now);
        self.download_speed_sample_bytes = self.total_downloaded_bytes;
    }

    pub fn update_download_estimate(&mut self, total_bytes: u64) {
        if self.current_sync_mode != Some(SyncMode::Download) {
            return;
        }
        if self.download_paused {
            return;
        }
        let now = Instant::now();
        if let Some(last_update) = self.download_eta_updated_at
            && now.duration_since(last_update) < Duration::from_secs(1)
        {
            return;
        }
        if total_bytes == 0 || self.download_speed_bps <= 0.0 {
            return;
        }
        let downloaded = self.total_downloaded_bytes.min(total_bytes);
        let remaining_bytes = total_bytes.saturating_sub(downloaded);
        let remaining_secs = (remaining_bytes as f64 / self.download_speed_bps).max(0.0);
        self.download_eta_remaining = Some(Duration::from_secs_f64(remaining_secs));
        self.download_eta_updated_at = Some(now);
    }
}
