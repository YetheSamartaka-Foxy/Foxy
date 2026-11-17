use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};

pub(crate) const RECORD_SCHEMA: u32 = 1;
pub(crate) const RETAINED_RECORDS: usize = 64;
pub(crate) const MAX_STORED_RESPONSE: usize = 16 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_LISTED: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RconCommandState {
    Queued,
    Sending,
    Replied,
    /// Sent without a complete reply: the effect may have happened, so it is never resent.
    Unknown,
    /// Never transmitted, so the command had no effect.
    NotSent,
}

impl RconCommandState {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Replied | Self::Unknown | Self::NotSent)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RconCommandReason {
    NoReply,
    IncompleteReply,
    InvalidReply,
    ReceiveFailed,
    NotConnected,
    SendFailed,
    RunChanged,
    OwnerLost,
    TargetChanged,
}

impl RconCommandReason {
    pub fn describe(self) -> &'static str {
        match self {
            Self::NoReply => "The command was sent once and no reply arrived before the deadline.",
            Self::IncompleteReply => "The command was sent and only part of the reply arrived.",
            Self::InvalidReply => "The command was sent and the reply was malformed.",
            Self::ReceiveFailed => "The command was sent and receiving the reply failed.",
            Self::NotConnected => "RCon was not connected, so nothing was sent.",
            Self::SendFailed => "The command could not be transmitted.",
            Self::RunChanged => "The server run changed before the command was sent.",
            Self::OwnerLost => "The owner stopped while this command was in progress.",
            Self::TargetChanged => {
                "The current server reply could not verify the exact reviewed target; nothing was sent."
            }
        }
    }
}

impl From<super::UnknownReason> for RconCommandReason {
    fn from(reason: super::UnknownReason) -> Self {
        match reason {
            super::UnknownReason::NoReply => Self::NoReply,
            super::UnknownReason::IncompleteReply => Self::IncompleteReply,
            super::UnknownReason::InvalidReply => Self::InvalidReply,
            super::UnknownReason::ReceiveFailed => Self::ReceiveFailed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RconCommandRecord {
    pub schema: u32,
    pub command_id: String,
    pub instance_id: String,
    pub owner_id: String,
    pub owner_generation: u64,
    pub run_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_generation: Option<u32>,
    pub text: String,
    pub state: RconCommandState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<RconCommandReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub response_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) messages: Vec<super::ServerText>,
    pub requested_unix_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_unix_ms: Option<i64>,
}

impl RconCommandRecord {
    pub(crate) fn new(
        command_id: String,
        instance_id: &ServerInstanceId,
        owner_id: &str,
        owner_generation: u64,
        run_generation: u64,
        text: &str,
    ) -> Self {
        Self {
            schema: RECORD_SCHEMA,
            command_id,
            instance_id: instance_id.as_str().into(),
            owner_id: owner_id.into(),
            owner_generation,
            run_generation,
            session_generation: None,
            text: text.into(),
            state: RconCommandState::Queued,
            reason: None,
            response: None,
            response_truncated: false,
            messages: Vec::new(),
            requested_unix_ms: super::unix_ms(),
            sent_unix_ms: None,
            finished_unix_ms: None,
        }
    }

    pub(crate) fn finish(&mut self, state: RconCommandState, reason: Option<RconCommandReason>) {
        self.state = state;
        self.reason = reason;
        self.finished_unix_ms = Some(super::unix_ms());
    }

    pub(crate) fn set_response(&mut self, text: String) {
        let mut text = text;
        if text.len() > MAX_STORED_RESPONSE {
            let mut end = MAX_STORED_RESPONSE;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            self.response_truncated = true;
        }
        self.response = Some(text);
    }

    fn validate(&self, file_name: &str) -> bool {
        self.schema == RECORD_SCHEMA
            && valid_command_id(&self.command_id)
            && self.messages.len() <= 64
            && self
                .messages
                .iter()
                .map(|message| message.text.len())
                .sum::<usize>()
                <= MAX_STORED_RESPONSE
            && file_name == format!("{}.json", self.command_id)
            && foxy_supervisor::control::valid_rcon_text(&self.text)
            && self.owner_generation != 0
            && self.run_generation != 0
            && self
                .response
                .as_ref()
                .is_none_or(|response| response.len() <= MAX_STORED_RESPONSE)
            && ServerInstanceId::try_from(self.instance_id.clone()).is_ok()
    }
}

/// Deterministic per owner request, so a replayed request can never claim a second record.
pub fn command_id(owner_id: &str, owner_generation: u64, client: &str, request_id: u64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"foxy rcon command v1\0");
    hasher.update(owner_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(&owner_generation.to_le_bytes());
    hasher.update(client.as_bytes());
    hasher.update(b"\0");
    hasher.update(&request_id.to_le_bytes());
    let digest = hasher.finalize();
    format!("cmd_{}", hex::encode(&digest.as_bytes()[..16]))
}

pub fn valid_command_id(value: &str) -> bool {
    value.strip_prefix("cmd_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn record_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "RCon record path is not a directory",
                ))
            }
        }
        Err(error) => Err(error),
    }
}

