use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::{atomic_write, is_safe_child_path};

pub const EXTRA_FILES_FILE: &str = "extra_files.json";
pub const EXTRA_FILES_DIR: &str = "extra_files";
pub const GAME_DIR_PLACEHOLDER: &str = "{game_dir}";
const ACTIVATION_STATE_FILE: &str = "extra_file_activations.json";

const MAX_COPY_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraFileKind {
    File,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraFileEntry {
    pub id: String,
    pub name: String,
    pub source_name: String,
    pub destination: String,
    pub kind: ExtraFileKind,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ActivationState {
    entries: std::collections::BTreeMap<String, AppliedDestination>,
    #[serde(default)]
    reviewed_layer_signature: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AppliedDestination {
    destination: String,
    content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DestinationConflict {
    pub entry_id: String,
    pub name: String,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LayerConflict {
    pub target: String,
    pub layer_ids: Vec<String>,
    pub layers: Vec<String>,
    pub winner_id: String,
    pub winner: String,
    #[serde(skip)]
    source_hashes: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtraFilesFile {
    #[serde(default)]
    pub entries: Vec<ExtraFileEntry>,
}

impl ExtraFilesFile {
    pub fn entry(&self, id: &str) -> Option<&ExtraFileEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivationSummary {
    pub activated: Vec<(String, String)>,
    pub failed: Vec<(String, String)>,
    pub skipped_disabled: usize,
}

/// Entry ids and payload names become path components under the managed
/// store, so a hand-edited or imported store must never carry one that can
/// escape it.
pub fn validate_entry(entry: &ExtraFileEntry) -> Result<(), String> {
    if !is_safe_child_path(&entry.id) || entry.id.contains(['/', '\\']) {
        return Err(format!("Unsafe extra-file id {}", entry.id));
    }
    if !is_safe_child_path(&entry.source_name) || entry.source_name.contains(['/', '\\']) {
        return Err(format!(
            "Unsafe extra-file payload name {}",
            entry.source_name
        ));
    }
    Ok(())
}

pub fn store_path(space_dir: &Path) -> PathBuf {
    space_dir.join(EXTRA_FILES_FILE)
}

pub fn payload_dir(space_dir: &Path, entry_id: &str) -> PathBuf {
    space_dir.join(EXTRA_FILES_DIR).join(entry_id)
}

pub fn load_store(space_dir: &Path) -> Result<ExtraFilesFile, String> {
    let path = store_path(space_dir);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExtraFilesFile::default());
        }
        Err(err) => {
            return Err(format!(
                "Failed to read {}: {}",
                sanitize_log_path(&path),
                err
            ));
        }
    };
    serde_json::from_str(&raw)
        .map_err(|err| format!("Failed to parse {}: {}", sanitize_log_path(&path), err))
}

pub fn save_store(space_dir: &Path, store: &ExtraFilesFile) -> Result<(), String> {
    let path = store_path(space_dir);
    let serialized = serde_json::to_string_pretty(store)
        .map_err(|err| format!("Failed to serialize extra files: {}", err))?;
    atomic_write(&path, serialized.as_bytes())
        .map_err(|err| format!("Failed to write {}: {}", sanitize_log_path(&path), err))
}

pub fn add_entry(
    space_dir: &Path,
    name: &str,
    source: &Path,
    destination: &str,
    enabled: bool,
) -> Result<ExtraFileEntry, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Extra file name must not be empty".to_string());
    }
    validate_destination(space_dir, destination)?;
    let metadata = fs::metadata(source).map_err(|err| {
        format!(
            "Source {} is not accessible: {}",
            sanitize_log_path(source),
            err
        )
    })?;
    let kind = if metadata.is_dir() {
        ExtraFileKind::Folder
    } else {
        ExtraFileKind::File
    };
    let Some(source_name) = source.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return Err("Source path has no file name".to_string());
    };

    let mut store = load_store(space_dir)?;
    let id = unique_entry_id(space_dir, &store, name);
    let entry_payload_dir = payload_dir(space_dir, &id);
    if entry_payload_dir.exists() {
        fs::remove_dir_all(&entry_payload_dir).map_err(|err| {
            format!(
                "Failed to clear stale payload {}: {}",
                sanitize_log_path(&entry_payload_dir),
                err
            )
        })?;
    }
    fs::create_dir_all(&entry_payload_dir).map_err(|err| {
        format!(
            "Failed to create {}: {}",
            sanitize_log_path(&entry_payload_dir),
            err
        )
    })?;
    copy_recursively(source, &entry_payload_dir.join(&source_name), 0)?;

    let entry = ExtraFileEntry {
        id,
        name: name.to_string(),
        source_name,
        destination: destination.trim().to_string(),
        kind,
        enabled,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    store.entries.push(entry.clone());
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn remove_entry(space_dir: &Path, id: &str) -> Result<ExtraFileEntry, String> {
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry(id)
        .cloned()
        .ok_or_else(|| format!("Extra file {} does not exist", id))?;
    validate_entry(&entry)?;

    let entry_payload_dir = payload_dir(space_dir, &entry.id);
    if let Err(err) = fs::remove_dir_all(&entry_payload_dir)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!(
            "Failed to delete payload {}: {}",
            sanitize_log_path(&entry_payload_dir),
            err
        ));
    }

    store.entries.retain(|candidate| candidate.id != entry.id);
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn set_entry_enabled(
    space_dir: &Path,
    id: &str,
    enabled: bool,
) -> Result<ExtraFileEntry, String> {
    let mut store = load_store(space_dir)?;
    let entry = store
        .entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("Extra file {} does not exist", id))?;
    entry.enabled = enabled;
    let entry = entry.clone();
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn set_entry_order(space_dir: &Path, id: &str, order: usize) -> Result<ExtraFileEntry, String> {
    let mut store = load_store(space_dir)?;
    let current = store
        .entries
        .iter()
        .position(|entry| entry.id == id)
        .ok_or_else(|| format!("Extra file {} does not exist", id))?;
    let entry = store.entries.remove(current);
    let new_index = order.min(store.entries.len());
    store.entries.insert(new_index, entry);
    let entry = store.entries[new_index].clone();
    save_store(space_dir, &store)?;
    Ok(entry)
}

#[cfg(test)]
fn activate_entries(space_dir: &Path, game_dir: &str) -> Result<ActivationSummary, String> {
    let store = load_store(space_dir)?;
    let mut summary = ActivationSummary::default();
    for entry in &store.entries {
        if !entry.enabled {
            summary.skipped_disabled += 1;
            continue;
        }
        match activate_entry(space_dir, entry, game_dir) {
            Ok(destination) => summary.activated.push((entry.name.clone(), destination)),
            Err(err) => summary.failed.push((entry.name.clone(), err)),
        }
    }
    Ok(summary)
}

pub(crate) fn activate_entry(
    space_dir: &Path,
    entry: &ExtraFileEntry,
    game_dir: &str,
) -> Result<String, String> {
    validate_entry(entry)?;
    let destination = resolve_destination(&entry.destination, game_dir)?;
    // Re-checked at activation time because imported packs carry destinations
    // that were never seen by add-time validation.
    if path_is_within(&destination, space_dir)
        || path_is_within(
            &destination,
            &crate::core::utils::app_paths::foxy_data_dir(),
        )
    {
        return Err(
            "Destination points into Foxy's data directory; refusing to overwrite Foxy state"
                .to_string(),
        );
    }
    let source = payload_dir(space_dir, &entry.id).join(&entry.source_name);
    if !source.exists() {
        return Err("Stored payload is missing; re-add the entry".to_string());
    }
    let destination_dir = match entry.kind {
        ExtraFileKind::File => destination.parent().unwrap_or(&destination).to_path_buf(),
        ExtraFileKind::Folder => destination.clone(),
    };
    let result = match entry.kind {
        ExtraFileKind::File => fs::create_dir_all(&destination_dir)
            .map_err(|err| {
                format!(
                    "Failed to create {}: {}",
                    sanitize_log_path(&destination_dir),
                    err
                )
            })
            .and_then(|()| copy_recursively(&source, &destination, 0)),
        ExtraFileKind::Folder => copy_recursively(&source, &destination, 0),
    };
    result.map_err(|err| explain_unwritable_destination(&destination_dir, err))?;
    Ok(destination.display().to_string())
}

fn hash_extra_target(path: &Path) -> Result<String, std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "Extra-file evidence cannot follow links",
        ));
    }
    if metadata.is_file() {
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader(fs::File::open(path)?)?;
        Ok(hasher.finalize().to_hex().to_string())
    } else {
        crate::core::utils::content_hash::calculate_addon_folder_content_hash(path)
    }
}

