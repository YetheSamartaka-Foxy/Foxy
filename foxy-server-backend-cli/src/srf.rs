use anyhow::{Context, Result};
use std::path::Path;

use crate::cli::GenerationMode;
use crate::hash;
use crate::pbo;
use crate::types::{
    FoxyAddonFile, FoxyAddonJson, FoxyAddonPart, FoxyAddonsJson, ModEntry, ProcessedMod,
    RepoConfig, RepoJson, SrfFile, SrfManifest, SrfPart,
};

pub const FOXY_MODE_VERSION: &str = "FoxyModeV1";

// ---------------------------------------------------------------------------
// SwiftyMode: mod.srf
// ---------------------------------------------------------------------------

/// Write mod.srf for a single mod into its output directory (SwiftyMode/HybridMode).
pub fn write_mod_srf(processed_mod: &ProcessedMod, output_dir: &Path) -> Result<()> {
    let mod_dir = output_dir.join(&processed_mod.mod_name);
    let srf_path = mod_dir.join("mod.srf");

    let files: Vec<SrfFile> = processed_mod
        .files
        .iter()
        .map(|f| {
            let parts: Vec<SrfPart> = f
                .parts
                .iter()
                .map(|p| SrfPart {
                    path: p.path.clone(),
                    checksum: p.checksums.unwrap_md5().to_string(),
                    start: p.start,
                    length: p.length,
                })
                .collect();

            SrfFile {
                path: f.relative_path.replace('/', "\\"),
                checksum: f.checksums.unwrap_md5().to_string(),
                length: f.length,
                file_type: if pbo::is_pbo(Path::new(&f.relative_path)) {
                    "SwiftyPboFile".to_string()
                } else {
                    "SwiftyFile".to_string()
                },
                parts,
            }
        })
        .collect();

    let manifest = SrfManifest {
        name: processed_mod.mod_name.clone(),
        checksum: processed_mod.checksums.unwrap_md5().to_string(),
        files,
    };
    let json = serde_json::to_string(&manifest).context("Failed to serialize mod.srf")?;
    std::fs::write(&srf_path, json)
        .with_context(|| format!("Failed to write {}", srf_path.display()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// FoxyMode: foxy_addon.json (per-mod)
// ---------------------------------------------------------------------------

/// Write foxy_addon.json for a single mod into its output directory (FoxyMode/HybridMode).
pub fn write_foxy_addon_json(processed_mod: &ProcessedMod, output_dir: &Path) -> Result<()> {
    let mod_dir = output_dir.join(&processed_mod.mod_name);
    let path = mod_dir.join("foxy_addon.json");

    let files: Vec<FoxyAddonFile> = processed_mod
        .files
        .iter()
        .map(|f| {
            let parts: Vec<FoxyAddonPart> = f
                .parts
                .iter()
                .map(|p| FoxyAddonPart {
                    path: p.path.clone(),
                    checksum: p.checksums.unwrap_blake3().to_string(),
                    start: p.start,
                    length: p.length,
                })
                .collect();

            FoxyAddonFile {
                path: f.relative_path.replace('\\', "/"),
                checksum: f.checksums.unwrap_blake3().to_string(),
                length: f.length,
                file_type: if pbo::is_pbo(Path::new(&f.relative_path)) {
                    "FoxyPboFile".to_string()
                } else {
                    "FoxyFile".to_string()
                },
                parts,
            }
        })
        .collect();

    let manifest = FoxyAddonJson {
        name: processed_mod.mod_name.clone(),
        version: FOXY_MODE_VERSION.to_string(),
        checksum: processed_mod.checksums.unwrap_blake3().to_string(),
        hash_algorithm: "BLAKE3".to_string(),
        files,
    };

    let json = serde_json::to_string(&manifest).context("Failed to serialize foxy_addon.json")?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// FoxyMode: foxy_addons.json (repo-level)
// ---------------------------------------------------------------------------

fn collect_mod_entries(
    mods: &[ProcessedMod],
    checksum_fn: fn(&ProcessedMod) -> &str,
) -> (Vec<ModEntry>, Vec<ModEntry>) {
    let required = mods
        .iter()
        .filter(|m| m.is_required)
        .map(|m| ModEntry {
            mod_name: m.mod_name.clone(),
            check_sum: checksum_fn(m).to_string(),
            enabled: m.enabled,
            client_side: m.client_side,
        })
        .collect();
    let optional = mods
        .iter()
        .filter(|m| !m.is_required)
        .map(|m| ModEntry {
            mod_name: m.mod_name.clone(),
            check_sum: checksum_fn(m).to_string(),
            enabled: m.enabled,
            client_side: m.client_side,
        })
        .collect();
    (required, optional)
}

/// Write foxy_addons.json at the output root (FoxyMode/HybridMode).
pub fn write_foxy_addons_json(
    mods: &[ProcessedMod],
    foxy_repo_checksum: &str,
    output_dir: &Path,
) -> Result<()> {
    let (required_mods, optional_mods) = collect_mod_entries(mods, |m| m.checksums.unwrap_blake3());

    let foxy_addons = FoxyAddonsJson {
        version: FOXY_MODE_VERSION.to_string(),
        hash_algorithm: "BLAKE3".to_string(),
        checksum: foxy_repo_checksum.to_string(),
        required_mods,
        optional_mods,
    };

    let json =
        serde_json::to_string(&foxy_addons).context("Failed to serialize foxy_addons.json")?;
    let path = output_dir.join("foxy_addons.json");
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// repo.json
// ---------------------------------------------------------------------------

/// Write repo.json at the output root.
pub fn write_repo_json(
    config: &RepoConfig,
    mods: &[ProcessedMod],
    repo_checksum: &str,
    output_dir: &Path,
    mode: GenerationMode,
    app_update_url: Option<&str>,
) -> Result<()> {
    // In FoxyMode, repo.json mod lists are empty (data lives in foxy_addons.json).
    // In SwiftyMode/HybridMode, they contain MD5 checksums as before.
    let (required_mods, optional_mods) = match mode {
        GenerationMode::Foxy => (Vec::new(), Vec::new()),
        GenerationMode::Swifty | GenerationMode::Hybrid => {
            collect_mod_entries(mods, |m| m.checksums.unwrap_md5())
        }
    };

    let foxy_mode = match mode {
        GenerationMode::Foxy | GenerationMode::Hybrid => Some(FOXY_MODE_VERSION.to_string()),
        GenerationMode::Swifty => None,
    };
    let app_update_url = app_update_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string);

    // Hash and copy image files if they exist
    let base = Path::new(&config.base_path);
    let (repo_image_checksum, icon_image_checksum) = process_images(config, base, output_dir)?;

    let repo = RepoJson {
        repo_name: config.repo_name.clone(),
        checksum: repo_checksum.to_string(),
        foxy_mode,
        required_mods,
        optional_mods,
        icon_image_path: config.icon_image_path.clone(),
        icon_image_checksum,
        repo_image_path: config.repo_image_path.clone(),
        repo_image_checksum,
        app_update_url,
        client_parameters: config.client_parameters.clone(),
        repo_basic_authentication: config.repo_basic_authentication.clone(),
        version: config.version.clone(),
        servers: config.servers.clone(),
    };

    let json = serde_json::to_string(&repo).context("Failed to serialize repo.json")?;
    let repo_path = output_dir.join("repo.json");
    std::fs::write(&repo_path, json)
        .with_context(|| format!("Failed to write {}", repo_path.display()))?;

    Ok(())
}

fn process_images(config: &RepoConfig, base: &Path, output_dir: &Path) -> Result<(String, String)> {
    let repo_image_checksum = copy_and_hash_image(&config.repo_image_path, base, output_dir)?;
    let icon_image_checksum = copy_and_hash_image(&config.icon_image_path, base, output_dir)?;
    Ok((repo_image_checksum, icon_image_checksum))
}

fn copy_and_hash_image(image_path: &str, base: &Path, output_dir: &Path) -> Result<String> {
    if image_path.is_empty() {
        return Ok(String::new());
    }

    let source = base.join(image_path);
    if !source.is_file() {
        log::warn!("Image file not found: {}", source.display());
        return Ok(String::new());
    }

    let dest = output_dir.join(image_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create dir for image: {}", parent.display()))?;
    }
    std::fs::copy(&source, &dest)
        .with_context(|| format!("Failed to copy image: {}", source.display()))?;

    hash::hash_file_sha1(&dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Checksums, FilePart, ModFile};

    #[test]
    fn mod_srf_serializes_swifty_compatibility_fields() {
        let processed = ProcessedMod {
            mod_name: "@demo".to_string(),
            checksums: Checksums::Md5("ABC123".to_string()),
            is_required: true,
            enabled: true,
            files: vec![
                ModFile {
                    relative_path: "addons/demo.pbo".to_string(),
                    checksums: Checksums::Md5("FILESUM".to_string()),
                    length: 123,
                    data_order: 0,
                    parts: vec![
                        FilePart {
                            path: "$$HEADER$$".to_string(),
                            checksums: Checksums::Md5("PART1".to_string()),
                            start: 0,
                            length: 10,
                        },
                        FilePart {
                            path: "Data/Thing.bin".to_string(),
                            checksums: Checksums::Md5("PART2".to_string()),
                            start: 10,
                            length: 113,
                        },
                    ],
                },
                ModFile {
                    relative_path: "keys/demo.bikey".to_string(),
                    checksums: Checksums::Md5("FILESUM2".to_string()),
                    length: 55,
                    data_order: 1,
                    parts: vec![FilePart {
                        path: "demo.bikey_55".to_string(),
                        checksums: Checksums::Md5("PART3".to_string()),
                        start: 0,
                        length: 55,
                    }],
                },
            ],
            client_side: false,
        };

        let manifest = SrfManifest {
            name: processed.mod_name,
            checksum: processed.checksums.unwrap_md5().to_string(),
            files: processed
                .files
                .iter()
                .map(|f| SrfFile {
                    path: f.relative_path.replace('/', "\\"),
                    checksum: f.checksums.unwrap_md5().to_string(),
                    length: f.length,
                    file_type: if pbo::is_pbo(Path::new(&f.relative_path)) {
                        "SwiftyPboFile".to_string()
                    } else {
                        "SwiftyFile".to_string()
                    },
                    parts: f
                        .parts
                        .iter()
                        .map(|p| SrfPart {
                            path: p.path.replace('/', "\\"),
                            checksum: p.checksums.unwrap_md5().to_string(),
                            start: p.start,
                            length: p.length,
                        })
                        .collect(),
                })
                .collect(),
        };

        let json = serde_json::to_string(&manifest).expect("manifest serialization should work");

        assert!(!json.contains('\n'));
        assert!(json.contains("\"Name\":\"@demo\""));
        assert!(json.contains("\"Checksum\":\"ABC123\""));
        assert!(json.contains("\"Type\":\"SwiftyPboFile\""));
        assert!(json.contains("\"Type\":\"SwiftyFile\""));
        assert!(json.contains("\"Path\":\"addons\\\\demo.pbo\""));
        assert!(json.contains("\"Path\":\"Data\\\\Thing.bin\""));
        assert!(json.contains("\"Path\":\"demo.bikey_55\""));
    }

    #[test]
    fn repo_json_serializes_swifty_compatibility_fields() {
        let repo = RepoJson {
            repo_name: "Repo".to_string(),
            checksum: "REPOCHECKSUM".to_string(),
            foxy_mode: None,
            required_mods: vec![ModEntry {
                mod_name: "@required".to_string(),
                check_sum: "REQ".to_string(),
                enabled: true,
                client_side: false,
            }],
            optional_mods: vec![ModEntry {
                mod_name: "@optional".to_string(),
                check_sum: "OPT".to_string(),
                enabled: true,
                client_side: true,
            }],
            icon_image_path: "icon.png".to_string(),
            icon_image_checksum: "ICON".to_string(),
            repo_image_path: "repo.png".to_string(),
            repo_image_checksum: "REPOIMG".to_string(),
            app_update_url: None,
            client_parameters: "-skipIntro".to_string(),
            repo_basic_authentication: crate::types::RepoBasicAuthentication {
                username: "user".to_string(),
                password: "pass".to_string(),
            },
            version: "3.2.0.0".to_string(),
            servers: vec![crate::types::ServerEntry {
                name: "Server".to_string(),
                address: "127.0.0.1".to_string(),
                port: "2302".to_string(),
                password: "pw".to_string(),
                battle_eye: false,
            }],
        };

        let json = serde_json::to_string(&repo).expect("repo serialization should work");

        assert!(!json.contains('\n'));
        // foxyMode should NOT be present when None
        assert!(!json.contains("foxyMode"));
        assert!(json.contains(
            "\"requiredMods\":[{\"modName\":\"@required\",\"checkSum\":\"REQ\",\"enabled\":true}]"
        ));
        assert!(json.contains(
            "\"optionalMods\":[{\"modName\":\"@optional\",\"checkSum\":\"OPT\",\"enabled\":true,\"clientSide\":true}]"
        ));
        assert!(
            json.contains(
                "\"repoBasicAuthentication\":{\"username\":\"user\",\"password\":\"pass\"}"
            )
        );
        assert!(json.contains("\"version\":\"3.2.0.0\""));
    }

    #[test]
    fn repo_json_includes_foxy_mode_when_set() {
        let repo = RepoJson {
            repo_name: "FoxyRepo".to_string(),
            checksum: "CHECK".to_string(),
            foxy_mode: Some("FoxyModeV1".to_string()),
            required_mods: vec![],
            optional_mods: vec![],
            icon_image_path: String::new(),
            icon_image_checksum: String::new(),
            repo_image_path: String::new(),
            repo_image_checksum: String::new(),
            app_update_url: Some("https://updates.example.com/foxy/".to_string()),
            client_parameters: String::new(),
            repo_basic_authentication: crate::types::RepoBasicAuthentication::default(),
            version: "3.2.0.0".to_string(),
            servers: vec![],
        };

        let json = serde_json::to_string(&repo).expect("repo serialization should work");
        assert!(json.contains("\"foxyMode\":\"FoxyModeV1\""));
        assert!(json.contains("\"appUpdateUrl\":\"https://updates.example.com/foxy/\""));
        // In FoxyMode, mod lists are empty
        assert!(json.contains("\"requiredMods\":[]"));
        assert!(json.contains("\"optionalMods\":[]"));
    }

    #[test]
    fn foxy_addon_json_uses_camel_case_and_blake3() {
        let processed = ProcessedMod {
            mod_name: "@test".to_string(),
            checksums: Checksums::Blake3("B3MODSUM".to_string()),
            is_required: true,
            enabled: true,
            files: vec![ModFile {
                relative_path: "addons/test.pbo".to_string(),
                checksums: Checksums::Blake3("B3FILESUM".to_string()),
                length: 999,
                data_order: 0,
                parts: vec![FilePart {
                    path: "$$HEADER$$".to_string(),
                    checksums: Checksums::Blake3("B3PARTSUM".to_string()),
                    start: 0,
                    length: 100,
                }],
            }],
            client_side: false,
        };

        let files: Vec<crate::types::FoxyAddonFile> = processed
            .files
            .iter()
            .map(|f| crate::types::FoxyAddonFile {
                path: f.relative_path.replace('\\', "/"),
                checksum: f.checksums.unwrap_blake3().to_string(),
                length: f.length,
                file_type: "FoxyPboFile".to_string(),
                parts: f
                    .parts
                    .iter()
                    .map(|p| crate::types::FoxyAddonPart {
                        path: p.path.clone(),
                        checksum: p.checksums.unwrap_blake3().to_string(),
                        start: p.start,
                        length: p.length,
                    })
                    .collect(),
            })
            .collect();

        let manifest = crate::types::FoxyAddonJson {
            name: processed.mod_name,
            version: "FoxyModeV1".to_string(),
            checksum: processed.checksums.unwrap_blake3().to_string(),
            hash_algorithm: "BLAKE3".to_string(),
            files,
        };

        let json =
            serde_json::to_string(&manifest).expect("foxy_addon.json serialization should work");

        assert!(json.contains("\"hashAlgorithm\":\"BLAKE3\""));
        assert!(json.contains("\"version\":\"FoxyModeV1\""));
        assert!(json.contains("\"fileType\":\"FoxyPboFile\""));
        assert!(json.contains("\"checksum\":\"B3PARTSUM\""));
        // Should use forward slashes
        assert!(json.contains("\"path\":\"addons/test.pbo\""));
    }
}
