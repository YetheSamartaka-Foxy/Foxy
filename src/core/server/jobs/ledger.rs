use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::MaintenanceAction;
use crate::core::server::ServerInstanceId;

const OCCURRENCE_SCHEMA_VERSION: u32 = 1;
const MAX_OCCURRENCE_BYTES: usize = 16 * 1024;
const MAX_LISTED_OCCURRENCES: usize = 512;
/// Terminal occurrences beyond this count are pruned oldest first; open ones are never pruned.
pub const RETAINED_TERMINAL_OCCURRENCES: usize = 64;
const MAX_MESSAGE_CHARS: usize = 240;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OccurrenceTrigger {
    Scheduled {
        scheduled_unix_ms: u64,
    },
    Manual {
        requested_unix_ms: u64,
        request: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceState {
    Claimed,
    Deferred,
    Running,
    RetryWait,
    Succeeded,
    Failed,
    Skipped,
    Blocked,
    Cancelled,
    Unknown,
}

impl OccurrenceState {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Skipped
                | Self::Blocked
                | Self::Cancelled
                | Self::Unknown
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccurrenceReason {
    ServerNotRunning,
    StoppedOutsideJob,
    StartedOutsideJob,
    PlayersPresent,
    PlayerCountUnknown,
    OperationBusy,
    AdapterUnavailable,
    MissedWindow,
    OwnerLost,
    CancelledByOperator,
    StartPlanUnavailable,
    AttemptFailed,
    OverlappingOccurrence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    pub schema_version: u32,
    pub occurrence_id: String,
    pub instance_id: ServerInstanceId,
    pub job_id: String,
    pub policy_revision: u64,
    pub action: MaintenanceAction,
    pub trigger: OccurrenceTrigger,
    pub owner_id: String,
    pub owner_generation: u64,
    pub state: OccurrenceState,
    pub reason: Option<OccurrenceReason>,
    pub attempts: u8,
    /// This occurrence's own restart stopped the server, so a retry may start it again.
    pub stop_completed: bool,
    pub run_generation_before: Option<u64>,
    pub run_generation_after: Option<u64>,
    pub deferred_since_unix_ms: Option<u64>,
    pub next_attempt_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_consistency: Option<crate::core::server::BackupConsistencyMode>,
    pub message: Option<String>,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
}

impl Occurrence {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::core::server) fn new(
        instance_id: ServerInstanceId,
        job_id: &str,
        policy_revision: u64,
        action: MaintenanceAction,
        trigger: OccurrenceTrigger,
        owner_id: &str,
        owner_generation: u64,
        now_unix_ms: u64,
    ) -> Self {
        Self {
            schema_version: OCCURRENCE_SCHEMA_VERSION,
            occurrence_id: occurrence_id(&instance_id, job_id, policy_revision, &trigger),
            instance_id,
            job_id: job_id.into(),
            policy_revision,
            action,
            trigger,
            owner_id: owner_id.into(),
            owner_generation,
            state: OccurrenceState::Claimed,
            reason: None,
            attempts: 0,
            stop_completed: false,
            run_generation_before: None,
            run_generation_after: None,
            deferred_since_unix_ms: None,
            next_attempt_unix_ms: None,
            backup_id: None,
            backup_consistency: None,
            message: None,
            created_unix_ms: now_unix_ms,
            updated_unix_ms: now_unix_ms,
        }
    }

    pub(in crate::core::server) fn set_message(&mut self, message: impl AsRef<str>) {
        let text: String = message
            .as_ref()
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_MESSAGE_CHARS)
            .collect();
        self.message = (!text.is_empty()).then_some(text);
    }

    fn validate(&self, file_name: &str) -> bool {
        self.schema_version == OCCURRENCE_SCHEMA_VERSION
            && valid_occurrence_id(&self.occurrence_id)
            && file_name == format!("{}.json", self.occurrence_id)
            && self.occurrence_id
                == occurrence_id(
                    &self.instance_id,
                    &self.job_id,
                    self.policy_revision,
                    &self.trigger,
                )
            && super::model::valid_job_id(&self.job_id)
            && self.policy_revision > 0
            && self
                .backup_id
                .as_deref()
                .is_none_or(crate::core::server::valid_backup_id)
            && self
                .message
                .as_ref()
                .is_none_or(|message| message.chars().count() <= MAX_MESSAGE_CHARS)
    }
}

/// Deterministic for a scheduled time or a manual request, so a reconnecting client, a second
/// owner or a repeated tick finds the existing record instead of creating a duplicate effect.
pub fn occurrence_id(
    instance_id: &ServerInstanceId,
    job_id: &str,
    policy_revision: u64,
    trigger: &OccurrenceTrigger,
) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"foxy-maintenance-occurrence-v1\0");
    hash.update(instance_id.as_str().as_bytes());
    hash.update(&[0]);
    hash.update(job_id.as_bytes());
    hash.update(&[0]);
    match trigger {
        OccurrenceTrigger::Scheduled { scheduled_unix_ms } => {
            hash.update(b"scheduled\0");
            hash.update(&policy_revision.to_le_bytes());
            hash.update(&scheduled_unix_ms.to_le_bytes());
        }
        OccurrenceTrigger::Manual { request, .. } => {
            hash.update(b"manual\0");
            hash.update(request.as_bytes());
        }
    }
    format!("occ_{}", &hash.finalize().to_hex()[..32])
}

