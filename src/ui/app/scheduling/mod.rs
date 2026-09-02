//! Execution engine for Settings -> Scheduling jobs.
//!
//! A [`crate::ui::types::ScheduledJob`] is defined and persisted in settings;
//! this module turns a due job into a serial pipeline that reuses the existing
//! sync/download machinery: an optional recheck stage (`RemoteRefreshOnly`),
//! an optional auto-approved download stage (`Download`, only for repositories
//! that have a pending update), and an optional cancellable post-action (close
//! the app or shut down the PC).
//!
//! Design notes / safety:
//! - Single-flight: only one job runs at a time, and never while a manual sync,
//!   direct download, repository purge, or schema wipe is active.
//! - The scheduler only fires while Foxy is running. A job whose time passed
//!   while the app was closed is recorded as missed (not run late) once it is
//!   outside the grace window, so a stale "download then shut down" job never
//!   fires hours after the fact.
//! - Post-actions wait out a cancellable countdown overlay; PC shutdown also
//!   uses the OS `shutdown /t` timer as a second, independent abort path.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use log::{info, warn};

use crate::core::api::SyncMode;
use crate::ui::app::Foxy;
use crate::ui::types::{
    DueState, JobRunOutcome, JobRunResult, JobSchedule, JobTarget, POST_ACTION_COUNTDOWN_SECS,
    PostAction, RepoInstanceKey, RepoState,
};

/// Which stage of the pipeline an in-progress run is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobRunStage {
    Recheck,
    Download,
    Finalize,
}

/// Live state of the scheduled job currently executing. Runtime-only; never
/// persisted (only the job definition and last-run bookkeeping are).
#[derive(Debug, Clone)]
pub struct ScheduledJobRun {
    pub job_id: String,
    pub job_name: String,
    /// Scheduled instant that triggered this run (Unix millis), recorded as the
    /// job's `last_run` so the same occurrence cannot re-fire.
    fire_ms: u64,
    post_action: PostAction,
    post_action_only_on_success: bool,
    download: bool,
    /// All resolved targets for the job, kept so the download stage can be built
    /// after the recheck stage finishes.
    targets: Vec<RepoInstanceKey>,
    stage: JobRunStage,
    /// Remaining targets in the current stage.
    queue: VecDeque<RepoInstanceKey>,
    /// Target whose sync we dispatched and are waiting on.
    in_flight: Option<RepoInstanceKey>,
    succeeded: usize,
    failed: usize,
    skipped: usize,
}

impl ScheduledJobRun {
    /// Short human-readable description of the current stage, for the status row.
    pub fn stage_label(&self) -> &'static str {
        match self.stage {
            JobRunStage::Recheck => "Rechecking",
            JobRunStage::Download => "Downloading",
            JobRunStage::Finalize => "Finishing",
        }
    }
}

/// A post-action waiting out its cancellable countdown before it fires.
#[derive(Debug, Clone)]
pub struct PendingPostAction {
    pub action: PostAction,
    pub job_name: String,
    pub deadline: Instant,
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// Issue a PC shutdown. Windows uses `shutdown /s /t 30` which shows a native
/// warning and can be aborted with `shutdown /a`, giving a second independent
/// cancel path on top of the in-app countdown.
#[cfg(target_os = "windows")]
fn initiate_pc_shutdown() {
    match std::process::Command::new("shutdown")
        .args(["/s", "/t", "30", "/c", "Foxy scheduled shutdown"])
        .spawn()
    {
        Ok(_) => info!("Issued Windows shutdown (30s grace); run 'shutdown /a' to abort"),
        Err(err) => warn!("Failed to issue Windows shutdown: {}", err),
    }
}

/// Local decision returned by a short-lived borrow of the active run, so the
/// dispatch call that follows does not hold a borrow of `self`.
enum SchedStep {
    Dispatch(RepoInstanceKey, SyncMode),
    EndRecheck,
    EndDownload,
    Finalize,
}

impl Foxy {
    /// Per-frame scheduler tick. Drives a pending post-action countdown, advances
    /// an in-progress run, or starts the next due job. Cheap when idle.
    pub fn process_scheduled_jobs(&mut self, ctx: &egui::Context) {
        // Only run once the app has finished its startup work, so a job can never
        // fire during the first frames before state is ready.
        if !self.startup_tasks_started {
            return;
        }

        self.process_scheduled_post_action(ctx);

        // While a post-action counts down, do not advance or start jobs.
        if self.scheduler_pending_post_action.is_some() {
            return;
        }

        if self.scheduler_active_run.is_some() {
            self.advance_scheduled_job_run();
            return;
        }

        // Never begin new work while the app is shutting down.
        if self.close_requested_at.is_some() {
            return;
        }

        let now = Local::now();
        let Some((job_idx, due)) = self.find_due_scheduled_job(now) else {
            return;
        };
        match due {
            DueState::Missed { fire_ms } => self.mark_scheduled_job_missed(job_idx, fire_ms),
            DueState::Due { fire_ms } => {
                if self.scheduled_job_blocked() {
                    return; // defer; retried on a later tick
                }
                self.begin_scheduled_job_run(job_idx, fire_ms);
            }
            DueState::NotDue => {}
        }
    }

