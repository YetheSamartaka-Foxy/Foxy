use eframe::egui::{self, Response, RichText, Ui};

use crate::core::server::rcon::arma3::{
    ArmaAdminCommand, ArmaBanList, ArmaMissionList, ArmaRoster, parse_bans, parse_missions,
    parse_players,
};
use crate::core::server::rcon::{RconCommandRecord, RconCommandState};
use crate::ui::app::Foxy;
use crate::ui::app::server::ServerTaskAction;
use crate::ui::app::server::arma_admin::{ArmaAdminAction, ArmaAdminConfirmation};
use crate::ui::i18n::{tr, tr_fmt};

const MAX_ADMIN_ROWS: usize = 100;

fn focus_outline(ui: &Ui, response: &Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            2.0,
            egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
            egui::StrokeKind::Inside,
        );
    }
}

fn latest_reply<'a>(
    records: impl Iterator<Item = &'a RconCommandRecord>,
    command: &str,
    run_generation: u64,
) -> Option<&'a str> {
    records
        .filter(|record| {
            record.state == RconCommandState::Replied
                && !record.response_truncated
                && record.run_generation == run_generation
                && record.text.trim().eq_ignore_ascii_case(command)
        })
        .max_by_key(|record| record.requested_unix_ms)
        .and_then(|record| record.response.as_deref())
}

fn current_records<'a>(
    snapshot: &'a crate::ui::app::server::RconSnapshot,
    last: Option<&'a RconCommandRecord>,
    run: u64,
) -> impl Iterator<Item = &'a RconCommandRecord> + Clone {
    snapshot.recent.iter().chain(last).filter(move |record| {
        record.run_generation == run
            && snapshot.session.as_ref().is_some_and(|session| {
                session.owner_id == record.owner_id
                    && session.owner_generation == record.owner_generation
            })
    })
}

fn action_label(action: ArmaAdminAction) -> &'static str {
    match action {
        ArmaAdminAction::Players => "List players",
        ArmaAdminAction::Bans => "List bans",
        ArmaAdminAction::Missions => "List missions",
        ArmaAdminAction::Announce => "Announce message",
        ArmaAdminAction::Kick => "Kick player",
        ArmaAdminAction::AddBan => "Add ban",
        ArmaAdminAction::RemoveBan => "Remove ban",
        ArmaAdminAction::Lock => "Lock server",
        ArmaAdminAction::Unlock => "Unlock server",
        ArmaAdminAction::ChangeMission => "Change mission",
        ArmaAdminAction::RestartMission => "Restart mission",
    }
}

fn make_command(
    state: &crate::ui::app::server::arma_admin::ArmaAdminViewState,
) -> Result<ArmaAdminCommand, String> {
    let required = |value: &str, label: &str| {
        let value = value.trim();
        if value.is_empty() {
            Err(tr_fmt("Enter {field}.", &[("field", tr(label))]))
        } else {
            Ok(value.to_owned())
        }
    };
    match state.action {
        ArmaAdminAction::Players => Ok(ArmaAdminCommand::Players),
        ArmaAdminAction::Bans => Ok(ArmaAdminCommand::Bans),
        ArmaAdminAction::Missions => Ok(ArmaAdminCommand::Missions),
        ArmaAdminAction::Announce => Ok(ArmaAdminCommand::Announce {
            message: required(&state.message, "a message")?,
        }),
        ArmaAdminAction::Kick => Ok(ArmaAdminCommand::Kick {
            player_id: state
                .target
                .trim()
                .parse()
                .map_err(|_| tr("Enter a valid player ID."))?,
            expected_guid: required(&state.guid, "the player's GUID")?,
            reason: required(&state.reason, "a reason")?,
        }),
        ArmaAdminAction::AddBan => Ok(ArmaAdminCommand::AddBan {
            guid: required(&state.guid, "a GUID")?,
            minutes: state
                .minutes
                .trim()
                .parse()
                .map_err(|_| tr("Enter a valid ban duration in minutes."))?,
            reason: required(&state.reason, "a reason")?,
        }),
        ArmaAdminAction::RemoveBan => Ok(ArmaAdminCommand::RemoveBan {
            ban_id: state
                .target
                .trim()
                .parse()
                .map_err(|_| tr("Enter a valid ban ID."))?,
            expected_guid: required(&state.guid, "the banned player's GUID")?,
        }),
        ArmaAdminAction::Lock => Ok(ArmaAdminCommand::Lock),
        ArmaAdminAction::Unlock => Ok(ArmaAdminCommand::Unlock),
        ArmaAdminAction::ChangeMission => Ok(ArmaAdminCommand::ChangeMission {
            template: required(&state.mission, "a mission template")?,
        }),
        ArmaAdminAction::RestartMission => Ok(ArmaAdminCommand::RestartMission),
    }
}

