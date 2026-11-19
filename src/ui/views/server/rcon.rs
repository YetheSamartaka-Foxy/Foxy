use std::time::{Duration, Instant};

use eframe::egui::{self, Response, RichText, Ui};
use zeroize::Zeroizing;

use crate::core::server::ReforgerRconPermission;
use crate::core::server::owner::RconConnection;
use crate::core::server::rcon::{RconCommandRecord, RconCommandState, RconPassword};
use crate::ui::app::Foxy;
use crate::ui::app::server::{
    RconConfirmation, RconForm, RconNotice, RconPasswordEntry, ServerTaskAction,
};
use crate::ui::i18n::{tr, tr_fmt};

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);


fn connection_text(connection: RconConnection) -> String {
    tr(match connection {
        RconConnection::Connecting => "Connecting",
        RconConnection::Connected => "Connected",
        RconConnection::Rejected => "The server refused the RCon password",
        RconConnection::NoReply => "No reply yet; retrying",
        RconConnection::Unreachable => "Nothing listens on the RCon port yet; retrying",
        RconConnection::PasswordUnavailable => "The protected RCon password is unavailable",
        RconConnection::Lost => "Connection lost; reconnecting",
    })
}

fn state_text(state: RconCommandState) -> String {
    tr(match state {
        RconCommandState::Queued => "Queued",
        RconCommandState::Sending => "Sending",
        RconCommandState::Replied => "Replied",
        RconCommandState::Unknown => "Outcome unknown",
        RconCommandState::NotSent => "Not sent",
    })
}

fn local_time(unix_ms: i64) -> String {
    u64::try_from(unix_ms)
        .ok()
        .and_then(crate::core::schedule::unix_ms_to_local)
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| tr("unknown time"))
}

