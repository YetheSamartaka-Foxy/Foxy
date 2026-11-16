pub mod args;
mod commands;
mod exit_codes;
mod output;
pub mod toon;

use clap::{CommandFactory, Parser};

use self::args::{AddonCommand, CliArgs, CliCommand, RepoCommand, SpaceCommand};
use self::commands::{CommandError, run_command};
use self::output::CliOutput;
use crate::ui::app::agent_driver::AgentGuiLaunchConfig;

pub enum CliExecution {
    RunUi {
        mode_override: Option<crate::core::app_mode::AppMode>,
        debug_mode: bool,
        agent_gui: AgentGuiLaunchConfig,
        debug_modals: Vec<crate::ui::app::debug_modals::DebugModal>,
    },
    Exit(i32),
}

#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn cleanup_witness_entry(arguments: &[String]) -> Option<i32> {
    if arguments.get(1).map(String::as_str) != Some("server")
        || arguments.get(2).map(String::as_str) != Some("cleanup-witness")
    {
        return None;
    }
    let handles = (arguments.len() == 7)
        .then(|| {
            arguments[3..]
                .iter()
                .map(|value| value.parse::<u64>().ok().filter(|value| *value > 0))
                .collect::<Option<Vec<_>>>()
        })
        .flatten();
    Some(match handles {
        Some(handles) => match foxy_supervisor::process::run_cleanup_witness(
            handles[0], handles[1], handles[2], handles[3],
        ) {
            Ok(()) => exit_codes::SUCCESS,
            Err(_) => exit_codes::OPERATION_FAILED,
        },
        None => exit_codes::VALIDATION_ERROR,
    })
}

#[cfg(windows)]
pub(crate) fn console_signal_entry(arguments: &[String]) -> Option<i32> {
    if arguments.get(1).map(String::as_str) != Some("server")
        || arguments.get(2).map(String::as_str) != Some("console-signal")
    {
        return None;
    }
    let ctrl_break = arguments.len() == 5 && arguments[3] == "--ctrl-break";
    let handle = (arguments.len() == 4 || ctrl_break)
        .then(|| {
            arguments[if ctrl_break { 4 } else { 3 }]
                .parse::<u64>()
                .ok()
        })
        .flatten()
        .filter(|handle| *handle != 0);
    Some(match handle {
        Some(handle) => match if ctrl_break {
            foxy_supervisor::process::run_console_break_helper(handle)
        } else {
            foxy_supervisor::process::run_console_signal_helper(handle)
        } {
            Ok(code) => code,
            Err(error) => error
                .raw_os_error()
                .and_then(|code| code.checked_add(1000))
                .unwrap_or(exit_codes::VALIDATION_ERROR),
        },
        None => exit_codes::VALIDATION_ERROR,
    })
}

#[cfg(all(test, windows))]
#[test]
fn console_signal_entry_requires_exact_arguments_and_nonzero_inherited_authority() {
    for args in [
        vec!["foxy", "server", "console-signal", "0"],
        vec!["foxy", "server", "console-signal", "--ctrl-break", "0"],
        vec!["foxy", "server", "console-signal", "--unknown", "1"],
        vec!["foxy", "server", "console-signal", "--ctrl-break"],
    ] {
        let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(
            console_signal_entry(&args),
            Some(exit_codes::VALIDATION_ERROR)
        );
    }
    assert_eq!(
        console_signal_entry(&["foxy".into(), "version".into()]),
        None
    );
}

fn server_monitor_stream(cli: &CliArgs) -> bool {
    !cli.dry_run
        && matches!(cli.command.as_ref(), Some(args::CliCommand::Server {
        command: args::ServerCommand::Instance { command: args::ServerInstanceCommand::Monitor(monitor), .. }
    }) if monitor.follow && monitor.stream_json)
}

