use crate::core::server::{ConfigurationImportInput, ConfigurationOwnership, ConfigurationRole};
use crate::ui::app::Foxy;
use crate::ui::app::agent_support;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, Ui};
use rfd::FileDialog;

impl Foxy {
    pub(super) fn render_server_import_export(&mut self, ui: &mut Ui) {
        let Some(editor) = self.server_view.editor.as_ref() else {
            return;
        };
        let id = editor.baseline.id.clone();
        let revision = editor.baseline.revision;
        let game = crate::core::game::spaces::active_game_space();
        let capabilities = crate::core::game::registry()
            .get(&game.game_id)
            .map(|module| module.server_capabilities())
            .unwrap_or_default();
        if !capabilities.configuration_import && !capabilities.instance_packs {
            return;
        }
        ui.separator();
        ui.heading(tr("Configuration import and instance packs"));
        let mut inputs = Vec::new();
        if capabilities.configuration_import {
            ui.label(tr("Import full server, basic, or profile files from disk. Secret values are masked before review."));
            for (label, role, path, managed) in [
                (
                    "Server configuration file",
                    ConfigurationRole::Server,
                    &mut self.server_view.import_server_path,
                    &mut self.server_view.import_managed_server,
                ),
                (
                    "Basic configuration file",
                    ConfigurationRole::Basic,
                    &mut self.server_view.import_basic_path,
                    &mut self.server_view.import_managed_basic,
                ),
                (
                    "Profile configuration file",
                    ConfigurationRole::Profile,
                    &mut self.server_view.import_profile_path,
                    &mut self.server_view.import_managed_profile,
                ),
            ] {
                ui.horizontal_wrapped(|ui| {
                    let caption = ui.label(tr(label));
                    ui.add(egui::TextEdit::singleline(path).desired_width(280.0))
                        .labelled_by(caption.id);
                    if ui.button(tr("Choose file")).clicked()
                        && let Some(selected) =
                            agent_support::pick_file(|| FileDialog::new().pick_file())
                    {
                        *path = selected.display().to_string();
                    }
                    ui.checkbox(
                        managed,
                        tr("Convert supported fields to managed configuration"),
                    );
                });
                if !path.trim().is_empty() {
                    inputs.push(ConfigurationImportInput {
                        role,
                        ownership: if *managed {
                            ConfigurationOwnership::Managed
                        } else {
                            ConfigurationOwnership::Operator
                        },
                        path: path.trim().into(),
                    });
                }
            }
            if ui
                .add_enabled(
                    !inputs.is_empty() && !self.server_view.controls_busy(),
                    egui::Button::new(tr("Review configuration import")),
                )
                .clicked()
            {
                self.start_server_task(ServerTaskAction::PlanConfigurationImport {
                    id: id.clone(),
                    expected_revision: revision,
                    inputs,
                });
            }
            let mut apply_configuration = false;
            let mut cancel_configuration = false;
            if let Some(plan) = self.server_view.configuration_import_plan.as_mut() {
                ui.label(tr("Review masked changes before applying them."));
                ui.monospace(&plan.report.fingerprint);
                for line in plan
                    .report
                    .diff
                    .iter()
                    .chain(plan.report.warnings.iter())
                    .chain(plan.report.unresolved_paths.iter())
                    .chain(plan.report.secret_references.iter())
                {
                    ui.label(line);
                }
                if !plan.report.unresolved_paths.is_empty() {
                    ui.checkbox(
                        &mut self.server_view.import_paths_reviewed,
                        tr("I reviewed the unresolved paths and accept them."),
                    );
                    plan.set_paths_reviewed(self.server_view.import_paths_reviewed);
                }
                ui.horizontal(|ui| {
                    apply_configuration = ui
                        .add_enabled(
                            plan.report.unresolved_paths.is_empty()
                                || self.server_view.import_paths_reviewed,
                            egui::Button::new(tr("Apply reviewed configuration import")),
                        )
                        .clicked();
                    cancel_configuration = ui.button(tr("Cancel import review")).clicked();
                });
            }
            if cancel_configuration {
                self.server_view.configuration_import_plan = None;
            }
            if apply_configuration
                && let Some(plan) = self.server_view.configuration_import_plan.take()
            {
                self.start_server_task(ServerTaskAction::ApplyConfigurationImport(plan));
            }
        }

        if capabilities.instance_packs {
            ui.separator();
            ui.checkbox(&mut self.server_view.export_pack_sanitized, tr("Sanitize export: remove operator text and policy identifiers; the pack will be incomplete."));
            if ui.button(tr("Export instance pack")).clicked()
                && let Some(path) = agent_support::save_file(|| {
                    FileDialog::new()
                        .add_filter(tr("Foxy server instance pack"), &["foxyserver"])
                        .save_file()
                })
            {
                self.start_server_task(ServerTaskAction::ExportInstancePack {
                    id: id.clone(),
                    path,
                    sanitized: self.server_view.export_pack_sanitized,
                });
            }
        }
        self.render_reforger_configuration(ui);
    }