    /// The interval at which the loop should wake to service the scheduler, or
    /// `None` when there is nothing scheduled. Capped so a hidden/idle app still
    /// fires jobs near their time without burning frames.
    pub(in crate::ui::app) fn scheduler_repaint_interval(&self) -> Option<Duration> {
        if self.scheduler_pending_post_action.is_some() || self.scheduler_active_run.is_some() {
            return Some(Duration::from_millis(250));
        }
        let now = Local::now();
        let mut soonest: Option<chrono::Duration> = None;
        for job in &self.settings_view_state.scheduled_jobs {
            if !job.enabled {
                continue;
            }
            if let Some(next) = job.schedule.next_fire_at(now) {
                let delta = next - now;
                match soonest {
                    Some(current) if current <= delta => {}
                    _ => soonest = Some(delta),
                }
            }
        }
        let delta = soonest?;
        let millis = delta.num_milliseconds().clamp(1000, 30_000) as u64;
        Some(Duration::from_millis(millis))
    }

    /// True while any condition makes it unsafe to start a scheduled sync.
    fn scheduled_job_blocked(&self) -> bool {
        self.repository_sync_active()
            || self.is_direct_download_running()
            || !self.pending_repository_db_wipes.is_empty()
            || self.pending_db_schema_wipe.is_some()
            || self.db_lock_conflict.is_some()
    }

    fn find_due_scheduled_job(&self, now: DateTime<Local>) -> Option<(usize, DueState)> {
        for (idx, job) in self.settings_view_state.scheduled_jobs.iter().enumerate() {
            match job.due_state(now) {
                DueState::NotDue => {}
                other => return Some((idx, other)),
            }
        }
        None
    }

    fn mark_scheduled_job_missed(&mut self, job_idx: usize, fire_ms: u64) {
        let Some(job) = self.settings_view_state.scheduled_jobs.get(job_idx) else {
            return;
        };
        let (id, name) = (job.id.clone(), job.name.clone());
        warn!(
            "Scheduled job '{}' missed its run (Foxy was not running within the grace window)",
            name
        );
        self.record_scheduled_job_result(&id, fire_ms, JobRunOutcome::Missed, 0, 0, 0);
    }

    /// Manually trigger a job now (the "Run now" button), ignoring its schedule
    /// but still respecting the single-flight guards.
    pub fn run_scheduled_job_now(&mut self, job_idx: usize) -> bool {
        if self.scheduler_active_run.is_some() || self.scheduler_pending_post_action.is_some() {
            self.show_error_toast(self.t("A scheduled job is already running."));
            return false;
        }
        if self.scheduled_job_blocked() {
            self.show_error_toast(
                self.t("Cannot start the job while another sync is in progress."),
            );
            return false;
        }
        // For a one-time job, anchor the run to its scheduled instant so a manual
        // "Run now" also marks that occurrence handled and it cannot fire again on
        // schedule (important when the post-action is a shutdown). Recurring jobs
        // anchor to now so only the next future occurrence remains.
        let fire_ms = match self.settings_view_state.scheduled_jobs.get(job_idx) {
            Some(job) => match &job.schedule {
                JobSchedule::OnceAt { unix_ms } => *unix_ms,
                JobSchedule::Recurring { .. } => now_unix_ms(),
            },
            None => return false,
        };
        self.begin_scheduled_job_run(job_idx, fire_ms);
        self.scheduler_active_run.is_some()
    }

