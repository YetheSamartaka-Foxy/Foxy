use super::SteamLaunchCommand;
use std::ffi::{CStr, CString, OsString};
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use winapi::um::heapapi::{GetProcessHeap, HeapFree};
use winapi::um::libloaderapi::{GetModuleHandleW, GetProcAddress};

pub(super) fn is_wine() -> bool {
    static IS_WINE: OnceLock<bool> = OnceLock::new();
    *IS_WINE.get_or_init(|| unsafe {
        let module = GetModuleHandleW(wide("ntdll.dll").as_ptr());
        !module.is_null() && !GetProcAddress(module, c"wine_get_version".as_ptr()).is_null()
    })
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

pub(super) fn dos_path(path: &str) -> Option<PathBuf> {
    let path = CString::new(path).ok()?;
    unsafe {
        let module = GetModuleHandleW(wide("kernel32.dll").as_ptr());
        if module.is_null() {
            return None;
        }
        let address = GetProcAddress(module, c"wine_get_dos_file_name".as_ptr());
        if address.is_null() {
            return None;
        }
        let convert: unsafe extern "C" fn(*const i8) -> *mut u16 = std::mem::transmute(address);
        let buffer = convert(path.as_ptr());
        if buffer.is_null() {
            return None;
        }
        let mut len = 0;
        while *buffer.add(len) != 0 {
            len += 1;
        }
        let result = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(buffer, len)));
        HeapFree(GetProcessHeap(), 0, buffer.cast());
        Some(result)
    }
}

pub(super) fn unix_path(path: &Path) -> Option<String> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let module = GetModuleHandleW(wide("kernel32.dll").as_ptr());
        if module.is_null() {
            return None;
        }
        let address = GetProcAddress(module, c"wine_get_unix_file_name".as_ptr());
        if address.is_null() {
            return None;
        }
        let convert: unsafe extern "C" fn(*const u16) -> *mut i8 = std::mem::transmute(address);
        let buffer = convert(path.as_ptr());
        if buffer.is_null() {
            return None;
        }
        let result = CStr::from_ptr(buffer).to_str().ok().map(str::to_owned);
        HeapFree(GetProcessHeap(), 0, buffer.cast());
        result
    }
}

fn host_env(name: &str) -> Option<String> {
    std::env::var(format!("WINE_HOST_{name}")).ok().or_else(|| {
        let contents = fs::read(dos_path("/proc/self/environ")?).ok()?;
        contents.split(|byte| *byte == 0).find_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            entry.strip_prefix(&format!("{name}=")).map(str::to_owned)
        })
    })
}

pub(super) fn library_candidates() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("WINEHOMEDIR")
        .and_then(|path| unix_path(Path::new(&path)))
        .or_else(|| host_env("HOME"))
    else {
        return Vec::new();
    };
    [
        ".local/share/Steam",
        ".steam/steam",
        ".steam/debian-installation",
        ".var/app/com.valvesoftware.Steam/data/Steam",
        "snap/steam/common/.local/share/Steam",
    ]
    .into_iter()
    .filter_map(|suffix| dos_path(&format!("{home}/{suffix}")))
    .collect()
}

fn host_command(name: &str) -> Option<String> {
    host_env("PATH")
        .unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin:/usr/games:/snap/bin".to_string())
        .split(':')
        .filter(|directory| directory.starts_with('/'))
        .map(|directory| format!("{directory}/{name}"))
        .find(|path| dos_path(path).is_some_and(|path| path.is_file()))
}

pub(super) fn steam_launch_command(configured: &str) -> Option<SteamLaunchCommand> {
    let roots = library_candidates();
    let configured_path = if configured.trim().starts_with('/') {
        dos_path(configured.trim())
    } else {
        Some(PathBuf::from(configured.trim()))
    };
    if configured_path
        .as_deref()
        .is_some_and(windows_steam_configured)
    {
        return None;
    }
    let selected_path = configured_path
        .as_deref()
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            roots
                .iter()
                .find(|root| root.join("steamapps").is_dir())
                .map(PathBuf::as_path)
        });
    let selected_root = selected_path.and_then(unix_path).unwrap_or_default();
    let configured_launcher = selected_path
        .and_then(launcher_in_root)
        .and_then(|path| unix_path(&path));
    let (launcher, prefix) = launcher_for_root(&selected_root);
    let (program, args) = if launcher == "steam" {
        (
            configured_launcher.or_else(|| host_command("steam"))?,
            Vec::new(),
        )
    } else {
        (
            host_command(launcher)?,
            prefix.iter().map(|arg| arg.to_string()).collect(),
        )
    };
    let (program, args) = if program.ends_with(".sh") {
        let mut script_args = vec![program];
        script_args.extend(args);
        (host_command("env")?, script_args)
    } else {
        (program, args)
    };
    let windows = std::env::var_os("WINDIR")?;
    let bridge = PathBuf::from(windows).join("system32/start.exe");
    bridge
        .is_file()
        .then(|| native_launch(bridge, program, args))
}