    pub(super) fn render_server_instance_pack_import(&mut self, ui: &mut Ui) {
        let game = crate::core::game::spaces::active_game_space();
        let capabilities = crate::core::game::registry()
            .get(&game.game_id)
            .map(|module| module.server_capabilities())
            .unwrap_or_default();
        if !capabilities.instance_packs {
            return;
        }

        ui.separator();
        ui.heading(tr("Foxy server instance pack"));
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.import_pack_path)
                    .desired_width(240.0),
            )
            .accessible_name(tr("Instance pack file"));
            if ui.button(tr("Choose pack file")).clicked()
                && let Some(path) = agent_support::pick_file(|| {
                    FileDialog::new()
                        .add_filter(tr("Foxy server instance pack"), &["foxyserver"])
                        .pick_file()
                })
            {
                self.server_view.import_pack_path = path.display().to_string();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.import_pack_name)
                    .desired_width(180.0)
                    .hint_text(tr("New draft name")),
            )
            .accessible_name(tr("New draft name"));
            if ui
                .add_enabled(
                    !self.server_view.import_pack_path.is_empty()
                        && !self.server_view.import_pack_name.trim().is_empty(),
                    egui::Button::new(tr("Review pack import")),
                )
                .clicked()
            {
                self.start_server_task(ServerTaskAction::PlanInstancePackImport {
                    pack_path: self.server_view.import_pack_path.clone().into(),
                    name: self.server_view.import_pack_name.trim().to_owned(),
                });
            }
        });
        let mut apply_pack = false;
        let mut cancel_pack = false;
        if let Some(plan) = self.server_view.instance_pack_import_plan.as_mut() {
            ui.label(tr("Review pack contents and unresolved paths. The new draft has no installation binding."));
            ui.monospace(&plan.report.fingerprint);
            for line in plan
                .report
                .diff
                .iter()
                .chain(plan.report.warnings.iter())
                .chain(plan.report.unresolved_paths.iter())
                .chain(plan.report.secret_references.iter())
            {
                ui.label(line);
            }
            if !plan.report.unresolved_paths.is_empty() {
                ui.checkbox(
                    &mut self.server_view.import_pack_paths_reviewed,
                    tr("I reviewed the unresolved pack paths and accept them."),
                );
                plan.set_paths_reviewed(self.server_view.import_pack_paths_reviewed);
            }
            ui.horizontal(|ui| {
                apply_pack = ui
                    .add_enabled(
                        plan.report.unresolved_paths.is_empty()
                            || self.server_view.import_pack_paths_reviewed,
                        egui::Button::new(tr("Import as new draft")),
                    )
                    .clicked();
                cancel_pack = ui.button(tr("Cancel pack review")).clicked();
            });
        }
        if cancel_pack {
            self.server_view.instance_pack_import_plan = None;
        }
        if apply_pack && let Some(plan) = self.server_view.instance_pack_import_plan.take() {
            self.start_server_task(ServerTaskAction::ApplyInstancePackImport(plan));
        }
    }

    fn render_reforger_configuration(&mut self, ui: &mut Ui) {
        let Some(editor) = self.server_view.editor.as_ref() else {
            return;
        };
        let crate::core::server::ServerDefinition::Reforger(_) = &editor.draft.definition else {
            return;
        };
        let id = editor.draft.id.clone();
        let selected = match &editor.draft.definition {
            crate::core::server::ServerDefinition::Reforger(settings) => {
                settings.scenario_id.clone()
            }
            _ => None,
        };
        ui.separator();
        ui.heading(tr("Reforger server.json"));
        ui.label(tr(
            "Import keeps typed fields and unknown JSON. Secret values are removed. Save the draft after using an import.",
        ));
        ui.horizontal_wrapped(|ui| {
            let import_label = ui.label(tr("Reforger server.json file"));
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.reforger_import_path)
                    .desired_width(280.0),
            )
            .labelled_by(import_label.id);
            if ui.button(tr("Choose file")).clicked()
                && let Some(selected) = agent_support::pick_file(|| {
                    FileDialog::new()
                        .add_filter(tr("JSON"), &["json"])
                        .pick_file()
                })
            {
                self.server_view.reforger_import_path = selected.display().to_string();
            }
        });
        if ui
            .add_enabled(
                !self.server_view.reforger_import_path.trim().is_empty()
                    && !self.server_view.controls_busy(),
                egui::Button::new(tr("Review Reforger import")),
            )
            .clicked()
        {
            self.start_server_task(ServerTaskAction::ReviewReforgerImport(
                self.server_view.reforger_import_path.trim().into(),
            ));
        }
        let mut use_import = false;
        if let Some(review) = self.server_view.reforger_import_review.as_ref() {
            ui.label(tr(
                "Imported configuration replaces the open draft. Save the draft to keep it. Secrets are not stored.",
            ));
            for warning in &review.warnings {
                ui.label(tr(warning));
            }
            for path in review.unknown_paths.iter().take(32) {
                ui.monospace(path);
            }
            use_import = ui
                .button(tr("Use imported Reforger configuration"))
                .clicked();
        }
        if use_import
            && let Some(review) = self.server_view.reforger_import_review.clone()
            && let Some(editor) = self.server_view.editor.as_mut()
            && let crate::core::server::ServerDefinition::Reforger(settings) =
                &mut editor.draft.definition
        {
            let count = review.definition.addons.len() as u64;
            editor.addon_keys = (0..count).collect();
            editor.next_row_key = count;
            **settings = review.definition;
        }
        ui.horizontal_wrapped(|ui| {
            let export_label = ui.label(tr("Export saved server.json"));
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.reforger_export_path)
                    .desired_width(280.0),
            )
            .labelled_by(export_label.id);
            if ui.button(tr("Choose export file")).clicked()
                && let Some(selected) = agent_support::pick_file(|| {
                    FileDialog::new()
                        .add_filter(tr("JSON"), &["json"])
                        .set_file_name("server.json")
                        .save_file()
                })
            {
                self.server_view.reforger_export_path = selected.display().to_string();
            }
        });
        ui.label(tr(
            "Export writes the saved draft, including preserved fields, without secret values.",
        ));
        if ui
            .add_enabled(
                !self.server_view.reforger_export_path.trim().is_empty()
                    && !self.server_view.controls_busy(),
                egui::Button::new(tr("Export saved server.json")),
            )
            .clicked()
        {
            self.start_server_task(ServerTaskAction::ExportReforgerJson {
                id: id.clone(),
                path: self.server_view.reforger_export_path.trim().into(),
            });
        }
        ui.separator();
        ui.heading(tr("Scenario listing"));
        ui.label(tr(
            "A failed listing is not stored and does not change the selected scenario.",
        ));
        ui.horizontal_wrapped(|ui| {
            let log_label = ui.label(tr("Scenario listing log"));
            ui.add(
                egui::TextEdit::singleline(&mut self.server_view.scenario_log_path)
                    .desired_width(280.0),
            )
            .labelled_by(log_label.id);
            if ui.button(tr("Choose listing log")).clicked()
                && let Some(selected) = agent_support::pick_file(|| FileDialog::new().pick_file())
            {
                self.server_view.scenario_log_path = selected.display().to_string();
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.server_view.scenario_log_path.trim().is_empty()
                        && !self.server_view.controls_busy(),
                    egui::Button::new(tr("Store scenario listing")),
                )
                .clicked()
            {
                self.start_server_task(ServerTaskAction::ImportScenarioLog {
                    id: id.clone(),
                    path: self.server_view.scenario_log_path.trim().into(),
                });
            }
            if ui
                .add_enabled(
                    !self.server_view.controls_busy(),
                    egui::Button::new(tr("Refresh stored scenario listing")),
                )
                .clicked()
            {
                self.start_server_task(ServerTaskAction::LoadScenarioDiscovery(id));
            }
        });
        let note = crate::core::server::scenario_listing_note(
            selected.as_deref(),
            self.server_view.scenario_discovery.as_deref(),
        );
        ui.label(tr(note));
        if let Some(discovery) = self.server_view.scenario_discovery.clone() {
            ui.label(tr_fmt(
                "Stored listing has {count} official scenarios.",
                &[("count", discovery.scenarios.len().to_string())],
            ));
            let mut chosen = None;
            for scenario in discovery.scenarios.iter().take(64) {
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(&scenario.resource);
                    if let Some(label) = &scenario.label {
                        ui.label(label);
                    }
                    if ui.button(tr("Use scenario resource")).clicked() {
                        chosen = Some(scenario.resource.clone());
                    }
                });
            }
            if discovery.scenarios.len() > 64 {
                ui.label(tr("Showing the first 64 stored scenarios."));
            }
            if let Some(resource) = chosen
                && let Some(editor) = self.server_view.editor.as_mut()
                && let crate::core::server::ServerDefinition::Reforger(settings) =
                    &mut editor.draft.definition
            {
                settings.scenario_id = Some(resource);
            }
        }
    }
}
