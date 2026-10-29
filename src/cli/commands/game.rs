use super::{AppState, CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, GameCommand, GameCreateArgs, GameLaunchArgs, GameRemoveArgs, GameUseArgs,
    ReforgerAddArgs, ReforgerCommand, ReforgerExportArgs, ReforgerFreezeArgs, ReforgerImportArgs,
    ReforgerRemoveArgs, ReforgerResolveArgs, ReforgerSetArgs, ReforgerUnfreezeArgs,
};
use crate::cli::exit_codes;
use crate::core::game::{
    GameLaunchCtx, GameModule, LaunchCommand, LaunchError, LaunchPlan, generic_game,
    launch_preparation::{LaunchApproval, LaunchFailure, LaunchHandoff},
    reforger, spaces, twwh3,
};
use crate::core::steam::{SteamEnsureResult, ensure_steam_running};
use serde_json::{Value, json};
use std::fs;

pub fn run_game_command(
    cli: &CliArgs,
    command: GameCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        GameCommand::List => cmd_game_list(),
        GameCommand::Use(args) => cmd_game_use(cli, args),
        GameCommand::Create(args) => cmd_game_create(cli, args),
        GameCommand::Remove(args) => cmd_game_remove(cli, args),
        GameCommand::Launch(args) => cmd_game_launch(cli, args),
        GameCommand::Reforger { command } => run_reforger_command(cli, command),
    }
}

fn load_registry(action: &str) -> Result<spaces::GamesRegistryFile, CommandError> {
    spaces::load_registry().map_err(|err| CommandError::operation(action, err))
}

fn space_json(registry: &spaces::GamesRegistryFile, entry: &spaces::GameSpaceEntry) -> Value {
    json!({
        "id": entry.id,
        "game_id": entry.game_id,
        "display_name": entry.display_name,
        "created_at": entry.created_at,
        "active": entry.id == registry.active_game_space_id,
        "path": spaces::game_space_dir_for(&entry.id).display().to_string(),
        "module_available": crate::core::game::registry().get(&entry.game_id).is_some(),
    })
}

