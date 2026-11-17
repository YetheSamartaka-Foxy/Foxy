use serde_json::{Value, json};

use super::instances::{capture, store_error, success};
use super::lifecycle::attach_existing;
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerArmaAdminAddBanArgs, ServerArmaAdminCommand as AdminRequest,
    ServerArmaAdminKickArgs, ServerArmaAdminMessageArgs, ServerArmaAdminMissionArgs,
    ServerArmaAdminRemoveBanArgs, ServerArmaAdminRequestArgs, ServerContextArgs,
};
use crate::cli::exit_codes;
use crate::core::server::rcon::{
    RconCommandState,
    arma3::{ArmaAdminCommand, parse_bans, parse_missions, parse_players},
};
use crate::core::server::{ServerContext, ServerInstanceId, ServerStore};

struct AdminOperation {
    verb: &'static str,
    request: ServerArmaAdminRequestArgs,
    command: ArmaAdminCommand,
    side_effect: bool,
}

pub(super) fn run(
    cli: &CliArgs,
    context_args: ServerContextArgs,
    request: AdminRequest,
) -> Result<CommandSuccess, CommandError> {
    let AdminOperation {
        verb,
        request: request_args,
        command,
        side_effect,
    } = match request {
        AdminRequest::Players(args) => AdminOperation {
            verb: "players",
            request: args,
            command: ArmaAdminCommand::Players,
            side_effect: false,
        },
        AdminRequest::Bans(args) => AdminOperation {
            verb: "bans",
            request: args,
            command: ArmaAdminCommand::Bans,
            side_effect: false,
        },
        AdminRequest::Missions(args) => AdminOperation {
            verb: "missions",
            request: args,
            command: ArmaAdminCommand::Missions,
            side_effect: false,
        },
        AdminRequest::Announce(args) => announce(args),
        AdminRequest::Kick(args) => kick(args),
        AdminRequest::AddBan(args) => add_ban(args),
        AdminRequest::RemoveBan(args) => remove_ban(args),
        AdminRequest::Lock(args) => AdminOperation {
            verb: "lock",
            request: args,
            command: ArmaAdminCommand::Lock,
            side_effect: true,
        },
        AdminRequest::Unlock(args) => AdminOperation {
            verb: "unlock",
            request: args,
            command: ArmaAdminCommand::Unlock,
            side_effect: true,
        },
        AdminRequest::Mission(args) => change_mission(args),
        AdminRequest::RestartMission(args) => AdminOperation {
            verb: "restart-mission",
            request: args,
            command: ArmaAdminCommand::RestartMission,
            side_effect: true,
        },
    };
    let action = format!("server.admin.{verb}");
    let wire_text = command
        .wire_text()
        .map_err(|message| CommandError::validation(&action, message))?;
    let id = parse_id(request_args.id.clone(), &action)?;
    if request_args.expected_run_generation == Some(0) {
        return Err(CommandError::validation(
            &action,
            "--expected-run-generation must be greater than zero",
        ));
    }
    if side_effect && request_args.expected_run_generation.is_none() {
        return Err(CommandError::validation(
            &action,
            "Side-effecting Arma administration requires --expected-run-generation",
        ));
    }

    let context = capture(context_args, &action)?;
    require_capability(&context, &action)?;
    if cli.dry_run {
        return Ok(success(
            &action,
            "Dry run: the typed command was validated; no owner was contacted and nothing was sent."
                .into(),
            json!({
                "dry_run": true,
                "instance_id": id,
                "command": command,
                "wire_text": wire_text,
                "expected_run_generation": request_args.expected_run_generation,
                "owner_contacted": false,
                "database_opened": false,
                "writes_performed": false,
            }),
        ));
    }
    if side_effect && !cli.yes {
        return Err(CommandError::validation(
            &action,
            "Side-effecting Arma administration requires --yes",
        ));
    }

    let attachment = attach_existing(&context, &action)?;
    let run_generation = match request_args.expected_run_generation {
        Some(generation) => generation,
        None => current_run_generation(&attachment, &id, &action)?,
    };
    let reply = match attachment.arma_admin_command(
        &id,
        run_generation,
        &command,
        request_args.timeout_ms,
    ) {
        Ok(reply) => reply,
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            return Ok(CommandSuccess {
                action,
                message: "The owner request timed out; the command outcome is unknown. Check RCon history before retrying.".into(),
                data: json!({
                    "command_id": null,
                    "state": "unknown",
                    "reason": "owner_reply_timeout",
                    "record": null,
                    "run_generation": run_generation,
                    "parsed": null,
                    "database_opened": false,
                }),
                exit_code: exit_codes::PARTIAL_SUCCESS,
            });
        }
        Err(error) => return Err(CommandError::operation(&action, error.to_string())),
    };

    let store = ServerStore::new(context).map_err(|error| store_error(&action, error))?;
    let record = reply
        .command_id
        .as_deref()
        .map(|command_id| {
            store
                .rcon_command(&id, command_id)
                .map_err(|error| store_error(&action, error))
        })
        .transpose()?
        .flatten();
    let state = reply
        .state
        .or_else(|| record.as_ref().map(|record| record.state));
    let state =
        if reply.error.is_some() && state.is_some_and(|state| state != RconCommandState::NotSent) {
            Some(RconCommandState::Unknown)
        } else {
            state
        };
    let reason = reply
        .reason
        .or_else(|| record.as_ref().and_then(|record| record.reason));
    let response = record
        .as_ref()
        .and_then(|record| record.response.as_deref());
    let parsed = match (state, response) {
        (Some(RconCommandState::Replied), Some(response))
            if is_list_command(&command)
                && record
                    .as_ref()
                    .is_some_and(|record| record.response_truncated) =>
        {
            unknown_format(response)
        }
        (Some(RconCommandState::Replied), Some(response)) => parsed_reply(&command, response),
        (Some(RconCommandState::Replied), None) if is_list_command(&command) => unknown_format(""),
        _ => Value::Null,
    };
    let unparsed_list = state == Some(RconCommandState::Replied)
        && is_list_command(&command)
        && parsed.get("complete") == Some(&Value::Bool(false));

    let (message, exit_code) = match state {
        Some(RconCommandState::Replied) if response.is_some_and(|response| response.trim().eq_ignore_ascii_case("Unknown command")) => (
            "The server rejected the command as unknown; its reply is retained in the command record.".into(),
            exit_codes::OPERATION_FAILED,
        ),
        Some(RconCommandState::Replied) if unparsed_list => (
            "The server replied, but the response format was not recognized completely; the raw reply is retained in the command record.".into(),
            exit_codes::PARTIAL_SUCCESS,
        ),
        Some(RconCommandState::Replied) if side_effect => (
            "The server returned an RCon reply. The command effect is not independently verified.".into(),
            exit_codes::SUCCESS,
        ),
        Some(RconCommandState::Replied) => (
            "The server returned a complete RCon reply.".into(),
            exit_codes::SUCCESS,
        ),
        Some(RconCommandState::Unknown) => (
            format!(
                "The command was sent once but its outcome is unknown: {} Check the server before retrying.",
                reply.error.as_deref().unwrap_or_else(|| {
                    reason.map(|reason| reason.describe()).unwrap_or("no complete reply arrived.")
                })
            ),
            exit_codes::PARTIAL_SUCCESS,
        ),
        Some(RconCommandState::NotSent) => (
            format!(
                "The command was not sent: {}",
                reply.error.as_deref().unwrap_or_else(|| {
                    reason.map(|reason| reason.describe()).unwrap_or("the owner refused the request")
                })
            ),
            exit_codes::OPERATION_FAILED,
        ),
        Some(RconCommandState::Queued | RconCommandState::Sending) | None => (
            "The owner returned no terminal command outcome; check RCon history before retrying."
                .into(),
            exit_codes::PARTIAL_SUCCESS,
        ),
    };
    if record.is_none() && reply.command_id.is_some() {
        return Err(CommandError::operation(
            &action,
            "The owner returned a command id but its durable command record is unavailable",
        ));
    }
    if record.is_none() && reply.command_id.is_none() {
        return Err(CommandError::operation(
            &action,
            reply
                .error
                .unwrap_or_else(|| "The owner returned no command record".into()),
        ));
    }

    Ok(CommandSuccess {
        action,
        message,
        data: json!({
            "command_id": reply.command_id,
            "state": state,
            "reason": reason,
            "record": record,
            "run_generation": run_generation,
            "parsed": parsed,
            "error": reply.error,
            "database_opened": false,
        }),
        exit_code,
    })
}