impl Foxy {
    /// Returns the section's focus anchor for restoring focus after an RCon task.
    pub(in crate::ui::views) fn render_server_rcon(&mut self, ui: &mut Ui) -> Option<Response> {
        let capabilities = crate::core::game::registry().active().server_capabilities();
        if !capabilities.lifecycle {
            return None;
        }
        let id = self.server_view.selected.clone()?;
        let busy = self.server_view.controls_busy();
        let refresh_due = self
            .server_view
            .rcon
            .last_refresh
            .is_none_or(|last| last.elapsed() >= REFRESH_INTERVAL);
        if refresh_due && !self.server_view.busy() {
            self.server_view.rcon.last_refresh = Some(Instant::now());
            self.start_server_task(ServerTaskAction::LoadRcon(id.clone()));
        }
        ui.ctx().request_repaint_after(REFRESH_INTERVAL);

        ui.separator();
        ui.heading(tr("RCon"));
        ui.label(tr(
            "RCon lets the server owner send administration commands to a running server. The password is kept in protected storage and is written only into private runtime state when the server starts. Changes apply after commit, prepare, activate and a restart.",
        ));
        if let Some(error) = &self.server_view.rcon.load_error {
            ui.colored_label(
                self.color_text_error(),
                tr_fmt(
                    "RCon state could not be read: {reason}",
                    &[("reason", tr(error))],
                ),
            );
        }
        let Some(snapshot) = self.server_view.rcon.for_instance(&id).cloned() else {
            if self.server_view.rcon.load_error.is_none() {
                ui.horizontal(|ui| {
                    ui.add(crate::ui::app::PacedSpinner::new());
                    ui.label(tr("Loading RCon state..."));
                });
            }
            return None;
        };
        if matches!(
            &self.server_view.rcon.confirmation,
            Some(RconConfirmation::Disable { revision }) if *revision != snapshot.draft_revision
        ) || matches!(
            &self.server_view.rcon.confirmation,
            Some(RconConfirmation::Send { run_generation, .. }) if Some(*run_generation) != snapshot.run_generation
        ) {
            self.server_view.rcon.confirmation = None;
        }
        match &snapshot.draft {
            Some(settings) => {
                let mut summary = tr_fmt(
                    "Draft endpoint {address}:{port}",
                    &[
                        ("address", settings.address.to_string()),
                        ("port", settings.port.to_string()),
                    ],
                );
                if let Some(permission) = settings.permission {
                    summary.push_str("; ");
                    summary.push_str(&tr(match permission {
                        ReforgerRconPermission::Admin => "every connection has admin permission",
                        ReforgerRconPermission::Monitor => {
                            "every connection has monitor permission"
                        }
                    }));
                }
                ui.label(summary);
                ui.label(tr(if snapshot.password_available {
                    "A protected RCon password is stored."
                } else {
                    "No RCon password is stored yet. The server cannot start with this RCon configuration until one is set."
                }));
                if !settings.address.is_loopback() {
                    ui.colored_label(
                        self.color_warn(),
                        tr("RCon sends its password without encryption. Keep an endpoint that other computers can reach on a private network or a tunnel."),
                    );
                }
            }
            None => {
                ui.label(tr("RCon is not configured for this instance."));
            }
        }
        let mut action = None;
        let anchor = ui
            .horizontal_wrapped(|ui| {
                let configure = ui.add_enabled(
                    !busy && self.server_view.rcon.form.is_none(),
                    egui::Button::new(tr(if snapshot.draft.is_some() {
                        "Edit RCon settings"
                    } else {
                        "Configure RCon"
                    })),
                );
                Self::focus_outline(ui, &configure);
                if configure.clicked() {
                    self.server_view.rcon.form = Some(RconForm::open(&snapshot));
                    self.server_view.rcon.focus_form = true;
                    self.server_view.rcon.password = None;
                    self.server_view.rcon.confirmation = None;
                    self.server_view.rcon.notice = None;
                    self.server_view.rcon.action_error = None;
                }
                if snapshot.draft.is_some() {
                    let password = ui.add_enabled(
                        !busy && self.server_view.rcon.password.is_none(),
                        egui::Button::new(tr("Set RCon password")),
                    );
                    Self::focus_outline(ui, &password);
                    if password.clicked() {
                        self.server_view.rcon.password = Some(RconPasswordEntry {
                            base_revision: snapshot.draft_revision,
                            value: Zeroizing::new(String::new()),
                            error: None,
                        });
                        self.server_view.rcon.focus_password = true;
                        self.server_view.rcon.form = None;
                        self.server_view.rcon.confirmation = None;
                        self.server_view.rcon.notice = None;
                        self.server_view.rcon.action_error = None;
                    }
                    let remove = ui.add_enabled(!busy, egui::Button::new(tr("Remove RCon")));
                    Self::focus_outline(ui, &remove);
                    if remove.clicked() {
                        self.server_view.rcon.confirmation = Some(RconConfirmation::Disable {
                            revision: snapshot.draft_revision,
                        });
                        self.server_view.rcon.focus_confirmation = true;
                        self.server_view.rcon.notice = None;
                        self.server_view.rcon.action_error = None;
                    }
                }
                let refresh = ui.add_enabled(!busy, egui::Button::new(tr("Refresh RCon")));
                Self::focus_outline(ui, &refresh);
                if refresh.clicked() {
                    self.server_view.rcon.last_refresh = Some(Instant::now());
                    self.server_view.rcon.notice = None;
                    self.server_view.rcon.action_error = None;
                    action = Some(ServerTaskAction::LoadRcon(id.clone()));
                }
                configure
            })
            .inner;
        if let Some(notice) = self.server_view.rcon.notice {
            ui.label(match notice {
                RconNotice::SettingsSaved => tr("RCon settings saved in the draft. Commit, prepare and activate, then restart the server to apply them."),
                RconNotice::Disabled => tr("RCon was removed from the draft. Commit, prepare and activate, then restart the server to apply."),
                RconNotice::PasswordStored => tr("A new protected RCon password was stored for the draft. Commit, prepare and activate, then restart the server to use it."),
                RconNotice::TestReplied { round_trip_ms } => tr_fmt(
                    "RCon answered in {ms} ms.",
                    &[("ms", round_trip_ms.to_string())],
                ),
            });
        }
        if let Some(error) = &self.server_view.rcon.action_error {
            ui.colored_label(self.color_text_error(), tr(error));
        }
        if let Some(form_action) = self.render_rcon_form(ui, &id, busy) {
            action = Some(form_action);
        }
        if let Some(password_action) = self.render_rcon_password(ui, &id, busy) {
            action = Some(password_action);
        }
        if let Some(confirmation) = self.server_view.rcon.confirmation.clone()
            && matches!(confirmation, RconConfirmation::Disable { .. })
            && let Some(confirm_action) = self.render_rcon_confirmation(ui, &id, confirmation, busy)
        {
            action = Some(confirm_action);
        }
        if let Some(session_action) = self.render_rcon_session(ui, &id, &snapshot, busy) {
            action = Some(session_action);
        }
        if let Some(record) = self.server_view.rcon.last_command.clone()
            && record.instance_id == id.as_str()
        {
            self.render_rcon_outcome(ui, &record);
        }
        if snapshot.unreadable > 0 {
            ui.colored_label(
                self.color_text_error(),
                tr_fmt(
                    "{count} unreadable RCon command records were left untouched.",
                    &[("count", snapshot.unreadable.to_string())],
                ),
            );
        }
        if !snapshot.recent.is_empty() {
            egui::CollapsingHeader::new(tr_fmt(
                "Recent RCon commands ({count})",
                &[("count", snapshot.recent.len().to_string())],
            ))
            .id_salt(("server-rcon-history", id.as_str()))
            .show(ui, |ui| {
                for record in &snapshot.recent {
                    ui.group(|ui| {
                        ui.label(
                            RichText::new(tr_fmt(
                                "{text}: {state}",
                                &[
                                    ("text", record.text.clone()),
                                    ("state", state_text(record.state)),
                                ],
                            ))
                            .strong(),
                        );
                        ui.label(tr_fmt(
                            "Requested {when}; run {run}",
                            &[
                                ("when", local_time(record.requested_unix_ms)),
                                ("run", record.run_generation.to_string()),
                            ],
                        ));
                        if let Some(reason) = record.reason {
                            ui.label(tr(reason.describe()));
                        }
                    });
                }
            });
        }
        if let Some(action) = action {
            self.start_server_task(action);
        }
        Some(anchor)
    }

