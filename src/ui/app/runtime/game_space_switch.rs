//! Runtime game-space switching: swap the process to another game space
//! without restarting. The flow is start -> drain pending saves -> activate
//! the target space -> reset space-scoped state -> reload, mirroring what a
//! fresh startup would have loaded for that space.

use std::time::Duration;

use eframe::egui;
use log::{info, warn};

use crate::core::game::spaces::{self, GameSpaceEntry};
use crate::ui::app::Foxy;
use crate::ui::types::{FoxyView, RepositorySettingsTab, RepositoryViewState};

impl Foxy {
    /// True while background work that mutates or reads the active space
    /// mid-operation is running; switching waits until it is finished so the
    /// old space's database and files are never pulled out from under a task.
    pub(crate) fn game_space_switch_blocked(&self) -> bool {
        self.repository_sync_active()
            || self.update_modal_open
            || !self.active_quick_scan_instance_keys.is_empty()
            || self.is_direct_download_running()
            || self.settings_view_state.debug_mode
            || self
                .addon_backup_worker
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
            || self.repository_space_import_in_flight
            || self.addon_hash_recalc_in_flight
            || !self.pending_repository_db_wipes.is_empty()
            || !self.pending_addon_deletes.is_empty()
            || self.scheduler_active_run.is_some()
    }

    /// Begin a runtime switch. The target is only remembered here; the switch
    /// completes in [`Self::process_pending_game_space_switch`] once the
    /// persistence queue is drained, so queued settings/repository writes
    /// always land in the space they belong to.
    pub(crate) fn start_game_space_switch(&mut self, entry: &GameSpaceEntry) {
        if self.pending_game_space_switch.is_some() {
            return;
        }
        if self.game_space_switch_blocked() {
            self.show_error_toast(
                self.t("Finish downloads and scans before switching game spaces."),
            );
            return;
        }
        info!("Game space switch requested: {}", entry.id);
        self.pending_game_space_switch = Some(entry.clone());
        self.needs_repaint = true;
    }

