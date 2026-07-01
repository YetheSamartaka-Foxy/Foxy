/// Scanning and parsing logic for Swifty configuration data.
use log::{debug, info, warn};
use serde::Deserialize;
use std::path::PathBuf;

use super::types::{DerivedUrls, SwiftyDetectedRepo, SwiftyGlobalSettings};

/// Raw Swifty settings file shape (only the fields we need).
#[derive(Debug, Deserialize)]
struct SwiftySettings {
    #[serde(default)]
    repositories: Vec<SwiftyRepoEntry>,
    /// Global addons/mod paths configured in Swifty (fallback for per-repo paths).
    #[serde(default, alias = "addonsPaths")]
    addons_paths: Vec<String>,
    /// Arma 3 installation directory.
    #[serde(default, alias = "armaPath")]
    arma_path: String,
    /// Temporary download directory.
    #[serde(default, alias = "tempPath")]
    temp_path: String,
}

/// A single entry inside the Swifty `settings.json` `repositories` array.
#[derive(Debug, Deserialize)]
struct SwiftyRepoEntry {
    #[serde(default)]
    name: String,
    #[serde(default, alias = "url")]
    address: String,
    #[serde(default, alias = "modFolder", alias = "mod_folder", alias = "path")]
    mod_folder: String,
    /// Raw launch-parameter string (e.g. `"-skipIntro -noSplash"`).
    #[serde(default)]
    parameters: String,
    /// Whether Swifty auto-checks this repository on launch.
    #[serde(default)]
    autocheck: bool,
}

/// Return candidate directories where Swifty may store its config.
pub fn swifty_candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(&appdata).join("Swifty"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(&local).join("Swifty"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            // XDG config directory
            dirs.push(home.join(".config/Swifty"));
            // XDG data directory
            dirs.push(home.join(".local/share/Swifty"));
            // Direct home directory
            dirs.push(home.join(".swifty"));
            // Wine prefix (default)
            dirs.push(
                home.join(".wine/drive_c/users")
                    .join(
                        std::env::var("USER")
                            .or_else(|_| std::env::var("LOGNAME"))
                            .unwrap_or_else(|_| "user".to_string()),
                    )
                    .join("AppData/Roaming/Swifty"),
            );
        }

        // XDG_CONFIG_HOME override
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            dirs.push(PathBuf::from(xdg_config).join("Swifty"));
        }
    }

    dirs
}

/// Scan all known Swifty directories and return detected repositories and global settings.
pub fn scan_swifty_repositories() -> (
    Vec<SwiftyDetectedRepo>,
    SwiftyGlobalSettings,
    Option<String>,
) {
    let dirs = swifty_candidate_dirs();
    if dirs.is_empty() {
        return (
            Vec::new(),
            SwiftyGlobalSettings::default(),
            Some("No candidate Swifty directories found".into()),
        );
    }

    let mut repos: Vec<SwiftyDetectedRepo> = Vec::new();
    let mut global = SwiftyGlobalSettings::default();
    let mut found_any_dir = false;

    for dir in &dirs {
        let settings_path = dir.join("settings.json");
        if !settings_path.exists() {
            debug!("Swifty settings not found at {}", settings_path.display());
            continue;
        }
        found_any_dir = true;
        info!("Reading Swifty settings from {}", settings_path.display());

        let content = match std::fs::read_to_string(&settings_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read {}: {}", settings_path.display(), e);
                continue;
            }
        };

        let parsed: SwiftySettings = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to parse {}: {}", settings_path.display(), e);
                continue;
            }
        };

        // Capture global settings from the first valid Swifty config.
        if global.arma_path.is_empty() && !parsed.arma_path.trim().is_empty() {
            global.arma_path = parsed.arma_path.trim().to_string();
        }
        if global.temp_path.is_empty() && !parsed.temp_path.trim().is_empty() {
            global.temp_path = parsed.temp_path.trim().to_string();
        }

        // Use the first global addons path as fallback for repos without a per-repo path.
        let global_path = parsed
            .addons_paths
            .iter()
            .find(|p| !p.trim().is_empty())
            .cloned()
            .unwrap_or_default();

        for entry in parsed.repositories {
            if entry.address.trim().is_empty() {
                continue;
            }
            let name = if entry.name.trim().is_empty() {
                repo_name_from_address(&entry.address)
            } else {
                entry.name.clone()
            };
            let mod_folder = if entry.mod_folder.trim().is_empty() {
                global_path.clone()
            } else {
                entry.mod_folder
            };
            repos.push(SwiftyDetectedRepo {
                name,
                address: entry.address,
                mod_folder,
                parameters: entry.parameters,
                autocheck: entry.autocheck,
                selected: true,
            });
        }
    }

    if !found_any_dir {
        return (
            Vec::new(),
            global,
            Some("No Swifty installation found on this system".into()),
        );
    }

    info!("Swifty scan found {} repositories", repos.len());
    (repos, global, None)
}

