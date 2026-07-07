mod addon;
mod agent_gui;
mod config;
mod direct_download;
mod game;
mod launch;
mod profile;
mod repo;
mod server;
mod settings;
mod space;
mod steam_helper;
mod workshop;

use crate::cli::args::{CliArgs, CliCommand};
use crate::cli::exit_codes;
use crate::core::api::{self, ModDiffSummary, ProgressEvent, SyncMode};
use crate::ui::app::Foxy;
use crate::ui::i18n::{migrate_locale_preference, sanitize_locale_preference};
use crate::ui::types::{
    Repository, RepositorySpace, SettingsViewState, normalize_loaded_repositories,
    sanitize_repository_paths, sanitize_repository_spaces_paths, sanitize_settings_paths,
    sanitize_user_path,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::TryRecvError as BroadcastTryRecvError;
use tokio::sync::{broadcast, watch};

use self::addon::run_addon_command;
use self::agent_gui::run_agent_gui_command;
use self::config::run_config_command;
use self::direct_download::cmd_direct_download;
use self::game::run_game_command;
use self::launch::cmd_launch;
use self::profile::run_profile_command;
use self::repo::run_repo_command;
use self::server::run_server_command;
use self::settings::run_settings_command;
use self::space::run_space_command;
use self::steam_helper::run_steam_helper_command;
use self::workshop::run_workshop_command;

#[derive(Clone, Debug)]
pub struct CommandSuccess {
    pub action: String,
    pub message: String,
    pub data: Value,
    pub exit_code: i32,
}

#[derive(Clone, Debug)]
pub struct CommandError {
    pub action: String,
    pub message: String,
    pub code: i32,
}

impl CommandError {
    fn validation(action: &str, message: impl Into<String>) -> Self {
        Self {
            action: action.to_string(),
            message: message.into(),
            code: exit_codes::VALIDATION_ERROR,
        }
    }

    fn not_found(action: &str, message: impl Into<String>) -> Self {
        Self {
            action: action.to_string(),
            message: message.into(),
            code: exit_codes::NOT_FOUND,
        }
    }

    fn operation(action: &str, message: impl Into<String>) -> Self {
        Self {
            action: action.to_string(),
            message: message.into(),
            code: exit_codes::OPERATION_FAILED,
        }
    }

    fn partial(action: &str, message: impl Into<String>) -> Self {
        Self {
            action: action.to_string(),
            message: message.into(),
            code: exit_codes::PARTIAL_SUCCESS,
        }
    }
}

struct AppState {
    settings: SettingsViewState,
    repositories: Vec<Repository>,
    spaces: Vec<RepositorySpace>,
}

impl AppState {
    fn load() -> Result<Self, CommandError> {
        let merged = crate::core::game::spaces::read_merged_settings_value(
            &Foxy::get_app_settings_path(),
            &Foxy::get_game_settings_path(),
        )
        .map_err(|e| CommandError::operation("settings.load", e))?;
        let mut settings: SettingsViewState = match merged {
            Some(value) => {
                let defaults = serde_json::to_value(SettingsViewState::default()).map_err(|e| {
                    CommandError::operation(
                        "settings.load",
                        format!("Failed to serialize default settings: {}", e),
                    )
                })?;
                let value = crate::core::game::spaces::merge_value_over_defaults(defaults, value);
                serde_json::from_value(value).map_err(|e| {
                    CommandError::operation(
                        "settings.load",
                        format!("Failed to parse settings: {}", e),
                    )
                })?
            }
            None => SettingsViewState::default(),
        };
        sanitize_settings(&mut settings);

        let mut repositories: Vec<Repository> =
            read_json_or_default(&Foxy::get_repositories_path())
                .map_err(|e| CommandError::operation("repo.load", e))?;
        normalize_loaded_repositories(&mut repositories);

        let spaces: Vec<RepositorySpace> =
            read_json_or_default(&Foxy::get_repository_spaces_path())
                .map_err(|e| CommandError::operation("space.load", e))?;
        let mut spaces = spaces;
        sanitize_repository_spaces_paths(&mut spaces);

        Ok(Self {
            settings,
            repositories,
            spaces,
        })
    }

    fn save_settings(&self) -> Result<(), CommandError> {
        let mut settings = self.settings.clone();
        sanitize_settings(&mut settings);
        let value = serde_json::to_value(&settings).map_err(|e| {
            CommandError::operation(
                "settings.save",
                format!("Failed to serialize settings: {}", e),
            )
        })?;
        crate::core::game::spaces::write_split_settings(
            &value,
            &Foxy::get_app_settings_path(),
            &Foxy::get_game_settings_path(),
        )
        .map_err(|e| CommandError::operation("settings.save", e))
    }

    fn save_repositories(&self) -> Result<(), CommandError> {
        let repositories = repositories_for_save(&self.repositories);
        write_json_pretty(&Foxy::get_repositories_path(), &repositories)
            .map_err(|e| CommandError::operation("repo.save", e))
    }

    fn save_spaces(&self) -> Result<(), CommandError> {
        let mut spaces = self.spaces.clone();
        sanitize_repository_spaces_paths(&mut spaces);
        write_json_pretty(&Foxy::get_repository_spaces_path(), &spaces)
            .map_err(|e| CommandError::operation("space.save", e))
    }
}

fn progress_output_muted(cli: &CliArgs) -> bool {
    cli.quiet || cli.json || cli.no_progress
}

pub fn apply_config_override(config_dir: Option<&PathBuf>) -> Result<(), CommandError> {
    if let Some(path) = config_dir {
        let raw = sanitize_user_path(&path.display().to_string());
        if raw.trim().is_empty() {
            return Err(CommandError::validation(
                "config",
                "--config-dir must be non-empty",
            ));
        }
        unsafe {
            std::env::set_var("FOXY_CONFIG_DIR", raw);
        }
    }
    Ok(())
}

fn ensure_backend_ready() {
    crate::core::tasks::init_database::check_and_wipe_database();
}

pub fn run_command(cli: &CliArgs, command: CliCommand) -> Result<CommandSuccess, CommandError> {
    crate::core::game::spaces::ensure_game_spaces_layout();
    let started = Instant::now();
    let result = match command {
        CliCommand::Version => Ok(CommandSuccess {
            action: "version".to_string(),
            message: crate::build_info::version_label(),
            data: json!({
                "version": crate::build_info::VERSION,
                "build_kind": crate::build_info::build_kind(),
                "commit": crate::build_info::GIT_HASH,
                "official": crate::build_info::is_official_build(),
            }),
            exit_code: exit_codes::SUCCESS,
        }),
        CliCommand::AgentGui { command } => run_agent_gui_command(cli, command),
        CliCommand::Settings { command } => run_settings_command(cli, *command),
        CliCommand::Repo { command } => run_repo_command(cli, command),
        CliCommand::Sync(args) => repo::cmd_repo_sync(cli, args),
        CliCommand::Addon { command } => run_addon_command(cli, command),
        CliCommand::Profile { command } => run_profile_command(cli, command),
        CliCommand::Space { command } => run_space_command(cli, command),
        CliCommand::Game { command } => run_game_command(cli, command),
        CliCommand::Config { command } => run_config_command(cli, command),
        CliCommand::Workshop { command } => run_workshop_command(cli, command),
        CliCommand::SteamHelper { command } => run_steam_helper_command(command),
        CliCommand::Server { command } => run_server_command(cli, command),
        CliCommand::DirectDownload(args) => cmd_direct_download(cli, args),
        CliCommand::Launch(args) => cmd_launch(cli, args),
        CliCommand::Ui(_) => Err(CommandError::validation("ui", "handled by caller")),
    };

    match &result {
        Ok(success) => log::info!(
            "CLI command finished: action={} elapsed={:.2?} exit_code={}",
            success.action,
            started.elapsed(),
            success.exit_code
        ),
        Err(error) => log::warn!(
            "CLI command failed: action={} elapsed={:.2?} exit_code={}",
            error.action,
            started.elapsed(),
            error.code
        ),
    }

    result
}

fn read_json_or_default<T>(path: &Path) -> Result<T, String>
where
    T: DeserializeOwned + Default,
{
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str::<T>(&content)
            .map_err(|e| format!("Failed to parse {}: {}", path.display(), e)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(format!("Failed to read {}: {}", path.display(), err)),
    }
}

fn write_json_pretty<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: serde::Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    let content = serde_json::to_string_pretty(value)
        .map_err(|e| format!("Failed to serialize {}: {}", path.display(), e))?;
    fs::write(path, content).map_err(|e| format!("Failed to write {}: {}", path.display(), e))
}

