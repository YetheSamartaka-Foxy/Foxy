use serde_json::{Value, json};

use super::instances::{capture, store_error, success};
use super::lifecycle::attach_existing;
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerReforgerAdminBanArgs, ServerReforgerAdminBansArgs,
    ServerReforgerAdminCommand as AdminRequest, ServerReforgerAdminIdentityArgs,
    ServerReforgerAdminKickArgs, ServerReforgerAdminRequestArgs,
};
use crate::cli::exit_codes;
use crate::core::server::rcon::{
    RconCommandState,
    reforger::{ReforgerAdminCommand, parse_bans, parse_players, response_rejected},
};
use crate::core::server::{ServerContext, ServerInstanceId, ServerStore};

struct AdminOperation {
    verb: &'static str,
    request: ServerReforgerAdminRequestArgs,
    command: ReforgerAdminCommand,
}

pub(super) fn run(
    cli: &CliArgs,
    context_args: ServerContextArgs,
    request: AdminRequest,
) -> Result<CommandSuccess, CommandError> {
    let operation = match request {
        AdminRequest::Players(args) => AdminOperation {
            verb: "players",
            request: args,
            command: ReforgerAdminCommand::Players,
        },
        AdminRequest::Bans(args) => bans(args),
        AdminRequest::Kick(args) => kick(args),
        AdminRequest::Ban(args) => ban(args),
        AdminRequest::RemoveBan(args) => remove_ban(args),
        AdminRequest::ReloadScenario(args) => AdminOperation {
            verb: "reload-scenario",
            request: args,
            command: ReforgerAdminCommand::ReloadScenario,
        },
        AdminRequest::Logout(args) => AdminOperation {
            verb: "logout",
            request: args,
            command: ReforgerAdminCommand::Logout,
        },
    };
    let AdminOperation {
        verb,
        request,
        command,
    } = operation;
    let action = format!("server.reforger-admin.{verb}");
    let wire_text = command
        .wire_text()
        .map_err(|message| CommandError::validation(&action, message))?;
    let id = parse_id(request.id.clone(), &action)?;
    if request.expected_run_generation == Some(0) {
        return Err(CommandError::validation(
            &action,
            "--expected-run-generation must be greater than zero",
        ));
    }
    let side_effect = !matches!(
        command,
        ReforgerAdminCommand::Players | ReforgerAdminCommand::Bans { .. }
    );
    if side_effect && request.expected_run_generation.is_none() {
        return Err(CommandError::validation(
            &action,
            "Side-effecting Reforger administration requires --expected-run-generation",
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
                "expected_run_generation": request.expected_run_generation,
                "owner_contacted": false,
                "database_opened": false,
                "writes_performed": false,
            }),
        ));
    }
    if side_effect && !cli.yes {
        return Err(CommandError::validation(
            &action,
            "Side-effecting Reforger administration requires --yes",
        ));
    }

    let attachment = attach_existing(&context, &action)?;
    let run_generation = match request.expected_run_generation {
        Some(generation) => generation,
        None => current_run_generation(&attachment, &id, &action)?,
    };
    if command == ReforgerAdminCommand::ReloadScenario {
        let outcome = attachment
            .run_instance(
                foxy_supervisor::control::Operation::ReloadReforgerScenario {
                    instance_id: id.as_str().into(),
                    expected_run_generation: run_generation,
                    timeout_ms: request.timeout_ms,
                },
                &crate::core::server::ContentStageControl::default(),
            )
            .map_err(|error| CommandError::operation(&action, error.to_string()))?;
        return Ok(CommandSuccess {
            action,
            message: outcome.message.clone().unwrap_or_default(),
            exit_code: if outcome.completed {
                exit_codes::SUCCESS
            } else {
                exit_codes::PARTIAL_SUCCESS
            },
            data: json!({"outcome": outcome, "database_opened": false, "process_restarted": false}),
        });
    }
    let reply = match attachment.reforger_admin_command(
        &id,
        run_generation,
        &command,
        request.timeout_ms,
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
    let transcript = record
        .as_ref()
        .map(|record| {
            record
                .messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let mut parsed = match &command {
        ReforgerAdminCommand::Players => {
            let result = parse_players(&transcript);
            json!({"format":"players", "complete":result.complete, "players":result.players, "raw_transcript":result.raw})
        }
        ReforgerAdminCommand::Bans { page } => {
            let result = parse_bans(&transcript, *page);
            json!({"format":"bans", "page":page, "complete":result.complete, "bans":result.bans, "raw_transcript":result.raw})
        }
        _ => Value::Null,
    };
    if parsed.is_object()
        && record
            .as_ref()
            .is_some_and(|record| record.messages.iter().any(|message| message.truncated))
    {
        parsed["complete"] = false.into();
    }
    let is_list = matches!(
        command,
        ReforgerAdminCommand::Players | ReforgerAdminCommand::Bans { .. }
    );
    let incomplete_list = is_list && parsed.get("complete") != Some(&Value::Bool(true));
    let rejected = response_rejected(&transcript);
    let (message, exit_code) = match state {
        Some(RconCommandState::Replied) if rejected => (
            "The server rejected the command; its native message is retained in the transcript.".into(),
            exit_codes::OPERATION_FAILED,
        ),
        Some(RconCommandState::Replied) if incomplete_list => (
            "The server replied, but did not provide a proven complete list; the raw transcript is retained.".into(),
            exit_codes::PARTIAL_SUCCESS,
        ),
        Some(RconCommandState::Replied) if side_effect => (
            "The server returned an RCon reply. The command effect is not independently verified.".into(),
            exit_codes::PARTIAL_SUCCESS,
        ),
        Some(RconCommandState::Replied) => (
            "The server returned a semantic RCon message.".into(),
            exit_codes::SUCCESS,
        ),
        Some(RconCommandState::Unknown) => (
            "The command was sent once but its outcome is unknown. Check the server before retrying.".into(),
            exit_codes::PARTIAL_SUCCESS,
        ),
        Some(RconCommandState::NotSent) => (
            "The command was not sent by the owner.".into(),
            exit_codes::OPERATION_FAILED,
        ),
        Some(RconCommandState::Queued | RconCommandState::Sending) | None => (
            "The owner returned no terminal command outcome; check RCon history before retrying.".into(),
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
            "raw_transcript": transcript,
            "error": reply.error,
            "database_opened": false,
            "effect_verified": false,
        }),
        exit_code,
    })
}

