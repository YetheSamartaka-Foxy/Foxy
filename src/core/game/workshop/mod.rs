use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::steam;
use crate::core::utils::addon_backup;
use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::{atomic_write, is_safe_child_path};

pub const WORKSHOP_FILE: &str = "workshop.json";
pub const WORKSHOP_DIR: &str = "workshop";
pub const FROZEN_DIR: &str = "frozen";
pub const STEAM_SOURCE: &str = "steam";
pub const STEAM_WORKSHOP_URL_PREFIX: &str =
    "https://steamcommunity.com/sharedfiles/filedetails/?id=";
const WORKSHOP_SCHEMA_VERSION: u32 = 1;
const MAX_WEB_API_IDS_PER_REQUEST: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkshopFile {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<SteamWorkshopItem>,
}

fn default_schema_version() -> u32 {
    WORKSHOP_SCHEMA_VERSION
}

impl WorkshopFile {
    pub fn entry(&self, app_id: u32, item_id: &str) -> Option<&SteamWorkshopItem> {
        self.entries
            .iter()
            .find(|entry| entry.app_id == app_id && entry.item_id == item_id)
    }

    pub fn entry_mut(&mut self, app_id: u32, item_id: &str) -> Option<&mut SteamWorkshopItem> {
        self.entries
            .iter_mut()
            .find(|entry| entry.app_id == app_id && entry.item_id == item_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamWorkshopItem {
    #[serde(default = "default_source")]
    pub source: String,
    pub app_id: u32,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub url: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_updated: Option<u64>,
    #[serde(default)]
    pub added_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

fn default_source() -> String {
    STEAM_SOURCE.to_string()
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkshopMetadata {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_updated: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertResult {
    pub item: SteamWorkshopItem,
    pub added: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveSummary {
    pub item: SteamWorkshopItem,
    pub deleted_content_path: Option<String>,
    pub deleted_frozen_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeSummary {
    pub item_id: String,
    pub source_path: String,
    pub frozen_path: String,
    pub content_hash: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPathResolution {
    pub item_id: String,
    pub path: String,
    pub frozen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamHelperOutcome {
    pub app_id: u32,
    pub item_id: String,
    #[serde(default)]
    pub subscribed: bool,
    #[serde(default)]
    pub download_started: bool,
    #[serde(default)]
    pub installed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamCmdOutcome {
    pub app_id: u32,
    pub item_id: String,
    pub status_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub fn store_path(space_dir: &Path) -> PathBuf {
    space_dir.join(WORKSHOP_FILE)
}

pub fn workshop_root(space_dir: &Path) -> PathBuf {
    space_dir.join(WORKSHOP_DIR)
}

pub fn frozen_item_root(space_dir: &Path, item_id: &str) -> PathBuf {
    workshop_root(space_dir).join(FROZEN_DIR).join(item_id)
}

pub fn load_store(space_dir: &Path) -> Result<WorkshopFile, String> {
    let path = store_path(space_dir);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorkshopFile {
                schema_version: WORKSHOP_SCHEMA_VERSION,
                entries: Vec::new(),
            });
        }
        Err(err) => {
            return Err(format!(
                "Failed to read {}: {}",
                sanitize_log_path(&path),
                err
            ));
        }
    };
    let mut store: WorkshopFile = serde_json::from_str(&raw)
        .map_err(|err| format!("Failed to parse {}: {}", sanitize_log_path(&path), err))?;
    if store.schema_version == 0 {
        store.schema_version = WORKSHOP_SCHEMA_VERSION;
    }
    if store.schema_version != WORKSHOP_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported workshop schema version {}",
            store.schema_version
        ));
    }
    Ok(store)
}

pub fn save_store(space_dir: &Path, store: &WorkshopFile) -> Result<(), String> {
    let mut store = store.clone();
    store.schema_version = WORKSHOP_SCHEMA_VERSION;
    store.entries.sort_by(|a, b| {
        a.app_id
            .cmp(&b.app_id)
            .then_with(|| numeric_id_key(&a.item_id).cmp(&numeric_id_key(&b.item_id)))
    });
    let path = store_path(space_dir);
    let serialized = serde_json::to_string_pretty(&store)
        .map_err(|err| format!("Failed to serialize workshop store: {}", err))?;
    atomic_write(&path, serialized.as_bytes())
        .map_err(|err| format!("Failed to write {}: {}", sanitize_log_path(&path), err))
}

pub fn upsert_item(
    space_dir: &Path,
    app_id: u32,
    item_id: &str,
    title: Option<String>,
    metadata: Option<&WorkshopMetadata>,
    helper: Option<&SteamHelperOutcome>,
    enabled: bool,
) -> Result<UpsertResult, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let now = unix_timestamp_now();
    let mut store = load_store(space_dir)?;
    let added = store.entry(app_id, &item_id).is_none();
    let existing = store.entry_mut(app_id, &item_id);
    let item = match existing {
        Some(entry) => {
            entry.source = STEAM_SOURCE.to_string();
            entry.url = workshop_url(&item_id);
            entry.enabled = enabled;
            if let Some(title) = non_empty(title) {
                entry.title = Some(title);
            } else if let Some(title) = metadata.and_then(|meta| meta.title.clone()) {
                entry.title = Some(title);
            }
            if let Some(meta) = metadata {
                entry.size_bytes = meta.file_size.or(entry.size_bytes);
                entry.time_updated = meta.time_updated.or(entry.time_updated);
            }
            if let Some(helper) = helper {
                entry.installed_path = helper
                    .installed_path
                    .clone()
                    .or(entry.installed_path.clone());
                entry.size_bytes = helper.size_bytes.or(entry.size_bytes);
            }
            entry.updated_at = now;
            entry.clone()
        }
        None => {
            let meta_title = metadata.and_then(|meta| meta.title.clone());
            let size_bytes = helper
                .and_then(|value| value.size_bytes)
                .or_else(|| metadata.and_then(|meta| meta.file_size));
            let item = SteamWorkshopItem {
                source: STEAM_SOURCE.to_string(),
                app_id,
                item_id: item_id.clone(),
                title: non_empty(title).or(meta_title),
                url: workshop_url(&item_id),
                enabled,
                frozen: false,
                version: metadata
                    .and_then(|meta| meta.time_updated)
                    .map(|value| value.to_string()),
                installed_path: helper.and_then(|value| value.installed_path.clone()),
                frozen_path: None,
                size_bytes,
                time_updated: metadata.and_then(|meta| meta.time_updated),
                added_at: now,
                updated_at: now,
            };
            store.entries.push(item.clone());
            item
        }
    };
    save_store(space_dir, &store)?;
    Ok(UpsertResult { item, added })
}

pub fn upsert_imported_item(
    space_dir: &Path,
    item: SteamWorkshopItem,
) -> Result<UpsertResult, String> {
    let item_id = normalize_workshop_id(&item.item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item.item_id))?;
    let mut store = load_store(space_dir)?;
    let now = unix_timestamp_now();
    let added = store.entry(item.app_id, &item_id).is_none();
    let mut imported = item;
    imported.source = STEAM_SOURCE.to_string();
    imported.item_id = item_id;
    imported.url = workshop_url(&imported.item_id);
    // Packs never carry frozen payloads, so an imported entry cannot claim a
    // frozen copy it does not have.
    imported.frozen = false;
    imported.frozen_path = None;
    if imported.added_at == 0 {
        imported.added_at = now;
    }
    imported.updated_at = now;
    if let Some(existing) = store.entry_mut(imported.app_id, &imported.item_id) {
        *existing = imported.clone();
    } else {
        store.entries.push(imported.clone());
    }
    save_store(space_dir, &store)?;
    Ok(UpsertResult {
        item: imported,
        added,
    })
}

pub fn set_item_enabled(
    space_dir: &Path,
    app_id: u32,
    item_id: &str,
    enabled: bool,
) -> Result<SteamWorkshopItem, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry_mut(app_id, &item_id)
        .ok_or_else(|| format!("Steam Workshop item {} is not managed", item_id))?;
    entry.enabled = enabled;
    entry.updated_at = unix_timestamp_now();
    let entry = entry.clone();
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn remove_item(
    space_dir: &Path,
    app_id: u32,
    item_id: &str,
    steam_directory: &str,
    delete_data: bool,
) -> Result<RemoveSummary, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let mut store = load_store(space_dir)?;
    let item = store
        .entry(app_id, &item_id)
        .cloned()
        .ok_or_else(|| format!("Steam Workshop item {} is not managed", item_id))?;

    let mut deleted_content_path = None;
    let mut deleted_frozen_path = None;
    if delete_data {
        if let Some(content_path) = resolve_installed_path(&item, steam_directory)
            && content_path.exists()
        {
            // The stored installed_path can come from imports or stale data;
            // only delete a folder that is provably this item's Steam Workshop
            // content directory.
            if !is_steam_workshop_item_dir(&content_path, app_id, &item_id) {
                return Err(format!(
                    "Refusing to delete {}: it is not a steamapps/workshop/content/{}/{} directory. Remove the folder manually if intended.",
                    sanitize_log_path(&content_path),
                    app_id,
                    item_id
                ));
            }
            fs::remove_dir_all(&content_path).map_err(|err| {
                format!(
                    "Failed to delete {}: {}",
                    sanitize_log_path(&content_path),
                    err
                )
            })?;
            deleted_content_path = Some(content_path.display().to_string());
        }
        let frozen_root = frozen_item_root(space_dir, &item_id);
        if frozen_root.exists() {
            fs::remove_dir_all(&frozen_root).map_err(|err| {
                format!(
                    "Failed to delete {}: {}",
                    sanitize_log_path(&frozen_root),
                    err
                )
            })?;
            deleted_frozen_path = Some(frozen_root.display().to_string());
        }
    }

    store
        .entries
        .retain(|entry| !(entry.app_id == app_id && entry.item_id == item_id));
    save_store(space_dir, &store)?;
    Ok(RemoveSummary {
        item,
        deleted_content_path,
        deleted_frozen_path,
    })
}

pub fn freeze_item(
    space_dir: &Path,
    app_id: u32,
    item_id: &str,
    steam_directory: &str,
) -> Result<FreezeSummary, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry(app_id, &item_id)
        .cloned()
        .ok_or_else(|| format!("Steam Workshop item {} is not managed", item_id))?;
    let source = resolve_installed_path(&entry, steam_directory).ok_or_else(|| {
        format!(
            "Steam Workshop item {} is not installed under any known Steam library",
            item_id
        )
    })?;
    if !source.is_dir() {
        return Err(format!(
            "Steam Workshop item {} has no readable folder at {}",
            item_id,
            sanitize_log_path(&source)
        ));
    }

    let backup_root = frozen_item_root(space_dir, &item_id);
    let record = addon_backup::backup_addon(&backup_root, &source)
        .map_err(|err| format!("Failed to freeze Steam Workshop item {}: {}", item_id, err))?;
    let now = unix_timestamp_now();
    let entry = store
        .entry_mut(app_id, &item_id)
        .ok_or_else(|| format!("Steam Workshop item {} is not managed", item_id))?;
    entry.frozen = true;
    entry.frozen_path = Some(record.path.display().to_string());
    entry.version = Some(record.content_hash.clone());
    entry.size_bytes = Some(record.size_bytes);
    entry.updated_at = now;
    let summary = FreezeSummary {
        item_id,
        source_path: source.display().to_string(),
        frozen_path: record.path.display().to_string(),
        content_hash: record.content_hash,
        size_bytes: record.size_bytes,
    };
    save_store(space_dir, &store)?;
    Ok(summary)
}

pub fn unfreeze_item(
    space_dir: &Path,
    app_id: u32,
    item_id: &str,
) -> Result<SteamWorkshopItem, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry_mut(app_id, &item_id)
        .ok_or_else(|| format!("Steam Workshop item {} is not managed", item_id))?;
    entry.frozen = false;
    entry.updated_at = unix_timestamp_now();
    let entry = entry.clone();
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn resolve_launch_path(
    space_dir: &Path,
    app_id: u32,
    item_id: &str,
    steam_directory: &str,
) -> Result<LaunchPathResolution, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let store = load_store(space_dir)?;
    let entry = store
        .entry(app_id, &item_id)
        .ok_or_else(|| format!("Steam Workshop item {} is not managed", item_id))?;
    if entry.frozen
        && let Some(path) = entry.frozen_path.as_deref().map(PathBuf::from)
        && path.is_dir()
    {
        return Ok(LaunchPathResolution {
            item_id,
            path: path.display().to_string(),
            frozen: true,
        });
    }
    let path = resolve_installed_path(entry, steam_directory).ok_or_else(|| {
        format!(
            "Steam Workshop item {} is not installed under any known Steam library",
            item_id
        )
    })?;
    Ok(LaunchPathResolution {
        item_id,
        path: path.display().to_string(),
        frozen: false,
    })
}

pub fn resolve_launch_path_override_for_path(
    space_dir: &Path,
    app_id: u32,
    path: &str,
) -> Option<PathBuf> {
    let item_id = workshop_item_id_from_path(path, app_id)?;
    let store = load_store(space_dir).ok()?;
    let item = store.entry(app_id, &item_id)?;
    if !item.frozen {
        return None;
    }
    let frozen_path = item.frozen_path.as_deref().map(PathBuf::from)?;
    frozen_path.is_dir().then_some(frozen_path)
}

pub fn resolve_installed_path(item: &SteamWorkshopItem, steam_directory: &str) -> Option<PathBuf> {
    if let Some(path) = item.installed_path.as_deref().map(PathBuf::from)
        && path.is_dir()
    {
        return Some(path);
    }
    workshop_content_path(steam_directory, item.app_id, &item.item_id)
}

pub fn workshop_content_path(steam_directory: &str, app_id: u32, item_id: &str) -> Option<PathBuf> {
    let item_id = normalize_workshop_id(item_id)?;
    for library_root in steam::steam_library_roots(steam_directory) {
        let candidate = library_root
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join(app_id.to_string())
            .join(&item_id);
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// Deletion-grade check that `path` is exactly a
/// `steamapps/workshop/content/<app_id>/<item_id>` directory. Stricter than
/// `workshop_item_id_from_path`, which also accepts looser layouts for launch
/// resolution.
fn is_steam_workshop_item_dir(path: &Path, app_id: u32, item_id: &str) -> bool {
    let components: Vec<&str> = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    let Some(tail) = components.len().checked_sub(5) else {
        return false;
    };
    let tail = &components[tail..];
    tail[0].eq_ignore_ascii_case("steamapps")
        && tail[1].eq_ignore_ascii_case("workshop")
        && tail[2].eq_ignore_ascii_case("content")
        && tail[3] == app_id.to_string()
        && tail[4] == item_id
}

pub fn workshop_item_id_from_path(path: &str, app_id: u32) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let app_id = app_id.to_string();

    for window in parts.windows(4) {
        if window[0].eq_ignore_ascii_case("workshop")
            && window[1].eq_ignore_ascii_case("content")
            && window[2] == app_id
            && normalize_workshop_id(window[3]).is_some()
        {
            return Some(window[3].to_string());
        }
    }

    for pair in parts.windows(2) {
        if pair[0] == app_id && normalize_workshop_id(pair[1]).is_some() {
            return Some(pair[1].to_string());
        }
    }

    None
}

pub fn normalize_workshop_id(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | ',' | ';' | ')' | '(' | '[' | ']' | '<' | '>' | '.'
        )
    });
    if trimmed.is_empty() || trimmed.len() > 20 || !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let normalized = trimmed.trim_start_matches('0');
    Some(if normalized.is_empty() {
        "0".to_string()
    } else {
        normalized.to_string()
    })
}

