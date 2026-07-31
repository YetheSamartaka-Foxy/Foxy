use std::path::{Path, PathBuf};

use crate::core::steam;
use crate::core::utils::fs_safety::resolve_child_dir_case_insensitive;
use crate::ui::types::{
    Repository, RepositoryServer, SettingsViewState, push_arma3_profile_launch_args,
    selected_creator_dlc_codes, split_additional_launch_params,
};

use super::{
    DirectorySetting, GameCapabilities, GameDetectCtx, GameLaunchCtx, GameModule,
    GameSettingsSchema, LaunchCommand, LaunchError, LaunchPlan, ResolvedMod, ServerTarget,
    ToggleSetting,
};

pub const ARMA3_GAME_ID: &str = "arma3";

pub struct Arma3Module;

/// Key of the Arma 3 half of the module-owned `Profile::launch` blob: the
/// full `RepositoryProfile` (DLC flags, launch flags, params, addon
/// selection detail), so a pack round-trip is lossless.
const ARMA3_PROFILE_LAUNCH_KEY: &str = "arma3_repository_profile";

impl GameModule for Arma3Module {
    fn id(&self) -> &'static str {
        ARMA3_GAME_ID
    }

    fn display_name(&self) -> &str {
        "Arma 3"
    }

    fn capabilities(&self) -> GameCapabilities {
        GameCapabilities {
            repository_sync: true,
            repository_launch: true,
            steam_workshop: true,
            direct_download: true,
            extra_files: true,
            profiles: true,
            foxy_config_export: true,
            teamspeak3_plugins: true,
        }
    }

    fn detect_install_dir(&self, ctx: &GameDetectCtx) -> Option<PathBuf> {
        steam::detect_arma3_install_directory(ctx.steam_directory)
    }

    fn validate_install_dir(&self, path: &Path) -> bool {
        steam::is_valid_arma3_dir(path)
    }

    fn build_launch(
        &self,
        plan: &LaunchPlan,
        ctx: &GameLaunchCtx,
    ) -> Result<LaunchCommand, LaunchError> {
        let install_dir = ctx.install_dir.trim();
        let install_dir_path = if install_dir.is_empty() {
            Path::new(".")
        } else {
            Path::new(install_dir)
        };

        let mut game_args = plan.launch_args.clone();

        if !plan.mods.is_empty() {
            let mod_values: Vec<&str> = plan.mods.iter().map(ResolvedMod::launch_value).collect();
            game_args.push(format!("-mod={}", mod_values.join(";")));
        }

        if let Some(server) = &plan.server {
            game_args.push(format!("-connect={}", server.address));
            game_args.push(format!("-port={}", server.port));
            if !server.password.is_empty() {
                game_args.push(format!("-password={}", server.password));
            }
        }

        let Some(launch) =
            steam::arma3_launch_command(install_dir_path, ctx.steam_directory.trim())
        else {
            log::warn!("Cannot create launch command: Steam launch command is unavailable");
            return Err(LaunchError::LauncherUnavailable);
        };

        let cwd = (!install_dir.is_empty() && install_dir_path.exists())
            .then(|| install_dir_path.to_path_buf());

        let mut args = launch.args;
        args.extend(game_args);

        Ok(LaunchCommand {
            program: launch.program,
            args,
            cwd,
        })
    }

    fn repository_profile_to_profile(
        &self,
        repository_profile: &crate::ui::types::RepositoryProfile,
        repository_url: &str,
    ) -> crate::core::game::Profile {
        let mut profile = crate::core::game::profile::generic_profile_from_repository_profile(
            repository_profile,
            repository_url,
        );
        match serde_json::to_value(repository_profile) {
            Ok(value) => {
                profile.launch = serde_json::json!({ ARMA3_PROFILE_LAUNCH_KEY: value });
            }
            Err(err) => {
                log::warn!("Failed to serialize the Arma 3 launch blob: {}", err);
            }
        }
        profile
    }

    fn profile_to_repository_profile(
        &self,
        profile: &crate::core::game::Profile,
    ) -> crate::ui::types::RepositoryProfile {
        if let Some(blob) = profile.launch.get(ARMA3_PROFILE_LAUNCH_KEY) {
            match serde_json::from_value::<crate::ui::types::RepositoryProfile>(blob.clone()) {
                Ok(mut repository_profile) => {
                    repository_profile.name = profile.name.clone();
                    return repository_profile;
                }
                Err(err) => {
                    log::warn!(
                        "Ignoring unreadable Arma 3 launch blob for profile {}: {}",
                        profile.name,
                        err
                    );
                }
            }
        }
        crate::core::game::profile::generic_repository_profile_from_profile(profile)
    }

    fn settings_schema(&self) -> GameSettingsSchema {
        GameSettingsSchema {
            directories: vec![
                DirectorySetting {
                    id: "arma3_directory",
                    label: "Arma 3 Directory",
                    help: None,
                    auto_detect: true,
                    is_install_dir: true,
                },
                DirectorySetting {
                    id: "teamspeak3_directory",
                    label: "TeamSpeak 3 Directory",
                    help: None,
                    auto_detect: true,
                    is_install_dir: false,
                },
                DirectorySetting {
                    id: "arma3_profiles_directory",
                    label: "Arma 3 Profiles Directory",
                    help: Some(
                        "Optional. Foxy passes this to Arma 3 as -profiles so profile files are stored outside Documents or OneDrive.",
                    ),
                    auto_detect: false,
                    is_install_dir: false,
                },
            ],
            toggles: vec![
                ToggleSetting {
                    id: "apply_repo_json_client_parameters",
                    label: "Auto apply repo.json launch parameters",
                    help: "Automatically apply launch parameters from the repository's repo.json when launching Arma 3.",
                },
                ToggleSetting {
                    id: "apply_repo_json_dlc_content",
                    label: "Auto apply repo.json DLC content",
                    help: "Automatically enable DLC content specified by the repository's repo.json when launching Arma 3.",
                },
                ToggleSetting {
                    id: "warn_editor_external_addons",
                    label: "Warn before launching editor with external addons",
                    help: "Show a confirmation before opening Eden Editor when additional/external addons are enabled.",
                },
                ToggleSetting {
                    id: "enable_editor_mission_list",
                    label: "Show Editor Missions list",
                    help: "Show the Editor Missions section in the repository view. Can be overridden per repository.",
                },
                ToggleSetting {
                    id: "enable_server_list",
                    label: "Show Servers list",
                    help: "Show the Servers section in the repository view. Can be overridden per repository.",
                },
                ToggleSetting {
                    id: "check_server_addons_before_join",
                    label: "Check server addons before joining",
                    help: "Before joining a server, query its addon list and offer to enable matching disabled local addons.",
                },
                ToggleSetting {
                    id: "check_ts3_running_before_join",
                    label: "Check TeamSpeak is running before joining",
                    help: "Before joining a server with a repository that ships TeamSpeak plugins, warn if TeamSpeak 3 is not running and offer to launch it.",
                },
                ToggleSetting {
                    id: "check_steam_running_before_launch",
                    label: "Check Steam is running before launching",
                    help: "Before launching or joining, warn if Steam is not running (Arma 3 needs Steam) and offer to launch it.",
                },
            ],
        }
    }

    fn install_dir_from_settings<'a>(&self, settings: &'a SettingsViewState) -> &'a str {
        &settings.arma3_directory
    }

    fn steam_app_id(&self) -> Option<u32> {
        Some(107410)
    }
}

