#![cfg_attr(not(test), allow(dead_code))]

use super::{DeploymentCondition, PhaseOutcome, RuntimeHealth};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OmissionPicture {
    pub phase: PhaseOutcome,
    pub deployment: DeploymentCondition,
    pub health: RuntimeHealth,
}

pub(crate) fn omitted_addon_picture(process_running: bool) -> OmissionPicture {
    OmissionPicture {
        phase: PhaseOutcome::Partial,
        deployment: DeploymentCondition::Degraded,
        health: if process_running {
            RuntimeHealth::Running
        } else {
            RuntimeHealth::Stopped
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RosterEvidence {
    Empty,
    Occupied,
    Unknown,
}

pub(crate) fn empty_server_satisfied(roster: RosterEvidence) -> bool {
    matches!(roster, RosterEvidence::Empty)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DryRunEvidence {
    Cached,
    Missing,
}

pub(crate) fn dry_run_fetches_missing(evidence: DryRunEvidence) -> bool {
    let _ = evidence;
    false
}

pub(crate) fn dry_run_is_unresolved(evidence: DryRunEvidence) -> bool {
    matches!(evidence, DryRunEvidence::Missing)
}

pub(crate) fn job_scope_authorized(preauthorized: &[&str], resolved: &[&str]) -> bool {
    !resolved.is_empty() && resolved.iter().all(|target| preauthorized.contains(target))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FollowUp {
    Restart,
    None,
}

pub(crate) fn follow_up_after_operator_stop(recovery_was_pending: bool) -> FollowUp {
    let _ = recovery_was_pending;
    FollowUp::None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InterruptClass {
    Cancelled,
    Incomplete,
}

pub(crate) fn activation_interrupt_class(deleted_old_state: bool) -> InterruptClass {
    if deleted_old_state {
        InterruptClass::Incomplete
    } else {
        InterruptClass::Cancelled
    }
}

pub(crate) fn observation_from_pid_file(pid_present: bool) -> RuntimeHealth {
    let _ = pid_present;
    RuntimeHealth::Unknown
}

pub(crate) fn start_publishes() -> bool {
    false
}

pub(crate) fn publish_starts_server() -> bool {
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpaceHolders {
    pub owner: bool,
    pub server: bool,
    pub deployment: bool,
    pub update: bool,
    pub backup: bool,
}

pub(crate) fn space_removal_allowed(holders: &SpaceHolders) -> bool {
    !holders.owner && !holders.server && !holders.deployment && !holders.update && !holders.backup
}

pub(crate) fn client_return_stops_servers(servers_running: bool) -> bool {
    let _ = servers_running;
    false
}

pub(crate) fn detach_available() -> bool {
    false
}

pub(crate) fn foreground_close_ends_supervision() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RestoreCompatibility {
    pub archive_present: bool,
    pub engine_compatible: bool,
    pub config_compatible: bool,
    pub content_compatible: bool,
    pub provider_compatible: bool,
}

pub(crate) fn restore_is_compatible(evidence: &RestoreCompatibility) -> bool {
    evidence.archive_present
        && evidence.engine_compatible
        && evidence.config_compatible
        && evidence.content_compatible
        && evidence.provider_compatible
}

pub(crate) fn marker_commits_deployment(source_complete: bool, marker_present: bool) -> bool {
    let _ = (source_complete, marker_present);
    false
}

pub(crate) fn save_now_command() -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_addons_are_partial_and_degraded() {
        let running = omitted_addon_picture(true);
        assert_eq!(running.phase, PhaseOutcome::Partial);
        assert_eq!(running.deployment, DeploymentCondition::Degraded);
        assert_eq!(running.health, RuntimeHealth::Running);
        assert_ne!(running.phase, PhaseOutcome::Succeeded);
        assert_ne!(running.deployment, DeploymentCondition::Committed);
        assert_ne!(running.deployment, DeploymentCondition::Unchanged);
        assert_eq!(omitted_addon_picture(false).health, RuntimeHealth::Stopped);
    }

    #[test]
    fn an_unknown_roster_cannot_satisfy_an_empty_server_policy() {
        assert!(empty_server_satisfied(RosterEvidence::Empty));
        assert!(!empty_server_satisfied(RosterEvidence::Occupied));
        assert!(!empty_server_satisfied(RosterEvidence::Unknown));
    }

    #[test]
    fn a_dry_run_reports_missing_evidence_as_unresolved() {
        assert!(!dry_run_fetches_missing(DryRunEvidence::Missing));
        assert!(dry_run_is_unresolved(DryRunEvidence::Missing));
        assert!(!dry_run_is_unresolved(DryRunEvidence::Cached));
    }

    #[test]
    fn an_expanded_job_scope_is_blocked() {
        assert!(job_scope_authorized(&["alpha"], &["alpha"]));
        assert!(!job_scope_authorized(&["alpha"], &["alpha", "beta"]));
        assert!(!job_scope_authorized(&["alpha"], &[]));
    }

    #[test]
    fn an_operator_stop_overrides_pending_recovery() {
        assert_eq!(follow_up_after_operator_stop(true), FollowUp::None);
        assert_eq!(follow_up_after_operator_stop(false), FollowUp::None);
        assert_ne!(follow_up_after_operator_stop(true), FollowUp::Restart);
    }

    #[test]
    fn an_activation_interrupt_after_delete_is_not_cancelled() {
        assert_eq!(activation_interrupt_class(false), InterruptClass::Cancelled);
        assert_eq!(activation_interrupt_class(true), InterruptClass::Incomplete);
    }

    #[test]
    fn a_pid_file_is_not_a_running_server() {
        assert_eq!(observation_from_pid_file(true), RuntimeHealth::Unknown);
        assert_eq!(observation_from_pid_file(false), RuntimeHealth::Unknown);
    }

    #[test]
    fn start_does_not_publish_and_publish_does_not_start() {
        assert!(!start_publishes());
        assert!(!publish_starts_server());
    }

    #[test]
    fn a_live_holder_blocks_space_removal() {
        let clear = SpaceHolders {
            owner: false,
            server: false,
            deployment: false,
            update: false,
            backup: false,
        };
        assert!(space_removal_allowed(&clear));
        for holders in [
            SpaceHolders {
                owner: true,
                ..clear
            },
            SpaceHolders {
                server: true,
                ..clear
            },
            SpaceHolders {
                deployment: true,
                ..clear
            },
            SpaceHolders {
                update: true,
                ..clear
            },
            SpaceHolders {
                backup: true,
                ..clear
            },
        ] {
            assert!(!space_removal_allowed(&holders));
        }
    }

    #[test]
    fn returning_to_the_client_does_not_stop_running_servers() {
        assert!(!client_return_stops_servers(true));
        assert!(!client_return_stops_servers(false));
    }

    #[test]
    fn detach_is_unavailable_and_foreground_close_ends_supervision() {
        assert!(!detach_available());
        assert!(foreground_close_ends_supervision());
    }

    #[test]
    fn an_archive_or_local_marker_does_not_commit_or_restore() {
        let archive_only = RestoreCompatibility {
            archive_present: true,
            engine_compatible: false,
            config_compatible: true,
            content_compatible: true,
            provider_compatible: true,
        };
        assert!(!restore_is_compatible(&archive_only));
        assert!(restore_is_compatible(&RestoreCompatibility {
            engine_compatible: true,
            ..archive_only
        }));
        assert!(!marker_commits_deployment(true, true));
        assert!(save_now_command().is_none());
    }
}
