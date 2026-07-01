use super::{AppState, CommandError, CommandSuccess, progress_output_muted, run_repository_sync};
use crate::cli::args::{CliArgs, SpaceCommand, SpaceSyncArgs, SpaceSyncMode};
use crate::cli::exit_codes;
use crate::core::api::{ModDiffSummary, SyncMode};
use crate::core::models::pending_update::load_pending_update_payload_for_path;
use crate::ui::app::Foxy;
use crate::ui::types::{Repository, RepositorySpaceEntry};
use serde_json::{Value, json};
use std::collections::HashSet;
use tokio::runtime::Runtime;

pub fn run_space_command(
    cli: &CliArgs,
    command: SpaceCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        SpaceCommand::List => cmd_space_list(),
        SpaceCommand::Sync(args) => cmd_space_sync(cli, args),
    }
}

fn cmd_space_list() -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    Ok(CommandSuccess {
        action: "space.list".to_string(),
        message: serde_json::to_string_pretty(&state.spaces).unwrap_or_else(|_| "[]".to_string()),
        data: json!(state.spaces),
        exit_code: exit_codes::SUCCESS,
    })
}

fn cmd_space_sync(cli: &CliArgs, args: SpaceSyncArgs) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let space = state
        .spaces
        .iter()
        .find(|space| space.id == args.space_id)
        .cloned()
        .ok_or_else(|| {
            CommandError::not_found("space.sync", format!("Space {} not found", args.space_id))
        })?;

    let mode = resolve_space_sync_mode(&args)?;
    let mode_label = format!("{mode:?}");

    let mut targets =
        collect_repository_space_sync_targets(&space.id, &space.entries, &state.repositories);
    if targets.is_empty() {
        return Err(CommandError::not_found(
            "space.sync",
            format!("No repositories attached to space {}", space.name),
        ));
    }
    targets = filter_space_sync_targets(targets, &state.repositories, &args.select, &args.exclude)?;
    if mode == SyncMode::Download {
        targets = filter_targets_with_pending_updates(targets, &state.repositories)?;
    }
    if targets.is_empty() {
        return Err(CommandError::not_found(
            "space.sync",
            format!(
                "No repositories matched the requested filters for {}",
                space.name
            ),
        ));
    }

    if cli.dry_run {
        let repos: Vec<Value> = targets
            .iter()
            .filter_map(|idx| state.repositories.get(*idx))
            .map(|repo| json!({"name": repo.name, "address": repo.address, "path": repo.path}))
            .collect();
        return Ok(CommandSuccess {
            action: "space.sync".to_string(),
            message: "Dry-run: space sync previewed".to_string(),
            data: json!({"space": space, "mode": mode_label, "repositories": repos, "dry_run": true}),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let mut results = Vec::new();
    let mut failures = Vec::new();
    for idx in targets {
        let Some(repo) = state.repositories.get(idx).cloned() else {
            continue;
        };
        match run_repository_sync(
            &repo,
            &state.settings,
            mode,
            progress_output_muted(cli),
            false,
            false,
        ) {
            Ok(summary) => {
                results.push(json!({"repository": repo.name, "ok": true, "summary": summary}))
            }
            Err(err) => {
                failures.push(format!("{}: {}", repo.name, err.message));
                results.push(json!({"repository": repo.name, "ok": false, "error": err.message}));
            }
        }
    }

    if !failures.is_empty() {
        return Err(CommandError::partial(
            "space.sync",
            format!(
                "Space sync finished with failures: {}",
                failures.join(" | ")
            ),
        ));
    }

    Ok(CommandSuccess {
        action: "space.sync".to_string(),
        message: format!("Space sync completed for {}", space.name),
        data: json!({"space": space, "mode": mode_label, "results": results}),
        exit_code: exit_codes::SUCCESS,
    })
}

fn resolve_space_sync_mode(args: &SpaceSyncArgs) -> Result<SyncMode, CommandError> {
    if args.update_all {
        return Ok(SyncMode::Download);
    }
    if args.recheck_all {
        return Ok(SyncMode::RemoteRefreshOnly);
    }
    match args.mode {
        Some(SpaceSyncMode::RemoteRefresh) => Ok(SyncMode::RemoteRefreshOnly),
        Some(SpaceSyncMode::QuickCheck) => Ok(SyncMode::QuickCheckOnly),
        None => Err(CommandError::validation(
            "space.sync",
            "Specify one operation: --mode <remote-refresh|quick-check>, --recheck-all, or --update-all",
        )),
    }
}

fn repository_matches_space_selector(repo: &Repository, idx: usize, selector: &str) -> bool {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return false;
    }

    if let Ok(one_based_index) = trimmed.parse::<usize>()
        && one_based_index > 0
        && idx + 1 == one_based_index
    {
        return true;
    }

    if repo.name.eq_ignore_ascii_case(trimmed) {
        return true;
    }

    let normalized_selector =
        Foxy::normalize_repo_url(&Foxy::normalize_repository_address_input(trimmed));
    let normalized_repo = Foxy::normalize_repo_url(&repo.address);
    normalized_repo.eq_ignore_ascii_case(&normalized_selector)
}

fn filter_space_sync_targets(
    targets: Vec<usize>,
    repositories: &[Repository],
    select: &[String],
    exclude: &[String],
) -> Result<Vec<usize>, CommandError> {
    let select_tokens: Vec<&str> = select
        .iter()
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .collect();
    let exclude_tokens: Vec<&str> = exclude
        .iter()
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .collect();

    if !select_tokens.is_empty() && !exclude_tokens.is_empty() {
        let overlap = select_tokens.iter().any(|selected| {
            exclude_tokens
                .iter()
                .any(|excluded| selected.eq_ignore_ascii_case(excluded))
        });
        if overlap {
            return Err(CommandError::validation(
                "space.sync",
                "--select and --exclude contain overlapping selectors",
            ));
        }
    }

    Ok(targets
        .into_iter()
        .filter(|idx| {
            let Some(repo) = repositories.get(*idx) else {
                return false;
            };
            let selected = if select_tokens.is_empty() {
                true
            } else {
                select_tokens
                    .iter()
                    .any(|token| repository_matches_space_selector(repo, *idx, token))
            };
            if !selected {
                return false;
            }
            !exclude_tokens
                .iter()
                .any(|token| repository_matches_space_selector(repo, *idx, token))
        })
        .collect())
}

fn filter_targets_with_pending_updates(
    targets: Vec<usize>,
    repositories: &[Repository],
) -> Result<Vec<usize>, CommandError> {
    let runtime = Runtime::new().map_err(|err| {
        CommandError::operation(
            "space.sync",
            format!(
                "Failed to initialize runtime for pending-update lookup: {}",
                err
            ),
        )
    })?;

    let mut filtered = Vec::new();
    for idx in targets {
        let Some(repo) = repositories.get(idx) else {
            continue;
        };

        let normalized_url = Foxy::normalize_repo_url(&repo.address);
        let has_pending = match runtime.block_on(load_pending_update_payload_for_path(
            &normalized_url,
            &repo.path,
        )) {
            Ok(Some(payload)) => serde_json::from_str::<Vec<ModDiffSummary>>(&payload)
                .map(|mods| mods.iter().any(|m| m.needs_update))
                .unwrap_or(false),
            Ok(None) => false,
            Err(_) => false,
        };

        if has_pending {
            filtered.push(idx);
        }
    }

    Ok(filtered)
}

fn collect_repository_space_sync_targets(
    space_id: &str,
    entries: &[RepositorySpaceEntry],
    repositories: &[Repository],
) -> Vec<usize> {
    let mut ordered_entry_urls = Vec::new();
    for required in [true, false] {
        for entry in entries {
            if entry.required == required {
                ordered_entry_urls.push(Foxy::normalize_repo_url(&entry.address));
            }
        }
    }

    let mut seen = HashSet::new();
    let mut ordered = Vec::new();

    for entry_url in &ordered_entry_urls {
        for (idx, repo) in repositories.iter().enumerate() {
            if repo.repository_space_id.as_deref() != Some(space_id) || seen.contains(&idx) {
                continue;
            }
            if Foxy::normalize_repo_url(&repo.address) == *entry_url {
                seen.insert(idx);
                ordered.push(idx);
            }
        }
    }

    for (idx, repo) in repositories.iter().enumerate() {
        if repo.repository_space_id.as_deref() == Some(space_id) && seen.insert(idx) {
            ordered.push(idx);
        }
    }

    ordered
}
