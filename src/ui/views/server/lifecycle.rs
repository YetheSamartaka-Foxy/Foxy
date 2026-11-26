use super::chrome::{Tone, yes_no};
use crate::core::server::InstanceRuntimePhase;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use std::time::{Duration, Instant};

impl Foxy {
    pub(super) fn observed_server_status(
        &self,
        id: &crate::core::server::ServerInstanceId,
    ) -> Option<&crate::core::server::InstanceRuntimeStatus> {
        let owner = self.mode_ownership.owner.as_ref()?.attachment.scope();
        self.server_view.lifecycle_status.as_ref().filter(|status| {
            &status.instance_id == id
                && status.owner_id == owner.owner_id
                && status.owner_generation == owner.generation
        })
    }

    pub(super) fn poll_server_workspace_runtime(&mut self, ui: &Ui) {
        if self.mode_ownership.owner.is_none()
            || !crate::core::game::registry()
                .active()
                .server_capabilities()
                .lifecycle
        {
            return;
        }
        let Some(id) = self.server_view.selected.clone() else {
            return;
        };
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
        let process_exists = self
            .observed_server_status(&id)
            .is_some_and(|status| status.process_exists);
        if process_exists || self.server_view.monitoring_last_refresh.is_none() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            let refresh_due = self
                .server_view
                .monitoring_last_refresh
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(3));
            if !self.server_view.busy() && refresh_due {
                self.server_view.monitoring_last_refresh = Some(Instant::now());
                self.start_server_task(ServerTaskAction::PollInstanceMonitoring(id));
            }
        }
    }

    pub(super) fn render_server_runtime_refresh(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .lifecycle
        {
            return None;
        }
        let id = self.server_view.selected.clone()?;
        let response = ui.add_enabled(
            !self.server_view.controls_busy() && !self.server_view.dirty(),
            egui::Button::new(tr("Refresh runtime status")),
        );
        Self::focus_outline(ui, &response);
        if response.clicked() {
            self.server_view.monitoring_paused = false;
            self.server_view.monitoring_last_refresh = Some(Instant::now());
            self.start_server_task(ServerTaskAction::InspectInstanceRuntime(id));
        }
        Some(response)
    }

    pub(super) fn render_server_owner_tools(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .lifecycle
        {
            self.render_owner_status(ui);
            return;
        }
        let busy = self.server_view.controls_busy();
        let dirty = self.server_view.dirty();
        ui.heading(tr("Server owner"));
        self.render_owner_status(ui);
        if let Some(snapshot) = self.mode_ownership.snapshot.as_ref() {
            self.server_facts(
                ui,
                "owner-lifetime",
                &[(
                    tr("Owner lifetime"),
                    tr(match snapshot.lifetime {
                        crate::core::server::owner::OwnerLifetime::Foreground => "Foreground",
                        crate::core::server::owner::OwnerLifetime::ParentBound => "Parent-bound",
                        crate::core::server::owner::OwnerLifetime::Detached => {
                            "Detached; server remains supervised after Foxy closes"
                        }
                    }),
                )],
            );
        }
        let detached = self
            .mode_ownership
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.lifetime == crate::core::server::owner::OwnerLifetime::Detached
            });
        self.server_note(ui, tr(if detached {
            "The detached owner remains supervised when this window closes. Its database and process claims stay held."
        } else {
            "Starting accepts this foreground lifetime policy: closing its owner requests shutdown, waits five seconds, then force-cleans remaining server trees. Forced cleanup may lose saves. Closing an attached window leaves its external owner running."
        }));
        let can_detach = crate::core::game::registry()
            .active()
            .server_capabilities()
            .durable_owner
            && self
                .mode_ownership
                .owner
                .as_ref()
                .is_some_and(|owner| owner.is_owned());
        if can_detach {
            self.server_note(ui, tr("Detach owner keeps the server running after this Foxy window closes. The detached owner retains database and process claims until explicitly stopped."));
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if can_detach
                && ui
                    .add_enabled(
                        !busy && !dirty,
                        egui::Button::new(tr("Detach owner and keep server running")),
                    )
                    .clicked()
            {
                self.detach_server_owner();
            }
            if ui
                .add_enabled(!busy, egui::Button::new(tr("Reconnect to server owner")))
                .clicked()
            {
                self.reconnect_server_owner();
            }
        });
    }

    pub(super) fn server_runtime_badge(
        &self,
        id: &crate::core::server::ServerInstanceId,
    ) -> (String, Tone) {
        self.observed_server_status(id)
            .map(|status| {
                (
                    runtime_phase_label(status.phase),
                    runtime_phase_tone(status.phase),
                )
            })
            .unwrap_or_else(|| (tr("Unknown"), Tone::Neutral))
    }

    pub(crate) fn render_server_lifecycle(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .lifecycle
        {
            return None;
        }
        let id = self.server_view.selected.clone()?;
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
        let status = self.observed_server_status(&id).cloned();
        let can_start = status.as_ref().is_some_and(|status| {
            status.phase == InstanceRuntimePhase::NotOwned
                || (status.phase == InstanceRuntimePhase::Exited && status.cleanup_verified)
        });
        let run_generation = status.as_ref().map(|status| status.run_generation);
        let can_stop = status.as_ref().is_some_and(|status| {
            status.phase != InstanceRuntimePhase::NotOwned
                && status.phase != InstanceRuntimePhase::Exited
        });

        Some(ui.push_id("server.lifecycle-controls", |ui| {
            let (phase_label, phase_tone) = self.server_runtime_badge(&id);
            ui.horizontal_wrapped(|ui| {
                ui.heading(tr("Server lifecycle"));
                ui.add_space(6.0);
                let message = tr_fmt("Server status: {status}", &[("status", phase_label.clone())]);
                let pill = self
                    .server_status_pill(ui, &phase_label, phase_tone)
                    .on_hover_text(tr("Foxy reports only servers it started. Processes started outside Foxy are not tracked."));
                super::workspace::server_live_status(ui, &pill, &message);
            });
            if dirty {
                self.server_callout(
                    ui,
                    Tone::Warning,
                    &tr("Save or discard draft edits before planning or changing the running server."),
                    |_| {},
                );
            }
            if let Some(status) = status.as_ref() {
                let observed = |value: bool| tr(if value { "Observed" } else { "Not observed" });
                let verified = |value: bool| tr(if value { "Verified" } else { "Not verified" });
                self.server_facts(
                    ui,
                    "lifecycle",
                    &[
                        (tr("Server process"), observed(status.process_exists)),
                        (tr("Readiness"), verified(status.readiness_verified)),
                        (tr("Cleanup"), verified(status.cleanup_verified)),
                    ],
                );
                if status.stop_timed_out {
                    self.server_callout(ui, Tone::Danger, &tr("Stop timed out. The owner retains the process and cleanup responsibility."), |_| {});
                }
                let query_status = self.server_view.lifecycle_monitoring.as_ref().filter(|monitoring| {
                    status.process_exists
                        && monitoring.status.instance_id == status.instance_id
                        && monitoring.status.owner_id == status.owner_id
                        && monitoring.status.owner_generation == status.owner_generation
                        && monitoring.status.run_generation == status.run_generation
                });
                if let Some(monitoring) = query_status {
                    super::monitoring::render_query_status(ui, monitoring);
                }
                if status.phase == InstanceRuntimePhase::Running && !status.readiness_verified
                    && !query_status.is_some_and(|monitoring| monitoring.query_ready)
                {
                    self.server_note(ui, tr("A running process was observed, but readiness was not verified."));
                }
                if status.phase == InstanceRuntimePhase::CleanupUncertain {
                    self.server_callout(ui, Tone::Danger, &tr("Cleanup is uncertain. Refresh status and keep the owner window open."), |_| {});
                }
            } else {
                self.server_note(ui, tr("Runtime ownership and readiness are unknown until status is refreshed."));
            }

            ui.add_space(6.0);
            let mut status_button = None;
            ui.horizontal_wrapped(|ui| {
                status_button = self.render_server_runtime_refresh(ui);
                let plan = ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Plan server start")));
                Self::focus_outline(ui, &plan);
                if plan.clicked() {
                    self.server_view.lifecycle_plan = None;
                    self.start_server_task(ServerTaskAction::PlanInstanceStart(id.clone()));
                }
                if can_stop {
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
                                egui::Button::new(
                                    egui::RichText::new(tr("Force stop server"))
                                        .color(self.color_error()),
                                ),
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
                }
            });
            if can_stop {
                ui.checkbox(
                    &mut self.server_view.lifecycle_allow_force,
                    tr("Allow force termination"),
                );
                if self.server_view.lifecycle_allow_force {
                    self.server_callout(ui, Tone::Danger, &tr("Force termination may lose server saves or leave cleanup incomplete. Use it only after graceful stop fails."), |_| {});
                }
            }

            if let Some(plan) = self
                .server_view
                .lifecycle_plan
                .as_ref()
                .filter(|plan| plan.instance_id == id)
            {
                ui.add_space(6.0);
                let detached = self.mode_ownership.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.lifetime == crate::core::server::owner::OwnerLifetime::Detached
                });
                let rows = [
                    (tr("Runtime"), plan.runtime_id.clone()),
                    (tr("Activation generation"), plan.activation_generation.to_string()),
                    (tr("Fingerprint"), plan.fingerprint.clone()),
                ];
                let headless = plan.launch.headless.clone();
                egui::CollapsingHeader::new(tr("Reviewed server start plan"))
                    .default_open(true)
                    .show(ui, |ui| {
                        self.server_facts(ui, "start-plan", &rows);
                        self.server_note(ui, tr("Review the committed runtime and fingerprint before starting. A process ID alone does not verify readiness."));
                        self.server_note(ui, tr(if detached {
                            "The detached owner remains supervised when this window closes. Its database and process claims stay held."
                        } else {
                            "Starting accepts this foreground lifetime policy: closing its owner requests shutdown, waits five seconds, then force-cleans remaining server trees. Forced cleanup may lose saves. Closing an attached window leaves its external owner running."
                        }));
                        if !headless.is_empty() {
                            egui::CollapsingHeader::new(tr_fmt(
                                "Headless clients in this plan: {count}",
                                &[("count", headless.len().to_string())],
                            )).id_salt("server-headless-start-plan").show(ui, |ui| {
                                for client in &headless {
                                    ui.label(format!(
                                        "{} ({}): {}",
                                        client.name,
                                        client.id,
                                        client.arguments.join(" ")
                                    ));
                                }
                                if headless.iter().any(|client| client.join_password) {
                                    self.server_note(ui, tr("Headless clients read the join password from a private parameter file, not the command line."));
                                }
                            });
                        }
                    });
                let start = self.add_sized_primary_button(
                    ui,
                    egui::Vec2::ZERO,
                    egui::Button::new(tr("Start reviewed server")),
                    !busy && !dirty && can_start,
                );
                Self::focus_outline(ui, &start);
                if start.clicked()
                    && let Some(plan) = self.server_view.lifecycle_plan.take()
                {
                    self.start_server_task(ServerTaskAction::StartInstance(plan));
                }
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
                ui.separator();
                self.render_headless_lifecycle(ui, &id, status, busy, dirty);
            }
            if let Some(outcome) = self
                .server_view
                .lifecycle_outcome
                .as_ref()
                .filter(|outcome| outcome.status.instance_id == id)
            {
                let result = tr_fmt(
                    "Lifecycle result: {phase}; completed: {completed}; cancelled: {cancelled}",
                    &[
                        ("phase", runtime_phase_label(outcome.status.phase)),
                        ("completed", yes_no(outcome.completed)),
                        ("cancelled", yes_no(outcome.cancelled)),
                    ],
                );
                self.server_note(ui, result);
                if let Some(message) = &outcome.message {
                    ui.label(message);
                }
            }
            if let Some(status) = status.as_ref() {
                egui::CollapsingHeader::new(tr("Technical details"))
                    .id_salt("server-lifecycle-details")
                    .show(ui, |ui| {
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
                                ("exists", yes_no(status.process_exists)),
                                ("ready", yes_no(status.readiness_verified)),
                                ("cleanup", yes_no(status.cleanup_verified)),
                            ],
                        ));
                    });
            }
            status_button
        }).inner).flatten()
    }
}

