use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub latest: String,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub version: String,
    pub changelog: String,
    pub platforms: HashMap<String, PlatformEntry>,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogVersion {
    pub version: String,
    #[serde(default)]
    pub date: String,
    pub sections: Vec<ChangelogSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogSection {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AppUpdateInfo {
    pub source_base_url: String,

    pub manifest: UpdateManifest,

    pub current_version: String,

    pub fetched_changelogs: Vec<ChangelogVersion>,
}

#[derive(Debug, Clone)]
pub enum UpdateCheckStatus {
    Idle,

    Checking,

    Available(AppUpdateInfo),

    Downloading {
        progress: f32,
        bytes_done: u64,
        bytes_total: u64,
    },

    Verifying,

    ReadyToInstall {
        installer_path: PathBuf,
    },

    Failed(String),

    UpToDate(AppUpdateInfo),
}

#[derive(Debug, Clone)]
pub enum AppUpdateEvent {
    ManifestFetched(AppUpdateInfo),

    ChangelogFetched(ChangelogVersion),

    ChangelogFetchFailed(String),

    DownloadProgress { bytes_done: u64, bytes_total: u64 },

    Verifying,

    InstallerReady { installer_path: PathBuf },

    Error(String),

    UpToDate(AppUpdateInfo),
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub body: Option<String>,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<GitHubAsset>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
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
            version: "1.2.0".to_string(),
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
        assert_eq!(deserialized.version, "1.2.0");
        assert_eq!(deserialized.date, "2025-01-15");
        assert_eq!(deserialized.sections.len(), 2);
        assert_eq!(deserialized.sections[0].items.len(), 2);
    }

    #[test]
    fn changelog_version_empty_date_default() {
        let json = r#"{"version":"1.0.0","sections":[]}"#;
        let changelog: ChangelogVersion = serde_json::from_str(json).unwrap();
        assert_eq!(changelog.version, "1.0.0");
        assert_eq!(changelog.date, "");
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
