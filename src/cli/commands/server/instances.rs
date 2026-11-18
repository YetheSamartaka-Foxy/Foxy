use serde_json::json;

use super::super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerInstanceCommand};
use crate::cli::exit_codes;
use crate::core::server::{
    DraftChange, InstallationId, ServerContext, ServerInstance, ServerInstanceId, ServerStore,
    ServerStoreError,
};

pub(super) fn capture(
    args: ServerContextArgs,
    action: &str,
) -> Result<ServerContext, CommandError> {
    let root = crate::core::utils::app_paths::configured_data_dir();
    ServerContext::capture(&root, args.game_space.as_deref())
        .map_err(|message| CommandError::validation(action, message))
}

pub(super) fn capabilities(args: ServerContextArgs) -> Result<CommandSuccess, CommandError> {
    let action = "server.capabilities";
    let context = capture(args, action)?;
    let module = crate::core::game::registry()
        .get(context.game_id())
        .ok_or_else(|| CommandError::validation(action, "Game module is unavailable"))?;
    let report = crate::core::server::support_report(&context);
    let capabilities = report.capabilities;
    let available = capabilities.draft_definitions;
    Ok(CommandSuccess {
        action: action.to_string(),
        message: if let Some(error) = &report.storage_error {
            format!("Server draft management is blocked: {error}")
        } else if available {
            "Server drafts, installation adoption, configuration snapshots, verification, rollback and read-only deployment preflight are implemented. Writes require exclusive game-space ownership. Game launch, runtime deployment and crash recovery remain unavailable.".to_string()
        } else {
            "Server draft management is unavailable for this game module.".to_string()
        },
        data: json!({
            "game_space_id": context.space_id(),
            "game_id": context.game_id(),
            "dedicated_server_support": module.dedicated_server_support(),
            "capabilities": capabilities,
            "availability": report.availability,
            "runtime_prerequisites": report.runtime_prerequisites,
            "storage_error": report.storage_error,
            "owner_availability": "not_probed",
            "database_opened": false
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

pub(super) fn run(
    cli: &CliArgs,
    args: ServerContextArgs,
    command: ServerInstanceCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerInstanceCommand::Render(_) => "server.instance.render",
        ServerInstanceCommand::List => "server.instance.list",
        ServerInstanceCommand::Adopt(_) => "server.instance.adopt",
        ServerInstanceCommand::Bind(_) => "server.instance.bind",
        ServerInstanceCommand::Preflight(_) => "server.instance.preflight",
        ServerInstanceCommand::Show(_) => "server.instance.show",
        ServerInstanceCommand::Add(_) => "server.instance.add",
        ServerInstanceCommand::Clone(_) => "server.instance.clone",
        ServerInstanceCommand::Rename(_) => "server.instance.rename",
        ServerInstanceCommand::Replace(_) => "server.instance.replace",
        ServerInstanceCommand::Validate(_) => "server.instance.validate",
        ServerInstanceCommand::Remove(_) => "server.instance.remove",
        ServerInstanceCommand::LaunchPlan(_) => "server.instance.launch-plan",
        ServerInstanceCommand::StartPlan(_)
        | ServerInstanceCommand::RecoveryPlan(_)
        | ServerInstanceCommand::Recover(_)
        | ServerInstanceCommand::Start(_)
        | ServerInstanceCommand::Status(_)
        | ServerInstanceCommand::Monitor(_)
        | ServerInstanceCommand::Logs(_)
        | ServerInstanceCommand::Stop(_)
        | ServerInstanceCommand::Restart(_) => {
            return Err(CommandError::validation(
                "server.instance",
                "Lifecycle command requires the foreground owner route",
            ));
        }
    };
    let context = capture(args, action)?;
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    let change = match command {
        ServerInstanceCommand::Render(args) => {
            let preview = store
                .configuration_preview(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            let mut lines = vec![format!(
                "Configuration preview {}; draft revision {}; no files written",
                preview.fingerprint, preview.instance_revision
            )];
            for file in &preview.files {
                lines.push(format!(
                    "{} [{}]\n{}",
                    file.relative_path, file.blake3, file.contents
                ));
            }
            lines.extend(preview.warnings.iter().cloned());
            return Ok(success(
                action,
                lines.join("\n"),
                json!({"configuration_preview":preview,"database_opened":false,"writes_performed":false}),
            ));
        }
        ServerInstanceCommand::LaunchPlan(args) => {
            return launch_command(cli, &store, &context, args, action, false);
        }
        ServerInstanceCommand::StartPlan(_)
        | ServerInstanceCommand::RecoveryPlan(_)
        | ServerInstanceCommand::Recover(_)
        | ServerInstanceCommand::Start(_)
        | ServerInstanceCommand::Status(_)
        | ServerInstanceCommand::Monitor(_)
        | ServerInstanceCommand::Logs(_)
        | ServerInstanceCommand::Stop(_)
        | ServerInstanceCommand::Restart(_) => unreachable!("lifecycle command dispatched above"),
        ServerInstanceCommand::Preflight(args) => {
            let preflight = store
                .preflight(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            let mut lines = vec![
                "Deployment preflight completed; deployment is not ready. No files were written."
                    .into(),
            ];
            lines.extend(
                preflight
                    .paths
                    .iter()
                    .map(|path| format!("{}: {}", path.role, path.path.display())),
            );
            lines.extend(
                preflight
                    .blockers
                    .iter()
                    .map(|blocker| format!("{}: {}", blocker.code, blocker.message)),
            );
            return Ok(success(
                action,
                lines.join("\n"),
                json!({"preflight": preflight, "database_opened": false}),
            ));
        }
        ServerInstanceCommand::Adopt(args) => DraftChange::Adopt {
            id: parse_id(args.id, action)?,
            expected_revision: args.expected_revision,
            installation: store
                .inspect_installation(&args.directory)
                .map_err(|error| store_error(action, error))?,
        },
        ServerInstanceCommand::Bind(args) => DraftChange::Bind {
            id: parse_id(args.id, action)?,
            expected_revision: args.expected_revision,
            installation_id: args
                .installation
                .map(InstallationId::try_from)
                .transpose()
                .map_err(|message| CommandError::validation(action, message))?,
        },
        ServerInstanceCommand::List => {
            let (catalog, migration_pending) = store
                .load_with_status()
                .map_err(|error| store_error(action, error))?;
            let message = if catalog.instances.is_empty() {
                "No server drafts in this game space".to_string()
            } else {
                catalog
                    .instances
                    .iter()
                    .map(|instance| {
                        format!(
                            "{}  revision={}  {}",
                            instance.id.as_str(),
                            instance.revision,
                            instance.display_name
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            return Ok(success(
                action,
                message,
                json!({"catalog": catalog, "migration_pending": migration_pending, "database_opened": false}),
            ));
        }
        ServerInstanceCommand::Show(args) => {
            let instance = store
                .show(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            return Ok(success(
                action,
                serde_json::to_string_pretty(&instance).unwrap_or_default(),
                json!({"instance": instance, "database_opened": false}),
            ));
        }
        ServerInstanceCommand::Validate(args) => {
            let instance = read_instance(&store, &args.file, action)?;
            return Ok(success(
                action,
                "Valid server draft; deployment and launch readiness are not evaluated".to_string(),
                json!({"instance": instance, "valid_draft": true, "deployment_readiness": "not_evaluated", "database_opened": false}),
            ));
        }
        ServerInstanceCommand::Add(args) => DraftChange::Add { name: args.name },
        ServerInstanceCommand::Clone(args) => DraftChange::Clone {
            id: parse_id(args.id, action)?,
            name: args.name,
            expected_revision: args.expected_revision,
        },
        ServerInstanceCommand::Rename(args) => DraftChange::Rename {
            id: parse_id(args.id, action)?,
            name: args.name,
            expected_revision: args.expected_revision,
        },
        ServerInstanceCommand::Replace(args) => DraftChange::Replace {
            instance: read_instance(&store, &args.file, action)?,
            expected_revision: args.expected_revision,
        },
        ServerInstanceCommand::Remove(args) => {
            if !cli.yes && !cli.dry_run {
                return Err(CommandError::validation(
                    action,
                    "Removing a server draft requires --yes",
                ));
            }
            DraftChange::Remove {
                id: parse_id(args.id, action)?,
                expected_revision: args.expected_revision,
            }
        }
    };
    save_change(cli, &store, &context, change, action)
}

pub(super) fn save_change(
    cli: &CliArgs,
    store: &ServerStore,
    context: &ServerContext,
    change: DraftChange,
    action: &str,
) -> Result<CommandSuccess, CommandError> {
    let plan = store
        .plan(change)
        .map_err(|error| store_error(action, error))?;
    let mut summary = format!(
        "{}: {} ({})\nGame space: {}; draft revision: {}; catalog revision: {}\nCatalog action: {} server/servers.json",
        action,
        plan.instance.display_name,
        plan.instance.id.as_str(),
        context.space_id(),
        plan.instance.revision,
        plan.catalog_revision(),
        if plan.removes_instance {
            "remove draft from"
        } else {
            "save draft in"
        },
    );
    let mut writes = vec![json!({"kind":"replace_catalog","relative_path":"server/servers.json"})];
    if let Some(path) = plan.migration_backup_relative_path() {
        summary.push_str(&format!("\nPreserve exact v1 backup: {}", path.display()));
        writes.push(json!({"kind":"preserve_v1_backup","relative_path":path}));
    }
    let data = json!({
        "game_space_id": context.space_id(),
        "instance": plan.instance,
        "catalog_revision": plan.catalog_revision(),
        "dry_run": cli.dry_run,
        "writes": writes,
        "removes_draft": plan.removes_instance,
        "migrates_from_v1": plan.migrates_from_v1,
        "runtime_data_changed": false,
        "database_opened": false,
        "requires_exclusive_owner": true
    });
    let message = if cli.dry_run {
        format!(
            "{summary}\nPreview only; saving requires exclusive game-space ownership. No files were written."
        )
    } else {
        let _claim = super::content::claim(store, action)?;
        store
            .apply(plan)
            .map_err(|error| store_error(action, error))?;
        format!("{summary}\nServer draft change saved.")
    };
    Ok(success(action, message, data))
}

fn launch_command(
    cli: &CliArgs,
    store: &ServerStore,
    context: &ServerContext,
    args: crate::cli::args::ServerInstanceSelector,
    action: &str,
    start: bool,
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    let instance = store
        .show(&id)
        .map_err(|error| store_error(action, error))?;
    let installation_id = instance.installation_id.clone().ok_or_else(|| {
        CommandError::validation(action, "Bind an inspected installation before launch")
    })?;
    let catalog = store.load().map_err(|error| store_error(action, error))?;
    let installation = catalog
        .installations
        .iter()
        .find(|item| item.id == installation_id)
        .ok_or_else(|| {
            CommandError::validation(action, "The bound installation is not in this game space")
        })?;
    let host_matches = match installation.platform {
        crate::core::server::ServerPlatform::WindowsX64 => cfg!(windows),
        crate::core::server::ServerPlatform::LinuxX64 => cfg!(target_os = "linux"),
    };
    if !host_matches {
        return Err(CommandError::validation(
            action,
            "This build cannot start that dedicated-server platform",
        ));
    }
    let build = store
        .inspect_installation_build(installation)
        .map_err(|error| store_error(action, error))?;
    if build.tested_engine_layout.is_some() {
        match store
            .native_runtime_launch_plan(&id, &crate::core::server::ContentStageControl::default())
        {
            Ok(native) => {
                if start && !cli.dry_run {
                    return Err(CommandError::validation(
                        action,
                        "Dedicated Start remains unavailable until foreground lifecycle acceptance. The committed native runtime can be inspected with launch-plan.",
                    ));
                }
                return Ok(success(
                    action,
                    "Committed native runtime launch plan. No process was started.".into(),
                    json!({"database_opened":false,"spawned":false,"executable":native.executable,"arguments":native.arguments,"engine_layout":"native_isolated_copy","runtime_materialized":true,"native_runtime":native}),
                ));
            }
            Err(crate::core::server::ServerStoreError::NotFound) => {}
            Err(error) => return Err(store_error(action, error)),
        }
    }
    let port = match &instance.definition {
        crate::core::server::ServerDefinition::Arma3(settings) => settings.game_port,
        crate::core::server::ServerDefinition::Reforger(settings) => settings.bind_port,
    };
    let runtime = context
        .space_dir()
        .join("server")
        .join("instances")
        .join(id.as_str())
        .join("runtime");
    let default_names = crate::core::server::Arma3ConfigurationNames::default();
    let names = match &instance.definition {
        crate::core::server::ServerDefinition::Arma3(settings) => &settings.configuration,
        crate::core::server::ServerDefinition::Reforger(_) => &default_names,
    };
    let plan = crate::core::server::dedicated_plan(
        installation.game,
        &installation.directory,
        &installation.executable,
        &runtime,
        port,
        names,
    )
    .map_err(|_| {
        CommandError::validation(action, "The installation executable name is not usable")
    })?;
    let arguments = plan
        .arguments
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if !start || cli.dry_run {
        return Ok(success(
            action,
            format!(
                "Launch plan for {}; no process was started. Instance runtime materialization and Start remain unavailable.",
                instance.display_name
            ),
            json!({
                "database_opened": false,
                "spawned": false,
                "executable": plan.executable,
                "arguments": arguments,
                "engine_layout": if build.tested_engine_layout.is_some() { "native_tested_compatibility_only" } else { "unsupported" },
                "runtime_materialized": false,
                "tested_engine_layout": build.tested_engine_layout,
            }),
        ));
    }
    Err(CommandError::validation(
        action,
        "Dedicated Start is unavailable until native engine layout, deployment admission and owner lifecycle are accepted. Use launch-plan to inspect intent; server supervise hosts metadata ownership only.",
    ))
}

pub(super) fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}

fn read_instance(
    store: &ServerStore,
    path: &std::path::Path,
    action: &str,
) -> Result<ServerInstance, CommandError> {
    let bytes = crate::core::server::read_bounded(path).map_err(|_| {
        CommandError::validation(
            action,
            "Cannot read a typed instance JSON document (maximum 4 MiB)",
        )
    })?;
    store
        .parse_instance(&bytes)
        .map_err(|error| CommandError::validation(action, error.to_string()))
}

pub(super) fn success(action: &str, message: String, data: serde_json::Value) -> CommandSuccess {
    CommandSuccess {
        action: action.to_string(),
        message,
        data,
        exit_code: exit_codes::SUCCESS,
    }
}

pub(super) fn store_error(action: &str, error: ServerStoreError) -> CommandError {
    let message = error.to_string();
    match error {
        ServerStoreError::NotFound | ServerStoreError::InstallationNotFound => {
            CommandError::not_found(action, message)
        }
        ServerStoreError::Invalid(_)
        | ServerStoreError::UnsupportedVersion
        | ServerStoreError::UnsupportedGame
        | ServerStoreError::StaleRevision => CommandError::validation(action, message),
        _ => CommandError::operation(action, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_errors_keep_existing_exit_codes_and_do_not_echo_payloads() {
        for (error, expected) in [
            (ServerStoreError::NotFound, exit_codes::NOT_FOUND),
            (
                ServerStoreError::StaleRevision,
                exit_codes::VALIDATION_ERROR,
            ),
            (
                ServerStoreError::UnsupportedVersion,
                exit_codes::VALIDATION_ERROR,
            ),
            (
                ServerStoreError::UnsupportedGame,
                exit_codes::VALIDATION_ERROR,
            ),
            (ServerStoreError::Corrupt, exit_codes::OPERATION_FAILED),
            (ServerStoreError::Ownership, exit_codes::OPERATION_FAILED),
            (ServerStoreError::UnsafePath, exit_codes::OPERATION_FAILED),
        ] {
            assert_eq!(store_error("server.instance.replace", error).code, expected);
        }
        assert!(parse_id("../other".into(), "server.instance.show").is_err());
    }
}
