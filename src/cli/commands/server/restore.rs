use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerRestoreCommand};
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError, owner,
};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerRestoreCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerRestoreCommand::Plan(_) => "server.restore.plan",
        ServerRestoreCommand::Apply(_) => "server.restore.apply",
        ServerRestoreCommand::Status(_) => "server.restore.status",
        ServerRestoreCommand::Rollback(_) => "server.restore.rollback",
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control: ContentStageControl| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let fail = |error: ServerStoreError| store_error(action, error);
        match command {
            ServerRestoreCommand::Plan(args) => {
                let id = parse_id(args.id, action)?;
                let plan = store
                    .plan_server_restore(&id, &args.backup_id, None, &control)
                    .map_err(fail)?;
                let mut lines = vec![format!(
                    "Restore plan {} for {} from {}: {} file(s), {} byte(s); rollback requires {} byte(s), free space required {} byte(s) ({} available). Blockers: {}. No files were written.",
                    plan.fingerprint,
                    plan.instance_id.as_str(),
                    plan.backup_id,
                    plan.restore_files,
                    plan.restore_bytes,
                    plan.rollback_bytes,
                    plan.required_free_bytes,
                    plan.available_free_bytes,
                    plan.blockers.len()
                )];
                lines.extend(
                    plan.warnings
                        .iter()
                        .map(|warning| format!("Warning: {warning}")),
                );
                lines.extend(
                    plan.blockers
                        .iter()
                        .map(|blocker| format!("Blocker: {blocker}")),
                );
                Ok(success(
                    action,
                    lines.join("\n"),
                    json!({"plan":plan,"database_opened":false,"writes_performed":false}),
                ))
            }
            ServerRestoreCommand::Apply(args) => {
                if !dry_run && !yes {
                    return Err(CommandError::validation(
                        action,
                        "Applying a server restore requires --yes",
                    ));
                }
                let id = parse_id(args.id, action)?;
                let plan = store
                    .plan_server_restore(&id, &args.backup_id, None, &control)
                    .map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if !plan.blockers.is_empty() {
                    return Err(CommandError::validation(action, plan.blockers.join(" ")));
                }
                if dry_run {
                    return Ok(success(
                        action,
                        format!(
                            "Restore dry run verified plan {}. No files were written.",
                            plan.fingerprint
                        ),
                        json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":true}),
                    ));
                }
                let outcome = owner::restore_server_backup(
                    &store,
                    &id,
                    &args.backup_id,
                    &args.expected_plan_fingerprint,
                    &control,
                )
                .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Restored {} file(s) ({} byte(s)) from backup {}; rollback journal {}.",
                        outcome.restored_files,
                        outcome.restored_bytes,
                        outcome.backup_id,
                        outcome.restore_id
                    ),
                    json!({"outcome":outcome,"database_opened":false,"writes_performed":outcome.writes_performed,"dry_run":false}),
                ))
            }
            ServerRestoreCommand::Status(args) => {
                let id = parse_id(args.id, action)?;
                let status = store.server_restore_status(&id).map_err(fail)?;
                let message = match &status.pending {
                    Some(pending) => format!(
                        "Restore {} from backup {} is pending; fingerprint {}.",
                        pending.restore_id, pending.backup_id, pending.fingerprint
                    ),
                    None => format!("No pending restore for {}.", status.instance_id.as_str()),
                };
                Ok(success(
                    action,
                    message,
                    json!({"status":status,"database_opened":false,"writes_performed":false}),
                ))
            }
            ServerRestoreCommand::Rollback(args) => {
                if !dry_run && !yes {
                    return Err(CommandError::validation(
                        action,
                        "Rolling back a server restore requires --yes",
                    ));
                }
                let id = parse_id(args.id, action)?;
                let status = store.server_restore_status(&id).map_err(fail)?;
                let Some(pending) = status.pending.as_ref() else {
                    return Err(fail(ServerStoreError::NotFound));
                };
                if pending.fingerprint != args.expected_journal_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(
                        action,
                        format!(
                            "Rollback dry run verified journal {}. No files were written.",
                            pending.fingerprint
                        ),
                        json!({"status":status,"database_opened":false,"writes_performed":false,"dry_run":true}),
                    ));
                }
                let outcome = owner::rollback_server_restore(
                    &store,
                    &id,
                    &args.expected_journal_fingerprint,
                    &control,
                )
                .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Rolled back restore {} from backup {} ({} file(s), {} byte(s)).",
                        outcome.restore_id,
                        outcome.backup_id,
                        outcome.restored_files,
                        outcome.restored_bytes
                    ),
                    json!({"outcome":outcome,"database_opened":false,"writes_performed":outcome.writes_performed,"dry_run":false}),
                ))
            }
        }
    })
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}
