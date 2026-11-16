//! Persist job definitions and lightweight run history only; live execution state stays in skipped runtime fields.

use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

pub use crate::core::schedule::{JobSchedule, local_to_unix_ms, unix_ms_to_local};
use crate::core::schedule::first_valid_local_instant;

/// Jobs more than the grace window late are marked missed, preventing stale download-and-shutdown actions after Foxy reopens.
pub const SCHEDULER_MISSED_GRACE_SECS: i64 = 10 * 60;

/// Seconds the cancellable countdown overlay is shown before a post-action
/// (close app / shut down PC) actually runs.
pub const POST_ACTION_COUNTDOWN_SECS: u64 = 60;

/// All seven weekdays selected (Monday..Sunday), i.e. a daily recurring job.
pub const WEEKDAYS_ALL: u8 = 0b0111_1111;

/// Repository targets use `(remote_url, local_path)` because the same URL in another folder is a separate instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInstanceKey {
    pub remote_url: String,
    pub local_path: String,
}

/// Resolve targets to current repository indices when a job fires; missing targets are skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobTarget {
    /// A single repository instance.
    Repository(RepoInstanceKey),
    /// Every member repository of a repository space, resolved when the job runs.
    Space { space_id: String },
    /// An explicit hand-picked set of repository instances.
    Custom { repos: Vec<RepoInstanceKey> },
}

/// Platforms without shutdown support show the post-action as unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PostAction {
    #[default]
    None,
    CloseApp,
    ShutdownPc,
}

/// Outcome recorded after a job run, surfaced on the job card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobRunOutcome {
    Success,
    PartialFailure,
    Failed,
    Skipped,
    Missed,
}

/// Lightweight record of the most recent run, persisted with the job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRunResult {
    pub finished_unix_ms: u64,
    pub outcome: JobRunOutcome,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Whether a job should run right now, computed from its schedule and last run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueState {
    /// Nothing to do this tick.
    NotDue,
    /// The job is due; `fire_ms` is the scheduled instant that triggered it.
    Due { fire_ms: u64 },
    /// The scheduled instant is more than the grace window in the past; record
    /// it as missed without executing. `fire_ms` is that instant.
    Missed { fire_ms: u64 },
}

/// A single user-defined scheduled job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub schedule: JobSchedule,
    pub target: JobTarget,
    /// Stage 1: refresh remote state and detect updates.
    #[serde(default = "default_true")]
    pub recheck: bool,
    /// Stage 2: download pending updates without a per-repo confirmation.
    #[serde(default)]
    pub download: bool,
    /// Stage 3: terminal action once the stages above finish.
    #[serde(default)]
    pub post_action: PostAction,
    /// When set, the post-action only runs if no operation failed.
    #[serde(default = "default_true")]
    pub post_action_only_on_success: bool,
    /// Unix millis of the scheduled instant of the last run (success, miss, or
    /// skip). Used to avoid re-firing the same occurrence.
    #[serde(default)]
    pub last_run_unix_ms: Option<u64>,
    #[serde(default)]
    pub last_result: Option<JobRunResult>,
}

fn default_true() -> bool {
    true
}

impl ScheduledJob {
    /// Only successful explicit user actions should change the recorded outcome.
    pub fn due_state(&self, now: DateTime<Local>) -> DueState {
        if !self.enabled {
            return DueState::NotDue;
        }
        let Some(fire) = self.schedule.most_recent_fire_at_or_before(now) else {
            return DueState::NotDue;
        };
        let fire_ms = local_to_unix_ms(fire);
        if self.last_run_unix_ms.is_some_and(|last| last >= fire_ms) {
            return DueState::NotDue;
        }
        if (now - fire).num_seconds() > SCHEDULER_MISSED_GRACE_SECS {
            DueState::Missed { fire_ms }
        } else {
            DueState::Due { fire_ms }
        }
    }

    /// An occurrence already observed and held for other work remains due beyond the grace window.
    pub fn due_state_with_hold(&self, now: DateTime<Local>, held_fire_ms: Option<u64>) -> DueState {
        match self.due_state(now) {
            DueState::Missed { fire_ms } if held_fire_ms == Some(fire_ms) => {
                DueState::Due { fire_ms }
            }
            other => other,
        }
    }
}

