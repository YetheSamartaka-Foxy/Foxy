use serde_json::json;

use super::content::claim;
use super::content_stage::controlled;
use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerKeyCommand};
use crate::core::server::ServerStore;

pub(super) fn run(
    cli: &CliArgs,
    context_args: ServerContextArgs,
    command: ServerKeyCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.keys.audit";
    let ServerKeyCommand::Audit {
        id,
        stage_id,
        operator_files,
        expected_revision,
        selected,
        fingerprint,
        apply,
    } = command;
    let id = crate::core::server::ServerInstanceId::try_from(id)
        .map_err(|message| CommandError::validation(action, message))?;
    if apply && !cli.dry_run && !cli.yes {
        return Err(CommandError::validation(
            action,
            "Applying a server key trust policy requires --apply --yes",
        ));
    }
    if apply && fingerprint.is_none() {
        return Err(CommandError::validation(
            action,
            "Applying a server key trust policy requires the reviewed --fingerprint",
        ));
    }
    let context = capture(context_args, action)?;
    let dry_run = cli.dry_run;
    controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let review = store
            .review_server_keys(&id, stage_id.as_deref(), &operator_files, &control)
            .map_err(|error| store_error(action, error))?;
        if expected_revision.is_some_and(|revision| revision != review.instance_revision) {
            return Err(store_error(
                action,
                crate::core::server::ServerStoreError::StaleRevision,
            ));
        }
        if let Some(expected) = fingerprint.as_deref()
            && expected != review.fingerprint
        {
            return Err(store_error(
                action,
                crate::core::server::ServerStoreError::StaleRevision,
            ));
        }
        if apply && !dry_run {
            let _claim = claim(&store, action)?;
            let instance = store
                .apply_server_key_review(&review, &selected, &control)
                .map_err(|error| store_error(action, error))?;
            return Ok(success(
                action,
                format!(
                    "Saved the reviewed server key trust policy at draft revision {}. Cryptographic signature verification was not performed.",
                    instance.revision
                ),
                json!({
                    "review": review,
                    "instance": instance,
                    "dry_run": false,
                    "writes_performed": true,
                    "database_opened": false,
                    "changes_client_enablement": false,
                    "cryptographic_verification": false
                }),
            ));
        }
        Ok(success(
            action,
            format!(
                "Reviewed {} server key candidate(s); no trust policy was saved. Cryptographic signature verification was not performed.",
                review.candidates.len()
            ),
            json!({
                "review": review,
                "dry_run": dry_run,
                "writes_performed": false,
                "database_opened": false,
                "ownership_claimed": false,
                "changes_client_enablement": false,
                "cryptographic_verification": false,
                "requires_apply_and_yes": true
            }),
        ))
    })
}
