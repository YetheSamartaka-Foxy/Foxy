use super::ledger::OccurrenceReason;
use super::model::{MaintenanceJob, MaintenancePolicy, PlayerPolicy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum ScheduledDue {
    /// The latest scheduled time of this policy revision, still inside its grace window.
    Run { scheduled_unix_ms: u64 },
    /// The latest scheduled time passed beyond the grace window while no owner ran it.
    Missed { scheduled_unix_ms: u64 },
}

/// The owner scans every few seconds, so a smaller grace would skip every scheduled time.
const MIN_GRACE_MS: u64 = 60_000;

fn grace_ms(policy: &MaintenancePolicy) -> u64 {
    (u64::from(policy.grace_minutes) * 60_000).max(MIN_GRACE_MS)
}

/// The occurrence a job owes at `now`; its id is deterministic, so evaluating repeatedly is safe.
pub(in crate::core::server) fn scheduled_due(
    job: &MaintenanceJob,
    now_unix_ms: u64,
) -> Option<ScheduledDue> {
    if !job.definition.enabled {
        return None;
    }
    let scheduled = job
        .definition
        .schedule
        .most_recent_fire_ms(now_unix_ms, job.definition.clock)?;
    if scheduled < job.revised_unix_ms {
        return None;
    }
    Some(
        if now_unix_ms.saturating_sub(scheduled) > grace_ms(&job.definition.policy) {
            ScheduledDue::Missed {
                scheduled_unix_ms: scheduled,
            }
        } else {
            ScheduledDue::Run {
                scheduled_unix_ms: scheduled,
            }
        },
    )
}

/// A scheduled occurrence that was claimed but never started (for example while another
/// occurrence ran) is skipped once its grace and deferral allowance have both passed.
pub(in crate::core::server) fn claim_expired(
    scheduled_unix_ms: u64,
    policy: &MaintenancePolicy,
    now_unix_ms: u64,
) -> bool {
    now_unix_ms.saturating_sub(scheduled_unix_ms)
        > grace_ms(policy) + u64::from(policy.max_defer_minutes) * 60_000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum PlayerEvidence {
    Empty,
    Occupied,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) struct RestartInputs {
    pub server_running: bool,
    /// The run is in a phase another operation owns (stopping, cleanup uncertain).
    pub server_settled: bool,
    pub stop_completed_by_occurrence: bool,
    /// The run generation moved since this occurrence's last attempt, so someone else started it.
    pub started_since_stop: bool,
    pub players: PlayerEvidence,
    pub operation_busy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum RestartStep {
    /// Stop (if running) and start the same committed deployment.
    Restart,
    /// The occurrence already stopped the server; only the start remains.
    StartOnly,
    Defer(OccurrenceReason),
    Skip(OccurrenceReason),
}

/// A scheduled restart never starts a server it did not stop itself.
pub(in crate::core::server) fn restart_step(
    inputs: RestartInputs,
    policy: &MaintenancePolicy,
    deferred_since_unix_ms: Option<u64>,
    now_unix_ms: u64,
) -> RestartStep {
    if inputs.stop_completed_by_occurrence {
        if inputs.server_running || inputs.started_since_stop {
            return RestartStep::Skip(OccurrenceReason::StartedOutsideJob);
        }
        return if inputs.operation_busy {
            defer_or_skip(
                OccurrenceReason::OperationBusy,
                policy,
                deferred_since_unix_ms,
                now_unix_ms,
            )
        } else {
            RestartStep::StartOnly
        };
    }
    if !inputs.server_running {
        return RestartStep::Skip(if deferred_since_unix_ms.is_some() {
            OccurrenceReason::StoppedOutsideJob
        } else {
            OccurrenceReason::ServerNotRunning
        });
    }
    let blocker = if inputs.operation_busy || !inputs.server_settled {
        Some(OccurrenceReason::OperationBusy)
    } else {
        match (policy.player_policy, inputs.players) {
            (PlayerPolicy::Ignore, _) | (PlayerPolicy::RequireEmpty, PlayerEvidence::Empty) => None,
            (PlayerPolicy::RequireEmpty, PlayerEvidence::Occupied) => {
                Some(OccurrenceReason::PlayersPresent)
            }
            (PlayerPolicy::RequireEmpty, PlayerEvidence::Unknown) => {
                Some(OccurrenceReason::PlayerCountUnknown)
            }
        }
    };
    match blocker {
        None => RestartStep::Restart,
        Some(reason) => defer_or_skip(reason, policy, deferred_since_unix_ms, now_unix_ms),
    }
}

fn defer_or_skip(
    reason: OccurrenceReason,
    policy: &MaintenancePolicy,
    deferred_since_unix_ms: Option<u64>,
    now_unix_ms: u64,
) -> RestartStep {
    let since = deferred_since_unix_ms.unwrap_or(now_unix_ms);
    let limit = u64::from(policy.max_defer_minutes) * 60_000;
    if now_unix_ms.saturating_sub(since) >= limit {
        RestartStep::Skip(reason)
    } else {
        RestartStep::Defer(reason)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) struct AttemptResult {
    pub completed: bool,
    pub cancelled: bool,
    pub stop_completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum AfterAttempt {
    Succeeded,
    Cancelled,
    Retry { next_unix_ms: u64 },
    Failed,
}

pub(in crate::core::server) fn after_attempt(
    result: AttemptResult,
    attempts: u8,
    policy: &MaintenancePolicy,
    now_unix_ms: u64,
) -> AfterAttempt {
    if result.completed {
        AfterAttempt::Succeeded
    } else if result.cancelled {
        AfterAttempt::Cancelled
    } else if attempts < policy.max_attempts {
        AfterAttempt::Retry {
            next_unix_ms: now_unix_ms + u64::from(policy.retry_delay_seconds) * 1000,
        }
    } else {
        AfterAttempt::Failed
    }
}
