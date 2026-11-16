use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::super::content_staging::{StageEntry, StageEntryKind, read_bounded_controlled};
use super::super::deployment_records::DeploymentRecordManifest;
use super::super::{
    ContentStageControl, ServerContext, ServerInstanceId, ServerStore, ServerStoreError,
};
use super::{DeploymentPreparationManifest, MAX_BYTES};

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ServerStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_BYTES {
        return Err(ServerStoreError::Invalid(
            "Deployment preparation exceeds 48 MiB".into(),
        ));
    }
    Ok(bytes)
}

pub(super) fn hash_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl ServerStore {
    pub(super) fn preparation_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("deployments/prepared")
    }

    pub(super) fn validate_preparation_context(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().deployment_preparation {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let current =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &current != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }

    pub(super) fn preparation_inputs(
        &self,
        id: &ServerInstanceId,
        record: &DeploymentRecordManifest,
    ) -> (PathBuf, PathBuf) {
        let root = self
            .context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str());
        (
            root.join("configuration/revisions")
                .join(&record.configuration_revision),
            self.stage_root(id).join(&record.content_stage_id),
        )
    }

    pub(super) fn preparation_manifest(
        &self,
        id: &ServerInstanceId,
        record_id: &str,
        record: &DeploymentRecordManifest,
        configuration: &Path,
        content: &Path,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationManifest, ServerStoreError> {
        let (preview, stage) =
            self.inspect_record_archives_at(id, record, configuration, content, control)?;
        let directory = |path: &str| StageEntry {
            path: path.into(),
            kind: StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        };
        let file = |path: String, bytes: &[u8]| StageEntry {
            path,
            kind: StageEntryKind::File,
            bytes: bytes.len() as u64,
            blake3: Some(blake3::hash(bytes).to_hex().to_string()),
        };
        let mut entries = vec![directory("configuration"), directory("content")];
        for item in preview.files {
            entries.push(file(
                format!("configuration/{}", item.relative_path),
                item.contents.as_bytes(),
            ));
        }
        for (prefix, root, limit) in [
            ("configuration", configuration, 4 * 1024 * 1024),
            ("content", content, 32 * 1024 * 1024),
        ] {
            let bytes = read_bounded_controlled(&root.join("manifest.json"), limit, control)?;
            entries.push(file(format!("{prefix}/manifest.json"), &bytes));
        }
        entries.extend(stage.entries.into_iter().map(|mut entry| {
            entry.path = format!("content/{}", entry.path);
            entry
        }));
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let total_bytes = entries.iter().try_fold(0u64, |total, entry| {
            total
                .checked_add(entry.bytes)
                .ok_or(ServerStoreError::Corrupt)
        })?;
        let manifest = DeploymentPreparationManifest {
            schema_version: 1,
            layout_version: 1,
            record_id: record_id.into(),
            record: record.clone(),
            entries,
            total_bytes,
        };
        self.validate_preparation_manifest(id, &manifest)?;
        Ok(manifest)
    }

    pub(super) fn validate_preparation_manifest(
        &self,
        id: &ServerInstanceId,
        manifest: &DeploymentPreparationManifest,
    ) -> Result<(), ServerStoreError> {
        if manifest.schema_version != 1 || manifest.layout_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        self.validate_record_manifest(id, &manifest.record)?;
        super::super::content_staging::validate_generic_entries(
            &manifest.entries,
            manifest.total_bytes,
        )?;
        let record_bytes =
            serde_json::to_vec(&manifest.record).map_err(|_| ServerStoreError::Corrupt)?;
        if manifest.record_id != format!("deployment_{}", blake3::hash(&record_bytes).to_hex())
            || manifest.entries.len() > 100_000
            || manifest.entries.iter().any(|entry| {
                !matches!(
                    entry.path.split('/').next(),
                    Some("configuration" | "content")
                )
            })
        {
            return Err(ServerStoreError::Corrupt);
        }
        encode(manifest)?;
        Ok(())
    }

    pub(super) fn read_preparation_manifest(
        &self,
        id: &ServerInstanceId,
        preparation_id: &str,
        directory: &Path,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationManifest, ServerStoreError> {
        if !hash_id(preparation_id, "prepared_") {
            return Err(ServerStoreError::Invalid(
                "Invalid deployment preparation ID".into(),
            ));
        }
        super::super::store::check_directory_chain(directory)?;
        if !directory.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Err(ServerStoreError::NotFound);
        }
        let bytes = read_bounded_controlled(&directory.join("manifest.json"), MAX_BYTES, control)?;
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
            layout_version: u32,
        }
        let version: Version =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if version.schema_version != 1 || version.layout_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let manifest = serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        self.validate_preparation_manifest(id, &manifest)
            .map_err(|error| {
                if error == ServerStoreError::UnsupportedVersion {
                    error
                } else {
                    ServerStoreError::Corrupt
                }
            })?;
        if preparation_id != format!("prepared_{}", blake3::hash(&encode(&manifest)?).to_hex()) {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(manifest)
    }

    pub(super) fn check_preparation_destination(
        &self,
        id: &ServerInstanceId,
        record: &DeploymentRecordManifest,
    ) -> Result<(), ServerStoreError> {
        let root = self.preparation_root(id);
        self.check_deployment_destination(&root, Some(&record.content_bindings))?;
        let physical = super::super::content_staging::physical_directory(&root)?;
        let (configuration, content) = self.preparation_inputs(id, record);
        for source in [configuration, content] {
            let source = super::super::content_staging::physical_directory(&source)?;
            if super::super::installation::paths_overlap(&physical, &source) {
                return Err(ServerStoreError::UnsafePath);
            }
        }
        Ok(())
    }
}
