use super::game::launch_error_to_command_error_for;
use super::{AppState, CommandError, CommandSuccess, effective_repository, find_repository_index};
use crate::cli::args::{CliArgs, LaunchArgs};
use crate::cli::exit_codes;
use crate::core::game::{
    GameLaunchCtx, GameModule, LaunchCommand,
    launch_preparation::{LaunchApproval, LaunchFailure},
};
use crate::ui::types::{Repository, RepositoryServer, SettingsViewState};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

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
                "extra_file_preparation": super::extra_file_launch_preview("launch", &state.settings)?,
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

    let approval = LaunchApproval {
        incomplete_extra_files: args.allow_incomplete_preparation,
        live_workshop_fallback: args.allow_live_fallback,
        extra_file_conflicts: super::extra_file_launch_choices(
            "launch",
            &state.settings,
            &args.keep_extra_file,
            &args.reapply_extra_file,
            args.approve_extra_file_layers,
            cli.yes,
        )?,
        approve_extra_file_layers: super::approved_extra_file_layers(
            "launch",
            &state.settings,
            args.approve_extra_file_layers,
        )?,
    };
    let cancelled = AtomicBool::new(false);
    let handoff = crate::core::game::launch_preparation::dispatch_repository(
        module,
        &state.settings,
        &effective,
        selected_server.as_ref(),
        approval,
        None,
        &cancelled,
    )
    .map_err(|failure| launch_failure_to_command_error("launch", failure))?;
    let pid = handoff.pid;
    let command = handoff.command;
    let steam_status = match handoff.steam_status {
        Some(crate::core::steam::SteamEnsureResult::AlreadyRunning) => "already_running",
        Some(crate::core::steam::SteamEnsureResult::Started) => "started",
        Some(crate::core::steam::SteamEnsureResult::SkippedMissingDirectory) => {
            "not_configured_fallback"
        }
        None => "not_needed",
    };
    let message = format!("Launch command started (pid={})", pid);

    Ok(CommandSuccess {
        action: "launch".to_string(),
        message,
        data: json!({
            "repository": repo.name,
            "selected_profile": repo.selected_profile,
            "executable": command.program.display().to_string(),
            "args": command.args,
            "cwd": command.cwd.as_ref().unwrap_or(&launch_spec.cwd).display().to_string(),
            "steam_status": steam_status,
            "extra_files": {
                "activated": handoff.extra_files.activated.len(),
                "failed": handoff.extra_files.failed.len(),
                "skipped_disabled": handoff.extra_files.skipped_disabled
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
    let command: LaunchCommand = module.build_launch_preview(&plan, &ctx).map_err(to_error)?;
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

fn launch_failure_to_command_error(action: &str, failure: LaunchFailure) -> CommandError {
    match failure {
        LaunchFailure::ExtraFileConflicts(conflicts) => CommandError::validation(
            action,
            format!(
                "Extra files changed: {}. Choose --keep-extra-file <id> or --reapply-extra-file <id> --yes.",
                conflicts
                    .iter()
                    .map(|entry| entry.entry_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        LaunchFailure::ExtraFileLayers(_) => CommandError::validation(
            action,
            "Review extra-file layer replacements with config extra-file activate --dry-run, then pass --approve-extra-file-layers --yes",
        ),
        LaunchFailure::ExtraFiles(failures) => CommandError::validation(
            action,
            format!(
                "Extra-file preparation failed ({} issue(s)); pass --allow-incomplete-preparation to continue: {}",
                failures.len(),
                failures.join("; ")
            ),
        ),
        LaunchFailure::FrozenCopies(missing) => CommandError::validation(
            action,
            format!(
                "Frozen Workshop copies are missing for: {}; pass --allow-live-fallback to use live Workshop content",
                missing.join(", ")
            ),
        ),
        LaunchFailure::Failed(error) | LaunchFailure::Recovery(error) => {
            CommandError::operation(action, error)
        }
    }
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
