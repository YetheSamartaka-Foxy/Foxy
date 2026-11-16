use super::instances::{store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContentAcquisitionCommand};
use crate::core::server::{
    ContentAcquisitionStatus, ServerContext, ServerInstanceId, ServerStore, ServerStoreError,
};
use serde_json::json;

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContext,
    command: ServerContentAcquisitionCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match command {
        ServerContentAcquisitionCommand::Plan(_) => "server.content.acquire.plan",
        ServerContentAcquisitionCommand::Apply(_) => "server.content.acquire.apply",
    };
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let (args, expected) = match command {
            ServerContentAcquisitionCommand::Plan(args) => (args, None),
            ServerContentAcquisitionCommand::Apply(args) => {
                (args.inputs, Some(args.expected_plan_fingerprint))
            }
        };
        if expected.is_some() && !dry_run && !yes {
            return Err(CommandError::validation(
                action,
                "Repository acquisition requires --yes",
            ));
        }
        let id = ServerInstanceId::try_from(args.id)
            .map_err(|message| CommandError::validation(action, message))?;
        let plan = store
            .plan_content_acquisition(&id, args.seed_stage.as_deref(), &control)
            .map_err(|error| store_error(action, error))?;
        if args
            .expected_bindings_fingerprint
            .as_ref()
            .is_some_and(|expected| expected != &plan.bindings_fingerprint)
            || expected
                .as_ref()
                .is_some_and(|expected| expected != &plan.fingerprint)
        {
            return Err(store_error(action, ServerStoreError::StaleRevision));
        }
        if expected.is_none() || dry_run {
            return Ok(success(
                action,
                format!(
                    "Reviewed {} repository profile(s); {} blocker(s). Fingerprint: {}. No network requests or writes were performed.",
                    plan.profiles.len(),
                    plan.blockers.len(),
                    plan.fingerprint
                ),
                json!({"plan":plan,"database_opened":false,"writes_performed":false,"downloads_started":false,"dry_run":dry_run}),
            ));
        }
        if !plan.blockers.is_empty() {
            return Err(CommandError::validation(action, plan.blockers.join(" ")));
        }
        let _claim = super::content::claim(&store, action)?;
        let outcome = store
            .acquire_content(&plan, &control)
            .map_err(|error| store_error(action, error))?;
        let completed = outcome.status == ContentAcquisitionStatus::Complete;
        let message = outcome.message.clone();
        let mut result = success(
            action,
            message,
            json!({"outcome":outcome,"database_opened":false,"writes_performed":outcome.writes_performed,"downloads_started":outcome.downloads_started,"dry_run":false}),
        );
        if !completed {
            result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
        }
        Ok(result)
    })
}
