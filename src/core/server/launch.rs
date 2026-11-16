#![cfg_attr(not(test), allow(dead_code))]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{Arma3ConfigurationNames, ServerDefinitionKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub arguments: Vec<OsString>,
    pub spawned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchError {
    Executable,
}

pub fn dedicated_plan(
    kind: ServerDefinitionKind,
    installation_dir: &Path,
    executable_name: &str,
    runtime_dir: &Path,
    port: u16,
    names: &Arma3ConfigurationNames,
) -> Result<LaunchPlan, LaunchError> {
    if executable_name.is_empty() || executable_name.contains(['/', '\\']) {
        return Err(LaunchError::Executable);
    }
    let executable = installation_dir.join(executable_name);
    let mut arguments = Vec::new();
    match kind {
        ServerDefinitionKind::Arma3 => {
            arguments.push(OsString::from(format!("-port={port}")));
            arguments.push(OsString::from(format!(
                "-config={}",
                runtime_dir.join(names.server_file()).display()
            )));
            arguments.push(OsString::from(format!(
                "-cfg={}",
                runtime_dir.join(names.basic_file()).display()
            )));
            arguments.push(OsString::from(format!(
                "-profiles={}",
                runtime_dir.join("profiles").display()
            )));
            arguments.push(OsString::from(format!("-name={}", names.profile_name())));
        }
        ServerDefinitionKind::Reforger => {
            arguments.push(OsString::from("-config"));
            arguments.push(OsString::from(runtime_dir.join("server.json")));
            arguments.push(OsString::from("-profile"));
            arguments.push(OsString::from(runtime_dir.join("profile-root")));
            arguments.push(OsString::from("-addonDownloadDir"));
            arguments.push(OsString::from(runtime_dir.join("downloads")));
            arguments.push(OsString::from(format!("-bindPort={port}")));
        }
    }
    Ok(LaunchPlan {
        executable,
        directory: installation_dir.to_path_buf(),
        arguments,
        spawned: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_plans_do_not_spawn_or_carry_a_password() {
        let install = Path::new("R:/Games/Arma3Server");
        let runtime = Path::new("games/alpha/server/instances/server_a/runtime");
        let arma = dedicated_plan(
            ServerDefinitionKind::Arma3,
            install,
            "arma3server_x64.exe",
            runtime,
            2302,
            &Arma3ConfigurationNames::default(),
        )
        .unwrap();
        assert!(!arma.spawned);
        assert!(arma.executable.ends_with("arma3server_x64.exe"));
        let rendered = arma
            .arguments
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(rendered.contains("-port=2302"));
        assert!(rendered.contains("server.cfg"));
        assert!(rendered.contains("-name=FoxyServer"));
        assert!(!rendered.to_ascii_lowercase().contains("password"));
        let renamed = dedicated_plan(
            ServerDefinitionKind::Arma3,
            install,
            "arma3server_x64.exe",
            runtime,
            2302,
            &Arma3ConfigurationNames {
                server: Some("dedicated.cfg".into()),
                basic: Some("network.cfg".into()),
                profile_name: Some("Managed".into()),
            },
        )
        .unwrap();
        let renamed = renamed
            .arguments
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            renamed
                .iter()
                .any(|arg| arg.starts_with("-config=") && arg.ends_with("dedicated.cfg"))
        );
        assert!(
            renamed
                .iter()
                .any(|arg| arg.starts_with("-cfg=") && arg.ends_with("network.cfg"))
        );
        assert!(renamed.iter().any(|arg| arg == "-name=Managed"));
    }
}
