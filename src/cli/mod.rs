pub mod args;
mod commands;
mod exit_codes;
mod output;
pub mod toon;

use clap::{CommandFactory, Parser};

use self::args::{CliArgs, CliCommand};
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

    // Read-only schema gate: warn (never wipe) when the local database is behind
    // a breaking schema bump. Bootstraps the sidecar for fresh installs exactly
    // like the GUI path. Suppressed in machine-readable output modes.
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
        Ok(rt) => rt.block_on(crate::core::tasks::init_database::wipe_database_live()),
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
