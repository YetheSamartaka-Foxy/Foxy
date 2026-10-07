use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const STEAM_START_TIMEOUT: Duration = Duration::from_secs(30);
const STEAM_POLL_INTERVAL: Duration = Duration::from_secs(2);
const STEAM_SETTLE_DELAY: Duration = Duration::from_secs(5);
const ARMA3_APP_ID: &str = "107410";

#[cfg(target_os = "windows")]
const STEAM_EXECUTABLE: &str = "steam.exe";

#[cfg(not(target_os = "windows"))]
const STEAM_EXECUTABLE: &str = "steam";

#[cfg(target_os = "windows")]
const ARMA3_EXECUTABLE: &str = "arma3_x64.exe";

#[cfg(not(target_os = "windows"))]
const ARMA3_EXECUTABLE: &str = "arma3_x64.exe";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteamLaunchCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamEnsureResult {
    AlreadyRunning,
    Started,
    SkippedMissingDirectory,
}

pub fn ensure_steam_running(steam_directory: &str) -> Result<SteamEnsureResult, String> {
    if is_steam_running() {
        return Ok(SteamEnsureResult::AlreadyRunning);
    }

    let Some(steam_launch) = resolve_steam_launch_command(steam_directory) else {
        return Ok(SteamEnsureResult::SkippedMissingDirectory);
    };

    spawn_steam(&steam_launch)?;

    let start = Instant::now();
    while start.elapsed() < STEAM_START_TIMEOUT {
        thread::sleep(STEAM_POLL_INTERVAL);
        if is_steam_running() {
            thread::sleep(STEAM_SETTLE_DELAY);
            return Ok(SteamEnsureResult::Started);
        }
    }

    Err("Timed out waiting for Steam to start".to_string())
}

/// Start the Steam client without waiting for it to come up.
///
/// Unlike [`ensure_steam_running`], this returns as soon as the process has
/// been spawned, which keeps the UI responsive when the user clicks a
/// "Launch Steam" button in the pre-launch modal. The running state is then
/// re-polled by the caller via [`is_steam_running`].
pub fn launch_steam(steam_directory: &str) -> Result<(), String> {
    let Some(steam_launch) = resolve_steam_launch_command(steam_directory) else {
        return Err(
            "Could not find the Steam client. Set the Steam directory in Settings or start it manually."
                .to_string(),
        );
    };

    spawn_steam(&steam_launch)
}

/// Start Steam without this process's elevation, so the game Steam launches
/// does not end up with an admin token it would inherit from Foxy.
fn spawn_steam(steam_launch: &SteamLaunchCommand) -> Result<(), String> {
    let args: Vec<std::ffi::OsString> = steam_launch
        .args
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
    crate::core::utils::deelevate::spawn_unelevated(steam_launch.program.as_os_str(), &args, None)
        .map(|_| ())
        .map_err(|err| format!("Failed to start Steam: {}", err))
}

#[allow(dead_code)]
pub fn resolve_steam_executable_path(steam_directory: &str) -> Option<PathBuf> {
    resolve_steam_launch_command(steam_directory).map(|cmd| cmd.program)
}

