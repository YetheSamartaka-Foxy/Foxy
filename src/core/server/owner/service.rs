use super::worker::{
    DeploymentWorker, deployment_reply_error, finish_deployment, operation_instance,
    publish_deployment_progress,
};
use super::*;
use crate::core::server::{ContentStageControl, ServerStore};
use crate::core::tasks::db_process_lock::{self, LockOutcome};
use foxy_supervisor::control::{Ledger, OperationResult, ServerSession, transport::Listener};
use std::{
    fs::TryLockError,
    io::Read,
    process::{Child, ChildStdin, Command, Stdio},
    sync::atomic::{AtomicU8, Ordering},
    thread::JoinHandle,
};

mod durable;
pub(crate) use durable::start_detached;

pub(crate) struct ForegroundOwner {
    pub attachment: Attachment,
    child: Option<Child>,
    lifetime: Option<ChildStdin>,
}

impl ForegroundOwner {
    pub(crate) fn is_owned(&self) -> bool {
        self.child.is_some()
    }

    pub(crate) fn stop(&mut self) -> io::Result<()> {
        let Some(child) = &mut self.child else {
            return Ok(());
        };
        if self.attachment.inspect()?.lifetime == OwnerLifetime::Detached {
            self.lifetime.take();
            self.child.take();
            return Ok(());
        }
        self.lifetime.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                self.child.take();
                if status.success() {
                    return Ok(());
                }
                return Err(invalid("Foreground owner exited without orderly release"));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Owner is still draining metadata operations",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn detach(&mut self) -> io::Result<OwnerSnapshot> {
        durable::detach_foreground(self)
    }
}

pub(crate) fn start_or_attach(context: &ServerContext) -> io::Result<ForegroundOwner> {
    if let Ok(Some(attachment)) = attach(context) {
        return Ok(ForegroundOwner {
            attachment,
            child: None,
            lifetime: None,
        });
    }
    let launch_id = durable::launch_id();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--config-dir")
        .arg(context.data_root())
        .args([
            "server",
            "supervise",
            "--game-space",
            context.space_id(),
            "--parent-bound",
            "--launch-id",
            launch_id.as_str(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    durable::isolate_owner_process(&mut command)?;
    let mut child = command.spawn()?;
    let lifetime = child.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "Foreground owner could not start ({status})"
            )));
        }
        match attach(context) {
            Ok(Some(attachment)) => {
                let snapshot = attachment.inspect()?;
                if snapshot.launch_id.as_deref() != Some(launch_id.as_str()) {
                    drop(lifetime);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(invalid("Started owner identity did not match its launch"));
                }
                return Ok(ForegroundOwner {
                    attachment,
                    child: Some(child),
                    lifetime,
                });
            }
            Ok(None) => {}
            Err(error) if Instant::now() >= deadline => {
                drop(lifetime);
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            drop(lifetime);
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Foreground owner readiness timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
pub(crate) fn supervise(
    context: &ServerContext,
    parent_bound: bool,
    stopped: Arc<AtomicBool>,
) -> io::Result<()> {
    supervise_with_launch(context, parent_bound, None, stopped)
}

pub(crate) fn supervise_with_launch(
    context: &ServerContext,
    parent_bound: bool,
    launch_id: Option<String>,
    stopped: Arc<AtomicBool>,
) -> io::Result<()> {
    if let Some(launch_id) = &launch_id
        && (!parent_bound || !durable::valid_launch_id(launch_id) || !durable::launch_is_isolated())
    {
        return Err(invalid("Prepared owner launch identity is invalid"));
    }
    let lifetime = Arc::new(AtomicU8::new(if parent_bound {
        durable::PARENT_BOUND
    } else {
        durable::FOREGROUND
    }));
    let parent_eof = Arc::new(AtomicBool::new(false));
    let _input: Option<JoinHandle<()>> = if parent_bound {
        let parent_eof = parent_eof.clone();
        Some(std::thread::spawn(move || {
            let mut byte = [0];
            while std::io::stdin()
                .read(&mut byte)
                .is_ok_and(|count| count != 0)
            {}
            parent_eof.store(true, Ordering::Release);
        }))
    } else {
        None
    };
    serve(
        context,
        &stopped,
        LifetimeState {
            lifetime: &lifetime,
            parent_eof: &parent_eof,
            launch_id,
        },
    )
}

struct LifetimeState<'a> {
    lifetime: &'a AtomicU8,
    parent_eof: &'a AtomicBool,
    launch_id: Option<String>,
}

fn serve(
    context: &ServerContext,
    stopped: &AtomicBool,
    lifetime_state: LifetimeState<'_>,
) -> io::Result<()> {
    let (directory, name, key) = location(context, true)?;
    let directory = directory.ok_or_else(|| invalid("Owner directory unavailable"))?;
    let operation = operation_file(&directory)?;
    operation.try_lock().map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "A metadata operation is still running",
        )
    })?;
    match db_process_lock::acquire(&context.database_path()) {
        LockOutcome::Acquired => {}
        LockOutcome::Busy { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another process owns this game space",
            ));
        }
        LockOutcome::Unavailable(_) => {
            return Err(io::Error::other(
                "Game-space ownership could not be verified",
            ));
        }
    }
    let result = serve_claimed(
        context,
        stopped,
        lifetime_state,
        &directory,
        &name,
        &key,
        &operation,
    );
    db_process_lock::release_for(&context.database_path())
        .map_err(|_| invalid("Owner could not release its claim"))?;
    result
}

