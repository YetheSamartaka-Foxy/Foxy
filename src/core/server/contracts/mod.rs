#![cfg_attr(not(test), allow(dead_code))]

mod endpoint;
mod lifecycle;
mod policy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestAcceptance {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhaseOutcome {
    InProgress,
    Succeeded,
    Failed,
    Partial,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeploymentCondition {
    Committed,
    Degraded,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeHealth {
    Ready,
    Running,
    Stopped,
    Unknown,
    StopPending,
}