pub fn resolve_steam_launch_command(steam_directory: &str) -> Option<SteamLaunchCommand> {
    #[cfg(target_os = "windows")]
    {
        let configured = steam_directory.trim();
        let program = if !configured.is_empty() {
            Path::new(configured).join(STEAM_EXECUTABLE)
        } else {
            detect_steam_install_directory()?.join(STEAM_EXECUTABLE)
        };
        Some(SteamLaunchCommand {
            program,
            args: Vec::new(),
        })
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(cmd) = linux_steam_launch_command(steam_directory) {
            return Some(cmd);
        }
        return None;
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = steam_directory;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_steam_launch_command(steam_directory: &str) -> Option<SteamLaunchCommand> {
    let configured = steam_directory.trim();
    if !configured.is_empty() {
        let configured_path = PathBuf::from(configured);
        if configured_path.is_file() {
            return Some(SteamLaunchCommand {
                program: configured_path,
                args: Vec::new(),
            });
        }
        let native = configured_path.join(STEAM_EXECUTABLE);
        if native.exists() {
            return Some(SteamLaunchCommand {
                program: native,
                args: Vec::new(),
            });
        }
        if is_flatpak_steam_data_root(&configured_path) {
            return flatpak_steam_launch_command();
        }
        if is_snap_steam_data_root(&configured_path) {
            return snap_steam_launch_command();
        }
    }

    if flatpak_steam_data_root().is_some() {
        return flatpak_steam_launch_command();
    }
    if snap_steam_data_root().is_some() {
        return snap_steam_launch_command();
    }

    detect_steam_install_directory()
        .map(|path| path.join(STEAM_EXECUTABLE))
        .filter(|path| path.exists())
        .map(|program| SteamLaunchCommand {
            program,
            args: Vec::new(),
        })
        .or_else(native_steam_launch_command)
}

#[cfg(target_os = "linux")]
fn flatpak_steam_launch_command() -> Option<SteamLaunchCommand> {
    if !crate::core::utils::platform::command_exists("flatpak") {
        return None;
    }
    Command::new("flatpak")
        .args(["info", "com.valvesoftware.Steam"])
        .status()
        .ok()
        .filter(|status| status.success())?;
    Some(SteamLaunchCommand {
        program: PathBuf::from("flatpak"),
        args: vec!["run".to_string(), "com.valvesoftware.Steam".to_string()],
    })
}

#[cfg(target_os = "linux")]
fn snap_steam_launch_command() -> Option<SteamLaunchCommand> {
    crate::core::utils::platform::command_exists("snap").then(|| SteamLaunchCommand {
        program: PathBuf::from("snap"),
        args: vec!["run".to_string(), "steam".to_string()],
    })
}

#[cfg(target_os = "linux")]
fn native_steam_launch_command() -> Option<SteamLaunchCommand> {
    crate::core::utils::platform::command_in_path(STEAM_EXECUTABLE).map(|program| {
        SteamLaunchCommand {
            program,
            args: Vec::new(),
        }
    })
}

#[cfg(target_os = "linux")]
fn flatpak_steam_data_root() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let path = home.join(".var/app/com.valvesoftware.Steam/data/Steam");
    path.join("steamapps").exists().then_some(path)
}

#[cfg(target_os = "linux")]
fn snap_steam_data_root() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let path = home.join("snap/steam/common/.local/share/Steam");
    path.join("steamapps").exists().then_some(path)
}

#[cfg(target_os = "linux")]
fn is_flatpak_steam_data_root(path: &Path) -> bool {
    path.to_string_lossy()
        .contains(".var/app/com.valvesoftware.Steam/data/Steam")
}

#[cfg(target_os = "linux")]
fn is_snap_steam_data_root(path: &Path) -> bool {
    path.to_string_lossy()
        .contains("snap/steam/common/.local/share/Steam")
}

/// Returns the Arma 3 executable path within the given directory.
#[allow(dead_code)]
pub fn arma3_executable_path(arma3_dir: &Path) -> PathBuf {
    arma3_dir.join(ARMA3_EXECUTABLE)
}

pub fn arma3_launch_command(arma3_dir: &Path, steam_directory: &str) -> Option<SteamLaunchCommand> {
    steam_app_launch_command(
        ARMA3_APP_ID.parse().ok()?,
        arma3_dir,
        &[ARMA3_EXECUTABLE],
        steam_directory,
    )
}

pub fn steam_app_launch_command(
    app_id: u32,
    install_dir: &Path,
    executable_names: &[&str],
    steam_directory: &str,
) -> Option<SteamLaunchCommand> {
    #[cfg(target_os = "windows")]
    {
        let _ = app_id;
        let _ = steam_directory;
        let executable_name = executable_names.first().copied()?;
        let program = executable_names
            .iter()
            .map(|name| install_dir.join(name))
            .find(|candidate| candidate.exists())
            .unwrap_or_else(|| install_dir.join(executable_name));
        Some(SteamLaunchCommand {
            program,
            args: Vec::new(),
        })
    }

    #[cfg(target_os = "linux")]
    {
        let _ = install_dir;
        let _ = executable_names;
        let mut command = resolve_steam_launch_command(steam_directory)?;
        command.args.push("-applaunch".to_string());
        command.args.push(app_id.to_string());
        Some(command)
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = app_id;
        let _ = install_dir;
        let _ = executable_names;
        let _ = steam_directory;
        None
    }
}

/// Validates whether the given path is a valid Arma 3 installation directory.
pub fn is_valid_arma3_dir(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        path.join(ARMA3_EXECUTABLE).exists()
    }

    #[cfg(target_os = "linux")]
    {
        // On Linux via Proton, check for the exe or the addons directory
        path.exists() && (path.join(ARMA3_EXECUTABLE).exists() || path.join("addons").exists())
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        path.join(ARMA3_EXECUTABLE).exists()
    }
}

