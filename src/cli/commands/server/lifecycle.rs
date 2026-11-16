use foxy_supervisor::control::Operation;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use super::content_stage::controlled;
use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerInstanceCommand, ServerInstanceLogsArgs,
    ServerInstanceMonitorArgs, ServerInstanceRestartArgs, ServerInstanceStartArgs,
    ServerInstanceStopArgs,
};
use crate::cli::exit_codes;
use crate::core::server::{
    ContentStageControl, InstanceMonitoring, InstanceStartPlan, LogLine, LogSnapshot,
    RuntimeOperationOutcome, ServerContext, ServerInstanceId, ServerStore, owner,
};

pub(super) fn run(
    cli: &CliArgs,
    args: ServerContextArgs,
    command: ServerInstanceCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerInstanceCommand::StartPlan(_) => "server.instance.start-plan",
        ServerInstanceCommand::Start(_) => "server.instance.start",
        ServerInstanceCommand::Status(_) => "server.instance.status",
        ServerInstanceCommand::Monitor(args) if args.follow => "server.instance.monitor.follow",
        ServerInstanceCommand::Monitor(_) => "server.instance.monitor",
        ServerInstanceCommand::Logs(_) => "server.instance.logs",
        ServerInstanceCommand::Stop(_) => "server.instance.stop",
        ServerInstanceCommand::Restart(_) => "server.instance.restart",
        _ => {
            return Err(CommandError::validation(
                "server.instance.lifecycle",
                "Invalid lifecycle command",
            ));
        }
    };
    if cli.wipe_db {
        return Err(CommandError::validation(
            action,
            "Server lifecycle commands do not support --wipe-db",
        ));
    }
    let context = capture(args, action)?;
    let dry_run = cli.dry_run;
    let yes = cli.yes;
    let json_output = cli.json;
    let toon_output = cli.toon;
    controlled(cli, action, move |control| {
        let store =
            ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
        match command {
            ServerInstanceCommand::StartPlan(args) => {
                let id = parse_id(args.id, action)?;
                let plan = store
                    .plan_instance_start(&id, &control)
                    .map_err(|error| store_error(action, error))?;
                Ok(start_plan_success(action, plan))
            }
            ServerInstanceCommand::Start(args) => {
                start(action, &store, &context, args, &control, dry_run, yes)
            }
            ServerInstanceCommand::Status(args) => {
                let id = parse_id(args.id, action)?;
                if dry_run {
                    return Ok(intent_preview(
                        action,
                        "Status",
                        json!({"instance_id": id, "operation": "inspect_instance"}),
                    ));
                }
                let attachment = attach_existing(&context, action)?;
                let monitoring = attachment
                    .instance_monitoring(&store, &id)
                    .map_err(|_| {
                        CommandError::operation(
                            action,
                            "Authenticated runtime monitoring is unavailable; refresh the owner status and retry.",
                        )
                    })?;
                Ok(success(
                    action,
                    format!(
                        "Runtime status for {}: phase {:?}, run generation {}. {}",
                        monitoring.status.instance_id.as_str(),
                        monitoring.status.phase,
                        monitoring.status.run_generation,
                        monitoring_summary(&monitoring)
                    ),
                    json!({
                        "status": monitoring.status.clone(),
                        "monitoring": monitoring,
                        "database_opened": false,
                        "writes_performed": false,
                        "process_authority": false,
                        "readiness_claimed": false
                    }),
                ))
            }
            ServerInstanceCommand::Monitor(args) => monitor(
                action,
                &store,
                &context,
                args,
                &control,
                dry_run,
                (json_output, toon_output),
            ),
            ServerInstanceCommand::Logs(args) => logs(action, &store, &context, args, dry_run),
            ServerInstanceCommand::Stop(args) => {
                stop(action, &context, args, &control, dry_run, yes)
            }
            ServerInstanceCommand::Restart(args) => {
                restart(action, &store, &context, args, &control, dry_run, yes)
            }
            _ => Err(CommandError::validation(
                action,
                "Invalid lifecycle command",
            )),
        }
    })
}

