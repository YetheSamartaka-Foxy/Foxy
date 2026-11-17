use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::json;

use super::instances::{capture, store_error, success};
use super::lifecycle::attach_existing;
use super::steam_auth::input::PrivateInput;
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerRconCommand, ServerRconConfigureArgs,
    ServerRconPermissionArg, ServerRconProbeArgs,
};
use crate::cli::exit_codes;
use crate::core::server::rcon::{
    self, CommandOutcome, LoginFailure, MAX_COMMAND_BYTES, RconCommandState, RconPassword,
    RconSettingsInput, Retransmit, Session, response_text,
};
use crate::core::server::{
    ContentStageControl, ReforgerRconPermission, ServerContext, ServerDefinitionKind,
    ServerInstanceId, ServerStore,
};

pub(super) fn run(
    cli: &CliArgs,
    args: ServerContextArgs,
    command: ServerRconCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerRconCommand::Status(_) => "server.rcon.status",
        ServerRconCommand::Configure(_) => "server.rcon.configure",
        ServerRconCommand::Disable(_) => "server.rcon.disable",
        ServerRconCommand::SetPassword(_) => "server.rcon.set-password",
        ServerRconCommand::Test(_) => "server.rcon.test",
        ServerRconCommand::Command(_) => "server.rcon.command",
        ServerRconCommand::History(_) => "server.rcon.history",
        ServerRconCommand::Probe(_) => "server.rcon.probe",
    };
    let context = capture(args, action)?;
    if !crate::core::game::registry()
        .get(context.game_id())
        .is_some_and(|game| game.server_capabilities().lifecycle)
    {
        return Err(CommandError::validation(
            action,
            "RCon is available for game spaces with a native server lifecycle.",
        ));
    }
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    match command {
        ServerRconCommand::Status(args) => {
            status(action, &context, &store, &parse_id(args.id, action)?)
        }
        ServerRconCommand::Configure(args) => configure(cli, action, &store, args),
        ServerRconCommand::Disable(args) => {
            let id = parse_id(args.id, action)?;
            if !cli.yes && !cli.dry_run {
                return Err(CommandError::validation(
                    action,
                    "Removing the RCon settings requires --yes",
                ));
            }
            let instance = store
                .show(&id)
                .map_err(|error| store_error(action, error))?;
            if cli.dry_run {
                return Ok(success(
                    action,
                    "Dry run: the RCon settings would be removed from the draft.".into(),
                    json!({"dry_run": true, "revision": instance.revision}),
                ));
            }
            let _claim = super::content::claim(&store, action)?;
            let saved = store
                .configure_rcon(&id, args.expected_revision, None)
                .map_err(|error| store_error(action, error))?;
            Ok(success(
                action,
                format!(
                    "Removed RCon from draft revision {}. Commit, prepare and activate, then restart the server to apply.",
                    saved.revision
                ),
                json!({"revision": saved.revision, "rcon": null}),
            ))
        }
        ServerRconCommand::SetPassword(args) => {
            let id = parse_id(args.id, action)?;
            let instance = store
                .show(&id)
                .map_err(|error| store_error(action, error))?;
            if rcon::draft_settings(&instance).is_none() {
                return Err(CommandError::validation(
                    action,
                    "Configure the RCon port first with server rcon configure",
                ));
            }
            let password = read_password(cli, action)?;
            if cli.dry_run {
                return Ok(success(
                    action,
                    "Dry run: the password was read and validated; nothing was stored.".into(),
                    json!({"dry_run": true, "revision": instance.revision}),
                ));
            }
            let _claim = super::content::claim(&store, action)?;
            let saved = store
                .set_rcon_password(&id, args.expected_revision, &password)
                .map_err(|error| store_error(action, error))?;
            Ok(success(
                action,
                format!(
                    "Stored a new protected RCon password for draft revision {}. Commit, prepare and activate, then restart the server to use it.",
                    saved.revision
                ),
                json!({"revision": saved.revision, "password_stored": true}),
            ))
        }
        ServerRconCommand::Test(args) => test(action, &context, &parse_id(args.id, action)?),
        ServerRconCommand::Command(args) => {
            let id = parse_id(args.id, action)?;
            validate_command_text(action, &args.text)?;
            send_command(
                cli,
                action,
                &context,
                &store,
                &id,
                &args.text,
                args.timeout_ms,
            )
        }
        ServerRconCommand::History(args) => {
            let id = parse_id(args.id, action)?;
            let listing = store
                .rcon_history(&id)
                .map_err(|error| store_error(action, error))?;
            let mut lines = Vec::new();
            for record in listing.records.iter().take(20) {
                lines.push(format!(
                    "{} {} {}{}",
                    record.command_id,
                    state_label(record.state),
                    record.text,
                    record
                        .reason
                        .map(|reason| format!(" ({})", reason.describe()))
                        .unwrap_or_default()
                ));
            }
            let message = if lines.is_empty() {
                "No RCon commands are recorded for this instance.".to_string()
            } else {
                lines.join("\n")
            };
            Ok(success(action, message, json!({"history": listing})))
        }
        ServerRconCommand::Probe(args) => probe(cli, action, &store, args),
    }
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}

