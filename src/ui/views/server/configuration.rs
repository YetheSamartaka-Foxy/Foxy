use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::Ui;

impl Foxy {
    pub(super) fn render_server_configuration(
        &mut self,
        ui: &mut Ui,
    ) -> Option<eframe::egui::Response> {
        let Some(editor) = &self.server_view.editor else {
            return None;
        };
        let id = editor.baseline.id.clone();
        let revision = editor.baseline.revision;
        let dirty = editor.dirty();
        let capabilities = crate::core::game::registry()
            .get(&crate::core::game::spaces::active_game_space().game_id)
            .map(|module| module.server_capabilities())
            .unwrap_or_default();
        if !capabilities.configuration_preview && !capabilities.configuration_snapshots {
            return None;
        }
        if self
            .server_view
            .configuration_preview
            .as_ref()
            .is_some_and(|preview| {
                dirty || preview.instance_id != id || preview.instance_revision != revision
            })
        {
            self.server_view.configuration_preview = None;
            self.server_view.configuration_plan = None;
        }
        ui.heading(tr("Saved configuration revisions"));
        if dirty {
            self.server_view.configuration_plan = None;
            self.server_view.configuration_rollback = None;
        }
        let mut action = None;
        let mut focus_anchor = None;
        ui.label(tr("Configuration snapshots are separate from runtime deployments. Saving or selecting one does not launch a server or change game files."));
        ui.add_enabled_ui(!dirty, |ui| {
            ui.horizontal_wrapped(|ui| {
                if capabilities.configuration_preview {
                    let response = ui.button(tr("Preview configuration"));
                    if response.clicked() {
                        action = Some(ServerTaskAction::Render(id.clone()));
                    }
                    focus_anchor = Some(response);
                }
                if capabilities.configuration_snapshots {
                    let response = ui.button(tr("Prepare configuration commit"));
                    if response.clicked() {
                        action = Some(ServerTaskAction::PlanConfiguration(id.clone()));
                    }
                    if focus_anchor.is_none() {
                        focus_anchor = Some(response);
                    }
                }
                if capabilities.configuration_snapshots
                    && ui.button(tr("Refresh configuration history")).clicked()
                {
                    action = Some(ServerTaskAction::ListConfigurations(id.clone()));
                }
            });
            if let Some(plan) = &self.server_view.configuration_plan {
                ui.label(tr_fmt(
                    "Configuration archive: {path}",
                    &[("path", plan.destination_directory().display().to_string())],
                ));
                ui.label(tr_fmt(
                    "Expected configuration generation: {generation}",
                    &[("generation", plan.expected_generation.to_string())],
                ));
                if ui
                    .button(tr("Commit reviewed configuration snapshot"))
                    .clicked()
                {
                    action = Some(ServerTaskAction::CommitConfiguration(plan.clone()));
                }
            }
            if let Some(state) = self.server_view.configuration_revisions.clone() {
                ui.label(tr_fmt(
                    "Configuration history generation: {generation}",
                    &[("generation", state.generation.to_string())],
                ));
                let label = |index: usize| {
                    tr_fmt(
                        "Configuration revision {number}",
                        &[("number", (index + 1).to_string())],
                    )
                };
                let selected = self
                    .server_view
                    .configuration_choice
                    .as_ref()
                    .and_then(|choice| {
                        state
                            .revisions
                            .iter()
                            .position(|revision| revision == choice)
                    });
                let selector =
                    eframe::egui::ComboBox::from_id_salt(("configuration-revision", &id))
                        .selected_text(
                            selected
                                .map(label)
                                .unwrap_or_else(|| tr("No configuration revision")),
                        )
                        .show_ui(ui, |ui| {
                            for (index, revision) in state.revisions.iter().enumerate() {
                                ui.selectable_value(
                                    &mut self.server_view.configuration_choice,
                                    Some(revision.clone()),
                                    label(index),
                                )
                                .on_hover_text(revision);
                            }
                        });
                let selector_response = selector
                    .response
                    .accessible_name(tr("Selected configuration revision"));
                if selector_response.has_focus() && !state.revisions.is_empty() {
                    let offset = ui.input_mut(|input| {
                        if input
                            .consume_key(eframe::egui::Modifiers::NONE, eframe::egui::Key::ArrowUp)
                        {
                            -1
                        } else if input.consume_key(
                            eframe::egui::Modifiers::NONE,
                            eframe::egui::Key::ArrowDown,
                        ) {
                            1
                        } else {
                            0
                        }
                    });
                    if offset != 0 {
                        let next = (selected.unwrap_or(0) as isize + offset)
                            .clamp(0, state.revisions.len() as isize - 1)
                            as usize;
                        self.server_view.configuration_choice = Some(state.revisions[next].clone());
                        ui.memory_mut(|memory| {
                            memory.move_focus(eframe::egui::FocusDirection::None)
                        });
                        selector_response.request_focus();
                    }
                }
                if let Some(revision) = self.server_view.configuration_choice.clone() {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(tr("Verify selected configuration")).clicked() {
                            action = Some(ServerTaskAction::VerifyConfiguration {
                                id: id.clone(),
                                revision: revision.clone(),
                            });
                        }
                        if capabilities.configuration_rollback
                            && ui.button(tr("Review configuration rollback")).clicked()
                        {
                            action = Some(ServerTaskAction::PlanConfigurationRollback {
                                id: id.clone(),
                                revision,
                            });
                        }
                    });
                }
                if let Some(active) = state.active {
                    ui.label(tr_fmt(
                        "Selected configuration: {revision}",
                        &[("revision", active)],
                    ));
                }
            }
        });
        if let Some(report) = &self.server_view.configuration_verification {
            ui.label(tr(if report.valid {
                "Configuration snapshot is intact. Launch readiness remains unverified."
            } else {
                "Configuration snapshot failed verification. Rollback to it is blocked."
            }));
            ui.label(&report.revision_id);
            for issue in &report.issues {
                ui.label(tr(issue));
            }
        }
        if let Some(preview) = &self.server_view.configuration_preview {
            for warning in &preview.warnings {
                ui.label(tr(warning));
            }
            for file in &preview.files {
                ui.collapsing(&file.relative_path, |ui| {
                    ui.monospace(&file.contents);
                });
            }
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        focus_anchor
    }
}
