use crate::core::server::DeploymentCompositionRequest;
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

fn role_label(role: crate::core::server::ServerAddonRole) -> &'static str {
    match role {
        crate::core::server::ServerAddonRole::Required => "Required",
        crate::core::server::ServerAddonRole::Optional => "Optional",
        crate::core::server::ServerAddonRole::ServerOnly => "Server only",
    }
}

impl Foxy {
    pub(crate) fn render_server_deployment_records(
        &mut self,
        ui: &mut Ui,
    ) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .deployment_records
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let revision = editor.baseline.revision;
        let dirty = editor.dirty();
        let catalog_revision = self
            .server_view
            .catalog
            .as_ref()
            .map(|catalog| catalog.revision);
        let active_configuration = self
            .server_view
            .configuration_revisions
            .as_ref()
            .filter(|revisions| revisions.instance_id == id);
        let has_configuration = active_configuration.is_some();
        let active_revision = active_configuration.and_then(|revisions| revisions.active.clone());
        let configuration_generation = active_configuration.map(|revisions| revisions.generation);
        let deployment_generation = self
            .server_view
            .deployment_record_state
            .as_ref()
            .filter(|state| state.instance_id == id)
            .map(|state| state.generation);
        let selected_stage = self.server_view.content_stage_selection.clone();
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
            .deployment_record_plan
            .as_ref()
            .is_some_and(|plan| {
                dirty
                    || plan.manifest.instance_id != id
                    || plan.manifest.instance_revision != revision
                    || Some(plan.manifest.catalog_revision) != catalog_revision
                    || plan.manifest.content_stage_id
                        != selected_stage.as_deref().unwrap_or_default()
                    || active_revision
                        .as_ref()
                        .is_some_and(|active| plan.manifest.configuration_revision != *active)
                    || configuration_generation.is_some_and(|generation| {
                        plan.review.configuration.generation != generation
                    })
                    || deployment_generation
                        .is_some_and(|generation| generation != plan.expected_generation)
                    || self
                        .server_view
                        .content_sources
                        .as_ref()
                        .is_some_and(|sources| {
                            plan.review.content.current_sources_fingerprint != sources.fingerprint
                        })
                    || self
                        .server_view
                        .saved_content_bindings
                        .as_ref()
                        .is_some_and(|bindings| {
                            plan.review.content.current_bindings.fingerprint != bindings.fingerprint
                        })
                    || loaded_bindings
                        .as_ref()
                        .is_some_and(|bindings| bindings != &self.server_view.content_bindings)
            })
        {
            self.server_view.invalidate_deployment_record_plan();
        }
        if self
            .server_view
            .deployment_record_selection_plan
            .as_ref()
            .is_some_and(|plan| {
                plan.instance_id != id
                    || plan.target.manifest.game_space_id
                        != crate::core::game::spaces::active_game_space().space_id
                    || deployment_generation != Some(plan.expected_generation)
                    || self
                        .server_view
                        .deployment_record_inspection_selection
                        .as_deref()
                        != Some(plan.target.record_id.as_str())
                    || !self
                        .server_view
                        .deployment_record_selection_plan_is_current(plan)
            })
        {
            self.server_view
                .invalidate_deployment_record_selection_plan();
        }

