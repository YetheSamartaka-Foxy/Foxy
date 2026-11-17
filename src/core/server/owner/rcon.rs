use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::runtime::RuntimeService;
use super::*;
use crate::core::server::deployment_activation::NativeRconPlan;
use crate::core::server::rcon::arma3::ArmaAdminCommand;
use crate::core::server::rcon::{
    self, CommandOutcome, KEEPALIVE_INTERVAL, LoginFailure, RconPassword, Retransmit, ServerText,
    Session, SessionCounters,
    records::{self, RconCommandReason, RconCommandRecord, RconCommandState},
};
use crate::core::server::{ServerInstanceId, ServerStore};
use foxy_supervisor::control::{
    CompletionTicket, Ledger, MAX_PAYLOAD_BYTES, OperationResult, ServerSession,
};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(3);
const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_secs(15);
const RETAINED_MESSAGES: usize = 64;
const QUEUE: usize = 4;
const STOP_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RconConnection {
    Connecting,
    Connected,
    Rejected,
    NoReply,
    Unreachable,
    PasswordUnavailable,
    Lost,
}

/// Owner-held RCon session state for one run. Console messages stay in memory only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RconSessionStatus {
    pub instance_id: String,
    pub owner_id: String,
    pub owner_generation: u64,
    pub run_generation: u64,
    pub endpoint: String,
    pub connection: RconConnection,
    pub session_generation: u32,
    pub failed_attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_trip_ms: Option<u64>,
    pub messages_acknowledged: u64,
    pub duplicate_messages: u64,
    pub invalid_datagrams: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ServerText>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RconInspectReply {
    pub status: Option<RconSessionStatus>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RconTestReply {
    pub replied: bool,
    pub round_trip_ms: Option<u64>,
    pub session_generation: Option<u32>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RconCommandReply {
    pub command_id: Option<String>,
    pub state: Option<RconCommandState>,
    pub reason: Option<RconCommandReason>,
    pub error: Option<String>,
}

fn encode<T: Serialize>(value: &T, succeeded: bool) -> io::Result<OperationResult> {
    let payload = serde_json::to_vec(value).map_err(|_| invalid("RCon reply encoding failed"))?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid("RCon reply exceeds the limit"));
    }
    Ok(OperationResult { succeeded, payload })
}

fn command_refused(message: impl Into<String>) -> RconCommandReply {
    RconCommandReply {
        command_id: None,
        state: None,
        reason: None,
        error: Some(message.into()),
    }
}

fn test_refused(message: impl Into<String>) -> RconTestReply {
    RconTestReply {
        replied: false,
        round_trip_ms: None,
        session_generation: None,
        error: Some(message.into()),
    }
}

/// Keeps the newest messages that fit the owner reply bound.
pub(crate) fn inspect_payload(status: &RconSessionStatus) -> io::Result<OperationResult> {
    let mut bounded = status.clone();
    loop {
        let reply = RconInspectReply {
            status: Some(bounded.clone()),
            error: None,
        };
        match encode(&reply, true) {
            Ok(result) => return Ok(result),
            Err(error) if bounded.messages.is_empty() => return Err(error),
            Err(_) => {
                bounded.messages.remove(0);
            }
        }
    }
}

enum Work {
    Test {
        key: u64,
    },
    Send {
        key: u64,
        record: Box<RconCommandRecord>,
        timeout: Duration,
        guard: Option<ArmaAdminCommand>,
    },
}

struct Finished {
    key: u64,
    result: OperationResult,
}

struct Shared {
    status: RconSessionStatus,
    messages: VecDeque<ServerText>,
}

struct Worker {
    run_generation: u64,
    shared: Arc<Mutex<Shared>>,
    work: mpsc::SyncSender<Work>,
    finished: mpsc::Receiver<Finished>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn status(&self) -> RconSessionStatus {
        let shared = self
            .shared
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut status = shared.status.clone();
        status.messages = shared.messages.iter().cloned().collect();
        status
    }

    /// A worker inside a login or command sees the flag only afterwards; it is detached then,
    /// and still finalizes its own record.
    fn stop(&mut self, wait: Duration) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let deadline = Instant::now() + wait;
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

struct Job {
    ticket: CompletionTicket,
    key: u64,
    instance: String,
    test: bool,
}

pub(super) struct RconService {
    context: ServerContext,
    scope: Scope,
    workers: BTreeMap<String, Worker>,
    jobs: Vec<Job>,
    recovered: BTreeSet<String>,
    recovered_all: bool,
    next_key: u64,
}

impl RconService {
    pub(super) fn new(context: &ServerContext, scope: &Scope) -> Self {
        Self {
            context: context.clone(),
            scope: scope.clone(),
            workers: BTreeMap::new(),
            jobs: Vec::new(),
            recovered: BTreeSet::new(),
            recovered_all: false,
            next_key: 1,
        }
    }

