use super::LaunchDispatchResult;
use super::launch_policy::{
    join_preflight_fact_keys, merge_join_preflight_choices, paths_overlap, repository_launch_paths,
};
use crate::core::addon_metadata::load_addon_display_name_snapshot;
use crate::core::arma3_missions::EditorMission;
use crate::core::arma3_server_query::{
    ServerAddonQueryResult, creator_dlc_by_app_id, query_server_addon_requirements,
};
use crate::core::steam;
use crate::ui::app::{
    Foxy, JoinPreflightQueryResult, LaunchRequest, PendingJoinPreflightQuery,
    PendingJoinStatusQuery, PendingMissionEditorLaunchWarningState, PendingUpdateLaunch,
};
use crate::ui::types::{Repository, RepositoryServer, ServerOnlineStatus};
use eframe::egui;
use log::{info, warn};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const EDITOR_LAUNCH_COOLDOWN: Duration = Duration::from_secs(30);
static NEXT_JOIN_PREFLIGHT_QUERY_ID: AtomicU64 = AtomicU64::new(1);

impl Foxy {
    fn repository_space_shared_path(&self, repository: &Repository) -> Option<&str> {
        let space_id = repository.repository_space_id.as_deref()?;
        self.repository_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.shared_path.as_str())
    }

    fn conflicting_download_instance(&self, target: &Repository) -> Option<String> {
        if self.current_sync_mode != Some(crate::core::api::SyncMode::Download) {
            return None;
        }
        let source = self
            .syncing_repository
            .and_then(|index| self.repository_view_state.repositories.get(index))?;
        let source_shared = self.repository_space_shared_path(source);
        let target_shared = self.repository_space_shared_path(target);
        let source_paths = repository_launch_paths(source, source_shared);
        let target_paths = repository_launch_paths(target, target_shared);
        source_paths
            .iter()
            .any(|source_path| {
                target_paths
                    .iter()
                    .any(|target_path| paths_overlap(source_path, target_path))
            })
            .then(|| Self::repo_instance_key(&source.address, &source.path))
    }

    fn queue_launch_if_conflicting_update(
        &mut self,
        effective: &Repository,
        server: Option<&RepositoryServer>,
        repo_name: &str,
        launch_label: &str,
    ) -> bool {
        let target_instance_key = Self::repo_instance_key(&effective.address, &effective.path);
        if let Some(pending) = self.pending_update_launch.as_mut()
            && pending.target_instance_key == target_instance_key
        {
            if pending.server.as_ref() != server || pending.repository != *effective {
                pending.replacement = Some(Box::new(PendingUpdateLaunch {
                    repository: effective.clone(),
                    server: server.cloned(),
                    repo_name: repo_name.to_string(),
                    blocking_instance_key: pending.blocking_instance_key.clone(),
                    target_instance_key,
                    update_finished: pending.update_finished,
                    update_succeeded: pending.update_succeeded,
                    replacement: None,
                }));
            }
            return true;
        }
        let Some(blocking_instance_key) = self.conflicting_download_instance(effective) else {
            return false;
        };
        if let Some(pending) = self.pending_update_launch.as_mut() {
            pending.replacement = Some(Box::new(PendingUpdateLaunch {
                repository: effective.clone(),
                server: server.cloned(),
                repo_name: repo_name.to_string(),
                blocking_instance_key: pending.blocking_instance_key.clone(),
                target_instance_key,
                update_finished: pending.update_finished,
                update_succeeded: pending.update_succeeded,
                replacement: None,
            }));
            return true;
        }
        info!(
            "Queued {} for repository {} until overlapping update completes",
            launch_label, repo_name
        );
        self.pending_update_launch = Some(PendingUpdateLaunch {
            repository: effective.clone(),
            server: server.cloned(),
            repo_name: repo_name.to_string(),
            blocking_instance_key,
            target_instance_key,
            update_finished: false,
            update_succeeded: false,
            replacement: None,
        });
        true
    }

    pub(crate) fn finish_queued_update_launch(
        &mut self,
        blocking_instance_key: &str,
        succeeded: bool,
    ) {
        let Some(pending) = self.pending_update_launch.as_mut() else {
            return;
        };
        if pending.blocking_instance_key != blocking_instance_key {
            return;
        }
        pending.update_finished = true;
        pending.update_succeeded = succeeded;
    }

    pub(crate) fn render_queued_update_launch(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_update_launch.clone() else {
            return;
        };
        if let Some(replacement) = pending.replacement.as_ref() {
            let mut replace = false;
            let mut keep = false;
            egui::Modal::new(egui::Id::unique("replace_queued_launch")).show(ctx, |ui| {
                ui.heading(self.t("Replace queued launch?"));
                ui.label(self.t_fmt(
                    "Current request: {target}",
                    &[(
                        "target",
                        format!(
                                "{} - {} ({}) - {}",
                                pending.repo_name,
                                pending
                                    .server
                                    .as_ref()
                                    .map(|server| server.name.as_str())
                                    .unwrap_or("Launch"),
                                pending
                                    .repository
                                    .selected_profile
                                    .as_deref()
                                    .unwrap_or("Default"),
                                pending.repository.path
                            ),
                    )],
                ));
                ui.label(self.t_fmt(
                    "New request: {target}",
                    &[(
                        "target",
                        format!(
                                "{} - {} ({}) - {}",
                                replacement.repo_name,
                                replacement
                                    .server
                                    .as_ref()
                                    .map(|server| server.name.as_str())
                                    .unwrap_or("Launch"),
                                replacement
                                    .repository
                                    .selected_profile
                                    .as_deref()
                                    .unwrap_or("Default"),
                                replacement.repository.path
                            ),
                    )],
                ));
                ui.horizontal(|ui| {
                    keep = ui.button(self.t("Keep current request")).clicked();
                    replace = ui.button(self.t("Replace queued launch")).clicked();
                });
            });
            if replace {
                let mut chosen = *replacement.clone();
                chosen.update_finished = pending.update_finished;
                chosen.update_succeeded = pending.update_succeeded;
                chosen.blocking_instance_key = pending.blocking_instance_key;
                self.pending_update_launch = Some(chosen);
            } else if keep && let Some(current) = self.pending_update_launch.as_mut() {
                current.replacement = None;
            }
            return;
        }
        let mut cancel = false;
        egui::Window::new(self.t("Launch queued for update"))
            .frame(self.modal_window_chrome(ctx))
            .title_frame(self.modal_window_chrome(ctx))
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(480.0)
            .show(ctx, |ui| {
                if pending.update_finished && !pending.update_succeeded {
                    ui.label(self.t("The update did not complete. The game was not launched."));
                } else {
                    ui.label(self.t_fmt(
                        "Waiting for the update to finish before launching {repository}.",
                        &[("repository", pending.repo_name.clone())],
                    ));
                    if !pending.update_finished {
                        ui.add(crate::ui::app::PacedSpinner::new());
                    }
                }
                ui.add_space(12.0);
                cancel = ui.button(self.t("Cancel queued launch")).clicked();
            });
        if cancel {
            self.pending_update_launch = None;
            return;
        }
        if !pending.update_finished {
            return;
        }
        if !pending.update_succeeded {
            self.pending_update_launch = None;
            self.show_error_toast(
                self.t("The update did not complete. The game was not launched."),
            );
            return;
        }
        if let Some(blocker) = self.conflicting_download_instance(&pending.repository) {
            if let Some(queued) = self.pending_update_launch.as_mut() {
                queued.blocking_instance_key = blocker;
                queued.update_finished = false;
                queued.update_succeeded = false;
            }
            return;
        }
        self.pending_update_launch = None;
        if self
            .repo_index_by_instance(
                &Self::normalize_repo_url(&pending.repository.address),
                &Self::repo_instance_path_key(&pending.repository.path),
            )
            .is_none()
        {
            return;
        }
        if let Some(server) = pending.server.as_ref() {
            self.try_join_repository_server(ctx, &pending.repository, server, &pending.repo_name);
        } else {
            self.present_launch_preflight(ctx, &pending.repository, &pending.repo_name);
        }
    }

    fn enabled_external_addons_for_editor_warning(repo: &Repository) -> Vec<String> {
        let mut addons: Vec<String> = repo
            .external_addons
            .iter()
            .filter(|(_, enabled, _)| *enabled)
            .map(|(addon, _, path)| {
                let addon = addon.trim();
                let path = path.trim();
                if path.is_empty() {
                    addon.to_string()
                } else {
                    format!("{addon} ({path})")
                }
            })
            .filter(|label| !label.is_empty())
            .collect();

        addons.sort();
        addons.dedup();
        addons
    }

    pub(crate) fn handle_post_launch_window_behavior(
        &mut self,
        ctx: &egui::Context,
        reason: &'static str,
    ) {
        if self.settings_view_state.close_after_launch {
            info!("App configured to close after launch");
            self.request_app_close(ctx, reason);
        } else if self.settings_view_state.hide_to_tray_after_launch {
            self.hide_app_to_tray(ctx, reason);
        }
    }

    pub(crate) fn launch_repository_with_database_guard(
        &mut self,
        effective: &Repository,
        server: Option<&RepositoryServer>,
        repo_name: &str,
        launch_label: &'static str,
    ) -> LaunchDispatchResult {
        if self.queue_launch_if_conflicting_update(effective, server, repo_name, launch_label) {
            return LaunchDispatchResult::Deferred;
        }
        self.refresh_database_repair_requirement();
        if self.database_updates_blocked() {
            if self.pending_database_launch.is_none() {
                self.pending_database_launch = Some(crate::ui::app::DatabaseLaunchContinuation {
                    repository: effective.clone(),
                    server: server.cloned(),
                    repo_name: repo_name.to_string(),
                    launch_label,
                });
            }
            self.show_db_schema_wipe_prompt = true;
            self.needs_repaint = true;
            return LaunchDispatchResult::Deferred;
        }
        self.launch_repository_with_steam_guard(effective, server, repo_name, launch_label)
    }

    /// A game build the repository downloaded starts its own executable, never
    /// a Steam install.
    fn repository_launch_uses_steam(&self, effective: &Repository) -> bool {
        effective.game_build.is_none()
            && crate::core::game::registry()
                .active()
                .steam_app_id_from_settings(&self.settings_view_state)
                .is_some()
    }

    fn show_game_build_launch_error(&mut self, effective: &Repository, repo_name: &str) {
        let Some(build) = effective.game_build.as_ref() else {
            return;
        };
        let repo_path = Path::new(effective.path.trim());
        let dir = crate::core::utils::fs_safety::resolve_child_dir_case_insensitive(
            repo_path,
            &build.addon,
        );
        let message = match dir {
            None => self.t_fmt(
                "{game} has not been downloaded yet. Update the repository first.",
                &[("game", repo_name.to_string())],
            ),
            Some(dir) if !dir.join(&build.executable).is_file() => self.t_fmt(
                "{file} was not found in the downloaded {game} files. Recheck the repository.",
                &[
                    ("file", build.executable.clone()),
                    ("game", repo_name.to_string()),
                ],
            ),
            Some(_) => self.t_fmt(
                "{game} could not be launched. Check the executable and launch settings in Game space settings.",
                &[("game", repo_name.to_string())],
            ),
        };
        self.show_error_toast(message);
    }

    pub(crate) fn launch_repository_with_steam_guard(
        &mut self,
        effective: &Repository,
        server: Option<&RepositoryServer>,
        repo_name: &str,
        launch_label: &str,
    ) -> LaunchDispatchResult {
        let Some(command) = self.create_launch_command(effective, server) else {
            let module = crate::core::game::registry().active();
            if !module.capabilities().repository_launch {
                self.show_error_toast(self.t_fmt(
                    "Launching from a repository is not supported for {game}.",
                    &[("game", module.display_name().to_string())],
                ));
                return LaunchDispatchResult::Failed;
            }
            if effective.game_build.is_some() {
                self.show_game_build_launch_error(effective, repo_name);
                return LaunchDispatchResult::Failed;
            }
            let game = module.display_name().to_string();
            let install_dir = module
                .install_dir_from_settings(&self.settings_view_state)
                .trim()
                .to_string();
            #[cfg(target_os = "linux")]
            {
                let _ = install_dir;
                self.show_error_toast(
                    self.t(
                        "Could not find the Steam client. Set the Steam directory in Settings or start it manually.",
                    ),
                );
            }
            #[cfg(not(target_os = "linux"))]
            {
                if install_dir.is_empty() {
                    self.show_error_toast(self.t_fmt(
                        "{game} directory is not configured. Set it in Game space settings.",
                        &[("game", game)],
                    ));
                } else if !std::path::Path::new(&install_dir).exists() {
                    self.show_error_toast(self.t_fmt(
                        "{game} directory does not exist. Check the path in Game space settings.",
                        &[("game", game)],
                    ));
                } else if !module.validate_install_dir(std::path::Path::new(&install_dir)) {
                    self.show_error_toast(self.t_fmt(
                        "{game} executable not found at the configured path.",
                        &[("game", game)],
                    ));
                } else {
                    self.show_error_toast(self.t_fmt(
                        "{game} could not be launched. Check the executable and launch settings in Game space settings.",
                        &[("game", game)],
                    ));
                }
            }
            return LaunchDispatchResult::Failed;
        };

        let _ = (command, launch_label);
        self.start_launch_action(crate::ui::app::LaunchRequest {
            repository: effective.clone(),
            server: server.cloned(),
            name: repo_name.to_string(),
            mission_path: None,
        });
        LaunchDispatchResult::Deferred
    }

    pub(super) fn try_join_repository_server(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
    ) {
        if self.pending_join_preflight.is_some() {
            info!(
                "Join request ignored for repository {} while join addon preflight dialog is open",
                repo_name
            );
            return;
        }
        if self.pending_join_preflight_query.is_some()
            || self
                .join_preflight_worker
                .as_ref()
                .is_some_and(|h| !h.is_finished())
        {
            info!(
                "Join request ignored for repository {} while join addon preflight query is running",
                repo_name
            );
            return;
        }

        if self.pending_join_status_query.is_some() {
            info!(
                "Join request ignored for repository {} while a server-status query is in flight",
                repo_name
            );
            return;
        }

        let ts3_gate = self.evaluate_ts3_join_gate(effective, repo_name);
        let steam_gate = self.evaluate_steam_launch_gate(effective, repo_name);
        if let Some(preflight) = crate::ui::app::PendingJoinPreflightState::for_join_readiness(
            repo_name,
            server.clone(),
            effective.clone(),
            ts3_gate,
            steam_gate,
        ) {
            self.prelaunch_process_checks = Default::default();
            self.pending_join_preflight = Some(preflight);
            ctx.request_repaint();
            return;
        }

        self.start_join_server_query(effective, server, repo_name);
    }

    pub(super) fn start_join_server_query(
        &mut self,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
    ) {
        info!(
            "Join requested for repository {} server {}:{}",
            repo_name, server.address, server.port
        );

        // Query the server status off the UI thread (DNS resolution + UDP
        // round-trip). The join decision resumes in `poll_pending_join_status`
        // once a fresh status arrives, so a slow or unreachable server can no
        // longer freeze the UI.
        let (query_id, started_at, result_rx) = self.start_join_server_status_query(server);
        self.pending_join_status_query = Some(PendingJoinStatusQuery {
            repo_name: repo_name.to_string(),
            server: server.clone(),
            effective_repository: effective.clone(),
            query_id,
            started_at,
            result_rx,
        });
        self.needs_repaint = true;
    }

    /// Resume a pending Join once its background server-status query resolves,
    /// or give up after a timeout so the join action never wedges if the worker
    /// fails to deliver a result.
    pub(crate) fn poll_pending_join_status(&mut self, ctx: &egui::Context) {
        const JOIN_STATUS_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

        // Peek by reference first: the common case is "still waiting", and the
        // pending query embeds a full `Repository`, so we must not clone it
        // every frame just to find nothing ready.
        let Some(pending) = self.pending_join_status_query.as_ref() else {
            return;
        };

        let result = pending.result_rx.try_recv().ok();
        let status = result
            .filter(|result| result.matches_pending(pending.query_id, pending.started_at))
            .map(|result| result.status);
        if status.is_none() && pending.started_at.elapsed() < JOIN_STATUS_QUERY_TIMEOUT {
            return; // still waiting - no clone, no take
        }

        // A status is ready, or we've timed out: now consume the pending query.
        let pending = self
            .pending_join_status_query
            .take()
            .expect("pending join status present");
        match status {
            Some(status) => self.continue_join_after_status(ctx, &pending, status),
            None => {
                warn!(
                    "Join cancelled for repository {} because the server-status query for {}:{} timed out",
                    pending.repo_name, pending.server.address, pending.server.port
                );
                self.show_error_toast(self.t("Cannot join: server appears offline."));
                self.needs_repaint = true;
            }
        }
    }

    pub(crate) fn render_pending_join_status(&mut self, ctx: &egui::Context) {
        if self.pending_join_status_query.is_none() {
            return;
        }
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("pending_join_status_query")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(self.t("Refreshing server and local addon requirements..."));
            ui.add(crate::ui::app::PacedSpinner::new());
            cancel = ui.button(self.t("Cancel")).clicked();
        });
        if cancel {
            self.pending_join_status_query = None;
            self.needs_repaint = true;
        }
    }

    /// Apply the join decision now that a server status is known. Mirrors the
    /// logic that previously ran synchronously in `try_join_repository_server`.
    fn continue_join_after_status(
        &mut self,
        ctx: &egui::Context,
        pending: &PendingJoinStatusQuery,
        status: ServerOnlineStatus,
    ) {
        let effective = &pending.effective_repository;
        let server = &pending.server;
        let repo_name = pending.repo_name.as_str();
        match status {
            ServerOnlineStatus::Online { .. } => {
                info!(
                    "Join server status online for repository {} server {}:{}",
                    repo_name, server.address, server.port
                );
                let preflight_supported = crate::core::game::registry()
                    .active()
                    .capabilities()
                    .join_addon_preflight;
                if preflight_supported && self.repo_check_server_addons_before_join(effective) {
                    info!(
                        "Join addon preflight enabled for repository {} server {}:{}",
                        repo_name, server.address, server.port
                    );
                    match server.port.parse::<u16>() {
                        Ok(port) => {
                            self.spawn_join_preflight_query(
                                ctx, effective, server, repo_name, port,
                            );
                            return;
                        }
                        Err(err) => {
                            warn!(
                                "Join addon preflight skipped for repository {} because server port {} is invalid: {}",
                                repo_name, server.port, err
                            );
                        }
                    }
                } else {
                    info!(
                        "Join addon preflight disabled for repository {} server {}:{}",
                        repo_name, server.address, server.port
                    );
                }

                info!(
                    "Joining repository {} without addon preflight dialog because no addon preflight gate was active",
                    repo_name
                );
                self.present_join_addon_preflight(ctx, effective, server, repo_name, None);
            }
            ServerOnlineStatus::Offline => {
                warn!(
                    "Join cancelled for repository {} because selected server {}:{} appears offline",
                    repo_name, server.address, server.port
                );
                self.show_error_toast(self.t("Cannot join: server appears offline."));
            }
        }
    }

    fn spawn_join_preflight_query(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
        port: u16,
    ) {
        if self.pending_join_preflight_query.is_some()
            || self
                .join_preflight_worker
                .as_ref()
                .is_some_and(|h| !h.is_finished())
        {
            info!("Join addon preflight is already running; ignoring duplicate join request");
            return;
        }

        self.pending_join_preflight_query = Some(PendingJoinPreflightQuery {
            query_id: NEXT_JOIN_PREFLIGHT_QUERY_ID.fetch_add(1, Ordering::Relaxed),
            repo_name: repo_name.to_string(),
            server: server.clone(),
            original_repository: effective.clone(),
        });

        let address = server.address.clone();
        let database_metadata_available = !self.database_updates_blocked();
        let repo_urls = self
            .repository_view_state
            .repositories
            .iter()
            .map(|repo| Self::normalize_repo_url(&repo.address))
            .filter(|url| !url.trim().is_empty())
            .collect::<Vec<_>>();
        info!(
            "Starting join addon preflight query for repository {} server {}:{}",
            repo_name, address, port
        );
        let tx = self.join_preflight_result_tx.clone();
        let query_id = self
            .pending_join_preflight_query
            .as_ref()
            .expect("preflight request was just installed")
            .query_id;
        let repaint_ctx = self.repaint_ctx.clone().or_else(|| Some(ctx.clone()));
        self.join_preflight_worker = Some(std::thread::spawn(move || {
            let started_at = Instant::now();
            info!(
                "Join addon preflight worker querying server addon metadata from {}:{}",
                address, port
            );
            let result = query_server_addon_requirements(&address, port).map_err(|err| {
                format!(
                    "Failed to query addon metadata from {}:{}: {}",
                    address, port, err
                )
            });
            let display_names = if database_metadata_available {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match rt {
                    Ok(rt) => rt.block_on(async {
                        let db = crate::core::db::FoxyDb::from_handle(
                            crate::core::tasks::init_database::init_database().await,
                        );
                        load_addon_display_name_snapshot(&db, &repo_urls).await
                    }),
                    Err(err) => {
                        warn!(
                            "Join addon preflight worker failed to build DB runtime for display-name snapshot: {}",
                            err
                        );
                        Default::default()
                    }
                }
            } else {
                Default::default()
            };
            match &result {
                Ok(query_result) => {
                    info!(
                        "Join addon preflight worker finished query for {}:{} in {:.2}s: query_port={}, requirements={}, rules={}, server_browser_protocol={}",
                        address,
                        port,
                        started_at.elapsed().as_secs_f32(),
                        query_result.query_port,
                        query_result.requirements.len(),
                        query_result.rules.len(),
                        query_result.server_browser_protocol.is_some()
                    );
                }
                Err(err) => {
                    warn!(
                        "Join addon preflight worker failed query for {}:{} in {:.2}s: {}",
                        address,
                        port,
                        started_at.elapsed().as_secs_f32(),
                        err
                    );
                }
            }
            if tx
                .send(JoinPreflightQueryResult {
                    query_id,
                    address,
                    port,
                    result,
                    display_names,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        }));
    }

    pub(crate) fn finish_join_preflight(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
        result: &ServerAddonQueryResult,
        display_names: &crate::core::addon_metadata::AddonDisplayNameSnapshot,
    ) {
        Self::log_join_preflight_query_output(repo_name, server, result);
        let confirmation = self
            .pending_join_preflight
            .as_ref()
            .filter(|pending| {
                pending.server.address == server.address
                    && pending.server.port == server.port
                    && pending.final_confirmation_choice.is_some()
            })
            .cloned();
        let mut addon_state = Self::build_join_preflight_state(
            effective,
            &self.repository_view_state.repositories,
            server,
            repo_name,
            &result.requirements,
            display_names,
        );
        Self::merge_join_preflight_dlc_changes(
            &mut addon_state,
            effective,
            server,
            repo_name,
            result,
        );
        if let Some(previous) = confirmation {
            let mut refreshed = addon_state.take().unwrap_or_else(|| {
                crate::ui::app::PendingJoinPreflightState::empty(
                    repo_name,
                    server.clone(),
                    effective.clone(),
                    false,
                )
            });
            merge_join_preflight_choices(&previous, &mut refreshed);
            let facts_changed =
                join_preflight_fact_keys(&previous) != join_preflight_fact_keys(&refreshed);
            let keep_suggestions = previous.final_confirmation_choice.unwrap_or(false);
            self.pending_join_preflight = None;
            if facts_changed {
                refreshed.final_facts_refreshed = true;
                refreshed.final_confirmation_choice = None;
                self.pending_join_preflight = Some(refreshed);
                self.prelaunch_process_checks = Default::default();
                ctx.request_repaint();
                return;
            }
            refreshed.final_facts_refreshed = true;
            self.dispatch_refreshed_join(ctx, &refreshed, keep_suggestions);
            return;
        }
        if let Some(preflight) = &addon_state {
            info!(
                "Join addon preflight dialog opened for repository {} server {}:{}: requirements={}, suggestions={}, ambiguous={}, known_remote={}, extra_enabled={}",
                repo_name,
                server.address,
                server.port,
                result.requirements.len(),
                preflight.suggestions.len(),
                preflight.ambiguous.len(),
                preflight.known_remote.len(),
                preflight.extra_enabled.len()
            );
            Self::log_join_preflight_modal_contents(preflight, result.requirements.len());
        } else {
            info!(
                "Join addon preflight found no actionable addon changes for repository {} server {}:{} (requirements={})",
                repo_name,
                server.address,
                server.port,
                result.requirements.len()
            );
        }
        self.present_join_addon_preflight(ctx, effective, server, repo_name, addon_state);
    }

    fn dispatch_refreshed_join(
        &mut self,
        ctx: &egui::Context,
        pending: &crate::ui::app::PendingJoinPreflightState,
        keep_suggestions: bool,
    ) {
        let mut pending = pending.clone();
        if pending.steam_required {
            pending.steam_running =
                steam::is_steam_running(&self.settings_view_state.steam_directory);
            if !pending.can_launch() {
                self.pending_join_preflight = Some(pending);
                ctx.request_repaint();
                return;
            }
        }
        let repository = if keep_suggestions {
            Self::repository_with_join_preflight_selections(&pending)
        } else {
            Self::repository_without_join_preflight_suggestions(&pending)
        };
        let result = self.launch_repository_with_database_guard(
            &repository,
            Some(&pending.server),
            &pending.repo_name,
            "join",
        );
        if result == super::LaunchDispatchResult::Launched {
            self.handle_post_launch_window_behavior(ctx, "join launch completed");
        }
    }

    pub(super) fn confirm_final_join_facts(
        &mut self,
        ctx: &egui::Context,
        pending: &crate::ui::app::PendingJoinPreflightState,
        keep_suggestions: bool,
    ) -> bool {
        if pending.launch_only {
            return false;
        }
        if self.pending_join_preflight_query.is_some()
            || self
                .join_preflight_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
        {
            return true;
        }
        let Ok(port) = pending.server.port.parse::<u16>() else {
            self.show_error_toast(
                self.t("Cannot refresh server requirements because its query port is invalid."),
            );
            return true;
        };
        let mut pending = pending.clone();
        pending.final_confirmation_choice = Some(keep_suggestions);
        pending.final_query_error = None;
        self.pending_join_preflight = Some(pending.clone());
        self.spawn_join_preflight_query(
            ctx,
            &pending.original_repository,
            &pending.server,
            &pending.repo_name,
            port,
        );
        true
    }

    /// Adds the Creator DLC differences to the join modal. Skipped when the
    /// server's DLC list could not be decoded, since an unknown list must not
    /// be read as "the server runs no DLCs".
    fn merge_join_preflight_dlc_changes(
        state: &mut Option<crate::ui::app::PendingJoinPreflightState>,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
        result: &ServerAddonQueryResult,
    ) {
        if !crate::core::game::registry()
            .active()
            .capabilities()
            .creator_dlc
        {
            return;
        }
        let Some(server_app_ids) = result
            .server_browser_protocol
            .as_ref()
            .and_then(|protocol| protocol.creator_dlc_app_ids.as_deref())
        else {
            info!(
                "Join DLC preflight skipped for repository {} server {}:{}: server did not report a decodable Creator DLC list",
                repo_name, server.address, server.port
            );
            return;
        };
        let unknown_app_ids = server_app_ids
            .iter()
            .filter(|app_id| creator_dlc_by_app_id(**app_id).is_none())
            .collect::<Vec<_>>();
        if !unknown_app_ids.is_empty() {
            warn!(
                "Join DLC preflight for repository {} server {}:{}: server runs unrecognized Creator DLC app IDs {:?}",
                repo_name, server.address, server.port, unknown_app_ids
            );
        }
        let (dlc_enable, dlc_disable) = Self::join_preflight_dlc_changes(effective, server_app_ids);
        if dlc_enable.is_empty() && dlc_disable.is_empty() {
            info!(
                "Join DLC preflight for repository {} server {}:{}: Creator DLCs already match the server",
                repo_name, server.address, server.port
            );
            return;
        }
        let state = state.get_or_insert_with(|| {
            crate::ui::app::PendingJoinPreflightState::empty(
                repo_name,
                server.clone(),
                effective.clone(),
                false,
            )
        });
        state.dlc_enable = dlc_enable;
        state.dlc_disable = dlc_disable;
    }

    pub(crate) fn present_join_addon_preflight(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
        addon_state: Option<crate::ui::app::PendingJoinPreflightState>,
    ) {
        self.prelaunch_process_checks = Default::default();
        let (steam_required, steam_running) = self.evaluate_steam_launch_gate(effective, repo_name);
        let mut addon_state = addon_state.or_else(|| {
            Self::build_join_preflight_state(
                effective,
                &self.repository_view_state.repositories,
                server,
                repo_name,
                &[],
                &crate::core::addon_metadata::AddonDisplayNameSnapshot::new(),
            )
        });
        if steam_required && !steam_running {
            addon_state.get_or_insert_with(|| {
                crate::ui::app::PendingJoinPreflightState::empty(
                    repo_name,
                    server.clone(),
                    effective.clone(),
                    false,
                )
            });
        }

        match addon_state {
            Some(mut preflight) => {
                preflight.steam_required = steam_required;
                preflight.steam_running = steam_running;
                self.pending_join_preflight = Some(preflight);
            }
            None => {
                info!(
                    "Join preflight has no actionable changes for repository {} server {}:{}; launching join",
                    repo_name, server.address, server.port
                );
                self.finish_join_without_preflight(ctx, effective, server, repo_name);
            }
        }
    }

    /// Evaluate the TeamSpeak running gate for a join. Returns
    /// `(ts3_required, ts3_running)`. `ts3_required` is true only when the
    /// per-repo/global check is enabled and the repository ships `.ts3_plugin`
    /// files. Synchronous - acceptable for a user-initiated join action.
    fn evaluate_ts3_join_gate(&self, effective: &Repository, repo_name: &str) -> (bool, bool) {
        if !crate::core::game::registry()
            .active()
            .capabilities()
            .teamspeak3_plugins
            || !self.repo_check_ts3_running_before_join(effective)
        {
            return (false, false);
        }
        if effective.path.trim().is_empty() {
            return (false, false);
        }
        let has_ts3_plugins =
            !crate::core::ts3_plugin::scan_repository_for_ts3_plugins(&effective.path).is_empty();
        if !has_ts3_plugins {
            return (false, false);
        }
        let running = crate::core::ts3_plugin::is_teamspeak_running();
        info!(
            "TeamSpeak join gate for repository {}: ts3_required=true ts3_running={}",
            repo_name, running
        );
        (true, running)
    }

    /// Evaluate the Steam running gate. Returns `(steam_required, steam_running)`.
    /// `steam_required` is true whenever the per-repo/global check is enabled,
    /// since Steam must be running for Arma 3 to launch. Synchronous - acceptable
    /// for a user-initiated launch/join action.
    fn evaluate_steam_launch_gate(&self, effective: &Repository, repo_name: &str) -> (bool, bool) {
        if !self.repository_launch_uses_steam(effective)
            || !self.repo_check_steam_running_before_launch(effective)
        {
            return (false, false);
        }
        let running = steam::is_steam_running(&self.settings_view_state.steam_directory);
        info!(
            "Steam launch gate for repository {}: steam_required=true steam_running={}",
            repo_name, running
        );
        (true, running)
    }

    /// Convergence point for the plain "Launch" button (no server to join).
    ///
    /// When the Steam-running check is enabled and Steam is not running, this
    /// opens the pre-launch modal so the user can start Steam first. Otherwise
    /// it launches directly (the Steam guard still auto-starts Steam as a
    /// fallback when the check is disabled).
    pub(crate) fn present_launch_preflight(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        repo_name: &str,
    ) {
        let (steam_required, steam_running) = self.evaluate_steam_launch_gate(effective, repo_name);
        let steam_attention = steam_required && !steam_running;
        // A plain launch has no server requirement list, so the only addon check
        // it runs is for enabled external addons whose files are missing - these
        // would otherwise be dropped silently at launch.
        let unavailable_enabled = Self::unavailable_enabled_external_addons(
            effective,
            &self.repository_view_state.repositories,
        );
        if steam_attention || !unavailable_enabled.is_empty() {
            info!(
                "Launch preflight opening for repository {} (steam_attention={}, unavailable_enabled={})",
                repo_name,
                steam_attention,
                unavailable_enabled.len()
            );
            self.prelaunch_process_checks = Default::default();
            self.pending_join_preflight = Some(crate::ui::app::PendingJoinPreflightState {
                unavailable_enabled,
                steam_required,
                steam_running,
                ..crate::ui::app::PendingJoinPreflightState::empty(
                    repo_name,
                    RepositoryServer::default(),
                    effective.clone(),
                    true,
                )
            });
            return;
        }

        let launch_result =
            self.launch_repository_with_database_guard(effective, None, repo_name, "regular");
        if launch_result == LaunchDispatchResult::Launched {
            self.handle_post_launch_window_behavior(ctx, "launch completed");
        }
    }

    fn log_join_preflight_query_output(
        repo_name: &str,
        server: &RepositoryServer,
        result: &ServerAddonQueryResult,
    ) {
        let protocol_mod_count = result
            .server_browser_protocol
            .as_ref()
            .map_or(0, |protocol| protocol.mods.len());
        info!(
            "Join addon preflight query output for repository {} server {}:{}: resolved_address={}, game_port={}, query_port={}, requirements={}, rules={}, server_browser_protocol={}, protocol_mods={}, info_keywords={:?}",
            repo_name,
            server.address,
            server.port,
            result.address,
            result.game_port,
            result.query_port,
            result.requirements.len(),
            result.rules.len(),
            result.server_browser_protocol.is_some(),
            protocol_mod_count,
            result.info_keywords
        );

        if result.requirements.is_empty() {
            info!(
                "Join addon preflight requirements for repository {} server {}:{}: none reported",
                repo_name, server.address, server.port
            );
        } else {
            let addon_names = result
                .requirements
                .iter()
                .map(|requirement| requirement.display_name.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            info!(
                "Join addon preflight required addons for repository {} server {}:{} ({}): {}",
                repo_name,
                server.address,
                server.port,
                result.requirements.len(),
                addon_names
            );
        }

        if let Some(protocol) = &result.server_browser_protocol {
            info!(
                "Join addon preflight Server Browser Protocol 3 for repository {} server {}:{}: version={}, difficulty={:?}, ai_level={:?}, dlc_flags={:?}, official_dlc={:?}, creator_dlc_app_ids={:?}, mods={}",
                repo_name,
                server.address,
                server.port,
                protocol.version,
                protocol.difficulty,
                protocol.ai_level,
                protocol.dlc_flags,
                protocol.official_dlc_names(),
                protocol.creator_dlc_app_ids,
                protocol.mods.len()
            );
            if !protocol.mods.is_empty() {
                let protocol_mod_names = protocol
                    .mods
                    .iter()
                    .map(|protocol_mod| protocol_mod.display_name.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                info!(
                    "Join addon preflight Server Browser Protocol 3 addon names for repository {} server {}:{} ({}): {}",
                    repo_name,
                    server.address,
                    server.port,
                    protocol.mods.len(),
                    protocol_mod_names
                );
            }
        }

        info!(
            "Join addon preflight A2S_RULES summary for repository {} server {}:{}: {} text rules returned; raw binary rule payloads are not logged",
            repo_name,
            server.address,
            server.port,
            result.rules.len()
        );
    }

    pub(crate) fn finish_join_without_preflight(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
    ) {
        let launch_result =
            self.launch_repository_with_database_guard(effective, Some(server), repo_name, "join");
        if matches!(
            launch_result,
            LaunchDispatchResult::Launched | LaunchDispatchResult::Deferred
        ) {
            info!(
                "Joining server {}:{} for repository {}",
                server.address, server.port, repo_name
            );
            if launch_result == LaunchDispatchResult::Launched {
                self.handle_post_launch_window_behavior(ctx, "join launch completed");
            }
        }
    }

    pub(super) fn request_editor_mission_launch(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        mission: &EditorMission,
        repo_idx: usize,
        repo_name: &str,
    ) {
        // The general external-addons warning is opt-in per repo/global setting.
        let external_addons = if self.repo_warn_editor_external_addons(effective) {
            Self::enabled_external_addons_for_editor_warning(effective)
        } else {
            Vec::new()
        };
        // The missing-addons check always runs: an enabled external addon whose
        // files are gone would be dropped silently when the editor launches.
        let unavailable_enabled = Self::unavailable_enabled_external_addons(
            effective,
            &self.repository_view_state.repositories,
        );
        if !external_addons.is_empty() || !unavailable_enabled.is_empty() {
            self.pending_mission_editor_launch_warning =
                Some(PendingMissionEditorLaunchWarningState {
                    repo_idx,
                    repo_name: repo_name.to_string(),
                    effective_repository: effective.clone(),
                    mission: mission.clone(),
                    external_addons,
                    unavailable_enabled,
                });
            return;
        }

        self.launch_editor_with_mission(ctx, effective, mission, repo_name);
    }

    /// Launch Arma 3 with a mission loaded in Eden Editor.
    ///
    /// Constructs the same base command as a normal launch (flags + mods),
    /// but instead of server connection params, passes the mission.sqm path
    /// as a positional argument to open it in the editor.
    pub(super) fn launch_editor_with_mission(
        &mut self,
        _ctx: &egui::Context,
        effective: &Repository,
        mission: &EditorMission,
        repo_name: &str,
    ) {
        let now = Instant::now();
        if let Some(until) = self.editor_launch_cooldown_until
            && until > now
        {
            info!(
                "Ignoring duplicate editor launch for repository {} (cooldown active)",
                repo_name
            );
            self.show_success_toast(self.t("Arma 3 Editor is already starting..."));
            return;
        }

        info!(
            "Launching Eden Editor with mission {} ({}) for repository {}",
            mission.display_name, mission.folder_name, repo_name
        );

        self.editor_launch_cooldown_until = Some(now + EDITOR_LAUNCH_COOLDOWN);
        self.show_success_toast(self.t("Starting Arma 3 Editor..."));
        self.start_launch_action(LaunchRequest {
            repository: effective.clone(),
            server: None,
            name: repo_name.to_string(),
            mission_path: Some(Path::new(&mission.sqm_path).to_path_buf()),
        });
    }
}
