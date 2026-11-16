use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerNativeInstallCommand};
use crate::core::server::{
    ContentChannel, NativeInstallRequest, NativeInstallStatus, ServerInstanceId, ServerStore,
    ServerStoreError,
};

pub(super) fn inspect(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: crate::cli::args::ServerInstallationCommand,
) -> Result<CommandSuccess, CommandError> {
    use crate::cli::args::ServerInstallationCommand;
    let (action, id, account, validate) = match command {
        ServerInstallationCommand::Check(args) => (
            "server.installation.check",
            args.id,
            (!args.anonymous).then_some(args.account),
            false,
        ),
        ServerInstallationCommand::Validate(args) => {
            ("server.installation.validate", args.id, None, true)
        }
        _ => unreachable!(),
    };
    let context = capture(context, action)?;
    let id = ServerInstanceId::try_from(id)
        .map_err(|message| CommandError::validation(action, message))?;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        if validate {
            let verification = store
                .validate_native_installation(&id, &control)
                .map_err(|error| store_error(action, error))?;
            let mut result = success(
                action,
                if verification.valid {
                    format!(
                        "Managed inventory verified: {} file(s), {} bytes.",
                        verification.files, verification.bytes
                    )
                } else {
                    verification
                        .error
                        .clone()
                        .unwrap_or_else(|| "Managed inventory verification failed".into())
                },
                json!({"verification":verification,"writes_performed":false,"database_opened":false,"downloads_started":false}),
            );
            if !verification.valid {
                result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
            }
            Ok(result)
        } else {
            let check = store
                .check_native_installation(&id, account.as_deref(), &control)
                .map_err(|error| store_error(action, error))?;
            Ok(success(
                action,
                format!(
                    "Installed channel {}; installed build {:?}; Steam build {:?}; update available: {}. No files downloaded or written.",
                    match check.channel {
                        ContentChannel::Stable => "stable",
                        ContentChannel::Experimental => "experimental",
                    },
                    check.installed_build_id,
                    check.review.selection.build_id,
                    check.update_available
                ),
                json!({"check":check,"writes_performed":false,"database_opened":false,"downloads_started":false}),
            ))
        }
    })
}

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerNativeInstallCommand,
    maintenance: Option<crate::core::server::NativeMaintenanceAction>,
) -> Result<CommandSuccess, CommandError> {
    use crate::core::server::NativeMaintenanceAction::{Repair, Update};
    let action = match (&command, maintenance) {
        (ServerNativeInstallCommand::Plan(_), None) => "server.installation.install.plan",
        (ServerNativeInstallCommand::Apply(_), None) => "server.installation.install.apply",
        (ServerNativeInstallCommand::Plan(_), Some(Update)) => "server.installation.update.plan",
        (ServerNativeInstallCommand::Apply(_), Some(Update)) => "server.installation.update.apply",
        (ServerNativeInstallCommand::Plan(_), Some(Repair)) => "server.installation.repair.plan",
        (ServerNativeInstallCommand::Apply(_), Some(Repair)) => "server.installation.repair.apply",
    };
    let (args, expected) = match command {
        ServerNativeInstallCommand::Plan(args) => (args, None),
        ServerNativeInstallCommand::Apply(args) => {
            (args.install, Some(args.expected_plan_fingerprint))
        }
    };
    if expected.is_some() && !cli.dry_run && !cli.yes {
        return Err(CommandError::validation(
            action,
            "Installing a dedicated server requires --yes",
        ));
    }
    let context = capture(context, action)?;
    let instance_id = ServerInstanceId::try_from(args.id)
        .map_err(|message| CommandError::validation(action, message))?;
    let request = NativeInstallRequest {
        instance_id,
        directory: args.directory,
        branch: args.branch,
        account: (!args.anonymous).then_some(args.account),
        channel: args.channel.map(|channel| match channel {
            crate::cli::args::ServerNativeInstallChannel::Stable => ContentChannel::Stable,
            crate::cli::args::ServerNativeInstallChannel::Experimental => {
                ContentChannel::Experimental
            }
        }),
    };
    let dry_run = cli.dry_run;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let plan = match maintenance {
            Some(mode) => store.plan_native_maintenance(request, mode, &control),
            None => store.plan_native_install(request, &control),
        }
        .map_err(|error| store_error(action, error))?;
        if expected
            .as_ref()
            .is_some_and(|expected| expected != &plan.fingerprint)
        {
            return Err(store_error(action, ServerStoreError::StaleRevision));
        }
        if expected.is_none() || dry_run {
            let selection = &plan.review.selection;
            let mut lines = vec![
                format!(
                    "Steam app {} channel {} branch {} build {}; entitled: {}",
                    plan.app_id,
                    match plan.channel {
                        ContentChannel::Stable => "stable",
                        ContentChannel::Experimental => "experimental",
                    },
                    plan.branch,
                    selection
                        .build_id
                        .map_or_else(|| "unknown".to_owned(), |build| build.to_string()),
                    plan.review.entitled
                ),
                format!(
                    "Destination: {}; required bytes: {}; available bytes: {}",
                    plan.directory.display(),
                    plan.required_bytes,
                    plan.available_bytes
                        .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string())
                ),
            ];
            lines.extend(selection.depots.iter().map(|depot| {
                format!(
                    "Depot {} manifest {} ({} bytes)",
                    depot.depot_id,
                    depot.manifest_gid,
                    depot
                        .size
                        .map_or_else(|| "unknown".to_owned(), |size| size.to_string())
                )
            }));
            lines.extend(
                plan.blockers
                    .iter()
                    .map(|blocker| format!("Blocked: {blocker}")),
            );
            lines.push(format!(
                "Fingerprint: {}. Steam was reviewed read-only; nothing was downloaded or written.",
                plan.fingerprint
            ));
            if let Some(review) = &plan.maintenance {
                lines.push(format!("Previous build: {:?}; update available: {}; peer instances retained in this game space: {}. Only this instance switches to the verified successor; previous files are retained. Stop all consumers before applying. No automatic restart.", review.previous_build_id, review.update_available, review.retained_peer_instances.len()));
            }
            return Ok(success(
                action,
                lines.join("\n"),
                json!({"plan":plan,"database_opened":false,"writes_performed":false,"downloads_started":false,"dry_run":dry_run}),
            ));
        }
        if !plan.blockers.is_empty() {
            return Err(CommandError::validation(action, plan.blockers.join(" ")));
        }
        let _claim = super::content::claim(&store, action)?;
        let outcome = store
            .apply_native_install(&plan, &control)
            .map_err(|error| store_error(action, error))?;
        let message = match outcome.status {
            NativeInstallStatus::Installed => format!(
                "Installed and verified {} file(s), {} byte(s) at {}; bound as a managed installation.",
                outcome.files,
                outcome.bytes,
                outcome.directory.display()
            ),
            NativeInstallStatus::PublishedUnbound => format!(
                "Published the verified installation at {} but did not bind it: {}",
                outcome.directory.display(),
                outcome.error.as_deref().unwrap_or("binding failed")
            ),
            NativeInstallStatus::Cancelled => {
                "Installation cancelled; the destination was not created.".to_owned()
            }
            NativeInstallStatus::Failed => format!(
                "Installation failed; the destination was not created. {}",
                outcome
                    .content_error
                    .map(|error| error.to_string())
                    .or_else(|| outcome.auth_error.map(|error| error.to_string()))
                    .or_else(|| outcome.error.clone())
                    .unwrap_or_default()
            ),
        };
        let installed = outcome.status == NativeInstallStatus::Installed;
        let mut result = success(
            action,
            message,
            json!({"outcome":outcome,"database_opened":false,"writes_performed":outcome.writes_performed,"downloads_started":outcome.downloads_started,"dry_run":false}),
        );
        if !installed {
            result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
        }
        Ok(result)
    })
}
