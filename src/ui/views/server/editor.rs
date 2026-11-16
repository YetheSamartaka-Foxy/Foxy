mod arma3;
mod reforger;

use crate::core::server::{DraftChange, ServerDefinition};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerDialog;
use crate::ui::i18n::tr;
use eframe::egui::{self, Ui};

impl Foxy {
    pub(crate) fn clear_server_edit_buffers(&self) {
        if let Some(ctx) = &self.repaint_ctx {
            arma3::clear_edit_buffers(ctx);
        }
    }
    pub(super) fn render_server_editor(&mut self, ui: &mut Ui) {
        let Some(mut editor) = self.server_view.editor.take() else {
            ui.label(self.t("Select a server draft to edit its configuration."));
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.heading(&editor.draft.display_name);
            ui.label(editor.draft.id.as_str());
            ui.label(self.t_fmt(
                "Revision: {revision}",
                &[("revision", editor.draft.revision.to_string())],
            ));
        });
        ui.label(self.t("Draft validation does not establish deployment or launch readiness."));
        let invalid = arma3::has_invalid_edits(ui.ctx(), editor.field_scope());
        let dirty = editor.dirty() || invalid;
        let mut change = None;
        let mut discard = false;
        let mut dialog = None;
        if invalid {
            ui.colored_label(
                ui.visuals().error_fg_color,
                self.t("Correct the highlighted fields before saving. Invalid text is not saved."),
            );
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(dirty && !invalid, egui::Button::new(self.t("Save draft")))
                .clicked()
            {
                change = Some(DraftChange::Replace {
                    instance: editor.draft.clone(),
                    expected_revision: editor.baseline.revision,
                });
            }
            if ui
                .add_enabled(dirty, egui::Button::new(self.t("Discard changes")))
                .clicked()
            {
                discard = true;
            }
            ui.add_enabled_ui(!dirty, |ui| {
                for (label, action) in [
                    ("Rename draft", ServerDialog::Rename),
                    ("Clone draft", ServerDialog::Clone),
                    ("Remove draft", ServerDialog::Remove),
                ] {
                    if ui.button(self.t(label)).clicked() {
                        dialog = Some(action);
                    }
                }
            });
        });
        let field_scope = editor.field_scope();
        let headless_clients = crate::core::game::registry()
            .get(&crate::core::game::spaces::active_game_space().game_id)
            .is_some_and(|module| module.server_capabilities().headless_clients);
        let operator_owned = editor
            .draft
            .configuration_import
            .as_ref()
            .is_some_and(|imported| !imported.files.is_empty());
        if operator_owned {
            ui.label(tr("Operator-owned files are authoritative. Import a replacement or explicitly convert them to managed configuration to edit their fields."));
            if let ServerDefinition::Arma3(settings) = &mut editor.draft.definition {
                number(ui, "Game port", &mut settings.game_port, u16::MAX);
                ui.checkbox(
                    &mut settings.auto_init,
                    tr("Initialize mission automatically"),
                );
            }
        }
        ui.add_enabled_ui(!operator_owned, |ui| {
            ui.push_id(field_scope, |ui| {
                match &mut editor.draft.definition {
                    ServerDefinition::Arma3(settings) => {
                        arma3::render(
                            ui,
                            settings,
                            &mut editor.mission_keys,
                            &mut editor.next_row_key,
                            field_scope,
                            headless_clients,
                        );
                        ui.label(tr(
                            "Use configuration import for operator files and protected password binding.",
                        ));
                    }
                    ServerDefinition::Reforger(settings) => reforger::render(
                        ui,
                        settings,
                        &mut editor.addon_keys,
                        &mut editor.next_row_key,
                        field_scope,
                    ),
                }
            });
        });
        self.server_view.editor = Some(editor);
        if discard {
            arma3::clear_edit_buffers(ui.ctx());
            self.server_view.discard();
        }
        if let Some(dialog) = dialog {
            self.open_server_draft_dialog(dialog);
        }
        if let Some(change) = change {
            self.start_server_draft_task(Some(change));
        }
    }
}

fn text(ui: &mut Ui, key: &str, id: egui::Id, value: &mut String) {
    ui.horizontal_wrapped(|ui| {
        ui.label(tr(key));
        ui.add(
            egui::TextEdit::singleline(value)
                .id(id)
                .desired_width(300.0),
        );
    });
}

fn number(ui: &mut Ui, key: &str, value: &mut u16, max: u16) {
    ui.horizontal_wrapped(|ui| {
        ui.label(tr(key));
        ui.add(egui::DragValue::new(value).range(1..=max));
    });
}
