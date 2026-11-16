use std::path::{Path, PathBuf};

use crate::core::game::spaces::{
    APP_SETTINGS_FILE, GAMES_DIR_NAME, GAMES_REGISTRY_FILE, GameSpaceEntry, GamesRegistryFile,
    is_valid_game_space_id,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerContext {
    data_root: PathBuf,
    space_id: String,
    game_id: String,
    display_name: String,
}

impl ServerContext {
    pub fn capture(root: &Path, selected_space: Option<&str>) -> Result<Self, String> {
        if root.as_os_str().is_empty() {
            return Err("The config root must be non-empty".to_string());
        }
        if selected_space.is_some_and(|id| !is_valid_game_space_id(id)) {
            return Err("Invalid game space id".to_string());
        }
        let data_root =
            std::path::absolute(root).map_err(|_| "Cannot resolve the config root".to_string())?;
        let bytes = std::fs::read(data_root.join(GAMES_REGISTRY_FILE))
            .map_err(|error| format!("Cannot read games.json: {}", error.kind()))?;
        let registry: GamesRegistryFile =
            serde_json::from_slice(&bytes).map_err(|_| "Cannot parse games.json".to_string())?;
        let space_id = selected_space.unwrap_or(&registry.active_game_space_id);
        if !is_valid_game_space_id(space_id) {
            return Err("Invalid game space id in games.json".to_string());
        }
        let mut matches = registry
            .game_spaces
            .iter()
            .filter(|entry| entry.id == space_id);
        let entry = matches
            .next()
            .ok_or_else(|| "Game space does not exist".to_string())?;
        if matches.next().is_some() {
            return Err("Game space id is ambiguous in games.json".to_string());
        }
        if crate::core::game::registry().get(&entry.game_id).is_none() {
            return Err("Game space uses an unregistered game module".to_string());
        }
        Ok(Self::from_entry(data_root, entry))
    }

    fn from_entry(data_root: PathBuf, entry: &GameSpaceEntry) -> Self {
        Self {
            data_root,
            space_id: entry.id.clone(),
            game_id: entry.game_id.clone(),
            display_name: entry.display_name.clone(),
        }
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub fn space_id(&self) -> &str {
        &self.space_id
    }

    pub fn game_id(&self) -> &str {
        &self.game_id
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn space_dir(&self) -> PathBuf {
        self.data_root.join(GAMES_DIR_NAME).join(&self.space_id)
    }

    pub fn database_path(&self) -> PathBuf {
        self.space_dir().join("database.db")
    }

    pub fn app_settings_path(&self) -> PathBuf {
        self.data_root.join(APP_SETTINGS_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registry_bytes() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "active_game_space_id": "alpha",
            "last_opened_game_space_id": "beta",
            "game_spaces": [
                {"id":"alpha", "game_id":"arma3", "display_name":"Alpha", "created_at":0},
                {"id":"beta", "game_id":"reforger", "display_name":"Beta", "created_at":0}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn capture_pins_an_explicit_space_without_remembering_or_creating_it() {
        let root = tempfile::tempdir().unwrap();
        let bytes = registry_bytes();
        std::fs::write(root.path().join(GAMES_REGISTRY_FILE), &bytes).unwrap();
        let selected = ServerContext::capture(root.path(), Some("beta")).unwrap();
        let remembered = ServerContext::capture(root.path(), None).unwrap();
        assert_eq!(remembered.space_id(), "alpha");
        assert_eq!(selected.space_id(), "beta");
        assert_eq!(selected.game_id(), "reforger");
        assert_eq!(
            selected.database_path(),
            root.path().join("games/beta/database.db")
        );
        assert_eq!(
            selected.app_settings_path(),
            root.path().join(APP_SETTINGS_FILE)
        );
        assert_eq!(
            std::fs::read(root.path().join(GAMES_REGISTRY_FILE)).unwrap(),
            bytes
        );
        assert!(!root.path().join(GAMES_DIR_NAME).exists());
    }

    #[test]
    fn missing_corrupt_and_legacy_roots_are_never_repaired() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert!(ServerContext::capture(&missing, None).is_err());
        assert!(!missing.exists());
        std::fs::write(root.path().join("settings.json"), b"{}").unwrap();
        assert!(ServerContext::capture(root.path(), None).is_err());
        assert!(!root.path().join(GAMES_REGISTRY_FILE).exists());
        let path = root.path().join(GAMES_REGISTRY_FILE);
        std::fs::write(&path, b"{").unwrap();
        assert!(ServerContext::capture(root.path(), None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{");
        assert_eq!(
            std::fs::read(root.path().join("settings.json")).unwrap(),
            b"{}"
        );
        assert!(!root.path().join(GAMES_DIR_NAME).exists());
    }

    #[test]
    fn unsafe_missing_ambiguous_and_unknown_spaces_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(GAMES_REGISTRY_FILE);
        std::fs::write(&path, registry_bytes()).unwrap();
        for id in ["../beta", "con", "Beta", "missing"] {
            assert!(ServerContext::capture(root.path(), Some(id)).is_err());
        }
        let mut registry: GamesRegistryFile = serde_json::from_slice(&registry_bytes()).unwrap();
        registry.game_spaces.push(registry.game_spaces[0].clone());
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
        assert!(ServerContext::capture(root.path(), None).is_err());
        registry.game_spaces.pop();
        registry.game_spaces[0].game_id = "unknown".to_string();
        std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
        assert!(ServerContext::capture(root.path(), None).is_err());
        assert!(!root.path().join(GAMES_DIR_NAME).exists());
    }
}