    fn record_root(&self, instance: &str) -> Option<PathBuf> {
        let id = ServerInstanceId::try_from(instance.to_owned()).ok()?;
        ServerStore::new(self.context.clone())
            .ok()
            .map(|store| store.rcon_command_root(&id))
    }

    pub(super) fn tick(&mut self, runtime: &RuntimeService, ledger: &mut Ledger) -> io::Result<()> {
        if !self.recovered_all {
            self.recovered_all = true;
            if let Ok(store) = ServerStore::new(self.context.clone())
                && let Ok(catalog) = store.load()
            {
                for instance in &catalog.instances {
                    let root = store.rcon_command_root(&instance.id);
                    if root.is_dir() {
                        let _ = records::recover_abandoned(
                            &root,
                            &self.scope.owner_id,
                            self.scope.generation,
                        );
                        let _ = records::prune(&root);
                    }
                    self.recovered.insert(instance.id.as_str().to_owned());
                }
            }
        }
        let targets = runtime.rcon_targets();
        let stale: Vec<String> = self
            .workers
            .iter()
            .filter(|(id, worker)| {
                targets
                    .get(*id)
                    .is_none_or(|(generation, _)| *generation != worker.run_generation)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some(mut worker) = self.workers.remove(&id) {
                worker.stop(Duration::ZERO);
                self.drain(&id, &worker, ledger)?;
            }
        }
        for (id, (generation, plan)) in targets {
            if self.workers.contains_key(&id) {
                continue;
            }
            let Some(root) = self.record_root(&id) else {
                continue;
            };
            if self.recovered.insert(id.clone()) {
                let _ =
                    records::recover_abandoned(&root, &self.scope.owner_id, self.scope.generation);
                let _ = records::prune(&root);
            }
            if let Ok(worker) = self.spawn(&id, generation, plan, root) {
                self.workers.insert(id, worker);
            }
        }
        let ids: Vec<String> = self.workers.keys().cloned().collect();
        for id in ids {
            if let Some(worker) = self.workers.remove(&id) {
                let result = self.drain(&id, &worker, ledger);
                self.workers.insert(id, worker);
                result?;
            }
        }
        Ok(())
    }

    fn drain(&mut self, id: &str, worker: &Worker, ledger: &mut Ledger) -> io::Result<()> {
        while let Ok(finished) = worker.finished.try_recv() {
            if let Some(index) = self
                .jobs
                .iter()
                .position(|job| job.key == finished.key && job.instance == id)
            {
                let job = self.jobs.remove(index);
                ledger.complete_pending(job.ticket, finished.result)?;
            }
        }
        if !self.workers.contains_key(id) && worker.handle.is_none() {
            let orphaned: Vec<usize> = self
                .jobs
                .iter()
                .enumerate()
                .filter(|(_, job)| job.instance == id)
                .map(|(index, _)| index)
                .rev()
                .collect();
            for index in orphaned {
                let job = self.jobs.remove(index);
                let result = if job.test {
                    encode(&test_refused("The server run ended"), false)?
                } else {
                    encode(
                        &command_refused(
                            "The server run ended; check RCon history for the outcome",
                        ),
                        false,
                    )?
                };
                ledger.complete_pending(job.ticket, result)?;
            }
        }
        Ok(())
    }

    fn spawn(
        &self,
        id: &str,
        run_generation: u64,
        plan: NativeRconPlan,
        root: PathBuf,
    ) -> io::Result<Worker> {
        let shared = Arc::new(Mutex::new(Shared {
            status: RconSessionStatus {
                instance_id: id.into(),
                owner_id: self.scope.owner_id.clone(),
                owner_generation: self.scope.generation,
                run_generation,
                endpoint: plan.endpoint.to_string(),
                connection: RconConnection::Connecting,
                session_generation: 0,
                failed_attempts: 0,
                connected_unix_ms: None,
                last_reply_unix_ms: None,
                round_trip_ms: None,
                messages_acknowledged: 0,
                duplicate_messages: 0,
                invalid_datagrams: 0,
                messages: Vec::new(),
            },
            messages: VecDeque::new(),
        }));
        let (work, queue) = mpsc::sync_channel(QUEUE);
        let (done, finished) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let context = self.context.clone();
        let instance = ServerInstanceId::try_from(id.to_owned())
            .map_err(|_| invalid("Invalid RCon instance"))?;
        let handle = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("foxy-rcon".into())
                .spawn(move || {
                    run_worker(WorkerInputs {
                        context,
                        instance,
                        plan,
                        root,
                        shared,
                        queue,
                        done,
                        stop,
                    })
                })?
        };
        Ok(Worker {
            run_generation,
            shared,
            work,
            finished,
            stop,
            handle: Some(handle),
        })
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
            Operation::InspectRcon { .. }
                | Operation::TestRcon { .. }
                | Operation::SendRconCommand { .. }
                | Operation::SendArmaAdminCommand { .. }
        ) {
            return Ok(None);
        }
        if let Operation::InspectRcon { instance_id } = &request.operation {
            // A read without effects: answered directly so passive polls never hold the
            // controller cursor or occupy ledger capacity.
            let result = match self.workers.get(instance_id) {
                Some(worker) => inspect_payload(&worker.status())?,
                None => encode(
                    &RconInspectReply {
                        status: None,
                        error: Some(
                            "No running server of this instance has RCon configured in this owner"
                                .into(),
                        ),
                    },
                    false,
                )?,
            };
            return Ok(Some(ServerMessage::Request {
                reply: RequestReply::Complete {
                    request_id: request.request_id,
                    result,
                },
            }));
        }
        let response = ledger.handle(session, message.clone())?;
        let ServerMessage::Request {
            reply: RequestReply::Accepted { request_id },
        } = &response
        else {
            return Ok(Some(response));
        };
        let ticket = ledger.completion_ticket(session, *request_id)?;
        match &request.operation {
            Operation::TestRcon {
                instance_id,
                expected_run_generation,
            } => {
                let key = self.next_key;
                self.next_key += 1;
                let queued = match self.workers.get(instance_id) {
                    Some(worker) if worker.run_generation != *expected_run_generation => Err(
                        test_refused("The server run changed; refresh its status first"),
                    ),
                    Some(worker) => worker
                        .work
                        .try_send(Work::Test { key })
                        .map_err(|_| test_refused("RCon is busy; try again shortly")),
                    None => Err(test_refused(
                        "No running server of this instance has RCon configured in this owner",
                    )),
                };
                match queued {
                    Ok(()) => self.jobs.push(Job {
                        ticket,
                        key,
                        instance: instance_id.clone(),
                        test: true,
                    }),
                    Err(reply) => {
                        ledger.complete_pending(ticket, encode(&reply, false)?)?;
                    }
                }
            }
            Operation::SendRconCommand {
                instance_id,
                expected_run_generation,
                text,
                timeout_ms,
            } => {
                let key = self.next_key;
                self.next_key += 1;
                let outcome = self.queue_command(
                    instance_id,
                    *expected_run_generation,
                    text,
                    None,
                    *timeout_ms,
                    session.client_id(),
                    *request_id,
                    key,
                );
                match outcome {
                    Ok(()) => self.jobs.push(Job {
                        ticket,
                        key,
                        instance: instance_id.clone(),
                        test: false,
                    }),
                    Err(reply) => {
                        let succeeded = reply.error.is_none();
                        ledger.complete_pending(ticket, encode(&reply, succeeded)?)?;
                    }
                }
            }
            Operation::SendArmaAdminCommand {
                instance_id,
                expected_run_generation,
                command_json,
                timeout_ms,
            } => {
                let key = self.next_key;
                self.next_key += 1;
                let command = serde_json::from_str::<ArmaAdminCommand>(command_json)
                    .map_err(|_| command_refused("Invalid typed Arma command"));
                let outcome = command.and_then(|command| {
                    if runtime.busy() {
                        return Err(command_refused("A server lifecycle or maintenance operation is running; nothing was sent"));
                    }
                    let store = ServerStore::new(self.context.clone())
                        .map_err(|_| command_refused("The server store is unavailable"))?;
                    if !store.capabilities().arma_administration {
                        return Err(command_refused(
                            "Arma administration is unavailable in this game space",
                        ));
                    }
                    let text = command.wire_text().map_err(command_refused)?;
                    self.queue_command(
                        instance_id,
                        *expected_run_generation,
                        &text,
                        Some(command),
                        *timeout_ms,
                        session.client_id(),
                        *request_id,
                        key,
                    )
                });
                match outcome {
                    Ok(()) => self.jobs.push(Job {
                        ticket,
                        key,
                        instance: instance_id.clone(),
                        test: false,
                    }),
                    Err(reply) => {
                        let succeeded = reply.error.is_none();
                        ledger.complete_pending(ticket, encode(&reply, succeeded)?)?;
                    }
                }
            }
            _ => {}
        }
        Ok(Some(response))
    }

