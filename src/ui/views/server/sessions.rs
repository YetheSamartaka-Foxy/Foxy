use crate::core::server::ReforgerSessionIntent;
use crate::ui::app::Foxy;
use crate::ui::i18n::{tr, tr_fmt};
use eframe::egui::{self, RichText, Ui};

impl Foxy {
    pub(crate) fn render_server_sessions(&mut self, ui: &mut Ui) -> Option<egui::Response> {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .reforger_sessions
        {
            return None;
        }
        let editor = self.server_view.editor.as_ref()?;
        let id = editor.baseline.id.clone();
        let dirty = editor.dirty();
        let busy = self.server_view.busy();
        let state = &mut self.server_view.sessions;
        if state.mode.is_empty() {
            state.mode = "configured".into();
        }
        let response = ui.push_id(editor.field_scope().with("sessions"), |ui| {
            let mut anchor = ui.heading(tr("Reforger sessions"));
            ui.weak(tr("Review local save points and choose the next launch intent. This does not start or stop the server."));
            ui.weak(tr("Session intent changes require a stopped server with an idle persistence writer."));
            ui.weak(tr("Automatic save selection covers local default persistence. Use server configuration to keep a custom provider unchanged."));
            if dirty {
                ui.label(tr("Save or discard server draft edits before reviewing a session intent."));
            }

            ui.horizontal_wrapped(|ui| {
                let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh session saves")));
                anchor = refresh.clone();
                self.server_view.sessions.controls_focus = Some(refresh.id);
                Self::focus_outline(ui, &refresh);
                if refresh.clicked() {
                    self.server_view.sessions.plan = None;
                    self.server_view.sessions.confirmation = false;
                    self.request_server_sessions();
                }
            });

            if let Some(inventory) = self
                .server_view
                .sessions
                .inventory
                .as_ref()
                .filter(|inventory| inventory.instance_id == id)
                .cloned()
            {
                ui.label(tr_fmt(
                    "Session inventory generation {generation}; fingerprint {fingerprint}; {count} save point(s).",
                    &[
                        ("generation", inventory.generation.to_string()),
                        ("fingerprint", inventory.fingerprint.clone()),
                        ("count", inventory.sessions.len().to_string()),
                    ],
                ));
                ui.label(tr_fmt(
                    "Local default provider: {local}; blockers: {blockers}",
                    &[
                        ("local", inventory.provider.local_default.to_string()),
                        ("blockers", inventory.provider.blockers.len().to_string()),
                    ],
                ));
                for blocker in &inventory.provider.blockers {
                    ui.colored_label(self.color_text_error(), blocker);
                }
                if inventory.sessions.is_empty() {
                    ui.label(tr("No recognized local Reforger save points were found."));
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("server-session-inventory", id.as_str()))
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for session in &inventory.sessions {
                                ui.group(|ui| {
                                    ui.label(RichText::new(&session.save_id).strong());
                                    ui.label(tr_fmt(
                                        "Scenario: {scenario}; engine version: {version}; compatible: {compatible}",
                                        &[
                                            ("scenario", session.scenario.clone()),
                                            ("version", session.game_version.clone()),
                                            ("compatible", session.compatible.to_string()),
                                        ],
                                    ));
                                    ui.label(tr_fmt(
                                        "Playthrough {playthrough}; save point {savepoint}; saved {time}; {bytes} bytes",
                                        &[
                                            ("playthrough", session.playthrough.to_string()),
                                            ("savepoint", session.savepoint.to_string()),
                                            ("time", session.saved_unix.to_string()),
                                            ("bytes", session.bytes.to_string()),
                                        ],
                                    ));
                                    for blocker in &session.blockers {
                                        ui.colored_label(self.color_text_error(), blocker);
                                    }
                                    if session.compatible {
                                        let select = ui.button(tr("Select this save point"));
                                        Self::focus_outline(ui, &select);
                                        if select.clicked() {
                                            self.server_view.sessions.mode = "selected".into();
                                            self.server_view.sessions.save_id = session.save_id.clone();
                                            self.server_view.sessions.plan = None;
                                            self.server_view.sessions.confirmation = false;
                                        }
                                    }
                                });
                            }
                        });
                }
            }

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                let caption = ui.label(tr("Next launch session intent:"));
                let before = self.server_view.sessions.mode.clone();
                egui::ComboBox::from_id_salt(("server-session-mode", id.as_str()))
                    .selected_text(tr(session_mode_label(&self.server_view.sessions.mode)))
                    .show_ui(ui, |ui| {
                        for (mode, label) in [
                            ("configured", "Use server configuration"),
                            ("latest", "Continue latest compatible save"),
                            ("selected", "Continue selected save"),
                            ("new", "Start a new session"),
                        ] {
                            ui.selectable_value(
                                &mut self.server_view.sessions.mode,
                                mode.into(),
                                tr(label),
                            );
                        }
                    })
                    .response
                    .labelled_by(caption.id);
                if before != self.server_view.sessions.mode {
                    self.server_view.sessions.plan = None;
                    self.server_view.sessions.confirmation = false;
                }
            });
            if self.server_view.sessions.mode == "selected" {
                ui.horizontal_wrapped(|ui| {
                    let caption = ui.label(tr("Save point UUID:"));
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.server_view.sessions.save_id)
                                .desired_width(300.0)
                                .id(egui::Id::unique(("server-session-save-id", id.as_str()))),
                        )
                        .labelled_by(caption.id);
                    Self::focus_outline(ui, &response);
                });
            }
            ui.label(tr("Changing this setting applies only to a later launch. It does not request a save, change the running process, or start a server."));
            let plan_button = ui.add_enabled(
                !busy
                    && !dirty
                    && self.server_view.sessions.intent().is_some()
                    && self.server_view.sessions.inventory.as_ref().is_some_and(|inventory| inventory.instance_id == id),
                egui::Button::new(tr("Review session intent")),
            );
            Self::focus_outline(ui, &plan_button);
            if plan_button.clicked() {
                self.server_view.sessions.plan = None;
                self.server_view.sessions.confirmation = false;
                self.request_server_session_plan();
            }

            if let Some(plan) = self
                .server_view
                .sessions
                .plan
                .as_ref()
                .filter(|plan| plan.instance_id == id)
            {
                ui.group(|ui| {
                    ui.label(tr_fmt(
                        "Session intent review {fingerprint}",
                        &[("fingerprint", plan.fingerprint.clone())],
                    ));
                    ui.label(tr_fmt(
                        "Expected inventory generation: {generation}; inventory fingerprint: {inventory}",
                        &[
                            ("generation", plan.expected_generation.to_string()),
                            ("inventory", plan.inventory_fingerprint.clone()),
                        ],
                    ));
                    ui.label(tr_fmt(
                        "Selected save point: {save}",
                        &[("save", plan.selected_save_id.clone().unwrap_or_else(|| tr("None")))],
                    ));
                    for warning in &plan.warnings {
                        ui.label(warning);
                    }
                    for blocker in &plan.blockers {
                        ui.colored_label(self.color_text_error(), blocker);
                    }
                    ui.label(tr("This review records next-launch intent only. The engine must still accept and load the selected save."));
                });
                let review_matches = self.server_view.sessions.intent().is_some_and(|intent| intent == plan.intent);
                let confirm = ui.add_enabled(
                    !busy && !dirty && review_matches && plan.blockers.is_empty(),
                    egui::Button::new(tr("Confirm session intent")),
                );
                Self::focus_outline(ui, &confirm);
                if confirm.clicked() {
                    self.server_view.sessions.confirmation = true;
                    self.server_view.sessions.focus_cancel = true;
                }
            }

            anchor
        });
        Some(response.inner)
    }

    pub(crate) fn render_server_session_confirmation(&mut self, ctx: &egui::Context) {
        let Some(editor) = &self.server_view.editor else {
            return;
        };
        let id = editor.baseline.id.clone();
        let dirty = editor.dirty();
        let busy = self.server_view.busy();
        let Some(plan) = self
            .server_view
            .sessions
            .plan
            .as_ref()
            .filter(|plan| plan.instance_id == id && plan.blockers.is_empty())
        else {
            self.server_view.sessions.confirmation = false;
            return;
        };
        if !self.server_view.sessions.confirmation {
            return;
        }
        let fingerprint = plan.fingerprint.clone();
        let intent = plan.intent.clone();
        let selected = plan.selected_save_id.clone();
        let mut apply = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique(("server-session-confirmation", id.as_str()))).show(ctx, |ui| {
            ui.heading(tr("Save this next-launch session intent?"));
            ui.label(tr_fmt("Reviewed intent: {intent}", &[("intent", intent_label(&intent))]));
            ui.label(tr_fmt("Selected save UUID: {save}", &[("save", selected.unwrap_or_else(|| tr("None")))]));
            ui.label(tr_fmt("Review fingerprint: {fingerprint}", &[("fingerprint", fingerprint)]));
            ui.label(tr("Foxy records this intent for a later server launch. It does not start or stop the server, convert a save, or recover external-provider data."));
            ui.horizontal_wrapped(|ui| {
                let safe = ui.button(tr("Cancel"));
                if self.server_view.sessions.focus_cancel {
                    safe.request_focus();
                    self.server_view.sessions.focus_cancel = false;
                }
                Self::focus_outline(ui, &safe);
                if safe.clicked() {
                    cancel = true;
                }
                let confirm = ui.add_enabled(!busy && !dirty, egui::Button::new(tr("Save next-launch intent")));
                Self::focus_outline(ui, &confirm);
                if confirm.clicked() {
                    apply = true;
                }
            });
            if !busy
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            {
                cancel = true;
            }
        });
        if cancel {
            self.server_view.sessions.confirmation = false;
            self.server_view.pending_focus = Some("server-session-controls");
        }
        if apply {
            self.server_view.sessions.confirmation = false;
            self.apply_server_session_plan();
        }
    }
}

fn session_mode_label(mode: &str) -> &'static str {
    match mode {
        "latest" => "Continue latest compatible save",
        "selected" => "Continue selected save",
        "new" => "Start a new session",
        _ => "Use server configuration",
    }
}

fn intent_label(intent: &ReforgerSessionIntent) -> String {
    match intent {
        ReforgerSessionIntent::Configured => tr("Use server configuration"),
        ReforgerSessionIntent::Latest => tr("Continue latest compatible save"),
        ReforgerSessionIntent::Selected { .. } => tr("Continue selected save"),
        ReforgerSessionIntent::New => tr("Start a new session"),
    }
}
