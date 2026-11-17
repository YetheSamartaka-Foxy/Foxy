use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, TimeZone, Utc};
use foxy_supervisor::control::Operation;
use serde_json::json;

use super::instances::{capture, store_error, success};
use super::lifecycle::attach_existing;
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerMaintenanceActionArg, ServerMaintenanceCommand,
    ServerMaintenanceDefinitionArgs,
};
use crate::core::schedule::{JobSchedule, ScheduleClock, first_valid_local_instant};
use crate::core::server::jobs::{
    self, JobStatus, MaintenanceAction, MaintenanceChange, MaintenanceJobDefinition,
    MaintenancePolicy, Occurrence, PlayerPolicy,
};
use crate::core::server::{ServerContext, ServerInstanceId, ServerStore};

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

pub(super) fn run(
    cli: &CliArgs,
    args: ServerContextArgs,
    command: ServerMaintenanceCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerMaintenanceCommand::List(_) => "server.maintenance.list",
        ServerMaintenanceCommand::Add(_) => "server.maintenance.add",
        ServerMaintenanceCommand::Set(_) => "server.maintenance.set",
        ServerMaintenanceCommand::Remove(_) => "server.maintenance.remove",
        ServerMaintenanceCommand::Run(_) => "server.maintenance.run",
        ServerMaintenanceCommand::Cancel(_) => "server.maintenance.cancel",
        ServerMaintenanceCommand::History(_) => "server.maintenance.history",
    };
    let context = capture(args, action)?;
    if !crate::core::game::registry()
        .get(context.game_id())
        .is_some_and(|game| game.server_capabilities().lifecycle)
    {
        return Err(CommandError::validation(
            action,
            "Scheduled maintenance is available for game spaces with a native server lifecycle.",
        ));
    }
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    match command {
        ServerMaintenanceCommand::List(args) => {
            let id = parse_id(args.id, action)?;
            list(action, &store, &id)
        }
        ServerMaintenanceCommand::History(args) => {
            let id = parse_id(args.id, action)?;
            history(action, &store, &id)
        }
        ServerMaintenanceCommand::Add(args) => {
            let id = parse_id(args.id, action)?;
            let definition = definition(args.definition, action)?;
            change(
                cli,
                action,
                &store,
                &id,
                args.expected_revision,
                MaintenanceChange::Add(definition),
            )
        }
        ServerMaintenanceCommand::Set(args) => {
            let id = parse_id(args.id, action)?;
            job_id(&args.job_id, action)?;
            let definition = definition(args.definition, action)?;
            change(
                cli,
                action,
                &store,
                &id,
                args.expected_revision,
                MaintenanceChange::Set {
                    id: args.job_id,
                    definition,
                },
            )
        }
        ServerMaintenanceCommand::Remove(args) => {
            let id = parse_id(args.id, action)?;
            job_id(&args.job_id, action)?;
            if !cli.yes && !cli.dry_run {
                return Err(CommandError::validation(
                    action,
                    "Removing a maintenance job requires --yes",
                ));
            }
            change(
                cli,
                action,
                &store,
                &id,
                args.expected_revision,
                MaintenanceChange::Remove { id: args.job_id },
            )
        }
        ServerMaintenanceCommand::Run(args) => {
            let id = parse_id(args.id, action)?;
            job_id(&args.job_id, action)?;
            if args.expected_policy_revision == 0 {
                return Err(CommandError::validation(
                    action,
                    "--expected-policy-revision must be the job's current revision",
                ));
            }
            owner_request(
                cli,
                action,
                &context,
                Operation::RunMaintenanceJob {
                    instance_id: id.as_str().into(),
                    job_id: args.job_id,
                    expected_policy_revision: args.expected_policy_revision,
                },
                "Running a maintenance job now requires --yes",
            )
        }
        ServerMaintenanceCommand::Cancel(args) => {
            let id = parse_id(args.id, action)?;
            if !jobs::valid_occurrence_id(&args.occurrence_id) {
                return Err(CommandError::validation(
                    action,
                    "The occurrence id must look like occ_ followed by 32 lowercase hex digits",
                ));
            }
            owner_request(
                cli,
                action,
                &context,
                Operation::CancelMaintenanceOccurrence {
                    instance_id: id.as_str().into(),
                    occurrence_id: args.occurrence_id,
                },
                "Cancelling a maintenance occurrence requires --yes",
            )
        }
    }
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}

