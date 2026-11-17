use std::path::{Path, PathBuf};

use super::runtime::{InternalCompletion, RuntimeService};
use super::*;
use crate::core::server::jobs::{
    self, AfterAttempt, AttemptResult, MaintenanceAction, MaintenancePolicy, Occurrence,
    OccurrenceReason, OccurrenceState, OccurrenceTrigger, PlayerEvidence, RestartInputs,
    RestartStep, ScheduledDue,
};
use crate::core::server::{
    ContentStageControl, InstanceRuntimePhase, InstanceRuntimeStatus, ServerInstanceId, ServerStore,
};
use foxy_supervisor::control::{Ledger, MAX_PAYLOAD_BYTES, OperationResult, ServerSession};

const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const CLIENT_ID: &str = "maintenance";

/// The owner's answer to a maintenance run or cancel request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaintenanceReply {
    pub(crate) occurrence_id: Option<String>,
    pub(crate) state: Option<OccurrenceState>,
    pub(crate) created: bool,
    pub(crate) error: Option<String>,
}

struct Active {
    root: PathBuf,
    occurrence: Occurrence,
    policy: MaintenancePolicy,
    cancel_requested: bool,
}

pub(super) struct MaintenanceService {
    context: ServerContext,
    scope: Scope,
    store: Option<ServerStore>,
    next_scan: Instant,
    recovered: bool,
    active: Option<Active>,
}

fn reply(value: &MaintenanceReply) -> io::Result<OperationResult> {
    let payload =
        serde_json::to_vec(value).map_err(|_| invalid("Maintenance reply encoding failed"))?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid("Maintenance reply exceeds the limit"));
    }
    Ok(OperationResult {
        succeeded: value.error.is_none(),
        payload,
    })
}

fn refused(message: impl Into<String>) -> MaintenanceReply {
    MaintenanceReply {
        occurrence_id: None,
        state: None,
        created: false,
        error: Some(message.into()),
    }
}

fn request_identity(occurrence_id: &str, attempt: u8) -> u64 {
    let mut hash = blake3::Hasher::new();
    hash.update(occurrence_id.as_bytes());
    hash.update(&[attempt]);
    let bytes: [u8; 8] = hash.finalize().as_bytes()[..8]
        .try_into()
        .expect("eight bytes");
    u64::from_le_bytes(bytes) | 1
}

fn player_evidence(store: &ServerStore, status: &InstanceRuntimeStatus) -> PlayerEvidence {
    let Ok(Some(snapshot)) = store.read_monitoring_snapshot(status) else {
        return PlayerEvidence::Unknown;
    };
    let report = crate::core::server::monitoring::InstanceMonitoring::new(
        status.clone(),
        Some(snapshot),
        jobs::unix_ms(),
    );
    if !report.query_ready {
        return PlayerEvidence::Unknown;
    }
    match report
        .observation
        .and_then(|observation| observation.query.last_good)
    {
        Some(summary) if summary.players == 0 => PlayerEvidence::Empty,
        Some(_) => PlayerEvidence::Occupied,
        None => PlayerEvidence::Unknown,
    }
}

impl MaintenanceService {
    pub(super) fn new(context: &ServerContext, scope: &Scope) -> Self {
        Self {
            context: context.clone(),
            scope: scope.clone(),
            store: None,
            next_scan: Instant::now(),
            recovered: false,
            active: None,
        }
    }

    fn store(&mut self) -> Option<&ServerStore> {
        if self.store.is_none() {
            self.store = ServerStore::new(self.context.clone()).ok();
        }
        self.store.as_ref()
    }

    pub(super) fn handle(
        &mut self,
        session: &ServerSession<'_>,
        message: &ClientMessage,
        ledger: &mut Ledger,
        runtime: &RuntimeService,
    ) -> io::Result<Option<ServerMessage>> {
        let ClientMessage::Submit { request } = message else {
            return Ok(None);
        };
        if !matches!(
            request.operation,
            Operation::RunMaintenanceJob { .. } | Operation::CancelMaintenanceOccurrence { .. }
        ) {
            return Ok(None);
        }
        let response = ledger.handle(session, message.clone())?;
        if let ServerMessage::Request {
            reply: RequestReply::Accepted { request_id },
        } = &response
        {
            let ticket = ledger.completion_ticket(session, *request_id)?;
            let outcome = match &request.operation {
                Operation::RunMaintenanceJob {
                    instance_id,
                    job_id,
                    expected_policy_revision,
                } => self.request_run(
                    instance_id,
                    job_id,
                    *expected_policy_revision,
                    &format!(
                        "{}/{}/{}/{}",
                        self.scope.owner_id,
                        self.scope.generation,
                        session.client_id(),
                        request.request_id
                    ),
                ),
                Operation::CancelMaintenanceOccurrence {
                    instance_id,
                    occurrence_id,
                } => self.request_cancel(instance_id, occurrence_id, runtime),
                _ => refused("Invalid maintenance operation"),
            };
            ledger.complete_pending(ticket, reply(&outcome)?)?;
        }
        Ok(Some(response))
    }