    fn begin_scheduled_job_run(&mut self, job_idx: usize, fire_ms: u64) {
        let Some(job) = self
            .settings_view_state
            .scheduled_jobs
            .get(job_idx)
            .cloned()
        else {
            return;
        };
        let targets = self.resolve_scheduled_targets(&job.target);
        if targets.is_empty() {
            warn!(
                "Scheduled job '{}' has no resolvable targets; recording skip",
                job.name
            );
            self.record_scheduled_job_result(&job.id, fire_ms, JobRunOutcome::Skipped, 0, 0, 0);
            return;
        }

        let stage = if job.recheck {
            JobRunStage::Recheck
        } else if job.download {
            JobRunStage::Download
        } else {
            JobRunStage::Finalize
        };
        let queue = match stage {
            JobRunStage::Recheck => targets.iter().cloned().collect(),
            JobRunStage::Download => self.build_scheduled_download_queue(&targets),
            JobRunStage::Finalize => VecDeque::new(),
        };

        info!(
            "Scheduled job '{}' started: targets={} recheck={} download={} post_action={:?}",
            job.name,
            targets.len(),
            job.recheck,
            job.download,
            job.post_action
        );

        self.scheduler_active_run = Some(ScheduledJobRun {
            job_id: job.id,
            job_name: job.name,
            fire_ms,
            post_action: job.post_action,
            post_action_only_on_success: job.post_action_only_on_success,
            download: job.download,
            targets,
            stage,
            queue,
            in_flight: None,
            succeeded: 0,
            failed: 0,
            skipped: 0,
        });
    }

    /// Resolve a job target to concrete `(remote_url, local_path)` instances.
    /// Space members are resolved at run time; vanished repositories are dropped
    /// here and skipped at dispatch.
    fn resolve_scheduled_targets(&self, target: &JobTarget) -> Vec<RepoInstanceKey> {
        match target {
            JobTarget::Repository(key) => vec![key.clone()],
            JobTarget::Custom { repos } => repos.clone(),
            JobTarget::Space { space_id } => self
                .collect_repository_space_sync_targets(space_id)
                .into_iter()
                .filter_map(|idx| self.repository_view_state.repositories.get(idx))
                .map(|repo| RepoInstanceKey {
                    remote_url: repo.address.clone(),
                    local_path: repo.path.clone(),
                })
                .collect(),
        }
    }

    /// Targets that currently have a pending update, in target order. Only these
    /// are downloaded, so the download stage never re-fetches up-to-date repos.
    fn build_scheduled_download_queue(
        &self,
        targets: &[RepoInstanceKey],
    ) -> VecDeque<RepoInstanceKey> {
        targets
            .iter()
            .filter(|key| {
                self.repo_state_for_address(&key.remote_url, &key.local_path)
                    == RepoState::PendingUpdate
            })
            .cloned()
            .collect()
    }

    fn advance_scheduled_job_run(&mut self) {
        // Wait while any sync/download is in flight (ours, or a manual one).
        if self.repository_sync_active() || self.is_direct_download_running() {
            return;
        }

        loop {
            let step = {
                let Some(run) = self.scheduler_active_run.as_mut() else {
                    return;
                };
                // The completion hook clears in_flight; clear defensively in case
                // a dispatched sync never reported back.
                run.in_flight = None;
                match run.stage {
                    JobRunStage::Recheck => match run.queue.pop_front() {
                        Some(target) => SchedStep::Dispatch(target, SyncMode::RemoteRefreshOnly),
                        None => SchedStep::EndRecheck,
                    },
                    JobRunStage::Download => match run.queue.pop_front() {
                        Some(target) => SchedStep::Dispatch(target, SyncMode::Download),
                        None => SchedStep::EndDownload,
                    },
                    JobRunStage::Finalize => SchedStep::Finalize,
                }
            };

            match step {
                SchedStep::Dispatch(target, mode) => {
                    if self.start_scheduled_sync(&target, mode) {
                        if let Some(run) = self.scheduler_active_run.as_mut() {
                            run.in_flight = Some(target);
                        }
                        return; // wait for completion
                    }
                    if let Some(run) = self.scheduler_active_run.as_mut() {
                        run.skipped += 1;
                    }
                    // loop to the next target
                }
                SchedStep::EndRecheck => self.transition_scheduled_to_download(),
                SchedStep::EndDownload => {
                    if let Some(run) = self.scheduler_active_run.as_mut() {
                        run.stage = JobRunStage::Finalize;
                    }
                }
                SchedStep::Finalize => {
                    self.finalize_scheduled_job_run();
                    return;
                }
            }
        }
    }

    fn transition_scheduled_to_download(&mut self) {
        let (download, targets) = match self.scheduler_active_run.as_ref() {
            Some(run) => (run.download, run.targets.clone()),
            None => return,
        };
        if !download {
            if let Some(run) = self.scheduler_active_run.as_mut() {
                run.stage = JobRunStage::Finalize;
            }
            return;
        }
        let queue = self.build_scheduled_download_queue(&targets);
        if let Some(run) = self.scheduler_active_run.as_mut() {
            run.stage = JobRunStage::Download;
            run.queue = queue;
        }
    }