pub fn build_launch_plan(
    settings: &SettingsViewState,
    repo: &Repository,
    server: Option<&RepositoryServer>,
) -> Result<LaunchPlan, LaunchError> {
    let arma3_directory = settings.arma3_directory.trim();

    #[cfg(target_os = "windows")]
    {
        if arma3_directory.is_empty() {
            log::warn!(
                "Cannot create launch command: Arma 3 directory is not configured (raw value {:?})",
                settings.arma3_directory
            );
            return Err(LaunchError::InstallDirNotConfigured);
        }
        let arma3_dir_path = Path::new(arma3_directory);
        if !arma3_dir_path.exists() {
            log::warn!(
                "Cannot create launch command: Arma 3 directory does not exist: {}",
                arma3_directory
            );
            return Err(LaunchError::InstallDirMissing);
        }
        if !steam::is_valid_arma3_dir(arma3_dir_path) {
            log::warn!(
                "Cannot create launch command: Arma 3 directory is not valid: {}",
                arma3_directory
            );
            return Err(LaunchError::InstallDirInvalid);
        }
    }

    let mut launch_args = Vec::new();

    // Re-detect profiles at launch time so -name decisions reflect the
    // current on-disk state, not a cached list.
    let custom_profiles_dir = settings.arma3_profiles_directory.trim();
    let custom_profiles_dir = if custom_profiles_dir.is_empty() {
        None
    } else {
        Some(Path::new(custom_profiles_dir))
    };
    let detected_profiles = crate::core::arma3_profiles::detect_all_profiles(custom_profiles_dir);
    push_arma3_profile_launch_args(settings, repo, &detected_profiles, &mut launch_args);

    if repo.skip_intro {
        launch_args.push("-skipIntro".to_string());
    }
    if repo.no_splash {
        launch_args.push("-noSplash".to_string());
    }
    if repo.world_empty {
        launch_args.push("-world=empty".to_string());
    }
    if repo.load_mission_to_memory {
        launch_args.push("-loadMissionToMemory".to_string());
    }
    if repo.enable_ht {
        launch_args.push("-enableHT".to_string());
    }
    if repo.huge_pages {
        launch_args.push("-hugePages".to_string());
    }
    if repo.no_logs {
        launch_args.push("-noLogs".to_string());
    }

    if !repo.additional_params.is_empty() {
        launch_args.extend(split_additional_launch_params(&repo.additional_params));
    }

    let mods = resolve_launch_mods(repo, arma3_directory);

    let server = server.map(|server| ServerTarget {
        address: server.address.clone(),
        port: server.port.clone(),
        password: server.password.clone(),
    });

    Ok(LaunchPlan {
        launch_args,
        mods,
        server,
    })
}

