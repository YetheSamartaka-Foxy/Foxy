use super::instances::{capture, store_error, success};
use crate::cli::args::{CliArgs, ServerConfigurationCommand, ServerContextArgs};
use crate::cli::commands::{CommandError, CommandSuccess};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};
use serde_json::json;

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerConfigurationCommand,
) -> Result<CommandSuccess, CommandError> {
    let command = match command {
        ServerConfigurationCommand::Import(args) => {
            return super::configuration_transfer::import(cli, context, args);
        }
        ServerConfigurationCommand::ExportPack(args) => {
            return super::configuration_transfer::export_pack(cli, context, args);
        }
        ServerConfigurationCommand::ImportPack(args) => {
            return super::configuration_transfer::import_pack(cli, context, args);
        }
        ServerConfigurationCommand::ImportReforger(args) => {
            return super::reforger_configuration::import(cli, context, args);
        }
        ServerConfigurationCommand::ExportReforger(args) => {
            return super::reforger_configuration::export(cli, context, args);
        }
        ServerConfigurationCommand::Scenario(args) => {
            return super::reforger_configuration::scenario(cli, context, args);
        }
        other => other,
    };
    let action = match &command {
        ServerConfigurationCommand::Plan(_) => "server.configuration.plan",
        ServerConfigurationCommand::List(_) => "server.configuration.list",
        ServerConfigurationCommand::Commit(_) => "server.configuration.commit",
        ServerConfigurationCommand::Verify(_) => "server.configuration.verify",
        ServerConfigurationCommand::Rollback(_) => "server.configuration.rollback",
        _ => unreachable!("transfer commands handled above"),
    };
    let context = capture(context, action)?;
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    let id = match &command {
        ServerConfigurationCommand::Plan(args) | ServerConfigurationCommand::List(args) => &args.id,
        ServerConfigurationCommand::Commit(args) => &args.id,
        ServerConfigurationCommand::Verify(args) => &args.id,
        ServerConfigurationCommand::Rollback(args) => &args.id,
        _ => unreachable!("transfer commands handled above"),
    };
    let id = ServerInstanceId::try_from(id.clone())
        .map_err(|message| CommandError::validation(action, message))?;
    let claim = || super::content::claim(&store, action);
    match command {
        ServerConfigurationCommand::Import(_)
        | ServerConfigurationCommand::ExportPack(_)
        | ServerConfigurationCommand::ImportPack(_)
        | ServerConfigurationCommand::ImportReforger(_)
        | ServerConfigurationCommand::ExportReforger(_)
        | ServerConfigurationCommand::Scenario(_) => {
            unreachable!("transfer commands handled above")
        }
        ServerConfigurationCommand::List(_) => {
            let state = store
                .configuration_revisions(&id)
                .map_err(|error| store_error(action, error))?;
            Ok(success(
                action,
                format!(
                    "Configuration generation {}; active: {}\n{}",
                    state.generation,
                    state.active.as_deref().unwrap_or("none"),
                    state.revisions.join("\n")
                ),
                json!({"state":state,"database_opened":false}),
            ))
        }
        ServerConfigurationCommand::Verify(args) => {
            let report = store
                .verify_configuration(&id, &args.revision)
                .map_err(|error| store_error(action, error))?;
            let mut result = success(
                action,
                if report.valid {
                    "Configuration snapshot is intact; launch readiness remains unverified.".into()
                } else {
                    format!(
                        "Configuration snapshot verification failed: {}",
                        report.issues.join("; ")
                    )
                },
                json!({"verification":report,"database_opened":false,"writes_performed":false}),
            );
            if !report.valid {
                result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
            }
            Ok(result)
        }
        ServerConfigurationCommand::Plan(_) => {
            let plan = store
                .plan_configuration_commit(&id)
                .map_err(|error| store_error(action, error))?;
            let message = format!(
                "Configuration snapshot {}\nExpected draft revision: {}; generation: {}; fingerprint: {}\nFiles: {}\nApply writes only the instance configuration archive and state.json. No game process, installation or save is changed.",
                plan.revision_id,
                plan.preview.instance_revision,
                plan.expected_generation,
                plan.preview.fingerprint,
                plan.preview
                    .files
                    .iter()
                    .map(|file| file.relative_path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            Ok(success(
                action,
                format!(
                    "{message}\nConfiguration archive: {}",
                    plan.destination_directory().display()
                ),
                json!({"directory":plan.destination_directory(),"preview":plan.preview,"revision_id":plan.revision_id,"expected_generation":plan.expected_generation,"database_opened":false,"writes_performed":false}),
            ))
        }
        ServerConfigurationCommand::Commit(args) => {
            let plan = store
                .plan_configuration_commit(&id)
                .map_err(|error| store_error(action, error))?;
            if plan.expected_generation != args.expected_generation
                || plan.preview.instance_revision != args.expected_revision
                || plan.preview.fingerprint != args.expected_fingerprint
            {
                return Err(store_error(action, ServerStoreError::StaleRevision));
            }
            let data = if cli.dry_run {
                json!({"dry_run":true,"preview":plan.preview,"expected_generation":plan.expected_generation,"revision_id":plan.revision_id,"database_opened":false,"requires_exclusive_owner":true})
            } else {
                let _claim = claim()?;
                let state = store
                    .apply_configuration_commit(plan)
                    .map_err(|error| store_error(action, error))?;
                json!({"dry_run":false,"state":state,"database_opened":false})
            };
            Ok(success(
                action,
                if cli.dry_run {
                    "Configuration commit preview; no files were written.".into()
                } else {
                    "Configuration snapshot committed; no server was started.".into()
                },
                data,
            ))
        }
        ServerConfigurationCommand::Rollback(args) => {
            if !cli.yes && !cli.dry_run {
                return Err(CommandError::validation(
                    action,
                    "Configuration rollback requires --yes",
                ));
            }
            let plan = store
                .plan_configuration_rollback(&id, &args.revision)
                .map_err(|error| store_error(action, error))?;
            if plan.expected_generation != args.expected_generation {
                return Err(store_error(action, ServerStoreError::StaleRevision));
            }
            let data = if cli.dry_run {
                json!({"dry_run":true,"revision_id":plan.revision_id,"expected_generation":plan.expected_generation,"database_opened":false})
            } else {
                let _claim = claim()?;
                let state = store
                    .apply_configuration_rollback(plan)
                    .map_err(|error| store_error(action, error))?;
                json!({"dry_run":false,"state":state,"database_opened":false})
            };
            Ok(success(
                action,
                if cli.dry_run {
                    "Configuration rollback preview; no files were written.".into()
                } else {
                    "Configuration revision selected; the draft, game installation and runtime were not changed.".into()
                },
                data,
            ))
        }
    }
}
