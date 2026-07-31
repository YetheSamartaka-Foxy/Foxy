use serde_json::json;

use super::{AppState, CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ConfigCommand, ConfigExportArgs, ConfigExtraFileAddArgs, ConfigExtraFileCommand,
    ConfigExtraFileRemoveArgs, ConfigExtraFileSetArgs, ConfigImportArgs,
};
use crate::cli::exit_codes;
use crate::core::game::{extra_files, foxypack, spaces};

pub fn run_config_command(
    cli: &CliArgs,
    command: ConfigCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ConfigCommand::Export(args) => cmd_config_export(cli, args),
        ConfigCommand::Import(args) => cmd_config_import(cli, args),
        ConfigCommand::ExtraFile { command } => run_extra_file_command(cli, command),
    }
}

fn cmd_config_export(
    cli: &CliArgs,
    args: ConfigExportArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let active_space = spaces::active_game_space();
    let module = crate::core::game::registry().active();
    let space_dir = spaces::active_game_space_dir();

    if cli.dry_run {
        let extra_files = crate::core::game::extra_files::load_store(&space_dir)
            .map_err(|err| CommandError::operation("config.export", err))?;
        let workshop_count = if module.id() == crate::core::game::reforger::REFORGER_GAME_ID {
            crate::core::game::reforger::load_store(&space_dir)
                .map_err(|err| CommandError::operation("config.export", err))?
                .entries
                .len()
        } else {
            crate::core::game::workshop::load_store(&space_dir)
                .map_err(|err| CommandError::operation("config.export", err))?
                .entries
                .len()
        };
        let profile_count: usize = state
            .repositories
            .iter()
            .map(|repository| repository.profiles.len())
            .sum();
        return Ok(CommandSuccess {
            action: "config.export".to_string(),
            message: "Dry-run: config export previewed".to_string(),
            data: json!({
                "pack_path": args.output.display().to_string(),
                "game_id": module.id(),
                "game_space_id": active_space.space_id,
                "repository_count": state.repositories.len(),
                "repository_space_count": state.spaces.len(),
                "profile_count": profile_count,
                "extra_file_count": extra_files.entries.len(),
                "workshop_count": workshop_count,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let summary = foxypack::export_pack(
        &space_dir,
        &args.output,
        module,
        &active_space,
        &state.repositories,
        &state.spaces,
    )
    .map_err(|err| CommandError::operation("config.export", err))?;

    Ok(CommandSuccess {
        action: "config.export".to_string(),
        message: format!("Exported config pack to {}", args.output.display()),
        data: json!(summary),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_config_import(
    cli: &CliArgs,
    args: ConfigImportArgs,
) -> Result<CommandSuccess, CommandError> {
    if cli.dry_run {
        let inspection = foxypack::inspect_pack(&args.input)
            .map_err(|err| CommandError::operation("config.import", err))?;
        // Mirror the game check a real import performs so the preview never
        // reports success for a pack the import would reject.
        let active_game_id = crate::core::game::registry().active().id();
        if inspection.game_id != active_game_id {
            return Err(CommandError::validation(
                "config.import",
                format!(
                    "Pack is for game {} but the active game is {}",
                    inspection.game_id, active_game_id
                ),
            ));
        }
        return Ok(CommandSuccess {
            action: "config.import".to_string(),
            message: "Dry-run: config import previewed".to_string(),
            data: json!({
                "pack": inspection,
                "game_compatible": true,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    if !cli.yes {
        // Packs are shared between users and choose their own destinations, so
        // the refusal names every path the import would write to rather than
        // asking for a blind confirmation.
        let targets = foxypack::inspect_pack(&args.input)
            .map(|inspection| {
                inspection
                    .write_targets
                    .iter()
                    .map(|target| format!("\n  {} {} -> {}", target.kind, target.name, target.path))
                    .collect::<String>()
            })
            .unwrap_or_default();
        return Err(CommandError::validation(
            "config.import",
            format!(
                "Importing a config pack modifies the active game space and writes to these paths; pass --yes to confirm{}",
                targets
            ),
        ));
    }

    let mut state = AppState::load()?;
    let module = crate::core::game::registry().active();
    let space_dir = spaces::active_game_space_dir();
    let summary = foxypack::import_pack(
        &space_dir,
        &args.input,
        module,
        &mut state.repositories,
        &mut state.spaces,
    )
    .map_err(|err| CommandError::operation("config.import", err))?;
    state.save_repositories()?;
    state.save_spaces()?;

    Ok(CommandSuccess {
        action: "config.import".to_string(),
        message: format!("Imported config pack from {}", args.input.display()),
        data: json!({
            "summary": summary,
            "active_game_space_id": spaces::active_game_space().space_id
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn run_extra_file_command(
    cli: &CliArgs,
    command: ConfigExtraFileCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ConfigExtraFileCommand::List => cmd_extra_file_list(),
        ConfigExtraFileCommand::Add(args) => cmd_extra_file_add(cli, args),
        ConfigExtraFileCommand::Remove(args) => cmd_extra_file_remove(cli, args),
        ConfigExtraFileCommand::Set(args) => cmd_extra_file_set(cli, args),
        ConfigExtraFileCommand::Activate => cmd_extra_file_activate(cli),
    }
}

fn cmd_extra_file_list() -> Result<CommandSuccess, CommandError> {
    let space_dir = spaces::active_game_space_dir();
    let store = extra_files::load_store(&space_dir)
        .map_err(|err| CommandError::operation("config.extra-file.list", err))?;
    let message = if store.entries.is_empty() {
        "No managed extra files".to_string()
    } else {
        store
            .entries
            .iter()
            .map(|entry| format!("{} - {} -> {}", entry.id, entry.name, entry.destination))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(CommandSuccess {
        action: "config.extra-file.list".to_string(),
        message,
        data: json!({"entries": store.entries}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_extra_file_add(
    cli: &CliArgs,
    args: ConfigExtraFileAddArgs,
) -> Result<CommandSuccess, CommandError> {
    let name = args.name.trim();
    if name.is_empty() {
        return Err(CommandError::validation(
            "config.extra-file.add",
            "Extra file name must be non-empty",
        ));
    }
    if args.destination.trim().is_empty() {
        return Err(CommandError::validation(
            "config.extra-file.add",
            "Destination must be non-empty",
        ));
    }
    if !args.source.exists() {
        return Err(CommandError::not_found(
            "config.extra-file.add",
            format!("Source {} not found", args.source.display()),
        ));
    }

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "config.extra-file.add".to_string(),
            message: "Dry-run: extra-file add previewed".to_string(),
            data: json!({
                "name": name,
                "source": args.source.display().to_string(),
                "destination": args.destination,
                "enabled": !args.disabled,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let space_dir = spaces::active_game_space_dir();
    let entry = extra_files::add_entry(
        &space_dir,
        name,
        &args.source,
        &args.destination,
        !args.disabled,
    )
    .map_err(|err| CommandError::operation("config.extra-file.add", err))?;
    Ok(CommandSuccess {
        action: "config.extra-file.add".to_string(),
        message: format!("Added managed extra file {}", entry.id),
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_extra_file_remove(
    cli: &CliArgs,
    args: ConfigExtraFileRemoveArgs,
) -> Result<CommandSuccess, CommandError> {
    let id = args.id.trim();
    if id.is_empty() {
        return Err(CommandError::validation(
            "config.extra-file.remove",
            "Extra-file id must be non-empty",
        ));
    }

    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = extra_files::load_store(&space_dir)
            .map_err(|err| CommandError::operation("config.extra-file.remove", err))?;
        let entry = store.entry(id).ok_or_else(|| {
            CommandError::not_found(
                "config.extra-file.remove",
                format!("Extra file {} not found", id),
            )
        })?;
        return Ok(CommandSuccess {
            action: "config.extra-file.remove".to_string(),
            message: format!("Dry-run: would remove managed extra file {}", id),
            data: json!({"entry": entry, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if !cli.yes {
        return Err(CommandError::validation(
            "config.extra-file.remove",
            "Removing a managed extra file deletes Foxy's stored copy; pass --yes to confirm",
        ));
    }

    let removed = extra_files::remove_entry(&space_dir, id)
        .map_err(|err| CommandError::operation("config.extra-file.remove", err))?;
    Ok(CommandSuccess {
        action: "config.extra-file.remove".to_string(),
        message: format!("Removed managed extra file {}", removed.id),
        data: json!({"entry": removed}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_extra_file_set(
    cli: &CliArgs,
    args: ConfigExtraFileSetArgs,
) -> Result<CommandSuccess, CommandError> {
    let id = args.id.trim();
    if id.is_empty() {
        return Err(CommandError::validation(
            "config.extra-file.set",
            "Extra-file id must be non-empty",
        ));
    }

    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = extra_files::load_store(&space_dir)
            .map_err(|err| CommandError::operation("config.extra-file.set", err))?;
        let entry = store.entry(id).ok_or_else(|| {
            CommandError::not_found(
                "config.extra-file.set",
                format!("Extra file {} not found", id),
            )
        })?;
        return Ok(CommandSuccess {
            action: "config.extra-file.set".to_string(),
            message: format!("Dry-run: would update managed extra file {}", id),
            data: json!({"entry": entry, "enabled": args.enabled, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let entry = extra_files::set_entry_enabled(&space_dir, id, args.enabled)
        .map_err(|err| CommandError::operation("config.extra-file.set", err))?;
    Ok(CommandSuccess {
        action: "config.extra-file.set".to_string(),
        message: format!("Updated managed extra file {}", entry.id),
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_extra_file_activate(cli: &CliArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let module = crate::core::game::registry().active();
    let install_dir = module.install_dir_from_settings(&state.settings);
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = extra_files::load_store(&space_dir)
            .map_err(|err| CommandError::operation("config.extra-file.activate", err))?;
        let enabled_count = store.entries.iter().filter(|entry| entry.enabled).count();
        return Ok(CommandSuccess {
            action: "config.extra-file.activate".to_string(),
            message: "Dry-run: extra-file activation previewed".to_string(),
            data: json!({
                "enabled_count": enabled_count,
                "skipped_disabled": store.entries.len().saturating_sub(enabled_count),
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if !cli.yes {
        return Err(CommandError::validation(
            "config.extra-file.activate",
            "Activating extra files overwrites destination files; pass --yes to confirm",
        ));
    }

    let summary = extra_files::activate_for_launch(&space_dir, install_dir)
        .map_err(|err| CommandError::operation("config.extra-file.activate", err))?;
    let exit_code = if summary.failed.is_empty() {
        exit_codes::SUCCESS
    } else {
        exit_codes::PARTIAL_SUCCESS
    };
    Ok(CommandSuccess {
        action: "config.extra-file.activate".to_string(),
        message: format!(
            "Activated {} managed extra file(s), {} failed",
            summary.activated.len(),
            summary.failed.len()
        ),
        data: json!({
            "activated": summary.activated,
            "failed": summary.failed,
            "skipped_disabled": summary.skipped_disabled
        }),
        exit_code,
    })
}
