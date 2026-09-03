use super::{AppState, CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, SettingsCommand, SettingsSetArgs};
use crate::cli::exit_codes;
use crate::ui::types::{
    additional_folder_alias_key, sanitize_additional_folder_alias, sanitize_user_path,
};
use serde_json::json;
use std::collections::HashSet;

pub fn run_settings_command(
    cli: &CliArgs,
    command: SettingsCommand,
) -> Result<CommandSuccess, CommandError> {
    let mut state = AppState::load()?;
    match command {
        SettingsCommand::Show => Ok(CommandSuccess {
            action: "settings.show".to_string(),
            message: serde_json::to_string_pretty(&state.settings)
                .unwrap_or_else(|_| "{}".to_string()),
            data: json!(state.settings),
            exit_code: exit_codes::SUCCESS,
        }),
        SettingsCommand::Set(args) => cmd_settings_set(cli, &mut state, *args),
        SettingsCommand::Reset => {
            if cli.dry_run {
                return Ok(CommandSuccess {
                    action: "settings.reset".to_string(),
                    message: "Dry-run: settings reset previewed".to_string(),
                    data: json!({"dry_run": true}),
                    exit_code: exit_codes::SUCCESS,
                });
            }
            state.settings = crate::ui::types::SettingsViewState::default();
            state.save_settings()?;
            Ok(CommandSuccess {
                action: "settings.reset".to_string(),
                message: "Settings reset to defaults".to_string(),
                data: json!({"reset": true}),
                exit_code: exit_codes::SUCCESS,
            })
        }
    }
}

