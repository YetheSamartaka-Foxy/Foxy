use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::super::{
    AddonSupply, ContentBindingsDocument, ServerContext, ServerInstanceId, ServerStore,
    ServerStoreError,
};
use super::{
    ContentStageManifest, MAX_MANIFEST_BYTES, SCHEMA_VERSION, StageEntryKind, StageSource, tree,
};

pub(super) fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(super) fn valid_stage_id(value: &str) -> bool {
    value.strip_prefix("stage_").is_some_and(valid_hash)
}

pub(super) fn fingerprint(manifest: &ContentStageManifest) -> Result<String, ServerStoreError> {
    let bytes = serde_json::to_vec(manifest).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ServerStoreError::Invalid(
            "Content stage manifest exceeds 32 MiB".into(),
        ));
    }
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn expected_sources(
    document: &ContentBindingsDocument,
) -> Result<Vec<StageSource>, ServerStoreError> {
    expected_sources_with_acquisitions(document, &[])
}

pub(super) fn expected_sources_with_acquisitions(
    document: &ContentBindingsDocument,
    acquisitions: &[super::StageRepositorySnapshot],
) -> Result<Vec<StageSource>, ServerStoreError> {
    if acquisitions.len() > 256 {
        return Err(ServerStoreError::Corrupt);
    }
    let mut acquired_profiles = HashSet::new();
    for snapshot in acquisitions {
        if !acquired_profiles.insert(snapshot.profile_name.as_str())
            || !repository_checksum(&snapshot.publication_checksum)
            || snapshot.addons.is_empty()
            || snapshot.addons.len() > 2048
            || snapshot
                .addons
                .windows(2)
                .any(|pair| pair[0].name >= pair[1].name)
            || snapshot.addons.iter().any(|addon| {
                !tree::safe_component(&addon.name) || !repository_checksum(&addon.checksum)
            })
        {
            return Err(ServerStoreError::Corrupt);
        }
        let binding = document
            .bindings
            .iter()
            .find(|item| item.profile.name == snapshot.profile_name)
            .ok_or(ServerStoreError::Corrupt)?;
        if binding.profile.supply != AddonSupply::Download
            || binding.profile.remote_url != snapshot.repository_url
            || binding.profile.local_path != snapshot.repository_local_path
        {
            return Err(ServerStoreError::Corrupt);
        }
        if !binding.profile.addons.is_empty() {
            let mut selected = binding.profile.addons.clone();
            selected.sort();
            if selected
                != snapshot
                    .addons
                    .iter()
                    .map(|addon| addon.name.clone())
                    .collect::<Vec<_>>()
            {
                return Err(ServerStoreError::Corrupt);
            }
        }
    }
    let mut sources = Vec::new();
    let mut names = HashSet::new();
    let mut path_bytes = 0usize;
    for item in &document.bindings {
        let paths = if item.profile.supply == AddonSupply::Download {
            let snapshot=acquisitions.iter().find(|snapshot|snapshot.profile_name==item.profile.name).ok_or_else(||ServerStoreError::Invalid("Content acquisition is required. Only already-present profiles can be staged locally.".into()))?;
            let names = if item.profile.addons.is_empty() {
                snapshot
                    .addons
                    .iter()
                    .map(|addon| addon.name.as_str())
                    .collect::<Vec<_>>()
            } else {
                item.profile.addons.iter().map(String::as_str).collect()
            };
            names
                .into_iter()
                .map(|name| item.profile.local_path.join(name))
                .collect()
        } else if item.profile.addons.is_empty() {
            vec![item.folder.path.clone()]
        } else {
            let expanded = item
                .folder
                .path
                .as_os_str()
                .len()
                .saturating_mul(item.profile.addons.len())
                .saturating_add(item.profile.addons.iter().map(String::len).sum::<usize>());
            if expanded > 4 * 1024 * 1024 {
                return Err(ServerStoreError::Invalid(
                    "Content plan exceeds its path size limit".into(),
                ));
            }
            item.profile
                .addons
                .iter()
                .map(|name| item.folder.path.join(name))
                .collect()
        };
        for source in paths {
            path_bytes = path_bytes.saturating_add(source.as_os_str().len());
            if sources.len() >= 2048 || path_bytes > 4 * 1024 * 1024 {
                return Err(ServerStoreError::Invalid(
                    "Content stage source limit exceeded".into(),
                ));
            }
            let basename = source
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| tree::safe_component(name))
                .ok_or(ServerStoreError::UnsafePath)?;
            if !names.insert(basename.to_uppercase()) {
                return Err(ServerStoreError::Invalid(
                    "Selected addons have colliding stage directory names.".into(),
                ));
            }
            sources.push(StageSource {
                profile_name: item.profile.name.clone(),
                destination: format!("addons/{basename}"),
                source,
            });
        }
    }
    Ok(sources)
}

