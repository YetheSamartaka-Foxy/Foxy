use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

use super::{
    ReforgerAddonChannel, ReforgerAddonLock, ReforgerAddonProvider, metadata, resolve_addons,
};
use crate::core::server::content_staging::{
    ContentStageManifest, StageEntry, StageEntryKind, StageSource, copy_sources, inspect_sources,
    parse_relative, publish_no_replace, read_bounded_controlled, verify_stage_tree,
};
use crate::core::server::{
    ContentStageControl, ServerDefinition, ServerInstanceId, ServerStore, ServerStoreError,
};

const BASE_GUID: &str = "58D0FB3206B6F859";
const MAX_METADATA: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
    schema_version: u32,
    game_space_id: String,
    instance_id: ServerInstanceId,
    lock: ReforgerAddonLock,
    entries: Vec<StageEntry>,
    total_bytes: u64,
}

fn error(message: String) -> ServerStoreError {
    ServerStoreError::Invalid(message)
}

fn validate_lock(lock: &ReforgerAddonLock) -> Result<(), ServerStoreError> {
    let mut expected = resolve_addons(
        &lock.requested,
        lock.mods_required_by_default,
        &lock.scenario_id,
        lock.scenario_provider.as_deref(),
        lock.channel,
        &lock.realized,
    )
    .map_err(error)?;
    expected.content_sources = lock.content_sources.clone();
    if expected != *lock
        || lock.content_sources.len() != lock.realized.len()
        || lock
            .realized
            .iter()
            .any(|item| !lock.content_sources.contains_key(&item.addon_id))
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(())
}

pub(in crate::core::server) fn validate_stage_lock(
    manifest: &ContentStageManifest,
) -> Result<(), ServerStoreError> {
    let Some(lock) = &manifest.reforger_addons else {
        return Ok(());
    };
    if manifest.bindings.game_id != "reforger" {
        return Err(ServerStoreError::Corrupt);
    }
    validate_lock(lock)?;
    for item in &lock.realized {
        let destination = format!("addons/{}", item.addon_id);
        if !manifest.sources.iter().any(|source| {
            source.destination == destination
                && source.source == lock.content_sources[&item.addon_id]
                && source.profile_name == format!("reforger-{}", item.addon_id)
        }) || !manifest.entries.iter().any(|entry| {
            entry.path == format!("{destination}/addon.gproj") && entry.kind == StageEntryKind::File
        }) || !manifest.entries.iter().any(|entry| {
            entry.path == format!("{destination}/ServerData.json")
                && entry.kind == StageEntryKind::File
        }) {
            return Err(ServerStoreError::Corrupt);
        }
    }
    if manifest.sources.len() != lock.realized.len() {
        return Err(ServerStoreError::Invalid(
            "A Reforger addon stage must contain exactly its reviewed locked addon set.".into(),
        ));
    }
    Ok(())
}

fn read_metadata(
    root: &Path,
    channel: ReforgerAddonChannel,
    provider: ReforgerAddonProvider,
    scenario: &str,
    control: &ContentStageControl,
) -> Result<super::ReforgerAddonMetadata, ServerStoreError> {
    let data = read_bounded_controlled(&root.join("ServerData.json"), MAX_METADATA, control)?;
    let project = read_bounded_controlled(&root.join("addon.gproj"), MAX_METADATA, control)?;
    let project = std::str::from_utf8(&project).map_err(|_| ServerStoreError::Corrupt)?;
    let mut metadata =
        metadata::parse_metadata(&data, project, channel, provider, &[BASE_GUID.into()])
            .map_err(error)?;
    let database = read_bounded_controlled(
        &root.join("resourceDatabase.rdb"),
        super::resources::MAX_RESOURCE_DATABASE_BYTES,
        control,
    )?;
    metadata.scenarios.clear();
    if super::resources::contains_resource(&database, scenario).map_err(error)? {
        metadata.scenarios.push(scenario.into());
    }
    Ok(metadata)
}