    fn request_run(
        &mut self,
        instance_id: &str,
        job_id: &str,
        expected_policy_revision: u64,
        request: &str,
    ) -> MaintenanceReply {
        let owner_id = self.scope.owner_id.clone();
        let owner_generation = self.scope.generation;
        let Ok(id) = ServerInstanceId::try_from(instance_id.to_owned()) else {
            return refused("Invalid server instance");
        };
        let Some(store) = self.store() else {
            return refused("The server store is unavailable");
        };
        let catalog = match store.load_maintenance(&id) {
            Ok(catalog) => catalog,
            Err(error) => return refused(error.to_string()),
        };
        let Some(job) = catalog.job(job_id) else {
            return refused("No maintenance job with that id exists");
        };
        if job.revision != expected_policy_revision {
            return refused("The job changed since it was reviewed; review it again");
        }
        let root = store.occurrence_root(&id);
        let Ok(history) = store.maintenance_history(&id) else {
            return refused("The occurrence records are unreadable");
        };
        if let Some(open) = history
            .occurrences
            .iter()
            .find(|occurrence| occurrence.job_id == job.id && !occurrence.state.terminal())
        {
            return MaintenanceReply {
                occurrence_id: Some(open.occurrence_id.clone()),
                state: Some(open.state),
                created: false,
                error: None,
            };
        }
        let now = jobs::unix_ms();
        let occurrence = Occurrence::new(
            id,
            &job.id,
            job.revision,
            job.definition.action,
            OccurrenceTrigger::Manual {
                requested_unix_ms: now,
                request: request.to_owned(),
            },
            &owner_id,
            owner_generation,
            now,
        );
        let result = jobs::claim(&root, &occurrence);
        self.next_scan = Instant::now();
        match result {
            Ok(true) => MaintenanceReply {
                occurrence_id: Some(occurrence.occurrence_id),
                state: Some(OccurrenceState::Claimed),
                created: true,
                error: None,
            },
            Ok(false) => {
                let existing = jobs::read(&root, &occurrence.occurrence_id).ok().flatten();
                MaintenanceReply {
                    state: existing.map(|existing| existing.state),
                    occurrence_id: Some(occurrence.occurrence_id),
                    created: false,
                    error: None,
                }
            }
            Err(_) => refused("The occurrence could not be recorded"),
        }
    }

