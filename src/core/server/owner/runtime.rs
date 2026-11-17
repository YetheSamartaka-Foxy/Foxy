use super::lifecycle::LifecycleReply;
use super::*;
use crate::core::server::{
    ContentStageControl, RuntimeOperationOutcome, ServerInstanceId, lifecycle::RuntimeOwner,
};
use foxy_supervisor::control::{
    CompletionTicket, Ledger, MAX_PAYLOAD_BYTES, OperationResult, ServerSession,
};

struct Pending {
    ticket: Option<CompletionTicket>,
    cancel_ticket: Option<CompletionTicket>,
    control: ContentStageControl,
    client: String,
    request: u64,
    internal: Option<String>,
}

/// A finished owner-internal operation, keyed by the maintenance occurrence that submitted it.
pub(super) struct InternalCompletion {
    pub(super) key: String,
    pub(super) result: Result<RuntimeOperationOutcome, String>,
}

pub(super) struct RuntimeService {
    context: ServerContext,
    scope: Scope,
    owner: RuntimeOwner,
    pending: Option<Pending>,
}

fn reply(result: Result<RuntimeOperationOutcome, String>) -> io::Result<OperationResult> {
    let (outcome, error) = match result {
        Ok(outcome) => (Some(outcome), None),
        Err(error) => (None, Some(error.chars().take(160).collect())),
    };
    let succeeded = outcome.as_ref().is_some_and(|outcome| outcome.completed);
    let payload = serde_json::to_vec(&LifecycleReply { outcome, error })
        .map_err(|_| invalid("Runtime result encoding failed"))?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(invalid("Runtime result exceeds the limit"));
    }
    Ok(OperationResult { succeeded, payload })
}

