//! Blocking-worker local control protocol. A connection grants no process or database ownership.

pub mod handoff;
mod ledger;
mod session;
pub mod transport;

pub use ledger::{CompletionTicket, Event, EventBatch, Ledger, OperationResult, RequestReply};
pub use session::{ClientSession, ServerSession};

use serde::{Deserialize, Serialize};
use std::{fmt, io};

pub const PROTOCOL_VERSION: u16 = 14;
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub context_key: String,
    pub space_id: String,
    pub instance_id: String,
    pub owner_id: String,
    pub generation: u64,
}

impl Scope {
    pub fn validate(&self) -> io::Result<()> {
        if self.generation == 0
            || self.context_key.len() != 64
            || !self.context_key.bytes().all(|b| b.is_ascii_hexdigit())
            || [&self.space_id, &self.instance_id, &self.owner_id]
                .iter()
                .any(|id| !valid_id(id))
        {
            return Err(invalid("invalid owner scope"));
        }
        Ok(())
    }
}

pub struct Capability([u8; 32]);

impl Capability {
    pub fn generate() -> Self {
        Self(rand::random())
    }

    /// Only pass through an already protected same-user handoff, never discovery or argv.
    pub fn from_bytes(bytes: [u8; 32]) -> io::Result<Self> {
        if bytes == [0; 32] {
            return Err(invalid("empty owner capability"));
        }
        Ok(Self(bytes))
    }

    pub fn expose_for_handoff(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Capability([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    DetachOwner {},
    ShutdownOwner {},
    Inspect {
        expected_revision: Option<u64>,
    },
    MetadataPermit {
        permit_id: String,
    },
    ActivateDeployment {
        instance_id: String,
        preparation_id: String,
        expected_selection_generation: u64,
        expected_activation_generation: u64,
        review_fingerprint: String,
    },
    ResolveDeployment {
        instance_id: String,
        review_fingerprint: String,
    },
    CancelDeployment {
        request_id: u64,
    },
    InspectInstance {
        instance_id: String,
    },
    StartInstance {
        instance_id: String,
        expected_activation_generation: u64,
        review_fingerprint: String,
    },
    StopInstance {
        instance_id: String,
        expected_run_generation: u64,
        force: bool,
        timeout_ms: u32,
    },
    RestartInstance {
        instance_id: String,
        expected_run_generation: u64,
        expected_activation_generation: u64,
        review_fingerprint: String,
        force: bool,
        timeout_ms: u32,
    },
    StartHeadlessClients {
        instance_id: String,
        expected_run_generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headless_id: Option<String>,
    },
    StopHeadlessClients {
        instance_id: String,
        expected_run_generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headless_id: Option<String>,
        force: bool,
        timeout_ms: u32,
    },
    RestartHeadlessClients {
        instance_id: String,
        expected_run_generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headless_id: Option<String>,
        timeout_ms: u32,
    },
    RunMaintenanceJob {
        instance_id: String,
        job_id: String,
        expected_policy_revision: u64,
    },
    CancelMaintenanceOccurrence {
        instance_id: String,
        occurrence_id: String,
    },
    InspectRcon {
        instance_id: String,
    },
    TestRcon {
        instance_id: String,
        expected_run_generation: u64,
    },
    SendRconCommand {
        instance_id: String,
        expected_run_generation: u64,
        text: String,
        timeout_ms: u32,
    },
    SendArmaAdminCommand {
        instance_id: String,
        expected_run_generation: u64,
        command_json: String,
        timeout_ms: u32,
    },
    SendReforgerAdminCommand {
        instance_id: String,
        expected_run_generation: u64,
        command_json: String,
        timeout_ms: u32,
    },
    ReloadReforgerScenario {
        instance_id: String,
        expected_run_generation: u64,
        timeout_ms: u32,
    },
    RotateArmaMission {
        instance_id: String,
        expected_run_generation: u64,
        template: String,
        timeout_ms: u32,
    },
    CancelLifecycle {
        request_id: u64,
    },
    CreateServerBackup {
        instance_id: String,
        mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_plan_fingerprint: Option<String>,
        apply_retention: bool,
    },
    PruneServerBackups {
        instance_id: String,
        expected_plan_fingerprint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keep_last_override: Option<u16>,
    },
    CancelServerBackup {
        request_id: u64,
    },
    RestoreServerBackup {
        instance_id: String,
        backup_id: String,
        expected_plan_fingerprint: String,
    },
    RollbackServerRestore {
        instance_id: String,
        expected_journal_fingerprint: String,
    },
}

fn valid_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_prefixed_hex(value: &str, prefix: &str, length: usize) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|suffix| valid_lower_hex(suffix, length))
}

fn valid_preparation_id(value: &str) -> bool {
    value
        .strip_prefix("prepared_")
        .is_some_and(|suffix| valid_lower_hex(suffix, 64))
}

fn valid_server_instance_id(value: &str) -> bool {
    value
        .strip_prefix("server_")
        .is_some_and(|suffix| valid_lower_hex(suffix, 32))
}

/// RCon command text: 1-1024 bytes, not blank, no control characters.
pub fn valid_rcon_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub request_id: u64,
    pub operation: Operation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    Submit { request: Request },
    Acknowledge { request_id: u64 },
    Events { after: u64 },
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
    Request { reply: RequestReply },
    Acknowledged { request_id: u64 },
    Events { batch: EventBatch },
    Closed,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
