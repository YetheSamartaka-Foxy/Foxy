use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::steam;
use crate::core::utils::addon_backup;
use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::{atomic_write, is_safe_child_path};
use crate::ui::types::SettingsViewState;

use super::{
    DirectorySetting, GameCapabilities, GameDetectCtx, GameLaunchCtx, GameModule,
    GameSettingsSchema, LaunchCommand, LaunchError, LaunchPlan, ResolvedMod, ToggleSetting,
};

pub const REFORGER_GAME_ID: &str = "reforger";
pub const REFORGER_APP_ID: u32 = 1874880;
pub const REFORGER_EXECUTABLE: &str = "ArmaReforgerSteam.exe";
pub const REFORGER_FALLBACK_EXECUTABLE: &str = "ArmaReforger.exe";
pub const REFORGER_INSTALL_DIR_SETTING_ID: &str = "reforger_directory";
pub const REFORGER_ADDONS_FILE: &str = "reforger_addons.json";
pub const REFORGER_SOURCE: &str = "reforger";

const REFORGER_DIR: &str = "reforger";
const MANAGED_ADDONS_DIR: &str = "addons";
const LIVE_DIR: &str = "live";
const FROZEN_DIR: &str = "frozen";
const REFORGER_SCHEMA_VERSION: u32 = 1;

pub struct ReforgerModule;