fn monitor(
    action: &str,
    store: &ServerStore,
    context: &ServerContext,
    args: ServerInstanceMonitorArgs,
    control: &ContentStageControl,
    dry_run: bool,
    output_mode: (bool, bool),
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    validate_monitor_mode(args.follow, args.stream_json, output_mode.0, output_mode.1)
        .map_err(|message| CommandError::validation(action, message))?;
    if args.interval_ms.is_some() && !args.follow {
        return Err(CommandError::validation(
            action,
            "--interval-ms requires --follow",
        ));
    }
    if dry_run {
        return Ok(intent_preview(
            action,
            "Monitor",
            json!({
                "instance_id": id,
                "follow": args.follow,
                "interval_ms": args.interval_ms.unwrap_or(1000),
                "stream_json": args.stream_json
            }),
        ));
    }
    let attachment = attach_existing(context, action)?;
    if args.follow {
        follow_monitor(
            action,
            &attachment,
            store,
            &id,
            control,
            Duration::from_millis(u64::from(args.interval_ms.unwrap_or(1000))),
        )
    } else {
        let report = attachment.instance_monitoring(store, &id).map_err(|_| {
            CommandError::operation(
                action,
                "Monitoring is unavailable; refresh the owner status and retry.",
            )
        })?;
        Ok(monitoring_success(action, report))
    }
}

fn logs(
    action: &str,
    store: &ServerStore,
    context: &ServerContext,
    args: ServerInstanceLogsArgs,
    dry_run: bool,
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    if !(1..=128).contains(&args.limit) {
        return Err(CommandError::validation(
            action,
            "--limit must be between 1 and 128",
        ));
    }
    if args
        .contains
        .as_ref()
        .is_some_and(|value| value.len() > 256)
    {
        return Err(CommandError::validation(
            action,
            "--contains is limited to 256 bytes",
        ));
    }
    if args.contains.as_ref().is_some_and(String::is_empty) {
        return Err(CommandError::validation(
            action,
            "--contains must not be empty",
        ));
    }
    if dry_run {
        return Ok(intent_preview(
            action,
            "Logs",
            json!({
                "instance_id": id,
                "contains": args.contains,
                "after_sequence": args.after_sequence,
                "limit": args.limit
            }),
        ));
    }
    let attachment = attach_existing(context, action)?;
    let report = attachment.instance_monitoring(store, &id).map_err(|_| {
        CommandError::operation(
            action,
            "Monitoring is unavailable; refresh the owner status and retry.",
        )
    })?;
    let logs = report.observation.as_ref().map(|observation| {
        let mut snapshot = observation.logs.clone();
        snapshot.entries = select_log_lines(
            &snapshot,
            args.contains.as_deref(),
            args.after_sequence,
            args.limit,
        );
        snapshot
    });
    let count = logs.as_ref().map_or(0, |snapshot| snapshot.entries.len());
    Ok(success(
        action,
        format!(
            "{} cached log line(s) returned for {}; history is limited to 128 lines/32 KiB and belongs only to the current run. {}",
            count,
            id.as_str(),
            monitoring_summary(&report)
        ),
        json!({
            "instance_id": id,
            "logs": logs,
            "owner_id": report.status.owner_id,
            "owner_generation": report.status.owner_generation,
            "run_generation": report.status.run_generation,
            "monitoring_health": report.health,
            "observation_age_ms": report.observation_age_ms,
            "database_opened": false,
            "writes_performed": false,
            "process_authority": false,
            "readiness_claimed": false
        }),
    ))
}

