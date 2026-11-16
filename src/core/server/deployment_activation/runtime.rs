use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{NativeRuntimeManifest, NativeRuntimeReference};
use crate::core::server::content_staging::{
    StageEntry, StageEntryKind, copy_verified_file, parse_relative, publish_no_replace,
    validate_generic_entries,
};
use crate::core::server::{
    ContentStageControl, DeploymentPreparationReport, ServerDefinition, ServerInstallation,
    ServerInstanceId, ServerPlatform, ServerStore, ServerStoreError, runtime_payload,
};

pub(super) const MAX_MANIFEST_BYTES: usize = 48 * 1024 * 1024;

pub(super) fn entry_bytes(entries: &[StageEntry]) -> Result<u64, ServerStoreError> {
    entries.iter().try_fold(0u64, |total, entry| {
        total
            .checked_add(entry.bytes)
            .ok_or(ServerStoreError::Corrupt)
    })
}

pub(super) fn check_native_path(
    path: &Path,
    platform: ServerPlatform,
) -> Result<(), ServerStoreError> {
    let value = super::launch::argument_path(path)?;
    if platform == ServerPlatform::WindowsX64 && value.encode_utf16().count() >= 260 {
        return Err(ServerStoreError::Invalid("The native runtime path exceeds the Windows engine limit. Choose a shorter Foxy configuration directory before activation.".into()));
    }
    Ok(())
}

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ServerStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(ServerStoreError::Invalid(
            "Native runtime manifest exceeds 48 MiB".into(),
        ));
    }
    Ok(bytes)
}

pub(super) fn fingerprint<T: serde::Serialize>(value: &T) -> Result<String, ServerStoreError> {
    Ok(blake3::hash(&encode(value)?).to_hex().to_string())
}

pub(super) fn hash_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