impl GameModule for ReforgerModule {
    fn id(&self) -> &'static str {
        REFORGER_GAME_ID
    }

    fn display_name(&self) -> &str {
        "Arma Reforger"
    }

    fn capabilities(&self) -> GameCapabilities {
        GameCapabilities {
            // Reforger addons are plain file trees, so repository sync works;
            // launching goes through `-addons`/`-addonsDir` from the managed
            // GUID store, not through an Arma-shaped repository launch plan.
            repository_sync: true,
            repository_launch: false,
            steam_workshop: false,
            direct_download: true,
            extra_files: true,
            // Profiles are still a repository-launch concept
            // (`RepositoryProfile`); there is no game-space profile store yet.
            profiles: false,
            foxy_config_export: true,
            teamspeak3_plugins: false,
        }
    }

    fn detect_install_dir(&self, ctx: &GameDetectCtx) -> Option<PathBuf> {
        steam::detect_steam_app_install_directory(
            ctx.steam_directory,
            REFORGER_APP_ID,
            &["Arma Reforger"],
            is_valid_reforger_dir,
        )
    }

    fn validate_install_dir(&self, path: &Path) -> bool {
        is_valid_reforger_dir(path)
    }

    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        let install_dir = ctx.install_dir.trim();
        if install_dir.is_empty() {
            return Err(LaunchError::InstallDirNotConfigured);
        }
        let install_path = Path::new(install_dir);
        if !install_path.exists() {
            return Err(LaunchError::InstallDirMissing);
        }
        if !is_launchable_reforger_dir(install_path) {
            return Err(LaunchError::InstallDirInvalid);
        }

        let launch = steam::steam_app_launch_command(
            REFORGER_APP_ID,
            install_path,
            &[REFORGER_EXECUTABLE, REFORGER_FALLBACK_EXECUTABLE],
            ctx.steam_directory,
        )
        .ok_or(LaunchError::LauncherUnavailable)?;

        let mut game_args = plan.launch_args.clone();
        if !plan.mods.is_empty() {
            game_args.push("-addons".to_string());
            game_args.push(
                plan.mods
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            let addon_dirs = launch_addons_dirs(&plan.mods);
            if !addon_dirs.is_empty() {
                game_args.push("-addonsDir".to_string());
                game_args.push(addon_dirs.join(","));
            }
        }

        let mut args = launch.args;
        args.extend(game_args);
        Ok(LaunchCommand {
            program: launch.program,
            args,
            cwd: Some(install_path.to_path_buf()),
        })
    }

    fn settings_schema(&self) -> GameSettingsSchema {
        GameSettingsSchema {
            directories: vec![DirectorySetting {
                id: REFORGER_INSTALL_DIR_SETTING_ID,
                label: "Arma Reforger Directory",
                help: Some(
                    "Foxy passes Reforger Workshop GUIDs with -addons and managed addon roots with -addonsDir.",
                ),
                auto_detect: true,
                is_install_dir: true,
            }],
            toggles: vec![ToggleSetting {
                id: "check_steam_running_before_launch",
                label: "Check Steam is running before launching",
                help: "Before launching, warn if Steam is not running and offer to launch it.",
            }],
        }
    }

    fn install_dir_from_settings<'a>(&self, settings: &'a SettingsViewState) -> &'a str {
        &settings.reforger_directory
    }

    fn steam_app_id(&self) -> Option<u32> {
        Some(REFORGER_APP_ID)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReforgerAddonsFile {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub entries: Vec<ReforgerAddonEntry>,
}

fn default_schema_version() -> u32 {
    REFORGER_SCHEMA_VERSION
}

impl ReforgerAddonsFile {
    pub fn entry(&self, guid: &str) -> Option<&ReforgerAddonEntry> {
        self.entries.iter().find(|entry| entry.guid == guid)
    }

    pub fn entry_mut(&mut self, guid: &str) -> Option<&mut ReforgerAddonEntry> {
        self.entries.iter_mut().find(|entry| entry.guid == guid)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReforgerAddonEntry {
    #[serde(default = "default_source")]
    pub source: String,
    pub guid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub added_at: u64,
    #[serde(default)]
    pub updated_at: u64,
}

fn default_source() -> String {
    REFORGER_SOURCE.to_string()
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerUpsertResult {
    pub item: ReforgerAddonEntry,
    pub added: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerRemoveSummary {
    pub item: ReforgerAddonEntry,
    pub deleted_managed_path: Option<String>,
    pub deleted_frozen_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerFreezeSummary {
    pub guid: String,
    pub source_path: String,
    pub frozen_path: String,
    pub content_hash: String,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerLaunchPathResolution {
    pub guid: String,
    pub name: Option<String>,
    pub path: String,
    pub frozen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerAddonIssue {
    pub guid: String,
    pub name: Option<String>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReforgerLaunchPlan {
    pub plan: LaunchPlan,
    pub addons: Vec<ReforgerLaunchPathResolution>,
    pub issues: Vec<ReforgerAddonIssue>,
}

/// Loose check used for settings validation: an install root or a bare addon
/// root (only an `addons/` folder) are both acceptable for managing addons.
pub fn is_valid_reforger_dir(path: &Path) -> bool {
    is_launchable_reforger_dir(path) || (path.is_dir() && path.join("addons").is_dir())
}

/// Strict check used for launching: the directory must contain a Reforger
/// executable; a bare addon root cannot start the game.
pub fn is_launchable_reforger_dir(path: &Path) -> bool {
    path.is_dir()
        && [REFORGER_EXECUTABLE, REFORGER_FALLBACK_EXECUTABLE]
            .iter()
            .any(|name| path.join(name).is_file())
}

pub fn store_path(space_dir: &Path) -> PathBuf {
    space_dir.join(REFORGER_ADDONS_FILE)
}

pub fn reforger_root(space_dir: &Path) -> PathBuf {
    space_dir.join(REFORGER_DIR)
}

pub fn managed_addons_root(space_dir: &Path) -> PathBuf {
    reforger_root(space_dir)
        .join(MANAGED_ADDONS_DIR)
        .join(LIVE_DIR)
}

pub fn frozen_addons_root(space_dir: &Path) -> PathBuf {
    reforger_root(space_dir)
        .join(MANAGED_ADDONS_DIR)
        .join(FROZEN_DIR)
}

pub fn default_user_addons_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("USERPROFILE").map(|home| {
            PathBuf::from(home)
                .join("Documents")
                .join("My Games")
                .join("ArmaReforger")
                .join("addons")
        })
    } else {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Documents")
                .join("My Games")
                .join("ArmaReforger")
                .join("addons")
        })
    }
}

pub fn load_store(space_dir: &Path) -> Result<ReforgerAddonsFile, String> {
    let path = store_path(space_dir);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReforgerAddonsFile {
                schema_version: REFORGER_SCHEMA_VERSION,
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
    let mut store: ReforgerAddonsFile = serde_json::from_str(&raw)
        .map_err(|err| format!("Failed to parse {}: {}", sanitize_log_path(&path), err))?;
    if store.schema_version == 0 {
        store.schema_version = REFORGER_SCHEMA_VERSION;
    }
    if store.schema_version != REFORGER_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported Reforger addon schema version {}",
            store.schema_version
        ));
    }
    Ok(store)
}

pub fn save_store(space_dir: &Path, store: &ReforgerAddonsFile) -> Result<(), String> {
    let mut store = store.clone();
    store.schema_version = REFORGER_SCHEMA_VERSION;
    store
        .entries
        .sort_by(|left, right| left.guid.cmp(&right.guid));
    let serialized = serde_json::to_string_pretty(&store)
        .map_err(|err| format!("Failed to serialize Reforger addon store: {}", err))?;
    atomic_write(&store_path(space_dir), serialized.as_bytes())
        .map_err(|err| format!("Failed to write Reforger addon store: {}", err))
}

pub fn upsert_addon(
    space_dir: &Path,
    guid: &str,
    name: Option<String>,
    source_path: Option<&Path>,
    enabled: bool,
) -> Result<ReforgerUpsertResult, String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    validate_guid_path_component(&guid)?;
    let now = unix_timestamp_now();
    let copied_path = source_path
        .map(|path| copy_addon_folder_to_managed(space_dir, &guid, path))
        .transpose()?;
    let metadata = copied_path
        .as_deref()
        .map(Path::new)
        .or(source_path)
        .and_then(read_server_data_metadata);
    let existing_live_path = copied_path
        .clone()
        .or_else(|| default_addon_path_for_guid(&guid).map(|path| path.display().to_string()));
    if copied_path.is_none() && existing_live_path.is_none() {
        return Err(format!(
            "Arma Reforger addon {} is not present. Download it in-game or provide --source.",
            guid
        ));
    }

    let mut store = load_store(space_dir)?;
    let added = store.entry(&guid).is_none();
    let item = match store.entry_mut(&guid) {
        Some(entry) => {
            entry.source = REFORGER_SOURCE.to_string();
            entry.enabled = enabled;
            entry.name = non_empty(name)
                .or_else(|| metadata.as_ref().and_then(|meta| meta.name.clone()))
                .or(entry.name.clone());
            entry.version = metadata
                .as_ref()
                .and_then(|meta| meta.version.clone())
                .or(entry.version.clone());
            if let Some(path) = copied_path {
                entry.managed_path = Some(path);
            } else if entry.installed_path.is_none() {
                entry.installed_path = existing_live_path;
            }
            entry.size_bytes = resolve_live_path(entry)
                .and_then(|path| directory_total_size(&path).ok())
                .or(entry.size_bytes);
            entry.updated_at = now;
            entry.clone()
        }
        None => {
            let size_bytes = existing_live_path
                .as_deref()
                .map(Path::new)
                .and_then(|path| directory_total_size(path).ok());
            let item = ReforgerAddonEntry {
                source: REFORGER_SOURCE.to_string(),
                guid: guid.clone(),
                name: non_empty(name)
                    .or_else(|| metadata.as_ref().and_then(|meta| meta.name.clone())),
                enabled,
                frozen: false,
                version: metadata.and_then(|meta| meta.version),
                installed_path: if copied_path.is_some() {
                    None
                } else {
                    existing_live_path
                },
                managed_path: copied_path,
                frozen_path: None,
                size_bytes,
                added_at: now,
                updated_at: now,
            };
            store.entries.push(item.clone());
            item
        }
    };
    save_store(space_dir, &store)?;
    Ok(ReforgerUpsertResult { item, added })
}

pub fn upsert_imported_addon(
    space_dir: &Path,
    mut item: ReforgerAddonEntry,
) -> Result<ReforgerUpsertResult, String> {
    let guid = normalize_reforger_guid(&item.guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", item.guid))?;
    validate_guid_path_component(&guid)?;
    let mut store = load_store(space_dir)?;
    let now = unix_timestamp_now();
    let added = store.entry(&guid).is_none();
    item.source = REFORGER_SOURCE.to_string();
    item.guid = guid;
    item.managed_path = None;
    item.frozen_path = None;
    // Packs never carry frozen payloads, so an imported entry cannot claim a
    // frozen copy it does not have.
    item.frozen = false;
    if item.added_at == 0 {
        item.added_at = now;
    }
    item.updated_at = now;
    if let Some(existing) = store.entry_mut(&item.guid) {
        *existing = item.clone();
    } else {
        store.entries.push(item.clone());
    }
    save_store(space_dir, &store)?;
    Ok(ReforgerUpsertResult { item, added })
}

pub fn set_addon_enabled(
    space_dir: &Path,
    guid: &str,
    enabled: bool,
) -> Result<ReforgerAddonEntry, String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry_mut(&guid)
        .ok_or_else(|| format!("Arma Reforger addon {} is not managed", guid))?;
    entry.enabled = enabled;
    entry.updated_at = unix_timestamp_now();
    let entry = entry.clone();
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn remove_addon(
    space_dir: &Path,
    guid: &str,
    delete_data: bool,
) -> Result<ReforgerRemoveSummary, String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    let mut store = load_store(space_dir)?;
    let item = store
        .entry(&guid)
        .cloned()
        .ok_or_else(|| format!("Arma Reforger addon {} is not managed", guid))?;

    let mut deleted_managed_path = None;
    let mut deleted_frozen_path = None;
    if delete_data {
        let managed = managed_addons_root(space_dir).join(&guid);
        if managed.exists() {
            fs::remove_dir_all(&managed).map_err(|err| {
                format!("Failed to delete {}: {}", sanitize_log_path(&managed), err)
            })?;
            deleted_managed_path = Some(managed.display().to_string());
        }
        let frozen = frozen_addons_root(space_dir).join(&guid);
        if frozen.exists() {
            fs::remove_dir_all(&frozen).map_err(|err| {
                format!("Failed to delete {}: {}", sanitize_log_path(&frozen), err)
            })?;
            deleted_frozen_path = Some(frozen.display().to_string());
        }
    }

    store.entries.retain(|entry| entry.guid != guid);
    save_store(space_dir, &store)?;
    Ok(ReforgerRemoveSummary {
        item,
        deleted_managed_path,
        deleted_frozen_path,
    })
}

pub fn freeze_addon(space_dir: &Path, guid: &str) -> Result<ReforgerFreezeSummary, String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry(&guid)
        .cloned()
        .ok_or_else(|| format!("Arma Reforger addon {} is not managed", guid))?;
    let source = resolve_live_path(&entry).ok_or_else(|| {
        format!(
            "Arma Reforger addon {} is not present under a managed or default addon folder",
            guid
        )
    })?;
    if !source.is_dir() {
        return Err(format!(
            "Arma Reforger addon {} has no readable folder at {}",
            guid,
            sanitize_log_path(&source)
        ));
    }

    let backup_root = frozen_addons_root(space_dir).join(&guid);
    let record = addon_backup::backup_addon(&backup_root, &source)
        .map_err(|err| format!("Failed to freeze Arma Reforger addon {}: {}", guid, err))?;
    let entry = store
        .entry_mut(&guid)
        .ok_or_else(|| format!("Arma Reforger addon {} is not managed", guid))?;
    entry.frozen = true;
    entry.frozen_path = Some(record.path.display().to_string());
    entry.version = Some(record.content_hash.clone());
    entry.size_bytes = Some(record.size_bytes);
    entry.updated_at = unix_timestamp_now();
    let summary = ReforgerFreezeSummary {
        guid,
        source_path: source.display().to_string(),
        frozen_path: record.path.display().to_string(),
        content_hash: record.content_hash,
        size_bytes: record.size_bytes,
    };
    save_store(space_dir, &store)?;
    Ok(summary)
}

pub fn unfreeze_addon(space_dir: &Path, guid: &str) -> Result<ReforgerAddonEntry, String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    let mut store = load_store(space_dir)?;
    let entry = store
        .entry_mut(&guid)
        .ok_or_else(|| format!("Arma Reforger addon {} is not managed", guid))?;
    entry.frozen = false;
    entry.updated_at = unix_timestamp_now();
    let entry = entry.clone();
    save_store(space_dir, &store)?;
    Ok(entry)
}

