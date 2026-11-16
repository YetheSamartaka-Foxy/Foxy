use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContentCommand, ServerContentFolderArgs, ServerContentPlanArgs,
    ServerContentProfileArgs, ServerContextArgs,
};
use crate::core::server::{
    AddonSupply, ContentPlanRequest, ContentSources, RepositoryProfile, ServerAddonBinding,
    ServerInstanceId, ServerStore,
};

pub(super) fn run(
    cli: &CliArgs,
    context_args: ServerContextArgs,
    command: ServerContentCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.content";
    let context = capture(context_args, action)?;
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    let command = match command {
        ServerContentCommand::Acquire { command } => {
            return super::content_acquisition::run(cli, context, command);
        }
        ServerContentCommand::Plan(args) => return plan(&store, args),
        ServerContentCommand::Consumers => {
            let report = store
                .content_consumers()
                .map_err(|error| store_error("server.content.consumers", error))?;
            return Ok(success(
                "server.content.consumers",
                format!(
                    "{} source profile(s) inspected; {} diagnostic(s). Nothing was written, downloaded or retired.",
                    report.sources.len(),
                    report.diagnostics.len()
                ),
                json!({"report": report, "database_opened": false, "writes_performed": false}),
            ));
        }
        ServerContentCommand::Bindings { command } => {
            return super::content_bindings::run(cli, &store, command);
        }
        ServerContentCommand::Stage { command } => {
            return super::content_stage::run(cli, &store, context, command);
        }
        command => command,
    };
    let snapshot = store
        .content_sources()
        .map_err(|error| store_error(action, error))?;
    let mut sources = snapshot.sources;
    match command {
        ServerContentCommand::List => Ok(success(
            action,
            format!(
                "{} addon folder(s) and {} repository profile(s). No download was started.",
                sources.folders.len(),
                sources.profiles.len()
            ),
            json!({
                "database_opened": false,
                "folders": sources.folders,
                "profiles": sources.profiles,
                "fingerprint": snapshot.fingerprint,
            }),
        )),
        ServerContentCommand::AddFolder(args) => add_folder(
            cli,
            &store,
            snapshot.fingerprint.as_deref(),
            &mut sources,
            args,
        ),
        ServerContentCommand::AddProfile(args) => add_profile(
            cli,
            &store,
            snapshot.fingerprint.as_deref(),
            &mut sources,
            args,
        ),
        ServerContentCommand::Plan(_)
        | ServerContentCommand::Acquire { .. }
        | ServerContentCommand::Bindings { .. }
        | ServerContentCommand::Stage { .. }
        | ServerContentCommand::Consumers => unreachable!(),
    }
}

fn add_folder(
    cli: &CliArgs,
    store: &ServerStore,
    fingerprint: Option<&str>,
    sources: &mut ContentSources,
    args: ServerContentFolderArgs,
) -> Result<CommandSuccess, CommandError> {
    sources
        .add_folder(&args.id, &args.path)
        .map_err(|error| CommandError::validation("server.content", error.to_string()))?;
    if !cli.dry_run {
        persist_sources(store, sources, fingerprint)?;
    }
    Ok(success(
        "server.content",
        format!("Addon folder {} recorded. Nothing was downloaded.", args.id),
        json!({
            "database_opened": false,
            "dry_run": cli.dry_run,
            "folder_id": args.id,
            "path": args.path,
        }),
    ))
}

fn add_profile(
    cli: &CliArgs,
    store: &ServerStore,
    fingerprint: Option<&str>,
    sources: &mut ContentSources,
    args: ServerContentProfileArgs,
) -> Result<CommandSuccess, CommandError> {
    let supply = match args.supply.as_str() {
        "download" => AddonSupply::Download,
        "already-present" => AddonSupply::AlreadyPresent,
        _ => {
            return Err(CommandError::validation(
                "server.content",
                "Supply must be download or already-present",
            ));
        }
    };
    sources
        .add_profile(RepositoryProfile {
            name: args.name.clone(),
            remote_url: args.url,
            local_path: args.local_path,
            folder_id: args.folder,
            supply,
            addons: args.addon,
        })
        .map_err(|error| CommandError::validation("server.content", error.to_string()))?;
    let resolved = sources
        .resolve(&args.name)
        .map_err(|error| CommandError::validation("server.content", error.to_string()))?;
    if !cli.dry_run {
        persist_sources(store, sources, fingerprint)?;
    }
    Ok(success(
        "server.content",
        if resolved.download {
            format!(
                "Profile {} will be downloaded into its addon folder later. No download was started.",
                args.name
            )
        } else {
            format!(
                "Profile {} uses files already on this machine. They will not be downloaded again.",
                args.name
            )
        },
        json!({
            "database_opened": false,
            "dry_run": cli.dry_run,
            "download": resolved.download,
            "root": resolved.root,
            "addon_paths": resolved.addon_paths,
        }),
    ))
}

