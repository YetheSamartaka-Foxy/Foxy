use super::{LaunchDispatchResult, spawn_launch_process};
use crate::core::addon_metadata::load_addon_display_name_snapshot;
use crate::core::arma3_missions::EditorMission;
use crate::core::arma3_server_query::{ServerAddonQueryResult, query_server_addon_requirements};
use crate::core::steam::{self, SteamEnsureResult};
use crate::ui::app::{
    Foxy, JoinPreflightQueryResult, PendingJoinPreflightQuery, PendingJoinStatusQuery,
    PendingMissionEditorLaunchWarningState,
};
use crate::ui::types::{Repository, RepositoryServer, ServerOnlineStatus};
use eframe::egui;
use log::{info, warn};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EDITOR_LAUNCH_COOLDOWN: Duration = Duration::from_secs(30);
const JOIN_PREFLIGHT_CACHE_TTL: Duration = Duration::from_secs(60);

impl Foxy {
    fn activate_extra_files_before_launch(&mut self) {
        let space_dir = crate::core::game::spaces::active_game_space_dir();
        let game_dir = crate::core::game::registry()
            .active()
            .install_dir_from_settings(&self.settings_view_state)
            .to_string();
        match crate::core::game::extra_files::activate_for_launch(&space_dir, &game_dir) {
            Ok(summary) => {
                if !summary.failed.is_empty() {
                    warn!(
                        "Extra-file activation had {} failure(s) before launch",
                        summary.failed.len()
                    );
                }
            }
            Err(err) => {
                warn!("Extra-file activation failed before launch: {}", err);
            }
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

    pub(super) fn handle_post_launch_window_behavior(
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

    pub(super) fn launch_repository_with_steam_guard(
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
            let arma3_directory = self.settings_view_state.arma3_directory.trim();
            #[cfg(target_os = "linux")]
            {
                let _ = arma3_directory;
                self.show_error_toast(
                    self.t(
                        "Could not find the Steam client. Set the Steam directory in Settings or start it manually.",
                    ),
                );
            }
            #[cfg(not(target_os = "linux"))]
            {
                if arma3_directory.is_empty() {
                    self.show_error_toast(
                        self.t(
                            "Arma 3 directory is not configured. Set it in Game space settings.",
                        ),
                    );
                } else if !std::path::Path::new(arma3_directory).exists() {
                    self.show_error_toast(self.t(
                        "Arma 3 directory does not exist. Check the path in Game space settings.",
                    ));
                } else if !crate::core::game::registry()
                    .active()
                    .validate_install_dir(std::path::Path::new(arma3_directory))
                {
                    self.show_error_toast(
                        self.t("Arma 3 executable not found at the configured path."),
                    );
                }
            }
            return LaunchDispatchResult::Failed;
        };

        let executable = command.get_program().to_os_string();
        let args: Vec<OsString> = command.get_args().map(|arg| arg.to_os_string()).collect();
        let cwd: Option<PathBuf> = command.get_current_dir().map(Path::to_path_buf);
        self.activate_extra_files_before_launch();

        if steam::is_steam_running() {
            return match spawn_launch_process(&executable, &args, cwd.as_deref()) {
                Ok(child) => {
                    info!(
                        "Launched Arma 3 for repository {} (pid={})",
                        repo_name,
                        child.id()
                    );
                    LaunchDispatchResult::Launched
                }
                Err(err) => {
                    warn!(
                        "Failed to launch Arma 3 for repository {}: {}",
                        repo_name, err
                    );
                    self.show_error_toast(self.t("Failed to launch Arma 3."));
                    LaunchDispatchResult::Failed
                }
            };
        }

        info!(
            "Steam is not running; preparing Steam before {} launch for repository {}",
            launch_label, repo_name
        );

        let steam_directory = self.settings_view_state.steam_directory.clone();
        let repo_name_owned = repo_name.to_string();
        let launch_label_owned = launch_label.to_string();
        let executable_for_thread = executable;
        let args_for_thread = args;
        let cwd_for_thread = cwd;
        std::thread::spawn(move || {
            match steam::ensure_steam_running(&steam_directory) {
                Ok(SteamEnsureResult::AlreadyRunning) => {}
                Ok(SteamEnsureResult::Started) => {
                    info!(
                        "Steam started successfully before {} launch for repository {}",
                        launch_label_owned, repo_name_owned
                    );
                }
                Ok(SteamEnsureResult::SkippedMissingDirectory) => {
                    warn!(
                        "Steam directory is not configured and auto-detection failed; proceeding \
                         with direct {} launch for repository {}",
                        launch_label_owned, repo_name_owned
                    );
                }
                Err(err) => {
                    warn!(
                        "Failed to prepare Steam before {} launch for repository {}: {}",
                        launch_label_owned, repo_name_owned, err
                    );
                    return;
                }
            }

            match spawn_launch_process(
                &executable_for_thread,
                &args_for_thread,
                cwd_for_thread.as_deref(),
            ) {
                Ok(child) => {
                    info!(
                        "Launched Arma 3 for repository {} (pid={})",
                        repo_name_owned,
                        child.id()
                    );
                }
                Err(err) => {
                    warn!(
                        "Failed to launch Arma 3 for repository {} after Steam preparation: {}",
                        repo_name_owned, err
                    );
                }
            }
        });

        LaunchDispatchResult::Deferred
    }

    pub(super) fn try_join_repository_server(
        &mut self,
        _ctx: &egui::Context,
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

        info!(
            "Join requested for repository {} server {}:{}",
            repo_name, server.address, server.port
        );

        // Query the server status off the UI thread (DNS resolution + UDP
        // round-trip). The join decision resumes in `poll_pending_join_status`
        // once a fresh status arrives, so a slow or unreachable server can no
        // longer freeze the UI.
        self.pending_join_status_query = Some(PendingJoinStatusQuery {
            repo_name: repo_name.to_string(),
            server: server.clone(),
            effective_repository: effective.clone(),
            key: (server.address.clone(), server.port.clone()),
            started_at: Instant::now(),
        });
        self.spawn_query_thread(server);
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

        // Accept the freshest status that landed at or after we dispatched.
        let status = self
            .server_statuses
            .get(&pending.key)
            .filter(|cache| cache.last_check >= pending.started_at)
            .map(|cache| cache.status);
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
                if self.repo_check_server_addons_before_join(effective) {
                    info!(
                        "Join addon preflight enabled for repository {} server {}:{}",
                        repo_name, server.address, server.port
                    );
                    match server.port.parse::<u16>() {
                        Ok(port) => {
                            if self.handle_cached_join_preflight(
                                ctx, effective, server, repo_name, port,
                            ) {
                                info!(
                                    "Join addon preflight handled from cache for repository {} server {}:{}",
                                    repo_name, server.address, server.port
                                );
                                return;
                            }
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
                    "Joining repository {} without addon preflight dialog because no addon preflight gate was active; evaluating TeamSpeak gate",
                    repo_name
                );
                self.present_join_preflight(ctx, effective, server, repo_name, None);
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

    fn handle_cached_join_preflight(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
        port: u16,
    ) -> bool {
        let key = (server.address.clone(), port);
        let Some(entry) = self.join_preflight_cache.get(&key) else {
            return false;
        };
        if entry.cached_at.elapsed() > JOIN_PREFLIGHT_CACHE_TTL {
            info!(
                "Join addon preflight cache expired for repository {} server {}:{}",
                repo_name, server.address, server.port
            );
            self.join_preflight_cache.remove(&key);
            return false;
        }
        info!(
            "Join addon preflight cache hit for repository {} server {}:{} with {} requirement(s)",
            repo_name,
            server.address,
            server.port,
            entry.result.requirements.len()
        );
        let result = entry.result.clone();
        let display_names = entry.display_names.clone();
        self.finish_join_preflight(ctx, effective, server, repo_name, &result, &display_names);
        true
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
            repo_name: repo_name.to_string(),
            server: server.clone(),
            original_repository: effective.clone(),
        });

        let address = server.address.clone();
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
            let display_names = {
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
        let addon_state = Self::build_join_preflight_state(
            effective,
            &self.repository_view_state.repositories,
            server,
            repo_name,
            &result.requirements,
            display_names,
        );
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
        self.present_join_preflight(ctx, effective, server, repo_name, addon_state);
    }

    /// Single convergence point that decides whether to open the join preflight
    /// modal (for addon actions and/or a TeamSpeak warning) or launch directly.
    ///
    /// The TS3 gate is evaluated here so it applies regardless of whether the
    /// addon preflight setting is enabled.
    pub(crate) fn present_join_preflight(
        &mut self,
        ctx: &egui::Context,
        effective: &Repository,
        server: &RepositoryServer,
        repo_name: &str,
        addon_state: Option<crate::ui::app::PendingJoinPreflightState>,
    ) {
        let (ts3_required, ts3_running) = self.evaluate_ts3_join_gate(effective, repo_name);
        let (steam_required, steam_running) = self.evaluate_steam_launch_gate(effective, repo_name);
        let needs_attention = (ts3_required && !ts3_running) || (steam_required && !steam_running);
        self.prelaunch_recheck_at = None;

        match addon_state {
            Some(mut preflight) => {
                preflight.ts3_required = ts3_required;
                preflight.ts3_running = ts3_running;
                preflight.steam_required = steam_required;
                preflight.steam_running = steam_running;
                self.pending_join_preflight = Some(preflight);
            }
            None if needs_attention => {
                info!(
                    "Join preflight opening for repository {} server {}:{} with pre-join warnings only (ts3_attention={}, steam_attention={})",
                    repo_name,
                    server.address,
                    server.port,
                    ts3_required && !ts3_running,
                    steam_required && !steam_running
                );
                self.pending_join_preflight = Some(crate::ui::app::PendingJoinPreflightState {
                    repo_name: repo_name.to_string(),
                    server: server.clone(),
                    original_repository: effective.clone(),
                    suggestions: Vec::new(),
                    ambiguous: Vec::new(),
                    known_remote: Vec::new(),
                    extra_enabled: Vec::new(),
                    unavailable_enabled: Vec::new(),
                    ts3_required,
                    ts3_running,
                    steam_required,
                    steam_running,
                    launch_only: false,
                });
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
        if !self.repo_check_ts3_running_before_join(effective) {
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
        if !self.repo_check_steam_running_before_launch(effective) {
            return (false, false);
        }
        let running = steam::is_steam_running();
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
            self.prelaunch_recheck_at = None;
            self.pending_join_preflight = Some(crate::ui::app::PendingJoinPreflightState {
                repo_name: repo_name.to_string(),
                server: RepositoryServer::default(),
                original_repository: effective.clone(),
                suggestions: Vec::new(),
                ambiguous: Vec::new(),
                known_remote: Vec::new(),
                extra_enabled: Vec::new(),
                unavailable_enabled,
                ts3_required: false,
                ts3_running: false,
                steam_required,
                steam_running,
                launch_only: true,
            });
            return;
        }

        let launch_result =
            self.launch_repository_with_steam_guard(effective, None, repo_name, "regular");
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
                "Join addon preflight Server Browser Protocol 3 for repository {} server {}:{}: version={}, difficulty={:?}, ai_level={:?}, dlc_flags={:?}, mods={}",
                repo_name,
                server.address,
                server.port,
                protocol.version,
                protocol.difficulty,
                protocol.ai_level,
                protocol.dlc_flags,
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
            self.launch_repository_with_steam_guard(effective, Some(server), repo_name, "join");
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
        ctx: &egui::Context,
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

        let Some(mut command) = self.create_launch_command(effective, None) else {
            self.show_error_toast(self.t("Failed to launch Arma 3 Editor."));
            return;
        };

        // Add the mission.sqm path as a positional argument.
        command.arg(&mission.sqm_path);

        info!(
            "Launching Eden Editor with mission {} ({}) for repository {}",
            mission.display_name, mission.folder_name, repo_name
        );

        self.editor_launch_cooldown_until = Some(now + EDITOR_LAUNCH_COOLDOWN);
        self.show_success_toast(self.t("Starting Arma 3 Editor..."));

        let executable = command.get_program().to_os_string();
        let args: Vec<OsString> = command.get_args().map(|a| a.to_os_string()).collect();
        let cwd: Option<PathBuf> = command.get_current_dir().map(Path::to_path_buf);
        self.activate_extra_files_before_launch();

        if steam::is_steam_running() {
            match spawn_launch_process(&executable, &args, cwd.as_deref()) {
                Ok(child) => {
                    info!("Launched Eden Editor (pid={})", child.id());
                    self.handle_post_launch_window_behavior(ctx, "editor launch completed");
                }
                Err(err) => {
                    warn!("Failed to launch Eden Editor: {}", err);
                    self.editor_launch_cooldown_until = None;
                    self.show_error_toast(self.t("Failed to launch Arma 3 Editor."));
                }
            }
            return;
        }

        let steam_directory = self.settings_view_state.steam_directory.clone();
        let repo_name_owned = repo_name.to_string();
        std::thread::spawn(
            move || match steam::ensure_steam_running(&steam_directory) {
                Ok(_) => match spawn_launch_process(&executable, &args, cwd.as_deref()) {
                    Ok(child) => info!(
                        "Launched Eden Editor after Steam startup (pid={}) for repository {}",
                        child.id(),
                        repo_name_owned
                    ),
                    Err(err) => warn!("Failed to launch Eden Editor: {}", err),
                },
                Err(err) => warn!("Failed to start Steam: {}", err),
            },
        );
    }
}
