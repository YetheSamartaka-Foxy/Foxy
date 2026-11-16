use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{NativeRuntimeLaunchPlan, NativeRuntimeManifest, NativeRuntimeReference};
use crate::core::server::content_staging::{ContentStageManifest, StageEntry, StageEntryKind};
use crate::core::server::{
    ConfigurationPreview, ContentStageControl, DeploymentPreparationReport, ReforgerAddonLock,
    ReforgerServerDefinition, ServerDefinition, ServerInstallation, ServerInstanceId, ServerStore,
    ServerStoreError, runtime_payload::PayloadInventory,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct NativeConfiguration {
    #[serde(default)]
    bind_address: Option<Ipv4Addr>,
    bind_port: u16,
    a2s: Option<QueryConfiguration>,
    game: GameConfiguration,
}

#[derive(Deserialize)]
struct QueryConfiguration {
    #[serde(default)]
    address: Option<Ipv4Addr>,
    port: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameConfiguration {
    name: String,
    max_players: u16,
    visible: bool,
    scenario_id: String,
    mods: Vec<NativeMod>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeMod {
    mod_id: String,
    version: String,
    required: bool,
}

impl NativeConfiguration {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, ServerStoreError> {
        let configuration: Self =
            serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
        let definition = ReforgerServerDefinition {
            name: configuration.game.name.clone(),
            bind_address: configuration.bind_address,
            bind_port: configuration.bind_port,
            a2s_port: configuration.a2s.as_ref().map(|query| query.port),
            max_players: configuration.game.max_players,
            visible: configuration.game.visible,
            scenario_id: Some(configuration.game.scenario_id.clone()),
            addons: configuration
                .game
                .mods
                .iter()
                .map(|item| crate::core::server::ReforgerAddonRequirement {
                    addon_id: item.mod_id.clone(),
                    version: Some(item.version.clone()),
                    required: Some(item.required),
                    preserved: Default::default(),
                })
                .collect(),
            cross_platform: None,
            mods_required_by_default: None,
            game_properties: Default::default(),
            preserved: Default::default(),
            redacted_secret_paths: Vec::new(),
        };
        crate::core::server::ServerDefinition::Reforger(Box::new(definition))
            .validate()
            .map_err(|_| ServerStoreError::Corrupt)?;
        if configuration.bind_address.is_none()
            || configuration.a2s.is_none()
            || configuration.game.mods.iter().any(|item| !item.required)
            || configuration
                .a2s
                .as_ref()
                .is_some_and(|query| query.address != configuration.bind_address)
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(configuration)
    }

    pub(super) fn ports(&self) -> Vec<u16> {
        let mut ports = vec![self.bind_port];
        if let Some(query) = &self.a2s {
            ports.push(query.port);
        }
        ports
    }
}

pub(super) fn locked_server_json(
    original: &[u8],
    lock: &ReforgerAddonLock,
) -> Result<Vec<u8>, ServerStoreError> {
    let review =
        crate::core::server::import_server_json(original).map_err(ServerStoreError::Invalid)?;
    crate::core::server::reforger_addons::check_definition(
        &ServerDefinition::Reforger(Box::new(review.definition)),
        lock,
    )?;
    let mut value: Value =
        serde_json::from_slice(original).map_err(|_| ServerStoreError::Corrupt)?;
    let mods = lock
        .realized
        .iter()
        .map(|item| {
            json!({
                "modId": item.addon_id.to_ascii_uppercase(),
                "version": item.version,
                "required": true
            })
        })
        .collect::<Vec<_>>();
    value
        .get_mut("game")
        .and_then(Value::as_object_mut)
        .ok_or(ServerStoreError::Corrupt)?
        .insert("mods".into(), Value::Array(mods));
    serde_json::to_vec(&value).map_err(|_| ServerStoreError::Corrupt)
}

fn validate_locked_mods(bytes: &[u8], lock: &ReforgerAddonLock) -> Result<(), ServerStoreError> {
    let configuration = NativeConfiguration::parse(bytes)?;
    let expected: Vec<_> = lock
        .realized
        .iter()
        .map(|item| (item.addon_id.clone(), item.version.as_str(), true))
        .collect();
    let actual: Vec<_> = configuration
        .game
        .mods
        .iter()
        .map(|item| {
            (
                item.mod_id.to_ascii_lowercase(),
                item.version.as_str(),
                item.required,
            )
        })
        .collect();
    if actual != expected {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(())
}

pub(super) fn validate_configuration(
    bytes: &[u8],
    manifest: &NativeRuntimeManifest,
) -> Result<(), ServerStoreError> {
    let configuration = NativeConfiguration::parse(bytes)?;
    if let Some(lock) = &manifest.reforger_addons {
        validate_locked_mods(bytes, lock)?;
        if configuration.game.scenario_id != lock.scenario_id {
            return Err(ServerStoreError::Corrupt);
        }
    } else if !configuration.game.mods.is_empty() {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(())
}

pub(super) fn validate_manifest(manifest: &NativeRuntimeManifest) -> Result<(), ServerStoreError> {
    if !manifest.headless_clients.is_empty() {
        return Err(ServerStoreError::Corrupt);
    }
    if let Some(lock) = &manifest.reforger_addons {
        validate_locked_manifest_entries(manifest, lock)?;
        return Ok(());
    }
    if !manifest.mission_templates.is_empty()
        || !manifest.server_mods.is_empty()
        || !manifest.required_mods.is_empty()
        || !manifest.optional_sources.is_empty()
        || manifest.key_trust.is_some()
        || !manifest.credential_bindings.is_empty()
        || manifest.realized_entries.len() != 2
        || manifest.realized_entries[0]
            != (StageEntry {
                path: "configuration".into(),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            })
        || manifest.realized_entries[1].path != "configuration/server.json"
        || manifest.realized_entries[1].kind != StageEntryKind::File
        || manifest.file_sources
            != BTreeMap::from([(
                "configuration/server.json".into(),
                "configuration/server.json".into(),
            )])
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(())
}

fn validate_locked_manifest_entries(
    manifest: &NativeRuntimeManifest,
    lock: &ReforgerAddonLock,
) -> Result<(), ServerStoreError> {
    if lock.channel != crate::core::server::ReforgerAddonChannel::Stable {
        return Err(ServerStoreError::Corrupt);
    }
    if !manifest.mission_templates.is_empty()
        || !manifest.server_mods.is_empty()
        || !manifest.required_mods.is_empty()
        || !manifest.optional_sources.is_empty()
        || manifest.key_trust.is_some()
        || !manifest.credential_bindings.is_empty()
    {
        return Err(ServerStoreError::Corrupt);
    }
    let mut expected_sources = BTreeMap::from([(
        "configuration/server.json".to_owned(),
        "configuration/server.json".to_owned(),
    )]);
    let realized: BTreeSet<_> = lock
        .realized
        .iter()
        .map(|item| item.addon_id.clone())
        .collect();
    if realized.len() != lock.realized.len() {
        return Err(ServerStoreError::Corrupt);
    }
    let mut file_paths = BTreeSet::from(["configuration/server.json".to_owned()]);
    for (destination, source) in &manifest.file_sources {
        if destination == "configuration/server.json" {
            continue;
        }
        let Some(tail) = destination.strip_prefix("engine/foxy-content/") else {
            return Err(ServerStoreError::Corrupt);
        };
        let Some((id, rest)) = tail.split_once('/') else {
            return Err(ServerStoreError::Corrupt);
        };
        if !realized.contains(id)
            || rest.is_empty()
            || source != &format!("content/addons/{id}/{rest}")
        {
            return Err(ServerStoreError::Corrupt);
        }
        file_paths.insert(destination.clone());
        expected_sources.insert(destination.clone(), source.clone());
    }
    if manifest.file_sources != expected_sources {
        return Err(ServerStoreError::Corrupt);
    }
    let mut seen = BTreeSet::new();
    let mut addon_roots = BTreeSet::new();
    for entry in &manifest.realized_entries {
        if !seen.insert(entry.path.as_str()) {
            return Err(ServerStoreError::Corrupt);
        }
        if entry.path == "configuration" {
            if entry.kind != StageEntryKind::Directory || entry.bytes != 0 || entry.blake3.is_some()
            {
                return Err(ServerStoreError::Corrupt);
            }
        } else if entry.path == "configuration/server.json" {
            if entry.kind != StageEntryKind::File
                || entry.blake3.as_ref().is_none_or(|hash| hash.len() != 64)
            {
                return Err(ServerStoreError::Corrupt);
            }
        } else if matches!(entry.path.as_str(), "engine" | "engine/foxy-content") {
            if entry.kind != StageEntryKind::Directory || entry.bytes != 0 || entry.blake3.is_some()
            {
                return Err(ServerStoreError::Corrupt);
            }
        } else {
            let Some(tail) = entry.path.strip_prefix("engine/foxy-content/") else {
                return Err(ServerStoreError::Corrupt);
            };
            let (id, rest) = tail.split_once('/').unwrap_or((tail, ""));
            if !realized.contains(id) || (entry.kind == StageEntryKind::File && rest.is_empty()) {
                return Err(ServerStoreError::Corrupt);
            }
            addon_roots.insert(id.to_owned());
            match entry.kind {
                StageEntryKind::Directory if entry.bytes == 0 && entry.blake3.is_none() => {}
                StageEntryKind::File
                    if entry.blake3.as_ref().is_some_and(|hash| hash.len() == 64)
                        && file_paths.contains(&entry.path) => {}
                _ => return Err(ServerStoreError::Corrupt),
            }
        }
    }
    if seen.len() != manifest.realized_entries.len()
        || !seen.contains("configuration")
        || !seen.contains("configuration/server.json")
        || addon_roots != realized
        || file_paths.len()
            != manifest
                .realized_entries
                .iter()
                .filter(|entry| entry.kind == StageEntryKind::File)
                .count()
    {
        return Err(ServerStoreError::Corrupt);
    }
    let config = manifest
        .realized_entries
        .iter()
        .find(|entry| entry.path == "configuration/server.json")
        .ok_or(ServerStoreError::Corrupt)?;
    if config.kind != StageEntryKind::File
        || config.blake3.as_ref().is_none_or(|hash| hash.len() != 64)
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(())
}

fn validate_mutable_roots(
    root: &Path,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    for name in ["profiles", "data", "cache", "runtime", "config"] {
        crate::core::server::store::check_directory_chain(&root.join("home").join(name))?;
    }
    let mut pending = vec![(root.join("home"), 0usize)];
    let mut count = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        control.checkpoint()?;
        crate::core::server::store::check_directory_chain(&directory)?;
        for entry in std::fs::read_dir(&directory).map_err(|_| ServerStoreError::Io)? {
            control.checkpoint()?;
            count += 1;
            if count > 100_000 || depth >= 32 {
                return Err(ServerStoreError::Invalid(
                    "Reforger mutable state exceeds safe inspection limits.".into(),
                ));
            }
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|_| ServerStoreError::Io)?;
            if metadata.is_dir() {
                crate::core::server::store::check_directory_chain(&entry.path())?;
                pending.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                crate::core::server::store::check_regular_file(&entry.path())?;
            } else {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    Ok(())
}

impl ServerStore {
    pub(super) fn reforger_runtime_launch_plan(
        &self,
        id: &ServerInstanceId,
        reference: &NativeRuntimeReference,
        manifest: &NativeRuntimeManifest,
        runtime: &Path,
        control: &ContentStageControl,
    ) -> Result<NativeRuntimeLaunchPlan, ServerStoreError> {
        validate_manifest(manifest)?;
        super::launch::validate_runtime_state(self, id, control)?;
        let config = runtime.join("configuration/server.json");
        let bytes = crate::core::server::content_staging::read_bounded_controlled(
            &config,
            1024 * 1024,
            control,
        )?;
        validate_configuration(&bytes, manifest)?;
        let configuration = NativeConfiguration::parse(&bytes)?;
        let state = self.record_root(id).join("runtime-state");
        validate_mutable_roots(&state, control)?;
        let path = super::launch::argument_path;
        let engine = runtime.join("engine");
        let executable = engine.join(&manifest.installation.executable);
        crate::core::server::store::check_regular_file(&executable)?;
        let project = engine.join("addons/data/ArmaReforger.gproj");
        crate::core::server::store::check_regular_file(&project)?;
        #[cfg(windows)]
        let (engine, executable) = (
            std::path::PathBuf::from(path(&engine)?),
            std::path::PathBuf::from(path(&executable)?),
        );
        let mut arguments = vec![
            "-config".into(),
            path(&config)?,
            "-gproj".into(),
            path(&project)?,
            "-profile".into(),
            path(&state.join("home/profiles"))?,
            "-addonDownloadDir".into(),
            path(&state.join("home/data"))?,
            "-addonTempDir".into(),
            format!(
                "{}{}",
                path(&state.join("home/cache"))?,
                std::path::MAIN_SEPARATOR
            ),
            "-logsDir".into(),
            path(&state.join("home/runtime"))?,
            "-nothrow".into(),
            "-maxFPS".into(),
            "30".into(),
        ];
        if manifest
            .reforger_addons
            .as_ref()
            .is_some_and(|lock| !lock.realized.is_empty())
        {
            arguments.extend([
                "-addonsDir".into(),
                path(&runtime.join("engine/foxy-content"))?,
            ]);
        }
        let monitoring_address = configuration
            .a2s
            .as_ref()
            .map(|query| query.address.unwrap_or(Ipv4Addr::LOCALHOST))
            .filter(|address| !address.is_unspecified())
            .unwrap_or(Ipv4Addr::LOCALHOST);
        let monitoring = Some(crate::core::server::monitoring::NativeMonitoringInput {
            endpoint: configuration
                .a2s
                .as_ref()
                .map(|query| SocketAddr::from((monitoring_address, query.port))),
            expected_name: Some(configuration.game.name.clone()),
            executable: executable.clone(),
            log_roots: vec![state.join("home/runtime")],
        });
        Ok(NativeRuntimeLaunchPlan {
            instance_id: id.clone(),
            runtime_id: reference.runtime_id.clone(),
            executable,
            directory: engine,
            arguments,
            udp_ports: configuration.ports(),
            environment: super::launch::runtime_environment(
                &state,
                manifest.installation.platform,
            )?,
            profiles: state.join("home/profiles/profile"),
            monitoring,
            profile_seed: None,
            configuration_seeds: Vec::new(),
            headless: Vec::new(),
            spawned: false,
        })
    }

    pub(super) fn reforger_runtime_manifest(
        &self,
        preparation: &DeploymentPreparationReport,
        installation: ServerInstallation,
        payload: PayloadInventory,
        settings: &ReforgerServerDefinition,
        preview: ConfigurationPreview,
        stage: &ContentStageManifest,
    ) -> Result<NativeRuntimeManifest, ServerStoreError> {
        if settings.bind_address.is_none() || settings.a2s_port.is_none() {
            return Err(ServerStoreError::Invalid(
                "Choose an explicit IPv4 bind address and A2S query port before Reforger native activation. Unspecified endpoint defaults have not been verified.".into(),
            ));
        }
        let addon_lock = stage.reforger_addons.as_ref();
        if !stage.bindings.bindings.is_empty()
            || !stage.acquisitions.is_empty()
            || !stage.missions.is_empty()
        {
            return Err(ServerStoreError::Invalid(
                "Reforger native activation does not accept unrelated staged content.".into(),
            ));
        }
        match addon_lock {
            Some(lock) => {
                crate::core::server::reforger_addons::check_definition(
                    &ServerDefinition::Reforger(Box::new(settings.clone())),
                    lock,
                )?;
                let expected: BTreeSet<_> = lock
                    .realized
                    .iter()
                    .map(|item| format!("addons/{}", item.addon_id))
                    .collect();
                let sources: BTreeSet<_> = stage
                    .sources
                    .iter()
                    .map(|source| source.destination.clone())
                    .collect();
                let staged_roots: BTreeSet<_> = stage
                    .entries
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .path
                            .strip_prefix("addons/")?
                            .split('/')
                            .next()
                            .map(|id| format!("addons/{id}"))
                    })
                    .collect();
                if sources != expected
                    || staged_roots != expected
                    || stage.entries.iter().any(|entry| {
                        entry.path != "addons"
                            && !expected.iter().any(|root| {
                                entry.path.starts_with(&format!("{root}/")) || entry.path == *root
                            })
                    })
                {
                    return Err(ServerStoreError::Corrupt);
                }
            }
            None if !settings.addons.is_empty()
                || !stage.sources.is_empty()
                || stage.total_bytes != 0
                || stage
                    .entries
                    .iter()
                    .any(|entry| entry.kind == StageEntryKind::File) =>
            {
                return Err(ServerStoreError::Invalid(
                    "Acquire and review the exact Reforger addon set before native activation."
                        .into(),
                ));
            }
            None => {}
        }
        if settings.scenario_id.is_none() {
            return Err(ServerStoreError::Invalid(
                "Native activation requires a reviewed Reforger scenario resource.".into(),
            ));
        }
        if super::super::engine_layout::observe_tested_layout(&installation).is_none() {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let [file] = preview.files.as_slice() else {
            return Err(ServerStoreError::Corrupt);
        };
        if file.relative_path != "server.json" {
            return Err(ServerStoreError::Corrupt);
        }
        let config_bytes = if let Some(lock) = addon_lock {
            locked_server_json(file.contents.as_bytes(), lock)?
        } else {
            file.contents.as_bytes().to_vec()
        };
        NativeConfiguration::parse(&config_bytes)?;
        let original_entry = preparation
            .manifest
            .entries
            .iter()
            .find(|entry| entry.path == "configuration/server.json")
            .ok_or(ServerStoreError::Corrupt)?;
        if original_entry.bytes != file.contents.len() as u64
            || original_entry.blake3.as_deref()
                != Some(blake3::hash(file.contents.as_bytes()).to_hex().as_str())
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let entry = StageEntry {
            path: "configuration/server.json".into(),
            kind: StageEntryKind::File,
            bytes: config_bytes.len() as u64,
            blake3: Some(blake3::hash(&config_bytes).to_hex().to_string()),
        };
        if original_entry.path != "configuration/server.json" {
            return Err(ServerStoreError::Corrupt);
        }
        let mut realized_entries = vec![
            StageEntry {
                path: "configuration".into(),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            },
            entry,
        ];
        let mut file_sources = BTreeMap::from([(
            "configuration/server.json".into(),
            "configuration/server.json".into(),
        )]);
        if let Some(lock) = addon_lock {
            for staged in stage
                .entries
                .iter()
                .filter(|entry| entry.path.starts_with("addons/"))
            {
                let tail = staged
                    .path
                    .strip_prefix("addons/")
                    .ok_or(ServerStoreError::Corrupt)?;
                let destination = format!("engine/foxy-content/{tail}");
                let mut entry = staged.clone();
                entry.path = destination.clone();
                if entry.kind == StageEntryKind::File {
                    file_sources.insert(destination, format!("content/{}", staged.path));
                }
                realized_entries.push(entry);
            }
            if lock.realized.iter().any(|item| {
                !stage.entries.iter().any(|entry| {
                    entry.path == format!("addons/{}", item.addon_id)
                        || entry
                            .path
                            .starts_with(&format!("addons/{}/", item.addon_id))
                })
            }) {
                return Err(ServerStoreError::Corrupt);
            }
            if !lock.realized.is_empty() {
                realized_entries.push(StageEntry {
                    path: "engine".into(),
                    kind: StageEntryKind::Directory,
                    bytes: 0,
                    blake3: None,
                });
                realized_entries.push(StageEntry {
                    path: "engine/foxy-content".into(),
                    kind: StageEntryKind::Directory,
                    bytes: 0,
                    blake3: None,
                });
            }
        }
        realized_entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut unique = BTreeSet::new();
        if realized_entries
            .iter()
            .any(|entry| !unique.insert(entry.path.clone()))
        {
            return Err(ServerStoreError::Corrupt);
        }
        let mut locked_addons = addon_lock.cloned();
        if let Some(lock) = &mut locked_addons {
            lock.content_sources.clear();
        }
        let manifest = NativeRuntimeManifest {
            schema_version: 1,
            game_space_id: self.context().space_id().into(),
            instance_id: preparation.manifest.record.instance_id.clone(),
            preparation_id: preparation.preparation_id.clone(),
            installation,
            payload,
            realized_entries,
            file_sources,
            mission_templates: Vec::new(),
            server_mods: Vec::new(),
            required_mods: Vec::new(),
            optional_sources: Vec::new(),
            key_trust: None,
            credential_bindings: Vec::new(),
            reforger_addons: locked_addons,
            headless_clients: Vec::new(),
        };
        validate_manifest(&manifest)?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration() -> serde_json::Value {
        serde_json::json!({
            "bindAddress": "127.0.0.1", "bindPort": 2001,
            "a2s": {"address": "127.0.0.1", "port": 17777},
            "game": {
                "name": "Fixture", "maxPlayers": 4, "visible": false,
                "scenarioId": "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf", "mods": []
            }
        })
    }

    #[test]
    fn native_definition_example_preserves_explicit_loopback_binding() {
        let definition: crate::core::server::ServerDefinition = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/reforger_native_definition.json"
        ))
        .unwrap();
        definition.validate().unwrap();
        let crate::core::server::ServerDefinition::Reforger(settings) = definition else {
            panic!("wrong example game")
        };
        assert_eq!(settings.bind_address, Some(Ipv4Addr::LOCALHOST));
        assert_eq!(settings.bind_port, 2001);
        assert_eq!(settings.a2s_port, Some(17777));
        assert!(settings.addons.is_empty());
    }

    #[test]
    fn reforger_manifest_rejects_arma_artifacts_and_unreviewed_configuration_sources() {
        let mut manifest: NativeRuntimeManifest = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/native_runtime_manifest.json"
        ))
        .unwrap();
        manifest.realized_entries = vec![
            StageEntry {
                path: "configuration".into(),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            },
            StageEntry {
                path: "configuration/server.json".into(),
                kind: StageEntryKind::File,
                bytes: 2,
                blake3: Some(blake3::hash(b"{}").to_hex().to_string()),
            },
        ];
        manifest.file_sources = BTreeMap::from([(
            "configuration/server.json".into(),
            "configuration/server.json".into(),
        )]);
        validate_manifest(&manifest).unwrap();
        let mut changed = manifest.clone();
        changed.required_mods.push("foxy-content/RepoAlpha".into());
        assert_eq!(validate_manifest(&changed), Err(ServerStoreError::Corrupt));
        changed = manifest.clone();
        changed.mission_templates.push("Example.World".into());
        assert_eq!(validate_manifest(&changed), Err(ServerStoreError::Corrupt));
        changed = manifest.clone();
        changed.realized_entries[1].path = "engine/foxy-keys/key.bikey".into();
        assert_eq!(validate_manifest(&changed), Err(ServerStoreError::Corrupt));
        changed = manifest;
        changed.file_sources.insert(
            "configuration/server.json".into(),
            "content/other.json".into(),
        );
        assert_eq!(validate_manifest(&changed), Err(ServerStoreError::Corrupt));
    }

    #[test]
    fn native_configuration_protects_only_its_typed_udp_endpoints() {
        let mut value = configuration();
        let parse = |value: &serde_json::Value| {
            NativeConfiguration::parse(&serde_json::to_vec(value).unwrap())
        };
        assert_eq!(parse(&value).unwrap().ports(), vec![2001, 17777]);
        value.as_object_mut().unwrap().remove("a2s");
        assert!(parse(&value).is_err());
        value = configuration();
        value.as_object_mut().unwrap().remove("bindAddress");
        assert!(parse(&value).is_err());
        value = configuration();
        value["bindPort"] = 0.into();
        assert!(parse(&value).is_err());
        value = configuration();
        value["a2s"]["port"] = 2001.into();
        assert!(parse(&value).is_err());
        value = configuration();
        value["a2s"]["address"] = "0.0.0.0".into();
        assert!(parse(&value).is_err());
        value = configuration();
        value["game"]["mods"] = serde_json::json!([{"modId": "1234567890ABCDEF"}]);
        assert!(parse(&value).is_err());
        value = configuration();
        value["profile"] = "outside".into();
        value["game"]["gameProperties"] = serde_json::json!({
            "serverMaxViewDistance": 1600,
            "missionHeader": {"kept": true}
        });
        assert_eq!(parse(&value).unwrap().ports(), vec![2001, 17777]);
    }

    #[test]
    fn locked_configuration_renders_pins_as_required_and_omits_optional_missing_mods() {
        let id = "aaaaaaaaaaaaaaaa";
        let omitted = "bbbbbbbbbbbbbbbb";
        let scenario = "{0123456789ABCDEF}missions/main.conf";
        let mut original = configuration();
        original["game"]["scenarioId"] = scenario.into();
        original["game"]["gameProperties"] = serde_json::json!({"customSetting": 17});
        original["customRootField"] = "preserved".into();
        original["game"]["modsRequiredByDefault"] = false.into();
        original["game"]["mods"] = serde_json::json!([
            {"modId": id, "required": true},
            {"modId": omitted, "required": false}
        ]);
        let requirements = vec![
            crate::core::server::ReforgerAddonRequirement {
                addon_id: id.into(),
                version: None,
                required: Some(true),
                preserved: Default::default(),
            },
            crate::core::server::ReforgerAddonRequirement {
                addon_id: omitted.into(),
                version: None,
                required: Some(false),
                preserved: Default::default(),
            },
        ];
        let metadata = vec![
            crate::core::server::reforger_addons::ReforgerAddonMetadata {
                addon_id: id.into(),
                version: "1.2.3".into(),
                channel: crate::core::server::ReforgerAddonChannel::Stable,
                provider: crate::core::server::ReforgerAddonProvider::Local,
                dependencies: Vec::new(),
                scenarios: Vec::new(),
            },
        ];
        let lock = crate::core::server::reforger_addons::resolve_addons(
            &requirements,
            Some(false),
            scenario,
            None,
            crate::core::server::ReforgerAddonChannel::Stable,
            &metadata,
        )
        .unwrap();
        let original_bytes = serde_json::to_vec(&original).unwrap();
        let rendered = locked_server_json(&original_bytes, &lock).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&rendered).unwrap();
        assert_eq!(value["customRootField"], "preserved");
        assert_eq!(value["game"]["gameProperties"]["customSetting"], 17);
        assert_eq!(
            value["game"]["mods"],
            serde_json::json!([
                {"modId": id.to_ascii_uppercase(), "version": "1.2.3", "required": true}
            ])
        );
        validate_locked_mods(&rendered, &lock).unwrap();
        let mut different_scenario = original.clone();
        different_scenario["game"]["scenarioId"] = "{FEDCBA9876543210}Missions/Other.conf".into();
        assert!(
            locked_server_json(&serde_json::to_vec(&different_scenario).unwrap(), &lock).is_err()
        );
        let mut changed = original;
        changed["game"]["mods"][0]["version"] = "0.1".into();
        assert!(locked_server_json(&serde_json::to_vec(&changed).unwrap(), &lock).is_err());
    }

    #[test]
    fn mutable_state_preserves_files_and_refuses_file_directory_collisions() {
        let root = tempfile::tempdir().unwrap();
        for name in ["profiles", "data", "cache", "runtime"] {
            std::fs::create_dir_all(root.path().join("home").join(name)).unwrap();
        }
        let file = root.path().join("home/profiles/saved.bin");
        std::fs::write(&file, b"retained").unwrap();
        validate_mutable_roots(root.path(), &ContentStageControl::default()).unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"retained");
        std::fs::remove_dir(root.path().join("home/cache")).unwrap();
        std::fs::write(root.path().join("home/cache"), b"operator data").unwrap();
        assert_eq!(
            validate_mutable_roots(root.path(), &ContentStageControl::default()),
            Err(ServerStoreError::UnsafePath)
        );
    }

    #[cfg(unix)]
    #[test]
    fn mutable_state_refuses_nested_links_without_following_them() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        for name in ["profiles", "data", "cache", "runtime"] {
            std::fs::create_dir_all(root.path().join("home").join(name)).unwrap();
        }
        std::os::unix::fs::symlink(outside.path(), root.path().join("home/data/addons")).unwrap();
        assert_eq!(
            validate_mutable_roots(root.path(), &ContentStageControl::default()),
            Err(ServerStoreError::UnsafePath)
        );
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}
