use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Top-level update manifest (`foxy-app-updater.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub latest: String,
    pub versions: Vec<VersionEntry>,
}

/// A single version entry in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub version: String,
    pub changelog: String,
    pub platforms: HashMap<String, PlatformEntry>,
}

/// Platform-specific installer info.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformEntry {
    pub installer_path: String,
    pub installer_hash: String,
    #[serde(default = "default_installer_hash_algorithm")]
    pub installer_hash_algorithm: String,
    pub installer_size: u64,
}

pub fn default_installer_hash_algorithm() -> String {
    "blake3".to_string()
}

pub const MANIFEST_FILENAME: &str = "foxy-app-updater.json";
pub const CHANGELOGS_DIR: &str = "changelogs";
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Compute the blake3 hash of a file and return (hex_hash, file_size).
pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    let data =
        std::fs::read(path).with_context(|| format!("Failed to read file: {}", path.display()))?;
    let hash = blake3::hash(&data);
    Ok((hash.to_hex().to_string(), data.len() as u64))
}

/// Build a `PlatformEntry` from an installer file path.
/// The `installer_path` in the entry uses forward slashes relative to server root.
pub fn build_platform_entry(installer_path: &Path, relative_prefix: &str) -> Result<PlatformEntry> {
    let (hash, size) = hash_file(installer_path)?;
    let file_name = installer_path
        .file_name()
        .context("Installer path has no file name")?
        .to_string_lossy();
    let relative = format!("{}/{}", relative_prefix, file_name);
    Ok(PlatformEntry {
        installer_path: relative,
        installer_hash: hash,
        installer_hash_algorithm: default_installer_hash_algorithm(),
        installer_size: size,
    })
}

/// Read an existing manifest from disk.
pub fn read_manifest(server_root: &Path) -> Result<UpdateManifest> {
    let manifest_path = server_root.join(MANIFEST_FILENAME);
    let content = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))
}

/// Write a manifest to disk (pretty-printed).
pub fn write_manifest(manifest: &UpdateManifest, server_root: &Path) -> Result<()> {
    let manifest_path = server_root.join(MANIFEST_FILENAME);
    let json = serde_json::to_string_pretty(manifest).context("Failed to serialize manifest")?;
    std::fs::write(&manifest_path, json)
        .with_context(|| format!("Failed to write {}", manifest_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;

    // ── hash_file ───────────────────────────────────────────────────────

    #[test]
    fn hash_file_returns_blake3_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.bin");
        fs::write(&file, b"hello world").unwrap();

        let (hash, size) = hash_file(&file).unwrap();
        assert_eq!(size, 11);
        assert!(!hash.is_empty());
        // BLAKE3 full hex is 64 chars
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn hash_file_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("det.bin");
        fs::write(&file, b"deterministic content").unwrap();

        let (h1, s1) = hash_file(&file).unwrap();
        let (h2, s2) = hash_file(&file).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(s1, s2);
    }

    #[test]
    fn hash_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.bin");
        fs::File::create(&file).unwrap();

        let (hash, size) = hash_file(&file).unwrap();
        assert_eq!(size, 0);
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn hash_file_missing_errors() {
        let result = hash_file(Path::new("/nonexistent/file.bin"));
        assert!(result.is_err());
    }

    // ── build_platform_entry ────────────────────────────────────────────

    #[test]
    fn build_platform_entry_constructs_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let installer = dir.path().join("Foxy-0.6.0-setup.exe");
        fs::write(&installer, b"installer content").unwrap();

        let entry = build_platform_entry(&installer, "installers").unwrap();
        assert_eq!(entry.installer_path, "installers/Foxy-0.6.0-setup.exe");
        assert_eq!(entry.installer_size, 17);
        assert_eq!(entry.installer_hash.len(), 64);
    }

    // ── UpdateManifest round-trip ───────────────────────────────────────

    #[test]
    fn manifest_write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();

        let mut platforms = HashMap::new();
        platforms.insert(
            "windows".to_string(),
            PlatformEntry {
                installer_path: "installers/setup.exe".to_string(),
                installer_hash: "abc123".to_string(),
                installer_hash_algorithm: default_installer_hash_algorithm(),
                installer_size: 1024,
            },
        );

        let manifest = UpdateManifest {
            schema_version: CURRENT_SCHEMA_VERSION,
            latest: "0.6.0".to_string(),
            versions: vec![VersionEntry {
                version: "0.6.0".to_string(),
                changelog: "changelogs/0.6.0.json".to_string(),
                platforms,
            }],
        };

        write_manifest(&manifest, dir.path()).unwrap();

        let loaded = read_manifest(dir.path()).unwrap();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.latest, "0.6.0");
        assert_eq!(loaded.versions.len(), 1);
        assert_eq!(loaded.versions[0].version, "0.6.0");
        assert!(loaded.versions[0].platforms.contains_key("windows"));
        let win = &loaded.versions[0].platforms["windows"];
        assert_eq!(win.installer_size, 1024);
        assert_eq!(win.installer_hash_algorithm, "blake3");
    }

    #[test]
    fn read_manifest_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_manifest(dir.path());
        assert!(result.is_err());
    }

    // ── Constants ───────────────────────────────────────────────────────

    #[test]
    fn manifest_filename_is_expected() {
        assert_eq!(MANIFEST_FILENAME, "foxy-app-updater.json");
    }

    #[test]
    fn schema_version_is_one() {
        assert_eq!(CURRENT_SCHEMA_VERSION, 1);
    }
}
