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
    pub(in crate::ui::views) fn render_server_editor(&mut self, ui: &mut Ui) {
        let Some(mut editor) = self.server_view.editor.take() else {
            self.server_section(ui, |app, ui| {
                app.server_note(
                    ui,
                    app.t("Select a server draft to edit its configuration."),
                );
            });
            return;
        };
        let invalid = arma3::has_invalid_edits(ui.ctx(), editor.field_scope());
        let dirty = editor.dirty() || invalid;
        let mut change = None;
        let mut discard = false;
        let mut dialog = None;
        let card = self.server_card_frame();
        card.show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let save = self.add_sized_primary_button(
                    ui,
                    egui::Vec2::ZERO,
                    egui::Button::new(self.t("Save draft")),
                    dirty && !invalid,
                );
                Self::focus_outline(ui, &save);
                if save.clicked() {
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
                ui.separator();
                ui.add_enabled_ui(!dirty, |ui| {
                    for (label, action) in [
                        ("Rename draft", ServerDialog::Rename),
                        ("Clone draft", ServerDialog::Clone),
                    ] {
                        if ui.button(self.t(label)).clicked() {
                            dialog = Some(action);
                        }
                    }
                    if ui
                        .button(
                            egui::RichText::new(self.t("Remove draft")).color(self.color_error()),
                        )
                        .clicked()
                    {
                        dialog = Some(ServerDialog::Remove);
                    }
                });
            });
            if invalid {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    self.t(
                        "Correct the highlighted fields before saving. Invalid text is not saved.",
                    ),
                );
            }
            self.server_note(
                ui,
                format!(
                    "{}  {}",
                    self.t_fmt(
                        "Revision: {revision}",
                        &[("revision", editor.draft.revision.to_string())],
                    ),
                    self.t("Draft validation does not establish deployment or launch readiness."),
                ),
            )
            .on_hover_text(editor.draft.id.as_str());
        });
        ui.add_space(10.0);
        let field_scope = editor.field_scope();
        let headless_clients = crate::core::game::registry()
            .get(&crate::core::game::spaces::active_game_space().game_id)
            .is_some_and(|module| module.server_capabilities().headless_clients);
        let operator_owned = editor
            .draft
            .configuration_import
            .as_ref()
            .is_some_and(|imported| !imported.files.is_empty());
        let headless_profiles = arma3::HeadlessProfiles {
            registered: self
                .server_view
                .content_sources
                .as_ref()
                .map(|snapshot| &snapshot.sources),
            reviewed: self
                .server_view
                .saved_content_bindings
                .as_ref()
                .filter(|snapshot| snapshot.review_current)
                .and_then(|snapshot| snapshot.document.as_ref()),
        };
        if operator_owned {
            card.show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                self.server_callout(ui, super::chrome::Tone::Info, &tr("Operator-owned files are authoritative. Import a replacement or explicitly convert them to managed configuration to edit their fields."), |_| {});
                if let ServerDefinition::Arma3(settings) = &mut editor.draft.definition {
                    number(ui, "Game port", &mut settings.game_port, u16::MAX);
                    ui.checkbox(
                        &mut settings.auto_init,
                        tr("Initialize mission automatically"),
                    );
                }
            });
            ui.add_space(10.0);
        }
        let note = self.color_text_dim();
        ui.add_enabled_ui(!operator_owned, |ui| {
            ui.push_id(field_scope, |ui| {
                match &mut editor.draft.definition {
                    ServerDefinition::Arma3(settings) => {
                        arma3::render(
                            ui,
                            card,
                            settings,
                            &mut editor.mission_keys,
                            &mut editor.next_row_key,
                            field_scope,
                            headless_clients.then_some(headless_profiles),
                        );
                        ui.label(
                            egui::RichText::new(tr(
                                "Use configuration import for operator files and protected password binding.",
                            ))
                            .color(note),
                        );
                    }
                    ServerDefinition::Reforger(settings) => reforger::render(
                        ui,
                        card,
                        settings,
                        &mut editor.addon_keys,
                        &mut editor.next_row_key,
                        field_scope,
                    ),
                }
            });
        });
        editor.invalid_input = arma3::has_invalid_edits(ui.ctx(), field_scope);
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
        let label = ui.label(tr(key));
        ui.add(
            egui::TextEdit::singleline(value)
                .id(id)
                .desired_width(300.0),
        )
        .labelled_by(label.id);
    });
}

fn number(ui: &mut Ui, key: &str, value: &mut u16, max: u16) {
    ui.horizontal_wrapped(|ui| {
        let label = ui.label(tr(key));
        let value = ui
            .add(egui::DragValue::new(value).range(1..=max))
            .labelled_by(label.id);
        match key {
            "Game port" => {
                value.on_hover_text(tr(
                    "UDP port number used by clients to reach the game server. The engine may use neighboring ports too.",
                ));
            }
            "Bind port" => {
                value.on_hover_text(tr("UDP port number for Reforger game traffic."));
            }
            "A2S query port" => {
                value.on_hover_text(tr(
                    "UDP port number for the separate server information query endpoint.",
                ));
            }
            "Maximum players" => {
                value.on_hover_text(tr(
                    "Maximum number of player slots advertised and accepted by the server.",
                ));
            }
            _ => {}
        }
    });
}
