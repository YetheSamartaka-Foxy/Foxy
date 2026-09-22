use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::types::{RepoGame, ResolvedMod};
use crate::{config, discover, keys, mod_line, mod_line_files, output, space};

pub fn validate(config_path: &Path, is_space: bool, output_dir: Option<&Path>) -> Result<()> {
    let repositories: Vec<(String, Vec<ResolvedMod>)> = if is_space {
        let space = space::load_space_config(config_path)?;
        let launch_files: Vec<Vec<PathBuf>> = space
            .repos
            .iter()
            .map(|repo| mod_line_files::resolve(&repo.config, &repo.config_path))
            .collect();
        for (repo, files) in space.repos.iter().zip(&launch_files) {
            mod_line_files::check(files, &mod_line::launch_flags(repo.config.game))
                .with_context(|| format!("Repository {}", repo.folder))?;
        }
        mod_line_files::ensure_distinct(
            space
                .repos
                .iter()
                .map(|repo| repo.folder.as_str())
                .zip(launch_files.iter().map(Vec::as_slice)),
        )?;
        space
            .repos
            .into_iter()
            .map(|repo| (repo.folder, repo.mods))
            .collect()
    } else {
        let (config, mods) = config::load_config(config_path)?;
        mod_line_files::check(
            &mod_line_files::resolve(&config, config_path),
            &mod_line::launch_flags(config.game),
        )?;
        vec![(config.repo_name, mods)]
    };
    let mut total_mods = 0;
    let mut names = Vec::new();
    for (repo, mods) in &repositories {
        let mut seen = BTreeSet::new();
        for item in mods {
            total_mods += 1;
            if !seen.insert(item.mod_name.to_lowercase()) {
                bail!(
                    "Repository {repo} publishes {} more than once",
                    item.mod_name
                );
            }
            if let Some(output) = output_dir {
                let source = std::fs::canonicalize(&item.source_path)?;
                let output = std::path::absolute(output)?;
                if output.starts_with(&source) || source.starts_with(&output) {
                    bail!("Output overlaps source mod {}", item.mod_name);
                }
            }
            names.push(item.mod_name.clone());
        }
    }
    println!(
        "Valid: {} repositories, {} mod references",
        repositories.len(),
        total_mods
    );
    output::set_details(json!({
        "repositories": repositories.len(),
        "modReferences": total_mods,
        "publishedNames": names,
    }));
    Ok(())
}

pub fn audit_keys(
    config_path: &Path,
    is_space: bool,
    strict: bool,
    additional_keys: &[PathBuf],
) -> Result<()> {
    let mods: Vec<ResolvedMod> = if is_space {
        space::load_space_config(config_path)?
            .repos
            .into_iter()
            .flat_map(|repo| repo.mods)
            .collect()
    } else {
        config::load_config(config_path)?.1
    };
    let mut key_paths = BTreeMap::<String, PathBuf>::new();
    let mut unsigned = Vec::new();
    let mut conflicts = Vec::new();
    let mut signature_keys = Vec::new();
    for item in &mods {
        for file in discover::discover_files(&item.source_path)? {
            let path = &file.absolute_path;
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.to_ascii_lowercase().ends_with(".bikey") {
                let key = name.to_ascii_lowercase();
                if let Some(first) = key_paths.get(&key) {
                    if std::fs::read(first)? != std::fs::read(path)? {
                        conflicts.push(name.to_string());
                    }
                } else {
                    key_paths.insert(key, path.clone());
                }
            }
            if name.to_ascii_lowercase().ends_with(".pbo") {
                let signatures: Vec<String> = path
                    .parent()
                    .and_then(|parent| std::fs::read_dir(parent).ok())
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
                    .filter(|candidate| {
                        let candidate = candidate.to_ascii_lowercase();
                        candidate.starts_with(&format!("{}.", name.to_ascii_lowercase()))
                            && candidate.ends_with(".bisign")
                    })
                    .collect();
                if signatures.is_empty() {
                    unsigned.push(format!("{}/{}", item.mod_name, file.relative_path));
                } else {
                    let prefix_len = name.len() + 1;
                    for signature in signatures {
                        let key_name = &signature[prefix_len..signature.len() - ".bisign".len()];
                        if !key_name.is_empty() {
                            signature_keys.push(format!("{key_name}.bikey").to_ascii_lowercase());
                        }
                    }
                }
            }
        }
    }
    for source in additional_keys {
        for path in keys::additional_key_paths(source)? {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let key = name.to_ascii_lowercase();
            if let Some(first) = key_paths.get(&key) {
                if std::fs::read(first)? != std::fs::read(&path)? {
                    conflicts.push(name.to_string());
                }
            } else {
                key_paths.insert(key, path);
            }
        }
    }
    let mut missing_keys: Vec<String> = signature_keys
        .into_iter()
        .filter(|name| !key_paths.contains_key(name))
        .collect();
    missing_keys.sort();
    missing_keys.dedup();
    unsigned.sort();
    conflicts.sort();
    conflicts.dedup();
    println!(
        "Key audit: {} distinct keys, {} unsigned PBOs, {} missing keys, {} conflicting key names",
        key_paths.len(),
        unsigned.len(),
        missing_keys.len(),
        conflicts.len()
    );
    for name in &unsigned {
        println!("  unsigned: {name}");
    }
    for name in &conflicts {
        println!("  conflicting key: {name}");
    }
    for name in &missing_keys {
        println!("  missing key: {name}");
    }
    output::set_details(json!({
        "keyCount": key_paths.len(),
        "unsignedPbos": unsigned,
        "missingKeys": missing_keys,
        "conflictingKeyNames": conflicts,
    }));
    if strict && (!unsigned.is_empty() || !missing_keys.is_empty() || !conflicts.is_empty()) {
        bail!("Key audit failed in strict mode");
    }
    Ok(())
}

