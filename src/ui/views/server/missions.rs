use crate::core::server::{
    Arma3Difficulty, Arma3Mission, Arma3MissionSource, ConfigurationOwnership, ConfigurationRole,
    MissionLibraryEntry, MissionOrigin, ServerDefinition,
};
use crate::ui::app::Foxy;
use crate::ui::app::agent_support;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use rfd::FileDialog;

impl Foxy {
    pub(super) fn render_server_missions(&mut self, ui: &mut Ui) {
        let game_space = crate::core::game::spaces::active_game_space();
        let capabilities = crate::core::game::registry()
            .get(&game_space.game_id)
            .map(|module| module.server_capabilities())
            .unwrap_or_default();
        if !capabilities.mission_library {
            return;
        }

        ui.separator();
        ui.heading(tr("Mission library"));
        ui.label(tr("Review mission payloads in the game-space library, then add an exact revision to the server draft cycle."));

        let busy = self.server_view.busy();
        let operator_owned = self.server_view.editor.as_ref().is_some_and(|editor| {
            editor
                .draft
                .configuration_import
                .as_ref()
                .is_some_and(|imported| {
                    imported.files.iter().any(|file| {
                        file.role == ConfigurationRole::Server
                            && file.ownership == ConfigurationOwnership::Operator
                    })
                })
        });
        if operator_owned {
            ui.label(tr("Operator-owned server configuration controls the mission cycle. Library review remains available, but cycle changes are disabled."));
        }
        let mut action = None;
        ui.add_enabled_ui(!busy, |ui| {
            if ui.button(tr("Refresh mission library")).clicked() {
                action = Some(ServerTaskAction::LoadMissionLibrary);
            }
            let label = ui.label(tr("PBO or mission folder path"));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.server_view.mission_import_path)
                        .id(egui::Id::unique((
                            "server.mission-import-path",
                            &game_space.space_id,
                        )))
                        .desired_width(ui.available_width())
                        .hint_text(tr("PBO or mission folder path")),
                )
                .labelled_by(label.id)
                .changed()
            {
                self.server_view.mission_import_plan = None;
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr("Choose mission file")).clicked()
                    && let Some(path) = agent_support::pick_file(|| {
                        FileDialog::new().add_filter(tr("Arma mission PBO"), &["pbo"]).pick_file()
                    })
                {
                    self.server_view.mission_import_path = path.display().to_string();
                    self.server_view.mission_import_plan = None;
                }
                if ui.button(tr("Choose mission folder")).clicked()
                    && let Some(path) = agent_support::pick_folder(|| FileDialog::new().pick_folder())
                {
                    self.server_view.mission_import_path = path.display().to_string();
                    self.server_view.mission_import_plan = None;
                }
                if ui.button(tr("Review mission import")).clicked()
                    && !self.server_view.mission_import_path.trim().is_empty()
                {
                    action = Some(ServerTaskAction::PlanMissionImport(
                        self.server_view.mission_import_path.trim().into(),
                    ));
                }
            });

            ui.collapsing(tr("Acquire an HTTP mission PBO"), |ui| {
                ui.label(tr("Downloads are reviewed before they enter the library. Enter the expected BLAKE3 hash."));
                let url = ui.label(tr("HTTP URL"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_view.mission_acquire_url)
                        .desired_width(ui.available_width()),
                )
                .labelled_by(url.id);
                let template = ui.label(tr("Template.World"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_view.mission_acquire_template)
                        .desired_width(ui.available_width()),
                )
                .labelled_by(template.id);
                let hash = ui.label(tr("Expected BLAKE3"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.server_view.mission_acquire_hash)
                        .desired_width(ui.available_width()),
                )
                .labelled_by(hash.id);
                if ui.button(tr("Review mission download")).clicked()
                    && !self.server_view.mission_acquire_url.trim().is_empty()
                    && !self.server_view.mission_acquire_template.trim().is_empty()
                    && !self.server_view.mission_acquire_hash.trim().is_empty()
                {
                    action = Some(ServerTaskAction::PlanMissionAcquisition {
                        url: self.server_view.mission_acquire_url.trim().to_owned(),
                        template: self.server_view.mission_acquire_template.trim().to_owned(),
                        expected_blake3: self.server_view.mission_acquire_hash.trim().to_owned(),
                    });
                }
            });
        });

        if let Some(action) = action {
            self.start_server_task(action);
        }

        if let Some(snapshot) = self.server_view.mission_library.as_ref() {
            ui.add(
                egui::Label::new(tr_fmt(
                    "Mission library fingerprint: {fingerprint}",
                    &[("fingerprint", snapshot.fingerprint.clone())],
                ))
                .wrap(),
            );
            if snapshot.unrecognized_entries > 0 {
                ui.label(tr_fmt(
                    "Unrecognized library entries: {count}",
                    &[("count", snapshot.unrecognized_entries.to_string())],
                ));
            }
            if snapshot.writes_performed {
                ui.label(tr(
                    "The library read completed with writes reported by the store.",
                ));
            }
            let mut refresh = None;
            let mut add_to_cycle = None;
            egui::ScrollArea::vertical()
                .id_salt("mission-library-entries")
                .max_height(240.0)
                .min_scrolled_height(240.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for entry in &snapshot.entries {
                        ui.push_id(("mission-library", entry.mission_id.as_str()), |ui| {
                            ui.group(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "{} / {}",
                                                entry.mission_id, entry.revision_id
                                            ))
                                            .strong(),
                                        )
                                        .wrap(),
                                    );
                                    ui.label(tr_fmt(
                                        "Template: {template}",
                                        &[("template", entry.payload.template.clone())],
                                    ));
                                    ui.label(tr_fmt(
                                        "World: {world}",
                                        &[("world", entry.payload.world.clone())],
                                    ));
                                });
                                ui.label(tr_fmt(
                                    "Payload bytes: {bytes}",
                                    &[("bytes", entry.payload.payload_bytes.to_string())],
                                ));
                                ui.add(
                                    egui::Label::new(tr_fmt(
                                        "BLAKE3: {hash}",
                                        &[("hash", entry.payload.payload_blake3.clone())],
                                    ))
                                    .wrap(),
                                );
                                if let Some(name) = &entry.payload.metadata.display_name {
                                    ui.label(tr_fmt(
                                        "Mission display name: {name}",
                                        &[("name", name.clone())],
                                    ));
                                }
                                if let Some(author) = &entry.payload.metadata.author {
                                    ui.label(tr_fmt(
                                        "Mission author: {author}",
                                        &[("author", author.clone())],
                                    ));
                                }
                                if !entry.payload.metadata.declared_addons.is_empty() {
                                    ui.label(tr_fmt(
                                        "Declared addons: {addons}",
                                        &[(
                                            "addons",
                                            entry.payload.metadata.declared_addons.join(", "),
                                        )],
                                    ));
                                }
                                if !entry.payload.metadata.params.is_empty() {
                                    ui.label(tr_fmt(
                                        "Recognized mission parameters: {parameters}",
                                        &[(
                                            "parameters",
                                            entry
                                                .payload
                                                .metadata
                                                .params
                                                .iter()
                                                .map(|parameter| {
                                                    if parameter.choices.is_empty() {
                                                        parameter.name.clone()
                                                    } else {
                                                        format!(
                                                            "{}: {}",
                                                            parameter.name,
                                                            parameter.choices.join(", ")
                                                        )
                                                    }
                                                })
                                                .collect::<Vec<_>>()
                                                .join(", "),
                                        )],
                                    ));
                                }
                                for warning in &entry.payload.metadata.warnings {
                                    ui.label(tr_fmt(
                                        "Metadata warning: {warning}",
                                        &[("warning", warning.clone())],
                                    ));
                                }
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .add_enabled(
                                            !busy
                                                && !operator_owned
                                                && self.server_view.editor.is_some(),
                                            egui::Button::new(tr("Add revision to mission cycle")),
                                        )
                                        .clicked()
                                    {
                                        add_to_cycle = Some(entry.clone());
                                    }
                                    if ui
                                        .add_enabled(
                                            !busy,
                                            egui::Button::new(tr(
                                                "Review current library revision",
                                            )),
                                        )
                                        .clicked()
                                    {
                                        refresh = Some(entry.mission_id.clone());
                                    }
                                });
                            });
                        });
                    }
                    if snapshot.entries.is_empty() {
                        ui.label(tr("The mission library is empty."));
                    }
                });
            if let Some(entry) = add_to_cycle {
                self.add_library_mission_to_draft(&entry);
            }
            if let Some(mission_id) = refresh {
                self.start_server_task(ServerTaskAction::PlanMissionRefresh(mission_id));
            }
            self.render_mission_rebind_controls(ui);
        } else {
            ui.label(tr("Refresh the mission library to review its entries."));
        }

        self.render_mission_import_review(ui);
    }

    fn add_library_mission_to_draft(&mut self, entry: &MissionLibraryEntry) {
        let Some(editor) = self.server_view.editor.as_mut() else {
            return;
        };
        let ServerDefinition::Arma3(settings) = &mut editor.draft.definition else {
            return;
        };
        settings.mission_cycle.push(Arma3Mission {
            template: entry.payload.template.clone(),
            difficulty: Arma3Difficulty::Regular,
            params: Default::default(),
            enabled: true,
            source: Some(Arma3MissionSource::Library {
                mission_id: entry.mission_id.clone(),
                revision_id: entry.revision_id.clone(),
            }),
        });
        editor.mission_keys.push(editor.next_row_key);
        editor.next_row_key += 1;
    }

    fn render_mission_rebind_controls(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.server_view.mission_library.as_ref() else {
            return;
        };
        let operator_owned = self.server_view.editor.as_ref().is_some_and(|editor| {
            editor
                .draft
                .configuration_import
                .as_ref()
                .is_some_and(|imported| {
                    imported.files.iter().any(|file| {
                        file.role == ConfigurationRole::Server
                            && file.ownership == ConfigurationOwnership::Operator
                    })
                })
        });
        let busy = self.server_view.busy();
        let mut rebind = None;
        if let Some(editor) = self.server_view.editor.as_ref()
            && let ServerDefinition::Arma3(settings) = &editor.draft.definition
        {
            for (index, mission) in settings.mission_cycle.iter().enumerate() {
                let Some(Arma3MissionSource::Library {
                    mission_id,
                    revision_id,
                }) = &mission.source
                else {
                    continue;
                };
                let Some(latest) = snapshot
                    .entries
                    .iter()
                    .find(|entry| &entry.mission_id == mission_id)
                else {
                    continue;
                };
                if &latest.revision_id != revision_id {
                    let key = editor
                        .mission_keys
                        .get(index)
                        .copied()
                        .unwrap_or(index as u64);
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::Label::new(tr_fmt(
                                "A newer library revision is available for {mission}.",
                                &[("mission", mission_id.clone())],
                            ))
                            .wrap(),
                        );
                        if ui
                            .add_enabled(
                                !busy && !operator_owned,
                                egui::Button::new(tr("Rebind to current revision")),
                            )
                            .clicked()
                        {
                            rebind = Some((
                                key,
                                latest.mission_id.clone(),
                                latest.revision_id.clone(),
                                latest.payload.template.clone(),
                            ));
                        }
                    });
                }
            }
        }
        if let Some((key, mission_id, revision_id, template)) = rebind
            && let Some(editor) = self.server_view.editor.as_mut()
            && let ServerDefinition::Arma3(settings) = &mut editor.draft.definition
            && let Some(index) = editor
                .mission_keys
                .iter()
                .position(|candidate| *candidate == key)
            && let Some(mission) = settings.mission_cycle.get_mut(index)
        {
            mission.template = template;
            mission.source = Some(Arma3MissionSource::Library {
                mission_id,
                revision_id,
            });
        }
    }

    fn render_mission_import_review(&mut self, ui: &mut Ui) {
        let Some(plan) = self.server_view.mission_import_plan.as_ref() else {
            return;
        };
        let entry = &plan.entry;
        ui.separator();
        ui.heading(tr("Review mission payload"));
        ui.add(
            egui::Label::new(
                egui::RichText::new(tr_fmt(
                    "Review fingerprint: {fingerprint}",
                    &[("fingerprint", plan.fingerprint.clone())],
                ))
                .monospace(),
            )
            .wrap(),
        );
        ui.label(tr_fmt(
            "Template: {template}",
            &[("template", entry.payload.template.clone())],
        ));
        ui.label(tr_fmt(
            "World: {world}",
            &[("world", entry.payload.world.clone())],
        ));
        ui.label(tr_fmt(
            "Payload bytes: {bytes}",
            &[("bytes", entry.payload.payload_bytes.to_string())],
        ));
        ui.label(tr_fmt(
            "Payload BLAKE3: {hash}",
            &[("hash", entry.payload.payload_blake3.clone())],
        ));
        match &entry.origin {
            MissionOrigin::Local { path } => {
                ui.add(
                    egui::Label::new(tr_fmt(
                        "Local source: {path}",
                        &[("path", path.display().to_string())],
                    ))
                    .wrap(),
                );
            }
            MissionOrigin::Http {
                url,
                template,
                expected_blake3,
            } => {
                ui.add(
                    egui::Label::new(tr_fmt("HTTP source: {url}", &[("url", url.clone())])).wrap(),
                );
                ui.add(
                    egui::Label::new(tr_fmt(
                        "Requested template: {template}",
                        &[("template", template.clone())],
                    ))
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(tr_fmt(
                        "Expected BLAKE3: {hash}",
                        &[("hash", expected_blake3.clone())],
                    ))
                    .wrap(),
                );
            }
        };
        if let Some(name) = &entry.payload.metadata.display_name {
            ui.label(tr_fmt(
                "Mission display name: {name}",
                &[("name", name.clone())],
            ));
        } else {
            ui.label(tr("Mission display name is unknown."));
        }
        if let Some(author) = &entry.payload.metadata.author {
            ui.label(tr_fmt(
                "Mission author: {author}",
                &[("author", author.clone())],
            ));
        } else {
            ui.label(tr("Mission author is unknown."));
        }
        if entry.payload.metadata.declared_addons.is_empty() {
            ui.label(tr("Declared addons are unknown or none were recognized."));
        } else {
            ui.label(tr_fmt(
                "Declared addons: {addons}",
                &[("addons", entry.payload.metadata.declared_addons.join(", "))],
            ));
        }
        if entry.payload.metadata.params.is_empty() {
            ui.label(tr(
                "Mission parameter definitions are unknown or none were recognized.",
            ));
        } else {
            ui.label(tr_fmt(
                "Recognized mission parameters: {parameters}",
                &[(
                    "parameters",
                    entry
                        .payload
                        .metadata
                        .params
                        .iter()
                        .map(|parameter| {
                            if parameter.choices.is_empty() {
                                parameter.name.clone()
                            } else {
                                format!("{}: {}", parameter.name, parameter.choices.join(", "))
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                )],
            ));
        }
        for warning in &entry.payload.metadata.warnings {
            ui.label(tr_fmt(
                "Metadata warning: {warning}",
                &[("warning", warning.clone())],
            ));
        }
        if entry.payload.metadata.warnings.is_empty() {
            ui.label(tr("No metadata warnings were reported."));
        }
        let mut apply = false;
        let mut cancel = false;
        ui.add_enabled_ui(!self.server_view.busy(), |ui| {
            ui.horizontal_wrapped(|ui| {
                apply = ui.button(tr("Apply reviewed mission to library")).clicked();
                cancel = ui.button(tr("Cancel mission review")).clicked();
            });
        });
        if cancel {
            self.server_view.mission_import_plan = None;
        } else if apply && let Some(plan) = self.server_view.mission_import_plan.take() {
            self.start_server_task(ServerTaskAction::ApplyMissionImport(plan));
        }
    }
}