fn repository_checksum(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(in crate::core::server) fn physical_directory(
    path: &Path,
) -> Result<PathBuf, ServerStoreError> {
    super::super::store::check_directory_chain(path)?;
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(metadata) if metadata.is_dir() => {
                let mut resolved = cursor.canonicalize().map_err(|_| ServerStoreError::Io)?;
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Ok(_) => return Err(ServerStoreError::UnsafePath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    cursor
                        .file_name()
                        .ok_or(ServerStoreError::UnsafePath)?
                        .to_os_string(),
                );
                cursor = cursor.parent().ok_or(ServerStoreError::UnsafePath)?;
            }
            Err(_) => return Err(ServerStoreError::Io),
        }
    }
}

impl ServerStore {
    pub(super) fn validate_stage_context(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().content_staging {
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

    pub(in crate::core::server) fn stage_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("content-stages")
    }

    pub(super) fn current_stage_bindings(
        &self,
        id: &ServerInstanceId,
        expected: &str,
    ) -> Result<ContentBindingsDocument, ServerStoreError> {
        self.validate_stage_context()?;
        let snapshot = self.content_bindings(id)?;
        if snapshot.fingerprint.as_deref() != Some(expected) || !snapshot.review_current {
            return Err(ServerStoreError::StaleRevision);
        }
        snapshot.document.ok_or_else(|| {
            ServerStoreError::Invalid(
                "Save reviewed content bindings before verifying local content.".into(),
            )
        })
    }

    pub(super) fn check_stage_inputs(
        &self,
        manifest: &ContentStageManifest,
    ) -> Result<(), ServerStoreError> {
        let current = self.current_stage_bindings(
            &manifest.bindings.instance_id,
            &manifest.bindings_fingerprint,
        )?;
        if current != manifest.bindings
            || super::super::missions::cycle_library_sources(
                &self.show(&manifest.bindings.instance_id)?.definition,
            ) != manifest.missions
        {
            return Err(ServerStoreError::StaleRevision);
        }
        if let Some(lock) = &manifest.reforger_addons {
            self.check_reforger_lock_intent(&manifest.bindings.instance_id, lock)?;
        }
        Ok(())
    }

    pub(super) fn check_stage_separation(
        &self,
        id: &ServerInstanceId,
        sources: &[StageSource],
    ) -> Result<(), ServerStoreError> {
        let root = physical_directory(&self.stage_root(id))?;
        let catalog = self.load()?;
        let installations = catalog
            .installations
            .iter()
            .map(|item| physical_directory(&item.directory))
            .collect::<Result<Vec<_>, _>>()?;
        if installations
            .iter()
            .any(|installation| super::super::installation::paths_overlap(installation, &root))
        {
            return Err(ServerStoreError::UnsafePath);
        }
        for source in sources {
            super::super::store::check_directory_chain(&source.source)?;
            if !source.source.is_dir() {
                return Err(ServerStoreError::Invalid(
                    "An already-present content directory is missing or unsafe.".into(),
                ));
            }
            let location = physical_directory(&source.source)?;
            if super::super::installation::paths_overlap(&location, &root)
                || installations.iter().any(|installation| {
                    super::super::installation::paths_overlap(&location, installation)
                })
            {
                return Err(ServerStoreError::UnsafePath);
            }
        }
        Ok(())
    }

    pub(super) fn validate_stage_manifest(
        &self,
        id: &ServerInstanceId,
        manifest: &ContentStageManifest,
    ) -> Result<(), ServerStoreError> {
        if manifest.schema_version != SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        self.validate_content_bindings(id, &manifest.bindings)?;
        super::super::reforger_addons::validate_stage_lock(manifest)?;
        let mut expected =
            expected_sources_with_acquisitions(&manifest.bindings, &manifest.acquisitions)
                .map_err(|_| ServerStoreError::Corrupt)?;
        self.append_reforger_stage_sources(id, manifest.reforger_addons.as_ref(), &mut expected)?;
        self.append_mission_stage_sources(&mut expected, &manifest.missions)?;
        if !valid_hash(&manifest.bindings_fingerprint) || expected != manifest.sources {
            return Err(ServerStoreError::Corrupt);
        }
        tree::validate_entries(manifest)?;
        let source_roots: HashSet<_> = manifest
            .sources
            .iter()
            .map(|source| source.destination.as_str())
            .collect();
        let directories: HashSet<_> = manifest
            .entries
            .iter()
            .filter(|entry| entry.kind == StageEntryKind::Directory)
            .map(|entry| entry.path.as_str())
            .collect();
        let mut total = 0u64;
        for entry in &manifest.entries {
            if entry.path != "addons" {
                let end = entry
                    .path
                    .match_indices('/')
                    .nth(1)
                    .map_or(entry.path.len(), |(index, _)| index);
                if !source_roots.contains(&entry.path[..end]) {
                    return Err(ServerStoreError::Corrupt);
                }
            }
            if entry.kind == StageEntryKind::File {
                total = total
                    .checked_add(entry.bytes)
                    .ok_or(ServerStoreError::Corrupt)?;
            }
        }
        if total != manifest.total_bytes {
            return Err(ServerStoreError::Corrupt);
        }
        for source in &manifest.sources {
            if !directories.contains(source.destination.as_str()) {
                return Err(ServerStoreError::Corrupt);
            }
        }
        fingerprint(manifest)?;
        Ok(())
    }
}