fn launcher_in_root(path: &Path) -> Option<PathBuf> {
    if path.is_file()
        && !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Some(path.to_path_buf());
    }
    ["steam", "steam.sh"]
        .into_iter()
        .map(|name| path.join(name))
        .find(|path| path.is_file())
}

fn windows_steam_configured(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && (path.join("steam.exe").is_file()
            || (path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("steam.exe"))))
}

fn launcher_for_root(root: &str) -> (&'static str, &'static [&'static str]) {
    if root.contains("/.var/app/com.valvesoftware.Steam/") {
        ("flatpak", &["run", "com.valvesoftware.Steam"])
    } else if root.contains("/snap/steam/") {
        ("snap", &["run", "steam"])
    } else {
        ("steam", &[])
    }
}

fn native_launch(bridge: PathBuf, program: String, args: Vec<String>) -> SteamLaunchCommand {
    // Wine's direct Unix spawn has no Windows process handle; start.exe provides a normal child.
    let mut bridge_args = vec!["/unix".to_string(), program];
    bridge_args.extend(args);
    SteamLaunchCommand {
        program: bridge,
        args: bridge_args,
    }
}

pub(super) fn is_native_launch(command: &SteamLaunchCommand) -> bool {
    command.args.first().is_some_and(|arg| arg == "/unix")
}

pub(super) fn steam_running() -> bool {
    dos_path("/proc").is_some_and(|root| steam_running_in(&root))
}

fn steam_running_in(root: &Path) -> bool {
    let Some(uid) = fs::read_to_string(root.join("self/status"))
        .ok()
        .and_then(|status| process_uid(&status))
    else {
        return false;
    };
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        !name.is_empty()
            && name
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            && fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "steam")
            && fs::read_to_string(entry.path().join("status"))
                .is_ok_and(|status| process_uid(&status) == Some(uid))
    })
}

fn process_uid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        line.strip_prefix("Uid:")?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    })
}

pub(super) fn arma3_args(
    args: Vec<String>,
    convert: impl Fn(&Path) -> Option<String>,
) -> Vec<String> {
    let mut args: Vec<String> = args
        .into_iter()
        .map(|arg| {
            let Some((flag, value)) = arg.split_once('=') else {
                return proton_path(&arg, &convert);
            };
            if !["-mod", "-profiles", "-par", "-cfg", "-config", "-bepath"]
                .iter()
                .any(|candidate| flag.eq_ignore_ascii_case(candidate))
            {
                return arg;
            }
            let value = if flag.eq_ignore_ascii_case("-mod") {
                value
                    .split(';')
                    .map(|value| proton_path(value, &convert))
                    .collect::<Vec<_>>()
                    .join(";")
            } else {
                proton_path(value, &convert)
            };
            format!("{flag}={value}")
        })
        .collect();
    if !args
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case("-nolauncher"))
    {
        args.insert(0, "-nolauncher".to_string());
    }
    args
}