fn follow_monitor(
    action: &str,
    attachment: &owner::Attachment,
    store: &ServerStore,
    id: &ServerInstanceId,
    control: &ContentStageControl,
    interval: Duration,
) -> Result<CommandSuccess, CommandError> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut emitted = 0usize;
    let mut identity: Option<(String, u64, u64)> = None;
    let mut last_sequence = None;
    loop {
        if control.snapshot().cancel_requested {
            if let Err(error) = write_stream_frame(
                &mut output,
                &json!({"type":"end","reason":"cancelled","instance_id":id}),
            ) {
                if emitted == 0 {
                    return Err(stream_output_error(action, emitted, error));
                }
                return Ok(stream_failure(
                    "Monitoring stream ended during cancellation.".into(),
                ));
            }
            return Ok(stream_completion());
        }
        let report = match attachment.instance_monitoring(store, id) {
            Ok(report) => report,
            Err(_) if emitted == 0 => {
                return Err(CommandError::operation(
                    action,
                    "Monitoring is unavailable; refresh the owner status and retry.",
                ));
            }
            Err(_) => {
                let message = "Monitoring stopped because the owner or run changed.";
                let frame = json!({"type":"error","instance_id":id,"error":message});
                let _ = write_stream_frame(&mut output, &frame);
                return Ok(stream_failure(message.into()));
            }
        };
        let current_identity = (
            report.status.owner_id.clone(),
            report.status.owner_generation,
            report.status.run_generation,
        );
        let run_changed = identity
            .as_ref()
            .is_some_and(|previous| previous != &current_identity);
        if run_changed {
            last_sequence = None;
        }
        identity = Some(current_identity);
        let entries = report
            .observation
            .as_ref()
            .map(|observation| new_log_lines(&observation.logs.entries, &mut last_sequence))
            .unwrap_or_default();
        let frame = match monitoring_frame(&report, entries, run_changed) {
            Ok(frame) => frame,
            Err(_) if emitted == 0 => {
                return Err(CommandError::operation(
                    action,
                    "Cannot serialize monitoring snapshot.",
                ));
            }
            Err(_) => {
                let message = "Monitoring stopped because a snapshot could not be serialized.";
                let _ = write_stream_frame(
                    &mut output,
                    &json!({"type":"error","instance_id":id,"error":message}),
                );
                return Ok(stream_failure(message.into()));
            }
        };
        if let Err(error) = write_stream_frame(&mut output, &frame) {
            if emitted == 0 {
                return Err(stream_output_error(action, emitted, error));
            }
            let message = "Monitoring stream ended because stdout could not accept another frame.";
            let _ = write_stream_frame(
                &mut output,
                &json!({"type":"error","instance_id":id,"error":message}),
            );
            return Ok(stream_failure(message.into()));
        }
        emitted = emitted.saturating_add(1);
        let started = Instant::now();
        while started.elapsed() < interval && !control.snapshot().cancel_requested {
            std::thread::sleep(
                Duration::from_millis(25).min(interval.saturating_sub(started.elapsed())),
            );
        }
    }
}

fn monitoring_frame(
    report: &InstanceMonitoring,
    entries: Vec<LogLine>,
    run_changed: bool,
) -> Result<Value, serde_json::Error> {
    let mut monitoring = serde_json::to_value(report)?;
    if let Some(observation) = monitoring
        .get_mut("observation")
        .and_then(Value::as_object_mut)
        && let Some(logs) = observation.get_mut("logs").and_then(Value::as_object_mut)
    {
        logs.insert("entries".into(), json!(entries));
    }
    Ok(json!({
        "type": "snapshot",
        "run_changed": run_changed,
        "monitoring": monitoring
    }))
}

fn write_stream_frame(output: &mut impl Write, frame: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, frame).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn stream_output_error(action: &str, emitted: usize, error: io::Error) -> CommandError {
    CommandError::operation(
        action,
        if emitted == 0 {
            format!("Cannot write monitoring stream: {error}")
        } else {
            format!("Monitoring stream ended after {emitted} frame(s): {error}")
        },
    )
}

fn stream_completion() -> CommandSuccess {
    CommandSuccess {
        action: "server.instance.monitor.follow".into(),
        message: String::new(),
        data: json!({"cancelled":true,"stream_complete":true}),
        exit_code: exit_codes::SUCCESS,
    }
}

fn stream_failure(message: String) -> CommandSuccess {
    CommandSuccess {
        action: "server.instance.monitor.follow".into(),
        message,
        data: json!({"stream_complete":false}),
        exit_code: exit_codes::OPERATION_FAILED,
    }
}

fn monitoring_success(action: &str, report: InstanceMonitoring) -> CommandSuccess {
    success(
        action,
        monitoring_summary(&report),
        json!({
            "monitoring": report,
            "database_opened": false,
            "writes_performed": false,
            "process_authority": false,
            "readiness_claimed": false
        }),
    )
}

fn monitoring_summary(report: &InstanceMonitoring) -> String {
    let observation_age = report
        .observation_age_ms
        .map_or_else(|| "unknown".into(), |age| format!("{age} ms"));
    let query_age = report
        .query_age_ms
        .map_or_else(|| "unknown".into(), |age| format!("{age} ms"));
    let query_state = report
        .observation
        .as_ref()
        .map(|observation| format!("{:?}", observation.query.state))
        .unwrap_or_else(|| "Unavailable".into());
    format!(
        "Health {:?}; process {}; query {query_state} (query-ready: {}, last success age: {query_age}); observation age: {observation_age}. This is process/query telemetry only; joinability, game FPS, scenario readiness, and mission state are unknown.",
        report.health,
        if report.status.process_exists {
            "present"
        } else {
            "absent"
        },
        report.query_ready
    )
}

