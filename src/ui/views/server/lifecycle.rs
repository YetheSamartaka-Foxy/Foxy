use crate::core::server::InstanceRuntimePhase;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use std::time::{Duration, Instant};

impl Foxy {
    pub(crate) fn render_server_lifecycle(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .lifecycle
        {
            return None;
        }
        let id = self.server_view.selected.clone()?;
        if let (Some(monitoring), Some(last)) = (
            self.server_view.lifecycle_monitoring.as_mut(),
            self.server_view.monitoring_last_received,
        ) {
            let elapsed_ms = u64::try_from(last.elapsed().as_millis()).unwrap_or(u64::MAX);
            if elapsed_ms >= 1000 {
                monitoring.age_by(elapsed_ms);
                self.server_view.monitoring_last_received = Some(Instant::now());
            }
        }
        let current_owner = self.mode_ownership.owner.as_ref().map(|owner| {
            (
                owner.attachment.scope().owner_id.clone(),
                owner.attachment.scope().generation,
            )
        });
        if self
            .server_view
            .lifecycle_monitoring
            .as_ref()
            .is_some_and(|monitoring| {
                current_owner.as_ref().is_none_or(|(owner, generation)| {
                    owner != &monitoring.status.owner_id
                        || *generation != monitoring.status.owner_generation
                })
            })
        {
            self.server_view.lifecycle_monitoring = None;
            self.server_view.monitoring_last_refresh = None;
            self.server_view.monitoring_search.clear();
            self.server_view.monitoring_paused = false;
            self.server_view.monitoring_auto_scroll = true;
        }
        let busy = self.server_view.controls_busy();
        let dirty = self.server_view.dirty();
        if self
            .server_view
            .lifecycle_plan
            .as_ref()
            .is_some_and(|plan| plan.instance_id != id)
        {
            self.server_view.lifecycle_plan = None;
        }
        let plan_matches = self
            .server_view
            .lifecycle_plan
            .as_ref()
            .is_some_and(|plan| plan.instance_id == id);
        let status = self
            .server_view
            .lifecycle_status
            .as_ref()
            .filter(|status| status.instance_id == id)
            .cloned();
        let can_start = status.as_ref().is_some_and(|status| {
            status.phase == InstanceRuntimePhase::NotOwned
                || (status.phase == InstanceRuntimePhase::Exited && status.cleanup_verified)
        });
        let run_generation = status.as_ref().map(|status| status.run_generation);
        let can_stop = status.as_ref().is_some_and(|status| {
            status.phase != InstanceRuntimePhase::NotOwned
                && status.phase != InstanceRuntimePhase::Exited
        });

        ui.separator();
        Some(ui.push_id("server.lifecycle-controls", |ui| {
            ui.heading(tr("Server lifecycle"));
            if self
                .mode_ownership
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| {
                    snapshot.lifetime == crate::core::server::owner::OwnerLifetime::Detached
                })
            {
                ui.label(tr("The detached owner remains supervised when this window closes. Its database and process claims stay held."));
            } else {
                ui.label(tr("Starting accepts this foreground lifetime policy: closing its owner requests shutdown, waits five seconds, then force-cleans remaining server trees. Forced cleanup may lose saves. Closing an attached window leaves its external owner running."));
            }
            if let Some(snapshot) = self.mode_ownership.snapshot.as_ref() {
                ui.label(tr_fmt(
                    "Owner lifetime: {lifetime}",
                    &[("lifetime", tr(match snapshot.lifetime {
                        crate::core::server::owner::OwnerLifetime::Foreground => "Foreground",
                        crate::core::server::owner::OwnerLifetime::ParentBound => "Parent-bound",
                        crate::core::server::owner::OwnerLifetime::Detached => "Detached; server remains supervised after Foxy closes",
                    }))],
                ));
            }
            if crate::core::game::registry()
                .active()
                .server_capabilities()
                .durable_owner
                && self.mode_ownership.owner.as_ref().is_some_and(|owner| owner.is_owned())
            {
                ui.label(tr("Detach owner keeps the server running after this Foxy window closes. The detached owner retains database and process claims until explicitly stopped."));
                if ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Detach owner and keep server running"))).clicked() {
                    self.detach_server_owner();
                }
            }
            if ui.add_enabled(!busy, egui::Button::new(tr("Reconnect to server owner"))).clicked() {
                self.reconnect_server_owner();
            }
            if dirty {
                ui.label(tr("Save or discard draft edits before planning or changing the running server."));
            }
            let status_button = ui.add_enabled(!busy, egui::Button::new(tr("Refresh runtime status")));
            if status_button.clicked() {
                self.server_view.monitoring_paused = false;
                self.server_view.monitoring_last_refresh = Some(Instant::now());
                self.start_server_task(ServerTaskAction::InspectInstanceRuntime(id.clone()));
            }
            if let Some(status) = status.as_ref() {
                ui.label(tr_fmt(
                    "Runtime phase: {phase}; owner: {owner}; run generation: {generation}",
                    &[
                        ("phase", runtime_phase_label(status.phase)),
                        ("owner", status.owner_id.clone()),
                        ("generation", status.run_generation.to_string()),
                    ],
                ));
                ui.label(tr_fmt(
                    "Process observed: {exists}; readiness verified: {ready}; cleanup verified: {cleanup}",
                    &[
                        ("exists", status.process_exists.to_string()),
                        ("ready", status.readiness_verified.to_string()),
                        ("cleanup", status.cleanup_verified.to_string()),
                    ],
                ));
                if status.stop_timed_out {
                    ui.label(tr("Stop timed out. The owner retains the process and cleanup responsibility."));
                }
                if status.phase == InstanceRuntimePhase::Running && !status.readiness_verified {
                    ui.label(tr("A running process was observed, but readiness was not verified."));
                }
                if status.phase == InstanceRuntimePhase::CleanupUncertain {
                    ui.label(tr("Cleanup is uncertain. Refresh status and keep the owner window open."));
                }
            } else {
                ui.label(tr("Runtime ownership and readiness are unknown until status is refreshed."));
            }

            if ui
                .add_enabled(!busy && !dirty, egui::Button::new(tr("Plan server start")))
                .clicked()
            {
                self.server_view.lifecycle_plan = None;
                self.start_server_task(ServerTaskAction::PlanInstanceStart(id.clone()));
            }
            if let Some(plan) = self
                .server_view
                .lifecycle_plan
                .as_ref()
                .filter(|plan| plan.instance_id == id)
            {
                egui::CollapsingHeader::new(tr("Reviewed server start plan"))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.label(tr_fmt(
                            "Plan fingerprint: {fingerprint}",
                            &[("fingerprint", plan.fingerprint.clone())],
                        ));
                        ui.label(tr_fmt(
                            "Runtime: {runtime}; activation generation: {generation}",
                            &[
                                ("runtime", plan.runtime_id.clone()),
                                ("generation", plan.activation_generation.to_string()),
                            ],
                        ));
                        ui.label(tr("Review the committed runtime and fingerprint before starting. A process ID alone does not verify readiness."));
                        if !plan.launch.headless.is_empty() {
                            ui.label(tr_fmt(
                                "Headless clients in this plan: {count}",
                                &[("count", plan.launch.headless.len().to_string())],
                            ));
                            for client in &plan.launch.headless {
                                ui.label(format!(
                                    "{} ({}): {}",
                                    client.name,
                                    client.id,
                                    client.arguments.join(" ")
                                ));
                            }
                            if plan.launch.headless.iter().any(|client| client.join_password) {
                                ui.label(tr("Headless clients read the join password from a private parameter file, not the command line."));
                            }
                        }
                    });
                if ui
                    .add_enabled(
                        !busy && !dirty && can_start,
                        egui::Button::new(tr("Start reviewed server")),
                    )
                    .clicked()
                    && let Some(plan) = self.server_view.lifecycle_plan.take()
                {
                    self.start_server_task(ServerTaskAction::StartInstance(plan));
                }
            }

