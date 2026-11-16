use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::core::schedule::{JobSchedule, ScheduleClock};
use crate::core::server::{ServerCapabilities, ServerInstanceId};

pub const MAX_JOBS_PER_INSTANCE: usize = 16;
pub const MAINTENANCE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceAction {
    Restart,
    Update,
    Backup,
}

impl MaintenanceAction {
    /// The reason an occurrence of this action cannot execute for a game, or `None` when it can.
    pub fn unavailable_reason(self, capabilities: &ServerCapabilities) -> Option<&'static str> {
        match self {
            Self::Restart if capabilities.lifecycle => None,
            Self::Restart => Some("This game has no native server lifecycle to restart."),
            Self::Update => Some(
                "Unattended installation updates are not available yet. Update the installation explicitly.",
            ),
            Self::Backup => Some("Server backups are not available yet."),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerPolicy {
    #[default]
    Ignore,
    RequireEmpty,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenancePolicy {
    /// A scheduled time missed by more than this many minutes is recorded as skipped, not run late.
    #[serde(default = "default_grace")]
    pub grace_minutes: u16,
    #[serde(default)]
    pub player_policy: PlayerPolicy,
    #[serde(default = "default_defer")]
    pub max_defer_minutes: u16,
    #[serde(default = "default_attempts")]
    pub max_attempts: u8,
    #[serde(default = "default_retry_delay")]
    pub retry_delay_seconds: u16,
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_seconds: u16,
}

fn default_grace() -> u16 {
    10
}
fn default_defer() -> u16 {
    30
}
fn default_attempts() -> u8 {
    2
}
fn default_retry_delay() -> u16 {
    60
}
fn default_stop_timeout() -> u16 {
    60
}

impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self {
            grace_minutes: default_grace(),
            player_policy: PlayerPolicy::default(),
            max_defer_minutes: default_defer(),
            max_attempts: default_attempts(),
            retry_delay_seconds: default_retry_delay(),
            stop_timeout_seconds: default_stop_timeout(),
        }
    }
}

impl MaintenancePolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.grace_minutes > 1440 {
            return Err("The missed-run grace must be 0-1440 minutes.".into());
        }
        if self.max_defer_minutes > 720 {
            return Err("The deferral limit must be 0-720 minutes.".into());
        }
        if !(1..=5).contains(&self.max_attempts) {
            return Err("Attempts must be 1-5.".into());
        }
        if !(10..=3600).contains(&self.retry_delay_seconds) {
            return Err("The retry delay must be 10-3600 seconds.".into());
        }
        if !(5..=120).contains(&self.stop_timeout_seconds) {
            return Err("The stop timeout must be 5-120 seconds.".into());
        }
        Ok(())
    }
}

