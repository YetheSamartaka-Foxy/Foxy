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

/// Percent-encode a profile name into its on-disk folder name, mirroring
/// [`percent_decode`]. Arma 3 keeps ASCII letters, digits, `-` and `_`
/// literal and encodes everything else (e.g., "John Doe" -> "John%20Doe",
/// "Player [TAG]" -> "Player%20%5BTAG%5D").
pub fn percent_encode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{:02X}", byte));
        }
    }
    result
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

/// Why a proposed profile name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileNameError {
    Empty,
    TooLong,
    UnsupportedCharacters,
    Reserved,
}

/// Why a profile management operation (rename/clone/delete) failed.
#[derive(Debug)]
pub enum ProfileOpError {
    InvalidName(ProfileNameError),
    TargetAlreadyExists,
    SourceMissing,
    DefaultProfileProtected,
    UnsafePath,
    Io(std::io::Error),
}

impl std::fmt::Display for ProfileOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName(reason) => write!(f, "invalid profile name ({:?})", reason),
            Self::TargetAlreadyExists => write!(f, "a profile with that name already exists"),
            Self::SourceMissing => write!(f, "profile files were not found on disk"),
            Self::DefaultProfileProtected => write!(f, "the default profile cannot be deleted"),
            Self::UnsafePath => write!(f, "the profile path cannot be modified safely"),
            Self::Io(err) => write!(f, "io error: {}", err),
        }
    }
}

/// Validate a proposed new profile name. The allowed character set is kept
/// conservative on purpose: it only contains characters whose on-disk
/// encoding by Arma 3 is known, so `-name=<name>` resolves to the folder
/// Foxy creates instead of silently spawning a fresh empty profile.
pub fn validate_new_profile_name(name: &str) -> Result<(), ProfileNameError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ProfileNameError::Empty);
    }
    if name.chars().count() > 64 {
        return Err(ProfileNameError::TooLong);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '[' | ']'))
    {
        return Err(ProfileNameError::UnsupportedCharacters);
    }
    let upper = name.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(ProfileNameError::Reserved);
    }
    Ok(())
}

/// Normalized comparison key for directory paths (separator- and, on
/// Windows, case-insensitive).
fn dir_key(path: &Path) -> String {
    let mut key = path.to_string_lossy().replace('\\', "/");
    while key.ends_with('/') {
        key.pop();
    }
    if cfg!(windows) {
        key.make_ascii_lowercase();
    }
    key
}

/// Whether two paths refer to the same directory after normalization.
pub fn paths_refer_to_same_dir(a: &Path, b: &Path) -> bool {
    dir_key(a) == dir_key(b)
}

/// Whether `child` is `root` or lives underneath it, using the same
/// normalization as [`paths_refer_to_same_dir`].
pub fn path_is_under(child: &Path, root: &Path) -> bool {
    let child_key = dir_key(child);
    let root_key = dir_key(root);
    child_key == root_key || child_key.starts_with(&format!("{}/", root_key))
}

/// Whether `path` is one of the vanilla Arma 3 profile locations in
/// Documents. Passing such a path as `-profiles` makes the game relocate
/// player profiles into a `Users` subfolder and start with fresh settings
/// and keybinds, so callers must never forward these as `-profiles`.
pub fn is_vanilla_profiles_location(path: &Path) -> bool {
    let Some(documents) = dirs::document_dir() else {
        return false;
    };
    paths_refer_to_same_dir(path, &documents.join("Arma 3"))
        || paths_refer_to_same_dir(path, &documents.join("Arma 3 - Other Profiles"))
}

/// The vanilla root for named ("Other") profiles, used as the clone target
/// when the source is the default profile.
pub fn other_profiles_root() -> Option<PathBuf> {
    dirs::document_dir().map(|documents| documents.join("Arma 3 - Other Profiles"))
}

/// Collect the profile file renames/copies for a profile directory: every
/// file named `<stem>.<...>Arma3Profile<...>` is mapped to the same name
/// with `new_name` as the stem (covers `.Arma3Profile`,
/// `.vars.Arma3Profile`, `.3den.Arma3Profile` and backup variants).
fn profile_file_transfers(
    dir: &Path,
    source_stem: &str,
    new_name: &str,
) -> Result<Vec<(PathBuf, String)>, ProfileOpError> {
    let prefix = format!("{}.", source_stem);
    let mut transfers = Vec::new();
    let entries = fs::read_dir(dir).map_err(ProfileOpError::Io)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with(&prefix) {
            continue;
        }
        let remainder = &file_name[source_stem.len()..];
        if !remainder.to_ascii_lowercase().contains("arma3profile") {
            continue;
        }
        transfers.push((path, format!("{}{}", new_name, remainder)));
    }
    Ok(transfers)
}