    fn render_rcon_form(
        &mut self,
        ui: &mut Ui,
        id: &crate::core::server::ServerInstanceId,
        busy: bool,
    ) -> Option<ServerTaskAction> {
        let mut form = self.server_view.rcon.form.clone()?;
        let mut action = None;
        let mut close = false;
        ui.group(|ui| {
            ui.label(RichText::new(tr("RCon settings")).strong());
            ui.horizontal(|ui| {
                let caption = ui.label(tr("Port"));
                let port = ui
                    .add(egui::DragValue::new(&mut form.port).range(1..=65535))
                    .labelled_by(caption.id);
                if self.server_view.rcon.focus_form {
                    port.request_focus();
                    self.server_view.rcon.focus_form = false;
                }
            });
            if !form.reforger {
                ui.label(tr("For Arma, keep the RCon port outside the game port range (game port to game port + 4). BattlEye must be on."));
            }
            ui.horizontal(|ui| {
                let caption = ui.label(tr("Listen address"));
                ui.add(egui::TextEdit::singleline(&mut form.address).desired_width(140.0))
                    .labelled_by(caption.id);
            });
            if form.reforger {
                ui.horizontal(|ui| {
                    ui.label(tr("Permission for every connection"));
                    ui.radio_value(&mut form.permission, ReforgerRconPermission::Monitor, tr("Monitor"));
                    ui.radio_value(&mut form.permission, ReforgerRconPermission::Admin, tr("Admin"));
                });
                ui.horizontal(|ui| {
                    let caption = ui.label(tr("Maximum RCon clients (0 uses the engine default)"));
                    ui.add(egui::DragValue::new(&mut form.max_clients).range(0..=16))
                        .labelled_by(caption.id);
                });
            }
            if let Some(error) = &form.error {
                ui.colored_label(self.color_text_error(), tr(error));
            }
            ui.horizontal(|ui| {
                let save = ui.add_enabled(!busy, egui::Button::new(tr("Save RCon settings")));
                Self::focus_outline(ui, &save);
                let cancel = ui.button(tr("Cancel"));
                Self::focus_outline(ui, &cancel);
                if save.clicked() {
                    match form.build() {
                        Ok(input) => {
                            form.error = None;
                            action = Some(ServerTaskAction::ConfigureRcon {
                                id: id.clone(),
                                expected_revision: form.base_revision,
                                input: Some(input),
                            });
                        }
                        Err(message) => form.error = Some(message.into()),
                    }
                }
                if cancel.clicked()
                    || (!busy
                        && ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                        }))
                {
                    close = true;
                }
            });
        });
        if close {
            self.server_view.rcon.form = None;
            self.server_view.pending_focus = Some("server-rcon-controls");
        } else {
            self.server_view.rcon.form = Some(form);
        }
        action
    }

    fn render_rcon_password(
        &mut self,
        ui: &mut Ui,
        id: &crate::core::server::ServerInstanceId,
        busy: bool,
    ) -> Option<ServerTaskAction> {
        let mut action = None;
        let mut close = false;
        let focus = std::mem::take(&mut self.server_view.rcon.focus_password);
        let error_color = self.color_text_error();
        let entry = self.server_view.rcon.password.as_mut()?;
        ui.group(|ui| {
            let caption = ui.label(RichText::new(tr("New RCon password")).strong());
            ui.label(tr(
                "Use 3 to 64 visible characters without spaces. The password is never shown again.",
            ));
            let field = ui
                .add(
                    egui::TextEdit::singleline(&mut *entry.value)
                        .password(true)
                        .desired_width(220.0),
                )
                .labelled_by(caption.id);
            if focus {
                field.request_focus();
            }
            if let Some(error) = &entry.error {
                ui.colored_label(error_color, tr(error));
            }
            ui.horizontal(|ui| {
                let save = ui.add_enabled(!busy, egui::Button::new(tr("Save RCon password")));
                Self::focus_outline(ui, &save);
                let cancel = ui.button(tr("Cancel"));
                Self::focus_outline(ui, &cancel);
                if save.clicked() {
                    match RconPassword::new(entry.value.to_string()) {
                        Ok(password) => {
                            entry.error = None;
                            action = Some(ServerTaskAction::SetRconPassword {
                                id: id.clone(),
                                expected_revision: entry.base_revision,
                                password,
                            });
                        }
                        Err(message) => entry.error = Some(message.into()),
                    }
                }
                if cancel.clicked()
                    || (!busy
                        && ui.input_mut(|i| {
                            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                        }))
                {
                    close = true;
                }
            });
        });
        if close {
            self.server_view.rcon.password = None;
            self.server_view.pending_focus = Some("server-rcon-controls");
        }
        action
    }

    fn render_rcon_confirmation(
        &mut self,
        ui: &mut Ui,
        id: &crate::core::server::ServerInstanceId,
        confirmation: RconConfirmation,
        busy: bool,
    ) -> Option<ServerTaskAction> {
        let (question, confirm) = match &confirmation {
            RconConfirmation::Disable { .. } => (
                "Remove RCon from this draft? The stored password stays for committed revisions that still use it.",
                "Remove RCon",
            ),
            RconConfirmation::Send { .. } => (
                "Send this command once? It is never resent automatically. If no reply arrives, its outcome is unknown and you should check the server before sending it again.",
                "Send command",
            ),
        };
        ui.label(tr(question));
        let mut action = None;
        ui.horizontal(|ui| {
            let apply = ui.add_enabled(!busy, egui::Button::new(tr(confirm)));
            Self::focus_outline(ui, &apply);
            let safe = ui.button(tr("Cancel"));
            if self.server_view.rcon.focus_confirmation {
                safe.request_focus();
                safe.scroll_to_me(Some(egui::Align::Center));
                self.server_view.rcon.focus_confirmation = false;
            }
            Self::focus_outline(ui, &safe);
            if safe.clicked()
                || (!busy
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)))
            {
                self.server_view.rcon.confirmation = None;
                self.server_view.pending_focus = Some("server-rcon-controls");
            }
            if apply.clicked() {
                action = Some(match confirmation {
                    RconConfirmation::Disable { revision } => ServerTaskAction::ConfigureRcon {
                        id: id.clone(),
                        expected_revision: revision,
                        input: None,
                    },
                    RconConfirmation::Send {
                        text,
                        run_generation,
                    } => ServerTaskAction::SendRconCommand {
                        id: id.clone(),
                        run_generation,
                        text,
                    },
                });
            }
        });
        action
    }

    fn render_rcon_session(
        &mut self,
        ui: &mut Ui,
        id: &crate::core::server::ServerInstanceId,
        snapshot: &crate::ui::app::server::RconSnapshot,
        busy: bool,
    ) -> Option<ServerTaskAction> {
        let mut action = None;
        ui.label(RichText::new(tr("Live RCon session")).strong());
        let Some(status) = &snapshot.session else {
            ui.label(match &snapshot.session_note {
                Some(note) => tr(note),
                None => tr("No running server of this instance has an RCon session."),
            });
            return None;
        };
        ui.label(tr_fmt(
            "{state} at {endpoint} (run {run}, session {session})",
            &[
                ("state", connection_text(status.connection)),
                ("endpoint", status.endpoint.clone()),
                ("run", status.run_generation.to_string()),
                ("session", status.session_generation.to_string()),
            ],
        ));
        if let Some(round_trip) = status.round_trip_ms {
            ui.label(tr_fmt(
                "Last keepalive answered in {ms} ms; {acknowledged} server messages acknowledged.",
                &[
                    ("ms", round_trip.to_string()),
                    ("acknowledged", status.messages_acknowledged.to_string()),
                ],
            ));
        }
        let connected = status.connection == RconConnection::Connected;
        let run_generation = snapshot.run_generation;
        ui.horizontal(|ui| {
            let test = ui.add_enabled(
                !busy && connected && run_generation.is_some(),
                egui::Button::new(tr("Test RCon connection")),
            );
            Self::focus_outline(ui, &test);
            if test.clicked()
                && let Some(run_generation) = run_generation
            {
                self.server_view.rcon.notice = None;
                self.server_view.rcon.action_error = None;
                action = Some(ServerTaskAction::TestRcon {
                    id: id.clone(),
                    run_generation,
                });
            }
        });
        ui.horizontal(|ui| {
            let caption = ui.label(tr("Command"));
            ui.add_enabled(
                connected,
                egui::TextEdit::singleline(&mut self.server_view.rcon.command_text)
                    .desired_width(320.0),
            )
            .labelled_by(caption.id);
            let text = self.server_view.rcon.command_text.trim().to_owned();
            let send = ui.add_enabled(
                !busy
                    && connected
                    && run_generation.is_some()
                    && foxy_supervisor::control::valid_rcon_text(&text)
                    && !crate::core::server::rcon::carries_secret(&text),
                egui::Button::new(tr("Send command...")),
            );
            Self::focus_outline(ui, &send);
            if send.clicked()
                && let Some(run_generation) = run_generation
            {
                self.server_view.rcon.confirmation = Some(RconConfirmation::Send {
                    text,
                    run_generation,
                });
                self.server_view.rcon.focus_confirmation = true;
                self.server_view.rcon.notice = None;
                self.server_view.rcon.action_error = None;
            }
        });
        if crate::core::server::rcon::carries_secret(&self.server_view.rcon.command_text) {
            ui.label(tr(
                "Commands that carry a password are refused. Use Set RCon password instead.",
            ));
        }
        if let Some(confirmation) = self.server_view.rcon.confirmation.clone()
            && matches!(confirmation, RconConfirmation::Send { .. })
            && let Some(confirm_action) = self.render_rcon_confirmation(ui, id, confirmation, busy)
        {
            action = Some(confirm_action);
        }
        if !status.messages.is_empty() {
            egui::CollapsingHeader::new(tr_fmt(
                "Recent server messages ({count})",
                &[("count", status.messages.len().to_string())],
            ))
            .id_salt(("server-rcon-messages", id.as_str()))
            .show(ui, |ui| {
                for message in &status.messages {
                    ui.label(format!(
                        "{}  {}",
                        local_time(message.received_unix_ms),
                        message.text
                    ));
                }
            });
        }
        action
    }

    fn render_rcon_outcome(&mut self, ui: &mut Ui, record: &RconCommandRecord) {
        ui.group(|ui| {
            ui.label(
                RichText::new(tr_fmt(
                    "Last command {text}: {state}",
                    &[
                        ("text", record.text.clone()),
                        ("state", state_text(record.state)),
                    ],
                ))
                .strong(),
            );
            match record.state {
                RconCommandState::Unknown => {
                    ui.colored_label(
                        self.color_warn(),
                        tr("The command was sent once and no complete reply arrived. It may or may not have taken effect, so check the server before sending it again."),
                    );
                }
                RconCommandState::NotSent => {
                    ui.label(tr("The command was not sent, so it had no effect."));
                }
                _ => {}
            }
            if let Some(reason) = record.reason {
                ui.label(tr(reason.describe()));
            }
            if let Some(response) = record.response.as_deref().filter(|text| !text.is_empty()) {
                egui::ScrollArea::vertical()
                    .id_salt(("server-rcon-response", record.command_id.as_str()))
                    .max_height(180.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(response).monospace());
                    });
                if record.response_truncated {
                    ui.label(tr("Only the first 16 KiB of the reply were kept."));
                }
            } else if record.state == RconCommandState::Replied {
                ui.label(tr("The server acknowledged the command with an empty reply."));
            }
        });
    }
}
