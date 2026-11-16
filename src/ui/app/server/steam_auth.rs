use std::thread::JoinHandle;

use foxy_steam::auth::AuthPhase;
use foxy_steam::{AuthError, Secret};

use crate::core::game::spaces;
use crate::core::server::ServerContext;
use crate::core::server::steam_auth::{
    SteamAuthControl, SteamAuthOperation, SteamAuthOutcome, run_steam_auth,
};
use crate::ui::app::Foxy;

pub(crate) struct ServerSteamState {
    pub alias: String,
    pub account_name: Secret,
    pub password: Secret,
    pub code: Secret,
    pub outcome: Option<SteamAuthOutcome>,
    pub error: Option<AuthError>,
    pub forget_confirmed: bool,
    pub code_focus_phase: Option<AuthPhase>,
    pub restore_focus: Option<egui::Id>,
    pub cancel_focused: bool,
    pub job: Option<SteamAuthJob>,
}

impl Default for ServerSteamState {
    fn default() -> Self {
        Self {
            alias: "default".into(),
            account_name: Secret::default(),
            password: Secret::default(),
            code: Secret::default(),
            outcome: None,
            error: None,
            forget_confirmed: false,
            code_focus_phase: None,
            restore_focus: None,
            cancel_focused: false,
            job: None,
        }
    }
}

pub(crate) struct SteamAuthJob {
    pub context: ServerContext,
    pub control: SteamAuthControl,
    pub worker: JoinHandle<SteamAuthOutcome>,
}

impl Foxy {
    pub(crate) fn start_server_steam_auth(&mut self, operation: SteamAuthOperation) {
        if self.server_view.busy() || self.pending_game_space_switch.is_some() {
            return;
        }
        let space = spaces::active_game_space();
        let context =
            match ServerContext::capture(&Self::get_config_directory(), Some(&space.space_id)) {
                Ok(context) if context.game_id() == space.game_id => context,
                _ => {
                    self.server_view.steam_auth.error = Some(AuthError::InvalidInput);
                    return;
                }
            };
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .steam_authentication
        {
            self.server_view.steam_auth.error = Some(AuthError::InvalidInput);
            return;
        }
        let alias = self.server_view.steam_auth.alias.trim().to_owned();
        let control = SteamAuthControl::default();
        let worker_control = control.clone();
        let worker_context = context.clone();
        let repaint = self.repaint_ctx.clone();
        let worker = std::thread::Builder::new()
            .name("native-steam-ui".into())
            .spawn(move || {
                let result = run_steam_auth(&worker_context, &alias, operation, &worker_control);
                Self::request_background_repaint(repaint.as_ref());
                result
            });
        match worker {
            Ok(worker) => {
                let state = &mut self.server_view.steam_auth;
                state.outcome = None;
                state.error = None;
                state.code = Secret::default();
                state.forget_confirmed = false;
                state.code_focus_phase = None;
                state.cancel_focused = false;
                state.job = Some(SteamAuthJob {
                    context,
                    control,
                    worker,
                });
            }
            Err(_) => self.server_view.steam_auth.error = Some(AuthError::Network),
        }
    }

    pub(crate) fn poll_server_steam_auth(&mut self) {
        if let Some(job) = &self.server_view.steam_auth.job {
            if self.close_requested_at.is_some() {
                job.control.cancel();
            }
            if !job.worker.is_finished() {
                return;
            }
        } else {
            return;
        }
        let job = self.server_view.steam_auth.job.take().unwrap();
        let result = job.worker.join();
        self.server_view.steam_auth.code = Secret::default();
        let space = spaces::active_game_space();
        if job.context.space_id() != space.space_id
            || job.context.game_id() != space.game_id
            || std::path::absolute(Self::get_config_directory())
                .ok()
                .as_deref()
                != Some(job.context.data_root())
        {
            return;
        }
        match result {
            Ok(outcome) => {
                self.server_view.steam_auth.outcome = Some(outcome);
            }
            Err(_) => self.server_view.steam_auth.error = Some(AuthError::Network),
        }
    }
}
