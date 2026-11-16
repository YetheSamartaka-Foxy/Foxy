use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerDeploymentRuntimeCommand};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerDeploymentRuntimeCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match command {
        ServerDeploymentRuntimeCommand::Plan(_) => "server.deploy.runtime.plan",
        ServerDeploymentRuntimeCommand::Build(_) => "server.deploy.runtime.build",
        ServerDeploymentRuntimeCommand::List(_) => "server.deploy.runtime.list",
        ServerDeploymentRuntimeCommand::Verify(_) => "server.deploy.runtime.verify",
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    super::content_stage::controlled(cli, action, move |control| {
        let fail = |error: ServerStoreError| {
            if error == ServerStoreError::UnsupportedVersion {
                CommandError::operation(action, error.to_string())
            } else {
                store_error(action, error)
            }
        };
        let store = ServerStore::new(context).map_err(fail)?;
        let id = |value: String| {
            ServerInstanceId::try_from(value)
                .map_err(|message| CommandError::validation(action, message))
        };
        match command {
            ServerDeploymentRuntimeCommand::Plan(args) => {
                let plan = store
                    .plan_deployment_preparation(
                        &id(args.id)?,
                        args.expected_deployment_generation,
                        &control,
                    )
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Unactivated deployment preparation review {}; generation {}; selected record {}. {} bytes will be copied. No runtime activation or process start.",
                        plan.fingerprint,
                        plan.expected_generation,
                        plan.manifest.record_id,
                        plan.manifest.total_bytes
                    ),
                    json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":dry_run}),
                ))
            }
            ServerDeploymentRuntimeCommand::Build(args) => {
                let plan = store
                    .plan_deployment_preparation(
                        &id(args.inputs.id)?,
                        args.inputs.expected_deployment_generation,
                        &control,
                    )
                    .map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(action, "Deployment preparation dry run verified the reviewed copy. No files or claims were created.".into(),
                        json!({"plan":plan,"database_opened":false,"writes_performed":false,"dry_run":true})));
                }
                let _claim = super::content::claim(&store, action)?;
                let report = store
                    .apply_deployment_preparation(plan, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Prepared generation {}; verified {} files / {} bytes, published: {}, reused: {}. This generation is not activated or engine-ready.",
                        report.preparation_id,
                        report.files,
                        report.bytes,
                        report.published,
                        report.reused_existing
                    ),
                    json!({"writes_performed":report.writes_performed,"report":report,"database_opened":false,"dry_run":false}),
                ))
            }
            ServerDeploymentRuntimeCommand::List(args) => {
                let preparations = store.deployment_preparations(&id(args.id)?).map_err(fail)?;
                if control.snapshot().cancel_requested {
                    return Err(fail(ServerStoreError::PreparationCancelled));
                }
                Ok(success(
                    action,
                    format!(
                        "Prepared generations (inventory only, not verified):\n{}\nUnrecognized entries: {}",
                        preparations.preparation_ids.join("\n"),
                        preparations.unrecognized_entries
                    ),
                    json!({"preparations":preparations,"database_opened":false,"writes_performed":false}),
                ))
            }
            ServerDeploymentRuntimeCommand::Verify(args) => {
                let report = store
                    .verify_deployment_preparation(&id(args.id)?, &args.preparation_id, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Prepared generation {} is intact. Runtime layout and activation remain unavailable.",
                        report.preparation_id
                    ),
                    json!({"report":report,"database_opened":false,"writes_performed":false}),
                ))
            }
        }
    })
}
