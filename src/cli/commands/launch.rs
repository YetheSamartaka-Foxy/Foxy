use super::{AppState, CommandError, CommandSuccess, effective_repository, find_repository_index};
use crate::cli::args::{CliArgs, LaunchArgs};
use crate::cli::exit_codes;
use crate::core::steam::{SteamEnsureResult, ensure_steam_running};
use crate::core::utils::fs_safety::resolve_child_dir_case_insensitive;
use crate::ui::types::{
    Repository, RepositoryServer, SettingsViewState, push_arma3_profile_launch_args,
    selected_creator_dlc_codes, split_additional_launch_params,
};
use serde_json::json;
use std::path::{Path, PathBuf};

pub fn cmd_launch(cli: &CliArgs, args: LaunchArgs) -> Result<CommandSuccess, CommandError> {
    let active = crate::core::game::spaces::active_game_space();
    let Some(module) = crate::core::game::registry().active_module() else {
        return Err(CommandError::validation(
            "launch",
            format!(
                "Game space {} is for game {}, which this build does not support",
                active.space_id, active.game_id
            ),
        ));
    };
    if !module.capabilities().repository_launch {
        return Err(CommandError::validation(
            "launch",
            format!(
                "launch starts a game from a repository, which {} does not support (active game space {}). Use `foxy game launch` for the active game.",
                module.display_name(),
                active.space_id
            ),
        ));
    }
    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let effective = effective_repository(&repo);

    let selected_server = if let Some(name) = args.server.as_deref() {
        let Some(server) = effective
            .servers
            .iter()
            .find(|server| server.name.eq_ignore_ascii_case(name))
            .cloned()
        else {
            return Err(CommandError::not_found(
                "launch",
                format!("Server {} not found", name),
            ));
        };
        Some(server)
    } else {
        None
    };

    let launch_spec = build_launch_spec(&state.settings, &effective, selected_server.as_ref())?;

    if cli.dry_run || !args.execute {
        return Ok(CommandSuccess {
            action: "launch".to_string(),
            message: "Launch command generated".to_string(),
            data: json!({
                "repository": repo.name,
                "selected_profile": repo.selected_profile,
                "executable": launch_spec.executable.display().to_string(),
                "args": launch_spec.args,
                "cwd": launch_spec.cwd.display().to_string(),
                "execute": false,
                "dry_run": cli.dry_run
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    if launch_spec.requires_existing_executable && !launch_spec.executable.exists() {
        return Err(CommandError::validation(
            "launch",
            format!(
                "Arma executable not found: {}",
                launch_spec.executable.display()
            ),
        ));
    }

    let extra_file_activation = crate::core::game::extra_files::activate_for_launch(
        &crate::core::game::spaces::active_game_space_dir(),
        &state.settings.arma3_directory,
    )
    .map_err(|err| {
        CommandError::operation(
            "launch",
            format!("Failed to apply extra files before launch: {}", err),
        )
    })?;

    let steam_status = match ensure_steam_running(&state.settings.steam_directory) {
        Ok(SteamEnsureResult::AlreadyRunning) => "already_running",
        Ok(SteamEnsureResult::Started) => "started",
        Ok(SteamEnsureResult::SkippedMissingDirectory) => "not_configured_fallback",
        Err(err) => {
            return Err(CommandError::operation(
                "launch",
                format!("Failed to prepare Steam before launch: {}", err),
            ));
        }
    };

    let mut cmd = std::process::Command::new(&launch_spec.executable);
    cmd.args(&launch_spec.args);
    cmd.current_dir(&launch_spec.cwd);
    let child = cmd
        .spawn()
        .map_err(|e| CommandError::operation("launch", format!("Failed to launch: {}", e)))?;

    let message = match steam_status {
        "started" => format!(
            "Steam started and launch command started (pid={})",
            child.id()
        ),
        "not_configured_fallback" => format!(
            "Launch command started without Steam pre-check (pid={})",
            child.id()
        ),
        _ => format!("Launch command started (pid={})", child.id()),
    };

    Ok(CommandSuccess {
        action: "launch".to_string(),
        message,
        data: json!({
            "repository": repo.name,
            "selected_profile": repo.selected_profile,
            "executable": launch_spec.executable.display().to_string(),
            "args": launch_spec.args,
            "cwd": launch_spec.cwd.display().to_string(),
            "steam_status": steam_status,
            "extra_files": {
                "activated": extra_file_activation.activated.len(),
                "failed": extra_file_activation.failed.len(),
                "skipped_disabled": extra_file_activation.skipped_disabled
            },
            "execute": true,
            "pid": child.id()
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

#[derive(Clone, Debug)]
struct LaunchSpec {
    executable: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    requires_existing_executable: bool,
}

fn build_launch_spec(
    settings: &SettingsViewState,
    repo: &Repository,
    server: Option<&RepositoryServer>,
) -> Result<LaunchSpec, CommandError> {
    let custom_profiles_dir = settings.arma3_profiles_directory.trim();
    let custom_profiles_dir = if custom_profiles_dir.is_empty() {
        None
    } else {
        Some(Path::new(custom_profiles_dir))
    };
    let detected_profiles = crate::core::arma3_profiles::detect_all_profiles(custom_profiles_dir);
    build_launch_spec_with_profiles(settings, repo, server, &detected_profiles)
}

fn build_launch_spec_with_profiles(
    settings: &SettingsViewState,
    repo: &Repository,
    server: Option<&RepositoryServer>,
    detected_profiles: &[crate::core::arma3_profiles::Arma3Profile],
) -> Result<LaunchSpec, CommandError> {
    let arma3_directory = settings.arma3_directory.trim();
    #[cfg(target_os = "windows")]
    if arma3_directory.is_empty() {
        return Err(CommandError::validation(
            "launch",
            "Arma 3 directory is not set in settings",
        ));
    }

    let cwd = launch_working_directory(arma3_directory)?;
    let mut args = Vec::new();

    push_arma3_profile_launch_args(settings, repo, detected_profiles, &mut args);

    if repo.skip_intro {
        args.push("-skipIntro".to_string());
    }
    if repo.no_splash {
        args.push("-noSplash".to_string());
    }
    if repo.world_empty {
        args.push("-world=empty".to_string());
    }
    if repo.load_mission_to_memory {
        args.push("-loadMissionToMemory".to_string());
    }
    if repo.enable_ht {
        args.push("-enableHT".to_string());
    }
    if repo.huge_pages {
        args.push("-hugePages".to_string());
    }
    if repo.no_logs {
        args.push("-noLogs".to_string());
    }

    if !repo.additional_params.trim().is_empty() {
        args.extend(split_additional_launch_params(&repo.additional_params));
    }

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
        .chain(
            repo.external_addons
                .iter()
                .map(|(addon, enabled, _)| (addon, *enabled)),
        )
        .filter_map(|(addon, enabled)| if enabled { Some(addon.clone()) } else { None })
        .collect();

    if !creator_dlc_codes.is_empty() || !enabled_addons.is_empty() {
        let mut resolved_addons = Vec::new();
        let repo_path = repo.path.trim();

        for creator_dlc_code in creator_dlc_codes {
            resolved_addons.push(creator_dlc_code.to_string());
        }

        for addon in &enabled_addons {
            if let Some(addon_path) =
                resolve_child_dir_case_insensitive(Path::new(repo_path), addon)
            {
                resolved_addons.push(addon_path.to_string_lossy().to_string());
            } else if let Some(arma3_addon_path) =
                resolve_child_dir_case_insensitive(Path::new(arma3_directory), addon)
            {
                resolved_addons.push(arma3_addon_path.to_string_lossy().to_string());
            }
        }

        for (addon, enabled, path) in &repo.external_addons {
            if *enabled {
                let trimmed_path = path.trim();
                if let Some(external_path) =
                    resolve_child_dir_case_insensitive(Path::new(trimmed_path), addon)
                {
                    resolved_addons.push(external_path.to_string_lossy().to_string());
                } else {
                    resolved_addons.push(trimmed_path.to_string());
                }
            }
        }

        if !resolved_addons.is_empty() {
            args.push(format!("-mod={}", resolved_addons.join(";")));
        }
    }

    if let Some(server) = server {
        args.push(format!("-connect={}", server.address));
        args.push(format!("-port={}", server.port));
        if !server.password.trim().is_empty() {
            args.push(format!("-password={}", server.password));
        }
    }

    let launch = crate::core::steam::arma3_launch_command(&cwd, &settings.steam_directory)
        .ok_or_else(|| {
            CommandError::validation(
                "launch",
                "Could not find the Steam launch command for this platform",
            )
        })?;
    let requires_existing_executable = cfg!(target_os = "windows");

    Ok(LaunchSpec {
        executable: launch.program,
        args: launch.args.into_iter().chain(args).collect(),
        cwd,
        requires_existing_executable,
    })
}

fn launch_working_directory(arma3_directory: &str) -> Result<PathBuf, CommandError> {
    #[cfg(target_os = "windows")]
    {
        Ok(PathBuf::from(arma3_directory))
    }

    #[cfg(not(target_os = "windows"))]
    {
        if !arma3_directory.trim().is_empty() {
            let configured = PathBuf::from(arma3_directory);
            if configured.is_dir() {
                return Ok(configured);
            }
        }

        std::env::current_dir().map_err(|err| {
            CommandError::operation(
                "launch",
                format!("Failed to resolve current working directory: {}", err),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::effective_repository;
    use super::{build_launch_spec, build_launch_spec_with_profiles};
    use crate::ui::types::{Repository, RepositoryProfile, SettingsViewState};
    use std::path::PathBuf;

    #[test]
    fn build_launch_spec_puts_creator_dlc_into_mod_argument() {
        let settings = SettingsViewState {
            arma3_directory: "C:\\Arma3".to_string(),
            ..SettingsViewState::default()
        };
        let repo = Repository {
            gm: true,
            ..Repository::default()
        };

        let launch_spec =
            build_launch_spec(&settings, &repo, None).expect("launch spec should build");

        assert!(launch_spec.args.iter().any(|arg| arg == "-mod=gm"));
        assert!(!launch_spec.args.iter().any(|arg| arg == "-gm"));
    }

    #[test]
    fn build_launch_spec_includes_basic_flags_and_preserves_quoted_additional_params() {
        let settings = SettingsViewState {
            arma3_directory: "C:\\Arma3".to_string(),
            arma3_profiles_directory: "D:\\Arma Profiles".to_string(),
            ..SettingsViewState::default()
        };
        let repo = Repository {
            arma3_profile: Some("Jane Doe".to_string()),
            skip_intro: true,
            no_logs: true,
            additional_params: r#""-profiles=C:\Arma 3 Profiles" -window"#.to_string(),
            ..Repository::default()
        };
        let detected_profiles = vec![crate::core::arma3_profiles::Arma3Profile {
            name: "Jane Doe".to_string(),
            path: PathBuf::from("D:\\Arma Profiles\\Users\\Jane%20Doe"),
            is_default: false,
        }];

        let launch_spec =
            build_launch_spec_with_profiles(&settings, &repo, None, &detected_profiles)
                .expect("launch spec should build");

        assert!(launch_spec.args.iter().any(|arg| arg == "-skipIntro"));
        assert!(launch_spec.args.iter().any(|arg| arg == "-noLogs"));
        assert!(
            launch_spec
                .args
                .iter()
                .any(|arg| arg == "-profiles=D:\\Arma Profiles")
        );
        assert!(launch_spec.args.iter().any(|arg| arg == "-name=Jane Doe"));
        assert!(
            launch_spec
                .args
                .iter()
                .any(|arg| arg == "-profiles=C:\\Arma 3 Profiles")
        );
        assert!(launch_spec.args.iter().any(|arg| arg == "-window"));
    }

    #[test]
    fn effective_repository_applies_selected_profile_launch_settings() {
        let repo = Repository {
            profiles: vec![RepositoryProfile {
                name: "Operations".to_string(),
                gm: true,
                skip_intro: true,
                no_logs: true,
                include_steam_addons: true,
                additional_params: "-window".to_string(),
                ..RepositoryProfile::default()
            }],
            selected_profile: Some("Operations".to_string()),
            ..Repository::default()
        };

        let effective = effective_repository(&repo);

        assert!(effective.gm);
        assert!(effective.skip_intro);
        assert!(effective.no_logs);
        assert!(effective.include_steam_addons);
        assert_eq!(effective.additional_params, "-window");
    }
}
