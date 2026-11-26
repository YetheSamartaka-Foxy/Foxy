use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_content_stage(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .content_staging
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let game_space_id = editor.baseline.game_space_id.clone();
        let instance_revision = editor.baseline.revision;
        let dirty = editor.dirty();
        let bindings_match_saved = self
            .server_view
            .saved_content_bindings
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot.document.as_ref().is_some_and(|document| {
                    let saved = document
                        .bindings
                        .iter()
                        .map(|entry| entry.binding.clone())
                        .collect::<Vec<_>>();
                    let source_fingerprint = self
                        .server_view
                        .content_sources
                        .as_ref()
                        .and_then(|sources| sources.fingerprint.as_deref());
                    let catalog_revision = self
                        .server_view
                        .catalog
                        .as_ref()
                        .map(|catalog| catalog.revision);
                    saved == self.server_view.content_bindings
                        && document.game_space_id == game_space_id
                        && document.instance_id == id
                        && document.instance_revision == instance_revision
                        && Some(document.catalog_revision) == catalog_revision
                        && document.content_sources_fingerprint.as_deref() == source_fingerprint
                })
            });
        if dirty {
            self.server_view.invalidate_content_stage();
        } else if !bindings_match_saved {
            self.server_view.content_stage_plan = None;
        }
        ui.heading(tr("Verified local content staging"));
        ui.weak(tr("Stages saved local content with hashes. Staging does not download, activate, or change the active deployment."));
        let saved = self.server_view.saved_content_bindings.as_ref();
        let baseline_ready = !dirty
            && bindings_match_saved
            && saved.is_some_and(|snapshot| {
                snapshot.review_current
                    && snapshot.document.is_some()
                    && snapshot.fingerprint.is_some()
            });
        if !baseline_ready {
            ui.label(tr(
                "Reload and review saved content bindings before planning a local stage.",
            ));
        }
        let busy = self.server_view.busy();
        let mut action = None;
        let plan_response = ui.add_enabled(
            baseline_ready && !busy,
            egui::Button::new(tr("Plan verified local content stage")),
        );
        if plan_response.clicked()
            && let Some(fingerprint) = saved.and_then(|snapshot| snapshot.fingerprint.clone())
        {
            action = Some(ServerTaskAction::PlanContentStage {
                id: id.clone(),
                expected_bindings_fingerprint: fingerprint,
            });
        }
        if let Some(plan) = &self.server_view.content_stage_plan {
            ui.label(tr_fmt("Stage id: {id}", &[("id", plan.stage_id.clone())]));
            ui.label(tr_fmt(
                "Stage directory: {path}",
                &[("path", plan.directory.display().to_string())],
            ));
            ui.label(tr_fmt(
                "Manifest fingerprint: {fingerprint}",
                &[("fingerprint", plan.fingerprint.clone())],
            ));
            let files = plan
                .manifest
                .entries
                .iter()
                .filter(|entry| matches!(entry.kind, crate::core::server::StageEntryKind::File))
                .count();
            let hashes = plan
                .manifest
                .entries
                .iter()
                .filter(|entry| entry.blake3.is_some())
                .count();
            ui.label(tr_fmt("Manifest entries: {entries}; files: {files}; hashed files: {hashes}; total bytes: {bytes}", &[("entries", plan.manifest.entries.len().to_string()), ("files", files.to_string()), ("hashes", hashes.to_string()), ("bytes", plan.manifest.total_bytes.to_string())]));
            let stage =
                ui.add_enabled(!busy, egui::Button::new(tr("Stage verified local content")));
            if stage.clicked() {
                action = self
                    .server_view
                    .content_stage_plan
                    .take()
                    .map(ServerTaskAction::ApplyContentStage);
            }
        }
        let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh saved content stages")));
        if refresh.clicked() {
            action = Some(ServerTaskAction::ListContentStages(id.clone()));
        }
        if let Some(list) = &self.server_view.content_stage_list {
            ui.label(tr_fmt(
                "Unrecognized stage entries: {count}",
                &[("count", list.unrecognized_entries.to_string())],
            ));
            ui.label(tr("Stage listing does not verify stage integrity."));
            for stage_id in list.stage_ids.clone() {
                let selected =
                    self.server_view.content_stage_selection.as_deref() == Some(&stage_id);
                if ui.selectable_label(selected, stage_id.clone()).clicked() {
                    self.server_view.content_stage_selection = Some(stage_id);
                }
            }
            if let Some(stage_id) = self.server_view.content_stage_selection.clone() {
                let verify = ui.add_enabled(!busy, egui::Button::new(tr("Verify recorded stage")));
                if verify.clicked() {
                    action = Some(ServerTaskAction::VerifyContentStage {
                        id: id.clone(),
                        stage_id,
                    });
                }
            }
        }
        if let Some(report) = &self.server_view.content_stage_report {
            ui.label(tr_fmt(
                "Verified stage: {id}",
                &[("id", report.stage_id.clone())],
            ));
            ui.label(tr_fmt(
                "Stage directory: {path}",
                &[("path", report.directory.display().to_string())],
            ));
            ui.label(tr_fmt(
                "Verified files: {files}; bytes: {bytes}",
                &[
                    ("files", report.files.to_string()),
                    ("bytes", report.bytes.to_string()),
                ],
            ));
            ui.label(tr_fmt(
                "Published: {published}; reused existing stage: {reused}",
                &[
                    ("published", super::chrome::yes_no(report.published)),
                    ("reused", super::chrome::yes_no(report.reused_existing)),
                ],
            ));
            ui.label(tr(if report.verified {
                "Stage integrity verified."
            } else {
                "Stage integrity was not verified."
            }));
            ui.label(tr_fmt(
                "Deployment readiness: {ready}. Active deployment changed: {changed}. Downloads started: {downloads}.",
                &[
                    ("ready", super::chrome::yes_no(report.deployment_ready)),
                    ("changed", super::chrome::yes_no(report.active_deployment_changed)),
                    ("downloads", super::chrome::yes_no(report.downloads_started)),
                ],
            ));
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(if plan_response.enabled() {
            plan_response
        } else {
            refresh
        })
    }
}
