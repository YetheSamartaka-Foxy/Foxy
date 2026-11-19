use eframe::egui::{self, RichText, Ui};

use crate::core::server::owner::RconConnection;
use crate::core::server::rcon::{RconCommandRecord, RconCommandState, reforger};
use crate::core::server::{ServerInstanceId, rcon::reforger::ReforgerAdminCommand};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::app::server::reforger_admin::{
    ReforgerAdminAction, ReforgerAdminConfirmation, ReforgerAdminViewState,
};
use crate::ui::i18n::{tr, tr_fmt};

fn action_label(action: ReforgerAdminAction) -> &'static str {
    match action {
        ReforgerAdminAction::Players => "List players",
        ReforgerAdminAction::Bans => "List bans",
        ReforgerAdminAction::Kick => "Kick player",
        ReforgerAdminAction::Ban => "Ban player identity",
        ReforgerAdminAction::RemoveBan => "Remove ban",
        ReforgerAdminAction::ReloadScenario => "Reload scenario",
        ReforgerAdminAction::Logout => "Log out RCon session",
    }
}

fn required(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(tr_fmt("Enter {field}.", &[("field", tr(label))]))
    } else {
        Ok(value.to_owned())
    }
}

fn make_command(
    state: &ReforgerAdminViewState,
    records: &[&RconCommandRecord],
) -> Result<ReforgerAdminCommand, String> {
    match state.action {
        ReforgerAdminAction::Players => Ok(ReforgerAdminCommand::Players),
        ReforgerAdminAction::Bans => Ok(ReforgerAdminCommand::Bans {
            page: state
                .page
                .trim()
                .parse()
                .ok()
                .filter(|page: &u32| *page > 0)
                .ok_or_else(|| tr("Enter a ban-list page of at least 1."))?,
        }),
        ReforgerAdminAction::Kick => {
            let command = ReforgerAdminCommand::Kick {
                player_id: state
                    .player_id
                    .trim()
                    .parse()
                    .map_err(|_| tr("Enter a valid player ID."))?,
                expected_identity: required(&state.identity, "the player's identity")?,
            };
            let roster = records
                .iter()
                .copied()
                .filter(|record| {
                    record.state == RconCommandState::Replied
                        && record.text.trim().eq_ignore_ascii_case("#players")
                })
                .max_by_key(|record| record.requested_unix_ms)
                .ok_or_else(|| tr("First request a current player list."))?;
            if roster.messages.iter().any(|message| message.truncated) {
                return Err(tr(
                    "The current player transcript was truncated. Request a new player list.",
                ));
            }
            let transcript = roster
                .messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            command.validate_preflight(&transcript).map_err(|_| {
                tr("The player ID and expected identity do not match the current player list.")
            })?;
            Ok(command)
        }
        ReforgerAdminAction::Ban => Ok(ReforgerAdminCommand::Ban {
            identity: required(&state.identity, "a player identity")?,
            duration_seconds: state
                .duration_seconds
                .trim()
                .parse()
                .map_err(|_| tr("Enter a valid ban duration in seconds."))?,
            reason: required(&state.reason, "a reason")?,
        }),
        ReforgerAdminAction::RemoveBan => Ok(ReforgerAdminCommand::RemoveBan {
            identity: required(&state.identity, "a banned player identity")?,
        }),
        ReforgerAdminAction::ReloadScenario => Ok(ReforgerAdminCommand::ReloadScenario),
        ReforgerAdminAction::Logout => Ok(ReforgerAdminCommand::Logout),
    }
}

fn current_records<'a>(
    snapshot: &'a crate::ui::app::server::RconSnapshot,
    last: Option<&'a RconCommandRecord>,
    id: &ServerInstanceId,
    run: u64,
) -> Vec<&'a RconCommandRecord> {
    snapshot
        .recent
        .iter()
        .chain(last)
        .filter(|record| {
            record.instance_id == id.as_str()
                && record.run_generation == run
                && snapshot.session.as_ref().is_some_and(|session| {
                    session.owner_id == record.owner_id
                        && session.owner_generation == record.owner_generation
                })
        })
        .collect()
}

