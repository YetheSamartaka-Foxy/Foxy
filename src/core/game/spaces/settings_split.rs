use std::io::ErrorKind;
use std::path::Path;

use serde_json::{Map, Value};

use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::atomic_write;

/// Settings fields that belong to the active game space (`game_settings.json`).
/// Everything else in the settings object is app-global (`app_settings.json`).
/// Keys must match the serde field names on `SettingsViewState`.
///
/// Moving a key between halves needs no migration: reads merge both files and
/// the next save re-partitions every key by this list, so a moved key leaves
/// its old file automatically.
pub const GAME_SPACE_SETTINGS_KEYS: &[&str] = &[
    "arma3_directory",
    "twwh3_directory",
    "reforger_directory",
    "arma3_profiles_directory",
    "teamspeak3_directory",
    "apply_repo_json_client_parameters",
    "apply_repo_json_dlc_content",
    "warn_editor_external_addons",
    "enable_editor_mission_list",
    "enable_server_list",
    "check_server_addons_before_join",
    "check_ts3_running_before_join",
    "check_steam_running_before_launch",
    "ts3_installed_plugin_hashes",
    "ts3_plugin_statuses",
    "swifty_migration_offered",
    // Additional search folders point at game-specific addon sources, so each
    // game space keeps its own list.
    "additional_folders",
    "additional_folder_aliases",
    // Repository-scoped update state: these reference the space's own
    // repositories, so they must not bleed into other spaces on a switch.
    "update_summary_notices",
    "active_update_sessions",
    // Scheduled jobs target `(remote_url, local_path)` repository instances,
    // which only exist inside one game space; cleanup folders list that game's
    // addon directories. Both are meaningless against another space.
    "scheduled_jobs",
    "cleanup_folders",
];

fn is_game_space_key(key: &str) -> bool {
    GAME_SPACE_SETTINGS_KEYS.contains(&key)
}

/// Partition a full settings object into (app-global, game-space) objects.
pub fn split_settings_value(value: &Value) -> (Value, Value) {
    let Some(object) = value.as_object() else {
        return (value.clone(), Value::Object(Map::new()));
    };

    let mut app = Map::new();
    let mut game = Map::new();
    for (key, field) in object {
        if is_game_space_key(key) {
            game.insert(key.clone(), field.clone());
        } else {
            app.insert(key.clone(), field.clone());
        }
    }
    (Value::Object(app), Value::Object(game))
}

/// Merge the two settings files back into one object. The key sets are
/// disjoint by construction; on overlap the game-space value wins.
pub fn merge_settings_values(app: Value, game: Value) -> Value {
    let mut merged = match app {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    if let Value::Object(game) = game {
        for (key, field) in game {
            merged.insert(key, field);
        }
    }
    Value::Object(merged)
}

pub fn merge_value_over_defaults(mut defaults: Value, saved: Value) -> Value {
    merge_value_into(&mut defaults, saved);
    defaults
}

fn merge_value_into(target: &mut Value, saved: Value) {
    match (target, saved) {
        (Value::Object(target), Value::Object(saved)) => {
            for (key, value) in saved {
                if let Some(target_value) = target.get_mut(&key) {
                    merge_value_into(target_value, value);
                } else {
                    target.insert(key, value);
                }
            }
        }
        (target, saved) => *target = saved,
    }
}

fn read_optional_value(path: &Path) -> Result<Option<Value>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "Failed to read {}: {}",
                sanitize_log_path(path),
                err
            ));
        }
    };
    serde_json::from_str::<Value>(&raw)
        .map(Some)
        .map_err(|err| format!("Failed to parse {}: {}", sanitize_log_path(path), err))
}

/// Read and merge the split settings files. `Ok(None)` means neither file
/// exists (first run); parse and IO failures are errors so callers keep the
/// existing "leave the broken file alone" behavior.
pub fn read_merged_settings_value(
    app_path: &Path,
    game_path: &Path,
) -> Result<Option<Value>, String> {
    let app = read_optional_value(app_path)?;
    let game = read_optional_value(game_path)?;
    match (app, game) {
        (None, None) => Ok(None),
        (app, game) => Ok(Some(merge_settings_values(
            app.unwrap_or_else(|| Value::Object(Map::new())),
            game.unwrap_or_else(|| Value::Object(Map::new())),
        ))),
    }
}

/// Split a full settings object and write both halves atomically.
pub fn write_split_settings(
    value: &Value,
    app_path: &Path,
    game_path: &Path,
) -> Result<(), String> {
    let (app, game) = split_settings_value(value);
    for (path, half) in [(app_path, &app), (game_path, &game)] {
        write_settings_half(path, half)?;
    }
    Ok(())
}

/// Split a full settings object and write only the game-space half. Used to
/// seed a newly created game space without touching `app_settings.json`.
pub fn write_game_settings_half(value: &Value, game_path: &Path) -> Result<(), String> {
    let (_, game) = split_settings_value(value);
    write_settings_half(game_path, &game)
}