fn sanitize_settings(settings: &mut SettingsViewState) {
    (settings.locale, settings.locale_preference_migrated) =
        migrate_locale_preference(&settings.locale, settings.locale_preference_migrated);
    if settings.download_speed_limit_mbps == Some(0) {
        settings.download_speed_limit_mbps = Some(1);
    }
    settings.app_update_url = settings.app_update_url.trim().to_string();
    if settings.app_update_url.is_empty() {
        settings.app_update_url_user_override = false;
    }
    for notice in &mut settings.update_summary_notices {
        notice.repository_url = Foxy::normalize_repo_url(&notice.repository_url);
        if notice.pending_ack_count == 0 {
            notice.pending_ack_count = 1;
        }
    }
    settings.additional_folders_filter.clear();
    settings.cleanup_folders_filter.clear();
    settings.font_sizes.clamp_to_limits();
    settings.locale = sanitize_locale_preference(&settings.locale);
    settings.locale_preference_migrated = true;
    sanitize_settings_paths(settings);
}

fn repositories_for_save(repositories: &[Repository]) -> Vec<Repository> {
    let mut out = Vec::with_capacity(repositories.len());
    for repo in repositories {
        let mut cloned = repo.clone();
        sanitize_repository_paths(&mut cloned);
        cloned.addons = cloned.addons.into_iter().collect();
        cloned.optional_addons = cloned.optional_addons.into_iter().collect();
        for profile in &mut cloned.profiles {
            profile.addons.retain(|(_, enabled)| !*enabled);
            profile.optional_addons.retain(|(_, enabled)| *enabled);
        }
        out.push(cloned);
    }
    out
}