/// Check whether any Swifty configuration exists on disk (cheap check).
pub fn swifty_data_exists() -> bool {
    swifty_candidate_dirs()
        .iter()
        .any(|d| d.join("settings.json").exists())
}

/// Derive base, updater, and repository-space URLs from a Swifty repository address.
///
/// For `http://a3.tfrod.cz:8080/mody/TFR_Main`:
///   base     = `http://a3.tfrod.cz:8080/mody/`
///   updater  = `http://a3.tfrod.cz:8080/mody/Foxy`
///   space    = `http://a3.tfrod.cz:8080/mody/repository_space.json`
pub fn derive_urls(address: &str) -> Option<DerivedUrls> {
    let trimmed = address.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    // Find the last `/` that separates the parent path from the repo-name segment.
    let scheme_end = trimmed.find("://").map(|i| i + 3).unwrap_or(0);
    let last_slash = trimmed[scheme_end..].rfind('/').map(|i| i + scheme_end);

    let base_url = match last_slash {
        Some(pos) => format!("{}/", &trimmed[..pos]),
        None => {
            // No path at all (e.g. `http://server:8080`) - use address as-is with trailing slash.
            format!("{}/", trimmed)
        }
    };

    Some(DerivedUrls {
        updater_url: format!("{}Foxy", base_url),
        space_url: format!("{}repository_space.json", base_url),
        base_url,
    })
}

/// Parse a Swifty launch-parameter string and apply recognised flags to a
/// [`Repository`].  Unknown tokens are collected into `additional_params`.
pub fn apply_swifty_parameters(repo: &mut crate::ui::types::Repository, params: &str) {
    use crate::ui::types::split_additional_launch_params;

    let mut additional: Vec<String> = Vec::new();
    for arg in split_additional_launch_params(params) {
        let lower: String = arg.to_ascii_lowercase();
        match lower.as_str() {
            "-skipintro" => repo.skip_intro = true,
            "-nosplash" => repo.no_splash = true,
            "-world=empty" | "-noland" => repo.world_empty = true,
            "-loadmissiontomemory" => repo.load_mission_to_memory = true,
            "-enableht" => repo.enable_ht = true,
            "-hugepages" => repo.huge_pages = true,
            "-nologs" => repo.no_logs = true,
            _ if lower.starts_with("-mod=") => {
                // `-mod=gm` or `-mod=gm;spe`
                let codes: &str = lower.trim_start_matches("-mod=");
                for code in codes.split(';') {
                    match code.trim() {
                        "csla" => repo.csla = true,
                        "ef" => repo.ef = true,
                        "gm" => repo.gm = true,
                        "rf" => repo.rf = true,
                        "spe" => repo.spe = true,
                        "vn" => repo.vn = true,
                        "ws" => repo.ws = true,
                        _ => {
                            // Non-DLC mod flag - keep as additional param.
                            additional.push(arg.clone());
                            break;
                        }
                    }
                }
            }
            _ => additional.push(arg),
        }
    }

    if !additional.is_empty() {
        repo.additional_params = additional.join(" ");
    }
}