fn bans(args: ServerReforgerAdminBansArgs) -> AdminOperation {
    AdminOperation {
        verb: "bans",
        request: args.request,
        command: ReforgerAdminCommand::Bans { page: args.page },
    }
}

fn kick(args: ServerReforgerAdminKickArgs) -> AdminOperation {
    AdminOperation {
        verb: "kick",
        request: args.request,
        command: ReforgerAdminCommand::Kick {
            player_id: args.player_id,
            expected_identity: args.expected_identity,
        },
    }
}

fn ban(args: ServerReforgerAdminBanArgs) -> AdminOperation {
    AdminOperation {
        verb: "ban",
        request: args.request,
        command: ReforgerAdminCommand::Ban {
            identity: args.identity,
            duration_seconds: args.duration_seconds,
            reason: args.reason,
        },
    }
}

fn remove_ban(args: ServerReforgerAdminIdentityArgs) -> AdminOperation {
    AdminOperation {
        verb: "remove-ban",
        request: args.request,
        command: ReforgerAdminCommand::RemoveBan {
            identity: args.identity,
        },
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

fn require_capability(context: &ServerContext, action: &str) -> Result<(), CommandError> {
    let available = crate::core::game::registry()
        .get(context.game_id())
        .is_some_and(|game| game.server_capabilities().reforger_administration);
    if available {
        Ok(())
    } else {
        Err(CommandError::validation(
            action,
            "Typed Reforger administration is unavailable for this game space or platform",
        ))
    }
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}
