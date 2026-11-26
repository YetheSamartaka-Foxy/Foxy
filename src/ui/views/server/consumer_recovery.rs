use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

/// Focus target for the safe choice of an opened cleanup confirmation; this section, not the page,
/// moves focus to it once the button exists.
pub(super) const CONFIRM_CANCEL_FOCUS: &str = "server-consumer-confirm-cancel";

impl Foxy {
    pub(super) fn render_consumer_recovery(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .consumer_recovery
        {
            return;
        }
        let Some(id) = self.server_view.selected.clone() else {
            return;
        };
        let dirty = self.server_view.dirty();
        let busy = self.server_view.busy();
        if self
            .server_view
            .consumer_recovery_plan
            .as_ref()
            .is_some_and(|plan| plan.instance_id != id)
        {
            self.server_view.consumer_recovery_plan = None;
            self.server_view.consumer_recovery_confirmation = false;
        }

        ui.heading(tr("Consumer recovery"));
        ui.weak(tr("Review crash cleanup records for this installation. Recovery preserves unknown consumers."));
        if dirty {
            ui.label(tr(
                "Save or discard server draft edits before reviewing consumer recovery.",
            ));
        }
        if ui
            .add_enabled(
                !busy && !dirty,
                egui::Button::new(tr("Review crash cleanup")),
            )
            .clicked()
        {
            self.server_view.consumer_recovery_plan = None;
            self.server_view.consumer_recovery_outcome = None;
            self.server_view.consumer_recovery_confirmation = false;
            self.start_server_task(ServerTaskAction::PlanConsumerRecovery(id.clone()));
        }

        if let Some(plan) = self
            .server_view
            .consumer_recovery_plan
            .as_ref()
            .filter(|plan| plan.instance_id == id)
            .cloned()
        {
            ui.label(tr_fmt(
                "Review fingerprint: {fingerprint}",
                &[("fingerprint", plan.fingerprint.clone())],
            ));
            ui.label(tr_fmt(
                "Installation: {installation}; inspection complete: {complete}; owner authenticated: {authenticated}",
                &[
                    ("installation", plan.installation_id.as_str().to_owned()),
                    ("complete", super::chrome::yes_no(plan.inspection_complete)),
                    ("authenticated", plan.owner_authenticated.to_string()),
                ],
            ));
            ui.label(tr_fmt(
                "Eligible: {eligible}; pending: {pending}; completed history: {history}",
                &[
                    ("eligible", plan.eligible_count.to_string()),
                    ("pending", plan.pending_count.to_string()),
                    ("history", plan.completed_history.to_string()),
                ],
            ));
            if !plan.owner_authenticated {
                ui.label(tr(
                    "Owner authentication is unavailable. No consumer records can be retired.",
                ));
            }
            if !plan.inspection_complete {
                ui.label(tr(
                    "Inspection is incomplete. Unknown and legacy records are preserved.",
                ));
            }
            for blocker in &plan.blockers {
                ui.label(tr_fmt("Blocked: {reason}", &[("reason", blocker.clone())]));
            }
            egui::ScrollArea::vertical()
                .id_salt(("consumer-recovery-entries", id.as_str()))
                .max_height(220.0)
                .show(ui, |ui| {
                    for entry in plan.entries.iter().filter(|entry| entry.eligible).chain(plan.entries.iter().filter(|entry| !entry.eligible)).take(128) {
                        ui.group(|ui| {
                            ui.label(tr_fmt(
                                "Session: {space} / {instance} / {operation} / generation {generation}",
                                &[
                                    ("space", entry.session.game_space_id.clone()),
                                    ("instance", entry.session.instance_id.clone()),
                                    ("operation", entry.session.operation_id.clone()),
                                    ("generation", entry.session.run_generation.to_string()),
                                ],
                            ));
                            ui.label(tr_fmt(
                                "Receipt: {state}; registration present: {present}",
                                &[
                                    ("state", entry.receipt_state.clone()),
                                    ("present", super::chrome::yes_no(entry.registration_present)),
                                ],
                            ));
                            ui.label(if entry.eligible {
                                tr("Eligible based on a verified old-owner kernel receipt and the selected record group.")
                            } else {
                                tr_fmt("Preserved: {reason}", &[("reason", entry.reason.clone())])
                            });
                            ui.label(tr_fmt(
                                "Consumer fingerprint: {fingerprint}",
                                &[("fingerprint", entry.consumer_fingerprint.clone())],
                            ));
                        });
                    }
                    if plan.entries.len() > 128 {
                        ui.label(tr_fmt(
                            "Showing 128 of {total} records. Apply is limited to the exact eligible records in this review.",
                            &[("total", plan.entries.len().to_string())],
                        ));
                    }
                });
            let can_apply = !busy
                && !dirty
                && plan.owner_authenticated
                && plan.inspection_complete
                && plan.eligible_count > 0
                && plan.eligible_count <= 128;
            if plan.eligible_count > 128 {
                ui.label(tr("Review more than 128 eligible consumer records through the CLI before applying cleanup."));
            }
            if self.server_view.consumer_recovery_confirmation {
                ui.label(tr("Confirm retiring only the exact eligible records in this reviewed plan. Unknown, legacy, malformed, and current-owner records remain untouched."));
                ui.horizontal(|ui| {
                    let apply =
                        ui.add_enabled(can_apply, egui::Button::new(tr("Apply reviewed cleanup")));
                    Self::focus_outline(ui, &apply);
                    if apply.clicked() {
                        let fingerprint = plan.fingerprint.clone();
                        self.server_view.consumer_recovery_confirmation = false;
                        self.server_view.consumer_recovery_outcome = None;
                        self.start_server_task(ServerTaskAction::ApplyConsumerRecovery {
                            id: id.clone(),
                            expected_plan_fingerprint: fingerprint,
                        });
                    }
                    let cancel = ui.button(tr("Cancel cleanup"));
                    if self.server_view.pending_focus == Some(CONFIRM_CANCEL_FOCUS) {
                        cancel.request_focus();
                        cancel.scroll_to_me(Some(egui::Align::Center));
                        self.server_view.pending_focus = None;
                    }
                    Self::focus_outline(ui, &cancel);
                    if cancel.clicked()
                        || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                    {
                        self.server_view.consumer_recovery_confirmation = false;
                    }
                });
            } else if ui
                .add_enabled(
                    can_apply,
                    egui::Button::new(tr("Review cleanup confirmation")),
                )
                .clicked()
            {
                self.server_view.consumer_recovery_confirmation = true;
                self.server_view.pending_focus = Some(CONFIRM_CANCEL_FOCUS);
            }
        }

        if let Some(outcome) = self
            .server_view
            .consumer_recovery_outcome
            .as_ref()
            .filter(|outcome| outcome.instance_id == id)
        {
            if !outcome.inspection_complete {
                ui.label(tr("Recovery inspection is incomplete; the remaining count is last observed. Review again before continuing."));
            }
            if let Some(error) = &outcome.error {
                ui.label(tr_fmt(
                    "Recovery stopped: {reason}",
                    &[("reason", error.clone())],
                ));
            }
            ui.label(tr_fmt(
                "Recovery retired {retired} records; {remaining} remain; writes performed: {writes}; cancelled: {cancelled}",
                &[
                    ("retired", outcome.retired_count.to_string()),
                    ("remaining", outcome.remaining_records.to_string()),
                    ("writes", super::chrome::yes_no(outcome.writes_performed)),
                    ("cancelled", super::chrome::yes_no(outcome.cancelled)),
                ],
            ));
            if outcome.cancelled {
                ui.label(tr(
                    "Cleanup was cancelled. Review the current records before trying again.",
                ));
            }
        }
    }
}