pub(super) fn proton_path(value: &str, convert: &impl Fn(&Path) -> Option<String>) -> String {
    let unix = if value.starts_with('/') {
        Some(value.to_string())
    } else if Path::new(value).is_absolute() {
        convert(Path::new(value))
    } else {
        None
    };
    // The game's Proton prefix maps Z: to the host root, independently of Foxy's drives.
    unix.map(|path| format!("Z:{}", path.replace('/', "\\")))
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bridge_keeps_launcher_and_game_arguments_separate() {
        let mut command = native_launch(
            PathBuf::from("R:/Wine/start.exe"),
            "/usr/bin/flatpak".to_string(),
            vec!["run".to_string(), "com.valvesoftware.Steam".to_string()],
        );
        command.args.extend([
            "-applaunch".to_string(),
            "107410".to_string(),
            "-mod=Z:\\Mods\\@mod_01;gm".to_string(),
        ]);
        assert_eq!(command.program, PathBuf::from("R:/Wine/start.exe"));
        assert_eq!(
            command.args,
            [
                "/unix",
                "/usr/bin/flatpak",
                "run",
                "com.valvesoftware.Steam",
                "-applaunch",
                "107410",
                "-mod=Z:\\Mods\\@mod_01;gm"
            ]
        );
        assert!(is_native_launch(&command));
        assert!(!is_native_launch(&SteamLaunchCommand {
            program: PathBuf::from("steam.exe"),
            args: Vec::new()
        }));
    }

    #[test]
    fn host_process_probe_requires_a_real_steam_process() {
        let root = tempfile::tempdir().unwrap();
        assert!(!steam_running_in(root.path()));
        fs::create_dir(root.path().join("self")).unwrap();
        fs::write(
            root.path().join("self/status"),
            "Uid:\t1000\t1000\t1000\t1000\n",
        )
        .unwrap();
        fs::create_dir(root.path().join("100")).unwrap();
        fs::write(
            root.path().join("100/status"),
            "Uid:\t1001\t1001\t1001\t1001\n",
        )
        .unwrap();
        fs::write(root.path().join("100/comm"), "notsteam\n").unwrap();
        assert!(!steam_running_in(root.path()));
        fs::write(root.path().join("100/comm"), "steamwebhelper\n").unwrap();
        assert!(!steam_running_in(root.path()));
        fs::write(root.path().join("100/comm"), "steam\n").unwrap();
        assert!(!steam_running_in(root.path()));
        fs::write(
            root.path().join("100/status"),
            "Uid:\t1000\t1000\t1000\t1000\n",
        )
        .unwrap();
        assert!(steam_running_in(root.path()));
        fs::remove_file(root.path().join("100/comm")).unwrap();
        assert!(!steam_running_in(root.path()));
        assert!(!steam_running_in(&root.path().join("missing")));
    }

    #[test]
    fn steam_launcher_matches_the_selected_native_or_packaged_library() {
        assert_eq!(launcher_for_root("/Steam"), ("steam", &[][..]));
        assert_eq!(
            launcher_for_root("/Home/.var/app/com.valvesoftware.Steam/data/Steam"),
            ("flatpak", &["run", "com.valvesoftware.Steam"][..])
        );
        assert_eq!(
            launcher_for_root("/Home/snap/steam/common/.local/share/Steam"),
            ("snap", &["run", "steam"][..])
        );
    }

    #[test]
    fn explicitly_configured_windows_steam_keeps_the_windows_launch_path() {
        let root = tempfile::tempdir().unwrap();
        assert!(!windows_steam_configured(root.path()));
        assert!(!windows_steam_configured(Path::new("")));
        let executable = root.path().join("steam.exe");
        fs::write(&executable, "").unwrap();
        assert!(windows_steam_configured(root.path()));
        assert!(windows_steam_configured(&executable));
    }

    #[test]
    fn steam_data_root_uses_its_script_instead_of_a_directory() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(launcher_in_root(root.path()), None);
        fs::create_dir(root.path().join("steam")).unwrap();
        let script = root.path().join("steam.sh");
        fs::write(&script, "").unwrap();
        assert_eq!(launcher_in_root(root.path()), Some(script.clone()));
        assert_eq!(launcher_in_root(&script), Some(script));
    }

    #[test]
    fn steam_data_root_prefers_its_native_launcher_when_present() {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("steam");
        fs::write(&native, "").unwrap();
        fs::write(root.path().join("steam.sh"), "").unwrap();
        assert_eq!(launcher_in_root(root.path()), Some(native));
    }

    #[test]
    fn process_uid_requires_the_effective_numeric_uid() {
        assert_eq!(
            process_uid("Name:\tsteam\nUid:\t1000\t1001\t1000\t1000\n"),
            Some(1001)
        );
        assert_eq!(process_uid("Uid:\t1000\n"), None);
        assert_eq!(process_uid("Uid:\t1000\tinvalid\n"), None);
        assert_eq!(process_uid(""), None);
    }

    #[test]
    fn arma_paths_cross_prefixes_without_changing_mod_codes_or_server_arguments() {
        let args = arma3_args(
            vec![
                "-mod=R:\\Mods\\@mod_01;gm".to_string(),
                "-profiles=C:\\Profiles".to_string(),
                "-connect=127.0.0.1".to_string(),
                "-port=2302".to_string(),
                "-password=sample password".to_string(),
            ],
            |path| match path.to_str()? {
                "R:\\Mods\\@mod_01" => Some("/Mods/@mod_01".to_string()),
                "C:\\Profiles" => Some("/Profiles With Spaces".to_string()),
                _ => None,
            },
        );
        assert_eq!(
            args,
            [
                "-nolauncher",
                "-mod=Z:\\Mods\\@mod_01;gm",
                "-profiles=Z:\\Profiles With Spaces",
                "-connect=127.0.0.1",
                "-port=2302",
                "-password=sample password"
            ]
        );
    }

    #[test]
    fn unmapped_paths_and_existing_nolauncher_are_preserved() {
        let args = vec![
            "-nolauncher".to_string(),
            "-mod=R:\\Mods\\@mod_01;@mod_02".to_string(),
            "-window".to_string(),
        ];
        assert_eq!(arma3_args(args.clone(), |_| None), args);
    }

    #[test]
    fn unix_paths_and_editor_missions_use_the_proton_host_drive() {
        let args = arma3_args(
            vec![
                "-mod=/Mods/@mod_01;gm".to_string(),
                "R:\\Missions\\mission.sqm".to_string(),
            ],
            |path| {
                (path == Path::new("R:\\Missions\\mission.sqm"))
                    .then(|| "/Missions/mission.sqm".to_string())
            },
        );
        assert_eq!(
            args,
            [
                "-nolauncher",
                "-mod=Z:\\Mods\\@mod_01;gm",
                "Z:\\Missions\\mission.sqm"
            ]
        );
    }
}
