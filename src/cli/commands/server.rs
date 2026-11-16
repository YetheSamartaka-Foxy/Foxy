use super::{CommandError, CommandSuccess};
mod configuration_transfer;
mod configurations;
mod consumer_recovery;
mod content;
mod content_acquisition;
mod content_bindings;
mod content_stage;
mod deployment_activation;
mod deployment_composition;
mod deployment_preparation;
mod deployment_records;
mod headless;
mod installations;
mod instances;
mod keys;
mod lifecycle;
mod maintenance;
mod missions;
mod native_install;
mod owner_control;
mod reconciliation;
mod reforger_addons;
mod reforger_configuration;
mod steam_auth;
mod workshop;

use crate::cli::args::{CliArgs, ServerCommand, ServerContextArgs, ServerInspectAddonsArgs};
use crate::cli::exit_codes;
use crate::core::arma3_server_query::query_server_addon_requirements;
use serde_json::{Value, json};

pub fn run_server_command(
    cli: &CliArgs,
    command: ServerCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ServerCommand::ReforgerAddons { context, command } => {
            reforger_addons::run(cli, context, command)
        }
        ServerCommand::Steam { context, command } => steam_auth::run(cli, context, command),
        ServerCommand::ConsoleSignal { .. } => Err(CommandError::validation(
            "server.console-signal",
            "Console control requires an inherited native process handle.",
        )),
        ServerCommand::CleanupWitness { .. } => Err(CommandError::validation(
            "server.cleanup-witness",
            "Cleanup witnessing requires explicitly inherited native authority",
        )),
        ServerCommand::Supervise(args) => owner_control::supervise(cli, args),
        ServerCommand::Owner(args) => {
            let context = instances::capture(args.context, "server.owner")?;
            let owner = crate::core::server::owner::attach(&context)
                .map_err(|error| CommandError::operation("server.owner", error.to_string()))?
                .ok_or_else(|| {
                    CommandError::operation(
                        "server.owner",
                        "No foreground owner is available. Run server supervise --game-space <id>.",
                    )
                })?;
            let snapshot = owner
                .inspect()
                .map_err(|error| CommandError::operation("server.owner", error.to_string()))?;
            let replay = args
                .events_after
                .map(|after| owner.replay_events(after))
                .transpose()
                .map_err(|error| CommandError::operation("server.owner", error.to_string()))?;
            Ok(CommandSuccess {
                action: "server.owner".into(),
                message: "Authenticated owner; Turso was not opened by this client.".into(),
                data: json!({"owner":snapshot,"events":replay,"database_opened":false}),
                exit_code: exit_codes::SUCCESS,
            })
        }
        ServerCommand::Context(args) => cmd_server_context(args),
        ServerCommand::Capabilities(args) => instances::capabilities(args),
        ServerCommand::Mission { context, command } => missions::run(cli, context, command),
        ServerCommand::Keys { context, command } => keys::run(cli, context, command),
        ServerCommand::Instance { context, command } => {
            if matches!(
                &command,
                crate::cli::args::ServerInstanceCommand::RecoveryPlan(_)
                    | crate::cli::args::ServerInstanceCommand::Recover(_)
            ) {
                return consumer_recovery::run(cli, context, command);
            }
            if matches!(
                &command,
                crate::cli::args::ServerInstanceCommand::StartPlan(_)
                    | crate::cli::args::ServerInstanceCommand::Start(_)
                    | crate::cli::args::ServerInstanceCommand::Status(_)
                    | crate::cli::args::ServerInstanceCommand::Monitor(_)
                    | crate::cli::args::ServerInstanceCommand::Logs(_)
                    | crate::cli::args::ServerInstanceCommand::Stop(_)
                    | crate::cli::args::ServerInstanceCommand::Restart(_)
            ) {
                lifecycle::run(cli, context, command)
            } else {
                instances::run(cli, context, command)
            }
        }
        ServerCommand::Workshop { context, command } => workshop::run(cli, context, command),
        ServerCommand::Headless { context, command } => headless::run(cli, context, command),
        ServerCommand::Maintenance { context, command } => maintenance::run(cli, context, command),
        ServerCommand::Installation {
            context,
            command: crate::cli::args::ServerInstallationCommand::Install { command },
        } => native_install::run(cli, context, command, None),
        ServerCommand::Installation {
            context,
            command: crate::cli::args::ServerInstallationCommand::Update { command },
        } => native_install::run(
            cli,
            context,
            command,
            Some(crate::core::server::NativeMaintenanceAction::Update),
        ),
        ServerCommand::Installation {
            context,
            command: crate::cli::args::ServerInstallationCommand::Repair { command },
        } => native_install::run(
            cli,
            context,
            command,
            Some(crate::core::server::NativeMaintenanceAction::Repair),
        ),
        ServerCommand::Installation {
            context,
            command:
                command @ (crate::cli::args::ServerInstallationCommand::Check(_)
                | crate::cli::args::ServerInstallationCommand::Validate(_)),
        } => native_install::inspect(cli, context, command),
        ServerCommand::Installation { context, command } => installations::run(context, command),
        ServerCommand::Configuration { context, command } => {
            configurations::run(cli, context, command)
        }
        ServerCommand::InspectAddons(args) => cmd_server_inspect_addons(args),
        ServerCommand::Content { context, command } => content::run(cli, context, command),
        ServerCommand::Deploy { context, command } => {
            deployment_composition::run(cli, context, command)
        }
    }
}

