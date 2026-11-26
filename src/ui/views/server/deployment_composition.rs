use crate::core::server::DeploymentCompositionRequest;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

fn composition_status(status: &str) -> &'static str {
    match status {
        "verified_configuration_only" => "Verified configuration archive",
        "verified_content_only" => "Verified local content archive",
        "not_selected" => "Not selected",
        "missing" => "Missing",
        "invalid" => "Invalid",
        _ => "Unknown status",
    }
}

fn candidate_state(state: &str) -> &'static str {
    match state {
        "candidate" => "Candidate",
        "conflict" => "Conflict",
        "missing" => "Missing",
        _ => "Unknown state",
    }
}

impl Foxy {
    pub(crate) fn render_server_deployment_composition(
        &mut self,
        ui: &mut Ui,
    ) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .deployment_composition
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let instance_id = editor.baseline.id.clone();
        let instance_revision = editor.baseline.revision;
        let dirty = editor.dirty();
        let catalog_revision = self
            .server_view
            .catalog
            .as_ref()
            .map(|catalog| catalog.revision);
        let selected_stage = self.server_view.content_stage_selection.clone();
        let configuration_active = self
            .server_view
            .configuration_revisions
            .as_ref()
            .filter(|revisions| revisions.instance_id == instance_id)
            .map(|revisions| revisions.active.clone());
        let configuration_generation = self
            .server_view
            .configuration_revisions
            .as_ref()
            .filter(|revisions| revisions.instance_id == instance_id)
            .map(|revisions| revisions.generation);
        let loaded_bindings = self
            .server_view
            .saved_content_bindings
            .as_ref()
            .and_then(|snapshot| snapshot.document.as_ref())
            .map(|document| {
                document
                    .bindings
                    .iter()
                    .map(|entry| entry.binding.clone())
                    .collect::<Vec<_>>()
            });
        if self
            .server_view
            .deployment_composition
            .as_ref()
            .is_some_and(|report| {
                dirty
                    || report.instance_id != instance_id
                    || report.instance_revision != instance_revision
                    || Some(report.catalog_revision) != catalog_revision
                    || report.content.requested_stage_id != selected_stage
                    || configuration_active.as_ref().is_some_and(|active| {
                        report.configuration.revision_id.as_ref() != active.as_ref()
                    })
                    || configuration_generation
                        .is_some_and(|generation| report.configuration.generation != generation)
                    || self
                        .server_view
                        .content_sources
                        .as_ref()
                        .is_some_and(|sources| {
                            report.content.current_sources_fingerprint != sources.fingerprint
                        })
                    || self
                        .server_view
                        .saved_content_bindings
                        .as_ref()
                        .is_some_and(|bindings| {
                            report.content.current_bindings.fingerprint != bindings.fingerprint
                        })
                    || loaded_bindings
                        .as_ref()
                        .is_some_and(|bindings| bindings != &self.server_view.content_bindings)
            })
        {
            self.server_view.invalidate_deployment_composition();
        }