pub(super) fn state_label(state: RconCommandState) -> &'static str {
    match state {
        RconCommandState::Queued => "queued",
        RconCommandState::Sending => "sending",
        RconCommandState::Replied => "replied",
        RconCommandState::Unknown => "outcome unknown",
        RconCommandState::NotSent => "not sent",
    }
}

fn status(
    action: &str,
    context: &ServerContext,
    store: &ServerStore,
    id: &ServerInstanceId,
) -> Result<CommandSuccess, CommandError> {
    let instance = store.show(id).map_err(|error| store_error(action, error))?;
    let draft = rcon::draft_settings(&instance);
    let password_available = store
        .rcon_password_available(id)
        .map_err(|error| store_error(action, error))?;
    let (session, session_error, owner_contacted) =
        match crate::core::server::owner::attach(context) {
            Ok(Some(attachment)) => match attachment.rcon_inspect(id) {
                Ok(reply) => (reply.status, reply.error, true),
                Err(error) => (None, Some(error.to_string()), true),
            },
            Ok(None) => (
                None,
                Some("No owner is running for this game space".to_string()),
                false,
            ),
            Err(error) => (
                None,
                Some(format!("Cannot attach to the owner: {error}")),
                false,
            ),
        };
    let mut message = match &draft {
        Some(settings) => format!(
            "Draft RCon {}:{}{}; password {}.",
            settings.address,
            settings.port,
            settings
                .permission
                .map(|permission| format!(", permission {}", permission.native()))
                .unwrap_or_default(),
            if password_available {
                "stored"
            } else {
                "not set"
            }
        ),
        None => "RCon is not configured in the draft.".into(),
    };
    match (&session, &session_error) {
        (Some(status), _) => message.push_str(&format!(
            " Live session: {} to {} (run {}, session {}).",
            connection_label(status.connection),
            status.endpoint,
            status.run_generation,
            status.session_generation
        )),
        (None, Some(error)) => message.push_str(&format!(" Live session: {error}.")),
        (None, None) => {}
    }
    Ok(success(
        action,
        message,
        json!({
            "draft": draft,
            "password_available": password_available,
            "owner_contacted": owner_contacted,
            "session": session,
            "session_error": session_error,
        }),
    ))
}

pub(super) fn connection_label(
    connection: crate::core::server::owner::RconConnection,
) -> &'static str {
    use crate::core::server::owner::RconConnection::*;
    match connection {
        Connecting => "connecting",
        Connected => "connected",
        Rejected => "password refused",
        NoReply => "no reply",
        Unreachable => "unreachable",
        PasswordUnavailable => "protected password unavailable",
        Lost => "lost, reconnecting",
    }
}