fn announce(args: ServerArmaAdminMessageArgs) -> AdminOperation {
    AdminOperation {
        verb: "announce",
        request: args.request,
        command: ArmaAdminCommand::Announce {
            message: args.message,
        },
        side_effect: true,
    }
}

fn kick(args: ServerArmaAdminKickArgs) -> AdminOperation {
    AdminOperation {
        verb: "kick",
        request: args.request,
        command: ArmaAdminCommand::Kick {
            player_id: args.player_id,
            expected_guid: args.expected_guid,
            reason: args.reason,
        },
        side_effect: true,
    }
}

fn add_ban(args: ServerArmaAdminAddBanArgs) -> AdminOperation {
    AdminOperation {
        verb: "add-ban",
        request: args.request,
        command: ArmaAdminCommand::AddBan {
            guid: args.guid,
            minutes: args.minutes,
            reason: args.reason,
        },
        side_effect: true,
    }
}

fn remove_ban(args: ServerArmaAdminRemoveBanArgs) -> AdminOperation {
    AdminOperation {
        verb: "remove-ban",
        request: args.request,
        command: ArmaAdminCommand::RemoveBan {
            ban_id: args.ban_id,
            expected_guid: args.expected_guid,
        },
        side_effect: true,
    }
}

fn change_mission(args: ServerArmaAdminMissionArgs) -> AdminOperation {
    AdminOperation {
        verb: "mission",
        request: args.request,
        command: ArmaAdminCommand::ChangeMission {
            template: args.template,
        },
        side_effect: true,
    }
}

