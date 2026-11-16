use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::{CommandSpec, ConsoleSignalHelper, TreeExit};
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};

use super::Runs;
use crate::core::server::headless::{
    ClientSchedule, Due, ExitDecision, HeadlessClientState, HeadlessClientStatus,
    HeadlessConnection, HeadlessExit, HeadlessGroupStatus, HeadlessReadiness, HeadlessReason,
    JoinEvidence, SharedHeadlessStatus, consumer_instance_id, prepare_headless_state,
};
use crate::core::server::monitoring::unix_ms;
use crate::core::server::{
    ContentStageControl, HeadlessLaunchPlan, InstanceRuntimePhase, RuntimeOperationOutcome,
    ServerInstallation, ServerInstanceId, ServerStoreError, owner::DeploymentExecution,
};

const EVIDENCE_POLL: Duration = Duration::from_secs(1);
const ORPHAN_RETRY: Duration = Duration::from_secs(30);

pub(super) struct StartIntent {
    hold: bool,
    schedules: Vec<(usize, ClientSchedule, Option<u64>)>,
}

struct Client {
    plan: HeadlessLaunchPlan,
    schedule: ClientSchedule,
    session: Option<ForegroundSession>,
    stop_requested: bool,
    process_id: Option<u32>,
    started: Option<(Instant, u64)>,
    evidence: Option<JoinEvidence>,
    connection: Option<HeadlessConnection>,
    joined_unix_ms: Option<u64>,
    starts: u32,
    last_exit: Option<HeadlessExit>,
    next_start_unix_ms: Option<u64>,
}

impl Client {
    fn exited(&mut self, exit: TreeExit, server_running: bool, now: Instant) {
        self.session = None;
        self.process_id = None;
        self.evidence = None;
        self.connection = None;
        let requested = std::mem::take(&mut self.stop_requested);
        self.last_exit = Some(HeadlessExit {
            unix_ms: unix_ms(),
            exit_code: exit.root_exit_code,
            forced: exit.forced,
            requested,
        });
        if !server_running {
            return;
        }
        match self.schedule.exited(requested, now) {
            ExitDecision::Requested => {}
            ExitDecision::Restart(delay) => {
                self.next_start_unix_ms = Some(
                    unix_ms().saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)),
                );
                log::info!(
                    "Headless client {} exited unexpectedly; automatic restart in {} s",
                    self.plan.id,
                    delay.as_secs()
                );
            }
            ExitDecision::RestartDisabled => log::info!(
                "Headless client {} exited unexpectedly; automatic restart is disabled",
                self.plan.id
            ),
            ExitDecision::Suspended => log::warn!(
                "Headless client {} exited again; automatic restarts are suspended until started explicitly",
                self.plan.id
            ),
        }
    }
}

#[derive(Default)]
pub(super) struct HeadlessGroup {
    clients: Vec<Client>,
    ready_since: Option<(Instant, u64)>,
    server_ended: bool,
    hold: bool,
    shared: SharedHeadlessStatus,
    next_evidence_poll: Option<Instant>,
    orphan_retry: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GroupStop {
    Completed,
    TimedOut,
    Cancelled,
}

impl HeadlessGroup {
    pub(super) fn new(
        plans: Vec<HeadlessLaunchPlan>,
        carried: Option<&BTreeMap<String, bool>>,
        shared: SharedHeadlessStatus,
    ) -> Self {
        let clients = plans
            .into_iter()
            .map(|plan| Client {
                schedule: ClientSchedule::new(
                    plan.lifecycle.clone(),
                    carried.and_then(|wanted| wanted.get(&plan.id).copied()),
                ),
                plan,
                session: None,
                stop_requested: false,
                process_id: None,
                started: None,
                evidence: None,
                connection: None,
                joined_unix_ms: None,
                starts: 0,
                last_exit: None,
                next_start_unix_ms: None,
            })
            .collect();
        let mut group = Self {
            clients,
            shared,
            ..Self::default()
        };
        group.publish(false);
        group
    }

