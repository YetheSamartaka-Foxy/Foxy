mod migration;
mod settings_split;

pub use settings_split::{
    merge_value_over_defaults, read_merged_settings_value, write_game_settings_half,
    write_split_settings,
};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::core::utils::app_paths;
use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::atomic_write;

pub const DEFAULT_GAME_SPACE_ID: &str = "arma3";
const DEFAULT_GAME_SPACE_NAME: &str = "Arma 3";

pub const GAMES_DIR_NAME: &str = "games";
pub const GAMES_REGISTRY_FILE: &str = "games.json";
pub const APP_SETTINGS_FILE: &str = "app_settings.json";
pub const GAME_SETTINGS_FILE: &str = "game_settings.json";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GameSpaceEntry {
    pub id: String,
    pub game_id: String,
    pub display_name: String,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GamesRegistryFile {
    pub active_game_space_id: String,
    #[serde(default)]
    pub last_opened_game_space_id: String,
    #[serde(default)]
    pub game_spaces: Vec<GameSpaceEntry>,
}

impl GamesRegistryFile {
    fn single_space_default() -> Self {
        Self {
            active_game_space_id: DEFAULT_GAME_SPACE_ID.to_string(),
            last_opened_game_space_id: DEFAULT_GAME_SPACE_ID.to_string(),
            game_spaces: vec![default_game_space_entry()],
        }
    }

    fn entry(&self, space_id: &str) -> Option<&GameSpaceEntry> {
        self.game_spaces.iter().find(|entry| entry.id == space_id)
    }

    /// Entry that should load: the active id, then the last-opened id, then
    /// the first listed space. `None` only when `game_spaces` is empty.
    fn resolve_active_entry(&self) -> Option<&GameSpaceEntry> {
        self.entry(&self.active_game_space_id)
            .or_else(|| self.entry(&self.last_opened_game_space_id))
            .or_else(|| self.game_spaces.first())
    }
}

fn default_game_space_entry() -> GameSpaceEntry {
    GameSpaceEntry {
        id: DEFAULT_GAME_SPACE_ID.to_string(),
        game_id: DEFAULT_GAME_SPACE_ID.to_string(),
        display_name: DEFAULT_GAME_SPACE_NAME.to_string(),
        created_at: unix_timestamp_now(),
    }
}

fn unix_timestamp_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The game space this process is running against. `ensure_game_spaces_layout()`
/// seeds this from `games.json` at startup; `activate_game_space()` retargets
/// it at runtime when the user switches spaces in the UI.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveGameSpace {
    pub space_id: String,
    pub game_id: String,
    pub display_name: String,
}

impl ActiveGameSpace {
    fn fallback() -> Self {
        Self {
            space_id: DEFAULT_GAME_SPACE_ID.to_string(),
            game_id: DEFAULT_GAME_SPACE_ID.to_string(),
            display_name: DEFAULT_GAME_SPACE_NAME.to_string(),
        }
    }
}

impl From<&GameSpaceEntry> for ActiveGameSpace {
    fn from(entry: &GameSpaceEntry) -> Self {
        Self {
            space_id: entry.id.clone(),
            game_id: entry.game_id.clone(),
            display_name: entry.display_name.clone(),
        }
    }
}

static ACTIVE_SPACE: OnceLock<RwLock<ActiveGameSpace>> = OnceLock::new();

fn active_space_state() -> &'static RwLock<ActiveGameSpace> {
    ACTIVE_SPACE.get_or_init(|| RwLock::new(ActiveGameSpace::fallback()))
}

