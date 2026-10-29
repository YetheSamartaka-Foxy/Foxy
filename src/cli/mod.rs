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
        debug_mode: bool,
        agent_gui: AgentGuiLaunchConfig,
        debug_modals: Vec<crate::ui::app::debug_modals::DebugModal>,
    },
    Exit(i32),
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
            debug_mode: args.debug_mode,
            agent_gui: AgentGuiLaunchConfig {
                enabled: args.agent_gui,
                port: args.agent_port,
            },
            debug_modals: args.debug_modals.clone(),
        };
    }

    if matches!(cli.command.as_ref(), Some(CliCommand::Version)) {
        match run_command(&cli, CliCommand::Version) {
            Ok(success) => {
                output.success(&success.action, &success.message, success.data);
                return CliExecution::Exit(success.exit_code);
            }
            Err(err) => {
                emit_error(&output, err);
                return CliExecution::Exit(exit_codes::OPERATION_FAILED);
            }
        }
    }

    // Turso has no multi-process access, so a command that touches the database
    // must own it outright. Claim it before dispatching rather than letting the
    // open race a running GUI.
    if command_uses_database(cli.command.as_ref(), cli.wipe_db)
        && let crate::core::tasks::db_process_lock::LockOutcome::Busy { holder_pid } =
            crate::core::tasks::db_process_lock::acquire_for_active_space()
    {
        let owner = holder_pid
            .map(|pid| format!(" (PID {pid})"))
            .unwrap_or_default();
        emit_error(
            &output,
            CommandError {
                action: "database-lock".to_string(),
                message: format!(
                    "the database for this game space is in use by another Foxy process{owner}; close it and retry"
                ),
                code: exit_codes::DATABASE_BUSY,
            },
        );
        return CliExecution::Exit(exit_codes::DATABASE_BUSY);
    }

    // `--wipe-db` is a standalone, non-interactive maintenance operation: wipe
    // and rebuild the whole local database, then exit. Destructive, so it
    // requires explicit `--yes` consent (the CLI never wipes implicitly).
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
            let has_import = space.join(".foxypack-import-recovery").join("pending.json").exists();
            let has_launch = std::fs::read_dir(space.join("launch_preparation"))
                .is_ok_and(|mut entries| entries.next().is_some());
            if has_import || has_launch {
                Err("Recovery is pending. Open Foxy or run a non-preview operation before previewing this setup.".to_string())
            } else { Ok(()) }
        } else {
            crate::core::game::launch_preparation::recover(&space)
                .and_then(|()| crate::core::game::foxypack::recover_pending_import(&space))
        };
        if let Err(message) = result {
            emit_error(&output, CommandError { action: "configuration-recovery".to_string(), message, code: exit_codes::OPERATION_FAILED });
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

/// Whether a dispatch will open the game space database, and so must hold the
/// exclusive process claim.
///
/// The exemptions are the commands that deliberately run *alongside* a live GUI:
/// `agent-gui` drives one over TCP, `steam-helper` is the Steamworks subprocess
/// the GUI itself spawns, and `server` generates repository manifests from the
/// filesystem. `version` and `ui` are handled before this point.
fn command_uses_database(command: Option<&CliCommand>, wipe_db: bool) -> bool {
    if wipe_db {
        return true;
    }
    match command {
        None => false,
        Some(
            CliCommand::AgentGui { .. }
            | CliCommand::SteamHelper { .. }
            | CliCommand::Server { .. }
            | CliCommand::Version,
        ) => false,
        Some(_) => true,
    }
}

fn emit_error(output: &CliOutput, error: CommandError) {
    output.failure(&error.action, &error.message, vec![error.message.clone()]);
}

/// Non-interactively wipe and rebuild the entire local database, then record the
/// current schema version (the same live wipe the GUI schema-upgrade prompt
/// performs). Requires `--yes`; this never prompts.
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
