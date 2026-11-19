use serde::{Deserialize, Serialize};
use std::fmt;

/// Hashing preference: prefer BLAKE3 when FoxyMode is available, or force legacy MD5.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum HashAlgorithmPreference {
    #[default]
    PreferFoxy,
    PreferSwifty,
}

/// Disk-I/O profile for local hashing: Auto benchmarks, Conservative limits concurrency, Balanced moderates it, and Aggressive uses high concurrency.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum HashIoProfilePreference {
    #[default]
    Auto,
    #[serde(alias = "HddSafe")]
    Conservative,
    Balanced,
    Aggressive,
}

/// Renderer preference: Auto falls back after a WGPU crash; Wgpu and Glow force their respective renderers.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum UiRendererPreference {
    #[default]
    Auto,
    Wgpu,
    Glow,
}

/// Worker threads may use adjusted priority; the interface and launched programs stay normal.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum WorkPriorityPreference {
    #[default]
    Normal,
    BelowNormal,
    Lowest,
}

/// Storage type assumed for every drive instead of the detected one.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum StorageTypePreference {
    #[default]
    Auto,
    Ssd,
    Hdd,
    Removable,
}

/// Memory pressure tier assumed instead of the sampled one.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum MemoryPressurePreference {
    #[default]
    Auto,
    Normal,
    Constrained,
    Severe,
}

/// HTTP proxy source: system settings/environment, direct connection, or Foxy's manual proxy.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default, Serialize, Deserialize)]
pub enum ProxyMode {
    #[default]
    System,
    None,
    Manual,
}

/// Update source is a custom server manifest by default, or GitHub Releases.
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

impl fmt::Display for WorkPriorityPreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkPriorityPreference::Normal => write!(f, "normal"),
            WorkPriorityPreference::BelowNormal => write!(f, "below_normal"),
            WorkPriorityPreference::Lowest => write!(f, "lowest"),
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
    ServerInstances,
    Settings,
    RepositorySettings,
    RepositorySpaceSettings,
    Help,
    Changelog,
    About,
    AppUpdate,
    VersionBrowser,
    SwiftyMigration,
    GameSpaces,
    GameSpaceSettings,
    None,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HelpTab {
    Overview,
    GettingStarted,
    GameSpaces,
    ChecksAndUpdates,
    ProfilesAndLaunch,
    ServerAddonProfiles,
    RepositorySpacesAndAddons,
    SteamWorkshopAndExtraFiles,
    EditorMissions,
    RecoveryAndTools,
    SettingsAndStatus,
    Benchmarks,
    RendererAndPerformance,
    CommandLine,
    KeyboardShortcuts,
    ThirdPartyOverlays,
    Troubleshooting,
}

impl HelpTab {
    pub fn as_str(&self) -> &'static str {
        match self {
            HelpTab::Overview => "Overview",
            HelpTab::GettingStarted => "Getting started",
            HelpTab::GameSpaces => "Game spaces",
            HelpTab::ChecksAndUpdates => "Checks and updates",
            HelpTab::ProfilesAndLaunch => "Profiles and launch",
            HelpTab::ServerAddonProfiles => "Server addon profiles",
            HelpTab::RepositorySpacesAndAddons => "Repository spaces and addons",
            HelpTab::SteamWorkshopAndExtraFiles => "Steam Workshop and extra files",
            HelpTab::EditorMissions => "Editor missions",
            HelpTab::RecoveryAndTools => "Recovery and tools",
            HelpTab::SettingsAndStatus => "Settings and status",
            HelpTab::Benchmarks => "Benchmarks",
            HelpTab::RendererAndPerformance => "Renderer and performance",
            HelpTab::CommandLine => "Command line",
            HelpTab::KeyboardShortcuts => "Keyboard shortcuts",
            HelpTab::ThirdPartyOverlays => "Third-party overlays",
            HelpTab::Troubleshooting => "Troubleshooting",
        }
    }

    pub fn all_tabs() -> [HelpTab; 17] {
        [
            HelpTab::Overview,
            HelpTab::GettingStarted,
            HelpTab::GameSpaces,
            HelpTab::ChecksAndUpdates,
            HelpTab::ProfilesAndLaunch,
            HelpTab::ServerAddonProfiles,
            HelpTab::RepositorySpacesAndAddons,
            HelpTab::SteamWorkshopAndExtraFiles,
            HelpTab::EditorMissions,
            HelpTab::RecoveryAndTools,
            HelpTab::SettingsAndStatus,
            HelpTab::Benchmarks,
            HelpTab::RendererAndPerformance,
            HelpTab::CommandLine,
            HelpTab::KeyboardShortcuts,
            HelpTab::ThirdPartyOverlays,
            HelpTab::Troubleshooting,
        ]
    }
}

/// About-view tabs for the overview, Foxy license, licensing guide, and generated third-party notices.
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
