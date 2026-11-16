use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_deployment_activation(
        &mut self,
        ui: &mut Ui,
    ) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .deployment_activation
        {
            return None;
        }
        let id = self.server_view.selected.clone()?;
        let state = self
            .server_view
            .deployment_record_state
            .as_ref()
            .filter(|state| state.instance_id == id);
        let generation = state.map(|state| state.generation);
        let preparation_id = self.server_view.preparation_selection.clone();
        let activation_generation = self
            .server_view
            .activation_status
            .as_ref()
            .filter(|status| status.instance_id == id)
            .map(|status| status.activation_generation);
        if self
            .server_view
            .activation_plan
            .as_ref()
            .is_some_and(|plan| {
                plan.instance_id != id
                    || self.server_view.preparation_selection.as_deref()
                        != Some(plan.preparation_id.as_str())
                    || generation != Some(plan.expected_selection_generation)
                    || activation_generation != Some(plan.expected_activation_generation)
            })
        {
            self.server_view.activation_plan = None;
        }
        let busy = self.server_view.busy() || self.server_view.dirty();
        ui.separator();
        Some(ui.push_id("server.deployment-activation-controls", |ui| {
            ui.heading(tr("Native deployment activation"));
            ui.label(tr("Create an immutable native runtime copy from a reviewed preparation. The adopted installation and preparation source remain unchanged; activation does not start a process."));
            ui.label(tr(if crate::core::game::registry().active().server_capabilities().key_trust {
                "Activation copies reviewed required and server-only addons, the tested base key and explicitly trusted public keys. Optional addons are not loaded."
            } else {
                "Reforger activation copies the reviewed base engine and server.json. Choose explicit IPv4/game/A2S endpoints and a built-in scenario with no addon intent or staged addon content."
            }));
            if self.server_view.activation_status.as_ref().is_none_or(|status| status.instance_id != id) {
                ui.label(tr("Load activation status before planning so the current activation generation can be checked."));
            }
            let status_button = ui.add_enabled(!busy, egui::Button::new(tr("Refresh activation status")));
            if status_button.clicked() {
                self.start_server_task(ServerTaskAction::InspectNativeActivation(id.clone()));
            }
            if let Some(status) = self.server_view.activation_status.as_ref().filter(|status| status.instance_id == id) {
                ui.label(tr_fmt("Activation generation: {generation}; runtime integrity verified: {verified}; recovery required: {recovery}", &[
                    ("generation", status.activation_generation.to_string()),
                    ("verified", status.integrity_verified.to_string()),
                    ("recovery", status.recovery_required.to_string()),
                ]));
                if let Some(runtime) = &status.runtime {
                    ui.label(tr_fmt("Active runtime: {runtime}; manifest fingerprint: {fingerprint}", &[
                        ("runtime", runtime.runtime_id.clone()),
                        ("fingerprint", runtime.manifest_fingerprint.clone()),
                    ]));
                }
            }
            let ready_to_plan = generation.is_some()
                && activation_generation.is_some()
                && preparation_id.is_some()
                && self.server_view.activation_status.as_ref().is_some_and(|status| status.instance_id == id && !status.recovery_required);
            let plan_button = ui.add_enabled(!busy && ready_to_plan, egui::Button::new(tr("Plan activation")));
            if plan_button.clicked()
                && let (Some(expected_selection_generation), Some(expected_activation_generation), Some(preparation_id)) =
                    (generation, activation_generation, preparation_id.clone())
            {
                self.start_server_task(ServerTaskAction::PlanNativeActivation {
                    id: id.clone(),
                    preparation_id,
                    expected_selection_generation,
                    expected_activation_generation,
                });
            }
            if let Some(preparation_id) = preparation_id {
                ui.label(tr_fmt("Selected prepared generation: {id}", &[("id", preparation_id)]));
            } else {
                ui.label(tr("Select a prepared generation before planning activation."));
            }
            if let Some(plan) = &self.server_view.activation_plan {
                egui::CollapsingHeader::new(tr("Reviewed activation plan")).default_open(true).show(ui, |ui| {
                    ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                    ui.label(tr_fmt("Preparation source: {id}", &[("id", plan.preparation_id.clone())]));
                    ui.label(tr_fmt("Runtime candidate: {id}", &[("id", plan.runtime.runtime_id.clone())]));
                    ui.label(tr_fmt("Runtime directory: {path}", &[("path", plan.runtime_directory.display().to_string())]));
                    ui.label(tr_fmt("Expected selection generation: {generation}; activation generation: {activation}", &[
                        ("generation", plan.expected_selection_generation.to_string()),
                        ("activation", plan.expected_activation_generation.to_string()),
                    ]));
                    ui.label(tr_fmt("Native payload: {files} entries; {bytes} bytes; platform {platform}", &[
                        ("files", plan.native_runtime.payload.entries.len().to_string()),
                        ("bytes", plan.native_runtime.payload.total_bytes.to_string()),
                        ("platform", format!("{:?}", plan.native_runtime.payload.platform)),
                    ]));
                    for template in plan.native_runtime.mission_templates.iter().take(32) {
                        ui.label(tr_fmt("Mission template: {template}", &[("template", template.clone())]));
                    }
                    for path in plan.native_runtime.server_mods.iter().take(32) {
                        ui.label(tr_fmt("Server-only addon: {path}", &[("path", path.clone())]));
                    }
                    let omitted = plan.native_runtime.mission_templates.len().saturating_sub(32) + plan.native_runtime.server_mods.len().saturating_sub(32);
                    if omitted > 0 { ui.label(tr_fmt("Additional reviewed content entries omitted: {count}", &[("count", omitted.to_string())])); }
                    egui::CollapsingHeader::new(tr("Native engine file inventory")).show(ui, |ui| {
                        for entry in plan.native_runtime.payload.entries.iter().take(32) {
                            ui.label(tr_fmt("{path} ({bytes} bytes)", &[("path", entry.path.clone()), ("bytes", entry.bytes.to_string())]));
                        }
                        if plan.native_runtime.payload.entries.len() > 32 {
                            ui.label(tr_fmt("Additional runtime entries omitted: {count}", &[("count", (plan.native_runtime.payload.entries.len() - 32).to_string())]));
                        }
                    });
                    ui.label(tr("Activation creates a private runtime copy and changes the active reference. It does not modify the adopted source or start a process."));
                });
                if ui.add_enabled(!busy, egui::Button::new(tr("Activate reviewed deployment"))).clicked()
                    && let Some(plan) = self.server_view.activation_plan.take()
                {
                    self.start_server_task(ServerTaskAction::ApplyNativeActivation(plan));
                }
            }

            ui.separator();
            ui.heading(tr("Deployment recovery"));
            ui.label(tr("Recovery review reads the retained transaction and installation intent. Resolve only the exact reviewed outcome."));
            if ui.add_enabled(!busy, egui::Button::new(tr("Review deployment recovery"))).clicked() {
                self.start_server_task(ServerTaskAction::PlanNativeRecovery(id.clone()));
            }
            if let Some(review) = &self.server_view.activation_resolution {
                ui.label(tr_fmt("Recovery fingerprint: {fingerprint}; expected outcome: {outcome}", &[
                    ("fingerprint", review.fingerprint.clone()),
                    ("outcome", review.expected_outcome.clone()),
                ]));
                ui.label(tr_fmt("Activation generation: {generation}; runtime: {runtime}; journal present: {journal}", &[
                    ("generation", review.activation_generation.to_string()),
                    ("runtime", review.runtime_id.clone().unwrap_or_else(|| tr("None"))),
                    ("journal", review.journal_present.to_string()),
                ]));
                ui.label(tr("Resolution preserves the observed active deployment and retires only the reviewed transaction protection."));
                if ui.add_enabled(!busy, egui::Button::new(tr("Resolve reviewed recovery"))).clicked()
                    && let Some(review) = self.server_view.activation_resolution.take()
                {
                    self.start_server_task(ServerTaskAction::ResolveNativeActivation(review));
                }
            }
            if let Some(outcome) = &self.server_view.activation_outcome {
                ui.label(tr_fmt("Activation result: {status}; generation {generation}; runtime {runtime}", &[
                    ("status", outcome.status.clone()),
                    ("generation", outcome.activation_generation.map_or_else(|| tr("Unknown"), |value| value.to_string())),
                    ("runtime", outcome.runtime_id.clone().unwrap_or_else(|| tr("Unknown"))),
                ]));
                ui.label(tr_fmt("Runtime verified: {verified}; journal pending: {journal}; installation intent pending: {intent}; process started: {started}", &[
                    ("verified", outcome.runtime_verified.to_string()),
                    ("journal", outcome.journal_pending.to_string()),
                    ("intent", outcome.intent_pending.to_string()),
                    ("started", outcome.process_started.to_string()),
                ]));
                if outcome.journal_pending || outcome.intent_pending {
                    ui.label(tr("The transaction may need recovery. Refresh activation status and review recovery before retrying."));
                }
            }
            if plan_button.enabled() { plan_button } else { status_button }
        }).inner)
    }
}