fn validate_monitor_mode(
    follow: bool,
    stream_json: bool,
    json_output: bool,
    toon_output: bool,
) -> Result<(), &'static str> {
    if follow != stream_json {
        return Err("--follow and --stream-json must be used together");
    }
    if stream_json && (json_output || toon_output) {
        return Err("--stream-json cannot be combined with global --json or --toon");
    }
    Ok(())
}

fn select_log_lines(
    snapshot: &LogSnapshot,
    contains: Option<&str>,
    after_sequence: Option<u64>,
    limit: usize,
) -> Vec<LogLine> {
    let needle = contains.map(str::to_lowercase);
    let mut selected: Vec<_> = snapshot
        .entries
        .iter()
        .filter(|line| after_sequence.is_none_or(|sequence| line.sequence > sequence))
        .filter(|line| {
            needle
                .as_ref()
                .is_none_or(|needle| line.text.to_lowercase().contains(needle))
        })
        .cloned()
        .collect();
    let keep_from = selected.len().saturating_sub(limit);
    selected.drain(..keep_from);
    selected
}

fn new_log_lines(lines: &[LogLine], last_sequence: &mut Option<u64>) -> Vec<LogLine> {
    let mut selected: Vec<_> = lines
        .iter()
        .filter(|line| last_sequence.is_none_or(|sequence| line.sequence > sequence))
        .cloned()
        .collect();
    selected.sort_by_key(|line| line.sequence);
    selected.dedup_by_key(|line| line.sequence);
    if let Some(sequence) = selected.last().map(|line| line.sequence) {
        *last_sequence = Some(sequence);
    }
    selected
}

fn start(
    action: &str,
    store: &ServerStore,
    context: &ServerContext,
    args: ServerInstanceStartArgs,
    control: &ContentStageControl,
    dry_run: bool,
    yes: bool,
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    let plan = store
        .plan_instance_start(&id, control)
        .map_err(|error| store_error(action, error))?;
    let reviewed = match (
        args.expected_activation_generation,
        args.expected_plan_fingerprint.as_deref(),
    ) {
        (Some(generation), Some(fingerprint)) => {
            plan.require_review(generation, fingerprint)
                .map_err(|error| store_error(action, error))?;
            Some((generation, fingerprint.to_owned()))
        }
        (None, None) if dry_run => None,
        _ => {
            return Err(CommandError::validation(
                action,
                "Start requires both --expected-activation-generation and --expected-plan-fingerprint from start-plan",
            ));
        }
    };
    if dry_run {
        let mut preview = start_plan_preview(action, plan, reviewed.is_some());
        preview.data["detach"] = json!(args.detach);
        return Ok(preview);
    }
    if !yes {
        return Err(CommandError::validation(
            action,
            "Starting a dedicated server requires --yes to accept its owner lifetime",
        ));
    }
    let (expected_activation_generation, review_fingerprint) = reviewed.ok_or_else(|| {
        CommandError::validation(
            action,
            "Real Start requires the exact generation and fingerprint from start-plan",
        )
    })?;
    let detached = if args.detach {
        Some(owner::start_detached(context).map_err(|error| {
            CommandError::operation(
                action,
                format!("Cannot establish a detached owner: {error}"),
            )
        })?)
    } else {
        None
    };
    let attachment = attach_existing(context, action)?;
    if detached
        .as_ref()
        .is_some_and(|snapshot| &snapshot.scope != attachment.scope())
    {
        return Err(CommandError::operation(
            action,
            "Owner changed after detach; inspect status before retrying",
        ));
    }
    let outcome = attachment
        .run_instance(
            Operation::StartInstance {
                instance_id: id.as_str().into(),
                expected_activation_generation,
                review_fingerprint,
            },
            control,
        )
        .map_err(|error| lifecycle_error(action, "Start", error))?;
    completed_outcome(action, "Start", outcome)
}