    #[allow(clippy::too_many_arguments)]
    fn queue_command(
        &self,
        instance_id: &str,
        expected_run_generation: u64,
        text: &str,
        guard: Option<ArmaAdminCommand>,
        timeout_ms: u32,
        client: &str,
        request_id: u64,
        key: u64,
    ) -> Result<(), RconCommandReply> {
        if rcon::carries_secret(text) {
            return Err(command_refused(
                "Commands that carry a password are refused; change the RCon password with Set RCon password",
            ));
        }
        let Some(worker) = self.workers.get(instance_id) else {
            return Err(command_refused(
                "No running server of this instance has RCon configured in this owner",
            ));
        };
        if worker.run_generation != expected_run_generation {
            return Err(command_refused(
                "The server run changed; refresh its status first",
            ));
        }
        let Ok(id) = ServerInstanceId::try_from(instance_id.to_owned()) else {
            return Err(command_refused("Invalid server instance"));
        };
        let Some(root) = self.record_root(instance_id) else {
            return Err(command_refused("The server store is unavailable"));
        };
        let command_id = records::command_id(
            &self.scope.owner_id,
            self.scope.generation,
            client,
            request_id,
        );
        let mut record = RconCommandRecord::new(
            command_id.clone(),
            &id,
            &self.scope.owner_id,
            self.scope.generation,
            expected_run_generation,
            text,
        );
        match records::claim(&root, &record) {
            Ok(true) => {}
            Ok(false) => {
                let existing = records::read(&root, &command_id).ok().flatten();
                return Err(RconCommandReply {
                    command_id: Some(command_id),
                    state: existing.as_ref().map(|record| record.state),
                    reason: existing.and_then(|record| record.reason),
                    error: None,
                });
            }
            Err(_) => {
                return Err(command_refused(
                    "The RCon command record could not be written; nothing was sent",
                ));
            }
        }
        let work = Work::Send {
            key,
            record: Box::new(record.clone()),
            timeout: Duration::from_millis(u64::from(timeout_ms)),
            guard,
        };
        if worker.work.try_send(work).is_err() {
            record.finish(
                RconCommandState::NotSent,
                Some(RconCommandReason::NotConnected),
            );
            let _ = records::update(&root, &record);
            return Err(RconCommandReply {
                command_id: Some(command_id),
                state: Some(RconCommandState::NotSent),
                reason: Some(RconCommandReason::NotConnected),
                error: Some("RCon is busy with other commands; nothing was sent".into()),
            });
        }
        Ok(())
    }

