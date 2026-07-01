use crate::core::utils::format::{sanitize_log_path, sanitize_log_path_str};
use log::{debug, info, warn};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Information about a detected TS3 plugin file inside a repository addon.
#[derive(Debug, Clone)]
pub struct Ts3PluginInfo {
    /// Display-friendly addon folder name (e.g. `@task_force_radio`).
    pub addon_name: String,
    /// Absolute path to the `.ts3_plugin` file.
    pub plugin_path: PathBuf,
    /// BLAKE3 hex hash (first 32 chars) of the file content.
    pub file_hash: String,
}

/// Diagnostic result for a best-effort lookup of installed TeamSpeak plugin files.
#[derive(Debug, Clone)]
pub struct Ts3InstalledPluginLookup {
    pub search_name: String,
    pub expected_files: Vec<String>,
    pub checked_dirs: Vec<PathBuf>,
    pub existing_dirs: Vec<PathBuf>,
    pub matched_files: Vec<PathBuf>,
    pub missing_files: Vec<String>,
    pub hash_mismatched_files: Vec<PathBuf>,
    pub is_installed: bool,
    pub is_up_to_date: bool,
}

#[derive(Debug, Clone)]
struct Ts3ExpectedInstalledFile {
    relative_path: String,
    package_hash: String,
}

/// Scan a single repository local path for `.ts3_plugin` files.
///
/// Walks one level of addon directories (folders starting with `@`) and checks
/// up to two subdirectory levels for any `*.ts3_plugin` file.
pub fn scan_repository_for_ts3_plugins(repo_path: &str) -> Vec<Ts3PluginInfo> {
    let base = Path::new(repo_path);
    if !base.is_dir() {
        info!(
            "Skipping TS3 plugin scan because repository path is not a directory: {}",
            sanitize_log_path_str(repo_path)
        );
        return Vec::new();
    }

    let mut results = Vec::new();

    let Ok(entries) = fs::read_dir(base) else {
        warn!(
            "Failed to read repository directory while scanning for TS3 plugins: {}",
            sanitize_log_path(base)
        );
        return results;
    };

    info!(
        "Scanning repository for TS3 plugins: {}",
        sanitize_log_path(base)
    );

    let mut addon_dirs_crawled = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Only scan addon folders (start with @)
        if !dir_name.starts_with('@') {
            continue;
        }

        addon_dirs_crawled += 1;
        let mut visited = std::collections::HashSet::new();
        collect_ts3_plugins_recursive(&path, dir_name, &mut results, 0, 3, &mut visited);
    }

    info!(
        "Finished TS3 plugin scan for repository: path={} addon_dirs_crawled={} plugins_found={} found_plugins={}",
        sanitize_log_path(base),
        addon_dirs_crawled,
        results.len(),
        format_detected_plugin_list(&results)
    );
    results
}

/// Scan all repository paths and collect TS3 plugin info.
pub fn scan_all_repositories_for_ts3_plugins(repo_paths: &[String]) -> Vec<Ts3PluginInfo> {
    info!(
        "Starting TS3 plugin scan across repositories: repository_count={}",
        repo_paths.len()
    );
    let mut all = Vec::new();
    for path in repo_paths {
        all.extend(scan_repository_for_ts3_plugins(path));
    }
    let before_dedup = all.len();
    // Deduplicate by plugin path
    all.sort_by(|a, b| a.plugin_path.cmp(&b.plugin_path));
    all.dedup_by(|a, b| a.plugin_path == b.plugin_path);
    info!(
        "Finished TS3 plugin scan across repositories: found_before_dedup={} found_after_dedup={}",
        before_dedup,
        all.len()
    );
    all
}