fn job_id(value: &str, action: &str) -> Result<(), CommandError> {
    if jobs::valid_job_id(value) {
        Ok(())
    } else {
        Err(CommandError::validation(
            action,
            "The job id must look like job_ followed by 16 lowercase hex digits",
        ))
    }
}

fn parse_time_of_day(value: &str) -> Option<u16> {
    let time = NaiveTime::parse_from_str(value, "%H:%M").ok()?;
    use chrono::Timelike;
    u16::try_from(time.hour() * 60 + time.minute()).ok()
}

fn parse_weekdays(value: &str) -> Option<u8> {
    if value.eq_ignore_ascii_case("daily") {
        return Some(0b0111_1111);
    }
    let mut bits = 0u8;
    for day in value.split(',') {
        let index = DAYS
            .iter()
            .position(|known| known.eq_ignore_ascii_case(day.trim()))?;
        bits |= 1 << index;
    }
    (bits != 0).then_some(bits)
}

fn parse_at(value: &str, clock: ScheduleClock) -> Option<u64> {
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return u64::try_from(time.timestamp_millis()).ok();
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").ok()?;
    let millis = match clock {
        ScheduleClock::Utc => Utc.from_utc_datetime(&naive).timestamp_millis(),
        ScheduleClock::Local => {
            first_valid_local_instant(naive, |local| Local.from_local_datetime(local))?
                .timestamp_millis()
        }
    };
    u64::try_from(millis).ok()
}

fn definition(
    args: ServerMaintenanceDefinitionArgs,
    action: &str,
) -> Result<MaintenanceJobDefinition, CommandError> {
    let clock = if args.utc {
        ScheduleClock::Utc
    } else {
        ScheduleClock::Local
    };
    let schedule = match (&args.at, &args.weekly, &args.time) {
        (Some(at), None, None) => JobSchedule::OnceAt {
            unix_ms: parse_at(at, clock).ok_or_else(|| {
                CommandError::validation(
                    action,
                    "--at must be RFC 3339 with an offset or YYYY-MM-DDTHH:MM",
                )
            })?,
        },
        (None, Some(days), Some(time)) => JobSchedule::Recurring {
            minutes_of_day: parse_time_of_day(time)
                .ok_or_else(|| CommandError::validation(action, "--time must be HH:MM"))?,
            weekdays: parse_weekdays(days).ok_or_else(|| {
                CommandError::validation(
                    action,
                    "--weekly must be daily or a comma list of mon, tue, wed, thu, fri, sat, sun",
                )
            })?,
        },
        _ => {
            return Err(CommandError::validation(
                action,
                "Give either --at for a one-time job or --weekly with --time",
            ));
        }
    };
    let definition = MaintenanceJobDefinition {
        name: args.name,
        enabled: !args.disabled,
        action: match args.action {
            ServerMaintenanceActionArg::Restart => MaintenanceAction::Restart,
            ServerMaintenanceActionArg::RotateMission => MaintenanceAction::RotateMission,
            ServerMaintenanceActionArg::Update => MaintenanceAction::Update,
            ServerMaintenanceActionArg::Backup => MaintenanceAction::Backup,
        },
        schedule,
        clock,
        policy: MaintenancePolicy {
            arma: (args.announcement.is_some()
                || args.announcement_delay_seconds > 0
                || args.mission_template.is_some()
                || args.require_backup)
                .then_some(crate::core::server::jobs::ArmaMaintenancePolicy {
                    announcement: args.announcement,
                    announcement_delay_seconds: args.announcement_delay_seconds,
                    mission_template: args.mission_template,
                    require_backup: args.require_backup,
                }),
            grace_minutes: args.grace_minutes,
            player_policy: if args.require_empty {
                PlayerPolicy::RequireEmpty
            } else {
                PlayerPolicy::Ignore
            },
            max_defer_minutes: args.max_defer_minutes,
            max_attempts: args.max_attempts,
            retry_delay_seconds: args.retry_delay_seconds,
            stop_timeout_seconds: args.stop_timeout_seconds,
        },
    };
    definition
        .validate()
        .map_err(|message| CommandError::validation(action, message))?;
    Ok(definition)
}

