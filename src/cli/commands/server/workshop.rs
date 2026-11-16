use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerWorkshopCommand};
use crate::core::server::{
    ServerStore, ServerStoreError, WorkshopBatchStatus, WorkshopItemAction, WorkshopRequest,
};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerWorkshopCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match command {
        ServerWorkshopCommand::List => "server.workshop.list",
        ServerWorkshopCommand::Plan(_) => "server.workshop.plan",
        ServerWorkshopCommand::Acquire(_) => "server.workshop.acquire",
        ServerWorkshopCommand::Verify(_) => "server.workshop.verify",
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let (args, expected) = match command {
            ServerWorkshopCommand::List => {
                let snapshot = store
                    .workshop_items()
                    .map_err(|error| store_error(action, error))?;
                let message = if snapshot.items.is_empty() {
                    "No tracked Workshop items in this game space.".to_owned()
                } else {
                    snapshot
                        .items
                        .iter()
                        .map(|item| {
                            format!(
                                "{}  {}  pinned manifest {}  profile {}",
                                item.item_id, item.title, item.pinned_manifest, item.profile
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                return Ok(success(
                    action,
                    message,
                    json!({"workshop":snapshot,"database_opened":false,"writes_performed":false}),
                ));
            }
            ServerWorkshopCommand::Verify(args) => {
                let report = store
                    .verify_workshop_item(&args.item, &control)
                    .map_err(|error| match error {
                        ServerStoreError::NotFound => CommandError {
                            action: action.into(),
                            message: "This Workshop item is not tracked in this game space".into(),
                            code: crate::cli::exit_codes::NOT_FOUND,
                        },
                        error => store_error(action, error),
                    })?;
                let message = if report.verified {
                    format!(
                        "Pinned revision {} of item {} verified: {} file(s), {} byte(s).",
                        report.manifest_gid, report.item_id, report.files, report.bytes
                    )
                } else {
                    format!(
                        "Pinned revision {} of item {} does not match its inventory: {} Use `server workshop acquire --repair`.",
                        report.manifest_gid,
                        report.item_id,
                        report.error.as_deref().unwrap_or("")
                    )
                };
                let verified = report.verified;
                let mut result = success(
                    action,
                    message,
                    json!({"verification":report,"database_opened":false}),
                );
                if !verified {
                    result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
                }
                return Ok(result);
            }
            ServerWorkshopCommand::Plan(args) => (args, None),
            ServerWorkshopCommand::Acquire(args) => {
                (args.inputs, Some(args.expected_plan_fingerprint))
            }
        };
        if expected.is_some() && !dry_run && !yes {
            return Err(CommandError::validation(
                action,
                "Acquiring Workshop items requires --yes",
            ));
        }
        let request = WorkshopRequest {
            items: args.items,
            account: (!args.anonymous).then_some(args.account),
            repair: args.repair,
        };
        let plan = store
            .plan_workshop(request, &control)
            .map_err(|error| store_error(action, error))?;
        if expected
            .as_ref()
            .is_some_and(|expected| expected != &plan.fingerprint)
        {
            return Err(store_error(action, ServerStoreError::StaleRevision));
        }
        if expected.is_none() || dry_run {
            let mut lines: Vec<String> = plan
                .items
                .iter()
                .map(|item| {
                    let action = match item.action {
                        WorkshopItemAction::Acquire => "acquire",
                        WorkshopItemAction::Update => "update",
                        WorkshopItemAction::Repair => "repair",
                        WorkshopItemAction::Current => "current",
                        WorkshopItemAction::Unavailable => "unavailable",
                    };
                    format!(
                        "{} {}: {} manifest {} ({} bytes){}",
                        item.item_id,
                        item.title,
                        action,
                        item.manifest_gid.as_deref().unwrap_or("none"),
                        item.file_size,
                        item.issue
                            .map(|issue| format!("; {}", issue.message()))
                            .unwrap_or_default()
                    )
                })
                .collect();
            lines.push(format!("Entitled to Workshop content: {}", plan.entitled));
            lines.extend(
                plan.blockers
                    .iter()
                    .map(|blocker| format!("Blocked: {blocker}")),
            );
            lines.push(format!(
                "Fingerprint: {}. Steam was reviewed read-only; nothing was downloaded or written.",
                plan.fingerprint
            ));
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
            .apply_workshop(&plan, &control)
            .map_err(|error| store_error(action, error))?;
        let complete = outcome.status == WorkshopBatchStatus::Complete;
        let succeeded = outcome.items.iter().filter(|item| item.pinned).count();
        let message = format!(
            "Workshop acquisition {}: {} item(s) pinned out of {} reviewed.",
            match outcome.status {
                WorkshopBatchStatus::Complete => "complete",
                WorkshopBatchStatus::Partial => "partial",
                WorkshopBatchStatus::Failed => "failed",
                WorkshopBatchStatus::Cancelled => "cancelled",
            },
            succeeded,
            outcome.items.len()
        );
        let mut result = success(
            action,
            message,
            json!({"outcome":outcome,"database_opened":false,"writes_performed":outcome.writes_performed,"dry_run":false}),
        );
        if !complete {
            result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
        }
        Ok(result)
    })
}