pub fn parse_workshop_item_ids(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for token in input.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';')) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        for id in ids_from_token(token) {
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
    }
    out
}

fn ids_from_token(token: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for marker in ["publishedfileids%5B0%5D=", "publishedfileids[0]=", "id="] {
        let mut rest = token;
        while let Some(idx) = rest.find(marker) {
            rest = &rest[idx + marker.len()..];
            let digits = rest
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>();
            if let Some(id) = normalize_workshop_id(&digits) {
                ids.push(id);
            }
        }
    }
    for marker in ["CommunityFilePage/", "filedetails/"] {
        if let Some(idx) = token.find(marker) {
            let rest = &token[idx + marker.len()..];
            let digits = rest
                .trim_start_matches(['/', '?', '#'])
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>();
            if let Some(id) = normalize_workshop_id(&digits) {
                ids.push(id);
            }
        }
    }
    if ids.is_empty()
        && let Some(id) = normalize_workshop_id(token)
    {
        ids.push(id);
    }
    ids
}

pub fn workshop_url(item_id: &str) -> String {
    format!(
        "{}{}",
        STEAM_WORKSHOP_URL_PREFIX,
        normalize_workshop_id(item_id).unwrap_or_else(|| item_id.trim().to_string())
    )
}

pub fn fetch_published_file_details(ids: &[String]) -> Result<Vec<WorkshopMetadata>, String> {
    let client = reqwest::blocking::Client::new();
    let mut out = Vec::new();
    for chunk in ids.chunks(MAX_WEB_API_IDS_PER_REQUEST) {
        if chunk.is_empty() {
            continue;
        }
        let mut form = Vec::with_capacity(chunk.len() + 1);
        form.push(("itemcount".to_string(), chunk.len().to_string()));
        for (idx, id) in chunk.iter().enumerate() {
            form.push((format!("publishedfileids[{}]", idx), id.clone()));
        }
        let response: Value = client
            .post(steam_api_url(
                "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/",
                &form,
            ))
            .send()
            .map_err(|err| format!("Failed to request Steam Workshop metadata: {}", err))?
            .error_for_status()
            .map_err(|err| format!("Steam Workshop metadata request failed: {}", err))?
            .json()
            .map_err(|err| format!("Failed to parse Steam Workshop metadata: {}", err))?;
        let details = response
            .get("response")
            .and_then(|value| value.get("publishedfiledetails"))
            .and_then(Value::as_array)
            .ok_or_else(|| "Steam Workshop metadata response is missing details".to_string())?;
        for detail in details {
            if let Some(item_id) = json_string(detail, "publishedfileid") {
                out.push(WorkshopMetadata {
                    item_id,
                    app_id: json_u64(detail, "consumer_app_id").map(|value| value as u32),
                    title: json_string(detail, "title"),
                    file_size: json_u64(detail, "file_size"),
                    time_updated: json_u64(detail, "time_updated"),
                    result: json_u64(detail, "result").map(|value| value as u32),
                });
            }
        }
    }
    Ok(out)
}