fn cmd_server_context(args: ServerContextArgs) -> Result<CommandSuccess, CommandError> {
    let root = crate::core::utils::app_paths::configured_data_dir();
    let context = crate::core::server::ServerContext::capture(&root, args.game_space.as_deref())
        .map_err(|error| CommandError::validation("server.context", error))?;
    let module = crate::core::game::registry()
        .get(context.game_id())
        .ok_or_else(|| CommandError::validation("server.context", "Game module is unavailable"))?;
    let support = module.dedicated_server_support();
    let label = match support {
        crate::core::game::DedicatedServerSupport::Unsupported => "unavailable",
        crate::core::game::DedicatedServerSupport::WorkInProgress => "work in progress",
    };
    Ok(CommandSuccess {
        action: "server.context".to_string(),
        message: format!(
            "Game space {} ({}) - dedicated server support: {}",
            context.display_name(),
            module.display_name(),
            label
        ),
        data: json!({
            "game_space_id": context.space_id(),
            "game_id": context.game_id(),
            "display_name": context.display_name(),
            "data_root": context.data_root(),
            "space_directory": context.space_dir(),
            "database_path": context.database_path(),
            "app_settings_path": context.app_settings_path(),
            "dedicated_server_support": support,
            "database_opened": false,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_server_inspect_addons(
    args: ServerInspectAddonsArgs,
) -> Result<CommandSuccess, CommandError> {
    let address = args.address.trim();
    if address.is_empty() {
        return Err(CommandError::validation(
            "server.inspect-addons",
            "Server address is required",
        ));
    }

    let result = query_server_addon_requirements(address, args.port).map_err(|err| {
        CommandError::operation(
            "server.inspect-addons",
            format!(
                "Failed to query addon metadata from {}:{}: {}",
                address, args.port, err
            ),
        )
    })?;

    let data = if args.include_rules {
        json!({
            "address": result.address,
            "game_port": result.game_port,
            "query_port": result.query_port,
            "requirements": result.requirements,
            "server_browser_protocol": result.server_browser_protocol,
            "info_keywords": result.info_keywords,
            "rules": result.rules,
        })
    } else {
        json!({
            "address": result.address,
            "game_port": result.game_port,
            "query_port": result.query_port,
            "requirements": result.requirements,
            "server_browser_protocol": result.server_browser_protocol,
            "rule_count": result.rules.len(),
        })
    };

    Ok(CommandSuccess {
        action: "server.inspect-addons".to_string(),
        message: inspect_addons_message(&data),
        data,
        exit_code: exit_codes::SUCCESS,
    })
}

fn inspect_addons_message(data: &Value) -> String {
    let address = data
        .get("address")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let game_port = data
        .get("game_port")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let requirements = data
        .get("requirements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if requirements.is_empty() {
        return format!(
            "No server addon requirements reported by {}:{}",
            address, game_port
        );
    }

    let mut lines = vec![format!(
        "Server {}:{} reported {} addon requirement(s):",
        address,
        game_port,
        requirements.len()
    )];
    for requirement in requirements {
        if let Some(name) = requirement.get("display_name").and_then(Value::as_str) {
            let hash = requirement
                .get("reported_hash")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| format!(" hash={}", value))
                .unwrap_or_default();
            let workshop_id = requirement
                .get("workshop_id")
                .and_then(Value::as_u64)
                .map(|value| format!(" workshop_id={}", value))
                .unwrap_or_default();
            lines.push(format!("- {}{}{}", name, workshop_id, hash));
        }
    }
    lines.join("\n")
}
