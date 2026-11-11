use std::fs;
use std::path::Path;

use serde_json::Value;

use super::settings_split;
use super::{APP_SETTINGS_FILE, GAME_SETTINGS_FILE};
use crate::core::tasks::db_turso::{
    DATABASE_ARTIFACT_FILE_NAMES, DATABASE_REBUILD_BACKUP_PREFIX, WIPE_MARKER_FILE_NAME,
};
use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::atomic_write;

const LEGACY_SETTINGS_FILE: &str = "settings.json";
const LEGACY_BACKUP_SUFFIX: &str = ".pre-gamespaces.bak";
const LEGACY_REVIEW_FILE: &str = "legacy_migration_review.json";
const REVIEWABLE_JSON_FILES: [&str; 3] = [
    "repositories.json",
    "repository_spaces.json",
    "repository_visual_folders.json",
];

/// Data files that move into the game space dir with a small backup copy left
/// at the legacy location so a rollback to an older build keeps working.
const MOVED_WITH_BACKUP: [&str; 3] = [
    "repositories.json",
    "repository_spaces.json",
    "repository_visual_folders.json",
];

/// Regenerable sidecars and caches that move without a backup copy (the
/// database can be many GB, the rest is derived state).
const MOVED_WITHOUT_BACKUP: [&str; 3] = [
    "db_meta.json",
    "quick_scan_addon_hash_cache.json",
    WIPE_MARKER_FILE_NAME,
];

const MOVED_DIRECTORIES: [&str; 1] = ["images"];

/// One-shot move of the legacy flat data layout into `games/<id>/`.
/// Idempotent: files already at the destination are skipped (with the legacy
/// copy left in place); user data is never deleted. Returns whether anything
/// was migrated.
pub(super) fn migrate_legacy_layout(root: &Path, space_dir: &Path) -> Result<bool, String> {
    let mut migrated = false;
    let mut review_files = Vec::new();

    for name in MOVED_WITH_BACKUP {
        if REVIEWABLE_JSON_FILES.contains(&name)
            && has_divergent_valid_json(name, &root.join(name), &space_dir.join(name))?
        {
            review_files.push(name.to_string());
        }
        migrated |= move_legacy_entry(&root.join(name), &space_dir.join(name), true)?;
    }
    for name in MOVED_WITHOUT_BACKUP {
        migrated |= move_legacy_entry(&root.join(name), &space_dir.join(name), false)?;
    }
    for name in DATABASE_ARTIFACT_FILE_NAMES {
        migrated |= move_legacy_entry(&root.join(name), &space_dir.join(name), false)?;
    }
    for name in MOVED_DIRECTORIES {
        migrated |= move_legacy_entry(&root.join(name), &space_dir.join(name), false)?;
    }
    migrated |= move_rebuild_backups(root, space_dir)?;
    migrated |= migrate_legacy_settings(root, space_dir)?;
    migrated |= persist_legacy_review(space_dir, review_files)?;

    Ok(migrated)
}

fn has_divergent_valid_json(name: &str, source: &Path, destination: &Path) -> Result<bool, String> {
    if !source.is_file() || !destination.is_file() {
        return Ok(false);
    }
    let (Ok(source_text), Ok(destination_text)) =
        (fs::read_to_string(source), fs::read_to_string(destination))
    else {
        return Ok(false);
    };
    let source_value = serde_json::from_str::<Value>(&source_text).ok();
    let destination_value = serde_json::from_str::<Value>(&destination_text).ok();
    let required_strings: &[&str] = if name == "repositories.json" {
        &["name", "address", "path"]
    } else {
        &["id", "name"]
    };
    let is_setup = |value: &Value| {
        value.as_array().is_some_and(|entries| {
            entries.iter().all(|entry| {
                required_strings.iter().all(|field| {
                    entry
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.trim().is_empty())
                })
            })
        })
    };
    Ok(
        matches!((source_value, destination_value), (Some(source), Some(destination))
        if is_setup(&source) && is_setup(&destination) && source != destination),
    )
}

