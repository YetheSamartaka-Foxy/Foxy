use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_deployment_preparation(
        &mut self,
        ui: &mut Ui,
    ) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .deployment_preparation
        {
            return None;
        }
        let id = self.server_view.selected.clone()?;
        let state = self
            .server_view
            .deployment_record_state
            .as_ref()
            .filter(|state| state.instance_id == id);
        let selected = state.and_then(|state| state.selected.clone());
        let generation = state.map(|state| state.generation);
        if self
            .server_view
            .preparation_plan
            .as_ref()
            .is_some_and(|plan| !self.server_view.preparation_plan_is_current(plan))
        {
            self.server_view.preparation_plan = None;
        }
        let busy = self.server_view.busy();
        ui.separator();
        Some(ui.push_id("server.deployment-preparation-controls", |ui| {
            ui.heading(tr("Offline deployment preparation"));
            ui.label(tr("Create and verify an immutable offline copy. This does not activate a deployment or start a server."));
            let plan_button = ui.add_enabled(!busy && selected.is_some(), egui::Button::new(tr("Plan preparation")));
            if plan_button.clicked() && let Some(expected_generation) = generation {
                self.start_server_task(ServerTaskAction::PlanDeploymentPreparation { id: id.clone(), expected_generation });
            }
            if let Some(selected) = &selected {
                ui.label(tr_fmt("Selected record: {id}", &[("id", selected.clone())]));
            } else {
                ui.label(tr("Load a saved offline record selection before planning preparation."));
            }
            if let Some(plan) = &self.server_view.preparation_plan {
                egui::CollapsingHeader::new(tr("Reviewed preparation plan")).default_open(true).show(ui, |ui| {
                    ui.label(tr_fmt("Selected record: {id}", &[("id", plan.manifest.record_id.clone())]));
                    ui.label(tr_fmt("Expected selection generation: {generation}", &[("generation", plan.expected_generation.to_string())]));
                    ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                    ui.label(tr_fmt("Preparation ID: {id}", &[("id", plan.preparation_id.clone())]));
                    ui.label(tr_fmt("Directory: {path}", &[("path", plan.directory.display().to_string())]));
                    ui.label(tr_fmt("Configuration revision: {revision}; content stage: {stage}", &[("revision", plan.manifest.record.configuration_revision.clone()), ("stage", plan.manifest.record.content_stage_id.clone())]));
                    ui.label(tr_fmt("Configuration manifest: {configuration}; content manifest: {content}", &[("configuration", plan.manifest.record.configuration_manifest_fingerprint.clone()), ("content", plan.manifest.record.content_manifest_fingerprint.clone())]));
                    ui.label(tr_fmt("Entries: {count}; bytes: {bytes}; layout: {layout}", &[("count", plan.manifest.entries.len().to_string()), ("bytes", plan.manifest.total_bytes.to_string()), ("layout", plan.manifest.layout_version.to_string())]));
                    ui.collapsing(tr("Prepared file inventory"), |ui| {
                        for entry in plan.manifest.entries.iter().take(128) {
                            ui.label(tr_fmt("{path} ({bytes} bytes, BLAKE3 {hash})", &[("path", entry.path.clone()), ("bytes", entry.bytes.to_string()), ("hash", entry.blake3.clone().unwrap_or_else(|| tr("Directory")))]));
                        }
                        if plan.manifest.entries.len() > 128 {
                            ui.label(tr_fmt("Additional preparation entries omitted: {count}", &[("count", (plan.manifest.entries.len() - 128).to_string())]));
                        }
                    });
                    ui.label(tr("Preparation remains offline and is not an accepted server runtime layout."));
                });
                if ui.add_enabled(!busy, egui::Button::new(tr("Build reviewed preparation"))).clicked()
                    && let Some(plan) = self.server_view.preparation_plan.take()
                {
                    self.start_server_task(ServerTaskAction::BuildDeploymentPreparation(plan));
                }
            }
            let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh prepared generations")));
            if refresh.clicked() {
                self.start_server_task(ServerTaskAction::ListDeploymentPreparations(id.clone()));
            }
            if let Some(list) = &self.server_view.preparation_list {
                ui.label(tr("Prepared generation inventory is not integrity evidence."));
                ui.label(tr_fmt("Unrecognized preparation entries: {count}", &[("count", list.unrecognized_entries.to_string())]));
                for preparation_id in list.preparation_ids.iter().take(1024) {
                    let selected = self.server_view.preparation_selection.as_deref() == Some(preparation_id.as_str());
                    if ui.selectable_label(selected, preparation_id.clone()).clicked() {
                        self.server_view.preparation_selection = Some(preparation_id.clone());
                        self.server_view.preparation_report = None;
                    }
                }
                if ui.add_enabled(!busy && self.server_view.preparation_selection.is_some(), egui::Button::new(tr("Verify selected generation"))).clicked()
                    && let Some(preparation_id) = self.server_view.preparation_selection.clone()
                {
                    self.start_server_task(ServerTaskAction::VerifyDeploymentPreparation { id, preparation_id });
                }
            }
            if let Some(report) = &self.server_view.preparation_report {
                ui.label(tr_fmt("Prepared generation: {id}", &[("id", report.preparation_id.clone())]));
                ui.label(tr_fmt("Files: {files}; bytes: {bytes}; verified: {verified}; published: {published}; reused: {reused}", &[("files", report.files.to_string()), ("bytes", report.bytes.to_string()), ("verified", report.integrity_verified.to_string()), ("published", report.published.to_string()), ("reused", report.reused_existing.to_string())]));
                ui.label(tr("Deployment ready: no. Active deployment changed: no. Process started: no."));
            }
            if plan_button.enabled() { plan_button } else { refresh }
        }).inner)
    }
}