            if can_stop {
                ui.checkbox(
                    &mut self.server_view.lifecycle_allow_force,
                    tr("Allow force termination"),
                );
                if self.server_view.lifecycle_allow_force {
                    ui.label(tr("Force termination may lose server saves or leave cleanup incomplete. Use it only after graceful stop fails."));
                }
                ui.horizontal_wrapped(|ui| {
                    let expected_run_generation = run_generation.unwrap_or_default();
                    if ui
                        .add_enabled(
                            !busy && !dirty,
                            egui::Button::new(tr("Stop server gracefully")),
                        )
                        .clicked()
                    {
                        self.start_server_task(ServerTaskAction::StopInstance {
                            id: id.clone(),
                            expected_run_generation,
                            force: false,
                            timeout_ms: 30_000,
                        });
                    }
                    if self.server_view.lifecycle_allow_force
                        && ui
                            .add_enabled(
                                !busy && !dirty,
                                egui::Button::new(tr("Force stop server")),
                            )
                            .clicked()
                    {
                        self.start_server_task(ServerTaskAction::StopInstance {
                            id: id.clone(),
                            expected_run_generation,
                            force: true,
                            timeout_ms: 30_000,
                        });
                    }
                    if plan_matches
                        && ui
                            .add_enabled(
                                !busy && !dirty,
                                egui::Button::new(tr("Restart with reviewed plan")),
                            )
                            .clicked()
                        && let Some(plan) = self.server_view.lifecycle_plan.take()
                    {
                        self.start_server_task(ServerTaskAction::RestartInstance {
                            plan,
                            expected_run_generation,
                            force: false,
                            timeout_ms: 30_000,
                        });
                    }
                });
            }

