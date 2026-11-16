use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerDeployCommand, ServerDeploymentPlanArgs};
use crate::core::server::{
    DeploymentCompositionRequest, ServerInstanceId, ServerStore, ServerStoreError,
};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerDeployCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match command {
        ServerDeployCommand::Prepare(_) => "server.deploy.prepare",
        ServerDeployCommand::Commit(_) => "server.deploy.commit",
        ServerDeployCommand::List(_) => "server.deploy.list",
        ServerDeployCommand::Verify(_) => "server.deploy.verify",
        ServerDeployCommand::SelectPlan(_) => "server.deploy.select-plan",
        ServerDeployCommand::Select(_) => "server.deploy.select",
        ServerDeployCommand::Plan(_)
        | ServerDeployCommand::Runtime { .. }
        | ServerDeployCommand::Activation { .. }
        | ServerDeployCommand::Pins(_)
        | ServerDeployCommand::Pin(_)
        | ServerDeployCommand::Unpin(_)
        | ServerDeployCommand::Reconcile { .. } => unreachable!(),
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| error_for(action, error))?;
        let fail = |error| error_for(action, error);
        match command {
            ServerDeployCommand::Prepare(args) => {
                let request = request(args.inputs, action)?;
                let plan = store
                    .plan_deployment_record(request, args.expected_deployment_generation, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Deployment record {} prepared. Fingerprint: {}. Expected selection generation: {}. Runtime activation and launch remain unavailable.",
                        plan.record_id, plan.fingerprint, plan.expected_generation
                    ),
                    json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":dry_run}),
                ))
            }
            ServerDeployCommand::Commit(args) => {
                let request = request(args.inputs.inputs, action)?;
                let plan = store
                    .plan_deployment_record(
                        request,
                        args.inputs.expected_deployment_generation,
                        &control,
                    )
                    .map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(action, "Deployment record dry run verified the exact reviewed plan. No files or claims were created.".into(), json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":true})));
                }
                let _claim = super::content::claim(&store, action)?;
                let commit = store
                    .apply_deployment_record(plan, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Deployment record {} retained; offline selection generation {} (changed: {}). No runtime deployment was activated.",
                        commit.record.record_id, commit.state.generation, commit.selection_changed
                    ),
                    json!({"commit":commit,"database_opened":false,"writes_performed":true,"dry_run":false}),
                ))
            }
            ServerDeployCommand::List(args) => {
                let id = parse_id(args.id, action)?;
                let state = store.deployment_records(&id).map_err(fail)?;
                if control.snapshot().cancel_requested {
                    return Err(fail(ServerStoreError::RecordCancelled));
                }
                Ok(success(
                    action,
                    format!(
                        "Deployment record selection generation {}; selected: {}. Listed identifiers have not been verified.\n{}",
                        state.generation,
                        state.selected.as_deref().unwrap_or("none"),
                        state.records.join("\n")
                    ),
                    json!({"state":state,"integrity_verified":false,"database_opened":false,"writes_performed":false}),
                ))
            }
            ServerDeployCommand::Verify(args) => {
                let id = parse_id(args.id, action)?;
                let verification = store
                    .verify_deployment_record(&id, &args.record_id, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Deployment record {} and its referenced archives are intact. Runtime readiness remains unverified.",
                        verification.record_id
                    ),
                    json!({"verification":verification,"database_opened":false,"writes_performed":false}),
                ))
            }
            ServerDeployCommand::SelectPlan(args) => {
                let id = parse_id(args.id, action)?;
                let plan = store
                    .plan_deployment_record_selection(
                        &id,
                        &args.record_id,
                        args.expected_deployment_generation,
                        &control,
                    )
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Offline deployment record selection review {}: {} -> {}; expected generation {}. No runtime activation.",
                        plan.fingerprint,
                        plan.previous_record_id.as_deref().unwrap_or("none"),
                        plan.target.record_id,
                        plan.expected_generation
                    ),
                    json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":dry_run}),
                ))
            }
            ServerDeployCommand::Select(args) => {
                if !dry_run && !yes {
                    return Err(CommandError::validation(
                        action,
                        "Offline deployment record selection requires --yes",
                    ));
                }
                let id = parse_id(args.inputs.id, action)?;
                let plan = store
                    .plan_deployment_record_selection(
                        &id,
                        &args.inputs.record_id,
                        args.inputs.expected_deployment_generation,
                        &control,
                    )
                    .map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(action, "Offline deployment record selection dry run verified the reviewed target. No files or claims were created.".into(),
                        json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":true})));
                }
                let _claim = super::content::claim(&store, action)?;
                let commit = store
                    .apply_deployment_record_selection(plan, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Selected offline deployment record {}; generation {} (changed: {}). Current draft and archives were retained. No runtime deployment was activated.",
                        commit.record.record_id, commit.state.generation, commit.selection_changed
                    ),
                    json!({"writes_performed":commit.selection_changed,"commit":commit,"database_opened":false,"dry_run":false}),
                ))
            }
            ServerDeployCommand::Plan(_)
            | ServerDeployCommand::Runtime { .. }
            | ServerDeployCommand::Activation { .. }
            | ServerDeployCommand::Pins(_)
            | ServerDeployCommand::Pin(_)
            | ServerDeployCommand::Unpin(_)
            | ServerDeployCommand::Reconcile { .. } => unreachable!(),
        }
    })
}

fn parse_id(value: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(value).map_err(|message| CommandError::validation(action, message))
}

fn request(
    args: ServerDeploymentPlanArgs,
    action: &str,
) -> Result<DeploymentCompositionRequest, CommandError> {
    Ok(DeploymentCompositionRequest {
        instance_id: parse_id(args.id, action)?,
        stage_id: args.stage,
        expected_revision: args.expected_revision,
        expected_configuration_generation: args.expected_configuration_generation,
    })
}

fn error_for(action: &str, error: ServerStoreError) -> CommandError {
    if error == ServerStoreError::UnsupportedVersion {
        CommandError::operation(action, error.to_string())
    } else {
        store_error(action, error)
    }
}
