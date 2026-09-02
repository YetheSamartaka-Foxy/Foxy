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
    /// Why a switch cannot start right now, or `None` when it can. Both the
    /// disabled-button hover text and the rejection toast use this so the user
    /// is always told the actual reason.
    pub(crate) fn game_space_switch_block_reason(&self) -> Option<&'static str> {
        // Debug mode swaps in shadow settings/repositories that belong to the
        // current space; reloading another space underneath them would leave
        // the shadows pointing at the wrong data.
        if self.settings_view_state.debug_mode {
            return Some("Leave debug mode before switching game spaces.");
        }
        let busy = self.repository_sync_active()
            || self.update_modal_open
            || !self.active_quick_scan_instance_keys.is_empty()
            || self.is_direct_download_running()
            || self
                .addon_backup_worker
                .as_ref()
                .is_some_and(|handle| !handle.is_finished())
            || self.repository_space_import_in_flight
            || self.addon_hash_recalc_in_flight
            || !self.pending_repository_db_wipes.is_empty()
            || !self.pending_addon_deletes.is_empty()
            || self.scheduler_active_run.is_some();
        busy.then_some("Finish downloads and scans before switching game spaces.")
    }

    /// Begin a runtime switch. The target is only remembered here; the switch
    /// completes in [`Self::process_pending_game_space_switch`] once the
    /// persistence queue is drained, so queued settings/repository writes
    /// always land in the space they belong to.
    pub(crate) fn start_game_space_switch(&mut self, entry: &GameSpaceEntry) {
        if self.pending_game_space_switch.is_some() {
            return;
        }
        if let Some(reason) = self.game_space_switch_block_reason() {
            self.show_error_toast(self.t(reason));
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
        // Release the outgoing space's database before retargeting. The handle
        // slot would otherwise only swap on the next database access, leaving
        // `database.db` open in a space the user can now remove.
        crate::core::tasks::init_database::close_active_database_sync();

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
        self.db_lock_conflict = self.claim_active_space_database();
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

        // Server queries and join preflight. Query threads from the old space
        // are already detached; dropping their handles and draining whatever
        // they already sent keeps their results out of the new space's status
        // map (late arrivals key on an address this space does not list).
        self.server_statuses.clear();
        self.server_refresh_indicator_until.clear();
        self.pending_server_queries.clear();
        self.pending_queries.clear();
        while self.server_updates.try_recv().is_ok() {}
        self.join_preflight_cache.clear();
        self.pending_join_preflight = None;
        self.pending_join_preflight_query = None;
        self.pending_join_status_query = None;

        // Scheduling drafts reference this space's repositories (scheduled jobs
        // are part of the game-space settings half).
        self.scheduling_editor = None;

        // Direct-download inputs point at the previous space's game folders.
        self.direct_download_url_input.clear();
        self.direct_download_destination_input.clear();
        self.direct_download_error = None;
        self.direct_download_update_view = false;

        // Watcher suppression belongs to the watcher that was just stopped.
        self.fs_watch_suppressed_until_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);

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

/// `Foxy` fields that deliberately survive a game-space switch because they are
/// app-global, not space-scoped: the window and renderer, theme and locale, the
/// activity log, app updates, the backup manager, the tray, scheduling
/// machinery that is not per-space, and every worker channel endpoint (dropping
/// a sender would break the receiver for the rest of the session).
///
/// [`reset_space_scoped_state`] cannot be verified at runtime - `Foxy` needs a
/// live `eframe::CreationContext` and has no `Default` - so the guard test below
/// checks the source instead: every field must be either reset there or listed
/// here. A new space-scoped field that nobody resets is exactly the silent
/// cross-space data leak the `(remote_url, local_path)` identity rules exist to
/// prevent, so forgetting one has to fail loudly somewhere.
#[cfg(test)]
const APP_GLOBAL_FOXY_FIELDS: &[&str] = &[
    // Window, rendering, and view chrome.
    "app_icon",
    "app_icon_texture_bytes",
    "default_repo_image",
    "default_repo_image_texture_bytes",
    "repaint_ctx",
    "needs_repaint",
    "startup_frame_rendered",
    "startup_tasks_started",
    "close_requested_at",
    "current_view",
    "last_view",
    "main_view_state",
    "current_help_tab",
    "current_about_tab",
    "fps_ema",
    "last_applied_palette",
    "cached_color32",
    "last_font_image_size",
    "last_saved_window_state",
    "last_logged_display_metrics",
    "tray_manager",
    "hidden_to_tray",
    "ui_toast",
    "pending_renderer_fallback_notice",
    // App-global settings, locale, and debug flags.
    "settings_view_state",
    "i18n",
    "launch_debug_mode",
    "show_debug_windows",
    "previous_debug_mode",
    "debug_modal_previews",
    "agent_gui",
    // Reloaded by the switch itself rather than blanked first.
    "pending_db_schema_wipe",
    "db_lock_conflict",
    "pending_low_space_notice",
    "game_spaces_view_state",
    "pending_game_space_switch",
    // Activity log and diagnostics.
    "activity_log_cache",
    "activity_log_galleys",
    "activity_log_generation",
    "activity_log_last_poll_at",
    "activity_log_filter_error",
    "activity_log_filter_warn",
    "activity_log_filter_info",
    "activity_log_filter_debug",
    "activity_log_filter_trace",
    "activity_log_search",
    "show_memory_diagnostics_window",
    "memory_diagnostics_history",
    "memory_diagnostics_pinned_baseline",
    "memory_diagnostics_last_sample_at",
    "memory_diagnostics_last_logged_stage_key",
    "memory_diagnostics_process_map",
    "memory_diagnostics_last_process_map_at",
    // Backup manager (backups are app-global by plan section 19).
    "backup_manager_records",
    "backup_manager_records_version",
    "backup_manager_loaded",
    "backup_manager_filter",
    "backup_manager_view_cache",
    "backup_manager_notice",
    "backup_manager_confirm_action",
    "backup_inventory_refresh_requested",
    "backup_inventory_refresh_in_progress",
    "backup_inventory_request_id",
    "backup_inventory_in_flight_request_id",
    // Direct download: the transfer itself blocks a switch, and the remaining
    // fields are app-global preferences rather than space state.
    "show_direct_download_screen",
    "direct_download_use_global_speed_limit",
    "direct_download_override_speed_unlimited",
    "direct_download_override_speed_limit_mbps",
    "direct_download_session",
    "direct_download_progress_rx",
    "direct_download_worker",
    // App self-update.
    "app_update_status",
    "app_update_event_rx",
    "app_update_download_rx",
    "app_update_changelog_rx",
    "app_update_changelog_tx",
    "app_update_last_check",
    "app_update_changelogs",
    "app_update_changelog_loading",
    "app_update_changelogs_requested",
    "pending_app_update_prompt",
    "app_update_prompt_armed",
    // Persistence queue: drained before the switch completes.
    "persistence_request_tx",
    "persistence_result_rx",
    "settings_dirty",
    "settings_revision",
    "settings_last_mutated_at",
    "settings_save_in_flight_revision",
    "settings_completed_revision",
    "repositories_dirty",
    "repositories_revision",
    "repositories_last_mutated_at",
    "repositories_save_in_flight_revision",
    "repositories_completed_revision",
    // Long-lived worker channel endpoints. These outlive a switch by design;
    // the state they feed is reset above.
    "repository_settings_addon_preload_rx",
    "repository_settings_addon_preload_tx",
    "repository_addon_size_load_rx",
    "repository_addon_size_load_tx",
    "server_updates",
    "updates_sender",
    "join_preflight_worker",
    "join_preflight_result_rx",
    "join_preflight_result_tx",
    "image_result_rx",
    "image_result_tx",
    "repo_metadata_result_rx",
    "repo_metadata_result_tx",
    "repository_space_import_result_rx",
    "repository_space_import_result_tx",
    "addon_hash_recalc_result_rx",
    "addon_hash_recalc_result_tx",
    "addon_delete_result_rx",
    "addon_delete_result_tx",
    "cached_update_load_result_rx",
    "cached_update_load_result_tx",
    "quick_scan_rx",
    "quick_scan_tx",
    "quick_scan_progress_rx",
    "quick_scan_progress_tx",
    "fs_watch_rx",
    "fs_watch_tx",
    "fs_watch_worker",
    "fs_watch_stop",
    "repository_db_wipe_rx",
    "repository_db_wipe_tx",
    "database_wipe_rx",
    "database_wipe_tx",
    "addon_backup_task_rx",
    "addon_backup_task_tx",
];

#[cfg(test)]
mod tests {
    use super::APP_GLOBAL_FOXY_FIELDS;

    const FOXY_STRUCT_SOURCE: &str = include_str!("../mod.rs");
    const SWITCH_SOURCE: &str = include_str!("game_space_switch.rs");

    /// Field names declared on `pub struct Foxy`.
    fn foxy_fields() -> Vec<String> {
        let body = FOXY_STRUCT_SOURCE
            .split_once("pub struct Foxy {")
            .expect("Foxy struct declaration")
            .1
            .split_once("\n}")
            .expect("Foxy struct terminator")
            .0;

        body.lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with("//") || line.starts_with("#[") {
                    return None;
                }
                let (name, rest) = line.split_once(':')?;
                // A wrapped type line such as `crate::ui::types::Foo,` also
                // contains a colon; a real field is `name: Type`, never `::`.
                if rest.starts_with(':') {
                    return None;
                }
                let name = name.trim().rsplit(' ').next()?;
                name.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
                    .then(|| name.to_string())
            })
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// Fields assigned or mutated through `self.<field>` inside a function.
    fn fields_touched_by(source: &str, signature: &str) -> Vec<String> {
        let body = source
            .split_once(signature)
            .unwrap_or_else(|| panic!("function {signature} exists"))
            .1;
        let mut touched = Vec::new();
        let mut rest = body;
        while let Some(index) = rest.find("self.") {
            rest = &rest[index + "self.".len()..];
            let name: String = rest
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect();
            // `self.foo()` is a method call, not a field reset.
            if !name.is_empty() && !rest[name.len()..].starts_with('(') {
                touched.push(name);
            }
        }
        touched
    }

    #[test]
    fn every_foxy_field_is_either_space_scoped_and_reset_or_listed_app_global() {
        let reset = fields_touched_by(SWITCH_SOURCE, "fn reset_space_scoped_state(&mut self) {");
        // Cleared indirectly by the helpers the reset calls
        // (`invalidate_addon_inventory_cache`, `clear_mod_diff_cache`, and the
        // list-version bumps).
        let helper_cleared = [
            "cached_all_addons",
            "addon_inventory_generation",
            "addon_inventory_view_cache",
            "repository_addons_list_cache",
            "repository_optional_addons_list_cache",
            "repository_external_addons_list_cache",
            "repository_addon_size_bytes_by_repo_and_addon",
            "repository_addon_size_load_pending",
            "mod_diff_cache",
            "repository_list_data_version",
            "repository_spaces_version",
            "repository_visual_folders_version",
        ];

        let unaccounted: Vec<String> = foxy_fields()
            .into_iter()
            .filter(|field| {
                !reset.contains(field)
                    && !helper_cleared.contains(&field.as_str())
                    && !APP_GLOBAL_FOXY_FIELDS.contains(&field.as_str())
            })
            .collect();

        assert!(
            unaccounted.is_empty(),
            "these Foxy fields are neither reset by reset_space_scoped_state nor listed in \
             APP_GLOBAL_FOXY_FIELDS; a space-scoped field left out of the reset leaks the \
             previous game space's data into the next one: {unaccounted:?}"
        );
    }

    /// Guards the allowlist against naming fields that no longer exist, which
    /// would silently stop covering a renamed field.
    #[test]
    fn app_global_allowlist_has_no_stale_entries() {
        let fields = foxy_fields();
        let stale: Vec<&&str> = APP_GLOBAL_FOXY_FIELDS
            .iter()
            .filter(|name| !fields.iter().any(|field| field == *name))
            .collect();

        assert!(
            stale.is_empty(),
            "APP_GLOBAL_FOXY_FIELDS names fields that Foxy no longer declares: {stale:?}"
        );
    }
}