    pub(in crate::ui::app) fn process_pending_game_space_switch(&mut self, ctx: &egui::Context) {
        if self.pending_game_space_switch.is_none() {
            return;
        }
        self.maybe_dispatch_persistence_requests(true);
        if self.has_pending_persistence_writes() {
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        let Some(entry) = self.pending_game_space_switch.take() else {
            return;
        };
        self.finish_game_space_switch(ctx, &entry);
    }

    fn finish_game_space_switch(&mut self, ctx: &egui::Context, entry: &GameSpaceEntry) {
        let opened = match spaces::activate_game_space(&entry.id) {
            Ok(opened) => opened,
            Err(err) => {
                warn!("Failed to switch game space: {}", err);
                self.show_error_toast(
                    self.t_fmt("Could not switch game space: {error}", &[("error", err)]),
                );
                return;
            }
        };
        info!(
            "Switching to game space {} (game {}) without restart",
            opened.id, opened.game_id
        );

        // Honor a pending wipe marker in the target space before its database
        // is first opened, mirroring the startup ordering in main.rs.
        crate::core::tasks::init_database::check_and_wipe_database();

        self.stop_fs_watcher();
        self.reset_space_scoped_state();
        self.reload_active_game_space(ctx);

        self.current_view = FoxyView::RepositoryList;
        self.last_view = FoxyView::None;
        self.show_success_toast(self.t_fmt(
            "Game space {name} is now active.",
            &[("name", opened.display_name.clone())],
        ));
        self.needs_repaint = true;
    }

    /// Load the newly activated space the same way startup does: settings,
    /// schema gate, repositories, spaces, folders, profiles, images, and the
    /// space-scoped startup tasks (pending-update restore, quick scan,
    /// filesystem watcher, rechecks).
    fn reload_active_game_space(&mut self, ctx: &egui::Context) {
        self.load_settings();
        // Debug mode stays a launch-flag-only feature across reloads;
        // switching is blocked while it is active.
        self.settings_view_state.debug_mode = self.launch_debug_mode;
        self.sanitize_settings_debug_artifacts();
        self.sync_debug_runtime_state();
        self.i18n.set_language(&self.settings_view_state.locale);
        self.pending_db_schema_wipe =
            crate::core::tasks::db_schema_version::evaluate_and_bootstrap();
        self.load_repositories();
        self.load_repository_spaces();
        self.reconcile_repository_space_paths();
        self.load_repository_visual_folders();
        info!(
            "Game space state loaded: repositories={} repository_spaces={}",
            self.repository_view_state.repositories.len(),
            self.repository_spaces.len()
        );
        self.detect_arma3_profiles_now();
        self.request_repository_images(ctx);

        if !self.settings_view_state.debug_mode {
            self.restore_pending_updates();
            if self.settings_view_state.auto_quick_scan_on_launch {
                self.start_quick_local_scan();
            }
            self.start_fs_watcher();
        }
        self.queue_startup_rechecks();
        self.start_startup_ts3_plugin_scan();
    }

    pub(in crate::ui::app) fn detect_arma3_profiles_now(&mut self) {
        let custom_profiles_dir = self.settings_view_state.arma3_profiles_directory.trim();
        let custom_profiles_dir = if custom_profiles_dir.is_empty() {
            None
        } else {
            Some(std::path::Path::new(custom_profiles_dir))
        };
        self.detected_arma3_profiles =
            crate::core::arma3_profiles::detect_all_profiles(custom_profiles_dir);
        self.detected_active_arma3_profile =
            crate::core::arma3_profiles::detect_active_profile(&self.detected_arma3_profiles);
    }

    pub(in crate::ui::app) fn request_repository_images(&mut self, ctx: &egui::Context) {
        let repos = self.repository_view_state.repositories.clone();
        for repo in repos {
            if !repo.icon_image_checksum.is_empty() {
                self.download_and_load_image(
                    ctx,
                    &repo.address,
                    &repo.icon_image_path,
                    &repo.icon_image_checksum,
                    true,
                );
            }
            if !repo.repo_image_checksum.is_empty() {
                self.download_and_load_image(
                    ctx,
                    &repo.address,
                    &repo.repo_image_path,
                    &repo.repo_image_checksum,
                    false,
                );
            }
        }
        let spaces = self.repository_spaces.clone();
        for space in spaces {
            if !space.icon_image_checksum.is_empty() {
                self.download_and_load_image(
                    ctx,
                    &space.source_base_url,
                    &space.icon_image_path,
                    &space.icon_image_checksum,
                    true,
                );
            }
            if !space.repo_image_checksum.is_empty() {
                self.download_and_load_image(
                    ctx,
                    &space.source_base_url,
                    &space.repo_image_path,
                    &space.repo_image_checksum,
                    false,
                );
            }
        }
    }

    /// Return every field that mirrors the previous space's data to its
    /// startup default. App-global state (window, theme, locale, activity
    /// log, app updates, backups, tray) is deliberately left alone.
    fn reset_space_scoped_state(&mut self) {
        // Repository list, spaces, and folders.
        self.repository_view_state = RepositoryViewState::default();
        self.repository_list_cache = Default::default();
        self.bump_repository_list_data_version();
        self.drag_source_repo_index = None;
        self.drag_drop_target_index = None;
        self.drag_drop_target_visual_folder_id = None;
        self.repository_spaces.clear();
        self.bump_repository_spaces_version();
        self.repository_visual_folders.clear();
        self.bump_repository_visual_folders_version();
        self.selected_repository_space_id = None;
        self.selected_repository_visual_folder_id = None;
        self.repository_space_detail_filter.clear();
        self.repository_space_detail_filter_space_id = None;
        self.repository_space_selector_state = None;
        self.repository_space_settings_state = None;
        self.pending_repository_space_bulk_action = None;
        self.repository_space_bulk_progress = None;
        self.repository_space_import_in_flight = false;
        self.repository_selection = None;
        self.selected_repository_for_settings = None;
        self.current_repository_settings_tab = RepositorySettingsTab::Configuration;

        // Modals, confirmations, and pending destructive actions.
        self.show_delete_confirmation = false;
        self.delete_repository_delete_files = false;
        self.show_force_redownload_confirmation = false;
        self.show_wipe_db_confirmation = false;
        self.show_wipe_repo_db_confirmation = false;
        self.show_add_repository_modal = false;
        self.add_repository_input_address.clear();
        self.add_repository_input_name.clear();
        self.add_repository_input_path.clear();
        self.add_repository_input_error = None;
        self.pending_repository_duplicate_add = None;
        self.pending_mission_duplicate = None;
        self.pending_mission_delete = None;
        self.pending_mission_remove_dependencies = None;
        self.pending_mission_editor_launch_warning = None;
        self.pending_addon_destructive_confirmation = None;
        self.pending_settings_folder_removal = None;
        self.pending_repository_context_confirmation = None;
        self.pending_repository_space_delete_id = None;
        self.pending_repository_visual_folder_edit = None;
        self.pending_repository_visual_folder_delete = None;
        self.pending_settings_reset_confirmation = false;
        self.new_profile_name.clear();
        self.show_add_profile_window = false;
        self.show_rename_profile_window = false;
        self.pending_profile_confirm_action = None;

        // Addon inventory, missions, and per-repository caches.
        self.invalidate_addon_inventory_cache();
        self.cached_missions = None;
        self.mission_row_galleys = Default::default();
        self.repository_list_galleys = Default::default();
        self.update_detail_file_galleys = Default::default();
        self.bulk_action_entry_galleys = Default::default();
        self.space_selector_entry_galleys = Default::default();
        self.space_selector_candidate_galleys = Default::default();
        self.space_detail_candidate_galleys = Default::default();
        self.server_row_galleys = Default::default();
        self.repository_settings_addon_preload_worker = None;
        self.repository_addon_size_load_pending = false;
        self.repository_addon_size_bytes_by_repo_and_addon.clear();
        self.addons_filter.clear();
        self.addons_search_files = false;
        self.optional_addons_filter.clear();
        self.external_addons_filter.clear();
        self.external_addons_origin_filter = "All".to_string();
        self.external_addons_group_by_origin = false;
        self.optional_addons_search_files = false;
        self.external_addons_search_files = false;
        self.addon_state_filter.clear();
        self.addon_favorites_only_filter = false;
        self.addon_client_side_only_filter = false;
        self.editor_mission_search.clear();
        self.editor_mission_folder.clear();
        self.editor_mission_show_folders = false;
        self.editor_mission_terrain_filter.clear();

        // Server queries and join preflight.
        self.server_statuses.clear();
        self.server_refresh_indicator_until.clear();
        self.pending_server_queries.clear();
        self.join_preflight_cache.clear();
        self.pending_join_preflight = None;
        self.pending_join_preflight_query = None;
        self.pending_join_status_query = None;

        // Images and metadata fetches.
        self.pending_image_jobs.clear();
        self.cached_icons.clear();
        self.cached_repo_images.clear();
        self.tracked_icon_texture_bytes.clear();
        self.tracked_repo_image_texture_bytes.clear();
        self.pending_repo_metadata_jobs.clear();
        self.pending_repo_metadata_refresh.clear();

        // Quick scan, filesystem watch, and recheck pipelines.
        self.quick_scan_worker = None;
        self.startup_quick_scan_filter_rx = None;
        self.startup_quick_scan_filter_worker = None;
        self.deferred_fs_scan.clear();
        self.pending_quick_scan_urls.clear();
        self.pending_quick_scan_prevalidated_urls.clear();
        self.pending_quick_scan_force_fresh_addon_hash_urls.clear();
        self.quick_scan_pending.clear();
        self.active_quick_scan_instance_keys.clear();
        self.quick_scan_progress_by_instance.clear();
        self.repo_db_reset_pending_recheck.clear();
        self.pending_repository_db_wipes.clear();
        self.pending_repository_force_redownloads.clear();
        self.pending_repository_db_wipe_started_at.clear();
        self.pending_addon_deletes.clear();
        self.pending_cached_update_loads.clear();
        self.addon_hash_recalc_in_flight = false;
        self.addon_hash_recalc_queue.clear();
        self.startup_recheck_queue.clear();
        self.repository_space_sync_queue.clear();
        self.repository_visual_folder_sync_queue.clear();
        self.startup_pending_restore_rx = None;
        self.startup_pending_restore_worker = None;
        self.startup_repository_layout_logged = false;
        self.prelaunch_recheck_at = None;

        // Sync/download progress and results.
        self.backend_progress_rx = None;
        self.backend_worker = None;
        self.sync_started_at = None;
        self.syncing_repository = None;
        self.scheduler_active_run = None;
        self.scheduler_pending_post_action = None;
        self.pending_update_cache.clear();
        self.clear_mod_diff_cache();
        self.progress_events.clear();
        self.update_modal_open = false;
        self.update_ready_repo = None;
        self.current_sync_mode = None;
        self.download_progress = None;
        self.download_finished = false;
        self.download_finished_repo = None;
        self.download_summary = None;
        self.open_update_after_sync = false;
        self.mod_download_progress.clear();
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
        self.cancel_tx = None;
        self.recheck_stage_label = None;
        self.recheck_stage_percent = None;
        self.recheck_hash_counter = None;
        self.recheck_hash_part_counter = None;
        self.last_hash_progress_repaint = None;
        self.download_hash_sample_at = None;
        self.download_hash_sample_files = 0;
        self.download_hash_sample_parts = 0;
        self.completed_repository_check_banner = None;
        self.completed_repository_db_wipe_banner = None;
        self.update_modal_sorted_mod_indices.clear();
        self.update_modal_mod_name_lowers.clear();
        self.update_modal_sort_generation = 0;
        self.update_modal_sorted_generation = 0;
        self.update_modal_sort_last_progress_invalidation = None;
        self.last_incomplete_config_sync_toast_at = None;

        // Repository status maps.
        self.repo_states.clear();
        self.repo_states_version = self.repo_states_version.wrapping_add(1);
        self.repo_foxy_modes.clear();

        // Arma-specific integrations.
        self.detected_arma3_profiles.clear();
        self.detected_active_arma3_profile = None;
        self.pending_arma3_profile_action = None;
        self.ts3_plugin_update_prompt = None;
        self.ts3_plugin_cache = None;
        self.ts3_plugin_scan_rx = None;
        self.ts3_plugin_scanning = false;
        self.ts3_running_cache = None;
        self.ts3_plugin_scan_prompt_on_update = false;
        self.ts3_plugin_scan_requeued = false;
        self.editor_launch_cooldown_until = None;

        // Backup task transients (the worker is finished before a switch is
        // allowed; the app-global backup manager records stay).
        self.addon_backup_worker = None;
        self.addon_backup_status = None;
        self.addon_backup_notice = None;
        self.addon_backup_restore_state = None;

        // Debug-mode shadows and one-off flows.
        self.stored_settings = None;
        self.stored_repositories = None;
        self.swifty_migration_state = Default::default();
        self.game_space_settings_view_state = Default::default();
    }
}
