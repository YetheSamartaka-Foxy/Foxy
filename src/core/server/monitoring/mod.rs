mod cache;
mod logs;
mod observer;
mod resources;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::InstanceRuntimeStatus;

pub(in crate::core::server) use logs::open_regular_log;
pub use logs::{LogLine, LogReadError, LogSnapshot};
pub(super) use observer::{PreparedObserver, RunObserver};
pub use resources::{ResourceSample, ResourceState};

const FRESH_MS: u64 = 10_000;
const QUERY_FRESH_MS: u64 = 15_000;

#[derive(Clone, Debug, Serialize)]
pub struct NativeMonitoringInput {
    pub endpoint: Option<SocketAddr>,
    pub expected_name: Option<String>,
    pub executable: PathBuf,
    pub log_roots: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryState {
    Unavailable,
    Pending,
    Responding,
    Timeout,
    InvalidResponse,
    NetworkError,
    IdentityMismatch,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuerySummary {
    pub name: String,
    pub map: String,
    pub players: u8,
    pub max_players: u8,
    pub bots: u8,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryAttempt {
    pub completed_unix_ms: u64,
    pub state: QueryState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueryObservation {
    pub state: QueryState,
    pub attempted_unix_ms: Option<u64>,
    pub last_success_unix_ms: Option<u64>,
    pub next_attempt_unix_ms: Option<u64>,
    pub consecutive_failures: u32,
    pub last_good: Option<QuerySummary>,
    pub attempt_history: Vec<QueryAttempt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourcePoint {
    pub sampled_unix_ms: u64,
    pub sample: ResourceSample,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitoringSnapshot {
    pub schema_version: u32,
    pub context_fingerprint: String,
    pub instance_id: super::ServerInstanceId,
    pub owner_id: String,
    pub owner_generation: u64,
    pub run_generation: u64,
    pub activation_generation: Option<u64>,
    pub runtime_id: Option<String>,
    pub sampled_unix_ms: u64,
    pub query: QueryObservation,
    pub resources: Vec<ResourcePoint>,
    pub logs: LogSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless: Option<super::HeadlessGroupStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringHealth {
    NotRunning,
    Unknown,
    Starting,
    Healthy,
    Degraded,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstanceMonitoring {
    pub status: InstanceRuntimeStatus,
    pub observation: Option<MonitoringSnapshot>,
    pub observation_age_ms: Option<u64>,
    pub query_age_ms: Option<u64>,
    pub query_ready: bool,
    pub health: MonitoringHealth,
    pub observation_unavailable: bool,
}

impl InstanceMonitoring {
    pub(crate) fn age_by(&mut self, elapsed_ms: u64) {
        self.status.observation_age_ms = self.status.observation_age_ms.saturating_add(elapsed_ms);
        self.observation_age_ms = self
            .observation_age_ms
            .map(|age| age.saturating_add(elapsed_ms));
        self.query_age_ms = self.query_age_ms.map(|age| age.saturating_add(elapsed_ms));
        if self.status.process_exists
            && self.observation.is_some()
            && (self.status.observation_age_ms > FRESH_MS
                || self.observation_age_ms.is_none_or(|age| age > FRESH_MS))
        {
            self.query_ready = false;
            self.health = MonitoringHealth::Stale;
        } else if self.query_age_ms.is_some_and(|age| age > QUERY_FRESH_MS) {
            self.query_ready = false;
            if self.status.process_exists && self.health == MonitoringHealth::Healthy {
                self.health = MonitoringHealth::Degraded;
            }
        }
    }

    pub(super) fn new(
        status: InstanceRuntimeStatus,
        observation: Option<MonitoringSnapshot>,
        now: u64,
    ) -> Self {
        let age = observation
            .as_ref()
            .map(|sample| now.saturating_sub(sample.sampled_unix_ms));
        let query_age = observation
            .as_ref()
            .and_then(|sample| sample.query.last_success_unix_ms)
            .map(|time| now.saturating_sub(time));
        let current = age.is_some_and(|age| age <= FRESH_MS)
            && observation
                .as_ref()
                .is_some_and(|sample| sample.sampled_unix_ms <= now)
            && status.observation_age_ms <= FRESH_MS;
        let query_ready = status.process_exists
            && current
            && observation
                .as_ref()
                .is_some_and(|sample| sample.query.state == QueryState::Responding)
            && query_age.is_some_and(|age| age <= QUERY_FRESH_MS)
            && observation
                .as_ref()
                .and_then(|sample| sample.query.last_success_unix_ms)
                .is_some_and(|time| time <= now);
        let health = if !status.process_exists {
            MonitoringHealth::NotRunning
        } else if observation.is_none() {
            MonitoringHealth::Unknown
        } else if !current {
            MonitoringHealth::Stale
        } else if query_ready {
            MonitoringHealth::Healthy
        } else {
            match observation
                .as_ref()
                .expect("observation exists")
                .query
                .state
            {
                QueryState::Pending => MonitoringHealth::Starting,
                QueryState::Unavailable | QueryState::Cancelled => MonitoringHealth::Unknown,
                _ => MonitoringHealth::Degraded,
            }
        };
        Self {
            status,
            observation,
            observation_age_ms: age,
            query_age_ms: query_age,
            query_ready,
            health,
            observation_unavailable: false,
        }
    }
}

pub(super) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

impl super::ServerStore {
    pub(crate) fn runtime_log_directory(
        &self,
        id: &super::ServerInstanceId,
    ) -> Result<PathBuf, super::ServerStoreError> {
        self.show(id)?;
        super::deployment_activation::launch::validate_runtime_state(
            self,
            id,
            &super::ContentStageControl::default(),
        )?;
        Ok(self.record_root(id).join("runtime-state"))
    }

    pub(crate) fn read_monitoring_snapshot(
        &self,
        status: &InstanceRuntimeStatus,
    ) -> Result<Option<MonitoringSnapshot>, super::ServerStoreError> {
        cache::read(self, status)
    }
}
