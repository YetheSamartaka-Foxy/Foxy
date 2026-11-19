use crate::core::server::ContentAcquisitionStatus;
use crate::ui::app::Foxy;
use crate::ui::app::server::{ServerDialog, ServerTaskAction};
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn render_server_content_acquisition(
        &mut self,
        ui: &mut Ui,
    ) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .content_acquisition
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let dirty = editor.dirty();
        let busy = self.server_view.busy();
        ui.separator();
        Some(ui.push_id("server-content-acquisition-controls", |ui| {
            ui.heading(tr("Acquire repository content"));
            ui.label(tr("Download selected repository profiles into a retained content stage. Acquisition does not activate a deployment or change client enablement."));
            ui.label(tr("Already present profiles can be added through the existing local content staging action."));
            ui.label(tr("A plan reviews local intent and works offline. Current remote publication is resolved when acquisition starts, so the preview does not pin remote content."));

            ui.horizontal_wrapped(|ui| {
                let seed_label = ui.label(tr("Seed stage id (optional)"));
                let seed_input = ui
                    .add_enabled(
                        !busy && !dirty,
                        egui::TextEdit::singleline(&mut self.server_view.acquisition_seed)
                            .desired_width(240.0),
                    )
                    .labelled_by(seed_label.id);
                if seed_input.changed() {
                    self.server_view.invalidate_content_acquisition_plan();
                }
            });
            let review = ui.add_enabled(
                !busy && !dirty,
                egui::Button::new(tr("Review repository content acquisition")),
            );
            if review.clicked() {
                let seed = self.server_view.acquisition_seed.trim();
                self.start_server_task(ServerTaskAction::PlanContentAcquisition {
                    id: id.clone(),
                    seed_stage: (!seed.is_empty()).then(|| seed.to_owned()),
                });
            }

            if let Some(plan) = &self.server_view.acquisition_plan {
                ui.label(tr_fmt(
                    "Acquisition plan fingerprint: {fingerprint}",
                    &[("fingerprint", plan.fingerprint.clone())],
                ));
                ui.label(tr_fmt(
                    "Bindings fingerprint: {fingerprint}; seed stage: {seed}",
                    &[
                        ("fingerprint", plan.bindings_fingerprint.clone()),
                        ("seed", plan.seed_stage.clone().unwrap_or_else(|| tr("None"))),
                    ],
                ));
                for profile in &plan.profiles {
                    ui.collapsing(
                        tr_fmt("Download profile: {name}", &[("name", profile.name.clone())]),
                        |ui| {
                            ui.label(tr_fmt(
                                "Repository: {url} at {path}",
                                &[
                                    ("url", profile.repository_url.clone()),
                                    (
                                        "path",
                                        profile.repository_local_path.display().to_string(),
                                    ),
                                ],
                            ));
                            if profile.addons.is_empty() {
                                ui.label(tr("All repository addons"));
                            } else {
                                ui.label(tr("Addons"));
                                for addon in &profile.addons {
                                    ui.label(addon);
                                }
                            }
                        },
                    );
                }
                if plan.profiles.is_empty() {
                    ui.label(tr("No download profiles are included in this plan."));
                }
                if !plan.blockers.is_empty() {
                    ui.label(tr("Acquisition blockers"));
                    for blocker in &plan.blockers {
                        ui.label(blocker);
                    }
                }
                let apply = ui.add_enabled(
                    !busy && !dirty && plan.blockers.is_empty(),
                    egui::Button::new(tr("Acquire reviewed repository content")),
                );
                if apply.clicked() {
                    self.server_view.open_dialog(ServerDialog::AcquireRepositoryContent);
                }
            }

            if let Some(outcome) = &self.server_view.acquisition_outcome {
                ui.label(tr_fmt(
                    "Acquisition result: {status}",
                    &[("status", acquisition_status_text(&outcome.status))],
                ));
                ui.label(outcome.message.as_str());
                for item in &outcome.items {
                    ui.collapsing(
                        tr_fmt(
                            "Profile {profile}: {status}",
                            &[
                                ("profile", item.profile.clone()),
                                ("status", acquisition_status_text(&item.status)),
                            ],
                        ),
                        |ui| {
                            ui.label(tr_fmt(
                                "Planned transfer: {planned} bytes; full download: {full}; patch files planned: {patches}",
                                &[
                                    ("planned", item.planned_transfer_bytes.to_string()),
                                    ("full", item.full_download_bytes.to_string()),
                                    ("patches", item.patch_files_planned.to_string()),
                                ],
                            ));
                            for addon in &item.addons {
                                ui.label(tr_fmt(
                                    "Addon {name}: checksum {checksum}; verified: {verified}",
                                    &[
                                        ("name", addon.name.clone()),
                                        ("checksum", addon.checksum.clone()),
                                        ("verified", addon.verified.to_string()),
                                    ],
                                ));
                            }
                            if let Some(message) = &item.message {
                                ui.label(message);
                            }
                        },
                    );
                }
                if let Some(stage) = &outcome.stage {
                    ui.label(tr_fmt(
                        "Retained stage: {id}; verified: {verified}; files: {files}; bytes: {bytes}",
                        &[
                            ("id", stage.stage_id.clone()),
                            ("verified", stage.verified.to_string()),
                            ("files", stage.files.to_string()),
                            ("bytes", stage.bytes.to_string()),
                        ],
                    ));
                    ui.label(tr_fmt(
                        "Stage directory: {path}",
                        &[("path", stage.directory.display().to_string())],
                    ));
                    ui.label(tr("The retained stage can be recorded through the existing deployment record flow."));
                }
                match &outcome.status {
                    ContentAcquisitionStatus::Complete => {
                        ui.label(tr("Acquisition completed. The stage remains retained and inactive."));
                    }
                    ContentAcquisitionStatus::Failed => {
                        ui.label(tr("Acquisition failed. Verified items may be retained, but the overall result is not successful."));
                    }
                    ContentAcquisitionStatus::Cancelled => {
                        ui.label(tr("Acquisition was cancelled. Verified items may be retained, but the overall result is not successful."));
                    }
                }
            }
            review
        }).inner)
    }
}

fn acquisition_status_text(status: &ContentAcquisitionStatus) -> String {
    tr(match status {
        ContentAcquisitionStatus::Complete => "Complete",
        ContentAcquisitionStatus::Failed => "Failed",
        ContentAcquisitionStatus::Cancelled => "Cancelled",
    })
}
