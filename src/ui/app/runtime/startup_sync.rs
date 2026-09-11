use std::time::{Duration, Instant};

use log::info;

use crate::core::api;
use crate::core::utils::speed_of_light::{SolLight, sol_line};
use crate::ui::app::Foxy;

/// One launch's startup timeline, from process start to the last repository's
/// sync verdict (`conventions/SPEED_OF_LIGHT.md` O8).
///
/// It drives two things: the `startup-sync` agent-driver busy reason, which is
/// how the test kit brackets a startup measurement, and the single
/// `SOL op=startup` line that carries the whole timeline.
pub(crate) struct StartupSyncTracker {
    dispatched_at: Instant,
    to_first_frame: Duration,
    to_dispatch: Duration,
    repositories: usize,
    eligibility_elapsed: Option<Duration>,
    eligible: usize,
    prevalidated: usize,
    remote_changed: usize,
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
            rechecks_queued: 0,
            settled: false,
        }
    }
}

fn secs(value: Duration) -> String {
    format!("{:.3}", value.as_secs_f64())
}

impl Foxy {
    /// Start the O8 timeline. Called once, on the frame that dispatches startup
    /// background work (which is the frame after first paint).
    pub(in crate::ui::app) fn begin_startup_sync_tracking(&mut self, to_first_frame: Duration) {
        let repositories = self.repository_view_state.repositories.len();
        self.startup_sync = Some(StartupSyncTracker::new(repositories, to_first_frame));
    }

    pub(in crate::ui::app) fn note_startup_eligibility_plan(
        &mut self,
        eligible: usize,
        prevalidated: usize,
        remote_changed: usize,
    ) {
        if let Some(tracker) = self.startup_sync.as_mut() {
            tracker.eligibility_elapsed = Some(tracker.dispatched_at.elapsed());
            tracker.eligible = eligible;
            tracker.prevalidated = prevalidated;
            tracker.remote_changed = remote_changed;
        }
    }

    pub(in crate::ui::app) fn note_startup_rechecks_queued(&mut self, queued: usize) {
        if let Some(tracker) = self.startup_sync.as_mut() {
            tracker.rechecks_queued = tracker.rechecks_queued.saturating_add(queued);
        }
    }

    /// True until every repository has a startup verdict. Surfaced as the
    /// `startup-sync` busy reason, which is how the test kit brackets O8.
    ///
    /// Startup work is dispatched on the frame *after* first paint, so an agent
    /// that connects as soon as the first frame renders would otherwise see an
    /// idle app and measure nothing. Treat the gap as busy.
    pub(in crate::ui::app) fn startup_sync_in_progress(&self) -> bool {
        if !self.startup_tasks_started {
            return true;
        }
        self.startup_sync
            .as_ref()
            .is_some_and(|tracker| !tracker.settled)
    }

    /// Every startup-scoped background job has finished. Also the gate for
    /// opportunistic idle work, so nothing competes with sync for the disk.
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

    /// Drain the background startup system summary once it lands.
    pub(in crate::ui::app) fn poll_startup_diagnostics(&mut self) {
        let Some(rx) = self.startup_diagnostics_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(report) => {
                self.pending_low_space_notice = report.low_space;
                if !self
                    .previewing_debug_modal(crate::ui::app::debug_modals::DebugModal::StorageCheck)
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

    /// Emit `SOL op=startup` once every repository has a verdict.
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
                    ("rechecks", tracker.rechecks_queued.to_string()),
                    ("first_frame_s", secs(tracker.to_first_frame)),
                    ("dispatch_s", secs(tracker.to_dispatch)),
                    (
                        "eligibility_s",
                        secs(tracker.eligibility_elapsed.unwrap_or_default()),
                    ),
                    ("verdict_s", secs(verdict)),
                ],
            )
        );
        self.needs_repaint = true;
    }
}