pub fn run_from_env() -> CliExecution {
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.len() <= 1 {
        let mut command = CliArgs::command();
        let _ = command.print_help();
        println!();
        return CliExecution::Exit(exit_codes::SUCCESS);
    }

    let mut cli = match CliArgs::try_parse_from(raw_args) {
        Ok(cli) => cli,
        Err(err) => {
            let exit_code = match err.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                | clap::error::ErrorKind::DisplayVersion => exit_codes::SUCCESS,
                _ => exit_codes::VALIDATION_ERROR,
            };
            err.print().ok();
            return CliExecution::Exit(exit_code);
        }
    };

    if !cli.toon
        && foxy_agent_toon_enabled()
        && matches!(cli.command, Some(CliCommand::AgentGui { .. }))
    {
        cli.toon = true;
    }

    if cli.toon {
        cli.json = true;
    }

    let output = CliOutput {
        json: cli.json,
        toon: cli.toon,
        quiet: cli.quiet,
    };

    if let Err(err) = commands::apply_config_override(cli.config_dir.as_ref()) {
        emit_error(&output, err);
        return CliExecution::Exit(exit_codes::VALIDATION_ERROR);
    }

    if let Some(CliCommand::Ui(args)) = cli.command.as_ref() {
        return CliExecution::RunUi {
            mode_override: args.mode.map(Into::into),
            debug_mode: args.debug_mode,
            agent_gui: AgentGuiLaunchConfig {
                enabled: args.agent_gui,
                port: args.agent_port,
            },
            debug_modals: args.debug_modals.clone(),
        };
    }

    if matches!(cli.command, Some(CliCommand::ContentAcquisitionWorker))
        && (cli.dry_run || cli.wipe_db)
    {
        emit_error(
            &output,
            CommandError {
                action: "content-acquisition-worker".into(),
                message: "Worker execution cannot be combined with dry-run or database wipe".into(),
                code: exit_codes::VALIDATION_ERROR,
            },
        );
        return CliExecution::Exit(exit_codes::VALIDATION_ERROR);
    }

    if !cli.wipe_db
        && matches!(
            cli.command.as_ref(),
            Some(
                CliCommand::Version
                    | CliCommand::Server {
                        command: args::ServerCommand::Owner(_)
                            | args::ServerCommand::Supervise(_)
                            | args::ServerCommand::Context(_)
                            | args::ServerCommand::Capabilities(_)
                            | args::ServerCommand::Instance { .. }
                            | args::ServerCommand::Installation { .. }
                            | args::ServerCommand::Configuration { .. }
                            | args::ServerCommand::Content { .. }
                            | args::ServerCommand::Mission { .. }
                            | args::ServerCommand::Keys { .. }
                            | args::ServerCommand::Deploy { .. }
                            | args::ServerCommand::Steam { .. }
                            | args::ServerCommand::Workshop { .. }
                            | args::ServerCommand::Headless { .. }
                            | args::ServerCommand::Maintenance { .. }
                            | args::ServerCommand::ReforgerAddons { .. }
                    }
            )
        )
    {
        let streamed = server_monitor_stream(&cli);
        let command = cli.command.take().expect("matched command");
        match run_command(&cli, command) {
            Ok(success) => {
                if !streamed {
                    output.success(&success.action, &success.message, success.data);
                }
                return CliExecution::Exit(success.exit_code);
            }
            Err(err) => {
                let code = err.code;
                emit_error(&output, err);
                return CliExecution::Exit(code);
            }
        }
    }

    if cli.wipe_db
        && matches!(
            cli.command.as_ref(),
            Some(CliCommand::Server {
                command: args::ServerCommand::Owner(_)
                    | args::ServerCommand::Supervise(_)
                    | args::ServerCommand::Context(_)
                    | args::ServerCommand::Capabilities(_)
                    | args::ServerCommand::Instance { .. }
                    | args::ServerCommand::Installation { .. }
                    | args::ServerCommand::Configuration { .. }
                    | args::ServerCommand::Content { .. }
                    | args::ServerCommand::Mission { .. }
                    | args::ServerCommand::Keys { .. }
                    | args::ServerCommand::Deploy { .. }
                    | args::ServerCommand::Steam { .. }
                    | args::ServerCommand::Workshop { .. }
                    | args::ServerCommand::Headless { .. }
                    | args::ServerCommand::Maintenance { .. },
            })
        )
    {
        emit_error(
            &output,
            CommandError {
                action: "server".to_string(),
                message: "Server draft commands cannot be combined with --wipe-db".to_string(),
                code: exit_codes::VALIDATION_ERROR,
            },
        );
        return CliExecution::Exit(exit_codes::VALIDATION_ERROR);
    }

    if command_uses_database(cli.command.as_ref(), cli.wipe_db) {
        match crate::core::tasks::config_process_lock::acquire_for_config_root() {
            crate::core::tasks::db_process_lock::LockOutcome::Acquired => {}
            crate::core::tasks::db_process_lock::LockOutcome::Busy { .. } => {
                emit_error(
                    &output,
                    CommandError {
                        action: "configuration-lock".to_string(),
                        message: "another Foxy process owns this configuration; close it and retry"
                            .to_string(),
                        code: exit_codes::DATABASE_BUSY,
                    },
                );
                return CliExecution::Exit(exit_codes::DATABASE_BUSY);
            }
            crate::core::tasks::db_process_lock::LockOutcome::Unavailable(_) => {
                emit_error(
                    &output,
                    CommandError {
                        action: "configuration-lock".to_string(),
                        message: "could not claim this configuration; restore access and retry"
                            .to_string(),
                        code: exit_codes::OPERATION_FAILED,
                    },
                );
                return CliExecution::Exit(exit_codes::OPERATION_FAILED);
            }
        }
        if let Err(error) = crate::core::game::spaces::prepare_game_spaces_layout() {
            emit_error(
                &output,
                CommandError {
                    action: "game.bootstrap".to_string(),
                    message: error,
                    code: exit_codes::OPERATION_FAILED,
                },
            );
            return CliExecution::Exit(exit_codes::OPERATION_FAILED);
        }
        if let Some(error) =
            database_claim_error(crate::core::tasks::db_process_lock::acquire_for_active_space())
        {
            let code = error.code;
            emit_error(&output, error);
            return CliExecution::Exit(code);
        }
        let path = crate::core::tasks::db_turso::database_file_path();
        if let Err(message) = crate::core::tasks::init_database::resume_database_access_sync(&path)
        {
            emit_error(
                &output,
                CommandError {
                    action: "database-lock".to_string(),
                    message,
                    code: exit_codes::OPERATION_FAILED,
                },
            );
            return CliExecution::Exit(exit_codes::OPERATION_FAILED);
        }
    }

    // A standalone database wipe must require explicit --yes consent and exit after rebuilding.
    if cli.wipe_db {
        return run_db_wipe(&output, cli.yes);
    }

    let Some(command) = cli.command.take() else {
        let mut command = CliArgs::command();
        let _ = command.print_help();
        println!();
        return CliExecution::Exit(exit_codes::SUCCESS);
    };

    crate::core::api::ensure_logger();

    if command_uses_database(Some(&command), false) {
        let preview = cli.dry_run
            || matches!(&command, CliCommand::Launch(args) if !args.execute)
            || matches!(&command, CliCommand::Game { command: args::GameCommand::Launch(args) } if !args.execute);
        let space = crate::core::game::spaces::active_game_space_dir();
        let result = if preview {
            let has_import = space
                .join(".foxypack-import-recovery")
                .join("pending.json")
                .exists();
            let has_launch = std::fs::read_dir(space.join("launch_preparation"))
                .is_ok_and(|mut entries| entries.next().is_some());
            if has_import || has_launch {
                Err("Recovery is pending. Open Foxy or run a non-preview operation before previewing this setup.".to_string())
            } else {
                Ok(())
            }
        } else {
            crate::core::game::launch_preparation::recover(&space)
                .and_then(|()| crate::core::game::foxypack::recover_pending_import(&space))
        };
        if let Err(message) = result {
            emit_error(
                &output,
                CommandError {
                    action: "configuration-recovery".to_string(),
                    message,
                    code: exit_codes::OPERATION_FAILED,
                },
            );
            return CliExecution::Exit(exit_codes::OPERATION_FAILED);
        }
    }

    if !cli.dry_run
        && let Some(action) = update_action(&command)
    {
        // Honor an already requested wipe before the readiness check opens the DB.
        crate::core::tasks::init_database::check_and_wipe_database();
        let ready = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime.block_on(
                crate::core::tasks::db_schema_version::ensure_database_ready_for_updates(),
            ),
            Err(err) => Err(format!(
                "Failed to create runtime for database check: {err}"
            )),
        };
        if let Err(message) = ready {
            emit_error(
                &output,
                CommandError {
                    action: action.to_string(),
                    message,
                    code: exit_codes::OPERATION_FAILED,
                },
            );
            return CliExecution::Exit(exit_codes::OPERATION_FAILED);
        }
    }

    if !output.json
        && command_uses_database(Some(&command), false)
        && let Some(hint) = crate::core::tasks::db_schema_version::cli_wipe_hint()
    {
        eprintln!("warning: {hint}");
    }

    match run_command(&cli, command) {
        Ok(success) => {
            output.success(&success.action, &success.message, success.data);
            CliExecution::Exit(success.exit_code)
        }
        Err(err) => {
            emit_error(&output, err.clone());
            CliExecution::Exit(err.code)
        }
    }
}

