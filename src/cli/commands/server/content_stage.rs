pub(super) mod signal;

use std::time::{Duration, Instant};

use serde_json::json;

use super::content::claim;
use super::instances::{store_error as definition_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContentStageCommand};
use crate::core::server::{
    ContentStageControl, ContentStagePlan, ServerContext, ServerInstanceId, ServerStore,
    ServerStoreError,
};

pub(super) fn run(
    cli: &CliArgs,
    store: &ServerStore,
    context: ServerContext,
    command: ServerContentStageCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerContentStageCommand::Plan(_) => "server.content.stage.plan",
        ServerContentStageCommand::Apply(_) => "server.content.stage.apply",
        ServerContentStageCommand::Verify(_) => "server.content.stage.verify",
        ServerContentStageCommand::List(_) => "server.content.stage.list",
    };
    if let ServerContentStageCommand::List(args) = command {
        let id = parse_id(args.id, action)?;
        let stages = store
            .content_stages(&id)
            .map_err(|error| store_error(action, error))?;
        return Ok(success(
            action,
            format!(
                "{} stage identifier(s); {} unrecognized entry(s) retained. Listed stages have not been verified.\n{}",
                stages.stage_ids.len(),
                stages.unrecognized_entries,
                stages.stage_ids.join("\n")
            ),
            json!({"stages":stages,"database_opened":false}),
        ));
    }
    let dry_run = cli.dry_run;
    controlled(cli, action, move |control| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        match command {
            ServerContentStageCommand::Plan(args) => {
                let id = parse_id(args.id, action)?;
                let plan = current_plan(
                    &store,
                    &id,
                    args.expected_bindings_fingerprint.as_deref(),
                    &control,
                    action,
                )?;
                let mut lines = vec![format!(
                    "Verified local stage plan {}\nFingerprint: {}\nDestination: {}\nFiles: {}; bytes: {}. No files were written. This does not activate a deployment.",
                    plan.stage_id,
                    plan.fingerprint,
                    plan.directory.display(),
                    plan.manifest
                        .entries
                        .iter()
                        .filter(|entry| entry.blake3.is_some())
                        .count(),
                    plan.manifest.total_bytes
                )];
                lines.extend(plan.manifest.sources.iter().map(|source| {
                    format!(
                        "{}: {} -> {}",
                        source.profile_name,
                        source.source.display(),
                        source.destination
                    )
                }));
                Ok(success(
                    action,
                    lines.join("\n"),
                    json!({"plan":plan,"database_opened":false,"writes_performed":false,"deployment_ready":false,"active_deployment_changed":false,"downloads_started":false}),
                ))
            }
            ServerContentStageCommand::Apply(args) => {
                let id = parse_id(args.inputs.id, action)?;
                let plan = current_plan(
                    &store,
                    &id,
                    args.inputs.expected_bindings_fingerprint.as_deref(),
                    &control,
                    action,
                )?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(store_error(action, ServerStoreError::StaleRevision));
                }
                if dry_run {
                    return Ok(success(action,"Local stage dry run verified the reviewed bytes; no files or ownership claims were created.".into(),json!({"plan":plan,"dry_run":true,"writes_performed":false,"database_opened":false,"active_deployment_changed":false})));
                }
                let _claim = claim(&store, action)?;
                let report = store
                    .apply_content_stage(plan, &control)
                    .map_err(|error| store_error(action, error))?;
                Ok(success(
                    action,
                    format!(
                        "Verified local stage {} retained (new: {}, reused: {}). No deployment was activated.",
                        report.stage_id, report.published, report.reused_existing
                    ),
                    json!({"stage":report,"database_opened":false,"dry_run":false}),
                ))
            }
            ServerContentStageCommand::Verify(args) => {
                let id = parse_id(args.id, action)?;
                let report = store
                    .verify_content_stage(&id, &args.stage_id, &control)
                    .map_err(|error| store_error(action, error))?;
                Ok(success(
                    action,
                    format!(
                        "Stage {}: {} files and {} bytes verified. Deployment readiness remains unavailable.",
                        report.stage_id, report.files, report.bytes
                    ),
                    json!({"stage":report,"database_opened":false,"writes_performed":false}),
                ))
            }
            ServerContentStageCommand::List(_) => unreachable!(),
        }
    })
}