fn stop(
    action: &str,
    context: &ServerContext,
    args: ServerInstanceStopArgs,
    control: &ContentStageControl,
    dry_run: bool,
    yes: bool,
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    validate_stop_inputs(action, args.expected_run_generation, args.timeout_ms)?;
    if dry_run {
        return Ok(intent_preview(
            action,
            "Stop",
            json!({
                "operation": "stop_instance",
                "instance_id": id,
                "expected_run_generation": args.expected_run_generation,
                "force": args.force,
                "timeout_ms": args.timeout_ms
            }),
        ));
    }
    if !yes {
        return Err(CommandError::validation(
            action,
            "Stopping a dedicated server requires --yes",
        ));
    }
    let attachment = attach_existing(context, action)?;
    let outcome = attachment
        .run_instance(
            Operation::StopInstance {
                instance_id: id.as_str().into(),
                expected_run_generation: args.expected_run_generation,
                force: args.force,
                timeout_ms: args.timeout_ms,
            },
            control,
        )
        .map_err(|error| lifecycle_error(action, "Stop", error))?;
    completed_outcome(action, "Stop", outcome)
}

fn restart(
    action: &str,
    store: &ServerStore,
    context: &ServerContext,
    args: ServerInstanceRestartArgs,
    control: &ContentStageControl,
    dry_run: bool,
    yes: bool,
) -> Result<CommandSuccess, CommandError> {
    let id = parse_id(args.id, action)?;
    validate_stop_inputs(action, args.expected_run_generation, args.timeout_ms)?;
    let plan = store
        .plan_instance_start(&id, control)
        .map_err(|error| store_error(action, error))?;
    plan.require_review(
        args.expected_activation_generation,
        &args.expected_plan_fingerprint,
    )
    .map_err(|error| store_error(action, error))?;
    if dry_run {
        return Ok(intent_preview(
            action,
            "Restart",
            json!({
                "operation": "restart_instance",
                "instance_id": id,
                "expected_run_generation": args.expected_run_generation,
                "expected_activation_generation": args.expected_activation_generation,
                "expected_plan_fingerprint": args.expected_plan_fingerprint,
                "force": args.force,
                "timeout_ms": args.timeout_ms,
                "start_plan": plan
            }),
        ));
    }
    if !yes {
        return Err(CommandError::validation(
            action,
            "Restarting a dedicated server requires --yes",
        ));
    }
    let attachment = attach_existing(context, action)?;
    let outcome = attachment
        .run_instance(
            Operation::RestartInstance {
                instance_id: id.as_str().into(),
                expected_run_generation: args.expected_run_generation,
                expected_activation_generation: args.expected_activation_generation,
                review_fingerprint: args.expected_plan_fingerprint,
                force: args.force,
                timeout_ms: args.timeout_ms,
            },
            control,
        )
        .map_err(|error| lifecycle_error(action, "Restart", error))?;
    completed_outcome(action, "Restart", outcome)
}

pub(super) fn attach_existing(
    context: &ServerContext,
    action: &str,
) -> Result<owner::Attachment, CommandError> {
    owner::attach(context)
        .map_err(|error| {
            CommandError::operation(action, format!("Cannot attach to foreground owner: {error}"))
        })?
        .ok_or_else(|| {
            CommandError::operation(
                action,
                "No foreground owner is available. Start it with server supervise --game-space <id>.",
            )
        })
}

fn start_plan_success(action: &str, plan: InstanceStartPlan) -> CommandSuccess {
    success(
        action,
        format!(
            "Reviewed start plan for {}; activation generation {}; runtime {}. No owner was contacted and no process was started. Starting accepts the foreground owner lifetime: closing its owner requests shutdown, waits five seconds, then force-cleans remaining trees; saves may be lost.",
            plan.instance_id.as_str(),
            plan.activation_generation,
            plan.runtime_id
        ),
        json!({
            "plan": plan,
            "database_opened": false,
            "writes_performed": false,
            "owner_contacted": false,
            "process_started": false,
            "requires_yes_for_foreground_lifetime": true
        }),
    )
}

fn start_plan_preview(action: &str, plan: InstanceStartPlan, review: bool) -> CommandSuccess {
    success(
        action,
        format!(
            "Start dry run for {}; activation generation {}; plan fingerprint {}. No owner was contacted and no process was started.",
            plan.instance_id.as_str(),
            plan.activation_generation,
            plan.fingerprint
        ),
        json!({
            "plan": plan,
            "review_arguments_supplied": review,
            "dry_run": true,
            "database_opened": false,
            "writes_performed": false,
            "owner_contacted": false,
            "process_started": false
        }),
    )
}