fn resolve_launch_mods(repo: &Repository, arma3_directory: &str) -> Vec<ResolvedMod> {
    let creator_dlc_codes = selected_creator_dlc_codes(repo);
    let enabled_addons: Vec<String> = repo
        .addons
        .iter()
        .map(|(addon, enabled)| (addon, *enabled))
        .chain(
            repo.optional_addons
                .iter()
                .map(|(addon, enabled)| (addon, *enabled)),
        )
        .filter_map(|(addon, enabled)| if enabled { Some(addon.clone()) } else { None })
        .collect();
    let enabled_external_addons = repo
        .external_addons
        .iter()
        .filter(|(_, enabled, _)| *enabled)
        .collect::<Vec<_>>();

    if creator_dlc_codes.is_empty()
        && enabled_addons.is_empty()
        && enabled_external_addons.is_empty()
    {
        return Vec::new();
    }

    let mut resolved: Vec<ResolvedMod> = Vec::new();
    let repo_path = repo.path.trim();

    for creator_dlc_code in creator_dlc_codes {
        resolved.push(ResolvedMod {
            id: creator_dlc_code.to_string(),
            path: None,
        });
    }

    for addon in &enabled_addons {
        if let Some(addon_path) = resolve_child_dir_case_insensitive(Path::new(repo_path), addon) {
            resolved.push(ResolvedMod {
                id: addon.clone(),
                path: Some(addon_path.to_string_lossy().to_string()),
            });
        } else if let Some(arma3_addon_path) =
            resolve_child_dir_case_insensitive(Path::new(arma3_directory), addon)
        {
            resolved.push(ResolvedMod {
                id: addon.clone(),
                path: Some(arma3_addon_path.to_string_lossy().to_string()),
            });
        } else {
            log::error!(
                "Addon not found in repository or Arma 3 directory: {}",
                addon
            );
        }
    }

    for (addon, _, path) in enabled_external_addons {
        if let Some(external_path) = resolve_external_launch_addon_path(addon, path) {
            resolved.push(ResolvedMod {
                id: addon.clone(),
                path: Some(external_path.to_string_lossy().to_string()),
            });
        } else {
            log::error!(
                "External addon not found at configured path: addon={} path={}",
                addon,
                path
            );
        }
    }

    resolved
}

fn resolve_external_launch_addon_path(addon: &str, path: &str) -> Option<PathBuf> {
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return None;
    }

    let base_path = Path::new(trimmed_path);
    if let Some(frozen_path) = crate::core::game::workshop::resolve_launch_path_override_for_path(
        &crate::core::game::spaces::active_game_space_dir(),
        107410,
        trimmed_path,
    ) {
        return Some(frozen_path);
    }

    if let Some(nested_path) = resolve_child_dir_case_insensitive(base_path, addon) {
        return Some(nested_path);
    }

    if base_path.is_dir() {
        if workshop_id_from_launch_path(trimmed_path).is_some() {
            return Some(base_path.to_path_buf());
        }
        let base_name = base_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if base_name.trim_start().starts_with('@') {
            return Some(base_path.to_path_buf());
        }
        let base_name = normalize_launch_addon_name(base_name);
        let addon_key = normalize_launch_addon_name(addon);
        if !base_name.is_empty() && base_name == addon_key {
            return Some(base_path.to_path_buf());
        }
    }

    None
}

