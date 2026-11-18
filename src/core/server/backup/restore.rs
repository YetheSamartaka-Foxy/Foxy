mod model;
#[cfg(test)]
mod tests;
mod transaction;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use super::archive::{build_backup_plan_internal, hash_and_measure_file, verify_backup_directory};
use super::model::{
    BackupConsistencyMode, BackupSetKind, MAX_BACKUP_MANIFEST_BYTES, MAX_BACKUP_TOTAL_BYTES,
    ServerBackupManifest, is_forbidden_secret_archive_path, valid_archive_relative_path,
    valid_lower_hex,
};
use crate::core::server::content_staging::{ContentStageControl, read_bounded_controlled};
use crate::core::server::store::{check_directory_chain, check_regular_file};
use crate::core::server::{InstanceRuntimeStatus, ServerInstanceId, ServerStore, ServerStoreError};

use model::{RestoreEntry, RestoreJournal};
pub use model::{
    ServerRestoreOutcome, ServerRestorePending, ServerRestorePlan, ServerRestoreStatus,
};

const DISK_MARGIN: u64 = 16 * 1024 * 1024;
const MAX_ROLLBACK_BYTES: u64 = MAX_BACKUP_TOTAL_BYTES;

fn digest(value: &impl Serialize) -> Result<String, ServerStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn valid_restore_id(value: &str) -> bool {
    value
        .strip_prefix("restore_")
        .is_some_and(|value| valid_lower_hex(value, 64))
}

fn portable_path(path: &str) -> bool {
    valid_archive_relative_path(path)
        && path.len() <= 1024
        && crate::core::server::content_staging::parse_relative(path).is_ok()
}

fn save_target(path: &str) -> Result<Option<&str>, ServerStoreError> {
    let rel = path
        .strip_prefix("saves/")
        .ok_or(ServerStoreError::Corrupt)?;
    if rel == "ownership.json" {
        return Ok(None);
    }
    if !portable_path(rel) || is_forbidden_secret_archive_path(path) {
        return Err(ServerStoreError::UnsafePath);
    }
    let allowed = rel.starts_with("home/profiles/")
        || rel.starts_with("home/config/")
        || rel
            .strip_prefix("headless/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(id, rest)| {
                crate::core::server::valid_headless_id(id)
                    && (rest.starts_with("home/profiles/") || rest.starts_with("home/config/"))
            });
    if !allowed {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(Some(rel))
}

fn measure_optional(
    path: &Path,
    control: &ContentStageControl,
) -> Result<Option<(u64, String)>, ServerStoreError> {
    check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
    check_regular_file(path)?;
    if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
        return Ok(None);
    }
    hash_and_measure_file(path, control).map(Some)
}

fn ordinary_ancestor(path: &Path) -> Result<PathBuf, ServerStoreError> {
    check_directory_chain(path)?;
    let mut current = path;
    while !current.try_exists().map_err(|_| ServerStoreError::Io)? {
        current = current.parent().ok_or(ServerStoreError::UnsafePath)?;
    }
    Ok(current.to_path_buf())
}