pub fn resolve_launch_path(
    space_dir: &Path,
    guid: &str,
) -> Result<ReforgerLaunchPathResolution, String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    let store = load_store(space_dir)?;
    let entry = store
        .entry(&guid)
        .ok_or_else(|| format!("Arma Reforger addon {} is not managed", guid))?;
    if entry.frozen
        && let Some(path) = entry.frozen_path.as_deref().map(PathBuf::from)
        && path.is_dir()
    {
        return Ok(ReforgerLaunchPathResolution {
            guid,
            name: entry.name.clone(),
            path: path.display().to_string(),
            frozen: true,
        });
    }
    let path = resolve_live_path(entry).ok_or_else(|| {
        format!(
            "Arma Reforger addon {} is not present under a managed or default addon folder",
            guid
        )
    })?;
    Ok(ReforgerLaunchPathResolution {
        guid,
        name: entry.name.clone(),
        path: path.display().to_string(),
        frozen: false,
    })
}

pub fn build_workshop_launch_plan(
    space_dir: &Path,
    include_disabled: bool,
) -> Result<ReforgerLaunchPlan, String> {
    let store = load_store(space_dir)?;
    let mut mods = Vec::new();
    let mut addons = Vec::new();
    let mut issues = Vec::new();
    for entry in store
        .entries
        .iter()
        .filter(|entry| include_disabled || entry.enabled)
    {
        match resolve_launch_path(space_dir, &entry.guid) {
            Ok(resolution) => {
                mods.push(ResolvedMod {
                    id: resolution.guid.clone(),
                    path: Some(resolution.path.clone()),
                });
                addons.push(resolution);
            }
            Err(error) => issues.push(ReforgerAddonIssue {
                guid: entry.guid.clone(),
                name: entry.name.clone(),
                error,
            }),
        }
    }
    Ok(ReforgerLaunchPlan {
        plan: LaunchPlan {
            launch_args: Vec::new(),
            mods,
            server: None,
        },
        addons,
        issues,
    })
}

