use std::time::{Duration, Instant};

use log::info;

use crate::core::api;
use crate::core::utils::speed_of_light::{SolLight, sol_line};
use crate::ui::app::Foxy;

/// Track startup through the final repository verdict for the `startup-sync` busy reason and O8 `SOL op=startup` record.
pub(crate) struct StartupSyncTracker {
    dispatched_at: Instant,
    to_first_frame: Duration,
    to_dispatch: Duration,
    repositories: usize,
    eligibility_elapsed: Option<Duration>,
    eligible: usize,
    prevalidated: usize,
    remote_changed: usize,
    pending_update: usize,
    rechecks_queued: usize,
    settled: bool,
}

impl StartupSyncTracker {
    fn new(repositories: usize, to_first_frame: Duration) -> Self {
        let now = Instant::now();
        Self {
            dispatched_at: now,
            to_first_frame,
            to_dispatch: api::process_start_elapsed(),
            repositories,
            eligibility_elapsed: None,
            eligible: 0,
            prevalidated: 0,
            remote_changed: 0,
            pending_update: 0,
            rechecks_queued: 0,
            settled: false,
        }
    }
}

fn secs(value: Duration) -> String {
    format!("{:.3}", value.as_secs_f64())
}

fn startup_dependency_bound(
    first_frame: Duration,
    dispatch: Duration,
    verdict: Duration,
) -> Duration {
    first_frame.max(dispatch.saturating_add(verdict))
}

impl Foxy {
    /// Start O8 timing on the frame that dispatches startup work, after first paint.
    pub(in crate::ui::app) fn begin_startup_sync_tracking(&mut self, to_first_frame: Duration) {
        let repositories = self.repository_view_state.repositories.len();
        self.startup_sync = Some(StartupSyncTracker::new(repositories, to_first_frame));
    }

    pub(in crate::ui::app) fn note_startup_eligibility_plan(
        &mut self,
        eligible: usize,
        prevalidated: usize,
        remote_changed: usize,
        pending_update: usize,
    ) {
        if let Some(tracker) = self.startup_sync.as_mut() {
            tracker.eligibility_elapsed = Some(tracker.dispatched_at.elapsed());
            tracker.eligible = eligible;
            tracker.prevalidated = prevalidated;
            tracker.remote_changed = remote_changed;
            tracker.pending_update = pending_update;
        }
    }

    pub(in crate::ui::app) fn note_startup_rechecks_queued(&mut self, queued: usize) {
        if let Some(tracker) = self.startup_sync.as_mut() {
            tracker.rechecks_queued = tracker.rechecks_queued.saturating_add(queued);
        }
    }

    /// Keep the app busy between first paint and startup-work dispatch so an early harness connection cannot measure an idle app.
    pub(in crate::ui::app) fn startup_sync_in_progress(&self) -> bool {
        if self.database_updates_blocked() {
            return false;
        }
        if !self.startup_tasks_started {
            return true;
        }
        self.startup_sync
            .as_ref()
            .is_some_and(|tracker| !tracker.settled)
    }

    /// Wait for startup jobs to finish before scheduling opportunistic idle work, so it cannot compete with sync for disk.
    pub(in crate::ui::app) fn startup_sync_settled(&self) -> bool {
        self.startup_quick_scan_filter_worker.is_none()
            && self.startup_quick_scan_filter_rx.is_none()
            && self.startup_pending_restore_worker.is_none()
            && self.quick_scan_worker.is_none()
            && self.pending_quick_scan_urls.is_empty()
            && self.startup_recheck_queue.is_empty()
            && self.backend_worker.is_none()
            && self.syncing_repository.is_none()
    }

    pub(in crate::ui::app) fn poll_startup_diagnostics(&mut self) {
        let Some(rx) = self.startup_diagnostics_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(report) => {
                self.pending_low_space_notice = report.low_space;
                if self.client_db_admission.allows()
                    && !self.previewing_debug_modal(
                        crate::ui::app::debug_modals::DebugModal::StorageCheck,
                    )
                {
                    self.storage_compat_notice =
                        self.build_storage_compat_notice(report.storage_issues);
                }
                self.startup_diagnostics_rx = None;
                self.needs_repaint = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.startup_diagnostics_rx = None;
            }
        }
    }

    pub(in crate::ui::app) fn maybe_emit_startup_sol(&mut self) {
        if !self.startup_sync_in_progress() || !self.startup_sync_settled() {
            return;
        }
        let Some(tracker) = self.startup_sync.as_mut() else {
            return;
        };
        tracker.settled = true;
        let verdict = tracker.dispatched_at.elapsed();
        let total = api::process_start_elapsed();
        let dependency_bound =
            startup_dependency_bound(tracker.to_first_frame, tracker.to_dispatch, verdict);
        let join = total.saturating_sub(dependency_bound);
        let dependency_coverage_percent = if total.is_zero() {
            0.0
        } else {
            100.0 * dependency_bound.as_secs_f64() / total.as_secs_f64()
        };
        let quick_scan_requested = self.startup_quick_scan_requested;
        let Some(tracker) = self.startup_sync.as_ref() else {
            return;
        };
        info!(
            "{}",
            sol_line(
                "startup",
                0,
                total,
                &SolLight::SelfBaseline,
                &[
                    ("repos", tracker.repositories.to_string()),
                    ("quick_scan_repos", quick_scan_requested.to_string()),
                    ("eligible", tracker.eligible.to_string()),
                    ("prevalidated", tracker.prevalidated.to_string()),
                    ("remote_changed", tracker.remote_changed.to_string()),
                    ("pending_update", tracker.pending_update.to_string()),
                    ("rechecks", tracker.rechecks_queued.to_string()),
                    ("first_frame_s", secs(tracker.to_first_frame)),
                    ("dispatch_s", secs(tracker.to_dispatch)),
                    (
                        "eligibility_s",
                        secs(tracker.eligibility_elapsed.unwrap_or_default()),
                    ),
                    ("verdict_s", secs(verdict)),
                    ("dependency_bound_s", secs(dependency_bound)),
                    ("join_s", secs(join)),
                    (
                        "dependency_coverage_percent",
                        format!("{dependency_coverage_percent:.2}"),
                    ),
                    ("outcome", "settled".to_string()),
                    ("op_id", api::startup_operation_id().to_string()),
                ],
            )
        );
        self.needs_repaint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::startup_dependency_bound;
    use std::time::Duration;

    #[test]
    fn startup_dependency_bound_uses_overlapping_branch_completion() {
        assert_eq!(
            startup_dependency_bound(
                Duration::from_millis(450),
                Duration::from_millis(470),
                Duration::from_millis(80),
            ),
            Duration::from_millis(550)
        );
        assert_eq!(
            startup_dependency_bound(
                Duration::from_millis(600),
                Duration::from_millis(470),
                Duration::from_millis(80),
            ),
            Duration::from_millis(600)
        );
    }
}