fn workshop_id_from_launch_path(path: &str) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for window in parts.windows(4) {
        if window[0].eq_ignore_ascii_case("workshop")
            && window[1].eq_ignore_ascii_case("content")
            && window[2] == "107410"
            && window[3].chars().all(|ch| ch.is_ascii_digit())
        {
            return Some(window[3].to_string());
        }
    }

    for pair in parts.windows(2) {
        if pair[0] == "107410" && pair[1].chars().all(|ch| ch.is_ascii_digit()) {
            return Some(pair[1].to_string());
        }
    }

    None
}

fn normalize_launch_addon_name(name: &str) -> String {
    name.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .chars()
        .filter_map(|ch| {
            if ch.is_whitespace() || matches!(ch, '-' | '_' | '.') {
                Some('_')
            } else if ch == '@' {
                None
            } else if ch.is_ascii() {
                Some(ch.to_ascii_lowercase())
            } else {
                ch.to_lowercase().next()
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_arma3_install() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("arma3_x64.exe"), "").expect("exe marker");
        dir
    }

    #[test]
    fn external_launch_addon_resolves_repo_root_plus_addon_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_dir).expect("addon dir");

        let resolved =
            resolve_external_launch_addon_path("@burnem_redux", &dir.path().to_string_lossy())
                .expect("external addon path should resolve");

        assert_eq!(resolved, addon_dir);
    }

    #[test]
    fn external_launch_addon_rejects_repo_root_when_addon_folder_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");

        let resolved =
            resolve_external_launch_addon_path("@burnem_redux", &dir.path().to_string_lossy());

        assert!(resolved.is_none());
    }

    #[test]
    fn external_launch_addon_accepts_direct_at_folder_with_display_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_dir).expect("addon dir");

        let resolved =
            resolve_external_launch_addon_path("Burn Em Redux", &addon_dir.to_string_lossy())
                .expect("direct @addon path should resolve");

        assert_eq!(resolved, addon_dir);
    }

    #[test]
    fn external_launch_addon_accepts_direct_workshop_id_folder_with_display_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir
            .path()
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join("107410")
            .join("463939057");
        std::fs::create_dir_all(&addon_dir).expect("workshop addon dir");

        let resolved = resolve_external_launch_addon_path("ACE", &addon_dir.to_string_lossy())
            .expect("direct workshop ID folder path should resolve");

        assert_eq!(resolved, addon_dir);
    }

    #[test]
    fn launch_mods_include_external_addons_without_repo_addons() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@client_mod");
        std::fs::create_dir(&addon_dir).expect("addon dir");
        let repo = Repository {
            external_addons: vec![(
                "@client_mod".to_string(),
                true,
                addon_dir.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };

        let resolved = resolve_launch_mods(&repo, "");

        assert_eq!(
            resolved,
            vec![ResolvedMod {
                id: "@client_mod".to_string(),
                path: Some(addon_dir.to_string_lossy().to_string()),
            }]
        );
    }

    #[test]
    fn build_launch_plan_collects_flags_dlc_additional_params_and_server() {
        let install = valid_arma3_install();
        let settings = SettingsViewState {
            arma3_directory: install.path().to_string_lossy().to_string(),
            ..SettingsViewState::default()
        };
        let repo = Repository {
            gm: true,
            skip_intro: true,
            no_logs: true,
            additional_params: "-window".to_string(),
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "203.0.113.10".to_string(),
            port: "2302".to_string(),
            password: "secret".to_string(),
            battle_eye: false,
        };

        let plan = build_launch_plan(&settings, &repo, Some(&server)).expect("plan should build");

        assert_eq!(plan.launch_args, vec!["-skipIntro", "-noLogs", "-window"]);
        assert_eq!(
            plan.mods,
            vec![ResolvedMod {
                id: "gm".to_string(),
                path: None,
            }]
        );
        assert_eq!(
            plan.server,
            Some(ServerTarget {
                address: "203.0.113.10".to_string(),
                port: "2302".to_string(),
                password: "secret".to_string(),
            })
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn build_launch_produces_exact_arma3_command() {
        let install = valid_arma3_install();
        let install_dir = install.path().to_string_lossy().to_string();
        let settings = SettingsViewState {
            arma3_directory: install_dir.clone(),
            ..SettingsViewState::default()
        };
        let repo = Repository {
            gm: true,
            skip_intro: true,
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "203.0.113.10".to_string(),
            port: "2302".to_string(),
            password: "secret".to_string(),
            battle_eye: false,
        };

        let plan = build_launch_plan(&settings, &repo, Some(&server)).expect("plan should build");
        let ctx = GameLaunchCtx {
            install_dir: &settings.arma3_directory,
            steam_directory: &settings.steam_directory,
        };
        let command = Arma3Module
            .build_launch(&plan, &ctx)
            .expect("command should build");

        assert_eq!(command.program, install.path().join("arma3_x64.exe"));
        assert_eq!(
            command.args,
            vec![
                "-skipIntro",
                "-mod=gm",
                "-connect=203.0.113.10",
                "-port=2302",
                "-password=secret",
            ]
        );
        assert_eq!(command.cwd, Some(install.path().to_path_buf()));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn build_launch_plan_rejects_unconfigured_missing_and_invalid_install_dirs() {
        let repo = Repository::default();

        let empty = SettingsViewState::default();
        assert_eq!(
            build_launch_plan(&empty, &repo, None).unwrap_err(),
            LaunchError::InstallDirNotConfigured
        );

        let missing = SettingsViewState {
            arma3_directory: "C:\\does\\not\\exist\\foxy-test".to_string(),
            ..SettingsViewState::default()
        };
        assert_eq!(
            build_launch_plan(&missing, &repo, None).unwrap_err(),
            LaunchError::InstallDirMissing
        );

        let no_exe = tempfile::tempdir().expect("temp dir");
        let invalid = SettingsViewState {
            arma3_directory: no_exe.path().to_string_lossy().to_string(),
            ..SettingsViewState::default()
        };
        assert_eq!(
            build_launch_plan(&invalid, &repo, None).unwrap_err(),
            LaunchError::InstallDirInvalid
        );
    }

    #[test]
    fn arma3_profile_mapping_round_trips_through_the_launch_blob() {
        use crate::ui::types::RepositoryProfile;
        let original = RepositoryProfile {
            name: "Event".to_string(),
            gm: true,
            ws: true,
            skip_intro: true,
            huge_pages: true,
            additional_params: "-window".to_string(),
            addons: vec![("@core".to_string(), false)],
            optional_addons: vec![("@blastcore".to_string(), true)],
            optional_addon_favorites: vec!["@blastcore".to_string()],
            optional_addon_client_side: vec!["@blastcore".to_string()],
            external_addons: vec![("@client".to_string(), true, "D:/mods".to_string())],
            ..RepositoryProfile::default()
        };

        let generic =
            Arma3Module.repository_profile_to_profile(&original, "https://repo.example/main/");
        let restored = Arma3Module.profile_to_repository_profile(&generic);

        assert_eq!(restored, original);
        // The generic enabled set is still populated for module-agnostic readers.
        assert_eq!(generic.enabled_mods.len(), 2);
    }

    #[test]
    fn arma3_profile_import_falls_back_to_enabled_mods_without_a_blob() {
        let generic = crate::core::game::Profile {
            name: "Foreign".to_string(),
            enabled_mods: vec![crate::core::game::profile::ModRef {
                source: crate::core::game::profile::ModSourceKind::Repository,
                name: "@blastcore".to_string(),
                kind: crate::core::game::profile::ModRefKind::Optional,
                repository_url: None,
                path: None,
            }],
            config_folder: None,
            extra_files: Vec::new(),
            launch: serde_json::Value::Null,
        };

        let restored = Arma3Module.profile_to_repository_profile(&generic);

        assert_eq!(restored.name, "Foreign");
        assert_eq!(
            restored.optional_addons,
            vec![("@blastcore".to_string(), true)]
        );
        assert!(!restored.skip_intro);
    }

    #[test]
    fn registry_resolves_active_arma3_module() {
        let module = crate::core::game::registry().active();
        assert_eq!(module.id(), "arma3");
        assert_eq!(module.display_name(), "Arma 3");
        assert!(module.capabilities().repository_sync);
        assert!(module.capabilities().steam_workshop);
        assert_eq!(module.steam_app_id(), Some(107410));
    }
}
