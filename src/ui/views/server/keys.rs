use crate::core::server::{ServerKeyCandidate, ServerKeyOrigin, TrustedServerKey};
use crate::ui::app::Foxy;
use crate::ui::app::agent_support;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use rfd::FileDialog;

impl Foxy {
    pub(super) fn render_server_keys(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .key_trust
        {
            return;
        }
        let Some(editor) = self.server_view.editor.as_ref() else {
            return;
        };
        let instance_id = editor.baseline.id.clone();
        let dirty = editor.dirty();
        let busy = self.server_view.busy();

        ui.separator();
        ui.heading(tr("Server signature keys"));
        ui.label(tr("Review public BIKEY candidates from the installation, selected content stage, and operator files. Trust changes are saved only after explicit review and apply."));
        if dirty {
            ui.label(tr("Save the server draft before reviewing trusted keys."));
        }

        ui.label(tr("Selected content stage"));
        let stage_ids = self
            .server_view
            .content_stage_list
            .as_ref()
            .map(|list| list.stage_ids.clone())
            .unwrap_or_default();
        let selected_stage = self.server_view.content_stage_selection.clone();
        ui.add_enabled_ui(!busy, |ui| {
            egui::ComboBox::from_id_salt(("server-key-stage", instance_id.clone()))
                .selected_text(
                    selected_stage
                        .as_deref()
                        .map(str::to_owned)
                        .unwrap_or_else(|| tr("No stage selected")),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.server_view.content_stage_selection,
                        None,
                        tr("No stage selected"),
                    );
                    for stage_id in &stage_ids {
                        ui.selectable_value(
                            &mut self.server_view.content_stage_selection,
                            Some(stage_id.clone()),
                            stage_id,
                        );
                    }
                });
        });
        if self.server_view.content_stage_selection != selected_stage {
            self.server_view.key_review = None;
            self.server_view.key_selection.clear();
        }
        ui.label(tr("Only the stage selected here is included. Foxy does not choose the latest stage automatically."));

        ui.label(tr("Operator public key files"));
        let mut remove_operator = None;
        for (index, path) in self.server_view.key_operator_files.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                ui.add(egui::Label::new(name.to_owned()).wrap());
                if ui
                    .add_enabled(!busy, egui::Button::new(tr("Remove from review")))
                    .clicked()
                {
                    remove_operator = Some(index);
                }
            });
        }
        if let Some(index) = remove_operator {
            self.server_view.key_operator_files.remove(index);
            self.server_view.key_review = None;
            self.server_view.key_selection.clear();
        }
        if ui
            .add_enabled(!busy, egui::Button::new(tr("Add operator BIKEY file")))
            .clicked()
            && let Some(path) = agent_support::pick_file(|| {
                FileDialog::new()
                    .add_filter(tr("Arma public key file"), &["bikey"])
                    .pick_file()
            })
            && !self.server_view.key_operator_files.contains(&path)
        {
            self.server_view.key_operator_files.push(path);
            self.server_view.key_review = None;
            self.server_view.key_selection.clear();
        }

        let can_review = !busy && !dirty;
        let review_button = ui.add_enabled(
            can_review,
            egui::Button::new(tr("Review server signature keys")),
        );
        self.server_view.key_controls_focus = Some(review_button.id);
        if review_button.clicked() {
            self.server_view.key_review = None;
            self.server_view.key_selection.clear();
            self.start_server_task(ServerTaskAction::ReviewServerKeys {
                id: instance_id.clone(),
                stage_id: self.server_view.content_stage_selection.clone(),
                operator_files: self.server_view.key_operator_files.clone(),
            });
        }

        if let Some(review) = self.server_view.key_review.as_ref() {
            ui.separator();
            ui.heading(tr("Review trusted server keys"));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(tr_fmt(
                        "Review fingerprint: {fingerprint}",
                        &[("fingerprint", review.fingerprint.clone())],
                    ))
                    .monospace(),
                )
                .wrap(),
            );
            ui.label(tr_fmt(
                "Signature verification setting: {setting}",
                &[("setting", review.signature_verification.clone())],
            ));
            ui.label(tr("Cryptographic signature verification is not performed by this review. The game engine verifies signatures at runtime."));
            if review.cryptographic_verification {
                ui.label(tr("The review reports cryptographic verification."));
            }
            if let Some(stage_id) = &review.stage_id {
                ui.label(tr_fmt(
                    "Reviewed content stage: {stage}",
                    &[("stage", stage_id.clone())],
                ));
            } else {
                ui.label(tr("No content stage was included in this review."));
            }
            for issue in &review.issues {
                ui.add(
                    egui::Label::new(tr_fmt("Review issue: {issue}", &[("issue", issue.clone())]))
                        .wrap(),
                );
            }
            if review.writes_performed {
                ui.label(tr("The review reports writes performed."));
            }
            if review.candidates.is_empty() {
                ui.label(tr("No public key candidates were found."));
            }

            egui::ScrollArea::vertical()
                .id_salt(("server-key-candidates", review.instance_id.clone()))
                .max_height(320.0)
                .min_scrolled_height(160.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for candidate in &review.candidates {
                        ui.push_id(("server-key-candidate", candidate.id.as_str()), |ui| {
                            ui.group(|ui| {
                                render_candidate_details(ui, candidate);
                                if candidate.effective_base {
                                    ui.label(tr("This key is part of the effective tested base."));
                                }
                                if candidate.implicit_tested_base {
                                    let mut retained = true;
                                    ui.add_enabled(
                                        false,
                                        egui::Checkbox::new(
                                            &mut retained,
                                            tr("Required tested base key"),
                                        ),
                                    );
                                } else {
                                    let mut selected =
                                        self.server_view.key_selection.contains(&candidate.id);
                                    if ui
                                        .add_enabled(
                                            !busy,
                                            egui::Checkbox::new(
                                                &mut selected,
                                                tr("Trust this reviewed key"),
                                            ),
                                        )
                                        .changed()
                                    {
                                        if selected {
                                            self.server_view
                                                .key_selection
                                                .insert(candidate.id.clone());
                                        } else {
                                            self.server_view.key_selection.remove(&candidate.id);
                                        }
                                    }
                                }
                            });
                        });
                    }
                });
            let selected =
                selected_candidate_ids(&review.candidates, &self.server_view.key_selection);
            let review_is_current = review.instance_id == instance_id;
            let apply = ui.add_enabled(
                !busy && !dirty && review_is_current,
                egui::Button::new(tr("Apply reviewed key trust")),
            );
            if apply.clicked()
                && let Some(review) = self.server_view.key_review.take()
            {
                self.start_server_task(ServerTaskAction::ApplyServerKeys { review, selected });
            }
            if ui
                .add_enabled(!busy, egui::Button::new(tr("Cancel key review")))
                .clicked()
            {
                self.server_view.key_review = None;
                self.server_view.key_selection.clear();
            }
        } else if let Some(policy) =
            self.server_view
                .editor
                .as_ref()
                .and_then(|editor| match &editor.draft.definition {
                    crate::core::server::ServerDefinition::Arma3(settings) => {
                        settings.key_trust.as_deref()
                    }
                    _ => None,
                })
        {
            ui.separator();
            ui.label(tr("Saved trusted server keys"));
            if policy.instance_id != instance_id {
                ui.label(tr("Saved key trust belongs to another instance. Review and apply trust for this instance before deployment."));
            }
            for key in &policy.keys {
                render_trusted_key(ui, key);
            }
        }
    }
}

