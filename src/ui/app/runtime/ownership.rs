use crate::{
    core::{
        app_mode::AppMode,
        game::spaces,
        server::{
            ServerContext,
            owner::{self, ForegroundOwner, OwnerSnapshot},
        },
    },
    ui::app::Foxy,
};
use eframe::egui;
use std::{path::PathBuf, thread::JoinHandle, time::Duration};

type OwnerRetirement = JoinHandle<Result<(), Box<(ForegroundOwner, String)>>>;

pub(crate) struct ModeOwnership {
    pub active: AppMode,
    pub owner: Option<ForegroundOwner>,
    worker: Option<JoinHandle<Transition>>,
    target: Option<AppMode>,
    target_space: Option<String>,
    draining: bool,
    retiring: Vec<OwnerRetirement>,
    cleanup_error: Option<String>,
    pub error: Option<String>,
    pub snapshot: Option<OwnerSnapshot>,
}

struct Transition {
    operation: OwnershipOperation,
    idle_guard: Option<owner::IdleGuard>,
    mode: AppMode,
    owner: Option<ForegroundOwner>,
    snapshot: Option<OwnerSnapshot>,
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OwnershipOperation {
    Mode,
    Detach,
    Reconnect,
    SpaceSwitch,
}

impl Default for ModeOwnership {
    fn default() -> Self {
        Self {
            active: AppMode::Client,
            owner: None,
            worker: None,
            target: None,
            target_space: None,
            draining: false,
            retiring: Vec::new(),
            cleanup_error: None,
            error: None,
            snapshot: None,
        }
    }
}

impl ModeOwnership {
    pub(crate) fn busy(&self) -> bool {
        self.draining || self.worker.is_some() || self.target.is_some() || !self.retiring.is_empty()
    }
    pub(crate) fn ready(&self) -> bool {
        self.active == AppMode::Server
            && self.owner.is_some()
            && !self.busy()
            && self.error.is_none()
    }
}

fn capture(space: &str) -> Result<ServerContext, String> {
    ServerContext::capture(
        &crate::core::utils::app_paths::configured_data_dir(),
        Some(space),
    )
}

fn claim_client(path: &std::path::Path) -> Result<(), String> {
    use crate::core::tasks::db_process_lock::{self, LockOutcome};
    match db_process_lock::acquire(path) {
        LockOutcome::Acquired => crate::core::tasks::init_database::resume_database_access_sync(path),
        LockOutcome::Busy { .. } => Err("Another owner still holds this game space. Stop its servers and shut down that owner before returning to Client mode.".into()),
        LockOutcome::Unavailable(_) => Err("Client database ownership could not be verified.".into()),
    }
}

fn retire(owner: ForegroundOwner) -> OwnerRetirement {
    retire_with_idle(owner, None)
}

fn retire_with_idle(
    mut owner: ForegroundOwner,
    idle_guard: Option<owner::IdleGuard>,
) -> OwnerRetirement {
    std::thread::spawn(move || {
        let _idle_guard = idle_guard;
        match owner.stop() {
            Ok(()) => Ok(()),
            Err(error) => {
                std::thread::sleep(Duration::from_millis(100));
                Err(Box::new((owner, error.to_string())))
            }
        }
    })
}

impl Foxy {
    pub(crate) fn server_context_path(&self) -> Option<PathBuf> {
        if !self.mode_ownership.ready() {
            return None;
        }
        let active = spaces::active_game_space();
        let owner = self.mode_ownership.owner.as_ref()?;
        if owner.attachment.scope().space_id != active.space_id {
            return None;
        }
        capture(&active.space_id)
            .ok()
            .map(|context| context.database_path())
    }

    pub(crate) fn start_server_bootstrap(&mut self) {
        self.mode_ownership.active = AppMode::Server;
        self.begin_client_worker_handoff();
        self.startup_tasks_started = true;
        let space = spaces::active_game_space().space_id;
        self.launch_owner_transition(AppMode::Server, space, false);
    }

