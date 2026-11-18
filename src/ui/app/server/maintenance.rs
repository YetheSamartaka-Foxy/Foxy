use std::time::Instant;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};

use crate::core::schedule::{JobSchedule, ScheduleClock, first_valid_local_instant};
use crate::core::server::ServerInstanceId;
use crate::core::server::jobs::{
    ArmaMaintenancePolicy, MaintenanceAction, MaintenanceJob, MaintenanceJobDefinition,
    MaintenanceOverview, MaintenancePolicy, Occurrence, PlayerPolicy,
};
use crate::ui::types::{ScheduleKind, WEEKDAYS_ALL};

pub(crate) const RECENT_OCCURRENCES: usize = 20;

#[derive(Clone, Debug)]
pub(crate) struct MaintenanceSnapshot {
    pub instance_id: ServerInstanceId,
    pub overview: MaintenanceOverview,
    pub recent: Vec<Occurrence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaintenanceNotice {
    Saved,
    Removed,
    Recorded,
    AlreadyOpen,
    Cancelled,
    CancelRequested,
    AlreadyFinished,
}

impl MaintenanceNotice {
    /// The notice for a cancel reply, from the state the owner reported.
    pub fn for_cancel(state: Option<crate::core::server::jobs::OccurrenceState>) -> Self {
        use crate::core::server::jobs::OccurrenceState;
        match state {
            Some(OccurrenceState::Cancelled) => Self::Cancelled,
            Some(state) if state.terminal() => Self::AlreadyFinished,
            _ => Self::CancelRequested,
        }
    }
}

/// Each confirmation keeps the revision it was reviewed against, so a change made elsewhere in
/// the meantime is refused instead of overwritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MaintenanceConfirmation {
    Run {
        job_id: String,
        revision: u64,
    },
    Remove {
        job_id: String,
        catalog_revision: u64,
    },
    Cancel {
        occurrence_id: String,
    },
}

#[derive(Default)]
pub(crate) struct MaintenanceViewState {
    pub snapshot: Option<MaintenanceSnapshot>,
    pub form: Option<MaintenanceForm>,
    pub confirmation: Option<MaintenanceConfirmation>,
    /// Set when a confirmation opens so its safe choice takes focus once it is drawn.
    pub focus_confirmation: bool,
    pub focus_form: bool,
    pub notice: Option<MaintenanceNotice>,
    pub last_refresh: Option<Instant>,
    /// The last failed background load, shown instead of a loading spinner.
    pub load_error: Option<String>,
    /// The last failed save, run or cancel, shown next to the controls that caused it.
    pub action_error: Option<String>,
}