fn find_repository_index(
    repositories: &[Repository],
    selector: &crate::cli::args::RepoSelectorArgs,
) -> Result<usize, CommandError> {
    if let Some(name) = selector.repo_name.as_deref() {
        let mut matches = repositories
            .iter()
            .enumerate()
            .filter(|(_, repo)| repo.name.eq_ignore_ascii_case(name))
            .map(|(idx, _)| idx);
        let Some(first) = matches.next() else {
            return Err(CommandError::not_found(
                "repo.selector",
                format!("Repository with name {} not found", name),
            ));
        };
        if matches.next().is_some() {
            return Err(CommandError::validation(
                "repo.selector",
                format!("Repository name {} is ambiguous", name),
            ));
        }
        return Ok(first);
    }

    if let Some(url) = selector.repo_url.as_deref() {
        let normalized = Foxy::normalize_repo_url(&Foxy::normalize_repository_address_input(url));
        return repositories
            .iter()
            .enumerate()
            .find(|(_, repo)| {
                Foxy::normalize_repo_url(&repo.address).eq_ignore_ascii_case(&normalized)
            })
            .map(|(idx, _)| idx)
            .ok_or_else(|| {
                CommandError::not_found(
                    "repo.selector",
                    format!("Repository with URL {} not found", url),
                )
            });
    }

    Err(CommandError::validation(
        "repo.selector",
        "Provide --repo-name or --repo-url",
    ))
}

fn effective_repository(repo: &Repository) -> Repository {
    let mut effective = repo.clone();
    if let Some(profile_name) = repo.selected_profile.as_deref()
        && let Some(profile) = repo.profiles.iter().find(|p| p.name == profile_name)
    {
        Foxy::apply_profile_to_repository(&mut effective, profile);
    }
    effective
}