fn cmd_settings_set(
    cli: &CliArgs,
    state: &mut AppState,
    args: SettingsSetArgs,
) -> Result<CommandSuccess, CommandError> {
    let mut changed = false;

    if let Some(v) = args.show_debug_windows {
        state.settings.show_debug_windows = v;
        changed = true;
    }
    if let Some(v) = args.show_activity_log {
        state.settings.show_activity_log = v;
        changed = true;
    }
    if let Some(v) = args.show_memory_diagnostics_icon {
        state.settings.show_memory_diagnostics_icon = v;
        changed = true;
    }
    if let Some(v) = args.close_after_launch {
        state.settings.close_after_launch = v;
        changed = true;
    }
    if let Some(v) = args.hide_to_tray_after_launch {
        state.settings.hide_to_tray_after_launch = v;
        changed = true;
    }
    if let Some(v) = args.auto_recheck_on_launch {
        state.settings.auto_recheck_on_launch = v;
        changed = true;
    }
    if let Some(v) = args.auto_quick_scan_on_launch {
        state.settings.auto_quick_scan_on_launch = v;
        changed = true;
    }
    if let Some(v) = args.apply_repo_json_client_parameters {
        state.settings.apply_repo_json_client_parameters = v;
        changed = true;
    }
    if let Some(v) = args.apply_repo_json_dlc_content {
        state.settings.apply_repo_json_dlc_content = v;
        changed = true;
    }
    if let Some(v) = args.warn_editor_external_addons {
        state.settings.warn_editor_external_addons = v;
        changed = true;
    }
    if let Some(v) = args.enable_editor_mission_list {
        state.settings.enable_editor_mission_list = v;
        changed = true;
    }
    if let Some(v) = args.enable_server_list {
        state.settings.enable_server_list = v;
        changed = true;
    }
    if let Some(v) = args.arma3_dir {
        state.settings.arma3_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.twwh3_dir {
        state.settings.twwh3_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.reforger_dir {
        state.settings.reforger_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.generic_dir {
        state.settings.generic_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.generic_executable {
        state.settings.generic_executable = v.trim().to_string();
        changed = true;
    }
    if let Some(v) = args.generic_steam_app_id {
        state.settings.generic_steam_app_id = if v == 0 { String::new() } else { v.to_string() };
        changed = true;
    }
    if let Some(v) = args.generic_launch_args {
        state.settings.generic_launch_template = v.trim().to_string();
        changed = true;
    }
    if let Some(v) = args.generic_mods_manifest {
        state.settings.generic_mods_manifest = v.trim().to_string();
        changed = true;
    }
    if let Some(v) = args.arma3_profiles_dir {
        state.settings.arma3_profiles_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.steam_dir {
        state.settings.steam_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.temp_dir {
        state.settings.temp_directory = sanitize_user_path(&v.display().to_string());
        changed = true;
    }
    if let Some(v) = args.download_speed_limit_mbps {
        state.settings.download_speed_limit_mbps = Some(v.max(1));
        changed = true;
    }
    if args.download_speed_unlimited {
        state.settings.download_speed_limit_mbps = None;
        changed = true;
    }
    crate::ui::types::normalize_settings_launch_behavior(&mut state.settings);
    if let Some(v) = args.locale {
        state.settings.locale = v.trim().to_string();
        changed = true;
    }

    for folder in args.add_additional_folder {
        let candidate = folder.display().to_string();
        if !state
            .settings
            .additional_folders
            .iter()
            .any(|f| normalize_path(f) == normalize_path(&candidate))
        {
            state.settings.additional_folders.push(candidate);
            changed = true;
        }
    }
    for folder in args.remove_additional_folder {
        let candidate = normalize_path(&folder.display().to_string());
        let before = state.settings.additional_folders.len();
        let removed_alias_keys: HashSet<String> = state
            .settings
            .additional_folders
            .iter()
            .filter(|f| normalize_path(f) == candidate)
            .map(|path| additional_folder_alias_key(path))
            .collect();
        state
            .settings
            .additional_folders
            .retain(|f| normalize_path(f) != candidate);
        for key in removed_alias_keys {
            state.settings.additional_folder_aliases.remove(&key);
        }
        if state.settings.additional_folders.len() != before {
            changed = true;
        }
    }
    for assignment in args.set_additional_folder_alias {
        let (folder_path_raw, alias_raw) = parse_additional_folder_alias_assignment(&assignment)
            .map_err(|message| CommandError::validation("settings.set", message))?;
        let target_path_key = normalize_path(&folder_path_raw);
        let Some(existing_folder) = state
            .settings
            .additional_folders
            .iter()
            .find(|folder| normalize_path(folder) == target_path_key)
            .cloned()
        else {
            return Err(CommandError::validation(
                "settings.set",
                format!(
                    "Cannot set alias: additional folder {} is not configured",
                    folder_path_raw
                ),
            ));
        };

        let alias = sanitize_additional_folder_alias(&alias_raw);
        let alias_key = additional_folder_alias_key(&existing_folder);
        if state
            .settings
            .additional_folder_aliases
            .get(&alias_key)
            .is_none_or(|current| current != &alias)
        {
            state
                .settings
                .additional_folder_aliases
                .insert(alias_key, alias);
            changed = true;
        }
    }
    for folder in args.clear_additional_folder_alias {
        let candidate = normalize_path(&folder.display().to_string());
        let keys_to_remove: Vec<String> = state
            .settings
            .additional_folders
            .iter()
            .filter(|f| normalize_path(f) == candidate)
            .map(|path| additional_folder_alias_key(path))
            .collect();
        let mut removed_any = false;
        for key in keys_to_remove {
            removed_any = state
                .settings
                .additional_folder_aliases
                .remove(&key)
                .is_some()
                || removed_any;
        }
        if removed_any {
            changed = true;
        }
    }

    for folder in args.add_cleanup_folder {
        let candidate = folder.display().to_string();
        if !state
            .settings
            .cleanup_folders
            .iter()
            .any(|(f, _)| normalize_path(f) == normalize_path(&candidate))
        {
            state.settings.cleanup_folders.push((candidate, false));
            changed = true;
        }
    }
    for folder in args.remove_cleanup_folder {
        let candidate = normalize_path(&folder.display().to_string());
        let before = state.settings.cleanup_folders.len();
        state
            .settings
            .cleanup_folders
            .retain(|(f, _)| normalize_path(f) != candidate);
        if state.settings.cleanup_folders.len() != before {
            changed = true;
        }
    }

    if !changed {
        return Err(CommandError::validation(
            "settings.set",
            "No settings changes were provided",
        ));
    }

    super::sanitize_settings(&mut state.settings);
    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "settings.set".to_string(),
            message: "Dry-run: settings update previewed".to_string(),
            data: json!({"settings": state.settings, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    state.save_settings()?;
    Ok(CommandSuccess {
        action: "settings.set".to_string(),
        message: "Settings updated".to_string(),
        data: json!(state.settings),
        exit_code: exit_codes::SUCCESS,
    })
}

fn normalize_path(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path.trim())
}

fn parse_additional_folder_alias_assignment(input: &str) -> Result<(String, String), String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Additional folder alias assignment cannot be empty".to_string());
    }

    let Some((path_raw, alias_raw)) = trimmed.split_once('=') else {
        return Err(
            "Invalid additional folder alias assignment format. Use PATH=ALIAS".to_string(),
        );
    };

    let path = sanitize_user_path(path_raw);
    if path.trim().is_empty() {
        return Err("Additional folder alias assignment path cannot be empty".to_string());
    }

    let alias = sanitize_additional_folder_alias(alias_raw);
    if alias.is_empty() {
        return Err(
            "Additional folder alias assignment alias cannot be empty; use --clear-additional-folder-alias to remove it".to_string(),
        );
    }

    Ok((path, alias))
}
