pub mod addon_workshop;
pub mod arma3;
pub mod extra_files;
pub mod foxypack;
pub mod generic;
pub mod generic_game;
mod launch;
pub(crate) mod launch_preparation;
pub mod profile;
pub mod reforger;
mod registry;
pub mod spaces;
pub mod twwh3;
pub mod workshop;

pub use launch::{
    GameBuildTarget, GameLaunchCtx, LaunchCommand, LaunchError, LaunchPlan, ResolvedMod,
    ServerTarget,
};
pub use profile::Profile;
pub use registry::registry;

use crate::ui::types::{Repository, RepositoryProfile, RepositoryServer, SettingsViewState};
use std::path::{Path, PathBuf};

pub struct GameDetectCtx<'a> {
    pub steam_directory: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DedicatedServerSupport {
    Unsupported,
    WorkInProgress,
}

/// Game-specific capabilities used to gate UI and CLI features without module-id checks.
#[derive(Clone, Copy, Debug)]
pub struct GameCapabilities {
    pub repository_sync: bool,
    /// Repository sync does not imply support for the repository's game-specific addon launch plan.
    pub repository_launch: bool,
    pub steam_workshop: bool,
    /// Whether clients may enable addons absent from the server; Reforger activates
    /// exactly the server's mod set and does not support this capability.
    pub client_side_addons: bool,
    pub direct_download: bool,
    pub extra_files: bool,
    pub profiles: bool,
    pub foxy_config_export: bool,

    pub teamspeak3_plugins: bool,
    /// Creator DLC selection on a repository (the `-mod=` codes the game
    /// resolves from its own install) and the `dlcContent` manifest key.
    pub creator_dlc: bool,
    /// Only Arma 3 publishes addon lists over Steam rules for join-time enablement.
    pub join_addon_preflight: bool,
}

impl GameCapabilities {
    fn flags(&self) -> [(&'static str, bool); 11] {
        [
            ("repository_sync", self.repository_sync),
            ("repository_launch", self.repository_launch),
            ("steam_workshop", self.steam_workshop),
            ("client_side_addons", self.client_side_addons),
            ("direct_download", self.direct_download),
            ("extra_files", self.extra_files),
            ("profiles", self.profiles),
            ("foxy_config_export", self.foxy_config_export),
            ("teamspeak3_plugins", self.teamspeak3_plugins),
            ("creator_dlc", self.creator_dlc),
            ("join_addon_preflight", self.join_addon_preflight),
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
    pub texts: Vec<TextSetting>,
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

/// Custom games supply settings the module cannot know statically, including executables, app IDs and argument templates.
pub struct TextSetting {
    pub id: &'static str,
    pub label: &'static str,
    pub help: Option<&'static str>,
    pub placeholder: &'static str,
}

/// Arma flags use dedicated Repository fields; other games share additional_params with clientParameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchFlagField {
    SkipIntro,
    NoSplash,
    WorldEmpty,
    LoadMissionToMemory,
    EnableHt,
    HugePages,
    NoLogs,
    AdditionalParamToken,
}

pub struct RepositoryLaunchFlag {
    /// The exact token the game accepts, also used as the checkbox label.
    pub flag: &'static str,
    pub help: &'static str,
    pub field: LaunchFlagField,
}

pub trait GameModule: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &str;
    fn capabilities(&self) -> GameCapabilities;
    fn dedicated_server_support(&self) -> DedicatedServerSupport {
        DedicatedServerSupport::Unsupported
    }
    fn server_capabilities(&self) -> crate::core::server::ServerCapabilities {
        crate::core::server::ServerCapabilities::default()
    }
    fn detect_install_dir(&self, ctx: &GameDetectCtx) -> Option<PathBuf>;
    fn validate_install_dir(&self, path: &Path) -> bool;
    fn is_addon_directory(&self, path: &Path) -> bool {
        crate::core::utils::fs_safety::resolve_child_dir_case_insensitive(path, "addons").is_some()
    }
    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError>;
    fn build_launch_preview(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        self.build_launch(plan, ctx)
    }
    fn launch_write_targets(&self, _plan: &LaunchPlan, _ctx: &GameLaunchCtx) -> Vec<PathBuf> {
        Vec::new()
    }
    fn running_game_status(&self, _settings: &SettingsViewState) -> Option<bool> {
        None
    }
    fn settings_schema(&self) -> GameSettingsSchema;

    /// A single format ID is trusted without probing; an empty list requires manifest/local-format detection.
    fn content_formats(&self) -> &'static [&'static str] {
        &[]
    }

    /// Require repository_launch and refuse by default so a new module cannot inherit another game's argument shape.
    fn build_repository_launch_plan(
        &self,
        _settings: &SettingsViewState,
        _repo: &Repository,
        _server: Option<&RepositoryServer>,
    ) -> Result<LaunchPlan, LaunchError> {
        Err(LaunchError::RepositoryLaunchUnsupported)
    }

    fn repository_launch_flags(&self) -> Vec<RepositoryLaunchFlag> {
        Vec::new()
    }

    /// A2S defaults to game port + 1; games with an independent query port must override this.
    fn server_query_port(&self, game_port: u16) -> Option<u16> {
        game_port.checked_add(1)
    }

    fn install_dir_from_settings<'a>(&self, _settings: &'a SettingsViewState) -> &'a str {
        ""
    }

    fn steam_app_id(&self) -> Option<u32> {
        None
    }

    /// Fixed games inherit steam_app_id; custom games resolve their Workshop app from current space settings.
    fn steam_app_id_from_settings(&self, _settings: &SettingsViewState) -> Option<u32> {
        self.steam_app_id()
    }

    fn addon_workshop(
        &self,
        settings: &SettingsViewState,
    ) -> Option<addon_workshop::AddonWorkshop> {
        self.capabilities()
            .steam_workshop
            .then(|| self.steam_app_id_from_settings(settings))
            .flatten()
            .map(addon_workshop::AddonWorkshop::Steam)
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