impl ServerStore {
    fn restore_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.record_root(id).join("restore")
    }

    pub(in crate::core::server) fn require_no_server_restore(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(), ServerStoreError> {
        let root = self.restore_root(id);
        check_directory_chain(&root)?;
        let journal = root.join("journal.json");
        check_regular_file(&journal)?;
        if journal.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Err(ServerStoreError::Invalid("A server restore requires reviewed rollback before this operation. Inspect server restore status.".into()));
        }
        Ok(())
    }

    pub fn plan_server_restore(
        &self,
        id: &ServerInstanceId,
        backup_id: &str,
        witness: Option<&InstanceRuntimeStatus>,
        control: &ContentStageControl,
    ) -> Result<ServerRestorePlan, ServerStoreError> {
        let result = self
            .restore_plan_internal(id, backup_id, witness, control)
            .map(|(plan, _, _)| plan);
        control.finish(&result);
        result
    }

    fn restore_plan_internal(
        &self,
        id: &ServerInstanceId,
        backup_id: &str,
        witness: Option<&InstanceRuntimeStatus>,
        control: &ContentStageControl,
    ) -> Result<(ServerRestorePlan, ServerBackupManifest, Vec<RestoreEntry>), ServerStoreError>
    {
        self.require_no_server_restore(id)?;
        if !self.capabilities().server_backups {
            return Err(ServerStoreError::UnsupportedGame);
        }
        self.validate_record_context()?;
        self.show(id)?;
        if !super::model::valid_backup_id(backup_id) {
            return Err(ServerStoreError::Invalid(
                "Invalid backup ID format.".into(),
            ));
        }
        let directory = self.backups_root(id).join(backup_id);
        check_directory_chain(&directory)?;
        if !directory.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Err(ServerStoreError::NotFound);
        }
        let (report, manifest) = verify_backup_directory(self, id, backup_id, &directory, control)?;
        let (current, _) = build_backup_plan_internal(
            self,
            id,
            BackupConsistencyMode::StoppedVerified,
            witness,
            control,
        )?;
        let mut blockers = current.blockers;
        let mut warnings = vec!["Only backed-up local save/profile files are restored. Unknown files, secrets and ownership metadata are preserved. Engine/configuration/content references must already match; save formats are never converted.".into()];
        if !manifest.consistency.stopped_verified || !manifest.provider_set.writer_stopped_verified
        {
            blockers.push("This backup has no stopped-consistency witness; live snapshots cannot be used for verified restore.".into());
        }
        if manifest.engine_set != current.engine_set {
            blockers.push("Engine, build, channel, installation receipt or active runtime differs. Select the exact retained compatible deployment before restoring; automatic cross-version migration is unavailable.".into());
        }
        if manifest.configuration_set.active_configuration_revision_id
            != current.configuration_set.active_configuration_revision_id
        {
            blockers.push("The active configuration revision differs from this backup.".into());
        }
        if manifest.content_set.active_deployment_record_id
            != current.content_set.active_deployment_record_id
            || manifest.content_set.content_stage_ids != current.content_set.content_stage_ids
            || manifest.content_set.reforger_addon_lock_digest
                != current.content_set.reforger_addon_lock_digest
        {
            blockers.push(
                "The retained content/scenario/provider selection differs from this backup.".into(),
            );
        }
        if manifest.provider_set.covered_providers != current.provider_set.covered_providers
            || manifest.provider_set.uncovered_providers != current.provider_set.uncovered_providers
        {
            blockers.push("Persistence provider coverage differs from this backup.".into());
        }
        if current.provider_set.reforger_save_tree_version == Some(1)
            && manifest.provider_set.reforger_save_tree_version != Some(1)
        {
            blockers.push("This Reforger backup predates complete native save-tree coverage. Preserve it for manual recovery and create a new stopped backup before verified restore.".into());
        }
        warnings.push(format!(
            "Providers outside this archive remain uncovered: {}.",
            manifest.provider_set.uncovered_providers.join(", ")
        ));
        let current_entries: BTreeMap<_, _> = current
            .entries
            .iter()
            .map(|entry| (entry.archive_path.as_str(), entry))
            .collect();
        let archived_instance: crate::core::server::ServerInstance =
            archive_json(&directory, "metadata/instance.json")?;
        let current_instance = self.show(id)?;
        if archived_instance.id != current_instance.id
            || archived_instance.game_space_id != current_instance.game_space_id
            || archived_instance.installation_id != current_instance.installation_id
            || archived_instance.definition != current_instance.definition
            || archived_instance.configuration_import != current_instance.configuration_import
        {
            blockers.push(
                "Instance configuration or installation mapping differs from this backup.".into(),
            );
        }
        for entry in manifest.entries.iter().filter(|entry| {
            matches!(
                entry.set,
                BackupSetKind::Configuration | BackupSetKind::Content
            ) && !matches!(
                entry.archive_path.as_str(),
                "metadata/instance.json"
                    | "configuration/state.json"
                    | "content/deployments/state.json"
                    | "content/deployments/pins.json"
                    | "content/deployments/activation/active.json"
            )
        }) {
            if current_entries.get(entry.archive_path.as_str()).copied() != Some(entry) {
                blockers.push("Configuration, instance mapping or content metadata bytes differ from this backup.".into());
                break;
            }
        }
        if manifest.engine_set.active_runtime_id.is_none()
            || manifest.engine_set.installation_id.is_none()
        {
            blockers.push(
                "Restore requires a verified retained native engine and active deployment.".into(),
            );
        } else if let Err(error) = self.native_runtime_launch_plan(id, control) {
            control.checkpoint()?;
            if matches!(
                error,
                ServerStoreError::Cancelled | ServerStoreError::DeploymentCancelled
            ) {
                return Err(error);
            }
            blockers.push("The retained native runtime/configuration/content set could not be fully verified.".into());
        }
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        let mut rollback_bytes = 0u64;
        let mut restore_bytes = 0u64;
        let state_root = self.record_root(id).join("runtime-state");
        for entry in manifest
            .entries
            .iter()
            .filter(|entry| entry.set == BackupSetKind::Save)
        {
            let Some(rel) = save_target(&entry.archive_path)? else {
                continue;
            };
            let key = rel.to_ascii_lowercase();
            if !names.insert(key.clone())
                || names.iter().any(|name| {
                    name != &key
                        && (key.starts_with(&format!("{name}/"))
                            || name.starts_with(&format!("{key}/")))
                })
            {
                return Err(ServerStoreError::UnsafePath);
            }
            let prior = measure_optional(&state_root.join(rel), control)?;
            rollback_bytes = rollback_bytes
                .checked_add(prior.as_ref().map_or(0, |(size, _)| *size))
                .ok_or(ServerStoreError::Corrupt)?;
            restore_bytes = restore_bytes
                .checked_add(entry.size_bytes)
                .ok_or(ServerStoreError::Corrupt)?;
            entries.push(RestoreEntry {
                path: rel.into(),
                bytes: entry.size_bytes,
                blake3: entry.blake3.clone(),
                prior,
            });
        }
        if rollback_bytes > MAX_ROLLBACK_BYTES {
            blockers.push("Current save files exceed the bounded rollback budget.".into());
        }
        if entries.is_empty() {
            blockers.push("This archive contains no restorable local save/profile files.".into());
        }
        let required_free_bytes = required_budget(
            report.archive_bytes,
            manifest.total_uncompressed_bytes,
            rollback_bytes,
            restore_bytes,
        )?;
        let available_free_bytes = fs4::available_space(ordinary_ancestor(&self.restore_root(id))?)
            .map_err(|_| ServerStoreError::Io)?;
        if available_free_bytes < required_free_bytes {
            blockers.push("Insufficient disk space for complete staging, preserved current saves and atomic replacement.".into());
        }
        let fingerprint = digest(&(
            self.context().game_id(),
            self.context().space_id(),
            id,
            backup_id,
            &report.archive_blake3,
            &current.engine_set,
            &current.configuration_set,
            &current.content_set,
            &entries,
        ))?;
        Ok((
            ServerRestorePlan {
                instance_id: id.clone(),
                backup_id: backup_id.into(),
                archive_blake3: report.archive_blake3,
                restore_files: entries.len(),
                restore_bytes,
                rollback_bytes,
                required_free_bytes,
                available_free_bytes,
                blockers,
                warnings,
                fingerprint,
                writes_performed: false,
            },
            manifest,
            entries,
        ))
    }

    pub fn server_restore_status(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ServerRestoreStatus, ServerStoreError> {
        self.validate_record_context()?;
        self.show(id)?;
        let pending = self
            .read_restore_journal(id)?
            .map(|(journal, fingerprint)| ServerRestorePending {
                restore_id: journal.restore_id,
                backup_id: journal.backup_id,
                fingerprint,
            });
        Ok(ServerRestoreStatus {
            instance_id: id.clone(),
            pending,
            writes_performed: false,
        })
    }
}