impl Foxy {
    pub(super) fn render_arma_administration(&mut self, ui: &mut Ui) {
        if !crate::core::game::registry()
            .active()
            .server_capabilities()
            .arma_administration
        {
            return;
        }
        let Some(id) = self.server_view.selected.clone() else {
            return;
        };
        let Some(snapshot) = self.server_view.rcon.for_instance(&id).cloned() else {
            return;
        };
        let last_command = self
            .server_view
            .rcon
            .last_command
            .as_ref()
            .filter(|record| record.instance_id == id.as_str())
            .cloned();

        ui.separator();
        ui.heading(tr("Arma administration"));
        ui.label(tr(
            "Typed BattlEye RCon actions for the running Arma server.",
        ));
        let Some(run_generation) = snapshot.run_generation else {
            ui.label(tr(
                "Start this server with RCon enabled to use Arma administration.",
            ));
            return;
        };
        let connected = snapshot.session.as_ref().is_some_and(|session| {
            session.connection == crate::core::server::owner::RconConnection::Connected
        });
        if !connected {
            ui.label(tr(
                "Arma administration is unavailable until the RCon session is connected.",
            ));
        }

        let mut state = std::mem::take(&mut self.server_view.arma_admin);
        if state.confirmation.as_ref().is_some_and(|confirmation| {
            confirmation.instance_id.as_str() != id.as_str()
                || confirmation.run_generation != run_generation
                || snapshot.session.as_ref().is_none_or(|session| {
                    session.owner_id != confirmation.owner_id
                        || session.owner_generation != confirmation.owner_generation
                })
        }) {
            state.confirmation = None;
        }
        ui.add_enabled_ui(state.confirmation.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Action"));
                egui::ComboBox::from_id_salt(("arma-admin-action", id.as_str()))
                    .selected_text(tr(action_label(state.action)))
                    .show_ui(ui, |ui| {
                        for action in [
                            ArmaAdminAction::Players,
                            ArmaAdminAction::Bans,
                            ArmaAdminAction::Missions,
                            ArmaAdminAction::Announce,
                            ArmaAdminAction::Kick,
                            ArmaAdminAction::AddBan,
                            ArmaAdminAction::RemoveBan,
                            ArmaAdminAction::Lock,
                            ArmaAdminAction::Unlock,
                            ArmaAdminAction::ChangeMission,
                            ArmaAdminAction::RestartMission,
                        ] {
                            ui.selectable_value(
                                &mut state.action,
                                action,
                                tr(action_label(action)),
                            );
                        }
                    });
            })
        });

        ui.add_enabled_ui(state.confirmation.is_none(), |ui| match state.action {
            ArmaAdminAction::Announce => {
                ui.label(tr("Message"));
                ui.add(egui::TextEdit::multiline(&mut state.message).desired_rows(3).char_limit(1024));
            }
            ArmaAdminAction::Kick => {
                ui.horizontal(|ui| {
                    ui.label(tr("Player ID"));
                    ui.add(egui::TextEdit::singleline(&mut state.target).desired_width(100.0).char_limit(10));
                    ui.label(tr("Expected GUID"));
                    ui.add(egui::TextEdit::singleline(&mut state.guid).desired_width(220.0).char_limit(128));
                });
                ui.label(tr("Reason"));
                ui.add(egui::TextEdit::singleline(&mut state.reason).desired_width(360.0).char_limit(512));
            }
            ArmaAdminAction::AddBan => {
                ui.horizontal(|ui| {
                    ui.label(tr("GUID"));
                    ui.add(egui::TextEdit::singleline(&mut state.guid).desired_width(220.0).char_limit(128));
                    ui.label(tr("Minutes"));
                    ui.add(egui::TextEdit::singleline(&mut state.minutes).desired_width(100.0).char_limit(10));
                });
                ui.label(tr("Reason"));
                ui.add(egui::TextEdit::singleline(&mut state.reason).desired_width(360.0).char_limit(512));
            }
            ArmaAdminAction::RemoveBan => {
                ui.horizontal(|ui| {
                    ui.label(tr("Ban ID"));
                    ui.add(egui::TextEdit::singleline(&mut state.target).desired_width(100.0).char_limit(10));
                    ui.label(tr("Expected GUID"));
                    ui.add(egui::TextEdit::singleline(&mut state.guid).desired_width(220.0).char_limit(128));
                });
            }
            ArmaAdminAction::ChangeMission => {
                ui.label(tr("Mission template"));
                ui.add(egui::TextEdit::singleline(&mut state.mission).desired_width(360.0).char_limit(256));
                ui.label(tr("Changing mission does not restart the server. Deploy the new mission before selecting it."));
            }
            _ => {}
        });

        if let Some(error) = &state.action_error {
            ui.colored_label(self.color_text_error(), tr(error));
        }
        let busy = self.server_view.controls_busy();
        let mut send_command = None;
        ui.horizontal(|ui| {
            let preview_button = ui.add_enabled(
                !busy && connected && state.confirmation.is_none(),
                egui::Button::new(tr("Review command")),
            );
            focus_outline(ui, &preview_button);
            if preview_button.clicked() {
                match make_command(&state).and_then(|command| {
                    let wire_text = command.wire_text()?;
                    if let Some(required) = command.required_preflight() {
                        let records =
                            current_records(&snapshot, last_command.as_ref(), run_generation);
                        let response =
                            latest_reply(records, required, run_generation).ok_or_else(|| {
                                tr_fmt(
                                    "First request a current {query} list.",
                                    &[("query", tr(required))],
                                )
                            })?;
                        command.validate_preflight(response)?;
                    }
                    Ok((command, wire_text))
                }) {
                    Ok((command, wire_text)) => {
                        state.action_error = None;
                        state.confirmation = Some(ArmaAdminConfirmation {
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
                ui.label(tr("The result may be unknown if RCon loses its reply. Check the server before repeating an action."));
                if matches!(&confirmation.command, ArmaAdminCommand::ChangeMission { .. } | ArmaAdminCommand::RestartMission) {
                    ui.label(tr("This changes or restarts the mission only. It does not restart the server."));
                }
                let mut cancel = None;
                ui.horizontal(|ui| {
                    let send = ui.add_enabled(!busy && connected, egui::Button::new(tr("Send once")));
                    focus_outline(ui, &send);
                    let safe = ui.button(tr("Cancel"));
                    if state.focus_cancel {
                        safe.request_focus();
                        safe.scroll_to_me(Some(egui::Align::Center));
                        state.focus_cancel = false;
                    }
                    focus_outline(ui, &safe);
                    if send.clicked() {
                        send_command = Some(confirmation);
                    }
                    if safe.clicked() {
                        cancel = Some(());
                    }
                });
                if cancel.is_some() {
                    state.confirmation = None;
                }
            });
        }

        let records = current_records(&snapshot, last_command.as_ref(), run_generation);
        render_submitted_outcome(ui, records.clone(), state.last_submitted.as_deref());
        let records = current_records(&snapshot, last_command.as_ref(), run_generation);
        render_admin_result(ui, records);

        self.server_view.arma_admin = state;
        if let Some(confirmation) = send_command {
            self.server_view.arma_admin.last_submitted = Some(confirmation.wire_text.clone());
            self.start_server_task(ServerTaskAction::SendArmaAdminCommand {
                id: confirmation.instance_id,
                run_generation: confirmation.run_generation,
                command: confirmation.command,
            });
        }
    }
}

fn render_submitted_outcome<'a>(
    ui: &mut Ui,
    records: impl Iterator<Item = &'a RconCommandRecord>,
    submitted: Option<&str>,
) {
    let Some(submitted) = submitted else {
        return;
    };
    let Some(record) = records
        .filter(|record| record.text == submitted)
        .max_by_key(|record| record.requested_unix_ms)
    else {
        return;
    };
    ui.group(|ui| {
        ui.label(RichText::new(tr_fmt(
            "Latest administration command: {state}",
            &[("state", tr(match record.state {
                RconCommandState::Queued => "Queued",
                RconCommandState::Sending => "Sending",
                RconCommandState::Replied => "Reply received",
                RconCommandState::Unknown => "Outcome unknown",
                RconCommandState::NotSent => "Not sent",
            }))],
        )).strong());
        match record.state {
            RconCommandState::Unknown => ui.label(tr("The action may or may not have taken effect. Check the server before sending it again.")),
            RconCommandState::Replied => ui.label(tr("A reply arrived. This does not by itself confirm that the requested effect completed.")),
            RconCommandState::NotSent => ui.label(tr("The command was not sent.")),
            RconCommandState::Queued | RconCommandState::Sending => ui.label(tr("The command is still in progress.")),
        };
        if let Some(response) = record.response.as_deref() {
            egui::CollapsingHeader::new(tr("Administration command response"))
                .id_salt(("arma-admin-command-result", &record.command_id))
                .show(ui, |ui| {
                    ui.add(egui::Label::new(response).wrap().selectable(true));
                    if record.response_truncated {
                        ui.label(tr("The stored response was truncated."));
                    }
                });
        }
    });
}

fn render_admin_result<'a>(ui: &mut Ui, records: impl Iterator<Item = &'a RconCommandRecord>) {
    let mut records = records.collect::<Vec<_>>();
    records.sort_by_key(|record| record.requested_unix_ms);
    let Some(record) = records.into_iter().rev().find(|record| {
        ["players", "bans", "missions"]
            .iter()
            .any(|query| record.text.trim().eq_ignore_ascii_case(query))
    }) else {
        return;
    };
    ui.group(|ui| {
        ui.label(RichText::new(tr_fmt("Latest {query} response", &[("query", record.text.clone())])).strong());
        if record.state != RconCommandState::Replied {
            ui.label(tr("No complete reply was received. The command outcome is not confirmed."));
            return;
        }
        let Some(response) = record.response.as_deref() else {
            ui.label(tr("The server replied without stored response text. An empty acknowledgement does not confirm a completed effect."));
            return;
        };
        if record.response_truncated {
            ui.label(tr("The stored response was truncated; parsed rows may be incomplete."));
        }
        match record.text.trim().to_ascii_lowercase().as_str() {
            "players" => render_players(ui, response),
            "bans" => render_bans(ui, response),
            _ => render_missions(ui, response),
        }
        egui::CollapsingHeader::new(tr("Raw RCon response"))
            .id_salt(("arma-admin-response", &record.command_id))
            .show(ui, |ui| {
                ui.add(egui::Label::new(response).wrap().selectable(true));
            });
    });
}

