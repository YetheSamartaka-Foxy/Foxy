use log::{debug, info, warn};
use std::fs;
use std::path::{Path, PathBuf};

/// An Arma 3 player profile detected on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arma3Profile {
    /// Human-readable display name (percent-decoded from folder name).
    pub name: String,
    /// Full path to the profile directory.
    pub path: PathBuf,
    /// Whether this is the default profile (in Documents\Arma 3\)
    /// vs an "Other Profile" (in Documents\Arma 3 - Other Profiles\).
    pub is_default: bool,
}

/// Percent-decode a folder name (e.g., "John%20Doe" -> "John Doe").
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &input[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                result.push(byte as char);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Detect all Arma 3 profiles on the system.
///
/// Scans:
/// - `Documents\Arma 3\` for the default profile
/// - `Documents\Arma 3 - Other Profiles\*\` for additional profiles
///
/// A directory is considered a valid profile if it contains at least one
/// `*.Arma3Profile` file.
pub fn detect_all_profiles(custom_profiles_dir: Option<&Path>) -> Vec<Arma3Profile> {
    let mut profiles = Vec::new();

    let Some(documents) = dirs::document_dir() else {
        warn!("Could not determine Documents directory for Arma 3 profile detection");
        if let Some(custom_profiles_dir) = custom_profiles_dir {
            detect_custom_profiles(custom_profiles_dir, &mut profiles);
        }
        detect_proton_profiles(&mut profiles);
        info!("Detected {} Arma 3 profile(s)", profiles.len());
        return profiles;
    };

    // 1. Default profile
    let default_dir = documents.join("Arma 3");
    if default_dir.is_dir()
        && let Some(profile_name) = find_arma3_profile_file(&default_dir)
    {
        debug!("Found default Arma 3 profile: {}", profile_name);
        profiles.push(Arma3Profile {
            name: profile_name,
            path: default_dir,
            is_default: true,
        });
    }

    // 2. Other Profiles
    let other_profiles_dir = documents.join("Arma 3 - Other Profiles");
    if other_profiles_dir.is_dir() {
        match fs::read_dir(&other_profiles_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    if !entry_path.is_dir() {
                        continue;
                    }

                    let folder_name = entry.file_name().to_string_lossy().to_string();
                    let display_name = percent_decode(&folder_name);

                    if find_arma3_profile_file(&entry_path).is_some() {
                        debug!(
                            "Found other Arma 3 profile: {} (folder: {})",
                            display_name, folder_name
                        );
                        profiles.push(Arma3Profile {
                            name: display_name,
                            path: entry_path,
                            is_default: false,
                        });
                    }
                }
            }
            Err(e) => {
                warn!("Failed to read Arma 3 Other Profiles directory: {}", e);
            }
        }
    }

    if let Some(custom_profiles_dir) = custom_profiles_dir {
        detect_custom_profiles(custom_profiles_dir, &mut profiles);
    }

    detect_proton_profiles(&mut profiles);

    info!("Detected {} Arma 3 profile(s)", profiles.len());
    profiles
}