    fn launch_owner_transition(&mut self, target: AppMode, space: String, close_client: bool) {
        let mut outgoing = self.mode_ownership.owner.take();
        self.mode_ownership.target = Some(target);
        self.mode_ownership.draining = false;
        self.mode_ownership.error = None;
        let old_mode = self.mode_ownership.active;
        self.mode_ownership.worker = Some(std::thread::spawn(move || {
            let context = match capture(&space) {
                Ok(context) => context,
                Err(error) => {
                    return Transition {
                        operation: OwnershipOperation::Mode,
                        idle_guard: None,
                        mode: old_mode,
                        owner: outgoing,
                        snapshot: None,
                        error: Some(error),
                    };
                }
            };
            let result = match target {
                AppMode::Server => {
                    if close_client
                        && let Err(error) =
                            crate::core::tasks::init_database::close_active_database_sync()
                    {
                        return Transition {
                            operation: OwnershipOperation::Mode,
                            idle_guard: None,
                            mode: old_mode,
                            owner: outgoing,
                            snapshot: None,
                            error: Some(error),
                        };
                    }
                    if let Some(owner) = outgoing.as_mut() {
                        if let Ok(snapshot) = owner.attachment.inspect() {
                            return Transition {
                                operation: OwnershipOperation::Mode,
                                idle_guard: None,
                                mode: target,
                                owner: outgoing,
                                snapshot: Some(snapshot),
                                error: None,
                            };
                        }
                        if owner.is_owned()
                            && let Err(error) = owner.stop()
                            && owner.is_owned()
                        {
                            return Transition {
                                operation: OwnershipOperation::Mode,
                                idle_guard: None,
                                mode: old_mode,
                                owner: outgoing,
                                snapshot: None,
                                error: Some(error.to_string()),
                            };
                        }
                    }
                    match owner::start_or_attach(&context) {
                        Ok(owner) => match owner.attachment.inspect() {
                            Ok(snapshot) => {
                                return Transition {
                                    operation: OwnershipOperation::Mode,
                                    idle_guard: None,
                                    mode: target,
                                    owner: Some(owner),
                                    snapshot: Some(snapshot),
                                    error: None,
                                };
                            }
                            Err(error) => {
                                return Transition {
                                    operation: OwnershipOperation::Mode,
                                    idle_guard: None,
                                    mode: if close_client {
                                        AppMode::Server
                                    } else {
                                        old_mode
                                    },
                                    owner: Some(owner),
                                    snapshot: None,
                                    error: Some(error.to_string()),
                                };
                            }
                        },
                        Err(error) => {
                            let restored = if close_client {
                                claim_client(&context.database_path())
                            } else {
                                Ok(())
                            };
                            Err(format!(
                                "{error}{}",
                                restored
                                    .err()
                                    .map_or(String::new(), |error| format!("; {error}"))
                            ))
                        }
                    }
                }
                AppMode::Client => {
                    let outgoing_snapshot = match outgoing
                        .as_ref()
                        .map(|owner| owner.attachment.inspect())
                        .transpose()
                    {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            return Transition {
                                operation: OwnershipOperation::Mode,
                                idle_guard: None,
                                mode: old_mode,
                                owner: outgoing,
                                snapshot: None,
                                error: Some(error.to_string()),
                            };
                        }
                    };
                    let detached = outgoing_snapshot.as_ref().is_some_and(|snapshot| {
                        snapshot.lifetime == owner::OwnerLifetime::Detached
                    });
                    let _idle_guard = match outgoing
                        .as_ref()
                        .filter(|owner| owner.is_owned() && !detached)
                        .map(|owner| owner.attachment.idle_guard())
                        .transpose()
                    {
                        Ok(guard) => guard,
                        Err(error) => {
                            return Transition {
                                operation: OwnershipOperation::Mode,
                                idle_guard: None,
                                mode: old_mode,
                                owner: outgoing,
                                snapshot: outgoing_snapshot,
                                error: Some(error.to_string()),
                            };
                        }
                    };
                    if let Some(owner) = outgoing.as_mut()
                        && owner.is_owned()
                        && !detached
                        && let Err(error) = owner.stop()
                    {
                        return Transition {
                            operation: OwnershipOperation::Mode,
                            idle_guard: None,
                            mode: old_mode,
                            owner: outgoing,
                            snapshot: outgoing_snapshot.clone(),
                            error: Some(error.to_string()),
                        };
                    }
                    match claim_client(&context.database_path()) {
                        Ok(()) => {
                            outgoing = None;
                            Ok(())
                        }
                        Err(error) => Err(error),
                    }
                }
            };
            match result {
                Ok(()) => Transition {
                    operation: OwnershipOperation::Mode,
                    idle_guard: None,
                    mode: target,
                    snapshot: outgoing
                        .as_ref()
                        .and_then(|owner| owner.attachment.inspect().ok()),
                    owner: outgoing,
                    error: None,
                },
                Err(error) => Transition {
                    operation: OwnershipOperation::Mode,
                    idle_guard: None,
                    mode: old_mode,
                    snapshot: outgoing
                        .as_ref()
                        .and_then(|owner| owner.attachment.inspect().ok()),
                    owner: outgoing,
                    error: Some(error),
                },
            }
        }));
    }

