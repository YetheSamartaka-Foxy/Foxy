use foxy_supervisor::control::Operation;
use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerDeploymentActivationCommand};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError, owner};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerDeploymentActivationCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerDeploymentActivationCommand::Plan(_) => "server.deploy.activation.plan",
        ServerDeploymentActivationCommand::Apply(_) => "server.deploy.activation.apply",
        ServerDeploymentActivationCommand::Status(_) => "server.deploy.activation.status",
        ServerDeploymentActivationCommand::RecoveryPlan(_) => {
            "server.deploy.activation.recovery-plan"
        }
        ServerDeploymentActivationCommand::Resolve(_) => "server.deploy.activation.resolve",
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control| {
        let store =
            ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
        let id = |value: String| {
            ServerInstanceId::try_from(value)
                .map_err(|message| CommandError::validation(action, message))
        };
        let fail = |error: ServerStoreError| store_error(action, error);
        match command {
            ServerDeploymentActivationCommand::Plan(args) => {
                let plan = store
                    .plan_deployment_activation(
                        &id(args.id)?,
                        &args.preparation_id,
                        args.expected_deployment_generation,
                        args.expected_activation_generation,
                        &control,
                    )
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Native activation review {}; prepared source {}; runtime candidate {}; {} files / {} bytes. Plan only; source and adopted installation are unchanged, and no process was started.",
                        plan.fingerprint,
                        plan.preparation_id,
                        plan.runtime.runtime_id,
                        plan.native_runtime.realized_entries.len(),
                        plan.native_runtime.payload.total_bytes
                    ),
                    json!({"plan":plan,"database_opened":false,"writes_performed":false,"process_started":false,"dry_run":dry_run}),
                ))
            }
            ServerDeploymentActivationCommand::Apply(args) => {
                let inputs = args.inputs;
                let plan = store
                    .plan_deployment_activation(
                        &id(inputs.id)?,
                        &inputs.preparation_id,
                        inputs.expected_deployment_generation,
                        inputs.expected_activation_generation,
                        &control,
                    )
                    .map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(action,"Activation dry run verified the exact reviewed plan. No owner, files, or installation admission were created.".into(),
                        json!({"plan":plan,"database_opened":false,"writes_performed":false,"process_started":false,"dry_run":true})));
                }
                if !yes {
                    return Err(CommandError::validation(
                        action,
                        "Activating a deployment requires --yes after reviewing the plan",
                    ));
                }
                let mut foreground = owner::start_or_attach(&context).map_err(|error| {
                    CommandError::operation(
                        action,
                        format!("Cannot attach to the game-space owner: {error}"),
                    )
                })?;
                let result = foreground.attachment.run_deployment(
                    Operation::ActivateDeployment {
                        instance_id: plan.instance_id.as_str().to_string(),
                        preparation_id: plan.preparation_id.clone(),
                        expected_selection_generation: plan.expected_selection_generation,
                        expected_activation_generation: plan.expected_activation_generation,
                        review_fingerprint: plan.fingerprint.clone(),
                    },
                    &control,
                );
                let stop_result = if foreground.is_owned() {
                    foreground.stop().err()
                } else {
                    None
                };
                let outcome = result.map_err(|error| CommandError::operation(
                    action,
                    format!("Deployment was not confirmed: {error}. Inspect activation status and recovery before submitting another operation."),
                ))?;
                if let Some(error) = stop_result {
                    return Err(CommandError::operation(
                        action,
                        format!(
                            "Activation outcome is recorded as {}; foreground owner cleanup failed: {error}. Inspect owner status before retrying.",
                            outcome.status
                        ),
                    ));
                }
                Ok(success(
                    action,
                    format!(
                        "Activation outcome: {}; generation {:?}; runtime {:?}; verified {}; journal pending {}; intent pending {}. No process was started.",
                        outcome.status,
                        outcome.activation_generation,
                        outcome.runtime_id,
                        outcome.runtime_verified,
                        outcome.journal_pending,
                        outcome.intent_pending
                    ),
                    json!({"outcome":outcome,"database_opened":false,"dry_run":false}),
                ))
            }
            ServerDeploymentActivationCommand::Status(args) => {
                let status = store
                    .deployment_activation_status(&id(args.id)?, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Activation generation {}; runtime integrity verified {}; recovery required {}. No files were written and no process was started.",
                        status.activation_generation,
                        status.integrity_verified,
                        status.recovery_required
                    ),
                    json!({"status":status,"database_opened":false,"writes_performed":false,"process_started":false}),
                ))
            }
            ServerDeploymentActivationCommand::RecoveryPlan(args) => {
                let review = store
                    .plan_deployment_resolution(&id(args.id)?, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Recovery review {}; expected outcome {}; activation generation {}; runtime {:?}; journal present {}. Review only; no files or owner state changed.",
                        review.fingerprint,
                        review.expected_outcome,
                        review.activation_generation,
                        review.runtime_id,
                        review.journal_present
                    ),
                    json!({"review":review,"database_opened":false,"writes_performed":false,"process_started":false,"dry_run":dry_run}),
                ))
            }
            ServerDeploymentActivationCommand::Resolve(args) => {
                let instance_id = id(args.id)?;
                let review = store
                    .plan_deployment_resolution(&instance_id, &control)
                    .map_err(fail)?;
                if review.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(action,"Recovery dry run verified the exact reviewed resolution. No owner, receipt, or journal change was created.".into(),
                        json!({"review":review,"database_opened":false,"writes_performed":false,"process_started":false,"dry_run":true})));
                }
                if !yes {
                    return Err(CommandError::validation(
                        action,
                        "Resolving deployment recovery requires --yes after reviewing the plan",
                    ));
                }
                let mut foreground = owner::start_or_attach(&context).map_err(|error| {
                    CommandError::operation(
                        action,
                        format!("Cannot attach to the game-space owner: {error}"),
                    )
                })?;
                let result = foreground.attachment.run_deployment(
                    Operation::ResolveDeployment {
                        instance_id: instance_id.as_str().to_string(),
                        review_fingerprint: review.fingerprint.clone(),
                    },
                    &control,
                );
                let stop_result = if foreground.is_owned() {
                    foreground.stop().err()
                } else {
                    None
                };
                let outcome = result.map_err(|error| CommandError::operation(
                    action,
                    format!("Recovery was not confirmed: {error}. Inspect activation status and recovery before submitting another operation."),
                ))?;
                if let Some(error) = stop_result {
                    return Err(CommandError::operation(
                        action,
                        format!(
                            "Recovery outcome is recorded as {}; foreground owner cleanup failed: {error}. Inspect owner status before retrying.",
                            outcome.status
                        ),
                    ));
                }
                Ok(success(
                    action,
                    format!(
                        "Recovery outcome: {}; generation {:?}; journal pending {}; intent pending {}. No process was started.",
                        outcome.status,
                        outcome.activation_generation,
                        outcome.journal_pending,
                        outcome.intent_pending
                    ),
                    json!({"outcome":outcome,"database_opened":false,"dry_run":false,"process_started":false}),
                ))
            }
        }
    })
}