fn intent_preview(action: &str, operation: &str, intent: serde_json::Value) -> CommandSuccess {
    success(
        action,
        format!(
            "{operation} dry run validated the requested intent. No owner was contacted and no lifecycle request was sent."
        ),
        json!({
            "intent": intent,
            "dry_run": true,
            "database_opened": false,
            "writes_performed": false,
            "owner_contacted": false,
            "request_sent": false
        }),
    )
}

fn validate_stop_inputs(
    action: &str,
    expected_run_generation: u64,
    timeout_ms: u32,
) -> Result<(), CommandError> {
    if expected_run_generation == 0 || !(1..=120_000).contains(&timeout_ms) {
        return Err(CommandError::validation(
            action,
            "Run generation must be positive and timeout must be between 1 and 120000 ms",
        ));
    }
    Ok(())
}

pub(super) fn completed_outcome(
    action: &str,
    operation: &str,
    outcome: RuntimeOperationOutcome,
) -> Result<CommandSuccess, CommandError> {
    if !outcome.completed {
        let message = outcome.message.clone().unwrap_or_else(|| {
            if outcome.cancelled {
                format!("{operation} was cancelled before completion")
            } else {
                format!("{operation} did not complete before its wait ended")
            }
        });
        return Err(CommandError {
            action: action.into(),
            message: format!(
                "{message} Inspect server instance status before retrying. Current phase: {:?}, run generation {}. Cancellation does not imply the process stopped or its runtime pointer stayed unchanged.",
                outcome.status.phase, outcome.status.run_generation
            ),
            code: exit_codes::OPERATION_FAILED,
        });
    }
    let force_note = if outcome.status.forced {
        "Forced termination was confirmed; saves may be lost."
    } else {
        ""
    };
    Ok(success(
        action,
        format!(
            "{operation} completed for {}; phase {:?}; run generation {}. The status is observational and does not establish engine readiness. {} {force_note}",
            outcome.status.instance_id.as_str(),
            outcome.status.phase,
            outcome.status.run_generation,
            outcome.message.as_deref().unwrap_or_default()
        ),
        json!({
            "outcome": outcome,
            "database_opened": false,
            "process_authority": false,
            "readiness_claimed": false
        }),
    ))
}

pub(super) fn lifecycle_error(
    action: &str,
    operation: &str,
    error: std::io::Error,
) -> CommandError {
    CommandError::operation(
        action,
        format!(
            "{operation} was not confirmed: {error}. Inspect server instance status before retrying."
        ),
    )
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(sequence: u64, text: &str) -> LogLine {
        LogLine {
            sequence,
            stream: "engine".into(),
            text: text.into(),
        }
    }

    #[test]
    fn monitor_requires_an_unambiguous_stream_mode() {
        assert!(validate_monitor_mode(false, false, false, false).is_ok());
        assert!(validate_monitor_mode(true, true, false, false).is_ok());
        assert!(validate_monitor_mode(true, false, false, false).is_err());
        assert!(validate_monitor_mode(false, true, false, false).is_err());
        assert!(validate_monitor_mode(true, true, true, false).is_err());
        assert!(validate_monitor_mode(true, true, false, true).is_err());
    }

    #[test]
    fn log_search_returns_recent_matching_lines_after_the_sequence() {
        let snapshot = LogSnapshot {
            entries: vec![
                line(1, "server started"),
                line(2, "Warning: first"),
                line(3, "connection accepted"),
                line(4, "warning: second"),
            ],
            next_sequence: 5,
            truncated: false,
            error: None,
        };
        let selected = select_log_lines(&snapshot, Some("WARNING"), Some(1), 1);
        assert_eq!(selected, vec![line(4, "warning: second")]);
    }

    #[test]
    fn follow_frames_emit_only_new_sequences_and_frame_each_json_value() {
        let first = vec![line(1, "first"), line(2, "second")];
        let mut last_sequence = None;
        assert_eq!(new_log_lines(&first, &mut last_sequence), first);
        assert_eq!(
            new_log_lines(
                &[line(1, "first"), line(2, "second"), line(3, "third")],
                &mut last_sequence
            ),
            vec![line(3, "third")]
        );

        let frame =
            json!({"type":"snapshot","run_changed":false,"monitoring":{"health":"healthy"}});
        let mut bytes = Vec::new();
        write_stream_frame(&mut bytes, &frame).unwrap();
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), frame);
    }
}