pub fn detect_arma3_install_directory(steam_directory: &str) -> Option<PathBuf> {
    detect_steam_app_install_directory(steam_directory, 107410, &["Arma 3"], is_valid_arma3_dir)
}

pub fn detect_steam_app_install_directory(
    steam_directory: &str,
    app_id: u32,
    default_install_dirs: &[&str],
    validate: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let library_roots = steam_library_roots(steam_directory);

    for library_root in &library_roots {
        if let Some(path) = steam_app_dir_from_manifest(library_root, app_id)
            && validate(&path)
        {
            return Some(path);
        }
    }

    for library_root in &library_roots {
        for default_install_dir in default_install_dirs {
            let candidate = library_root
                .join("steamapps")
                .join("common")
                .join(default_install_dir);
            if validate(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

pub fn steam_library_roots(steam_directory: &str) -> Vec<PathBuf> {
    let mut steam_roots = Vec::new();
    let configured = steam_directory.trim();
    if !configured.is_empty() {
        steam_roots.push(PathBuf::from(configured));
    }
    if let Some(path) = detect_steam_install_directory() {
        steam_roots.push(path);
    }
    steam_roots.extend(default_steam_library_candidates());

    let mut library_roots = Vec::new();
    let mut seen = HashSet::new();
    for root in steam_roots {
        add_library_root(&mut library_roots, &mut seen, root.clone());
        for library in read_steam_libraryfolders(&root) {
            add_library_root(&mut library_roots, &mut seen, library);
        }
    }

    library_roots
}

pub fn detect_steam_install_directory() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(path) =
            query_reg_value_for_path(r"HKLM\SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath")
        {
            return Some(path);
        }
        if let Some(path) = query_reg_value_for_path(r"HKCU\Software\Valve\Steam", "SteamPath") {
            return Some(path);
        }
        if let Ok(program_files_x86) = std::env::var("ProgramFiles(x86)") {
            let candidate = Path::new(&program_files_x86).join("Steam");
            if candidate.exists() {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let candidates = [
                home.join(".local/share/Steam"),
                home.join(".steam/steam"),
                home.join(".steam/debian-installation"),
            ];

            for candidate in &candidates {
                if candidate.join("steamapps").exists() || candidate.join("SteamApps").exists() {
                    return Some(candidate.clone());
                }
            }
        }

        // Check STEAM_DIR environment variable as a final fallback
        if let Ok(steam_dir) = std::env::var("STEAM_DIR") {
            let path = PathBuf::from(steam_dir);
            if path.join("steamapps").exists() || path.join("SteamApps").exists() {
                return Some(path);
            }
        }

        if let Some(path) = flatpak_steam_data_root() {
            return Some(path);
        }
        if let Some(path) = snap_steam_data_root() {
            return Some(path);
        }

        None
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        None
    }
}

fn add_library_root(library_roots: &mut Vec<PathBuf>, seen: &mut HashSet<String>, root: PathBuf) {
    if !root.join("steamapps").exists() && !root.join("SteamApps").exists() {
        return;
    }
    let key = root.to_string_lossy().to_lowercase();
    if seen.insert(key) {
        library_roots.push(root);
    }
}

fn default_steam_library_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        for drive in ["C", "D", "E", "F", "H", "I", "J", "K"] {
            let root = format!("{drive}:\\");
            candidates.push(Path::new(&root).join("Program Files (x86)").join("Steam"));
            candidates.push(Path::new(&root).join("Program Files").join("Steam"));
            candidates.push(Path::new(&root).join("Steam"));
            candidates.push(Path::new(&root).join("SteamLibrary"));
        }
        candidates
    }

    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

fn read_steam_libraryfolders(steam_root: &Path) -> Vec<PathBuf> {
    let Some(steamapps) = find_child_dir_case_insensitive(steam_root, "steamapps") else {
        return Vec::new();
    };
    let libraryfolders = steamapps.join("libraryfolders.vdf");
    let Ok(contents) = fs::read_to_string(libraryfolders) else {
        return Vec::new();
    };

    let tokens = vdf_tokens(&contents);
    let mut folders = Vec::new();
    for pair in tokens.windows(2) {
        if pair[0].eq_ignore_ascii_case("path")
            || (pair[0].chars().all(|c| c.is_ascii_digit())
                && (pair[1].contains('\\') || pair[1].contains('/')))
        {
            folders.push(PathBuf::from(unescape_vdf_path(&pair[1])));
        }
    }
    folders
}

/// The installed Steam build id of an app, read from its `appmanifest_*.acf`.
/// Two players on the same game patch report the same value, which is what
/// makes it comparable in a shared state checksum.
pub fn steam_app_build_id(steam_directory: &str, app_id: u32) -> Option<String> {
    for library_root in steam_library_roots(steam_directory) {
        let Some(steamapps) = find_child_dir_case_insensitive(&library_root, "steamapps") else {
            continue;
        };
        let manifest = steamapps.join(format!("appmanifest_{}.acf", app_id));
        let Ok(contents) = fs::read_to_string(manifest) else {
            continue;
        };
        let tokens = vdf_tokens(&contents);
        if let Some(build_id) = tokens
            .windows(2)
            .find(|pair| pair[0].eq_ignore_ascii_case("buildid"))
            .map(|pair| pair[1].trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return Some(build_id);
        }
    }
    None
}

fn steam_app_dir_from_manifest(library_root: &Path, app_id: u32) -> Option<PathBuf> {
    let steamapps = find_child_dir_case_insensitive(library_root, "steamapps")?;
    let app_id_text = app_id.to_string();
    let manifest = steamapps.join(format!("appmanifest_{}.acf", app_id));
    let contents = fs::read_to_string(manifest).ok()?;
    let tokens = vdf_tokens(&contents);

    if !tokens
        .windows(2)
        .any(|pair| pair[0].eq_ignore_ascii_case("appid") && pair[1].trim() == app_id_text)
    {
        return None;
    }

    let install_dir = tokens
        .windows(2)
        .find(|pair| pair[0].eq_ignore_ascii_case("installdir"))
        .map(|pair| unescape_vdf_path(&pair[1]))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| app_id_text.clone());

    Some(steamapps.join("common").join(install_dir))
}

fn find_child_dir_case_insensitive(parent: &Path, child: &str) -> Option<PathBuf> {
    let direct = parent.join(child);
    if direct.exists() {
        return Some(direct);
    }

    let entries = fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(child))
        {
            return Some(path);
        }
    }
    None
}