fn collect_ts3_plugins_recursive(
    dir: &Path,
    addon_name: &str,
    results: &mut Vec<Ts3PluginInfo>,
    depth: usize,
    max_depth: usize,
    visited: &mut std::collections::HashSet<PathBuf>,
) {
    if depth >= max_depth {
        return;
    }

    // Detect symlink loops by tracking canonical paths
    let canonical = match dir.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Failed to canonicalize directory while scanning for TS3 plugins: path={} error={}",
                sanitize_log_path(dir),
                e
            );
            return;
        }
    };
    if !visited.insert(canonical) {
        warn!(
            "Skipping already-visited directory (possible symlink loop): {}",
            sanitize_log_path(dir)
        );
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        warn!(
            "Failed to read directory while scanning for TS3 plugins: {}",
            sanitize_log_path(dir)
        );
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && ext.eq_ignore_ascii_case("ts3_plugin")
            {
                match compute_plugin_hash(&path) {
                    Ok(hash) => {
                        info!(
                            "Detected TS3 plugin: addon={} path={} hash={}",
                            addon_name,
                            sanitize_log_path(&path),
                            hash
                        );
                        results.push(Ts3PluginInfo {
                            addon_name: addon_name.to_string(),
                            plugin_path: path,
                            file_hash: hash,
                        });
                    }
                    Err(e) => {
                        warn!(
                            "Failed to hash TS3 plugin: path={} error={}",
                            sanitize_log_path(&path),
                            e
                        );
                    }
                }
            }
        } else if path.is_dir() {
            collect_ts3_plugins_recursive(
                &path,
                addon_name,
                results,
                depth + 1,
                max_depth,
                visited,
            );
        }
    }
}

/// Compute a BLAKE3 hash of the plugin file, returning the first 32 hex chars
/// (matching the convention used elsewhere in the codebase).
pub fn compute_plugin_hash(path: &Path) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    hash_reader(&mut file)
}

fn hash_reader(reader: &mut impl Read) -> Result<String, std::io::Error> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex()[..32].to_uppercase())
}

/// Search common TeamSpeak 3 client plugin folders for files installed by a
/// repository `.ts3_plugin` package.
pub fn lookup_installed_teamspeak_plugin(package_path: &Path) -> Ts3InstalledPluginLookup {
    let search_name = package_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let expected_files = read_expected_installed_files(package_path);
    let expected_file_names = expected_files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<Vec<_>>();
    let checked_dirs = teamspeak_plugin_search_dirs();
    let mut existing_dirs = Vec::new();
    let mut matched_files = Vec::new();
    let mut missing_files = Vec::new();
    let mut hash_mismatched_files = Vec::new();

    info!(
        "Searching TeamSpeak 3 installed plugin folders: package={} search_name={} expected_files={} candidate_dirs={}",
        sanitize_log_path(package_path),
        search_name,
        format_string_list(&expected_file_names),
        format_path_list(&checked_dirs)
    );

    if search_name.is_empty() && expected_files.is_empty() {
        warn!(
            "Skipping TeamSpeak 3 installed plugin lookup because package file stem and install payload are empty: {}",
            sanitize_log_path(package_path)
        );
        return Ts3InstalledPluginLookup {
            search_name,
            expected_files: expected_file_names,
            checked_dirs,
            existing_dirs,
            matched_files,
            missing_files,
            hash_mismatched_files,
            is_installed: false,
            is_up_to_date: false,
        };
    }

    for dir in &checked_dirs {
        if !dir.is_dir() {
            continue;
        }
        existing_dirs.push(dir.clone());
        if expected_files.is_empty() {
            collect_matching_installed_plugins(dir, &search_name, &mut matched_files, 0, 3);
            continue;
        }

        let mut candidate_matches = Vec::new();
        let mut candidate_missing = Vec::new();
        let mut candidate_hash_mismatches = Vec::new();
        compare_expected_installed_files(
            dir,
            &expected_files,
            &mut candidate_matches,
            &mut candidate_missing,
            &mut candidate_hash_mismatches,
        );

        if candidate_missing.is_empty() {
            matched_files.extend(candidate_matches);
            hash_mismatched_files.extend(candidate_hash_mismatches);
            missing_files.clear();
            break;
        }

        if matched_files.is_empty() || candidate_missing.len() < missing_files.len() {
            matched_files = candidate_matches;
            missing_files = candidate_missing;
            hash_mismatched_files = candidate_hash_mismatches;
        }
    }

    matched_files.sort();
    matched_files.dedup();
    missing_files.sort();
    missing_files.dedup();
    hash_mismatched_files.sort();
    hash_mismatched_files.dedup();
    let is_installed = if expected_files.is_empty() {
        !matched_files.is_empty()
    } else {
        missing_files.is_empty() && !matched_files.is_empty()
    };
    let is_up_to_date = is_installed && hash_mismatched_files.is_empty();
    info!(
        "Finished TeamSpeak 3 installed plugin lookup: package={} search_name={} expected_files={} existing_dirs={} matches={} missing_files={} hash_mismatches={} installed={} up_to_date={} matched_files={}",
        sanitize_log_path(package_path),
        search_name,
        expected_file_names.len(),
        format_path_list(&existing_dirs),
        matched_files.len(),
        missing_files.len(),
        hash_mismatched_files.len(),
        is_installed,
        is_up_to_date,
        format_path_list(&matched_files)
    );

    Ts3InstalledPluginLookup {
        search_name,
        expected_files: expected_file_names,
        checked_dirs,
        existing_dirs,
        matched_files,
        missing_files,
        hash_mismatched_files,
        is_installed,
        is_up_to_date,
    }
}