fn update_action(command: &CliCommand) -> Option<&'static str> {
    match command {
        CliCommand::Sync(_)
        | CliCommand::Repo {
            command: RepoCommand::Sync(_),
        } => Some("repo.sync"),
        CliCommand::Repo {
            command: RepoCommand::ForceRedownload(_),
        } => Some("repo.force-redownload"),
        CliCommand::Addon {
            command: AddonCommand::RecalcHashes(_),
        } => Some("addon.recalc-hashes"),
        CliCommand::Addon {
            command: AddonCommand::ForceRedownload(_),
        } => Some("addon.force-redownload"),
        CliCommand::Space {
            command: SpaceCommand::Sync(_),
        } => Some("space.sync"),
        _ => None,
    }
}

/// Commands that attach to an existing owner or inspect public metadata must work beside a live GUI without its DB claim.
fn command_uses_database(command: Option<&CliCommand>, wipe_db: bool) -> bool {
    if wipe_db {
        return true;
    }
    match command {
        None => false,
        Some(
            CliCommand::AgentGui { .. } | CliCommand::SteamHelper { .. } | CliCommand::Version,
        ) => false,
        Some(CliCommand::Server { command }) => match server_database_use(command) {
            ServerDatabaseUse::LocalMetadata
            | ServerDatabaseUse::PublicQuery
            | ServerDatabaseUse::OwnerClient
            | ServerDatabaseUse::ForegroundOwner => false,
        },
        Some(_) => true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerDatabaseUse {
    LocalMetadata,
    PublicQuery,
    OwnerClient,
    ForegroundOwner,
}

fn server_database_use(command: &args::ServerCommand) -> ServerDatabaseUse {
    match command {
        args::ServerCommand::Owner(_) => ServerDatabaseUse::OwnerClient,
        args::ServerCommand::Supervise(_) => ServerDatabaseUse::ForegroundOwner,
        args::ServerCommand::InspectAddons(_)
        | args::ServerCommand::ConsoleSignal { .. }
        | args::ServerCommand::CleanupWitness { .. } => ServerDatabaseUse::PublicQuery,
        args::ServerCommand::Context(_)
        | args::ServerCommand::Capabilities(_)
        | args::ServerCommand::Instance { .. }
        | args::ServerCommand::Installation { .. }
        | args::ServerCommand::Configuration { .. }
        | args::ServerCommand::Content { .. }
        | args::ServerCommand::Mission { .. }
        | args::ServerCommand::Keys { .. }
        | args::ServerCommand::Steam { .. }
        | args::ServerCommand::Workshop { .. }
        | args::ServerCommand::Headless { .. }
        | args::ServerCommand::Maintenance { .. }
        | args::ServerCommand::ReforgerAddons { .. }
        | args::ServerCommand::Deploy { .. } => ServerDatabaseUse::LocalMetadata,
    }
}

fn database_claim_error(
    outcome: crate::core::tasks::db_process_lock::LockOutcome,
) -> Option<CommandError> {
    use crate::core::tasks::db_process_lock::LockOutcome;
    match outcome {
        LockOutcome::Acquired => None,
        LockOutcome::Busy { holder_pid } => Some(CommandError {
            action: "database-lock".to_string(),
            message: format!("the database for this game space is in use by another Foxy process{}; close it and retry", holder_pid.map(|pid| format!(" (PID {pid})")).unwrap_or_default()),
            code: exit_codes::DATABASE_BUSY,
        }),
        LockOutcome::Unavailable(_) => Some(CommandError {
            action: "database-lock".to_string(),
            message: "Cannot verify exclusive access to this game space's database; check config-directory permissions and filesystem lock support, then retry".to_string(),
            code: exit_codes::OPERATION_FAILED,
        }),
    }
}

fn emit_error(output: &CliOutput, error: CommandError) {
    output.failure(&error.action, &error.message, vec![error.message.clone()]);
}

fn run_db_wipe(output: &CliOutput, confirmed: bool) -> CliExecution {
    crate::core::api::ensure_logger();
    const ACTION: &str = "db.wipe";

    if !confirmed {
        let message = "Wiping the database is destructive; re-run with --yes to confirm";
        output.failure(ACTION, message, vec![message.to_string()]);
        return CliExecution::Exit(exit_codes::VALIDATION_ERROR);
    }

    let result = match tokio::runtime::Runtime::new() {
        Ok(rt) => {
            let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rt.block_on(crate::core::tasks::init_database::wipe_database_live())
            }));
            match first {
                Ok(result) => result,
                Err(_) if crate::core::tasks::db_turso::corrupt_rebuild_requires_discard() => {
                    crate::core::tasks::db_turso::discard_corrupt_database().and_then(|()| {
                        rt.block_on(crate::core::tasks::init_database::wipe_database_live())
                    })
                }
                Err(_) => Err(
                    "Database initialization failed; existing records were preserved".to_string(),
                ),
            }
        }
        Err(err) => Err(format!("Failed to create runtime for database wipe: {err}")),
    };

    match result {
        Ok(()) => {
            crate::core::tasks::db_schema_version::mark_wiped();
            output.success(
                ACTION,
                "Database wiped and rebuilt",
                serde_json::json!({ "wiped": true }),
            );
            CliExecution::Exit(exit_codes::SUCCESS)
        }
        Err(err) => {
            output.failure(ACTION, &err, vec![err.clone()]);
            CliExecution::Exit(exit_codes::OPERATION_FAILED)
        }
    }
}

