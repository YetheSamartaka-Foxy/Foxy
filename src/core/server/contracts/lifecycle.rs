#![cfg_attr(not(test), allow(dead_code))]

use super::{PhaseOutcome, RequestAcceptance, RuntimeHealth};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlannedAction {
    Preview,
    SaveDraft,
    ApplyDraft,
    Start,
    Stop,
    Restart,
    Publish,
    ChangeAppMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteClass {
    LocalInspection,
    OfflineMutation,
    LiveOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerAttachment {
    Absent,
    Temporary,
    Foreground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteError {
    NeedsForegroundOwner,
    TemporaryOwnerCannotLaunch,
}

pub(crate) fn route_action(
    action: PlannedAction,
    attachment: OwnerAttachment,
) -> Result<RouteClass, RouteError> {
    match action {
        PlannedAction::Preview | PlannedAction::ChangeAppMode => Ok(RouteClass::LocalInspection),
        PlannedAction::SaveDraft | PlannedAction::ApplyDraft => Ok(RouteClass::OfflineMutation),
        PlannedAction::Start
        | PlannedAction::Stop
        | PlannedAction::Restart
        | PlannedAction::Publish => match attachment {
            OwnerAttachment::Foreground => Ok(RouteClass::LiveOwner),
            OwnerAttachment::Temporary => Err(RouteError::TemporaryOwnerCannotLaunch),
            OwnerAttachment::Absent => Err(RouteError::NeedsForegroundOwner),
        },
    }
}

pub(crate) fn server_mutations_for_app_mode_change() -> &'static [&'static str] {
    &[]
}

pub(crate) fn owner_acquire_allowed(already_held: bool) -> bool {
    !already_held
}

pub(crate) fn running_revision_after_draft_save(running: u64, saved_draft: u64) -> u64 {
    let _ = saved_draft;
    running
}

pub(crate) fn committed_start_applies_draft(revision: u64) -> bool {
    let _ = revision;
    false
}

pub(crate) fn newer_draft_start_applies_draft(committed: u64, draft: u64) -> bool {
    let _ = (committed, draft);
    false
}

pub(crate) fn start_has_committed_deployment(revision: Option<u64>) -> bool {
    matches!(revision, Some(value) if value != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanBlock {
    Stale,
    Broadened,
}

pub(crate) fn review_plan(
    expected: u64,
    current: u64,
    reviewed_deletions: u32,
    requested_deletions: u32,
) -> Result<(), PlanBlock> {
    if expected == 0 || expected != current {
        return Err(PlanBlock::Stale);
    }
    if requested_deletions != reviewed_deletions {
        return Err(PlanBlock::Broadened);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessAction {
    Restart,
    ReloadScenario,
}

pub(crate) fn restarts_process(action: ProcessAction) -> bool {
    matches!(action, ProcessAction::Restart)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservedRuntime {
    Running,
    StopPending,
    Stopped,
    Unknown,
}

pub(crate) fn ui_toggle_may_claim_running(observed: ObservedRuntime) -> bool {
    let _ = observed;
    false
}

pub(crate) fn activation_allowed(running_consumers: usize) -> bool {
    let _ = running_consumers;
    false
}

pub(crate) fn committed_revision_after_cancel(committed: u64, staged: u64) -> u64 {
    let _ = staged;
    committed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StopTimeout {
    pub release_leases: bool,
    pub allow_activation: bool,
    pub allow_removal: bool,
}

pub(crate) fn decision_after_stop_timeout() -> StopTimeout {
    StopTimeout {
        release_leases: false,
        allow_activation: false,
        allow_removal: false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShutdownKind {
    Graceful,
    ForcedUncertain,
}

pub(crate) fn shutdown_kind_after_owner_death() -> ShutdownKind {
    ShutdownKind::ForcedUncertain
}

pub(crate) fn acceptance_implies_ready(acceptance: RequestAcceptance) -> bool {
    let _ = acceptance;
    false
}

pub(crate) fn health_from_spawned_pid(pid: u32) -> RuntimeHealth {
    let _ = pid;
    RuntimeHealth::Unknown
}

pub(crate) fn is_full_success(phase: PhaseOutcome) -> bool {
    matches!(phase, PhaseOutcome::Succeeded)
}

pub(crate) fn health_after_transport_reconnect(health: RuntimeHealth) -> RuntimeHealth {
    health
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_local_and_start_needs_a_foreground_owner() {
        assert_eq!(
            route_action(PlannedAction::Preview, OwnerAttachment::Absent),
            Ok(RouteClass::LocalInspection)
        );
        assert_eq!(
            route_action(PlannedAction::SaveDraft, OwnerAttachment::Absent),
            Ok(RouteClass::OfflineMutation)
        );
        assert_eq!(
            route_action(PlannedAction::Start, OwnerAttachment::Absent),
            Err(RouteError::NeedsForegroundOwner)
        );
        assert_eq!(
            route_action(PlannedAction::Start, OwnerAttachment::Temporary),
            Err(RouteError::TemporaryOwnerCannotLaunch)
        );
        assert_eq!(
            route_action(PlannedAction::Start, OwnerAttachment::Foreground),
            Ok(RouteClass::LiveOwner)
        );
        assert_eq!(
            route_action(PlannedAction::ApplyDraft, OwnerAttachment::Absent),
            Ok(RouteClass::OfflineMutation)
        );
        assert_eq!(
            route_action(PlannedAction::Stop, OwnerAttachment::Absent),
            Err(RouteError::NeedsForegroundOwner)
        );
        assert_eq!(
            route_action(PlannedAction::Restart, OwnerAttachment::Temporary),
            Err(RouteError::TemporaryOwnerCannotLaunch)
        );
        assert_eq!(
            route_action(PlannedAction::Publish, OwnerAttachment::Foreground),
            Ok(RouteClass::LiveOwner)
        );
    }

    #[test]
    fn a_second_owner_is_rejected() {
        assert!(owner_acquire_allowed(false));
        assert!(!owner_acquire_allowed(true));
    }

    #[test]
    fn an_app_mode_change_performs_no_server_mutation() {
        assert_eq!(
            route_action(PlannedAction::ChangeAppMode, OwnerAttachment::Absent),
            Ok(RouteClass::LocalInspection)
        );
        assert!(server_mutations_for_app_mode_change().is_empty());
    }

    #[test]
    fn saving_a_draft_does_not_change_the_running_revision() {
        assert_eq!(running_revision_after_draft_save(3, 9), 3);
    }

    #[test]
    fn start_does_not_apply_a_newer_draft() {
        assert!(!committed_start_applies_draft(4));
        assert!(!newer_draft_start_applies_draft(4, 9));
    }

    #[test]
    fn start_without_a_committed_deployment_is_blocked() {
        assert!(!start_has_committed_deployment(None));
        assert!(!start_has_committed_deployment(Some(0)));
        assert!(start_has_committed_deployment(Some(4)));
    }

    #[test]
    fn a_stale_plan_does_not_broaden_scope() {
        assert_eq!(review_plan(0, 0, 1, 1), Err(PlanBlock::Stale));
        assert_eq!(review_plan(4, 5, 1, 1), Err(PlanBlock::Stale));
        assert_eq!(review_plan(4, 4, 1, 2), Err(PlanBlock::Broadened));
        assert!(review_plan(4, 4, 1, 1).is_ok());
    }

    #[test]
    fn reload_scenario_is_not_a_process_restart() {
        assert!(restarts_process(ProcessAction::Restart));
        assert!(!restarts_process(ProcessAction::ReloadScenario));
    }

    #[test]
    fn a_pending_stop_cannot_be_toggled_back_to_running() {
        assert!(!ui_toggle_may_claim_running(ObservedRuntime::StopPending));
        assert!(!ui_toggle_may_claim_running(ObservedRuntime::Unknown));
        assert!(!ui_toggle_may_claim_running(ObservedRuntime::Stopped));
        assert!(!ui_toggle_may_claim_running(ObservedRuntime::Running));
    }

    #[test]
    fn a_consumer_count_cannot_authorize_activation() {
        assert!(!activation_allowed(0));
        assert!(!activation_allowed(1));
    }

    #[test]
    fn cancel_before_stop_retains_the_committed_revision() {
        assert_eq!(committed_revision_after_cancel(7, 8), 7);
    }

    #[test]
    fn a_stop_timeout_keeps_leases() {
        let decision = decision_after_stop_timeout();
        assert!(!decision.release_leases);
        assert!(!decision.allow_activation);
        assert!(!decision.allow_removal);
    }

    #[test]
    fn owner_death_is_not_a_graceful_shutdown() {
        assert_eq!(
            shutdown_kind_after_owner_death(),
            ShutdownKind::ForcedUncertain
        );
        assert_ne!(shutdown_kind_after_owner_death(), ShutdownKind::Graceful);
    }

    #[test]
    fn an_accepted_request_is_not_ready() {
        assert!(!acceptance_implies_ready(RequestAcceptance::Accepted));
        assert!(!acceptance_implies_ready(RequestAcceptance::Rejected));
    }

    #[test]
    fn a_spawned_pid_is_not_ready() {
        assert_eq!(health_from_spawned_pid(4242), RuntimeHealth::Unknown);
        assert_ne!(health_from_spawned_pid(4242), RuntimeHealth::Ready);
    }

    #[test]
    fn a_partial_result_is_not_full_success() {
        assert!(is_full_success(PhaseOutcome::Succeeded));
        assert!(!is_full_success(PhaseOutcome::Partial));
        assert!(!is_full_success(PhaseOutcome::Failed));
        assert!(!is_full_success(PhaseOutcome::Cancelled));
        assert!(!is_full_success(PhaseOutcome::InProgress));
    }

    #[test]
    fn a_transport_reconnect_does_not_change_runtime_state() {
        assert_eq!(
            health_after_transport_reconnect(RuntimeHealth::StopPending),
            RuntimeHealth::StopPending
        );
        assert_eq!(
            health_after_transport_reconnect(RuntimeHealth::Unknown),
            RuntimeHealth::Unknown
        );
    }
}
