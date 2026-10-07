use serde_json::json;
use std::collections::HashMap;
use std::fs;

use super::{AppState, CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, WorkshopAddArgs, WorkshopBundleCommand, WorkshopBundleExportArgs,
    WorkshopBundleImportArgs, WorkshopBundleInspectArgs, WorkshopChecksumArgs, WorkshopCommand,
    WorkshopDownloadArgs, WorkshopDownloadBackend, WorkshopExportArgs, WorkshopExportFormat,
    WorkshopFreezeArgs, WorkshopImportArgs, WorkshopOrderArgs, WorkshopPinsArgs,
    WorkshopRemoveArgs, WorkshopResolveArgs, WorkshopSetArgs, WorkshopShareArgs,
    WorkshopUnfreezeArgs,
};
use crate::cli::exit_codes;
use crate::core::game::workshop::bundle::{BundleExportOptions, BundleManifest};
use crate::core::game::workshop::checksum::{self, StateChecksum};
use crate::core::game::workshop::pin;
use crate::core::game::workshop::share::{ShareCodeOptions, SharedItem};
use crate::core::game::{generic_game, spaces, workshop};
use crate::ui::types::SettingsViewState;

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
        WorkshopCommand::Pins(args) => cmd_workshop_pins(args),
        WorkshopCommand::Unfreeze(args) => cmd_workshop_unfreeze(cli, args),
        WorkshopCommand::Export(args) => cmd_workshop_export(args),
        WorkshopCommand::Share(args) => cmd_workshop_share(args),
        WorkshopCommand::Order(args) => cmd_workshop_order(cli, args),
        WorkshopCommand::Checksum(args) => cmd_workshop_checksum(args),
        WorkshopCommand::Bundle { command } => match command {
            WorkshopBundleCommand::Export(args) => cmd_workshop_bundle_export(cli, args),
            WorkshopBundleCommand::Inspect(args) => cmd_workshop_bundle_inspect(args),
            WorkshopBundleCommand::Import(args) => cmd_workshop_bundle_import(cli, args),
        },
        WorkshopCommand::Resolve(args) => cmd_workshop_resolve(args),
    }
}

fn active_app_id(action: &str, settings: &SettingsViewState) -> Result<u32, CommandError> {
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
    module.steam_app_id_from_settings(settings).ok_or_else(|| {
        CommandError::validation(
            action,
            format!("Active game {} has no Steam app id", module.id()),
        )
    })
}

/// Launch arguments that belong in the state checksum. Only a user-configured
/// game has arguments that differ between players; the fixed modules build
/// their own and would only add noise.
fn checksum_launch_args(settings: &SettingsViewState) -> Vec<String> {
    let module = crate::core::game::registry().active();
    if module.id() != generic_game::GENERIC_GAME_ID {
        return Vec::new();
    }
    generic_game::split_launch_args(&settings.generic_launch_template)
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
        vec![SharedItem {
            item_id,
            ..SharedItem::default()
        }],
        args.name,
        !args.disabled,
        args.freeze,
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
    let mut items = workshop::share::parse_share_code(&input);
    if items.is_empty() {
        return Err(CommandError::validation(
            "workshop.import",
            "Provide at least one Steam Workshop id, URL, or share code",
        ));
    }
    if args.collection {
        if args.download.skip_metadata {
            return Err(CommandError::validation(
                "workshop.import",
                "Collection import requires Steam Web API access; remove --skip-metadata",
            ));
        }
        let collection_ids = items
            .iter()
            .filter(|item| item.is_resolvable())
            .map(|item| item.item_id.clone())
            .collect::<Vec<_>>();
        let children = workshop::fetch_collection_children(&collection_ids)
            .map_err(|err| CommandError::operation("workshop.import", err))?;
        if children.is_empty() {
            return Err(CommandError::not_found(
                "workshop.import",
                "Steam collection did not return any child items",
            ));
        }
        items = children
            .into_iter()
            .map(|item_id| SharedItem {
                item_id,
                ..SharedItem::default()
            })
            .collect();
    }
    import_items(
        cli,
        "workshop.import",
        items,
        None,
        !args.disabled,
        args.freeze,
        &args.download,
    )
}