    pub(super) fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub(super) fn running(&self) -> u32 {
        u32::try_from(
            self.clients
                .iter()
                .filter(|client| client.session.is_some())
                .count(),
        )
        .unwrap_or(u32::MAX)
    }

    /// Operator stops survive a server restart; other clients follow their wanted state.
    pub(super) fn carried(&self) -> BTreeMap<String, bool> {
        self.clients
            .iter()
            .filter_map(|client| {
                if client.schedule.reason() == Some(HeadlessReason::StoppedByOperator) {
                    Some((client.plan.id.clone(), false))
                } else {
                    client
                        .schedule
                        .wanted()
                        .then(|| (client.plan.id.clone(), true))
                }
            })
            .collect()
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        self.clients.iter().any(|client| client.plan.id == id)
    }

    fn targets(&self, target: Option<&str>) -> Vec<usize> {
        self.clients
            .iter()
            .enumerate()
            .filter(|(_, client)| target.is_none_or(|id| client.plan.id == id))
            .map(|(index, _)| index)
            .collect()
    }

    pub(super) fn hold(&mut self) {
        self.hold = true;
    }

    pub(super) fn server_ended(&mut self) {
        self.server_ended = true;
    }

    pub(super) fn observe(&mut self) {
        let now = Instant::now();
        let server_running = !self.server_ended && !self.hold;
        for client in &mut self.clients {
            let exit = client
                .session
                .as_mut()
                .and_then(|session| session.observe().ok().flatten());
            if let Some(exit) = exit {
                client.exited(exit, server_running, now);
            }
        }
        if self.next_evidence_poll.is_some_and(|next| now < next) {
            return;
        }
        self.next_evidence_poll = Some(now + EVIDENCE_POLL);
        for client in &mut self.clients {
            let (Some(evidence), Some((started, _))) = (&mut client.evidence, client.started)
            else {
                continue;
            };
            if client.joined_unix_ms.is_some() {
                continue;
            }
            if evidence.poll() {
                client.connection = Some(HeadlessConnection::Joined);
                client.joined_unix_ms = Some(unix_ms());
            } else if now.saturating_duration_since(started) >= client.schedule.connect_timeout() {
                client.connection = Some(HeadlessConnection::NoEvidence);
            }
        }
    }