/// Rename an Arma 3 profile on disk: profile files get the new stem and,
/// for profiles living in their own folder, the folder is renamed to the
/// percent-encoded new name. Default-profile and profiles-root locations
/// are renamed in place (files only). Returns the profile directory after
/// the rename.
pub fn rename_profile(
    profile: &Arma3Profile,
    new_name: &str,
    protected_roots: &[PathBuf],
) -> Result<PathBuf, ProfileOpError> {
    let new_name = new_name.trim();
    validate_new_profile_name(new_name).map_err(ProfileOpError::InvalidName)?;
    if !profile.path.is_dir() {
        return Err(ProfileOpError::SourceMissing);
    }
    let source_stem =
        find_arma3_profile_file(&profile.path).ok_or(ProfileOpError::SourceMissing)?;
    if new_name == source_stem {
        return Err(ProfileOpError::TargetAlreadyExists);
    }

    let rename_in_place = profile.is_default
        || protected_roots
            .iter()
            .any(|root| paths_refer_to_same_dir(root, &profile.path));

    let transfers = profile_file_transfers(&profile.path, &source_stem, new_name)?;
    if transfers.is_empty() {
        return Err(ProfileOpError::SourceMissing);
    }
    for (_, target_name) in &transfers {
        if profile.path.join(target_name).exists() {
            return Err(ProfileOpError::TargetAlreadyExists);
        }
    }

    let target_dir = if rename_in_place {
        None
    } else {
        let parent = profile.path.parent().ok_or(ProfileOpError::UnsafePath)?;
        let target_dir = parent.join(percent_encode(new_name));
        if target_dir.exists() {
            return Err(ProfileOpError::TargetAlreadyExists);
        }
        Some(target_dir)
    };

    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (source, target_name) in &transfers {
        let target = profile.path.join(target_name);
        if let Err(err) = fs::rename(source, &target) {
            for (orig, moved) in renamed.iter().rev() {
                let _ = fs::rename(moved, orig);
            }
            return Err(ProfileOpError::Io(err));
        }
        renamed.push((source.clone(), target));
    }

    let Some(target_dir) = target_dir else {
        info!(
            "Renamed Arma 3 profile files in place: {} -> {}",
            source_stem, new_name
        );
        return Ok(profile.path.clone());
    };
    match fs::rename(&profile.path, &target_dir) {
        Ok(()) => {
            info!("Renamed Arma 3 profile: {} -> {}", source_stem, new_name);
            Ok(target_dir)
        }
        Err(err) => {
            for (orig, moved) in renamed.iter().rev() {
                let _ = fs::rename(moved, orig);
            }
            Err(ProfileOpError::Io(err))
        }
    }
}

/// Clone an Arma 3 profile: copies only the profile files (settings,
/// keybinds, editor preferences) into a new percent-encoded folder. The
/// default profile is cloned into `fallback_named_root` (normally
/// `Documents\Arma 3 - Other Profiles`); named profiles are cloned next to
/// the source folder. Returns the new profile directory.
pub fn clone_profile(
    profile: &Arma3Profile,
    new_name: &str,
    fallback_named_root: &Path,
    protected_roots: &[PathBuf],
) -> Result<PathBuf, ProfileOpError> {
    let new_name = new_name.trim();
    validate_new_profile_name(new_name).map_err(ProfileOpError::InvalidName)?;
    if !profile.path.is_dir() {
        return Err(ProfileOpError::SourceMissing);
    }
    let source_stem =
        find_arma3_profile_file(&profile.path).ok_or(ProfileOpError::SourceMissing)?;

    let profile_is_root = protected_roots
        .iter()
        .any(|root| paths_refer_to_same_dir(root, &profile.path));
    let dest_root = if profile.is_default {
        fallback_named_root.to_path_buf()
    } else if profile_is_root {
        profile.path.join("Users")
    } else {
        profile
            .path
            .parent()
            .ok_or(ProfileOpError::UnsafePath)?
            .to_path_buf()
    };
    let dest_dir = dest_root.join(percent_encode(new_name));
    if dest_dir.exists() {
        return Err(ProfileOpError::TargetAlreadyExists);
    }

    let transfers = profile_file_transfers(&profile.path, &source_stem, new_name)?;
    if transfers.is_empty() {
        return Err(ProfileOpError::SourceMissing);
    }

    fs::create_dir_all(&dest_dir).map_err(ProfileOpError::Io)?;
    for (source, target_name) in &transfers {
        if let Err(err) = fs::copy(source, dest_dir.join(target_name)) {
            let _ = fs::remove_dir_all(&dest_dir);
            return Err(ProfileOpError::Io(err));
        }
    }
    info!("Cloned Arma 3 profile: {} -> {}", source_stem, new_name);
    Ok(dest_dir)
}

