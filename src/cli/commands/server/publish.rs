use std::fmt::Write;

use serde_json::json;

use super::instances::{capture, parse_id, store_error, success};
use super::{CommandError, CommandSuccess, content_stage};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerPublishCommand};
use crate::core::server::{PlayerPublicationRequest, ServerAddonRole, ServerStore};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerPublishCommand,
) -> Result<CommandSuccess, CommandError> {
    let (action, inputs, apply, expected) = match command {
        ServerPublishCommand::Plan(inputs) => ("server.publish.plan", inputs, false, None),
        ServerPublishCommand::Apply {
            inputs,
            expected_plan_fingerprint,
        } => {
            if !cli.dry_run && !cli.yes {
                return Err(CommandError::validation(
                    "server.publish.apply",
                    "Publication requires --yes. Confirm you have permission to distribute the selected files.",
                ));
            }
            (
                "server.publish.apply",
                inputs,
                true,
                Some(expected_plan_fingerprint),
            )
        }
        ServerPublishCommand::Verify {
            inputs,
            expected_plan_fingerprint,
        } => (
            "server.publish.verify",
            inputs,
            false,
            expected_plan_fingerprint,
        ),
    };
    let context = capture(context, action)?;
    let request = PlayerPublicationRequest {
        instance_id: parse_id(inputs.id, action)?,
        output: inputs.output,
        repository_name: inputs.repository_name,
        recommendations: inputs.recommendations,
        include_public_keys: inputs.include_public_keys,
    };
    let preview = action == "server.publish.plan" || apply && cli.dry_run;
    let dry_run = cli.dry_run;
    content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        if preview {
            let plan = store
                .plan_player_publication(&request, &control)
                .map_err(|error| store_error(action, error))?;
            let mut message = format!(
                "Selected deployment {}\nReview fingerprint {}\n{} selected files, {} bytes. No files written.",
                plan.record_id, plan.fingerprint, plan.files, plan.bytes,
            );
            if plan.destination_exists {
                message.push_str("\nThis output exists and can only be verified.");
            }
            for addon in &plan.arma_addons {
                let role = match (addon.role, addon.selected) {
                    (ServerAddonRole::Required, _) => "Shared",
                    (ServerAddonRole::Optional, true) => "Selected recommendation",
                    (ServerAddonRole::Optional, false) => "Unselected recommendation",
                    (ServerAddonRole::ServerOnly, _) => "Excluded server-only",
                };
                let _ = write!(
                    message,
                    "\n{} ({}) - {}. {} eligible files, {} excluded by file policy.",
                    addon.name, addon.profile, role, addon.files, addon.excluded_files,
                );
            }
            if let Some(review) = &plan.reforger {
                let _ = write!(message, "\nScenario {}", review.scenario_id);
                if let Some(provider) = &review.scenario_provider {
                    let _ = write!(message, " from {provider}");
                }
                for addon in &review.addons {
                    let _ = write!(
                        message,
                        "\nReference {} {} ({:?})",
                        addon.id, addon.version, addon.provider
                    );
                }
                for omission in &review.omissions {
                    let _ = write!(message, "\nOmitted optional addon {omission}");
                }
            }
            return Ok(success(
                action,
                message,
                json!({
                    "plan": plan, "writes_performed": false, "database_opened": false, "dry_run": dry_run,
                }),
            ));
        }
        let result = if apply {
            store.apply_player_publication_request(
                &request,
                expected.as_deref().unwrap_or_default(),
                &control,
            )
        } else {
            store.verify_player_publication_request(&request, expected.as_deref(), &control)
        }
        .map_err(|error| store_error(action, error))?;
        let message = if apply {
            "Published and verified the selected player content."
        } else {
            "Verified the output against the selected deployment."
        };
        Ok(success(
            action,
            message.into(),
            json!({
                "outcome": result, "writes_performed": result.writes_performed,
                "database_opened": false, "dry_run": dry_run,
            }),
        ))
    })
}