pub fn fetch_collection_children(collection_ids: &[String]) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::new();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for chunk in collection_ids.chunks(MAX_WEB_API_IDS_PER_REQUEST) {
        if chunk.is_empty() {
            continue;
        }
        let mut form = Vec::with_capacity(chunk.len() + 1);
        form.push(("collectioncount".to_string(), chunk.len().to_string()));
        for (idx, id) in chunk.iter().enumerate() {
            form.push((format!("publishedfileids[{}]", idx), id.clone()));
        }
        let response: Value = client
            .post(steam_api_url(
                "https://api.steampowered.com/ISteamRemoteStorage/GetCollectionDetails/v1/",
                &form,
            ))
            .send()
            .map_err(|err| format!("Failed to request Steam collection details: {}", err))?
            .error_for_status()
            .map_err(|err| format!("Steam collection details request failed: {}", err))?
            .json()
            .map_err(|err| format!("Failed to parse Steam collection details: {}", err))?;
        let collections = response
            .get("response")
            .and_then(|value| value.get("collectiondetails"))
            .and_then(Value::as_array)
            .ok_or_else(|| "Steam collection response is missing details".to_string())?;
        for collection in collections {
            let Some(children) = collection.get("children").and_then(Value::as_array) else {
                continue;
            };
            for child in children {
                if let Some(id) = json_string(child, "publishedfileid")
                    .and_then(|value| normalize_workshop_id(&value))
                    && seen.insert(id.clone())
                {
                    out.push(id);
                }
            }
        }
    }
    Ok(out)
}