fn import_items(
    cli: &CliArgs,
    action: &str,
    items: Vec<SharedItem>,
    title_override: Option<String>,
    enabled: bool,
    freeze: bool,
    download: &WorkshopDownloadArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id(action, &state.settings)?;
    let space_dir = spaces::active_game_space_dir();
    let backend = effective_backend(download);

    let unresolved = items
        .iter()
        .filter(|item| !item.is_resolvable())
        .map(SharedItem::label)
        .collect::<Vec<_>>();
    let items = items
        .into_iter()
        .filter(SharedItem::is_resolvable)
        .collect::<Vec<_>>();
    if items.is_empty() {
        return Err(CommandError::not_found(
            action,
            "No Steam Workshop ids to import; the shared list only names local mods",
        ));
    }
    let ids = items
        .iter()
        .map(|item| item.item_id.clone())
        .collect::<Vec<_>>();
    // A shared version pin names files Steam no longer serves, so importing one
    // records the request without claiming the local copy matches it. The
    // matching .foxyshare bundle is what carries those files.
    let pinned = items
        .iter()
        .filter(|item| item.version.is_some())
        .map(|item| item.item_id.clone())
        .collect::<Vec<_>>();

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: action.to_string(),
            message: format!("Dry-run: would import {} Steam Workshop item(s)", ids.len()),
            data: json!({
                "app_id": app_id,
                "item_ids": ids,
                "unresolved": unresolved,
                "pinned_versions": pinned,
                "backend": format!("{:?}", backend),
                "metadata": !download.skip_metadata,
                "enabled": enabled,
                "freeze": freeze,
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
    let mut frozen = Vec::new();
    let mut freeze_failures = Vec::new();
    for item in &items {
        let id = item.item_id.as_str();
        let helper = match download_item(
            action,
            backend,
            app_id,
            id,
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
            id,
            title_override.clone().or_else(|| item.name.clone()),
            metadata.get(id),
            helper.as_ref(),
            enabled,
        )
        .map_err(|err| CommandError::operation(action, err))?;
        if item.load_order.is_some() {
            workshop::set_item_load_order(&space_dir, app_id, id, item.load_order)
                .map_err(|err| CommandError::operation(action, err))?;
        }
        if freeze {
            match workshop::freeze_item(&space_dir, app_id, id, &state.settings.steam_directory) {
                Ok(summary) => frozen.push(summary.item_id),
                Err(error) => freeze_failures.push(json!({"item_id": id, "error": error})),
            }
        }
        if result.added {
            added.push(result.item);
        } else {
            updated.push(result.item);
        }
    }

    let exit_code = if failed_downloads.is_empty() && freeze_failures.is_empty() {
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
            "unresolved": unresolved,
            "pinned_versions": pinned,
            "frozen": frozen,
            "freeze_failures": freeze_failures,
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
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.remove", &state.settings)?;
    let item_id = single_item_id("workshop.remove", &args.item)?;
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
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.set", &state.settings)?;
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
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.freeze", &state.settings)?;
    let space_dir = spaces::active_game_space_dir();
    if args.all {
        return cmd_workshop_freeze_all(cli, args, app_id, &state.settings.steam_directory);
    }
    let Some(item) = args.item.as_deref() else {
        return Err(CommandError::validation(
            "workshop.freeze",
            "Provide one Steam Workshop item id or URL, or pass --all",
        ));
    };
    let item_id = single_item_id("workshop.freeze", item)?;
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
            data: json!({"entry": entry, "refresh": args.refresh, "dry_run": true}),
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

fn cmd_workshop_freeze_all(
    cli: &CliArgs,
    args: WorkshopFreezeArgs,
    app_id: u32,
    steam_directory: &str,
) -> Result<CommandSuccess, CommandError> {
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let statuses = pin::pin_status(&space_dir, app_id, steam_directory)
            .map_err(|err| CommandError::operation("workshop.freeze", err))?;
        let candidates = statuses
            .iter()
            .filter(|status| args.include_disabled || status.enabled)
            .filter(|status| args.refresh || !status.frozen)
            .map(|status| status.item_id.clone())
            .collect::<Vec<_>>();
        return Ok(CommandSuccess {
            action: "workshop.freeze".to_string(),
            message: format!("Dry-run: would freeze {} item(s)", candidates.len()),
            data: json!({"item_ids": candidates, "refresh": args.refresh, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let summary = pin::freeze_all(
        &space_dir,
        app_id,
        steam_directory,
        args.include_disabled,
        args.refresh,
    )
    .map_err(|err| CommandError::operation("workshop.freeze", err))?;
    let exit_code = if summary.failed.is_empty() {
        exit_codes::SUCCESS
    } else {
        exit_codes::PARTIAL_SUCCESS
    };
    Ok(CommandSuccess {
        action: "workshop.freeze".to_string(),
        message: format!(
            "Froze {} item(s), skipped {} already pinned, {} failure(s)",
            summary.frozen.len(),
            summary.skipped.len(),
            summary.failed.len()
        ),
        data: json!(summary),
        exit_code,
    })
}

fn cmd_workshop_pins(args: WorkshopPinsArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.pins", &state.settings)?;
    let space_dir = spaces::active_game_space_dir();
    let statuses = pin::pin_status(&space_dir, app_id, &state.settings.steam_directory)
        .map_err(|err| CommandError::operation("workshop.pins", err))?;
    let drifted = statuses
        .iter()
        .filter(|status| status.state == pin::PinState::Drifted)
        .count();
    let listed = statuses
        .iter()
        .filter(|status| !args.drifted_only || status.state == pin::PinState::Drifted)
        .collect::<Vec<_>>();
    let message = if listed.is_empty() {
        "No managed Steam Workshop items to report".to_string()
    } else {
        listed
            .iter()
            .map(|status| {
                format!(
                    "{} - {} [{}]",
                    status.item_id,
                    status.title.as_deref().unwrap_or(&status.item_id),
                    status.state.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(CommandSuccess {
        action: "workshop.pins".to_string(),
        message,
        data: json!({"app_id": app_id, "drifted": drifted, "items": statuses}),
        exit_code: if drifted == 0 {
            exit_codes::SUCCESS
        } else {
            exit_codes::PARTIAL_SUCCESS
        },
    })
}

fn cmd_workshop_unfreeze(
    cli: &CliArgs,
    args: WorkshopUnfreezeArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.unfreeze", &state.settings)?;
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
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.export", &state.settings)?;
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

fn cmd_workshop_share(args: WorkshopShareArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.share", &state.settings)?;
    let space_dir = spaces::active_game_space_dir();
    let store = workshop::load_store(&space_dir)
        .map_err(|err| CommandError::operation("workshop.share", err))?;
    let items = workshop::share::shared_items_from_store(&store, app_id, args.all);
    let share_code = workshop::share::render_share_code(
        &items,
        ShareCodeOptions {
            include_load_order: args.load_order,
            include_versions: args.versions,
        },
    );
    Ok(CommandSuccess {
        action: "workshop.share".to_string(),
        message: share_code.clone(),
        data: json!({
            "app_id": app_id,
            "share_code": share_code,
            "items": items,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_order(
    cli: &CliArgs,
    args: WorkshopOrderArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.order", &state.settings)?;
    let item_id = single_item_id("workshop.order", &args.item)?;
    let position = match (args.clear, args.position) {
        (true, _) => None,
        (false, Some(position)) => Some(position),
        (false, None) => {
            return Err(CommandError::validation(
                "workshop.order",
                "Provide --position <n> or --clear",
            ));
        }
    };
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = workshop::load_store(&space_dir)
            .map_err(|err| CommandError::operation("workshop.order", err))?;
        let entry = store.entry(app_id, &item_id).ok_or_else(|| {
            CommandError::not_found(
                "workshop.order",
                format!("Steam Workshop item {} is not managed", item_id),
            )
        })?;
        return Ok(CommandSuccess {
            action: "workshop.order".to_string(),
            message: format!("Dry-run: would set load order of {}", item_id),
            data: json!({"entry": entry, "load_order": position, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let entry = workshop::set_item_load_order(&space_dir, app_id, &item_id, position)
        .map_err(|err| CommandError::operation("workshop.order", err))?;
    Ok(CommandSuccess {
        action: "workshop.order".to_string(),
        message: match position {
            Some(position) => format!("Set load order of {} to {}", item_id, position),
            None => format!("Cleared load order of {}", item_id),
        },
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_checksum(args: WorkshopChecksumArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.checksum", &state.settings)?;
    let module = crate::core::game::registry().active();
    let space_dir = spaces::active_game_space_dir();
    let local = checksum::state_checksum_for_space(
        &space_dir,
        module.id(),
        app_id,
        &state.settings.steam_directory,
        &checksum_launch_args(&state.settings),
    )
    .map_err(|err| CommandError::operation("workshop.checksum", err))?;

    let Some(compare_path) = args.compare else {
        return Ok(CommandSuccess {
            action: "workshop.checksum".to_string(),
            message: format!("{} ({} mods)", local.checksum, local.mods.len()),
            data: json!(local),
            exit_code: exit_codes::SUCCESS,
        });
    };

    let raw = fs::read_to_string(&compare_path).map_err(|err| {
        CommandError::operation(
            "workshop.checksum",
            format!("Failed to read {}: {}", compare_path.display(), err),
        )
    })?;
    let remote: StateChecksum = serde_json::from_str(&raw).map_err(|err| {
        CommandError::validation(
            "workshop.checksum",
            format!("Failed to parse checksum file: {}", err),
        )
    })?;
    let diff = checksum::diff_state_checksums(&local, &remote);
    let message = if diff.matches {
        format!("{} matches", local.checksum)
    } else {
        format!(
            "{} does not match {}: {} missing, {} extra, {} version mismatch(es)",
            local.checksum,
            remote.checksum,
            diff.missing_mods.len(),
            diff.extra_mods.len(),
            diff.version_mismatches.len()
        )
    };
    Ok(CommandSuccess {
        action: "workshop.checksum".to_string(),
        message,
        data: json!({"local": local, "remote": remote, "diff": diff}),
        exit_code: if diff.matches {
            exit_codes::SUCCESS
        } else {
            exit_codes::PARTIAL_SUCCESS
        },
    })
}

fn cmd_workshop_bundle_export(
    cli: &CliArgs,
    args: WorkshopBundleExportArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.bundle.export", &state.settings)?;
    let module = crate::core::game::registry().active();
    let space_dir = spaces::active_game_space_dir();
    let output = match args.output.extension() {
        Some(_) => args.output.clone(),
        None => args
            .output
            .with_extension(workshop::bundle::BUNDLE_EXTENSION),
    };
    let options = BundleExportOptions {
        include_disabled: args.all,
        include_frozen_payloads: !args.no_payloads,
    };
    let state_checksum = checksum::state_checksum_for_space(
        &space_dir,
        module.id(),
        app_id,
        &state.settings.steam_directory,
        &checksum_launch_args(&state.settings),
    )
    .map_err(|err| CommandError::operation("workshop.bundle.export", err))?;

    if cli.dry_run {
        let store = workshop::load_store(&space_dir)
            .map_err(|err| CommandError::operation("workshop.bundle.export", err))?;
        let manifest = workshop::bundle::build_manifest(
            &store,
            module.id(),
            app_id,
            options,
            Some(state_checksum),
            args.note,
        );
        return Ok(CommandSuccess {
            action: "workshop.bundle.export".to_string(),
            message: format!(
                "Dry-run: would write {} with {} item(s)",
                output.display(),
                manifest.items.len()
            ),
            data: json!({"manifest": manifest, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let summary = workshop::bundle::export_bundle(
        &space_dir,
        module.id(),
        app_id,
        &output,
        options,
        Some(state_checksum),
        args.note,
    )
    .map_err(|err| CommandError::operation("workshop.bundle.export", err))?;
    Ok(CommandSuccess {
        action: "workshop.bundle.export".to_string(),
        message: format!(
            "Wrote {} with {} item(s) and {} frozen payload(s)",
            summary.path, summary.item_count, summary.payload_count
        ),
        data: json!(summary),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_bundle_inspect(
    args: WorkshopBundleInspectArgs,
) -> Result<CommandSuccess, CommandError> {
    let manifest: BundleManifest = workshop::bundle::read_manifest(&args.input)
        .map_err(|err| CommandError::operation("workshop.bundle.inspect", err))?;
    let payloads = manifest.items.iter().filter(|item| item.payload).count();
    Ok(CommandSuccess {
        action: "workshop.bundle.inspect".to_string(),
        message: format!(
            "{} for app {}: {} item(s), {} frozen payload(s)",
            manifest.game_id,
            manifest.app_id,
            manifest.items.len(),
            payloads
        ),
        data: json!(manifest),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_workshop_bundle_import(
    cli: &CliArgs,
    args: WorkshopBundleImportArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.bundle.import", &state.settings)?;
    let space_dir = spaces::active_game_space_dir();
    let manifest = workshop::bundle::read_manifest(&args.input)
        .map_err(|err| CommandError::operation("workshop.bundle.import", err))?;

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "workshop.bundle.import".to_string(),
            message: format!(
                "Dry-run: would import {} item(s) from {}",
                manifest.items.len(),
                args.input.display()
            ),
            data: json!({"manifest": manifest, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if !cli.yes {
        return Err(CommandError::validation(
            "workshop.bundle.import",
            "Importing a share bundle rewrites workshop.json and restores frozen mod files; pass --yes to confirm",
        ));
    }

    let summary =
        workshop::bundle::import_bundle(&space_dir, app_id, &args.input, !args.skip_payloads)
            .map_err(|err| CommandError::operation("workshop.bundle.import", err))?;

    let backend = effective_backend(&args.download);
    let mut failed_downloads = Vec::new();
    let mut downloaded = Vec::new();
    for item_id in &summary.needs_download {
        match download_item(
            "workshop.bundle.import",
            backend,
            app_id,
            item_id,
            &state.settings.steam_directory,
            &args.download,
        ) {
            Ok(Some(helper)) => {
                workshop::upsert_item(&space_dir, app_id, item_id, None, None, Some(&helper), true)
                    .map_err(|err| CommandError::operation("workshop.bundle.import", err))?;
                downloaded.push(item_id.clone());
                if args.freeze
                    && let Err(error) = workshop::freeze_item(
                        &space_dir,
                        app_id,
                        item_id,
                        &state.settings.steam_directory,
                    )
                {
                    failed_downloads.push(json!({"item_id": item_id, "error": error}));
                }
            }
            Ok(None) => {}
            Err(err) => failed_downloads.push(json!({"item_id": item_id, "error": err.message})),
        }
    }

    let exit_code = if failed_downloads.is_empty() {
        exit_codes::SUCCESS
    } else {
        exit_codes::PARTIAL_SUCCESS
    };
    Ok(CommandSuccess {
        action: "workshop.bundle.import".to_string(),
        message: format!(
            "Imported {} added and {} updated item(s), restored {} frozen payload(s), {} download failure(s)",
            summary.added.len(),
            summary.updated.len(),
            summary.restored_payloads.len(),
            failed_downloads.len()
        ),
        data: json!({
            "summary": summary,
            "downloaded": downloaded,
            "failed_downloads": failed_downloads,
            "share_code": manifest.share_code,
            "state_checksum": manifest.state_checksum,
        }),
        exit_code,
    })
}

fn cmd_workshop_resolve(args: WorkshopResolveArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let app_id = active_app_id("workshop.resolve", &state.settings)?;
    let item_id = single_item_id("workshop.resolve", &args.item)?;
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
