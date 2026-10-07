use anyhow::{Context, Result, bail};
use indicatif::ProgressBar;
use serde_json::{Value, json};
use std::path::Path;

use crate::cli::GenerationMode;
use crate::types::ResolvedMod;
use crate::{hash, output, srf};

pub fn verify(output_dir: &Path) -> Result<()> {
    let mut repositories = 0;
    let mut mods = 0;
    if output_dir.join("repo.json").is_file() {
        mods += verify_repo(output_dir)?;
        repositories += 1;
    } else {
        for entry in std::fs::read_dir(output_dir)
            .with_context(|| format!("Failed to read {}", output_dir.display()))?
        {
            let dir = entry?.path();
            if dir.join("repo.json").is_file() {
                mods += verify_repo(&dir)?;
                repositories += 1;
            }
        }
    }
    if repositories == 0 {
        bail!(
            "No generated repositories found in {}",
            output_dir.display()
        );
    }
    println!("Verified {repositories} repositories and {mods} mods");
    output::set_details(json!({"repositories": repositories, "mods": mods}));
    Ok(())
}

fn verify_repo(dir: &Path) -> Result<usize> {
    let repo = srf::read_manifest(&dir.join("repo.json"))?;
    let foxy_path = dir.join("foxy_addons.json");
    let has_foxy = repo["foxyMode"].is_string();
    let foxy: Option<Value> = if has_foxy {
        Some(srf::read_manifest(&foxy_path)?)
    } else {
        None
    };
    let mode = match has_foxy {
        true if repo["requiredMods"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
            || repo["optionalMods"]
                .as_array()
                .is_some_and(|v| !v.is_empty()) =>
        {
            GenerationMode::Hybrid
        }
        true => GenerationMode::Foxy,
        false => GenerationMode::Swifty,
    };
    let listing = foxy.as_ref().unwrap_or(&repo);
    let mut count = 0;
    let mut processed_mods = Vec::new();
    for list in ["requiredMods", "optionalMods"] {
        for item in listing[list].as_array().into_iter().flatten() {
            let name = item["modName"]
                .as_str()
                .context("Manifest modName is missing")?;
            let path = dir.join(name);
            if !path.is_dir() {
                bail!(
                    "Published mod is missing or has a broken link: {}",
                    path.display()
                );
            }
            let resolved = ResolvedMod {
                mod_name: name.to_string(),
                source_path: path.clone(),
                is_required: list == "requiredMods",
                enabled: true,
                client_side: false,
            };
            let actual = hash::process_mods(
                &[resolved],
                None,
                &ProgressBar::hidden(),
                mode,
                false,
                false,
            )?;
            let mut actual = actual.into_iter().next().context("No hashed mod")?;
            let addon: Option<Value> = if mode != GenerationMode::Swifty {
                Some(srf::read_manifest(&path.join("foxy_addon.json"))?)
            } else {
                None
            };
            let srf: Option<Value> = if mode != GenerationMode::Foxy {
                Some(srf::read_manifest(&path.join("mod.srf"))?)
            } else {
                None
            };
            let files = addon
                .as_ref()
                .map(|manifest| &manifest["files"])
                .or_else(|| srf.as_ref().map(|manifest| &manifest["Files"]))
                .and_then(Value::as_array)
                .context("Per-mod files missing")?;
            if files.len() != actual.files.len() {
                bail!("Per-mod file count mismatch for {}", path.display());
            }
            for file in &mut actual.files {
                let name = file.relative_path.replace('\\', "/");
                let (order, entry) = files
                    .iter()
                    .enumerate()
                    .find(|(_, entry)| {
                        entry[if addon.is_some() { "path" } else { "Path" }]
                            .as_str()
                            .is_some_and(|path| path.replace('\\', "/") == name)
                    })
                    .with_context(|| format!("Per-mod file missing from manifest: {name}"))?;
                let expected_hash = entry[if addon.is_some() {
                    "checksum"
                } else {
                    "Checksum"
                }]
                .as_str()
                .context("Per-mod file checksum missing")?;
                let computed_hash = if addon.is_some() {
                    file.checksums.unwrap_blake3()
                } else {
                    file.checksums.unwrap_md5()
                };
                if expected_hash != computed_hash
                    || entry[if addon.is_some() { "length" } else { "Length" }].as_u64()
                        != Some(file.length)
                {
                    bail!("Per-mod file mismatch in {}: {name}", path.display());
                }
                if let Some(srf) = &srf {
                    let srf_files = srf["Files"].as_array().context("Swifty files missing")?;
                    if !srf_files.iter().any(|entry| {
                        entry["Path"]
                            .as_str()
                            .is_some_and(|path| path.replace('\\', "/") == name)
                            && entry["Checksum"].as_str() == Some(file.checksums.unwrap_md5())
                            && entry["Length"].as_u64() == Some(file.length)
                    }) {
                        bail!("Swifty file mismatch in {}: {name}", path.display());
                    }
                }
                file.data_order = order;
            }
            actual.files.sort_by_key(|file| file.data_order);
            actual.checksums = hash::compute_mod_checksums(&actual.files, mode);
            let expected = item["checkSum"]
                .as_str()
                .context("Manifest checksum missing")?;
            let computed = if mode == GenerationMode::Swifty {
                actual.checksums.unwrap_md5()
            } else {
                actual.checksums.unwrap_blake3()
            };
            if computed != expected {
                bail!("Checksum mismatch for {}", path.display());
            }
            if let Some(addon) = &addon
                && addon["checksum"].as_str() != Some(actual.checksums.unwrap_blake3())
            {
                bail!("Per-mod manifest checksum mismatch for {}", path.display());
            }
            if let Some(srf) = &srf
                && srf["Checksum"].as_str() != Some(actual.checksums.unwrap_md5())
            {
                bail!("Swifty manifest checksum mismatch for {}", path.display());
            }
            if mode == GenerationMode::Hybrid {
                let listed = repo[list]
                    .as_array()
                    .context("Hybrid repo mod list missing")?;
                if !listed.iter().any(|entry| {
                    entry["modName"].as_str() == Some(name)
                        && entry["checkSum"].as_str() == Some(actual.checksums.unwrap_md5())
                }) {
                    bail!("Hybrid MD5 checksum mismatch for {}", path.display());
                }
            }
            processed_mods.push(actual);
            count += 1;
        }
    }
    if let Some(foxy) = &foxy {
        let checksum = hash::compute_foxy_repo_checksum(&processed_mods);
        if foxy["checksum"].as_str() != Some(checksum.as_str()) {
            bail!("Repository checksum mismatch in {}", dir.display());
        }
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() && !path.exists() {
            bail!("Broken repository symlink: {}", path.display());
        }
    }
    Ok(count)
}