impl MaintenanceViewState {
    pub fn for_instance(&self, id: &ServerInstanceId) -> Option<&MaintenanceSnapshot> {
        self.snapshot
            .as_ref()
            .filter(|snapshot| &snapshot.instance_id == id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MaintenanceForm {
    pub editing: Option<String>,
    /// The catalog revision the form was opened against.
    pub base_revision: u64,
    pub name: String,
    pub action: MaintenanceAction,
    pub kind: ScheduleKind,
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub weekdays: u8,
    pub utc: bool,
    pub enabled: bool,
    pub require_empty: bool,
    pub arma_hooks: bool,
    pub announcement: String,
    pub announcement_delay_seconds: u16,
    pub mission_template: String,
    pub require_backup: bool,
    pub live_backup: bool,
    pub grace_minutes: u16,
    pub max_defer_minutes: u16,
    pub max_attempts: u8,
    pub retry_delay_seconds: u16,
    pub stop_timeout_seconds: u16,
    pub error: Option<String>,
}

impl MaintenanceForm {
    pub fn new_add(now: DateTime<Local>) -> Self {
        let start = now + chrono::Duration::hours(1);
        let policy = MaintenancePolicy::default();
        Self {
            editing: None,
            base_revision: 0,
            name: String::new(),
            action: MaintenanceAction::Restart,
            kind: ScheduleKind::Recurring,
            year: start.year(),
            month: start.month(),
            day: start.day(),
            hour: 4,
            minute: 0,
            weekdays: WEEKDAYS_ALL,
            utc: false,
            enabled: true,
            require_empty: false,
            arma_hooks: false,
            announcement: String::new(),
            announcement_delay_seconds: 0,
            mission_template: String::new(),
            require_backup: false,
            live_backup: false,
            grace_minutes: policy.grace_minutes,
            max_defer_minutes: policy.max_defer_minutes,
            max_attempts: policy.max_attempts,
            retry_delay_seconds: policy.retry_delay_seconds,
            stop_timeout_seconds: policy.stop_timeout_seconds,
            error: None,
        }
    }

    pub fn from_job(job: &MaintenanceJob, now: DateTime<Local>) -> Self {
        let definition = &job.definition;
        let mut form = Self::new_add(now);
        form.editing = Some(job.id.clone());
        form.name = definition.name.clone();
        form.action = definition.action;
        form.utc = definition.clock == ScheduleClock::Utc;
        form.enabled = definition.enabled;
        match definition.schedule {
            JobSchedule::OnceAt { unix_ms } => {
                form.kind = ScheduleKind::Once;
                let wall = i64::try_from(unix_ms).ok().and_then(|millis| {
                    if form.utc {
                        Utc.timestamp_millis_opt(millis)
                            .single()
                            .map(|time| time.naive_utc())
                    } else {
                        Local
                            .timestamp_millis_opt(millis)
                            .single()
                            .map(|time| time.naive_local())
                    }
                });
                if let Some(wall) = wall {
                    form.year = wall.year();
                    form.month = wall.month();
                    form.day = wall.day();
                    form.hour = wall.hour();
                    form.minute = wall.minute();
                }
            }
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } => {
                form.kind = ScheduleKind::Recurring;
                form.hour = u32::from(minutes_of_day / 60);
                form.minute = u32::from(minutes_of_day % 60);
                form.weekdays = weekdays;
            }
        }
        let policy = &definition.policy;
        form.require_empty = policy.player_policy == PlayerPolicy::RequireEmpty;
        form.live_backup =
            policy.backup_mode == crate::core::server::BackupConsistencyMode::BestEffortLive;
        form.grace_minutes = policy.grace_minutes;
        form.max_defer_minutes = policy.max_defer_minutes;
        form.max_attempts = policy.max_attempts;
        form.retry_delay_seconds = policy.retry_delay_seconds;
        form.stop_timeout_seconds = policy.stop_timeout_seconds;
        if let Some(arma) = &policy.arma {
            form.arma_hooks = true;
            form.announcement = arma.announcement.clone().unwrap_or_default();
            form.announcement_delay_seconds = arma.announcement_delay_seconds;
            form.mission_template = arma.mission_template.clone().unwrap_or_default();
            form.require_backup = arma.require_backup;
        }
        form
    }

    /// A one-time job must be in the future; the date and time are read in the job's clock.
    pub fn build(&self, now_unix_ms: u64) -> Result<MaintenanceJobDefinition, String> {
        let clock = if self.utc {
            ScheduleClock::Utc
        } else {
            ScheduleClock::Local
        };
        let schedule = match self.kind {
            ScheduleKind::Once => {
                let date = NaiveDate::from_ymd_opt(self.year, self.month, self.day)
                    .ok_or_else(|| "Invalid date.".to_string())?;
                let time = NaiveTime::from_hms_opt(self.hour, self.minute, 0)
                    .ok_or_else(|| "Invalid time.".to_string())?;
                let wall = date.and_time(time);
                let millis = match clock {
                    ScheduleClock::Utc => Utc.from_utc_datetime(&wall).timestamp_millis(),
                    ScheduleClock::Local => {
                        first_valid_local_instant(wall, |local| Local.from_local_datetime(local))
                            .ok_or_else(|| "That local time could not be resolved.".to_string())?
                            .timestamp_millis()
                    }
                };
                let unix_ms = u64::try_from(millis).map_err(|_| "Invalid date.".to_string())?;
                if unix_ms <= now_unix_ms {
                    return Err("Pick a date and time in the future.".into());
                }
                JobSchedule::OnceAt { unix_ms }
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
        let rotate_mission = self.action == MaintenanceAction::RotateMission;
        let reload_scenario = self.action == MaintenanceAction::ReloadScenario;
        let restart_hooks = matches!(
            self.action,
            MaintenanceAction::Restart | MaintenanceAction::RotateMission
        ) && self.arma_hooks;
        let announcement = if restart_hooks {
            let value = self.announcement.trim();
            if !value.is_empty() && (value.len() > 256 || value.chars().any(char::is_control)) {
                return Err(
                    "Announcement must be 256 bytes or fewer and contain no control characters."
                        .into(),
                );
            }
            if self.announcement_delay_seconds > 300 {
                return Err("Announcement delay must be between 0 and 300 seconds.".into());
            }
            if value.is_empty() && self.announcement_delay_seconds > 0 {
                return Err("An announcement delay requires an announcement.".into());
            }
            (!value.is_empty()).then(|| value.to_owned())
        } else {
            None
        };
        let mission_template = if rotate_mission {
            let value = self.mission_template.trim();
            if value.is_empty() {
                return Err("Enter a deployed mission template to rotate to.".into());
            }
            Some(value.to_owned())
        } else {
            None
        };
        let definition = MaintenanceJobDefinition {
            name: self.name.trim().to_owned(),
            enabled: self.enabled,
            action: self.action,
            schedule,
            clock,
            policy: MaintenancePolicy {
                grace_minutes: self.grace_minutes,
                player_policy: if self.require_empty {
                    PlayerPolicy::RequireEmpty
                } else {
                    PlayerPolicy::Ignore
                },
                max_defer_minutes: self.max_defer_minutes,
                max_attempts: self.max_attempts,
                retry_delay_seconds: self.retry_delay_seconds,
                stop_timeout_seconds: self.stop_timeout_seconds,
                arma: (restart_hooks
                    || rotate_mission
                    || (self.require_backup && !reload_scenario))
                    .then_some(ArmaMaintenancePolicy {
                        announcement,
                        announcement_delay_seconds: if restart_hooks {
                            self.announcement_delay_seconds
                        } else {
                            0
                        },
                        mission_template,
                        require_backup: self.require_backup && !reload_scenario,
                    }),
                backup_mode: if self.live_backup {
                    crate::core::server::BackupConsistencyMode::BestEffortLive
                } else {
                    crate::core::server::BackupConsistencyMode::StoppedVerified
                },
            },
        };
        definition.validate()?;
        Ok(definition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(definition: MaintenanceJobDefinition) -> MaintenanceJob {
        MaintenanceJob {
            id: "job_0123456789abcdef".into(),
            revision: 3,
            revised_unix_ms: 1,
            definition,
        }
    }

    #[test]
    fn weekly_form_round_trips_through_a_saved_job() {
        let now = Local::now();
        let mut form = MaintenanceForm::new_add(now);
        form.name = "  Nightly restart ".into();
        form.weekdays = 0b0100_0001;
        form.hour = 5;
        form.minute = 30;
        form.utc = true;
        form.require_empty = true;
        let definition = form.build(0).unwrap();
        assert_eq!(definition.name, "Nightly restart");
        assert_eq!(definition.clock, ScheduleClock::Utc);
        assert_eq!(
            definition.schedule,
            JobSchedule::Recurring {
                minutes_of_day: 330,
                weekdays: 0b0100_0001
            }
        );
        assert_eq!(definition.policy.player_policy, PlayerPolicy::RequireEmpty);
        let reopened = MaintenanceForm::from_job(&job(definition.clone()), now);
        assert_eq!(reopened.editing.as_deref(), Some("job_0123456789abcdef"));
        assert_eq!(reopened.build(0).unwrap(), definition);
    }

    #[test]
    fn arma_restart_hook_settings_round_trip_through_an_edit() {
        let now = Local::now();
        let mut form = MaintenanceForm::new_add(now);
        form.name = "Arma restart".into();
        form.arma_hooks = true;
        form.announcement = "Restart in five minutes".into();
        form.announcement_delay_seconds = 15;
        let definition = form.build(0).unwrap();
        let arma = definition.policy.arma.as_ref().unwrap();
        assert_eq!(
            arma.announcement.as_deref(),
            Some("Restart in five minutes")
        );
        assert_eq!(arma.announcement_delay_seconds, 15);
        assert_eq!(arma.mission_template, None);

        let reopened = MaintenanceForm::from_job(&job(definition.clone()), now);
        assert!(reopened.arma_hooks);
        assert_eq!(reopened.announcement, "Restart in five minutes");
        assert_eq!(reopened.announcement_delay_seconds, 15);
        assert_eq!(reopened.build(0).unwrap(), definition);
    }

    #[test]
    fn arma_rotation_requires_a_mission_and_only_stores_it_for_rotation() {
        let now = Local::now();
        let mut form = MaintenanceForm::new_add(now);
        form.name = "Arma rotation".into();
        form.action = MaintenanceAction::RotateMission;
        assert!(
            form.build(0)
                .unwrap_err()
                .contains("deployed mission template")
        );
        form.mission_template = "mission_one.vr".into();
        let definition = form.build(0).unwrap();
        assert_eq!(
            definition
                .policy
                .arma
                .as_ref()
                .unwrap()
                .mission_template
                .as_deref(),
            Some("mission_one.vr")
        );

        form.arma_hooks = true;
        form.announcement = "Mission rotation shortly".into();
        form.announcement_delay_seconds = 10;
        let announced = form.build(0).unwrap();
        assert_eq!(
            MaintenanceForm::from_job(&job(announced.clone()), now)
                .build(0)
                .unwrap(),
            announced
        );

        form.action = MaintenanceAction::Restart;
        form.arma_hooks = false;
        let restarted = form.build(0).unwrap();
        assert_eq!(restarted.policy.arma, None);
    }

    #[test]
    fn arma_announcement_validation_uses_bytes_delay_and_control_characters() {
        let mut form = MaintenanceForm::new_add(Local::now());
        form.name = "Arma restart".into();
        form.arma_hooks = true;
        form.announcement = "x".repeat(257);
        assert!(form.build(0).unwrap_err().contains("256 bytes"));
        form.announcement = "line one\nline two".into();
        assert!(form.build(0).unwrap_err().contains("control characters"));
        form.announcement.clear();
        form.announcement_delay_seconds = 1;
        assert!(
            form.build(0)
                .unwrap_err()
                .contains("requires an announcement")
        );
        form.announcement = "A short message".into();
        form.announcement_delay_seconds = 301;
        assert!(form.build(0).unwrap_err().contains("0 and 300"));
    }

    #[test]
    fn one_time_utc_form_reads_and_writes_utc_wall_time() {
        let now = Local::now();
        let mut form = MaintenanceForm::new_add(now);
        form.name = "Once".into();
        form.kind = ScheduleKind::Once;
        form.utc = true;
        (form.year, form.month, form.day, form.hour, form.minute) = (2099, 1, 2, 3, 4);
        let definition = form.build(0).unwrap();
        let expected = Utc
            .with_ymd_and_hms(2099, 1, 2, 3, 4, 0)
            .unwrap()
            .timestamp_millis() as u64;
        assert_eq!(
            definition.schedule,
            JobSchedule::OnceAt { unix_ms: expected }
        );
        let reopened = MaintenanceForm::from_job(&job(definition), now);
        assert_eq!(
            (
                reopened.year,
                reopened.month,
                reopened.day,
                reopened.hour,
                reopened.minute
            ),
            (2099, 1, 2, 3, 4)
        );
        assert!(
            form.build(expected).is_err(),
            "a past one-time job is refused"
        );
    }

    #[test]
    fn invalid_forms_explain_the_problem_without_saving() {
        let mut form = MaintenanceForm::new_add(Local::now());
        assert!(form.build(0).unwrap_err().contains("job name"));
        form.name = "Restart".into();
        form.weekdays = 0;
        assert_eq!(form.build(0).unwrap_err(), "Select at least one weekday.");
        form.weekdays = WEEKDAYS_ALL;
        form.max_attempts = 9;
        assert_eq!(form.build(0).unwrap_err(), "Attempts must be 1-5.");
        form.max_attempts = 2;
        form.kind = ScheduleKind::Once;
        form.month = 2;
        form.day = 30;
        assert_eq!(form.build(0).unwrap_err(), "Invalid date.");
    }
}
