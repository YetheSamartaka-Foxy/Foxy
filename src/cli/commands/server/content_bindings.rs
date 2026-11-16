use serde_json::json;

use super::content::{claim, request};
use super::instances::{store_error as definition_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContentBindingsCommand};
use crate::core::server::{
    ContentBindingsDocument, ServerInstanceId, ServerStore, ServerStoreError,
};

pub(super) fn run(
    cli: &CliArgs,
    store: &ServerStore,
    command: ServerContentBindingsCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ServerContentBindingsCommand::Show(args) => {
            let action = "server.content.bindings.show";
            let id = ServerInstanceId::try_from(args.id)
                .map_err(|message| CommandError::validation(action, message))?;
            let snapshot = store
                .content_bindings(&id)
                .map_err(|error| store_error(action, error))?;
            let mut lines = vec![match &snapshot.document {
                Some(document) => format!(
                    "Saved content binding generation {}: {} profile(s), review current: {}. No addon files were changed.",
                    document.generation,
                    document.bindings.len(),
                    snapshot.review_current
                ),
                None => "No saved content bindings. No files were written.".into(),
            }];
            lines.extend(snapshot.issues.iter().map(|issue| (*issue).to_string()));
            if let Some(document) = &snapshot.document {
                lines.extend(binding_lines(document));
            }
            Ok(success(
                action,
                lines.join("\n"),
                json!({
                    "state":snapshot,"database_opened":false,"writes_performed":false
                }),
            ))
        }
        ServerContentBindingsCommand::Plan(args) => {
            let action = "server.content.bindings.plan";
            let request = request(store, args, action)?;
            let before = store
                .content_bindings(&request.instance_id)
                .map_err(|error| store_error(action, error))?;
            let plan = store
                .plan_content_bindings(request, before.fingerprint.as_deref())
                .map_err(|error| store_error(action, error))?;
            let mut lines = vec![format!(
                "Review {} content profile(s) for binding generation {}.\nSave fingerprint: {}\nOnly content-bindings.json will be replaced. No content acquisition, copy or deployment is authorized.",
                plan.document.bindings.len(),
                plan.document.generation,
                plan.fingerprint
            )];
            lines.extend(binding_lines(&plan.document));
            for binding in &plan.preview.bindings {
                lines.extend(binding.addon_paths.iter().map(|path| {
                    format!(
                        "  {} -> {} [{:?}]",
                        path.source.display(),
                        path.destination.display(),
                        path.state
                    )
                }));
            }
            lines.extend(
                plan.preview
                    .blockers
                    .iter()
                    .map(|blocker| format!("{}: {}", blocker.code, blocker.message)),
            );
            Ok(success(
                action,
                lines.join("\n"),
                json!({"plan":plan,"database_opened":false,"writes_performed":false}),
            ))
        }
        ServerContentBindingsCommand::Save(args) => {
            let action = "server.content.bindings.save";
            let request = request(store, args.inputs, action)?;
            let before = store
                .content_bindings(&request.instance_id)
                .map_err(|error| store_error(action, error))?;
            let plan = store
                .plan_content_bindings(request, before.fingerprint.as_deref())
                .map_err(|error| store_error(action, error))?;
            if plan.fingerprint != args.expected_plan_fingerprint {
                return Err(store_error(action, ServerStoreError::StaleRevision));
            }
            if cli.dry_run {
                return Ok(success(
                    action,
                    "Content binding save preview; no files were written or ownership acquired."
                        .into(),
                    json!({"dry_run":true,"plan":plan,"database_opened":false,"writes_performed":false}),
                ));
            }
            let _claim = claim(store, action)?;
            let state = store
                .apply_content_bindings(plan)
                .map_err(|error| store_error(action, error))?;
            Ok(success(
                action,
                "Reviewed content bindings saved; no addon files were acquired or changed.".into(),
                json!({"dry_run":false,"state":state,"database_opened":false,"writes_performed":true,"downloads_started":false,"deployment_ready":false}),
            ))
        }
    }
}

fn binding_lines(document: &ContentBindingsDocument) -> impl Iterator<Item = String> + '_ {
    document.bindings.iter().map(|item| {
        format!(
            "{}: role={:?}, order={}\n  Repository: {} at {}\n  Source folder: {}",
            item.binding.source_id,
            item.binding.role,
            item.binding.order,
            item.profile.remote_url,
            item.profile.local_path.display(),
            item.folder.path.display()
        )
    })
}

fn store_error(action: &str, error: ServerStoreError) -> CommandError {
    if error == ServerStoreError::UnsupportedVersion {
        CommandError::operation(action, error.to_string())
    } else {
        definition_error(action, error)
    }
}