fn verify_package(
    root: &Path,
    item: &super::ReforgerAddonMetadata,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let bytes = read_bounded_controlled(&root.join("meta"), MAX_METADATA, control)?;
    let files = metadata::package_files(&bytes, &item.addon_id, &item.version).map_err(error)?;
    if !files.iter().any(|(name, _)| name == "addon.gproj")
        || !files.iter().any(|(name, _)| name == "resourceDatabase.rdb")
        || !files.iter().any(|(name, _)| name.ends_with(".pak"))
    {
        return Err(error(
            "Addon package is missing its project, resource database or payload.".into(),
        ));
    }
    for (name, size) in files {
        control.checkpoint()?;
        let path = root.join(parse_relative(&name)?);
        crate::core::server::store::check_regular_file(&path)?;
        let manifest_path = root.join(parse_relative(&format!(
            "{name}_{}_manifest.json",
            item.version
        ))?);
        let manifest: serde_json::Value = serde_json::from_slice(&read_bounded_controlled(
            &manifest_path,
            MAX_METADATA,
            control,
        )?)
        .map_err(|_| ServerStoreError::Corrupt)?;
        let expected = manifest
            .get("sha512")
            .and_then(serde_json::Value::as_str)
            .filter(|hash| hash.len() == 128 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or(ServerStoreError::Corrupt)?;
        if manifest.get("size").and_then(serde_json::Value::as_u64) != Some(size)
            || fs::metadata(&path).map_err(|_| ServerStoreError::Io)?.len() != size
        {
            return Err(ServerStoreError::Corrupt);
        }
        let mut file = fs::File::open(&path).map_err(|_| ServerStoreError::Io)?;
        let mut digest = Sha512::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            control.checkpoint()?;
            let read = file.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        if !hex::encode(digest.finalize()).eq_ignore_ascii_case(expected) {
            return Err(error(
                "Addon payload does not match its vendor file hash.".into(),
            ));
        }
    }
    Ok(())
}

impl ServerStore {
    fn reforger_addon_root(&self, id: &ServerInstanceId) -> Result<PathBuf, ServerStoreError> {
        let root = self
            .context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("reforger-addons");
        crate::core::server::content_staging::physical_directory(&root)
    }

    pub fn selected_reforger_addon_lock(
        &self,
        id: &ServerInstanceId,
    ) -> Result<Option<ReforgerAddonLock>, ServerStoreError> {
        if !matches!(self.show(id)?.definition, ServerDefinition::Reforger(_)) {
            return Ok(None);
        }
        let root = self.reforger_addon_root(id)?;
        let path = root.join("selected.json");
        crate::core::server::store::check_directory_chain(&root)?;
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok(None);
        }
        let bytes =
            read_bounded_controlled(&path, 4 * 1024 * 1024, &ContentStageControl::default())?;
        let lock: ReforgerAddonLock =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        validate_lock(&lock)?;
        for source in lock.content_sources.values() {
            if !source.starts_with(&root) {
                return Err(ServerStoreError::UnsafePath);
            }
            crate::core::server::store::check_directory_chain(source)?;
        }
        Ok(Some(lock))
    }

    pub(in crate::core::server) fn append_reforger_stage_sources(
        &self,
        id: &ServerInstanceId,
        lock: Option<&ReforgerAddonLock>,
        sources: &mut Vec<StageSource>,
    ) -> Result<(), ServerStoreError> {
        let Some(lock) = lock else {
            return Ok(());
        };
        validate_lock(lock)?;
        let root = self.reforger_addon_root(id)?;
        for item in &lock.realized {
            let source = lock
                .content_sources
                .get(&item.addon_id)
                .ok_or(ServerStoreError::Corrupt)?;
            if !source.starts_with(&root) {
                return Err(ServerStoreError::UnsafePath);
            }
            sources.push(StageSource {
                profile_name: format!("reforger-{}", item.addon_id),
                source: source.clone(),
                destination: format!("addons/{}", item.addon_id),
            });
        }
        Ok(())
    }

    pub(in crate::core::server) fn check_reforger_lock_intent(
        &self,
        id: &ServerInstanceId,
        lock: &ReforgerAddonLock,
    ) -> Result<(), ServerStoreError> {
        let instance = self.show(id)?;
        check_definition(&instance.definition, lock)
    }

    pub(in crate::core::server) fn inspect_reforger_stage_lock(
        &self,
        id: &ServerInstanceId,
        sources: &[StageSource],
        control: &ContentStageControl,
    ) -> Result<Option<ReforgerAddonLock>, ServerStoreError> {
        let instance = self.show(id)?;
        let ServerDefinition::Reforger(settings) = &instance.definition else {
            return Ok(None);
        };
        let Some(lock) = self.selected_reforger_addon_lock(id)? else {
            if !settings.addons.is_empty() {
                return Err(error(
                    "Acquire and review the Reforger addon set before staging it.".into(),
                ));
            }
            return Ok(None);
        };
        check_definition(&instance.definition, &lock)?;
        for item in &lock.realized {
            control.checkpoint()?;
            let path = &lock.content_sources[&item.addon_id];
            if read_metadata(
                path,
                lock.channel,
                item.provider,
                &lock.scenario_id,
                control,
            )? != *item
                || !sources.iter().any(|source| &source.source == path)
            {
                return Err(ServerStoreError::StaleRevision);
            }
            verify_package(path, item, control)?;
        }
        self.verify_reforger_revision(id, &lock, control)?;
        Ok(Some(lock))
    }

    fn verify_reforger_revision(
        &self,
        id: &ServerInstanceId,
        lock: &ReforgerAddonLock,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        let Some(source) = lock.content_sources.values().next() else {
            return Ok(());
        };
        let directory = source
            .parent()
            .and_then(Path::parent)
            .ok_or(ServerStoreError::UnsafePath)?;
        let bytes =
            read_bounded_controlled(&directory.join("manifest.json"), 32 * 1024 * 1024, control)?;
        let revision: Revision =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        let mut without_sources = lock.clone();
        without_sources.content_sources.clear();
        let hash = blake3::hash(
            &serde_json::to_vec(&(&without_sources, &revision.entries))
                .map_err(|_| ServerStoreError::Corrupt)?,
        )
        .to_hex()
        .to_string();
        if revision.schema_version != 1
            || revision.game_space_id != self.context().space_id()
            || &revision.instance_id != id
            || &revision.lock != lock
            || directory
                != self
                    .reforger_addon_root(id)?
                    .join(format!("revision_{hash}"))
            || lock
                .content_sources
                .iter()
                .any(|(guid, path)| path != &directory.join("addons").join(guid))
        {
            return Err(ServerStoreError::Corrupt);
        }
        crate::core::server::content_staging::verify_tree(
            &revision.entries,
            revision.total_bytes,
            directory,
            control,
        )
    }

    pub(in crate::core::server) fn publish_reforger_addons(
        &self,
        id: &ServerInstanceId,
        inputs: &[PathBuf],
        channel: ReforgerAddonChannel,
        scenario_provider: Option<&str>,
        base_resource_database: &[u8],
        control: &ContentStageControl,
    ) -> Result<ReforgerAddonLock, ServerStoreError> {
        let provider = ReforgerAddonProvider::BohemiaWorkshop;
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        self.validate_record_context()?;
        let instance = self.show(id)?;
        let ServerDefinition::Reforger(settings) = &instance.definition else {
            return Err(ServerStoreError::UnsupportedGame);
        };
        if inputs.len() > 256 {
            return Err(ServerStoreError::Corrupt);
        }
        let scenario = settings
            .scenario_id
            .as_deref()
            .ok_or_else(|| error("Select a scenario before acquiring addons.".into()))?;
        let mut metadata = Vec::new();
        let mut input_map = BTreeMap::new();
        for input in inputs {
            control.checkpoint()?;
            crate::core::server::store::check_directory_chain(input)?;
            let item = read_metadata(input, channel, provider, scenario, control)?;
            verify_package(input, &item, control)?;
            if input_map
                .insert(item.addon_id.clone(), input.clone())
                .is_some()
            {
                return Err(ServerStoreError::Corrupt);
            }
            metadata.push(item);
        }
        let base_owns_scenario =
            super::resources::contains_resource(base_resource_database, scenario).map_err(error)?;
        let owners: Vec<_> = metadata
            .iter()
            .filter(|item| item.scenarios.iter().any(|resource| resource == scenario))
            .map(|item| item.addon_id.clone())
            .collect();
        let resolved_provider = match (base_owns_scenario, owners.as_slice()) {
            (true, []) if scenario_provider.is_none() => None,
            (false, [owner]) if scenario_provider.is_none_or(|requested| requested.eq_ignore_ascii_case(owner)) => Some(owner.clone()),
            _ => return Err(error("Scenario ownership is missing, conflicting or differs from the reviewed provider. The previous addon selection was retained.".into())),
        };
        let mut lock = resolve_addons(
            &settings.addons,
            settings.mods_required_by_default,
            scenario,
            resolved_provider.as_deref(),
            channel,
            &metadata,
        )
        .map_err(error)?;
        if metadata.len() != lock.realized.len() {
            return Err(error("The engine acquired extra addons. Review their dependency ownership before retaining this set.".into()));
        }
        let sources: Vec<_> = lock
            .realized
            .iter()
            .map(|item| StageSource {
                profile_name: format!("reforger-{}", item.addon_id),
                source: input_map[&item.addon_id].clone(),
                destination: format!("addons/{}", item.addon_id),
            })
            .collect();
        let entries = inspect_sources(&sources, control)?;
        let total_bytes = entries.iter().try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.bytes)
                .ok_or(ServerStoreError::Corrupt)
        })?;
        let hash = blake3::hash(
            &serde_json::to_vec(&(&lock, &entries)).map_err(|_| ServerStoreError::Corrupt)?,
        )
        .to_hex()
        .to_string();
        let root = self.reforger_addon_root(id)?;
        self.check_deployment_destination(&root, None)?;
        crate::core::server::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        let destination = root.join(format!("revision_{hash}"));
        for item in &lock.realized {
            lock.content_sources.insert(
                item.addon_id.clone(),
                destination.join("addons").join(&item.addon_id),
            );
        }
        let bindings = self.content_bindings(id)?.document.ok_or_else(|| {
            error("Save reviewed content bindings before retaining an addon set.".into())
        })?;
        if !bindings.bindings.is_empty() {
            return Err(error(
                "Use empty repository bindings for an engine-acquired Reforger addon set.".into(),
            ));
        }
        let manifest = ContentStageManifest {
            schema_version: 1,
            bindings_fingerprint: String::new(),
            bindings,
            sources,
            acquisitions: Vec::new(),
            missions: Vec::new(),
            entries: entries.clone(),
            total_bytes,
            reforger_addons: Some(lock.clone()),
        };
        let revision = Revision {
            schema_version: 1,
            game_space_id: self.context().space_id().into(),
            instance_id: id.clone(),
            lock: lock.clone(),
            entries,
            total_bytes,
        };
        let temporary = tempfile::Builder::new()
            .prefix(".revision-")
            .tempdir_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        copy_sources(&manifest, temporary.path(), control)?;
        verify_stage_tree(&manifest, temporary.path(), control)?;
        for item in &lock.realized {
            let path = temporary.path().join("addons").join(&item.addon_id);
            if read_metadata(&path, channel, provider, scenario, control)? != *item {
                return Err(ServerStoreError::StaleRevision);
            }
            verify_package(&path, item, control)?;
        }
        let encoded = serde_json::to_vec(&revision).map_err(|_| ServerStoreError::Corrupt)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary.path().join("manifest.json"))
            .map_err(|_| ServerStoreError::Io)?;
        file.write_all(&encoded)
            .and_then(|()| file.sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        drop(file);
        control.checkpoint()?;
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            let result = (|| {
                self.validate_record_context()?;
                if self.show(id)? != instance {
                    return Err(ServerStoreError::StaleRevision);
                }
                if !publish_no_replace(temporary.path(), &destination)? {
                    let existing = read_bounded_controlled(
                        &destination.join("manifest.json"),
                        32 * 1024 * 1024,
                        control,
                    )?;
                    if existing != encoded {
                        return Err(ServerStoreError::Corrupt);
                    }
                    verify_stage_tree(&manifest, &destination, control)?;
                }
                let bytes =
                    serde_json::to_vec_pretty(&lock).map_err(|_| ServerStoreError::Corrupt)?;
                let path = root.join("selected.json");
                crate::core::server::store::check_directory_chain(&root)?;
                crate::core::server::store::check_regular_file(&path)?;
                let mut selected =
                    tempfile::NamedTempFile::new_in(&root).map_err(|_| ServerStoreError::Io)?;
                selected
                    .write_all(&bytes)
                    .and_then(|()| selected.as_file().sync_all())
                    .map_err(|_| ServerStoreError::Io)?;
                control.checkpoint()?;
                selected.persist(&path).map_err(|_| ServerStoreError::Io)?;
                Ok(lock.clone())
            })();
            Ok(result)
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }
}

