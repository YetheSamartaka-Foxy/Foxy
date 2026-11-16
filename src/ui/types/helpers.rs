use super::repository::{Repository, RepositoryGameBuild};
use super::repository_space::RepositorySpace;
use super::settings::SettingsViewState;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn selected_creator_dlc_codes(repo: &Repository) -> Vec<&'static str> {
    let mut codes = Vec::new();

    if repo.csla {
        codes.push("csla");
    }
    if repo.ef {
        codes.push("ef");
    }
    if repo.gm {
        codes.push("gm");
    }
    if repo.rf {
        codes.push("rf");
    }
    if repo.spe {
        codes.push("spe");
    }
    if repo.vn {
        codes.push("vn");
    }
    if repo.ws {
        codes.push("ws");
    }

    codes
}

pub fn set_creator_dlc_enabled(repo: &mut Repository, code: &str, enabled: bool) {
    let flag = match code {
        "csla" => &mut repo.csla,
        "ef" => &mut repo.ef,
        "gm" => &mut repo.gm,
        "rf" => &mut repo.rf,
        "spe" => &mut repo.spe,
        "vn" => &mut repo.vn,
        "ws" => &mut repo.ws,
        _ => return,
    };
    *flag = enabled;
}

pub fn split_additional_launch_params(params: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut active_quote: Option<char> = None;
    for ch in params.chars() {
        match ch {
            '"' | '\'' if active_quote.is_none() => active_quote = Some(ch),
            quote if Some(quote) == active_quote => active_quote = None,
            whitespace if whitespace.is_whitespace() && active_quote.is_none() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

/// Whether `params` already carries `token` as a standalone argument. Flags
/// are case-insensitive for the games Foxy launches.
pub fn has_launch_param_token(params: &str, token: &str) -> bool {
    split_additional_launch_params(params)
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case(token))
}

/// Add or remove a standalone flag in an additional-parameters string,
/// keeping every other argument. Arguments containing whitespace are quoted
/// again so the string splits back to the same argument list.
pub fn set_launch_param_token(params: &str, token: &str, enabled: bool) -> String {
    let mut args = split_additional_launch_params(params);
    let present = args.iter().any(|arg| arg.eq_ignore_ascii_case(token));
    if enabled && !present {
        args.push(token.to_string());
    } else if !enabled {
        args.retain(|arg| !arg.eq_ignore_ascii_case(token));
    }
    args.iter()
        .map(|arg| {
            if arg.chars().any(char::is_whitespace) {
                format!("\"{}\"", arg)
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn apply_repo_client_parameters(repo: &mut Repository, params: &str) {
    repo.skip_intro = false;
    repo.no_splash = false;
    repo.world_empty = false;
    repo.load_mission_to_memory = false;
    repo.enable_ht = false;
    repo.huge_pages = false;
    repo.no_logs = false;

    let mut additional = Vec::new();
    for arg in split_additional_launch_params(params) {
        match arg.to_ascii_lowercase().as_str() {
            "-skipintro" => repo.skip_intro = true,
            "-nosplash" => repo.no_splash = true,
            "-world=empty" => repo.world_empty = true,
            "-loadmissiontomemory" => repo.load_mission_to_memory = true,
            "-enableht" => repo.enable_ht = true,
            "-hugepages" => repo.huge_pages = true,
            "-nologs" => repo.no_logs = true,
            _ => additional.push(arg),
        }
    }

    repo.additional_params = additional.join(" ");
}

/// The `gameBuild` of a custom game's `repo.json`. Both names come from the
/// server, so anything that could leave the repository folder is refused.
pub fn repo_json_game_build(json: &Value) -> Option<RepositoryGameBuild> {
    let value = json.get("gameBuild")?;
    let addon = value.get("modName")?.as_str()?.trim();
    let executable = value.get("executable")?.as_str()?.trim().replace('\\', "/");
    let safe_addon = crate::core::utils::fs_safety::is_safe_child_path(addon)
        && !addon.contains(['/', '\\'])
        && addon != ".";
    let safe_executable = crate::core::utils::fs_safety::is_safe_child_path(&executable)
        && executable
            .split('/')
            .all(|component| !component.is_empty() && component != ".");
    if !safe_addon || !safe_executable {
        log::warn!("Ignoring a repo.json gameBuild that names an unsafe path");
        return None;
    }
    Some(RepositoryGameBuild {
        addon: addon.to_string(),
        executable,
    })
}

/// Record the repository's game build and drop it from every addon list, so
/// it can neither be shown nor disabled. Sync then falls back to the
/// manifest's own `enabled` flag for it, which the server always sets.
pub fn apply_repo_game_build(repo: &mut Repository, game_build: Option<RepositoryGameBuild>) {
    repo.game_build = game_build;
    strip_game_build_addon(repo);
}

pub fn strip_game_build_addon(repo: &mut Repository) {
    let Some(game_build) = repo.game_build.as_ref() else {
        return;
    };
    let is_game = |name: &String| name.eq_ignore_ascii_case(&game_build.addon);
    repo.addons.retain(|(name, _)| !is_game(name));
    repo.optional_addons.retain(|(name, _)| !is_game(name));
    for profile in &mut repo.profiles {
        profile.addons.retain(|(name, _)| !is_game(name));
        profile.optional_addons.retain(|(name, _)| !is_game(name));
    }
}

/// Locate the DLC list in a `repo.json`. Foxy publishes `dlcContent`; Swifty
/// repositories carry the same information as `requiredDLCs` (casing varies
/// between Swifty versions).
pub fn repo_json_dlc_content_value(json: &Value) -> Option<&Value> {
    if let Some(value) = json.get("dlcContent") {
        return Some(value);
    }
    json.as_object()?
        .iter()
        .find_map(|(key, value)| key.eq_ignore_ascii_case("requiredDLCs").then_some(value))
}

pub fn apply_repo_dlc_content_from_repo_json(repo: &mut Repository, value: &Value) {
    let (mut csla, mut ef, mut gm, mut rf, mut spe, mut vn, mut ws) =
        (false, false, false, false, false, false, false);

    match value {
        Value::Object(map) => {
            csla = map.get("csla").and_then(Value::as_bool).unwrap_or(false);
            ef = map.get("ef").and_then(Value::as_bool).unwrap_or(false);
            gm = map.get("gm").and_then(Value::as_bool).unwrap_or(false);
            rf = map.get("rf").and_then(Value::as_bool).unwrap_or(false);
            spe = map.get("spe").and_then(Value::as_bool).unwrap_or(false);
            vn = map.get("vn").and_then(Value::as_bool).unwrap_or(false);
            ws = map.get("ws").and_then(Value::as_bool).unwrap_or(false);
        }
        Value::Array(items) => {
            for item in items {
                let Some(raw_code) = item.as_str() else {
                    continue;
                };
                match raw_code.trim().to_ascii_lowercase().as_str() {
                    "csla" => csla = true,
                    "ef" => ef = true,
                    "gm" => gm = true,
                    "rf" => rf = true,
                    "spe" => spe = true,
                    "vn" => vn = true,
                    "ws" => ws = true,
                    _ => {}
                }
            }
        }
        _ => return,
    }

    repo.csla = csla;
    repo.ef = ef;
    repo.gm = gm;
    repo.rf = rf;
    repo.spe = spe;
    repo.vn = vn;
    repo.ws = ws;
}

fn sanitize_user_path_value(path: &str) -> String {
    path.trim().to_string()
}

fn normalized_user_path_key(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(&sanitize_user_path_value(path))
}

fn normalized_external_addon_name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

fn normalized_external_addon_key(name: &str, path: &str) -> (String, String) {
    (
        normalized_external_addon_name_key(name),
        normalized_user_path_key(path),
    )
}

fn sanitize_path_list(paths: &mut Vec<String>) {
    let mut seen = HashSet::new();
    for path in paths.iter_mut() {
        *path = sanitize_user_path_value(path);
    }
    paths.retain(|path| !path.is_empty() && seen.insert(normalized_user_path_key(path)));
}

fn sanitize_additional_folder_alias_map(settings: &mut SettingsViewState) {
    let valid_folder_keys: HashSet<String> = settings
        .additional_folders
        .iter()
        .map(|path| normalized_user_path_key(path))
        .collect();

    let mut sanitized_aliases: HashMap<String, String> = HashMap::new();
    for (path, alias) in std::mem::take(&mut settings.additional_folder_aliases) {
        let normalized_path = normalized_user_path_key(&path);
        let sanitized_alias = sanitize_additional_folder_alias(&alias);
        if sanitized_alias.is_empty() || !valid_folder_keys.contains(&normalized_path) {
            continue;
        }
        sanitized_aliases
            .entry(normalized_path)
            .or_insert(sanitized_alias);
    }

    settings.additional_folder_aliases = sanitized_aliases;
}

fn sanitize_cleanup_folder_list(paths: &mut Vec<(String, bool)>) {
    let mut seen = HashSet::new();
    for (path, _) in paths.iter_mut() {
        *path = sanitize_user_path_value(path);
    }
    paths.retain(|(path, _)| !path.is_empty() && seen.insert(normalized_user_path_key(path)));
}

pub fn sanitize_user_path(path: &str) -> String {
    sanitize_user_path_value(path)
}

pub fn additional_folder_alias_key(path: &str) -> String {
    normalized_user_path_key(path)
}

pub fn sanitize_additional_folder_alias(alias: &str) -> String {
    alias.trim().to_string()
}

/// Merge a remote-derived addon list with the existing local list so that
/// the user's enabled/disabled choice for each addon survives a `repo.json`
/// refresh. Addons no longer present remotely are dropped, newly added
/// remote addons inherit the remote default, and names are matched
/// case-insensitively because addon directory casing is not guaranteed.
pub fn merge_remote_addon_list(
    remote: Vec<(String, bool)>,
    local: &[(String, bool)],
) -> Vec<(String, bool)> {
    let local_by_name: HashMap<String, bool> = local
        .iter()
        .map(|(name, enabled)| (name.to_ascii_lowercase(), *enabled))
        .collect();
    remote
        .into_iter()
        .map(|(name, remote_enabled)| {
            let enabled = local_by_name
                .get(&name.to_ascii_lowercase())
                .copied()
                .unwrap_or(remote_enabled);
            (name, enabled)
        })
        .collect()
}

pub fn merge_remote_addon_categories(
    repo: &mut Repository,
    remote_required: Option<Vec<(String, bool)>>,
    remote_optional: Option<Vec<(String, bool)>>,
) -> Vec<String> {
    let old_required = repo.addons.clone();
    let old_optional = repo.optional_addons.clone();
    let update_both_categories = remote_required.is_some() && remote_optional.is_some();
    let required = remote_required.unwrap_or_else(|| old_required.clone());
    let optional = remote_optional.unwrap_or_else(|| old_optional.clone());
    let required_keys: HashSet<String> = required
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    let optional_keys: HashSet<String> = optional
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    let old_required_keys: HashSet<String> = old_required
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();
    let old_optional_keys: HashSet<String> = old_optional
        .iter()
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect();

    let moved_to_required: HashSet<String> = if update_both_categories {
        required_keys
            .intersection(&old_optional_keys)
            .cloned()
            .collect()
    } else {
        HashSet::new()
    };
    let moved_to_optional: HashSet<String> = if update_both_categories {
        optional_keys
            .intersection(&old_required_keys)
            .cloned()
            .collect()
    } else {
        HashSet::new()
    };

    let previous_profile_values: Vec<(String, HashMap<String, bool>)> = repo
        .profiles
        .iter()
        .map(|profile| {
            let values = profile
                .addons
                .iter()
                .chain(&profile.optional_addons)
                .map(|(name, enabled)| (name.to_ascii_lowercase(), *enabled))
                .collect();
            (profile.name.clone(), values)
        })
        .collect();

    repo.addons = merge_remote_addon_list(required.clone(), &old_required);
    repo.optional_addons = merge_remote_addon_list(optional.clone(), &old_optional);

    let required_profile_defaults: Vec<_> = required
        .iter()
        .map(|(name, published_default)| {
            let key = name.to_ascii_lowercase();
            let enabled = if moved_to_required.contains(&key) || old_required_keys.contains(&key) {
                *published_default
            } else {
                true
            };
            (name.clone(), enabled)
        })
        .collect();
    let optional_profile_defaults: Vec<_> = optional
        .iter()
        .map(|(name, published_default)| {
            let key = name.to_ascii_lowercase();
            let enabled = if moved_to_optional.contains(&key) || old_optional_keys.contains(&key) {
                *published_default
            } else {
                false
            };
            (name.clone(), enabled)
        })
        .collect();

    for profile in &mut repo.profiles {
        let same_category_required: Vec<_> = profile
            .addons
            .iter()
            .filter(|(name, _)| old_required_keys.contains(&name.to_ascii_lowercase()))
            .cloned()
            .collect();
        let same_category_optional: Vec<_> = profile
            .optional_addons
            .iter()
            .filter(|(name, _)| old_optional_keys.contains(&name.to_ascii_lowercase()))
            .cloned()
            .collect();
        profile.addons =
            merge_remote_addon_list(required_profile_defaults.clone(), &same_category_required);
        profile.optional_addons =
            merge_remote_addon_list(optional_profile_defaults.clone(), &same_category_optional);
    }

    let mut changed = Vec::new();
    for (name, _) in required.iter().chain(&optional) {
        let key = name.to_ascii_lowercase();
        let moved = moved_to_required.contains(&key) || moved_to_optional.contains(&key);
        if !moved {
            continue;
        }
        let old_default = old_required
            .iter()
            .chain(&old_optional)
            .find(|(old_name, _)| old_name.eq_ignore_ascii_case(name))
            .map(|(_, enabled)| *enabled);
        let new_default = repo
            .addons
            .iter()
            .chain(&repo.optional_addons)
            .find(|(new_name, _)| new_name.eq_ignore_ascii_case(name))
            .map(|(_, enabled)| *enabled);
        let profile_changed = previous_profile_values
            .iter()
            .any(|(profile_name, old_values)| {
                let old_value = old_values.get(&key).copied();
                let new_value = repo
                    .profiles
                    .iter()
                    .find(|profile| &profile.name == profile_name)
                    .and_then(|profile| {
                        profile
                            .addons
                            .iter()
                            .chain(&profile.optional_addons)
                            .find(|(new_name, _)| new_name.eq_ignore_ascii_case(name))
                            .map(|(_, enabled)| *enabled)
                    });
                old_value != new_value
            });
        if old_default != new_default || profile_changed {
            changed.push(name.clone());
        }
    }
    changed.sort_by_key(|name| name.to_ascii_lowercase());
    changed.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    changed
}

pub fn sanitize_external_addons(addons: &mut Vec<(String, bool, String)>) {
    let mut merged: Vec<(String, bool, String)> = Vec::with_capacity(addons.len());
    let mut index_by_key: HashMap<(String, String), usize> = HashMap::new();

    for (name, enabled, path) in addons.drain(..) {
        let trimmed_name = name.trim().to_string();
        if trimmed_name.is_empty() {
            continue;
        }

        let sanitized_path = sanitize_user_path_value(&path);
        let key = normalized_external_addon_key(&trimmed_name, &sanitized_path);

        if let Some(existing_index) = index_by_key.get(&key).copied() {
            merged[existing_index].1 = merged[existing_index].1 || enabled;
            continue;
        }

        let next_index = merged.len();
        index_by_key.insert(key, next_index);
        merged.push((trimmed_name, enabled, sanitized_path));
    }

    *addons = merged;
}

pub fn sanitize_addon_favorites(favorites: &mut Vec<String>) {
    let mut seen = HashSet::new();
    favorites.retain_mut(|name| {
        *name = name.trim().to_string();
        !name.is_empty() && seen.insert(normalized_external_addon_name_key(name))
    });
}

pub fn sanitize_external_addon_favorites(favorites: &mut Vec<String>) {
    let mut seen = HashSet::new();
    favorites.retain_mut(|path| {
        *path = sanitize_user_path_value(path);
        !path.trim().is_empty() && seen.insert(normalized_user_path_key(path))
    });
}

/// Returns `true` if the given filesystem path passes through a OneDrive sync
/// folder.
///
/// OneDrive's background file syncing can cause transient `PermissionDenied`
/// errors (Windows OS error 5) when the app tries to read, write, or delete
/// files that OneDrive is simultaneously uploading.
pub fn path_is_inside_onedrive(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|segment| {
        let lower = segment.trim().to_lowercase();
        lower == "onedrive" || lower.starts_with("onedrive ") || lower.starts_with("onedrive-")
    })
}

pub fn sanitize_settings_paths(settings: &mut SettingsViewState) {
    settings.arma3_directory = sanitize_user_path_value(&settings.arma3_directory);
    settings.twwh3_directory = sanitize_user_path_value(&settings.twwh3_directory);
    settings.reforger_directory = sanitize_user_path_value(&settings.reforger_directory);
    settings.generic_directory = sanitize_user_path_value(&settings.generic_directory);
    settings.arma3_profiles_directory =
        sanitize_user_path_value(&settings.arma3_profiles_directory);
    settings.steam_directory = sanitize_user_path_value(&settings.steam_directory);
    settings.temp_directory = sanitize_user_path_value(&settings.temp_directory);
    settings.backup_directory = sanitize_user_path_value(&settings.backup_directory);

    // Silently clear any paths that pass through a OneDrive sync folder to
    // prevent transient file-lock errors during downloads and launches.
    if path_is_inside_onedrive(&settings.arma3_directory) {
        log::warn!(
            "Clearing Arma 3 directory because it is inside a OneDrive folder: {}",
            settings.arma3_directory
        );
        settings.arma3_directory.clear();
    }
    if path_is_inside_onedrive(&settings.twwh3_directory) {
        log::warn!(
            "Clearing Total War: WARHAMMER III directory because it is inside a OneDrive folder: {}",
            settings.twwh3_directory
        );
        settings.twwh3_directory.clear();
    }
    if path_is_inside_onedrive(&settings.reforger_directory) {
        log::warn!(
            "Clearing Arma Reforger directory because it is inside a OneDrive folder: {}",
            settings.reforger_directory
        );
        settings.reforger_directory.clear();
    }
    if path_is_inside_onedrive(&settings.generic_directory) {
        log::warn!("Clearing generic game directory because it is inside a OneDrive folder");
        settings.generic_directory.clear();
    }
    if path_is_inside_onedrive(&settings.arma3_profiles_directory) {
        log::warn!(
            "Clearing Arma 3 profiles directory because it is inside a OneDrive folder: {}",
            settings.arma3_profiles_directory
        );
        settings.arma3_profiles_directory.clear();
    }
    if path_is_inside_onedrive(&settings.temp_directory) {
        log::warn!(
            "Clearing temporary directory because it is inside a OneDrive folder: {}",
            settings.temp_directory
        );
        settings.temp_directory.clear();
    }
    if path_is_inside_onedrive(&settings.backup_directory) {
        log::warn!(
            "Clearing backup directory because it is inside a OneDrive folder: {}",
            settings.backup_directory
        );
        settings.backup_directory.clear();
    }

    settings.additional_folders.retain(|folder| {
        if path_is_inside_onedrive(folder) {
            log::warn!(
                "Removing additional folder because it is inside a OneDrive folder: {}",
                folder
            );
            false
        } else {
            true
        }
    });

    sanitize_path_list(&mut settings.additional_folders);
    sanitize_additional_folder_alias_map(settings);
    sanitize_cleanup_folder_list(&mut settings.cleanup_folders);
}

pub fn normalize_settings_launch_behavior(settings: &mut SettingsViewState) {
    if settings.close_after_launch || !crate::ui::tray::TrayManager::is_available() {
        settings.hide_to_tray_after_launch = false;
    }
    #[cfg(not(target_os = "windows"))]
    {
        for job in &mut settings.scheduled_jobs {
            if job.post_action == super::scheduling::PostAction::ShutdownPc {
                job.post_action = super::scheduling::PostAction::None;
            }
        }
    }
}

/// Push `-profiles`/`-name` launch arguments.
///
/// Arma 3 treats `-name=<profile>` as "load the named profile from the
/// named-profiles root, creating a fresh one (default settings, default
/// keybinds) when it does not exist there". Passing `-name` for the default
/// profile in `Documents\Arma 3`, or forwarding that vanilla directory as
/// `-profiles`, therefore silently clones the player into a brand-new empty
/// profile. Both are filtered out here; `-name` is only passed when the
/// selected profile actually exists where Arma 3 will look for it.
pub fn push_arma3_profile_launch_args(
    settings: &SettingsViewState,
    repo: &Repository,
    detected_profiles: &[crate::core::arma3_profiles::Arma3Profile],
    args: &mut Vec<String>,
) {
    let profiles_directory = settings.arma3_profiles_directory.trim();
    let mut custom_profiles_dir: Option<&Path> = None;
    if !profiles_directory.is_empty() {
        let profiles_path = Path::new(profiles_directory);
        if crate::core::arma3_profiles::is_vanilla_profiles_location(profiles_path) {
            log::warn!(
                "Ignoring configured Arma 3 profiles directory because it is the vanilla profile location; passing it as -profiles would relocate profiles into a Users subfolder and reset settings and keybinds"
            );
        } else {
            args.push(format!("-profiles={}", profiles_directory));
            custom_profiles_dir = Some(profiles_path);
        }
    }

    let Some(arma3_profile) = repo
        .arma3_profile
        .as_deref()
        .map(str::trim)
        .filter(|profile| !profile.is_empty())
    else {
        return;
    };

    if let Some(custom_dir) = custom_profiles_dir {
        // With -profiles active, -name resolves against <dir>\Users\<name>.
        let exists_in_custom_dir = detected_profiles.iter().any(|profile| {
            profile.name.eq_ignore_ascii_case(arma3_profile)
                && crate::core::arma3_profiles::path_is_under(&profile.path, custom_dir)
        });
        if exists_in_custom_dir {
            args.push(format!("-name={}", arma3_profile));
        } else {
            log::warn!(
                "Skipping -name for Arma 3 profile '{}': it does not exist under the configured profiles directory, so Arma 3 would create a new empty profile",
                arma3_profile
            );
        }
        return;
    }

    let matches_named = detected_profiles
        .iter()
        .any(|profile| !profile.is_default && profile.name.eq_ignore_ascii_case(arma3_profile));
    let matches_default = detected_profiles
        .iter()
        .any(|profile| profile.is_default && profile.name.eq_ignore_ascii_case(arma3_profile));

    if matches_default {
        // The default profile is what Arma 3 loads without -name. Passing
        // -name here would make the game create an empty duplicate under
        // "Arma 3 - Other Profiles". When a duplicate named profile also
        // exists (a leftover of that exact accident), still prefer the
        // default profile because it holds the user's real settings.
        log::info!(
            "Selected Arma 3 profile '{}' is the default profile; omitting -name so the game loads it directly",
            arma3_profile
        );
    } else if matches_named {
        args.push(format!("-name={}", arma3_profile));
    } else {
        log::warn!(
            "Skipping -name for Arma 3 profile '{}': no such profile was detected on disk, so Arma 3 would create a new empty profile",
            arma3_profile
        );
    }
}

pub fn sanitize_repository_paths(repo: &mut Repository) {
    repo.path = sanitize_user_path_value(&repo.path);
    // A repository instance is keyed by `(remote_url, local_path)`; stray
    // whitespace around a configured address makes every DB lookup miss its own
    // row, so repair it on load instead of carrying it into the key.
    repo.address = repo.address.trim().to_string();
    if let Some(entry_address) = repo.repository_space_entry_address.as_mut() {
        *entry_address = entry_address.trim().to_string();
    }
    repo.app_update_url = repo.app_update_url.trim().to_string();
    sanitize_addon_favorites(&mut repo.optional_addon_favorites);
    sanitize_addon_favorites(&mut repo.optional_addon_client_side);
    sanitize_addon_favorites(&mut repo.remote_client_side_addons);
    sanitize_external_addons(&mut repo.external_addons);
    sanitize_external_addon_favorites(&mut repo.external_addon_favorites);
    sanitize_external_addon_favorites(&mut repo.external_addon_client_side);
    for profile in &mut repo.profiles {
        sanitize_addon_favorites(&mut profile.optional_addon_favorites);
        sanitize_addon_favorites(&mut profile.optional_addon_client_side);
        sanitize_external_addons(&mut profile.external_addons);
        sanitize_external_addon_favorites(&mut profile.external_addon_favorites);
        sanitize_external_addon_favorites(&mut profile.external_addon_client_side);
    }
}

pub fn normalize_loaded_repository(repo: &mut Repository) {
    sanitize_repository_paths(repo);
    repo.app_update_url = repo.app_update_url.trim().to_string();
    strip_game_build_addon(repo);

    let master_addons = repo.addons.clone();
    let master_optional = repo.optional_addons.clone();

    for profile in &mut repo.profiles {
        let disabled_addons: HashSet<_> = profile
            .addons
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        profile.addons = master_addons
            .iter()
            .map(|(name, _)| (name.clone(), !disabled_addons.contains(name)))
            .collect();

        let enabled_optional: HashSet<_> = profile
            .optional_addons
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        profile.optional_addons = master_optional
            .iter()
            .map(|(name, _)| (name.clone(), enabled_optional.contains(name)))
            .collect();

        sanitize_external_addons(&mut profile.external_addons);
    }
}

pub fn normalize_loaded_repositories(repositories: &mut [Repository]) {
    for repo in repositories {
        normalize_loaded_repository(repo);
    }
}

pub fn sanitize_repository_space_paths(space: &mut RepositorySpace) {
    space.shared_path = sanitize_user_path_value(&space.shared_path);
    space.app_update_url = space.app_update_url.trim().to_string();
    space.source_address = space.source_address.trim().to_string();
    space.source_base_url = space.source_base_url.trim().to_string();
    for entry in &mut space.entries {
        entry.address = entry.address.trim().to_string();
    }
}

pub fn sanitize_repository_spaces_paths(spaces: &mut [RepositorySpace]) {
    for space in spaces {
        sanitize_repository_space_paths(space);
    }
}