pub fn launch_addons_dirs(mods: &[ResolvedMod]) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for path in mods.iter().filter_map(|entry| entry.path.as_deref()) {
        let Some(parent) = Path::new(path).parent() else {
            continue;
        };
        let text = parent.display().to_string();
        let key = normalized_path_key(&text);
        if seen.insert(key) {
            dirs.push(text);
        }
    }
    dirs
}

pub fn normalize_reforger_guid(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | ',' | ';' | ')' | '(' | '[' | ']' | '<' | '>' | '.'
        )
    });
    let candidate = candidate_guid_from_token(trimmed).unwrap_or_else(|| trimmed.to_string());
    let candidate = strip_workshop_slug_suffix(&candidate);
    if candidate.len() < 8
        || candidate.len() > 64
        || !candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return None;
    }
    Some(candidate.to_ascii_uppercase())
}

pub fn parse_reforger_guids(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for token in input.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';')) {
        if let Some(guid) = normalize_reforger_guid(token)
            && seen.insert(guid.clone())
        {
            out.push(guid);
        }
    }
    out
}

pub fn validate_pack_entry_id(guid: &str) -> Result<(), String> {
    let guid = normalize_reforger_guid(guid)
        .ok_or_else(|| format!("Invalid Arma Reforger Workshop GUID {}", guid))?;
    validate_guid_path_component(&guid)
}

