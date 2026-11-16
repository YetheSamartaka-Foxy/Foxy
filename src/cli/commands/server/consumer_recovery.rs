use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerInstanceCommand};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};

pub(super) fn run(
    cli: &CliArgs,
    context_args: ServerContextArgs,
    command: ServerInstanceCommand,
) -> Result<CommandSuccess, CommandError> {
    let (action, applies) = match &command {
        ServerInstanceCommand::RecoveryPlan(_) => ("server.instance.recovery-plan", false),
        ServerInstanceCommand::Recover(_) => ("server.instance.recover", true),
        _ => {
            return Err(CommandError::validation(
                "server.instance",
                "Not a consumer recovery command",
            ));
        }
    };
    let context = capture(context_args, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control| {
        let store =
            ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
        let (id_value, expected) = match command {
            ServerInstanceCommand::RecoveryPlan(args) => (args.id, None),
            ServerInstanceCommand::Recover(args) => (args.id, Some(args.expected_plan_fingerprint)),
            _ => unreachable!(),
        };
        let id = ServerInstanceId::try_from(id_value)
            .map_err(|message| CommandError::validation(action, message))?;
        let plan = store
            .plan_consumer_recovery(&id)
            .map_err(|error| store_error(action, error))?;
        if !applies {
            let shown = plan.entries.len().min(128);
            let truncated = plan.entries.len().saturating_sub(shown);
            let mut message = format!(
                "Consumer recovery review {}; {} eligible, {} pending, {} completed-history records. Review only; no records were retired and no process was started.",
                plan.fingerprint, plan.eligible_count, plan.pending_count, plan.completed_history
            );
            if !plan.owner_authenticated {
                message
                    .push_str(" Owner authentication is unavailable; cleanup is not authorized.");
            }
            if !plan.inspection_complete {
                message.push_str(" Inspection is incomplete; unknown records are preserved.");
            }
            if truncated > 0 {
                message.push_str(&format!(
                    " {truncated} entries omitted from the text summary."
                ));
            }
            return Ok(success(
                action,
                message,
                json!({"plan":plan,"database_opened":false,"writes_performed":false,"process_started":false,"dry_run":dry_run,"displayed_entries":shown,"omitted_entries":truncated}),
            ));
        }

        let expected = expected.expect("recover command carries a reviewed fingerprint");
        if plan.fingerprint != expected {
            return Err(store_error(action, ServerStoreError::StaleRevision));
        }
        if dry_run {
            return Ok(success(
                action,
                "Consumer recovery dry run verified the exact reviewed plan. No records were retired and no owner or process state changed.".into(),
                json!({"plan":plan,"database_opened":false,"writes_performed":false,"process_started":false,"dry_run":true}),
            ));
        }
        if !yes {
            return Err(CommandError::validation(
                action,
                "Recovering consumers requires --yes after reviewing the plan",
            ));
        }
        if !plan.owner_authenticated || !plan.inspection_complete || plan.eligible_count == 0 {
            return Err(CommandError::validation(
                action,
                "Consumer recovery requires an authenticated owner, complete inspection, and at least one eligible record",
            ));
        }
        let permit = crate::core::server::owner::permit(&context).map_err(|error| {
            CommandError::operation(action, format!("Cannot obtain metadata authority: {error}"))
        })?;
        let Some(_permit) = permit else {
            return Err(CommandError::operation(
                action,
                "Cannot recover consumers without an authenticated server owner",
            ));
        };
        let outcome = store
            .recover_consumers(&id, &expected, &control)
            .map_err(|error| store_error(action, error))?;
        let mut message = format!(
            "Consumer recovery retired {} verified record(s); {} record(s) remain. {}. No process was started.",
            outcome.retired_count,
            outcome.remaining_records,
            if outcome.cancelled {
                "Operation cancelled"
            } else if outcome.writes_performed {
                "Writes completed"
            } else {
                "No writes were needed"
            }
        );
        if let Some(error) = &outcome.error {
            message.push_str(&format!(" Recovery stopped: {error}."));
        }
        if !outcome.inspection_complete {
            message.push_str(" Inspection is incomplete; the remaining count is last observed.");
        }
        let failed = outcome.cancelled || outcome.error.is_some() || !outcome.inspection_complete;
        let mut reply = success(
            action,
            message,
            json!({"outcome":outcome,"database_opened":false,"dry_run":false,"process_started":false}),
        );
        if failed {
            reply.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
        }
        Ok(reply)
    })
}
