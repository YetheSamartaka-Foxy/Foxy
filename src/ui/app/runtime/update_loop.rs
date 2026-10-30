use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, CentralPanel, CursorIcon, Frame, Id, PointerButton, Rect, ResizeDirection, Sense, Ui,
    Vec2, ViewportCommand,
};
use log::{info, warn};

use crate::core::api::SyncMode;
use crate::ui::app::debug_modals::DebugModal;
use crate::ui::app::runtime::frame_cost::{FrameSection, SectionTimer};
use crate::ui::app::{Foxy, FoxyView};
use crate::ui::tray::{TrayEvent, TrayManager};

/// Frames kept for the stall percentiles: a few seconds at a probe-driven
/// repaint rate.
const FRAME_INTERVAL_WINDOW: usize = 240;

/// How soon a frame follows a state change the UI flagged with `needs_repaint`.
const STATE_CHANGE_REPAINT_DELAY: Duration = Duration::from_millis(16);

/// Where the window is for progress pacing. A GUI-harness session stays in the
/// foreground pacing so its frame measurements compare across runs, whatever
/// window the operating system gives focus.
fn window_presence_for(
    harness_attached: bool,
    hidden_to_tray: bool,
    focused: Option<bool>,
    minimized: Option<bool>,
) -> crate::ui::app::WindowPresence {
    use crate::ui::app::WindowPresence;
    if harness_attached {
        WindowPresence::Foreground
    } else if hidden_to_tray || minimized == Some(true) {
        WindowPresence::Hidden
    } else if focused == Some(false) {
        WindowPresence::Background
    } else {
        WindowPresence::Foreground
    }
}

/// `(p50, p95, max)` of the recent frame intervals in milliseconds.
pub(crate) fn frame_interval_stats(intervals: &VecDeque<f32>) -> Option<(f32, f32, f32)> {
    if intervals.is_empty() {
        return None;
    }
    let mut sorted: Vec<f32> = intervals.iter().copied().collect();
    sorted.sort_by(f32::total_cmp);
    let at = |pct: usize| sorted[((sorted.len() - 1) * pct) / 100];
    Some((at(50), at(95), sorted[sorted.len() - 1]))
}