fn candidate_guid_from_token(token: &str) -> Option<String> {
    if let Some(query_start) = token.find('?') {
        for pair in token[query_start + 1..].split('&') {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            if key.eq_ignore_ascii_case("id") || key.eq_ignore_ascii_case("modId") {
                return Some(value.to_string());
            }
        }
    }
    token
        .replace('\\', "/")
        .split('/')
        .rfind(|part| !part.trim().is_empty())
        .map(str::to_string)
}

/// Workshop URLs on reforger.armaplatform.com append a display-name slug to
/// the GUID (`/workshop/<16-hex-GUID>-Name`); only the hex GUID half is the
/// mod id the game accepts in `-addons`.
fn strip_workshop_slug_suffix(candidate: &str) -> &str {
    match candidate.split_once('-') {
        Some((head, _)) if head.len() == 16 && head.chars().all(|ch| ch.is_ascii_hexdigit()) => {
            head
        }
        _ => candidate,
    }
}

fn validate_guid_path_component(guid: &str) -> Result<(), String> {
    if !is_safe_child_path(guid) || guid.contains(['/', '\\']) {
        return Err(format!("Unsafe Arma Reforger Workshop GUID {}", guid));
    }
    Ok(())
}

fn copy_addon_folder_to_managed(
    space_dir: &Path,
    guid: &str,
    source: &Path,
) -> Result<String, String> {
    if !source.is_dir() {
        return Err(format!(
            "Arma Reforger addon source is not a directory: {}",
            sanitize_log_path(source)
        ));
    }
    let target_root = managed_addons_root(space_dir);
    fs::create_dir_all(&target_root).map_err(|err| {
        format!(
            "Failed to create {}: {}",
            sanitize_log_path(&target_root),
            err
        )
    })?;
    let target = target_root.join(guid);
    if source.canonicalize().ok() == target.canonicalize().ok() {
        return Ok(target.display().to_string());
    }
    let staging = target_root.join(format!("{}.tmp.{}", guid, unix_timestamp_now()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|err| format!("Failed to clear {}: {}", sanitize_log_path(&staging), err))?;
    }
    if let Err(err) = copy_directory_recursive(source, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(err);
    }
    if target.exists() {
        fs::remove_dir_all(&target).map_err(|err| {
            let _ = fs::remove_dir_all(&staging);
            format!("Failed to replace {}: {}", sanitize_log_path(&target), err)
        })?;
    }
    fs::rename(&staging, &target).map_err(|err| {
        let _ = fs::remove_dir_all(&staging);
        format!("Failed to finalize {}: {}", sanitize_log_path(&target), err)
    })?;
    Ok(target.display().to_string())
}

