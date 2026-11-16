use foxy_supervisor::control::Operation;
use serde_json::json;

use super::content_stage::controlled;
use super::instances::{capture, save_change, store_error, success};
use super::lifecycle::{attach_existing, completed_outcome, lifecycle_error};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerHeadlessCommand, ServerHeadlessDefinitionArgs,
};
use crate::core::server::{
    Arma3HeadlessClient, Arma3ServerDefinition, DraftChange, HeadlessAddonSelection,
    HeadlessClientState, HeadlessClientStatus, HeadlessConnection, HeadlessLifecyclePolicy,
    HeadlessProcessOverrides, HeadlessReason, InstanceMonitoring, ServerDefinition, ServerInstance,
    ServerInstanceId, ServerStore, new_headless_id, ordered_headless_clients, valid_headless_id,
};

pub(super) fn run(
    cli: &CliArgs,
    args: ServerContextArgs,
    command: ServerHeadlessCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match &command {
        ServerHeadlessCommand::List(_) => "server.headless.list",
        ServerHeadlessCommand::Add(_) => "server.headless.add",
        ServerHeadlessCommand::Set(_) => "server.headless.set",
        ServerHeadlessCommand::Remove(_) => "server.headless.remove",
        ServerHeadlessCommand::Plan(_) => "server.headless.plan",
        ServerHeadlessCommand::Status(_) => "server.headless.status",
        ServerHeadlessCommand::Start(_) => "server.headless.start",
        ServerHeadlessCommand::Stop(_) => "server.headless.stop",
        ServerHeadlessCommand::Restart(_) => "server.headless.restart",
    };
    let context = capture(args, action)?;
    if !crate::core::game::registry()
        .get(context.game_id())
        .is_some_and(|game| game.server_capabilities().headless_clients)
    {
        return Err(CommandError::validation(
            action,
            "Headless clients are available for Arma 3 server spaces on Windows and Linux x64.",
        ));
    }
    let store = ServerStore::new(context.clone()).map_err(|error| store_error(action, error))?;
    match command {
        ServerHeadlessCommand::List(args) => {
            let instance = store
                .show(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            list(action, &instance)
        }
        ServerHeadlessCommand::Add(args) => {
            let mut instance = store
                .show(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            arma_mut(&mut instance, action)?
                .headless_clients
                .push(definition(new_headless_id(), args.definition));
            replace(
                cli,
                &store,
                &context,
                instance,
                args.expected_revision,
                action,
            )
        }
        ServerHeadlessCommand::Set(args) => {
            let mut instance = store
                .show(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            let clients = &mut arma_mut(&mut instance, action)?.headless_clients;
            let slot = clients
                .iter_mut()
                .find(|client| client.id == args.client_id)
                .ok_or_else(|| missing(action, &args.client_id))?;
            *slot = definition(args.client_id, args.definition);
            replace(
                cli,
                &store,
                &context,
                instance,
                args.expected_revision,
                action,
            )
        }
        ServerHeadlessCommand::Remove(args) => {
            if !cli.yes && !cli.dry_run {
                return Err(CommandError::validation(
                    action,
                    "Removing a headless client definition requires --yes",
                ));
            }
            let mut instance = store
                .show(&parse_id(args.id, action)?)
                .map_err(|error| store_error(action, error))?;
            let clients = &mut arma_mut(&mut instance, action)?.headless_clients;
            let before = clients.len();
            clients.retain(|client| client.id != args.client_id);
            if clients.len() == before {
                return Err(missing(action, &args.client_id));
            }
            replace(
                cli,
                &store,
                &context,
                instance,
                args.expected_revision,
                action,
            )
        }
        ServerHeadlessCommand::Plan(args) => {
            let id = parse_id(args.id, action)?;
            controlled(cli, action, move |control| {
                let store = ServerStore::new(context.clone())
                    .map_err(|error| store_error(action, error))?;
                let plan = store
                    .plan_instance_start(&id, &control)
                    .map_err(|error| store_error(action, error))?;
                let mut lines = vec![format!(
                    "Committed headless launch plans for {}: {} client(s), activation generation {}, start plan {}. Nothing was started.",
                    plan.instance_id.as_str(),
                    plan.launch.headless.len(),
                    plan.activation_generation,
                    plan.fingerprint
                )];
                for client in &plan.launch.headless {
                    lines.push(format!(
                        "{} ({}): {}{}",
                        client.name,
                        client.id,
                        client.arguments.join(" "),
                        if client.join_password {
                            " [join password from a private parameter file]"
                        } else {
                            ""
                        }
                    ));
                }
                lines.push("Headless clients start, automatically or explicitly, only while the server answers its query port (within the last 15 seconds). Mission slots and AI offload are not verified.".into());
                Ok(success(
                    action,
                    lines.join("\n"),
                    json!({
                        "instance_id": plan.instance_id,
                        "activation_generation": plan.activation_generation,
                        "start_plan_fingerprint": plan.fingerprint,
                        "headless_clients": plan.launch.headless,
                        "readiness_gate": "query_responding_within_15s",
                        "database_opened": false,
                        "owner_contacted": false,
                        "process_started": false
                    }),
                ))
            })
        }
        ServerHeadlessCommand::Status(args) => {
            let id = parse_id(args.id, action)?;
            let attachment = attach_existing(&context, action)?;
            let report = attachment.instance_monitoring(&store, &id).map_err(|_| {
                CommandError::operation(
                    action,
                    "Monitoring is unavailable; refresh the owner status and retry.",
                )
            })?;
            Ok(status_success(action, report))
        }
        ServerHeadlessCommand::Start(args) => {
            let target = target(args.id, args.expected_run_generation, args.client, action)?;
            let operation = Operation::StartHeadlessClients {
                instance_id: target.id.as_str().into(),
                expected_run_generation: target.expected_run_generation,
                headless_id: target.client.clone(),
            };
            owner_operation(cli, action, context, target, operation, "Headless start")
        }
        ServerHeadlessCommand::Stop(args) => {
            let target = target(args.id, args.expected_run_generation, args.client, action)?;
            let operation = Operation::StopHeadlessClients {
                instance_id: target.id.as_str().into(),
                expected_run_generation: target.expected_run_generation,
                headless_id: target.client.clone(),
                force: args.force,
                timeout_ms: args.timeout_ms,
            };
            owner_operation(cli, action, context, target, operation, "Headless stop")
        }
        ServerHeadlessCommand::Restart(args) => {
            let target = target(args.id, args.expected_run_generation, args.client, action)?;
            let operation = Operation::RestartHeadlessClients {
                instance_id: target.id.as_str().into(),
                expected_run_generation: target.expected_run_generation,
                headless_id: target.client.clone(),
                timeout_ms: args.timeout_ms,
            };
            owner_operation(cli, action, context, target, operation, "Headless restart")
        }
    }
}

struct Target {
    id: ServerInstanceId,
    expected_run_generation: u64,
    client: Option<String>,
}

fn target(
    id: String,
    expected_run_generation: u64,
    client: Option<String>,
    action: &str,
) -> Result<Target, CommandError> {
    let id = parse_id(id, action)?;
    if expected_run_generation == 0 {
        return Err(CommandError::validation(
            action,
            "--expected-run-generation must be the current nonzero run generation",
        ));
    }
    if client
        .as_deref()
        .is_some_and(|client| !valid_headless_id(client))
    {
        return Err(CommandError::validation(
            action,
            "--client must be a headless client id such as hc_0123456789abcdef",
        ));
    }
    Ok(Target {
        id,
        expected_run_generation,
        client,
    })
}

fn owner_operation(
    cli: &CliArgs,
    action: &'static str,
    context: crate::core::server::ServerContext,
    target: Target,
    operation: Operation,
    label: &'static str,
) -> Result<CommandSuccess, CommandError> {
    if cli.dry_run {
        let mut data = serde_json::to_value(&operation).unwrap_or_else(|_| json!({}));
        data["dry_run"] = json!(true);
        data["owner_contacted"] = json!(false);
        data["process_started"] = json!(false);
        return Ok(success(
            action,
            format!(
                "{label} dry run for {} run generation {}{}. No owner was contacted and no process was changed.",
                target.id.as_str(),
                target.expected_run_generation,
                target
                    .client
                    .as_deref()
                    .map(|client| format!(", client {client}"))
                    .unwrap_or_default()
            ),
            data,
        ));
    }
    if !cli.yes {
        return Err(CommandError::validation(
            action,
            format!(
                "{label} requires --yes to change headless client processes of the running server"
            ),
        ));
    }
    controlled(cli, action, move |control| {
        let attachment = attach_existing(&context, action)?;
        let outcome = attachment
            .run_instance(operation, &control)
            .map_err(|error| lifecycle_error(action, label, error))?;
        completed_outcome(action, label, outcome)
    })
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

fn status_success(action: &str, report: InstanceMonitoring) -> CommandSuccess {
    let headless = report
        .observation
        .as_ref()
        .and_then(|observation| observation.headless.clone());
    let now = unix_ms();
    let mut lines = vec![match &headless {
        Some(group) => format!(
            "Headless clients of {} run generation {}: {} defined, {} process(es) running{}.",
            report.status.instance_id.as_str(),
            report.status.run_generation,
            group.clients.len(),
            report.status.headless_running,
            group.ready_unix_ms.map_or_else(
                || ", server not query-ready yet".to_owned(),
                |time| format!(
                    ", server first query-ready {} s ago",
                    now.saturating_sub(time) / 1000
                )
            )
        ),
        None => format!(
            "No headless client status for {}: the server run is not running with committed headless clients, or its observation is unavailable.",
            report.status.instance_id.as_str()
        ),
    }];
    for client in headless.iter().flat_map(|group| &group.clients) {
        lines.push(client_line(client, now));
    }
    lines.push("Joined means the client's own log reports a started mission. A later disconnect of a live process is not detected; mission slots and AI offload are not verified.".into());
    success(
        action,
        lines.join("\n"),
        json!({
            "instance_id": report.status.instance_id,
            "run_generation": report.status.run_generation,
            "phase": report.status.phase,
            "headless_running": report.status.headless_running,
            "query_ready": report.query_ready,
            "ready_unix_ms": headless.as_ref().and_then(|group| group.ready_unix_ms),
            "clients": headless.as_ref().map(|group| group.clients.clone()).unwrap_or_default(),
            "observation_unavailable": report.observation_unavailable,
            "owner_contacted": true,
            "process_started": false
        }),
    )
}

fn client_line(client: &HeadlessClientStatus, now: u64) -> String {
    let mut parts = vec![format!(
        "{} ({}): {}",
        client.name,
        client.id,
        state_label(client.state)
    )];
    if let Some(reason) = client.reason {
        parts.push(reason_label(reason).into());
    }
    if let Some(connection) = client.connection {
        parts.push(match connection {
            HeadlessConnection::Pending => "join pending".to_owned(),
            HeadlessConnection::Joined => format!(
                "joined {} s ago",
                client
                    .joined_unix_ms
                    .map_or(0, |time| now.saturating_sub(time) / 1000)
            ),
            HeadlessConnection::NoEvidence => "no join evidence within the join timeout".to_owned(),
            HeadlessConnection::Unreadable => "join evidence unreadable".to_owned(),
        });
    }
    parts.push(format!(
        "{} start(s), {} automatic restart(s) in the last 10 minutes",
        client.starts, client.automatic_restarts
    ));
    if let Some(next) = client.next_start_unix_ms {
        parts.push(format!(
            "next start in {} s",
            next.saturating_sub(now).div_ceil(1000)
        ));
    }
    if let Some(exit) = &client.last_exit {
        parts.push(format!(
            "last exit {}{}",
            exit.exit_code.map_or_else(
                || "without a code".to_owned(),
                |code| format!("code {code}")
            ),
            if exit.requested { " (requested)" } else { "" }
        ));
    }
    parts.join("; ")
}

fn state_label(state: HeadlessClientState) -> &'static str {
    match state {
        HeadlessClientState::WaitingForServer => "waiting for the server to answer its query port",
        HeadlessClientState::Scheduled => "start scheduled",
        HeadlessClientState::Running => "running",
        HeadlessClientState::Stopped => "stopped",
        HeadlessClientState::RestartSuspended => "automatic restarts suspended",
    }
}

fn reason_label(reason: HeadlessReason) -> &'static str {
    match reason {
        HeadlessReason::ManualStartRequired => "starts only when started explicitly",
        HeadlessReason::StoppedByOperator => "stopped by an operator",
        HeadlessReason::RestartDisabled => "exited and automatic restart is off",
        HeadlessReason::RestartBudgetExhausted => "exited too often; start it explicitly to retry",
        HeadlessReason::StartFailed => {
            "start failed; inspect installation consumers, then start it explicitly"
        }
        HeadlessReason::ServerNotRunning => "the server run is not running",
        HeadlessReason::PausedByServerStop => {
            "automatic starts paused because a server stop did not finish; start it explicitly or stop the server"
        }
    }
}

fn definition(id: String, args: ServerHeadlessDefinitionArgs) -> Arma3HeadlessClient {
    Arma3HeadlessClient {
        id,
        name: args.name,
        order: args.order,
        enabled: !args.disabled,
        addons: HeadlessAddonSelection {
            inherit_required: !args.no_inherit_required,
            include: args.include,
            exclude: args.exclude,
        },
        process: HeadlessProcessOverrides {
            cpu_count: args.cpu_count,
            ex_threads: args.ex_threads,
            max_mem: args.max_mem,
            limit_fps: args.limit_fps,
        },
        lifecycle: HeadlessLifecyclePolicy {
            auto_start: !args.manual_start,
            start_delay_seconds: args.start_delay,
            connect_timeout_seconds: args.join_timeout,
            max_restarts: args.max_restarts,
        },
    }
}

fn arma_mut<'a>(
    instance: &'a mut ServerInstance,
    action: &str,
) -> Result<&'a mut Arma3ServerDefinition, CommandError> {
    match &mut instance.definition {
        ServerDefinition::Arma3(settings) => Ok(settings),
        ServerDefinition::Reforger(_) => Err(CommandError::validation(
            action,
            "Headless clients are Arma 3 only",
        )),
    }
}