#[cfg(target_os = "linux")]
fn detect_proton_profiles(profiles: &mut Vec<Arma3Profile>) {
    for library_root in crate::core::steam::steam_library_roots("") {
        let users_root = library_root
            .join("steamapps")
            .join("compatdata")
            .join("107410")
            .join("pfx")
            .join("drive_c")
            .join("users");
        let Ok(users) = fs::read_dir(&users_root) else {
            continue;
        };
        for user in users.flatten() {
            let documents = user.path().join("Documents");
            detect_profiles_in_documents(&documents, profiles);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn detect_proton_profiles(_profiles: &mut Vec<Arma3Profile>) {}

#[cfg(target_os = "linux")]
fn detect_profiles_in_documents(documents: &Path, profiles: &mut Vec<Arma3Profile>) {
    let default_dir = documents.join("Arma 3");
    if default_dir.is_dir()
        && let Some(profile_name) = find_arma3_profile_file(&default_dir)
    {
        push_profile_if_new(profiles, profile_name, default_dir, true);
    }

    let other_profiles_dir = documents.join("Arma 3 - Other Profiles");
    if other_profiles_dir.is_dir() {
        detect_profiles_in_root(&other_profiles_dir, false, profiles);
    }
}

fn detect_custom_profiles(custom_profiles_dir: &Path, profiles: &mut Vec<Arma3Profile>) {
    if !custom_profiles_dir.is_dir() {
        return;
    }

    detect_profiles_in_root(custom_profiles_dir, false, profiles);

    let users_dir = custom_profiles_dir.join("Users");
    if users_dir.is_dir() {
        detect_profiles_in_root(&users_dir, false, profiles);
    }
}

fn detect_profiles_in_root(root: &Path, is_default: bool, profiles: &mut Vec<Arma3Profile>) {
    if let Some(profile_name) = find_arma3_profile_file(root) {
        push_profile_if_new(profiles, profile_name, root.to_path_buf(), is_default);
    }

    let Ok(entries) = fs::read_dir(root) else {
        warn!(
            "Failed to read Arma 3 profiles directory: {}",
            root.display()
        );
        return;
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let folder_name = entry.file_name().to_string_lossy().to_string();
        let display_name = percent_decode(&folder_name);
        if find_arma3_profile_file(&entry_path).is_some() {
            debug!(
                "Found Arma 3 profile in custom root: {} ({})",
                display_name,
                entry_path.display()
            );
            push_profile_if_new(profiles, display_name, entry_path, is_default);
        }
    }
}

fn push_profile_if_new(
    profiles: &mut Vec<Arma3Profile>,
    name: String,
    path: PathBuf,
    is_default: bool,
) {
    if profiles
        .iter()
        .any(|profile| profile.name == name && profile.path == path)
    {
        return;
    }

    profiles.push(Arma3Profile {
        name,
        path,
        is_default,
    });
}

/// Find the first `*.Arma3Profile` file in a directory and return
/// its stem (filename without extension) as the profile name.
fn find_arma3_profile_file(dir: &Path) -> Option<String> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && let Some(ext) = path.extension()
            && ext.eq_ignore_ascii_case("Arma3Profile")
        {
            // Skip .vars.Arma3Profile and .3den.Arma3Profile
            let stem = path.file_stem()?.to_string_lossy().to_string();
            if stem.ends_with(".vars") || stem.ends_with(".3den") {
                continue;
            }
            return Some(stem);
        }
    }
    None
}

/// Attempt to detect which Arma 3 profile is currently selected.
///
/// Detection strategy (first match wins):
/// 1. Most recently modified `.Arma3Profile` file across all known profiles.
///    The game writes to this file on every session, so the newest one is
///    the profile last used.
/// 2. Arma 3 Launcher JSON config files in `%LOCALAPPDATA%\Arma 3 Launcher\`.
/// 3. Fallback to the default profile (from `Documents\Arma 3\`).
pub fn detect_active_profile(known_profiles: &[Arma3Profile]) -> Option<String> {
    // 1. Most recently modified .Arma3Profile file
    if let Some(name) = detect_most_recently_used_profile(known_profiles) {
        info!(
            "Detected active Arma 3 profile from most recently modified profile file: {}",
            name
        );
        return Some(name);
    }

    // 2. Launcher JSON configs
    if let Some(local_app_data) = dirs::data_local_dir() {
        let launcher_dir = local_app_data.join("Arma 3 Launcher");
        if launcher_dir.is_dir() {
            for filename in &["Local.json", "Parameters.json", "Startup.json"] {
                let config_path = launcher_dir.join(filename);
                if let Ok(content) = fs::read_to_string(&config_path)
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                    && let Some(name) = extract_profile_from_json(&json)
                    && known_profiles
                        .iter()
                        .any(|p| p.name.eq_ignore_ascii_case(&name))
                {
                    info!(
                        "Detected active Arma 3 profile from launcher config: {}",
                        name
                    );
                    return Some(name);
                }
            }
        } else {
            debug!("Arma 3 Launcher config directory not found");
        }
    }

    // 3. Default profile fallback
    fallback_default(known_profiles)
}

/// Find the profile whose `.Arma3Profile` file was most recently modified.
///
/// Arma 3 writes to `<ProfileName>.Arma3Profile` on every game session,
/// so the newest modification timestamp indicates the last-used profile.
fn detect_most_recently_used_profile(known_profiles: &[Arma3Profile]) -> Option<String> {
    let mut best: Option<(String, std::time::SystemTime)> = None;

    for profile in known_profiles {
        if let Some(mtime) = newest_arma3profile_mtime(&profile.path) {
            let dominated = best.as_ref().is_some_and(|(_, t)| mtime > *t);
            if best.is_none() || dominated {
                best = Some((profile.name.clone(), mtime));
            }
        }
    }

    best.map(|(name, _)| name)
}

/// Return the modification time of the primary `*.Arma3Profile` file in a
/// directory, skipping `.vars.Arma3Profile` and `.3den.Arma3Profile`.
fn newest_arma3profile_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && let Some(ext) = path.extension()
            && ext.eq_ignore_ascii_case("Arma3Profile")
        {
            let stem = path.file_stem()?.to_string_lossy().to_string();
            if stem.ends_with(".vars") || stem.ends_with(".3den") {
                continue;
            }
            return fs::metadata(&path).ok()?.modified().ok();
        }
    }
    None
}