fn change(
    cli: &CliArgs,
    action: &str,
    store: &ServerStore,
    id: &ServerInstanceId,
    expected_revision: u64,
    change: MaintenanceChange,
) -> Result<CommandSuccess, CommandError> {
    let (catalog, affected) = if cli.dry_run {
        store.preview_maintenance(id, expected_revision, change)
    } else {
        let _claim = super::content::claim(store, action)?;
        store.change_maintenance(id, expected_revision, change)
    }
    .map_err(|error| store_error(action, error))?;
    let message = format!(
        "{} maintenance catalog revision {} for {}{}.{}",
        if cli.dry_run { "Preview of" } else { "Saved" },
        catalog.revision,
        id.as_str(),
        affected
            .as_deref()
            .map(|job| format!(" (job {job})"))
            .unwrap_or_default(),
        if cli.dry_run {
            " Nothing was written."
        } else {
            " A running owner picks the change up within a few seconds. An occurrence that already stopped the server still starts it again; other open occurrences of a changed or removed job are cancelled."
        }
    );
    Ok(success(
        action,
        message,
        json!({
            "instance_id": id,
            "catalog": catalog,
            "job_id": affected,
            "dry_run": cli.dry_run,
            "owner_contacted": false,
            "process_started": false
        }),
    ))
}

fn owner_request(
    cli: &CliArgs,
    action: &'static str,
    context: &ServerContext,
    operation: Operation,
    confirmation: &str,
) -> Result<CommandSuccess, CommandError> {
    if cli.dry_run {
        let mut data = serde_json::to_value(&operation).unwrap_or_else(|_| json!({}));
        data["dry_run"] = json!(true);
        data["owner_contacted"] = json!(false);
        return Ok(success(
            action,
            "Dry run: no owner was contacted and no occurrence was recorded.".into(),
            data,
        ));
    }
    if !cli.yes {
        return Err(CommandError::validation(action, confirmation));
    }
    let attachment = attach_existing(context, action)?;
    let reply = attachment
        .maintenance(operation)
        .map_err(|error| CommandError::operation(action, error.to_string()))?;
    if let Some(error) = &reply.error {
        return Err(CommandError::operation(action, error.clone()));
    }
    let message = match (&reply.occurrence_id, reply.created) {
        (Some(occurrence), true) => format!(
            "Recorded occurrence {occurrence}. The owner runs it shortly; follow it with server maintenance history."
        ),
        (Some(occurrence), false) => format!(
            "Occurrence {occurrence} is {}.",
            reply.state.map(state_label).unwrap_or("recorded")
        ),
        (None, _) => "The owner returned no occurrence.".into(),
    };
    Ok(success(
        action,
        message,
        json!({
            "occurrence_id": reply.occurrence_id,
            "state": reply.state,
            "created": reply.created,
            "owner_contacted": true
        }),
    ))
}

fn state_label(state: jobs::OccurrenceState) -> &'static str {
    use jobs::OccurrenceState::*;
    match state {
        Claimed => "recorded",
        Deferred => "deferred",
        Running => "running",
        RetryWait => "waiting to retry",
        Succeeded => "succeeded",
        Failed => "failed",
        Skipped => "skipped",
        Blocked => "blocked",
        Cancelled => "cancelled",
        Unknown => "unknown",
    }
}

fn local_time(unix_ms: u64) -> String {
    crate::core::schedule::unix_ms_to_local(unix_ms)
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown time".into())
}