/// If Foxy or the computer sleeps beyond the grace window, held jobs expire instead of running late.
pub fn scheduler_paused_between(previous_tick: DateTime<Local>, now: DateTime<Local>) -> bool {
    (now - previous_tick).num_seconds() > SCHEDULER_MISSED_GRACE_SECS
}

/// Why a job's occurrence was missed, for the log.
pub fn missed_run_reason(
    previous_tick: Option<DateTime<Local>>,
    now: DateTime<Local>,
) -> &'static str {
    match previous_tick {
        None => "Foxy was not running at its scheduled time",
        Some(previous) if scheduler_paused_between(previous, now) => {
            "the computer was asleep or Foxy was paused at its scheduled time"
        }
        Some(_) => "it was enabled or changed after its scheduled time had passed",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    Once,
    Recurring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Repository,
    Space,
    Custom,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleJobDraft {
    pub editing_id: Option<String>,
    pub name: String,
    pub kind: ScheduleKind,
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub weekdays: u8,
    pub target_kind: TargetKind,
    pub target_repo: Option<RepoInstanceKey>,
    pub target_space_id: Option<String>,
    pub target_custom: Vec<RepoInstanceKey>,
    pub recheck: bool,
    pub download: bool,
    pub post_action: PostAction,
    pub post_action_only_on_success: bool,

    pub error: Option<String>,
}

impl ScheduleJobDraft {
    pub fn new_add(now: DateTime<Local>) -> Self {
        let start = now + chrono::Duration::hours(1);
        Self {
            editing_id: None,
            name: String::new(),
            kind: ScheduleKind::Once,
            year: start.year(),
            month: start.month(),
            day: start.day(),
            hour: start.hour(),
            minute: start.minute(),
            weekdays: WEEKDAYS_ALL,
            target_kind: TargetKind::Space,
            target_repo: None,
            target_space_id: None,
            target_custom: Vec::new(),
            recheck: true,
            download: false,
            post_action: PostAction::None,
            post_action_only_on_success: true,
            error: None,
        }
    }

    pub fn from_job(job: &ScheduledJob) -> Self {
        let mut draft = Self::new_add(Local::now());
        draft.editing_id = Some(job.id.clone());
        draft.name = job.name.clone();
        match &job.schedule {
            JobSchedule::OnceAt { unix_ms } => {
                draft.kind = ScheduleKind::Once;
                if let Some(dt) = unix_ms_to_local(*unix_ms) {
                    draft.year = dt.year();
                    draft.month = dt.month();
                    draft.day = dt.day();
                    draft.hour = dt.hour();
                    draft.minute = dt.minute();
                }
            }
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => {
                draft.kind = ScheduleKind::Recurring;
                draft.hour = u32::from(minutes_of_day / 60);
                draft.minute = u32::from(minutes_of_day % 60);
                draft.weekdays = *weekdays;
            }
        }
        match &job.target {
            JobTarget::Repository(key) => {
                draft.target_kind = TargetKind::Repository;
                draft.target_repo = Some(key.clone());
            }
            JobTarget::Space { space_id } => {
                draft.target_kind = TargetKind::Space;
                draft.target_space_id = Some(space_id.clone());
            }
            JobTarget::Custom { repos } => {
                draft.target_kind = TargetKind::Custom;
                draft.target_custom = repos.clone();
            }
        }
        draft.recheck = job.recheck;
        draft.download = job.download;
        draft.post_action = job.post_action;
        draft.post_action_only_on_success = job.post_action_only_on_success;
        draft
    }

    /// New jobs start at `now`, so recurring jobs only consider future occurrences and one-time jobs must be future-dated.
    pub fn build(&self, id: String, now: DateTime<Local>) -> Result<ScheduledJob, String> {
        if !self.recheck && !self.download && self.post_action == PostAction::None {
            return Err("Enable at least one action (recheck, download, or a post-action).".into());
        }
        let now_ms = local_to_unix_ms(now);
        let schedule = match self.kind {
            ScheduleKind::Once => {
                let date = chrono::NaiveDate::from_ymd_opt(self.year, self.month, self.day)
                    .ok_or_else(|| "Invalid date.".to_string())?;
                let time = NaiveTime::from_hms_opt(self.hour, self.minute, 0)
                    .ok_or_else(|| "Invalid time.".to_string())?;
                let local = first_valid_local_instant(date.and_time(time), |local| {
                    Local.from_local_datetime(local)
                })
                .ok_or_else(|| "That local time could not be resolved.".to_string())?;
                if local <= now {
                    return Err("Pick a date and time in the future.".into());
                }
                JobSchedule::OnceAt {
                    unix_ms: local_to_unix_ms(local),
                }
            }
            ScheduleKind::Recurring => {
                if self.weekdays == 0 {
                    return Err("Select at least one weekday.".into());
                }
                if self.hour > 23 || self.minute > 59 {
                    return Err("Invalid time.".into());
                }
                JobSchedule::Recurring {
                    minutes_of_day: (self.hour * 60 + self.minute) as u16,
                    weekdays: self.weekdays,
                }
            }
        };
        let target = match self.target_kind {
            TargetKind::Repository => {
                let key = self
                    .target_repo
                    .clone()
                    .ok_or_else(|| "Choose a repository.".to_string())?;
                JobTarget::Repository(key)
            }
            TargetKind::Space => {
                let space_id = self
                    .target_space_id
                    .clone()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "Choose a repository space.".to_string())?;
                JobTarget::Space { space_id }
            }
            TargetKind::Custom => {
                if self.target_custom.is_empty() {
                    return Err("Select at least one repository.".into());
                }
                JobTarget::Custom {
                    repos: self.target_custom.clone(),
                }
            }
        };
        let name = if self.name.trim().is_empty() {
            "Scheduled job".to_string()
        } else {
            self.name.trim().to_string()
        };
        Ok(ScheduledJob {
            id,
            name,
            enabled: true,
            schedule,
            target,
            recheck: self.recheck,
            download: self.download,
            post_action: self.post_action,
            post_action_only_on_success: self.post_action_only_on_success,
            last_run_unix_ms: Some(now_ms),
            last_result: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::LocalResult;
    use chrono::{Duration, TimeZone};

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn local_time_gap_uses_first_valid_instant() {
        let requested = chrono::NaiveDate::from_ymd_opt(2026, 3, 29)
            .unwrap()
            .and_hms_opt(2, 0, 0)
            .unwrap();
        let resolved = first_valid_local_instant(requested, |local| {
            if local.hour() == 2 {
                LocalResult::None
            } else {
                LocalResult::Single(chrono::Utc.from_utc_datetime(local))
            }
        })
        .unwrap();
        assert_eq!(resolved.naive_utc(), requested + chrono::Duration::hours(1));
    }

    #[test]
    fn ambiguous_local_time_uses_earlier_instant() {
        let requested = chrono::NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(2, 0, 0)
            .unwrap();
        let earlier = chrono::Utc.from_utc_datetime(&requested);
        let later = earlier + chrono::Duration::hours(1);
        let resolved =
            first_valid_local_instant(requested, |_| LocalResult::Ambiguous(later, earlier))
                .unwrap();
        assert_eq!(resolved, earlier);
    }

    fn once_job(fire: DateTime<Local>) -> ScheduledJob {
        ScheduledJob {
            id: "j".into(),
            name: "j".into(),
            enabled: true,
            schedule: JobSchedule::OnceAt {
                unix_ms: local_to_unix_ms(fire),
            },
            target: JobTarget::Space {
                space_id: "s".into(),
            },
            recheck: true,
            download: false,
            post_action: PostAction::None,
            post_action_only_on_success: true,
            last_run_unix_ms: None,
            last_result: None,
        }
    }

    #[test]
    fn once_in_future_is_not_due() {
        let now = at(2026, 6, 30, 12, 0);
        let job = once_job(now + Duration::minutes(30));
        assert_eq!(job.due_state(now), DueState::NotDue);
        assert!(job.schedule.next_fire_at(now).is_some());
    }

    #[test]
    fn once_just_passed_within_grace_is_due() {
        let now = at(2026, 6, 30, 12, 0);
        let fire = now - Duration::seconds(30);
        let job = once_job(fire);
        assert_eq!(
            job.due_state(now),
            DueState::Due {
                fire_ms: local_to_unix_ms(fire)
            }
        );
    }

    #[test]
    fn once_long_past_is_missed_not_run() {
        let now = at(2026, 6, 30, 12, 0);
        let fire = now - Duration::hours(6);
        let job = once_job(fire);
        assert_eq!(
            job.due_state(now),
            DueState::Missed {
                fire_ms: local_to_unix_ms(fire)
            }
        );
    }

    #[test]
    fn a_held_back_occurrence_stays_due_past_the_grace_window() {
        let now = at(2026, 6, 30, 12, 0);
        let fire = now - Duration::hours(2);
        let fire_ms = local_to_unix_ms(fire);
        let job = once_job(fire);
        assert_eq!(
            job.due_state_with_hold(now, Some(fire_ms)),
            DueState::Due { fire_ms }
        );
        assert_eq!(
            job.due_state_with_hold(now, None),
            DueState::Missed { fire_ms }
        );
        assert_eq!(
            job.due_state_with_hold(now, Some(fire_ms - 1)),
            DueState::Missed { fire_ms }
        );

        let mut done = job.clone();
        done.last_run_unix_ms = Some(fire_ms);
        assert_eq!(
            done.due_state_with_hold(now, Some(fire_ms)),
            DueState::NotDue
        );
    }

    #[test]
    fn a_missed_run_names_why_foxy_could_not_run_it() {
        let now = at(2026, 6, 30, 12, 0);
        assert_eq!(
            missed_run_reason(None, now),
            "Foxy was not running at its scheduled time"
        );
        let slept = now - Duration::minutes(SCHEDULER_MISSED_GRACE_SECS / 60 + 1);
        assert!(scheduler_paused_between(slept, now));
        assert!(missed_run_reason(Some(slept), now).contains("asleep"));
        let ticking = now - Duration::seconds(30);
        assert!(!scheduler_paused_between(ticking, now));
        assert!(missed_run_reason(Some(ticking), now).contains("enabled or changed"));
    }

    #[test]
    fn once_already_run_is_not_due() {
        let now = at(2026, 6, 30, 12, 0);
        let fire = now - Duration::seconds(30);
        let mut job = once_job(fire);
        job.last_run_unix_ms = Some(local_to_unix_ms(fire));
        assert_eq!(job.due_state(now), DueState::NotDue);
        // A one-time job that has fired has no future occurrence.
        assert!(job.schedule.next_fire_at(now).is_none());
    }

    #[test]
    fn disabled_job_is_never_due() {
        let now = at(2026, 6, 30, 12, 0);
        let mut job = once_job(now - Duration::seconds(10));
        job.enabled = false;
        assert_eq!(job.due_state(now), DueState::NotDue);
    }

    #[test]
    fn recurring_daily_fires_at_its_time_then_waits_for_tomorrow() {
        // 2026-06-30 is a Tuesday; 03:00 daily.
        let schedule = JobSchedule::Recurring {
            minutes_of_day: 3 * 60,
            weekdays: WEEKDAYS_ALL,
        };
        let mut job = once_job(at(2026, 6, 30, 3, 0));
        job.schedule = schedule.clone();

        let at_time = at(2026, 6, 30, 3, 0);
        let DueState::Due { fire_ms } = job.due_state(at_time) else {
            panic!("expected due at scheduled time");
        };
        assert_eq!(fire_ms, local_to_unix_ms(at(2026, 6, 30, 3, 0)));

        // After recording that run, the same day is no longer due and the next
        // fire is the following day.
        job.last_run_unix_ms = Some(fire_ms);
        let later = at(2026, 6, 30, 9, 0);
        assert_eq!(job.due_state(later), DueState::NotDue);
        assert_eq!(
            schedule.next_fire_at(later),
            Some(at(2026, 7, 1, 3, 0)),
            "next run should be tomorrow at the same time"
        );
    }

    #[test]
    fn recurring_skips_unselected_weekdays() {
        // Only Monday (bit 0). 2026-06-30 is a Tuesday, so the next fire is the
        // following Monday 2026-07-06.
        let schedule = JobSchedule::Recurring {
            minutes_of_day: 8 * 60,
            // Monday only: bit 0 (days from Monday == 0).
            weekdays: 0b0000_0001,
        };
        let tuesday = at(2026, 6, 30, 8, 0);
        assert_eq!(schedule.next_fire_at(tuesday), Some(at(2026, 7, 6, 8, 0)));

        // A job that already handled its most recent (Monday) occurrence is not
        // due again on Tuesday; the next fire is next Monday.
        let mut job = once_job(tuesday);
        job.schedule = schedule.clone();
        job.last_run_unix_ms = schedule
            .most_recent_fire_at_or_before(tuesday)
            .map(local_to_unix_ms);
        assert_eq!(job.due_state(tuesday), DueState::NotDue);
    }

    #[test]
    fn recurring_with_no_weekdays_never_fires() {
        let schedule = JobSchedule::Recurring {
            minutes_of_day: 600,
            weekdays: 0,
        };
        let now = at(2026, 6, 30, 12, 0);
        assert!(schedule.next_fire_at(now).is_none());
        assert!(schedule.most_recent_fire_at_or_before(now).is_none());
    }

    fn space_draft() -> ScheduleJobDraft {
        let mut draft = ScheduleJobDraft::new_add(at(2026, 6, 30, 12, 0));
        draft.target_kind = TargetKind::Space;
        draft.target_space_id = Some("space-1".into());
        draft
    }

    #[test]
    fn draft_requires_at_least_one_action() {
        let now = at(2026, 6, 30, 12, 0);
        let mut draft = space_draft();
        draft.recheck = false;
        draft.download = false;
        draft.post_action = PostAction::None;
        assert!(draft.build("id".into(), now).is_err());
    }

    #[test]
    fn draft_recurring_requires_a_weekday() {
        let now = at(2026, 6, 30, 12, 0);
        let mut draft = space_draft();
        draft.kind = ScheduleKind::Recurring;
        draft.weekdays = 0;
        assert!(draft.build("id".into(), now).is_err());
    }

    #[test]
    fn draft_space_requires_a_space_id() {
        let now = at(2026, 6, 30, 12, 0);
        let mut draft = space_draft();
        draft.target_space_id = None;
        assert!(draft.build("id".into(), now).is_err());
    }

    #[test]
    fn draft_rejects_once_in_the_past() {
        let now = at(2026, 6, 30, 12, 0);
        let mut draft = space_draft();
        draft.kind = ScheduleKind::Once;
        draft.year = 2026;
        draft.month = 6;
        draft.day = 30;
        draft.hour = 9;
        draft.minute = 0;
        assert!(draft.build("id".into(), now).is_err());
    }

    #[test]
    fn draft_builds_valid_once_job() {
        let now = at(2026, 6, 30, 12, 0);
        let mut draft = space_draft();
        draft.kind = ScheduleKind::Once;
        draft.year = 2026;
        draft.month = 7;
        draft.day = 1;
        draft.hour = 3;
        draft.minute = 30;
        draft.recheck = true;
        draft.download = true;
        draft.post_action = PostAction::ShutdownPc;

        let job = draft
            .build("job-1".into(), now)
            .expect("valid draft should build");
        assert_eq!(job.id, "job-1");
        assert_eq!(
            job.schedule,
            JobSchedule::OnceAt {
                unix_ms: local_to_unix_ms(at(2026, 7, 1, 3, 30))
            }
        );
        assert!(job.recheck && job.download);
        assert_eq!(job.post_action, PostAction::ShutdownPc);
        assert!(job.enabled);
        // The job is anchored to creation time so only future occurrences fire.
        assert_eq!(job.last_run_unix_ms, Some(local_to_unix_ms(now)));
    }
}