pub fn valid_occurrence_id(value: &str) -> bool {
    value.strip_prefix("occ_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn occurrence_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

fn write_new(root: &Path, occurrence: &Occurrence) -> io::Result<bool> {
    let bytes = serde_json::to_vec_pretty(occurrence).map_err(io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(occurrence_path(root, &occurrence.occurrence_id)) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

fn replace(root: &Path, occurrence: &Occurrence) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(occurrence).map_err(io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(occurrence_path(root, &occurrence.occurrence_id))
        .map_err(|error| error.error)?;
    Ok(())
}

/// Durably records the occurrence before any effect. `false` means it already exists, so the
/// caller must not act on it again.
pub(in crate::core::server) fn claim(root: &Path, occurrence: &Occurrence) -> io::Result<bool> {
    fs::create_dir_all(root)?;
    write_new(root, occurrence)
}

pub(in crate::core::server) fn read(root: &Path, id: &str) -> io::Result<Option<Occurrence>> {
    if !valid_occurrence_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid occurrence id",
        ));
    }
    let path = occurrence_path(root, id);
    let bytes = match crate::core::server::store::read_bounded(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if bytes.len() > MAX_OCCURRENCE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Occurrence record is too large",
        ));
    }
    let occurrence: Occurrence = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid occurrence record"))?;
    if !occurrence.validate(&format!("{id}.json")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid occurrence record",
        ));
    }
    Ok(Some(occurrence))
}

/// Only the owner that claimed an open occurrence may change it.
pub(in crate::core::server) fn update(root: &Path, occurrence: &Occurrence) -> io::Result<()> {
    let current = read(root, &occurrence.occurrence_id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Occurrence vanished"))?;
    if current.owner_id != occurrence.owner_id
        || current.owner_generation != occurrence.owner_generation
        || current.state.terminal()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Occurrence belongs to another owner or already ended",
        ));
    }
    replace(root, occurrence)
}

pub struct OccurrenceListing {
    pub occurrences: Vec<Occurrence>,
    pub unreadable: usize,
    pub truncated: bool,
}

pub(in crate::core::server) fn list(root: &Path) -> io::Result<OccurrenceListing> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(OccurrenceListing {
                occurrences: Vec::new(),
                unreadable: 0,
                truncated: false,
            });
        }
        Err(error) => return Err(error),
    };
    let mut names: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if !valid_occurrence_id(id) {
            continue;
        }
        if names.len() >= MAX_LISTED_OCCURRENCES {
            truncated = true;
            break;
        }
        names.push(id.into());
    }
    let mut occurrences = Vec::new();
    let mut unreadable = 0;
    for id in names {
        match read(root, &id) {
            Ok(Some(occurrence)) => occurrences.push(occurrence),
            Ok(None) => {}
            Err(_) => unreadable += 1,
        }
    }
    occurrences.sort_by(|left, right| {
        (right.created_unix_ms, &right.occurrence_id)
            .cmp(&(left.created_unix_ms, &left.occurrence_id))
    });
    Ok(OccurrenceListing {
        occurrences,
        unreadable,
        truncated,
    })
}

/// An occurrence left open by an owner that ended is never re-executed: it becomes Unknown so the
/// operator inspects the server first.
pub(in crate::core::server) fn recover_abandoned(
    root: &Path,
    current_owner: &str,
    now_unix_ms: u64,
) -> io::Result<usize> {
    let listing = list(root)?;
    let mut recovered = 0;
    for mut occurrence in listing.occurrences {
        if occurrence.state.terminal() || occurrence.owner_id == current_owner {
            continue;
        }
        occurrence.state = OccurrenceState::Unknown;
        occurrence.reason = Some(OccurrenceReason::OwnerLost);
        occurrence.next_attempt_unix_ms = None;
        occurrence.set_message(
            "The owner ended during this occurrence. Its effect is unknown; inspect the server before running the job again.",
        );
        occurrence.updated_unix_ms = now_unix_ms;
        replace(root, &occurrence)?;
        recovered += 1;
    }
    Ok(recovered)
}

/// Removes the oldest terminal records beyond the retention limit. Each job's latest scheduled
/// record is always kept, because deleting it would let its scheduled time be claimed again.
/// Unreadable records are never touched.
pub(in crate::core::server) fn prune(root: &Path) -> io::Result<usize> {
    let listing = list(root)?;
    let mut latest_scheduled: std::collections::HashMap<&str, (u64, &str)> =
        std::collections::HashMap::new();
    for occurrence in &listing.occurrences {
        if let OccurrenceTrigger::Scheduled { scheduled_unix_ms } = occurrence.trigger {
            let entry = latest_scheduled
                .entry(occurrence.job_id.as_str())
                .or_insert((scheduled_unix_ms, occurrence.occurrence_id.as_str()));
            if scheduled_unix_ms > entry.0 {
                *entry = (scheduled_unix_ms, occurrence.occurrence_id.as_str());
            }
        }
    }
    let protected: std::collections::HashSet<&str> =
        latest_scheduled.values().map(|(_, id)| *id).collect();
    let mut terminal: Vec<&Occurrence> = listing
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.state.terminal())
        .collect();
    terminal.sort_by_key(|occurrence| std::cmp::Reverse(occurrence.updated_unix_ms));
    let mut removed = 0;
    for occurrence in terminal
        .into_iter()
        .skip(RETAINED_TERMINAL_OCCURRENCES)
        .filter(|occurrence| !protected.contains(occurrence.occurrence_id.as_str()))
    {
        let path = occurrence_path(root, &occurrence.occurrence_id);
        if read(root, &occurrence.occurrence_id)?.as_ref() == Some(occurrence) {
            fs::remove_file(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}