fn cmd_game_list() -> Result<CommandSuccess, CommandError> {
    let registry = load_registry("game.list")?;
    let mut lines = Vec::new();
    for entry in &registry.game_spaces {
        let marker = if entry.id == registry.active_game_space_id {
            " [active]"
        } else {
            ""
        };
        lines.push(format!(
            "{} ({}) - {}{}",
            entry.id, entry.game_id, entry.display_name, marker
        ));
    }
    let spaces_json: Vec<Value> = registry
        .game_spaces
        .iter()
        .map(|entry| space_json(&registry, entry))
        .collect();
    Ok(CommandSuccess {
        action: "game.list".to_string(),
        message: lines.join("\n"),
        data: json!({
            "active_game_space_id": registry.active_game_space_id,
            "last_opened_game_space_id": registry.last_opened_game_space_id,
            "game_spaces": spaces_json,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_game_use(cli: &CliArgs, args: GameUseArgs) -> Result<CommandSuccess, CommandError> {
    if cli.dry_run {
        let registry = load_registry("game.use")?;
        let entry = registry
            .game_spaces
            .iter()
            .find(|entry| entry.id == args.space_id)
            .ok_or_else(|| {
                CommandError::not_found(
                    "game.use",
                    format!("Game space {} not found", args.space_id),
                )
            })?;
        return Ok(CommandSuccess {
            action: "game.use".to_string(),
            message: format!("Dry-run: would activate game space {}", entry.id),
            data: json!({"space": space_json(&registry, entry), "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let entry = spaces::set_active_game_space(&args.space_id).map_err(|err| {
        if err.contains("does not exist") {
            CommandError::not_found("game.use", err)
        } else {
            CommandError::operation("game.use", err)
        }
    })?;
    let registry = load_registry("game.use")?;
    Ok(CommandSuccess {
        action: "game.use".to_string(),
        message: format!(
            "Active game space is now {} ({}). The UI loads it on next start.",
            entry.id, entry.display_name
        ),
        data: json!({"space": space_json(&registry, &entry)}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_game_create(cli: &CliArgs, args: GameCreateArgs) -> Result<CommandSuccess, CommandError> {
    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "game.create".to_string(),
            message: format!(
                "Dry-run: would create game space {} for game {}",
                args.name, args.game
            ),
            data: json!({"name": args.name, "game_id": args.game, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let entry = spaces::create_game_space(&args.game, &args.name)
        .map_err(|err| CommandError::validation("game.create", err))?;
    // steam_directory is app-global, so the active space's merged settings are
    // a valid source for install-dir detection in the new space.
    let steam_directory = AppState::load()
        .map(|state| state.settings.steam_directory)
        .unwrap_or_default();
    let detected_install_dir = spaces::seed_new_game_space_settings(&entry, &steam_directory);
    let registry = load_registry("game.create")?;
    Ok(CommandSuccess {
        action: "game.create".to_string(),
        message: format!("Created game space {} ({})", entry.id, entry.display_name),
        data: json!({
            "space": space_json(&registry, &entry),
            "detected_install_dir": detected_install_dir,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_game_remove(cli: &CliArgs, args: GameRemoveArgs) -> Result<CommandSuccess, CommandError> {
    let registry = load_registry("game.remove")?;
    let entry = registry
        .game_spaces
        .iter()
        .find(|entry| entry.id == args.space_id)
        .ok_or_else(|| {
            CommandError::not_found(
                "game.remove",
                format!("Game space {} not found", args.space_id),
            )
        })?
        .clone();
    let payloads = spaces::private_payload_preview(&args.space_id)
        .map_err(|error| CommandError::operation("game.remove", error))?;
    let has_private_payloads =
        !payloads.workshop_item_ids.is_empty() || !payloads.extra_file_ids.is_empty();

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "game.remove".to_string(),
            message: format!(
                "Dry-run: would remove game space {} and delete {}",
                entry.id,
                spaces::game_space_dir_for(&entry.id).display()
            ),
            data: json!({
                "space": space_json(&registry, &entry),
                "private_payloads": payloads,
                "payload_action_required": has_private_payloads && !args.preserve_payloads && !args.discard_payloads,
                "preserve_payloads": args.preserve_payloads,
                "discard_payloads": args.discard_payloads,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if has_private_payloads && !args.preserve_payloads && !args.discard_payloads {
        return Err(CommandError::validation(
            "game.remove",
            format!(
                "Game space has {} frozen Workshop item(s) and {} extra-file payload(s); choose --preserve-payloads or --discard-payloads",
                payloads.workshop_item_ids.len(),
                payloads.extra_file_ids.len()
            ),
        ));
    }
    if !cli.yes {
        return Err(CommandError::validation(
            "game.remove",
            "Removing a game space deletes its Foxy workspace (repositories list, game settings, database); pass --yes to confirm",
        ));
    }

    let preserve_to = (has_private_payloads && args.preserve_payloads)
        .then(|| crate::core::utils::app_paths::foxy_data_dir().join("preserved_game_spaces"));
    let removed = spaces::remove_game_space_with_policy(
        &args.space_id,
        spaces::GameSpaceRemovalPolicy {
            preserve_private_payloads_to: preserve_to.clone(),
        },
    )
    .map_err(|err| CommandError::operation("game.remove", err))?;
    Ok(CommandSuccess {
        action: "game.remove".to_string(),
        message: format!(
            "Removed game space {} ({})",
            removed.id, removed.display_name
        ),
        data: json!({
            "id": removed.id,
            "game_id": removed.game_id,
            "display_name": removed.display_name,
            "preserved_payloads_to": preserve_to.map(|path| path.join(&removed.id).display().to_string()),
            "discarded_private_payloads": has_private_payloads && args.discard_payloads,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_game_launch(cli: &CliArgs, args: GameLaunchArgs) -> Result<CommandSuccess, CommandError> {
    let module = crate::core::game::registry().active();
    if module.id() == twwh3::TWWH3_GAME_ID {
        return cmd_twwh3_game_launch(cli, args);
    }
    if module.id() == reforger::REFORGER_GAME_ID {
        return cmd_reforger_game_launch(cli, args);
    }
    if module.id() == generic_game::GENERIC_GAME_ID {
        return cmd_generic_game_launch(cli, args);
    }

    if module.capabilities().repository_launch {
        return Err(CommandError::validation(
            "game.launch",
            format!(
                "{} launches from a repository; use `foxy launch` instead",
                module.display_name()
            ),
        ));
    }
    Err(CommandError::validation(
        "game.launch",
        format!(
            "Active game {} has no standalone launch path",
            module.display_name()
        ),
    ))
}

fn cmd_generic_game_launch(
    cli: &CliArgs,
    args: GameLaunchArgs,
) -> Result<CommandSuccess, CommandError> {
    let module = crate::core::game::registry().active();
    let state = AppState::load()?;
    let config = generic_game::config_from_settings(&state.settings);
    let space_dir = spaces::active_game_space_dir();

    let launch_plan = match config.steam_app_id {
        Some(app_id) => generic_game::build_workshop_launch_plan(
            &space_dir,
            app_id,
            &state.settings.steam_directory,
            args.include_disabled,
            Vec::new(),
        )
        .map_err(|err| CommandError::operation("game.launch", err))?,
        None => generic_game::GenericWorkshopLaunchPlan {
            plan: LaunchPlan {
                launch_args: Vec::new(),
                mods: Vec::new(),
                server: None,
                game_build: None,
            },
            issues: Vec::new(),
        },
    };

    if args.execute && !cli.dry_run && !launch_plan.issues.is_empty() {
        return Err(CommandError::validation(
            "game.launch",
            format!(
                "Cannot launch with {} unresolved Workshop item(s)",
                launch_plan.issues.len()
            ),
        ));
    }

    let preview_command = build_launch_preview(module, &state.settings, &launch_plan.plan)?;

    if cli.dry_run || !args.execute {
        return Ok(CommandSuccess {
            action: "game.launch".to_string(),
            message: "Launch command generated".to_string(),
            data: json!({
                "game_id": module.id(),
                "game_space_id": spaces::active_game_space().space_id,
                "executable": preview_command.program.display().to_string(),
                "args": preview_command.args,
                "cwd": preview_command.cwd.as_ref().map(|path| path.display().to_string()),
                "manifest": null,
                "config": config,
                "issues": launch_plan.issues,
                "execute": false,
                "dry_run": cli.dry_run,
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let (executed, steam_status) = dispatch_standalone_launch(
        module,
        &state.settings,
        &launch_plan.plan,
        LaunchApproval {
            incomplete_extra_files: args.allow_incomplete_preparation,
            live_workshop_fallback: args.allow_live_fallback,
        },
    )?;
    let command = executed.command;

    Ok(CommandSuccess {
        action: "game.launch".to_string(),
        message: format!("Launch command started (pid={})", executed.pid),
        data: json!({
            "game_id": module.id(),
            "game_space_id": spaces::active_game_space().space_id,
            "executable": command.program.display().to_string(),
            "args": command.args,
            "cwd": command.cwd.as_ref().map(|path| path.display().to_string()),
            "manifest": null,
            "steam_status": steam_status,
            "extra_files": activation_json(&executed.extra_files),
            "issues": launch_plan.issues,
            "pid": executed.pid,
            "execute": true,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_twwh3_game_launch(
    cli: &CliArgs,
    args: GameLaunchArgs,
) -> Result<CommandSuccess, CommandError> {
    let module = crate::core::game::registry().active();
    let state = AppState::load()?;
    let space_dir = spaces::active_game_space_dir();
    let launch_plan = twwh3::build_workshop_launch_plan(
        &space_dir,
        &state.settings.steam_directory,
        args.include_disabled,
        args.save_name.as_deref(),
    )
    .map_err(|err| CommandError::operation("game.launch", err))?;

    if args.execute && !cli.dry_run && !launch_plan.issues.is_empty() {
        return Err(CommandError::validation(
            "game.launch",
            format!(
                "Cannot launch with {} unresolved Workshop item(s)",
                launch_plan.issues.len()
            ),
        ));
    }

    let preview_command = build_launch_preview(module, &state.settings, &launch_plan.plan)?;

    if cli.dry_run || !args.execute {
        return Ok(CommandSuccess {
            action: "game.launch".to_string(),
            message: "Launch command generated".to_string(),
            data: json!({
                "game_id": module.id(),
                "game_space_id": spaces::active_game_space().space_id,
                "executable": preview_command.program.display().to_string(),
                "args": preview_command.args,
                "cwd": preview_command.cwd.as_ref().map(|path| path.display().to_string()),
                "manifest": null,
                "packs": launch_plan.packs,
                "issues": launch_plan.issues,
                "execute": false,
                "dry_run": cli.dry_run,
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let (executed, steam_status) = dispatch_standalone_launch(
        module,
        &state.settings,
        &launch_plan.plan,
        LaunchApproval {
            incomplete_extra_files: args.allow_incomplete_preparation,
            live_workshop_fallback: args.allow_live_fallback,
        },
    )?;
    let command = executed.command;

    Ok(CommandSuccess {
        action: "game.launch".to_string(),
        message: format!("Launch command started (pid={})", executed.pid),
        data: json!({
            "game_id": module.id(),
            "game_space_id": spaces::active_game_space().space_id,
            "executable": command.program.display().to_string(),
            "args": command.args,
            "cwd": command.cwd.as_ref().map(|path| path.display().to_string()),
            "manifest": null,
            "packs": launch_plan.packs,
            "steam_status": steam_status,
            "extra_files": activation_json(&executed.extra_files),
            "execute": true,
            "pid": executed.pid,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_game_launch(
    cli: &CliArgs,
    args: GameLaunchArgs,
) -> Result<CommandSuccess, CommandError> {
    let module = crate::core::game::registry().active();
    let state = AppState::load()?;
    let space_dir = spaces::active_game_space_dir();
    let launch_plan = reforger::build_workshop_launch_plan(&space_dir, args.include_disabled)
        .map_err(|err| CommandError::operation("game.launch", err))?;

    if args.execute && !cli.dry_run && !launch_plan.issues.is_empty() {
        return Err(CommandError::validation(
            "game.launch",
            format!(
                "Cannot launch with {} unresolved Reforger addon(s)",
                launch_plan.issues.len()
            ),
        ));
    }

    let preview_command = build_launch_preview(module, &state.settings, &launch_plan.plan)?;

    if cli.dry_run || !args.execute {
        return Ok(CommandSuccess {
            action: "game.launch".to_string(),
            message: "Launch command generated".to_string(),
            data: json!({
                "game_id": module.id(),
                "game_space_id": spaces::active_game_space().space_id,
                "executable": preview_command.program.display().to_string(),
                "args": preview_command.args,
                "cwd": preview_command.cwd.as_ref().map(|path| path.display().to_string()),
                "addons": launch_plan.addons,
                "issues": launch_plan.issues,
                "execute": false,
                "dry_run": cli.dry_run,
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let (executed, steam_status) = dispatch_standalone_launch(
        module,
        &state.settings,
        &launch_plan.plan,
        LaunchApproval {
            incomplete_extra_files: args.allow_incomplete_preparation,
            live_workshop_fallback: args.allow_live_fallback,
        },
    )?;
    let command = executed.command;

    Ok(CommandSuccess {
        action: "game.launch".to_string(),
        message: format!("Launch command started (pid={})", executed.pid),
        data: json!({
            "game_id": module.id(),
            "game_space_id": spaces::active_game_space().space_id,
            "executable": command.program.display().to_string(),
            "args": command.args,
            "cwd": command.cwd.as_ref().map(|path| path.display().to_string()),
            "addons": launch_plan.addons,
            "steam_status": steam_status,
            "extra_files": activation_json(&executed.extra_files),
            "execute": true,
            "pid": executed.pid,
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn build_launch_preview(
    module: &dyn GameModule,
    settings: &crate::ui::types::SettingsViewState,
    plan: &LaunchPlan,
) -> Result<LaunchCommand, CommandError> {
    let install_dir = module.install_dir_from_settings(settings);
    let ctx = GameLaunchCtx {
        install_dir,
        steam_directory: &settings.steam_directory,
        settings: Some(settings),
    };
    module
        .build_launch_preview(plan, &ctx)
        .map_err(|error| launch_error_to_command_error(module.display_name(), error))
}

fn dispatch_standalone_launch(
    module: &dyn GameModule,
    settings: &crate::ui::types::SettingsViewState,
    plan: &LaunchPlan,
    approval: LaunchApproval,
) -> Result<(LaunchHandoff, &'static str), CommandError> {
    let steam_required =
        plan.game_build.is_none() && module.steam_app_id_from_settings(settings).is_some();
    let steam_directory = &settings.steam_directory;
    let mut steam_status = if steam_required {
        "ready"
    } else {
        "not_required"
    };
    let handoff = crate::core::game::launch_preparation::dispatch_plan(
        module,
        settings,
        plan,
        approval,
        |command| {
            if steam_required {
                steam_status = match ensure_steam_running(steam_directory)
                    .map_err(|error| error.to_string())?
                {
                    SteamEnsureResult::AlreadyRunning => "already_running",
                    SteamEnsureResult::Started => "started",
                    SteamEnsureResult::SkippedMissingDirectory => "not_configured_fallback",
                };
                if !crate::core::steam::is_steam_running(steam_directory) {
                    return Err("Steam is not running".to_string());
                }
            }
            let args = command
                .args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            crate::core::utils::deelevate::spawn_unelevated(
                command.program.as_os_str(),
                &args,
                command.cwd.as_deref(),
            )
            .map_err(|error| error.to_string())
        },
    )
    .map_err(|failure| launch_failure_to_command_error("game.launch", failure))?;
    Ok((handoff, steam_status))
}

fn launch_failure_to_command_error(action: &str, failure: LaunchFailure) -> CommandError {
    match failure {
        LaunchFailure::ExtraFiles(failures) => CommandError::validation(
            action,
            format!(
                "Extra-file preparation failed ({} issue(s)); pass --allow-incomplete-preparation to continue: {}",
                failures.len(),
                failures.join("; ")
            ),
        ),
        LaunchFailure::FrozenCopies(missing) => CommandError::validation(
            action,
            format!(
                "Frozen Workshop copies are missing for: {}; pass --allow-live-fallback to use live Workshop content",
                missing.join(", ")
            ),
        ),
        LaunchFailure::Failed(error) | LaunchFailure::Recovery(error) => {
            CommandError::operation(action, error)
        }
    }
}

fn activation_json(summary: &crate::core::game::extra_files::ActivationSummary) -> Value {
    json!({
        "activated": summary.activated.len(),
        "failed": summary.failed.len(),
        "skipped_disabled": summary.skipped_disabled
    })
}

fn run_reforger_command(
    cli: &CliArgs,
    command: ReforgerCommand,
) -> Result<CommandSuccess, CommandError> {
    ensure_active_reforger("game.reforger")?;
    match command {
        ReforgerCommand::List => cmd_reforger_list(),
        ReforgerCommand::Add(args) => cmd_reforger_add(cli, args),
        ReforgerCommand::Import(args) => cmd_reforger_import(cli, args),
        ReforgerCommand::Remove(args) => cmd_reforger_remove(cli, args),
        ReforgerCommand::Set(args) => cmd_reforger_set(cli, args),
        ReforgerCommand::Freeze(args) => cmd_reforger_freeze(cli, args),
        ReforgerCommand::Unfreeze(args) => cmd_reforger_unfreeze(cli, args),
        ReforgerCommand::Export(args) => cmd_reforger_export(args),
        ReforgerCommand::Resolve(args) => cmd_reforger_resolve(args),
    }
}

fn ensure_active_reforger(action: &str) -> Result<(), CommandError> {
    let module = crate::core::game::registry().active();
    if module.id() == reforger::REFORGER_GAME_ID {
        Ok(())
    } else {
        Err(CommandError::validation(
            action,
            format!(
                "Active game {} is not Arma Reforger; switch to a Reforger game space first",
                module.id()
            ),
        ))
    }
}

fn cmd_reforger_list() -> Result<CommandSuccess, CommandError> {
    let space_dir = spaces::active_game_space_dir();
    let store = reforger::load_store(&space_dir)
        .map_err(|err| CommandError::operation("game.reforger.list", err))?;
    let message = if store.entries.is_empty() {
        "No managed Arma Reforger addons".to_string()
    } else {
        store
            .entries
            .iter()
            .map(|entry| {
                let title = entry.name.as_deref().unwrap_or(&entry.guid);
                let frozen = if entry.frozen { " frozen" } else { "" };
                let disabled = if entry.enabled { "" } else { " disabled" };
                format!("{} - {}{}{}", entry.guid, title, frozen, disabled)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(CommandSuccess {
        action: "game.reforger.list".to_string(),
        message,
        data: json!({"entries": store.entries}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_add(cli: &CliArgs, args: ReforgerAddArgs) -> Result<CommandSuccess, CommandError> {
    let guid = single_reforger_guid("game.reforger.add", &args.guid)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "game.reforger.add".to_string(),
            message: format!("Dry-run: would add Arma Reforger addon {}", guid),
            data: json!({
                "guid": guid,
                "name": args.name,
                "source": args.source.as_ref().map(|path| path.display().to_string()),
                "enabled": !args.disabled,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let result = reforger::upsert_addon(
        &space_dir,
        &guid,
        args.name,
        args.source.as_deref(),
        !args.disabled,
    )
    .map_err(|err| CommandError::operation("game.reforger.add", err))?;
    Ok(CommandSuccess {
        action: "game.reforger.add".to_string(),
        message: format!("Added Arma Reforger addon {}", result.item.guid),
        data: json!(result),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_import(
    cli: &CliArgs,
    args: ReforgerImportArgs,
) -> Result<CommandSuccess, CommandError> {
    let mut input = args.input.join("\n");
    if let Some(path) = args.from_file.as_ref() {
        let from_file = fs::read_to_string(path).map_err(|err| {
            CommandError::operation(
                "game.reforger.import",
                format!("Failed to read {}: {}", path.display(), err),
            )
        })?;
        if !input.is_empty() {
            input.push('\n');
        }
        input.push_str(&from_file);
    }
    let guids = reforger::parse_reforger_guids(&input);
    if guids.is_empty() {
        return Err(CommandError::validation(
            "game.reforger.import",
            "Provide at least one Arma Reforger Workshop GUID or URL",
        ));
    }
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "game.reforger.import".to_string(),
            message: format!(
                "Dry-run: would import {} Arma Reforger addon(s)",
                guids.len()
            ),
            data: json!({
                "guids": guids,
                "source_root": args.source_root.as_ref().map(|path| path.display().to_string()),
                "enabled": !args.disabled,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut failed = Vec::new();
    for guid in guids {
        let source = args.source_root.as_ref().map(|root| root.join(&guid));
        match reforger::upsert_addon(&space_dir, &guid, None, source.as_deref(), !args.disabled) {
            Ok(result) if result.added => added.push(result.item),
            Ok(result) => updated.push(result.item),
            Err(error) => failed.push(json!({"guid": guid, "error": error})),
        }
    }
    let exit_code = if failed.is_empty() {
        exit_codes::SUCCESS
    } else {
        exit_codes::PARTIAL_SUCCESS
    };
    Ok(CommandSuccess {
        action: "game.reforger.import".to_string(),
        message: format!(
            "Imported {} Arma Reforger addon(s), {} failure(s)",
            added.len() + updated.len(),
            failed.len()
        ),
        data: json!({
            "added": added,
            "updated": updated,
            "failed": failed,
        }),
        exit_code,
    })
}

fn cmd_reforger_remove(
    cli: &CliArgs,
    args: ReforgerRemoveArgs,
) -> Result<CommandSuccess, CommandError> {
    let guid = single_reforger_guid("game.reforger.remove", &args.guid)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = reforger::load_store(&space_dir)
            .map_err(|err| CommandError::operation("game.reforger.remove", err))?;
        let entry = store.entry(&guid).ok_or_else(|| {
            CommandError::not_found(
                "game.reforger.remove",
                format!("Arma Reforger addon {} is not managed", guid),
            )
        })?;
        return Ok(CommandSuccess {
            action: "game.reforger.remove".to_string(),
            message: format!("Dry-run: would remove Arma Reforger addon {}", guid),
            data: json!({
                "entry": entry,
                "delete_data": args.delete_data,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }
    if !cli.yes {
        return Err(CommandError::validation(
            "game.reforger.remove",
            "Removing a Reforger addon modifies reforger_addons.json; pass --yes to confirm",
        ));
    }
    let summary = reforger::remove_addon(&space_dir, &guid, args.delete_data)
        .map_err(|err| CommandError::operation("game.reforger.remove", err))?;
    Ok(CommandSuccess {
        action: "game.reforger.remove".to_string(),
        message: format!("Removed Arma Reforger addon {}", guid),
        data: json!(summary),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_set(cli: &CliArgs, args: ReforgerSetArgs) -> Result<CommandSuccess, CommandError> {
    let guid = single_reforger_guid("game.reforger.set", &args.guid)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = reforger::load_store(&space_dir)
            .map_err(|err| CommandError::operation("game.reforger.set", err))?;
        let entry = store.entry(&guid).ok_or_else(|| {
            CommandError::not_found(
                "game.reforger.set",
                format!("Arma Reforger addon {} is not managed", guid),
            )
        })?;
        return Ok(CommandSuccess {
            action: "game.reforger.set".to_string(),
            message: format!("Dry-run: would update Arma Reforger addon {}", guid),
            data: json!({"entry": entry, "enabled": args.enabled, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let entry = reforger::set_addon_enabled(&space_dir, &guid, args.enabled)
        .map_err(|err| CommandError::operation("game.reforger.set", err))?;
    Ok(CommandSuccess {
        action: "game.reforger.set".to_string(),
        message: format!("Updated Arma Reforger addon {}", guid),
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_freeze(
    cli: &CliArgs,
    args: ReforgerFreezeArgs,
) -> Result<CommandSuccess, CommandError> {
    let guid = single_reforger_guid("game.reforger.freeze", &args.guid)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let resolution = reforger::resolve_launch_path(&space_dir, &guid)
            .map_err(|err| CommandError::operation("game.reforger.freeze", err))?;
        return Ok(CommandSuccess {
            action: "game.reforger.freeze".to_string(),
            message: format!("Dry-run: would freeze Arma Reforger addon {}", guid),
            data: json!({"resolution": resolution, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let summary = reforger::freeze_addon(&space_dir, &guid)
        .map_err(|err| CommandError::operation("game.reforger.freeze", err))?;
    Ok(CommandSuccess {
        action: "game.reforger.freeze".to_string(),
        message: format!("Frozen Arma Reforger addon {}", guid),
        data: json!(summary),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_unfreeze(
    cli: &CliArgs,
    args: ReforgerUnfreezeArgs,
) -> Result<CommandSuccess, CommandError> {
    let guid = single_reforger_guid("game.reforger.unfreeze", &args.guid)?;
    let space_dir = spaces::active_game_space_dir();
    if cli.dry_run {
        let store = reforger::load_store(&space_dir)
            .map_err(|err| CommandError::operation("game.reforger.unfreeze", err))?;
        let entry = store.entry(&guid).ok_or_else(|| {
            CommandError::not_found(
                "game.reforger.unfreeze",
                format!("Arma Reforger addon {} is not managed", guid),
            )
        })?;
        return Ok(CommandSuccess {
            action: "game.reforger.unfreeze".to_string(),
            message: format!("Dry-run: would unfreeze Arma Reforger addon {}", guid),
            data: json!({"entry": entry, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }
    let entry = reforger::unfreeze_addon(&space_dir, &guid)
        .map_err(|err| CommandError::operation("game.reforger.unfreeze", err))?;
    Ok(CommandSuccess {
        action: "game.reforger.unfreeze".to_string(),
        message: format!("Unfrozen Arma Reforger addon {}", guid),
        data: json!({"entry": entry}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_export(args: ReforgerExportArgs) -> Result<CommandSuccess, CommandError> {
    let space_dir = spaces::active_game_space_dir();
    let store = reforger::load_store(&space_dir)
        .map_err(|err| CommandError::operation("game.reforger.export", err))?;
    let entries = store
        .entries
        .into_iter()
        .filter(|entry| args.all || entry.enabled)
        .collect::<Vec<_>>();
    let text = entries
        .iter()
        .map(|entry| entry.guid.clone())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(CommandSuccess {
        action: "game.reforger.export".to_string(),
        message: text.clone(),
        data: json!({"text": text, "items": entries}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_reforger_resolve(args: ReforgerResolveArgs) -> Result<CommandSuccess, CommandError> {
    let guid = single_reforger_guid("game.reforger.resolve", &args.guid)?;
    let space_dir = spaces::active_game_space_dir();
    let resolution = reforger::resolve_launch_path(&space_dir, &guid)
        .map_err(|err| CommandError::operation("game.reforger.resolve", err))?;
    Ok(CommandSuccess {
        action: "game.reforger.resolve".to_string(),
        message: resolution.path.clone(),
        data: json!(resolution),
        exit_code: exit_codes::SUCCESS,
    })
}

fn single_reforger_guid(action: &str, input: &str) -> Result<String, CommandError> {
    let guids = reforger::parse_reforger_guids(input);
    match guids.as_slice() {
        [guid] => Ok(guid.clone()),
        [] => Err(CommandError::validation(
            action,
            "Provide one Arma Reforger Workshop GUID or URL",
        )),
        _ => Err(CommandError::validation(
            action,
            "Provide exactly one Arma Reforger Workshop GUID or URL",
        )),
    }
}

pub(super) fn launch_error_to_command_error_for(
    action: &str,
    game_name: &str,
    err: LaunchError,
) -> CommandError {
    let message = match err {
        LaunchError::InstallDirNotConfigured => {
            format!("{} directory is not set in settings", game_name)
        }
        LaunchError::InstallDirMissing => {
            format!("{} directory does not exist", game_name)
        }
        LaunchError::InstallDirInvalid => {
            format!(
                "{} executable was not found in the configured directory",
                game_name
            )
        }
        LaunchError::LauncherUnavailable => {
            "Could not find the launch command for this platform".to_string()
        }
        LaunchError::LaunchPreparationFailed => "Failed to prepare launch files".to_string(),
        LaunchError::RepositoryLaunchUnsupported => {
            format!("{} cannot launch from a repository", game_name)
        }
        LaunchError::GameNotConfigured => {
            format!("{} is missing required launch settings", game_name)
        }
    };
    CommandError::validation(action, message)
}

fn launch_error_to_command_error(game_name: &str, err: LaunchError) -> CommandError {
    launch_error_to_command_error_for("game.launch", game_name, err)
}