    pub(crate) fn service_mode_ownership(&mut self, ctx: &egui::Context) {
        let mut index = 0;
        while index < self.mode_ownership.retiring.len() {
            if self.mode_ownership.retiring[index].is_finished() {
                match self.mode_ownership.retiring.swap_remove(index).join() {
                    Ok(Ok(())) => {}
                    Ok(Err(retained)) => {
                        let (owner, error) = *retained;
                        self.mode_ownership.cleanup_error = Some(error);
                        self.mode_ownership.retiring.push(retire(owner));
                        index += 1;
                    }
                    Err(_) => {
                        self.mode_ownership.error = Some("Owner cleanup worker failed.".into())
                    }
                }
            } else {
                index += 1;
            }
        }
        if self.mode_ownership.retiring.is_empty() {
            self.mode_ownership.cleanup_error = None;
        }
        if self
            .mode_ownership
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            let result = self.mode_ownership.worker.take().unwrap().join();
            self.mode_ownership.target = None;
            match result {
                Ok(transition) => {
                    if let Some(space) = self.mode_ownership.target_space.take() {
                        if let Some(error) = transition.error {
                            self.game_space_switch_open_error = Some(error);
                        } else if let Some(owner) = transition.owner {
                            self.finish_server_space_switch(ctx, &space, owner, transition.idle_guard, transition.snapshot);
                        }
                        self.needs_repaint = true;
                        return;
                    }
                    self.mode_ownership.owner = transition.owner;
                    self.mode_ownership.error = transition.error;
                    if matches!(transition.operation, OwnershipOperation::Detach | OwnershipOperation::Reconnect) {
                        self.mode_ownership.snapshot = transition.snapshot;
                        if transition.operation == OwnershipOperation::Reconnect && self.mode_ownership.error.is_none() {
                            self.refresh_reconnected_server(ctx);
                        }
                        self.needs_repaint = true;
                        return;
                    }
                    let previous = self.mode_ownership.active;
                    self.mode_ownership.active = transition.mode;
                    if previous != transition.mode { self.sync_app_mode_navigation(); }
                    self.mode_ownership.snapshot = transition.snapshot;
                    if self.mode_ownership.active == AppMode::Client
                        && crate::core::tasks::db_process_lock::holds(&crate::core::tasks::db_turso::database_file_path()) {
                            let was_admitted = self.client_db_admission.allows();
                            self.end_client_worker_handoff();
                            if previous == AppMode::Server || !was_admitted { self.reload_client_after_mode_switch(ctx); }
                    }
                    if self.mode_ownership.error.is_some() && self.launch_mode_override.is_none() {
                        self.settings_view_state.app_mode = self.mode_ownership.active;
                        self.mark_settings_dirty();
                    }
                }
                Err(_) => self.mode_ownership.error = Some("Ownership transition worker stopped unexpectedly. Retry before using this game space.".into()),
            }
            self.needs_repaint = true;
        }
        if self.mode_ownership.worker.is_some()
            || self.mode_ownership.target.is_some()
            || !self.mode_ownership.retiring.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(25));
            return;
        }
        let requested = self
            .settings_view_state
            .app_mode
            .effective(self.launch_mode_override);
        if requested == self.mode_ownership.active {
            if self.mode_ownership.draining && requested == AppMode::Client {
                self.mode_ownership.draining = false;
                self.end_client_worker_handoff();
            }
            return;
        }
        if self.server_view.busy()
            || self.server_view.dirty()
            || self.pending_game_space_switch.is_some()
            || self.close_requested_at.is_some()
        {
            self.settings_view_state.app_mode = self.mode_ownership.active;
            self.mode_ownership.error = Some("Save or discard server drafts and finish the current operation before changing modes.".into());
            self.mark_settings_dirty();
            self.mode_ownership.draining = false;
            if self.mode_ownership.active == AppMode::Client {
                self.end_client_worker_handoff();
            }
            return;
        }
        self.maybe_dispatch_persistence_requests(true);
        if self.has_pending_persistence_writes() {
            ctx.request_repaint_after(Duration::from_millis(25));
            return;
        }
        if requested == AppMode::Server {
            if !self.client_worker_handoff_active && self.client_handoff_block_reason().is_some() {
                self.mode_ownership.draining = true;
                ctx.request_repaint_after(Duration::from_millis(25));
                return;
            }
            self.begin_client_worker_handoff();
            self.mode_ownership.draining = true;
            let blocked = !self.client_worker_handoff_ready();
            if blocked {
                ctx.request_repaint_after(Duration::from_millis(25));
                return;
            }
        }
        self.launch_owner_transition(
            requested,
            spaces::active_game_space().space_id,
            requested == AppMode::Server,
        );
    }

    pub(crate) fn retry_mode_ownership(&mut self) {
        if self.mode_ownership.busy() {
            return;
        }
        self.mode_ownership.error = None;
        if self.mode_ownership.active == AppMode::Server {
            self.launch_owner_transition(
                AppMode::Server,
                spaces::active_game_space().space_id,
                false,
            );
        } else if !crate::core::tasks::db_process_lock::holds(
            &crate::core::tasks::db_turso::database_file_path(),
        ) {
            self.launch_owner_transition(
                AppMode::Client,
                spaces::active_game_space().space_id,
                false,
            );
        } else {
            self.end_client_worker_handoff();
        }
    }

    pub(crate) fn detach_server_owner(&mut self) {
        if self.mode_ownership.busy() || self.server_view.busy() || self.server_view.dirty() {
            return;
        }
        let Some(mut owner) = self.mode_ownership.owner.take() else {
            return;
        };
        if !owner.is_owned() {
            self.mode_ownership.owner = Some(owner);
            return;
        }
        let old_snapshot = self.mode_ownership.snapshot.clone();
        self.mode_ownership.error = None;
        self.mode_ownership.target = Some(AppMode::Server);
        self.mode_ownership.worker = Some(std::thread::spawn(move || match owner.detach() {
            Ok(snapshot) => Transition {
                operation: OwnershipOperation::Detach,
                idle_guard: None,
                mode: AppMode::Server,
                owner: Some(owner),
                snapshot: Some(snapshot),
                error: None,
            },
            Err(error) => Transition {
                operation: OwnershipOperation::Detach,
                idle_guard: None,
                mode: AppMode::Server,
                snapshot: old_snapshot,
                owner: Some(owner),
                error: Some(error.to_string()),
            },
        }));
    }

    pub(crate) fn reconnect_server_owner(&mut self) {
        if self.mode_ownership.busy() || self.server_view.busy() {
            return;
        }
        let space = spaces::active_game_space().space_id;
        let old_owner = self.mode_ownership.owner.take();
        self.mode_ownership.error = None;
        self.mode_ownership.target = Some(AppMode::Server);
        self.mode_ownership.worker = Some(std::thread::spawn(move || {
            let context = match capture(&space) {
                Ok(context) => context,
                Err(error) => {
                    return Transition {
                        operation: OwnershipOperation::Reconnect,
                        idle_guard: None,
                        mode: AppMode::Server,
                        snapshot: None,
                        owner: old_owner,
                        error: Some(error),
                    };
                }
            };
            if let Some(mut owner) = old_owner {
                match owner.attachment.inspect() {
                    Ok(snapshot) if snapshot.scope.space_id == space => {
                        if snapshot.lifetime == owner::OwnerLifetime::Detached && owner.is_owned() {
                            return match owner.detach() {
                                Ok(snapshot) => Transition {
                                    operation: OwnershipOperation::Reconnect,
                                    idle_guard: None,
                                    mode: AppMode::Server,
                                    snapshot: Some(snapshot),
                                    owner: Some(owner),
                                    error: None,
                                },
                                Err(error) => Transition {
                                    operation: OwnershipOperation::Reconnect,
                                    idle_guard: None,
                                    mode: AppMode::Server,
                                    snapshot: Some(snapshot),
                                    owner: Some(owner),
                                    error: Some(error.to_string()),
                                },
                            };
                        }
                        return Transition {
                            operation: OwnershipOperation::Reconnect,
                            idle_guard: None,
                            mode: AppMode::Server,
                            snapshot: Some(snapshot),
                            owner: Some(owner),
                            error: None,
                        };
                    }
                    _ if owner.is_owned() => return Transition {
                        operation: OwnershipOperation::Reconnect,
                        idle_guard: None,
                        mode: AppMode::Server,
                        snapshot: None,
                        owner: Some(owner),
                        error: Some("The retained owner is still running but could not be authenticated. Retry ownership connection.".into()),
                    },
                    _ => {}
                }
            }
            match owner::start_or_attach(&context) {
                Ok(owner) => match owner.attachment.inspect() {
                    Ok(snapshot) if snapshot.scope.space_id == space => Transition {
                        operation: OwnershipOperation::Reconnect,
                        idle_guard: None,
                        mode: AppMode::Server,
                        snapshot: Some(snapshot),
                        owner: Some(owner),
                        error: None,
                    },
                    Ok(_) => Transition {
                        operation: OwnershipOperation::Reconnect,
                        idle_guard: None,
                        mode: AppMode::Server,
                        snapshot: None,
                        owner: Some(owner),
                        error: Some(
                            "Authenticated owner belongs to a different game space.".into(),
                        ),
                    },
                    Err(error) => Transition {
                        operation: OwnershipOperation::Reconnect,
                        idle_guard: None,
                        mode: AppMode::Server,
                        snapshot: None,
                        owner: Some(owner),
                        error: Some(error.to_string()),
                    },
                },
                Err(error) => Transition {
                    operation: OwnershipOperation::Reconnect,
                    idle_guard: None,
                    mode: AppMode::Server,
                    snapshot: None,
                    owner: None,
                    error: Some(error.to_string()),
                },
            }
        }));
    }

    fn refresh_reconnected_server(&mut self, ctx: &egui::Context) {
        self.server_view.consumer_recovery_plan = None;
        self.server_view.consumer_recovery_outcome = None;
        self.server_view.consumer_recovery_confirmation = false;
        self.server_view.lifecycle_plan = None;
        self.server_view.lifecycle_status = None;
        self.server_view.lifecycle_monitoring = None;
        self.server_view.monitoring_last_received = None;
        self.server_view.monitoring_last_refresh = None;
        self.server_view.monitoring_search.clear();
        self.server_view.monitoring_paused = false;
        self.server_view.monitoring_auto_scroll = true;
        if let Some(id) = self.server_view.selected.clone() {
            self.start_server_task(
                crate::ui::app::server::ServerTaskAction::InspectInstanceRuntime(id),
            );
        }
        ctx.request_repaint();
    }

    pub(crate) fn ownership_close_ready(&mut self, ctx: &egui::Context) -> bool {
        if self.mode_ownership.busy() {
            egui::Modal::new(egui::Id::unique("owner-close-drain")).show(ctx, |ui| {
                ui.label(self.t("Waiting for the foreground owner and retained metadata operations before closing."));
                ui.add(crate::ui::app::PacedSpinner::new());
            });
            ctx.request_repaint_after(Duration::from_millis(25));
            return false;
        }
        if let Some(owner) = self.mode_ownership.owner.take() {
            self.mode_ownership.retiring.push(retire(owner));
            ctx.request_repaint_after(Duration::from_millis(25));
            return false;
        }
        true
    }

    pub(crate) fn process_server_space_switch(&mut self, ctx: &egui::Context) {
        if self.mode_ownership.busy() || self.game_space_switch_open_error.is_some() {
            return;
        }
        if self.has_pending_persistence_writes()
            || self.server_view.busy()
            || self.server_view.dirty()
        {
            return;
        }
        let Some(entry) = &self.pending_game_space_switch else {
            return;
        };
        let target = entry.id.clone();
        let root = crate::core::utils::app_paths::configured_data_dir();
        let outgoing = self
            .mode_ownership
            .owner
            .as_ref()
            .filter(|owner| owner.is_owned())
            .map(|owner| owner.attachment.clone());
        self.mode_ownership.target_space = Some(target.clone());
        self.mode_ownership.target = Some(AppMode::Server);
        self.mode_ownership.worker = Some(std::thread::spawn(move || {
            let idle_guard = match outgoing {
                Some(attachment) => match attachment.inspect() {
                    Ok(snapshot) if snapshot.lifetime == owner::OwnerLifetime::Detached => None,
                    Ok(_) => match attachment.idle_guard() {
                        Ok(guard) => Some(guard),
                        Err(error) => {
                            return Transition {
                                operation: OwnershipOperation::SpaceSwitch,
                                idle_guard: None,
                                mode: AppMode::Server,
                                owner: None,
                                snapshot: None,
                                error: Some(error.to_string()),
                            };
                        }
                    },
                    Err(error) => {
                        return Transition {
                            operation: OwnershipOperation::SpaceSwitch,
                            idle_guard: None,
                            mode: AppMode::Server,
                            owner: None,
                            snapshot: None,
                            error: Some(error.to_string()),
                        };
                    }
                },
                None => None,
            };
            match ServerContext::capture(&root, Some(&target)).and_then(|context| {
                owner::start_or_attach(&context).map_err(|error| error.to_string())
            }) {
                Ok(owner) => match owner.attachment.inspect() {
                    Ok(snapshot) => Transition {
                        operation: OwnershipOperation::SpaceSwitch,
                        idle_guard,
                        mode: AppMode::Server,
                        owner: Some(owner),
                        snapshot: Some(snapshot),
                        error: None,
                    },
                    Err(error) => Transition {
                        operation: OwnershipOperation::SpaceSwitch,
                        idle_guard: None,
                        mode: AppMode::Server,
                        owner: Some(owner),
                        snapshot: None,
                        error: Some(error.to_string()),
                    },
                },
                Err(error) => Transition {
                    operation: OwnershipOperation::SpaceSwitch,
                    idle_guard: None,
                    mode: AppMode::Server,
                    owner: None,
                    snapshot: None,
                    error: Some(error),
                },
            }
        }));
        ctx.request_repaint_after(Duration::from_millis(25));
    }

    fn finish_server_space_switch(
        &mut self,
        ctx: &egui::Context,
        space: &str,
        incoming: ForegroundOwner,
        idle_guard: Option<owner::IdleGuard>,
        snapshot: Option<OwnerSnapshot>,
    ) {
        match spaces::activate_game_space(space) {
            Ok(_) => {
                if let Some(outgoing) = self.mode_ownership.owner.replace(incoming) {
                    self.mode_ownership
                        .retiring
                        .push(retire_with_idle(outgoing, idle_guard));
                }
                self.mode_ownership.snapshot = snapshot;
                self.pending_game_space_switch = None;
                self.reset_space_scoped_state();
                self.begin_client_worker_handoff();
                self.load_settings();
                self.startup_tasks_started = true;
                self.current_view = self.mode_home_view();
                self.last_view = crate::ui::types::FoxyView::None;
                self.apply_runtime_palette_visuals(ctx);
                self.needs_repaint = true;
            }
            Err(error) => {
                self.mode_ownership.retiring.push(retire(incoming));
                self.game_space_switch_open_error = Some(error);
            }
        }
    }

    fn reload_client_after_mode_switch(&mut self, ctx: &egui::Context) {
        let navigation = self.mode_navigation;
        let views = (self.current_view, self.last_view);
        let server = std::mem::take(&mut self.server_view);
        self.reset_space_scoped_state();
        self.mode_navigation = navigation;
        self.server_view = server;
        (self.current_view, self.last_view) = views;
        self.reload_active_game_space(ctx);
        self.startup_tasks_started = false;
    }

    pub(crate) fn render_owner_status(&mut self, ui: &mut egui::Ui) {
        if self.mode_ownership.busy() {
            ui.horizontal(|ui| {
                ui.add(crate::ui::app::PacedSpinner::new());
                ui.label(self.t("Waiting for ownership handoff and retained workers."));
            });
        }
        if let Some(error) = self.mode_ownership.error.clone() {
            ui.label(self.t(&error));
            if ui.button(self.t("Retry ownership connection")).clicked() {
                self.retry_mode_ownership();
            }
        }
        if let Some(error) = &self.mode_ownership.cleanup_error {
            ui.label(error);
        }
        if self.mode_ownership.snapshot.is_some() {
            ui.label(self.t("Authenticated server owner. Client database access and repository schedules are paused."));
            if let Some(snapshot) = self.mode_ownership.snapshot.as_ref() {
                ui.label(self.t(match snapshot.lifetime {
                    owner::OwnerLifetime::Foreground => "Foreground owner closes with this window.",
                    owner::OwnerLifetime::ParentBound => "Parent-bound owner stops when its parent closes.",
                    owner::OwnerLifetime::Detached => "Detached owner keeps supervising server processes after this window closes.",
                }));
            }
        }
    }
}
