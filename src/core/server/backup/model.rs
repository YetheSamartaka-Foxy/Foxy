use serde::{Deserialize, Serialize};

use crate::core::server::{
    InstallationId, InstanceRuntimePhase, ServerDefinitionKind, ServerInstanceId, ServerStoreError,
};

pub const BACKUP_SCHEMA_VERSION: u32 = 1;
pub const BACKUP_RETENTION_SCHEMA_VERSION: u32 = 1;
pub const BACKUP_PRUNE_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub const MAX_BACKUP_FILES: usize = 4_096;
pub const MAX_BACKUP_SINGLE_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_BACKUP_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_BACKUPS_PER_INSTANCE: usize = 256;
pub const MAX_BACKUP_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BACKUP_RETENTION_BYTES: usize = 256 * 1024;
pub const MIN_BACKUP_TOTAL_BUDGET_BYTES: u64 = 1024 * 1024;
pub const DEFAULT_BACKUP_KEEP_LAST: u16 = 5;
pub const MAX_BACKUP_KEEP_LAST: u16 = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupConsistencyMode {
    #[default]
    StoppedVerified,
    BestEffortLive,
}

impl BackupConsistencyMode {
    pub fn is_stopped(&self) -> bool {
        matches!(self, Self::StoppedVerified)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::StoppedVerified => "stopped_verified",
            Self::BestEffortLive => "best_effort_live",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConsistencyWitness {
    pub mode: BackupConsistencyMode,
    pub stopped_verified: bool,
    pub owner_running: bool,
    pub owner_id: Option<String>,
    pub owner_generation: Option<u64>,
    pub observed_phase: Option<InstanceRuntimePhase>,
    pub observed_run_generation: Option<u64>,
    pub cleanup_verified: bool,
    pub headless_running: usize,
    pub active_consumers: usize,
    pub pending_activation_journal: bool,
    pub pending_installation_maintenance: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_save_path_conflicts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupSetKind {
    Engine,
    Configuration,
    Content,
    Provider,
    Save,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupEngineSet {
    pub installation_id: Option<InstallationId>,
    pub installation_mode: Option<String>,
    pub engine_build_id: Option<String>,
    pub active_deployment_record_id: Option<String>,
    pub active_preparation_id: Option<String>,
    pub active_runtime_id: Option<String>,
    pub runtime_manifest_digest: Option<String>,
    pub payload_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_receipt_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<crate::core::server::ContentChannel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfigurationSet {
    pub instance_revision: u64,
    pub catalog_fingerprint: String,
    pub active_configuration_revision_id: Option<String>,
    pub secret_reference_count: usize,
    pub excluded_secret_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupContentSet {
    pub selected_deployment_record_id: Option<String>,
    pub active_deployment_record_id: Option<String>,
    pub pinned_deployment_records: Vec<String>,
    pub content_stage_ids: Vec<String>,
    pub reforger_addon_lock_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupProviderSet {
    pub covered_providers: Vec<String>,
    pub uncovered_providers: Vec<String>,
    pub writer_stopped_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reforger_save_tree_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_save_path_conflicts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSaveSet {
    pub runtime_state_present: bool,
    pub runtime_state_owner_generation: Option<u64>,
    pub runtime_state_run_generation: Option<u64>,
    pub save_file_count: usize,
    pub save_total_bytes: u64,
    pub excluded_transient_paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupArchiveEntry {
    pub archive_path: String,
    pub set: BackupSetKind,
    pub size_bytes: u64,
    pub blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerBackupManifest {
    pub schema_version: u32,
    pub backup_id: String,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub instance_name: String,
    pub created_unix_ms: u64,
    pub plan_fingerprint: String,
    pub consistency: BackupConsistencyWitness,
    pub engine_set: BackupEngineSet,
    pub configuration_set: BackupConfigurationSet,
    pub content_set: BackupContentSet,
    pub provider_set: BackupProviderSet,
    pub save_set: BackupSaveSet,
    pub total_files: usize,
    pub total_uncompressed_bytes: u64,
    pub entries: Vec<BackupArchiveEntry>,
}

impl ServerBackupManifest {
    pub fn validate(&self) -> Result<(), ServerStoreError> {
        if self.schema_version != BACKUP_SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        if !valid_backup_id(&self.backup_id)
            || self.game_space_id.trim().is_empty()
            || !matches!(self.game_id.as_str(), "arma3" | "reforger")
            || self.instance_name.trim().is_empty()
            || !valid_lower_hex(&self.plan_fingerprint, 64)
            || self.total_files != self.entries.len()
            || self.entries.len() > MAX_BACKUP_FILES
            || self.total_uncompressed_bytes > MAX_BACKUP_TOTAL_BYTES
        {
            return Err(ServerStoreError::Corrupt);
        }
        if self.consistency.mode == BackupConsistencyMode::StoppedVerified
            && !self.consistency.stopped_verified
        {
            return Err(ServerStoreError::Corrupt);
        }
        if !self.consistency.owner_running && self.consistency.owner_id.is_some() {
            return Err(ServerStoreError::Corrupt);
        }
        let mut computed_bytes = 0u64;
        for (index, entry) in self.entries.iter().enumerate() {
            if !valid_archive_relative_path(&entry.archive_path)
                || is_forbidden_secret_archive_path(&entry.archive_path)
                || entry.size_bytes > MAX_BACKUP_SINGLE_FILE_BYTES
                || !valid_lower_hex(&entry.blake3, 64)
            {
                return Err(ServerStoreError::Corrupt);
            }
            if index > 0 && self.entries[index - 1].archive_path >= entry.archive_path {
                return Err(ServerStoreError::Corrupt);
            }
            computed_bytes = computed_bytes
                .checked_add(entry.size_bytes)
                .ok_or(ServerStoreError::Corrupt)?;
        }
        if computed_bytes != self.total_uncompressed_bytes {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServerBackupPlan {
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub instance_name: String,
    pub consistency: BackupConsistencyWitness,
    pub engine_set: BackupEngineSet,
    pub configuration_set: BackupConfigurationSet,
    pub content_set: BackupContentSet,
    pub provider_set: BackupProviderSet,
    pub save_set: BackupSaveSet,
    pub planned_files: usize,
    pub planned_uncompressed_bytes: u64,
    pub entries: Vec<BackupArchiveEntry>,
    pub blockers: Vec<String>,
    pub fingerprint: String,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerBackupOutcome {
    pub instance_id: ServerInstanceId,
    pub manifest: ServerBackupManifest,
    pub archive_bytes: u64,
    pub archive_blake3: String,
    pub verified: bool,
    pub pruned_backups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_receipt_id: Option<String>,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServerBackupSummary {
    pub backup_id: String,
    pub created_unix_ms: u64,
    pub mode: BackupConsistencyMode,
    pub stopped_verified: bool,
    pub pinned: bool,
    pub instance_revision: u64,
    pub active_deployment_record_id: Option<String>,
    pub total_files: usize,
    pub total_uncompressed_bytes: u64,
    pub archive_bytes: u64,
    pub covered_providers: Vec<String>,
    pub uncovered_providers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupRetentionPolicy {
    pub keep_last: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_bytes: Option<u64>,
}

impl Default for BackupRetentionPolicy {
    fn default() -> Self {
        Self {
            keep_last: DEFAULT_BACKUP_KEEP_LAST,
            max_total_bytes: None,
        }
    }
}

impl BackupRetentionPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_BACKUP_KEEP_LAST).contains(&self.keep_last) {
            return Err(format!(
                "Backup retention keep-last count must be 1-{MAX_BACKUP_KEEP_LAST}."
            ));
        }
        if let Some(bytes) = self.max_total_bytes
            && bytes < MIN_BACKUP_TOTAL_BUDGET_BYTES
        {
            return Err(format!(
                "Backup retention size budget must be at least {MIN_BACKUP_TOTAL_BUDGET_BYTES} bytes."
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackupRetentionState {
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub policy: BackupRetentionPolicy,
    pub pinned: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServerBackupListing {
    pub instance_id: ServerInstanceId,
    pub backups: Vec<ServerBackupSummary>,
    pub retention: BackupRetentionState,
    pub total_archive_bytes: u64,
    pub corrupt_backups: Vec<String>,
    pub unrecognized_entries: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServerBackupVerificationReport {
    pub instance_id: ServerInstanceId,
    pub backup_id: String,
    pub valid: bool,
    pub archive_bytes: u64,
    pub archive_blake3: String,
    pub verified_files: usize,
    pub verified_uncompressed_bytes: u64,
    pub consistency: BackupConsistencyWitness,
    pub issues: Vec<String>,
    pub manifest: Option<ServerBackupManifest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupPruneCandidate {
    pub backup_id: String,
    pub created_unix_ms: u64,
    pub mode: BackupConsistencyMode,
    pub archive_bytes: u64,
    pub manifest_fingerprint: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackupRetainedEntry {
    pub backup_id: String,
    pub created_unix_ms: u64,
    pub mode: BackupConsistencyMode,
    pub archive_bytes: u64,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackupPrunePlan {
    pub instance_id: ServerInstanceId,
    pub keep_last: u16,
    pub max_total_bytes: Option<u64>,
    pub pinned: Vec<String>,
    pub retained: Vec<BackupRetainedEntry>,
    pub candidates: Vec<BackupPruneCandidate>,
    pub reclaimable_bytes: u64,
    pub corrupt_backups: Vec<String>,
    pub unrecognized_entries: Vec<String>,
    pub blockers: Vec<String>,
    pub fingerprint: String,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupPruneOutcome {
    pub schema_version: u32,
    pub receipt_id: String,
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub created_unix_ms: u64,
    pub plan_fingerprint: String,
    pub removed: Vec<BackupPruneCandidate>,
    pub reclaimed_bytes: u64,
    pub retained_backups: Vec<String>,
    pub writes_performed: bool,
}

pub(super) fn persistence_providers(game: ServerDefinitionKind) -> (Vec<String>, Vec<String>) {
    match game {
        ServerDefinitionKind::Arma3 => (
            vec!["arma3_profile_namespace".into()],
            vec![
                "arma3_mission_custom_database".into(),
                "unknown_third_party_persistence".into(),
            ],
        ),
        ServerDefinitionKind::Reforger => (
            vec![
                "reforger_local_session_storage".into(),
                "reforger_local_gamemode_storage".into(),
            ],
            vec![
                "reforger_external_api_or_database".into(),
                "unknown_third_party_persistence".into(),
            ],
        ),
    }
}

pub(crate) fn valid_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn valid_backup_id(value: &str) -> bool {
    value
        .strip_prefix("backup_")
        .is_some_and(|suffix| valid_lower_hex(suffix, 64))
}

pub fn valid_prune_receipt_id(value: &str) -> bool {
    value
        .strip_prefix("prune_")
        .is_some_and(|suffix| valid_lower_hex(suffix, 64))
}

pub(super) fn valid_archive_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

pub(super) fn is_forbidden_secret_archive_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or("");
    file_name == "ownertoken.bin"
        || file_name == "join.par"
        || file_name.starts_with("beserver") && file_name.ends_with(".cfg")
        || lower.contains("/private-configuration/")
        || lower.starts_with("private-configuration/")
        || lower.contains("/battleye/")
        || lower.starts_with("battleye/")
        || lower.starts_with("saves/headless/") && lower.contains("/private/")
        || lower.starts_with("headless/") && lower.contains("/private/")
}
