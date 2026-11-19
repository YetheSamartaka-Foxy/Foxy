use crate::ui::app::Foxy;
use crate::ui::app::server::{ServerDialog, ServerTaskAction};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_reconciliation(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .content_reconciliation
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let dirty = editor.dirty();
        let busy = self.server_view.busy();
        ui.separator();
        let response = ui.push_id("server.reconciliation-controls", |ui| {
            ui.heading(tr("Content consumers and storage reconciliation"));
            ui.label(tr("Review shared content sources, saved profiles and repository instances across game spaces. URL matches alone do not identify the same repository."));

            let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh content consumer report")));
            if refresh.clicked() {
                self.start_server_task(ServerTaskAction::LoadContentConsumers);
            }
            if let Some(report) = &self.server_view.content_consumers {
                egui::ScrollArea::vertical()
                    .id_salt("server-content-consumers")
                    .max_height(240.0)
                    .show(ui, |ui| {
                        ui.label(tr_fmt("Game space: {space}; source profiles: {count}", &[("space", report.game_space_id.clone()), ("count", report.sources.len().to_string())]));
                        ui.label(tr_fmt("Inspection complete: {complete}; discovery scope: {scope}", &[("complete", report.inspection_complete.to_string()), ("scope", report.source_discovery_scope.to_owned())]));
                        for source in &report.sources {
                            ui.collapsing(tr_fmt("Profile {profile}", &[("profile", source.profile.clone())]), |ui| {
                                ui.label(tr_fmt("Identity: {url} at {path}; folder {folder}", &[("url", source.remote_url.clone()), ("path", source.local_path.clone()), ("folder", source.folder_id.clone())]));
                                ui.label(tr_fmt("Physical addon directories: {items}", &[("items", source.physical_addons.join(", "))]));
                                for item in &source.instances { ui.label(tr_fmt("Server instance {name} ({id}); binding role {role}; stages {stages}; active record {active}; pinned records {pins}", &[("name", item.display_name.clone()), ("id", item.instance_id.as_str().to_owned()), ("role", item.saved_binding_role.clone().unwrap_or_else(|| tr("None"))), ("stages", item.retained_stages.join(", ")), ("active", item.active_record.clone().unwrap_or_else(|| tr("None"))), ("pins", item.pinned_records.join(", "))])); }
                                for name in &source.client_repositories { ui.label(tr_fmt("Matching client repository instance: {name}", &[("name", name.clone())])); }
                                for name in &source.other_space_profiles { ui.label(tr_fmt("Shared profile in another game space: {name}", &[("name", name.clone())])); }
                                for name in &source.shared_with_profiles { ui.label(tr_fmt("Shares physical addon directories with: {name}", &[("name", name.clone())])); }
                            });
                        }
                        for diagnostic in &report.diagnostics { ui.label(tr_fmt("Diagnostic: {message}", &[("message", diagnostic.clone())])); }
                        ui.label(tr("Persistent installation consumer registrations"));
                        for inspection in &report.installation_consumers {
                            ui.label(tr_fmt("Installation {id}: inspection complete {complete}; recorded consumers {count}; cleanup required {cleanup}; liveness verified {live}; maintenance authorized {authorized}", &[("id", inspection.installation_id.as_str().to_owned()), ("complete", inspection.inspection_complete.to_string()), ("count", inspection.recorded_consumers.len().to_string()), ("cleanup", inspection.cleanup_required.map_or_else(|| tr("Unknown"), |value| value.to_string())), ("live", inspection.liveness_verified.to_string()), ("authorized", inspection.exclusive_maintenance_authorized.to_string())]));
                            for consumer in &inspection.recorded_consumers {
                                ui.label(tr_fmt("Consumer space {space}; instance {instance}; operation {operation}; run generation {generation}", &[("space", consumer.session.game_space_id.clone()), ("instance", consumer.session.instance_id.clone()), ("operation", consumer.session.operation_id.clone()), ("generation", consumer.session.run_generation.to_string())]));
                            }
                        }
                    });
            }

            let refresh_pins = ui.add_enabled(!busy, egui::Button::new(tr("Refresh deployment pins")));
            if refresh_pins.clicked() { self.start_server_task(ServerTaskAction::LoadDeploymentPins(id.clone())); }
            if let Some((pins, indexed_records)) = self
                .server_view
                .deployment_pins
                .as_ref()
                .filter(|(pins, _)| pins.instance_id == id)
                .map(|(pins, records)| (pins.clone(), records.clone()))
            {
                ui.label(tr_fmt("Pin generation: {generation}", &[("generation", pins.generation.to_string())]));
                egui::ScrollArea::vertical().id_salt("server-pin-records").max_height(160.0).show(ui, |ui| {
                    for record_id in &indexed_records {
                        let selected = self.server_view.deployment_record_inspection_selection.as_deref() == Some(record_id.as_str());
                        if ui.selectable_label(selected, record_id).clicked() { self.server_view.deployment_record_inspection_selection = Some(record_id.clone()); }
                    }
                });
                let selected = self.server_view.deployment_record_inspection_selection.as_deref();
                if selected.is_some_and(|record| indexed_records.iter().any(|indexed| indexed == record)) {
                    let record_id = selected.unwrap().to_owned();
                    let pinned = pins.records.contains(&record_id);
                    let label = if pinned { "Unpin selected deployment record" } else { "Pin selected deployment record" };
                    if ui.add_enabled(!busy && !dirty, egui::Button::new(tr(label))).clicked() {
                        self.start_server_task(ServerTaskAction::SetDeploymentPin { id: id.clone(), record_id, pinned: !pinned, expected_generation: pins.generation });
                    }
                }
            }

            ui.separator();
            ui.label(tr("Reconciliation removes only unreferenced artifacts for this server instance. Content sources, records, configuration archives, runtime state, other instances and unknown data are preserved."));
            if dirty { ui.label(tr("Save or discard server draft edits before reviewing storage reconciliation.")); }
            let plan_button = ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Review storage reconciliation")));
            if plan_button.clicked() {
                self.server_view.reconciliation_plan = None;
                self.start_server_task(ServerTaskAction::PlanReconciliation(id.clone()));
            }
            if let Some(plan) = self.server_view.reconciliation_plan.as_ref().filter(|plan| plan.instance_id == id) {
                egui::ScrollArea::vertical().id_salt("server-reconciliation-plan").max_height(240.0).show(ui, |ui| {
                    ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                    ui.label(tr_fmt("Reclaimable bytes: {bytes}; candidates: {count}", &[("bytes", plan.reclaimable_bytes.to_string()), ("count", plan.candidates.len().to_string())]));
                    ui.label(tr("Retained artifacts and reasons"));
                    for artifact in &plan.retained { ui.label(tr_fmt("{kind} {id}: {reasons}", &[("kind", format!("{:?}", artifact.kind)), ("id", artifact.id.clone()), ("reasons", artifact.reasons.join(", "))])); }
                    ui.label(tr("Reclamation candidates"));
                    for artifact in &plan.candidates { ui.label(tr_fmt("{kind} {id}: {bytes} bytes; fingerprint {fingerprint}", &[("kind", format!("{:?}", artifact.kind)), ("id", artifact.id.clone()), ("bytes", artifact.bytes.to_string()), ("fingerprint", artifact.fingerprint.clone())])); }
                    for unknown in &plan.unrecognized_entries { ui.label(tr_fmt("Preserved unknown entry: {name}", &[("name", unknown.clone())])); }
                    for blocker in &plan.blockers { ui.label(tr_fmt("Blocked: {reason}", &[("reason", blocker.clone())])); }
                });
                let review_enabled = !busy && !dirty && plan.blockers.is_empty() && !plan.candidates.is_empty();
                if ui.add_enabled(review_enabled, egui::Button::new(tr("Review reclamation confirmation"))).clicked() {
                    self.server_view.open_dialog(ServerDialog::ReclaimDeploymentStorage);
                }
            }
            if let Some(outcome) = self.server_view.reconciliation_outcome.as_ref().filter(|outcome| outcome.instance_id == id) {
                ui.label(tr_fmt("Reconciliation removed {count} artifacts and reclaimed {bytes} bytes; sources changed: {sources}; records changed: {records}", &[("count", outcome.removed.len().to_string()), ("bytes", outcome.reclaimed_bytes.to_string()), ("sources", outcome.sources_changed.to_string()), ("records", outcome.records_changed.to_string())]));
            }
            plan_button
        });
        Some(response.inner)
    }
}