fn render_players(ui: &mut Ui, response: &str) {
    let roster: ArmaRoster = parse_players(response);
    ui.label(tr_fmt(
        "Players: {count}",
        &[("count", roster.players.len().to_string())],
    ));
    ui.label(tr(if roster.complete {
        "The player list is complete."
    } else {
        "The player list may be incomplete."
    }));
    for player in roster.players.iter().take(MAX_ADMIN_ROWS) {
        ui.label(tr_fmt(
            "ID {id}: {name}, {address}, ping {ping} ms",
            &[
                ("id", player.player_id.to_string()),
                ("name", player.name.clone()),
                ("address", player.address.clone()),
                ("ping", player.ping.to_string()),
            ],
        ));
        if let Some(guid) = &player.guid {
            ui.label(tr_fmt(
                "GUID {guid}; verified: {verified}",
                &[
                    ("guid", guid.clone()),
                    ("verified", tr(if player.verified { "yes" } else { "no" })),
                ],
            ));
        }
    }
    if roster.players.len() > MAX_ADMIN_ROWS {
        ui.label(tr_fmt(
            "Showing the first {count} rows.",
            &[("count", MAX_ADMIN_ROWS.to_string())],
        ));
    }
    if roster.players.is_empty() && response.trim().is_empty() {
        ui.label(tr(
            "The reply was empty; this does not confirm an empty player list.",
        ));
    } else if !roster.complete {
        ui.label(tr(
            "The format was not fully recognized; review the raw response.",
        ));
    }
}