fn copy_directory_recursive(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target)
        .map_err(|err| format!("Failed to create {}: {}", sanitize_log_path(target), err))?;
    for entry in fs::read_dir(source)
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?
    {
        let entry = entry
            .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| "Reforger addon source contains a non-UTF-8 file name".to_string())?;
        if !is_safe_child_path(name) {
            return Err(format!("Unsafe Reforger addon payload path {}", name));
        }
        let child_source = entry.path();
        let child_target = target.join(name);
        let metadata = entry.metadata().map_err(|err| {
            format!(
                "Failed to read {}: {}",
                sanitize_log_path(&child_source),
                err
            )
        })?;
        if metadata.is_dir() {
            copy_directory_recursive(&child_source, &child_target)?;
        } else if metadata.is_file() {
            fs::copy(&child_source, &child_target).map_err(|err| {
                format!(
                    "Failed to copy {} to {}: {}",
                    sanitize_log_path(&child_source),
                    sanitize_log_path(&child_target),
                    err
                )
            })?;
        }
    }
    Ok(())
}

fn resolve_live_path(entry: &ReforgerAddonEntry) -> Option<PathBuf> {
    if let Some(path) = entry.managed_path.as_deref().map(PathBuf::from)
        && path.is_dir()
    {
        return Some(path);
    }
    if let Some(path) = entry.installed_path.as_deref().map(PathBuf::from)
        && path.is_dir()
    {
        return Some(path);
    }
    default_addon_path_for_guid(&entry.guid)
}

fn default_addon_path_for_guid(guid: &str) -> Option<PathBuf> {
    let path = default_user_addons_dir()?.join(guid);
    path.is_dir().then_some(path)
}

#[derive(Clone, Debug)]
struct ServerDataMetadata {
    name: Option<String>,
    version: Option<String>,
}