fn foxy_agent_toon_enabled() -> bool {
    std::env::var("FOXY_AGENT_TOON")
        .map(|value| env_flag_enabled(&value))
        .unwrap_or(false)
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_ownership_is_an_operation_failure_and_busy_stays_distinct() {
        use crate::core::tasks::db_process_lock::LockOutcome;
        assert!(database_claim_error(LockOutcome::Acquired).is_none());
        assert_eq!(
            database_claim_error(LockOutcome::Busy { holder_pid: None })
                .unwrap()
                .code,
            exit_codes::DATABASE_BUSY
        );
        let error =
            database_claim_error(LockOutcome::Unavailable("not supported".to_string())).unwrap();
        assert_eq!(error.code, exit_codes::OPERATION_FAILED);
        assert_eq!(error.action, "database-lock");
    }

    #[test]
    fn context_inspection_is_database_free_except_for_explicit_wipe() {
        let cli =
            CliArgs::try_parse_from(["foxy", "server", "context", "--game-space", "beta"]).unwrap();
        assert!(!command_uses_database(cli.command.as_ref(), false));
        assert!(command_uses_database(cli.command.as_ref(), true));
        let Some(CliCommand::Server {
            command: args::ServerCommand::Context(args),
        }) = cli.command
        else {
            panic!("expected context command")
        };
        assert_eq!(args.game_space.as_deref(), Some("beta"));
        assert!(
            CliArgs::try_parse_from([
                "foxy",
                "server",
                "context",
                "--game-space",
                "beta",
                "--wipe-db",
                "--yes",
            ])
            .is_err()
        );
    }

    #[test]
    fn draft_commands_use_captured_context_and_require_explicit_edit_revisions() {
        for tail in [
            vec!["capabilities"],
            vec!["instance", "list"],
            vec![
                "instance",
                "show",
                "server_00000000000000000000000000000001",
            ],
            vec!["instance", "add", "--name", "MainServer"],
            vec!["instance", "validate", "--file", "draft.json"],
            vec![
                "instance",
                "replace",
                "--file",
                "draft.json",
                "--expected-revision",
                "1",
            ],
            vec![
                "instance",
                "rename",
                "server_00000000000000000000000000000001",
                "--name",
                "MainServer",
                "--expected-revision",
                "1",
            ],
            vec![
                "instance",
                "clone",
                "server_00000000000000000000000000000001",
                "--name",
                "MainServer",
                "--expected-revision",
                "1",
            ],
            vec![
                "instance",
                "remove",
                "server_00000000000000000000000000000001",
                "--expected-revision",
                "1",
            ],
        ] {
            let mut argv = vec!["foxy", "--dry-run", "--json", "server"];
            argv.extend(tail);
            argv.extend(["--game-space", "beta"]);
            let cli = CliArgs::try_parse_from(argv).unwrap();
            assert!(!command_uses_database(cli.command.as_ref(), false));
            assert!(command_uses_database(cli.command.as_ref(), true));
            assert!(cli.dry_run && cli.json);
        }
        for command in ["rename", "clone", "remove"] {
            assert!(
                CliArgs::try_parse_from([
                    "foxy",
                    "server",
                    "instance",
                    command,
                    "server_00000000000000000000000000000001"
                ])
                .is_err()
            );
        }
        assert!(
            CliArgs::try_parse_from([
                "foxy",
                "server",
                "instance",
                "list",
                "--game-space",
                "beta",
                "--wipe-db",
                "--yes"
            ])
            .is_err()
        );
    }

    #[test]
    fn app_mode_flags_distinguish_session_overrides_and_saved_preferences() {
        use crate::core::app_mode::AppMode;
        for (value, expected) in [("client", AppMode::Client), ("server", AppMode::Server)] {
            let cli = CliArgs::try_parse_from(["foxy", "ui", "--mode", value]).unwrap();
            let Some(CliCommand::Ui(args)) = cli.command else {
                panic!("expected UI args")
            };
            assert_eq!(args.mode.map(AppMode::from), Some(expected));
            let cli =
                CliArgs::try_parse_from(["foxy", "settings", "set", "--app-mode", value]).unwrap();
            let Some(CliCommand::Settings { command }) = cli.command else {
                panic!("expected settings args")
            };
            let args::SettingsCommand::Set(args) = *command else {
                panic!("expected settings set")
            };
            assert_eq!(args.app_mode.map(AppMode::from), Some(expected));
        }
        assert!(CliArgs::try_parse_from(["foxy", "ui", "--mode", "other"]).is_err());
        assert!(
            CliArgs::try_parse_from(["foxy", "settings", "set", "--app-mode", "other"]).is_err()
        );
        let cli = CliArgs::try_parse_from(["foxy", "ui"]).unwrap();
        let Some(CliCommand::Ui(args)) = cli.command else {
            panic!("expected UI args")
        };
        assert!(args.mode.is_none());
    }

    #[test]
    fn server_inspection_keeps_its_database_exemption() {
        let cli = CliArgs::try_parse_from([
            "foxy",
            "server",
            "inspect-addons",
            "--address",
            "127.0.0.1",
            "--port",
            "2302",
        ])
        .unwrap();
        assert!(!command_uses_database(cli.command.as_ref(), false));
        assert!(command_uses_database(cli.command.as_ref(), true));
        let cli =
            CliArgs::try_parse_from(["foxy", "settings", "set", "--app-mode", "server"]).unwrap();
        assert!(command_uses_database(cli.command.as_ref(), false));
    }

    #[test]
    fn update_actions_cover_checks_downloads_and_destructive_redownloads() {
        for (args, action) in [
            (
                vec![
                    "foxy",
                    "sync",
                    "--repo-name",
                    "RepoAlpha",
                    "--mode",
                    "quick-check",
                ],
                "repo.sync",
            ),
            (
                vec![
                    "foxy",
                    "repo",
                    "sync",
                    "--repo-name",
                    "RepoAlpha",
                    "--mode",
                    "download",
                ],
                "repo.sync",
            ),
            (
                vec![
                    "foxy",
                    "repo",
                    "force-redownload",
                    "--repo-name",
                    "RepoAlpha",
                ],
                "repo.force-redownload",
            ),
            (
                vec![
                    "foxy",
                    "addon",
                    "recalc-hashes",
                    "--repo-name",
                    "RepoAlpha",
                    "--addon",
                    "@mod_01",
                ],
                "addon.recalc-hashes",
            ),
            (
                vec![
                    "foxy",
                    "addon",
                    "force-redownload",
                    "--repo-name",
                    "RepoAlpha",
                    "--addon",
                    "@mod_01",
                ],
                "addon.force-redownload",
            ),
            (
                vec![
                    "foxy",
                    "space",
                    "sync",
                    "--space-id",
                    "MainRepo",
                    "--update-all",
                ],
                "space.sync",
            ),
        ] {
            let cli = CliArgs::try_parse_from(args).unwrap();
            assert_eq!(update_action(cli.command.as_ref().unwrap()), Some(action));
        }
    }

    #[test]
    fn launches_and_explicit_wipes_are_not_update_actions() {
        for args in [
            vec!["foxy", "version"],
            vec!["foxy", "launch", "--repo-name", "RepoAlpha"],
            vec!["foxy", "game", "launch"],
            vec!["foxy", "repo", "wipe-db", "--repo-name", "RepoAlpha"],
        ] {
            let cli = CliArgs::try_parse_from(args).unwrap();
            assert_eq!(update_action(cli.command.as_ref().unwrap()), None);
        }
    }

    #[test]
    fn cpu_usage_limit_accepts_ten_to_one_hundred_percent() {
        use crate::cli::args::{CliCommand, SettingsCommand};
        use clap::CommandFactory;

        CliArgs::command().debug_assert();
        let cli = CliArgs::try_parse_from(["foxy", "settings", "set", "--cpu-usage-limit", "75"])
            .unwrap();
        let Some(CliCommand::Settings { command }) = cli.command else {
            panic!("expected a settings command");
        };
        let SettingsCommand::Set(args) = *command else {
            panic!("expected settings set");
        };
        assert_eq!(args.cpu_usage_limit, Some(75));
        assert!(!args.cpu_usage_unlimited);

        for rejected in ["5", "101", "-1"] {
            assert!(
                CliArgs::try_parse_from(["foxy", "settings", "set", "--cpu-usage-limit", rejected])
                    .is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn env_flag_enabled_accepts_common_truthy_values() {
        for value in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(env_flag_enabled(value));
        }
    }

    #[test]
    fn env_flag_enabled_rejects_falsey_or_unknown_values() {
        for value in ["", "0", "false", "off", "maybe"] {
            assert!(!env_flag_enabled(value));
        }
    }
}