fn write(root: &Path, record: &RconCommandRecord, replace: bool) -> io::Result<bool> {
    let bytes = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    let path = record_path(root, &record.command_id);
    if replace {
        file.persist(path).map_err(|error| error.error)?;
        return Ok(true);
    }
    match file.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

/// Durably records the command before it is queued. `false` means it already exists.
pub(crate) fn claim(root: &Path, record: &RconCommandRecord) -> io::Result<bool> {
    let parent = root
        .parent()
        .ok_or_else(|| io::Error::other("Invalid RCon root"))?;
    create_private_dir(parent)?;
    create_private_dir(root)?;
    write(root, record, false)
}

pub(crate) fn read(root: &Path, id: &str) -> io::Result<Option<RconCommandRecord>> {
    if !valid_command_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid command id",
        ));
    }
    let bytes = match crate::core::server::store::read_bounded(&record_path(root, id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RCon record is too large",
        ));
    }
    let record: RconCommandRecord = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid RCon record"))?;
    if !record.validate(&format!("{id}.json")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid RCon record",
        ));
    }
    Ok(Some(record))
}

/// Only the owner that claimed an open record may change it.
pub(crate) fn update(root: &Path, record: &RconCommandRecord) -> io::Result<()> {
    let current = read(root, &record.command_id)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "RCon record vanished"))?;
    if current.owner_id != record.owner_id
        || current.owner_generation != record.owner_generation
        || current.state.terminal()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "RCon record belongs to another owner or already ended",
        ));
    }
    write(root, record, true).map(drop)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RconCommandListing {
    pub records: Vec<RconCommandRecord>,
    pub unreadable: usize,
    pub truncated: bool,
}

pub(crate) fn list(root: &Path) -> io::Result<RconCommandListing> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(RconCommandListing::default());
        }
        Err(error) => return Err(error),
    };
    let mut listing = RconCommandListing::default();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_LISTED {
            listing.truncated = true;
            break;
        }
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|id| valid_command_id(id))
            .map(str::to_owned)
        else {
            continue;
        };
        match read(root, &id) {
            Ok(Some(record)) => listing.records.push(record),
            Ok(None) => {}
            Err(_) => listing.unreadable += 1,
        }
    }
    listing.records.sort_by_key(|record| {
        std::cmp::Reverse((record.requested_unix_ms, record.command_id.clone()))
    });
    Ok(listing)
}

/// Open records left by another owner can never be confirmed: queued ones were never sent,
/// sending ones may have reached the server.
pub(crate) fn recover_abandoned(
    root: &Path,
    owner_id: &str,
    owner_generation: u64,
) -> io::Result<usize> {
    let listing = list(root)?;
    let mut recovered = 0;
    for mut record in listing.records {
        if record.state.terminal()
            || (record.owner_id == owner_id && record.owner_generation == owner_generation)
        {
            continue;
        }
        let state = if record.state == RconCommandState::Queued {
            RconCommandState::NotSent
        } else {
            RconCommandState::Unknown
        };
        record.finish(state, Some(RconCommandReason::OwnerLost));
        write(root, &record, true)?;
        recovered += 1;
    }
    Ok(recovered)
}

