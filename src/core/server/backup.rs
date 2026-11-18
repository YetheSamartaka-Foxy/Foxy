mod archive;
mod model;
mod restore;
mod retention;
#[cfg(test)]
mod tests;

pub use model::{
    BackupConsistencyMode, BackupPruneOutcome, BackupPrunePlan, BackupRetentionPolicy,
    MAX_BACKUP_KEEP_LAST, ServerBackupListing, ServerBackupOutcome, ServerBackupPlan,
    ServerBackupVerificationReport, valid_backup_id, valid_prune_receipt_id,
};
pub use restore::{ServerRestoreOutcome, ServerRestorePlan, ServerRestoreStatus};

impl super::ServerStore {
    pub(in crate::core::server) fn require_stopped_save_writers(
        &self,
        id: &super::ServerInstanceId,
        control: &super::ContentStageControl,
    ) -> Result<(), super::ServerStoreError> {
        control.checkpoint()?;
        self.require_no_server_restore(id)?;
        self.validate_record_context()?;
        let (catalog, _, _) = self.read()?;
        let instance = catalog
            .instances
            .iter()
            .find(|item| &item.id == id)
            .ok_or(super::ServerStoreError::NotFound)?;
        let installation = instance
            .installation_id
            .as_ref()
            .and_then(|installation_id| {
                catalog
                    .installations
                    .iter()
                    .find(|item| &item.id == installation_id)
            });
        let (_, blockers) = archive::inspect_consistency_witness(
            self,
            id,
            instance,
            &catalog.instances,
            installation,
            BackupConsistencyMode::StoppedVerified,
            None,
        )?;
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(super::ServerStoreError::Invalid(blockers.join(" ")))
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackupEvidence {
    pub archive_present: bool,
    pub engine_compatible: bool,
    pub config_compatible: bool,
    pub content_compatible: bool,
    pub provider_compatible: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackupError {
    Incompatible,
    Escape,
    Io,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn backup_owned_files(
    source_dir: &std::path::Path,
    backup_dir: &std::path::Path,
    evidence: &BackupEvidence,
) -> Result<(), BackupError> {
    if !evidence.engine_compatible
        || !evidence.config_compatible
        || !evidence.content_compatible
        || !evidence.provider_compatible
    {
        return Err(BackupError::Incompatible);
    }
    if source_dir == backup_dir
        || backup_dir.starts_with(source_dir)
        || source_dir.starts_with(backup_dir)
    {
        return Err(BackupError::Escape);
    }
    std::fs::create_dir_all(backup_dir).map_err(|_| BackupError::Io)?;
    for entry in std::fs::read_dir(source_dir).map_err(|_| BackupError::Io)? {
        let entry = entry.map_err(|_| BackupError::Io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(BackupError::Escape);
        };
        if name.contains(['/', '\\']) || name == "." || name == ".." {
            return Err(BackupError::Escape);
        }
        let target = backup_dir.join(name);
        if !target.starts_with(backup_dir) {
            return Err(BackupError::Escape);
        }
        if entry.file_type().map_err(|_| BackupError::Io)?.is_file() {
            std::fs::copy(entry.path(), target).map_err(|_| BackupError::Io)?;
        }
    }
    Ok(())
}