impl Foxy {
    pub(super) fn render_reforger_administration(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .reforger_administration
        {
            return;
        }
        let mut state = std::mem::take(&mut self.server_view.reforger_admin);
        let Some(id) = self.server_view.selected.clone() else {
            state.confirmation = None;
            self.server_view.reforger_admin = state;
            return;
        };
        let Some(snapshot) = self.server_view.rcon.for_instance(&id).cloned() else {
            state.confirmation = None;
            self.server_view.reforger_admin = state;
            return;
        };

        ui.separator();
        ui.heading(tr("Reforger administration"));
        ui.label(tr("Typed RCon actions for the running Reforger server."));
        let Some(run_generation) = snapshot.run_generation else {
            state.confirmation = None;
            self.server_view.reforger_admin = state;
            ui.label(tr(
                "Start this server with RCon enabled to use Reforger administration.",
            ));
            return;
        };
        let connected = snapshot
            .session
            .as_ref()
            .is_some_and(|session| session.connection == RconConnection::Connected);
        if !connected {
            ui.label(tr(
                "Reforger administration is unavailable until the RCon session is connected.",
            ));
        }

        let last_command = self
            .server_view
            .rcon
            .last_command
            .as_ref()
            .filter(|record| record.instance_id == id.as_str())
            .cloned();
        let records = current_records(&snapshot, last_command.as_ref(), &id, run_generation);
        if state.page.is_empty() {
            state.page = "1".into();
        }
        if state.confirmation.as_ref().is_some_and(|confirmation| {
            confirmation.instance_id != id
                || confirmation.run_generation != run_generation
                || snapshot.session.as_ref().is_none_or(|session| {
                    session.owner_id != confirmation.owner_id
                        || session.owner_generation != confirmation.owner_generation
                })
        }) {
            state.confirmation = None;
            state.focus_cancel = false;
        }

        ui.add_enabled_ui(state.confirmation.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                let action_caption = ui.label(tr("Action"));
                egui::ComboBox::from_id_salt(("reforger-admin-action", id.as_str()))
                    .selected_text(tr(action_label(state.action)))
                    .show_ui(ui, |ui| {
                        for action in [
                            ReforgerAdminAction::Players,
                            ReforgerAdminAction::Bans,
                            ReforgerAdminAction::Kick,
                            ReforgerAdminAction::Ban,
                            ReforgerAdminAction::RemoveBan,
                            ReforgerAdminAction::ReloadScenario,
                            ReforgerAdminAction::Logout,
                        ] {
                            ui.selectable_value(
                                &mut state.action,
                                action,
                                tr(action_label(action)),
                            );
                        }
                    })
                    .response
                    .labelled_by(action_caption.id);
            });
        });
        ui.add_enabled_ui(state.confirmation.is_none(), |ui| match state.action {
            ReforgerAdminAction::Bans => {
                ui.horizontal(|ui| {
                    let label = ui.label(tr("Ban-list page"));
                    ui.add(
                        egui::TextEdit::singleline(&mut state.page)
                            .desired_width(80.0)
                            .char_limit(10),
                    ).labelled_by(label.id);
                });
            }
            ReforgerAdminAction::Kick => {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr("Player ID"));
                    ui.add(
                        egui::TextEdit::singleline(&mut state.player_id)
                            .desired_width(100.0)
                            .char_limit(10),
                    ).labelled_by(label.id);
                    let label = ui.label(tr("Expected identity"));
                    ui.add(
                        egui::TextEdit::singleline(&mut state.identity)
                            .desired_width(240.0)
                            .char_limit(64),
                    ).labelled_by(label.id);
                });
                ui.label(tr("The player ID must match this identity in a current player transcript."));
            }
            ReforgerAdminAction::Ban => {
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr("Player identity"));
                    ui.add(
                        egui::TextEdit::singleline(&mut state.identity)
                            .desired_width(240.0)
                            .char_limit(64),
                    ).labelled_by(label.id);
                    let label = ui.label(tr("Duration (seconds)"));
                    ui.add(
                        egui::TextEdit::singleline(&mut state.duration_seconds)
                            .desired_width(100.0)
                            .char_limit(10),
                    ).labelled_by(label.id);
                });
                let label = ui.label(tr("Reason"));
                ui.add(
                    egui::TextEdit::singleline(&mut state.reason)
                        .desired_width(360.0)
                        .char_limit(256),
                ).labelled_by(label.id);
            }
            ReforgerAdminAction::RemoveBan => {
                let label = ui.label(tr("Banned player identity"));
                ui.add(
                    egui::TextEdit::singleline(&mut state.identity)
                        .desired_width(240.0)
                        .char_limit(64),
                ).labelled_by(label.id);
            }
            ReforgerAdminAction::ReloadScenario => {
                ui.label(tr("Reload scenario sends the engine's #restart RCon command. Use the lifecycle Restart control to restart the server process."));
            }
            _ => {}
        });

        if let Some(error) = &state.action_error {
            ui.colored_label(self.color_text_error(), tr(error));
        }
        let busy = self.server_view.controls_busy();
        let mut send_command = None;
        ui.horizontal(|ui| {
            let review = ui.add_enabled(
                !busy && connected && state.confirmation.is_none(),
                egui::Button::new(tr("Review command")),
            );
            Self::focus_outline(ui, &review);
            if review.clicked() {
                match make_command(&state, &records).and_then(|command| {
                    let wire_text = command.wire_text()?;
                    Ok((command, wire_text))
                }) {
                    Ok((command, wire_text)) => {
                        state.action_error = None;
                        state.confirmation = Some(ReforgerAdminConfirmation {
                            instance_id: id.clone(),
                            command,
                            wire_text,
                            run_generation,
                            owner_id: snapshot
                                .session
                                .as_ref()
                                .map(|session| session.owner_id.clone())
                                .unwrap_or_default(),
                            owner_generation: snapshot
                                .session
                                .as_ref()
                                .map_or(0, |session| session.owner_generation),
                        });
                        state.focus_cancel = true;
                    }
                    Err(error) => state.action_error = Some(error),
                }
            }
        });

        if let Some(confirmation) = state.confirmation.clone() {
            ui.group(|ui| {
                ui.label(RichText::new(tr("Review the typed command before sending it.")).strong());
                ui.label(RichText::new(confirmation.wire_text.clone()).monospace());
                ui.label(tr("An RCon acknowledgment confirms delivery only. It does not prove that an action completed."));
                let mut cancel = false;
                ui.horizontal(|ui| {
                    let send = ui.add_enabled(
                        !busy && connected,
                        egui::Button::new(tr("Send once")),
                    );
                    Self::focus_outline(ui, &send);
                    let safe = ui.button(tr("Cancel"));
                    if state.focus_cancel {
                        safe.request_focus();
                        safe.scroll_to_me(Some(egui::Align::Center));
                        state.focus_cancel = false;
                    }
                    Self::focus_outline(ui, &safe);
                    if send.clicked() {
                        send_command = Some(confirmation);
                    }
                    cancel = safe.clicked()
                        || (!busy
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                            }));
                });
                if cancel {
                    state.confirmation = None;
                }
            });
        }

        render_submitted_result(ui, &records, state.last_submitted.as_deref());
        self.server_view.reforger_admin = state;
        if let Some(confirmation) = send_command {
            self.server_view.reforger_admin.confirmation = None;
            self.server_view.reforger_admin.focus_cancel = false;
            self.server_view.reforger_admin.last_submitted = Some(confirmation.wire_text.clone());
            self.start_server_task(ServerTaskAction::SendReforgerAdminCommand {
                id: confirmation.instance_id,
                run_generation: confirmation.run_generation,
                command: confirmation.command,
                timeout_ms: 60_000,
            });
        }
    }
}