        ui.push_id("server.deployment-composition-controls", |ui| {
            ui.heading(tr("Deployment composition review"));
            ui.weak(tr("Review saved configuration and a selected verified content stage together. This report does not deploy or start a server."));
            let busy = self.server_view.busy();
            let enabled = !dirty && !busy;
            let review = ui.add_enabled(
                enabled,
                egui::Button::new(tr("Review deployment composition")),
            );
            if review.clicked() {
                self.server_view.invalidate_deployment_composition();
                let request = DeploymentCompositionRequest {
                    instance_id: instance_id.clone(),
                    stage_id: selected_stage.clone(),
                    expected_revision: Some(instance_revision),
                    expected_configuration_generation: configuration_generation,
                };
                self.start_server_task(ServerTaskAction::PlanDeploymentComposition(request));
            }
            if dirty {
                ui.label(tr("Save or discard draft edits before reviewing deployment composition."));
            }
            if let Some(report) = &self.server_view.deployment_composition {
                ui.label(tr_fmt("Game space: {id}", &[("id", report.game_space_id.clone())]));
                ui.label(tr_fmt("Game: {id}; instance: {instance}", &[("id", report.game_id.clone()), ("instance", report.instance_id.as_str().to_owned())]));
                ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", report.fingerprint.clone())]));
                ui.label(tr_fmt("Instance revision: {revision}; catalog revision: {catalog}", &[("revision", report.instance_revision.to_string()), ("catalog", report.catalog_revision.to_string())]));

                ui.collapsing(tr("Configuration evidence"), |ui| {
                    ui.label(tr_fmt("Status: {status}; generation: {generation}", &[("status", tr(composition_status(report.configuration.status))), ("generation", report.configuration.generation.to_string())]));
                    ui.label(tr_fmt("Selected revision: {revision}", &[("revision", report.configuration.revision_id.clone().unwrap_or_else(|| tr("None")))]));
                    ui.label(tr_fmt("Selection fingerprint: {fingerprint}", &[("fingerprint", report.configuration.selection_fingerprint.clone().unwrap_or_else(|| tr("None")))]));
                    ui.label(tr_fmt("Manifest fingerprint: {fingerprint}", &[("fingerprint", report.configuration.manifest_fingerprint.clone().unwrap_or_else(|| tr("None")))]));
                    ui.label(tr_fmt("Integrity verified: {value}; matches current draft: {matches}", &[("value", super::chrome::yes_no(report.configuration.integrity_verified)), ("matches", report.configuration.matches_current_draft.map_or_else(|| tr("Unknown"), super::chrome::yes_no))]));
                    if let Some(preview) = &report.configuration.preview {
                        ui.label(tr_fmt("Configuration preview fingerprint: {fingerprint}; launch ready: {ready}", &[("fingerprint", preview.fingerprint.clone()), ("ready", super::chrome::yes_no(preview.launch_ready))]));
                    }
                });

                ui.collapsing(tr("Content stage evidence"), |ui| {
                    ui.label(tr_fmt("Status: {status}; requested stage: {stage}", &[("status", tr(composition_status(report.content.status))), ("stage", report.content.requested_stage_id.clone().unwrap_or_else(|| tr("None")))]));
                    ui.label(tr_fmt("Integrity verified: {value}; matches saved bindings: {matches}", &[("value", super::chrome::yes_no(report.content.integrity_verified)), ("matches", report.content.matches_saved_bindings.map_or_else(|| tr("Unknown"), super::chrome::yes_no))]));
                    ui.label(tr_fmt("Current bindings fingerprint: {fingerprint}", &[("fingerprint", report.content.current_bindings.fingerprint.clone().unwrap_or_else(|| tr("None")))]));
                    ui.label(tr_fmt("Current sources fingerprint: {fingerprint}", &[("fingerprint", report.content.current_sources_fingerprint.clone().unwrap_or_else(|| tr("None")))]));
                    if let Some(stage) = &report.content.stage {
                        ui.label(tr_fmt("Stage {id}: {files} files, {bytes} bytes; verified: {verified}", &[("id", stage.stage_id.clone()), ("files", stage.files.to_string()), ("bytes", stage.bytes.to_string()), ("verified", super::chrome::yes_no(stage.verified))]));
                    }
                    if let Some(bindings) = &report.content.reviewed_bindings {
                        ui.label(tr_fmt("Reviewed saved bindings: {count}", &[("count", bindings.bindings.len().to_string())]));
                    }
                });

                ui.collapsing(tr("Key file candidates"), |ui| {
                    ui.label(tr_fmt("Candidate files: {count}", &[("count", report.artifacts.key_files.to_string())]));
                    for key in report.artifacts.keys.iter().take(256) {
                        ui.collapsing(tr_fmt("{name}: {state}", &[("name", key.name.clone()), ("state", tr(candidate_state(key.state)))]), |ui| {
                            if key.flattened_collision {
                                ui.label(tr("Files with this key name have conflicting recorded bytes."));
                            }
                            for file in key.files.iter().take(64) {
                                ui.label(tr_fmt("{path} ({bytes} bytes, BLAKE3 {hash})", &[("path", file.path.clone()), ("bytes", file.bytes.to_string()), ("hash", file.blake3.clone())]));
                            }
                            if key.files.len() > 64 {
                                ui.label(tr_fmt("Additional paths omitted: {count}", &[("count", (key.files.len() - 64).to_string())]));
                            }
                        });
                    }
                    if report.artifacts.keys.len() > 256 {
                        ui.label(tr_fmt("Additional key groups omitted: {count}", &[("count", (report.artifacts.keys.len() - 256).to_string())]));
                    }
                });
                ui.collapsing(tr("Mission file candidates"), |ui| {
                    ui.label(tr_fmt("Candidate files: {count}", &[("count", report.artifacts.mission_files.to_string())]));
                    for mission in report.artifacts.missions.iter().take(256) {
                        ui.collapsing(tr_fmt("{template}: {state}", &[("template", mission.template.clone()), ("state", tr(candidate_state(mission.state)))]), |ui| {
                            for file in mission.files.iter().take(64) {
                                ui.label(tr_fmt("{path} ({bytes} bytes, BLAKE3 {hash})", &[("path", file.path.clone()), ("bytes", file.bytes.to_string()), ("hash", file.blake3.clone())]));
                            }
                            if mission.files.len() > 64 {
                                ui.label(tr_fmt("Additional paths omitted: {count}", &[("count", (mission.files.len() - 64).to_string())]));
                            }
                        });
                    }
                    if report.artifacts.missions.len() > 256 {
                        ui.label(tr_fmt("Additional mission groups omitted: {count}", &[("count", (report.artifacts.missions.len() - 256).to_string())]));
                    }
                });
                ui.collapsing(tr("Artifact review notes"), |ui| {
                    for issue in &report.artifacts.issues {
                        ui.label(tr_fmt("{code}: {message}", &[("code", tr(issue.code)), ("message", tr(issue.message))]));
                    }
                    if report.artifacts.issues.is_empty() {
                        ui.label(tr("No artifact review notes."));
                    }
                });
                if report.artifacts.truncated {
                    ui.label(tr("Candidate details were truncated; composition remains incomplete."));
                }
                egui::CollapsingHeader::new(tr_fmt("Review blockers ({count})", &[("count", report.blockers.len().to_string())])).default_open(true).show(ui, |ui| {
                    if report.blockers.is_empty() {
                        ui.label(tr("No composition blockers were reported."));
                    } else {
                        for blocker in &report.blockers {
                            ui.label(tr_fmt("{code}: {message}", &[("code", tr(blocker.code)), ("message", tr(&blocker.message))]));
                        }
                    }
                });
                ui.collapsing(tr("Runtime and installation evidence"), |ui| {
                    ui.label(tr_fmt("Installation: {id}", &[("id", report.installation_id.as_ref().map_or_else(|| tr("None"), |id| id.as_str().to_owned()))]));
                    ui.label(tr_fmt("Installation verified: {verified}; runtime launch available: {available}", &[("verified", super::chrome::yes_no(report.installation_verified)), ("available", super::chrome::yes_no(report.runtime_prerequisites.launch_available))]));
                    if let Some(consumers) = &report.installation_consumers {
                        ui.label(tr_fmt("Consumer inspection complete: {complete}; recorded consumers: {count}; cleanup required: {cleanup}; liveness verified: {liveness}; exclusive maintenance authorized: {authorized}", &[("complete", super::chrome::yes_no(consumers.inspection_complete)), ("count", consumers.recorded_consumers.len().to_string()), ("cleanup", consumers.cleanup_required.map_or_else(|| tr("Unknown"), super::chrome::yes_no)), ("liveness", super::chrome::yes_no(consumers.liveness_verified)), ("authorized", super::chrome::yes_no(consumers.exclusive_maintenance_authorized))]));
                        if let Some(error) = consumers.inspection_error {
                            ui.label(tr_fmt("Consumer inspection note: {message}", &[("message", tr(error))]));
                        }
                    } else {
                        ui.label(tr("Consumer inspection is unavailable."));
                    }
                });
                ui.label(tr_fmt("Deployment ready: {value}; writes performed: {writes}; active deployment changed: {changed}; downloads started: {downloads}; installation leases acquired: {leases}; process started: {process}", &[("value", super::chrome::yes_no(report.deployment_ready)), ("writes", super::chrome::yes_no(report.writes_performed)), ("changed", super::chrome::yes_no(report.active_deployment_changed)), ("downloads", super::chrome::yes_no(report.downloads_started)), ("leases", super::chrome::yes_no(report.installation_leases_acquired)), ("process", super::chrome::yes_no(report.process_started))]));
            }
            if self.server_view.pending_focus == Some("server-deployment-composition-controls")
                && ui.clip_rect().intersects(review.rect)
            {
                review.request_focus();
                self.server_view.pending_focus = None;
            }
            review
        })
        .inner
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::{candidate_state, composition_status};

    #[test]
    fn internal_statuses_map_to_plain_labels_with_safe_fallbacks() {
        assert_eq!(
            composition_status("verified_configuration_only"),
            "Verified configuration archive"
        );
        assert_eq!(
            composition_status("verified_content_only"),
            "Verified local content archive"
        );
        assert_eq!(
            composition_status("unknown_internal_code"),
            "Unknown status"
        );
        assert_eq!(candidate_state("conflict"), "Conflict");
        assert_eq!(candidate_state("unknown_internal_code"), "Unknown state");
    }
}