fn serve_claimed(
    context: &ServerContext,
    stopped: &AtomicBool,
    lifetime_state: LifetimeState<'_>,
    directory: &ProtectedDirectory,
    name: &str,
    key: &str,
    operation: &std::fs::File,
) -> io::Result<()> {
    let LifetimeState {
        lifetime,
        parent_eof,
        launch_id,
    } = lifetime_state;
    let capability = Capability::generate();
    let scope = Scope {
        context_key: key.into(),
        space_id: context.space_id().into(),
        instance_id: IDENTITY.into(),
        owner_id: format!("owner_{:032x}", rand::random::<u128>()),
        generation: rand::random::<u64>().max(1),
    };
    #[cfg(windows)]
    let endpoint = LocalEndpoint::NamedPipe(format!(r"\\.\pipe\foxy-control-{}", scope.owner_id));
    #[cfg(not(windows))]
    let endpoint = LocalEndpoint::UnixSocket(directory.path().join("owner.sock"));
    // The exclusive space claim and operation lock establish that an old owner
    // is gone; only its exact valid record may be retired here.
    if let Ok(bytes) = directory.read(name) {
        let old: OwnerRecord = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("Unknown protected owner record requires review"))?;
        old.scope.validate()?;
        Capability::from_bytes(old.capability)?;
        if old.version != RECORD_VERSION
            || old.scope.context_key != key
            || old.scope.space_id != context.space_id()
            || old.scope.instance_id != IDENTITY
        {
            return Err(invalid("Unknown protected owner record requires review"));
        }
        #[cfg(windows)]
        if old.endpoint != format!(r"\\.\pipe\foxy-control-{}", old.scope.owner_id) {
            return Err(invalid("Unknown owner endpoint requires review"));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
            let socket = directory.path().join("owner.sock");
            if old.endpoint != socket.to_string_lossy() {
                return Err(invalid("Unknown owner endpoint requires review"));
            }
            match std::fs::symlink_metadata(&socket) {
                Ok(metadata)
                    if metadata.file_type().is_socket()
                        && metadata.uid() == unsafe { libc::geteuid() }
                        && metadata.permissions().mode() & 0o777 == 0o600 =>
                {
                    std::fs::remove_file(&socket)?
                }
                Ok(_) => return Err(invalid("Unknown owner endpoint requires review")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        directory.remove_exact(name, &bytes)?;
    }
    let mut listener = Listener::bind(&endpoint)?;
    let record = OwnerRecord {
        version: RECORD_VERSION,
        scope: scope.clone(),
        capability: *capability.expose_for_handoff(),
        endpoint: match &endpoint {
            LocalEndpoint::NamedPipe(name) => name.clone(),
            LocalEndpoint::UnixSocket(path) => path.to_string_lossy().into_owned(),
        },
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| invalid("Owner record encoding failed"))?;
    directory.publish_new(name, &bytes)?;
    operation.unlock()?;
    let mut ledger = Ledger::new(scope.clone())?;
    let mut deployment: Option<DeploymentWorker> = None;
    let mut runtime = super::runtime::RuntimeService::new(context, &scope)?;
    let mut maintenance = super::maintenance::MaintenanceService::new(context, &scope);
    let result = (|| {
        while !stopped.load(Ordering::Acquire)
            && !durable::parent_eof_stops(
                match lifetime.load(Ordering::Acquire) {
                    durable::PARENT_BOUND => OwnerLifetime::ParentBound,
                    durable::DETACHED => OwnerLifetime::Detached,
                    _ => OwnerLifetime::Foreground,
                },
                parent_eof.load(Ordering::Acquire),
            )
        {
            finish_deployment(&mut deployment, &mut ledger)?;
            if let Some(completion) = runtime.finish(&mut ledger)? {
                maintenance.completed(completion, &runtime);
            }
            maintenance.tick(&mut runtime, deployment.is_some(), directory);
            publish_deployment_progress(&mut deployment, &mut ledger)?;
            let accept_budget = IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: stopped,
            };
            let connection = match listener.accept(accept_budget) {
                Ok(connection) => connection,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(_) => continue,
            };
            let budget = IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: stopped,
            };
            let mut session =
                match ServerSession::authenticate(connection, &scope, &capability, budget) {
                    Ok(session) => session,
                    Err(_) => continue,
                };
            if let Ok(message) = session.receive(budget) {
                if let ClientMessage::Submit { request } = &message
                    && matches!(
                        &request.operation,
                        Operation::DetachOwner {} | Operation::ShutdownOwner {}
                    )
                {
                    let shutting_down = matches!(&request.operation, Operation::ShutdownOwner {});
                    let succeeded = if shutting_down {
                        let idle = runtime.active_sessions() == 0
                            && !runtime.busy()
                            && deployment.is_none();
                        if idle {
                            match operation.try_lock() {
                                Ok(()) => operation.unlock().is_ok(),
                                Err(_) => false,
                            }
                        } else {
                            false
                        }
                    } else {
                        match lifetime.load(Ordering::Acquire) {
                            durable::DETACHED => true,
                            durable::PARENT_BOUND if launch_id.is_some() => {
                                lifetime.store(durable::DETACHED, Ordering::Release);
                                true
                            }
                            _ => false,
                        }
                    };
                    let snapshot = OwnerSnapshot {
                        scope: scope.clone(),
                        game_id: context.game_id().into(),
                        database_claimed: db_process_lock::holds(&context.database_path()),
                        database_opened: false,
                        metadata_permit: false,
                        active_sessions: runtime.active_sessions(),
                        lifetime: match lifetime.load(Ordering::Acquire) {
                            durable::PARENT_BOUND => OwnerLifetime::ParentBound,
                            durable::DETACHED => OwnerLifetime::Detached,
                            _ => OwnerLifetime::Foreground,
                        },
                        launch_id: launch_id.clone(),
                        lifecycle_available: crate::core::game::registry()
                            .get(context.game_id())
                            .is_some_and(|module| module.server_capabilities().lifecycle),
                    };
                    let payload = serde_json::to_vec(&snapshot)
                        .map_err(|_| invalid("Owner snapshot encoding failed"))?;
                    let reply = ServerMessage::Request {
                        reply: RequestReply::Complete {
                            request_id: request.request_id,
                            result: OperationResult { succeeded, payload },
                        },
                    };
                    if session.send(&reply, budget).is_ok()
                        && matches!(session.receive(budget), Ok(ClientMessage::Close))
                    {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                    if succeeded && shutting_down {
                        stopped.store(true, Ordering::Release);
                    }
                    continue;
                }
                if let Some(response) =
                    maintenance.handle(&session, &message, &mut ledger, &runtime)?
                {
                    let _ = session.send(&response, budget);
                    if matches!(session.receive(budget), Ok(ClientMessage::Close)) {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                    continue;
                }
                if let Some(response) = runtime.handle(
                    &session,
                    &message,
                    &mut ledger,
                    directory,
                    deployment.is_some(),
                )? {
                    let _ = session.send(&response, budget);
                    if matches!(session.receive(budget), Ok(ClientMessage::Close)) {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                    continue;
                }
                if matches!(
                    message,
                    ClientMessage::Acknowledge { .. } | ClientMessage::Events { .. }
                ) {
                    let response = ledger.handle(&session, message.clone())?;
                    let _ = session.send(&response, budget);
                    if matches!(session.receive(budget), Ok(ClientMessage::Close)) {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                    continue;
                }
                if let ClientMessage::Submit { request } = &message
                    && matches!(
                        &request.operation,
                        Operation::ActivateDeployment { .. } | Operation::ResolveDeployment { .. }
                    )
                {
                    let response = ledger.handle(&session, message.clone())?;
                    if let ServerMessage::Request {
                        reply: foxy_supervisor::control::RequestReply::Accepted { request_id },
                    } = &response
                    {
                        let ticket = ledger.completion_ticket(&session, *request_id)?;
                        if deployment.is_some() || runtime.busy() {
                            let result =
                                deployment_reply_error("Another deployment operation is running")?;
                            ledger.complete_pending(ticket, result)?;
                        } else {
                            let operation_handle = operation_file(directory)?;
                            match operation_handle.try_lock() {
                                Ok(()) => {
                                    let control = ContentStageControl::default();
                                    control.defer_terminal();
                                    let execution = match DeploymentExecution::new(
                                        context,
                                        &scope,
                                        session.client_id(),
                                        request.request_id,
                                        operation_instance(&request.operation)?,
                                        operation_handle,
                                    ) {
                                        Ok(execution) => execution,
                                        Err(_) => {
                                            ledger.complete_pending(
                                                ticket,
                                                deployment_reply_error(
                                                    "Owner deployment admission failed",
                                                )?,
                                            )?;
                                            let _ = session.send(&response, budget);
                                            continue;
                                        }
                                    };
                                    let captured = context.clone();
                                    let operation = request.operation.clone();
                                    let worker_control = control.clone();
                                    let handle = std::thread::spawn(move || {
                                        let store = ServerStore::new(captured).map_err(|_| {
                                            "Owner deployment store unavailable".to_string()
                                        })?;
                                        let result = store.execute_owner_deployment(
                                            operation,
                                            &execution,
                                            &worker_control,
                                        );
                                        worker_control.finish_operation(&result);
                                        result.map_err(|error| error.to_string())
                                    });
                                    deployment = Some(DeploymentWorker {
                                        ticket,
                                        cancel_ticket: None,
                                        handle,
                                        control,
                                        client_id: session.client_id().to_owned(),
                                        request_id: request.request_id,
                                        instance_id: match &request.operation {
                                            Operation::ActivateDeployment {
                                                instance_id, ..
                                            }
                                            | Operation::ResolveDeployment {
                                                instance_id, ..
                                            } => instance_id.clone(),
                                            _ => String::new(),
                                        },
                                        last_progress: None,
                                    });
                                }
                                Err(_) => {
                                    ledger.complete_pending(
                                        ticket,
                                        deployment_reply_error("Owner operation lock is busy")?,
                                    )?;
                                }
                            }
                        }
                    }
                    let _ = session.send(&response, budget);
                    if matches!(session.receive(budget), Ok(ClientMessage::Close)) {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                    continue;
                }
                if let ClientMessage::Submit { request } = &message
                    && let Operation::CancelDeployment { request_id: target } = &request.operation
                {
                    let response = ledger.handle(&session, message.clone())?;
                    if let ServerMessage::Request {
                        reply: foxy_supervisor::control::RequestReply::Accepted { request_id },
                    } = &response
                    {
                        let ticket = ledger.completion_ticket(&session, *request_id)?;
                        match deployment.as_mut().filter(|worker| {
                            worker.client_id == session.client_id() && worker.request_id == *target
                        }) {
                            Some(worker) if worker.cancel_ticket.is_none() => {
                                worker.control.cancel();
                                worker.cancel_ticket = Some(ticket);
                            }
                            _ => {
                                ledger.complete_pending(
                                    ticket,
                                    deployment_reply_error(
                                        "Deployment operation is no longer cancellable",
                                    )?,
                                )?;
                            }
                        }
                    }
                    let _ = session.send(&response, budget);
                    if matches!(session.receive(budget), Ok(ClientMessage::Close)) {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                    continue;
                }
                if let ClientMessage::Submit { request } = message {
                    let permit = match &request.operation {
                        Operation::Inspect { .. } => false,
                        Operation::MetadataPermit { .. } => match operation.try_lock() {
                            Err(TryLockError::WouldBlock) => true,
                            Ok(()) => {
                                operation.unlock()?;
                                false
                            }
                            Err(_) => false,
                        },
                        _ => false,
                    };
                    let snapshot = OwnerSnapshot {
                        scope: scope.clone(),
                        game_id: context.game_id().into(),
                        database_claimed: db_process_lock::holds(&context.database_path()),
                        database_opened: false,
                        metadata_permit: permit,
                        active_sessions: runtime.active_sessions(),
                        lifetime: match lifetime.load(Ordering::Acquire) {
                            durable::PARENT_BOUND => OwnerLifetime::ParentBound,
                            durable::DETACHED => OwnerLifetime::Detached,
                            _ => OwnerLifetime::Foreground,
                        },
                        launch_id: launch_id.clone(),
                        lifecycle_available: crate::core::game::registry()
                            .get(context.game_id())
                            .is_some_and(|module| module.server_capabilities().lifecycle),
                    };
                    let payload = serde_json::to_vec(&snapshot)
                        .map_err(|_| invalid("Owner snapshot encoding failed"))?;
                    let reply = ServerMessage::Request {
                        reply: RequestReply::Complete {
                            request_id: request.request_id,
                            result: OperationResult {
                                succeeded: matches!(request.operation, Operation::Inspect { .. })
                                    || permit,
                                payload,
                            },
                        },
                    };
                    if session.send(&reply, budget).is_ok()
                        && matches!(session.receive(budget), Ok(ClientMessage::Close))
                    {
                        let _ = session.send(&ServerMessage::Closed, budget);
                    }
                }
            }
        }
        if let Some(worker) = &deployment {
            worker.control.cancel();
        }
        runtime.shutdown(&mut ledger)?;
        finish_deployment(&mut deployment, &mut ledger)?;
        // A disconnected controller still owns its operation lock until its
        // retained worker completes; never release the space underneath it.
        loop {
            match operation.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => {
                    return Err(io::Error::other(
                        "Metadata operation drain could not be verified",
                    ));
                }
            }
        }
        directory.remove_exact(name, &bytes)
    })();
    let runtime_cleanup = runtime.shutdown(&mut ledger);
    if let Some(worker) = deployment.take() {
        worker.control.cancel();
        if worker.handle.join().is_err() && result.is_ok() {
            drop(listener);
            return Err(io::Error::other(
                "Owner deployment worker stopped unexpectedly",
            ));
        }
    }
    drop(listener);
    result.and(runtime_cleanup)
}