pub fn export_reforger_config(
    config_path: &Path,
    output_path: &Path,
    include_optional: bool,
) -> Result<()> {
    let (config, mods) = config::load_config(config_path)?;
    if config.game != RepoGame::Reforger {
        bail!("export-reforger-config requires a Reforger repository config");
    }
    if output_path.exists() {
        bail!("Output already exists: {}", output_path.display());
    }
    let mut ids = BTreeSet::new();
    let mut entries = Vec::new();
    for item in &mods {
        if !item.enabled || item.client_side || (!item.is_required && !include_optional) {
            continue;
        }
        let id = mod_line::reforger_mod_id(&item.source_path)
            .with_context(|| format!("No Reforger mod ID found for {}", item.mod_name))?;
        if ids.insert(id.to_ascii_lowercase()) {
            entries.push(json!({"modId": id, "name": item.mod_name}));
        }
    }
    let fragment = json!({"game": {"mods": entries}});
    let data = serde_json::to_vec_pretty(&fragment)?;
    std::fs::write(output_path, data)
        .with_context(|| format!("Failed to write {}", output_path.display()))?;
    println!(
        "Wrote {} Reforger mods to {}",
        entries.len(),
        output_path.display()
    );
    output::set_details(json!({"path": output_path, "modCount": entries.len()}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_duplicate_published_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("@mod")).unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "repoName": "Test",
                "basePath": dir.path(),
                "requiredMods": [{"modName": "@mod"}, {"modName": "@mod"}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(validate(&config, false, None).is_err());
    }

    #[test]
    fn audit_reports_unsigned_pbo_in_strict_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("@mod");
        std::fs::create_dir(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("data.pbo"), b"data").unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "repoName": "Test",
                "basePath": dir.path(),
                "requiredMods": [{"modName": "@mod"}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(audit_keys(&config, false, true, &[]).is_err());
        std::fs::write(mod_dir.join("data.pbo.testkey.bisign"), b"signature").unwrap();
        assert!(audit_keys(&config, false, true, &[]).is_err());
        let key = dir.path().join("testkey.bikey");
        std::fs::write(&key, b"key").unwrap();
        assert!(audit_keys(&config, false, true, &[key]).is_ok());
    }

    #[test]
    fn reforger_export_writes_game_mods_fragment() {
        let dir = tempfile::tempdir().unwrap();
        let mod_dir = dir.path().join("MyMod");
        std::fs::create_dir(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("addon.gproj"), b"GUID \"ABCDEF1234567890\"").unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "repoName": "Test",
                "game": "reforger",
                "basePath": dir.path(),
                "requiredMods": [{"modName": "MyMod"}]
            }))
            .unwrap(),
        )
        .unwrap();
        let output = dir.path().join("mods.json");
        export_reforger_config(&config, &output, false).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(value["game"]["mods"][0]["modId"], "ABCDEF1234567890");
    }
}
