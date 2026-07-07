use serde_json::json;

use super::{CommandError, CommandSuccess};
use crate::cli::args::{SteamHelperCommand, SteamHelperItemArgs};
use crate::cli::exit_codes;
use crate::core::game::workshop;

pub fn run_steam_helper_command(
    command: SteamHelperCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        SteamHelperCommand::Install(args) => helper_install(args),
        SteamHelperCommand::Remove(args) => helper_remove(args),
        SteamHelperCommand::Status(args) => helper_status(args),
    }
}

fn helper_install(args: SteamHelperItemArgs) -> Result<CommandSuccess, CommandError> {
    let outcome =
        workshop::steamworks_install_item(args.app_id, &args.item_id, args.timeout_seconds)
            .map_err(|err| CommandError::operation("steam-helper.install", err))?;
    Ok(CommandSuccess {
        action: "steam-helper.install".to_string(),
        message: format!("Installed Steam Workshop item {}", outcome.item_id),
        data: json!(outcome),
        exit_code: exit_codes::SUCCESS,
    })
}

fn helper_remove(args: SteamHelperItemArgs) -> Result<CommandSuccess, CommandError> {
    let outcome =
        workshop::steamworks_remove_item(args.app_id, &args.item_id, args.timeout_seconds)
            .map_err(|err| CommandError::operation("steam-helper.remove", err))?;
    Ok(CommandSuccess {
        action: "steam-helper.remove".to_string(),
        message: format!("Removed Steam Workshop item {}", outcome.item_id),
        data: json!(outcome),
        exit_code: exit_codes::SUCCESS,
    })
}

fn helper_status(args: SteamHelperItemArgs) -> Result<CommandSuccess, CommandError> {
    let outcome = workshop::steamworks_status_item(args.app_id, &args.item_id)
        .map_err(|err| CommandError::operation("steam-helper.status", err))?;
    Ok(CommandSuccess {
        action: "steam-helper.status".to_string(),
        message: format!("Read Steam Workshop item {}", outcome.item_id),
        data: json!(outcome),
        exit_code: exit_codes::SUCCESS,
    })
}
