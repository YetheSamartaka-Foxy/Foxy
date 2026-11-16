mod engine;
mod manager;
mod review;

use serde::{Deserialize, Serialize};

use super::ServerInstanceId;

#[derive(Clone, Debug, Serialize)]
pub struct InstanceStartPlan {
    pub instance_id: ServerInstanceId,
    pub activation_generation: u64,
    pub runtime_id: String,
    pub fingerprint: String,
    pub owner_exit_policy: &'static str,
    pub launch: super::deployment_activation::NativeRuntimeLaunchPlan,
}

pub(super) const OWNER_EXIT_POLICY: &str = "graceful_5s_then_forced_tree_cleanup_save_uncertain";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceRuntimePhase {
    NotOwned,
    Starting,
    Running,
    StopRequested,
    Exited,
    CleanupUncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceRuntimeStatus {
    pub instance_id: ServerInstanceId,
    pub owner_id: String,
    pub owner_generation: u64,
    pub run_generation: u64,
    pub activation_generation: Option<u64>,
    pub runtime_id: Option<String>,
    pub phase: InstanceRuntimePhase,
    pub process_id: Option<u32>,
    pub process_exists: bool,
    pub readiness_verified: bool,
    pub cleanup_verified: bool,
    pub stop_timed_out: bool,
    pub forced: bool,
    pub root_exit_code: Option<u32>,
    pub observation_age_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitoring_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub headless_running: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl InstanceRuntimeStatus {
    pub(crate) fn not_owned(id: ServerInstanceId, scope: &foxy_supervisor::control::Scope) -> Self {
        Self {
            instance_id: id,
            owner_id: scope.owner_id.clone(),
            owner_generation: scope.generation,
            run_generation: 0,
            activation_generation: None,
            runtime_id: None,
            phase: InstanceRuntimePhase::NotOwned,
            process_id: None,
            process_exists: false,
            readiness_verified: false,
            cleanup_verified: false,
            stop_timed_out: false,
            forced: false,
            root_exit_code: None,
            observation_age_ms: 0,
            monitoring_fingerprint: None,
            headless_running: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOperationOutcome {
    pub status: InstanceRuntimeStatus,
    pub completed: bool,
    pub cancelled: bool,
    pub message: Option<String>,
}

pub(crate) use manager::RuntimeOwner;