pub fn metadata_by_id(metadata: Vec<WorkshopMetadata>) -> HashMap<String, WorkshopMetadata> {
    metadata
        .into_iter()
        .filter_map(|entry| normalize_workshop_id(&entry.item_id).map(|id| (id, entry)))
        .collect()
}

pub fn validate_metadata_app_ids(
    metadata: &HashMap<String, WorkshopMetadata>,
    app_id: u32,
) -> Result<(), String> {
    let mismatches = metadata
        .values()
        .filter_map(|entry| {
            let consumer = entry.app_id?;
            (consumer != app_id).then(|| format!("{} belongs to app {}", entry.item_id, consumer))
        })
        .collect::<Vec<_>>();
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Steam Workshop item app mismatch: {}",
            mismatches.join(", ")
        ))
    }
}

pub fn run_steam_helper_install(
    app_id: u32,
    item_id: &str,
    timeout_secs: u64,
) -> Result<SteamHelperOutcome, String> {
    run_steam_helper_command("install", app_id, item_id, timeout_secs)
}

pub fn run_steam_helper_remove(
    app_id: u32,
    item_id: &str,
    timeout_secs: u64,
) -> Result<SteamHelperOutcome, String> {
    run_steam_helper_command("remove", app_id, item_id, timeout_secs)
}

