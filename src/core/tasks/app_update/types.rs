use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Types mirroring the server-side foxy-app-updater.json schema
// ---------------------------------------------------------------------------

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

/// A parsed per-version changelog from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogVersion {
    pub version: String,
    #[serde(default)]
    pub date: String,
    pub sections: Vec<ChangelogSection>,
}

/// A section within a changelog (e.g. "Added", "Fixed").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogSection {
    pub title: String,
    pub items: Vec<String>,
}

// ---------------------------------------------------------------------------
// App-update state tracked by the UI
// ---------------------------------------------------------------------------

/// Combined info about an available update from a specific source.
#[derive(Debug, Clone)]
pub struct AppUpdateInfo {
    /// Base URL of the update source.
    pub source_base_url: String,
    /// The full manifest from the server.
    pub manifest: UpdateManifest,
    /// The local app version at check time.
    pub current_version: String,
    /// Changelogs fetched so far (lazily populated).
    pub fetched_changelogs: Vec<ChangelogVersion>,
}

/// Progress/status of the update flow.
#[derive(Debug, Clone)]
pub enum UpdateCheckStatus {
    /// No update check running.
    Idle,
    /// Checking for updates.
    Checking,
    /// An update (or version list) is available.
    Available(AppUpdateInfo),
    /// Downloading an installer.
    Downloading {
        progress: f32,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// Verifying the downloaded installer hash.
    Verifying,
    /// Installer downloaded and verified, ready to run.
    ReadyToInstall { installer_path: PathBuf },
    /// Something went wrong.
    Failed(String),
    /// Already on the latest version.
    UpToDate(AppUpdateInfo),
}

/// Events sent from the background update worker to the UI via mpsc.
#[derive(Debug, Clone)]
pub enum AppUpdateEvent {
    /// Manifest fetched successfully.
    ManifestFetched(AppUpdateInfo),
    /// A changelog was fetched for a specific version.
    ChangelogFetched(ChangelogVersion),
    /// A changelog fetch failed for a specific version.
    ChangelogFetchFailed(String),
    /// Download progress update.
    DownloadProgress { bytes_done: u64, bytes_total: u64 },
    /// Verifying installer hash.
    Verifying,
    /// Download + verification complete.
    InstallerReady { installer_path: PathBuf },
    /// An error occurred.
    Error(String),
    /// Current version is already latest.
    UpToDate(AppUpdateInfo),
}

// ---------------------------------------------------------------------------
// GitHub Releases API types
// ---------------------------------------------------------------------------

/// A GitHub release as returned by the Releases API.
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub body: Option<String>,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<GitHubAsset>,
    pub published_at: Option<String>,
}

/// A single asset attached to a GitHub release.
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_manifest_serde_round_trip() {
        let manifest = UpdateManifest {
            schema_version: 1,
            latest: "1.2.0".to_string(),
            versions: vec![VersionEntry {
                version: "1.2.0".to_string(),
                changelog: "changelogs/1.2.0.json".to_string(),
                platforms: {
                    let mut map = HashMap::new();
                    map.insert(
                        "windows-x86_64".to_string(),
                        PlatformEntry {
                            installer_path: "installers/foxy-1.2.0-x64.exe".to_string(),
                            installer_hash: "ABCDEF123456".to_string(),
                            installer_hash_algorithm: default_installer_hash_algorithm(),
                            installer_size: 50_000_000,
                        },
                    );
                    map
                },
            }],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let deserialized: UpdateManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.schema_version, 1);
        assert_eq!(deserialized.latest, "1.2.0");
        assert_eq!(deserialized.versions.len(), 1);
        assert_eq!(deserialized.versions[0].version, "1.2.0");
        assert!(
            deserialized.versions[0]
                .platforms
                .contains_key("windows-x86_64")
        );
    }

    #[test]
    fn changelog_version_serde_round_trip() {
        let changelog = ChangelogVersion {
            version: "1.1.0".to_string(),
            date: "2025-01-15".to_string(),
            sections: vec![
                ChangelogSection {
                    title: "Added".to_string(),
                    items: vec!["New feature A".to_string(), "New feature B".to_string()],
                },
                ChangelogSection {
                    title: "Fixed".to_string(),
                    items: vec!["Bug fix X".to_string()],
                },
            ],
        };
        let json = serde_json::to_string(&changelog).unwrap();
        let deserialized: ChangelogVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, "1.1.0");
        assert_eq!(deserialized.date, "2025-01-15");
        assert_eq!(deserialized.sections.len(), 2);
        assert_eq!(deserialized.sections[0].items.len(), 2);
    }

    #[test]
    fn changelog_version_empty_date_default() {
        let json = r#"{"version":"1.0.0","sections":[]}"#;
        let changelog: ChangelogVersion = serde_json::from_str(json).unwrap();
        assert_eq!(changelog.version, "1.0.0");
        assert_eq!(changelog.date, ""); // default
        assert!(changelog.sections.is_empty());
    }

    #[test]
    fn github_release_deserialization() {
        let json = r#"{
            "tag_name": "v1.0.0",
            "name": "Release 1.0.0",
            "body": "Initial release",
            "prerelease": false,
            "draft": false,
            "assets": [{
                "name": "foxy-1.0.0.exe",
                "size": 12345678,
                "browser_download_url": "https://github.com/example/releases/download/v1.0.0/foxy-1.0.0.exe"
            }],
            "published_at": "2025-01-01T00:00:00Z"
        }"#;
        let release: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v1.0.0");
        assert!(!release.prerelease);
        assert!(!release.draft);
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "foxy-1.0.0.exe");
    }

    #[test]
    fn github_release_optional_fields() {
        let json = r#"{
            "tag_name": "v2.0.0-beta",
            "name": null,
            "body": null,
            "prerelease": true,
            "draft": false,
            "assets": [],
            "published_at": null
        }"#;
        let release: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v2.0.0-beta");
        assert!(release.body.is_none());
        assert!(release.prerelease);
        assert!(release.assets.is_empty());
        assert!(release.published_at.is_none());
    }

    #[test]
    fn platform_entry_serde_round_trip() {
        let entry = PlatformEntry {
            installer_path: "installers/foxy.exe".to_string(),
            installer_hash: "0123456789ABCDEF".to_string(),
            installer_hash_algorithm: "sha256".to_string(),
            installer_size: 999_999,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: PlatformEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.installer_path, "installers/foxy.exe");
        assert_eq!(deserialized.installer_hash, "0123456789ABCDEF");
        assert_eq!(deserialized.installer_hash_algorithm, "sha256");
        assert_eq!(deserialized.installer_size, 999_999);
    }

    #[test]
    fn platform_entry_defaults_hash_algorithm() {
        let json = r#"{
            "installerPath": "installers/foxy.exe",
            "installerHash": "0123456789ABCDEF",
            "installerSize": 999999
        }"#;
        let deserialized: PlatformEntry = serde_json::from_str(json).unwrap();
        assert_eq!(deserialized.installer_hash_algorithm, "blake3");
    }
}