pub(super) fn write_settings_half(path: &Path, half: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "Failed to create settings directory {}: {}",
                sanitize_log_path(parent),
                err
            )
        })?;
    }
    let serialized = serde_json::to_string_pretty(half)
        .map_err(|err| format!("Failed to serialize {}: {}", sanitize_log_path(path), err))?;
    atomic_write(path, serialized.as_bytes())
        .map_err(|err| format!("Failed to write {}: {}", sanitize_log_path(path), err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_partitions_game_keys_from_app_keys() {
        let full = json!({
            "locale": "en",
            "arma3_directory": "C:\\Arma3",
            "twwh3_directory": "C:\\WH3",
            "reforger_directory": "C:\\Reforger",
            "steam_directory": "C:\\Steam",
            "swifty_migration_offered": true,
            "additional_folders": ["C:\\Addons"],
            "additional_folder_aliases": {"c:\\addons": "Extras"},
            "update_summary_notices": [{"repository_url": "https://repo.example/main/"}],
            "unknown_future_field": 42,
        });

        let (app, game) = split_settings_value(&full);

        assert_eq!(
            app,
            json!({
                "locale": "en",
                "steam_directory": "C:\\Steam",
                "unknown_future_field": 42,
            })
        );
        assert_eq!(
            game,
            json!({
                "arma3_directory": "C:\\Arma3",
                "twwh3_directory": "C:\\WH3",
                "reforger_directory": "C:\\Reforger",
                "swifty_migration_offered": true,
                "additional_folders": ["C:\\Addons"],
                "additional_folder_aliases": {"c:\\addons": "Extras"},
                "update_summary_notices": [{"repository_url": "https://repo.example/main/"}],
            })
        );
    }

    /// The key list is matched against serde field names at runtime, so a
    /// rename (or a typo) would silently reclassify a setting as app-global and
    /// bleed it across game spaces. Nothing else catches that.
    #[test]
    fn every_game_space_key_exists_on_settings_view_state() {
        let defaults = serde_json::to_value(crate::ui::types::SettingsViewState::default())
            .expect("settings serialize");
        let object = defaults
            .as_object()
            .expect("settings serialize to an object");

        let unknown: Vec<&str> = GAME_SPACE_SETTINGS_KEYS
            .iter()
            .copied()
            .filter(|key| !object.contains_key(*key))
            .collect();

        assert!(
            unknown.is_empty(),
            "GAME_SPACE_SETTINGS_KEYS names fields that SettingsViewState does not serialize: {unknown:?}"
        );
    }

    #[test]
    fn game_space_key_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for key in GAME_SPACE_SETTINGS_KEYS {
            assert!(seen.insert(*key), "duplicate game-space settings key {key}");
        }
    }

    #[test]
    fn merge_recombines_split_halves() {
        let full = json!({
            "locale": "cs",
            "arma3_directory": "D:\\Games\\Arma 3",
            "ts3_installed_plugin_hashes": {"plugin.dll": "abc"},
        });

        let (app, game) = split_settings_value(&full);
        let merged = merge_settings_values(app, game);

        assert_eq!(merged, full);
    }

    #[test]
    fn merge_value_over_defaults_keeps_missing_defaults() {
        let defaults = json!({
            "debug_mode": false,
            "locale": "en",
            "font_sizes": {
                "body": 14,
                "title": 22
            }
        });
        let saved = json!({
            "locale": "cs",
            "font_sizes": {
                "body": 16
            },
            "twwh3_directory": "C:\\WH3"
        });

        let merged = merge_value_over_defaults(defaults, saved);

        assert_eq!(
            merged,
            json!({
                "debug_mode": false,
                "locale": "cs",
                "font_sizes": {
                    "body": 16,
                    "title": 22
                },
                "twwh3_directory": "C:\\WH3"
            })
        );
    }

    #[test]
    fn read_merged_returns_none_when_neither_file_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let merged = read_merged_settings_value(
            &dir.path().join("app_settings.json"),
            &dir.path().join("game_settings.json"),
        )
        .expect("read should succeed");
        assert!(merged.is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app_path = dir.path().join("app_settings.json");
        let game_path = dir
            .path()
            .join("games")
            .join("arma3")
            .join("game_settings.json");
        let full = json!({
            "locale": "en",
            "arma3_directory": "C:\\Arma3",
            "enable_server_list": true,
        });

        write_split_settings(&full, &app_path, &game_path).expect("write should succeed");
        let merged = read_merged_settings_value(&app_path, &game_path)
            .expect("read should succeed")
            .expect("files should exist");

        assert_eq!(merged, full);
        let app_raw = std::fs::read_to_string(&app_path).expect("app file");
        assert!(!app_raw.contains("arma3_directory"));
    }

    #[test]
    fn write_game_settings_half_writes_only_the_game_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let game_path = dir
            .path()
            .join("games")
            .join("new")
            .join("game_settings.json");
        let full = json!({
            "locale": "en",
            "arma3_directory": "C:\\Arma3",
            "enable_server_list": true,
        });

        write_game_settings_half(&full, &game_path).expect("write should succeed");

        let game_raw = std::fs::read_to_string(&game_path).expect("game file");
        let game: Value = serde_json::from_str(&game_raw).expect("parse");
        assert_eq!(game["arma3_directory"], "C:\\Arma3");
        assert!(game.get("locale").is_none());
        assert!(!dir.path().join("app_settings.json").exists());
    }

    #[test]
    fn read_merged_reports_parse_errors() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app_path = dir.path().join("app_settings.json");
        std::fs::write(&app_path, "{not json").expect("write");

        let result = read_merged_settings_value(&app_path, &dir.path().join("game_settings.json"));

        assert!(result.is_err());
    }
}