impl Foxy {
    fn service_proxy_change_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.proxy_change_prompt.as_ref() else {
            return;
        };
        let mut apply_to_new_work = false;
        let mut restart = false;
        egui::Modal::new(egui::Id::unique("proxy_change_active_work")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading(self.t("Proxy settings changed during active work"));
            if prompt.restart_requested {
                ui.label(self.t("Waiting for the current attempt to cancel and restore its files."));
            } else {
                ui.label(self.t("The current attempt keeps its existing connection. Choose when the new proxy settings should affect this work."));
                ui.horizontal(|ui| {
                    apply_to_new_work = ui.button(self.t("Apply to new work")).clicked();
                    restart = ui.button(self.t("Restart this work now")).clicked();
                });
            }
        });
        if apply_to_new_work {
            self.proxy_change_prompt = None;
        } else if restart && let Some(prompt) = self.proxy_change_prompt.as_mut() {
            prompt.restart_requested = true;
            self.cancel_sync();
        }

        let Some(prompt) = self.proxy_change_prompt.as_ref() else {
            return;
        };
        if !prompt.restart_requested
            || self.repository_sync_active()
            || self.current_sync_mode.is_some()
            || self
                .backend_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
        {
            return;
        }
        let prompt = self.proxy_change_prompt.take().expect("prompt was checked");
        let repo_index = self
            .repository_view_state
            .repositories
            .iter()
            .position(|repo| {
                repo.address == prompt.repository_url && repo.path == prompt.repository_path
            });
        if let Some(repo_index) = repo_index {
            if prompt.mode == SyncMode::Download {
                self.supersede_pending_retry_for_instance(
                    &prompt.repository_url,
                    &prompt.repository_path,
                );
            }
            let previous_backup_local_drift = self.backup_local_drift_requested;
            self.backup_local_drift_requested = prompt.backup_local_drift;
            self.start_core_sync_internal(
                repo_index,
                prompt.mode,
                Some(prompt.selected_mod_states),
                prompt.force_redownload,
                prompt.prepare_download_plan,
                prompt.force_full_downloads,
            );
            self.backup_local_drift_requested = previous_backup_local_drift;
        } else {
            self.show_error_toast(self.t("The repository changed before the work could restart."));
        }
    }

    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        let mut restore_requested = false;

        if let Some(tray_manager) = self.tray_manager.as_mut() {
            for event in tray_manager.drain_events() {
                if matches!(event, TrayEvent::RestoreRequested) {
                    restore_requested = true;
                }
            }
        }

        if restore_requested {
            if let Some(tray_manager) = self.tray_manager.as_ref() {
                tray_manager.hide_icon();
            }
            self.hidden_to_tray = false;
            info!("Restoring window from tray");
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    pub fn request_app_close(&mut self, ctx: &egui::Context, reason: &str) {
        if self.close_requested_at.is_none() {
            self.persist_window_state_if_changed(ctx);
            self.close_requested_at = Some(Instant::now());
            if self.shutdown_after_close {
                self.launch_action.invalidate_shutdown_sample();
            }
            info!("Window close requested ({reason})");
        }
        if let Some(tray_manager) = self.tray_manager.as_ref() {
            tray_manager.hide_icon();
        }
        self.hidden_to_tray = false;
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    pub fn hide_app_to_tray(&mut self, ctx: &egui::Context, reason: &str) {
        if self.tray_manager.is_none() {
            self.tray_manager = TrayManager::new(ctx.clone());
        }

        if let Some(tray_manager) = self.tray_manager.as_ref() {
            tray_manager.show_icon();
            self.hidden_to_tray = true;
            info!("Window hidden to tray ({reason})");
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            return;
        }

        warn!("Tray support unavailable; window will remain visible");
    }

    pub(crate) fn request_background_repaint(repaint_ctx: Option<&egui::Context>) {
        if let Some(ctx) = repaint_ctx {
            ctx.request_repaint();
        }
    }

    /// Update the smoothed frames-per-second estimate that backs the optional
    /// on-screen FPS counter and the agent GUI `fps` probe. While the counter
    /// is shown, or for a short window after each `fps` read, the UI is kept
    /// repainting continuously so the readout stays live; otherwise this
    /// resets the running average and the UI repaints only when it needs to.
    fn update_fps_estimate(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let agent_gui_probe = self
            .agent_gui
            .as_ref()
            .is_some_and(|runtime| runtime.fps_probe_active(now));
        if !self.settings_view_state.show_fps_counter && !agent_gui_probe {
            self.fps_ema = 0.0;
            self.frame_intervals_ms.clear();
            return;
        }
        if let Some(runtime) = self.agent_gui.as_mut()
            && std::mem::take(&mut runtime.fps_probe_warming)
        {
            ctx.request_repaint();
            return;
        }

        let dt = ctx.input(|i| i.stable_dt);
        if dt > 0.0 {
            if self.frame_intervals_ms.len() >= FRAME_INTERVAL_WINDOW {
                self.frame_intervals_ms.pop_front();
            }
            self.frame_intervals_ms.push_back(dt * 1000.0);
            let instant_fps = 1.0 / dt;
            self.fps_ema = if self.fps_ema <= 0.0 {
                instant_fps
            } else {
                self.fps_ema * 0.9 + instant_fps * 0.1
            };
        }

        // Repaint every frame so the counter or opt-in agent probe reflects the
        // current framerate; out of front the counter follows the slower pacing.
        if agent_gui_probe
            || crate::ui::app::window_presence() == crate::ui::app::WindowPresence::Foreground
        {
            ctx.request_repaint();
        }
    }

    /// Color for the FPS readout in the footer: green when healthy, amber when
    /// middling, red when low. Shared so the value tracks the same thresholds the
    /// overlay used.
    pub(crate) fn fps_readout_color(&self) -> egui::Color32 {
        if self.fps_ema >= 90.0 {
            self.color_success()
        } else if self.fps_ema >= 45.0 {
            self.color_warn()
        } else {
            self.color_text_error()
        }
    }

    fn merge_repaint_interval(next_interval: &mut Option<Duration>, candidate: Duration) {
        match next_interval {
            Some(current) if *current <= candidate => {}
            _ => *next_interval = Some(candidate),
        }
    }

    fn next_visible_repaint_interval(&self) -> Option<Duration> {
        const DOWNLOAD_PROGRESS_REPAINT_INTERVAL: Duration = Duration::from_millis(125);
        const STATUS_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
        const ACTIVITY_LOG_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
        const MEMORY_DIAGNOSTICS_REPAINT_INTERVAL: Duration = Duration::from_millis(500);
        const PERSISTENCE_REPAINT_INTERVAL: Duration = Duration::from_millis(100);

        let mut interval = None;

        if self.settings_view_state.periodic_remote_check_enabled
            && self.startup_tasks_started
            && !self.repository_sync_active()
            && self.quick_scan_worker.is_none()
            && !self.is_direct_download_running()
            && self.scheduler_active_run.is_none()
            && self.startup_recheck_queue.is_empty()
        {
            let period = Duration::from_secs(
                u64::from(
                    self.settings_view_state
                        .periodic_remote_check_interval_minutes
                        .max(5),
                )
                .saturating_mul(60),
            );
            let until_due = self
                .periodic_remote_check_last_run
                .map(|last| period.saturating_sub(last.elapsed()))
                .unwrap_or(Duration::ZERO);
            Self::merge_repaint_interval(&mut interval, until_due.max(Duration::from_millis(16)));
        }

        let repository_download_visible = self.update_modal_open
            && self.current_sync_mode == Some(SyncMode::Download)
            && self.syncing_repository.is_some();
        let direct_download_visible = self.update_modal_open
            && self.direct_download_update_view
            && self.is_direct_download_running();
        if repository_download_visible || direct_download_visible {
            Self::merge_repaint_interval(&mut interval, DOWNLOAD_PROGRESS_REPAINT_INTERVAL);
        }

        let repository_status_visible = self.current_view == FoxyView::RepositoryList
            && (self.syncing_repository.is_some()
                || !self.pending_repository_db_wipes.is_empty()
                // Keep the startup database maintenance banner's elapsed time live.
                || crate::core::tasks::db_turso::db_startup_compaction_active());
        let quick_scan_status_visible = self.current_view == FoxyView::RepositoryList
            && !self.active_quick_scan_instance_keys.is_empty();
        let addon_backup_status_visible = self.current_view == FoxyView::RepositorySettings
            && self.addon_backup_status.as_ref().is_some_and(|status| {
                self.selected_repository_for_settings == Some(status.repo_index)
            });
        if repository_status_visible
            || quick_scan_status_visible
            || addon_backup_status_visible
            || self.database_wipe_in_flight
        {
            Self::merge_repaint_interval(&mut interval, STATUS_REPAINT_INTERVAL);
        }

        // Keep ticking while a Join waits on its background server-status query
        // so the result is applied (and the timeout fallback fires) promptly.
        if self.pending_join_status_query.is_some() {
            Self::merge_repaint_interval(&mut interval, STATUS_REPAINT_INTERVAL);
        }

        if self.settings_view_state.show_activity_log {
            Self::merge_repaint_interval(&mut interval, ACTIVITY_LOG_REPAINT_INTERVAL);
        }
        if self.show_memory_diagnostics_window {
            Self::merge_repaint_interval(&mut interval, MEMORY_DIAGNOSTICS_REPAINT_INTERVAL);
        }
        if self.settings_dirty
            || self.settings_save_in_flight_revision.is_some()
            || self.repositories_dirty
            || self.repositories_save_in_flight_revision.is_some()
            || self.backup_inventory_refresh_requested
            || self.backup_inventory_refresh_in_progress
            || self.backup_cleanup_in_flight
        {
            Self::merge_repaint_interval(&mut interval, PERSISTENCE_REPAINT_INTERVAL);
        }

        // Keep waking to service scheduled jobs (next due fire, an in-progress
        // run, or a post-action countdown), even when the app is otherwise idle.
        if let Some(scheduler_interval) = self.scheduler_repaint_interval() {
            Self::merge_repaint_interval(&mut interval, scheduler_interval);
        }

        match crate::ui::app::progress_frame_interval_override() {
            Some(cap) => interval.map(|interval| interval.max(cap)),
            None => interval,
        }
    }

    pub fn update(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let repaint_causes = ui.ctx().repaint_causes();
        self.frame_cost.begin_frame(
            crate::core::utils::thread_cpu::current_thread_cpu(),
            repaint_causes.iter().map(|cause| (cause.file, cause.line)),
        );
        let polls = SectionTimer::start(FrameSection::Polls);
        let ctx = ui.ctx().clone();
        self.repaint_ctx = Some(ctx.clone());
        self.apply_runtime_palette_visuals(&ctx);

        if self.db_lock_conflict.is_some() {
            self.render_db_lock_conflict_prompt(&ctx);
            polls.stop(&mut self.frame_cost);
            return;
        }

        super::keyboard_focus::repair_lost_keyboard_focus();
        if self.render_configuration_recovery_error(&ctx) {
            polls.stop(&mut self.frame_cost);
            return;
        }
        self.apply_runtime_ui_scale(&ctx);
        self.apply_runtime_motion_preference(&ctx);
        self.invalidate_galley_caches_on_font_atlas_change(&ctx);
        self.log_display_metrics_if_changed(&ctx);
        let (focused, minimized) = ctx.input(|i| (i.viewport().focused, i.viewport().minimized));
        crate::ui::app::set_window_presence(window_presence_for(
            self.agent_gui.is_some(),
            self.hidden_to_tray,
            focused,
            minimized,
        ));
        self.update_fps_estimate(&ctx);
        self.poll_agent_gui(&ctx);
        self.handle_global_accessibility_shortcuts(&ctx);

        if self.close_requested_at.is_none() && ctx.input(|i| i.viewport().close_requested()) {
            self.persist_window_state_if_changed(&ctx);
            self.close_requested_at = Some(Instant::now());
            info!("Close requested by viewport event");
        }
        self.poll_launch_action(&ctx);
        self.poll_game_space_removal(&ctx);
        self.poll_scheduled_gameplay(&ctx);
        self.service_proxy_change_prompt(&ctx);
        self.render_pending_addon_backup_origin_confirmation(&ctx);
        self.poll_installer_request(&ctx);

        // Killing a sync mid-promotion leaves a mix of old and new files its
        // rollback can no longer revert, so a close cancels it and keeps the
        // window (showing the cancel) until the worker has rolled back.
        let close_waits_for_worker = self.close_requested_at.is_some()
            && (self
                .backend_worker
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
                || self.is_direct_download_running()
                || self.retained_files_cleanup_in_flight
                || self.hash_evidence_retry_in_flight
                || self.launch_action.is_busy()
                || self.game_space_removal_in_flight()
                || self
                    .workshop_task_worker
                    .as_ref()
                    .is_some_and(|worker| !worker.is_finished())
                || self
                    .addon_backup_worker
                    .as_ref()
                    .is_some_and(|worker| !worker.is_finished())
                || self
                    .backup_cleanup_worker
                    .as_ref()
                    .is_some_and(|worker| !worker.is_finished()));
        if close_waits_for_worker {
            self.close_worker_wait_started_at.get_or_insert_with(|| {
                info!(
                    "Close requested while a sync runs; cancelling it and waiting for its rollback"
                );
                Instant::now()
            });
            self.cancel_sync_and_queued_runs();
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if self.close_requested_at.is_some() {
            self.poll_backend_progress();
            if self.close_requested_at.is_none() {
                return;
            }
            if let Some(wait_started_at) = self.close_worker_wait_started_at.take() {
                info!(
                    "Cancelled sync finished {:.1}s after the close request",
                    wait_started_at.elapsed().as_secs_f64()
                );
                // The persistence flush gets its own window after the wait.
                self.close_requested_at = Some(Instant::now());
            }
            self.poll_persistence_results();
            self.maybe_dispatch_persistence_requests(true);
            if self.has_pending_persistence_writes() {
                if self.shutdown_after_close {
                    self.launch_action.invalidate_shutdown_sample();
                }
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                if self.show_unreadable_config_replacement {
                    self.render_unreadable_config_replacement(&ctx);
                } else {
                    self.render_close_wait(&ctx, true);
                }
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }

            #[cfg(target_os = "windows")]
            if self.shutdown_after_close {
                if !self.launch_action.ensure_shutdown_sample_ready() {
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                let gameplay_changed =
                    self.scheduler_pending_post_action
                        .as_ref()
                        .is_some_and(|pending| {
                            pending.gameplay_generation != self.launch_action.gameplay_generation
                        });
                if gameplay_changed {
                    if let Some(pending) = self.scheduler_pending_post_action.as_mut() {
                        pending.requires_confirmation = true;
                        pending.gameplay_generation = self.launch_action.gameplay_generation;
                        pending.handoff_generation = self.launch_action.handoff_generation;
                    }
                    self.close_requested_at = None;
                    self.close_worker_wait_started_at = None;
                    self.shutdown_after_close = false;
                    ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                    ctx.request_repaint();
                    return;
                }
                self.shutdown_after_close = false;
                if let Err(error) = crate::ui::app::scheduling::initiate_pc_shutdown() {
                    self.close_requested_at = None;
                    self.show_error_toast(
                        self.t_fmt("Could not request shutdown: {error}", &[("error", error)]),
                    );
                    return;
                }
                self.scheduler_pending_post_action = None;
                self.scheduler_deferred_post_actions.clear();
            }
            ctx.send_viewport_cmd(ViewportCommand::Close);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }

        self.poll_persistence_results();
        self.process_pending_game_space_switch(&ctx);
        self.poll_update_recovery();
        if self.db_lock_conflict.is_some() {
            self.render_db_lock_conflict_prompt(&ctx);
            polls.stop(&mut self.frame_cost);
            return;
        }
        self.refresh_live_database_repair_requirement();

        if self.startup_frame_rendered
            && !self.startup_tasks_started
            && !self.database_wipe_in_flight
        {
            self.startup_tasks_started = true;
            self.begin_startup_sync_tracking(self.startup_first_frame_at.unwrap_or_default());
            self.start_database_startup_tasks();
            if std::mem::take(&mut self.recheck_all_after_database_wipe) {
                self.queue_recheck_all_repositories();
            }
            self.start_startup_ts3_plugin_scan();
            self.start_repository_space_freshness_probe();
            self.maybe_auto_fill_app_update_url_from_metadata();
            // A real check would overwrite the seeded preview status.
            if !self.previewing_debug_modal(DebugModal::AppUpdate)
                && self.settings_view_state.app_update_auto_check
                && self.app_update_source_configured()
            {
                self.app_update_prompt_armed = true;
                self.start_update_check();
            }
        }

        if self.startup_tasks_started {
            self.maybe_recheck_app_update();
            self.maybe_recheck_repository_spaces();
        }
        self.poll_repository_space_freshness_results(&ctx);
        self.poll_startup_diagnostics();
        self.poll_mission_scan();
        self.poll_restore_pending_updates();
        self.poll_startup_quick_scan_filter_results();
        self.service_startup_probe_retry(&ctx);
        if self.startup_tasks_started {
            self.refresh_fs_watcher_if_stale();
        }
        self.poll_fs_watch_results();
        self.poll_quick_scan_progress();
        self.poll_quick_scan_results();
        self.poll_repository_db_wipe_results();
        self.poll_database_wipe_result(&ctx);
        self.poll_addon_delete_results();
        self.poll_addon_force_redownload_results();
        self.poll_addon_backup_results();
        self.poll_addon_workshop_lookups(&ctx);
        self.poll_repository_settings_addon_preload_results();
        self.maybe_warm_addon_inventory();
        self.poll_repository_addon_size_load_results();
        self.poll_persistence_results();
        if self.pending_game_space_switch.is_some() {
            self.render_pending_game_space_switch(&ctx);
            polls.stop(&mut self.frame_cost);
            return;
        }
        self.poll_backend_progress();
        self.poll_linked_target_preview_results();
        self.poll_hash_evidence_retry();
        self.poll_retained_file_task_results();
        self.poll_finished_backend_worker();
        self.process_pending_integrity_recheck();
        self.poll_benchmark_save_results();
        self.tick_benchmark_capture(&ctx);
        self.poll_benchmark_export_screenshot(&ctx);
        self.poll_join_preflight_results(&ctx);
        if self.close_requested_at.is_some() {
            self.render_close_wait(&ctx, false);
            polls.stop(&mut self.frame_cost);
            return;
        }
        self.render_corrupt_database_recovery(&ctx);
        self.render_unreadable_config_replacement(&ctx);
        if self.pending_game_space_switch.is_some() {
            self.render_pending_game_space_switch(&ctx);
            polls.stop(&mut self.frame_cost);
            return;
        }
        self.render_queued_update_launch(&ctx);
        self.service_download_retry(&ctx);
        // Drain any pending repo.json metadata refreshes queued by sync completion.
        if !self.pending_repo_metadata_refresh.is_empty() {
            let indices: Vec<usize> = self.pending_repo_metadata_refresh.drain(..).collect();
            for idx in indices {
                self.update_repository_from_url(idx, &ctx);
            }
        }
        self.poll_direct_download_progress();
        self.poll_workshop_task();
        self.poll_app_update_events();
        self.poll_image_results(&ctx);
        self.poll_repo_metadata_results(&ctx);
        self.poll_repository_space_import_results(&ctx);
        self.poll_addon_hash_recalc_results();
        self.poll_cached_update_load_results();
        self.poll_ts3_plugin_scan();
        self.maybe_dispatch_persistence_requests(false);
        self.maybe_queue_periodic_remote_checks();
        self.process_startup_rechecks();
        self.maybe_log_startup_repository_layout();
        self.process_repository_space_sync_queue();
        self.process_repository_visual_folder_sync_queue();
        self.process_scheduled_jobs(&ctx);
        self.process_addon_selection_checks();
        self.process_addon_hash_recalc_queue();
        self.maybe_sample_memory_diagnostics();
        self.maybe_emit_startup_sol();
        self.handle_tray_events(&ctx);

        while let Ok((address, port, status)) = self.server_updates.try_recv() {
            self.pending_server_queries
                .remove(&(address.clone(), port.clone()));
            self.server_statuses.insert(
                (address.clone(), port.clone()),
                crate::ui::types::ServerStatusCache {
                    last_check: Instant::now(),
                    status,
                },
            );
        }

        // Resume any Join that was waiting on a background server-status query.
        self.poll_pending_join_status(&ctx);
        self.render_pending_join_status(&ctx);

        if self.needs_repaint {
            // One 60 Hz frame later, not at once: while progress events keep
            // arriving, an immediate repaint chains frames at the display rate.
            let delay = crate::ui::app::progress_frame_interval_override()
                .map_or(STATE_CHANGE_REPAINT_DELAY, |cap| {
                    cap.max(STATE_CHANGE_REPAINT_DELAY)
                });
            crate::ui::app::request_frame_after(&ctx, delay);
            self.needs_repaint = false;
        } else if let Some(repaint_interval) = self.next_visible_repaint_interval() {
            ctx.request_repaint_after(repaint_interval);
        }

        self.pending_queries.retain(|h| !h.is_finished());
        if self
            .join_preflight_worker
            .as_ref()
            .is_some_and(|h| h.is_finished())
        {
            self.join_preflight_worker = None;
        }

        if self.settings_view_state.debug_mode != self.previous_debug_mode {
            self.update_debug_mode();
            self.previous_debug_mode = self.settings_view_state.debug_mode;
        }

        if self.show_debug_windows {
            egui::Window::new(self.t("Settings")).show(&ctx, |ui| {
                let ctx = ui.ctx().clone();
                ctx.settings_ui(ui)
            });
            egui::Window::new(self.t("Inspection")).show(&ctx, |ui| {
                let ctx = ui.ctx().clone();
                ctx.inspection_ui(ui)
            });
            egui::Window::new(self.t("Memory")).show(&ctx, |ui| {
                let ctx = ui.ctx().clone();
                ctx.memory_ui(ui)
            });
            egui::Window::new(self.t("Textures")).show(&ctx, |ui| {
                let ctx = ui.ctx().clone();
                ctx.texture_ui(ui)
            });
        }
        if self.show_memory_diagnostics_window {
            self.render_memory_diagnostics_window(&ctx);
        }

        polls.stop(&mut self.frame_cost);
        let panel_frame = Frame {
            fill: ctx.global_style().visuals.window_fill(),
            ..Default::default()
        };
        CentralPanel::default().frame(panel_frame).show(ui, |ui| {
            let main_view = SectionTimer::start(FrameSection::MainView);
            self.render_main_view(ui, frame);
            main_view.stop(&mut self.frame_cost);
            let content = SectionTimer::start(FrameSection::Content);
            ui.push_id(
                (
                    "main_content_area",
                    self.settings_view_state.show_activity_log,
                ),
                |ui| {
                    if self.update_modal_open {
                        self.render_repository_update_view(ui);
                    } else {
                        match self.current_view {
                            FoxyView::RepositoryList => {
                                self.render_repository_view(ui, frame);
                            }
                            FoxyView::Settings => {
                                self.render_settings_view(ui, frame);
                            }
                            FoxyView::RepositorySettings => {
                                self.render_repository_settings_view(ui, frame);
                            }
                            FoxyView::RepositorySpaceSettings => {
                                self.render_repository_space_settings_view(ui, frame);
                            }
                            FoxyView::Help => {
                                self.render_help_view(ui, frame);
                            }
                            FoxyView::Changelog => {
                                self.render_changelog_view(ui, frame);
                            }
                            FoxyView::About => {
                                self.render_about_view(ui, frame);
                            }
                            FoxyView::AppUpdate => {
                                self.render_app_update_view(ui, frame);
                            }
                            FoxyView::VersionBrowser => {
                                self.render_version_browser_view(ui, frame);
                            }
                            FoxyView::SwiftyMigration => {
                                self.render_swifty_migration_view(ui, frame);
                            }
                            FoxyView::GameSpaces => {
                                self.render_game_spaces_view(ui, frame);
                            }
                            FoxyView::GameSpaceSettings => {
                                self.render_game_space_settings_view(ui, frame);
                            }
                            FoxyView::None => {}
                        }
                    }
                },
            );
            content.stop(&mut self.frame_cost);
            let can_use_custom_resize = !self.main_view_state.use_window_decorations
                && ctx.input(|i| {
                    let viewport = i.viewport();
                    !viewport.minimized.unwrap_or(false)
                        && !viewport.maximized.unwrap_or(false)
                        && viewport.focused.unwrap_or(true)
                });
            if can_use_custom_resize {
                resize(ui);
            }
        });

        if self.pending_low_space_notice {
            self.pending_low_space_notice = false;
            self.show_error_toast(self.t(
                "A drive Foxy writes to is nearly full. Downloads and database writes can fail part-way through. Free up space before syncing.",
            ));
        }

        let overlays = SectionTimer::start(FrameSection::Overlays);
        self.render_ui_toast(&ctx);
        self.render_renderer_fallback_notice(&ctx);
        self.render_storage_compat_notice(&ctx);
        self.render_db_lock_conflict_prompt(&ctx);
        self.render_db_schema_wipe_prompt(&ctx);
        self.render_app_update_prompt(&ctx);
        self.render_legacy_migration_review(&ctx);
        self.render_benchmark_save_prompt(&ctx);
        self.render_scheduled_post_action_overlay(&ctx);
        self.render_integrity_recheck_prompt(&ctx);
        self.render_link_target_enrollment_prompt(&ctx);
        self.render_hash_evidence_notice(&ctx);
        self.render_retained_file_removal_confirmation(&ctx);
        overlays.stop(&mut self.frame_cost);

        if !self.startup_frame_rendered {
            self.startup_frame_rendered = true;
            self.startup_first_frame_at = Some(crate::core::api::process_start_elapsed());
            ctx.request_repaint();
        }
    }

    fn poll_join_preflight_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.join_preflight_result_rx.try_recv() {
            let Some(pending) = self.pending_join_preflight_query.as_ref() else {
                continue;
            };
            if !result.matches_pending(pending) {
                continue;
            }
            let pending = self
                .pending_join_preflight_query
                .take()
                .expect("matching preflight request is present");
            match result.result {
                Ok(query_result) => {
                    self.finish_join_preflight(
                        ctx,
                        &pending.original_repository,
                        &pending.server,
                        &pending.repo_name,
                        &query_result,
                        &result.display_names,
                    );
                }
                Err(err) => {
                    info!(
                        "Join addon preflight query failed for {}:{}: {}",
                        pending.server.address, pending.server.port, err
                    );
                    if let Some(pending) = self.pending_join_preflight.as_mut().filter(|pending| {
                        pending.server.address == result.address
                            && pending.server.port.parse::<u16>().ok() == Some(result.port)
                            && pending.final_confirmation_choice.is_some()
                    }) {
                        pending.final_query_error = Some(err);
                    }
                    self.show_error_toast(
                        self.t("The final Join refresh failed. Retry or cancel the reviewed Join."),
                    );
                    ctx.request_repaint();
                }
            }
        }
        if self.pending_join_preflight_query.is_some()
            && self
                .join_preflight_worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
        {
            let pending = self
                .pending_join_preflight_query
                .take()
                .expect("failed query is present");
            let message = self.t("The Join requirements refresh ended without a result.");
            if let Some(state) = self.pending_join_preflight.as_mut().filter(|state| {
                state.server.address == pending.server.address
                    && state.server.port == pending.server.port
                    && state.final_confirmation_choice.is_some()
            }) {
                state.final_query_error = Some(message);
            } else {
                self.show_error_toast(message);
            }
            ctx.request_repaint();
        }
    }

    fn render_renderer_fallback_notice(&mut self, ctx: &egui::Context) {
        if !self.pending_renderer_fallback_notice {
            return;
        }

        egui::Window::new(self.t("Renderer changed"))
            .frame(self.modal_window_chrome(ctx))
            .title_frame(self.modal_window_chrome(ctx))
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(self.t(
                    "Foxy detected a crash in the WGPU renderer during the previous run. The renderer setting has been changed to Glow so the app can start more reliably.",
                ));
                ui.add_space(10.0);
                ui.label(self.t(
                    "You can change this later in Application settings. Renderer changes take effect after restarting Foxy.",
                ));
                ui.add_space(16.0);

                ui.horizontal_wrapped(|ui| {
                    let ok_btn = ui.button(self.t("OK"));
                    if ok_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if ok_btn.clicked() {
                        self.dismiss_renderer_fallback_notice();
                    }

                    let settings_btn = ui.button(self.t("Open settings"));
                    if settings_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if settings_btn.clicked() {
                        self.current_view = FoxyView::Settings;
                        self.settings_view_state.current_tab = "Application".to_string();
                        self.dismiss_renderer_fallback_notice();
                    }
                    self.render_export_logs_button(ui);
                });
            });
    }

    /// Claim this game space's database for the process, returning the owning
    /// PID slot when another Foxy already holds it.
    pub(crate) fn claim_active_space_database(&self) -> Option<Option<u32>> {
        match crate::core::tasks::db_process_lock::acquire_for_active_space() {
            crate::core::tasks::db_process_lock::LockOutcome::Acquired => None,
            crate::core::tasks::db_process_lock::LockOutcome::Busy { holder_pid } => {
                Some(holder_pid)
            }
            crate::core::tasks::db_process_lock::LockOutcome::Unavailable(reason) => {
                warn!(
                    "Could not verify exclusive database access ({}); continuing",
                    reason
                );
                None
            }
        }
    }

    /// Terminal prompt shown when another Foxy process owns this game space's
    /// database. There is no safe "continue anyway": Turso does not support two
    /// processes on one file, so the only action is to close this window.
    fn render_db_lock_conflict_prompt(&mut self, ctx: &egui::Context) {
        let Some(holder_pid) = self.db_lock_conflict else {
            return;
        };
        let mut close_clicked = false;

        egui::Window::new(self.t("Foxy is already running"))
            .frame(self.modal_window_chrome(ctx))
            .title_frame(self.modal_window_chrome(ctx))
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label(self.t(
                    "Another Foxy process is already using this game space's database. Only one Foxy can use a game space at a time, so this window cannot continue.",
                ));
                if let Some(pid) = holder_pid {
                    ui.add_space(4.0);
                    ui.label(self.t_fmt(
                        "The database is held by process {pid}.",
                        &[("pid", pid.to_string())],
                    ));
                }
                ui.add_space(8.0);
                ui.label(self.t(
                    "Switch to the Foxy window that is already open, or close it and start Foxy again.",
                ));
                ui.add_space(16.0);

                ui.vertical_centered(|ui| {
                    let close_btn = ui.button(self.t("Close this window"));
                    if close_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if close_btn.clicked() {
                        close_clicked = true;
                    }
                });
            });

        if close_clicked {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn dismiss_renderer_fallback_notice(&mut self) {
        self.pending_renderer_fallback_notice = false;
        let marker_path = crate::core::utils::renderer_fallback::renderer_fallback_notice_path();
        if let Err(err) = std::fs::remove_file(&marker_path)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            warn!(
                "Failed to remove renderer fallback notice marker {}: {}",
                marker_path.display(),
                err
            );
        }
    }
}

