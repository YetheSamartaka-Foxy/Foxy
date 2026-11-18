use serde_json::json;

use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerBackupCommand, ServerContextArgs};
use crate::core::server::{
    BackupConsistencyMode, BackupRetentionPolicy, ContentStageControl, ServerInstanceId,
    ServerStore, ServerStoreError, owner,
};

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerBackupCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerBackupCommand::Plan(_) => "server.backup.plan",
        ServerBackupCommand::Create(_) => "server.backup.create",
        ServerBackupCommand::List(_) => "server.backup.list",
        ServerBackupCommand::Verify(_) => "server.backup.verify",
        ServerBackupCommand::Pin(_) => "server.backup.pin",
        ServerBackupCommand::Unpin(_) => "server.backup.unpin",
        ServerBackupCommand::Retention(_) => "server.backup.retention",
        ServerBackupCommand::PrunePlan(_) => "server.backup.prune-plan",
        ServerBackupCommand::Prune(_) => "server.backup.prune",
    };
    let context = capture(context, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    super::content_stage::controlled(cli, action, move |control: ContentStageControl| {
        let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
        let fail = |error: ServerStoreError| store_error(action, error);
        match command {
            ServerBackupCommand::Plan(args) => {
                let id = parse_id(args.id, action)?;
                let mode = consistency_mode(args.live);
                let plan = store
                    .plan_server_backup(&id, mode, None, &control)
                    .map_err(fail)?;
                let mut lines = vec![format!(
                    "Backup plan {} for {} ({:?}, stopped_verified={}, files={}, bytes={}). Blockers: {}.",
                    plan.fingerprint,
                    plan.instance_id.as_str(),
                    plan.consistency.mode,
                    plan.consistency.stopped_verified,
                    plan.planned_files,
                    plan.planned_uncompressed_bytes,
                    plan.blockers.len()
                )];
                lines.extend(plan.consistency.notes.iter().cloned());
                lines.extend(
                    plan.blockers
                        .iter()
                        .map(|blocker| format!("Blocker: {blocker}")),
                );
                Ok(success(
                    action,
                    lines.join("\n"),
                    json!({"plan": plan, "database_opened": false, "writes_performed": false}),
                ))
            }
            ServerBackupCommand::Create(args) => {
                let id = parse_id(args.id, action)?;
                let mode = consistency_mode(args.live);
                if dry_run {
                    let plan = store
                        .plan_server_backup(&id, mode, None, &control)
                        .map_err(fail)?;
                    if let Some(expected) = args.expected_plan_fingerprint.as_deref()
                        && plan.fingerprint != expected
                    {
                        return Err(fail(ServerStoreError::StaleRevision));
                    }
                    if !plan.blockers.is_empty() {
                        return Err(CommandError::validation(action, plan.blockers.join(" ")));
                    }
                    return Ok(success(
                        action,
                        format!(
                            "Backup dry run verified plan {} ({:?}, {} file(s), {} byte(s)). Nothing was written.",
                            plan.fingerprint,
                            plan.consistency.mode,
                            plan.planned_files,
                            plan.planned_uncompressed_bytes
                        ),
                        json!({"plan": plan, "database_opened": false, "writes_performed": false, "dry_run": true}),
                    ));
                }
                let outcome = owner::create_server_backup(
                    &store,
                    &id,
                    mode,
                    args.expected_plan_fingerprint.as_deref(),
                    !args.no_retention,
                    &control,
                )
                .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Created verified backup {} ({:?}, stopped_verified={}, archive={} bytes, pruned={}).",
                        outcome.manifest.backup_id,
                        outcome.manifest.consistency.mode,
                        outcome.manifest.consistency.stopped_verified,
                        outcome.archive_bytes,
                        outcome.pruned_backups.len()
                    ),
                    json!({
                        "outcome": outcome,
                        "database_opened": false,
                        "writes_performed": outcome.writes_performed,
                        "dry_run": false
                    }),
                ))
            }
            ServerBackupCommand::List(args) => {
                let id = parse_id(args.id, action)?;
                let listing = store.list_server_backups(&id).map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "{} backup(s), {} archive byte(s); keep_last={}, pinned={}, corrupt={}, unrecognized={}.",
                        listing.backups.len(),
                        listing.total_archive_bytes,
                        listing.retention.policy.keep_last,
                        listing.retention.pinned.len(),
                        listing.corrupt_backups.len(),
                        listing.unrecognized_entries.len()
                    ),
                    json!({"listing": listing, "database_opened": false, "writes_performed": false}),
                ))
            }
            ServerBackupCommand::Verify(args) => {
                let id = parse_id(args.id, action)?;
                let report = store
                    .verify_server_backup(&id, &args.backup_id, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Verified backup {} (valid={}, files={}, uncompressed={} bytes, archive={} bytes, mode={:?}, stopped_verified={}).",
                        report.backup_id,
                        report.valid,
                        report.verified_files,
                        report.verified_uncompressed_bytes,
                        report.archive_bytes,
                        report.consistency.mode,
                        report.consistency.stopped_verified
                    ),
                    json!({"verification": report, "database_opened": false, "writes_performed": false}),
                ))
            }
            ServerBackupCommand::Pin(args) | ServerBackupCommand::Unpin(args) => {
                let pinned = action == "server.backup.pin";
                let id = parse_id(args.id, action)?;
                let current = store.backup_retention(&id).map_err(fail)?;
                if current.generation != args.expected_generation {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if pinned {
                    store
                        .verify_server_backup(&id, &args.backup_id, &control)
                        .map_err(fail)?;
                }
                if dry_run {
                    return Ok(success(
                        action,
                        format!(
                            "Backup {} dry run verified generation {}. Nothing was written.",
                            if pinned { "pin" } else { "unpin" },
                            current.generation
                        ),
                        json!({"retention": current, "database_opened": false, "writes_performed": false, "dry_run": true}),
                    ));
                }
                let _claim = super::content::claim(&store, action)?;
                let retention = store
                    .set_backup_pin(&id, &args.backup_id, pinned, args.expected_generation)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "{} {}; retention generation {}.",
                        if pinned { "Pinned" } else { "Unpinned" },
                        args.backup_id,
                        retention.generation
                    ),
                    json!({"retention": retention, "database_opened": false, "writes_performed": true, "dry_run": false}),
                ))
            }
            ServerBackupCommand::Retention(args) => {
                let id = parse_id(args.id, action)?;
                let current = store.backup_retention(&id).map_err(fail)?;
                if current.generation != args.expected_generation {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                let policy = BackupRetentionPolicy {
                    keep_last: args.keep_last.unwrap_or(current.policy.keep_last),
                    max_total_bytes: if args.clear_max_total_bytes {
                        None
                    } else {
                        args.max_total_bytes.or(current.policy.max_total_bytes)
                    },
                };
                policy
                    .validate()
                    .map_err(|message| CommandError::validation(action, message))?;
                if dry_run {
                    return Ok(success(
                        action,
                        format!(
                            "Retention dry run validated keep_last={} max_total_bytes={:?}. Nothing was written.",
                            policy.keep_last, policy.max_total_bytes
                        ),
                        json!({"policy": policy, "generation": current.generation, "database_opened": false, "writes_performed": false, "dry_run": true}),
                    ));
                }
                let _claim = super::content::claim(&store, action)?;
                let retention = store
                    .set_backup_retention_policy(&id, policy, args.expected_generation)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Updated retention policy (keep_last={}, max_total_bytes={:?}); generation {}.",
                        retention.policy.keep_last,
                        retention.policy.max_total_bytes,
                        retention.generation
                    ),
                    json!({"retention": retention, "database_opened": false, "writes_performed": true, "dry_run": false}),
                ))
            }
            ServerBackupCommand::PrunePlan(args) => {
                let id = parse_id(args.id, action)?;
                let plan = store
                    .plan_backup_prune(&id, args.keep_last, &control)
                    .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Prune plan {} for {}: {} candidate(s), {} reclaimable byte(s); {} retained; {} corrupt; {} unrecognized.",
                        plan.fingerprint,
                        plan.instance_id.as_str(),
                        plan.candidates.len(),
                        plan.reclaimable_bytes,
                        plan.retained.len(),
                        plan.corrupt_backups.len(),
                        plan.unrecognized_entries.len()
                    ),
                    json!({"plan": plan, "database_opened": false, "writes_performed": false}),
                ))
            }
            ServerBackupCommand::Prune(args) => {
                if !dry_run && !yes {
                    return Err(CommandError::validation(
                        action,
                        "Pruning server backups requires --yes",
                    ));
                }
                let id = parse_id(args.id, action)?;
                let plan = store
                    .plan_backup_prune(&id, args.keep_last, &control)
                    .map_err(fail)?;
                if plan.fingerprint != args.expected_plan_fingerprint {
                    return Err(fail(ServerStoreError::StaleRevision));
                }
                if !plan.blockers.is_empty() {
                    return Err(CommandError::validation(action, plan.blockers.join(" ")));
                }
                if dry_run {
                    return Ok(success(
                        action,
                        format!(
                            "Backup prune dry run verified plan {} ({} candidate(s), {} byte(s)). Nothing was removed.",
                            plan.fingerprint,
                            plan.candidates.len(),
                            plan.reclaimable_bytes
                        ),
                        json!({"plan": plan, "database_opened": false, "writes_performed": false, "dry_run": true}),
                    ));
                }
                let outcome = owner::prune_server_backups(
                    &store,
                    &id,
                    &args.expected_plan_fingerprint,
                    args.keep_last,
                    &control,
                )
                .map_err(fail)?;
                Ok(success(
                    action,
                    format!(
                        "Pruned {} backup(s), reclaimed {} byte(s); retained {} backup(s). Receipt {}.",
                        outcome.removed.len(),
                        outcome.reclaimed_bytes,
                        outcome.retained_backups.len(),
                        outcome.receipt_id
                    ),
                    json!({
                        "outcome": outcome,
                        "database_opened": false,
                        "writes_performed": outcome.writes_performed,
                        "dry_run": false
                    }),
                ))
            }
        }
    })
}

fn consistency_mode(live: bool) -> BackupConsistencyMode {
    if live {
        BackupConsistencyMode::BestEffortLive
    } else {
        BackupConsistencyMode::StoppedVerified
    }
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}
