use super::*;

#[derive(Clone, Debug, Serialize)]
pub struct ServerRestorePlan {
    pub instance_id: ServerInstanceId,
    pub backup_id: String,
    pub archive_blake3: String,
    pub restore_files: usize,
    pub restore_bytes: u64,
    pub rollback_bytes: u64,
    pub required_free_bytes: u64,
    pub available_free_bytes: u64,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub fingerprint: String,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerRestorePending {
    pub restore_id: String,
    pub backup_id: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServerRestoreStatus {
    pub instance_id: ServerInstanceId,
    pub pending: Option<ServerRestorePending>,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerRestoreOutcome {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub restore_id: String,
    pub instance_id: ServerInstanceId,
    pub backup_id: String,
    pub plan_fingerprint: String,
    pub restored_files: usize,
    pub restored_bytes: u64,
    pub rolled_back: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreEntry {
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
    pub prior: Option<(u64, String)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreJournal {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub restore_id: String,
    pub backup_id: String,
    pub plan_fingerprint: String,
    pub entries: Vec<RestoreEntry>,
}
