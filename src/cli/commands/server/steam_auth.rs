pub(super) mod input;

use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use foxy_steam::auth::AuthPhase;
use foxy_steam::{AuthError, Secret};
use serde_json::json;

use crate::cli::args::{CliArgs, ServerContextArgs, ServerSteamCommand};
use crate::cli::commands::{CommandError, CommandSuccess};
use crate::cli::exit_codes;
use crate::core::server::steam_auth::{SteamAuthControl, SteamAuthOperation, run_steam_auth};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerSteamCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.steam";
    let context = super::instances::capture(context, action)?;
    if !crate::core::game::registry()
        .get(context.game_id())
        .is_some_and(|game| game.server_capabilities().steam_authentication)
    {
        return Err(CommandError::validation(
            action,
            "Native Steam authentication is unavailable for this game space",
        ));
    }
    let alias = match &command {
        ServerSteamCommand::Status(args)
        | ServerSteamCommand::Resume(args)
        | ServerSteamCommand::Forget(args) => args.account.clone(),
        ServerSteamCommand::Login { account, .. } => account.account.clone(),
        ServerSteamCommand::Anonymous => "anonymous".into(),
    };
    if cli.dry_run {
        let outcome = run_steam_auth(
            &context,
            &alias,
            SteamAuthOperation::Inspect,
            &SteamAuthControl::default(),
        );
        if let Some(error) = outcome.error {
            return Err(auth_error(error));
        }
        return Ok(CommandSuccess {
            action: action.into(),
            message: "Native Steam authentication preview; no network requests or writes".into(),
            data: json!({"alias":alias,"prepared":outcome.prepared,"dry_run":true,"writes_performed":false,"database_opened":false}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if matches!(&command, ServerSteamCommand::Forget(_)) && !cli.yes {
        return Err(CommandError::validation(
            action,
            "Forgetting a prepared Steam session requires --yes",
        ));
    }
    let _signal = super::content_stage::signal::SignalGuard::install().map_err(|()| {
        CommandError::operation(action, "Cannot register authentication cancellation")
    })?;
    let mut private_input = None;
    let operation = match command {
        ServerSteamCommand::Status(_) => SteamAuthOperation::Inspect,
        ServerSteamCommand::Forget(_) => SteamAuthOperation::Forget,
        ServerSteamCommand::Resume(_) => SteamAuthOperation::Resume,
        ServerSteamCommand::Anonymous => SteamAuthOperation::Anonymous,
        ServerSteamCommand::Login {
            credentials_file,
            credentials_stdin,
            ..
        } => {
            let credentials = if let Some(path) = credentials_file {
                input::load_credentials(&path).map_err(auth_error)?
            } else if credentials_stdin {
                let mut reader = input::PrivateInput::new().map_err(auth_error)?;
                if !cli.quiet {
                    eprintln!(
                        "Provide one private JSON line with account_name and password on stdin."
                    );
                }
                let line = loop {
                    if super::content_stage::signal::requested() {
                        return Err(auth_error(AuthError::Cancelled));
                    }
                    if let Some(line) = reader.poll_line().map_err(auth_error)? {
                        break line;
                    }
                    thread::sleep(Duration::from_millis(25));
                };
                let credentials = input::parse_credentials(line.expose()).map_err(auth_error)?;
                private_input = Some(reader);
                credentials
            } else {
                return Err(CommandError::validation(
                    action,
                    "Use --credentials-stdin or --credentials-file; passwords are never accepted in command arguments",
                ));
            };
            SteamAuthOperation::Password {
                account_name: credentials.0,
                password: credentials.1,
            }
        }
    };
    let control = SteamAuthControl::default();
    let worker_control = control.clone();
    let worker = thread::Builder::new()
        .name("native-steam-cli".into())
        .spawn(move || run_steam_auth(&context, &alias, operation, &worker_control))
        .map_err(|_| CommandError::operation(action, "Cannot start native Steam authentication"))?;
    let mut last_phase = None;
    let mut input_error = None;
    let mut last_notice = Instant::now();
    while !worker.is_finished() {
        if super::content_stage::signal::requested() {
            control.cancel();
        }
        let phase = control.phase();
        let wants_code = matches!(
            phase,
            AuthPhase::EmailCode
                | AuthPhase::DeviceCode
                | AuthPhase::CodeRejectedEmail
                | AuthPhase::CodeRejectedDevice
        );
        if last_phase != Some(phase) {
            if !cli.quiet {
                eprintln!("Native Steam: {}", phase_text(phase));
                let _ = std::io::stderr().flush();
            }
            last_phase = Some(phase);
            last_notice = Instant::now();
        }
        if wants_code && !control.is_cancelled() {
            if private_input.is_none() {
                match input::PrivateInput::new() {
                    Ok(reader) => private_input = Some(reader),
                    Err(error) => {
                        input_error = Some(error);
                        control.cancel();
                    }
                }
            }
            if let Some(reader) = private_input.as_mut() {
                match reader.poll_line() {
                    Ok(Some(code)) => {
                        if let Err(error) =
                            control.submit_code(Secret::new(code.expose().to_owned()))
                        {
                            input_error = Some(error);
                            control.cancel();
                        }
                    }
                    Ok(None) => (),
                    Err(error) => {
                        input_error = Some(error);
                        control.cancel();
                    }
                }
            }
        }
        if !cli.quiet && control.is_cancelled() && last_notice.elapsed() > Duration::from_secs(1) {
            eprintln!(
                "Cancelling native Steam authentication; waiting for the connection to close"
            );
            last_notice = Instant::now();
        }
        thread::sleep(Duration::from_millis(25));
    }
    let mut outcome = worker.join().map_err(|_| {
        CommandError::operation(
            action,
            "Native Steam authentication worker stopped unexpectedly",
        )
    })?;
    if outcome.error == Some(AuthError::Cancelled)
        && let Some(error) = input_error
    {
        outcome.error = Some(error);
    }
    let exit_code = outcome.error.map_or(exit_codes::SUCCESS, error_code);
    Ok(CommandSuccess {
        action: action.into(),
        message: outcome.error.map_or_else(
            || "Native Steam operation completed; connection closed".into(),
            |error| error.to_string(),
        ),
        data: json!({"outcome":outcome,"database_opened":false}),
        exit_code,
    })
}

fn auth_error(error: AuthError) -> CommandError {
    CommandError {
        action: "server.steam".into(),
        message: error.to_string(),
        code: error_code(error),
    }
}

fn error_code(error: AuthError) -> i32 {
    match error {
        AuthError::Busy => exit_codes::DATABASE_BUSY,
        AuthError::InvalidInput => exit_codes::VALIDATION_ERROR,
        _ => exit_codes::OPERATION_FAILED,
    }
}

fn phase_text(phase: AuthPhase) -> &'static str {
    match phase {
        AuthPhase::Connecting => "connecting",
        AuthPhase::Authenticating => "authenticating",
        AuthPhase::EmailCode => "enter the Steam Guard email code on private stdin",
        AuthPhase::DeviceCode => "enter the Steam Guard authenticator code on private stdin",
        AuthPhase::MobileApproval => "approve this login in the Steam mobile app",
        AuthPhase::CodeRejectedEmail => "email code rejected; enter another code",
        AuthPhase::CodeRejectedDevice => "authenticator code rejected; enter another code",
        AuthPhase::Authenticated => "authenticated",
    }
}