fn current_plan(
    store: &ServerStore,
    id: &ServerInstanceId,
    expected: Option<&str>,
    control: &ContentStageControl,
    action: &str,
) -> Result<ContentStagePlan, CommandError> {
    let snapshot = store
        .content_bindings(id)
        .map_err(|error| store_error(action, error))?;
    let fingerprint = expected
        .or(snapshot.fingerprint.as_deref())
        .ok_or_else(|| {
            CommandError::validation(
                action,
                "Save reviewed content bindings before verifying local content.",
            )
        })?;
    store
        .plan_content_stage(id, fingerprint, control)
        .map_err(|error| store_error(action, error))
}

fn parse_id(value: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(value).map_err(|message| CommandError::validation(action, message))
}

fn store_error(action: &str, error: ServerStoreError) -> CommandError {
    if error == ServerStoreError::UnsupportedVersion {
        CommandError::operation(action, error.to_string())
    } else {
        definition_error(action, error)
    }
}

pub(super) fn controlled(
    cli: &CliArgs,
    action: &'static str,
    operation: impl FnOnce(ContentStageControl) -> Result<CommandSuccess, CommandError> + Send + 'static,
) -> Result<CommandSuccess, CommandError> {
    let review = action.starts_with("server.deploy.");
    let _signal = signal::SignalGuard::install().map_err(|()| {
        CommandError::operation(
            action,
            "Cannot register cancellation before verifying server artifacts",
        )
    })?;
    let control = ContentStageControl::default();
    let worker_control = control.clone();
    let worker = std::thread::Builder::new()
        .name("server-content-cli".into())
        .spawn(move || operation(worker_control))
        .map_err(|_| CommandError::operation(action, "Cannot start the server artifact worker"))?;
    let mut printed = Instant::now();
    while !worker.is_finished() {
        if signal::requested() {
            control.cancel();
        }
        if !cli.json
            && !cli.quiet
            && !cli.no_progress
            && printed.elapsed() >= Duration::from_millis(500)
        {
            let p = control.snapshot();
            if action.starts_with("server.instance.") {
                eprintln!(
                    "Foreground operation{}",
                    if p.cancel_requested {
                        "; cancellation requested, waiting for the owner outcome"
                    } else {
                        "; waiting for the owner outcome"
                    }
                );
                printed = Instant::now();
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            let files = if p.total_files == 0 {
                format!("{} files completed (total unknown)", p.files_done)
            } else {
                format!("{} of {} files", p.files_done, p.total_files)
            };
            eprintln!(
                "{} {:?}: {}, {} of {} bytes{}",
                if review {
                    "Deployment review"
                } else {
                    "Content"
                },
                p.phase,
                files,
                p.bytes_done,
                p.total_bytes,
                if p.cancel_requested {
                    "; cancellation requested, waiting for cleanup"
                } else {
                    ""
                }
            );
            printed = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    worker.join().map_err(|_| {
        CommandError::operation(
            action,
            "Server artifact worker failed; inspect retained deployment status and recovery before retrying",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    #[test]
    fn signal_cancellation_waits_for_worker_cleanup_before_returning() {
        let cli = CliArgs::try_parse_from(["foxy", "--quiet", "version"]).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let cleanup = done.clone();
        let (started, receive) = mpsc::channel();
        let requester = std::thread::spawn(move || {
            receive.recv().unwrap();
            signal::request_for_test();
        });
        let result = controlled(&cli, "server.content.stage.apply", move |control| {
            started.send(()).unwrap();
            while !control.snapshot().cancel_requested {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(20));
            cleanup.store(true, Ordering::Release);
            Err(CommandError::operation(
                "server.content.stage.apply",
                "cancelled",
            ))
        });
        requester.join().unwrap();
        assert_eq!(
            result.unwrap_err().code,
            crate::cli::exit_codes::OPERATION_FAILED
        );
        assert!(done.load(Ordering::Acquire));
    }
}
