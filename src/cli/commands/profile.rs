use super::{AppState, CommandError, CommandSuccess, find_repository_index};
use crate::cli::args::{
    CliArgs, ProfileAddArgs, ProfileCommand, ProfileDeleteArgs, ProfileListArgs, ProfileSelectArgs,
};
use crate::cli::exit_codes;
use crate::ui::types::RepositoryProfile;
use serde_json::{Value, json};

pub fn run_profile_command(
    cli: &CliArgs,
    command: ProfileCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ProfileCommand::List(args) => cmd_profile_list(args),
        ProfileCommand::Select(args) => cmd_profile_select(cli, args),
        ProfileCommand::Add(args) => cmd_profile_add(cli, args),
        ProfileCommand::Delete(args) => cmd_profile_delete(cli, args),
    }
}

fn cmd_profile_list(args: ProfileListArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let repo = state.repositories[idx].clone();

    let profiles: Vec<Value> = repo
        .profiles
        .iter()
        .map(|p| {
            json!({
                "name": p.name,
                "selected": repo.selected_profile.as_deref() == Some(p.name.as_str())
            })
        })
        .collect();

    Ok(CommandSuccess {
        action: "profile.list".to_string(),
        message: serde_json::to_string_pretty(&profiles).unwrap_or_else(|_| "[]".to_string()),
        data: json!({"repository": repo.name, "profiles": profiles, "selected_profile": repo.selected_profile}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_profile_select(
    cli: &CliArgs,
    args: ProfileSelectArgs,
) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let profile = args.profile.trim();
    if profile.is_empty() {
        return Err(CommandError::validation(
            "profile.select",
            "Profile name must be non-empty",
        ));
    }

    let (repo_name, selected_profile) = {
        let repo = state
            .repositories
            .get_mut(idx)
            .ok_or_else(|| CommandError::not_found("profile.select", "Repository not found"))?;

        if profile.eq_ignore_ascii_case("default") {
            repo.selected_profile = None;
        } else if repo.profiles.iter().any(|p| p.name == profile) {
            repo.selected_profile = Some(profile.to_string());
        } else {
            return Err(CommandError::not_found(
                "profile.select",
                format!("Profile {} not found", profile),
            ));
        }

        let repo_name = repo.name.clone();
        let selected_profile = repo.selected_profile.clone();
        if cli.dry_run {
            return Ok(CommandSuccess {
                action: "profile.select".to_string(),
                message: "Dry-run: profile select previewed".to_string(),
                data: json!({"repository": repo_name, "selected_profile": selected_profile, "dry_run": true}),
                exit_code: exit_codes::SUCCESS,
            });
        }
        (repo_name, selected_profile)
    };

    state.save_repositories()?;
    Ok(CommandSuccess {
        action: "profile.select".to_string(),
        message: "Profile selected".to_string(),
        data: json!({"repository": repo_name, "selected_profile": selected_profile}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_profile_add(cli: &CliArgs, args: ProfileAddArgs) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let profile_name = args.profile.trim();
    if profile_name.is_empty() {
        return Err(CommandError::validation(
            "profile.add",
            "Profile name must be non-empty",
        ));
    }

    let (repo_name, profile, selected_profile) = {
        let repo = state
            .repositories
            .get_mut(idx)
            .ok_or_else(|| CommandError::not_found("profile.add", "Repository not found"))?;
        if repo.profiles.iter().any(|p| p.name == profile_name) {
            return Err(CommandError::validation(
                "profile.add",
                "Profile name must be unique",
            ));
        }

        let profile = RepositoryProfile {
            name: profile_name.to_string(),
            addons: repo
                .addons
                .iter()
                .map(|(name, _)| (name.clone(), true))
                .collect(),
            optional_addons: repo
                .optional_addons
                .iter()
                .map(|(name, _)| (name.clone(), false))
                .collect(),
            optional_addon_favorites: repo.optional_addon_favorites.clone(),
            optional_addon_client_side: repo.optional_addon_client_side.clone(),
            external_addons: repo
                .external_addons
                .iter()
                .map(|(name, _, path)| (name.clone(), false, path.clone()))
                .collect(),
            external_addon_favorites: repo.external_addon_favorites.clone(),
            external_addon_client_side: repo.external_addon_client_side.clone(),
            ..Default::default()
        };

        let repo_name = repo.name.clone();
        if cli.dry_run {
            return Ok(CommandSuccess {
                action: "profile.add".to_string(),
                message: "Dry-run: profile add previewed".to_string(),
                data: json!({"repository": repo_name, "profile": profile, "dry_run": true}),
                exit_code: exit_codes::SUCCESS,
            });
        }

        repo.profiles.push(profile.clone());
        repo.selected_profile = Some(profile.name.clone());
        (repo_name, profile, repo.selected_profile.clone())
    };
    state.save_repositories()?;

    Ok(CommandSuccess {
        action: "profile.add".to_string(),
        message: format!("Profile {} added", profile.name),
        data: json!({"repository": repo_name, "profile": profile, "selected_profile": selected_profile}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_profile_delete(
    cli: &CliArgs,
    args: ProfileDeleteArgs,
) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    let idx = find_repository_index(&state.repositories, &args.selector)?;
    let profile_name = args.profile.trim();
    if profile_name.is_empty() {
        return Err(CommandError::validation(
            "profile.delete",
            "Profile name must be non-empty",
        ));
    }

    let (repo_name, selected_profile) = {
        let repo = state
            .repositories
            .get_mut(idx)
            .ok_or_else(|| CommandError::not_found("profile.delete", "Repository not found"))?;
        let before = repo.profiles.len();
        repo.profiles.retain(|p| p.name != profile_name);
        if before == repo.profiles.len() {
            return Err(CommandError::not_found(
                "profile.delete",
                format!("Profile {} not found", profile_name),
            ));
        }
        if repo.selected_profile.as_deref() == Some(profile_name) {
            repo.selected_profile = None;
        }

        let repo_name = repo.name.clone();
        let selected_profile = repo.selected_profile.clone();
        if cli.dry_run {
            return Ok(CommandSuccess {
                action: "profile.delete".to_string(),
                message: "Dry-run: profile delete previewed".to_string(),
                data: json!({"repository": repo_name, "deleted_profile": profile_name, "dry_run": true}),
                exit_code: exit_codes::SUCCESS,
            });
        }
        (repo_name, selected_profile)
    };

    state.save_repositories()?;
    Ok(CommandSuccess {
        action: "profile.delete".to_string(),
        message: format!("Profile {} deleted", profile_name),
        data: json!({"repository": repo_name, "deleted_profile": profile_name, "selected_profile": selected_profile}),
        exit_code: exit_codes::SUCCESS,
    })
}