/// File name of the Steamworks redistributable for this platform.
pub const STEAMWORKS_LIBRARY_NAME: &str = if cfg!(target_os = "windows") {
    "steam_api64.dll"
} else if cfg!(target_os = "macos") {
    "libsteam_api.dylib"
} else {
    "libsteam_api.so"
};

/// Refuse before spawning the helper when the Steamworks redistributable is not
/// beside the executable.
///
/// The import is delay-loaded, so the helper process would otherwise die inside
/// the loader (`0xC06D007E` on Windows) with no output for the parent to report,
/// surfacing as a bare "download failure". A packaging mistake should say what is
/// actually wrong.
fn steamworks_library_available(exe: &Path) -> Result<(), String> {
    let Some(dir) = exe.parent() else {
        return Ok(());
    };
    if dir.join(STEAMWORKS_LIBRARY_NAME).is_file() {
        return Ok(());
    }
    Err(format!(
        "{} is missing next to the Foxy executable, so Steam Workshop downloads are unavailable in this install. Reinstall Foxy, or use --backend steamcmd or --backend none.",
        STEAMWORKS_LIBRARY_NAME
    ))
}

fn run_steam_helper_command(
    command: &str,
    app_id: u32,
    item_id: &str,
    timeout_secs: u64,
) -> Result<SteamHelperOutcome, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let exe =
        std::env::current_exe().map_err(|err| format!("Failed to find current exe: {}", err))?;
    steamworks_library_available(&exe)?;
    let output = Command::new(&exe)
        .arg("--json")
        .arg("steam-helper")
        .arg(command)
        .arg("--app-id")
        .arg(app_id.to_string())
        .arg("--item-id")
        .arg(&item_id)
        .arg("--timeout-seconds")
        .arg(timeout_secs.to_string())
        .output()
        .map_err(|err| {
            format!(
                "Failed to run Steam helper {}: {}",
                sanitize_log_path(&exe),
                err
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: Value = serde_json::from_str(stdout.trim()).map_err(|err| {
        format!(
            "Steam helper returned unreadable output: {} stderr={}",
            err,
            stderr.trim()
        )
    })?;
    if !parsed.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let errors = parsed
            .get("errors")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| stderr.trim().to_string());
        return Err(format!("Steam helper failed: {}", errors));
    }
    if !output.status.success() {
        return Err(format!(
            "Steam helper exited with status {:?}: {}",
            output.status.code(),
            stderr.trim()
        ));
    }
    serde_json::from_value(
        parsed
            .get("data")
            .cloned()
            .ok_or_else(|| "Steam helper output is missing data".to_string())?,
    )
    .map_err(|err| format!("Failed to parse Steam helper result: {}", err))
}