fn persist_sources(
    store: &ServerStore,
    sources: &ContentSources,
    fingerprint: Option<&str>,
) -> Result<(), CommandError> {
    let action = "server.content";
    let _claim = claim(store, action)?;
    store
        .save_content_sources(sources, fingerprint)
        .map_err(|error| store_error(action, error))
}

pub(super) fn claim(
    store: &ServerStore,
    action: &str,
) -> Result<Option<crate::core::server::owner::MetadataPermit>, CommandError> {
    use crate::core::tasks::db_process_lock::LockOutcome;
    let permit =
        crate::core::server::owner::permit(store.context()).map_err(|error| CommandError {
            action: action.into(),
            message: format!("Server metadata admission failed: {error}"),
            code: if error.kind() == std::io::ErrorKind::WouldBlock {
                crate::cli::exit_codes::DATABASE_BUSY
            } else {
                crate::cli::exit_codes::OPERATION_FAILED
            },
        })?;
    let path = store
        .validate_claim_path()
        .map_err(|error| store_error(action, error))?;
    match crate::core::server::owner::claim_write(&path) {
        LockOutcome::Acquired => {}
        LockOutcome::Busy { .. } => {
            return Err(CommandError {
                action: action.into(),
                message:
                    "Another Foxy owns this game space; close it before saving content settings"
                        .into(),
                code: crate::cli::exit_codes::DATABASE_BUSY,
            });
        }
        LockOutcome::Unavailable(_) => {
            return Err(CommandError::operation(
                action,
                "Cannot verify exclusive ownership of the selected game space",
            ));
        }
    }
    Ok(permit)
}

fn plan(store: &ServerStore, args: ServerContentPlanArgs) -> Result<CommandSuccess, CommandError> {
    let action = "server.content.plan";
    let request = request(store, args, action)?;
    let plan = store
        .plan_content(request)
        .map_err(|error| store_error(action, error))?;
    let mut lines = vec![format!(
        "Content preview {} for {} (draft revision {}). No download or copy was started; deployment remains unavailable.",
        plan.fingerprint,
        plan.instance_id.as_str(),
        plan.instance_revision
    )];
    for binding in &plan.bindings {
        lines.push(format!(
            "{}: role={:?}, order={}",
            binding.profile_name, binding.role, binding.order
        ));
        lines.extend(binding.addon_paths.iter().map(|path| {
            format!(
                "  {} -> {} [{:?}]",
                path.source.display(),
                path.destination.display(),
                path.state
            )
        }));
    }
    lines.extend(
        plan.blockers
            .iter()
            .map(|blocker| format!("{}: {}", blocker.code, blocker.message)),
    );
    Ok(success(
        action,
        lines.join("\n"),
        json!({"plan":plan,"database_opened":false}),
    ))
}

pub(super) fn request(
    store: &ServerStore,
    args: ServerContentPlanArgs,
    action: &str,
) -> Result<ContentPlanRequest, CommandError> {
    let id = ServerInstanceId::try_from(args.id)
        .map_err(|message| CommandError::validation(action, message))?;
    let bytes = crate::core::server::read_bounded(&args.bindings).map_err(|_| {
        CommandError::validation(
            action,
            "Cannot read a content binding JSON array (maximum 4 MiB)",
        )
    })?;
    let bindings: Vec<ServerAddonBinding> = serde_json::from_slice(&bytes).map_err(|_| {
        CommandError::validation(
            action,
            "Expected a JSON array of source_id, role and order bindings",
        )
    })?;
    let catalog = store.load().map_err(|error| store_error(action, error))?;
    let instance = catalog
        .instances
        .iter()
        .find(|instance| instance.id == id)
        .ok_or_else(|| {
            CommandError::not_found(
                action,
                "Server instance was not found in the selected game space",
            )
        })?;
    let sources = store
        .content_sources()
        .map_err(|error| store_error(action, error))?;
    Ok(ContentPlanRequest {
        instance_id: id,
        expected_instance_revision: args.expected_revision.unwrap_or(instance.revision),
        expected_catalog_revision: catalog.revision,
        content_sources_fingerprint: args.expected_sources_fingerprint.or(sources.fingerprint),
        bindings,
    })
}
