use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMod {
    pub id: String,
    pub path: Option<String>,
}

impl ResolvedMod {
    /// The value passed to the game's mod list: the resolved on-disk path,
    /// or the bare id for entries the game mounts by name (Creator DLC codes).
    pub fn launch_value(&self) -> &str {
        self.path.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerTarget {
    pub address: String,
    pub port: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub launch_args: Vec<String>,
    pub mods: Vec<ResolvedMod>,
    pub server: Option<ServerTarget>,
}

pub struct GameLaunchCtx<'a> {
    pub install_dir: &'a str,
    pub steam_directory: &'a str,
    /// Present when the caller has the live settings. A user-configured game
    /// reads its executable, app id, and argument template from here.
    pub settings: Option<&'a crate::ui::types::SettingsViewState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl LaunchCommand {
    pub fn into_process_command(self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchError {
    InstallDirNotConfigured,
    InstallDirMissing,
    InstallDirInvalid,
    LauncherUnavailable,
    LaunchPreparationFailed,
    RepositoryLaunchUnsupported,
    GameNotConfigured,
}