pub fn active_game_space() -> ActiveGameSpace {
    match active_space_state().read() {
        Ok(active) => active.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn set_process_active_game_space(active: ActiveGameSpace) {
    match active_space_state().write() {
        Ok(mut current) => *current = active,
        Err(poisoned) => *poisoned.into_inner() = active,
    }
}

const MAX_GAME_SPACE_ID_LEN: usize = 64;
const MAX_GAME_SPACE_ID_BASE_LEN: usize = 48;

/// Game-space ids become a single path component under `games/`, so only a
/// narrow ASCII slug is accepted; anything else in a hand-edited or corrupt
/// `games.json` must never reach a filesystem operation.
pub fn is_valid_game_space_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_GAME_SPACE_ID_LEN
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && !is_windows_reserved_name(id)
}

fn is_windows_reserved_name(id: &str) -> bool {
    matches!(id, "con" | "prn" | "aux" | "nul")
        || (id.len() == 4
            && (id.starts_with("com") || id.starts_with("lpt"))
            && id.as_bytes()[3].is_ascii_digit())
}

fn validated_space_id(space_id: &str) -> Result<&str, String> {
    if is_valid_game_space_id(space_id) {
        Ok(space_id)
    } else {
        Err(format!("Invalid game space id {:?}", space_id))
    }
}

fn game_space_dir_at(root: &Path, space_id: &str) -> PathBuf {
    root.join(GAMES_DIR_NAME).join(space_id)
}

pub fn game_space_dir_for(space_id: &str) -> PathBuf {
    game_space_dir_at(&app_paths::foxy_data_dir(), space_id)
}

/// Directory of the active game space, created on demand. All per-game data
/// (repositories, spaces, visual folders, game settings, database) lives here;
/// app-global files stay in `foxy_data_dir()`.
pub fn active_game_space_dir() -> PathBuf {
    let active = active_game_space();
    let dir = game_space_dir_at(&app_paths::foxy_data_dir(), &active.space_id);
    if let Err(err) = fs::create_dir_all(&dir) {
        log::error!(
            "Failed to create game space directory {}: {}",
            sanitize_log_path(&dir),
            err
        );
    }
    dir
}

fn games_registry_path(root: &Path) -> PathBuf {
    root.join(GAMES_REGISTRY_FILE)
}

fn load_games_registry(path: &Path) -> Result<GamesRegistryFile, String> {
    let raw = fs::read_to_string(path)
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(path), err))?;
    serde_json::from_str(&raw)
        .map_err(|err| format!("Failed to parse {}: {}", sanitize_log_path(path), err))
}

fn save_games_registry(path: &Path, registry: &GamesRegistryFile) -> Result<(), String> {
    let serialized = serde_json::to_string_pretty(registry)
        .map_err(|err| format!("Failed to serialize games registry: {}", err))?;
    atomic_write(path, serialized.as_bytes())
        .map_err(|err| format!("Failed to write {}: {}", sanitize_log_path(path), err))
}

pub fn load_registry() -> Result<GamesRegistryFile, String> {
    load_games_registry(&games_registry_path(&app_paths::foxy_data_dir()))
}

/// Mark a game space as the active one in `games.json` and remember the
/// previously active one as last-opened. Does not affect the running process;
/// the UI uses [`activate_game_space`] for runtime switching.
pub fn set_active_game_space(space_id: &str) -> Result<GameSpaceEntry, String> {
    set_active_game_space_at(&app_paths::foxy_data_dir(), space_id)
}

/// Runtime switch: persist the active space to `games.json`, retarget this
/// process to it, and make sure its directory exists. After this returns,
/// every `active_game_space_dir()` consumer (settings, repositories, the
/// database handle slot) resolves to the new space.
pub fn activate_game_space(space_id: &str) -> Result<GameSpaceEntry, String> {
    let entry = set_active_game_space(space_id)?;
    let dir = game_space_dir_for(&entry.id);
    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "Failed to create game space directory {}: {}",
            sanitize_log_path(&dir),
            err
        )
    })?;
    set_process_active_game_space(ActiveGameSpace::from(&entry));
    Ok(entry)
}

fn set_active_game_space_at(root: &Path, space_id: &str) -> Result<GameSpaceEntry, String> {
    let space_id = validated_space_id(space_id)?;
    let path = games_registry_path(root);
    let mut registry = load_games_registry(&path)?;
    let entry = registry
        .entry(space_id)
        .cloned()
        .ok_or_else(|| format!("Game space {} does not exist", space_id))?;
    if registry.active_game_space_id != entry.id {
        registry.last_opened_game_space_id = registry.active_game_space_id.clone();
        registry.active_game_space_id = entry.id.clone();
        save_games_registry(&path, &registry)?;
    }
    Ok(entry)
}

pub fn create_game_space(game_id: &str, display_name: &str) -> Result<GameSpaceEntry, String> {
    create_game_space_at(&app_paths::foxy_data_dir(), game_id, display_name)
}

