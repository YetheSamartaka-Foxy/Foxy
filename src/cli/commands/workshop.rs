use serde_json::json;
use std::collections::HashMap;
use std::fs;

use super::{AppState, CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, WorkshopAddArgs, WorkshopCommand, WorkshopDownloadArgs, WorkshopDownloadBackend,
    WorkshopExportArgs, WorkshopExportFormat, WorkshopFreezeArgs, WorkshopImportArgs,
    WorkshopRemoveArgs, WorkshopResolveArgs, WorkshopSetArgs, WorkshopUnfreezeArgs,
};
use crate::cli::exit_codes;
use crate::core::game::{spaces, workshop};

pub fn run_workshop_command(
    cli: &CliArgs,
    command: WorkshopCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        WorkshopCommand::List => cmd_workshop_list(),
        WorkshopCommand::Add(args) => cmd_workshop_add(cli, args),
        WorkshopCommand::Import(args) => cmd_workshop_import(cli, args),
        WorkshopCommand::Remove(args) => cmd_workshop_remove(cli, args),
        WorkshopCommand::Set(args) => cmd_workshop_set(cli, args),
        WorkshopCommand::Freeze(args) => cmd_workshop_freeze(cli, args),
        WorkshopCommand::Unfreeze(args) => cmd_workshop_unfreeze(cli, args),
        WorkshopCommand::Export(args) => cmd_workshop_export(args),
        WorkshopCommand::Resolve(args) => cmd_workshop_resolve(args),
    }
}

fn active_app_id(action: &str) -> Result<u32, CommandError> {
    let module = crate::core::game::registry().active();
    if !module.capabilities().steam_workshop {
        return Err(CommandError::validation(
            action,
            format!(
                "Active game {} does not support Steam Workshop",
                module.id()
            ),
        ));
    }
    module.steam_app_id().ok_or_else(|| {
        CommandError::validation(
            action,
            format!("Active game {} has no Steam app id", module.id()),
        )
    })
}