fn read_server_data_metadata(addon_dir: &Path) -> Option<ServerDataMetadata> {
    let raw = fs::read_to_string(addon_dir.join("ServerData.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let version = value
        .get("revision")
        .and_then(|revision| revision.get("version"))
        .and_then(|version| {
            version
                .as_str()
                .map(str::to_string)
                .or_else(|| version.as_u64().map(|value| value.to_string()))
        });
    Some(ServerDataMetadata { name, version })
}

fn directory_total_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn normalized_path_key(path: &str) -> String {
    let normalized = path.replace('\\', "/").trim_end_matches('/').to_string();
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    fn write_reforger_install(path: &Path) {
        fs::create_dir_all(path).expect("install dir");
        fs::write(path.join(REFORGER_EXECUTABLE), "").expect("exe");
    }

    fn write_addon(root: &Path, guid: &str, name: &str) -> PathBuf {
        let addon = root.join(guid);
        fs::create_dir_all(&addon).expect("addon dir");
        fs::write(
            addon.join("ServerData.json"),
            format!(
                r#"{{"id":"{}","name":"{}","revision":{{"version":"1.2.3"}}}}"#,
                guid, name
            ),
        )
        .expect("server data");
        fs::write(addon.join("payload.bin"), "payload").expect("payload");
        addon
    }

    #[test]
    fn normalize_reforger_guid_accepts_urls_and_plain_ids() {
        assert_eq!(
            normalize_reforger_guid("https://reforger.armaplatform.com/workshop/596ABCDEF0123456")
                .as_deref(),
            Some("596ABCDEF0123456")
        );
        assert_eq!(
            parse_reforger_guids("596abcdef0123456, 596ABCDEF0123456 5A658E3BEE1D4AE2"),
            vec!["596ABCDEF0123456", "5A658E3BEE1D4AE2"]
        );
    }

    #[test]
    fn normalize_reforger_guid_strips_workshop_url_name_slugs() {
        // Workshop pages use `<GUID>-<Name>` slugs; only the GUID may reach -addons.
        assert_eq!(
            normalize_reforger_guid(
                "https://reforger.armaplatform.com/workshop/59651E5CD1F1BB0B-ACEChopping"
            )
            .as_deref(),
            Some("59651E5CD1F1BB0B")
        );
        assert_eq!(
            normalize_reforger_guid("59651E5CD1F1BB0B-ACEChopping").as_deref(),
            Some("59651E5CD1F1BB0B")
        );
        // A dash after a non-hex prefix is part of the id, not a slug.
        assert_eq!(
            normalize_reforger_guid("notahexstring123-x").as_deref(),
            Some("NOTAHEXSTRING123-X")
        );
    }

    #[test]
    fn upsert_addon_copies_source_and_reads_metadata() {
        let space = tempfile::tempdir().expect("space");
        let source_root = tempfile::tempdir().expect("source");
        let source = write_addon(source_root.path(), "596ABCDEF0123456", "Capture");

        let result = upsert_addon(space.path(), "596abcdef0123456", None, Some(&source), true)
            .expect("upsert");

        assert!(result.added);
        assert_eq!(result.item.guid, "596ABCDEF0123456");
        assert_eq!(result.item.name.as_deref(), Some("Capture"));
        assert_eq!(result.item.version.as_deref(), Some("1.2.3"));
        assert!(result.item.size_bytes.is_some_and(|size| size > 0));
        let managed = managed_addons_root(space.path()).join("596ABCDEF0123456");
        assert!(managed.join("payload.bin").is_file());
    }

    #[test]
    fn remove_addon_deletes_managed_data_only_when_requested() {
        let space = tempfile::tempdir().expect("space");
        let source_root = tempfile::tempdir().expect("source");
        let source = write_addon(source_root.path(), "596ABCDEF0123456", "Capture");
        upsert_addon(space.path(), "596ABCDEF0123456", None, Some(&source), true).expect("upsert");
        let managed = managed_addons_root(space.path()).join("596ABCDEF0123456");

        let summary = remove_addon(space.path(), "596ABCDEF0123456", true).expect("remove");

        assert_eq!(summary.item.guid, "596ABCDEF0123456");
        assert!(summary.deleted_managed_path.is_some());
        assert!(!managed.exists());
        let store = load_store(space.path()).expect("store");
        assert!(store.entries.is_empty());
    }

    #[test]
    fn imported_addon_never_claims_a_frozen_copy_it_does_not_have() {
        let space = tempfile::tempdir().expect("space");
        let entry = ReforgerAddonEntry {
            source: REFORGER_SOURCE.to_string(),
            guid: "596ABCDEF0123456".to_string(),
            name: Some("Capture".to_string()),
            enabled: true,
            frozen: true,
            version: None,
            installed_path: None,
            managed_path: Some("C:\\somewhere\\else".to_string()),
            frozen_path: Some("C:\\somewhere\\frozen".to_string()),
            size_bytes: None,
            added_at: 0,
            updated_at: 0,
        };

        let result = upsert_imported_addon(space.path(), entry).expect("import");

        assert!(!result.item.frozen);
        assert!(result.item.frozen_path.is_none());
        assert!(result.item.managed_path.is_none());
    }

    #[test]
    fn launchable_dir_check_requires_an_executable() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(dir.path().join("addons")).expect("addons dir");

        assert!(is_valid_reforger_dir(dir.path()));
        assert!(!is_launchable_reforger_dir(dir.path()));

        fs::write(dir.path().join(REFORGER_EXECUTABLE), "").expect("exe");
        assert!(is_launchable_reforger_dir(dir.path()));
    }

    #[test]
    fn freeze_addon_resolves_frozen_launch_path() {
        let space = tempfile::tempdir().expect("space");
        let source_root = tempfile::tempdir().expect("source");
        let source = write_addon(source_root.path(), "596ABCDEF0123456", "Capture");
        upsert_addon(space.path(), "596ABCDEF0123456", None, Some(&source), true).expect("upsert");

        let summary = freeze_addon(space.path(), "596ABCDEF0123456").expect("freeze");
        let resolution = resolve_launch_path(space.path(), "596ABCDEF0123456").expect("resolve");

        assert!(Path::new(&summary.frozen_path).is_dir());
        assert!(resolution.frozen);
        assert_eq!(resolution.path, summary.frozen_path);
    }

    #[test]
    fn build_workshop_launch_plan_uses_enabled_addons() {
        let space = tempfile::tempdir().expect("space");
        let source_root = tempfile::tempdir().expect("source");
        let first = write_addon(source_root.path(), "596ABCDEF0123456", "Capture");
        let second = write_addon(source_root.path(), "5A658E3BEE1D4AE2", "Disabled");
        upsert_addon(space.path(), "596ABCDEF0123456", None, Some(&first), true).expect("first");
        upsert_addon(space.path(), "5A658E3BEE1D4AE2", None, Some(&second), false).expect("second");

        let plan = build_workshop_launch_plan(space.path(), false).expect("plan");

        assert!(plan.issues.is_empty());
        assert_eq!(plan.plan.mods.len(), 1);
        assert_eq!(plan.plan.mods[0].id, "596ABCDEF0123456");
        assert_eq!(plan.addons.len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reforger_module_builds_exact_arg_vector() {
        let install = tempfile::tempdir().expect("install");
        write_reforger_install(install.path());
        let addon_root = tempfile::tempdir().expect("addons");
        let addon = write_addon(addon_root.path(), "596ABCDEF0123456", "Capture");
        let plan = LaunchPlan {
            launch_args: vec!["-profile".to_string(), "Foxy".to_string()],
            mods: vec![ResolvedMod {
                id: "596ABCDEF0123456".to_string(),
                path: Some(addon.display().to_string()),
            }],
            server: None,
        };
        let install_dir = install.path().display().to_string();
        let ctx = GameLaunchCtx {
            install_dir: &install_dir,
            steam_directory: "",
        };

        let command = ReforgerModule.build_launch(&plan, &ctx).expect("launch");

        assert_eq!(command.program, install.path().join(REFORGER_EXECUTABLE));
        assert_eq!(
            command.args,
            vec![
                "-profile".to_string(),
                "Foxy".to_string(),
                "-addons".to_string(),
                "596ABCDEF0123456".to_string(),
                "-addonsDir".to_string(),
                addon_root.path().display().to_string(),
            ]
        );
        assert_eq!(command.cwd, Some(install.path().to_path_buf()));
    }

    #[test]
    fn launch_addons_dirs_deduplicates_parent_dirs() {
        let mods = vec![
            ResolvedMod {
                id: "a".to_string(),
                path: Some("D:/mods/a".to_string()),
            },
            ResolvedMod {
                id: "b".to_string(),
                path: Some("D:/mods/b".to_string()),
            },
        ];

        assert_eq!(launch_addons_dirs(&mods), vec!["D:/mods"]);
    }
}