fn vdf_tokens(contents: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = contents.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            let mut token = String::new();
            let mut escaped = false;
            for next in chars.by_ref() {
                if escaped {
                    token.push(next);
                    escaped = false;
                } else if next == '\\' {
                    escaped = true;
                    token.push(next);
                } else if next == '"' {
                    break;
                } else {
                    token.push(next);
                }
            }
            tokens.push(token);
        }
    }
    tokens
}

fn unescape_vdf_path(path: &str) -> String {
    path.replace("\\\\", "\\")
}

pub fn is_steam_running() -> bool {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq steam.exe", "/FO", "CSV", "/NH"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_lowercase();
                stdout.contains("steam.exe")
            }
            _ => false,
        }
    }
    #[cfg(target_os = "linux")]
    {
        linux_steam_running_sysinfo() || linux_steam_running_pgrep()
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "linux")))]
    {
        Command::new("pgrep")
            .args(["-f", "(^|/)(steam|steamwebhelper)( |$)"])
            .status()
            .is_ok_and(|status| status.success())
    }
}

#[cfg(target_os = "linux")]
fn linux_steam_running_sysinfo() -> bool {
    let system = sysinfo::System::new_all();
    system.processes().values().any(linux_process_is_steam)
}

#[cfg(target_os = "linux")]
fn linux_process_is_steam(process: &sysinfo::Process) -> bool {
    let name = process.name().to_string_lossy().to_ascii_lowercase();
    if linux_process_name_is_steam(&name) {
        return true;
    }

    process.cmd().iter().any(|part| {
        let part = part.to_string_lossy().to_ascii_lowercase();
        part.contains("com.valvesoftware.steam") || linux_process_name_is_steam(&part)
    })
}

#[cfg(target_os = "linux")]
fn linux_process_name_is_steam(name: &str) -> bool {
    matches!(
        name,
        "steam" | "steamwebhelper" | "steam-runtime" | "steam-runtime-launcher"
    ) || name.ends_with("/steam")
        || name.ends_with("/steamwebhelper")
}

#[cfg(target_os = "linux")]
fn linux_steam_running_pgrep() -> bool {
    if !crate::core::utils::platform::command_exists("pgrep") {
        return false;
    }
    Command::new("pgrep")
        .args(["-f", "(^|/)(steam|steamwebhelper)( |$)"])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "windows")]