fn read_expected_installed_files(package_path: &Path) -> Vec<Ts3ExpectedInstalledFile> {
    let Ok(file) = fs::File::open(package_path) else {
        warn!(
            "Failed to open TS3 plugin package for install payload inspection: {}",
            sanitize_log_path(package_path)
        );
        return Vec::new();
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        warn!(
            "Failed to read TS3 plugin package as zip archive: {}",
            sanitize_log_path(package_path)
        );
        return Vec::new();
    };

    let mut expected_files = Vec::new();
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }

        let Some(enclosed_name) = entry.enclosed_name() else {
            warn!(
                "Skipping unsafe TS3 plugin archive entry: package={} entry={}",
                sanitize_log_path(package_path),
                entry.name()
            );
            continue;
        };
        let relative_path = normalize_plugins_payload_path(&enclosed_name);
        let Some(relative_path) = relative_path else {
            continue;
        };
        if !platform_payload_is_compatible(&relative_path) {
            continue;
        }

        match hash_reader(&mut entry) {
            Ok(package_hash) => expected_files.push(Ts3ExpectedInstalledFile {
                relative_path,
                package_hash,
            }),
            Err(err) => warn!(
                "Failed to hash TS3 plugin archive entry: package={} entry={} error={}",
                sanitize_log_path(package_path),
                entry.name(),
                err
            ),
        }
    }

    expected_files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    expected_files.dedup_by(|a, b| a.relative_path == b.relative_path);
    expected_files
}

fn normalize_plugins_payload_path(path: &Path) -> Option<String> {
    let mut components = path.components();
    let first = components.next()?.as_os_str().to_str()?;
    if !first.eq_ignore_ascii_case("plugins") {
        return None;
    }

    let relative = components
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if relative.is_empty() {
        return None;
    }
    Some(relative.join("/"))
}

fn platform_payload_is_compatible(relative_path: &str) -> bool {
    let lower = relative_path.to_ascii_lowercase();
    let is_windows_binary = lower.ends_with(".dll");
    let is_linux_binary = lower.ends_with(".so");
    let is_macos_binary = lower.ends_with(".dylib");
    let _ = (is_windows_binary, is_linux_binary, is_macos_binary);

    #[cfg(target_os = "windows")]
    {
        !is_linux_binary && !is_macos_binary
    }

    #[cfg(target_os = "linux")]
    {
        !is_windows_binary && !is_macos_binary
    }

    #[cfg(target_os = "macos")]
    {
        !is_windows_binary && !is_linux_binary
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        !is_windows_binary && !is_linux_binary && !is_macos_binary
    }
}

fn compare_expected_installed_files(
    plugin_dir: &Path,
    expected_files: &[Ts3ExpectedInstalledFile],
    matched_files: &mut Vec<PathBuf>,
    missing_files: &mut Vec<String>,
    hash_mismatched_files: &mut Vec<PathBuf>,
) {
    for expected in expected_files {
        let installed_path = expected
            .relative_path
            .split('/')
            .fold(plugin_dir.to_path_buf(), |path, component| {
                path.join(component)
            });
        if !installed_path.is_file() {
            missing_files.push(expected.relative_path.clone());
            continue;
        }

        matched_files.push(installed_path.clone());
        match compute_plugin_hash(&installed_path) {
            Ok(installed_hash) if installed_hash == expected.package_hash => {}
            Ok(_) => hash_mismatched_files.push(installed_path),
            Err(err) => {
                warn!(
                    "Failed to hash installed TeamSpeak 3 plugin payload file: path={} error={}",
                    sanitize_log_path(&installed_path),
                    err
                );
                hash_mismatched_files.push(installed_path);
            }
        }
    }
}