fn schedule_label(definition: &MaintenanceJobDefinition) -> String {
    let clock = match definition.clock {
        ScheduleClock::Local => "local time",
        ScheduleClock::Utc => "UTC",
    };
    match definition.schedule {
        JobSchedule::OnceAt { unix_ms } => format!("once at {} local time", local_time(unix_ms)),
        JobSchedule::Recurring {
            minutes_of_day,
            weekdays,
        } => {
            let days: Vec<&str> = DAYS
                .iter()
                .enumerate()
                .filter(|(index, _)| weekdays & (1 << index) != 0)
                .map(|(_, day)| *day)
                .collect();
            format!(
                "weekly {} at {:02}:{:02} {clock}",
                if weekdays == 0b0111_1111 {
                    "daily".to_owned()
                } else {
                    days.join(",")
                },
                minutes_of_day / 60,
                minutes_of_day % 60
            )
        }
    }
}

fn occurrence_line(occurrence: &Occurrence) -> String {
    format!(
        "{} {} ({} attempt(s), policy revision {}){}{}",
        occurrence.occurrence_id,
        state_label(occurrence.state),
        occurrence.attempts,
        occurrence.policy_revision,
        occurrence
            .reason
            .map(|reason| format!(": {}", reason.describe()))
            .unwrap_or_default(),
        occurrence
            .message
            .as_ref()
            .filter(|message| occurrence
                .reason
                .is_none_or(|reason| reason.describe() != message.as_str()))
            .map(|message| format!(" {message}"))
            .unwrap_or_default()
    )
}

fn job_line(status: &JobStatus) -> String {
    let job = &status.job;
    let action = match job.definition.action {
        MaintenanceAction::Restart => "restart",
        MaintenanceAction::RotateMission => "rotate_mission",
        MaintenanceAction::Update => "update",
        MaintenanceAction::Backup => "backup",
    };
    let mut line = format!(
        "{} ({}, policy revision {}): {action} {}",
        job.definition.name,
        job.id,
        job.revision,
        schedule_label(&job.definition)
    );
    if let Some(next) = status.next_run_unix_ms {
        line.push_str(&format!("; next run {}", local_time(next)));
    }
    if let Some(last) = &status.last {
        line.push_str(&format!("; last {}", state_label(last.state)));
    }
    if let Some(blocker) = &status.blocker {
        line.push_str(&format!("; {blocker}"));
    }
    line
}

fn list(
    action: &str,
    store: &ServerStore,
    id: &ServerInstanceId,
) -> Result<CommandSuccess, CommandError> {
    let (overview, _) = store
        .maintenance_overview(id)
        .map_err(|error| store_error(action, error))?;
    let owner_running = overview.owner_running;
    let mut lines = vec![format!(
        "{} maintenance job(s) for {} (catalog revision {}). {}",
        overview.jobs.len(),
        id.as_str(),
        overview.catalog_revision,
        if owner_running {
            "A server owner is running and executes due jobs."
        } else {
            "No server owner is running, so no job runs until one starts."
        }
    )];
    lines.extend(overview.jobs.iter().map(job_line));
    Ok(success(
        action,
        lines.join("\n"),
        json!({
            "instance_id": id,
            "overview": overview,
            "database_opened": false,
            "writes_performed": false
        }),
    ))
}

fn history(
    action: &str,
    store: &ServerStore,
    id: &ServerInstanceId,
) -> Result<CommandSuccess, CommandError> {
    let history = store
        .maintenance_history(id)
        .map_err(|error| store_error(action, error))?;
    let mut lines = vec![format!(
        "{} recorded occurrence(s) for {}{}{}.",
        history.occurrences.len(),
        id.as_str(),
        if history.unreadable > 0 {
            format!(
                "; {} unreadable record(s) left untouched",
                history.unreadable
            )
        } else {
            String::new()
        },
        if history.truncated {
            "; the listing was truncated"
        } else {
            ""
        }
    )];
    lines.extend(history.occurrences.iter().map(occurrence_line));
    Ok(success(
        action,
        lines.join("\n"),
        json!({
            "instance_id": id,
            "occurrences": history.occurrences,
            "unreadable": history.unreadable,
            "truncated": history.truncated,
            "writes_performed": false
        }),
    ))
}