pub fn steamworks_install_item(
    app_id: u32,
    item_id: &str,
    timeout_secs: u64,
) -> Result<SteamHelperOutcome, String> {
    let item_id_u64 = item_id_u64(item_id)?;
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let client = steamworks::Client::init_app(steamworks::AppId(app_id)).map_err(|err| {
        format!(
            "Failed to initialize Steamworks for app {}: {:?}",
            app_id, err
        )
    })?;
    let ugc = client.ugc();
    let published_file_id = steamworks::PublishedFileId(item_id_u64);
    let (tx, rx) = std::sync::mpsc::channel();
    ugc.subscribe_item(published_file_id, move |result| {
        let _ = tx.send(result.map_err(|err| format!("{:?}", err)));
    });
    wait_for_steam_callback(&client, rx, timeout)?;
    let download_started = ugc.download_item(published_file_id, true);
    poll_install_info(&client, &ugc, app_id, item_id, published_file_id, timeout).map(
        |mut outcome| {
            outcome.subscribed = true;
            outcome.download_started = download_started;
            outcome
        },
    )
}

pub fn steamworks_remove_item(
    app_id: u32,
    item_id: &str,
    timeout_secs: u64,
) -> Result<SteamHelperOutcome, String> {
    let item_id_u64 = item_id_u64(item_id)?;
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let client = steamworks::Client::init_app(steamworks::AppId(app_id)).map_err(|err| {
        format!(
            "Failed to initialize Steamworks for app {}: {:?}",
            app_id, err
        )
    })?;
    let ugc = client.ugc();
    let published_file_id = steamworks::PublishedFileId(item_id_u64);
    let (tx, rx) = std::sync::mpsc::channel();
    ugc.unsubscribe_item(published_file_id, move |result| {
        let _ = tx.send(result.map_err(|err| format!("{:?}", err)));
    });
    wait_for_steam_callback(&client, rx, timeout)?;
    Ok(helper_status_from_ugc(
        &ugc,
        app_id,
        item_id,
        published_file_id,
    ))
}

pub fn steamworks_status_item(app_id: u32, item_id: &str) -> Result<SteamHelperOutcome, String> {
    let item_id_u64 = item_id_u64(item_id)?;
    let client = steamworks::Client::init_app(steamworks::AppId(app_id)).map_err(|err| {
        format!(
            "Failed to initialize Steamworks for app {}: {:?}",
            app_id, err
        )
    })?;
    let ugc = client.ugc();
    let published_file_id = steamworks::PublishedFileId(item_id_u64);
    Ok(helper_status_from_ugc(
        &ugc,
        app_id,
        item_id,
        published_file_id,
    ))
}