impl RuntimeService {
    pub(super) fn new(context: &ServerContext, scope: &Scope) -> io::Result<Self> {
        Ok(Self {
            context: context.clone(),
            scope: scope.clone(),
            owner: RuntimeOwner::new(context.clone(), scope.clone())?,
            pending: None,
        })
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn active_sessions(&self) -> u32 {
        self.owner
            .active_sessions()
            .saturating_add(u32::from(self.busy()))
    }

    pub(super) fn finish(&mut self, ledger: &mut Ledger) -> io::Result<Option<InternalCompletion>> {
        if self.pending.is_none() {
            return Ok(None);
        }
        let Some(result) = self.owner.try_complete() else {
            return Ok(None);
        };
        let pending = self.pending.take().expect("pending runtime exists");
        if let Some(key) = pending.internal {
            return Ok(Some(InternalCompletion { key, result }));
        }
        let Some(ticket) = pending.ticket else {
            return Ok(None);
        };
        let result = match reply(result) {
            Ok(result) => result,
            Err(_) => reply(Err(
                "Runtime result is unavailable; inspect status before retrying".into(),
            ))?,
        };
        ledger.publish("lifecycle_result", result.payload.clone())?;
        ledger.complete_pending(ticket, result.clone())?;
        if let Some(ticket) = pending.cancel_ticket {
            ledger.complete_pending(ticket, result)?;
        }
        Ok(None)
    }

    pub(super) fn submit_internal(
        &mut self,
        operation: Operation,
        execution: DeploymentExecution,
        control: ContentStageControl,
        key: String,
    ) -> io::Result<()> {
        if self.pending.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another owner operation is running",
            ));
        }
        self.owner.submit(operation, execution, control.clone())?;
        self.pending = Some(Pending {
            ticket: None,
            cancel_ticket: None,
            control,
            client: String::new(),
            request: 0,
            internal: Some(key),
        });
        Ok(())
    }

    pub(super) fn cancel_internal(&self, key: &str) -> bool {
        match &self.pending {
            Some(pending) if pending.internal.as_deref() == Some(key) => {
                pending.control.cancel();
                true
            }
            _ => false,
        }
    }

    pub(super) fn rcon_targets(
        &self,
    ) -> std::collections::BTreeMap<
        String,
        (
            u64,
            crate::core::server::deployment_activation::NativeRconPlan,
        ),
    > {
        self.owner.rcon_targets()
    }

    pub(super) fn instance_status(
        &self,
        id: &ServerInstanceId,
    ) -> Option<crate::core::server::InstanceRuntimeStatus> {
        self.owner.status(id).ok()
    }

    pub(super) fn handle(
        &mut self,
        session: &ServerSession<'_>,
        message: &ClientMessage,
        ledger: &mut Ledger,
        directory: &ProtectedDirectory,
        deployment_busy: bool,
    ) -> io::Result<Option<ServerMessage>> {
        let ClientMessage::Submit { request } = message else {
            return Ok(None);
        };
        if let Operation::InspectInstance { instance_id } = &request.operation {
            let status = ServerInstanceId::try_from(instance_id.clone())
                .ok()
                .and_then(|id| self.owner.status(&id).ok());
            let payload = status
                .as_ref()
                .map(serde_json::to_vec)
                .transpose()
                .map_err(|_| invalid("Runtime status encoding failed"))?
                .unwrap_or_default();
            return Ok(Some(ServerMessage::Request {
                reply: RequestReply::Complete {
                    request_id: request.request_id,
                    result: OperationResult {
                        succeeded: status.is_some() && payload.len() <= MAX_PAYLOAD_BYTES,
                        payload,
                    },
                },
            }));
        }
        if let Operation::CancelLifecycle { request_id: target } = &request.operation {
            let response = ledger.handle(session, message.clone())?;
            if let ServerMessage::Request {
                reply: RequestReply::Accepted { request_id },
            } = &response
            {
                let ticket = ledger.completion_ticket(session, *request_id)?;
                if let Some(pending) = self.pending.as_mut().filter(|pending| {
                    pending.client == session.client_id()
                        && pending.request == *target
                        && pending.cancel_ticket.is_none()
                }) {
                    pending.control.cancel();
                    pending.cancel_ticket = Some(ticket);
                } else {
                    ledger.complete_pending(
                        ticket,
                        reply(Err("Runtime operation is no longer cancellable".into()))?,
                    )?;
                }
            }
            return Ok(Some(response));
        }
        let instance = match &request.operation {
            Operation::StartInstance { instance_id, .. }
            | Operation::StopInstance { instance_id, .. }
            | Operation::RestartInstance { instance_id, .. }
            | Operation::RotateArmaMission { instance_id, .. }
            | Operation::ReloadReforgerScenario { instance_id, .. }
            | Operation::StartHeadlessClients { instance_id, .. }
            | Operation::StopHeadlessClients { instance_id, .. }
            | Operation::RestartHeadlessClients { instance_id, .. } => instance_id,
            _ => return Ok(None),
        };
        let response = ledger.handle(session, message.clone())?;
        if let ServerMessage::Request {
            reply: RequestReply::Accepted { request_id },
        } = &response
        {
            let ticket = ledger.completion_ticket(session, *request_id)?;
            let prepare = || -> io::Result<(DeploymentExecution, ContentStageControl)> {
                if self.pending.is_some() || deployment_busy {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "Another owner operation is running",
                    ));
                }
                let id = ServerInstanceId::try_from(instance.clone())
                    .map_err(|_| invalid("Invalid lifecycle target"))?;
                let file = operation_file(directory)?;
                file.try_lock().map_err(|_| {
                    io::Error::new(io::ErrorKind::WouldBlock, "Owner operation lock is busy")
                })?;
                let execution = DeploymentExecution::new(
                    &self.context,
                    &self.scope,
                    session.client_id(),
                    request.request_id,
                    id,
                    file,
                )?;
                Ok((execution, ContentStageControl::default()))
            };
            match prepare() {
                Ok((execution, control)) => {
                    match self
                        .owner
                        .submit(request.operation.clone(), execution, control.clone())
                    {
                        Ok(()) => {
                            self.pending = Some(Pending {
                                ticket: Some(ticket),
                                cancel_ticket: None,
                                control,
                                client: session.client_id().into(),
                                request: request.request_id,
                                internal: None,
                            })
                        }
                        Err(error) => {
                            ledger.complete_pending(ticket, reply(Err(error.to_string()))?)?;
                        }
                    }
                }
                Err(error) => {
                    ledger.complete_pending(ticket, reply(Err(error.to_string()))?)?;
                }
            }
        }
        Ok(Some(response))
    }

    pub(super) fn shutdown(&mut self, ledger: &mut Ledger) -> io::Result<()> {
        if let Some(pending) = &self.pending {
            pending.control.cancel();
        }
        let stopped = self.owner.shutdown();
        self.finish(ledger)?;
        stopped
    }
}