/// Extract a human-readable name from the last path segment of a URL.
fn repo_name_from_address(address: &str) -> String {
    let trimmed = address.trim().trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_urls_standard_path() {
        let urls = derive_urls("http://a3.tfrod.cz:8080/mody/TFR_Main").unwrap();
        assert_eq!(urls.base_url, "http://a3.tfrod.cz:8080/mody/");
        assert_eq!(urls.updater_url, "http://a3.tfrod.cz:8080/mody/Foxy");
        assert_eq!(
            urls.space_url,
            "http://a3.tfrod.cz:8080/mody/repository_space.json"
        );
    }

    #[test]
    fn derive_urls_trailing_slash() {
        let urls = derive_urls("http://server.com/repos/MyRepo/").unwrap();
        assert_eq!(urls.base_url, "http://server.com/repos/");
        assert_eq!(urls.updater_url, "http://server.com/repos/Foxy");
        assert_eq!(
            urls.space_url,
            "http://server.com/repos/repository_space.json"
        );
    }

    #[test]
    fn derive_urls_no_path() {
        let urls = derive_urls("http://server.com").unwrap();
        assert_eq!(urls.base_url, "http://server.com/");
        assert_eq!(urls.updater_url, "http://server.com/Foxy");
        assert_eq!(urls.space_url, "http://server.com/repository_space.json");
    }

    #[test]
    fn derive_urls_empty_returns_none() {
        assert!(derive_urls("").is_none());
        assert!(derive_urls("  ").is_none());
    }

    #[test]
    fn derive_urls_deep_path() {
        let urls = derive_urls("http://host:9000/a/b/c/Repo").unwrap();
        assert_eq!(urls.base_url, "http://host:9000/a/b/c/");
        assert_eq!(urls.updater_url, "http://host:9000/a/b/c/Foxy");
        assert_eq!(
            urls.space_url,
            "http://host:9000/a/b/c/repository_space.json"
        );
    }

    #[test]
    fn apply_swifty_parameters_maps_known_flags() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(
            &mut repo,
            "-skipIntro -noSplash -world=empty -LoadMissionToMemory -enableHT -hugePages -noLogs",
        );
        assert!(repo.skip_intro);
        assert!(repo.no_splash);
        assert!(repo.world_empty);
        assert!(repo.load_mission_to_memory);
        assert!(repo.enable_ht);
        assert!(repo.huge_pages);
        assert!(repo.no_logs);
        assert!(repo.additional_params.is_empty());
    }

    #[test]
    fn apply_swifty_parameters_maps_dlc_mod_flags() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(&mut repo, "-skipIntro -mod=gm");
        assert!(repo.skip_intro);
        assert!(repo.gm);
        assert!(!repo.csla);
        assert!(repo.additional_params.is_empty());
    }

    #[test]
    fn apply_swifty_parameters_handles_semicolon_dlcs() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(&mut repo, "-mod=gm;spe;ws");
        assert!(repo.gm);
        assert!(repo.spe);
        assert!(repo.ws);
        assert!(!repo.vn);
    }

    #[test]
    fn apply_swifty_parameters_preserves_unknown_tokens() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(&mut repo, "-skipIntro -exThreads=7 -noSplash -window");
        assert!(repo.skip_intro);
        assert!(repo.no_splash);
        assert_eq!(repo.additional_params, "-exThreads=7 -window");
    }

    #[test]
    fn apply_swifty_parameters_maps_no_land_to_world_empty() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(&mut repo, "-skipIntro -noLand -window");
        assert!(repo.skip_intro);
        assert!(repo.world_empty);
        assert_eq!(repo.additional_params, "-window");
    }

    #[test]
    fn apply_swifty_parameters_handles_quoted_paths() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(&mut repo, r#"-skipIntro "C:\Users\test\mission.sqm""#);
        assert!(repo.skip_intro);
        assert_eq!(repo.additional_params, r"C:\Users\test\mission.sqm");
    }

    #[test]
    fn repo_name_from_address_works() {
        assert_eq!(
            repo_name_from_address("http://x.com/mods/TFR_Main"),
            "TFR_Main"
        );
        assert_eq!(
            repo_name_from_address("http://x.com/mods/TFR_Main/"),
            "TFR_Main"
        );
        assert_eq!(repo_name_from_address("http://x.com"), "x.com");
    }

    #[test]
    fn derive_urls_https_scheme() {
        let urls = derive_urls("https://secure.example.com/repos/Main").unwrap();
        assert!(urls.base_url.starts_with("https://"));
        assert_eq!(urls.base_url, "https://secure.example.com/repos/");
    }

    #[test]
    fn derive_urls_port_only_no_path() {
        let urls = derive_urls("http://192.168.1.1:8080").unwrap();
        assert_eq!(urls.base_url, "http://192.168.1.1:8080/");
        assert_eq!(urls.updater_url, "http://192.168.1.1:8080/Foxy");
    }

    #[test]
    fn apply_swifty_parameters_all_dlcs() {
        let mut repo = crate::ui::types::Repository::default();
        apply_swifty_parameters(&mut repo, "-mod=csla;ef;gm;rf;spe;vn;ws");
        assert!(repo.csla);
        assert!(repo.ef);
        assert!(repo.gm);
        assert!(repo.rf);
        assert!(repo.spe);
        assert!(repo.vn);
        assert!(repo.ws);
    }
}
