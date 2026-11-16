use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{
    HEADLESS_RESTART_WINDOW_SECONDS, HeadlessClientState, HeadlessLifecyclePolicy, HeadlessReason,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum Due {
    Idle,
    WaitingForServer,
    At(Instant),
    Now,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::core::server) enum ExitDecision {
    Requested,
    Restart(Duration),
    RestartDisabled,
    Suspended,
}

/// Automatic start and restart intent for one headless client within one server run.
#[derive(Clone, Debug)]
pub(in crate::core::server) struct ClientSchedule {
    policy: HeadlessLifecyclePolicy,
    wanted: bool,
    reason: Option<HeadlessReason>,
    restarts: VecDeque<Instant>,
    next_attempt: Option<Instant>,
    suspended: bool,
}

impl ClientSchedule {
    pub(in crate::core::server) fn new(
        policy: HeadlessLifecyclePolicy,
        carried: Option<bool>,
    ) -> Self {
        let wanted = carried.unwrap_or(policy.auto_start);
        let reason = (!wanted).then_some(if carried.is_some() {
            HeadlessReason::StoppedByOperator
        } else {
            HeadlessReason::ManualStartRequired
        });
        Self {
            policy,
            wanted,
            reason,
            restarts: VecDeque::new(),
            next_attempt: None,
            suspended: false,
        }
    }

    pub(in crate::core::server) fn wanted(&self) -> bool {
        self.wanted
    }

    pub(in crate::core::server) fn reason(&self) -> Option<HeadlessReason> {
        self.reason
    }

    pub(in crate::core::server) fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.policy.connect_timeout_seconds.into())
    }

    pub(in crate::core::server) fn automatic_restarts(&mut self, now: Instant) -> u32 {
        self.prune(now);
        u32::try_from(self.restarts.len()).unwrap_or(u32::MAX)
    }

    fn prune(&mut self, now: Instant) {
        let window = Duration::from_secs(HEADLESS_RESTART_WINDOW_SECONDS);
        while self
            .restarts
            .front()
            .is_some_and(|time| now.saturating_duration_since(*time) >= window)
        {
            self.restarts.pop_front();
        }
    }

    pub(in crate::core::server) fn due(
        &self,
        running: bool,
        ready_since: Option<Instant>,
        ready: bool,
        now: Instant,
    ) -> Due {
        if running || !self.wanted || self.suspended {
            return Due::Idle;
        }
        let Some(ready_since) = ready_since.filter(|_| ready) else {
            return Due::WaitingForServer;
        };
        let at = self.next_attempt.unwrap_or_else(|| {
            ready_since + Duration::from_secs(self.policy.start_delay_seconds.into())
        });
        if now >= at { Due::Now } else { Due::At(at) }
    }

    pub(in crate::core::server) fn started(&mut self) {
        self.next_attempt = None;
        self.reason = None;
    }

    pub(in crate::core::server) fn start_failed(&mut self) {
        self.next_attempt = None;
        self.suspended = true;
        self.reason = Some(HeadlessReason::StartFailed);
    }

    pub(in crate::core::server) fn exited(
        &mut self,
        requested: bool,
        now: Instant,
    ) -> ExitDecision {
        if requested || !self.wanted {
            return ExitDecision::Requested;
        }
        if self.policy.max_restarts == 0 {
            self.wanted = false;
            self.reason = Some(HeadlessReason::RestartDisabled);
            return ExitDecision::RestartDisabled;
        }
        self.prune(now);
        if self.restarts.len() >= usize::from(self.policy.max_restarts) {
            self.suspended = true;
            self.reason = Some(HeadlessReason::RestartBudgetExhausted);
            return ExitDecision::Suspended;
        }
        let delay = HeadlessLifecyclePolicy::restart_delay(self.restarts.len());
        self.restarts.push_back(now);
        self.next_attempt = Some(now + delay);
        ExitDecision::Restart(delay)
    }

    pub(in crate::core::server) fn operator_start(&mut self) {
        self.wanted = true;
        self.suspended = false;
        self.reason = None;
        self.restarts.clear();
        self.next_attempt = None;
    }

    pub(in crate::core::server) fn operator_stop(&mut self) {
        self.wanted = false;
        self.suspended = false;
        self.reason = Some(HeadlessReason::StoppedByOperator);
        self.next_attempt = None;
    }

    pub(in crate::core::server) fn state(
        &self,
        running: bool,
        ready: bool,
        server_running: bool,
    ) -> (HeadlessClientState, Option<HeadlessReason>) {
        if running {
            return (HeadlessClientState::Running, None);
        }
        if !server_running {
            return (
                HeadlessClientState::Stopped,
                self.reason.or(Some(HeadlessReason::ServerNotRunning)),
            );
        }
        if self.suspended {
            return (HeadlessClientState::RestartSuspended, self.reason);
        }
        if !self.wanted {
            return (HeadlessClientState::Stopped, self.reason);
        }
        if !ready {
            return (HeadlessClientState::WaitingForServer, None);
        }
        (HeadlessClientState::Scheduled, None)
    }
}