pub(crate) fn destination_conflicts(
    space_dir: &Path,
    game_dir: &str,
) -> Result<Vec<DestinationConflict>, String> {
    let state_path = space_dir.join(ACTIVATION_STATE_FILE);
    let state: ActivationState = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "Failed to parse {}: {error}",
                sanitize_log_path(&state_path)
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Failed to read {}: {error}",
                sanitize_log_path(&state_path)
            ));
        }
    };
    let store = load_store(space_dir)?;
    let mut conflicts = Vec::new();
    for entry in store.entries.iter().filter(|entry| entry.enabled) {
        let Some(previous) = state.entries.get(&entry.id) else {
            continue;
        };
        let destination = resolve_destination(&entry.destination, game_dir)?;
        if previous.destination != destination.to_string_lossy() || !destination.exists() {
            continue;
        }
        let current_hash = hash_extra_target(&destination).map_err(|error| {
            format!(
                "Failed to inspect {}: {error}",
                sanitize_log_path(&destination)
            )
        })?;
        if current_hash != previous.content_hash {
            conflicts.push(DestinationConflict {
                entry_id: entry.id.clone(),
                name: entry.name.clone(),
                destination: destination.display().to_string(),
            });
        }
    }
    Ok(conflicts)
}

pub(crate) fn save_activation_evidence(
    space_dir: &Path,
    entries: &[ExtraFileEntry],
    game_dir: &str,
) -> Result<(), String> {
    let path = space_dir.join(ACTIVATION_STATE_FILE);
    let mut state: ActivationState = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ActivationState::default(),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let destination = resolve_destination(&entry.destination, game_dir)?;
        let content_hash = hash_extra_target(&destination).map_err(|error| {
            format!(
                "Failed to inspect {}: {error}",
                sanitize_log_path(&destination)
            )
        })?;
        state.entries.insert(
            entry.id.clone(),
            AppliedDestination {
                destination: destination.to_string_lossy().into_owned(),
                content_hash,
            },
        );
    }
    let serialized = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    atomic_write(&path, &serialized).map_err(|error| error.to_string())
}

