use super::{
    AppState, CommandError, CommandSuccess, ensure_backend_ready, find_repository_index,
    progress_output_muted, run_repository_sync,
};
use crate::cli::args::{
    CliArgs, RepoAddArgs, RepoCloneArgs, RepoCommand, RepoForceRedownloadArgs, RepoRemoveArgs,
    RepoSyncArgs, RepoSyncMode, RepoWipeDbArgs,
};
use crate::cli::exit_codes;
use crate::core::api::SyncMode;
use crate::core::tasks::purge_repository::{
    purge_repository_by_url, purge_repository_db_only_by_url,
};
use crate::ui::app::Foxy;
use crate::ui::types::{
    Repository, RepositoryServer, apply_repo_client_parameters,
    apply_repo_dlc_content_from_repo_json, sanitize_repository_paths, sanitize_user_path,
};
use reqwest::blocking::get;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

/// Normalize a local download folder for identity comparison, mirroring the GUI
/// `Foxy::normalize_repo_path_identity`: ignore separator style, trailing
/// slashes and (on Windows) case. A blank folder has no download location yet
/// and never collides.
fn normalize_folder_identity(path: &str) -> String {
    let mut normalized = path.trim().replace('\\', "/");
    while normalized.ends_with('/') {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized = normalized.to_ascii_lowercase();
    }
    normalized
}

pub fn run_repo_command(
    cli: &CliArgs,
    command: RepoCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        RepoCommand::List => cmd_repo_list(),
        RepoCommand::Add(args) => cmd_repo_add(cli, args),
        RepoCommand::Remove(args) => cmd_repo_remove(cli, args),
        RepoCommand::Clone(args) => cmd_repo_clone(cli, args),
        RepoCommand::Sync(args) => cmd_repo_sync(cli, args),
        RepoCommand::WipeDb(args) => cmd_repo_wipe_db(cli, args),
        RepoCommand::ForceRedownload(args) => cmd_repo_force_redownload(cli, args),
    }
}