fn cmd_workshop_list() -> Result<CommandSuccess, CommandError> {
    let space_dir = spaces::active_game_space_dir();
    let store = workshop::load_store(&space_dir)
        .map_err(|err| CommandError::operation("workshop.list", err))?;
    let message = if store.entries.is_empty() {
        "No managed Steam Workshop items".to_string()
    } else {
        store
            .entries
            .iter()
            .map(|entry| {
                let title = entry.title.as_deref().unwrap_or(&entry.item_id);
                let frozen = if entry.frozen { " frozen" } else { "" };
                let disabled = if entry.enabled { "" } else { " disabled" };
                format!("{} - {}{}{}", entry.item_id, title, frozen, disabled)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(CommandSuccess {
        action: "workshop.list".to_string(),
        message,
        data: json!({"entries": store.entries}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_add(cli: &CliArgs, args: WorkshopAddArgs) -> Result<CommandSuccess, CommandError> {
    let item_id = single_item_id("workshop.add", &args.item)?;
    import_items(
        cli,
        "workshop.add",
        vec![item_id],
        args.name,
        !args.disabled,
        &args.download,
    )
}

fn cmd_workshop_import(
    cli: &CliArgs,
    args: WorkshopImportArgs,
) -> Result<CommandSuccess, CommandError> {
    let mut input = args.input.join("\n");
    if let Some(path) = args.from_file.as_ref() {
        let from_file = fs::read_to_string(path).map_err(|err| {
            CommandError::operation(
                "workshop.import",
                format!("Failed to read {}: {}", path.display(), err),
            )
        })?;
        if !input.is_empty() {
            input.push('\n');
        }
        input.push_str(&from_file);
    }
    let mut ids = workshop::parse_workshop_item_ids(&input);
    if ids.is_empty() {
        return Err(CommandError::validation(
            "workshop.import",
            "Provide at least one Steam Workshop id or URL",
        ));
    }
    if args.collection {
        if args.download.skip_metadata {
            return Err(CommandError::validation(
                "workshop.import",
                "Collection import requires Steam Web API access; remove --skip-metadata",
            ));
        }
        ids = workshop::fetch_collection_children(&ids)
            .map_err(|err| CommandError::operation("workshop.import", err))?;
        if ids.is_empty() {
            return Err(CommandError::not_found(
                "workshop.import",
                "Steam collection did not return any child items",
            ));
        }
    }
    import_items(
        cli,
        "workshop.import",
        ids,
        None,
        !args.disabled,
        &args.download,
    )
}

fn import_items(
    cli: &CliArgs,
    action: &str,
    ids: Vec<String>,
    title_override: Option<String>,
    enabled: bool,
    download: &WorkshopDownloadArgs,
) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id(action)?;
    let space_dir = spaces::active_game_space_dir();
    let state = AppState::load()?;
    let backend = effective_backend(download);

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: action.to_string(),
            message: format!("Dry-run: would import {} Steam Workshop item(s)", ids.len()),
            data: json!({
                "app_id": app_id,
                "item_ids": ids,
                "backend": format!("{:?}", backend),
                "metadata": !download.skip_metadata,
                "enabled": enabled,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let metadata = if download.skip_metadata {
        HashMap::new()
    } else {
        let metadata = workshop::fetch_published_file_details(&ids)
            .map_err(|err| CommandError::operation(action, err))?;
        let metadata = workshop::metadata_by_id(metadata);
        workshop::validate_metadata_app_ids(&metadata, app_id)
            .map_err(|err| CommandError::validation(action, err))?;
        metadata
    };

    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut failed_downloads = Vec::new();
    for id in ids {
        let helper = match download_item(
            action,
            backend,
            app_id,
            &id,
            &state.settings.steam_directory,
            download,
        ) {
            Ok(helper) => helper,
            Err(err) => {
                failed_downloads.push(json!({"item_id": id, "error": err.message}));
                None
            }
        };
        let result = workshop::upsert_item(
            &space_dir,
            app_id,
            &id,
            title_override.clone(),
            metadata.get(&id),
            helper.as_ref(),
            enabled,
        )
        .map_err(|err| CommandError::operation(action, err))?;
        if result.added {
            added.push(result.item);
        } else {
            updated.push(result.item);
        }
    }

    let exit_code = if failed_downloads.is_empty() {
        exit_codes::SUCCESS
    } else {
        exit_codes::PARTIAL_SUCCESS
    };
    Ok(CommandSuccess {
        action: action.to_string(),
        message: format!(
            "Imported {} Steam Workshop item(s), {} download failure(s)",
            added.len() + updated.len(),
            failed_downloads.len()
        ),
        data: json!({
            "app_id": app_id,
            "added": added,
            "updated": updated,
            "failed_downloads": failed_downloads,
            "backend": format!("{:?}", backend),
        }),
        exit_code,
    })
}

fn download_item(
    action: &str,
    backend: WorkshopDownloadBackend,
    app_id: u32,
    item_id: &str,
    steam_directory: &str,
    args: &WorkshopDownloadArgs,
) -> Result<Option<workshop::SteamHelperOutcome>, CommandError> {
    match backend {
        WorkshopDownloadBackend::None => Ok(None),
        WorkshopDownloadBackend::SteamHelper => {
            workshop::run_steam_helper_install(app_id, item_id, args.timeout_seconds)
                .map(Some)
                .map_err(|err| CommandError::operation(action, err))
        }
        WorkshopDownloadBackend::Steamcmd => {
            workshop::run_steamcmd_download(
                app_id,
                item_id,
                args.steamcmd.as_deref(),
                args.steamcmd_user.as_deref(),
            )
            .map_err(|err| CommandError::operation(action, err))?;
            let installed_path = workshop::workshop_content_path(steam_directory, app_id, item_id);
            Ok(Some(workshop::SteamHelperOutcome {
                app_id,
                item_id: item_id.to_string(),
                subscribed: false,
                download_started: true,
                installed: installed_path.is_some(),
                installed_path: installed_path.map(|path| path.display().to_string()),
                size_bytes: None,
                timestamp: None,
                downloaded_bytes: None,
                total_bytes: None,
            }))
        }
    }
}

fn cmd_workshop_remove(
    cli: &CliArgs,
    args: WorkshopRemoveArgs,
) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id("workshop.remove")?;
    let item_id = single_item_id("workshop.remove", &args.item)?;
    let state = AppState::load()?;
    let space_dir = spaces::active_game_space_dir();
    let store = workshop::load_store(&space_dir)
        .map_err(|err| CommandError::operation("workshop.remove", err))?;
    let entry = store.entry(app_id, &item_id).ok_or_else(|| {
        CommandError::not_found(
            "workshop.remove",
            format!("Steam Workshop item {} is not managed", item_id),
        )
    })?;

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "workshop.remove".to_string(),
            message: format!("Dry-run: would remove Steam Workshop item {}", item_id),
            data: json!({
                "entry": entry,
                "delete_data": args.delete_data,
                "backend": format!("{:?}", args.backend),
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if !cli.yes {
        return Err(CommandError::validation(
            "workshop.remove",
            "Removing a Workshop item modifies workshop.json and may unsubscribe or delete files; pass --yes to confirm",
        ));
    }
    match args.backend {
        WorkshopDownloadBackend::SteamHelper => {
            workshop::run_steam_helper_remove(app_id, &item_id, args.timeout_seconds)
                .map_err(|err| CommandError::operation("workshop.remove", err))?;
        }
        WorkshopDownloadBackend::None => {}
        WorkshopDownloadBackend::Steamcmd => {
            return Err(CommandError::validation(
                "workshop.remove",
                "SteamCMD cannot unsubscribe Workshop items; use --backend steam-helper or --backend none",
            ));
        }
    }
    let summary = workshop::remove_item(
        &space_dir,
        app_id,
        &item_id,
        &state.settings.steam_directory,
        args.delete_data,
    )
    .map_err(|err| CommandError::operation("workshop.remove", err))?;
    Ok(CommandSuccess {
        action: "workshop.remove".to_string(),
        message: format!("Removed Steam Workshop item {}", item_id),
        data: json!(summary),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_set(cli: &CliArgs, args: WorkshopSetArgs) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id("workshop.set")?;
    let item_id = single_item_id("workshop.set", &args.item)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = workshop::load_store(&space_dir)
            .map_err(|err| CommandError::operation("workshop.set", err))?;
        let entry = store.entry(app_id, &item_id).ok_or_else(|| {
            CommandError::not_found(
                "workshop.set",
                format!("Steam Workshop item {} is not managed", item_id),
            )
        })?;
        return Ok(CommandSuccess {
            action: "workshop.set".to_string(),
            message: format!("Dry-run: would update Steam Workshop item {}", item_id),
            data: json!({"entry": entry, "enabled": args.enabled, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let entry = workshop::set_item_enabled(&space_dir, app_id, &item_id, args.enabled)
        .map_err(|err| CommandError::operation("workshop.set", err))?;
    Ok(CommandSuccess {
        action: "workshop.set".to_string(),
        message: format!("Updated Steam Workshop item {}", item_id),
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_freeze(
    cli: &CliArgs,
    args: WorkshopFreezeArgs,
) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id("workshop.freeze")?;
    let item_id = single_item_id("workshop.freeze", &args.item)?;
    let state = AppState::load()?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = workshop::load_store(&space_dir)
            .map_err(|err| CommandError::operation("workshop.freeze", err))?;
        let entry = store.entry(app_id, &item_id).ok_or_else(|| {
            CommandError::not_found(
                "workshop.freeze",
                format!("Steam Workshop item {} is not managed", item_id),
            )
        })?;
        return Ok(CommandSuccess {
            action: "workshop.freeze".to_string(),
            message: format!("Dry-run: would freeze Steam Workshop item {}", item_id),
            data: json!({"entry": entry, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let summary = workshop::freeze_item(
        &space_dir,
        app_id,
        &item_id,
        &state.settings.steam_directory,
    )
    .map_err(|err| CommandError::operation("workshop.freeze", err))?;
    Ok(CommandSuccess {
        action: "workshop.freeze".to_string(),
        message: format!("Frozen Steam Workshop item {}", item_id),
        data: json!(summary),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_unfreeze(
    cli: &CliArgs,
    args: WorkshopUnfreezeArgs,
) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id("workshop.unfreeze")?;
    let item_id = single_item_id("workshop.unfreeze", &args.item)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = workshop::load_store(&space_dir)
            .map_err(|err| CommandError::operation("workshop.unfreeze", err))?;
        let entry = store.entry(app_id, &item_id).ok_or_else(|| {
            CommandError::not_found(
                "workshop.unfreeze",
                format!("Steam Workshop item {} is not managed", item_id),
            )
        })?;
        return Ok(CommandSuccess {
            action: "workshop.unfreeze".to_string(),
            message: format!("Dry-run: would unfreeze Steam Workshop item {}", item_id),
            data: json!({"entry": entry, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let entry = workshop::unfreeze_item(&space_dir, app_id, &item_id)
        .map_err(|err| CommandError::operation("workshop.unfreeze", err))?;
    Ok(CommandSuccess {
        action: "workshop.unfreeze".to_string(),
        message: format!("Unfrozen Steam Workshop item {}", item_id),
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_export(args: WorkshopExportArgs) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id("workshop.export")?;
    let space_dir = spaces::active_game_space_dir();
    let store = workshop::load_store(&space_dir)
        .map_err(|err| CommandError::operation("workshop.export", err))?;
    let entries = store
        .entries
        .into_iter()
        .filter(|entry| entry.app_id == app_id)
        .filter(|entry| args.all || entry.enabled)
        .collect::<Vec<_>>();
    let lines = entries
        .iter()
        .map(|entry| match args.format {
            WorkshopExportFormat::Ids => entry.item_id.clone(),
            WorkshopExportFormat::Urls => entry.url.clone(),
        })
        .collect::<Vec<_>>();
    let text = lines.join("\n");
    Ok(CommandSuccess {
        action: "workshop.export".to_string(),
        message: text.clone(),
        data: json!({
            "format": format!("{:?}", args.format),
            "text": text,
            "items": entries,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_resolve(args: WorkshopResolveArgs) -> Result<CommandSuccess, CommandError> {
    let app_id = active_app_id("workshop.resolve")?;
    let item_id = single_item_id("workshop.resolve", &args.item)?;
    let state = AppState::load()?;
    let space_dir = spaces::active_game_space_dir();
    let resolution = workshop::resolve_launch_path(
        &space_dir,
        app_id,
        &item_id,
        &state.settings.steam_directory,
    )
    .map_err(|err| CommandError::operation("workshop.resolve", err))?;
    Ok(CommandSuccess {
        action: "workshop.resolve".to_string(),
        message: resolution.path.clone(),
        data: json!(resolution),
        exit_code: exit_codes::SUCCESS,
    })
}

fn effective_backend(args: &WorkshopDownloadArgs) -> WorkshopDownloadBackend {
    if args.skip_download {
        WorkshopDownloadBackend::None
    } else {
        args.backend
    }
}

fn single_item_id(action: &str, input: &str) -> Result<String, CommandError> {
    let ids = workshop::parse_workshop_item_ids(input);
    match ids.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(CommandError::validation(
            action,
            "Provide one Steam Workshop item id or URL",
        )),
        _ => Err(CommandError::validation(
            action,
            "Provide exactly one Steam Workshop item id or URL",
        )),
    }
}