fn wait_for_steam_callback(
    client: &steamworks::Client,
    rx: std::sync::mpsc::Receiver<Result<(), String>>,
    timeout: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        client.run_callbacks();
        match rx.try_recv() {
            Ok(result) => return result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("Steamworks callback channel closed".to_string());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if started.elapsed() >= timeout {
            return Err("Timed out waiting for Steamworks operation".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn poll_install_info(
    client: &steamworks::Client,
    ugc: &steamworks::UGC,
    app_id: u32,
    item_id: &str,
    published_file_id: steamworks::PublishedFileId,
    timeout: Duration,
) -> Result<SteamHelperOutcome, String> {
    let started = Instant::now();
    loop {
        client.run_callbacks();
        let outcome = helper_status_from_ugc(ugc, app_id, item_id, published_file_id);
        if outcome.installed {
            return Ok(outcome);
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "Timed out waiting for Steam Workshop item {} to install",
                item_id
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn helper_status_from_ugc(
    ugc: &steamworks::UGC,
    app_id: u32,
    item_id: &str,
    published_file_id: steamworks::PublishedFileId,
) -> SteamHelperOutcome {
    let state = ugc.item_state(published_file_id);
    let install = ugc.item_install_info(published_file_id);
    let download = ugc.item_download_info(published_file_id);
    SteamHelperOutcome {
        app_id,
        item_id: item_id.to_string(),
        subscribed: state.contains(steamworks::ItemState::SUBSCRIBED),
        download_started: false,
        installed: state.contains(steamworks::ItemState::INSTALLED) || install.is_some(),
        installed_path: install.as_ref().map(|info| info.folder.clone()),
        size_bytes: install.as_ref().map(|info| info.size_on_disk),
        timestamp: install.as_ref().map(|info| info.timestamp as u64),
        downloaded_bytes: download.map(|value| value.0),
        total_bytes: download.map(|value| value.1),
    }
}

pub fn run_steamcmd_download(
    app_id: u32,
    item_id: &str,
    steamcmd_path: Option<&Path>,
    login_user: Option<&str>,
) -> Result<SteamCmdOutcome, String> {
    let item_id = normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?;
    let program = steamcmd_path
        .map(PathBuf::from)
        .unwrap_or_else(default_steamcmd_program);
    let user = login_user
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("anonymous");
    let output = Command::new(&program)
        .arg("+login")
        .arg(user)
        .arg("+workshop_download_item")
        .arg(app_id.to_string())
        .arg(&item_id)
        .arg("+quit")
        .output()
        .map_err(|err| format!("Failed to run SteamCMD: {}", err))?;
    let outcome = SteamCmdOutcome {
        app_id,
        item_id,
        status_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    };
    if output.status.success() {
        Ok(outcome)
    } else {
        Err(format!(
            "SteamCMD failed with status {:?}: {}",
            outcome.status_code,
            outcome.stderr.trim()
        ))
    }
}

fn default_steamcmd_program() -> PathBuf {
    if cfg!(target_os = "windows") {
        PathBuf::from("steamcmd.exe")
    } else {
        PathBuf::from("steamcmd")
    }
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|value| {
        if let Some(text) = value.as_str() {
            non_empty(Some(text.to_string()))
        } else if value.is_number() {
            Some(value.to_string())
        } else {
            None
        }
    })
}

fn steam_api_url(base: &str, params: &[(String, String)]) -> String {
    let mut url = String::from(base);
    url.push('?');
    for (idx, (key, value)) in params.iter().enumerate() {
        if idx > 0 {
            url.push('&');
        }
        url.push_str(&key.replace('[', "%5B").replace(']', "%5D"));
        url.push('=');
        url.push_str(value);
    }
    url
}

fn json_u64(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<u64>().ok()))
    })
}

fn item_id_u64(item_id: &str) -> Result<u64, String> {
    normalize_workshop_id(item_id)
        .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item_id))?
        .parse::<u64>()
        .map_err(|err| format!("Invalid Steam Workshop item id {}: {}", item_id, err))
}