pub(in crate::core::server) fn check_definition(
    definition: &ServerDefinition,
    lock: &ReforgerAddonLock,
) -> Result<(), ServerStoreError> {
    let ServerDefinition::Reforger(settings) = definition else {
        return Err(ServerStoreError::Corrupt);
    };
    let expected = resolve_addons(
        &settings.addons,
        settings.mods_required_by_default,
        settings
            .scenario_id
            .as_deref()
            .ok_or(ServerStoreError::Corrupt)?,
        lock.scenario_provider.as_deref(),
        lock.channel,
        &lock.realized,
    )
    .map_err(error)?;
    let mut actual = lock.clone();
    actual.content_sources.clear();
    if actual != expected {
        return Err(ServerStoreError::StaleRevision);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::reforger_addons::ReforgerAddonMetadata;

    #[cfg(windows)]
    #[test]
    fn addon_revision_root_is_identical_with_or_without_the_windows_verbatim_prefix() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("games.json"),
            include_bytes!("../../../../examples/json/appdata/games.json"),
        )
        .unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let ordinary = PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        let id = ServerInstanceId::try_from("server_00000000000000000000000000000001".to_string())
            .unwrap();
        let stores = [canonical, ordinary].map(|path| {
            let context =
                crate::core::server::ServerContext::capture(&path, Some("reforger")).unwrap();
            ServerStore::new(context).unwrap()
        });
        assert_eq!(
            stores[0].reforger_addon_root(&id).unwrap(),
            stores[1].reforger_addon_root(&id).unwrap()
        );
    }

    #[test]
    fn example_retains_latest_intent_separately_from_the_exact_realized_pin() {
        let lock: ReforgerAddonLock = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/reforger_addon_lock.json"
        ))
        .unwrap();
        validate_lock(&lock).unwrap();
        assert_eq!(lock.requested[0].version, None);
        assert_eq!(lock.realized[0].version, "1.0.0");
        assert!(lock.degraded);
        assert_eq!(lock.requested.len(), 2);
    }

    #[test]
    fn package_verification_detects_same_size_payload_corruption() {
        let root = tempfile::tempdir().unwrap();
        let item = ReforgerAddonMetadata {
            addon_id: "aaaaaaaaaaaaaaaa".into(),
            version: "1.0.0".into(),
            channel: ReforgerAddonChannel::Stable,
            provider: ReforgerAddonProvider::Local,
            dependencies: Vec::new(),
            scenarios: Vec::new(),
        };
        let files = [
            ("addon.gproj", b"project".as_slice()),
            ("resourceDatabase.rdb", b"resources".as_slice()),
            ("data.pak", b"payload".as_slice()),
        ];
        let mut rows = Vec::new();
        for (name, bytes) in files {
            fs::write(root.path().join(name), bytes).unwrap();
            let manifest = serde_json::json!({"version":1,"size":bytes.len(),"sha512":hex::encode(Sha512::digest(bytes))});
            fs::write(
                root.path().join(format!("{name}_1.0.0_manifest.json")),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            rows.push(serde_json::json!({"name":name,"size":bytes.len(),"isDownloaded":true,"isCorrupted":false}));
        }
        let meta = serde_json::json!({"meta":{"id":item.addon_id,"selectedRev":0,"versions":[{"version":"1.0.0","corrupted":false,"package":{"files":rows}}]}});
        fs::write(root.path().join("meta"), serde_json::to_vec(&meta).unwrap()).unwrap();
        let control = ContentStageControl::default();
        verify_package(root.path(), &item, &control).unwrap();
        fs::write(root.path().join("data.pak"), b"changed").unwrap();
        assert!(verify_package(root.path(), &item, &control).is_err());
    }
}
