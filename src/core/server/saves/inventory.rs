use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::model::select_session;
use serde_json::Value;

use super::super::{
    ContentStageControl, ReforgerProviderCoverage, ReforgerSessionSummary, ServerDefinitionKind,
    ServerInstanceId, ServerStore, ServerStoreError,
};
use super::model::{IntentDocument, ReforgerSessionIntent, ReforgerSessionInventory, digest};

const NATIVE_SAVE_VERSION: &str = "1.9.0.37";
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_SAVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_INVENTORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SESSIONS: usize = 256;
const MAX_SAVEPOINT_FILES: usize = 4096;
const MAX_DISCOVERED_ENTRIES: usize = 16_384;

#[derive(Clone, Debug)]
struct NativeMetadata {
    save_id: String,
    save_time: i64,
    scenario: String,
    game_version: String,
    playthrough: u32,
    savepoint: u32,
    addons_empty: bool,
    unknown_fields: bool,
}

impl ServerStore {
    pub fn reforger_sessions(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<ReforgerSessionInventory, ServerStoreError> {
        control.checkpoint()?;
        if self.capabilities().definition_kind != Some(ServerDefinitionKind::Reforger) {
            return Err(ServerStoreError::UnsupportedGame);
        }
        self.show(id)?;
        let (activation, _) = self
            .read_activation_state(id, control)
            .map_err(|_| ServerStoreError::Corrupt)?
            .ok_or(ServerStoreError::NotFound)?;
        let runtime = activation
            .deployment
            .runtime
            .as_ref()
            .ok_or(ServerStoreError::NotFound)?;
        let manifest = self.verify_native_runtime(id, runtime, control)?;
        if manifest.installation.game != ServerDefinitionKind::Reforger {
            return Err(ServerStoreError::Corrupt);
        }
        let runtime_dir = self.runtime_directory(id, runtime)?;
        let config_path = runtime_dir.join("configuration/server.json");
        crate::core::server::store::check_regular_file(&config_path)?;
        let config = read_file(&config_path, 1024 * 1024, control)?;
        let config_value: Value =
            serde_json::from_slice(&config).map_err(|_| ServerStoreError::Corrupt)?;
        let scenario = config_value
            .pointer("/game/scenarioId")
            .and_then(Value::as_str)
            .ok_or(ServerStoreError::Corrupt)?
            .to_owned();
        let provider = provider_coverage(&config)?;
        let (generation, intent) = self.read_intent(id)?;
        let profile_root = self
            .record_root(id)
            .join("runtime-state/home/profiles/profile");
        let save_root = profile_root.join(".save");
        let session_root = save_root.join("game");
        crate::core::server::store::check_directory_chain(&profile_root)?;
        crate::core::server::store::check_directory_chain(&save_root)?;
        crate::core::server::store::check_directory_chain(&session_root)?;

        let mut metadata_files = Vec::new();
        if provider.local_default
            && session_root
                .try_exists()
                .map_err(|_| ServerStoreError::Io)?
        {
            collect_metadata_files(&session_root, &mut metadata_files, control)?;
        }
        metadata_files.sort();
        if metadata_files.len() > MAX_SESSIONS {
            return Err(ServerStoreError::Invalid(
                "Reforger save inventory exceeds the bounded entry limit.".into(),
            ));
        }
        let mut sessions = Vec::with_capacity(metadata_files.len());
        let mut save_ids = BTreeSet::new();
        let mut total_bytes = 0u64;
        for metadata_path in metadata_files {
            control.checkpoint()?;
            let rel_meta = metadata_path
                .strip_prefix(&session_root)
                .map_err(|_| ServerStoreError::UnsafePath)?;
            let relative_path = portable_relative(rel_meta)?;
            let location = parse_location(rel_meta)?;
            let metadata_bytes = read_file(&metadata_path, MAX_METADATA_BYTES, control)?;
            let parsed = parse_metadata(&metadata_bytes, location).map_err(|_| {
                ServerStoreError::Invalid("A Reforger save has unsupported or damaged metadata. Preserve a backup and use its matching engine and configuration, or use a separate clean instance. No save was changed.".into())
            })?;
            if !save_ids.insert(parsed.save_id.to_ascii_lowercase()) {
                return Err(ServerStoreError::Corrupt);
            }
            let savepoint_root = metadata_path.parent().ok_or(ServerStoreError::UnsafePath)?;
            let (bytes, blake3, world_state_present) = hash_savepoint(savepoint_root, control)?;
            total_bytes = total_bytes
                .checked_add(bytes)
                .ok_or(ServerStoreError::Corrupt)?;
            if total_bytes > MAX_INVENTORY_BYTES {
                return Err(ServerStoreError::Invalid(
                    "Reforger save inventory exceeds the bounded byte limit.".into(),
                ));
            }
            let mut blockers = Vec::new();
            blockers.extend(session_compatibility_blockers(
                &parsed,
                &scenario,
                manifest
                    .reforger_addons
                    .as_ref()
                    .is_some_and(|lock| !lock.realized.is_empty()),
            ));
            if !world_state_present {
                blockers.push("The savepoint has no WorldState blob.".into());
            }
            blockers.extend(provider.blockers.iter().cloned());
            sessions.push(ReforgerSessionSummary {
                save_id: parsed.save_id,
                scenario: parsed.scenario,
                game_version: parsed.game_version,
                saved_unix: parsed.save_time,
                playthrough: parsed.playthrough,
                savepoint: parsed.savepoint,
                relative_path,
                bytes,
                blake3,
                compatible: blockers.is_empty(),
                blockers,
            });
        }
        let fingerprint = digest(&(
            self.context().space_id(),
            self.context().game_id(),
            id,
            activation.generation,
            &activation.context_fingerprint,
            &runtime.manifest_fingerprint,
            blake3::hash(&config).to_hex().to_string(),
            generation,
            &intent,
            &scenario,
            &provider,
            &sessions,
        ))?;
        Ok(ReforgerSessionInventory {
            instance_id: id.clone(),
            generation,
            intent,
            sessions,
            provider,
            fingerprint,
            writes_performed: false,
        })
    }

    pub fn reforger_session_launch(
        &self,
        id: &ServerInstanceId,
        config_bytes: &[u8],
        manifest: &crate::core::server::deployment_activation::NativeRuntimeManifest,
        control: &ContentStageControl,
    ) -> Result<super::model::ReforgerSessionLaunch, ServerStoreError> {
        control.checkpoint()?;
        let state = self
            .read_activation_state(id, control)
            .map_err(|_| ServerStoreError::Corrupt)?
            .ok_or(ServerStoreError::NotFound)?
            .0;
        let runtime = state
            .deployment
            .runtime
            .as_ref()
            .ok_or(ServerStoreError::NotFound)?;
        let active_manifest = self.verify_native_runtime(id, runtime, control)?;
        if active_manifest != *manifest {
            return Err(ServerStoreError::StaleRevision);
        }
        let runtime_config = read_file(
            &self
                .runtime_directory(id, runtime)?
                .join("configuration/server.json"),
            1024 * 1024,
            control,
        )?;
        if runtime_config != config_bytes {
            return Err(ServerStoreError::StaleRevision);
        }
        let config: Value =
            serde_json::from_slice(config_bytes).map_err(|_| ServerStoreError::Corrupt)?;
        let provider = provider_coverage(config_bytes)?;
        let inventory = self.reforger_sessions(id, control)?;
        let scenario = config
            .pointer("/game/scenarioId")
            .and_then(Value::as_str)
            .ok_or(ServerStoreError::Corrupt)?;
        let configured_load = config
            .pointer("/game/gameProperties/persistence/loadSessionSave")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let configured_keep = config
            .pointer("/game/gameProperties/persistence/keepSessionSave")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let intent = inventory.intent.clone();
        let provider_blockers = provider.intent_blockers(&intent);
        if !provider_blockers.is_empty() {
            return Err(ServerStoreError::Invalid(provider_blockers.join(" ")));
        }
        let mut blockers = Vec::new();
        let selected_save_id = match &intent {
            ReforgerSessionIntent::Configured
                if configured_load
                    && !inventory
                        .sessions
                        .iter()
                        .any(|session| session.scenario == scenario) =>
            {
                None
            }
            ReforgerSessionIntent::Configured if configured_load => {
                select_session(&inventory, &ReforgerSessionIntent::Latest, &mut blockers)
            }
            ReforgerSessionIntent::Configured | ReforgerSessionIntent::New => None,
            selected => select_session(&inventory, selected, &mut blockers),
        };
        if let Some(save_id) = selected_save_id.as_ref() {
            let selected = inventory
                .sessions
                .iter()
                .find(|session| &session.save_id == save_id)
                .ok_or(ServerStoreError::Corrupt)?;
            if selected.scenario
                != manifest.reforger_addons.as_ref().map_or_else(
                    || selected.scenario.clone(),
                    |lock| lock.scenario_id.clone(),
                )
            {
                blockers.push(
                    "Selected save scenario differs from the committed runtime scenario.".into(),
                );
            }
            if manifest
                .reforger_addons
                .as_ref()
                .is_some_and(|lock| !lock.realized.is_empty())
            {
                blockers.push("Save metadata does not provide a validated used-addon identity set for the committed runtime.".into());
            }
        }
        if !blockers.is_empty() {
            return Err(ServerStoreError::Invalid(blockers.join(" ")));
        }
        let selected = selected_save_id.as_deref().and_then(|save_id| {
            inventory
                .sessions
                .iter()
                .find(|session| session.save_id == save_id)
        });
        let load = selected.is_some()
            || matches!(intent, ReforgerSessionIntent::Configured) && configured_load;
        let keep = if matches!(intent, ReforgerSessionIntent::New) {
            true
        } else {
            configured_keep
        };
        let fingerprint = digest(&(
            &inventory.fingerprint,
            &intent,
            &selected_save_id,
            load,
            keep,
            &provider,
        ))?;
        Ok(super::model::ReforgerSessionLaunch {
            intent,
            selected_save_id,
            playthrough: selected.map(|save| save.playthrough),
            savepoint: selected.map(|save| save.savepoint),
            load,
            keep,
            fingerprint,
            inventory_fingerprint: inventory.fingerprint,
            provider,
        })
    }

    fn read_intent(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(u64, ReforgerSessionIntent), ServerStoreError> {
        let path = self.record_root(id).join("session-intent.json");
        crate::core::server::store::check_directory_chain(
            path.parent().ok_or(ServerStoreError::UnsafePath)?,
        )?;
        crate::core::server::store::check_regular_file(&path)?;
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok((0, ReforgerSessionIntent::Configured));
        }
        let bytes = crate::core::server::content_staging::read_bounded_controlled(
            &path,
            MAX_METADATA_BYTES as usize,
            &ContentStageControl::default(),
        )?;
        let document: IntentDocument =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if document.schema_version != 1
            || document.game_space_id != self.context().space_id()
            || document.game_id != self.context().game_id()
            || &document.instance_id != id
            || document.generation == 0
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok((document.generation, document.intent))
    }
}

pub(crate) fn provider_coverage(
    config_bytes: &[u8],
) -> Result<ReforgerProviderCoverage, ServerStoreError> {
    let config: Value =
        serde_json::from_slice(config_bytes).map_err(|_| ServerStoreError::Corrupt)?;
    let mut blockers = Vec::new();
    for pointer in [
        "/persistence",
        "/game/persistence",
        "/game/gameProperties/persistence",
    ] {
        let Some(value) = config.pointer(pointer).filter(|value| !value.is_null()) else {
            continue;
        };
        let object = value.as_object().ok_or(ServerStoreError::Corrupt)?;
        for (key, value) in object {
            match key.as_str() {
                "loadSessionSave" | "keepSessionSave" => {
                    if !value.is_boolean() {
                        return Err(ServerStoreError::Corrupt);
                    }
                }
                "autoSaveInterval" => {
                    if value.as_u64().is_none_or(|n| n > 60) {
                        return Err(ServerStoreError::Corrupt);
                    }
                }
                "saveRetention" => {
                    if !value.as_u64().is_some_and(|n| (1..=128).contains(&n)) {
                        return Err(ServerStoreError::Corrupt);
                    }
                }
                "hiveId" => {
                    if value.as_u64().is_none_or(|n| n > 16_383) {
                        return Err(ServerStoreError::Corrupt);
                    }
                }
                "databases" | "storages" => {
                    if !(value.as_array().is_some_and(Vec::is_empty)
                        || value.as_object().is_some_and(serde_json::Map::is_empty))
                    {
                        blockers.push("Custom Reforger persistence databases or storages require an external provider adapter; only local profile saves are supported.".into());
                    }
                }
                _ => blockers.push(
                    "Unknown Reforger persistence settings require an external provider adapter."
                        .into(),
                ),
            }
        }
    }
    Ok(ReforgerProviderCoverage {
        local_default: blockers.is_empty(),
        blockers,
    })
}

fn collect_metadata_files(
    root: &Path,
    result: &mut Vec<PathBuf>,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut discovered = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        control.checkpoint()?;
        if depth > 8 {
            return Err(ServerStoreError::UnsafePath);
        }
        crate::core::server::store::check_directory_chain(&directory)?;
        for item in fs::read_dir(&directory).map_err(|_| ServerStoreError::Io)? {
            control.checkpoint()?;
            let item = item.map_err(|_| ServerStoreError::Io)?;
            discovered += 1;
            if discovered > MAX_DISCOVERED_ENTRIES {
                return Err(ServerStoreError::Invalid(
                    "Reforger save inventory exceeds the bounded entry limit.".into(),
                ));
            }
            let path = item.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink()
                || !item.file_type().map_err(|_| ServerStoreError::Io)?.is_dir()
                    && !metadata.is_file()
            {
                return Err(ServerStoreError::UnsafePath);
            }
            if metadata.is_dir() {
                stack.push((path, depth + 1));
            } else if item.file_name() == "meta-info.json" {
                crate::core::server::store::check_regular_file(&path)?;
                result.push(path);
                if result.len() > MAX_SESSIONS {
                    return Err(ServerStoreError::Invalid(
                        "Reforger save inventory exceeds the bounded entry limit.".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn parse_location(path: &Path) -> Result<(u32, u32), ServerStoreError> {
    portable_relative(path)?;
    let parts: Vec<_> = path.components().collect();
    if parts.len() != 4
        || parts[0].as_os_str().is_empty()
        || parts[3].as_os_str() != "meta-info.json"
    {
        return Err(ServerStoreError::UnsafePath);
    }
    let parse_number = |value: &std::ffi::OsStr, prefix: &str| {
        value
            .to_str()
            .and_then(|value| value.strip_prefix(prefix))
            .and_then(|n| n.parse::<u32>().ok())
            .ok_or(ServerStoreError::UnsafePath)
    };
    let playthrough = parse_number(parts[1].as_os_str(), "playthrough")?;
    let savepoint = parse_number(parts[2].as_os_str(), "savepoint")?;
    Ok((playthrough, savepoint))
}

fn portable_relative(path: &Path) -> Result<String, ServerStoreError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(ServerStoreError::UnsafePath);
        };
        let part = part.to_str().ok_or(ServerStoreError::UnsafePath)?;
        if part.is_empty() || part.contains(['/', '\\', ':']) || part.chars().any(char::is_control)
        {
            return Err(ServerStoreError::UnsafePath);
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn parse_metadata(bytes: &[u8], location: (u32, u32)) -> Result<NativeMetadata, ServerStoreError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    let object = value.as_object().ok_or(ServerStoreError::Corrupt)?;
    let save_id = string_field(object, "m_Id")?.to_owned();
    if !super::valid_uuid(&save_id) {
        return Err(ServerStoreError::Corrupt);
    }
    let scenario = string_field(object, "m_sMissionResource")?.to_owned();
    if !valid_scenario_resource(&scenario) {
        return Err(ServerStoreError::Corrupt);
    }
    let game_version = string_field(object, "m_sGameVersion")?.to_owned();
    let _save_type = integer_field(object, "m_eType")?;
    if integer_field(object, "m_iSavedAtUnix")? < 0
        || integer_field(object, "m_iStartedUnix")? < 0
        || integer_field(object, "m_iSavePointNr")? != i64::from(location.1)
        || integer_field(object, "m_iPlaythroughNr")? != i64::from(location.0)
    {
        return Err(ServerStoreError::Corrupt);
    }
    for name in ["m_sSavePointDisplayName", "m_sPlaythroughDisplayName"] {
        object
            .get(name)
            .and_then(Value::as_str)
            .ok_or(ServerStoreError::Corrupt)?;
    }
    let _started_unix = integer_field(object, "m_iStartedUnix")?;
    if integer_field(object, "m_iPlaytimeSeconds")? < 0 {
        return Err(ServerStoreError::Corrupt);
    }
    let used_addons = object
        .get("m_aUsedAddons")
        .ok_or(ServerStoreError::Corrupt)?;
    let addons_empty = used_addons.as_array().is_some_and(Vec::is_empty);
    let known_fields = [
        "m_Id",
        "m_eType",
        "m_iSavedAtUnix",
        "m_iSavePointNr",
        "m_sSavePointDisplayName",
        "m_sMissionResource",
        "m_iPlaythroughNr",
        "m_sPlaythroughDisplayName",
        "m_iStartedUnix",
        "m_iPlaytimeSeconds",
        "m_sGameVersion",
        "m_aUsedAddons",
    ];
    let unknown_fields = object
        .keys()
        .any(|key| !known_fields.contains(&key.as_str()));
    Ok(NativeMetadata {
        save_id,
        save_time: integer_field(object, "m_iSavedAtUnix")?,
        scenario,
        game_version,
        playthrough: location.0,
        savepoint: location.1,
        addons_empty,
        unknown_fields,
    })
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a str, ServerStoreError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ServerStoreError::Corrupt)
}

fn integer_field(
    object: &serde_json::Map<String, Value>,
    name: &str,
) -> Result<i64, ServerStoreError> {
    object
        .get(name)
        .and_then(Value::as_i64)
        .ok_or(ServerStoreError::Corrupt)
}

fn session_compatibility_blockers(
    metadata: &NativeMetadata,
    active_scenario: &str,
    runtime_has_addons: bool,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if metadata.game_version != NATIVE_SAVE_VERSION {
        blockers.push(format!(
            "Save engine version {} is not the accepted version {NATIVE_SAVE_VERSION}.",
            metadata.game_version
        ));
    }
    if metadata.scenario != active_scenario {
        blockers.push("Save scenario does not exactly match the active runtime scenario.".into());
    }
    if !metadata.addons_empty || runtime_has_addons {
        blockers.push("Save metadata does not provide a validated used-addon identity set for the committed runtime.".into());
    }
    if metadata.unknown_fields {
        blockers.push("Save metadata contains unknown fields that are preserved but not treated as compatible.".into());
    }
    blockers
}

fn valid_scenario_resource(value: &str) -> bool {
    let value = value.strip_prefix("full").unwrap_or(value);
    let Some((guid, resource)) = value
        .strip_prefix('{')
        .and_then(|value| value.split_once('}'))
    else {
        return false;
    };
    guid.len() == 16
        && guid.bytes().all(|byte| byte.is_ascii_hexdigit())
        && resource.starts_with("Missions/")
        && resource.ends_with(".conf")
        && !resource.contains(['\\', ':'])
        && !resource
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !value.chars().any(char::is_control)
}

fn hash_savepoint(
    root: &Path,
    control: &ContentStageControl,
) -> Result<(u64, String, bool), ServerStoreError> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut world_state_present = false;
    let mut discovered = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        control.checkpoint()?;
        if depth > 8 {
            return Err(ServerStoreError::UnsafePath);
        }
        for item in fs::read_dir(&directory).map_err(|_| ServerStoreError::Io)? {
            control.checkpoint()?;
            let item = item.map_err(|_| ServerStoreError::Io)?;
            discovered += 1;
            if discovered > MAX_SAVEPOINT_FILES {
                return Err(ServerStoreError::Invalid(
                    "A Reforger savepoint exceeds the bounded file count.".into(),
                ));
            }
            let path = item.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink() {
                return Err(ServerStoreError::UnsafePath);
            }
            if metadata.is_dir() {
                stack.push((path, depth + 1));
            } else if metadata.is_file() {
                crate::core::server::store::check_regular_file(&path)?;
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| ServerStoreError::UnsafePath)?;
                let name = portable_relative(relative)?;
                let (bytes, hash) = hash_file(&path, control)?;
                total = total.checked_add(bytes).ok_or(ServerStoreError::Corrupt)?;
                if total > MAX_INVENTORY_BYTES {
                    return Err(ServerStoreError::Invalid(
                        "Reforger save inventory exceeds the bounded byte limit.".into(),
                    ));
                }
                world_state_present |= relative
                    .parent()
                    .is_some_and(|parent| parent == Path::new("WorldState"))
                    && relative
                        .extension()
                        .is_some_and(|extension| extension == "blob");
                entries.push((name, bytes, hash));
            } else {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    if entries.is_empty() {
        return Err(ServerStoreError::Corrupt);
    }
    Ok((total, digest(&entries)?, world_state_present))
}

fn read_file(
    path: &Path,
    limit: u64,
    control: &ContentStageControl,
) -> Result<Vec<u8>, ServerStoreError> {
    crate::core::server::store::check_regular_file(path)?;
    let mut file = fs::File::open(path).map_err(|_| ServerStoreError::Io)?;
    if file.metadata().map_err(|_| ServerStoreError::Io)?.len() > limit {
        return Err(ServerStoreError::Corrupt);
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.checkpoint()?;
        let count = file.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > limit {
            return Err(ServerStoreError::Corrupt);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

fn hash_file(
    path: &Path,
    control: &ContentStageControl,
) -> Result<(u64, String), ServerStoreError> {
    crate::core::server::store::check_regular_file(path)?;
    let mut file = fs::File::open(path).map_err(|_| ServerStoreError::Io)?;
    if file.metadata().map_err(|_| ServerStoreError::Io)?.len() > MAX_SAVE_BYTES {
        return Err(ServerStoreError::Invalid(
            "A Reforger session save exceeds the bounded file size.".into(),
        ));
    }
    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.checkpoint()?;
        let count = file.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or(ServerStoreError::Corrupt)?;
        if bytes > MAX_SAVE_BYTES {
            return Err(ServerStoreError::Invalid(
                "A Reforger session save exceeds the bounded file size.".into(),
            ));
        }
        hasher.update(&buffer[..count]);
    }
    Ok((bytes, hasher.finalize().to_hex().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_uuid_and_location_are_exact() {
        assert!(super::super::valid_uuid(
            "01234567-89ab-cdef-0123-456789abcdef"
        ));
        assert!(!super::super::valid_uuid(
            "0123456789abcdef0123456789abcdef"
        ));
        assert_eq!(
            parse_location(Path::new(
                "scenario/playthrough12/savepoint7/meta-info.json"
            )),
            Ok((12, 7))
        );
        assert_eq!(
            parse_location(Path::new(
                "scenario/playthrough0/savepoint000/meta-info.json"
            )),
            Ok((0, 0))
        );
        assert!(parse_location(Path::new("../playthrough1/savepoint1/meta-info.json")).is_err());
    }

    #[test]
    fn metadata_requires_known_identity_shape_and_preserves_unknown_addon_shapes_as_incompatible() {
        let mut value = serde_json::json!({
            "m_Id": "01234567-89ab-cdef-0123-456789abcdef",
            "m_eType": 1,
            "m_iSavedAtUnix": 100,
            "m_iSavePointNr": 1,
            "m_sSavePointDisplayName": "Save",
            "m_sMissionResource": "{0123456789ABCDEF}Missions/Test.conf",
            "m_iPlaythroughNr": 1,
            "m_sPlaythroughDisplayName": "",
            "m_iStartedUnix": 90,
            "m_iPlaytimeSeconds": 10,
            "m_sGameVersion": NATIVE_SAVE_VERSION,
            "m_aUsedAddons": []
        });
        assert!(
            parse_metadata(&serde_json::to_vec(&value).unwrap(), (1, 1))
                .unwrap()
                .addons_empty
        );
        value["m_aUsedAddons"] = serde_json::json!({"unknown": true});
        assert!(
            !parse_metadata(&serde_json::to_vec(&value).unwrap(), (1, 1))
                .unwrap()
                .addons_empty
        );
        value["m_aUsedAddons"] = serde_json::json!([]);
        value["m_futureField"] = serde_json::json!("opaque");
        assert!(
            parse_metadata(&serde_json::to_vec(&value).unwrap(), (1, 1))
                .unwrap()
                .unknown_fields
        );
        value["m_Id"] = "bad".into();
        assert_eq!(
            parse_metadata(&serde_json::to_vec(&value).unwrap(), (1, 1)).unwrap_err(),
            ServerStoreError::Corrupt
        );
    }

    #[test]
    fn scenario_resource_requires_a_guid_and_conf_path() {
        assert!(valid_scenario_resource(
            "{0123456789ABCDEF}Missions/Test.conf"
        ));
        assert!(valid_scenario_resource(
            "full{0123456789ABCDEF}Missions/Test.conf"
        ));
        assert!(!valid_scenario_resource("Missions/Test.conf"));
        assert!(!valid_scenario_resource("{not-a-guid}Missions/Test.conf"));
        assert!(!valid_scenario_resource("{0123456789ABCDEF}../escape.conf"));
    }

    #[test]
    fn session_compatibility_requires_exact_engine_scenario_and_known_addon_shape() {
        let metadata = NativeMetadata {
            save_id: "01234567-89ab-cdef-0123-456789abcdef".into(),
            save_time: 10,
            scenario: "{0123456789ABCDEF}Missions/Test.conf".into(),
            game_version: NATIVE_SAVE_VERSION.into(),
            playthrough: 1,
            savepoint: 1,
            addons_empty: true,
            unknown_fields: false,
        };
        assert!(session_compatibility_blockers(&metadata, &metadata.scenario, false).is_empty());
        let mut old = metadata.clone();
        old.game_version = "1.6.0.0".into();
        assert!(!session_compatibility_blockers(&old, &metadata.scenario, false).is_empty());
        assert!(!session_compatibility_blockers(&metadata, "another scenario", false).is_empty());
        assert!(!session_compatibility_blockers(&metadata, &metadata.scenario, true).is_empty());
    }

    #[test]
    fn custom_persistence_is_uncovered() {
        assert!(provider_coverage(b"{}").unwrap().local_default);
        assert!(provider_coverage(br#"{"game":{"gameProperties":{"persistence":{"loadSessionSave":true,"autoSaveInterval":60,"saveRetention":4,"hiveId":16383}}}}"#).unwrap().local_default);
        assert!(
            !provider_coverage(
                br#"{"game":{"gameProperties":{"persistence":{"databases":[{"type":"custom"}]}}}}"#
            )
            .unwrap()
            .local_default
        );
        assert!(
            provider_coverage(
                br#"{"game":{"gameProperties":{"persistence":{"saveRetention":0}}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn native_metadata_example_allows_empty_names_and_savepoint_zero() {
        let mut value: Value = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/reforger_session_metadata.json"
        ))
        .unwrap();
        value["m_sPlaythroughDisplayName"] = "".into();
        value["m_sSavePointDisplayName"] = "".into();
        value["m_iSavePointNr"] = 0.into();
        let metadata = parse_metadata(&serde_json::to_vec(&value).unwrap(), (2, 0)).unwrap();
        assert_eq!(metadata.save_id, "01234567-89ab-cdef-0123-456789abcdef");
        assert!(session_compatibility_blockers(&metadata, &metadata.scenario, false).is_empty());
    }

    #[test]
    fn savepoint_fingerprint_covers_all_binary_paths_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("WorldState")).unwrap();
        fs::write(root.path().join("meta-info.json"), b"metadata").unwrap();
        let blob = root
            .path()
            .join("WorldState/11111111-1111-1111-1111-111111111111.blob");
        fs::write(&blob, b"world-one").unwrap();
        let control = ContentStageControl::default();
        let before = hash_savepoint(root.path(), &control).unwrap();
        assert_eq!(before.0, 17);
        assert!(before.2);
        fs::write(&blob, b"world-two").unwrap();
        let changed = hash_savepoint(root.path(), &control).unwrap();
        assert_ne!(before.1, changed.1);
        fs::rename(
            &blob,
            root.path()
                .join("WorldState/22222222-2222-2222-2222-222222222222.blob"),
        )
        .unwrap();
        let renamed = hash_savepoint(root.path(), &control).unwrap();
        assert_ne!(changed.1, renamed.1);
        control.cancel();
        assert_eq!(
            hash_savepoint(root.path(), &control),
            Err(ServerStoreError::Cancelled)
        );
    }
}