fn render_candidate_details(ui: &mut Ui, candidate: &ServerKeyCandidate) {
    render_trusted_key(ui, &candidate.key);
    ui.label(tr_fmt(
        "Candidate status: {status}",
        &[(
            "status",
            if candidate.trusted {
                tr("Currently trusted").to_owned()
            } else {
                tr("Not currently trusted").to_owned()
            },
        )],
    ));
}

fn render_trusted_key(ui: &mut Ui, key: &TrustedServerKey) {
    ui.label(tr_fmt(
        "Public key filename: {name}",
        &[("name", key.name.clone())],
    ));
    ui.label(tr_fmt(
        "Authority: {authority}",
        &[("authority", key.identity.authority.clone())],
    ));
    ui.label(tr_fmt(
        "RSA key size: {bits} bits",
        &[("bits", key.identity.bits.to_string())],
    ));
    ui.add(
        egui::Label::new(tr_fmt(
            "Key fingerprint: {fingerprint}",
            &[("fingerprint", key.identity.fingerprint.clone())],
        ))
        .wrap(),
    );
    ui.add(
        egui::Label::new(tr_fmt(
            "File BLAKE3: {hash}",
            &[("hash", key.blake3.clone())],
        ))
        .wrap(),
    );
    for origin in &key.origins {
        let description = match origin {
            ServerKeyOrigin::Base {
                installation_id,
                path,
            } => tr_fmt(
                "Installation base: {installation} / {path}",
                &[
                    ("installation", installation_id.clone()),
                    ("path", path.clone()),
                ],
            ),
            ServerKeyOrigin::Stage { source_id, path } => tr_fmt(
                "Selected stage source: {source} / {path}",
                &[("source", source_id.clone()), ("path", path.clone())],
            ),
            ServerKeyOrigin::Operator { filename } => tr_fmt(
                "Operator file: {filename}",
                &[("filename", filename.clone())],
            ),
        };
        ui.add(egui::Label::new(description).wrap());
    }
}

fn selected_candidate_ids(
    candidates: &[ServerKeyCandidate],
    selected: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    candidates
        .iter()
        .filter(|candidate| candidate.implicit_tested_base || selected.contains(&candidate.id))
        .map(|candidate| candidate.id.clone())
        .collect()
}