        ui.separator();
        ui.push_id("server.deployment-record-controls", |ui| {
            ui.heading(tr("Offline deployment records"));
            ui.label(tr("Save verified configuration and content archive identities as an offline selection. This does not activate a deployment or start a server."));
            let busy = self.server_view.busy();
            let prepare_enabled = !dirty
                && !busy
                && has_configuration
                && active_revision.is_some()
                && selected_stage.is_some();
            let prepare = ui.add_enabled(
                prepare_enabled,
                egui::Button::new(tr("Prepare deployment record")),
            );
            if prepare.clicked() {
                self.server_view.invalidate_deployment_record_plan();
                let request = DeploymentCompositionRequest {
                    instance_id: id.clone(),
                    stage_id: selected_stage.clone(),
                    expected_revision: Some(revision),
                    expected_configuration_generation: configuration_generation,
                };
                self.start_server_task(ServerTaskAction::PrepareDeploymentRecord {
                    request,
                    expected_selection_generation: deployment_generation,
                });
            }
            if dirty {
                ui.label(tr("Save or discard server draft edits before preparing a deployment record."));
            } else if !has_configuration {
                ui.label(tr("Load the current saved configuration selection before preparing a record."));
            } else if active_revision.is_none() {
                ui.label(tr("Select and save a configuration revision before preparing a deployment record."));
            } else if selected_stage.is_none() {
                ui.label(tr("Select a verified local content stage before preparing a deployment record."));
            }

            if let Some(plan) = &self.server_view.deployment_record_plan {
                egui::CollapsingHeader::new(tr("Reviewed deployment record"))
                    .default_open(true)
                    .show(ui, |ui| {
                    ui.label(tr_fmt("Manifest schema version: {version}", &[("version", plan.manifest.schema_version.to_string())]));
                    ui.label(tr_fmt("Record ID: {id}", &[("id", plan.record_id.clone())]));
                    ui.label(tr_fmt("Record fingerprint: {fingerprint}", &[("fingerprint", plan.fingerprint.clone())]));
                    ui.label(tr_fmt("Expected offline selection generation: {generation}", &[("generation", plan.expected_generation.to_string())]));
                    ui.label(tr_fmt("Game space: {space}; game: {game}; instance: {instance}", &[("space", plan.manifest.game_space_id.clone()), ("game", plan.manifest.game_id.clone()), ("instance", plan.manifest.instance_id.as_str().to_owned())]));
                    ui.label(tr_fmt("Instance revision: {revision}; catalog revision: {catalog}", &[("revision", plan.manifest.instance_revision.to_string()), ("catalog", plan.manifest.catalog_revision.to_string())]));
                    ui.label(tr_fmt("Catalog fingerprint: {fingerprint}", &[("fingerprint", plan.manifest.catalog_fingerprint.clone())]));
                    ui.label(tr_fmt("Configuration revision: {revision}", &[("revision", plan.manifest.configuration_revision.clone())]));
                    ui.label(tr_fmt("Configuration manifest fingerprint: {fingerprint}", &[("fingerprint", plan.manifest.configuration_manifest_fingerprint.clone())]));
                    ui.label(tr_fmt("Configuration content fingerprint: {fingerprint}", &[("fingerprint", plan.manifest.configuration_fingerprint.clone())]));
                    ui.label(tr_fmt("Content stage: {id}", &[("id", plan.manifest.content_stage_id.clone())]));
                    ui.label(tr_fmt("Content manifest fingerprint: {fingerprint}", &[("fingerprint", plan.manifest.content_manifest_fingerprint.clone())]));
                    ui.label(tr_fmt("Saved bindings fingerprint: {fingerprint}", &[("fingerprint", plan.manifest.content_bindings_fingerprint.clone())]));
                    ui.label(tr_fmt("Saved bindings generation: {generation}; reviewed entries: {count}", &[("generation", plan.manifest.content_bindings.generation.to_string()), ("count", plan.manifest.content_bindings.bindings.len().to_string())]));
                    ui.label(tr_fmt("Bindings document catalog fingerprint: {fingerprint}; source fingerprint: {sources}", &[("fingerprint", plan.manifest.content_bindings.catalog_fingerprint.clone()), ("sources", plan.manifest.content_bindings.content_sources_fingerprint.clone().unwrap_or_else(|| tr("None")))]));
                    ui.collapsing(tr("Pinned binding identities"), |ui| {
                        for entry in plan.manifest.content_bindings.bindings.iter().take(128) {
                            ui.label(tr_fmt("{source} - {role}, order {order}; profile {profile}; folder {folder}", &[("source", entry.binding.source_id.clone()), ("role", tr(role_label(entry.binding.role))), ("order", entry.binding.order.to_string()), ("profile", entry.profile.name.clone()), ("folder", entry.folder.id.clone())]));
                        }
                        if plan.manifest.content_bindings.bindings.len() > 128 {
                            ui.label(tr_fmt("Additional pinned binding identities omitted: {count}", &[("count", (plan.manifest.content_bindings.bindings.len() - 128).to_string())]));
                        }
                    });
                    ui.label(tr_fmt("Composition review fingerprint: {fingerprint}", &[("fingerprint", plan.review.fingerprint.clone())]));
                    ui.label(tr_fmt("Configuration verified: {value}; content verified: {content}; configuration matches saved draft: {matches}", &[("value", plan.review.configuration.integrity_verified.to_string()), ("content", plan.review.content.integrity_verified.to_string()), ("matches", plan.review.configuration.matches_current_draft.map_or_else(|| tr("Unknown"), |value| value.to_string()))]));
                    if !plan.review.blockers.is_empty() {
                        ui.label(tr_fmt("Runtime readiness gates in this review: {count}", &[("count", plan.review.blockers.len().to_string())]));
                        for blocker in &plan.review.blockers {
                            ui.label(tr_fmt("Gate {code}: {message}", &[("code", tr(blocker.code)), ("message", tr(&blocker.message))]));
                        }
                    }
                });
                ui.label(tr("The record pins retained archives. Saving selects only an offline record; it does not activate content or change a running server."));
                let save = ui.add_enabled(!busy, egui::Button::new(tr("Save reviewed deployment record")));
                if save.clicked()
                    && let Some(plan) = self.server_view.deployment_record_plan.take()
                {
                    self.start_server_task(ServerTaskAction::CommitDeploymentRecord(plan));
                }
            }

            let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh deployment records")));
            if refresh.clicked() {
                self.start_server_task(ServerTaskAction::ListDeploymentRecords(id.clone()));
            }
            if let Some(state) = &self.server_view.deployment_record_state {
                ui.label(tr_fmt("Offline selection generation: {generation}; selected record: {selected}", &[("generation", state.generation.to_string()), ("selected", state.selected.clone().unwrap_or_else(|| tr("None")))]));
                ui.label(tr("Record index is inventory only; records must be verified individually."));
                if state.records.is_empty() {
                    ui.label(tr("No deployment records are indexed."));
                }
                let record_ids = state.records.clone();
                for record_id in record_ids.iter().take(1024) {
                    let selected = self.server_view.deployment_record_inspection_selection.as_deref() == Some(record_id.as_str());
                    if ui.selectable_label(selected, record_id.clone()).clicked() {
                        self.server_view.invalidate_deployment_record_selection_plan();
                        self.server_view.deployment_record_inspection_selection = Some(record_id.clone());
                        self.server_view.deployment_record_verification = None;
                        self.server_view.deployment_record_commit = None;
                    }
                }
                let verify = ui.add_enabled(
                    !busy && self.server_view.deployment_record_inspection_selection.is_some(),
                    egui::Button::new(tr("Verify selected deployment record")),
                );
                if verify.clicked()
                    && let Some(record_id) = self.server_view.deployment_record_inspection_selection.clone()
                {
                    self.start_server_task(ServerTaskAction::VerifyDeploymentRecord {
                        id: id.clone(),
                        record_id,
                    });
                }
                let select_offline = ui.add_enabled(
                    !busy
                        && deployment_generation.is_some()
                        && self.server_view.deployment_record_inspection_selection.is_some(),
                    egui::Button::new(tr("Review offline record selection")),
                );
                if select_offline.clicked()
                    && let (Some(record_id), Some(expected_generation)) = (
                        self.server_view.deployment_record_inspection_selection.clone(),
                        deployment_generation,
                    )
                {
                    self.server_view.invalidate_deployment_record_selection_plan();
                    self.start_server_task(ServerTaskAction::PlanDeploymentRecordSelection {
                        id: id.clone(),
                        record_id,
                        expected_generation,
                    });
                }
            }
            if let Some(report) = &self.server_view.deployment_record_verification {
                ui.collapsing(tr("Deployment record verification result"), |ui| {
                    ui.label(tr_fmt("Record ID: {id}; integrity verified: {integrity}", &[("id", report.record_id.clone()), ("integrity", report.integrity_verified.to_string())]));
                    ui.label(tr_fmt("Manifest schema version: {version}", &[("version", report.manifest.schema_version.to_string())]));
                    ui.label(tr_fmt("Configuration verified: {configuration}; content verified: {content}", &[("configuration", report.configuration_verified.to_string()), ("content", report.content_verified.to_string())]));
                    ui.label(tr_fmt("Configuration revision: {revision}; content stage: {stage}", &[("revision", report.manifest.configuration_revision.clone()), ("stage", report.manifest.content_stage_id.clone())]));
                    ui.label(tr_fmt("Game space: {space}; game: {game}; instance: {instance}", &[("space", report.manifest.game_space_id.clone()), ("game", report.manifest.game_id.clone()), ("instance", report.manifest.instance_id.as_str().to_owned())]));
                    ui.label(tr_fmt("Instance revision: {revision}; catalog revision: {catalog}; catalog fingerprint: {fingerprint}", &[("revision", report.manifest.instance_revision.to_string()), ("catalog", report.manifest.catalog_revision.to_string()), ("fingerprint", report.manifest.catalog_fingerprint.clone())]));
                    ui.label(tr_fmt("Configuration manifest: {configuration}; content manifest: {content}", &[("configuration", report.manifest.configuration_manifest_fingerprint.clone()), ("content", report.manifest.content_manifest_fingerprint.clone())]));
                    ui.label(tr_fmt("Configuration fingerprint: {fingerprint}; saved bindings fingerprint: {bindings}", &[("fingerprint", report.manifest.configuration_fingerprint.clone()), ("bindings", report.manifest.content_bindings_fingerprint.clone())]));
                    ui.label(tr_fmt("Saved bindings generation: {generation}; entries: {count}", &[("generation", report.manifest.content_bindings.generation.to_string()), ("count", report.manifest.content_bindings.bindings.len().to_string())]));
                    ui.label(tr_fmt("Bindings document catalog fingerprint: {fingerprint}; source fingerprint: {sources}", &[("fingerprint", report.manifest.content_bindings.catalog_fingerprint.clone()), ("sources", report.manifest.content_bindings.content_sources_fingerprint.clone().unwrap_or_else(|| tr("None")))]));
                    ui.collapsing(tr("Verified pinned binding identities"), |ui| {
                        for entry in report.manifest.content_bindings.bindings.iter().take(128) {
                            ui.label(tr_fmt("{source} - {role}, order {order}; profile {profile}; folder {folder}", &[("source", entry.binding.source_id.clone()), ("role", tr(role_label(entry.binding.role))), ("order", entry.binding.order.to_string()), ("profile", entry.profile.name.clone()), ("folder", entry.folder.id.clone())]));
                        }
                        if report.manifest.content_bindings.bindings.len() > 128 {
                            ui.label(tr_fmt("Additional pinned binding identities omitted: {count}", &[("count", (report.manifest.content_bindings.bindings.len() - 128).to_string())]));
                        }
                    });
                    ui.label(tr("Verification is independent of current draft, saved bindings, and selected record state."));
                    ui.label(tr_fmt("Deployment ready: {ready}; writes performed: {writes}; active deployment changed: {changed}", &[("ready", report.deployment_ready.to_string()), ("writes", report.writes_performed.to_string()), ("changed", report.active_deployment_changed.to_string())]));
                });
            }
            if let Some(commit) = &self.server_view.deployment_record_commit {
                ui.label(tr_fmt("Saved offline selection generation: {generation}; selection changed: {changed}; record published: {published}", &[("generation", commit.state.generation.to_string()), ("changed", commit.selection_changed.to_string()), ("published", commit.published.to_string())]));
                ui.label(tr("No runtime deployment was activated and no process was started."));
            }
            let focus_anchor = if prepare_enabled { &prepare } else { &refresh };
            if self.server_view.pending_focus == Some("server-deployment-record-controls")
                && ui.clip_rect().intersects(focus_anchor.rect)
            {
                focus_anchor.request_focus();
                self.server_view.pending_focus = None;
            }
            focus_anchor.clone()
        })
        .inner
        .into()
    }
}
