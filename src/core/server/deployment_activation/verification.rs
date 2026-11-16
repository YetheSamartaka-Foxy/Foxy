use std::path::Path;

use super::runtime::{MAX_MANIFEST_BYTES, entry_bytes, fingerprint, hash_id};
use super::{NativeRuntimeManifest, NativeRuntimeReference};
use crate::core::server::content_staging::{
    StageEntry, read_bounded_controlled, validate_generic_entries, verify_subtree,
};
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError, runtime_payload,
};

fn verify_content_root(
    directory: &Path,
    prefix: &str,
    entries: &[StageEntry],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let root = directory.join(prefix);
    if !entries
        .iter()
        .any(|entry| entry.path.starts_with(&format!("{prefix}/")))
        && !root.try_exists().map_err(|_| ServerStoreError::Io)?
    {
        return Ok(());
    }
    verify_subtree(entries, directory, prefix, control)
}

impl ServerStore {
    pub(in crate::core::server) fn verify_native_runtime(
        &self,
        id: &ServerInstanceId,
        reference: &NativeRuntimeReference,
        control: &ContentStageControl,
    ) -> Result<NativeRuntimeManifest, ServerStoreError> {
        self.verify_runtime_at(
            id,
            reference,
            &self.runtime_directory(id, reference)?,
            control,
        )
    }

    pub(super) fn verify_runtime_at(
        &self,
        id: &ServerInstanceId,
        reference: &NativeRuntimeReference,
        directory: &Path,
        control: &ContentStageControl,
    ) -> Result<NativeRuntimeManifest, ServerStoreError> {
        super::super::store::check_directory_chain(directory)?;
        let bytes = read_bounded_controlled(
            &directory.join("manifest.json"),
            MAX_MANIFEST_BYTES,
            control,
        )?;
        let manifest: NativeRuntimeManifest =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if manifest.schema_version != 1
            || (manifest.installation.game == super::super::ServerDefinitionKind::Arma3
                && manifest.reforger_addons.is_some())
            || manifest.game_space_id != self.context().space_id()
            || &manifest.instance_id != id
            || fingerprint(&manifest)? != reference.manifest_fingerprint
            || !hash_id(&manifest.preparation_id, "prepared_")
            || self.capabilities().definition_kind != Some(manifest.installation.game)
            || !manifest.installation.validate(manifest.installation.game)
            || super::super::engine_layout::observe_tested_layout(&manifest.installation).is_none()
        {
            return Err(ServerStoreError::Corrupt);
        }
        let private_keys =
            if manifest.installation.game == super::super::ServerDefinitionKind::Arma3 {
                super::super::keys::runtime::private_keys(
                    manifest.key_trust.as_deref(),
                    id,
                    &manifest.installation,
                    &manifest.payload,
                    None,
                )?
            } else {
                super::reforger::validate_manifest(&manifest)?;
                let config = read_bounded_controlled(
                    &directory.join("configuration/server.json"),
                    1024 * 1024,
                    control,
                )?;
                super::reforger::validate_configuration(&config, &manifest)?;
                Vec::new()
            };
        let key_entries: Vec<_> = manifest
            .realized_entries
            .iter()
            .filter(|entry| {
                entry.kind == super::super::StageEntryKind::File
                    && entry.path.starts_with("engine/foxy-keys/")
            })
            .collect();
        if key_entries.len() != private_keys.len()
            || private_keys.iter().any(|key| {
                !key_entries.iter().any(|entry| {
                    entry.path == format!("engine/foxy-keys/{}", key.name)
                        && entry.bytes == key.material.len() as u64
                        && entry.blake3.as_ref() == Some(&key.blake3)
                })
            })
        {
            return Err(ServerStoreError::Corrupt);
        }
        if runtime_payload::inventory(
            manifest.installation.game,
            &directory.join("engine"),
            manifest.installation.platform,
            control,
        )? != manifest.payload
        {
            return Err(ServerStoreError::Corrupt);
        }
        validate_generic_entries(
            &manifest.realized_entries,
            entry_bytes(&manifest.realized_entries)?,
        )?;
        for prefix in [
            "configuration",
            "engine/foxy-content",
            "engine/foxy-keys",
            if manifest.installation.platform == crate::core::server::ServerPlatform::WindowsX64 {
                "engine/MPMissions"
            } else {
                "engine/mpmissions"
            },
        ] {
            verify_content_root(directory, prefix, &manifest.realized_entries, control)?;
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::content_staging::StageEntryKind;

    #[test]
    fn native_content_rejects_unreviewed_additions_without_removing_them() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("configuration")).unwrap();
        std::fs::write(root.path().join("configuration/server.cfg"), b"cfg").unwrap();
        let entries = vec![
            StageEntry {
                path: "configuration".into(),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            },
            StageEntry {
                path: "configuration/server.cfg".into(),
                kind: StageEntryKind::File,
                bytes: 3,
                blake3: Some(blake3::hash(b"cfg").to_hex().to_string()),
            },
        ];
        let control = ContentStageControl::default();
        verify_content_root(root.path(), "configuration", &entries, &control).unwrap();
        let unexpected = root.path().join("configuration/unreviewed.cfg");
        std::fs::write(&unexpected, b"retained").unwrap();
        assert_eq!(
            verify_content_root(root.path(), "configuration", &entries, &control),
            Err(ServerStoreError::Corrupt)
        );
        assert_eq!(std::fs::read(unexpected).unwrap(), b"retained");
    }
}