/// Delete an Arma 3 profile by moving its folder into `trash_root` instead
/// of removing it outright, so an accidental delete stays recoverable. The
/// default profile and profile-root directories are refused. Returns the
/// path the profile folder was moved to.
pub fn delete_profile(
    profile: &Arma3Profile,
    trash_root: &Path,
    protected_roots: &[PathBuf],
) -> Result<PathBuf, ProfileOpError> {
    if profile.is_default {
        return Err(ProfileOpError::DefaultProfileProtected);
    }
    if protected_roots
        .iter()
        .any(|root| paths_refer_to_same_dir(root, &profile.path))
    {
        return Err(ProfileOpError::UnsafePath);
    }
    if !profile.path.is_dir() {
        return Err(ProfileOpError::SourceMissing);
    }
    if find_arma3_profile_file(&profile.path).is_none() {
        return Err(ProfileOpError::UnsafePath);
    }
    // Require at least two ancestors so shallow paths like a drive root or
    // a direct drive child can never be deleted through this code path.
    if profile
        .path
        .parent()
        .and_then(|parent| parent.parent())
        .is_none()
    {
        return Err(ProfileOpError::UnsafePath);
    }

    fs::create_dir_all(trash_root).map_err(ProfileOpError::Io)?;
    let folder_name = profile
        .path
        .file_name()
        .ok_or(ProfileOpError::UnsafePath)?
        .to_string_lossy()
        .to_string();
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let mut dest = trash_root.join(format!("{}_{}", timestamp, folder_name));
    let mut counter = 1;
    while dest.exists() {
        dest = trash_root.join(format!("{}_{}_{}", timestamp, counter, folder_name));
        counter += 1;
    }

    if fs::rename(&profile.path, &dest).is_err() {
        // Cross-device move: fall back to copy + remove.
        copy_dir_recursive(&profile.path, &dest).map_err(ProfileOpError::Io)?;
        fs::remove_dir_all(&profile.path).map_err(ProfileOpError::Io)?;
    }
    info!(
        "Deleted Arma 3 profile {} (moved to backup location)",
        profile.name
    );
    Ok(dest)
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Whether an Arma 3 game process is currently running. Profile files are
/// held and rewritten by the game, so management operations are refused
/// while it runs.
pub fn is_arma3_running() -> bool {
    #[cfg(target_os = "windows")]
    {
        for image in ["arma3_x64.exe", "arma3.exe"] {
            let output = std::process::Command::new("tasklist")
                .args([
                    "/FI",
                    &format!("IMAGENAME eq {}", image),
                    "/FO",
                    "CSV",
                    "/NH",
                ])
                .output();
            if let Ok(out) = output
                && out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .to_lowercase()
                    .contains(image)
            {
                return true;
            }
        }
        false
    }
    #[cfg(not(target_os = "windows"))]
    {
        let system = sysinfo::System::new_all();
        system.processes().values().any(|process| {
            process
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("arma3")
        })
    }
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

    #[test]
    fn percent_encode_round_trips_supported_names() {
        for name in ["SimpleName", "John Doe", "Player [TAG]", "a-b_c 1"] {
            assert_eq!(percent_decode(&percent_encode(name)), name);
        }
        assert_eq!(percent_encode("John Doe"), "John%20Doe");
        assert_eq!(percent_encode("Player [TAG]"), "Player%20%5BTAG%5D");
    }

    #[test]
    fn validate_new_profile_name_rules() {
        assert_eq!(validate_new_profile_name("CoreX"), Ok(()));
        assert_eq!(validate_new_profile_name("John Doe [TAG]"), Ok(()));
        assert_eq!(
            validate_new_profile_name("   "),
            Err(ProfileNameError::Empty)
        );
        assert_eq!(
            validate_new_profile_name("a.b"),
            Err(ProfileNameError::UnsupportedCharacters)
        );
        assert_eq!(
            validate_new_profile_name("we/ird"),
            Err(ProfileNameError::UnsupportedCharacters)
        );
        assert_eq!(
            validate_new_profile_name("com1"),
            Err(ProfileNameError::Reserved)
        );
        let long_name = "x".repeat(65);
        assert_eq!(
            validate_new_profile_name(&long_name),
            Err(ProfileNameError::TooLong)
        );
    }

    fn make_profile_dir(root: &Path, folder: &str, stem: &str) -> PathBuf {
        let dir = root.join(folder);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{stem}.Arma3Profile")), "keybinds").unwrap();
        fs::write(dir.join(format!("{stem}.vars.Arma3Profile")), "vars").unwrap();
        fs::write(dir.join(format!("{stem}.3den.Arma3Profile")), "eden").unwrap();
        fs::write(dir.join("unrelated.txt"), "keep").unwrap();
        dir
    }

    #[test]
    fn rename_profile_renames_files_and_folder() {
        let root = tempfile::tempdir().unwrap();
        let dir = make_profile_dir(root.path(), "Old%20Name", "Old Name");
        let profile = Arma3Profile {
            name: "Old Name".to_string(),
            path: dir,
            is_default: false,
        };

        let new_dir = rename_profile(&profile, "New Name", &[]).unwrap();

        assert_eq!(new_dir, root.path().join("New%20Name"));
        assert!(new_dir.join("New Name.Arma3Profile").is_file());
        assert!(new_dir.join("New Name.vars.Arma3Profile").is_file());
        assert!(new_dir.join("New Name.3den.Arma3Profile").is_file());
        assert!(new_dir.join("unrelated.txt").is_file());
        assert!(!root.path().join("Old%20Name").exists());
    }

    #[test]
    fn rename_profile_default_renames_files_in_place() {
        let root = tempfile::tempdir().unwrap();
        let dir = make_profile_dir(root.path(), "Arma 3", "CoreX");
        let profile = Arma3Profile {
            name: "CoreX".to_string(),
            path: dir.clone(),
            is_default: true,
        };

        let result_dir = rename_profile(&profile, "Renamed", &[]).unwrap();

        assert_eq!(result_dir, dir);
        assert!(dir.join("Renamed.Arma3Profile").is_file());
        assert!(!dir.join("CoreX.Arma3Profile").exists());
    }

    #[test]
    fn rename_profile_rejects_existing_target() {
        let root = tempfile::tempdir().unwrap();
        let dir = make_profile_dir(root.path(), "One", "One");
        make_profile_dir(root.path(), "Two", "Two");
        let profile = Arma3Profile {
            name: "One".to_string(),
            path: dir,
            is_default: false,
        };

        assert!(matches!(
            rename_profile(&profile, "Two", &[]),
            Err(ProfileOpError::TargetAlreadyExists)
        ));
    }

    #[test]
    fn clone_profile_copies_profile_files_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = make_profile_dir(root.path(), "Arma 3", "CoreX");
        let other_root = root.path().join("Arma 3 - Other Profiles");
        let profile = Arma3Profile {
            name: "CoreX".to_string(),
            path: dir,
            is_default: true,
        };

        let dest = clone_profile(&profile, "CoreX Copy", &other_root, &[]).unwrap();

        assert_eq!(dest, other_root.join("CoreX%20Copy"));
        assert!(dest.join("CoreX Copy.Arma3Profile").is_file());
        assert!(dest.join("CoreX Copy.vars.Arma3Profile").is_file());
        assert!(!dest.join("unrelated.txt").exists());
        assert_eq!(
            fs::read_to_string(dest.join("CoreX Copy.Arma3Profile")).unwrap(),
            "keybinds"
        );
    }

    #[test]
    fn delete_profile_moves_folder_to_trash_and_protects_default() {
        let root = tempfile::tempdir().unwrap();
        let dir = make_profile_dir(root.path(), "nested/Clone", "Clone");
        let trash = root.path().join("trash");
        let profile = Arma3Profile {
            name: "Clone".to_string(),
            path: dir.clone(),
            is_default: false,
        };

        let moved_to = delete_profile(&profile, &trash, &[]).unwrap();

        assert!(!dir.exists());
        assert!(moved_to.join("Clone.Arma3Profile").is_file());

        let default_profile = Arma3Profile {
            name: "Main".to_string(),
            path: make_profile_dir(root.path(), "Arma 3", "Main"),
            is_default: true,
        };
        assert!(matches!(
            delete_profile(&default_profile, &trash, &[]),
            Err(ProfileOpError::DefaultProfileProtected)
        ));
    }

    #[test]
    fn delete_profile_refuses_protected_roots() {
        let root = tempfile::tempdir().unwrap();
        let dir = make_profile_dir(root.path(), "custom/root", "Server");
        let profile = Arma3Profile {
            name: "Server".to_string(),
            path: dir.clone(),
            is_default: false,
        };

        assert!(matches!(
            delete_profile(&profile, &root.path().join("trash"), &[dir]),
            Err(ProfileOpError::UnsafePath)
        ));
    }

    #[test]
    fn paths_refer_to_same_dir_normalizes_separators() {
        assert!(paths_refer_to_same_dir(
            Path::new("C:/Users/x/Documents/Arma 3/"),
            Path::new("C:\\Users\\x\\Documents\\Arma 3")
        ));
        assert!(!paths_refer_to_same_dir(
            Path::new("C:/Users/x/Documents/Arma 3"),
            Path::new("C:/Users/x/Documents/Arma 3 - Other Profiles")
        ));
    }
}