fn render_bans(ui: &mut Ui, response: &str) {
    let bans: ArmaBanList = parse_bans(response);
    ui.label(tr_fmt(
        "Bans: {count}",
        &[("count", bans.bans.len().to_string())],
    ));
    ui.label(tr(if bans.complete {
        "The ban list is complete."
    } else {
        "The ban list may be incomplete."
    }));
    for ban in bans.bans.iter().take(MAX_ADMIN_ROWS) {
        ui.label(tr_fmt(
            "Ban {id}: {guid}, {minutes} minutes, {reason}",
            &[
                ("id", ban.ban_id.to_string()),
                (
                    "guid",
                    ban.guid
                        .clone()
                        .or_else(|| ban.address.clone())
                        .unwrap_or_else(|| tr("unknown target")),
                ),
                ("minutes", ban.minutes_left.clone()),
                ("reason", ban.reason.clone()),
            ],
        ));
    }
    if bans.bans.len() > MAX_ADMIN_ROWS {
        ui.label(tr_fmt(
            "Showing the first {count} rows.",
            &[("count", MAX_ADMIN_ROWS.to_string())],
        ));
    }
    if bans.bans.is_empty() && response.trim().is_empty() {
        ui.label(tr(
            "The reply was empty; this does not confirm an empty ban list.",
        ));
    } else if !bans.complete {
        ui.label(tr(
            "The format was not fully recognized; review the raw response.",
        ));
    }
}

fn render_missions(ui: &mut Ui, response: &str) {
    let missions: ArmaMissionList = parse_missions(response);
    ui.label(tr_fmt(
        "Missions: {count}",
        &[("count", missions.missions.len().to_string())],
    ));
    ui.label(tr(if missions.complete {
        "The mission list is complete."
    } else {
        "The mission list may be incomplete."
    }));
    for mission in missions.missions.iter().take(MAX_ADMIN_ROWS) {
        ui.label(mission);
    }
    if missions.missions.len() > MAX_ADMIN_ROWS {
        ui.label(tr_fmt(
            "Showing the first {count} rows.",
            &[("count", MAX_ADMIN_ROWS.to_string())],
        ));
    }
    if missions.missions.is_empty() && response.trim().is_empty() {
        ui.label(tr(
            "The reply was empty; this does not confirm an empty mission list.",
        ));
    } else if !missions.complete {
        ui.label(tr(
            "The format was not fully recognized; review the raw response.",
        ));
    }
}
