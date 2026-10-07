use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::types::{RepoConfig, RepoGame, ResolvedMod};

/// Read and validate the config JSON, expanding any wildcard mod references.
pub fn load_config(config_path: &Path) -> Result<(RepoConfig, Vec<ResolvedMod>)> {
    let content = std::fs::read_to_string(config_path).context("Failed to read config file")?;
    let config: RepoConfig =
        serde_json::from_str(&content).context("Failed to parse config JSON")?;

    let base = PathBuf::from(&config.base_path);
    if !base.is_dir() {
        bail!(
            "basePath does not exist or is not a directory: {}",
            base.display()
        );
    }

    let mut resolved = Vec::new();

    for mod_ref in &config.required_mods {
        let expanded = expand_mod_ref(
            &base,
            &mod_ref.mod_name,
            true,
            mod_ref.enabled,
            mod_ref.client_side,
        )?;
        resolved.extend(expanded);
    }
    for mod_ref in &config.optional_mods {
        let expanded = expand_mod_ref(
            &base,
            &mod_ref.mod_name,
            false,
            mod_ref.enabled,
            mod_ref.client_side,
        )?;
        resolved.extend(expanded);
    }

    if resolved.is_empty() {
        bail!("No mods found after expanding all mod references");
    }

    Ok((config, resolved))
}

fn published_mod_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .filter(|name| !name.is_empty())
        .context("Mod path does not end with a valid directory name")
}

fn expand_mod_ref(
    base: &Path,
    mod_name: &str,
    is_required: bool,
    enabled: bool,
    client_side: bool,
) -> Result<Vec<ResolvedMod>> {
    let full_path = if Path::new(mod_name).is_absolute() {
        PathBuf::from(mod_name)
    } else {
        base.join(mod_name)
    };

    let pattern_str = full_path
        .to_str()
        .context("Mod path contains invalid UTF-8")?;

    // Wildcard expansion only applies to the final path segment (directory
    // globbing), and the parent directory enumeration order is preserved.
    if pattern_str.contains('*') || pattern_str.contains('?') || pattern_str.contains('[') {
        let parent_dir = full_path
            .parent()
            .context("Wildcard mod path does not have a parent directory")?;
        let file_name_pattern = full_path
            .file_name()
            .and_then(|name| name.to_str())
            .context("Wildcard mod path does not end with a valid directory pattern")?;
        let pattern = glob::Pattern::new(file_name_pattern)
            .with_context(|| format!("Invalid wildcard pattern: {}", file_name_pattern))?;

        let mut result = Vec::new();
        for entry in std::fs::read_dir(parent_dir).with_context(|| {
            format!(
                "Failed to read wildcard directory: {}",
                parent_dir.display()
            )
        })? {
            let entry = entry.with_context(|| {
                format!("Failed to read wildcard entry in {}", parent_dir.display())
            })?;
            let entry_path = entry.path();
            if !entry_path.is_dir() {
                continue;
            }

            let entry_name = entry.file_name();
            let entry_name = entry_name
                .to_str()
                .context("Mod path contains invalid UTF-8")?;
            if !pattern.matches(entry_name) {
                continue;
            }

            result.push(ResolvedMod {
                mod_name: published_mod_name(&entry_path)?,
                source_path: entry_path,
                is_required,
                enabled,
                client_side,
            });
        }

        if result.is_empty() {
            log::warn!("Wildcard pattern matched no directories: {}", pattern_str);
        }

        return Ok(result);
    }

    if !full_path.is_dir() {
        bail!(
            "Mod directory does not exist: {} (resolved to {})",
            mod_name,
            full_path.display()
        );
    }

    Ok(vec![ResolvedMod {
        mod_name: published_mod_name(&full_path)?,
        source_path: full_path,
        is_required,
        enabled,
        client_side,
    }])
}

/// Generate a blank config JSON template for the given game.
pub fn generate_template_config(output: &Path, game: RepoGame) -> Result<()> {
    let template = template_config_value(game);
    let json =
        serde_json::to_string_pretty(&template).context("Failed to serialize template config")?;
    std::fs::write(output, json).context("Failed to write config file")?;
    Ok(())
}