    pub(super) fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub(super) fn shutdown(&mut self, ledger: &mut Ledger) -> io::Result<()> {
        let ids: Vec<String> = self.workers.keys().cloned().collect();
        for id in ids {
            if let Some(mut worker) = self.workers.remove(&id) {
                worker.stop(STOP_WAIT);
                self.drain(&id, &worker, ledger)?;
            }
        }
        Ok(())
    }
}

struct WorkerInputs {
    context: ServerContext,
    instance: ServerInstanceId,
    plan: NativeRconPlan,
    root: PathBuf,
    shared: Arc<Mutex<Shared>>,
    queue: mpsc::Receiver<Work>,
    done: mpsc::Sender<Finished>,
    stop: Arc<AtomicBool>,
}

fn update_status(shared: &Mutex<Shared>, change: impl FnOnce(&mut RconSessionStatus)) {
    let mut shared = shared.lock().unwrap_or_else(|error| error.into_inner());
    change(&mut shared.status);
}

fn absorb(shared: &Mutex<Shared>, session: &mut Session, base: SessionCounters) {
    let messages = session.take_messages();
    let counters = session.counters();
    let mut shared = shared.lock().unwrap_or_else(|error| error.into_inner());
    for message in messages {
        if shared.messages.len() == RETAINED_MESSAGES {
            shared.messages.pop_front();
        }
        shared.messages.push_back(message);
    }
    shared.status.messages_acknowledged =
        base.messages_acknowledged + counters.messages_acknowledged;
    shared.status.duplicate_messages = base.duplicate_messages + counters.duplicate_messages;
    shared.status.invalid_datagrams = base.invalid_datagrams + counters.invalid_datagrams;
}