pub(crate) fn layer_conflicts(
    space_dir: &Path,
    game_dir: &str,
) -> Result<Vec<LayerConflict>, String> {
    fn collect(
        source: &Path,
        target: &Path,
        entry: &ExtraFileEntry,
        files: &mut std::collections::BTreeMap<String, Vec<(String, String, String)>>,
    ) -> Result<(), String> {
        let metadata = match fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_symlink() {
            return Err("A managed extra-file source contains a link".to_string());
        }
        if metadata.is_dir() {
            for child in fs::read_dir(source).map_err(|error| error.to_string())? {
                let child = child.map_err(|error| error.to_string())?;
                collect(&child.path(), &target.join(child.file_name()), entry, files)?;
            }
        } else if metadata.is_file() {
            let hash = hash_extra_target(source).map_err(|error| error.to_string())?;
            let key = normalized_path_text(target);
            files
                .entry(key)
                .or_default()
                .push((entry.id.clone(), entry.name.clone(), hash));
        }
        Ok(())
    }

    let store = load_store(space_dir)?;
    let mut files = std::collections::BTreeMap::new();
    for entry in store.entries.iter().filter(|entry| entry.enabled) {
        validate_entry(entry)?;
        let destination = resolve_destination(&entry.destination, game_dir)?;
        let source = payload_dir(space_dir, &entry.id).join(&entry.source_name);
        match entry.kind {
            ExtraFileKind::File => collect(&source, &destination, entry, &mut files)?,
            ExtraFileKind::Folder => collect(&source, &destination, entry, &mut files)?,
        }
    }
    Ok(files
        .into_iter()
        .filter_map(|(target, layers)| {
            let distinct: std::collections::BTreeSet<_> =
                layers.iter().map(|layer| &layer.2).collect();
            if distinct.len() < 2 {
                return None;
            }
            let names = layers
                .iter()
                .map(|layer| layer.1.clone())
                .collect::<Vec<_>>();
            Some(LayerConflict {
                target,
                layer_ids: layers.iter().map(|layer| layer.0.clone()).collect(),
                winner_id: layers
                    .last()
                    .map(|layer| layer.0.clone())
                    .unwrap_or_default(),
                winner: names.last().cloned().unwrap_or_default(),
                layers: names,
                source_hashes: layers.iter().map(|layer| layer.2.clone()).collect(),
            })
        })
        .collect())
}

