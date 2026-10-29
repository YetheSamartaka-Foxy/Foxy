use std::sync::mpsc::{Receiver, TryRecvError};

use eframe::egui;

use crate::core::game::launch_preparation::{LaunchApproval, LaunchFailure, LaunchHandoff};
use crate::ui::app::Foxy;
use crate::ui::types::{Repository, RepositoryServer};

#[derive(Clone)]
pub(crate) struct LaunchRequest {
    pub repository: Repository,
    pub server: Option<RepositoryServer>,
    pub name: String,
    pub mission_path: Option<std::path::PathBuf>,
}

#[derive(Default)]
pub(crate) struct LaunchActionState {
    request: Option<LaunchRequest>,
    approval: LaunchApproval,
    receiver: Option<Receiver<Result<LaunchHandoff, LaunchFailure>>>,
    consent: Option<LaunchFailure>,
    pub handoff_generation: u64,
    pub gameplay_generation: u64,
    pub game_running: Option<bool>,
    gameplay_receiver: Option<Receiver<Option<bool>>>,
    gameplay_checked_at: Option<std::time::Instant>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LaunchActionState {
    pub(crate) fn is_busy(&self) -> bool {
        self.receiver.is_some()
    }
    pub(crate) fn is_pending(&self) -> bool {
        self.receiver.is_some() || self.request.is_some()
    }
    pub(crate) fn awaits_consent(&self) -> bool {
        self.consent.is_some()
    }
}

impl Foxy {
    pub(crate) fn poll_scheduled_gameplay(&mut self, ctx: &egui::Context) {
        if self.scheduler_active_run.is_none() && self.scheduler_pending_post_action.is_none() {
            return;
        }
        if let Some(receiver) = self.launch_action.gameplay_receiver.as_ref() {
            match receiver.try_recv() {
                Ok(status) => {
                    if self.launch_action.game_running == Some(false) && status == Some(true) {
                        self.launch_action.gameplay_generation =
                            self.launch_action.gameplay_generation.wrapping_add(1);
                    }
                    self.launch_action.game_running = status;
                    self.launch_action.gameplay_receiver = None;
                    self.launch_action.gameplay_checked_at = Some(std::time::Instant::now());
                }
                Err(TryRecvError::Disconnected) => {
                    self.launch_action.gameplay_receiver = None;
                    self.launch_action.game_running = None;
                    self.launch_action.gameplay_checked_at = Some(std::time::Instant::now());
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.launch_action.gameplay_receiver.is_none()
            && self
                .launch_action
                .gameplay_checked_at
                .is_none_or(|at| at.elapsed().as_secs() >= 1)
        {
            let game_id = crate::core::game::spaces::active_game_space().game_id;
            let settings = self.settings_view_state.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            self.launch_action.gameplay_receiver = Some(receiver);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let status = crate::core::game::registry()
                    .get(&game_id)
                    .and_then(|module| module.running_game_status(&settings));
                let _ = sender.send(status);
                ctx.request_repaint();
            });
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    pub(crate) fn start_launch_action(&mut self, request: LaunchRequest) {
        if self.launch_action.is_pending()
            || self.close_requested_at.is_some()
            || self.pending_game_space_switch.is_some()
        {
            return;
        }
        self.launch_action.approval = LaunchApproval::default();
        self.launch_action.request = Some(request);
        self.dispatch_launch_action();
    }

    fn dispatch_launch_action(&mut self) {
        let Some(request) = self.launch_action.request.clone() else {
            return;
        };
        let settings = self.settings_view_state.clone();
        let game_id = crate::core::game::spaces::active_game_space().game_id;
        let approval = self.launch_action.approval;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.launch_action.receiver = Some(receiver);
        self.launch_action.consent = None;
        self.launch_action.cancelled = Default::default();
        let cancelled = self.launch_action.cancelled.clone();
        let repaint = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let outcome = crate::core::game::registry()
                .get(&game_id)
                .ok_or_else(|| LaunchFailure::Failed("This game is not supported".to_string()))
                .and_then(|module| {
                    crate::core::game::launch_preparation::dispatch_repository(
                        module,
                        &settings,
                        &request.repository,
                        request.server.as_ref(),
                        approval,
                        request.mission_path.as_deref(),
                        &cancelled,
                    )
                });
            let _ = sender.send(outcome);
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        });
    }

    pub(crate) fn poll_launch_action(&mut self, ctx: &egui::Context) {
        if self.close_requested_at.is_some() {
            self.launch_action
                .cancelled
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let outcome =
            self.launch_action
                .receiver
                .as_ref()
                .and_then(|receiver| match receiver.try_recv() {
                    Ok(outcome) => Some(outcome),
                    Err(TryRecvError::Disconnected) => Some(Err(LaunchFailure::Failed(
                        "Launch worker ended without a result".to_string(),
                    ))),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(outcome) = outcome {
            self.launch_action.receiver = None;
            match outcome {
                Ok(handoff) => {
                    if let Some(request) = self.launch_action.request.take() {
                        self.mark_repository_launched(
                            &request.repository.address,
                            &request.repository.path,
                        );
                    }
                    self.launch_action.handoff_generation =
                        self.launch_action.handoff_generation.wrapping_add(1);
                    log::info!("Launch command handoff accepted (pid={})", handoff.pid);
                    self.show_success_toast(self.t("Launch request sent."));
                    self.handle_post_launch_window_behavior(ctx, "launch command handed off");
                }
                Err(error @ (LaunchFailure::ExtraFiles(_) | LaunchFailure::FrozenCopies(_))) => {
                    self.launch_action.consent = Some(error);
                }
                Err(LaunchFailure::Recovery(error)) => {
                    self.launch_action.request = None;
                    self.configuration_recovery_error = Some(error);
                }
                Err(LaunchFailure::Failed(error)) => {
                    self.launch_action.request = None;
                    self.show_error_toast(
                        self.t_fmt("Launch request failed: {error}", &[("error", error)]),
                    );
                }
            }
        }
        if self.launch_action.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("launch_preparation_busy")).show(ctx, |ui| {
                ui.heading(self.t("Preparing launch"));
                if let Some(request) = self.launch_action.request.as_ref() {
                    ui.label(&request.name);
                }
                ui.add(crate::ui::app::PacedSpinner::new());
                cancel = ui.button(self.t("Cancel")).clicked();
            });
            if cancel {
                self.launch_action
                    .cancelled
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        if self.close_requested_at.is_some() && !self.launch_action.is_busy() {
            self.launch_action.request = None;
            self.launch_action.consent = None;
            return;
        }
        let Some(consent) = self.launch_action.consent.as_ref() else {
            return;
        };
        let (frozen, details) = match consent {
            LaunchFailure::ExtraFiles(details) => (false, details.clone()),
            LaunchFailure::FrozenCopies(details) => (true, details.clone()),
            LaunchFailure::Failed(_) | LaunchFailure::Recovery(_) => return,
        };
        let mut retry = false;
        let mut proceed = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("launch_preparation_consent")).show(ctx, |ui| {
            ui.set_max_width(540.0);
            ui.heading(self.t(if frozen { "Frozen Workshop copies are unavailable" } else { "Extra files could not be applied" }));
            ui.label(self.t(if frozen {
                "Using live copies can change this launch's mod versions. Your saved pins will remain unchanged."
            } else {
                "Some game files could not be prepared. Review the failures before continuing for this launch only."
            }));
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for detail in &details { ui.label(detail); }
            });
            ui.horizontal(|ui| {
                retry = ui.button(self.t("Retry")).clicked();
                proceed = ui.button(self.t(if frozen { "Use live copies for this launch" } else { "Continue for this launch" })).clicked();
                cancel = ui.button(self.t("Cancel")).clicked();
            });
        });
        if cancel {
            self.launch_action.consent = None;
            self.launch_action.request = None;
        } else if proceed || retry {
            if proceed {
                if frozen {
                    self.launch_action.approval.live_workshop_fallback = true;
                } else {
                    self.launch_action.approval.incomplete_extra_files = true;
                }
            }
            self.dispatch_launch_action();
        }
    }
}