fn create_game_space_at(
    root: &Path,
    game_id: &str,
    display_name: &str,
) -> Result<GameSpaceEntry, String> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err("Game space name must not be empty".to_string());
    }
    if crate::core::game::registry().get(game_id).is_none() {
        return Err(format!("Unknown game id {}", game_id));
    }

    let path = games_registry_path(root);
    let mut registry = load_games_registry(&path)?;
    let id = unique_space_id(root, &registry, display_name);
    let entry = GameSpaceEntry {
        id: id.clone(),
        game_id: game_id.to_string(),
        display_name: display_name.to_string(),
        created_at: unix_timestamp_now(),
    };

    // Directory first so a failed registry write never leaves an entry that
    // points at nothing; the empty directory is removed again on failure.
    let space_dir = game_space_dir_at(root, &id);
    fs::create_dir_all(&space_dir).map_err(|err| {
        format!(
            "Failed to create game space directory {}: {}",
            sanitize_log_path(&space_dir),
            err
        )
    })?;
    registry.game_spaces.push(entry.clone());
    if let Err(err) = save_games_registry(&path, &registry) {
        let _ = fs::remove_dir(&space_dir);
        return Err(err);
    }
    Ok(entry)
}

/// Remove a game space: delete Foxy's `games/<id>/` workspace (never any game
/// install or mod folders outside it), then drop the registry entry. The
/// active space cannot be removed.
pub fn remove_game_space(space_id: &str) -> Result<GameSpaceEntry, String> {
    remove_game_space_at(&app_paths::foxy_data_dir(), space_id)
}

fn remove_game_space_at(root: &Path, space_id: &str) -> Result<GameSpaceEntry, String> {
    let space_id = validated_space_id(space_id)?;
    let path = games_registry_path(root);
    let mut registry = load_games_registry(&path)?;
    let entry = registry
        .entry(space_id)
        .cloned()
        .ok_or_else(|| format!("Game space {} does not exist", space_id))?;
    validated_space_id(&entry.id)?;
    if registry.active_game_space_id == entry.id {
        return Err("The active game space cannot be removed".to_string());
    }

    // Workspace first so a failure leaves the entry in place and the removal
    // can simply be retried.
    let space_dir = game_space_dir_at(root, &entry.id);
    if let Err(err) = fs::remove_dir_all(&space_dir)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!(
            "Failed to delete game space directory {}: {}",
            sanitize_log_path(&space_dir),
            err
        ));
    }

    registry.game_spaces.retain(|space| space.id != entry.id);
    if registry.last_opened_game_space_id == entry.id {
        registry.last_opened_game_space_id = registry.active_game_space_id.clone();
    }
    save_games_registry(&path, &registry)?;
    Ok(entry)
}

pub fn seed_new_game_space_settings(
    entry: &GameSpaceEntry,
    steam_directory: &str,
) -> Option<String> {
    let mut defaults = match serde_json::to_value(crate::ui::types::SettingsViewState::default()) {
        Ok(value) => value,
        Err(err) => {
            log::warn!("Failed to build default game settings: {}", err);
            return None;
        }
    };
    let mut detected_dir = None;
    if let Some(module) = crate::core::game::registry().get(&entry.game_id)
        && let Some(install_setting) = module.settings_schema().install_dir_setting()
        && let Some(install_dir) =
            module.detect_install_dir(&crate::core::game::GameDetectCtx { steam_directory })
    {
        let install_dir = install_dir.display().to_string();
        defaults[install_setting.id] = serde_json::Value::String(install_dir.clone());
        detected_dir = Some(install_dir);
        log::info!("Pre-filled the detected install directory for the new game space");
    }
    let game_settings_path = game_space_dir_for(&entry.id).join(GAME_SETTINGS_FILE);
    if let Err(err) = write_game_settings_half(&defaults, &game_settings_path) {
        log::warn!(
            "Failed to seed game settings for the new game space: {}",
            err
        );
    }
    detected_dir
}

pub(crate) fn slug_from_display_name(display_name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = true;
    for ch in display_name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "space".to_string()
    } else {
        slug
    }
}

