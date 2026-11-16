use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Hands the owner's headless status to the run observer, which publishes it in the next
/// monitoring snapshot; revisions let an operation wait until its result is readable.
#[derive(Debug, Default)]
pub(in crate::core::server) struct HeadlessStatusChannel {
    status: Mutex<Option<HeadlessGroupStatus>>,
    published: AtomicU64,
    written: AtomicU64,
}

pub(in crate::core::server) type SharedHeadlessStatus = Arc<HeadlessStatusChannel>;

impl HeadlessStatusChannel {
    pub(in crate::core::server) fn publish(&self, status: HeadlessGroupStatus) {
        let mut current = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if current.as_ref() != Some(&status) {
            *current = Some(status);
            self.published.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(in crate::core::server) fn snapshot(&self) -> (Option<HeadlessGroupStatus>, u64) {
        let current = self
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (current.clone(), self.published.load(Ordering::Acquire))
    }

    pub(in crate::core::server) fn pending(&self) -> bool {
        self.written.load(Ordering::Acquire) < self.published.load(Ordering::Acquire)
    }

    pub(in crate::core::server) fn mark_written(&self, revision: u64) {
        self.written.fetch_max(revision, Ordering::AcqRel);
    }

    pub(in crate::core::server) fn wait_written(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.pending() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessClientState {
    WaitingForServer,
    Scheduled,
    Running,
    Stopped,
    RestartSuspended,
}

/// Join evidence comes only from the client's own log; a live process alone is `Pending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessConnection {
    Pending,
    Joined,
    NoEvidence,
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessReason {
    ManualStartRequired,
    StoppedByOperator,
    RestartDisabled,
    RestartBudgetExhausted,
    StartFailed,
    ServerNotRunning,
    PausedByServerStop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessExit {
    pub unix_ms: u64,
    pub exit_code: Option<u32>,
    pub forced: bool,
    pub requested: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessClientStatus {
    pub id: String,
    pub name: String,
    pub state: HeadlessClientState,
    pub wanted: bool,
    pub reason: Option<HeadlessReason>,
    pub process_id: Option<u32>,
    pub started_unix_ms: Option<u64>,
    pub connection: Option<HeadlessConnection>,
    pub joined_unix_ms: Option<u64>,
    pub starts: u32,
    pub automatic_restarts: u32,
    pub next_start_unix_ms: Option<u64>,
    pub last_exit: Option<HeadlessExit>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessGroupStatus {
    pub ready_unix_ms: Option<u64>,
    pub clients: Vec<HeadlessClientStatus>,
}

impl HeadlessGroupStatus {
    pub(crate) fn valid(&self) -> bool {
        self.clients.len() <= super::MAX_HEADLESS_CLIENTS
            && self.clients.iter().all(|client| {
                super::valid_headless_id(&client.id)
                    && crate::core::server::profile_name_valid(&client.name)
            })
    }
}
