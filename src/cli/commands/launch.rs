use super::game::launch_error_to_command_error_for;
use super::{AppState, CommandError, CommandSuccess, effective_repository, find_repository_index};
use crate::cli::args::{CliArgs, LaunchArgs};
use crate::cli::exit_codes;
use crate::core::game::{GameLaunchCtx, GameModule, LaunchCommand};
use crate::core::steam::{SteamEnsureResult, ensure_steam_running};
use crate::ui::types::{Repository, RepositoryServer, SettingsViewState};
use serde_json::json;
use std::path::PathBuf;

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

    let launch_spec = build_launch_spec(
        module,
        &state.settings,
        &effective,
        selected_server.as_ref(),
    )?;

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
                "{} executable not found: {}",
                module.display_name(),
                launch_spec.executable.display()
            ),
        ));
    }

    let extra_file_activation = crate::core::game::extra_files::activate_for_launch(
        &crate::core::game::spaces::active_game_space_dir(),
        module.install_dir_from_settings(&state.settings),
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

    let args: Vec<std::ffi::OsString> = launch_spec
        .args
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
    let pid = crate::core::utils::deelevate::spawn_unelevated(
        launch_spec.executable.as_os_str(),
        &args,
        Some(launch_spec.cwd.as_ref()),
    )
    .map_err(|e| CommandError::operation("launch", format!("Failed to launch: {}", e)))?;

    let message = match steam_status {
        "started" => format!("Steam started and launch command started (pid={})", pid),
        "not_configured_fallback" => format!(
            "Launch command started without Steam pre-check (pid={})",
            pid
        ),
        _ => format!("Launch command started (pid={})", pid),
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
            "pid": pid
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

/// Build the launch command through the active game module so the CLI and the
/// GUI share one plan per game instead of a second Arma-shaped copy here.
fn build_launch_spec(
    module: &dyn GameModule,
    settings: &SettingsViewState,
    repo: &Repository,
    server: Option<&RepositoryServer>,
) -> Result<LaunchSpec, CommandError> {
    let install_dir = module
        .install_dir_from_settings(settings)
        .trim()
        .to_string();
    let to_error = |err| launch_error_to_command_error_for("launch", module.display_name(), err);
    let plan = module
        .build_repository_launch_plan(settings, repo, server)
        .map_err(to_error)?;
    let ctx = GameLaunchCtx {
        install_dir: &install_dir,
        steam_directory: &settings.steam_directory,
        settings: Some(settings),
    };
    let command: LaunchCommand = module.build_launch(&plan, &ctx).map_err(to_error)?;
    let cwd = match command.cwd {
        Some(cwd) => cwd,
        None => launch_working_directory(&install_dir)?,
    };

    Ok(LaunchSpec {
        executable: command.program,
        args: command.args,
        cwd,
        requires_existing_executable: cfg!(target_os = "windows"),
    })
}

fn launch_working_directory(install_dir: &str) -> Result<PathBuf, CommandError> {
    #[cfg(target_os = "windows")]
    {
        Ok(PathBuf::from(install_dir))
    }

    #[cfg(not(target_os = "windows"))]
    {
        if !install_dir.trim().is_empty() {
            let configured = PathBuf::from(install_dir);
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
    use super::build_launch_spec;
    use crate::core::game::arma3::Arma3Module;
    use crate::ui::types::{Repository, RepositoryProfile, SettingsViewState};

    fn valid_arma3_install() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("arma3_x64.exe"), "").expect("exe marker");
        dir
    }

    #[test]
    fn build_launch_spec_puts_creator_dlc_into_mod_argument() {
        let install = valid_arma3_install();
        let settings = SettingsViewState {
            arma3_directory: install.path().to_string_lossy().to_string(),
            ..SettingsViewState::default()
        };
        let repo = Repository {
            gm: true,
            ..Repository::default()
        };

        let launch_spec = build_launch_spec(&Arma3Module, &settings, &repo, None)
            .expect("launch spec should build");

        assert!(launch_spec.args.iter().any(|arg| arg == "-mod=gm"));
        assert!(!launch_spec.args.iter().any(|arg| arg == "-gm"));
    }

    #[test]
    fn build_launch_spec_includes_basic_flags_and_preserves_quoted_additional_params() {
        let install = valid_arma3_install();
        let settings = SettingsViewState {
            arma3_directory: install.path().to_string_lossy().to_string(),
            arma3_profiles_directory: "D:\\Arma Profiles".to_string(),
            ..SettingsViewState::default()
        };
        let repo = Repository {
            skip_intro: true,
            no_logs: true,
            additional_params: r#""-profiles=C:\Arma 3 Profiles" -window"#.to_string(),
            ..Repository::default()
        };

        let launch_spec = build_launch_spec(&Arma3Module, &settings, &repo, None)
            .expect("launch spec should build");

        assert!(launch_spec.args.iter().any(|arg| arg == "-skipIntro"));
        assert!(launch_spec.args.iter().any(|arg| arg == "-noLogs"));
        assert!(
            launch_spec
                .args
                .iter()
                .any(|arg| arg == "-profiles=D:\\Arma Profiles")
        );
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