fn add(base: &mut SessionCounters, counters: SessionCounters) {
    base.messages_acknowledged += counters.messages_acknowledged;
    base.duplicate_messages += counters.duplicate_messages;
    base.invalid_datagrams += counters.invalid_datagrams;
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn run_worker(inputs: WorkerInputs) {
    let WorkerInputs {
        context,
        instance,
        plan,
        root,
        shared,
        queue,
        done,
        stop,
    } = inputs;
    let store = ServerStore::new(context).ok();
    let mut session: Option<Session> = None;
    let mut base = SessionCounters::default();
    let mut backoff = Duration::from_secs(1);
    let mut next_attempt = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if session.is_none() && Instant::now() >= next_attempt {
            let password = store
                .as_ref()
                .and_then(|store| {
                    store
                        .resolve_config_secret(&instance, &plan.password_reference)
                        .ok()
                })
                .and_then(|secret| RconPassword::new(secret).ok());
            let result =
                match &password {
                    Some(password) => Session::login(plan.endpoint, password, LOGIN_TIMEOUT)
                        .map_err(|failure| match failure {
                            LoginFailure::Rejected => RconConnection::Rejected,
                            LoginFailure::NoReply | LoginFailure::LocalSocket => {
                                RconConnection::NoReply
                            }
                            LoginFailure::Unreachable => RconConnection::Unreachable,
                        }),
                    None => Err(RconConnection::PasswordUnavailable),
                };
            drop(password);
            match result {
                Ok(connected) => {
                    session = Some(connected);
                    backoff = Duration::from_secs(1);
                    update_status(&shared, |status| {
                        status.connection = RconConnection::Connected;
                        status.session_generation = status.session_generation.saturating_add(1);
                        status.failed_attempts = 0;
                        status.connected_unix_ms = Some(rcon::unix_ms());
                        status.last_reply_unix_ms = status.connected_unix_ms;
                    });
                }
                Err(connection) => {
                    update_status(&shared, |status| {
                        status.connection = connection;
                        status.failed_attempts = status.failed_attempts.saturating_add(1);
                    });
                    next_attempt = Instant::now() + backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
        let work = match &mut session {
            Some(current) => {
                let healthy = current.pump(Duration::from_millis(50)).is_ok();
                absorb(&shared, current, base);
                if !healthy {
                    lose(&shared, &mut session, &mut base, &mut next_attempt);
                    continue;
                }
                match queue.try_recv() {
                    Ok(work) => Some(work),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                }
            }
            None => match queue.recv_timeout(Duration::from_millis(200)) {
                Ok(work) => Some(work),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
        };
        match work {
            Some(Work::Test { key }) => {
                let reply = match &mut session {
                    Some(current) => {
                        let started = Instant::now();
                        match current.command(b"", TEST_TIMEOUT, Retransmit::Safe) {
                            CommandOutcome::Replied(_) => {
                                let round_trip = elapsed_ms(started);
                                update_status(&shared, |status| {
                                    status.round_trip_ms = Some(round_trip);
                                    status.last_reply_unix_ms = Some(rcon::unix_ms());
                                });
                                let generation = shared
                                    .lock()
                                    .map(|shared| shared.status.session_generation)
                                    .ok();
                                RconTestReply {
                                    replied: true,
                                    round_trip_ms: Some(round_trip),
                                    session_generation: generation,
                                    error: None,
                                }
                            }
                            _ => {
                                lose(&shared, &mut session, &mut base, &mut next_attempt);
                                test_refused("The RCon session stopped answering; reconnecting")
                            }
                        }
                    }
                    None => test_refused("RCon is not connected"),
                };
                let succeeded = reply.replied;
                if let Ok(result) = encode(&reply, succeeded) {
                    let _ = done.send(Finished { key, result });
                }
            }
            Some(Work::Send {
                key,
                mut record,
                timeout,
                guard,
            }) => {
                let reply = match guard.as_ref() {
                    Some(guard) => send_guarded(
                        &shared,
                        &root,
                        &mut session,
                        &mut record,
                        timeout,
                        Some(guard),
                    ),
                    None => send(&shared, &root, &mut session, &mut record, timeout),
                };
                let _ = records::prune(&root);
                if matches!(record.state, RconCommandState::Unknown)
                    && let Some(current) = &mut session
                    && !matches!(
                        current.command(b"", TEST_TIMEOUT, Retransmit::Safe),
                        CommandOutcome::Replied(_)
                    )
                {
                    lose(&shared, &mut session, &mut base, &mut next_attempt);
                }
                let succeeded = reply.error.is_none();
                if let Ok(result) = encode(&reply, succeeded) {
                    let _ = done.send(Finished { key, result });
                }
            }
            None => {}
        }
        if let Some(current) = &mut session
            && current.idle() >= KEEPALIVE_INTERVAL
        {
            let started = Instant::now();
            match current.command(b"", TEST_TIMEOUT, Retransmit::Safe) {
                CommandOutcome::Replied(_) => {
                    let round_trip = elapsed_ms(started);
                    update_status(&shared, |status| {
                        status.round_trip_ms = Some(round_trip);
                        status.last_reply_unix_ms = Some(rcon::unix_ms());
                    });
                }
                _ => lose(&shared, &mut session, &mut base, &mut next_attempt),
            }
        }
    }
    while let Ok(work) = queue.try_recv() {
        if let Work::Send { mut record, .. } = work {
            record.finish(
                RconCommandState::NotSent,
                Some(RconCommandReason::RunChanged),
            );
            let _ = records::update(&root, &record);
        }
    }
}

fn lose(
    shared: &Mutex<Shared>,
    session: &mut Option<Session>,
    base: &mut SessionCounters,
    next_attempt: &mut Instant,
) {
    if let Some(lost) = session.take() {
        add(base, lost.counters());
    }
    update_status(shared, |status| status.connection = RconConnection::Lost);
    *next_attempt = Instant::now();
}

fn send(
    shared: &Mutex<Shared>,
    root: &std::path::Path,
    session: &mut Option<Session>,
    record: &mut RconCommandRecord,
    timeout: Duration,
) -> RconCommandReply {
    send_guarded(shared, root, session, record, timeout, None)
}

fn send_guarded(
    shared: &Mutex<Shared>,
    root: &std::path::Path,
    session: &mut Option<Session>,
    record: &mut RconCommandRecord,
    timeout: Duration,
    guard: Option<&ArmaAdminCommand>,
) -> RconCommandReply {
    let Some(current) = session else {
        record.finish(
            RconCommandState::NotSent,
            Some(RconCommandReason::NotConnected),
        );
        let _ = records::update(root, record);
        return RconCommandReply {
            command_id: Some(record.command_id.clone()),
            state: Some(record.state),
            reason: record.reason,
            error: None,
        };
    };
    if let Some(guard) = guard
        && let Some(preflight) = guard.required_preflight()
    {
        let valid = match current.command(preflight.as_bytes(), timeout, Retransmit::Never) {
            CommandOutcome::Replied(body) => guard.validate_preflight(&rcon::response_text(&body)),
            _ => Err("The current target could not be verified from the server reply".into()),
        };
        if let Err(error) = valid {
            record.finish(
                RconCommandState::NotSent,
                Some(RconCommandReason::TargetChanged),
            );
            let persisted = records::update(root, record).is_ok();
            return RconCommandReply {
                command_id: Some(record.command_id.clone()),
                state: Some(record.state),
                reason: record.reason,
                error: Some(if persisted {
                    error
                } else {
                    "The refused command outcome could not be recorded".into()
                }),
            };
        }
    }
    record.state = RconCommandState::Sending;
    record.sent_unix_ms = Some(rcon::unix_ms());
    record.session_generation = shared
        .lock()
        .map(|shared| shared.status.session_generation)
        .ok();
    if records::update(root, record).is_err() {
        return command_refused("The RCon command record could not be updated; nothing was sent");
    }
    match current.command(record.text.as_bytes(), timeout, Retransmit::Never) {
        CommandOutcome::Replied(body) => {
            record.set_response(rcon::response_text(&body));
            record.finish(RconCommandState::Replied, None);
            update_status(shared, |status| {
                status.last_reply_unix_ms = Some(rcon::unix_ms())
            });
        }
        CommandOutcome::Unknown(reason) => {
            record.finish(RconCommandState::Unknown, Some(reason.into()));
        }
        CommandOutcome::NotSent => {
            record.finish(
                RconCommandState::NotSent,
                Some(RconCommandReason::SendFailed),
            );
        }
    }
    let error = records::update(root, record)
        .err()
        .map(|_| "The RCon outcome could not be recorded; check RCon history".to_string());
    RconCommandReply {
        command_id: Some(record.command_id.clone()),
        state: Some(record.state),
        reason: record.reason,
        error,
    }
}

impl Attachment {
    fn rcon_request<T: serde::de::DeserializeOwned>(
        &self,
        operation: Operation,
        wait: Duration,
    ) -> io::Result<T> {
        let identity = self.request_identity()?;
        let client_id = identity.client_id.to_owned();
        let request_id = identity.request_id;
        let cancelled = AtomicBool::new(false);
        let deadline = Instant::now() + wait;
        let result = loop {
            let budget = transport::IoBudget {
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
                    "The RCon request outcome is unknown; check RCon history before sending it again",
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let _ = self.deployment_message(
            &client_id,
            ClientMessage::Acknowledge { request_id },
            transport::IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            },
        );
        if result.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("RCon reply is too large"));
        }
        serde_json::from_slice(&result.payload).map_err(|_| invalid("Invalid RCon reply"))
    }

    pub(crate) fn rcon_inspect(&self, instance: &ServerInstanceId) -> io::Result<RconInspectReply> {
        let cancelled = AtomicBool::new(false);
        let message = self.deployment_message(
            "metadata_controller",
            ClientMessage::Submit {
                request: Request {
                    request_id: 1,
                    operation: Operation::InspectRcon {
                        instance_id: instance.as_str().into(),
                    },
                },
            },
            transport::IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            },
        )?;
        let ServerMessage::Request {
            reply:
                RequestReply::Complete {
                    request_id: 1,
                    result,
                },
        } = message
        else {
            return Err(invalid("Owner RCon inspection was not completed"));
        };
        if result.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("RCon reply is too large"));
        }
        let reply: RconInspectReply =
            serde_json::from_slice(&result.payload).map_err(|_| invalid("Invalid RCon reply"))?;
        if let Some(status) = &reply.status
            && (status.instance_id != instance.as_str()
                || status.owner_id != self.scope().owner_id
                || status.owner_generation != self.scope().generation)
        {
            return Err(invalid("RCon status does not match the owner request"));
        }
        Ok(reply)
    }

    pub(crate) fn rcon_test(
        &self,
        instance: &ServerInstanceId,
        run_generation: u64,
    ) -> io::Result<RconTestReply> {
        self.rcon_request(
            Operation::TestRcon {
                instance_id: instance.as_str().into(),
                expected_run_generation: run_generation,
            },
            TEST_TIMEOUT + Duration::from_secs(10),
        )
    }

    pub(crate) fn arma_admin_command(
        &self,
        instance: &ServerInstanceId,
        run_generation: u64,
        command: &ArmaAdminCommand,
        timeout_ms: u32,
    ) -> io::Result<RconCommandReply> {
        command
            .wire_text()
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let command_json =
            serde_json::to_string(command).map_err(|_| invalid("Arma command encoding failed"))?;
        self.rcon_request(
            Operation::SendArmaAdminCommand {
                instance_id: instance.as_str().into(),
                expected_run_generation: run_generation,
                command_json,
                timeout_ms,
            },
            Duration::from_millis(u64::from(timeout_ms) * 2)
                + TEST_TIMEOUT
                + Duration::from_secs(15),
        )
    }

    pub(crate) fn rcon_command(
        &self,
        instance: &ServerInstanceId,
        run_generation: u64,
        text: &str,
        timeout_ms: u32,
    ) -> io::Result<RconCommandReply> {
        let reply: RconCommandReply = self.rcon_request(
            Operation::SendRconCommand {
                instance_id: instance.as_str().into(),
                expected_run_generation: run_generation,
                text: text.into(),
                timeout_ms,
            },
            Duration::from_millis(u64::from(timeout_ms)) + TEST_TIMEOUT + Duration::from_secs(15),
        )?;
        if reply
            .command_id
            .as_deref()
            .is_some_and(|id| !records::valid_command_id(id))
        {
            return Err(invalid("Invalid RCon command reply"));
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::rcon::tests::{accepting, echo, password};

    fn shared() -> Mutex<Shared> {
        Mutex::new(Shared {
            status: RconSessionStatus {
                instance_id: format!("server_{}", "3".repeat(32)),
                owner_id: "owner".into(),
                owner_generation: 1,
                run_generation: 1,
                endpoint: "127.0.0.1:1".into(),
                connection: RconConnection::Connected,
                session_generation: 1,
                failed_attempts: 0,
                connected_unix_ms: None,
                last_reply_unix_ms: None,
                round_trip_ms: None,
                messages_acknowledged: 0,
                duplicate_messages: 0,
                invalid_datagrams: 0,
                messages: Vec::new(),
            },
            messages: VecDeque::new(),
        })
    }

    fn claimed(root: &std::path::Path, request: u64, text: &str) -> RconCommandRecord {
        let id = ServerInstanceId::try_from(format!("server_{}", "3".repeat(32))).unwrap();
        let record = RconCommandRecord::new(
            records::command_id("owner", 1, "client", request),
            &id,
            "owner",
            1,
            1,
            text,
        );
        assert!(records::claim(root, &record).unwrap());
        record
    }

    #[test]
    fn owner_commands_are_recorded_before_sending_and_a_lost_reply_stays_unknown() {
        let server = accepting(|socket, peer, kind, payload| {
            if kind == 0x01 && payload[1..] != *b"say -1 lost" {
                echo(socket, peer, kind, payload);
            }
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("rcon/commands");
        let shared = shared();
        let mut session = Some(Session::login(server.address, &password(), LOGIN_TIMEOUT).unwrap());

        let mut lost = claimed(&root, 1, "say -1 lost");
        let reply = send(
            &shared,
            &root,
            &mut session,
            &mut lost,
            Duration::from_millis(1500),
        );
        assert_eq!(reply.state, Some(RconCommandState::Unknown));
        assert_eq!(reply.reason, Some(RconCommandReason::NoReply));
        assert!(reply.error.is_none());
        let stored = records::read(&root, &lost.command_id).unwrap().unwrap();
        assert_eq!(stored.state, RconCommandState::Unknown);
        assert!(stored.sent_unix_ms.is_some() && stored.session_generation == Some(1));
        let transmissions = server
            .received()
            .iter()
            .filter(|(kind, payload)| *kind == 0x01 && payload[1..] == *b"say -1 lost")
            .count();
        assert_eq!(transmissions, 1);

        let mut replied = claimed(&root, 2, "players");
        let reply = send(
            &shared,
            &root,
            &mut session,
            &mut replied,
            Duration::from_secs(2),
        );
        assert_eq!(reply.state, Some(RconCommandState::Replied));
        let stored = records::read(&root, &replied.command_id).unwrap().unwrap();
        assert_eq!(stored.response.as_deref(), Some("players"));

        let mut disconnected = None;
        let mut queued = claimed(&root, 3, "players");
        let reply = send(
            &shared,
            &root,
            &mut disconnected,
            &mut queued,
            Duration::from_secs(1),
        );
        assert_eq!(
            (reply.state, reply.reason),
            (
                Some(RconCommandState::NotSent),
                Some(RconCommandReason::NotConnected)
            )
        );
        assert_eq!(
            records::read(&root, &queued.command_id)
                .unwrap()
                .unwrap()
                .state,
            RconCommandState::NotSent
        );
    }

    #[test]
    fn inspect_replies_drop_old_messages_to_fit_the_owner_bound() {
        let shared = shared();
        let mut status = shared.lock().unwrap().status.clone();
        for index in 0..40 {
            status.messages.push(ServerText {
                received_unix_ms: index,
                text: format!("message {index} {}", "x".repeat(60)),
                truncated: false,
            });
        }
        let result = inspect_payload(&status).unwrap();
        assert!(result.payload.len() <= MAX_PAYLOAD_BYTES);
        let reply: RconInspectReply = serde_json::from_slice(&result.payload).unwrap();
        let kept = reply.status.unwrap().messages;
        assert!(!kept.is_empty() && kept.len() < 40);
        assert_eq!(kept.last().unwrap().received_unix_ms, 39);
    }

    #[test]
    fn guarded_kick_rechecks_the_live_identity_before_any_effect() {
        const GUID: &str = "0123456789abcdef0123456789abcdef";
        let server = accepting(|socket, peer, kind, payload| {
            if kind == 0x01 && payload.get(1..) == Some(&b"players"[..]) {
                let mut reply = vec![payload[0]];
                reply.extend_from_slice(format!("Players on server:\n[#] [IP Address]:[Port] [Ping] [GUID] [Name]\n--------------------------------------------------\n0 127.0.0.1:2306 -1 {GUID}(OK) PlayerAlpha\n(1 players in total)").as_bytes());
                echo(socket, peer, kind, &reply);
            } else {
                echo(socket, peer, kind, payload);
            }
        });
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("rcon/commands");
        let shared = shared();
        let mut session = Some(Session::login(server.address, &password(), LOGIN_TIMEOUT).unwrap());
        let mut wrong = claimed(&root, 8, "kick 0 ReviewedReason");
        let guard = ArmaAdminCommand::Kick {
            player_id: 0,
            expected_guid: "f".repeat(32),
            reason: "ReviewedReason".into(),
        };
        let refused = send_guarded(
            &shared,
            &root,
            &mut session,
            &mut wrong,
            Duration::from_secs(1),
            Some(&guard),
        );
        assert_eq!(refused.state, Some(RconCommandState::NotSent));
        assert_eq!(refused.reason, Some(RconCommandReason::TargetChanged));
        assert!(wrong.sent_unix_ms.is_none());
        assert!(!server.received().iter().any(|(kind, payload)| {
            *kind == 1
                && payload
                    .get(1..)
                    .is_some_and(|text| text.starts_with(b"kick"))
        }));
        let mut correct = claimed(&root, 9, "kick 0 ReviewedReason");
        let guard = ArmaAdminCommand::Kick {
            player_id: 0,
            expected_guid: GUID.into(),
            reason: "ReviewedReason".into(),
        };
        let replied = send_guarded(
            &shared,
            &root,
            &mut session,
            &mut correct,
            Duration::from_secs(1),
            Some(&guard),
        );
        assert_eq!(replied.state, Some(RconCommandState::Replied));
        assert_eq!(
            server
                .received()
                .iter()
                .filter(|(kind, payload)| *kind == 1
                    && payload
                        .get(1..)
                        .is_some_and(|text| text.starts_with(b"kick")))
                .count(),
            1
        );
    }
}
