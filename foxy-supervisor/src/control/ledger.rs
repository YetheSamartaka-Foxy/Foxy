use super::{
    ClientMessage, MAX_PAYLOAD_BYTES, Request, Scope, ServerMessage, ServerSession, invalid,
    valid_id,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
};

pub const MAX_CLIENTS: usize = 32;
pub const MAX_RESULTS_PER_CLIENT: usize = 32;
pub const MAX_EVENTS: usize = 128;
pub const MAX_EVENT_BATCH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationResult {
    pub succeeded: bool,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestReply {
    Accepted {
        request_id: u64,
    },
    Pending {
        request_id: u64,
    },
    Complete {
        request_id: u64,
        result: OperationResult,
    },
    Conflict {
        request_id: u64,
    },
    Expired {
        request_id: u64,
    },
    Capacity {
        request_id: u64,
    },
}

impl RequestReply {
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Accepted { request_id }
            | Self::Pending { request_id }
            | Self::Complete { request_id, .. }
            | Self::Conflict { request_id }
            | Self::Expired { request_id }
            | Self::Capacity { request_id } => *request_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub sequence: u64,
    pub kind: String,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBatch {
    pub after: u64,
    pub earliest_available: u64,
    pub head: u64,
    pub gap: bool,
    pub events: Vec<Event>,
}

struct Retained {
    request: Request,
    result: Option<OperationResult>,
}

#[derive(Default)]
struct Client {
    high_water: u64,
    requests: VecDeque<Retained>,
}

/// Keep this ledger for the entire owner generation, including disconnected clients.
/// Recreating it requires a new scope generation and capability.
pub struct Ledger {
    scope: Scope,
    clients: BTreeMap<String, Client>,
    events: VecDeque<Event>,
    head: u64,
}

pub struct CompletionTicket {
    scope: Scope,
    client_id: String,
    request: Request,
}

impl Ledger {
    pub fn new(scope: Scope) -> io::Result<Self> {
        scope.validate()?;
        Ok(Self {
            scope,
            clients: BTreeMap::new(),
            events: VecDeque::new(),
            head: 0,
        })
    }

    pub fn handle(
        &mut self,
        session: &ServerSession<'_>,
        message: ClientMessage,
    ) -> io::Result<ServerMessage> {
        self.check_session(session)?;
        self.handle_client(session.client_id(), message)
    }

    fn check_session(&self, session: &ServerSession<'_>) -> io::Result<()> {
        if session.scope() != &self.scope {
            return Err(invalid("control ledger owner mismatch"));
        }
        Ok(())
    }

    fn handle_client(
        &mut self,
        client_id: &str,
        message: ClientMessage,
    ) -> io::Result<ServerMessage> {
        validate_client(&message)?;
        if !valid_id(client_id) {
            return Err(invalid("invalid control client"));
        }
        match message {
            ClientMessage::Submit { request } => {
                if !self.clients.contains_key(client_id) && self.clients.len() >= MAX_CLIENTS {
                    return Ok(ServerMessage::Request {
                        reply: RequestReply::Capacity {
                            request_id: request.request_id,
                        },
                    });
                }
                let client = self.clients.entry(client_id.into()).or_default();
                let reply = submit(client, request);
                Ok(ServerMessage::Request { reply })
            }
            ClientMessage::Acknowledge { request_id } => {
                let client = self
                    .clients
                    .get_mut(client_id)
                    .ok_or_else(|| invalid("unknown control request"))?;
                if request_id > client.high_water {
                    return Err(invalid("unknown control request"));
                }
                if let Some(index) = client
                    .requests
                    .iter()
                    .position(|r| r.request.request_id == request_id)
                {
                    if client.requests[index].result.is_none() {
                        return Err(invalid("pending control request"));
                    }
                    client.requests.remove(index);
                }
                Ok(ServerMessage::Acknowledged { request_id })
            }
            ClientMessage::Events { after } => Ok(ServerMessage::Events {
                batch: self.events_after(after)?,
            }),
            ClientMessage::Close => Ok(ServerMessage::Closed),
        }
    }

    /// Record completion before sending it. A lost send never rolls back an operation.
    pub fn complete(
        &mut self,
        session: &ServerSession<'_>,
        request_id: u64,
        result: OperationResult,
    ) -> io::Result<RequestReply> {
        self.check_session(session)?;
        self.complete_client(session.client_id(), request_id, result)
    }

    pub fn completion_ticket(
        &self,
        session: &ServerSession<'_>,
        request_id: u64,
    ) -> io::Result<CompletionTicket> {
        self.check_session(session)?;
        self.ticket_for(session.client_id(), request_id)
    }

    fn ticket_for(&self, client_id: &str, request_id: u64) -> io::Result<CompletionTicket> {
        let retained = self
            .clients
            .get(client_id)
            .and_then(|client| {
                client
                    .requests
                    .iter()
                    .find(|retained| retained.request.request_id == request_id)
            })
            .ok_or_else(|| invalid("unknown control completion"))?;
        if retained.result.is_some() {
            return Err(invalid("control request is already complete"));
        }
        Ok(CompletionTicket {
            scope: self.scope.clone(),
            client_id: client_id.to_owned(),
            request: retained.request.clone(),
        })
    }

    pub fn complete_pending(
        &mut self,
        ticket: CompletionTicket,
        result: OperationResult,
    ) -> io::Result<RequestReply> {
        if ticket.scope != self.scope {
            return Err(invalid("control ledger owner mismatch"));
        }
        let retained = self
            .clients
            .get(&ticket.client_id)
            .and_then(|client| {
                client
                    .requests
                    .iter()
                    .find(|retained| retained.request.request_id == ticket.request.request_id)
            })
            .ok_or_else(|| invalid("unknown control completion"))?;
        if retained.request != ticket.request || retained.result.is_some() {
            return Err(invalid("control completion ticket is no longer pending"));
        }
        self.complete_client(&ticket.client_id, ticket.request.request_id, result)
    }

    fn complete_client(
        &mut self,
        client_id: &str,
        request_id: u64,
        result: OperationResult,
    ) -> io::Result<RequestReply> {
        validate_payload(&result.payload)?;
        let retained = self
            .clients
            .get_mut(client_id)
            .and_then(|c| {
                c.requests
                    .iter_mut()
                    .find(|r| r.request.request_id == request_id)
            })
            .ok_or_else(|| invalid("unknown control completion"))?;
        if let Some(previous) = &retained.result {
            if previous != &result {
                return Err(invalid("conflicting control completion"));
            }
        } else {
            retained.result = Some(result.clone());
        }
        Ok(RequestReply::Complete { request_id, result })
    }

    pub fn publish(&mut self, kind: &str, payload: Vec<u8>) -> io::Result<u64> {
        if !valid_id(kind) {
            return Err(invalid("invalid control event kind"));
        }
        validate_payload(&payload)?;
        let sequence = self
            .head
            .checked_add(1)
            .ok_or_else(|| invalid("control event sequence exhausted"))?;
        self.events.push_back(Event {
            sequence,
            kind: kind.into(),
            payload,
        });
        self.head = sequence;
        if self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        Ok(sequence)
    }

    pub fn events_after(&self, after: u64) -> io::Result<EventBatch> {
        if after > self.head {
            return Err(invalid("control event cursor ahead of owner"));
        }
        let earliest = self
            .events
            .front()
            .map_or(self.head.saturating_add(1), |e| e.sequence);
        Ok(EventBatch {
            after,
            earliest_available: earliest,
            head: self.head,
            gap: after.saturating_add(1) < earliest,
            events: self
                .events
                .iter()
                .filter(|e| e.sequence > after)
                .take(MAX_EVENT_BATCH)
                .cloned()
                .collect(),
        })
    }
}

fn submit(client: &mut Client, request: Request) -> RequestReply {
    let request_id = request.request_id;
    if let Some(previous) = client
        .requests
        .iter()
        .find(|r| r.request.request_id == request_id)
    {
        return if previous.request != request {
            RequestReply::Conflict { request_id }
        } else if let Some(result) = &previous.result {
            RequestReply::Complete {
                request_id,
                result: result.clone(),
            }
        } else {
            RequestReply::Pending { request_id }
        };
    }
    if request_id <= client.high_water {
        return RequestReply::Expired { request_id };
    }
    if client.requests.len() >= MAX_RESULTS_PER_CLIENT {
        return RequestReply::Capacity { request_id };
    }
    client.high_water = request_id;
    client.requests.push_back(Retained {
        request,
        result: None,
    });
    RequestReply::Accepted { request_id }
}

fn validate_payload(payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        Err(invalid("control payload too large"))
    } else {
        Ok(())
    }
}

pub(super) fn validate_client(message: &ClientMessage) -> io::Result<()> {
    match message {
        ClientMessage::Submit { request } => {
            if request.request_id == 0
                || matches!(
                    request.operation,
                    super::Operation::Inspect {
                        expected_revision: Some(0)
                    }
                )
            {
                return Err(invalid("invalid control request"));
            }
            if matches!(&request.operation, super::Operation::MetadataPermit { permit_id } if !valid_id(permit_id))
            {
                return Err(invalid("invalid metadata permit identity"));
            }
            match &request.operation {
                super::Operation::ActivateDeployment {
                    instance_id,
                    preparation_id,
                    expected_selection_generation,
                    review_fingerprint,
                    ..
                } if !valid_id(instance_id)
                    || !super::valid_preparation_id(preparation_id)
                    || *expected_selection_generation == 0
                    || !super::valid_lower_hex(review_fingerprint, 64) =>
                {
                    return Err(invalid("invalid deployment activation request"));
                }
                super::Operation::ResolveDeployment {
                    instance_id,
                    review_fingerprint,
                } if !valid_id(instance_id) || !super::valid_lower_hex(review_fingerprint, 64) => {
                    return Err(invalid("invalid deployment resolution request"));
                }
                super::Operation::CancelDeployment { request_id: 0 } => {
                    return Err(invalid("invalid deployment cancellation request"));
                }
                super::Operation::InspectInstance { instance_id }
                    if !super::valid_server_instance_id(instance_id) =>
                {
                    return Err(invalid("invalid server instance inspection request"));
                }
                super::Operation::StartInstance {
                    instance_id,
                    expected_activation_generation,
                    review_fingerprint,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_activation_generation == 0
                    || !super::valid_lower_hex(review_fingerprint, 64) =>
                {
                    return Err(invalid("invalid server instance start request"));
                }
                super::Operation::StopInstance {
                    instance_id,
                    expected_run_generation,
                    timeout_ms,
                    ..
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || !(1..=120_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid server instance stop request"));
                }
                super::Operation::RestartInstance {
                    instance_id,
                    expected_run_generation,
                    expected_activation_generation,
                    review_fingerprint,
                    timeout_ms,
                    ..
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || *expected_activation_generation == 0
                    || !super::valid_lower_hex(review_fingerprint, 64)
                    || !(1..=120_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid server instance restart request"));
                }
                super::Operation::StartHeadlessClients {
                    instance_id,
                    expected_run_generation,
                    headless_id,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || headless_id
                        .as_deref()
                        .is_some_and(|id| !super::valid_id(id)) =>
                {
                    return Err(invalid("invalid headless client start request"));
                }
                super::Operation::StopHeadlessClients {
                    instance_id,
                    expected_run_generation,
                    headless_id,
                    timeout_ms,
                    ..
                }
                | super::Operation::RestartHeadlessClients {
                    instance_id,
                    expected_run_generation,
                    headless_id,
                    timeout_ms,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || headless_id
                        .as_deref()
                        .is_some_and(|id| !super::valid_id(id))
                    || !(1..=120_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid headless client stop or restart request"));
                }
                super::Operation::RunMaintenanceJob {
                    instance_id,
                    job_id,
                    expected_policy_revision,
                } if !super::valid_server_instance_id(instance_id)
                    || !super::valid_prefixed_hex(job_id, "job_", 16)
                    || *expected_policy_revision == 0 =>
                {
                    return Err(invalid("invalid maintenance run request"));
                }
                super::Operation::CancelMaintenanceOccurrence {
                    instance_id,
                    occurrence_id,
                } if !super::valid_server_instance_id(instance_id)
                    || !super::valid_prefixed_hex(occurrence_id, "occ_", 32) =>
                {
                    return Err(invalid("invalid maintenance cancellation request"));
                }
                super::Operation::InspectRcon { instance_id }
                    if !super::valid_server_instance_id(instance_id) =>
                {
                    return Err(invalid("invalid RCon inspection request"));
                }
                super::Operation::TestRcon {
                    instance_id,
                    expected_run_generation,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0 =>
                {
                    return Err(invalid("invalid RCon test request"));
                }
                super::Operation::SendRconCommand {
                    instance_id,
                    expected_run_generation,
                    text,
                    timeout_ms,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || !super::valid_rcon_text(text)
                    || !(1_000..=60_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid RCon command request"));
                }
                super::Operation::SendArmaAdminCommand {
                    instance_id,
                    expected_run_generation,
                    command_json,
                    timeout_ms,
                }
                | super::Operation::SendReforgerAdminCommand {
                    instance_id,
                    expected_run_generation,
                    command_json,
                    timeout_ms,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || command_json.is_empty()
                    || command_json.len() > 768
                    || !(1_000..=60_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid Arma administration request"));
                }
                super::Operation::ReloadReforgerScenario {
                    instance_id,
                    expected_run_generation,
                    timeout_ms,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || !(1_000..=120_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid Reforger scenario reload request"));
                }
                super::Operation::RotateArmaMission {
                    instance_id,
                    expected_run_generation,
                    template,
                    timeout_ms,
                } if !super::valid_server_instance_id(instance_id)
                    || *expected_run_generation == 0
                    || template.is_empty()
                    || template.len() > 128
                    || !template
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                    || !(1_000..=120_000).contains(timeout_ms) =>
                {
                    return Err(invalid("invalid Arma mission rotation request"));
                }
                super::Operation::CancelLifecycle { request_id: 0 } => {
                    return Err(invalid("invalid lifecycle cancellation request"));
                }
                _ => {}
            }
        }
        ClientMessage::Acknowledge { request_id: 0 } => {
            return Err(invalid("invalid control acknowledgment"));
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn validate_server(message: &ServerMessage) -> io::Result<()> {
    match message {
        ServerMessage::Request { reply } => {
            let id = match reply {
                RequestReply::Accepted { request_id }
                | RequestReply::Pending { request_id }
                | RequestReply::Conflict { request_id }
                | RequestReply::Expired { request_id }
                | RequestReply::Capacity { request_id } => *request_id,
                RequestReply::Complete { request_id, result } => {
                    validate_payload(&result.payload)?;
                    *request_id
                }
            };
            if id == 0 {
                return Err(invalid("invalid control result"));
            }
        }
        ServerMessage::Events { batch } => {
            if batch.events.len() > MAX_EVENT_BATCH
                || batch.after > batch.head
                || batch.earliest_available == 0
                || batch.earliest_available > batch.head.saturating_add(1)
                || batch.gap != (batch.after.saturating_add(1) < batch.earliest_available)
            {
                return Err(invalid("invalid control event window"));
            }
            let mut previous = batch.after.max(batch.earliest_available.saturating_sub(1));
            for event in &batch.events {
                if event.sequence
                    != previous
                        .checked_add(1)
                        .ok_or_else(|| invalid("control event sequence exhausted"))?
                    || event.sequence > batch.head
                    || !valid_id(&event.kind)
                {
                    return Err(invalid("invalid control event order"));
                }
                validate_payload(&event.payload)?;
                previous = event.sequence;
            }
            if batch.events.is_empty() && batch.after < batch.head {
                return Err(invalid("missing control events"));
            }
        }
        ServerMessage::Acknowledged { request_id: 0 } => {
            return Err(invalid("invalid control acknowledgment"));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{Operation, Request};
    use super::*;

    fn ticket_request(request_id: u64, expected_revision: u64) -> Request {
        Request {
            request_id,
            operation: Operation::Inspect {
                expected_revision: Some(expected_revision),
            },
        }
    }

    fn ticket_result(payload: &[u8]) -> OperationResult {
        OperationResult {
            succeeded: true,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn detached_ticket_completes_and_retries_preserve_terminal_result() {
        let mut ledger = Ledger::new(super::super::tests::scope()).unwrap();
        let call = ticket_request(1, 7);
        assert!(matches!(
            ledger
                .handle_client(
                    "client",
                    ClientMessage::Submit {
                        request: call.clone()
                    }
                )
                .unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Accepted { request_id: 1 }
            }
        ));
        let ticket = ledger.ticket_for("client", 1).unwrap();
        let completed = ledger
            .complete_pending(ticket, ticket_result(b"done"))
            .unwrap();
        assert_eq!(
            completed,
            RequestReply::Complete {
                request_id: 1,
                result: ticket_result(b"done")
            }
        );
        assert_eq!(
            ledger
                .handle_client("client", ClientMessage::Submit { request: call })
                .unwrap(),
            ServerMessage::Request { reply: completed }
        );
        assert_eq!(
            ledger
                .handle_client(
                    "client",
                    ClientMessage::Submit {
                        request: ticket_request(1, 8)
                    }
                )
                .unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Conflict { request_id: 1 }
            }
        );
        ledger
            .handle_client("client", ClientMessage::Acknowledge { request_id: 1 })
            .unwrap();
        assert_eq!(
            ledger
                .handle_client(
                    "client",
                    ClientMessage::Submit {
                        request: ticket_request(1, 7)
                    }
                )
                .unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Expired { request_id: 1 }
            }
        );
    }

    #[test]
    fn detached_ticket_rejects_unknown_completed_wrong_scope_and_oversized_work() {
        let mut ledger = Ledger::new(super::super::tests::scope()).unwrap();
        assert!(ledger.ticket_for("missing", 1).is_err());
        ledger
            .handle_client(
                "client",
                ClientMessage::Submit {
                    request: ticket_request(1, 1),
                },
            )
            .unwrap();
        let ticket = ledger.ticket_for("client", 1).unwrap();
        assert!(
            ledger
                .complete_pending(
                    ticket,
                    OperationResult {
                        succeeded: true,
                        payload: vec![0; MAX_PAYLOAD_BYTES + 1]
                    }
                )
                .is_err()
        );
        assert!(ledger.ticket_for("client", 1).is_ok());
        let ticket = ledger.ticket_for("client", 1).unwrap();
        ledger
            .complete_pending(ticket, ticket_result(b"done"))
            .unwrap();
        assert!(ledger.ticket_for("client", 1).is_err());

        ledger
            .handle_client(
                "other",
                ClientMessage::Submit {
                    request: ticket_request(1, 1),
                },
            )
            .unwrap();
        let ticket = ledger.ticket_for("other", 1).unwrap();
        let mut other_scope = super::super::tests::scope();
        other_scope.generation += 1;
        let mut other_ledger = Ledger::new(other_scope).unwrap();
        assert!(
            other_ledger
                .complete_pending(ticket, ticket_result(b"done"))
                .is_err()
        );
        assert!(ledger.ticket_for("other", 1).is_ok());
    }
    fn scope() -> Scope {
        super::super::tests::scope()
    }
    fn request(id: u64, revision: u64) -> ClientMessage {
        ClientMessage::Submit {
            request: Request {
                request_id: id,
                operation: super::super::Operation::Inspect {
                    expected_revision: Some(revision),
                },
            },
        }
    }
    fn result() -> OperationResult {
        OperationResult {
            succeeded: true,
            payload: b"snapshot".to_vec(),
        }
    }

    #[test]
    fn lost_ack_pending_complete_conflicting_and_retired_requests_never_reexecute() {
        let mut ledger = Ledger::new(scope()).unwrap();
        let call = request(1, 5);
        assert_eq!(
            ledger.handle_client("client", call.clone()).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Accepted { request_id: 1 }
            }
        );
        assert_eq!(
            ledger.handle_client("client", call.clone()).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Pending { request_id: 1 }
            }
        );
        assert_eq!(
            ledger.handle_client("client", request(1, 6)).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Conflict { request_id: 1 }
            }
        );
        assert!(
            ledger
                .handle_client("client", ClientMessage::Acknowledge { request_id: 1 })
                .is_err()
        );
        let completed = ledger.complete_client("client", 1, result()).unwrap();
        assert_eq!(
            ledger.handle_client("client", call.clone()).unwrap(),
            ServerMessage::Request { reply: completed }
        );
        assert!(
            ledger
                .complete_client(
                    "client",
                    1,
                    OperationResult {
                        succeeded: false,
                        payload: vec![]
                    }
                )
                .is_err()
        );
        ledger
            .handle_client("client", ClientMessage::Acknowledge { request_id: 1 })
            .unwrap();
        assert_eq!(
            ledger.handle_client("client", call).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Expired { request_id: 1 }
            }
        );
    }

    #[test]
    fn capacities_preserve_unacknowledged_work_and_high_water_after_acknowledgment() {
        let mut ledger = Ledger::new(scope()).unwrap();
        for id in 1..=MAX_RESULTS_PER_CLIENT as u64 {
            ledger.handle_client("client", request(id, 1)).unwrap();
        }
        assert_eq!(
            ledger.handle_client("client", request(33, 1)).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Capacity { request_id: 33 }
            }
        );
        ledger.complete_client("client", 1, result()).unwrap();
        assert_eq!(
            ledger.handle_client("client", request(33, 1)).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Capacity { request_id: 33 }
            }
        );
        ledger
            .handle_client("client", ClientMessage::Acknowledge { request_id: 1 })
            .unwrap();
        ledger.handle_client("client", request(33, 1)).unwrap();
        assert_eq!(
            ledger.handle_client("client", request(1, 1)).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Expired { request_id: 1 }
            }
        );
        for id in 1..MAX_CLIENTS {
            ledger
                .handle_client(&format!("client_{id}"), request(1, 1))
                .unwrap();
        }
        assert_eq!(
            ledger.handle_client("overflow", request(1, 1)).unwrap(),
            ServerMessage::Request {
                reply: RequestReply::Capacity { request_id: 1 }
            }
        );
    }

    #[test]
    fn event_retention_exposes_gaps_and_bounded_ordered_reconnect_batches() {
        let mut ledger = Ledger::new(scope()).unwrap();
        for _ in 0..MAX_EVENTS + 3 {
            ledger
                .publish("changed", vec![0; MAX_PAYLOAD_BYTES])
                .unwrap();
        }
        let batch = ledger.events_after(0).unwrap();
        assert!(batch.gap);
        assert_eq!(batch.earliest_available, 4);
        assert_eq!(batch.events.len(), MAX_EVENT_BATCH);
        validate_server(&ServerMessage::Events {
            batch: batch.clone(),
        })
        .unwrap();
        let mut malformed = batch;
        malformed.events[1].sequence = malformed.events[0].sequence;
        assert!(validate_server(&ServerMessage::Events { batch: malformed }).is_err());
        assert!(ledger.events_after(132).is_err());
        assert!(
            ledger
                .publish("changed", vec![0; MAX_PAYLOAD_BYTES + 1])
                .is_err()
        );
    }
}
