use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerDeployCommand, ServerDeploymentReconcileCommand,
};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerDeployCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerDeployCommand::Pins(_) => "server.deploy.pins",
        ServerDeployCommand::Pin(_) => "server.deploy.pin",
        ServerDeployCommand::Unpin(_) => "server.deploy.unpin",
        ServerDeployCommand::Reconcile {
            command: ServerDeploymentReconcileCommand::Plan(_),
        } => "server.deploy.reconcile.plan",
        ServerDeployCommand::Reconcile {
            command: ServerDeploymentReconcileCommand::Apply(_),
        } => "server.deploy.reconcile.apply",
        _ => unreachable!(),
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let fail = |error: ServerStoreError| store_error(action, error);
        match command {
            ServerDeployCommand::Pins(args) => {
                let id = parse_id(args.id, action)?;
                let pins = store.deployment_pins(&id).map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "{} pinned deployment record(s); pin generation {}.",
                        pins.records.len(),
                        pins.generation
                    ),
                    json!({"pins": pins, "database_opened": false, "writes_performed": false}),
                ))
            }
            ServerDeployCommand::Pin(args) | ServerDeployCommand::Unpin(args) => {
                let pinned = action == "server.deploy.pin";
                if !dry_run && !yes {
                    return Err(CommandError::validation(
                        action,
                        "Changing deployment pins requires --yes",
                    ));
                }
                let id = parse_id(args.id, action)?;
                let current = store
                    .inspect_deployment_pin(
                        &id,
                        &args.record_id,
                        pinned,
                        args.expected_pin_generation,
                    )
                    .map_err(fail)?;
                if dry_run {
                    return Ok(success(
                        action,
                        "Pin dry run verified the expected generation. Nothing was written.".into(),
                        json!({"pins": current, "database_opened": false, "writes_performed": false, "dry_run": true}),
                    ));
                }
                let _claim = super::content::claim(&store, action)?;
                let pins = store
                    .set_deployment_pin(&id, &args.record_id, pinned, args.expected_pin_generation)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "{} {}; pin generation {}. No deployment artifact was deleted.",
                        if pinned { "Pinned" } else { "Unpinned" },
                        args.record_id,
                        pins.generation
                    ),
                    json!({"pins": pins, "database_opened": false, "writes_performed": true, "dry_run": false}),
                ))
            }
            ServerDeployCommand::Reconcile {
                command: ServerDeploymentReconcileCommand::Plan(args),
            } => {
                let id = parse_id(args.id, action)?;
                let plan = store.plan_reconciliation(&id, &control).map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "{} reclaimable artifact(s), {} byte(s); {} retained; {} blocker(s). Fingerprint: {}. Nothing was removed.",
                        plan.candidates.len(),
                        plan.reclaimable_bytes,
                        plan.retained.len(),
                        plan.blockers.len(),
                        plan.fingerprint
                    ),
                    json!({"plan": plan, "database_opened": false, "writes_performed": false}),
                ))
            }
            ServerDeployCommand::Reconcile {
                command: ServerDeploymentReconcileCommand::Apply(args),
            } => {
                if !dry_run && !yes {
                    return Err(CommandError::validation(
                        action,
                        "Reclaiming deployment artifacts requires --yes",
                    ));
                }
                let id = parse_id(args.id, action)?;
                let plan = store.plan_reconciliation(&id, &control).map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if !plan.blockers.is_empty() {
                    return Err(CommandError::validation(action, plan.blockers.join(" ")));
                }
                if dry_run {
                    return Ok(success(
                        action,
                        "Reconciliation dry run verified the exact reviewed plan. Nothing was removed.".into(),
                        json!({"plan": plan, "database_opened": false, "writes_performed": false, "dry_run": true}),
                    ));
                }
                let _claim = super::content::claim(&store, action)?;
                let outcome = store.apply_reconciliation(&plan, &control).map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Removed {} unreferenced artifact(s), {} byte(s). Sources, records, configuration archives and runtime state were kept.",
                        outcome.removed.len(),
                        outcome.reclaimed_bytes
                    ),
                    json!({"outcome": outcome, "database_opened": false, "writes_performed": outcome.writes_performed, "dry_run": false}),
                ))
            }
            _ => unreachable!(),
        }
    })
}

fn parse_id(value: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(value).map_err(|message| CommandError::validation(action, message))
}