    pub(super) fn due(&mut self, ready: bool) -> Vec<usize> {
        if self.server_ended || self.hold {
            return Vec::new();
        }
        let now = Instant::now();
        if ready && self.ready_since.is_none() {
            self.ready_since = Some((now, unix_ms()));
        }
        let since = self.ready_since.map(|(time, _)| time);
        self.clients
            .iter()
            .enumerate()
            .filter(|(_, client)| {
                client
                    .schedule
                    .due(client.session.is_some(), since, ready, now)
                    == Due::Now
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub(super) fn operator_start(
        &mut self,
        target: Option<&str>,
        ready: bool,
    ) -> (Vec<usize>, StartIntent) {
        let now = Instant::now();
        if ready && self.ready_since.is_none() {
            self.ready_since = Some((now, unix_ms()));
        }
        let targets = self.targets(target);
        let intent = StartIntent {
            hold: std::mem::take(&mut self.hold),
            schedules: targets
                .iter()
                .map(|index| {
                    let client = &self.clients[*index];
                    (*index, client.schedule.clone(), client.next_start_unix_ms)
                })
                .collect(),
        };
        for index in &targets {
            self.clients[*index].next_start_unix_ms = None;
            self.clients[*index].schedule.operator_start();
        }
        let pending = targets
            .into_iter()
            .filter(|index| self.clients[*index].session.is_none())
            .collect();
        (pending, intent)
    }

    /// Returns clients an interrupted explicit start did not launch to their earlier intent, so
    /// the owner loop does not finish a start the operator cancelled or that failed.
    pub(super) fn abandon_start(&mut self, intent: StartIntent, failed: Option<usize>) {
        self.hold = intent.hold;
        for (index, schedule, next_start) in intent.schedules {
            let client = &mut self.clients[index];
            if client.session.is_some() || Some(index) == failed {
                continue;
            }
            client.schedule = schedule;
            client.next_start_unix_ms = next_start;
        }
    }

    pub(super) fn orphan_cleanup_due(&self) -> bool {
        self.orphan_retry
            .is_none_or(|retry| Instant::now() >= retry)
    }

    pub(super) fn orphan_cleanup_result(&mut self, clean: bool) {
        self.orphan_retry = (!clean).then(|| Instant::now() + ORPHAN_RETRY);
    }

    pub(super) fn operator_stop(&mut self, target: Option<&str>) {
        for index in self.targets(target) {
            self.clients[index].next_start_unix_ms = None;
            self.clients[index].schedule.operator_stop();
        }
    }

    pub(super) fn plan(&self, index: usize) -> &HeadlessLaunchPlan {
        &self.clients[index].plan
    }

    pub(super) fn starts(&self, index: usize) -> u32 {
        self.clients[index].starts
    }

    pub(super) fn launched(
        &mut self,
        index: usize,
        session: ForegroundSession,
        evidence: JoinEvidence,
        started_unix_ms: u64,
    ) {
        let client = &mut self.clients[index];
        client.process_id = Some(session.process_id());
        client.session = Some(session);
        client.stop_requested = false;
        client.started = Some((Instant::now(), started_unix_ms));
        client.evidence = Some(evidence);
        client.connection = Some(HeadlessConnection::Pending);
        client.joined_unix_ms = None;
        client.starts = client.starts.saturating_add(1);
        client.next_start_unix_ms = None;
        client.schedule.started();
        self.next_evidence_poll = None;
    }

    pub(super) fn launch_failed(&mut self, index: usize) {
        self.clients[index].schedule.start_failed();
    }

    fn deliver_soft_stop(&mut self, helper: &ConsoleSignalHelper, targets: &[usize]) {
        for index in targets {
            let client = &mut self.clients[*index];
            if let Some(session) = &mut client.session {
                client.stop_requested = true;
                let _ = session.deliver_soft_stop(Some(helper));
            }
        }
    }

    pub(super) fn deliver_soft_stop_all(&mut self, helper: &ConsoleSignalHelper) {
        let targets = self.targets(None);
        self.deliver_soft_stop(helper, &targets);
    }

    fn running_targets(&self, targets: &[usize]) -> bool {
        targets
            .iter()
            .any(|index| self.clients[*index].session.is_some())
    }

    pub(super) fn stop(
        &mut self,
        helper: &ConsoleSignalHelper,
        deadline: Instant,
        control: Option<&ContentStageControl>,
        target: Option<&str>,
    ) -> GroupStop {
        self.observe();
        let targets = self.targets(target);
        if !self.running_targets(&targets) {
            return GroupStop::Completed;
        }
        self.deliver_soft_stop(helper, &targets);
        let mut next_delivery = Instant::now() + Duration::from_millis(500);
        loop {
            self.observe();
            if !self.running_targets(&targets) {
                return GroupStop::Completed;
            }
            if control.is_some_and(|control| control.snapshot().cancel_requested) {
                return GroupStop::Cancelled;
            }
            if Instant::now() >= deadline {
                return GroupStop::TimedOut;
            }
            if Instant::now() >= next_delivery {
                self.deliver_soft_stop(helper, &targets);
                next_delivery = Instant::now() + Duration::from_millis(500);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub(super) fn force(&mut self, target: Option<&str>) -> bool {
        let server_running = !self.server_ended && !self.hold;
        let now = Instant::now();
        let targets = self.targets(target);
        for index in &targets {
            let client = &mut self.clients[*index];
            let Some(session) = &mut client.session else {
                continue;
            };
            client.stop_requested = true;
            if let Ok(Some(exit)) = session.force_stop(Duration::from_secs(5)) {
                client.exited(exit, server_running, now);
            }
        }
        !self.running_targets(&targets)
    }

    pub(super) fn stop_with_owner_policy(&mut self, helper: &ConsoleSignalHelper) -> bool {
        if self.stop(helper, Instant::now() + Duration::from_secs(5), None, None)
            == GroupStop::Completed
        {
            return true;
        }
        self.force(None)
    }

    pub(super) fn status(&mut self, ready: bool) -> HeadlessGroupStatus {
        let now = Instant::now();
        let server_running = !self.server_ended;
        let since = self.ready_since.map(|(time, _)| time);
        let ready_wall = self.ready_since.map(|(_, wall)| wall);
        let hold = self.hold;
        let clients = self
            .clients
            .iter_mut()
            .map(|client| {
                let running = client.session.is_some();
                let (state, reason) = if hold && server_running && !running {
                    (
                        HeadlessClientState::Stopped,
                        Some(HeadlessReason::PausedByServerStop),
                    )
                } else {
                    client.schedule.state(running, ready, server_running)
                };
                let next_start_unix_ms = match client.schedule.due(running, since, ready, now) {
                    Due::At(_) if server_running && !hold => {
                        client.next_start_unix_ms.or_else(|| {
                            ready_wall.map(|wall| {
                                wall.saturating_add(
                                    u64::from(client.plan.lifecycle.start_delay_seconds) * 1000,
                                )
                            })
                        })
                    }
                    _ => None,
                };
                HeadlessClientStatus {
                    id: client.plan.id.clone(),
                    name: client.plan.name.clone(),
                    state,
                    wanted: client.schedule.wanted(),
                    reason,
                    process_id: client.process_id,
                    started_unix_ms: client.started.map(|(_, wall)| wall),
                    connection: client.connection,
                    joined_unix_ms: client.joined_unix_ms,
                    starts: client.starts,
                    automatic_restarts: client.schedule.automatic_restarts(now),
                    next_start_unix_ms,
                    last_exit: client.last_exit.clone(),
                }
            })
            .collect();
        HeadlessGroupStatus {
            ready_unix_ms: self.ready_since.map(|(_, wall)| wall),
            clients,
        }
    }

    pub(super) fn wait_written(&self, timeout: Duration) -> bool {
        self.shared.wait_written(timeout)
    }

    pub(super) fn publish(&mut self, ready: bool) {
        if self.clients.is_empty() {
            return;
        }
        let status = self.status(ready);
        self.shared.publish(status);
    }
}

fn output_file(path: &Path) -> Result<File, ServerStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(|_| ServerStoreError::Io)
}

struct Spawned {
    session: ForegroundSession,
    evidence: JoinEvidence,
    started_unix_ms: u64,
}

impl Runs {
    fn readiness(&self, id: &ServerInstanceId) -> HeadlessReadiness {
        self.runs
            .get(id.as_str())
            .and_then(|run| run.monitoring.as_ref())
            .map_or(HeadlessReadiness::NoQueryEndpoint, |observer| {
                observer.headless_readiness()
            })
    }

    fn running_run(
        &self,
        id: &ServerInstanceId,
        expected_run_generation: u64,
        target: Option<&str>,
    ) -> Result<(), ServerStoreError> {
        let run = self.runs.get(id.as_str()).ok_or_else(|| {
            ServerStoreError::Invalid(
                "This owner has no retained session for that instance.".into(),
            )
        })?;
        if expected_run_generation == 0 || run.status.run_generation != expected_run_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        if run.session.is_none() || run.status.phase != InstanceRuntimePhase::Running {
            return Err(ServerStoreError::Invalid(
                "Headless clients are managed only while their server run is running.".into(),
            ));
        }
        if run.headless.is_empty() {
            return Err(ServerStoreError::Invalid(
                "The committed runtime of this run defines no enabled headless clients.".into(),
            ));
        }
        if target.is_some_and(|target| !run.headless.contains(target)) {
            return Err(ServerStoreError::Invalid(
                "This run has no headless client with that id.".into(),
            ));
        }
        Ok(())
    }

    fn refresh_headless(&mut self, id: &ServerInstanceId) {
        let ready = self.readiness(id) == HeadlessReadiness::Ready;
        let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
        run.status.headless_running = run.headless.running();
        run.headless.publish(ready);
        if let Some(observer) = &run.monitoring
            && run.headless.wait_written(Duration::from_secs(1))
        {
            run.status.monitoring_fingerprint = observer.fingerprint();
        }
        run.observed = Instant::now();
        self.publish();
    }

    fn headless_outcome(
        &mut self,
        id: &ServerInstanceId,
        completed: bool,
        cancelled: bool,
        message: String,
    ) -> RuntimeOperationOutcome {
        self.refresh_headless(id);
        let run = self.runs.get(id.as_str()).expect("retained run exists");
        RuntimeOperationOutcome {
            status: run.status.clone(),
            completed,
            cancelled,
            message: Some(message),
        }
    }

    pub(in crate::core::server::lifecycle) fn tick_headless(&mut self) {
        let candidates: Vec<String> = self
            .runs
            .iter()
            .filter(|(_, run)| {
                run.session.is_some()
                    && run.status.phase == InstanceRuntimePhase::Running
                    && !run.headless.is_empty()
            })
            .map(|(id, _)| id.clone())
            .collect();
        for key in candidates {
            let Ok(id) = ServerInstanceId::try_from(key.clone()) else {
                continue;
            };
            let ready = self.readiness(&id) == HeadlessReadiness::Ready;
            let run = self.runs.get_mut(&key).expect("candidate run exists");
            let due = run.headless.due(ready);
            if due.is_empty() {
                continue;
            }
            if let Err((failed, _)) = self.launch(&id, &due, None, &ContentStageControl::default())
            {
                let run = self.runs.get_mut(&key).expect("candidate run exists");
                for index in failed.map_or(due, |index| vec![index]) {
                    run.headless.launch_failed(index);
                }
            }
            self.refresh_headless(&id);
        }
    }

    /// Launches clients in order and stops at the first failure, naming the client that failed
    /// when the failure belongs to one.
    fn launch(
        &mut self,
        id: &ServerInstanceId,
        indices: &[usize],
        execution: Option<&DeploymentExecution>,
        control: &ContentStageControl,
    ) -> Result<usize, (Option<usize>, ServerStoreError)> {
        let run = self.runs.get(id.as_str()).expect("retained run exists");
        let generation = run.status.run_generation;
        let plans: Vec<(usize, HeadlessLaunchPlan, u32)> = indices
            .iter()
            .map(|index| {
                (
                    *index,
                    run.headless.plan(*index).clone(),
                    run.headless.starts(*index),
                )
            })
            .collect();
        let prepared = (|| {
            control.checkpoint()?;
            crate::core::server::deployment_activation::launch::validate_runtime_state(
                &self.store,
                id,
                control,
            )?;
            let installation = self.store.native_installation(id)?;
            let state_root = self.store.record_root(id).join("runtime-state");
            let secret = match plans
                .first()
                .and_then(|(_, plan, _)| plan.password_reference.as_deref())
            {
                Some(reference) => Some(self.store.resolve_config_secret(id, reference)?),
                None => None,
            };
            Ok((installation, state_root, secret))
        })();
        let (installation, state_root, secret) = prepared.map_err(|error| (None, error))?;
        let mut started = 0;
        for (index, plan, starts) in plans {
            let operation_id = match execution {
                Some(execution) => execution.identity().operation_id.clone(),
                None => format!("auto_{generation}_{}_{starts}", plan.id),
            };
            let result = self.spawn_headless(
                id,
                generation,
                &plan,
                &state_root,
                secret.as_deref(),
                &installation,
                operation_id,
                execution,
                control,
            );
            match result {
                Ok(spawned) => {
                    let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
                    run.headless.launched(
                        index,
                        spawned.session,
                        spawned.evidence,
                        spawned.started_unix_ms,
                    );
                    started += 1;
                }
                Err(error) => {
                    if !control.snapshot().cancel_requested {
                        log::warn!(
                            "Headless client {} failed to start; automatic starts are suspended for it",
                            plan.id
                        );
                    }
                    return Err((Some(index), error));
                }
            }
        }
        Ok(started)
    }

    /// An explicit start or restart that is cancelled or fails returns every client it did not
    /// launch to its earlier intent; a client whose own spawn failed is suspended.
    fn launch_explicit(
        &mut self,
        id: &ServerInstanceId,
        pending: &[usize],
        intent: StartIntent,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<usize, ServerStoreError> {
        match self.launch(id, pending, Some(execution), control) {
            Ok(count) => Ok(count),
            Err((failed, error)) => {
                let cancelled = control.snapshot().cancel_requested;
                let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
                let failed = failed.filter(|_| !cancelled);
                if let Some(index) = failed {
                    run.headless.launch_failed(index);
                }
                run.headless.abandon_start(intent, failed);
                self.refresh_headless(id);
                Err(if cancelled {
                    ServerStoreError::Cancelled
                } else {
                    error
                })
            }
        }
    }

    pub(super) fn start_headless(
        &mut self,
        id: &ServerInstanceId,
        expected_run_generation: u64,
        target: Option<&str>,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        self.running_run(id, expected_run_generation, target)?;
        let readiness = self.readiness(id);
        if let Some(message) = readiness.refusal() {
            return Err(ServerStoreError::Invalid(message.into()));
        }
        control.checkpoint()?;
        let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
        let (pending, intent) = run.headless.operator_start(target, true);
        if pending.is_empty() {
            return Ok(self.headless_outcome(
                id,
                true,
                false,
                "The selected headless clients are already running.".into(),
            ));
        }
        let count = self.launch_explicit(id, &pending, intent, execution, control)?;
        Ok(self.headless_outcome(
            id,
            true,
            false,
            format!(
                "Started {count} headless client process(es). Join evidence appears in headless status once the client reports a started mission."
            ),
        ))
    }

    pub(super) fn stop_headless(
        &mut self,
        id: &ServerInstanceId,
        expected_run_generation: u64,
        target: Option<&str>,
        force: bool,
        timeout_ms: u32,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        if timeout_ms == 0 || timeout_ms > 120_000 {
            return Err(ServerStoreError::Invalid("Invalid stop timeout".into()));
        }
        self.running_run(id, expected_run_generation, target)?;
        control.checkpoint()?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
        let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
        run.headless.operator_stop(target);
        let result = if force {
            if run.headless.force(target) {
                GroupStop::Completed
            } else {
                GroupStop::TimedOut
            }
        } else {
            let helper = Self::helper(false).map_err(|_| ServerStoreError::Io)?;
            run.headless.stop(&helper, deadline, Some(control), target)
        };
        let message = match result {
            GroupStop::Completed => {
                "The selected headless clients are stopped and will not restart until started again."
            }
            GroupStop::Cancelled => {
                "Stop was cancelled; the selected headless clients stay stopped once they exit and will not restart automatically."
            }
            GroupStop::TimedOut if force => {
                "A forced headless client stop could not confirm cleanup; its session and protection remain owned."
            }
            GroupStop::TimedOut => {
                "A headless client did not stop within the timeout; retry, or force the stop."
            }
        };
        Ok(self.headless_outcome(
            id,
            result == GroupStop::Completed,
            result == GroupStop::Cancelled,
            message.into(),
        ))
    }

    pub(super) fn restart_headless(
        &mut self,
        id: &ServerInstanceId,
        expected_run_generation: u64,
        target: Option<&str>,
        timeout_ms: u32,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<RuntimeOperationOutcome, ServerStoreError> {
        if timeout_ms == 0 || timeout_ms > 120_000 {
            return Err(ServerStoreError::Invalid("Invalid stop timeout".into()));
        }
        self.running_run(id, expected_run_generation, target)?;
        if let Some(message) = self.readiness(id).refusal() {
            return Err(ServerStoreError::Invalid(message.into()));
        }
        control.checkpoint()?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.into());
        let helper = Self::helper(false).map_err(|_| ServerStoreError::Io)?;
        let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
        match run.headless.stop(&helper, deadline, Some(control), target) {
            GroupStop::Completed => {}
            GroupStop::Cancelled => {
                run.headless.operator_stop(target);
                return Ok(self.headless_outcome(
                    id,
                    false,
                    true,
                    "Restart was cancelled while stopping; the selected headless clients stay stopped until started again.".into(),
                ));
            }
            GroupStop::TimedOut => {
                return Ok(self.headless_outcome(
                    id,
                    false,
                    false,
                    "A headless client did not stop within the timeout; it starts again automatically once it exits, or force a stop.".into(),
                ));
            }
        }
        self.running_run(id, expected_run_generation, target)?;
        let ready = self.readiness(id) == HeadlessReadiness::Ready;
        let run = self.runs.get_mut(id.as_str()).expect("retained run exists");
        let (pending, intent) = run.headless.operator_start(target, ready);
        if !ready {
            return Ok(self.headless_outcome(
                id,
                false,
                false,
                "The selected headless clients stopped, but the server stopped answering its query port; they start automatically once it answers again.".into(),
            ));
        }
        let count = self.launch_explicit(id, &pending, intent, execution, control)?;
        Ok(self.headless_outcome(
            id,
            true,
            false,
            format!("Restarted {count} headless client process(es) with a fresh restart budget."),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_headless(
        &self,
        id: &ServerInstanceId,
        generation: u64,
        plan: &HeadlessLaunchPlan,
        state_root: &Path,
        secret: Option<&str>,
        installation: &ServerInstallation,
        operation_id: String,
        execution: Option<&DeploymentExecution>,
        control: &ContentStageControl,
    ) -> Result<Spawned, ServerStoreError> {
        control.checkpoint()?;
        prepare_headless_state(state_root, plan, secret)?;
        let started_unix_ms = unix_ms();
        let stdout = plan.state_root.join(format!(
            "run_{}_{generation}_{started_unix_ms}.log",
            self.scope.owner_id
        ));
        let output = output_file(&stdout)?;
        let evidence = JoinEvidence::before_spawn(stdout, plan.profiles.clone(), started_unix_ms);
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        let admission = locks.try_admit().map_err(|_| ServerStoreError::Ownership)?;
        let spec = CommandSpec {
            executable: plan.executable.clone(),
            directory: plan.directory.clone(),
            arguments: plan.arguments.iter().map(OsString::from).collect(),
            environment: plan
                .environment
                .iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        };
        let identity = SessionIdentity {
            owner_id: self.scope.owner_id.clone(),
            owner_generation: self.scope.generation,
            game_space_id: self.scope.space_id.clone(),
            instance_id: consumer_instance_id(id.as_str(), &plan.id),
            operation_id,
            run_generation: generation,
        };
        let witness = foxy_supervisor::process::CleanupWitnessCommand {
            executable: std::env::current_exe().map_err(|_| ServerStoreError::Io)?,
            arguments: vec!["server".into(), "cleanup-witness".into()],
            group_id: id.as_str().into(),
            context_key: self.scope.context_key.clone(),
        };
        let session = ForegroundSession::spawn_controlled_witnessed(
            identity,
            &spec,
            &output,
            &witness,
            admission,
            control.cancellation_flag(),
            || {
                control
                    .checkpoint()
                    .map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "Start cancelled"))?;
                if let Some(execution) = execution {
                    execution.validate(self.store.context())?;
                }
                installation
                    .verify()
                    .map_err(|_| io::Error::other("Installation evidence changed"))
            },
        )
        .map_err(|_| {
            ServerStoreError::Invalid(
                "A headless client failed to start; inspect installation consumers before retrying."
                    .into(),
            )
        })?;
        Ok(Spawned {
            session,
            evidence,
            started_unix_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::{HeadlessClientState, HeadlessLifecyclePolicy};

    fn plan(id: &str, name: &str, auto_start: bool) -> HeadlessLaunchPlan {
        HeadlessLaunchPlan {
            id: id.into(),
            name: name.into(),
            executable: "engine".into(),
            directory: ".".into(),
            arguments: Vec::new(),
            environment: Vec::new(),
            state_root: "state".into(),
            profiles: "state/profiles".into(),
            join_password: false,
            lifecycle: HeadlessLifecyclePolicy {
                auto_start,
                ..HeadlessLifecyclePolicy::default()
            },
            parameter_file: None,
            password_reference: None,
        }
    }

    const FIRST: &str = "hc_0000000000000001";
    const SECOND: &str = "hc_0000000000000002";

    #[test]
    fn group_starts_only_when_ready_and_carries_operator_intent_across_runs() {
        let shared = SharedHeadlessStatus::default();
        let mut group = HeadlessGroup::new(
            vec![plan(FIRST, "HC1", true), plan(SECOND, "HC2", false)],
            None,
            shared.clone(),
        );
        let published = shared.snapshot().0.unwrap();
        assert_eq!(
            published.clients[0].state,
            HeadlessClientState::WaitingForServer
        );
        assert_eq!(
            published.clients[1].reason,
            Some(HeadlessReason::ManualStartRequired)
        );
        assert!(group.due(false).is_empty());
        assert_eq!(group.due(true), [0]);
        assert!(group.status(true).ready_unix_ms.is_some());

        assert_eq!(group.operator_start(Some(SECOND), true).0, [1]);
        group.operator_stop(Some(FIRST));
        assert!(group.due(true).contains(&1));
        assert!(!group.due(true).contains(&0));
        let carried = group.carried();
        assert_eq!(carried.get(FIRST), Some(&false));
        assert_eq!(carried.get(SECOND), Some(&true));

        let mut next = HeadlessGroup::new(
            vec![plan(FIRST, "HC1", true), plan(SECOND, "HC2", false)],
            Some(&carried),
            SharedHeadlessStatus::default(),
        );
        assert_eq!(next.due(true), [1]);
        next.hold();
        assert!(next.due(true).is_empty());
        assert!(next.status(true).clients.iter().all(|client| {
            client.state == HeadlessClientState::Stopped
                && client.reason == Some(HeadlessReason::PausedByServerStop)
        }));
        let (pending, intent) = next.operator_start(None, true);
        assert_eq!(pending, [0, 1]);
        assert_eq!(next.due(true), [0, 1]);
        next.abandon_start(intent, None);
        assert!(next.due(true).is_empty());
        let (_, intent) = next.operator_start(None, true);
        next.abandon_start(intent, Some(0));
        next.launch_failed(0);
        next.hold = false;
        assert_eq!(next.due(true), [1]);
        next.operator_start(None, true);
        assert_eq!(next.due(true), [0, 1]);
        next.server_ended();
        assert!(next.due(true).is_empty());
        let ended = next.status(false);
        assert!(
            ended
                .clients
                .iter()
                .all(|client| client.state == HeadlessClientState::Stopped)
        );
        assert_eq!(
            ended.clients[0].reason,
            Some(HeadlessReason::ServerNotRunning)
        );
    }
}