pub(super) fn runtime_phase_label(phase: InstanceRuntimePhase) -> String {
    tr(match phase {
        InstanceRuntimePhase::NotOwned => "Not running",
        InstanceRuntimePhase::Starting => "Starting",
        InstanceRuntimePhase::Running => "Running",
        InstanceRuntimePhase::StopRequested => "Stop requested",
        InstanceRuntimePhase::Exited => "Exited",
        InstanceRuntimePhase::CleanupUncertain => "Cleanup uncertain",
    })
}

pub(super) fn runtime_phase_tone(phase: InstanceRuntimePhase) -> Tone {
    match phase {
        InstanceRuntimePhase::NotOwned | InstanceRuntimePhase::Exited => Tone::Neutral,
        InstanceRuntimePhase::Starting => Tone::Info,
        InstanceRuntimePhase::Running => Tone::Success,
        InstanceRuntimePhase::StopRequested => Tone::Warning,
        InstanceRuntimePhase::CleanupUncertain => Tone::Danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_phases_map_to_plain_labels_and_tones() {
        assert_eq!(
            runtime_phase_label(InstanceRuntimePhase::NotOwned),
            "Not running"
        );
        assert_eq!(
            runtime_phase_tone(InstanceRuntimePhase::NotOwned),
            Tone::Neutral
        );
        assert_eq!(
            runtime_phase_tone(InstanceRuntimePhase::Running),
            Tone::Success
        );
        assert_eq!(
            runtime_phase_tone(InstanceRuntimePhase::Starting),
            Tone::Info
        );
        assert_eq!(
            runtime_phase_tone(InstanceRuntimePhase::StopRequested),
            Tone::Warning
        );
        assert_eq!(
            runtime_phase_tone(InstanceRuntimePhase::CleanupUncertain),
            Tone::Danger
        );
    }
}