fn current_run_generation(
    attachment: &crate::core::server::owner::Attachment,
    id: &ServerInstanceId,
    action: &str,
) -> Result<u64, CommandError> {
    let status = attachment.instance_status(id).map_err(|error| {
        CommandError::operation(
            action,
            format!("Cannot read the current run status: {error}"),
        )
    })?;
    if status.run_generation == 0 || !status.process_exists {
        return Err(CommandError::operation(
            action,
            "The server is not running in this owner",
        ));
    }
    Ok(status.run_generation)
}

fn parsed_reply(command: &ArmaAdminCommand, response: &str) -> Value {
    match command {
        ArmaAdminCommand::Players => {
            let parsed = parse_players(response);
            if parsed.complete {
                json!({"format": "players", "complete": true, "players": parsed.players})
            } else {
                unknown_format(response)
            }
        }
        ArmaAdminCommand::Bans => {
            let parsed = parse_bans(response);
            if parsed.complete {
                json!({"format": "bans", "complete": true, "bans": parsed.bans})
            } else {
                unknown_format(response)
            }
        }
        ArmaAdminCommand::Missions => {
            let parsed = parse_missions(response);
            if parsed.complete {
                json!({"format": "missions", "complete": true, "missions": parsed.missions})
            } else {
                unknown_format(response)
            }
        }
        _ => Value::Null,
    }
}

fn unknown_format(response: &str) -> Value {
    json!({"format": "unknown", "complete": false, "raw_response": response})
}

fn is_list_command(command: &ArmaAdminCommand) -> bool {
    matches!(
        command,
        ArmaAdminCommand::Players | ArmaAdminCommand::Bans | ArmaAdminCommand::Missions
    )
}

fn require_capability(context: &ServerContext, action: &str) -> Result<(), CommandError> {
    let available = crate::core::game::registry()
        .get(context.game_id())
        .is_some_and(|game| game.server_capabilities().arma_administration);
    if available {
        Ok(())
    } else {
        Err(CommandError::validation(
            action,
            "Typed Arma administration is unavailable for this game space or platform",
        ))
    }
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}