fn configure(
    cli: &CliArgs,
    action: &str,
    store: &ServerStore,
    args: ServerRconConfigureArgs,
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    let instance = store
        .show(&id)
        .map_err(|error| store_error(action, error))?;
    let reforger = instance.definition.kind() == ServerDefinitionKind::Reforger;
    if !reforger && (args.permission.is_some() || args.max_clients.is_some()) {
        return Err(CommandError::validation(
            action,
            "--permission and --max-clients apply to Reforger only",
        ));
    }
    let input = RconSettingsInput {
        port: args.port,
        address: args.address,
        permission: reforger.then_some(match args.permission {
            Some(ServerRconPermissionArg::Admin) => ReforgerRconPermission::Admin,
            Some(ServerRconPermissionArg::Monitor) | None => ReforgerRconPermission::Monitor,
        }),
        max_clients: args.max_clients,
    };
    if cli.dry_run {
        return Ok(success(
            action,
            "Dry run: the RCon settings were not saved.".into(),
            json!({"dry_run": true, "port": input.port, "address": input.address.to_string()}),
        ));
    }
    let _claim = super::content::claim(store, action)?;
    let saved = store
        .configure_rcon(&id, args.expected_revision, Some(input))
        .map_err(|error| store_error(action, error))?;
    let draft = rcon::draft_settings(&saved);
    let mut message = format!("Saved RCon settings in draft revision {}.", saved.revision);
    if draft
        .as_ref()
        .is_some_and(|settings| !settings.password_stored)
    {
        message.push_str(" Store a password with server rcon set-password before committing.");
    } else {
        message.push_str(" Commit, prepare and activate, then restart the server to apply.");
    }
    if !input.address.is_loopback() {
        message.push_str(" RCon sends its password without encryption; keep a non-local endpoint on a private network or tunnel.");
    }
    Ok(success(
        action,
        message,
        json!({"revision": saved.revision, "rcon": draft}),
    ))
}

fn run_generation(
    action: &str,
    attachment: &crate::core::server::owner::Attachment,
    id: &ServerInstanceId,
) -> Result<u64, CommandError> {
    let status = attachment.instance_status(id).map_err(|error| {
        CommandError::operation(action, format!("Cannot read the run status: {error}"))
    })?;
    if status.run_generation == 0 || !status.process_exists {
        return Err(CommandError::operation(
            action,
            "The server is not running in this owner",
        ));
    }
    Ok(status.run_generation)
}

fn test(
    action: &str,
    context: &ServerContext,
    id: &ServerInstanceId,
) -> Result<CommandSuccess, CommandError> {
    let attachment = attach_existing(context, action)?;
    let generation = run_generation(action, &attachment, id)?;
    let reply = attachment
        .rcon_test(id, generation)
        .map_err(|error| CommandError::operation(action, error.to_string()))?;
    if !reply.replied {
        return Err(CommandError::operation(
            action,
            reply.error.unwrap_or_else(|| "RCon did not answer".into()),
        ));
    }
    Ok(success(
        action,
        format!(
            "RCon answered a keepalive in {} ms (session {}).",
            reply.round_trip_ms.unwrap_or_default(),
            reply.session_generation.unwrap_or_default()
        ),
        json!({"replied": true, "round_trip_ms": reply.round_trip_ms, "session_generation": reply.session_generation, "run_generation": generation}),
    ))
}