fn unique_space_id(root: &Path, registry: &GamesRegistryFile, display_name: &str) -> String {
    let mut base = slug_from_display_name(display_name);
    base.truncate(MAX_GAME_SPACE_ID_BASE_LEN);
    let mut base = base.trim_end_matches('-').to_string();
    if !is_valid_game_space_id(&base) {
        base = format!("{}-space", base);
    }
    let mut candidate = base.clone();
    let mut suffix = 2;
    while registry.entry(&candidate).is_some() || game_space_dir_at(root, &candidate).exists() {
        candidate = format!("{}-{}", base, suffix);
        suffix += 1;
    }
    candidate
}

/// Prepare the game-spaces on-disk layout for the current data root and pin
/// the active game space for this process. Runs the one-shot legacy migration
/// when `games.json` is absent. Call before any settings, repository, or
/// database file is read.
pub fn ensure_game_spaces_layout() {
    match ensure_layout_at(&app_paths::foxy_data_dir()) {
        Ok(active) => {
            set_process_active_game_space(active);
            let active = active_game_space();
            log::info!(
                "Active game space: {} (game {})",
                active.space_id,
                active.game_id
            );
        }
        Err(err) => {
            log::error!("Failed to prepare the game space layout: {}", err);
        }
    }
}

fn ensure_layout_at(root: &Path) -> Result<ActiveGameSpace, String> {
    let registry_path = games_registry_path(root);
    let registry = if registry_path.exists() {
        match load_games_registry(&registry_path) {
            Ok(registry) => sanitize_loaded_registry(&registry_path, registry)?,
            Err(err) => {
                log::warn!(
                    "Games registry is unreadable ({}); rewriting the default single-space registry",
                    err
                );
                let registry = GamesRegistryFile::single_space_default();
                save_games_registry(&registry_path, &registry)?;
                registry
            }
        }
    } else {
        let space_dir = game_space_dir_at(root, DEFAULT_GAME_SPACE_ID);
        fs::create_dir_all(&space_dir).map_err(|err| {
            format!(
                "Failed to create game space directory {}: {}",
                sanitize_log_path(&space_dir),
                err
            )
        })?;
        let migrated = migration::migrate_legacy_layout(root, &space_dir)?;
        if migrated {
            log::info!(
                "Migrated legacy data layout into game space {}",
                DEFAULT_GAME_SPACE_ID
            );
        }
        let registry = GamesRegistryFile::single_space_default();
        save_games_registry(&registry_path, &registry)?;
        registry
    };

    let active_entry = registry
        .resolve_active_entry()
        .expect("sanitized games registry always has at least one space");
    let active = ActiveGameSpace::from(active_entry);
    let space_dir = game_space_dir_at(root, &active.space_id);
    fs::create_dir_all(&space_dir).map_err(|err| {
        format!(
            "Failed to create game space directory {}: {}",
            sanitize_log_path(&space_dir),
            err
        )
    })?;
    for orphan in orphan_space_dir_names(root, &registry) {
        log::warn!(
            "Game space directory {:?} has no games.json entry (a rewritten registry or an interrupted create can leave one); Foxy ignores it, delete the folder to reclaim the space",
            orphan
        );
    }
    Ok(active)
}

/// Directories under `games/` that no registry entry claims. They cannot be
/// adopted automatically because the owning game id is not recoverable from
/// the directory alone.
fn orphan_space_dir_names(root: &Path, registry: &GamesRegistryFile) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root.join(GAMES_DIR_NAME)) else {
        return Vec::new();
    };
    let mut orphans = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if registry.entry(&name).is_none() {
            orphans.push(name);
        }
    }
    orphans.sort();
    orphans
}

