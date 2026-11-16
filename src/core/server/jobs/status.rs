use serde::Serialize;

use super::ledger::{Occurrence, OccurrenceListing, OccurrenceReason, OccurrenceState};
use super::model::{MaintenanceCatalog, MaintenanceJob};
use crate::core::server::ServerCapabilities;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct JobStatus {
    pub job: MaintenanceJob,
    pub next_run_unix_ms: Option<u64>,
    pub last: Option<Occurrence>,
    pub open: Option<Occurrence>,
    /// Why the job will not run as scheduled right now, if anything prevents it.
    pub blocker: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MaintenanceOverview {
    pub catalog_revision: u64,
    pub owner_running: bool,
    pub jobs: Vec<JobStatus>,
    pub unreadable_occurrences: usize,
    pub history_truncated: bool,
}

impl OccurrenceReason {
    pub fn describe(self) -> &'static str {
        match self {
            Self::ServerNotRunning => {
                "The server was not running; a scheduled restart never starts a stopped server."
            }
            Self::StoppedOutsideJob => {
                "The server stopped outside this job, so the job did not start it."
            }
            Self::StartedOutsideJob => {
                "The server was started outside this job after it stopped it, so it was not restarted again."
            }
            Self::PlayersPresent => "Players were connected and the job requires an empty server.",
            Self::PlayerCountUnknown => {
                "The player count was unknown and the job requires an empty server."
            }
            Self::OperationBusy => "Another server operation was running.",
            Self::AdapterUnavailable => "This action is not available for this server yet.",
            Self::MissedWindow => {
                "The scheduled time passed beyond the grace window while no owner was running."
            }
            Self::OwnerLost => "The owner ended during this occurrence; its effect is unknown.",
            Self::CancelledByOperator => "Cancelled by an operator.",
            Self::StartPlanUnavailable => "A start plan for the committed runtime was unavailable.",
            Self::AttemptFailed => "The restart did not complete.",
            Self::OverlappingOccurrence => {
                "Another occurrence of this job was still open, so this scheduled time was skipped."
            }
        }
    }
}

pub fn job_statuses(
    catalog: &MaintenanceCatalog,
    history: &OccurrenceListing,
    capabilities: &ServerCapabilities,
    owner_running: bool,
    now_unix_ms: u64,
) -> MaintenanceOverview {
    let jobs = catalog
        .jobs
        .iter()
        .map(|job| {
            let mine: Vec<&Occurrence> = history
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.job_id == job.id)
                .collect();
            let open = mine
                .iter()
                .find(|occurrence| !occurrence.state.terminal())
                .map(|occurrence| (*occurrence).clone());
            let last = mine.first().map(|occurrence| (*occurrence).clone());
            let definition = &job.definition;
            let next_run_unix_ms = definition
                .enabled
                .then(|| {
                    definition
                        .schedule
                        .next_fire_ms(now_unix_ms.max(job.revised_unix_ms), definition.clock)
                })
                .flatten();
            let blocker = if !definition.enabled {
                Some("Disabled.".to_owned())
            } else if let Some(reason) = definition.action.unavailable_reason(capabilities) {
                Some(reason.to_owned())
            } else if !owner_running {
                Some(
                    "No server owner is running for this game space, so this job is inactive."
                        .to_owned(),
                )
            } else if let Some(open) = open
                .as_ref()
                .filter(|open| open.state == OccurrenceState::Deferred)
            {
                open.reason
                    .map(|reason| format!("Deferred: {}", reason.describe()))
            } else if next_run_unix_ms.is_none() && open.is_none() {
                Some("No future scheduled time.".to_owned())
            } else {
                None
            };
            JobStatus {
                job: job.clone(),
                next_run_unix_ms,
                last,
                open,
                blocker,
            }
        })
        .collect();
    MaintenanceOverview {
        catalog_revision: catalog.revision,
        owner_running,
        jobs,
        unreadable_occurrences: history.unreadable,
        history_truncated: history.truncated,
    }
}