/// Arma 3 keeps the historical template. Reforger drops the keys the game
/// has no use for (Creator DLCs, client-side mods, BattlEye) and uses the
/// game's default port; mod folders are the unpacked addon directories that
/// hold the `.gproj` and `addons/*.pak`.
pub fn template_config_value(game: RepoGame) -> serde_json::Value {
    match game {
        RepoGame::Arma3 => serde_json::json!({
            "repoName": "My Repository",
            "game": "arma3",
            "basePath": ".",
            "appUpdateUrl": "",
            "requiredMods": [
                { "modName": "@example_mod", "enabled": true }
            ],
            "optionalMods": [
                { "modName": "@client_side_sound", "enabled": false, "clientSide": true }
            ],
            "iconImagePath": "icon.png",
            "repoImagePath": "repo.png",
            "clientParameters": "",
            "modLineFiles": [],
            "dlcContent": {
                "csla": false,
                "ef": false,
                "gm": false,
                "rf": false,
                "spe": false,
                "vn": false,
                "ws": false
            },
            "repoBasicAuthentication": {
                "username": "",
                "password": ""
            },
            "version": "3.2.0.0",
            "servers": [
                {
                    "name": "Main Server",
                    "address": "127.0.0.1",
                    "port": "2302",
                    "password": "",
                    "battleEye": true
                }
            ]
        }),
        RepoGame::Reforger => serde_json::json!({
            "repoName": "My Reforger Repository",
            "game": "reforger",
            "basePath": ".",
            "appUpdateUrl": "",
            "requiredMods": [
                { "modName": "MyReforgerMod", "enabled": true }
            ],
            "optionalMods": [
                { "modName": "OptionalReforgerMod", "enabled": false }
            ],
            "iconImagePath": "icon.png",
            "repoImagePath": "repo.png",
            "clientParameters": "-noSplash",
            "modLineFiles": [],
            "repoBasicAuthentication": {
                "username": "",
                "password": ""
            },
            "version": "1.0.0.0",
            "servers": [
                {
                    "name": "Main Server",
                    "address": "127.0.0.1",
                    "port": "2001",
                    "password": "",
                    "battleEye": false
                }
            ]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn template_configs_round_trip_through_the_config_parser() {
        for game in [RepoGame::Arma3, RepoGame::Reforger] {
            let value = template_config_value(game);
            let parsed: RepoConfig =
                serde_json::from_value(value.clone()).expect("template parses as RepoConfig");
            assert_eq!(parsed.game, game);
            assert_eq!(
                value["game"],
                serde_json::to_value(game).expect("game serializes")
            );
        }
        let reforger = template_config_value(RepoGame::Reforger);
        assert!(reforger.get("dlcContent").is_none());
        assert_eq!(reforger["servers"][0]["port"], "2001");
        assert_eq!(reforger["clientParameters"], "-noSplash");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("foxy-server-backend-cli-{name}-{unique}"))
    }

    #[test]
    fn direct_mod_refs_preserve_directory_name_case() {
        let base = temp_dir("direct-mod-name");
        let nested_mod = base.join("collections").join("@ACE");
        std::fs::create_dir_all(&nested_mod).expect("test mod directory should be created");

        let resolved = expand_mod_ref(&base, "collections/@ACE", true, true, false)
            .expect("direct mod ref should expand");

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].mod_name, "@ACE");
        assert_eq!(resolved[0].source_path, nested_mod);

        std::fs::remove_dir_all(base).expect("test temp dir should be removed");
    }

    #[test]
    fn wildcard_mod_refs_preserve_directory_name_case() {
        let base = temp_dir("wildcard-mod-name");
        let wildcard_root = base.join("collections");
        let first = wildcard_root.join("@ACE");
        let second = wildcard_root.join("@CBA_A3");
        std::fs::create_dir_all(&first).expect("first test mod directory should be created");
        std::fs::create_dir_all(&second).expect("second test mod directory should be created");

        let mut resolved = expand_mod_ref(&base, "collections/*", false, true, true)
            .expect("wildcard mod ref should expand");
        resolved.sort_by(|a, b| a.mod_name.cmp(&b.mod_name));

        let names: Vec<&str> = resolved.iter().map(|m| m.mod_name.as_str()).collect();
        assert_eq!(names, vec!["@ACE", "@CBA_A3"]);
        assert!(resolved.iter().all(|entry| entry.client_side));

        std::fs::remove_dir_all(base).expect("test temp dir should be removed");
    }

    #[test]
    fn load_config_parses_app_update_url_field() {
        let base = temp_dir("config-app-update-url");
        let mod_dir = base.join("@ace");
        std::fs::create_dir_all(&mod_dir).expect("test mod directory should be created");

        let config_path = base.join("config.json");
        let escaped_base = base.to_string_lossy().replace('\\', "\\\\").to_string();
        let config_json = format!(
            r#"{{
  "repoName": "Test Repo",
  "basePath": "{base_path}",
  "appUpdateUrl": "https://updates.example.com/foxy/",
  "requiredMods": [{{ "modName": "@ace", "enabled": true }}],
  "optionalMods": []
}}"#,
            base_path = escaped_base
        );
        std::fs::write(&config_path, config_json).expect("test config should be written");

        let (config, resolved) = load_config(&config_path).expect("config should parse");
        assert_eq!(
            config.app_update_url.as_deref(),
            Some("https://updates.example.com/foxy/")
        );
        assert_eq!(resolved.len(), 1);

        std::fs::remove_dir_all(base).expect("test temp dir should be removed");
    }
}