    /// Dispatch a single sync for a scheduled target. Returns true when a sync
    /// actually started (so the caller waits for completion).
    fn start_scheduled_sync(&mut self, target: &RepoInstanceKey, mode: SyncMode) -> bool {
        let normalized_url = Self::normalize_repo_url(&target.remote_url);
        let path_key = Self::repo_instance_path_key(&target.local_path);
        let Some(idx) = self.repo_index_by_url_and_path(&normalized_url, &path_key) else {
            warn!(
                "Scheduled job target no longer exists; skipping {}",
                normalized_url
            );
            return false;
        };
        self.start_core_sync(idx, mode);
        self.syncing_repository == Some(idx)
    }

    /// Hook called from the sync-completion path to count a scheduled target's
    /// result against the active run. No-op when no scheduled job is running or
    /// the completion does not match the in-flight target/stage.
    pub(in crate::ui::app) fn record_scheduled_job_completion(
        &mut self,
        repo_idx: Option<usize>,
        mode: Option<SyncMode>,
        finished_successfully: bool,
        _had_updates: bool,
    ) {
        let Some(repo_idx) = repo_idx else {
            return;
        };
        let Some(repo) = self.repository_view_state.repositories.get(repo_idx) else {
            return;
        };
        let repo_url = Self::normalize_repo_url(&repo.address);
        let repo_path_key = Self::repo_instance_path_key(&repo.path);

        let Some(run) = self.scheduler_active_run.as_mut() else {
            return;
        };
        let Some(in_flight) = run.in_flight.as_ref() else {
            return;
        };
        let expected_mode = match run.stage {
            JobRunStage::Recheck => SyncMode::RemoteRefreshOnly,
            JobRunStage::Download => SyncMode::Download,
            JobRunStage::Finalize => return,
        };
        if mode != Some(expected_mode) {
            return;
        }
        if Self::normalize_repo_url(&in_flight.remote_url) != repo_url
            || Self::repo_instance_path_key(&in_flight.local_path) != repo_path_key
        {
            return;
        }

        if finished_successfully {
            run.succeeded += 1;
        } else {
            run.failed += 1;
        }
        run.in_flight = None;
    }

    fn finalize_scheduled_job_run(&mut self) {
        let Some(run) = self.scheduler_active_run.take() else {
            return;
        };

        let outcome = if run.failed > 0 && run.succeeded > 0 {
            JobRunOutcome::PartialFailure
        } else if run.failed > 0 {
            JobRunOutcome::Failed
        } else if run.succeeded > 0 {
            JobRunOutcome::Success
        } else {
            JobRunOutcome::Skipped
        };
        info!(
            "Scheduled job '{}' finished: outcome={:?} ok={} failed={} skipped={}",
            run.job_name, outcome, run.succeeded, run.failed, run.skipped
        );
        self.record_scheduled_job_result(
            &run.job_id,
            run.fire_ms,
            outcome,
            run.succeeded,
            run.failed,
            run.skipped,
        );

        let summary = self.t_fmt(
            "Scheduled job '{name}' finished: {ok} updated, {failed} failed",
            &[
                ("name", run.job_name.clone()),
                ("ok", run.succeeded.to_string()),
                ("failed", run.failed.to_string()),
            ],
        );
        if run.failed > 0 {
            self.show_error_toast(summary);
        } else {
            self.show_success_toast(summary);
        }

        if run.post_action == PostAction::None {
            return;
        }
        if run.post_action_only_on_success && run.failed > 0 {
            warn!(
                "Scheduled job '{}' skipping post-action {:?}: {} operation(s) failed",
                run.job_name, run.post_action, run.failed
            );
            self.show_error_toast(
                self.t("Scheduled post-action skipped because some operations failed."),
            );
            return;
        }

        info!(
            "Scheduled job '{}' will run post-action {:?} in {}s",
            run.job_name, run.post_action, POST_ACTION_COUNTDOWN_SECS
        );
        self.scheduler_pending_post_action = Some(PendingPostAction {
            action: run.post_action,
            job_name: run.job_name,
            deadline: Instant::now() + Duration::from_secs(POST_ACTION_COUNTDOWN_SECS),
        });
    }