pub(crate) fn layer_review_needed(
    space_dir: &Path,
    conflicts: &[LayerConflict],
) -> Result<bool, String> {
    if conflicts.is_empty() {
        return Ok(false);
    }
    let signature = layer_review_signature(conflicts);
    let path = space_dir.join(ACTIVATION_STATE_FILE);
    let state: ActivationState = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ActivationState::default(),
        Err(error) => return Err(error.to_string()),
    };
    Ok(state.reviewed_layer_signature.as_deref() != Some(&signature))
}

pub(crate) fn record_layer_review(
    space_dir: &Path,
    conflicts: &[LayerConflict],
) -> Result<(), String> {
    let path = space_dir.join(ACTIVATION_STATE_FILE);
    let mut state: ActivationState = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ActivationState::default(),
        Err(error) => return Err(error.to_string()),
    };
    state.reviewed_layer_signature = Some(layer_review_signature(conflicts));
    atomic_write(
        &path,
        &serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn layer_review_signature(conflicts: &[LayerConflict]) -> String {
    let mut hasher = blake3::Hasher::new();
    for conflict in conflicts {
        for value in std::iter::once(conflict.target.as_str())
            .chain(conflict.layer_ids.iter().map(String::as_str))
            .chain(conflict.layers.iter().map(String::as_str))
            .chain(conflict.source_hashes.iter().map(String::as_str))
            .chain([conflict.winner_id.as_str(), conflict.winner.as_str()])
        {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    hasher.finalize().to_hex().to_string()
}

/// Turn a raw OS write failure into something the user can act on when the
/// cause is that the destination simply is not writable by this account. Only
/// probed on the failure path so a normal launch does no extra filesystem work.
fn explain_unwritable_destination(destination_dir: &Path, err: String) -> String {
    if crate::core::utils::fs_safety::destination_is_writable(destination_dir) {
        return err;
    }
    format!(
        "{} is not writable by your account, so the file was not applied. Move the game out of a \
         protected folder, or grant your account write access to it. ({})",
        sanitize_log_path(destination_dir),
        err
    )
}

/// Expand `{game_dir}` and require an absolute result so a pack made on
/// another machine can never scribble relative to the working directory.
///
/// `..` is rejected outright rather than resolved: the containment checks below
/// compare path text, and a destination that walks out of a guarded root and
/// back in would otherwise slip past them.
pub fn resolve_destination(destination: &str, game_dir: &str) -> Result<PathBuf, String> {
    let destination = destination.trim();
    if destination.is_empty() {
        return Err("Destination is empty".to_string());
    }
    let resolved = if destination.contains(GAME_DIR_PLACEHOLDER) {
        let game_dir = game_dir.trim();
        if game_dir.is_empty() {
            return Err(
                "Destination uses {game_dir} but the game directory is not configured".to_string(),
            );
        }
        destination.replace(GAME_DIR_PLACEHOLDER, game_dir)
    } else {
        destination.to_string()
    };
    reject_parent_traversal(&resolved)?;
    let path = PathBuf::from(&resolved);
    if !path.is_absolute() {
        return Err(format!("Destination {} is not an absolute path", resolved));
    }
    Ok(path)
}

/// Reject any `..` path component. Checked on the raw text (not on a
/// canonicalized path) because the destination usually does not exist yet.
fn reject_parent_traversal(path: &str) -> Result<(), String> {
    if path.split(['/', '\\']).any(|component| component == "..") {
        return Err("Destination must not contain a `..` path component".to_string());
    }
    Ok(())
}

fn validate_destination(space_dir: &Path, destination: &str) -> Result<(), String> {
    let destination = destination.trim();
    if destination.is_empty() {
        return Err("Destination must not be empty".to_string());
    }
    reject_parent_traversal(destination)?;
    if !destination.contains(GAME_DIR_PLACEHOLDER) && !Path::new(destination).is_absolute() {
        return Err(format!(
            "Destination must be an absolute path or start with {}",
            GAME_DIR_PLACEHOLDER
        ));
    }
    // Guard against activation overwriting Foxy's own state (the store, the
    // database, other game-space files).
    if path_is_within(Path::new(destination), space_dir) {
        return Err("Destination must not point into Foxy's game space data directory".to_string());
    }
    Ok(())
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = normalized_path_text(path);
    let root = normalized_path_text(root);
    !root.is_empty() && (path == root || path.starts_with(&format!("{}/", root)))
}

fn normalized_path_text(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/').to_string();
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

fn unique_entry_id(space_dir: &Path, store: &ExtraFilesFile, name: &str) -> String {
    let base = super::spaces::slug_from_display_name(name);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while store.entry(&candidate).is_some() || payload_dir(space_dir, &candidate).exists() {
        candidate = format!("{}-{}", base, suffix);
        suffix += 1;
    }
    candidate
}

fn copy_recursively(source: &Path, destination: &Path, depth: usize) -> Result<(), String> {
    if depth > MAX_COPY_DEPTH {
        return Err(format!(
            "Copy exceeded the maximum folder depth at {}",
            sanitize_log_path(source)
        ));
    }
    let metadata = fs::metadata(source)
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|err| {
            format!(
                "Failed to create {}: {}",
                sanitize_log_path(destination),
                err
            )
        })?;
        let entries = fs::read_dir(source)
            .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
        for dir_entry in entries {
            let dir_entry = dir_entry
                .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
            copy_recursively(
                &dir_entry.path(),
                &destination.join(dir_entry.file_name()),
                depth + 1,
            )?;
        }
    } else {
        fs::copy(source, destination).map_err(|err| {
            format!(
                "Failed to copy {} to {}: {}",
                sanitize_log_path(source),
                sanitize_log_path(destination),
                err
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_dest(dir: &Path, name: &str) -> String {
        dir.join(name).display().to_string()
    }

    #[test]
    fn add_entry_copies_payload_and_registers_it() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("server.cfg");
        std::fs::write(&source, "cfg-body").expect("source file");

        let entry = add_entry(
            space.path(),
            "Server Config",
            &source,
            &absolute_dest(work.path(), "target/server.cfg"),
            true,
        )
        .expect("add");

        assert_eq!(entry.id, "server-config");
        assert_eq!(entry.kind, ExtraFileKind::File);
        assert_eq!(entry.source_name, "server.cfg");
        let stored = payload_dir(space.path(), &entry.id).join("server.cfg");
        assert_eq!(std::fs::read_to_string(stored).expect("stored"), "cfg-body");
        let store = load_store(space.path()).expect("store");
        assert_eq!(store.entries.len(), 1);

        let second = add_entry(
            space.path(),
            "Server Config",
            &source,
            &absolute_dest(work.path(), "target2/server.cfg"),
            true,
        )
        .expect("add second");
        assert_eq!(second.id, "server-config-2");
    }

    #[test]
    fn changed_successful_destination_is_detected_as_a_conflict() {
        let space = tempfile::tempdir().expect("space");
        let work = tempfile::tempdir().expect("work");
        let source = work.path().join("config.cfg");
        let destination = work.path().join("game/config.cfg");
        fs::write(&source, b"template").expect("source");
        let entry = add_entry(
            space.path(),
            "Config",
            &source,
            &destination.display().to_string(),
            true,
        )
        .expect("entry");
        fs::create_dir_all(destination.parent().unwrap()).expect("destination dir");
        fs::copy(&source, &destination).expect("first activation");
        save_activation_evidence(space.path(), &[entry], "").expect("evidence");

        fs::write(&destination, b"user edit").expect("user edit");
        let conflicts = destination_conflicts(space.path(), "").expect("conflicts");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].entry_id, "config");
        assert_eq!(conflicts[0].destination, destination.display().to_string());
    }

    #[test]
    fn conflicting_layers_keep_order_and_require_review_after_reorder() {
        let space = tempfile::tempdir().expect("space");
        let work = tempfile::tempdir().expect("work");
        let first = work.path().join("first.cfg");
        let second = work.path().join("second.cfg");
        fs::write(&first, b"first").expect("first");
        fs::write(&second, b"second").expect("second");
        let destination = work.path().join("game/config.cfg").display().to_string();
        add_entry(space.path(), "First", &first, &destination, true).expect("first entry");
        add_entry(space.path(), "Second", &second, &destination, true).expect("second entry");

        let conflicts = layer_conflicts(space.path(), "").expect("preview");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].layers, ["First", "Second"]);
        assert_eq!(conflicts[0].winner, "Second");
        assert_eq!(conflicts[0].winner_id, "second");
        let approved_signature = layer_review_signature(&conflicts);
        assert!(layer_review_needed(space.path(), &conflicts).expect("needs review"));
        record_layer_review(space.path(), &conflicts).expect("record review");
        assert!(!layer_review_needed(space.path(), &conflicts).expect("reviewed"));

        let mut store = load_store(space.path()).expect("store");
        store.entries.reverse();
        save_store(space.path(), &store).expect("reorder");
        let reordered = layer_conflicts(space.path(), "").expect("reordered preview");
        assert_eq!(reordered[0].winner, "First");
        assert_ne!(layer_review_signature(&reordered), approved_signature);
        assert!(layer_review_needed(space.path(), &reordered).expect("review order change"));

        fs::write(
            payload_dir(space.path(), "first").join("first.cfg"),
            b"changed source",
        )
        .expect("change source");
        let changed_source = layer_conflicts(space.path(), "").expect("changed source preview");
        assert_ne!(
            layer_review_signature(&changed_source),
            layer_review_signature(&reordered)
        );
    }

    #[test]
    fn add_entry_validates_name_source_and_destination() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "x").expect("source");

        assert!(add_entry(space.path(), "  ", &source, "C:/x", true).is_err());
        assert!(
            add_entry(
                space.path(),
                "Missing",
                &work.path().join("absent.cfg"),
                &absolute_dest(work.path(), "x"),
                true
            )
            .is_err()
        );
        assert!(add_entry(space.path(), "Relative", &source, "relative/path", true).is_err());
        let store_dest = payload_dir(space.path(), "self").display().to_string();
        assert!(add_entry(space.path(), "Loop", &source, &store_dest, true).is_err());
        // {game_dir} destinations are accepted without being absolute yet.
        assert!(
            add_entry(
                space.path(),
                "Portable",
                &source,
                "{game_dir}/userconfig/a.cfg",
                true
            )
            .is_ok()
        );
    }

    /// `path_is_within` compares path text, so a destination that walks out of
    /// a guarded root and back in would defeat both containment checks. `..` is
    /// therefore refused at add time and again at activation time (imported
    /// packs never went through add-time validation).
    #[test]
    fn parent_traversal_destinations_are_refused() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "x").expect("source");

        let escape = space
            .path()
            .join("..")
            .join(space.path().file_name().expect("space name"))
            .join("database.db")
            .display()
            .to_string();

        assert!(add_entry(space.path(), "Escape", &source, &escape, true).is_err());
        assert!(resolve_destination(&escape, "").is_err());
        assert!(resolve_destination("{game_dir}/../../evil.cfg", "C:/Games/X").is_err());
        // The same shape without `..` is still accepted.
        assert!(resolve_destination("{game_dir}/userconfig/a.cfg", "C:/Games/X").is_ok());
    }

    #[test]
    fn activation_refuses_a_traversal_destination_from_an_imported_entry() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "payload").expect("source");
        let entry = add_entry(
            space.path(),
            "Config",
            &source,
            &absolute_dest(work.path(), "ok.cfg"),
            true,
        )
        .expect("add");

        // Simulate an imported pack rewriting the destination after add-time
        // validation already ran.
        let mut store = load_store(space.path()).expect("store");
        store.entries[0].destination = work
            .path()
            .join("..")
            .join("escaped.cfg")
            .display()
            .to_string();
        save_store(space.path(), &store).expect("save");

        let summary = activate_entries(space.path(), "").expect("activate");

        assert!(summary.activated.is_empty());
        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].0, entry.name);
        assert!(summary.failed[0].1.contains(".."));
    }

    #[test]
    fn remove_entry_deletes_payload_and_registration() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "x").expect("source");
        let entry = add_entry(
            space.path(),
            "A",
            &source,
            &absolute_dest(work.path(), "out/a.cfg"),
            true,
        )
        .expect("add");

        remove_entry(space.path(), &entry.id).expect("remove");

        assert!(!payload_dir(space.path(), &entry.id).exists());
        assert!(load_store(space.path()).expect("store").entries.is_empty());
        assert!(remove_entry(space.path(), &entry.id).is_err());
    }

    #[test]
    fn activate_entries_copies_files_and_folders_and_skips_disabled() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let file_source = work.path().join("solo.cfg");
        std::fs::write(&file_source, "solo").expect("file source");
        let folder_source = work.path().join("userconfig");
        std::fs::create_dir_all(folder_source.join("nested")).expect("folder source");
        std::fs::write(folder_source.join("nested").join("deep.hpp"), "deep").expect("nested");
        let disabled_source = work.path().join("off.cfg");
        std::fs::write(&disabled_source, "off").expect("disabled source");

        let game_dir = work.path().join("game");
        std::fs::create_dir_all(&game_dir).expect("game dir");
        add_entry(
            space.path(),
            "Solo",
            &file_source,
            "{game_dir}/configs/solo.cfg",
            true,
        )
        .expect("add file");
        add_entry(
            space.path(),
            "Userconfig",
            &folder_source,
            "{game_dir}/userconfig",
            true,
        )
        .expect("add folder");
        add_entry(
            space.path(),
            "Off",
            &disabled_source,
            "{game_dir}/off.cfg",
            false,
        )
        .expect("add disabled");

        let summary =
            activate_entries(space.path(), &game_dir.display().to_string()).expect("activate");

        assert_eq!(summary.activated.len(), 2);
        assert!(summary.failed.is_empty());
        assert_eq!(summary.skipped_disabled, 1);
        assert_eq!(
            std::fs::read_to_string(game_dir.join("configs").join("solo.cfg")).expect("solo"),
            "solo"
        );
        assert_eq!(
            std::fs::read_to_string(game_dir.join("userconfig").join("nested").join("deep.hpp"))
                .expect("deep"),
            "deep"
        );
        assert!(!game_dir.join("off.cfg").exists());
    }

    #[test]
    fn activate_entries_collects_failures_without_blocking_the_rest() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "x").expect("source");
        add_entry(
            space.path(),
            "Needs Game Dir",
            &source,
            "{game_dir}/a.cfg",
            true,
        )
        .expect("add");
        add_entry(
            space.path(),
            "Works",
            &source,
            &work.path().join("out/b.cfg").display().to_string(),
            true,
        )
        .expect("add second");

        let summary = activate_entries(space.path(), "").expect("activate");

        assert_eq!(summary.failed.len(), 1);
        assert_eq!(summary.failed[0].0, "Needs Game Dir");
        assert_eq!(summary.activated.len(), 1);
        assert!(work.path().join("out").join("b.cfg").is_file());
    }

    #[test]
    fn remove_entry_refuses_unsafe_store_ids_before_any_deletion() {
        let space = tempfile::tempdir().expect("space dir");
        let store = ExtraFilesFile {
            entries: vec![ExtraFileEntry {
                id: "..".to_string(),
                name: "Escape".to_string(),
                source_name: "x".to_string(),
                destination: "C:/somewhere/x".to_string(),
                kind: ExtraFileKind::File,
                enabled: true,
                created_at: 0,
            }],
        };
        save_store(space.path(), &store).expect("save");
        std::fs::write(space.path().join("marker.txt"), "keep").expect("marker");

        let err = remove_entry(space.path(), "..").expect_err("unsafe id must be rejected");

        assert!(err.contains("Unsafe extra-file id"));
        assert!(space.path().join("marker.txt").is_file());
        assert!(store_path(space.path()).is_file());
    }

    #[test]
    fn activation_refuses_destinations_inside_the_game_space() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "x").expect("source");
        let inside = space.path().join("database.db").display().to_string();
        assert!(add_entry(space.path(), "Bad", &source, &inside, true).is_err());

        // A store that carries such a destination anyway (e.g. from an
        // imported pack) must fail activation instead of overwriting state.
        let store = ExtraFilesFile {
            entries: vec![ExtraFileEntry {
                id: "bad".to_string(),
                name: "Bad".to_string(),
                source_name: "a.cfg".to_string(),
                destination: inside,
                kind: ExtraFileKind::File,
                enabled: true,
                created_at: 0,
            }],
        };
        save_store(space.path(), &store).expect("save");
        let payload = payload_dir(space.path(), "bad");
        std::fs::create_dir_all(&payload).expect("payload dir");
        std::fs::write(payload.join("a.cfg"), "x").expect("payload");

        let summary = activate_entries(space.path(), "").expect("activate");

        assert_eq!(summary.failed.len(), 1);
        assert!(!space.path().join("database.db").exists());
    }

    #[test]
    fn resolve_destination_requires_absolute_results() {
        assert!(resolve_destination("", "C:/game").is_err());
        assert!(resolve_destination("relative/x", "C:/game").is_err());
        assert!(resolve_destination("{game_dir}/x", "").is_err());
        let resolved = resolve_destination("{game_dir}/userconfig", "C:/game").expect("resolve");
        assert_eq!(resolved, PathBuf::from("C:/game/userconfig"));
    }

    #[test]
    fn set_entry_enabled_persists_the_flag() {
        let space = tempfile::tempdir().expect("space dir");
        let work = tempfile::tempdir().expect("work dir");
        let source = work.path().join("a.cfg");
        std::fs::write(&source, "x").expect("source");
        let entry = add_entry(
            space.path(),
            "A",
            &source,
            &work.path().join("out/a.cfg").display().to_string(),
            true,
        )
        .expect("add");

        set_entry_enabled(space.path(), &entry.id, false).expect("disable");

        let store = load_store(space.path()).expect("store");
        assert!(!store.entries[0].enabled);
        assert!(set_entry_enabled(space.path(), "missing", true).is_err());
    }
}