fn teamspeak_plugin_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let app_data = PathBuf::from(app_data);
            dirs.push(app_data.join("TS3Client").join("plugins"));
            dirs.push(app_data.join("TeamSpeak 3 Client").join("plugins"));
        }
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let local_app_data = PathBuf::from(local_app_data);
            dirs.push(local_app_data.join("TS3Client").join("plugins"));
            dirs.push(local_app_data.join("TeamSpeak 3 Client").join("plugins"));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            dirs.push(
                home.join("Library")
                    .join("Application Support")
                    .join("TS3Client")
                    .join("plugins"),
            );
        }
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".ts3client").join("plugins"));
            dirs.push(home.join(".config").join("ts3client").join("plugins"));
        }
    }

    dirs.sort();
    dirs.dedup();
    dirs
}

fn collect_matching_installed_plugins(
    dir: &Path,
    search_name: &str,
    matched_files: &mut Vec<PathBuf>,
    depth: usize,
    max_depth: usize,
) {
    if depth >= max_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        warn!(
            "Failed to read TeamSpeak 3 plugin directory: {}",
            sanitize_log_path(dir)
        );
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_matching_installed_plugins(
                &path,
                search_name,
                matched_files,
                depth + 1,
                max_depth,
            );
            continue;
        }

        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let stem = stem.to_lowercase();
        if stem.contains(search_name) || search_name.contains(&stem) {
            matched_files.push(path);
        }
    }
}

fn format_detected_plugin_list(plugins: &[Ts3PluginInfo]) -> String {
    if plugins.is_empty() {
        return "[]".to_string();
    }

    let items = plugins
        .iter()
        .map(|plugin| {
            format!(
                "{{addon={}, path={}, hash={}}}",
                plugin.addon_name,
                sanitize_log_path(&plugin.plugin_path),
                plugin.file_hash
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{}]", items)
}

fn format_path_list(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "[]".to_string();
    }

    let items = paths
        .iter()
        .map(|path| sanitize_log_path(path))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{}]", items)
}

fn format_string_list(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }

    format!("[{}]", items.join(", "))
}

/// Check whether a TeamSpeak 3 client process is currently running.
#[cfg(target_os = "windows")]
pub fn is_teamspeak_running() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    match Command::new("tasklist")
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
            let matched_processes = [
                "ts3client_win64.exe",
                "ts3client_win32.exe",
                "ts3client.exe",
            ]
            .into_iter()
            .filter(|process_name| stdout.contains(process_name))
            .collect::<Vec<_>>();
            let running = !matched_processes.is_empty();
            debug!(
                "TeamSpeak 3 running check completed: method=tasklist status_success={} matched_processes={:?} running={}",
                output.status.success(),
                matched_processes,
                running
            );
            running
        }
        Err(e) => {
            warn!("Failed to check running processes for TeamSpeak 3: {}", e);
            false
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_teamspeak_running() -> bool {
    use std::process::Command;
    match Command::new("pgrep")
        .args(["-i", "-f", "ts3client|teamspeak"])
        .output()
    {
        Ok(output) => {
            let running = output.status.success();
            debug!(
                "TeamSpeak 3 running check completed: method=pgrep status_success={} running={}",
                output.status.success(),
                running
            );
            running
        }
        Err(e) => {
            warn!("Failed to check running processes for TeamSpeak 3: {}", e);
            false
        }
    }
}

/// Client executable file names, 64-bit preferred over 32-bit.
#[cfg(target_os = "windows")]
const TEAMSPEAK_CLIENT_EXES: &[&str] = &[
    "ts3client_win64.exe",
    "ts3client_win32.exe",
    "ts3client.exe",
];

/// Candidate TeamSpeak 3 install directories to probe on Windows.
#[cfg(target_os = "windows")]
fn teamspeak_install_dir_candidates() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    for var in [
        "ProgramW6432",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "LOCALAPPDATA",
    ] {
        if let Some(value) = std::env::var_os(var) {
            bases.push(PathBuf::from(value).join("TeamSpeak 3 Client"));
        }
    }
    bases.sort();
    bases.dedup();
    bases
}

/// Build the ordered list of candidate TeamSpeak 3 client executable paths to
/// probe on Windows. Earlier entries are preferred (64-bit before 32-bit).
#[cfg(target_os = "windows")]
fn teamspeak_client_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for base in teamspeak_install_dir_candidates() {
        for exe in TEAMSPEAK_CLIENT_EXES {
            candidates.push(base.join(exe));
        }
    }
    candidates
}