    fn record_scheduled_job_result(
        &mut self,
        job_id: &str,
        fire_ms: u64,
        outcome: JobRunOutcome,
        succeeded: usize,
        failed: usize,
        skipped: usize,
    ) {
        if let Some(job) = self
            .settings_view_state
            .scheduled_jobs
            .iter_mut()
            .find(|job| job.id == job_id)
        {
            job.last_run_unix_ms = Some(fire_ms);
            job.last_result = Some(JobRunResult {
                finished_unix_ms: now_unix_ms(),
                outcome,
                succeeded,
                failed,
                skipped,
            });
        }
        self.mark_settings_dirty();
    }

    fn process_scheduled_post_action(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.scheduler_pending_post_action.as_ref() else {
            return;
        };
        if Instant::now() >= pending.deadline {
            let action = pending.action;
            self.scheduler_pending_post_action = None;
            self.execute_scheduled_post_action(action, ctx);
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn execute_scheduled_post_action(&mut self, action: PostAction, ctx: &egui::Context) {
        match action {
            PostAction::None => {}
            PostAction::CloseApp => {
                info!("Scheduled post-action: closing Foxy");
                self.request_app_close(ctx, "scheduled job post-action");
            }
            PostAction::ShutdownPc => {
                #[cfg(target_os = "windows")]
                {
                    info!("Scheduled post-action: shutting down the PC");
                    initiate_pc_shutdown();
                    self.request_app_close(ctx, "scheduled job shutdown");
                }
                #[cfg(not(target_os = "windows"))]
                {
                    warn!("Scheduled PC shutdown is not supported on this platform; skipping");
                    self.show_error_toast(
                        self.t("Scheduled shutdown is not supported on this platform."),
                    );
                }
            }
        }
    }

    /// Cancel a pending post-action from the countdown overlay.
    pub fn cancel_scheduled_post_action(&mut self) {
        if let Some(pending) = self.scheduler_pending_post_action.take() {
            info!(
                "Scheduled post-action {:?} for job '{}' cancelled by user",
                pending.action, pending.job_name
            );
        }
    }

    /// Run a pending post-action immediately, skipping the remaining countdown.
    pub fn proceed_scheduled_post_action_now(&mut self, ctx: &egui::Context) {
        if let Some(pending) = self.scheduler_pending_post_action.take() {
            self.execute_scheduled_post_action(pending.action, ctx);
        }
    }

    /// Centered modal countdown shown before a post-action runs. The main safety
    /// gate: a prominent Cancel always aborts; PC shutdown also keeps the OS
    /// `shutdown /a` window as a second abort path. Rendered every frame from the
    /// update loop so it appears regardless of the current view.
    pub fn render_scheduled_post_action_overlay(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.scheduler_pending_post_action.as_ref() else {
            return;
        };
        let action = pending.action;
        let job_name = pending.job_name.clone();
        let remaining = pending
            .deadline
            .saturating_duration_since(Instant::now())
            .as_secs();

        let (title, message, proceed_label) = match action {
            PostAction::ShutdownPc => (
                self.t("Scheduled shutdown"),
                self.t_fmt(
                    "Job '{name}' finished. The PC will shut down in {secs}s.",
                    &[("name", job_name.clone()), ("secs", remaining.to_string())],
                ),
                self.t("Shut down now"),
            ),
            PostAction::CloseApp => (
                self.t("Scheduled close"),
                self.t_fmt(
                    "Job '{name}' finished. Foxy will close in {secs}s.",
                    &[("name", job_name.clone()), ("secs", remaining.to_string())],
                ),
                self.t("Close now"),
            ),
            PostAction::None => return,
        };
        let cancel_label = self.t("Cancel");
        let card_bg = self.color_card_bg();
        let text_normal = self.color_text_normal();
        let destructive = self.color_action_destructive();

        let mut cancel = false;
        let mut proceed = false;
        egui::Window::new(title)
            .frame(
                egui::Frame::window(&ctx.global_style())
                    .fill(card_bg)
                    .stroke(egui::Stroke::new(1.0, text_normal))
                    .corner_radius(egui::CornerRadius::same(10)),
            )
            .title_bar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label(message);
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    let cancel_btn = ui.add(egui::Button::new(cancel_label));
                    if cancel_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if cancel_btn.clicked() {
                        cancel = true;
                    }

                    let proceed_btn = ui.add(egui::Button::new(proceed_label).fill(destructive));
                    if proceed_btn.hovered() {
                        ui.ctx().output_mut(Foxy::set_pointing_cursor_output);
                    }
                    if proceed_btn.clicked() {
                        proceed = true;
                    }
                });
            });

        if cancel {
            self.cancel_scheduled_post_action();
        } else if proceed {
            self.proceed_scheduled_post_action_now(ctx);
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}