    fn request_cancel(
        &mut self,
        instance_id: &str,
        occurrence_id: &str,
        runtime: &RuntimeService,
    ) -> MaintenanceReply {
        let Ok(id) = ServerInstanceId::try_from(instance_id.to_owned()) else {
            return refused("Invalid server instance");
        };
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.occurrence.occurrence_id == occurrence_id)
        {
            active.cancel_requested = true;
            runtime.cancel_internal(occurrence_id);
            return MaintenanceReply {
                occurrence_id: Some(occurrence_id.to_owned()),
                state: Some(OccurrenceState::Running),
                created: false,
                error: None,
            };
        }
        let owner_id = self.scope.owner_id.clone();
        let Some(store) = self.store() else {
            return refused("The server store is unavailable");
        };
        if store.maintenance_history(&id).is_err() {
            return refused("The occurrence records are unreadable");
        }
        let root = store.occurrence_root(&id);
        let mut occurrence = match jobs::read(&root, occurrence_id) {
            Ok(Some(occurrence)) if occurrence.instance_id == id => occurrence,
            Ok(_) => return refused("No occurrence with that id exists for this instance"),
            Err(_) => return refused("The occurrence record is unreadable"),
        };
        if occurrence.state.terminal() {
            return MaintenanceReply {
                occurrence_id: Some(occurrence.occurrence_id),
                state: Some(occurrence.state),
                created: false,
                error: None,
            };
        }
        if occurrence.owner_id != owner_id {
            return refused("Another owner recorded this occurrence; review recovery first");
        }
        occurrence.state = OccurrenceState::Cancelled;
        occurrence.reason = Some(OccurrenceReason::CancelledByOperator);
        occurrence.next_attempt_unix_ms = None;
        occurrence.updated_unix_ms = jobs::unix_ms();
        match jobs::update(&root, &occurrence) {
            Ok(()) => MaintenanceReply {
                occurrence_id: Some(occurrence.occurrence_id),
                state: Some(OccurrenceState::Cancelled),
                created: false,
                error: None,
            },
            Err(_) => refused("The occurrence could not be updated"),
        }
    }

    /// Claims due scheduled occurrences on every scan, even while one runs, then evaluates every
    /// ready occurrence in creation order until one becomes active.
    pub(super) fn tick(
        &mut self,
        runtime: &mut RuntimeService,
        deployment_busy: bool,
        directory: &ProtectedDirectory,
    ) {
        if Instant::now() < self.next_scan {
            return;
        }
        self.next_scan = Instant::now() + SCAN_INTERVAL;
        let owner_id = self.scope.owner_id.clone();
        let owner_generation = self.scope.generation;
        let recovered = self.recovered;
        let Some(store) = self.store() else {
            return;
        };
        let Ok(catalog) = store.load() else {
            return;
        };
        let now = jobs::unix_ms();
        let mut candidates: Vec<(PathBuf, Occurrence)> = Vec::new();
        for instance in &catalog.instances {
            let id = instance.id.clone();
            let root = store.occurrence_root(&id);
            let Ok(mut history) = store.maintenance_history(&id) else {
                continue;
            };
            if !recovered
                && history
                    .occurrences
                    .iter()
                    .any(|item| !item.state.terminal() && item.owner_id != owner_id)
            {
                let _ = jobs::recover_abandoned(&root, &owner_id, now);
                let Ok(recovered_history) = store.maintenance_history(&id) else {
                    continue;
                };
                history = recovered_history;
            }
            let mut open_jobs: std::collections::HashSet<String> = history
                .occurrences
                .iter()
                .filter(|occurrence| !occurrence.state.terminal())
                .map(|occurrence| occurrence.job_id.clone())
                .collect();
            if let Ok(maintenance) = store.load_maintenance(&id) {
                for job in &maintenance.jobs {
                    let (scheduled, missed) = match jobs::scheduled_due(job, now) {
                        Some(ScheduledDue::Run { scheduled_unix_ms }) => (scheduled_unix_ms, false),
                        Some(ScheduledDue::Missed { scheduled_unix_ms }) => {
                            (scheduled_unix_ms, true)
                        }
                        None => continue,
                    };
                    let mut occurrence = Occurrence::new(
                        id.clone(),
                        &job.id,
                        job.revision,
                        job.definition.action,
                        OccurrenceTrigger::Scheduled {
                            scheduled_unix_ms: scheduled,
                        },
                        &owner_id,
                        owner_generation,
                        now,
                    );
                    if missed {
                        occurrence.state = OccurrenceState::Skipped;
                        occurrence.reason = Some(OccurrenceReason::MissedWindow);
                    } else if open_jobs.contains(&job.id) {
                        occurrence.state = OccurrenceState::Skipped;
                        occurrence.reason = Some(OccurrenceReason::OverlappingOccurrence);
                        occurrence.set_message(OccurrenceReason::OverlappingOccurrence.describe());
                    }
                    if jobs::claim(&root, &occurrence).unwrap_or(false)
                        && occurrence.state == OccurrenceState::Claimed
                    {
                        open_jobs.insert(job.id.clone());
                        candidates.push((root.clone(), occurrence));
                    }
                }
            }
            for occurrence in history.occurrences {
                let ready = match occurrence.state {
                    OccurrenceState::Claimed | OccurrenceState::Deferred => true,
                    OccurrenceState::RetryWait => occurrence
                        .next_attempt_unix_ms
                        .is_none_or(|next| next <= now),
                    _ => false,
                };
                if ready && occurrence.owner_id == owner_id {
                    candidates.push((root.clone(), occurrence));
                }
            }
        }
        self.recovered = true;
        if self.active.is_some() {
            return;
        }
        candidates.sort_by_key(|(_, occurrence)| occurrence.created_unix_ms);
        for (root, occurrence) in candidates {
            self.process(root, occurrence, runtime, deployment_busy, directory);
            if self.active.is_some() {
                break;
            }
        }
    }

    fn finish(
        root: &Path,
        mut occurrence: Occurrence,
        state: OccurrenceState,
        reason: Option<OccurrenceReason>,
        message: Option<String>,
    ) {
        occurrence.state = state;
        occurrence.reason = reason;
        occurrence.next_attempt_unix_ms = None;
        if let Some(message) = message {
            occurrence.set_message(message);
        }
        occurrence.updated_unix_ms = jobs::unix_ms();
        let _ = jobs::update(root, &occurrence);
        if state.terminal() {
            let _ = jobs::prune(root);
        }
    }

    fn process(
        &mut self,
        root: PathBuf,
        mut occurrence: Occurrence,
        runtime: &mut RuntimeService,
        deployment_busy: bool,
        directory: &ProtectedDirectory,
    ) {
        let context = self.context.clone();
        let scope = self.scope.clone();
        let Some(store) = self.store() else {
            return;
        };
        let id = occurrence.instance_id.clone();
        let now = jobs::unix_ms();
        let Ok(catalog) = store.load_maintenance(&id) else {
            return;
        };
        let current = catalog
            .job(&occurrence.job_id)
            .filter(|job| job.revision == occurrence.policy_revision);
        let policy = match current {
            Some(job) => job.definition.policy.clone(),
            None if occurrence.stop_completed => catalog
                .job(&occurrence.job_id)
                .map(|job| job.definition.policy.clone())
                .unwrap_or_default(),
            None => {
                return Self::finish(
                    &root,
                    occurrence,
                    OccurrenceState::Cancelled,
                    Some(OccurrenceReason::CancelledByOperator),
                    Some("The job was removed or changed before this occurrence started.".into()),
                );
            }
        };
        if occurrence.state == OccurrenceState::Claimed
            && let OccurrenceTrigger::Scheduled { scheduled_unix_ms } = occurrence.trigger
            && jobs::claim_expired(scheduled_unix_ms, &policy, now)
        {
            return Self::finish(
                &root,
                occurrence,
                OccurrenceState::Skipped,
                Some(OccurrenceReason::MissedWindow),
                Some(OccurrenceReason::MissedWindow.describe().into()),
            );
        }
        if let Some(reason) = occurrence.action.unavailable_reason(&store.capabilities()) {
            return Self::finish(
                &root,
                occurrence,
                OccurrenceState::Blocked,
                Some(OccurrenceReason::AdapterUnavailable),
                Some(reason.into()),
            );
        }
        if policy.arma.as_ref().is_some_and(|arma| arma.require_backup) {
            return Self::finish(&root, occurrence, OccurrenceState::Blocked,
                Some(OccurrenceReason::AdapterUnavailable),
                Some("The job requires a consistent backup; the backup adapter is unavailable. No administration command was sent.".into()));
        }
        if !matches!(
            occurrence.action,
            MaintenanceAction::Restart
                | MaintenanceAction::RotateMission
                | MaintenanceAction::ReloadScenario
        ) {
            return;
        }
        let status = runtime.instance_status(&id);
        let started_since_stop = occurrence.stop_completed
            && status.as_ref().is_some_and(|status| {
                occurrence
                    .run_generation_after
                    .or(occurrence.run_generation_before)
                    .is_some_and(|last| last != status.run_generation)
            });
        let server_running = status.as_ref().is_some_and(|status| {
            status.process_exists
                && matches!(
                    status.phase,
                    InstanceRuntimePhase::Running
                        | InstanceRuntimePhase::Starting
                        | InstanceRuntimePhase::StopRequested
                        | InstanceRuntimePhase::CleanupUncertain
                )
        });
        let settled = status
            .as_ref()
            .is_some_and(|status| status.phase == InstanceRuntimePhase::Running);
        let players = status.as_ref().map_or(PlayerEvidence::Unknown, |status| {
            player_evidence(store, status)
        });
        let lock = operation_file(directory)
            .ok()
            .filter(|file| file.try_lock().is_ok());
        let operation_busy = runtime.busy() || deployment_busy || lock.is_none();
        let step = jobs::restart_step(
            RestartInputs {
                server_running,
                server_settled: settled,
                stop_completed_by_occurrence: occurrence.stop_completed,
                started_since_stop,
                players,
                operation_busy,
            },
            &policy,
            occurrence.deferred_since_unix_ms,
            now,
        );
        let start_only = match step {
            RestartStep::Defer(reason) => {
                if occurrence.state != OccurrenceState::Deferred
                    || occurrence.reason != Some(reason)
                {
                    occurrence.state = OccurrenceState::Deferred;
                    occurrence.reason = Some(reason);
                    occurrence.deferred_since_unix_ms.get_or_insert(now);
                    occurrence.updated_unix_ms = now;
                    occurrence.set_message(reason.describe());
                    let _ = jobs::update(&root, &occurrence);
                }
                return;
            }
            RestartStep::Skip(reason) => {
                return Self::finish(
                    &root,
                    occurrence,
                    OccurrenceState::Skipped,
                    Some(reason),
                    Some(reason.describe().into()),
                );
            }
            RestartStep::Restart => false,
            RestartStep::StartOnly => true,
        };
        let Some(lock) = lock else {
            return;
        };
        let control = ContentStageControl::default();
        let plan = match store.plan_instance_start(&id, &control) {
            Ok(plan) => plan,
            Err(error) => {
                occurrence.attempts = occurrence.attempts.saturating_add(1);
                return self.record_attempt(
                    &root,
                    occurrence,
                    &policy,
                    AttemptResult {
                        completed: false,
                        cancelled: false,
                        stop_completed: false,
                    },
                    Some(format!(
                        "{} {error}",
                        OccurrenceReason::StartPlanUnavailable.describe()
                    )),
                );
            }
        };
        occurrence.attempts = occurrence.attempts.saturating_add(1);
        if (matches!(
            occurrence.action,
            MaintenanceAction::RotateMission | MaintenanceAction::ReloadScenario
        ) || policy
            .arma
            .as_ref()
            .is_some_and(|arma| arma.announcement.is_some()))
            && plan.launch.rcon.is_none()
        {
            return Self::finish(&root, occurrence, OccurrenceState::Blocked,
                Some(OccurrenceReason::AdapterUnavailable),
                Some("This Arma maintenance action requires RCon in the committed runtime. Nothing was sent.".into()));
        }
        let operation = if occurrence.action == MaintenanceAction::ReloadScenario {
            Operation::ReloadReforgerScenario {
                instance_id: id.as_str().into(),
                expected_run_generation: status.as_ref().map_or(0, |status| status.run_generation),
                timeout_ms: u32::from(policy.stop_timeout_seconds) * 1000,
            }
        } else if occurrence.action == MaintenanceAction::RotateMission {
            Operation::RotateArmaMission {
                instance_id: id.as_str().into(),
                expected_run_generation: status.as_ref().map_or(0, |status| status.run_generation),
                template: policy
                    .arma
                    .as_ref()
                    .and_then(|arma| arma.mission_template.clone())
                    .unwrap_or_default(),
                timeout_ms: u32::from(policy.stop_timeout_seconds) * 1000,
            }
        } else if start_only {
            Operation::StartInstance {
                instance_id: id.as_str().into(),
                expected_activation_generation: plan.activation_generation,
                review_fingerprint: plan.fingerprint.clone(),
            }
        } else {
            let run_generation = status.as_ref().map_or(0, |status| status.run_generation);
            occurrence
                .run_generation_before
                .get_or_insert(run_generation);
            Operation::RestartInstance {
                instance_id: id.as_str().into(),
                expected_run_generation: run_generation,
                expected_activation_generation: plan.activation_generation,
                review_fingerprint: plan.fingerprint.clone(),
                force: false,
                timeout_ms: u32::from(policy.stop_timeout_seconds) * 1000,
            }
        };
        let mut execution = match DeploymentExecution::new(
            &context,
            &scope,
            CLIENT_ID,
            request_identity(&occurrence.occurrence_id, occurrence.attempts),
            id.clone(),
            lock,
        ) {
            Ok(execution) => execution,
            Err(error) => {
                return self.record_attempt(
                    &root,
                    occurrence,
                    &policy,
                    AttemptResult {
                        completed: false,
                        cancelled: false,
                        stop_completed: false,
                    },
                    Some(error.to_string()),
                );
            }
        };
        execution.arma_maintenance = policy
            .arma
            .clone()
            .map(|arma| (occurrence.occurrence_id.clone(), arma));
        execution.maintenance_player_policy = Some(policy.player_policy);
        occurrence.state = OccurrenceState::Running;
        occurrence.reason = None;
        occurrence.deferred_since_unix_ms = None;
        occurrence.next_attempt_unix_ms = None;
        occurrence.message = None;
        occurrence.updated_unix_ms = now;
        if jobs::update(&root, &occurrence).is_err() {
            return;
        }
        match runtime.submit_internal(
            operation,
            execution,
            control,
            occurrence.occurrence_id.clone(),
        ) {
            Ok(()) => {
                self.active = Some(Active {
                    root,
                    occurrence,
                    policy,
                    cancel_requested: false,
                });
            }
            Err(error) => self.record_attempt(
                &root,
                occurrence,
                &policy,
                AttemptResult {
                    completed: false,
                    cancelled: false,
                    stop_completed: false,
                },
                Some(error.to_string()),
            ),
        }
    }

    fn record_attempt(
        &self,
        root: &Path,
        mut occurrence: Occurrence,
        policy: &MaintenancePolicy,
        result: AttemptResult,
        message: Option<String>,
    ) {
        let now = jobs::unix_ms();
        occurrence.stop_completed |= result.stop_completed;
        let (state, reason) = match jobs::after_attempt(result, occurrence.attempts, policy, now) {
            AfterAttempt::Succeeded => (OccurrenceState::Succeeded, None),
            AfterAttempt::Cancelled => (
                OccurrenceState::Cancelled,
                Some(OccurrenceReason::CancelledByOperator),
            ),
            AfterAttempt::Retry { next_unix_ms } => {
                occurrence.state = OccurrenceState::RetryWait;
                occurrence.reason = Some(OccurrenceReason::AttemptFailed);
                occurrence.next_attempt_unix_ms = Some(next_unix_ms);
                if let Some(message) = message {
                    occurrence.set_message(message);
                }
                occurrence.updated_unix_ms = now;
                let _ = jobs::update(root, &occurrence);
                return;
            }
            AfterAttempt::Failed => (
                OccurrenceState::Failed,
                Some(OccurrenceReason::AttemptFailed),
            ),
        };
        Self::finish(root, occurrence, state, reason, message);
    }

    pub(super) fn completed(&mut self, completion: InternalCompletion, runtime: &RuntimeService) {
        let Some(active) = self
            .active
            .take_if(|active| active.occurrence.occurrence_id == completion.key)
        else {
            return;
        };
        let Active {
            root,
            mut occurrence,
            policy,
            cancel_requested,
        } = active;
        let after = runtime.instance_status(&occurrence.instance_id);
        // The attempt held the operation lock, so no other operation started or stopped the run;
        // a failed spawn advances the generation, so only process absence counts here.
        let stopped_by_us = after.as_ref().is_some_and(|status| !status.process_exists);
        let (completed, cancelled, message) = match completion.result {
            Ok(outcome) => (outcome.completed, outcome.cancelled, outcome.message),
            Err(error) => (false, false, Some(error)),
        };
        let cancelled = cancelled || cancel_requested;
        if !completed
            && !cancelled
            && (matches!(
                occurrence.action,
                MaintenanceAction::RotateMission | MaintenanceAction::ReloadScenario
            ) || policy
                .arma
                .as_ref()
                .is_some_and(|arma| arma.announcement.is_some()))
        {
            return Self::finish(&root, occurrence, OccurrenceState::Unknown,
                Some(OccurrenceReason::AttemptFailed), message.or_else(|| Some("The administration effect is unconfirmed; it will not be repeated automatically.".into())));
        }
        occurrence.run_generation_after = after.as_ref().map(|status| status.run_generation);
        self.record_attempt(
            &root,
            occurrence,
            &policy,
            AttemptResult {
                completed,
                cancelled,
                stop_completed: stopped_by_us,
            },
            message,
        );
    }
}

impl Attachment {
    /// Submits a maintenance run or cancel request; a retried request id returns the recorded answer.
    pub(crate) fn maintenance(&self, operation: Operation) -> io::Result<MaintenanceReply> {
        if !matches!(
            operation,
            Operation::RunMaintenanceJob { .. } | Operation::CancelMaintenanceOccurrence { .. }
        ) {
            return Err(invalid("Invalid maintenance operation"));
        }
        let identity = self.request_identity()?;
        let client_id = identity.client_id.to_owned();
        let request_id = identity.request_id;
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(15);
        let result = loop {
            let budget = IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            };
            match self.deployment_request(&client_id, request_id, operation.clone(), budget) {
                Ok(Some(result)) => break result,
                Ok(None) => {}
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => {}
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "The maintenance request outcome is unknown; review the job history before retrying",
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let _ = self.deployment_message(
            &client_id,
            ClientMessage::Acknowledge { request_id },
            IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            },
        );
        if result.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("Maintenance reply is too large"));
        }
        serde_json::from_slice(&result.payload).map_err(|_| invalid("Invalid maintenance reply"))
    }
}
