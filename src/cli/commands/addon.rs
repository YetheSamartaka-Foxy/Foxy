use super::{
    AppState, CommandError, CommandSuccess, effective_repository, ensure_backend_ready,
    find_repository_index, progress_output_muted, run_repository_sync,
};
use crate::cli::args::{
    AddonCommand, AddonForceRedownloadArgs, AddonListArgs, AddonRecalcHashesArgs, AddonSetArgs,
    CliArgs,
};
use crate::cli::exit_codes;
use crate::core::api::{self, SyncMode};
use crate::core::utils::fs_safety::resolve_child_dir_case_insensitive;
use crate::ui::app::Foxy;
use crate::ui::types::{Repository, RepositoryProfile};
use serde_json::json;
use std::fs;
use std::path::Path;
use tokio::runtime::Runtime;

pub fn run_addon_command(
    cli: &CliArgs,
    command: AddonCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        AddonCommand::List(args) => cmd_addon_list(args),
        AddonCommand::Set(args) => cmd_addon_set(cli, args),
        AddonCommand::RecalcHashes(args) => cmd_addon_recalc_hashes(cli, args),
        AddonCommand::ForceRedownload(args) => cmd_addon_force_redownload(cli, args),
    }
}

fn cmd_addon_list(args: AddonListArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let effective = effective_repository(&repo);

    let mut addons = Vec::new();
    for (name, enabled) in &effective.addons {
        addons.push(json!({"name": name, "enabled": enabled, "kind": "required"}));
    }
    for (name, enabled) in &effective.optional_addons {
        addons.push(json!({"name": name, "enabled": enabled, "kind": "optional"}));
    }
    for (name, enabled, path) in &effective.external_addons {
        addons.push(json!({"name": name, "enabled": enabled, "kind": "external", "path": path}));
    }

    Ok(CommandSuccess {
        action: "addon.list".to_string(),
        message: serde_json::to_string_pretty(&addons).unwrap_or_else(|_| "[]".to_string()),
        data: json!({"repository": repo.name, "selected_profile": repo.selected_profile, "addons": addons}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_addon_set(cli: &CliArgs, args: AddonSetArgs) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let addon_name = args.addon.trim();
    if addon_name.is_empty() {
        return Err(CommandError::validation(
            "addon.set",
            "Addon name must be non-empty",
        ));
    }

    let repo_name = {
        let repo = state
            .repositories
            .get_mut(idx)
            .ok_or_else(|| CommandError::not_found("addon.set", "Repository not found"))?;

        let changed = if let Some(selected) = repo.selected_profile.clone() {
            let profile = repo
                .profiles
                .iter_mut()
                .find(|p| p.name == selected)
                .ok_or_else(|| {
                    CommandError::not_found("addon.set", "Selected profile not found")
                })?;
            set_addon_enabled_profile(profile, addon_name, args.enabled)
        } else {
            set_addon_enabled_repo(repo, addon_name, args.enabled)
        };

        if !changed {
            return Err(CommandError::not_found(
                "addon.set",
                format!("Addon {} not found", addon_name),
            ));
        }

        let repo_name = repo.name.clone();
        if cli.dry_run {
            return Ok(CommandSuccess {
                action: "addon.set".to_string(),
                message: "Dry-run: addon set previewed".to_string(),
                data: json!({"repository": repo_name, "addon": addon_name, "enabled": args.enabled, "dry_run": true}),
                exit_code: exit_codes::SUCCESS,
            });
        }
        repo_name
    };

    state.save_repositories()?;
    Ok(CommandSuccess {
        action: "addon.set".to_string(),
        message: format!("Addon {} set to {}", addon_name, args.enabled),
        data: json!({"repository": repo_name, "addon": addon_name, "enabled": args.enabled}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_addon_recalc_hashes(
    cli: &CliArgs,
    args: AddonRecalcHashesArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let addon_name = args.addon.trim();
    if addon_name.is_empty() {
        return Err(CommandError::validation(
            "addon.recalc-hashes",
            "Addon name must be non-empty",
        ));
    }

    let normalized = Foxy::normalize_repo_url(&repo.address);

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "addon.recalc-hashes".to_string(),
            message: "Dry-run: addon recalc-hashes previewed".to_string(),
            data: json!({"repository": repo.name, "repository_url": normalized, "addon": addon_name, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    ensure_backend_ready();
    let runtime = Runtime::new().map_err(|e| {
        CommandError::operation("addon.recalc-hashes", format!("Runtime error: {}", e))
    })?;
    let recalculated = runtime
        .block_on(api::recalculate_hashes_for_addon_by_name(
            &normalized,
            addon_name,
        ))
        .map_err(|e| {
            CommandError::operation(
                "addon.recalc-hashes",
                format!("Failed to recalculate hashes: {}", e),
            )
        })?;

    if !recalculated {
        return Err(CommandError::not_found(
            "addon.recalc-hashes",
            format!("Addon {} not found", addon_name),
        ));
    }

    let summary = run_repository_sync(
        &repo,
        &state.settings,
        SyncMode::RecheckOnly,
        progress_output_muted(cli),
        false,
        false,
    )?;

    Ok(CommandSuccess {
        action: "addon.recalc-hashes".to_string(),
        message: format!("Addon hash recalculation completed for {}", addon_name),
        data: summary,
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_addon_force_redownload(
    cli: &CliArgs,
    args: AddonForceRedownloadArgs,
) -> Result<CommandSuccess, CommandError> {
    if !cli.yes {
        return Err(CommandError::validation(
            "addon.force-redownload",
            "This operation is destructive. Re-run with --yes",
        ));
    }

    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();
    let addon_name = args.addon.trim();
    if addon_name.is_empty() {
        return Err(CommandError::validation(
            "addon.force-redownload",
            "Addon name must be non-empty",
        ));
    }
    if repo.path.trim().is_empty() {
        return Err(CommandError::validation(
            "addon.force-redownload",
            "Repository has no local path",
        ));
    }

    // Resolve tolerant of case differences so a folder downloaded with a
    // different case (e.g. `@crows_electronic_warfare` vs manifest
    // `@Crows_Electronic_Warfare`) is the one removed, not left as a duplicate.
    let target = resolve_child_dir_case_insensitive(Path::new(repo.path.trim()), addon_name)
        .unwrap_or_else(|| Path::new(repo.path.trim()).join(addon_name));
    if !is_safe_addon_path(repo.path.trim(), &target) {
        return Err(CommandError::validation(
            "addon.force-redownload",
            "Resolved addon path is outside repository root",
        ));
    }

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "addon.force-redownload".to_string(),
            message: "Dry-run: addon force-redownload previewed".to_string(),
            data: json!({"repository": repo.name, "addon": addon_name, "target_path": target.display().to_string(), "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    if target.exists() {
        if !target.is_dir() {
            return Err(CommandError::validation(
                "addon.force-redownload",
                "Target addon path is not a directory",
            ));
        }
        fs::remove_dir_all(&target).map_err(|e| {
            CommandError::operation(
                "addon.force-redownload",
                format!("Failed to remove addon directory: {}", e),
            )
        })?;
    }

    let summary = run_repository_sync(
        &repo,
        &state.settings,
        SyncMode::RecheckOnly,
        progress_output_muted(cli),
        false,
        false,
    )?;

    Ok(CommandSuccess {
        action: "addon.force-redownload".to_string(),
        message: format!("Addon force-redownload completed for {}", addon_name),
        data: summary,
        exit_code: exit_codes::SUCCESS,
    })
}

fn set_addon_enabled_profile(
    profile: &mut RepositoryProfile,
    addon_name: &str,
    enabled: bool,
) -> bool {
    if let Some(entry) = profile
        .addons
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(addon_name))
    {
        entry.1 = enabled;
        return true;
    }
    if let Some(entry) = profile
        .optional_addons
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(addon_name))
    {
        entry.1 = enabled;
        return true;
    }
    if let Some(entry) = profile
        .external_addons
        .iter_mut()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(addon_name))
    {
        entry.1 = enabled;
        return true;
    }
    false
}

fn set_addon_enabled_repo(repo: &mut Repository, addon_name: &str, enabled: bool) -> bool {
    if let Some(entry) = repo
        .addons
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(addon_name))
    {
        entry.1 = enabled;
        return true;
    }
    if let Some(entry) = repo
        .optional_addons
        .iter_mut()
        .find(|(name, _)| name.eq_ignore_ascii_case(addon_name))
    {
        entry.1 = enabled;
        return true;
    }
    if let Some(entry) = repo
        .external_addons
        .iter_mut()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(addon_name))
    {
        entry.1 = enabled;
        return true;
    }
    false
}

fn normalize_path_for_addon_match(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path)
}

fn is_safe_addon_path(base_path: &str, addon_path: &Path) -> bool {
    if base_path.trim().is_empty() {
        return false;
    }
    let base_normalized = normalize_path_for_addon_match(base_path.trim());
    let addon_normalized = normalize_path_for_addon_match(&addon_path.to_string_lossy());
    let prefix = format!("{}/", base_normalized);
    addon_normalized.starts_with(&prefix)
}