fn numeric_id_key(value: &str) -> u64 {
    value.parse::<u64>().unwrap_or(0)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn validate_pack_entry_id(item_id: &str) -> Result<(), String> {
    if normalize_workshop_id(item_id).is_none()
        || !is_safe_child_path(item_id)
        || item_id.contains(['/', '\\'])
    {
        return Err(format!("Unsafe Steam Workshop item id {}", item_id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_workshop_item_ids_accepts_ids_urls_and_steam_urls() {
        let parsed = parse_workshop_item_ids(
            "463939057\nhttps://steamcommunity.com/sharedfiles/filedetails/?id=450814997&searchtext=ace steam://url/CommunityFilePage/620019431 463939057",
        );

        assert_eq!(parsed, vec!["463939057", "450814997", "620019431"]);
    }

    #[test]
    fn parse_workshop_item_ids_ignores_non_ids() {
        let parsed = parse_workshop_item_ids("hello id=abc 12x 00042");

        assert_eq!(parsed, vec!["42"]);
    }

    #[test]
    fn workshop_item_id_from_path_extracts_app_scoped_id() {
        let path = "D:/Steam/steamapps/workshop/content/107410/463939057";

        assert_eq!(
            workshop_item_id_from_path(path, 107410).as_deref(),
            Some("463939057")
        );
        assert_eq!(workshop_item_id_from_path(path, 1142710), None);
    }

    #[test]
    fn upsert_item_creates_and_updates_store_entry() {
        let space = tempfile::tempdir().expect("space");
        let metadata = WorkshopMetadata {
            item_id: "463939057".to_string(),
            app_id: Some(107410),
            title: Some("ACE3".to_string()),
            file_size: Some(10),
            time_updated: Some(123),
            result: Some(1),
        };

        let first = upsert_item(
            space.path(),
            107410,
            "463939057",
            None,
            Some(&metadata),
            None,
            true,
        )
        .expect("first upsert");
        let second = upsert_item(
            space.path(),
            107410,
            "463939057",
            Some("ACE Override".to_string()),
            Some(&metadata),
            None,
            false,
        )
        .expect("second upsert");

        assert!(first.added);
        assert!(!second.added);
        let store = load_store(space.path()).expect("store");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].title.as_deref(), Some("ACE Override"));
        assert!(!store.entries[0].enabled);
    }

    #[test]
    fn freeze_item_uses_hash_prefixed_backup_and_resolves_override() {
        let space = tempfile::tempdir().expect("space");
        let steam = tempfile::tempdir().expect("steam");
        let content = steam
            .path()
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join("107410")
            .join("463939057");
        fs::create_dir_all(&content).expect("content");
        fs::write(content.join("mod.cpp"), "body").expect("file");
        upsert_item(space.path(), 107410, "463939057", None, None, None, true).expect("upsert");

        let summary = freeze_item(
            space.path(),
            107410,
            "463939057",
            &steam.path().display().to_string(),
        )
        .expect("freeze");
        let override_path = resolve_launch_path_override_for_path(
            space.path(),
            107410,
            &content.display().to_string(),
        )
        .expect("override");

        assert!(Path::new(&summary.frozen_path).is_dir());
        assert!(override_path.is_dir());
        assert!(summary.frozen_path.contains("463939057"));
    }

    #[test]
    fn steam_workshop_item_dir_check_only_accepts_exact_content_dirs() {
        assert!(is_steam_workshop_item_dir(
            Path::new("D:/Steam/steamapps/workshop/content/107410/463939057"),
            107410,
            "463939057"
        ));
        assert!(is_steam_workshop_item_dir(
            Path::new("D:\\Steam\\SteamApps\\Workshop\\Content\\107410\\463939057"),
            107410,
            "463939057"
        ));
        for (path, app_id, item_id) in [
            ("D:/backup/107410/463939057", 107410, "463939057"),
            (
                "D:/Steam/steamapps/workshop/content/107410/463939057/sub",
                107410,
                "463939057",
            ),
            (
                "D:/Steam/steamapps/workshop/content/107410/463939057",
                107410,
                "1",
            ),
            (
                "D:/Steam/steamapps/workshop/content/1142710/463939057",
                107410,
                "463939057",
            ),
            ("C:/Users/someone/Documents", 107410, "463939057"),
        ] {
            assert!(
                !is_steam_workshop_item_dir(Path::new(path), app_id, item_id),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn remove_item_with_delete_data_refuses_non_workshop_installed_paths() {
        let space = tempfile::tempdir().expect("space");
        let victim = tempfile::tempdir().expect("victim");
        fs::write(victim.path().join("keep.txt"), "data").expect("file");
        let helper = SteamHelperOutcome {
            app_id: 107410,
            item_id: "463939057".to_string(),
            subscribed: true,
            download_started: false,
            installed: true,
            installed_path: Some(victim.path().display().to_string()),
            size_bytes: None,
            timestamp: None,
            downloaded_bytes: None,
            total_bytes: None,
        };
        upsert_item(
            space.path(),
            107410,
            "463939057",
            None,
            None,
            Some(&helper),
            true,
        )
        .expect("upsert");

        let err = remove_item(space.path(), 107410, "463939057", "", true)
            .expect_err("deletion of a non-workshop path must be refused");

        assert!(err.contains("Refusing to delete"));
        assert!(victim.path().join("keep.txt").is_file());
        let store = load_store(space.path()).expect("store");
        assert_eq!(store.entries.len(), 1, "entry must remain managed");
    }

    #[test]
    fn imported_item_never_claims_a_frozen_copy_it_does_not_have() {
        let space = tempfile::tempdir().expect("space");
        let item = SteamWorkshopItem {
            source: STEAM_SOURCE.to_string(),
            app_id: 107410,
            item_id: "463939057".to_string(),
            title: Some("ACE3".to_string()),
            url: String::new(),
            enabled: true,
            frozen: true,
            version: None,
            installed_path: None,
            frozen_path: Some("C:\\somewhere\\frozen".to_string()),
            size_bytes: None,
            time_updated: None,
            added_at: 0,
            updated_at: 0,
        };

        let result = upsert_imported_item(space.path(), item).expect("import");

        assert!(!result.item.frozen);
        assert!(result.item.frozen_path.is_none());
    }

    #[test]
    fn validate_metadata_app_ids_rejects_wrong_game() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "1".to_string(),
            WorkshopMetadata {
                item_id: "1".to_string(),
                app_id: Some(2),
                ..WorkshopMetadata::default()
            },
        );

        let err = validate_metadata_app_ids(&metadata, 107410).expect_err("mismatch");

        assert!(err.contains("belongs to app 2"));
    }
}