fn replace(
    cli: &CliArgs,
    store: &ServerStore,
    context: &crate::core::server::ServerContext,
    instance: ServerInstance,
    expected_revision: u64,
    action: &str,
) -> Result<CommandSuccess, CommandError> {
    save_change(
        cli,
        store,
        context,
        DraftChange::Replace {
            instance,
            expected_revision,
        },
        action,
    )
}

fn list(action: &str, instance: &ServerInstance) -> Result<CommandSuccess, CommandError> {
    let ServerDefinition::Arma3(settings) = &instance.definition else {
        return Err(CommandError::validation(
            action,
            "Headless clients are Arma 3 only",
        ));
    };
    let ordered = ordered_headless_clients(&settings.headless_clients);
    let mut lines = vec![format!(
        "{} headless client definition(s) for {} (draft revision {}). Changes apply after a configuration commit, deployment and activation.",
        ordered.len(),
        instance.display_name,
        instance.revision
    )];
    for client in &ordered {
        let mut addons = vec![if client.addons.inherit_required {
            "inherits required addons".to_owned()
        } else {
            "no inherited addons".to_owned()
        }];
        if !client.addons.include.is_empty() {
            addons.push(format!("adds {}", client.addons.include.join(", ")));
        }
        if !client.addons.exclude.is_empty() {
            addons.push(format!("skips {}", client.addons.exclude.join(", ")));
        }
        let overrides = client.process.arguments();
        let policy = &client.lifecycle;
        lines.push(format!(
            "{}. {} ({}){}: {}{}; {}, join timeout {} s, {}",
            client.order,
            client.name,
            client.id,
            if client.enabled { "" } else { " disabled" },
            addons.join("; "),
            if overrides.is_empty() {
                String::new()
            } else {
                format!("; {}", overrides.join(" "))
            },
            if !policy.auto_start {
                "manual start".to_owned()
            } else if policy.start_delay_seconds == 0 {
                "starts when the server is query-ready".to_owned()
            } else {
                format!(
                    "starts {} s after the server is query-ready",
                    policy.start_delay_seconds
                )
            },
            policy.connect_timeout_seconds,
            if policy.max_restarts == 0 {
                "no automatic restart".to_owned()
            } else {
                format!(
                    "up to {} automatic restarts in 10 minutes",
                    policy.max_restarts
                )
            }
        ));
    }
    Ok(success(
        action,
        lines.join("\n"),
        json!({
            "instance_id": instance.id,
            "instance_revision": instance.revision,
            "headless_clients": ordered,
            "server_admission": settings.server.headless_clients.clone().map_or_else(
                || json!("derived_loopback"),
                |addresses| json!(addresses)
            ),
            "database_opened": false,
            "writes_performed": false
        }),
    ))
}

fn missing(action: &str, client_id: &str) -> CommandError {
    CommandError::not_found(
        action,
        format!("No headless client {client_id} exists in this draft"),
    )
}

fn parse_id(id: String, action: &str) -> Result<ServerInstanceId, CommandError> {
    ServerInstanceId::try_from(id).map_err(|message| CommandError::validation(action, message))
}