fn run_repository_sync(
    repo: &Repository,
    settings: &SettingsViewState,
    mode: SyncMode,
    quiet: bool,
    dry_run: bool,
    force_redownload: bool,
) -> Result<Value, CommandError> {
    if repo.address.trim().is_empty() {
        return Err(CommandError::validation(
            "repo.sync",
            format!("Repository {} has no address", repo.name),
        ));
    }
    if repo.path.trim().is_empty() {
        return Err(CommandError::validation(
            "repo.sync",
            format!("Repository {} has no local path", repo.name),
        ));
    }

    let effective = effective_repository(repo);
    let selected_mod_states: Vec<(String, bool)> = effective
        .addons
        .iter()
        .chain(effective.optional_addons.iter())
        .map(|(name, enabled)| (name.clone(), *enabled))
        .collect();

    if dry_run {
        return Ok(json!({
            "repository": repo.name,
            "repository_url": repo.address,
            "repository_path": repo.path,
            "mode": format!("{:?}", mode),
            "selected_mod_states": selected_mod_states,
            "dry_run": true
        }));
    }

    ensure_backend_ready();
    let (tx, mut rx) = broadcast::channel(512);
    let (_pause_tx, pause_rx) = watch::channel(false);
    let worker = api::spawn_repository_sync(
        repo.address.clone(),
        repo.path.clone(),
        selected_mod_states,
        tx,
        mode,
        api::RepositorySyncOptions {
            operation_id: api::next_operation_id("cli-sync"),
            prepare_download_plan: false,
            repository_space_shared_path: None,
            auto_backup_directory: None,
            rollback_temp_directory: Some(if settings.temp_directory.trim().is_empty() {
                crate::core::utils::app_paths::foxy_data_dir()
                    .display()
                    .to_string()
            } else {
                settings.temp_directory.trim().to_string()
            }),
            download_speed_limit_mbps: settings
                .download_speed_limit_mbps
                .filter(|limit| *limit > 0),
            recent_local_path_reset: false,
            force_redownload,
            allow_suspect_full_redownload: force_redownload,
            download_pause_rx: pause_rx,
            cancel_rx: watch::channel(false).1,
            hash_algorithm_preference: repo.hash_algorithm_preference,
            hash_io_profile: settings.hash_io_profile,
        },
        None,
    );

    let started_at = Instant::now();
    let mut latest_stage: Option<String> = None;
    let mut latest_percent: Option<f32> = None;
    let mut latest_diff: Vec<ModDiffSummary> = Vec::new();
    let mut failed: Option<String> = None;
    let mut finished = false;
    let mut last_print = Instant::now();

    loop {
        match rx.try_recv() {
            Ok(event) => match event {
                ProgressEvent::Stage { label, percent } => {
                    latest_stage = Some(label.clone());
                    latest_percent = Some(percent);
                    if !quiet && last_print.elapsed() >= Duration::from_millis(300) {
                        println!("{} [{:.0}%]", label, percent * 100.0);
                        last_print = Instant::now();
                    }
                }
                ProgressEvent::Diff { mods } => {
                    latest_diff = mods;
                }
                ProgressEvent::DownloadPlan { .. } => {}
                ProgressEvent::DownloadTelemetry { .. } => {}
                ProgressEvent::HashTelemetry { .. } => {}
                ProgressEvent::HashSummary { .. } => {}
                ProgressEvent::SiblingPropagation { .. } => {}
                ProgressEvent::DownloadMod {
                    mod_name,
                    files_done,
                    files_total,
                    bytes_done,
                    bytes_total,
                    percent,
                } => {
                    if !quiet && last_print.elapsed() >= Duration::from_millis(300) {
                        println!(
                            "{}: {}/{} files, {}/{} bytes ({:.0}%)",
                            mod_name,
                            files_done,
                            files_total,
                            bytes_done,
                            bytes_total,
                            percent * 100.0
                        );
                        last_print = Instant::now();
                    }
                }
                ProgressEvent::RecheckHashProgress {
                    checked_files,
                    total_files,
                    checked_parts,
                    total_parts,
                } => {
                    if !quiet && last_print.elapsed() >= Duration::from_millis(300) {
                        if total_parts > 0 {
                            println!(
                                "Hashing files: {}/{} files, {}/{} parts",
                                checked_files, total_files, checked_parts, total_parts
                            );
                        } else {
                            println!("Hashing files: {}/{}", checked_files, total_files);
                        }
                        last_print = Instant::now();
                    }
                }
                ProgressEvent::RepositoryFoxyMode { .. } => {}
                ProgressEvent::Finished => finished = true,
                ProgressEvent::Cancelled => {
                    failed = Some("Cancelled".to_string());
                    finished = true;
                }
                ProgressEvent::Failed(message) => {
                    failed = Some(message);
                    finished = true;
                }
            },
            Err(BroadcastTryRecvError::Empty) => {
                if worker.is_finished() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(BroadcastTryRecvError::Closed) => {
                if worker.is_finished() {
                    break;
                }
            }
            Err(BroadcastTryRecvError::Lagged(_)) => continue,
        }

        if finished && worker.is_finished() {
            break;
        }
    }
    let _ = worker.join();

    if let Some(message) = failed {
        return Err(CommandError::operation(
            "repo.sync",
            format!("Repository sync failed: {}", message),
        ));
    }

    Ok(json!({
        "repository": repo.name,
        "repository_url": Foxy::normalize_repo_url(&repo.address),
        "repository_path": repo.path,
        "mode": format!("{:?}", mode),
        "latest_stage": latest_stage,
        "latest_percent": latest_percent,
        "updates_count": latest_diff.iter().filter(|m| m.needs_update).count(),
        "diff": latest_diff,
        "elapsed_ms": started_at.elapsed().as_millis()
    }))
}
