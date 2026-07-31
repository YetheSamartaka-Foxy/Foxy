pub mod arma3;
pub mod extra_files;
pub mod foxypack;
pub mod generic;
mod launch;
pub mod profile;
pub mod reforger;
mod registry;
pub mod spaces;
pub mod twwh3;
pub mod workshop;

pub use launch::{
    GameLaunchCtx, LaunchCommand, LaunchError, LaunchPlan, ResolvedMod, ServerTarget,
};
pub use profile::Profile;
pub use registry::registry;

use crate::ui::types::{RepositoryProfile, SettingsViewState};
use std::path::{Path, PathBuf};

pub struct GameDetectCtx<'a> {
    pub steam_directory: &'a str,
}

/// What a game module exposes. Every UI and CLI surface that can be absent for
/// some game gates on a flag here rather than on the module id, so a game that
/// lacks a feature never renders a dead control and adding a module never means
/// editing a shared `id() == "arma3"` check.
#[derive(Clone, Copy, Debug)]
pub struct GameCapabilities {
    /// File-sync repositories (URL + local path, tree hash, delta patch).
    pub repository_sync: bool,
    /// Launching the game from a repository's addon selection. Distinct from
    /// `repository_sync`: a game can sync repository file trees without the
    /// Arma-shaped `-mod=` launch plan being meaningful for it.
    pub repository_launch: bool,
    pub steam_workshop: bool,
    pub direct_download: bool,
    pub extra_files: bool,
    pub profiles: bool,
    pub foxy_config_export: bool,
    /// TeamSpeak 3 plugin discovery/installation and the join-time TS3 gate.
    pub teamspeak3_plugins: bool,
}

impl GameCapabilities {
    fn flags(&self) -> [(&'static str, bool); 8] {
        [
            ("repository_sync", self.repository_sync),
            ("repository_launch", self.repository_launch),
            ("steam_workshop", self.steam_workshop),
            ("direct_download", self.direct_download),
            ("extra_files", self.extra_files),
            ("profiles", self.profiles),
            ("foxy_config_export", self.foxy_config_export),
            ("teamspeak3_plugins", self.teamspeak3_plugins),
        ]
    }

    pub fn summary(&self) -> String {
        self.flags()
            .iter()
            .filter(|(_, enabled)| *enabled)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub struct GameSettingsSchema {
    pub directories: Vec<DirectorySetting>,
    pub toggles: Vec<ToggleSetting>,
}

pub struct DirectorySetting {
    pub id: &'static str,
    pub label: &'static str,
    pub help: Option<&'static str>,
    pub auto_detect: bool,
    pub is_install_dir: bool,
}

impl GameSettingsSchema {
    pub fn install_dir_setting(&self) -> Option<&DirectorySetting> {
        self.directories.iter().find(|entry| entry.is_install_dir)
    }
}

pub struct ToggleSetting {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
}

pub trait GameModule: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &str;
    fn capabilities(&self) -> GameCapabilities;
    fn detect_install_dir(&self, ctx: &GameDetectCtx) -> Option<PathBuf>;
    fn validate_install_dir(&self, path: &Path) -> bool;
    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError>;
    fn settings_schema(&self) -> GameSettingsSchema;

    fn install_dir_from_settings<'a>(&self, _settings: &'a SettingsViewState) -> &'a str {
        ""
    }

    fn steam_app_id(&self) -> Option<u32> {
        None
    }

    fn repository_profile_to_profile(
        &self,
        repository_profile: &RepositoryProfile,
        repository_url: &str,
    ) -> Profile {
        profile::generic_profile_from_repository_profile(repository_profile, repository_url)
    }

    fn profile_to_repository_profile(&self, profile: &Profile) -> RepositoryProfile {
        profile::generic_repository_profile_from_profile(profile)
    }
}
