use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerCommand, ServerInspectAddonsArgs};
use crate::cli::exit_codes;
use crate::core::arma3_server_query::query_server_addon_requirements;
use serde_json::{Value, json};

pub fn run_server_command(
    _cli: &CliArgs,
    command: ServerCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ServerCommand::InspectAddons(args) => cmd_server_inspect_addons(args),
    }
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