fn archive_json<T: serde::de::DeserializeOwned>(
    directory: &Path,
    name: &str,
) -> Result<T, ServerStoreError> {
    let mut zip = ZipArchive::new(
        File::open(directory.join("archive.zip")).map_err(|_| ServerStoreError::Io)?,
    )
    .map_err(|_| ServerStoreError::Corrupt)?;
    let mut input = zip
        .by_name(name)
        .map_err(|_| ServerStoreError::Corrupt)?
        .take(MAX_BACKUP_MANIFEST_BYTES as u64 + 1);
    let mut bytes = Vec::new();
    input
        .read_to_end(&mut bytes)
        .map_err(|_| ServerStoreError::Io)?;
    if bytes.len() > MAX_BACKUP_MANIFEST_BYTES {
        return Err(ServerStoreError::Corrupt);
    }
    serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)
}

fn required_budget(
    archive: u64,
    expanded: u64,
    rollback: u64,
    largest: u64,
) -> Result<u64, ServerStoreError> {
    archive
        .checked_add(expanded)
        .and_then(|n| n.checked_add(rollback))
        .and_then(|n| n.checked_add(largest))
        .and_then(|n| n.checked_add(DISK_MARGIN))
        .ok_or(ServerStoreError::Corrupt)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ServerStoreError> {
    let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
    check_directory_chain(parent)?;
    check_regular_file(path)?;
    fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| ServerStoreError::Corrupt)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
    temp.write_all(&bytes)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    temp.persist(path).map_err(|_| ServerStoreError::Io)?;
    sync_dir(parent)
}

fn sync_dir(path: &Path) -> Result<(), ServerStoreError> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn copy_verified(
    source: &Path,
    target: &Path,
    expected: &(u64, String),
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    check_directory_chain(target.parent().ok_or(ServerStoreError::UnsafePath)?)?;
    check_regular_file(source)?;
    fs::create_dir_all(target.parent().ok_or(ServerStoreError::UnsafePath)?)
        .map_err(|_| ServerStoreError::Io)?;
    let mut input = File::open(source).map_err(|_| ServerStoreError::Io)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|_| ServerStoreError::Io)?;
    stream_verified(&mut input, &mut output, expected, control)?;
    output.sync_all().map_err(|_| ServerStoreError::Io)
}

fn stream_verified(
    input: &mut impl Read,
    output: &mut impl Write,
    expected: &(u64, String),
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let mut hash = blake3::Hasher::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.checkpoint()?;
        let count = input.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(ServerStoreError::Corrupt)?;
        if total > expected.0 {
            return Err(ServerStoreError::Corrupt);
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|_| ServerStoreError::Io)?;
        control.record_bytes(count as u64);
    }
    if total != expected.0 || hash.finalize().to_hex().as_str() != expected.1 {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(())
}