fn cmd_repo_list() -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    Ok(CommandSuccess {
        action: "repo.list".to_string(),
        message: serde_json::to_string_pretty(&state.repositories)
            .unwrap_or_else(|_| "[]".to_string()),
        data: json!(state.repositories),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_repo_add(cli: &CliArgs, args: RepoAddArgs) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    let normalized_address = Foxy::normalize_repository_address_input(&args.address);
    if normalized_address.trim().is_empty() {
        return Err(CommandError::validation("repo.add", "Address is required"));
    }
    let normalized_with_trailing = Foxy::normalize_repo_url(&normalized_address);

    let mut repo = Repository {
        name: args
            .name
            .unwrap_or_else(|| default_repository_name_from_address(&normalized_address)),
        address: normalized_address,
        ..Repository::default()
    };
    if let Some(path) = args.path {
        repo.path = sanitize_user_path(&path.display().to_string());
    }
    sanitize_repository_paths(&mut repo);

    // Sharing of core database / pending-update state is bound to the local
    // download folder, not the remote URL: the same URL added to a different
    // folder is an independent install. Only reject an add that would collide
    // with an existing repository on both URL *and* folder (a blank folder has
    // no download location yet and never collides). CLI adds are standalone, so
    // there is no repository space to compare.
    let folder_key = normalize_folder_identity(&repo.path);
    if !folder_key.is_empty()
        && state.repositories.iter().any(|existing| {
            existing.repository_space_id.is_none()
                && Foxy::normalize_repo_url(&existing.address)
                    .eq_ignore_ascii_case(&normalized_with_trailing)
                && normalize_folder_identity(&existing.path) == folder_key
        })
    {
        return Err(CommandError::validation(
            "repo.add",
            "A repository with this URL already exists in the same folder",
        ));
    }
    let metadata_warning = populate_repo_from_remote_metadata(
        &mut repo,
        state.settings.apply_repo_json_client_parameters,
        state.settings.apply_repo_json_dlc_content,
    )
    .err();

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "repo.add".to_string(),
            message: "Dry-run: repository add previewed".to_string(),
            data: json!({"repository": repo, "metadata_warning": metadata_warning, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    state.repositories.push(repo.clone());
    state.save_repositories()?;
    Ok(CommandSuccess {
        action: "repo.add".to_string(),
        message: format!("Repository {} added", repo.name),
        data: json!({"repository": repo, "metadata_warning": metadata_warning}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_repo_remove(cli: &CliArgs, args: RepoRemoveArgs) -> Result<CommandSuccess, CommandError> {
    if !cli.yes {
        return Err(CommandError::validation(
            "repo.remove",
            "This operation is destructive. Re-run with --yes",
        ));
    }

    let mut state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let removed = state.repositories.remove(idx);

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "repo.remove".to_string(),
            message: "Dry-run: repository remove previewed".to_string(),
            data: json!({"removed": removed, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    state.save_repositories()?;
    Ok(CommandSuccess {
        action: "repo.remove".to_string(),
        message: format!("Repository {} removed", removed.name),
        data: json!({"removed": removed}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_repo_clone(cli: &CliArgs, args: RepoCloneArgs) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let suffix = args.suffix.trim();
    if suffix.is_empty() {
        return Err(CommandError::validation(
            "repo.clone",
            "Suffix must be non-empty",
        ));
    }

    let mut cloned = state.repositories[idx].clone();
    let base = cloned.name.trim().to_string();
    let mut candidate = format!("{} {}", base, suffix);
    let mut n = 2usize;
    while state
        .repositories
        .iter()
        .any(|repo| repo.name.eq_ignore_ascii_case(&candidate))
    {
        candidate = format!("{} {} {}", base, suffix, n);
        n += 1;
    }
    cloned.name = candidate;
    let insert_idx = idx + 1;

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "repo.clone".to_string(),
            message: "Dry-run: repository clone previewed".to_string(),
            data: json!({"source": state.repositories[idx], "cloned": cloned, "insert_index": insert_idx, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    state.repositories.insert(insert_idx, cloned.clone());
    state.save_repositories()?;
    Ok(CommandSuccess {
        action: "repo.clone".to_string(),
        message: format!("Repository cloned as {}", cloned.name),
        data: json!({"cloned": cloned, "insert_index": insert_idx}),
        exit_code: exit_codes::SUCCESS,
    })
}

pub fn cmd_repo_sync(cli: &CliArgs, args: RepoSyncArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let summary = run_repository_sync(
        &repo,
        &state.settings,
        repo_sync_mode_to_backend(args.mode),
        progress_output_muted(cli),
        cli.dry_run,
        false,
    )?;

    Ok(CommandSuccess {
        action: "repo.sync".to_string(),
        message: format!("Repository sync completed for {}", repo.name),
        data: summary,
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_repo_wipe_db(cli: &CliArgs, args: RepoWipeDbArgs) -> Result<CommandSuccess, CommandError> {
    if !cli.yes {
        return Err(CommandError::validation(
            "repo.wipe-db",
            "This operation is destructive. Re-run with --yes",
        ));
    }

    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let normalized = Foxy::normalize_repo_url(&repo.address);

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "repo.wipe-db".to_string(),
            message: "Dry-run: repo wipe-db previewed".to_string(),
            data: json!({"repository": repo.name, "repository_url": normalized, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    ensure_backend_ready();
    let runtime = Runtime::new()
        .map_err(|e| CommandError::operation("repo.wipe-db", format!("Runtime error: {}", e)))?;
    runtime
        .block_on(purge_repository_db_only_by_url(&normalized))
        .map_err(|e| CommandError::operation("repo.wipe-db", format!("Failed: {}", e)))?;

    Ok(CommandSuccess {
        action: "repo.wipe-db".to_string(),
        message: format!("Repository DB wiped for {}", repo.name),
        data: json!({"repository": repo.name, "repository_url": normalized}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_repo_force_redownload(
    cli: &CliArgs,
    args: RepoForceRedownloadArgs,
) -> Result<CommandSuccess, CommandError> {
    if !cli.yes {
        return Err(CommandError::validation(
            "repo.force-redownload",
            "This operation is destructive. Re-run with --yes",
        ));
    }

    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let normalized = Foxy::normalize_repo_url(&repo.address);
    let repo_path = if repo.path.trim().is_empty() {
        None
    } else {
        Some(repo.path.clone())
    };

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "repo.force-redownload".to_string(),
            message: "Dry-run: force-redownload previewed".to_string(),
            data: json!({"repository": repo.name, "repository_url": normalized, "repository_path": repo_path, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    ensure_backend_ready();
    let runtime = Runtime::new().map_err(|e| {
        CommandError::operation("repo.force-redownload", format!("Runtime error: {}", e))
    })?;
    runtime
        .block_on(purge_repository_by_url(&normalized, repo_path.as_deref()))
        .map_err(|e| {
            CommandError::operation(
                "repo.force-redownload",
                format!("Failed to purge repository: {}", e),
            )
        })?;

    let summary = run_repository_sync(
        &repo,
        &state.settings,
        SyncMode::Download,
        progress_output_muted(cli),
        false,
        true,
    )?;

    Ok(CommandSuccess {
        action: "repo.force-redownload".to_string(),
        message: format!("Force-redownload completed for {}", repo.name),
        data: summary,
        exit_code: exit_codes::SUCCESS,
    })
}

fn default_repository_name_from_address(address: &str) -> String {
    let trimmed = address.trim_end_matches('/');
    let candidate = trimmed.rsplit('/').next().unwrap_or_default().trim();
    if candidate.is_empty() {
        "New Repository".to_string()
    } else {
        candidate.to_string()
    }
}

fn populate_repo_from_remote_metadata(
    repo: &mut Repository,
    apply_client_parameters: bool,
    apply_dlc_content: bool,
) -> Result<(), String> {
    let repo_url = format!("{}/repo.json", repo.address.trim_end_matches('/'));
    let response = get(&repo_url).map_err(|e| format!("Failed to fetch {}: {}", repo_url, e))?;
    if !response.status().is_success() {
        return Err(format!(
            "Metadata endpoint returned HTTP {}",
            response.status()
        ));
    }
    let json = response
        .json::<Value>()
        .map_err(|e| format!("Failed to parse metadata: {}", e))?;

    if let Some(required_mods) = json.get("requiredMods").and_then(Value::as_array) {
        repo.addons = required_mods
            .iter()
            .filter_map(|m| {
                let name = m.get("modName").and_then(Value::as_str)?;
                let enabled = m.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                Some((name.to_string(), enabled))
            })
            .collect();
    }
    if let Some(optional_mods) = json.get("optionalMods").and_then(Value::as_array) {
        repo.optional_addons = optional_mods
            .iter()
            .filter_map(|m| {
                let name = m.get("modName").and_then(Value::as_str)?;
                let enabled = m.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                Some((name.to_string(), enabled))
            })
            .collect();
    }
    if let Some(servers) = json.get("servers").and_then(Value::as_array) {
        repo.servers = servers
            .iter()
            .filter_map(|server| {
                let name = server.get("name").and_then(Value::as_str)?;
                let address = server.get("address").and_then(Value::as_str)?;
                let port = server.get("port").and_then(Value::as_str)?;
                let password = server
                    .get("password")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let battle_eye = server
                    .get("battleEye")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Some(RepositoryServer {
                    name: name.to_string(),
                    address: address.to_string(),
                    port: port.to_string(),
                    password,
                    battle_eye,
                })
            })
            .collect();
    }

    if let Some(value) = json.get("iconImagePath").and_then(Value::as_str) {
        repo.icon_image_path = value.to_string();
    }
    if let Some(value) = json.get("iconImageChecksum").and_then(Value::as_str) {
        repo.icon_image_checksum = value.to_string();
    }
    if let Some(value) = json.get("repoImagePath").and_then(Value::as_str) {
        repo.repo_image_path = value.to_string();
    }
    if let Some(value) = json.get("repoImageChecksum").and_then(Value::as_str) {
        repo.repo_image_checksum = value.to_string();
    }
    repo.app_update_url = json
        .get("appUpdateUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();
    if let Some(value) = json.get("repoName").and_then(Value::as_str) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            repo.name = trimmed.to_string();
        }
    }
    if apply_client_parameters
        && let Some(value) = json.get("clientParameters").and_then(Value::as_str)
    {
        apply_repo_client_parameters(repo, value);
    }
    if apply_dlc_content && let Some(value) = json.get("dlcContent") {
        apply_repo_dlc_content_from_repo_json(repo, value);
    }

    Ok(())
}

fn repo_sync_mode_to_backend(mode: RepoSyncMode) -> SyncMode {
    match mode {
        RepoSyncMode::RemoteRefresh => SyncMode::RemoteRefreshOnly,
        RepoSyncMode::QuickCheck => SyncMode::QuickCheckOnly,
        RepoSyncMode::Recheck => SyncMode::RecheckOnly,
        RepoSyncMode::RecheckIntegrity => SyncMode::RecheckIntegrity,
        RepoSyncMode::Download => SyncMode::Download,
    }
}