/// Find a TeamSpeak 3 client executable inside `dir` (64-bit preferred).
#[cfg(target_os = "windows")]
pub fn teamspeak_client_exe_in(dir: &Path) -> Option<PathBuf> {
    TEAMSPEAK_CLIENT_EXES
        .iter()
        .map(|exe| dir.join(exe))
        .find(|path| path.is_file())
}

/// On unix the configured value may be the binary itself or a directory that
/// contains a `ts3client` binary.
#[cfg(not(target_os = "windows"))]
pub fn teamspeak_client_exe_in(dir: &Path) -> Option<PathBuf> {
    if dir.is_file() {
        return Some(dir.to_path_buf());
    }
    [
        "ts3client",
        "ts3client_runscript.sh",
        "ts3client_linux_amd64",
        "ts3client_linux_x86",
    ]
    .iter()
    .map(|name| dir.join(name))
    .find(|candidate| candidate.is_file())
}

/// Best-effort detection of the TeamSpeak 3 client install directory, used to
/// pre-fill the setting when it is unset (mirrors the Arma 3 directory probe).
#[cfg(target_os = "windows")]
pub fn detect_teamspeak_directory() -> Option<PathBuf> {
    let found = teamspeak_install_dir_candidates()
        .into_iter()
        .find(|dir| teamspeak_client_exe_in(dir).is_some());
    match &found {
        Some(dir) => info!("Detected TeamSpeak 3 directory: {}", sanitize_log_path(dir)),
        None => info!("Could not auto-detect a TeamSpeak 3 install directory"),
    }
    found
}

#[cfg(not(target_os = "windows"))]
pub fn detect_teamspeak_directory() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("TeamSpeak3-Client-linux_amd64"));
        candidates.push(home.join("TeamSpeak3-Client-linux_x86"));
        candidates.push(
            home.join("Applications")
                .join("TeamSpeak3-Client-linux_amd64"),
        );
        candidates.push(home.join(".local/share/TeamSpeak3-Client-linux_amd64"));
    }
    candidates.push(PathBuf::from("/opt/TeamSpeak3-Client-linux_amd64"));
    candidates
        .into_iter()
        .find(|dir| teamspeak_client_exe_in(dir).is_some())
}

