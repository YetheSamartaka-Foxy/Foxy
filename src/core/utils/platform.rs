#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn command_in_path(command: &str) -> Option<PathBuf> {
    if command.trim().is_empty() {
        return None;
    }

    let command_path = Path::new(command);
    if command_path.components().count() > 1 {
        return command_path.is_file().then(|| command_path.to_path_buf());
    }

    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }

        #[cfg(target_os = "windows")]
        {
            let pathext = std::env::var_os("PATHEXT")
                .map(|value| {
                    value
                        .to_string_lossy()
                        .split(';')
                        .map(|ext| ext.trim().trim_start_matches('.').to_string())
                        .filter(|ext| !ext.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["exe".to_string(), "bat".to_string(), "cmd".to_string()]);
            for ext in pathext {
                let candidate = dir.join(format!("{command}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

pub fn command_exists(command: &str) -> bool {
    command_in_path(command).is_some()
}

pub fn open_with_default_app(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .args(["/c", "start", "", &path.display().to_string()])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|err| format!("Failed to open {}: {}", path.display(), err))?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|err| format!("Failed to open {}: {}", path.display(), err))?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        for opener in linux_openers() {
            let Some(program) = command_in_path(opener.program) else {
                continue;
            };
            let mut command = Command::new(program);
            command.args(opener.args);
            command.arg(path.as_os_str());
            match command.spawn() {
                Ok(_) => return Ok(()),
                Err(err) => {
                    log::warn!(
                        "Failed to open {} with {}: {}",
                        path.display(),
                        opener.program,
                        err
                    );
                }
            }
        }
        return Err(format!(
            "Failed to open {}: no supported Linux opener found in PATH",
            path.display()
        ));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        Err("Opening files is not supported on this platform".to_string())
    }
}

#[cfg(target_os = "linux")]
struct LinuxOpener {
    program: &'static str,
    args: &'static [&'static str],
}

#[cfg(target_os = "linux")]
fn linux_openers() -> &'static [LinuxOpener] {
    &[
        LinuxOpener {
            program: "xdg-open",
            args: &[],
        },
        LinuxOpener {
            program: "gio",
            args: &["open"],
        },
        LinuxOpener {
            program: "kde-open5",
            args: &[],
        },
        LinuxOpener {
            program: "kde-open",
            args: &[],
        },
        LinuxOpener {
            program: "gnome-open",
            args: &[],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn command_in_path_rejects_empty_command() {
        assert!(command_in_path("").is_none());
        assert!(command_in_path("   ").is_none());
    }

    #[test]
    fn os_str_can_be_normalized_for_matching() {
        let normalized = OsStr::new("SteamWebHelper")
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(normalized.contains("steamwebhelper"));
    }
}