/// What an operator saves; identity and revision are assigned by the catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceJobDefinition {
    pub name: String,
    pub enabled: bool,
    pub action: MaintenanceAction,
    pub schedule: JobSchedule,
    #[serde(default)]
    pub clock: ScheduleClock,
    #[serde(default)]
    pub policy: MaintenancePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceJob {
    pub id: String,
    /// The approved policy revision; every saved change increments it.
    pub revision: u64,
    /// Scheduled times before this instant belong to an earlier policy and never run.
    pub revised_unix_ms: u64,
    pub definition: MaintenanceJobDefinition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceCatalog {
    pub schema_version: u32,
    pub instance_id: ServerInstanceId,
    pub revision: u64,
    pub jobs: Vec<MaintenanceJob>,
}

impl MaintenanceCatalog {
    pub fn empty(instance_id: ServerInstanceId) -> Self {
        Self {
            schema_version: MAINTENANCE_SCHEMA_VERSION,
            instance_id,
            revision: 0,
            jobs: Vec::new(),
        }
    }

    pub fn job(&self, id: &str) -> Option<&MaintenanceJob> {
        self.jobs.iter().find(|job| job.id == id)
    }

    pub fn validate(&self, instance_id: &ServerInstanceId) -> Result<(), String> {
        if self.schema_version != MAINTENANCE_SCHEMA_VERSION || &self.instance_id != instance_id {
            return Err("The maintenance catalog belongs to another instance or schema.".into());
        }
        if self.jobs.len() > MAX_JOBS_PER_INSTANCE {
            return Err(format!(
                "An instance can have at most {MAX_JOBS_PER_INSTANCE} maintenance jobs."
            ));
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for job in &self.jobs {
            if !valid_job_id(&job.id) || !ids.insert(job.id.as_str()) {
                return Err("Invalid or duplicate maintenance job id.".into());
            }
            if job.revision == 0 {
                return Err("A maintenance job revision must be at least 1.".into());
            }
            job.definition.validate()?;
            if !names.insert(job.definition.name.to_lowercase()) {
                return Err("Maintenance job names must be unique.".into());
            }
        }
        Ok(())
    }
}

impl MaintenanceJobDefinition {
    pub fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty()
            || name.chars().count() > 48
            || name != self.name
            || self.name.chars().any(char::is_control)
        {
            return Err(
                "A job name must be 1-48 characters without surrounding spaces or control characters."
                    .into(),
            );
        }
        match &self.schedule {
            JobSchedule::OnceAt { unix_ms } if *unix_ms == 0 => {
                return Err("A one-time job needs a time.".into());
            }
            JobSchedule::Recurring {
                minutes_of_day,
                weekdays,
            } if *minutes_of_day >= 1440 || *weekdays == 0 || *weekdays > 0b0111_1111 => {
                return Err("A weekly job needs a time of day and at least one weekday.".into());
            }
            _ => {}
        }
        self.policy.validate()
    }
}

pub fn new_job_id() -> String {
    format!("job_{:016x}", rand::random::<u64>())
}

pub fn valid_job_id(value: &str) -> bool {
    value.strip_prefix("job_").is_some_and(|suffix| {
        suffix.len() == 16
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// A saved catalog change. Adding assigns a new id; changing a job's definition advances its
/// policy revision, which ends open occurrences of the old revision unless they already stopped
/// the server.
#[derive(Clone, Debug)]
pub enum MaintenanceChange {
    Add(MaintenanceJobDefinition),
    Set {
        id: String,
        definition: MaintenanceJobDefinition,
    },
    Remove {
        id: String,
    },
}

pub(in crate::core::server) fn apply_change(
    catalog: &MaintenanceCatalog,
    change: MaintenanceChange,
    now_unix_ms: u64,
) -> Result<(MaintenanceCatalog, Option<String>), String> {
    let mut next = catalog.clone();
    let affected = match change {
        MaintenanceChange::Add(definition) => {
            let id = loop {
                let candidate = new_job_id();
                if next.job(&candidate).is_none() {
                    break candidate;
                }
            };
            next.jobs.push(MaintenanceJob {
                id: id.clone(),
                revision: 1,
                revised_unix_ms: now_unix_ms,
                definition,
            });
            Some(id)
        }
        MaintenanceChange::Set { id, definition } => {
            let job = next
                .jobs
                .iter_mut()
                .find(|job| job.id == id)
                .ok_or_else(|| "No maintenance job with that id exists.".to_string())?;
            if job.definition != definition {
                job.definition = definition;
                job.revision = job
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| "Job revisions are exhausted.".to_string())?;
                job.revised_unix_ms = now_unix_ms;
            }
            Some(id)
        }
        MaintenanceChange::Remove { id } => {
            let before = next.jobs.len();
            next.jobs.retain(|job| job.id != id);
            if next.jobs.len() == before {
                return Err("No maintenance job with that id exists.".into());
            }
            None
        }
    };
    next.revision = next
        .revision
        .checked_add(1)
        .ok_or_else(|| "Catalog revisions are exhausted.".to_string())?;
    next.validate(&catalog.instance_id)?;
    Ok((next, affected))
}