fn send_command(
    cli: &CliArgs,
    action: &str,
    context: &ServerContext,
    store: &ServerStore,
    id: &ServerInstanceId,
    text: &str,
    timeout_ms: u32,
) -> Result<CommandSuccess, CommandError> {
    if cli.dry_run {
        return Ok(success(
            action,
            "Dry run: no owner was contacted and nothing was sent.".into(),
            json!({"dry_run": true, "text": text, "owner_contacted": false}),
        ));
    }
    if !cli.yes {
        return Err(CommandError::validation(
            action,
            "Sending an RCon command requires --yes. It is sent once and never resent automatically.",
        ));
    }
    let attachment = attach_existing(context, action)?;
    let generation = run_generation(action, &attachment, id)?;
    let reply = attachment
        .rcon_command(id, generation, text, timeout_ms)
        .map_err(|error| {
            let mut failure = CommandError::operation(action, error.to_string());
            if error.kind() == std::io::ErrorKind::TimedOut {
                failure.code = exit_codes::PARTIAL_SUCCESS;
            }
            failure
        })?;
    let record = reply
        .command_id
        .as_deref()
        .and_then(|command_id| store.rcon_command(id, command_id).ok().flatten());
    let state = reply.state.or(record.as_ref().map(|record| record.state));
    let (message, exit_code) = match (state, &reply.error) {
        (Some(RconCommandState::Replied), _) => (
            match record
                .as_ref()
                .and_then(|record| record.response.as_deref())
            {
                Some(response) if !response.is_empty() => {
                    format!("The server replied:\n{response}")
                }
                _ => "The server acknowledged the command with an empty reply.".to_string(),
            },
            exit_codes::SUCCESS,
        ),
        (Some(RconCommandState::Unknown), _) => (
            format!(
                "The command was sent once but its outcome is unknown: {} Check the server before sending it again.",
                reply
                    .reason
                    .map(|reason| reason.describe())
                    .unwrap_or("no reply arrived.")
            ),
            exit_codes::PARTIAL_SUCCESS,
        ),
        (Some(RconCommandState::NotSent), _) => (
            format!(
                "The command was not sent. {}",
                reply
                    .reason
                    .map(|reason| reason.describe())
                    .unwrap_or_default()
            ),
            exit_codes::OPERATION_FAILED,
        ),
        (_, Some(error)) => return Err(CommandError::operation(action, error.clone())),
        _ => (
            "The owner returned no command outcome; check server rcon history.".into(),
            exit_codes::OPERATION_FAILED,
        ),
    };
    Ok(CommandSuccess {
        action: action.into(),
        message,
        data: json!({
            "command_id": reply.command_id,
            "state": state,
            "reason": reply.reason,
            "record": record,
            "run_generation": generation,
        }),
        exit_code,
    })
}

pub(super) fn read_password(cli: &CliArgs, action: &str) -> Result<RconPassword, CommandError> {
    let _signal = super::content_stage::signal::SignalGuard::install()
        .map_err(|()| CommandError::operation(action, "Cannot register input cancellation"))?;
    let mut reader = PrivateInput::new()
        .map_err(|_| CommandError::operation(action, "Cannot read private input from stdin"))?;
    if !cli.quiet {
        eprintln!("Enter the RCon password on one line (input is not shown).");
    }
    let line = loop {
        if super::content_stage::signal::requested() {
            return Err(CommandError::operation(
                action,
                "Password input was cancelled",
            ));
        }
        match reader.poll_line() {
            Ok(Some(line)) => break line,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                return Err(CommandError::validation(
                    action,
                    "No password line was read from stdin",
                ));
            }
        }
    };
    RconPassword::new(line.expose().to_owned())
        .map_err(|message| CommandError::validation(action, message))
}

fn validate_command_text(action: &str, text: &str) -> Result<(), CommandError> {
    if rcon::carries_secret(text) {
        return Err(CommandError::validation(
            action,
            "Commands that carry a password are refused; change the RCon password with server rcon set-password",
        ));
    }
    if !foxy_supervisor::control::valid_rcon_text(text) {
        return Err(CommandError::validation(
            action,
            format!(
                "An RCon command needs 1 to {MAX_COMMAND_BYTES} bytes of text without control characters"
            ),
        ));
    }
    Ok(())
}

pub(super) fn login_failure_text(failure: LoginFailure) -> &'static str {
    match failure {
        LoginFailure::Rejected => "The server refused the RCon password.",
        LoginFailure::NoReply => {
            "Nothing answered before the deadline. RCon may be disabled, still starting, or the endpoint is unreachable."
        }
        LoginFailure::Unreachable => "Nothing is listening on that RCon endpoint.",
        LoginFailure::LocalSocket => "A local UDP socket could not be opened.",
    }
}