fn persist_legacy_review(space_dir: &Path, files: Vec<String>) -> Result<bool, String> {
    if files.is_empty() {
        return Ok(false);
    }
    let path = space_dir.join(LEGACY_REVIEW_FILE);
    let mut existing = match fs::read(&path) {
        Ok(bytes) => parse_legacy_review(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(format!(
                "Failed to read legacy migration review marker: {error}"
            ));
        }
    };
    let previous_len = existing.len();
    for file in files {
        if REVIEWABLE_JSON_FILES.contains(&file.as_str()) && !existing.contains(&file) {
            existing.push(file);
        }
    }
    if existing.len() == previous_len {
        return Ok(false);
    }
    atomic_write(
        &path,
        &serde_json::to_vec_pretty(&existing).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Failed to save legacy migration review marker: {error}"))?;
    Ok(true)
}

fn parse_legacy_review(bytes: &[u8]) -> Result<Vec<String>, String> {
    let files: Vec<String> = serde_json::from_slice(bytes)
        .map_err(|error| format!("Legacy migration review marker is invalid: {error}"))?;
    if files
        .iter()
        .any(|file| !REVIEWABLE_JSON_FILES.contains(&file.as_str()))
    {
        return Err("Legacy migration review marker contains an unknown filename".to_string());
    }
    Ok(files)
}

pub(crate) fn pending_legacy_migration_review(space_dir: &Path) -> Result<Vec<String>, String> {
    let path = space_dir.join(LEGACY_REVIEW_FILE);
    match fs::read(&path) {
        Ok(bytes) => parse_legacy_review(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!(
            "Failed to read legacy migration review marker: {error}"
        )),
    }
}

pub(crate) fn acknowledge_legacy_migration_review(space_dir: &Path) -> Result<(), String> {
    let files = pending_legacy_migration_review(space_dir)?;
    if files.is_empty() {
        return Ok(());
    }
    fs::remove_file(space_dir.join(LEGACY_REVIEW_FILE))
        .map_err(|error| format!("Failed to clear legacy migration review marker: {error}"))
}

fn move_legacy_entry(src: &Path, dest: &Path, backup: bool) -> Result<bool, String> {
    if !src.exists() {
        return Ok(false);
    }
    if dest.exists() {
        log::warn!(
            "Skipping migration of {}: {} already exists; the legacy copy stays in place",
            sanitize_log_path(src),
            sanitize_log_path(dest)
        );
        return Ok(false);
    }

    if backup {
        let backup_path = backup_path_for(src);
        if backup_path.exists() {
            log::warn!(
                "Legacy backup {} already exists; not overwriting it",
                sanitize_log_path(&backup_path)
            );
        } else if let Err(err) = fs::copy(src, &backup_path) {
            log::warn!(
                "Failed to write legacy backup {}: {}; continuing without it",
                sanitize_log_path(&backup_path),
                err
            );
        }
    }

    fs::rename(src, dest).map_err(|err| {
        format!(
            "Failed to move {} to {}: {}",
            sanitize_log_path(src),
            sanitize_log_path(dest),
            err
        )
    })?;
    log::info!(
        "Migrated {} to {}",
        sanitize_log_path(src),
        sanitize_log_path(dest)
    );
    Ok(true)
}

fn move_rebuild_backups(root: &Path, space_dir: &Path) -> Result<bool, String> {
    let mut migrated = false;
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            return Err(format!(
                "Failed to scan {}: {}",
                sanitize_log_path(root),
                err
            ));
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_string_lossy()
            .starts_with(DATABASE_REBUILD_BACKUP_PREFIX)
        {
            migrated |= move_legacy_entry(&entry.path(), &space_dir.join(&name), false)?;
        }
    }
    Ok(migrated)
}

