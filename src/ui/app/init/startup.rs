use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use eframe::egui::{self, Visuals};
use log::info;

use crate::core::api::{self, QuickScanResult};
use crate::ui::app::{
    AddonBackupTaskResult, AddonDeleteResult, AddonHashRecalcResult, AddonInventoryViewCache,
    CachedUpdateLoadResult, Foxy, ImageLoadResult, JoinPreflightQueryResult, ListGalleyCache,
    MissionRowGalleyCache, PersistenceRequest, PersistenceResult, RepoMetadataFetchResult,
    RepositoryAddonListCache, RepositoryAddonSizeLoadResult, RepositoryDbWipeResult,
    RepositoryExternalAddonsListCache, RepositoryListCache, RepositorySettingsAddonPreloadResult,
    RepositorySpaceImportResult, agent_driver::AgentGuiLaunchConfig,
};
use crate::ui::i18n::I18n;
use crate::ui::palette;
use crate::ui::types::*;

fn push_configured_path(paths: &mut Vec<api::StartupStoragePath>, role: &'static str, path: &str) {
    if !path.trim().is_empty() {
        paths.push(api::StartupStoragePath::new(role, path.trim()));
    }
}

impl Foxy {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        launch_debug_mode: bool,
        agent_gui: AgentGuiLaunchConfig,
    ) -> Self {
        info!("Initializing Foxy UI state");
        let mut visuals = Visuals::dark();
        visuals.override_text_color = Some(palette::TEXT_NORMAL);
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.set_visuals(visuals);

        // Use Roboto as the default proportional typeface. It is inserted at
        // the front of the Proportional family so it takes priority over
        // egui's built-in font, while the Noto fonts below remain fallbacks
        // for scripts Roboto does not cover.
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "roboto".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/Roboto-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "roboto".to_owned());

        // Load Arabic font as a fallback for RTL text rendering
        fonts.font_data.insert(
            "noto_sans_arabic".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansArabic-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_arabic".to_owned());

        fonts.font_data.insert(
            "noto_sans_thai".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansThai-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_thai".to_owned());

        fonts.font_data.insert(
            "noto_sans_devanagari".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansDevanagari-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_devanagari".to_owned());

        fonts.font_data.insert(
            "noto_sans_bengali".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansBengali-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_bengali".to_owned());

        fonts.font_data.insert(
            "noto_sans_cjk_jp".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansCJKjp-Regular.otf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_cjk_jp".to_owned());

        fonts.font_data.insert(
            "noto_sans_cjk_sc".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansCJKsc-Regular.otf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_cjk_sc".to_owned());

        // Load Hebrew font as a fallback for Hebrew text rendering
        fonts.font_data.insert(
            "noto_sans_hebrew".to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../../fonts/NotoSansHebrew-Regular.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("noto_sans_hebrew".to_owned());

        cc.egui_ctx.set_fonts(fonts);

        api::ensure_logger();
        let (updates_sender, server_updates) =
            std::sync::mpsc::channel::<(String, String, ServerOnlineStatus)>();
        let (join_preflight_result_tx, join_preflight_result_rx) =
            std::sync::mpsc::channel::<JoinPreflightQueryResult>();
        let (image_result_tx, image_result_rx) = std::sync::mpsc::channel::<ImageLoadResult>();
        let (repo_metadata_result_tx, repo_metadata_result_rx) =
            std::sync::mpsc::channel::<RepoMetadataFetchResult>();
        let (repository_space_import_result_tx, repository_space_import_result_rx) =
            std::sync::mpsc::channel::<RepositorySpaceImportResult>();
        let (addon_hash_recalc_result_tx, addon_hash_recalc_result_rx) =
            std::sync::mpsc::channel::<AddonHashRecalcResult>();
        let (addon_delete_result_tx, addon_delete_result_rx) =
            std::sync::mpsc::channel::<AddonDeleteResult>();
        let (cached_update_load_result_tx, cached_update_load_result_rx) =
            std::sync::mpsc::channel::<CachedUpdateLoadResult>();
        let (quick_scan_tx, quick_scan_rx) = std::sync::mpsc::channel::<QuickScanResult>();
        let (fs_watch_tx, fs_watch_rx) = std::sync::mpsc::channel::<api::FsChangeEvent>();
        let fs_watch_suppressed_until_ms =
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (repository_db_wipe_tx, repository_db_wipe_rx) =
            std::sync::mpsc::channel::<RepositoryDbWipeResult>();
        let (addon_backup_task_tx, addon_backup_task_rx) =
            std::sync::mpsc::channel::<AddonBackupTaskResult>();
        let (repository_settings_addon_preload_tx, repository_settings_addon_preload_rx) =
            std::sync::mpsc::channel::<RepositorySettingsAddonPreloadResult>();
        let (repository_addon_size_load_tx, repository_addon_size_load_rx) =
            std::sync::mpsc::channel::<RepositoryAddonSizeLoadResult>();
        let (persistence_request_tx, persistence_request_rx) =
            std::sync::mpsc::channel::<PersistenceRequest>();
        let (persistence_result_tx, persistence_result_rx) =
            std::sync::mpsc::channel::<PersistenceResult>();
        std::thread::spawn(move || {
            Self::run_persistence_worker(persistence_request_rx, persistence_result_tx);
        });
        let mut app = Self {
            app_icon: None,
            default_repo_image: None,
            repaint_ctx: None,
            agent_gui: None,
            show_debug_windows: false,
            show_delete_confirmation: false,
            delete_repository_delete_files: false,
            show_force_redownload_confirmation: false,
            show_wipe_db_confirmation: false,
            show_wipe_repo_db_confirmation: false,
            pending_renderer_fallback_notice: false,
            pending_db_schema_wipe: None,
            current_view: FoxyView::RepositoryList,
            last_view: FoxyView::None,
            main_view_state: MainViewState {
                use_window_decorations: false,
            }, // So it works on Windows
            repository_view_state: RepositoryViewState::default(),
            repository_list_cache: RepositoryListCache::default(),
            repository_list_data_version: 1,
            drag_source_repo_index: None,
            drag_drop_target_index: None,
            drag_drop_target_visual_folder_id: None,
            repository_spaces_version: 1,
            repository_spaces: Vec::new(),
            repository_visual_folders_version: 1,
            repository_visual_folders: Vec::new(),
            selected_repository_space_id: None,
            selected_repository_visual_folder_id: None,
            repository_space_detail_filter: String::new(),
            repository_space_detail_filter_space_id: None,
            show_add_repository_modal: false,
            add_repository_input_address: String::new(),
            add_repository_input_name: String::new(),
            add_repository_input_path: String::new(),
            add_repository_input_error: None,
            pending_repository_duplicate_add: None,
            pending_mission_duplicate: None,
            pending_mission_delete: None,
            pending_mission_remove_dependencies: None,
            pending_mission_editor_launch_warning: None,
            pending_addon_destructive_confirmation: None,
            pending_settings_folder_removal: None,
            pending_join_preflight: None,
            pending_join_preflight_query: None,
            pending_join_status_query: None,
            editor_mission_search: String::new(),
            editor_mission_folder: String::new(),
            editor_mission_show_folders: false,
            editor_mission_terrain_filter: String::new(),
            repository_space_selector_state: None,
            repository_space_settings_state: None,
            pending_repository_space_bulk_action: None,
            repository_space_bulk_progress: None,
            settings_view_state: SettingsViewState::default(),
            i18n: I18n::new("system"),
            launch_debug_mode,
            previous_debug_mode: false,
            stored_settings: None,
            stored_repositories: None,
            cached_all_addons: None,
            addon_inventory_generation: 1,
            addon_inventory_view_cache: AddonInventoryViewCache::default(),
            repository_addons_list_cache: RepositoryAddonListCache::default(),
            repository_optional_addons_list_cache: RepositoryAddonListCache::default(),
            repository_external_addons_list_cache: RepositoryExternalAddonsListCache::default(),
            mission_row_galleys: MissionRowGalleyCache::default(),
            activity_log_galleys: ListGalleyCache::default(),
            repository_list_galleys: ListGalleyCache::default(),
            update_detail_file_galleys: ListGalleyCache::default(),
            bulk_action_entry_galleys: ListGalleyCache::default(),
            space_selector_entry_galleys: ListGalleyCache::default(),
            space_selector_candidate_galleys: ListGalleyCache::default(),
            space_detail_candidate_galleys: ListGalleyCache::default(),
            server_row_galleys: ListGalleyCache::default(),
            repository_settings_addon_preload_rx,
            repository_settings_addon_preload_tx,
            repository_settings_addon_preload_worker: None,
            repository_addon_size_load_rx,
            repository_addon_size_load_tx,
            repository_addon_size_load_pending: false,
            repository_addon_size_bytes_by_repo_and_addon: HashMap::new(),
            repository_selection: None,
            detected_arma3_profiles: Vec::new(),
            detected_active_arma3_profile: None,
            pending_arma3_profile_action: None,
            cached_missions: None,
            selected_repository_for_settings: None,
            current_repository_settings_tab: RepositorySettingsTab::Configuration,
            current_help_tab: HelpTab::Overview,
            current_about_tab: AboutTab::About,
            addons_filter: String::new(),
            addons_search_files: false,
            optional_addons_filter: String::new(),
            external_addons_filter: String::new(),
            external_addons_origin_filter: "All".to_string(),
            external_addons_group_by_origin: false,
            optional_addons_search_files: false,
            external_addons_search_files: false,
            addon_state_filter: String::new(),
            addon_favorites_only_filter: false,
            addon_client_side_only_filter: false,
            server_statuses: HashMap::new(),
            server_refresh_indicator_until: HashMap::new(),
            pending_server_queries: HashSet::new(),
            pending_queries: Vec::new(),
            server_updates,
            updates_sender,
            join_preflight_cache: HashMap::new(),
            join_preflight_worker: None,
            join_preflight_result_rx,
            join_preflight_result_tx,
            image_result_rx,
            image_result_tx,
            pending_image_jobs: HashSet::new(),
            repo_metadata_result_rx,
            repo_metadata_result_tx,
            pending_repo_metadata_jobs: HashSet::new(),
            repository_space_import_result_rx,
            repository_space_import_result_tx,
            repository_space_import_in_flight: false,
            addon_hash_recalc_result_rx,
            addon_hash_recalc_result_tx,
            addon_hash_recalc_in_flight: false,
            addon_delete_result_rx,
            addon_delete_result_tx,
            pending_addon_deletes: HashSet::new(),
            cached_update_load_result_rx,
            cached_update_load_result_tx,
            pending_cached_update_loads: HashSet::new(),
            quick_scan_rx,
            quick_scan_tx,
            quick_scan_worker: None,
            startup_quick_scan_filter_rx: None,
            startup_quick_scan_filter_worker: None,
            fs_watch_rx,
            fs_watch_tx,
            fs_watch_worker: None,
            fs_watch_suppressed_until_ms,
            deferred_fs_scan: HashSet::new(),
            pending_quick_scan_urls: HashSet::new(),
            pending_quick_scan_prevalidated_urls: HashSet::new(),
            pending_quick_scan_force_fresh_addon_hash_urls: HashSet::new(),
            quick_scan_pending: HashSet::new(),
            active_quick_scan_instance_keys: HashSet::new(),
            repo_db_reset_pending_recheck: HashSet::new(),
            pending_repository_db_wipes: HashSet::new(),
            pending_repository_force_redownloads: HashSet::new(),
            pending_repository_db_wipe_started_at: HashMap::new(),
            repository_db_wipe_rx,
            repository_db_wipe_tx,
            addon_backup_task_rx,
            addon_backup_task_tx,
            addon_backup_worker: None,
            addon_backup_status: None,
            addon_backup_notice: None,
            addon_backup_restore_state: None,
            backup_manager_records: Vec::new(),
            backup_manager_records_version: 0,
            backup_manager_loaded: false,
            backup_manager_filter: String::new(),
            backup_manager_view_cache: None,
            backup_manager_notice: None,
            backup_manager_confirm_action: None,
            cached_icons: HashMap::new(),
            cached_repo_images: HashMap::new(),
            new_profile_name: String::new(),
            show_add_profile_window: false,
            show_rename_profile_window: false,
            pending_profile_confirm_action: None,
            pending_settings_reset_confirmation: false,
            show_direct_download_screen: false,
            direct_download_url_input: String::new(),
            direct_download_destination_input: String::new(),
            direct_download_use_global_speed_limit: true,
            direct_download_override_speed_unlimited: true,
            direct_download_override_speed_limit_mbps: 1,
            direct_download_error: None,
            direct_download_session: None,
            direct_download_progress_rx: None,
            direct_download_worker: None,
            direct_download_update_view: false,
            ts3_plugin_update_prompt: None,
            ts3_plugin_cache: None,
            ts3_plugin_scan_rx: None,
            ts3_plugin_scanning: false,
            ts3_running_cache: None,
            prelaunch_recheck_at: None,
            backend_progress_rx: None,
            backend_worker: None,
            startup_pending_restore_rx: None,
            startup_pending_restore_worker: None,
            sync_started_at: None,
            startup_recheck_queue: VecDeque::new(),
            repository_space_sync_queue: VecDeque::new(),
            repository_visual_folder_sync_queue: VecDeque::new(),
            addon_hash_recalc_queue: VecDeque::new(),
            scheduler_active_run: None,
            scheduler_pending_post_action: None,
            scheduling_editor: None,
            syncing_repository: None,
            pending_repo_metadata_refresh: Vec::new(),
            pending_update_cache: HashMap::new(),
            mod_diff_cache: Vec::new(),
            progress_events: VecDeque::new(),
            update_modal_open: false,
            update_ready_repo: None,
            current_sync_mode: None,
            download_progress: None,
            download_finished: false,
            download_finished_repo: None,
            download_summary: None,
            open_update_after_sync: false,
            needs_repaint: false,
            mod_download_progress: HashMap::new(),
            download_started_at: None,
            download_stage_started_at: None,
            hash_stage_started_at: None,
            download_stage_duration: None,
            hash_stage_duration: None,
            cumulative_hash_duration: Duration::ZERO,
            download_speed_bps: 0.0,
            download_speed_sample_at: None,
            download_speed_sample_bytes: 0,
            total_downloaded_bytes: 0,
            download_eta_remaining: None,
            download_eta_updated_at: None,
            download_pause_tx: None,
            download_paused: false,
            cancel_tx: None,
            recheck_stage_label: None,
            recheck_stage_percent: None,
            recheck_hash_counter: None,
            recheck_hash_part_counter: None,
            last_hash_progress_repaint: None,
            download_hash_sample_at: None,
            download_hash_sample_files: 0,
            download_hash_sample_parts: 0,
            completed_repository_check_banner: None,
            completed_repository_db_wipe_banner: None,
            repo_states: HashMap::new(),
            repo_states_version: 0,
            repo_foxy_modes: HashMap::new(),
            pending_repository_context_confirmation: None,
            pending_repository_space_delete_id: None,
            pending_repository_visual_folder_edit: None,
            pending_repository_visual_folder_delete: None,
            startup_frame_rendered: false,
            startup_tasks_started: false,
            close_requested_at: None,
            update_modal_sorted_mod_indices: Vec::new(),
            update_modal_mod_name_lowers: Vec::new(),
            update_modal_sort_generation: 0,
            update_modal_sorted_generation: 0,
            update_modal_sort_last_progress_invalidation: None,
            activity_log_cache: Vec::new(),
            activity_log_generation: 0,
            activity_log_last_poll_at: None,
            activity_log_filter_error: true,
            activity_log_filter_warn: true,
            activity_log_filter_info: true,
            activity_log_filter_debug: false,
            activity_log_filter_trace: false,
            activity_log_search: String::new(),
            ui_toast: None,
            editor_launch_cooldown_until: None,
            last_incomplete_config_sync_toast_at: None,
            show_memory_diagnostics_window: false,
            fps_ema: 0.0,
            memory_diagnostics_history: VecDeque::new(),
            memory_diagnostics_pinned_baseline: None,
            memory_diagnostics_last_sample_at: None,
            memory_diagnostics_last_logged_stage_key: None,
            memory_diagnostics_process_map: None,
            memory_diagnostics_last_process_map_at: None,
            tracked_icon_texture_bytes: HashMap::new(),
            tracked_repo_image_texture_bytes: HashMap::new(),
            app_icon_texture_bytes: 0,
            default_repo_image_texture_bytes: 0,
            last_applied_palette: None,
            cached_color32: None,
            last_font_image_size: [0, 0],
            last_saved_window_state: Self::load_window_state(),
            last_logged_display_metrics: None,
            tray_manager: None,
            hidden_to_tray: false,
            persistence_request_tx,
            persistence_result_rx,
            settings_dirty: false,
            settings_revision: 0,
            settings_last_mutated_at: None,
            settings_save_in_flight_revision: None,
            settings_completed_revision: 0,
            repositories_dirty: false,
            repositories_revision: 0,
            repositories_last_mutated_at: None,
            repositories_save_in_flight_revision: None,
            repositories_completed_revision: 0,
            backup_inventory_refresh_requested: false,
            backup_inventory_refresh_in_progress: false,
            backup_inventory_request_id: 0,
            backup_inventory_in_flight_request_id: None,
            // App update system
            app_update_status: crate::core::tasks::app_update::UpdateCheckStatus::Idle,
            app_update_event_rx: None,
            app_update_download_rx: None,
            app_update_changelog_rx: None,
            app_update_changelog_tx: None,
            app_update_last_check: None,
            app_update_changelogs: Vec::new(),
            app_update_changelog_loading: HashSet::new(),
            app_update_changelogs_requested: false,
            // Swifty migration
            swifty_migration_state:
                crate::ui::views::swifty_migration::types::SwiftyMigrationState::default(),
        };
        app.load_settings();
        app.pending_renderer_fallback_notice =
            crate::core::utils::renderer_fallback::renderer_fallback_notice_path().exists();
        // Compare the local database schema generation against this binary's.
        // Bootstraps a sidecar for fresh/legacy databases (no prompt) and only
        // returns Some(..) when an out-of-date database needs an explicit wipe.
        app.pending_db_schema_wipe =
            crate::core::tasks::db_schema_version::evaluate_and_bootstrap();
        if app.settings_view_state.debug_mode && !app.launch_debug_mode {
            info!("Ignoring persisted debug mode; launch with `ui --debug-mode` to enable");
        }
        app.settings_view_state.debug_mode = app.launch_debug_mode;
        if app.launch_debug_mode {
            info!("Debug mode enabled by launch flag");
        }
        app.direct_download_destination_input = app.effective_temp_directory();
        let rollback_cleanup_root = std::path::PathBuf::from(app.effective_temp_directory());
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    log::warn!("Failed to create rollback startup cleanup runtime: {}", err);
                    return;
                }
            };
            if let Err(err) = runtime.block_on(
                crate::core::tasks::download_files::UpdateRollbackSession::cleanup_stale_sessions(
                    rollback_cleanup_root,
                    None,
                ),
            ) {
                log::warn!("Rollback startup cleanup failed: {}", err);
            }
        });
        app.apply_runtime_palette_visuals(&cc.egui_ctx);
        app.apply_runtime_ui_scale(&cc.egui_ctx);
        // Capture the renderer eframe actually created so `health` can report
        // it (wgpu vs the glow fallback path).
        let active_renderer = if cc.wgpu_render_state.is_some() {
            "wgpu"
        } else {
            "glow"
        };
        app.initialize_agent_gui(&cc.egui_ctx, &agent_gui, active_renderer);
        app.i18n.set_language(&app.settings_view_state.locale);
        app.load_repositories();
        app.load_repository_spaces();
        app.reconcile_repository_space_paths();
        app.load_repository_visual_folders();
        api::log_startup_system_diagnostics(&app.startup_storage_paths());
        info!(
            "Startup state loaded: repositories={} repository_spaces={} debug_mode={}",
            app.repository_view_state.repositories.len(),
            app.repository_spaces.len(),
            app.settings_view_state.debug_mode
        );
        app.update_debug_mode();
        app.previous_debug_mode = app.settings_view_state.debug_mode;
        let icon_bytes = include_bytes!("../../icons/foxy_256.png");
        if let Ok(image) = image::load_from_memory(icon_bytes).map(|img| img.to_rgba8()) {
            let (icon_width, icon_height) = image.dimensions();
            app.app_icon_texture_bytes = (icon_width as usize)
                .saturating_mul(icon_height as usize)
                .saturating_mul(4);
            let texture = cc.egui_ctx.load_texture(
                "app_icon",
                egui::ColorImage::from_rgba_unmultiplied(
                    [icon_width as usize, icon_height as usize],
                    &image,
                ),
                Default::default(),
            );
            app.app_icon = Some(texture);
        } else {
            log::error!("Failed to load embedded icon.");
        }
        let repo_placeholder_bytes = include_bytes!("../../repo-image-placeholder.png");
        if let Ok(image) = image::load_from_memory(repo_placeholder_bytes).map(|img| img.to_rgba8())
        {
            let (image_width, image_height) = image.dimensions();
            app.default_repo_image_texture_bytes = (image_width as usize)
                .saturating_mul(image_height as usize)
                .saturating_mul(4);
            let texture = cc.egui_ctx.load_texture(
                "repo_image_placeholder",
                egui::ColorImage::from_rgba_unmultiplied(
                    [image_width as usize, image_height as usize],
                    &image,
                ),
                Default::default(),
            );
            app.default_repo_image = Some(texture);
        } else {
            log::error!("Failed to load repository placeholder image.");
        }
        let repos = app.repository_view_state.repositories.clone();
        for repo in repos {
            if !repo.icon_image_checksum.is_empty() {
                app.download_and_load_image(
                    &cc.egui_ctx,
                    &repo.address,
                    &repo.icon_image_path,
                    &repo.icon_image_checksum,
                    true,
                );
            }
            if !repo.repo_image_checksum.is_empty() {
                app.download_and_load_image(
                    &cc.egui_ctx,
                    &repo.address,
                    &repo.repo_image_path,
                    &repo.repo_image_checksum,
                    false,
                );
            }
        }
        let spaces = app.repository_spaces.clone();
        for space in spaces {
            if !space.icon_image_checksum.is_empty() {
                app.download_and_load_image(
                    &cc.egui_ctx,
                    &space.source_base_url,
                    &space.icon_image_path,
                    &space.icon_image_checksum,
                    true,
                );
            }
            if !space.repo_image_checksum.is_empty() {
                app.download_and_load_image(
                    &cc.egui_ctx,
                    &space.source_base_url,
                    &space.repo_image_path,
                    &space.repo_image_checksum,
                    false,
                );
            }
        }

        // Detect Arma 3 profiles on startup
        let custom_profiles_dir = app.settings_view_state.arma3_profiles_directory.trim();
        let custom_profiles_dir = if custom_profiles_dir.is_empty() {
            None
        } else {
            Some(std::path::Path::new(custom_profiles_dir))
        };
        app.detected_arma3_profiles =
            crate::core::arma3_profiles::detect_all_profiles(custom_profiles_dir);
        app.detected_active_arma3_profile =
            crate::core::arma3_profiles::detect_active_profile(&app.detected_arma3_profiles);

        // First-run Swifty migration: if not yet offered, repos are empty, and Swifty data exists
        if !app.settings_view_state.swifty_migration_offered
            && app.repository_view_state.repositories.is_empty()
            && crate::ui::views::swifty_migration::scanner::swifty_data_exists()
        {
            info!("First-run Swifty migration detected; opening migration view");
            app.current_view = FoxyView::SwiftyMigration;
            app.ensure_swifty_migration_scanned();
        }

        app
    }

    pub(crate) fn startup_storage_paths(&self) -> Vec<api::StartupStoragePath> {
        let mut paths = vec![
            api::StartupStoragePath::new("app_data", Self::get_config_directory()),
            api::StartupStoragePath::new("logs", crate::core::utils::app_paths::foxy_logs_dir()),
            api::StartupStoragePath::new(
                "database",
                Self::get_config_directory().join("database.db"),
            ),
            api::StartupStoragePath::new("temp", self.effective_temp_directory()),
        ];
        if let Some(backup_dir) = self.configured_backup_directory() {
            paths.push(api::StartupStoragePath::new("backups", backup_dir));
        }

        push_configured_path(
            &mut paths,
            "arma3",
            self.settings_view_state.arma3_directory.trim(),
        );
        push_configured_path(
            &mut paths,
            "arma3_profiles",
            self.settings_view_state.arma3_profiles_directory.trim(),
        );
        push_configured_path(
            &mut paths,
            "steam",
            self.settings_view_state.steam_directory.trim(),
        );

        for folder in &self.settings_view_state.additional_folders {
            push_configured_path(&mut paths, "additional_folder", folder.trim());
        }
        for (folder, enabled) in &self.settings_view_state.cleanup_folders {
            if *enabled {
                push_configured_path(&mut paths, "cleanup_folder", folder.trim());
            }
        }
        for repo in &self.repository_view_state.repositories {
            push_configured_path(&mut paths, "repository", repo.path.trim());
        }
        for space in &self.repository_spaces {
            push_configured_path(&mut paths, "repository_space", space.shared_path.trim());
        }

        paths
    }

    pub(in crate::ui::app) fn is_generated_debug_folder(path: &str) -> bool {
        path.trim_start().starts_with("Debug Folder ")
    }

    pub(in crate::ui::app) fn is_generated_debug_repository(repo: &Repository) -> bool {
        repo.name.trim_start().starts_with("Debug Repo ")
            && repo
                .addons
                .iter()
                .all(|(name, _)| name.starts_with("Debug "))
            && repo
                .optional_addons
                .iter()
                .all(|(name, _)| name.starts_with("Debug "))
    }

    pub(in crate::ui::app) fn sanitize_settings_debug_artifacts(&mut self) {
        self.settings_view_state
            .additional_folders
            .retain(|folder| !Self::is_generated_debug_folder(folder));
        self.settings_view_state
            .cleanup_folders
            .retain(|(folder, _)| !Self::is_generated_debug_folder(folder));
        self.settings_view_state.additional_folders_filter.clear();
        self.settings_view_state.cleanup_folders_filter.clear();
    }

    fn remove_synthetic_debug_repositories(&mut self) {
        let previous_selected_address = self
            .repository_view_state
            .selected_repository
            .and_then(|idx| self.repository_view_state.repositories.get(idx))
            .map(|repo| repo.address.clone());

        self.repository_view_state
            .repositories
            .retain(|repo| !Self::is_generated_debug_repository(repo));
        self.bump_repository_list_data_version();

        self.repository_view_state.selected_repository =
            previous_selected_address.and_then(|addr| {
                self.repository_view_state
                    .repositories
                    .iter()
                    .position(|repo| repo.address == addr)
            });

        if self.repository_view_state.selected_repository.is_none()
            && !self.repository_view_state.repositories.is_empty()
        {
            self.repository_view_state.selected_repository = Some(0);
        }
    }

    pub(in crate::ui::app) fn sync_debug_runtime_state(&mut self) {
        self.show_debug_windows = self.settings_view_state.show_debug_windows;
        if !self.settings_view_state.show_memory_diagnostics_icon {
            self.show_memory_diagnostics_window = false;
        }
        self.previous_debug_mode = self.settings_view_state.debug_mode;
    }

    pub fn update_debug_mode(&mut self) {
        let debug_mode = self.settings_view_state.debug_mode;
        let show_debug_windows = self.settings_view_state.show_debug_windows;
        info!("Applying debug mode: {}", debug_mode);
        if debug_mode {
            if self.stored_settings.is_none() {
                self.stored_settings = Some(self.settings_view_state.clone());
            }
            if self.stored_repositories.is_none() {
                self.stored_repositories = Some(self.repository_view_state.clone());
            }

            self.repository_view_state.repositories = (1..=30)
                .map(|i| Repository {
                    name: format!("Debug Repo {}", i),
                    addons: (1..=20)
                        .map(|j| (format!("Debug Addon {}-{}", i, j), true))
                        .collect(),
                    optional_addons: (1..=20)
                        .map(|j| (format!("Debug Optional Addon {}-{}", i, j), true))
                        .collect(),
                    external_addons: (1..=20)
                        .map(|j| {
                            (
                                format!("Debug External Addon {}-{}", i, j),
                                true,
                                String::new(),
                            )
                        })
                        .collect(),
                    ..Default::default()
                })
                .collect();
            self.bump_repository_list_data_version();

            self.settings_view_state.additional_folders =
                (1..=30).map(|i| format!("Debug Folder {}", i)).collect();
            self.settings_view_state.additional_folder_aliases.clear();

            self.settings_view_state.cleanup_folders = (1..=30)
                .map(|i| (format!("Debug Folder {}", i), false))
                .collect();
        } else {
            if let Some(settings) = self.stored_settings.take() {
                self.settings_view_state = settings;
            } else {
                self.settings_view_state = SettingsViewState::default();
                self.load_settings();
            }

            if let Some(repositories) = self.stored_repositories.take() {
                self.repository_view_state = repositories;
                self.bump_repository_list_data_version();
            } else {
                self.repository_view_state = RepositoryViewState::default();
                self.bump_repository_list_data_version();
                self.load_repositories();
            }
            self.reconcile_repository_space_paths();
            self.remove_synthetic_debug_repositories();
        }

        self.settings_view_state.debug_mode = debug_mode;
        self.settings_view_state.show_debug_windows = show_debug_windows;
        self.sanitize_settings_debug_artifacts();
        self.sync_debug_runtime_state();
        self.i18n.set_language(&self.settings_view_state.locale);

        self.save_settings();
    }
}