fn probe(
    cli: &CliArgs,
    action: &str,
    store: &ServerStore,
    args: ServerRconProbeArgs,
) -> Result<CommandSuccess, CommandError> {
    let override_endpoint = args
        .endpoint
        .as_deref()
        .map(|value| {
            value.parse::<SocketAddr>().map_err(|_| {
                CommandError::validation(
                    action,
                    "--endpoint must be an IP socket address such as 127.0.0.1:2306",
                )
            })
        })
        .transpose()?;
    if let Some(text) = &args.command {
        validate_command_text(action, text)?;
        if !cli.yes && !cli.dry_run {
            return Err(CommandError::validation(
                action,
                "Sending an RCon command requires --yes. It is sent once and never resent.",
            ));
        }
    }
    let (endpoint, password) = match (&args.id, args.password_stdin) {
        (_, true) => {
            let endpoint = match (override_endpoint, &args.id) {
                (Some(endpoint), _) => endpoint,
                (None, Some(id)) => {
                    let id = parse_id(id.clone(), action)?;
                    store
                        .active_rcon_target(&id, &ContentStageControl::default())
                        .map_err(|error| store_error(action, error))?
                        .0
                }
                (None, None) => {
                    return Err(CommandError::validation(
                        action,
                        "Give an instance id or --endpoint",
                    ));
                }
            };
            (endpoint, read_password(cli, action)?)
        }
        (Some(id), false) => {
            let id = parse_id(id.clone(), action)?;
            let (endpoint, password) = store
                .active_rcon_target(&id, &ContentStageControl::default())
                .map_err(|error| store_error(action, error))?;
            (override_endpoint.unwrap_or(endpoint), password)
        }
        (None, false) => {
            return Err(CommandError::validation(
                action,
                "Give an instance id, or --endpoint with --password-stdin; passwords are never accepted in command arguments",
            ));
        }
    };
    if cli.dry_run {
        return Ok(success(
            action,
            format!("Dry run: would open one RCon session to {endpoint}. Nothing was sent."),
            json!({"endpoint": endpoint.to_string(), "command": args.command, "sent": false}),
        ));
    }
    let timeout = Duration::from_millis(u64::from(args.timeout_ms));
    let started = Instant::now();
    let mut session = match Session::login(endpoint, &password, timeout) {
        Ok(session) => session,
        Err(failure) => {
            return Ok(CommandSuccess {
                action: action.into(),
                message: login_failure_text(failure).into(),
                data: json!({"endpoint": endpoint.to_string(), "login": failure, "login_ms": started.elapsed().as_millis()}),
                exit_code: exit_codes::OPERATION_FAILED,
            });
        }
    };
    let login_ms = started.elapsed().as_millis();
    drop(password);
    let keepalive_started = Instant::now();
    let keepalive = session.command(b"", timeout, Retransmit::Safe);
    let keepalive_ms = keepalive_started.elapsed().as_millis();
    let keepalive_ok = matches!(keepalive, CommandOutcome::Replied(_));
    let mut command_report = serde_json::Value::Null;
    let mut exit_code = if keepalive_ok {
        exit_codes::SUCCESS
    } else {
        exit_codes::OPERATION_FAILED
    };
    let mut message = if keepalive_ok {
        format!("Logged in to {endpoint} and exchanged a keepalive in {keepalive_ms} ms.")
    } else {
        format!("Logged in to {endpoint}, but the keepalive got no reply.")
    };
    if let Some(text) = &args.command
        && keepalive_ok
    {
        let outcome = session.command(text.as_bytes(), timeout, Retransmit::Never);
        command_report = outcome_json(&outcome);
        match outcome {
            CommandOutcome::Replied(_) => message.push_str(" The command was acknowledged."),
            CommandOutcome::Unknown(_) => {
                exit_code = exit_codes::PARTIAL_SUCCESS;
                message.push_str(" The command was sent once but no reply arrived, so its outcome is unknown. Check the server before sending it again.");
            }
            CommandOutcome::NotSent => {
                exit_code = exit_codes::OPERATION_FAILED;
                message.push_str(" The command was not sent.");
            }
        }
    }
    let _ = session.pump(Duration::from_millis(300));
    Ok(CommandSuccess {
        action: action.into(),
        message,
        data: json!({
            "endpoint": endpoint.to_string(),
            "login": "accepted",
            "login_ms": login_ms,
            "keepalive": outcome_json(&keepalive),
            "keepalive_ms": keepalive_ms,
            "command": command_report,
            "messages": session.take_messages(),
            "counters": session.counters(),
        }),
        exit_code,
    })
}

fn outcome_json(outcome: &CommandOutcome) -> serde_json::Value {
    match outcome {
        CommandOutcome::Replied(body) => {
            json!({"outcome": "replied", "response": response_text(body)})
        }
        CommandOutcome::Unknown(reason) => json!({"outcome": "unknown", "reason": reason}),
        CommandOutcome::NotSent => json!({"outcome": "not_sent"}),
    }
}