/// Split the legacy `settings.json` into `app_settings.json` (data root) and
/// `game_settings.json` (game space dir), then rename the original to the
/// `.pre-gamespaces.bak` backup. Each half is written only when it is still
/// missing, so a run interrupted between the two writes finishes on retry
/// without overwriting the half that already landed. An unparseable legacy
/// file is backed up without splitting; the app then starts from default
/// settings, matching the pre-split behavior for a corrupt settings.json.
fn migrate_legacy_settings(root: &Path, space_dir: &Path) -> Result<bool, String> {
    let legacy = root.join(LEGACY_SETTINGS_FILE);
    if !legacy.exists() {
        return Ok(false);
    }

    let app_path = root.join(APP_SETTINGS_FILE);
    let game_path = space_dir.join(GAME_SETTINGS_FILE);
    if !app_path.exists() || !game_path.exists() {
        let raw = fs::read_to_string(&legacy)
            .map_err(|err| format!("Failed to read legacy settings: {}", err))?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(value) => {
                let (app, game) = settings_split::split_settings_value(&value);
                if !app_path.exists() {
                    settings_split::write_settings_half(&app_path, &app)?;
                }
                if !game_path.exists() {
                    settings_split::write_settings_half(&game_path, &game)?;
                }
            }
            Err(err) => {
                log::warn!(
                    "Legacy settings.json is unparseable ({}); backing it up without splitting",
                    err
                );
            }
        }
    }

    let backup_path = backup_path_for(&legacy);
    if backup_path.exists() {
        log::warn!(
            "Legacy settings backup {} already exists; leaving settings.json in place",
            sanitize_log_path(&backup_path)
        );
        return Ok(true);
    }
    fs::rename(&legacy, &backup_path).map_err(|err| {
        format!(
            "Failed to back up legacy settings to {}: {}",
            sanitize_log_path(&backup_path),
            err
        )
    })?;
    Ok(true)
}

