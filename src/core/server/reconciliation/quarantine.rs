use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    ContentStageControl, ReconciliationArtifact, ServerInstanceId, ServerStore, ServerStoreError,
};
use crate::core::server::content_staging::{
    StageEntry, StageEntryKind, parse_relative, read_bounded_controlled,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    schema_version: u32,
    context: String,
    instance_id: ServerInstanceId,
    pub kind: ReconciliationArtifact,
    pub original: String,
    pub entries: Vec<StageEntry>,
    pub installation: Option<PathBuf>,
}

impl Receipt {
    pub fn new(
        store: &ServerStore,
        id: &ServerInstanceId,
        kind: ReconciliationArtifact,
        original: String,
        entries: Vec<StageEntry>,
        installation: Option<PathBuf>,
    ) -> Self {
        Self {
            schema_version: 1,
            context: store.activation_context_fingerprint(),
            instance_id: id.clone(),
            kind,
            original,
            entries,
            installation,
        }
    }

    pub fn name(&self) -> Result<String, ServerStoreError> {
        Ok(format!(
            ".reconcile-{}",
            blake3::hash(&serde_json::to_vec(self).map_err(|_| ServerStoreError::Corrupt)?)
                .to_hex()
        ))
    }

    pub fn save(
        &self,
        store: &ServerStore,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        let root = store
            .record_root(&self.instance_id)
            .join("reconciliation-receipts");
        store.check_reconciliation_destination(&root)?;
        super::super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        let name = self.name()?;
        let path = root.join(format!("{name}.json"));
        let bytes = serde_json::to_vec(self).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(ServerStoreError::Invalid(
                "Reconciliation receipt exceeds its size limit".into(),
            ));
        }
        if path.try_exists().map_err(|_| ServerStoreError::Io)? {
            if read_bounded_controlled(&path, 64 * 1024 * 1024, control)? != bytes {
                return Err(ServerStoreError::Corrupt);
            }
            return Ok(());
        }
        let mut temp = tempfile::Builder::new()
            .prefix(".receipt-")
            .tempfile_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        temp.write_all(&bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        control.checkpoint()?;
        match temp.persist_noclobber(&path) {
            Ok(_) => Ok(()),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_bounded_controlled(&path, 64 * 1024 * 1024, control)? == bytes {
                    Ok(())
                } else {
                    Err(ServerStoreError::Corrupt)
                }
            }
            Err(_) => Err(ServerStoreError::Io),
        }
    }
}

pub(super) fn read(
    store: &ServerStore,
    id: &ServerInstanceId,
    kind: ReconciliationArtifact,
    name: &str,
    control: &ContentStageControl,
) -> Result<Receipt, ServerStoreError> {
    if !super::quarantine_name(name) {
        return Err(ServerStoreError::UnsafePath);
    }
    let bytes = read_bounded_controlled(
        &store
            .record_root(id)
            .join("reconciliation-receipts")
            .join(format!("{name}.json")),
        64 * 1024 * 1024,
        control,
    )?;
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if receipt.schema_version != 1
        || receipt.context != store.activation_context_fingerprint()
        || &receipt.instance_id != id
        || receipt.kind != kind
        || receipt.name()? != name
        || !super::owned_name(kind, &receipt.original)
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(receipt)
}

pub(super) fn remove(
    path: &Path,
    entries: &[StageEntry],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    for entry in entries.iter().rev() {
        control.checkpoint()?;
        let target = path.join(parse_relative(&entry.path)?);
        super::super::store::check_directory_chain(
            target.parent().ok_or(ServerStoreError::UnsafePath)?,
        )?;
        let metadata = fs::symlink_metadata(&target).map_err(|_| ServerStoreError::Io)?;
        if metadata.file_type().is_symlink() || super::is_reparse_point(&metadata) {
            return Err(ServerStoreError::UnsafePath);
        }
        match entry.kind {
            StageEntryKind::File if metadata.is_file() => fs::remove_file(&target),
            StageEntryKind::Directory if metadata.is_dir() => fs::remove_dir(&target),
            _ => return Err(ServerStoreError::UnsafePath),
        }
        .map_err(|_| ServerStoreError::Io)?;
    }
    control.checkpoint()?;
    fs::remove_dir(path).map_err(|_| ServerStoreError::Io)
}