fn render_submitted_result(ui: &mut Ui, records: &[&RconCommandRecord], submitted: Option<&str>) {
    let Some(submitted) = submitted else {
        return;
    };
    let Some(record) = records
        .iter()
        .copied()
        .filter(|record| record.text == submitted)
        .max_by_key(|record| record.requested_unix_ms)
    else {
        return;
    };
    ui.group(|ui| {
        ui.label(
            RichText::new(tr_fmt(
                "Latest administration command: {state}",
                &[("state", tr(match record.state {
                    RconCommandState::Queued => "Queued",
                    RconCommandState::Sending => "Sending",
                    RconCommandState::Replied => "Reply received",
                    RconCommandState::Unknown => "Outcome unknown",
                    RconCommandState::NotSent => "Not sent",
                }))],
            ))
            .strong(),
        );
        match record.state {
            RconCommandState::Unknown => ui.label(tr("The command may or may not have taken effect. Check the server before sending it again.")),
            RconCommandState::Replied => ui.label(tr("RCon acknowledged the command. That acknowledgment does not confirm its effect.")),
            RconCommandState::NotSent => ui.label(tr("The command was not sent.")),
            RconCommandState::Queued | RconCommandState::Sending => ui.label(tr("The command is still in progress.")),
        };
        if record.messages.is_empty() {
            ui.label(tr("No semantic server output was received. An empty RCon acknowledgment is not an empty player or ban list."));
            return;
        }
        let transcript = record
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if record.text.trim().eq_ignore_ascii_case("#players") {
            let roster = reforger::parse_players(&transcript);
            if roster.players.is_empty() {
                ui.label(tr("No player rows could be parsed. The response does not establish that the server is empty."));
            } else {
                ui.label(tr_fmt(
                    "Player rows observed: {count}",
                    &[("count", roster.players.len().to_string())],
                ));
                for player in roster.players {
                    ui.label(tr_fmt(
                        "Player {id}: {name} ({identity})",
                        &[
                            ("id", player.player_id.to_string()),
                            ("name", player.name.unwrap_or_else(|| tr("unknown name"))),
                            (
                                "identity",
                                player.identity.unwrap_or_else(|| tr("unknown identity")),
                            ),
                        ],
                    ));
                }
                ui.label(tr("The observed player transcript may be incomplete."));
            }
        } else if let Some(page) = record
            .text
            .trim()
            .strip_prefix("#ban list ")
            .and_then(|page| page.parse::<u32>().ok())
        {
            let bans = reforger::parse_bans(&transcript, page);
            if bans.complete && bans.bans.is_empty() && !record.messages.iter().any(|message| message.truncated) {
                ui.label(tr("The server reported no bans on this page."));
            } else if bans.bans.is_empty() {
                ui.label(tr("No ban rows were parsed. The response does not establish that the ban list is empty."));
            } else {
                ui.label(tr_fmt(
                    "Ban rows observed: {count}",
                    &[("count", bans.bans.len().to_string())],
                ));
                if !bans.complete {
                    ui.label(tr("The observed ban transcript may be incomplete."));
                }
            }
        }
        egui::CollapsingHeader::new(tr("Raw Reforger server messages"))
            .id_salt(("reforger-admin-messages", &record.command_id))
            .default_open(true)
            .show(ui, |ui| {
                for message in &record.messages {
                    ui.add(egui::Label::new(&message.text).wrap().selectable(true));
                    if message.truncated {
                        ui.label(tr("This server message was truncated."));
                    }
                }
            });
    });
}