fn resize_single(
    ui: &mut Ui,
    rect: Rect,
    id: &'static str,
    cursor: CursorIcon,
    direction: ResizeDirection,
) {
    let can_begin_resize = ui.input(|i| {
        let viewport = i.viewport();
        !viewport.minimized.unwrap_or(false)
            && !viewport.maximized.unwrap_or(false)
            && viewport.focused.unwrap_or(true)
    });
    if !can_begin_resize {
        return;
    }

    let response = ui.interact(rect, Id::unique(id), Sense::drag());
    if response.hovered() {
        ui.ctx().output_mut(|o| o.cursor_icon = cursor);
    }
    if response.drag_started_by(PointerButton::Primary) {
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

fn resize(ui: &mut Ui) {
    const EDGE_SIZE: f32 = 10.5;
    const CORNER_SIZE: f32 = 10.5;

    let app_rect = ui.max_rect();

    let top_edge = Rect::from_min_max(
        app_rect.min,
        app_rect.min + Vec2::new(app_rect.width(), EDGE_SIZE),
    );
    let bottom_edge = Rect::from_min_max(
        app_rect.max - Vec2::new(app_rect.width(), EDGE_SIZE),
        app_rect.max,
    );
    let left_edge = Rect::from_min_max(
        app_rect.min,
        app_rect.min + Vec2::new(EDGE_SIZE, app_rect.height()),
    );
    let right_edge = Rect::from_min_max(
        app_rect.max - Vec2::new(EDGE_SIZE, app_rect.height()),
        app_rect.max,
    );

    resize_single(
        ui,
        top_edge,
        "top_resize",
        CursorIcon::ResizeVertical,
        ResizeDirection::North,
    );
    resize_single(
        ui,
        right_edge,
        "right_resize",
        CursorIcon::ResizeHorizontal,
        ResizeDirection::East,
    );
    resize_single(
        ui,
        bottom_edge,
        "bottom_resize",
        CursorIcon::ResizeVertical,
        ResizeDirection::South,
    );
    resize_single(
        ui,
        left_edge,
        "left_resize",
        CursorIcon::ResizeHorizontal,
        ResizeDirection::West,
    );

    let top_left_corner = Rect::from_min_max(
        app_rect.min,
        app_rect.min + Vec2::new(CORNER_SIZE, CORNER_SIZE),
    );
    let top_right_corner = Rect::from_min_max(
        app_rect.min + Vec2::new(app_rect.width() - CORNER_SIZE, 0.0),
        app_rect.min + Vec2::new(app_rect.width(), CORNER_SIZE),
    );
    let bottom_left_corner = Rect::from_min_max(
        app_rect.min + Vec2::new(0.0, app_rect.height() - CORNER_SIZE),
        app_rect.min + Vec2::new(CORNER_SIZE, app_rect.height()),
    );
    let bottom_right_corner = Rect::from_min_max(
        app_rect.max - Vec2::new(CORNER_SIZE, CORNER_SIZE),
        app_rect.max,
    );

    resize_single(
        ui,
        top_left_corner,
        "top_left_resize",
        CursorIcon::ResizeNwSe,
        ResizeDirection::NorthWest,
    );
    resize_single(
        ui,
        top_right_corner,
        "top_right_resize",
        CursorIcon::ResizeNeSw,
        ResizeDirection::NorthEast,
    );
    resize_single(
        ui,
        bottom_right_corner,
        "bottom_right_resize",
        CursorIcon::ResizeNwSe,
        ResizeDirection::SouthEast,
    );
    resize_single(
        ui,
        bottom_left_corner,
        "bottom_left_resize",
        CursorIcon::ResizeNeSw,
        ResizeDirection::SouthWest,
    );
}

impl eframe::App for Foxy {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.agent_gui_raw_input(ctx, input);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from(visuals.window_fill()).to_array()
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        Foxy::update(self, ui, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_interval_stats_report_percentiles_and_the_worst_frame() {
        let mut intervals = VecDeque::new();
        assert_eq!(frame_interval_stats(&intervals), None);
        for value in [16.0, 17.0, 16.5, 16.2, 16.8, 16.4, 16.6, 16.1, 16.9, 250.0] {
            intervals.push_back(value);
        }
        let (p50, p95, max) = frame_interval_stats(&intervals).unwrap();
        assert!((16.0..=17.0).contains(&p50));
        assert!((16.0..=17.0).contains(&p95));
        assert_eq!(max, 250.0);
        intervals.push_back(300.0);
        let (_, _, max) = frame_interval_stats(&intervals).unwrap();
        assert_eq!(max, 300.0);
    }

    #[test]
    fn window_presence_follows_focus_minimize_and_the_tray() {
        use crate::ui::app::WindowPresence;
        assert_eq!(
            window_presence_for(false, false, Some(true), Some(false)),
            WindowPresence::Foreground
        );
        assert_eq!(
            window_presence_for(false, false, None, None),
            WindowPresence::Foreground
        );
        assert_eq!(
            window_presence_for(false, false, Some(false), Some(false)),
            WindowPresence::Background
        );
        assert_eq!(
            window_presence_for(false, false, Some(false), Some(true)),
            WindowPresence::Hidden
        );
        assert_eq!(
            window_presence_for(false, true, Some(true), None),
            WindowPresence::Hidden
        );
    }

    #[test]
    fn a_harness_session_keeps_foreground_pacing() {
        assert_eq!(
            window_presence_for(true, true, Some(false), Some(true)),
            crate::ui::app::WindowPresence::Foreground
        );
    }
}
