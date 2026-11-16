use serde_json::json;

use super::content::claim;
use super::content_stage::controlled;
use super::instances::{capture, store_error, success};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerReforgerAddonChannel, ServerReforgerAddonsCommand,
};
use crate::cli::commands::{CommandError, CommandSuccess};
use crate::core::server::{ReforgerAddonChannel, ServerInstanceId, ServerStore};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerReforgerAddonsCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerReforgerAddonsCommand::List(_) => "server.reforger-addons.list",
        ServerReforgerAddonsCommand::Plan(_) => "server.reforger-addons.plan",
        ServerReforgerAddonsCommand::Acquire(_) => "server.reforger-addons.acquire",
    };
    let context = capture(context, action)?;
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    let (inputs, expected) = match command {
        ServerReforgerAddonsCommand::List(args) => {
            let id = ServerInstanceId::try_from(args.id)
                .map_err(|message| CommandError::validation(action, message))?;
            let lock = store
                .selected_reforger_addon_lock(&id)
                .map_err(|error| store_error(action, error))?;
            return Ok(success(
                action,
                "Selected Reforger addon lock. Selection alone does not verify current file bytes."
                    .into(),
                json!({"lock":lock,"integrity_verified":false,"writes_performed":false,"database_opened":false}),
            ));
        }
        ServerReforgerAddonsCommand::Plan(args) => (args, None),
        ServerReforgerAddonsCommand::Acquire(args) => {
            (args.inputs, Some(args.expected_plan_fingerprint))
        }
    };
    let id = ServerInstanceId::try_from(inputs.id)
        .map_err(|message| CommandError::validation(action, message))?;
    let channel = match inputs.channel {
        ServerReforgerAddonChannel::Stable => ReforgerAddonChannel::Stable,
        ServerReforgerAddonChannel::Experimental => ReforgerAddonChannel::Experimental,
    };
    let plan = store
        .plan_reforger_addon_acquisition(&id, channel, inputs.scenario_provider.as_deref())
        .map_err(|error| store_error(action, error))?;
    if let Some(expected) = &expected
        && expected != &plan.fingerprint
    {
        return Err(CommandError::validation(
            action,
            "The addon acquisition review changed. Review again before applying.",
        ));
    }
    if expected.is_none() || cli.dry_run {
        return Ok(success(
            action,
            "Reforger addon acquisition reviewed. No engine, download or file writes started."
                .into(),
            json!({"plan":plan,"writes_performed":false,"process_started":false,"database_opened":false}),
        ));
    }
    if !cli.yes {
        return Err(CommandError::validation(
            action,
            "Acquisition requires --yes and the exact reviewed fingerprint.",
        ));
    }
    if !plan.blockers.is_empty() {
        return Err(CommandError::validation(action, plan.blockers.join(" ")));
    }
    controlled(cli, action, move |control| {
        let _claim = claim(&store, action)?;
        let lock = store
            .acquire_reforger_addons(plan, &control)
            .map_err(|error| acquisition_error(action, error))?;
        let degraded = lock.degraded;
        let mut result = success(action,
            if degraded { "Reforger addon revision retained with explicit optional omissions. Review the degraded set before deployment." }
            else { "Exact Reforger addon revision retained. Stage and review it before deployment." }.into(),
            json!({"lock":lock,"degraded":degraded,"writes_performed":true,"process_started":true,"database_opened":false,"active_deployment_changed":false}));
        if degraded {
            result.exit_code = crate::cli::exit_codes::PARTIAL_SUCCESS;
        }
        Ok(result)
    })
}

fn acquisition_error(action: &str, error: crate::core::server::ServerStoreError) -> CommandError {
    use crate::core::server::ServerStoreError;
    match error {
        ServerStoreError::StaleRevision
        | ServerStoreError::UnsupportedGame
        | ServerStoreError::UnsupportedVersion
        | ServerStoreError::NotFound
        | ServerStoreError::InstallationNotFound => store_error(action, error),
        _ => CommandError::operation(
            action,
            format!(
                "{error}. The previous selected addon revision was retained; private acquisition data may remain."
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::ServerStoreError;

    #[test]
    fn runtime_acquisition_failures_are_operational_while_stale_review_stays_validation() {
        assert_eq!(
            acquisition_error(
                "server.reforger-addons.acquire",
                ServerStoreError::Invalid("Engine exited before acquisition completed".into())
            )
            .code,
            crate::cli::exit_codes::OPERATION_FAILED
        );
        assert_eq!(
            acquisition_error(
                "server.reforger-addons.acquire",
                ServerStoreError::StaleRevision
            )
            .code,
            crate::cli::exit_codes::VALIDATION_ERROR
        );
    }
}