fn query_reg_value_for_path(key_path: &str, value_name: &str) -> Option<PathBuf> {
    let output = Command::new("reg")
        .args(["query", key_path, "/v", value_name])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with(value_name) {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let value = parts[2..].join(" ");
        if value.is_empty() {
            continue;
        }
        return Some(PathBuf::from(value));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_steam_executable_with_configured_path() {
        let result = resolve_steam_executable_path("C:\\Steam");
        assert_eq!(
            result,
            Some(PathBuf::from(format!("C:\\Steam\\{}", STEAM_EXECUTABLE)))
        );
    }

    #[test]
    fn resolve_steam_executable_with_whitespace_only_falls_back() {
        let result = resolve_steam_executable_path("   ");
        // Falls back to auto-detect; on CI this may be None
        // The key test is that it doesn't return "   /steam.exe"
        if let Some(path) = &result {
            assert!(!path.to_string_lossy().contains("   "));
        }
    }

    #[test]
    fn resolve_steam_executable_empty_string_falls_back() {
        let result = resolve_steam_executable_path("");
        if let Some(path) = &result {
            assert!(path.to_string_lossy().ends_with(STEAM_EXECUTABLE));
        }
    }

    #[test]
    fn resolve_steam_executable_path_trims_configured() {
        let result = resolve_steam_executable_path("  C:\\Games\\Steam  ");
        assert_eq!(
            result,
            Some(PathBuf::from(format!(
                "C:\\Games\\Steam\\{}",
                STEAM_EXECUTABLE
            )))
        );
    }

    #[test]
    fn arma3_executable_path_joins_correctly() {
        let dir = Path::new("/games/arma3");
        let exe = arma3_executable_path(dir);
        assert!(exe.to_string_lossy().contains("arma3"));
    }

    #[test]
    fn steam_ensure_result_equality() {
        assert_eq!(
            SteamEnsureResult::AlreadyRunning,
            SteamEnsureResult::AlreadyRunning
        );
        assert_eq!(SteamEnsureResult::Started, SteamEnsureResult::Started);
        assert_eq!(
            SteamEnsureResult::SkippedMissingDirectory,
            SteamEnsureResult::SkippedMissingDirectory
        );
        assert_ne!(
            SteamEnsureResult::AlreadyRunning,
            SteamEnsureResult::Started
        );
    }

    #[test]
    fn steam_ensure_result_debug_format() {
        let result = SteamEnsureResult::AlreadyRunning;
        let debug_str = format!("{:?}", result);
        assert_eq!(debug_str, "AlreadyRunning");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_name_matching_covers_common_steam_processes() {
        assert!(linux_process_name_is_steam("steam"));
        assert!(linux_process_name_is_steam("steamwebhelper"));
        assert!(linux_process_name_is_steam("/usr/bin/steam"));
        assert!(!linux_process_name_is_steam("notsteam"));
    }

    #[test]
    fn detects_arma3_directory_from_manifest_in_configured_library() {
        let temp = tempfile::tempdir().unwrap();
        let steamapps = temp.path().join("steamapps");
        let arma_dir = steamapps.join("common").join("Arma 3");
        fs::create_dir_all(&arma_dir).unwrap();
        fs::write(arma_dir.join(ARMA3_EXECUTABLE), "").unwrap();
        fs::write(
            steamapps.join("appmanifest_107410.acf"),
            r#""AppState"
{
    "appid" "107410"
    "installdir" "Arma 3"
}"#,
        )
        .unwrap();

        let detected = detect_arma3_install_directory(&temp.path().display().to_string());

        assert_eq!(detected, Some(arma_dir));
    }

    #[test]
    fn detects_arma3_directory_from_libraryfolders_vdf() {
        let temp = tempfile::tempdir().unwrap();
        let steam_root = temp.path().join("Steam");
        let library_root = temp.path().join("SteamLibrary");
        let steamapps = steam_root.join("steamapps");
        let library_steamapps = library_root.join("steamapps");
        let arma_dir = library_steamapps.join("common").join("Arma 3");
        fs::create_dir_all(&steamapps).unwrap();
        fs::create_dir_all(&arma_dir).unwrap();
        fs::write(arma_dir.join(ARMA3_EXECUTABLE), "").unwrap();
        fs::write(
            steamapps.join("libraryfolders.vdf"),
            format!(
                r#""libraryfolders"
{{
    "0"
    {{
        "path" "{}"
    }}
}}"#,
                library_root.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        fs::write(
            library_steamapps.join("appmanifest_107410.acf"),
            r#""AppState"
{
    "appid" "107410"
    "installdir" "Arma 3"
}"#,
        )
        .unwrap();

        let detected = detect_arma3_install_directory(&steam_root.display().to_string());

        assert_eq!(detected, Some(arma_dir));
    }
}
