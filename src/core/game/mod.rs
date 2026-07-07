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

#[derive(Clone, Copy, Debug)]
pub struct GameCapabilities {
    pub repository_sync: bool,
    pub steam_workshop: bool,
    pub direct_download: bool,
    pub extra_files: bool,
    pub profiles: bool,
    pub foxy_config_export: bool,
}

impl GameCapabilities {
    pub fn summary(&self) -> String {
        let mut enabled = Vec::new();
        if self.repository_sync {
            enabled.push("repository_sync");
        }
        if self.steam_workshop {
            enabled.push("steam_workshop");
        }
        if self.direct_download {
            enabled.push("direct_download");
        }
        if self.extra_files {
            enabled.push("extra_files");
        }
        if self.profiles {
            enabled.push("profiles");
        }
        if self.foxy_config_export {
            enabled.push("foxy_config_export");
        }
        enabled.join(", ")
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