pub(crate) fn prune(root: &Path) -> io::Result<usize> {
    let listing = list(root)?;
    let mut removed = 0;
    for record in listing
        .records
        .iter()
        .filter(|record| record.state.terminal())
        .skip(RETAINED_RECORDS)
    {
        match fs::remove_file(record_path(root, &record.command_id)) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(removed)
}

impl ServerStore {
    pub(crate) fn rcon_command_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("rcon/commands")
    }

    pub fn rcon_history(
        &self,
        id: &ServerInstanceId,
    ) -> Result<RconCommandListing, ServerStoreError> {
        self.show(id)?;
        let root = self.rcon_command_root(id);
        if root.exists() {
            crate::core::server::store::check_directory_chain(&root)?;
        }
        list(&root).map_err(|_| ServerStoreError::Io)
    }

    pub fn rcon_command(
        &self,
        id: &ServerInstanceId,
        command_id: &str,
    ) -> Result<Option<RconCommandRecord>, ServerStoreError> {
        self.show(id)?;
        read(&self.rcon_command_root(id), command_id).map_err(|_| ServerStoreError::Corrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(owner: &str, generation: u64, request: u64) -> RconCommandRecord {
        let id = ServerInstanceId::try_from(format!("server_{}", "2".repeat(32))).unwrap();
        RconCommandRecord::new(
            command_id(owner, generation, "client", request),
            &id,
            owner,
            generation,
            3,
            "players",
        )
    }

    #[test]
    fn command_ids_are_deterministic_per_owner_request() {
        let first = command_id("owner_a", 1, "client", 7);
        assert_eq!(first, command_id("owner_a", 1, "client", 7));
        assert!(valid_command_id(&first));
        for other in [
            command_id("owner_b", 1, "client", 7),
            command_id("owner_a", 2, "client", 7),
            command_id("owner_a", 1, "other", 7),
            command_id("owner_a", 1, "client", 8),
        ] {
            assert_ne!(first, other);
        }
        assert!(!valid_command_id("cmd_../x"));
    }

    #[test]
    fn claims_never_clobber_and_only_the_claiming_owner_updates_open_records() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("rcon/commands");
        let mut mine = record("owner_a", 1, 1);
        assert!(claim(&root, &mine).unwrap());
        assert!(!claim(&root, &mine).unwrap());
        let mut foreign = mine.clone();
        foreign.owner_id = "owner_b".into();
        assert!(update(&root, &foreign).is_err());
        mine.state = RconCommandState::Sending;
        update(&root, &mine).unwrap();
        mine.set_response("x".repeat(MAX_STORED_RESPONSE + 10));
        mine.finish(RconCommandState::Replied, None);
        update(&root, &mine).unwrap();
        let stored = read(&root, &mine.command_id).unwrap().unwrap();
        assert!(stored.response_truncated);
        assert_eq!(stored.response.unwrap().len(), MAX_STORED_RESPONSE);
        assert!(update(&root, &mine).is_err(), "terminal records are final");
    }

    #[test]
    fn abandoned_records_become_not_sent_or_unknown_and_are_never_reopened() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("rcon/commands");
        let queued = record("owner_old", 1, 1);
        let mut sending = record("owner_old", 1, 2);
        sending.state = RconCommandState::Sending;
        let current = record("owner_new", 1, 3);
        for item in [&queued, &sending, &current] {
            claim(&root, item).unwrap();
        }
        assert_eq!(recover_abandoned(&root, "owner_new", 1).unwrap(), 2);
        let queued = read(&root, &queued.command_id).unwrap().unwrap();
        assert_eq!(
            (queued.state, queued.reason),
            (
                RconCommandState::NotSent,
                Some(RconCommandReason::OwnerLost)
            )
        );
        let sending = read(&root, &sending.command_id).unwrap().unwrap();
        assert_eq!(
            (sending.state, sending.reason),
            (
                RconCommandState::Unknown,
                Some(RconCommandReason::OwnerLost)
            )
        );
        assert_eq!(
            read(&root, &current.command_id).unwrap().unwrap().state,
            RconCommandState::Queued
        );
        assert_eq!(recover_abandoned(&root, "owner_new", 1).unwrap(), 0);
    }

    #[test]
    fn retention_keeps_the_newest_terminal_records_and_every_open_one() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("rcon/commands");
        for request in 0..(RETAINED_RECORDS as u64 + 5) {
            let mut item = record("owner_a", 1, request);
            item.requested_unix_ms = 1_000 + i64::try_from(request).unwrap();
            item.finish(RconCommandState::Replied, None);
            claim(&root, &item).unwrap();
        }
        let mut open = record("owner_a", 1, 999);
        open.requested_unix_ms = 1;
        claim(&root, &open).unwrap();
        fs::write(root.join(format!("cmd_{}.json", "f".repeat(32))), b"{").unwrap();
        assert_eq!(prune(&root).unwrap(), 5);
        let listing = list(&root).unwrap();
        assert_eq!(listing.unreadable, 1);
        assert_eq!(listing.records.len(), RETAINED_RECORDS + 1);
        assert!(
            listing
                .records
                .iter()
                .any(|item| item.command_id == open.command_id)
        );
        assert_eq!(
            listing.records[0].requested_unix_ms,
            1_000 + RETAINED_RECORDS as i64 + 4
        );
    }
}

#[cfg(test)]
mod example_tests {
    use super::*;

    #[test]
    fn public_examples_are_a_valid_command_record_and_instance() {
        let record: RconCommandRecord = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/server_rcon_command.json"
        ))
        .unwrap();
        assert!(record.validate(&format!("{}.json", record.command_id)));
        assert_eq!(record.state, RconCommandState::Replied);
        let instance: crate::core::server::ServerInstance = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/arma3_rcon_instance.json"
        ))
        .unwrap();
        instance
            .validate("arma3", crate::core::server::ServerDefinitionKind::Arma3)
            .unwrap();
        let settings = super::super::draft_settings(&instance).unwrap();
        assert_eq!((settings.port, settings.password_stored), (2310, true));
    }
}