/// Repair a parsed registry so an active entry always resolves: entries whose
/// id is not a safe path slug are dropped before any filesystem use, an empty
/// space list gets the default space, and an active id that names no entry is
/// re-pointed at the resolvable one. Repairs are persisted.
fn sanitize_loaded_registry(
    registry_path: &Path,
    mut registry: GamesRegistryFile,
) -> Result<GamesRegistryFile, String> {
    let mut changed = false;
    let space_count = registry.game_spaces.len();
    registry.game_spaces.retain(|entry| {
        let valid = is_valid_game_space_id(&entry.id);
        if !valid {
            log::warn!(
                "Dropping game space entry with unsafe id {:?}; its data directory (if any) is left in place",
                entry.id
            );
        }
        valid
    });
    changed |= registry.game_spaces.len() != space_count;
    if registry.game_spaces.is_empty() {
        log::warn!("Games registry lists no game spaces; restoring the default space");
        registry.game_spaces.push(default_game_space_entry());
        changed = true;
    }
    let resolved_id = registry
        .resolve_active_entry()
        .map(|entry| entry.id.clone())
        .expect("non-empty registry resolves an active entry");
    if registry.active_game_space_id != resolved_id {
        log::warn!(
            "Active game space {} not found; falling back to {}",
            registry.active_game_space_id,
            resolved_id
        );
        registry.active_game_space_id = resolved_id;
        changed = true;
    }
    if registry
        .entry(&registry.last_opened_game_space_id)
        .is_none()
    {
        registry.last_opened_game_space_id = registry.active_game_space_id.clone();
        changed = true;
    }
    if changed {
        save_games_registry(registry_path, &registry)?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn games_registry_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(GAMES_REGISTRY_FILE);
        let registry = GamesRegistryFile::single_space_default();

        save_games_registry(&path, &registry).expect("save");
        let loaded = load_games_registry(&path).expect("load");

        assert_eq!(loaded, registry);
        assert_eq!(loaded.active_game_space_id, DEFAULT_GAME_SPACE_ID);
        assert_eq!(loaded.game_spaces.len(), 1);
    }

    #[test]
    fn ensure_layout_bootstraps_fresh_install() {
        let dir = tempfile::tempdir().expect("temp dir");

        let active = ensure_layout_at(dir.path()).expect("layout");

        assert_eq!(active.space_id, DEFAULT_GAME_SPACE_ID);
        assert_eq!(active.game_id, DEFAULT_GAME_SPACE_ID);
        assert!(dir.path().join(GAMES_REGISTRY_FILE).is_file());
        assert!(
            dir.path()
                .join(GAMES_DIR_NAME)
                .join(DEFAULT_GAME_SPACE_ID)
                .is_dir()
        );
        assert!(!dir.path().join(APP_SETTINGS_FILE).exists());
    }

    #[test]
    fn ensure_layout_is_idempotent_and_keeps_existing_registry() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("first layout");
        let registry_path = dir.path().join(GAMES_REGISTRY_FILE);
        let first = std::fs::read_to_string(&registry_path).expect("registry");

        ensure_layout_at(dir.path()).expect("second layout");

        let second = std::fs::read_to_string(&registry_path).expect("registry");
        assert_eq!(first, second);
    }

    #[test]
    fn ensure_layout_rewrites_corrupt_registry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let registry_path = dir.path().join(GAMES_REGISTRY_FILE);
        std::fs::write(&registry_path, "{broken").expect("write");

        let active = ensure_layout_at(dir.path()).expect("layout");

        assert_eq!(active.space_id, DEFAULT_GAME_SPACE_ID);
        let loaded = load_games_registry(&registry_path).expect("load");
        assert_eq!(loaded.active_game_space_id, DEFAULT_GAME_SPACE_ID);
    }

    #[test]
    fn ensure_layout_does_not_rerun_migration_once_registry_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("first layout");

        // A settings.json appearing after migration (e.g. an old build ran)
        // must not be re-split once the registry exists.
        std::fs::write(
            dir.path().join("settings.json"),
            json!({"locale": "en"}).to_string(),
        )
        .expect("write");
        ensure_layout_at(dir.path()).expect("second layout");

        assert!(dir.path().join("settings.json").is_file());
        assert!(!dir.path().join(APP_SETTINGS_FILE).exists());
    }

    #[test]
    fn ensure_layout_loads_the_persisted_active_space() {
        let dir = tempfile::tempdir().expect("temp dir");
        let registry_path = dir.path().join(GAMES_REGISTRY_FILE);
        let mut registry = GamesRegistryFile::single_space_default();
        registry.game_spaces.push(GameSpaceEntry {
            id: "arma3-alt".to_string(),
            game_id: "arma3".to_string(),
            display_name: "Arma 3 Alt".to_string(),
            created_at: 0,
        });
        registry.active_game_space_id = "arma3-alt".to_string();
        save_games_registry(&registry_path, &registry).expect("save");

        let active = ensure_layout_at(dir.path()).expect("layout");

        assert_eq!(active.space_id, "arma3-alt");
        assert_eq!(active.display_name, "Arma 3 Alt");
        assert!(dir.path().join(GAMES_DIR_NAME).join("arma3-alt").is_dir());
    }

    #[test]
    fn ensure_layout_repairs_unknown_active_space_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let registry_path = dir.path().join(GAMES_REGISTRY_FILE);
        let mut registry = GamesRegistryFile::single_space_default();
        registry.active_game_space_id = "missing".to_string();
        registry.last_opened_game_space_id = "missing".to_string();
        save_games_registry(&registry_path, &registry).expect("save");

        let active = ensure_layout_at(dir.path()).expect("layout");

        assert_eq!(active.space_id, DEFAULT_GAME_SPACE_ID);
        let repaired = load_games_registry(&registry_path).expect("load");
        assert_eq!(repaired.active_game_space_id, DEFAULT_GAME_SPACE_ID);
        assert_eq!(repaired.last_opened_game_space_id, DEFAULT_GAME_SPACE_ID);
    }

    #[test]
    fn create_game_space_appends_a_unique_slug_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");

        let first = create_game_space_at(dir.path(), "arma3", "My Second Setup").expect("create");
        let second = create_game_space_at(dir.path(), "arma3", "My Second Setup").expect("create");

        assert_eq!(first.id, "my-second-setup");
        assert_eq!(second.id, "my-second-setup-2");
        assert!(dir.path().join(GAMES_DIR_NAME).join(&first.id).is_dir());
        let registry =
            load_games_registry(&dir.path().join(GAMES_REGISTRY_FILE)).expect("registry");
        assert_eq!(registry.game_spaces.len(), 3);
        // Creating never changes what loads next launch.
        assert_eq!(registry.active_game_space_id, DEFAULT_GAME_SPACE_ID);
    }

    #[test]
    fn create_game_space_rejects_unknown_game_and_empty_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");

        assert!(create_game_space_at(dir.path(), "not-a-game", "Name").is_err());
        assert!(create_game_space_at(dir.path(), "arma3", "   ").is_err());
    }

    #[test]
    fn set_active_game_space_swaps_active_and_last_opened() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");
        let created = create_game_space_at(dir.path(), "arma3", "Second").expect("create");

        let opened = set_active_game_space_at(dir.path(), &created.id).expect("set active");

        assert_eq!(opened.id, created.id);
        let registry =
            load_games_registry(&dir.path().join(GAMES_REGISTRY_FILE)).expect("registry");
        assert_eq!(registry.active_game_space_id, created.id);
        assert_eq!(registry.last_opened_game_space_id, DEFAULT_GAME_SPACE_ID);
        assert!(set_active_game_space_at(dir.path(), "missing").is_err());
    }

    #[test]
    fn remove_game_space_deletes_workspace_and_entry_but_never_the_active_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");
        let created = create_game_space_at(dir.path(), "arma3", "Second").expect("create");
        let space_dir = dir.path().join(GAMES_DIR_NAME).join(&created.id);
        std::fs::write(space_dir.join("game_settings.json"), "{}").expect("seed file");

        assert!(remove_game_space_at(dir.path(), DEFAULT_GAME_SPACE_ID).is_err());
        remove_game_space_at(dir.path(), &created.id).expect("remove");

        assert!(!space_dir.exists());
        let registry =
            load_games_registry(&dir.path().join(GAMES_REGISTRY_FILE)).expect("registry");
        assert_eq!(registry.game_spaces.len(), 1);
        assert!(remove_game_space_at(dir.path(), &created.id).is_err());
    }

    #[test]
    fn game_space_id_validation_accepts_slugs_and_rejects_path_shapes() {
        for valid in ["arma3", "my-second-setup", "space-2", "3rd-setup"] {
            assert!(is_valid_game_space_id(valid), "{valid} should be valid");
        }
        for invalid in [
            "", "..", "a/b", "a\\b", "c:", "C:\\evil", "/abs", "Arma3", "-lead", "trail-",
            "space id", "con", "nul", "com1", "lpt9",
        ] {
            assert!(
                !is_valid_game_space_id(invalid),
                "{invalid:?} should be invalid"
            );
        }
        assert!(!is_valid_game_space_id(&"a".repeat(65)));
    }

    #[test]
    fn ensure_layout_drops_registry_entries_with_unsafe_ids() {
        let dir = tempfile::tempdir().expect("temp dir");
        let registry_path = dir.path().join(GAMES_REGISTRY_FILE);
        let mut registry = GamesRegistryFile::single_space_default();
        registry.game_spaces.push(GameSpaceEntry {
            id: "../escape".to_string(),
            game_id: "arma3".to_string(),
            display_name: "Escape".to_string(),
            created_at: 0,
        });
        registry.active_game_space_id = "../escape".to_string();
        save_games_registry(&registry_path, &registry).expect("save");

        let active = ensure_layout_at(dir.path()).expect("layout");

        assert_eq!(active.space_id, DEFAULT_GAME_SPACE_ID);
        let repaired = load_games_registry(&registry_path).expect("load");
        assert_eq!(repaired.game_spaces.len(), 1);
        assert_eq!(repaired.active_game_space_id, DEFAULT_GAME_SPACE_ID);
        assert!(!dir.path().join("escape").exists());
    }

    #[test]
    fn remove_and_set_active_reject_unsafe_ids_before_any_filesystem_use() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");
        // Simulate a hand-edited registry that sneaks in a traversal id.
        let registry_path = dir.path().join(GAMES_REGISTRY_FILE);
        let mut registry = load_games_registry(&registry_path).expect("load");
        registry.game_spaces.push(GameSpaceEntry {
            id: "../victim".to_string(),
            game_id: "arma3".to_string(),
            display_name: "Victim".to_string(),
            created_at: 0,
        });
        save_games_registry(&registry_path, &registry).expect("save");
        let victim_dir = dir.path().join("victim");
        std::fs::create_dir_all(&victim_dir).expect("victim dir");

        assert!(remove_game_space_at(dir.path(), "../victim").is_err());
        assert!(set_active_game_space_at(dir.path(), "../victim").is_err());
        assert!(victim_dir.is_dir());
    }

    #[test]
    fn orphan_space_dirs_are_reported_but_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");
        let orphan_dir = dir.path().join(GAMES_DIR_NAME).join("leftover");
        std::fs::create_dir_all(&orphan_dir).expect("orphan dir");
        std::fs::write(orphan_dir.join("game_settings.json"), "{}").expect("orphan file");

        let registry = load_games_registry(&dir.path().join(GAMES_REGISTRY_FILE)).expect("load");
        let orphans = orphan_space_dir_names(dir.path(), &registry);
        ensure_layout_at(dir.path()).expect("layout with orphan present");

        assert_eq!(orphans, vec!["leftover".to_string()]);
        assert!(orphan_dir.join("game_settings.json").is_file());
    }

    #[test]
    fn unique_space_id_avoids_reserved_names_and_caps_length() {
        let dir = tempfile::tempdir().expect("temp dir");
        ensure_layout_at(dir.path()).expect("layout");
        let registry = load_games_registry(&dir.path().join(GAMES_REGISTRY_FILE)).expect("load");

        let reserved = unique_space_id(dir.path(), &registry, "CON");
        let long = unique_space_id(dir.path(), &registry, &"very long name ".repeat(20));

        assert_eq!(reserved, "con-space");
        assert!(is_valid_game_space_id(&reserved));
        assert!(is_valid_game_space_id(&long));
        assert!(long.len() <= MAX_GAME_SPACE_ID_LEN);
    }

    #[test]
    fn slugs_are_lowercase_ascii_with_single_dashes() {
        assert_eq!(slug_from_display_name("My Second Setup"), "my-second-setup");
        assert_eq!(slug_from_display_name("  Arma 3!!  "), "arma-3");
        assert_eq!(slug_from_display_name("***"), "space");
        assert_eq!(
            slug_from_display_name("Průzkum jednotky"),
            "pr-zkum-jednotky"
        );
    }
}
