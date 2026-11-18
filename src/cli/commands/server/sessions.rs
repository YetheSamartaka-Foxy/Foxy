use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerSessionCommand};
use crate::core::server::{
    ContentStageControl, ReforgerSessionIntent, ServerInstanceId, ServerStore, ServerStoreError,
};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerSessionCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerSessionCommand::List(_) => "server.session.list",
        ServerSessionCommand::Plan(_) => "server.session.plan",
        ServerSessionCommand::Apply(_) => "server.session.apply",
    };
    if matches!(&command, ServerSessionCommand::Apply(_)) && !cli.yes && !cli.dry_run {
        return Err(CommandError::validation(
            action,
            "Applying a Reforger session intent requires --yes",
        ));
    }
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control: ContentStageControl| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        match command {
            ServerSessionCommand::List(args) => {
                let id = parse_id(args.id, action)?;
                let inventory = store
                    .reforger_sessions(&id, &control)
                    .map_err(|error| store_error(action, error))?;
                let message = format!(
                    "{} save point(s) for {}; generation {}; inventory {}.",
                    inventory.sessions.len(),
                    inventory.instance_id.as_str(),
                    inventory.generation,
                    inventory.fingerprint
                );
                Ok(success(
                    action,
                    message,
                    json!({
                        "inventory": inventory,
                        "database_opened": false,
                        "writes_performed": false
                    }),
                ))
            }
            ServerSessionCommand::Plan(args) => {
                let id = parse_id(args.id, action)?;
                let intent = session_intent(args.mode, args.save_id, action)?;
                let plan = store
                    .plan_reforger_session(&id, intent, args.expected_generation, &control)
                    .map_err(|error| store_error(action, error))?;
                let mut lines = vec![format!(
                    "Session intent plan {} for {} at generation {}. Blockers: {}.",
                    plan.fingerprint,
                    plan.instance_id.as_str(),
                    plan.expected_generation,
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
                    json!({
                        "plan": plan,
                        "database_opened": false,
                        "writes_performed": false
                    }),
                ))
            }
            ServerSessionCommand::Apply(args) => {
                let id = parse_id(args.id, action)?;
                let intent = session_intent(args.mode, args.save_id, action)?;
                let plan = store
                    .plan_reforger_session(&id, intent, args.expected_generation, &control)
                    .map_err(|error| store_error(action, error))?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(store_error(action, ServerStoreError::StaleRevision));
                }
                if !plan.blockers.is_empty() {
                    return Err(CommandError::validation(action, plan.blockers.join(" ")));
                }
                if dry_run {
                    return Ok(success(
                        action,
                        format!(
                            "Session intent dry run verified plan {}. No intent or owner was changed.",
                            plan.fingerprint
                        ),
                        json!({
                            "plan": plan,
                            "database_opened": false,
                            "writes_performed": false,
                            "dry_run": true
                        }),
                    ));
                }
                if !yes {
                    return Err(CommandError::validation(
                        action,
                        "Applying a Reforger session intent requires --yes",
                    ));
                }
                let _claim = super::content::claim(&store, action)?;
                let inventory = store
                    .apply_reforger_session(plan, &control)
                    .map_err(|error| store_error(action, error))?;
                Ok(success(
                    action,
                    format!(
                        "Saved session intent for {} at generation {}.",
                        inventory.instance_id.as_str(),
                        inventory.generation
                    ),
                    json!({
                        "inventory": inventory,
                        "database_opened": false,
                        "writes_performed": true,
                        "dry_run": false
                    }),
                ))
            }
        }
    })
}

fn session_intent(
    mode: String,
    save_id: Option<String>,
    action: &str,
) -> Result<ReforgerSessionIntent, CommandError> {
    match mode.as_str() {
        "configured" => {
            if save_id.is_some() {
                return Err(CommandError::validation(
                    action,
                    "--save-id is only valid with --mode selected",
                ));
            }
            Ok(ReforgerSessionIntent::Configured)
        }
        "latest" => {
            if save_id.is_some() {
                return Err(CommandError::validation(
                    action,
                    "--save-id is only valid with --mode selected",
                ));
            }
            Ok(ReforgerSessionIntent::Latest)
        }
        "selected" => {
            let save_id = save_id.ok_or_else(|| {
                CommandError::validation(action, "--mode selected requires --save-id <UUID>")
            })?;
            Ok(ReforgerSessionIntent::Selected { save_id })
        }
        "new" => {
            if save_id.is_some() {
                return Err(CommandError::validation(
                    action,
                    "--save-id is only valid with --mode selected",
                ));
            }
            Ok(ReforgerSessionIntent::New)
        }
        _ => Err(CommandError::validation(
            action,
            "--mode must be configured, latest, selected, or new",
        )),
    }
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}
