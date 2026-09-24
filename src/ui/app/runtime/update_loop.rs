use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, CentralPanel, CursorIcon, Frame, Id, PointerButton, Rect, ResizeDirection, Sense, Ui,
    Vec2, ViewportCommand,
};
use log::{error, info, warn};

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
        // current framerate.
        ctx.request_repaint();
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
        if repository_status_visible || quick_scan_status_visible || addon_backup_status_visible {
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
        {
            Self::merge_repaint_interval(&mut interval, PERSISTENCE_REPAINT_INTERVAL);
        }

        // Keep waking to service scheduled jobs (next due fire, an in-progress
        // run, or a post-action countdown), even when the app is otherwise idle.
        if let Some(scheduler_interval) = self.scheduler_repaint_interval() {
            Self::merge_repaint_interval(&mut interval, scheduler_interval);
        }

        interval
    }

    pub fn update(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        const CLOSE_FORCE_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
        let repaint_causes = ui.ctx().repaint_causes();
        self.frame_cost.begin_frame(
            crate::core::utils::thread_cpu::current_thread_cpu(),
            repaint_causes.iter().map(|cause| (cause.file, cause.line)),
        );
        let polls = SectionTimer::start(FrameSection::Polls);
        let ctx = ui.ctx().clone();
        self.repaint_ctx = Some(ctx.clone());

        super::keyboard_focus::repair_lost_keyboard_focus();
        self.apply_runtime_palette_visuals(&ctx);
        self.apply_runtime_ui_scale(&ctx);
        self.invalidate_galley_caches_on_font_atlas_change(&ctx);
        self.log_display_metrics_if_changed(&ctx);
        self.update_fps_estimate(&ctx);
        self.poll_agent_gui(&ctx);
        // After `poll_agent_gui`: the driver pushes injected key events into the
        // current frame's `InputState`, which egui rebuilds from RawInput next
        // frame, so a handler running before it never sees them.
        self.handle_global_accessibility_shortcuts(&ctx);

        if self.close_requested_at.is_none() && ctx.input(|i| i.viewport().close_requested()) {
            self.persist_window_state_if_changed(&ctx);
            self.close_requested_at = Some(Instant::now());
            info!("Close requested by viewport event");
        }

        if let Some(close_requested_at) = self.close_requested_at {
            if close_requested_at.elapsed() > CLOSE_FORCE_EXIT_TIMEOUT {
                error!(
                    "Close request timed out after {} seconds; forcing process exit",
                    CLOSE_FORCE_EXIT_TIMEOUT.as_secs()
                );
                std::process::exit(0);
            }

            self.poll_persistence_results();
            self.maybe_dispatch_persistence_requests(true);
            if self.has_pending_persistence_writes() {
                ctx.request_repaint_after(Duration::from_millis(16));
                return;
            }

            ctx.send_viewport_cmd(ViewportCommand::Close);
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }

        self.poll_persistence_results();
        self.process_pending_game_space_switch(&ctx);

        if self.startup_frame_rendered && !self.startup_tasks_started {
            self.startup_tasks_started = true;
            self.begin_startup_sync_tracking(self.startup_first_frame_at.unwrap_or_default());
            if !self.settings_view_state.debug_mode {
                self.restore_pending_updates();
                // `Foxy::new` already starts this plan when the database is
                // safe to open; only launches that skipped it start one here.
                if self.settings_view_state.auto_quick_scan_on_launch
                    && self.startup_quick_scan_filter_worker.is_none()
                    && self.startup_quick_scan_filter_rx.is_none()
                {
                    self.start_quick_local_scan();
                }
                self.start_fs_watcher();
            }
            self.queue_startup_rechecks();
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
        }
        self.poll_repository_space_freshness_results();
        self.poll_startup_diagnostics();
        self.poll_mission_scan();
        self.poll_restore_pending_updates();
        self.poll_startup_quick_scan_filter_results();
        if self.startup_tasks_started {
            self.refresh_fs_watcher_if_stale();
        }
        self.poll_fs_watch_results();
        self.poll_quick_scan_progress();
        self.poll_quick_scan_results();
        self.poll_repository_db_wipe_results();
        self.poll_database_wipe_result();
        self.poll_addon_delete_results();
        self.poll_addon_force_redownload_results();
        self.poll_addon_backup_results();
        self.poll_repository_settings_addon_preload_results();
        self.maybe_warm_addon_inventory();
        self.poll_repository_addon_size_load_results();
        self.poll_persistence_results();
        self.poll_backend_progress();
        self.poll_finished_backend_worker();
        self.poll_benchmark_save_results();
        self.tick_benchmark_capture(&ctx);
        self.poll_benchmark_export_screenshot(&ctx);
        self.poll_join_preflight_results(&ctx);
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
        self.process_startup_rechecks();
        self.maybe_log_startup_repository_layout();
        self.process_repository_space_sync_queue();
        self.process_repository_visual_folder_sync_queue();
        self.process_scheduled_jobs(&ctx);
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

        if self.needs_repaint {
            // One 60 Hz frame later, not at once: while progress events keep
            // arriving, an immediate repaint chains frames at the display rate.
            crate::ui::app::request_frame_after(&ctx, STATE_CHANGE_REPAINT_DELAY);
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
        self.render_benchmark_save_prompt(&ctx);
        self.render_scheduled_post_action_overlay(&ctx);
        overlays.stop(&mut self.frame_cost);

        if !self.startup_frame_rendered {
            self.startup_frame_rendered = true;
            self.startup_first_frame_at = Some(crate::core::api::process_start_elapsed());
            ctx.request_repaint();
        }
    }

    fn poll_join_preflight_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.join_preflight_result_rx.try_recv() {
            let Some(pending) = self.pending_join_preflight_query.take() else {
                continue;
            };
            match result.result {
                Ok(query_result) => {
                    self.join_preflight_cache.insert(
                        (result.address, result.port),
                        crate::ui::app::JoinPreflightCacheEntry {
                            result: query_result.clone(),
                            display_names: result.display_names.clone(),
                            cached_at: Instant::now(),
                        },
                    );
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
                    self.show_error_toast(self.t("Operation cancelled"));
                    ctx.request_repaint();
                }
            }
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

                ui.horizontal(|ui| {
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
    id: impl Into<Id>,
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

    let response = ui.interact(rect, id.into(), Sense::drag());
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
}
