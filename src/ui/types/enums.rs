use serde::{Deserialize, Serialize};
use std::fmt;

/// User preference for the hashing algorithm used when syncing a repository.
///
/// - `PreferFoxy` (default): Use BLAKE3 if the remote repository supports FoxyMode.
/// - `PreferSwifty`: Force legacy MD5 hashing even when FoxyMode is available.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum HashAlgorithmPreference {
    #[default]
    PreferFoxy,
    PreferSwifty,
}

/// User preference for how aggressively local file hashing uses disk I/O.
///
/// - `Auto` (default): Benchmark initial hash work and choose the fastest profile.
/// - `Conservative`: Low concurrency for constrained I/O workloads.
/// - `Balanced`: Moderate concurrency for mixed systems.
/// - `Aggressive`: Current high-concurrency behavior for fast workloads.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum HashIoProfilePreference {
    #[default]
    Auto,
    #[serde(alias = "HddSafe")]
    Conservative,
    Balanced,
    Aggressive,
}

/// User preference for the egui renderer used by the desktop UI.
///
/// - `Auto` (default): Use WGPU unless a previous WGPU crash forces a fallback.
/// - `Wgpu`: Force the WGPU renderer.
/// - `Glow`: Force the OpenGL/Glow renderer.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum UiRendererPreference {
    #[default]
    Auto,
    Wgpu,
    Glow,
}

/// Where to check for app updates.
///
/// - `Server` (default): Custom server manifest (`foxy-app-updater.json`).
/// - `GitHub`: GitHub Releases API for a public repository.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum AppUpdateMode {
    #[default]
    Server,
    GitHub,
}

impl fmt::Display for HashAlgorithmPreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashAlgorithmPreference::PreferFoxy => write!(f, "PreferFoxy"),
            HashAlgorithmPreference::PreferSwifty => write!(f, "PreferSwifty"),
        }
    }
}

impl fmt::Display for HashIoProfilePreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashIoProfilePreference::Auto => write!(f, "Auto"),
            HashIoProfilePreference::Conservative => write!(f, "Conservative"),
            HashIoProfilePreference::Balanced => write!(f, "Balanced"),
            HashIoProfilePreference::Aggressive => write!(f, "Aggressive"),
        }
    }
}

impl fmt::Display for UiRendererPreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UiRendererPreference::Auto => write!(f, "Auto"),
            UiRendererPreference::Wgpu => write!(f, "WGPU"),
            UiRendererPreference::Glow => write!(f, "Glow"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum FoxyView {
    RepositoryList,
    Settings,
    RepositorySettings,
    RepositorySpaceSettings,
    Help,
    Changelog,
    About,
    AppUpdate,
    VersionBrowser,
    SwiftyMigration,
    None,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HelpTab {
    Overview,
    GettingStarted,
    ChecksAndUpdates,
    ProfilesAndLaunch,
    RepositorySpacesAndAddons,
    EditorMissions,
    RecoveryAndTools,
    SettingsAndStatus,
    RendererAndPerformance,
    KeyboardShortcuts,
    ThirdPartyOverlays,
    Troubleshooting,
}

impl HelpTab {
    pub fn as_str(&self) -> &'static str {
        match self {
            HelpTab::Overview => "Overview",
            HelpTab::GettingStarted => "Getting started",
            HelpTab::ChecksAndUpdates => "Checks and updates",
            HelpTab::ProfilesAndLaunch => "Profiles and launch",
            HelpTab::RepositorySpacesAndAddons => "Repository spaces and addons",
            HelpTab::EditorMissions => "Editor missions",
            HelpTab::RecoveryAndTools => "Recovery and tools",
            HelpTab::SettingsAndStatus => "Settings and status",
            HelpTab::RendererAndPerformance => "Renderer and performance",
            HelpTab::KeyboardShortcuts => "Keyboard shortcuts",
            HelpTab::ThirdPartyOverlays => "Third-party overlays",
            HelpTab::Troubleshooting => "Troubleshooting",
        }
    }

    pub fn all_tabs() -> [HelpTab; 12] {
        [
            HelpTab::Overview,
            HelpTab::GettingStarted,
            HelpTab::ChecksAndUpdates,
            HelpTab::ProfilesAndLaunch,
            HelpTab::RepositorySpacesAndAddons,
            HelpTab::EditorMissions,
            HelpTab::RecoveryAndTools,
            HelpTab::SettingsAndStatus,
            HelpTab::RendererAndPerformance,
            HelpTab::KeyboardShortcuts,
            HelpTab::ThirdPartyOverlays,
            HelpTab::Troubleshooting,
        ]
    }
}

/// Tabs shown at the top of the About view.
///
/// - `About`: the embedded `README.md` overview (default).
/// - `License`: the Foxy Community Source License text (`LICENSE`).
/// - `Licensing`: the human-readable licensing overview (`LICENSING.md`).
/// - `ThirdPartyLicenses`: the generated third-party notices
///   (`THIRD-PARTY-LICENSES.txt`).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AboutTab {
    About,
    License,
    Licensing,
    ThirdPartyLicenses,
}

impl AboutTab {
    pub fn as_str(&self) -> &'static str {
        match self {
            AboutTab::About => "About",
            AboutTab::License => "License",
            AboutTab::Licensing => "Licensing",
            AboutTab::ThirdPartyLicenses => "Third-party licenses",
        }
    }

    pub fn all_tabs() -> [AboutTab; 4] {
        [
            AboutTab::About,
            AboutTab::License,
            AboutTab::Licensing,
            AboutTab::ThirdPartyLicenses,
        ]
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RepoState {
    Synced,
    PendingUpdate,
    Updating,
    Unknown,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RepositorySpaceBulkMode {
    RecheckAll,
    UpdateAll,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RepositorySettingsTab {
    Configuration,
    Addons,
    OptionalAddons,
    ExternalAddons,
}

impl RepositorySettingsTab {
    pub fn as_str(&self) -> &'static str {
        match self {
            RepositorySettingsTab::Configuration => "Configuration",
            RepositorySettingsTab::Addons => "Addons",
            RepositorySettingsTab::OptionalAddons => "Optional Addons",
            RepositorySettingsTab::ExternalAddons => "External Addons",
        }
    }

    pub fn all_tabs() -> [RepositorySettingsTab; 4] {
        [
            RepositorySettingsTab::Configuration,
            RepositorySettingsTab::Addons,
            RepositorySettingsTab::OptionalAddons,
            RepositorySettingsTab::ExternalAddons,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::{HashIoProfilePreference, UiRendererPreference};

    #[test]
    fn hash_io_profile_reads_legacy_hdd_safe_settings() {
        let profile: HashIoProfilePreference = serde_json::from_str("\"HddSafe\"").unwrap();

        assert_eq!(profile, HashIoProfilePreference::Conservative);
    }

    #[test]
    fn hash_io_profile_writes_generic_conservative_name() {
        let serialized = serde_json::to_string(&HashIoProfilePreference::Conservative).unwrap();

        assert_eq!(serialized, "\"Conservative\"");
    }

    #[test]
    fn renderer_preference_defaults_to_auto() {
        let preference: UiRendererPreference = serde_json::from_str("null").unwrap_or_default();

        assert_eq!(preference, UiRendererPreference::Auto);
    }
}
