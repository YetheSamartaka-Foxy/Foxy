use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerDeployCommand};
use crate::core::server::{
    DeploymentCompositionRequest, ServerInstanceId, ServerStore, ServerStoreError,
};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerDeployCommand,
) -> Result<CommandSuccess, CommandError> {
    let command = match command {
        ServerDeployCommand::Activation { command } => {
            return super::deployment_activation::run(cli, context, command);
        }
        ServerDeployCommand::Runtime { command } => {
            return super::deployment_preparation::run(cli, context, command);
        }
        ServerDeployCommand::Plan(args) => args,
        command @ (ServerDeployCommand::Pins(_)
        | ServerDeployCommand::Pin(_)
        | ServerDeployCommand::Unpin(_)
        | ServerDeployCommand::Reconcile { .. }) => {
            return super::reconciliation::run(cli, context, command);
        }
        command => return super::deployment_records::run(cli, context, command),
    };
    let action = "server.deploy.plan";
    let context = capture(context, action)?;
    let args = command;
    let instance_id = ServerInstanceId::try_from(args.id)
        .map_err(|message| CommandError::validation(action, message))?;
    let request = DeploymentCompositionRequest {
        instance_id,
        stage_id: args.stage,
        expected_revision: args.expected_revision,
        expected_configuration_generation: args.expected_configuration_generation,
    };
    let dry_run = cli.dry_run;
    super::content_stage::controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let plan = store
            .plan_deployment_composition(request, &control)
            .map_err(|error| {
                if error == ServerStoreError::UnsupportedVersion {
                    CommandError::operation(action, error.to_string())
                } else {
                    store_error(action, error)
                }
            })?;
        let mut lines = vec![
            format!("Deployment composition review {}", plan.fingerprint),
            format!(
                "Configuration: {}; integrity verified: {}; matches saved draft: {:?}",
                plan.configuration.status,
                plan.configuration.integrity_verified,
                plan.configuration.matches_current_draft
            ),
            format!(
                "Content stage: {}; integrity verified: {}; matches saved bindings: {:?}",
                plan.content
                    .requested_stage_id
                    .as_deref()
                    .unwrap_or("not selected"),
                plan.content.integrity_verified,
                plan.content.matches_saved_bindings
            ),
            format!(
                "Key candidates: {}; mission candidates: {}; diagnostics truncated: {}",
                plan.artifacts.key_files, plan.artifacts.mission_files, plan.artifacts.truncated
            ),
        ];
        for group in &plan.artifacts.keys {
            lines.push(format!("Key {}: {}", group.name, group.state));
            lines.extend(
                group
                    .files
                    .iter()
                    .map(|file| format!("  {} ({} bytes, {})", file.path, file.bytes, file.blake3)),
            );
        }
        for group in &plan.artifacts.missions {
            lines.push(format!("Mission {}: {}", group.template, group.state));
            lines.extend(
                group
                    .files
                    .iter()
                    .map(|file| format!("  {} ({} bytes, {})", file.path, file.bytes, file.blake3)),
            );
        }
        lines.extend(
            plan.blockers
                .iter()
                .map(|issue| format!("Blocked [{}]: {}", issue.code, issue.message)),
        );
        lines.push("Read-only review. Deployment readiness, writes, activation, downloads, leases and process start are false. The fingerprint grants no apply authority.".into());
        Ok(success(
            action,
            lines.join("\n"),
            json!({"plan":plan,"database_opened":false,"dry_run":dry_run,"writes_performed":false}),
        ))
    })
}