fn backup_path_for(src: &Path) -> std::path::PathBuf {
    let mut name = src.file_name().unwrap_or_default().to_os_string();
    name.push(LEGACY_BACKUP_SUFFIX);
    src.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, contents).expect("write file");
    }

    fn space_dir(root: &Path) -> std::path::PathBuf {
        let dir = root.join("games").join("arma3");
        fs::create_dir_all(&dir).expect("space dir");
        dir
    }

    #[test]
    fn migrates_full_legacy_layout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);

        write(
            &root.join("settings.json"),
            &json!({
                "locale": "cs",
                "arma3_directory": "C:\\Arma3",
                "steam_directory": "C:\\Steam",
            })
            .to_string(),
        );
        write(&root.join("repositories.json"), "[]");
        write(&root.join("repository_spaces.json"), "[]");
        write(&root.join("repository_visual_folders.json"), "[]");
        write(&root.join("database.db"), "db-bytes");
        write(&root.join("database.db-wal"), "wal-bytes");
        write(&root.join("db_meta.json"), "{}");
        write(&root.join("quick_scan_addon_hash_cache.json"), "{}");
        write(&root.join(".wipe_database_on_next_start"), "");
        write(&root.join("database.db.rebuild-backup-20260101"), "old");
        write(&root.join("images").join("abc.png"), "png");

        let migrated = migrate_legacy_layout(root, &space).expect("migration");
        assert!(migrated);

        for name in [
            "repositories.json",
            "repository_spaces.json",
            "repository_visual_folders.json",
            "database.db",
            "database.db-wal",
            "db_meta.json",
            "quick_scan_addon_hash_cache.json",
            ".wipe_database_on_next_start",
            "database.db.rebuild-backup-20260101",
        ] {
            assert!(space.join(name).is_file(), "{name} should be in the space");
            assert!(!root.join(name).exists(), "{name} should leave the root");
        }
        assert!(space.join("images").join("abc.png").is_file());
        assert!(!root.join("images").exists());

        assert!(root.join("settings.json.pre-gamespaces.bak").is_file());
        assert!(root.join("repositories.json.pre-gamespaces.bak").is_file());
        assert!(!root.join("settings.json").exists());

        let app: Value = serde_json::from_str(
            &fs::read_to_string(root.join("app_settings.json")).expect("app settings"),
        )
        .expect("parse app settings");
        assert_eq!(app.get("locale"), Some(&json!("cs")));
        assert_eq!(app.get("steam_directory"), Some(&json!("C:\\Steam")));
        assert!(app.get("arma3_directory").is_none());

        let game: Value = serde_json::from_str(
            &fs::read_to_string(space.join("game_settings.json")).expect("game settings"),
        )
        .expect("parse game settings");
        assert_eq!(game.get("arma3_directory"), Some(&json!("C:\\Arma3")));
    }

    #[test]
    fn fresh_install_migrates_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let space = space_dir(dir.path());

        let migrated = migrate_legacy_layout(dir.path(), &space).expect("migration");

        assert!(!migrated);
        assert!(!dir.path().join("app_settings.json").exists());
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);
        write(&root.join("repositories.json"), "[{\"name\":\"a\"}]");

        assert!(migrate_legacy_layout(root, &space).expect("first run"));
        assert!(!migrate_legacy_layout(root, &space).expect("second run"));

        assert_eq!(
            fs::read_to_string(space.join("repositories.json")).expect("moved file"),
            "[{\"name\":\"a\"}]"
        );
    }

    #[test]
    fn existing_destination_is_preserved_and_legacy_copy_stays() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);
        write(&root.join("repositories.json"), "legacy");
        write(&space.join("repositories.json"), "current");

        migrate_legacy_layout(root, &space).expect("migration");

        assert_eq!(
            fs::read_to_string(space.join("repositories.json")).expect("dest"),
            "current"
        );
        assert_eq!(
            fs::read_to_string(root.join("repositories.json")).expect("src"),
            "legacy"
        );
    }

    #[test]
    fn divergent_valid_configs_are_preserved_and_marked_for_review() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);
        let legacy = r#"[{"name":"Legacy","address":"https://repo.example.invalid/legacy/","path":"R:/Mods/RepoAlpha"}]"#;
        let current = r#"[{"name":"Current","address":"https://repo.example.invalid/current/","path":"R:/Mods/RepoBeta"}]"#;
        write(&root.join("repositories.json"), legacy);
        write(&space.join("repositories.json"), current);

        migrate_legacy_layout(root, &space).expect("migration");

        assert_eq!(
            fs::read_to_string(root.join("repositories.json")).unwrap(),
            legacy
        );
        assert_eq!(
            fs::read_to_string(space.join("repositories.json")).unwrap(),
            current
        );
        assert_eq!(
            pending_legacy_migration_review(&space).expect("review marker"),
            vec!["repositories.json"]
        );

        acknowledge_legacy_migration_review(&space).expect("acknowledge");
        assert!(
            pending_legacy_migration_review(&space)
                .expect("review cleared")
                .is_empty()
        );
    }

    #[test]
    fn matching_or_invalid_config_sources_do_not_create_a_review_marker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);
        write(&root.join("repositories.json"), "[]");
        write(&space.join("repositories.json"), "[ ]");
        write(&root.join("repository_spaces.json"), "{broken");
        write(&space.join("repository_spaces.json"), "[]");

        migrate_legacy_layout(root, &space).expect("migration");

        assert!(
            pending_legacy_migration_review(&space)
                .expect("no review marker")
                .is_empty()
        );
    }

    #[test]
    fn interrupted_settings_split_finishes_on_retry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);
        write(
            &root.join("settings.json"),
            &json!({"locale": "cs", "arma3_directory": "C:\\Arma3"}).to_string(),
        );
        // Simulate a crash after the app half landed but before the game half.
        write(
            &root.join("app_settings.json"),
            &json!({"locale": "en"}).to_string(),
        );

        let migrated = migrate_legacy_layout(root, &space).expect("migration");

        assert!(migrated);
        let app: Value = serde_json::from_str(
            &fs::read_to_string(root.join("app_settings.json")).expect("app settings"),
        )
        .expect("parse app settings");
        assert_eq!(app.get("locale"), Some(&json!("en")), "existing half kept");
        let game: Value = serde_json::from_str(
            &fs::read_to_string(space.join("game_settings.json")).expect("game settings"),
        )
        .expect("parse game settings");
        assert_eq!(game.get("arma3_directory"), Some(&json!("C:\\Arma3")));
        assert!(!root.join("settings.json").exists());
    }

    #[test]
    fn unparseable_settings_are_backed_up_without_split() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let space = space_dir(root);
        write(&root.join("settings.json"), "{not json");

        let migrated = migrate_legacy_layout(root, &space).expect("migration");

        assert!(migrated);
        assert!(root.join("settings.json.pre-gamespaces.bak").is_file());
        assert!(!root.join("settings.json").exists());
        assert!(!root.join("app_settings.json").exists());
        assert!(!space.join("game_settings.json").exists());
    }
}