impl ServerStore {
    pub(super) fn validate_native_runtime_paths(
        &self,
        manifest: &NativeRuntimeManifest,
        directory: &Path,
    ) -> Result<(), ServerStoreError> {
        for entry in &manifest.payload.entries {
            check_native_path(
                &directory.join("engine").join(parse_relative(&entry.path)?),
                manifest.installation.platform,
            )?;
        }
        for entry in &manifest.realized_entries {
            check_native_path(
                &directory.join(parse_relative(&entry.path)?),
                manifest.installation.platform,
            )?;
            if manifest.installation.game == super::super::ServerDefinitionKind::Arma3
                && let Some(name) = entry
                    .path
                    .strip_prefix("configuration/")
                    .and_then(|path| path.strip_suffix(".Arma3Profile"))
            {
                if !crate::core::server::profile_name_valid(name) {
                    return Err(ServerStoreError::UnsafePath);
                }
                check_native_path(
                    &super::profile::profile_destination(
                        &self
                            .record_root(&manifest.instance_id)
                            .join("runtime-state/home/profiles"),
                        manifest.installation.platform,
                        name,
                    ),
                    manifest.installation.platform,
                )?;
            }
        }
        check_native_path(
            &self
                .record_root(&manifest.instance_id)
                .join(match manifest.installation.game {
                    super::super::ServerDefinitionKind::Arma3 => {
                        "runtime-state/home/profiles/FoxyServer.Arma3Profile"
                    }
                    super::super::ServerDefinitionKind::Reforger => {
                        "runtime-state/home/profiles/profile"
                    }
                }),
            manifest.installation.platform,
        )
    }
    pub(in crate::core::server) fn native_runtime_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.record_root(id).join("runtimes")
    }

    pub(in crate::core::server) fn runtime_directory(
        &self,
        id: &ServerInstanceId,
        runtime: &NativeRuntimeReference,
    ) -> Result<PathBuf, ServerStoreError> {
        if !hash_id(&runtime.runtime_id, "runtime_")
            || runtime.runtime_id != format!("runtime_{}", runtime.manifest_fingerprint)
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(self.native_runtime_root(id).join(runtime.directory_name()))
    }

    pub(super) fn native_runtime_manifest(
        &self,
        preparation: &DeploymentPreparationReport,
        installation: ServerInstallation,
        payload: runtime_payload::PayloadInventory,
        control: &ContentStageControl,
    ) -> Result<NativeRuntimeManifest, ServerStoreError> {
        let id = &preparation.manifest.record.instance_id;
        let config = preparation.directory.join("configuration");
        let content = preparation.directory.join("content");
        let (preview, stage) = self.inspect_record_archives_at(
            id,
            &preparation.manifest.record,
            &config,
            &content,
            control,
        )?;
        let (configuration_instance, _, _) = self.read_verified_configuration_at(
            id,
            &preparation.manifest.record.configuration_revision,
            &config,
            control,
        )?;
        if configuration_instance.definition.kind() != installation.game {
            return Err(ServerStoreError::Corrupt);
        }
        if let ServerDefinition::Reforger(settings) = &configuration_instance.definition {
            return self.reforger_runtime_manifest(
                preparation,
                installation,
                payload,
                settings,
                preview,
                &stage,
            );
        }
        let ServerDefinition::Arma3(arma) = &configuration_instance.definition else {
            return Err(ServerStoreError::UnsupportedGame);
        };
        let layout = super::super::engine_layout::observe_tested_layout(&installation)
            .ok_or(ServerStoreError::UnsupportedGame)?;
        let version = match layout.tested_runtime_version {
            "2.22.154089" => 222,
            _ => return Err(ServerStoreError::UnsupportedGame),
        };
        arma.validate_engine_version(version)
            .map_err(ServerStoreError::Invalid)?;
        self.validate_configuration_bindings(&configuration_instance)?;
        if !arma.mission_cycle.iter().any(|mission| mission.enabled) {
            return Err(ServerStoreError::Invalid(
                "Native activation requires a complete reviewed mission cycle.".into(),
            ));
        }
        let private_keys = super::super::keys::runtime::private_keys(
            arma.key_trust.as_deref(),
            id,
            &installation,
            &payload,
            Some(&stage),
        )?;
        let roles =
            super::super::keys::roles::route(&stage.bindings, &stage.sources, &stage.missions)?;
        let headless_clients = super::super::ordered_headless_clients(&arma.headless_clients)
            .into_iter()
            .filter(|client| client.enabled)
            .map(|client| {
                Ok(super::super::NativeHeadlessClient {
                    id: client.id.clone(),
                    name: client.name.clone(),
                    mods: super::super::keys::roles::headless_mods(
                        &stage.bindings,
                        &stage.sources,
                        &stage.missions,
                        client,
                    )?,
                    process: client.process.clone(),
                    lifecycle: client.lifecycle.clone(),
                })
            })
            .collect::<Result<Vec<_>, ServerStoreError>>()?;
        let evidence = super::super::deployment_composition::inspect_artifacts(
            &configuration_instance.definition,
            &stage.entries,
        );
        if evidence.truncated
            || evidence.missions.iter().any(|mission| {
                mission.files.is_empty() || mission.conflicting_bytes || mission.missing_required
            })
        {
            return Err(ServerStoreError::Invalid("Mission evidence is incomplete or conflicting. Review the selected content before activation.".into()));
        }
        let mut files = BTreeMap::<String, (String, StageEntry)>::new();
        let mut add = |source: String,
                       destination: String,
                       entry: &StageEntry|
         -> Result<(), ServerStoreError> {
            // Trusted keys carry their reviewed bytes in the policy and have no staged source.
            if !source.is_empty() {
                parse_relative(&source)?;
            }
            parse_relative(&destination)?;
            if files
                .insert(
                    destination.clone(),
                    (
                        source,
                        StageEntry {
                            path: destination,
                            ..entry.clone()
                        },
                    ),
                )
                .is_some()
            {
                return Err(ServerStoreError::Invalid(
                    "Native runtime destinations collide.".into(),
                ));
            }
            Ok(())
        };
        for file in preview.files {
            let source = format!("configuration/{}", file.relative_path);
            let entry = preparation
                .manifest
                .entries
                .iter()
                .find(|entry| entry.path == source)
                .ok_or(ServerStoreError::Corrupt)?;
            add(
                source,
                format!("configuration/{}", file.relative_path),
                entry,
            )?;
        }
        let mission_directory = if installation.platform == ServerPlatform::WindowsX64 {
            "MPMissions"
        } else {
            "mpmissions"
        };
        let mut mission_templates = Vec::new();
        for mission in &evidence.missions {
            let file = mission.files.first().ok_or(ServerStoreError::Corrupt)?;
            let source = format!("content/{}", file.path);
            let entry = preparation
                .manifest
                .entries
                .iter()
                .find(|entry| entry.path == source)
                .ok_or(ServerStoreError::Corrupt)?;
            add(
                source,
                format!("engine/{mission_directory}/{}.pbo", mission.template),
                entry,
            )?;
            mission_templates.push(mission.template.clone());
        }
        let mission_ids: BTreeSet<_> = stage
            .missions
            .iter()
            .filter_map(|source| match source {
                super::super::Arma3MissionSource::Library { mission_id, .. } => {
                    Some(mission_id.as_str())
                }
                super::super::Arma3MissionSource::AddonClass => None,
            })
            .collect();
        let loaded: Vec<_> = stage
            .sources
            .iter()
            .filter(|source| {
                roles.loaded_destinations.contains(&source.destination)
                    || mission_ids.contains(source.profile_name.as_str())
            })
            .collect();
        for entry in &stage.entries {
            if entry.kind != StageEntryKind::File {
                continue;
            }
            if !loaded
                .iter()
                .any(|source| entry.path.starts_with(&format!("{}/", source.destination)))
            {
                continue;
            }
            let suffix = entry
                .path
                .strip_prefix("addons/")
                .ok_or(ServerStoreError::Corrupt)?;
            add(
                format!("content/{}", entry.path),
                format!("engine/foxy-content/{suffix}"),
                entry,
            )?;
        }
        for key in &private_keys {
            let destination = format!("engine/foxy-keys/{}", key.name);
            add(
                String::new(),
                destination.clone(),
                &StageEntry {
                    path: destination,
                    kind: StageEntryKind::File,
                    bytes: key.material.len() as u64,
                    blake3: Some(key.blake3.clone()),
                },
            )?;
        }
        let mut directories = BTreeSet::new();
        for path in files.keys() {
            let mut parent = Path::new(path).parent();
            while let Some(value) = parent.filter(|value| !value.as_os_str().is_empty()) {
                directories.insert(value.to_string_lossy().replace('\\', "/"));
                parent = value.parent();
            }
        }
        let mut realized_entries: Vec<_> = directories
            .into_iter()
            .map(|path| StageEntry {
                path,
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            })
            .collect();
        let file_sources = files
            .iter()
            .filter(|(_, (source, _))| !source.is_empty())
            .map(|(destination, (source, _))| (destination.clone(), source.clone()))
            .collect();
        realized_entries.extend(files.into_values().map(|(_, entry)| entry));
        realized_entries.sort_by(|a, b| a.path.cmp(&b.path));
        validate_generic_entries(&realized_entries, entry_bytes(&realized_entries)?)?;
        Ok(NativeRuntimeManifest {
            schema_version: 1,
            game_space_id: self.context().space_id().into(),
            instance_id: id.clone(),
            preparation_id: preparation.preparation_id.clone(),
            installation,
            payload,
            realized_entries,
            file_sources,
            mission_templates,
            server_mods: roles.server_mods,
            required_mods: roles.required_mods,
            optional_sources: roles.optional_sources,
            key_trust: arma.key_trust.clone(),
            credential_bindings: [
                ("password", &arma.server.password_ref),
                ("passwordAdmin", &arma.server.password_admin_ref),
                (
                    "serverCommandPassword",
                    &arma.server.server_command_password_ref,
                ),
            ]
            .into_iter()
            .filter_map(|(name, reference)| {
                reference
                    .as_ref()
                    .map(|reference| (name.to_string(), reference.clone()))
            })
            .collect(),
            reforger_addons: None,
            headless_clients,
        })
    }

    pub(super) fn materialize_native_runtime(
        &self,
        manifest: &NativeRuntimeManifest,
        preparation: &Path,
        control: &ContentStageControl,
    ) -> Result<NativeRuntimeReference, ServerStoreError> {
        let hash = fingerprint(manifest)?;
        let reference = NativeRuntimeReference {
            runtime_id: format!("runtime_{hash}"),
            manifest_fingerprint: hash,
        };
        let directory = self.runtime_directory(&manifest.instance_id, &reference)?;
        self.validate_native_runtime_paths(manifest, &directory)?;
        self.check_deployment_destination(&directory, None)?;
        if directory.try_exists().map_err(|_| ServerStoreError::Io)? {
            self.verify_native_runtime(&manifest.instance_id, &reference, control)?;
            return Ok(reference);
        }
        let root = self.native_runtime_root(&manifest.instance_id);
        super::super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        let manifest_bytes = encode(manifest)?.len() as u64;
        let required_bytes = manifest
            .payload
            .total_bytes
            .checked_add(entry_bytes(&manifest.realized_entries)?)
            .and_then(|bytes| bytes.checked_add(manifest_bytes))
            .ok_or(ServerStoreError::Corrupt)?;
        if fs4::available_space(&root).map_err(|_| ServerStoreError::Io)? < required_bytes {
            return Err(ServerStoreError::Invalid(
                "Insufficient free space for the isolated native runtime copy.".into(),
            ));
        }
        let temporary = tempfile::Builder::new()
            .prefix(".native-runtime-")
            .tempdir_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        control.start_phase(
            super::super::ContentStagePhase::Copying,
            manifest
                .payload
                .entries
                .iter()
                .filter(|entry| entry.kind == StageEntryKind::File)
                .count() as u64
                + manifest
                    .realized_entries
                    .iter()
                    .filter(|entry| entry.kind == StageEntryKind::File)
                    .count() as u64,
            manifest
                .payload
                .total_bytes
                .checked_add(entry_bytes(&manifest.realized_entries)?)
                .ok_or(ServerStoreError::Corrupt)?,
        );
        runtime_payload::copy(
            manifest.installation.game,
            &manifest.installation.directory,
            &temporary.path().join("engine"),
            &manifest.payload,
            control,
        )?;
        for entry in &manifest.realized_entries {
            control.checkpoint()?;
            let destination = temporary.path().join(parse_relative(&entry.path)?);
            match entry.kind {
                StageEntryKind::Directory if entry.path == "engine" => {}
                StageEntryKind::Directory => {
                    fs::create_dir(&destination).map_err(|_| ServerStoreError::Io)?
                }
                StageEntryKind::File => {
                    if let Some(source) = manifest.file_sources.get(&entry.path) {
                        if entry.path == "configuration/server.json"
                            && manifest.reforger_addons.is_some()
                        {
                            let original =
                                crate::core::server::content_staging::read_bounded_controlled(
                                    &preparation.join(parse_relative(source)?),
                                    1024 * 1024,
                                    control,
                                )?;
                            let realized = super::reforger::locked_server_json(
                                &original,
                                manifest
                                    .reforger_addons
                                    .as_ref()
                                    .ok_or(ServerStoreError::Corrupt)?,
                            )?;
                            if entry.bytes != realized.len() as u64
                                || entry.blake3.as_deref()
                                    != Some(blake3::hash(&realized).to_hex().as_str())
                            {
                                return Err(ServerStoreError::StaleRevision);
                            }
                            let mut file = fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&destination)
                                .map_err(|_| ServerStoreError::Io)?;
                            file.write_all(&realized)
                                .and_then(|()| file.sync_all())
                                .map_err(|_| ServerStoreError::Io)?;
                            control.record_bytes(realized.len() as u64);
                            control.record_file();
                        } else {
                            copy_verified_file(
                                &preparation.join(parse_relative(source)?),
                                &destination,
                                entry,
                                control,
                            )?;
                        }
                    } else {
                        let name = entry
                            .path
                            .strip_prefix("engine/foxy-keys/")
                            .ok_or(ServerStoreError::Corrupt)?;
                        let key = manifest
                            .key_trust
                            .as_ref()
                            .and_then(|trust| trust.keys.iter().find(|key| key.name == name))
                            .ok_or(ServerStoreError::Corrupt)?;
                        if entry.bytes != key.material.len() as u64
                            || entry.blake3.as_ref() != Some(&key.blake3)
                        {
                            return Err(ServerStoreError::Corrupt);
                        }
                        let mut file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&destination)
                            .map_err(|_| ServerStoreError::Io)?;
                        file.write_all(&key.material)
                            .and_then(|()| file.sync_all())
                            .map_err(|_| ServerStoreError::Io)?;
                        control.record_bytes(key.material.len() as u64);
                        control.record_file();
                    }
                }
            }
        }
        let bytes = encode(manifest)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary.path().join("manifest.json"))
            .map_err(|_| ServerStoreError::Io)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        drop(file);
        control.start_phase(super::super::ContentStagePhase::Checking, 0, 0);
        self.verify_runtime_at(&manifest.instance_id, &reference, temporary.path(), control)?;
        if runtime_payload::inventory(
            manifest.installation.game,
            &manifest.installation.directory,
            manifest.installation.platform,
            control,
        )? != manifest.payload
        {
            return Err(ServerStoreError::StaleRevision);
        }
        control.checkpoint()?;
        if publish_no_replace(temporary.path(), &directory)? {
            let _ = temporary.keep();
        }
        self.verify_native_runtime(&manifest.instance_id, &reference, control)?;
        Ok(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_runtime_byte_totals_return_an_error_without_overflow() {
        let entry = |bytes| StageEntry {
            path: "engine/file.pbo".into(),
            kind: StageEntryKind::File,
            bytes,
            blake3: Some("0".repeat(64)),
        };
        assert_eq!(entry_bytes(&[entry(17), entry(25)]).unwrap(), 42);
        assert_eq!(
            entry_bytes(&[entry(u64::MAX), entry(1)]),
            Err(ServerStoreError::Corrupt)
        );
    }

    #[test]
    fn windows_native_paths_are_bounded_before_copying() {
        assert!(check_native_path(Path::new(&"a".repeat(259)), ServerPlatform::WindowsX64).is_ok());
        assert!(
            check_native_path(Path::new(&"a".repeat(260)), ServerPlatform::WindowsX64).is_err()
        );
        assert!(check_native_path(Path::new(&"a".repeat(300)), ServerPlatform::LinuxX64).is_ok());
    }
}