            let headless_capable = crate::core::game::registry()
                .active()
                .server_capabilities()
                .headless_clients;
            if headless_capable
                && let Some(status) = status
                    .as_ref()
                    .filter(|status| status.phase == InstanceRuntimePhase::Running)
            {
                self.render_headless_lifecycle(ui, &id, status, busy, dirty);
            }
            if let Some(outcome) = self
                .server_view
                .lifecycle_outcome
                .as_ref()
                .filter(|outcome| outcome.status.instance_id == id)
            {
                ui.label(tr_fmt(
                    "Lifecycle result: {phase}; completed: {completed}; cancelled: {cancelled}",
                    &[
                        ("phase", runtime_phase_label(outcome.status.phase)),
                        ("completed", outcome.completed.to_string()),
                        ("cancelled", outcome.cancelled.to_string()),
                    ],
                ));
                if let Some(message) = &outcome.message {
                    ui.label(message);
                }
            }
            let process_exists = self
                .server_view
                .lifecycle_status
                .as_ref()
                .filter(|status| status.instance_id == id)
                .is_some_and(|status| status.process_exists);
            if process_exists {
                ui.ctx().request_repaint_after(Duration::from_secs(1));
                let refresh_due = self
                    .server_view
                    .monitoring_last_refresh
                    .is_none_or(|last| last.elapsed() >= Duration::from_secs(3));
                if !self.server_view.busy() && refresh_due {
                    self.server_view.monitoring_last_refresh = Some(Instant::now());
                    self.start_server_task(ServerTaskAction::PollInstanceMonitoring(id.clone()));
                }
            }
            self.render_server_monitoring(ui);
            status_button
        }).inner)
    }
}

fn runtime_phase_label(phase: InstanceRuntimePhase) -> String {
    tr(match phase {
        InstanceRuntimePhase::NotOwned => "Not owned",
        InstanceRuntimePhase::Starting => "Starting",
        InstanceRuntimePhase::Running => "Running",
        InstanceRuntimePhase::StopRequested => "Stop requested",
        InstanceRuntimePhase::Exited => "Exited",
        InstanceRuntimePhase::CleanupUncertain => "Cleanup uncertain",
    })
}