/// Search a JSON value for profile/name fields.
fn extract_profile_from_json(json: &serde_json::Value) -> Option<String> {
    for key in &["profile", "name", "profileName", "playerName"] {
        if let Some(val) = json.get(key).and_then(|v| v.as_str())
            && !val.is_empty()
        {
            return Some(val.to_string());
        }
    }

    for key in &[
        "parameters",
        "launchParams",
        "extra",
        "additionalParameters",
    ] {
        if let Some(params_str) = json.get(key).and_then(|v| v.as_str())
            && let Some(name) = parse_name_from_params(params_str)
        {
            return Some(name);
        }
    }

    if let Some(obj) = json.as_object() {
        for value in obj.values() {
            if value.is_object()
                && let Some(name) = extract_profile_from_json(value)
            {
                return Some(name);
            }
        }
    }

    None
}

/// Parse `-name=ProfileName` from a launch parameters string.
/// Handles both `-name=Profile` and `-name="Profile Name"` formats.
fn parse_name_from_params(params: &str) -> Option<String> {
    let prefix = "-name=";
    let start = params.find(prefix)?;
    let rest = &params[start + prefix.len()..];

    if let Some(inner) = rest.strip_prefix('"') {
        // Quoted value: find the closing quote
        let end = inner.find('"').unwrap_or(inner.len());
        let name = &inner[..end];
        if !name.is_empty() {
            return Some(name.to_string());
        }
    } else if let Some(inner) = rest.strip_prefix('\'') {
        let end = inner.find('\'').unwrap_or(inner.len());
        let name = &inner[..end];
        if !name.is_empty() {
            return Some(name.to_string());
        }
    } else {
        // Unquoted: take until the next whitespace
        let name: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
        if !name.is_empty() {
            return Some(name);
        }
    }

    None
}

fn fallback_default(known_profiles: &[Arma3Profile]) -> Option<String> {
    known_profiles
        .iter()
        .find(|p| p.is_default)
        .map(|p| p.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_simple_space() {
        assert_eq!(percent_decode("John%20Doe"), "John Doe");
    }

    #[test]
    fn percent_decode_brackets() {
        assert_eq!(percent_decode("Player%20%5BTAG%5D"), "Player [TAG]");
    }

    #[test]
    fn percent_decode_no_encoding() {
        assert_eq!(percent_decode("SimpleName"), "SimpleName");
    }

    #[test]
    fn percent_decode_invalid_hex() {
        assert_eq!(percent_decode("Test%ZZValue"), "Test%ZZValue");
    }

    #[test]
    fn percent_decode_trailing_percent() {
        assert_eq!(percent_decode("Trail%"), "Trail%");
    }

    #[test]
    fn parse_name_from_params_basic() {
        assert_eq!(
            parse_name_from_params("-skipIntro -name=MyProfile -noSplash"),
            Some("MyProfile".to_string())
        );
    }

    #[test]
    fn parse_name_from_params_quoted() {
        assert_eq!(
            parse_name_from_params("-name=\"My Profile\""),
            Some("My Profile".to_string())
        );
    }

    #[test]
    fn detect_all_profiles_includes_custom_users_root() {
        let dir = tempfile::tempdir().unwrap();
        let profile_dir = dir.path().join("Users").join("Jane%20Doe");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(profile_dir.join("Jane Doe.Arma3Profile"), "").unwrap();

        let profiles = detect_all_profiles(Some(dir.path()));

        assert!(profiles.iter().any(|profile| {
            profile.name == "Jane Doe" && profile.path == profile_dir && !profile.is_default
        }));
    }

    #[test]
    fn parse_name_from_params_missing() {
        assert_eq!(parse_name_from_params("-skipIntro -noSplash"), None);
    }
}