/// Launch the installed TeamSpeak 3 client (not connected to any server).
///
/// Prefers the user-configured install directory; when that is unset or invalid
/// it falls back to probing well-known install locations. Returns an `Err` with
/// a user-facing message when the client cannot be found or started.
pub fn launch_teamspeak(configured_dir: &str) -> Result<(), String> {
    let configured_dir = configured_dir.trim();
    info!(
        "Launching TeamSpeak 3 client: configured_dir_set={}",
        !configured_dir.is_empty()
    );

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let exe = (!configured_dir.is_empty())
            .then(|| teamspeak_client_exe_in(Path::new(configured_dir)))
            .flatten()
            .or_else(|| {
                teamspeak_client_candidates()
                    .into_iter()
                    .find(|p| p.is_file())
            });
        let Some(exe) = exe else {
            warn!(
                "Could not locate TeamSpeak 3 client executable (configured_dir_set={})",
                !configured_dir.is_empty()
            );
            return Err(
                "Could not find the TeamSpeak 3 client. Please start it manually.".to_string(),
            );
        };
        info!(
            "Found TeamSpeak 3 client executable: {}",
            sanitize_log_path(&exe)
        );
        // Launch through the shell (`start`) rather than spawning the exe
        // directly. The TeamSpeak client may be manifested to require
        // administrator rights; a direct `CreateProcess` would fail with
        // ERROR_ELEVATION_REQUIRED (os error 740), whereas the shell honors
        // elevation and shows the normal UAC consent prompt.
        let mut command = std::process::Command::new("cmd");
        command
            .args(["/c", "start", "", &exe.display().to_string()])
            .creation_flags(0x08000000); // CREATE_NO_WINDOW
        if let Some(dir) = exe.parent() {
            command.current_dir(dir);
        }
        command
            .spawn()
            .map_err(|e| format!("Failed to launch TeamSpeak 3: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        if !configured_dir.is_empty()
            && let Some(exe) = teamspeak_client_exe_in(Path::new(configured_dir))
        {
            std::process::Command::new(&exe)
                .spawn()
                .map_err(|e| format!("Failed to launch TeamSpeak 3: {}", e))?;
        } else if let Some(dir) = detect_teamspeak_directory()
            && let Some(exe) = teamspeak_client_exe_in(&dir)
        {
            std::process::Command::new(&exe)
                .spawn()
                .map_err(|e| format!("Failed to launch TeamSpeak 3: {}", e))?;
        } else {
            std::process::Command::new("ts3client")
                .spawn()
                .map_err(|e| format!("Failed to launch TeamSpeak 3: {}", e))?;
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = std::process::Command::new("open");
        if configured_dir.is_empty() {
            command.args(["-a", "TeamSpeak 3"]);
        } else {
            command.arg("-a").arg(configured_dir);
        }
        command
            .spawn()
            .map_err(|e| format!("Failed to launch TeamSpeak 3: {}", e))?;
    }

    Ok(())
}

/// Launch the `.ts3_plugin` file with the OS default handler.
/// Opens through the platform default handler, with Linux opener fallbacks.
pub fn open_ts3_plugin(path: &Path) -> Result<(), String> {
    info!("Opening TS3 plugin: {}", sanitize_log_path(path));

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &path.display().to_string()])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| format!("Failed to open TS3 plugin: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        crate::core::utils::platform::open_with_default_app(path)
            .map_err(|e| format!("Failed to open TS3 plugin: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open TS3 plugin: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn scan_finds_ts3_plugin_in_addon_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let addon_dir = tmp.path().join("@task_force_radio").join("teamspeak");
        fs::create_dir_all(&addon_dir).unwrap();
        let plugin_path = addon_dir.join("task_force_radio.ts3_plugin");
        fs::write(&plugin_path, b"fake ts3 plugin content").unwrap();

        let results = scan_repository_for_ts3_plugins(tmp.path().to_str().unwrap());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].addon_name, "@task_force_radio");
        assert_eq!(results[0].plugin_path, plugin_path);
        assert!(!results[0].file_hash.is_empty());
    }

    #[test]
    fn scan_ignores_non_addon_directories() {
        let tmp = TempDir::new().unwrap();
        let non_addon = tmp.path().join("regular_folder");
        fs::create_dir_all(&non_addon).unwrap();
        fs::write(
            non_addon.join("something.ts3_plugin"),
            b"should not be found",
        )
        .unwrap();

        let results = scan_repository_for_ts3_plugins(tmp.path().to_str().unwrap());
        assert!(results.is_empty());
    }

    #[test]
    fn scan_returns_empty_for_missing_path() {
        let results = scan_repository_for_ts3_plugins("/nonexistent/path/12345");
        assert!(results.is_empty());
    }

    #[test]
    fn compute_hash_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.ts3_plugin");
        fs::write(&file, b"deterministic content").unwrap();

        let h1 = compute_plugin_hash(&file).unwrap();
        let h2 = compute_plugin_hash(&file).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    #[test]
    fn scan_all_repositories_deduplicates_by_path() {
        let tmp = TempDir::new().unwrap();
        let addon = tmp.path().join("@radio").join("plugins");
        fs::create_dir_all(&addon).unwrap();
        fs::write(addon.join("radio.ts3_plugin"), b"plugin data").unwrap();

        let repo_path = tmp.path().to_str().unwrap().to_string();
        // Pass same path twice - should dedup
        let results = scan_all_repositories_for_ts3_plugins(&[repo_path.clone(), repo_path]);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn scan_finds_deeply_nested_plugin() {
        let tmp = TempDir::new().unwrap();
        let deep = tmp.path().join("@addon").join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("deep.ts3_plugin"), b"deep plugin").unwrap();

        let results = scan_repository_for_ts3_plugins(tmp.path().to_str().unwrap());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].addon_name, "@addon");
    }

    #[test]
    fn compute_plugin_hash_returns_uppercase_hex() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("upper.ts3_plugin");
        fs::write(&file, b"test").unwrap();

        let hash = compute_plugin_hash(&file).unwrap();
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        // Ensure uppercase
        assert_eq!(hash, hash.to_uppercase());
    }

    #[test]
    fn expected_installed_files_are_read_from_plugins_payload() {
        let tmp = TempDir::new().unwrap();
        let package = tmp.path().join("task_force_radio.ts3_plugin");
        write_test_ts3_package(
            &package,
            &[
                ("package.ini", b"Name = Test".as_slice()),
                ("plugins/TFAR_win64.dll", b"dll content".as_slice()),
                ("plugins/radio-sounds/on.wav", b"sound content".as_slice()),
            ],
        );

        let expected = read_expected_installed_files(&package);

        #[cfg(target_os = "windows")]
        {
            assert_eq!(expected.len(), 2);
            assert_eq!(expected[0].relative_path, "TFAR_win64.dll");
            assert_eq!(expected[1].relative_path, "radio-sounds/on.wav");
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(expected.len(), 1);
            assert_eq!(expected[0].relative_path, "radio-sounds/on.wav");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_expected_payload_uses_so_and_ignores_windows_dll() {
        let tmp = TempDir::new().unwrap();
        let package = tmp.path().join("task_force_radio.ts3_plugin");
        write_test_ts3_package(
            &package,
            &[
                ("plugins/TFAR_win64.dll", b"dll content".as_slice()),
                ("plugins/libtask_force_radio.so", b"so content".as_slice()),
            ],
        );

        let expected = read_expected_installed_files(&package);

        assert_eq!(expected.len(), 1);
        assert_eq!(expected[0].relative_path, "libtask_force_radio.so");
    }

    #[test]
    fn installed_lookup_matches_package_payload_not_package_name() {
        let tmp = TempDir::new().unwrap();
        let package = tmp.path().join("task_force_radio.ts3_plugin");
        let payload = compatible_test_payload_name();
        write_test_ts3_package(&package, &[(payload.as_str(), b"plugin content")]);

        let plugin_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugin_dir).unwrap();
        let installed_name = payload.strip_prefix("plugins/").unwrap_or(payload.as_str());
        fs::write(plugin_dir.join(installed_name), b"plugin content").unwrap();
        let expected = read_expected_installed_files(&package);
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        let mut mismatched = Vec::new();

        compare_expected_installed_files(
            &plugin_dir,
            &expected,
            &mut matched,
            &mut missing,
            &mut mismatched,
        );

        assert_eq!(matched.len(), 1);
        assert!(missing.is_empty());
        assert!(mismatched.is_empty());
    }

    #[test]
    fn installed_lookup_reports_hash_mismatch_for_stale_payload() {
        let tmp = TempDir::new().unwrap();
        let package = tmp.path().join("task_force_radio.ts3_plugin");
        let payload = compatible_test_payload_name();
        write_test_ts3_package(&package, &[(payload.as_str(), b"new plugin")]);

        let plugin_dir = tmp.path().join("plugins");
        fs::create_dir_all(&plugin_dir).unwrap();
        let installed_name = payload.strip_prefix("plugins/").unwrap_or(payload.as_str());
        fs::write(plugin_dir.join(installed_name), b"old plugin").unwrap();
        let expected = read_expected_installed_files(&package);
        let mut matched = Vec::new();
        let mut missing = Vec::new();
        let mut mismatched = Vec::new();

        compare_expected_installed_files(
            &plugin_dir,
            &expected,
            &mut matched,
            &mut missing,
            &mut mismatched,
        );

        assert_eq!(matched.len(), 1);
        assert!(missing.is_empty());
        assert_eq!(mismatched.len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn teamspeak_client_candidates_prefer_64bit_and_include_known_exes() {
        let candidates = teamspeak_client_candidates();
        // The probe order must list the 64-bit client before the 32-bit one so
        // we prefer it when both are installed.
        let win64 = candidates
            .iter()
            .position(|p| p.ends_with("ts3client_win64.exe"));
        let win32 = candidates
            .iter()
            .position(|p| p.ends_with("ts3client_win32.exe"));
        if let (Some(win64), Some(win32)) = (win64, win32) {
            assert!(
                win64 < win32,
                "64-bit client should be probed before 32-bit"
            );
        }
        // Every candidate must live under a TeamSpeak 3 Client folder.
        assert!(candidates.iter().all(|p| p.parent().is_some_and(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name == "TeamSpeak 3 Client")
        })));
    }

    fn write_test_ts3_package(path: &Path, entries: &[(&str, &[u8])]) {
        let file = fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, options).unwrap();
            std::io::Write::write_all(&mut writer, content).unwrap();
        }
        writer.finish().unwrap();
    }

    fn compatible_test_payload_name() -> String {
        #[cfg(target_os = "windows")]
        {
            "plugins/TFAR_win64.dll".to_string()
        }
        #[cfg(target_os = "linux")]
        {
            "plugins/libtask_force_radio.so".to_string()
        }
        #[cfg(target_os = "macos")]
        {
            "plugins/libtask_force_radio.dylib".to_string()
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
        {
            "plugins/task_force_radio.dat".to_string()
        }
    }
}
