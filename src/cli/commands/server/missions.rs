use serde_json::json;

use super::content::claim;
use super::content_stage::controlled;
use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerMissionCommand};
use crate::core::server::{
    Arma3Mission, ConfigurationOwnership, ConfigurationRole, ContentStageControl, DraftChange,
    MissionImportPlan, MissionOrigin, ServerDefinition, ServerInstanceId, ServerStore,
    ServerStoreError,
};

pub(super) fn run(
    cli: &CliArgs,
    context_args: ServerContextArgs,
    command: ServerMissionCommand,
) -> Result<CommandSuccess, CommandError> {
    let dry_run = cli.dry_run;
    let action = match &command {
        ServerMissionCommand::List => "server.mission.list",
        ServerMissionCommand::Import { .. } => "server.mission.import",
        ServerMissionCommand::Refresh { .. } => "server.mission.refresh",
        ServerMissionCommand::Acquire { .. } => "server.mission.acquire",
        ServerMissionCommand::Cycle { .. } => "server.mission.cycle",
    };
    let context = capture(context_args, action)?;

    if let ServerMissionCommand::Acquire {
        url,
        template,
        expected_blake3,
        ..
    } = &command
        && cli.dry_run
    {
        validate_acquisition(url, template, expected_blake3, action)?;
        return Ok(success(
            action,
            "Mission acquisition inputs are valid. Dry run performed no download, temporary staging, ownership claim, or library write.".into(),
            json!({
                "dry_run": true,
                "url_valid": true,
                "template": template,
                "expected_blake3": expected_blake3,
                "downloads_started": false,
                "temporary_download": false,
                "writes_performed": false,
                "database_opened": false,
                "ownership_claimed": false
            }),
        ));
    }

    let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
    match command {
        ServerMissionCommand::List => controlled(cli, action, move |control| {
            let library = store
                .mission_library(&control)
                .map_err(|error| store_error(action, error))?;
            Ok(success(
                action,
                format!(
                    "Mission library entries: {}; unrecognized entries: {}. No files were written.",
                    library.entries.len(),
                    library.unrecognized_entries
                ),
                json!({
                    "library": library,
                    "downloads_started": false,
                    "temporary_download": false,
                    "writes_performed": false,
                    "database_opened": false
                }),
            ))
        }),
        ServerMissionCommand::Import { source, apply } => {
            require_confirmation(cli, apply, action)?;
            controlled(cli, action, move |control| {
                let plan = store
                    .plan_mission_import(&source, &control)
                    .map_err(|error| store_error(action, error))?;
                finish_import(dry_run, &store, plan, apply, false, control, action)
            })
        }
        ServerMissionCommand::Refresh { mission_id, apply } => {
            require_confirmation(cli, apply, action)?;
            validate_mission_id(&mission_id, action)?;
            if cli.dry_run {
                controlled(cli, action, move |control| {
                    let library = store
                        .mission_library(&control)
                        .map_err(|error| store_error(action, error))?;
                    let entry = library
                        .entries
                        .into_iter()
                        .find(|entry| entry.mission_id == mission_id)
                        .ok_or_else(|| {
                            CommandError::not_found(action, "Mission library entry was not found")
                        })?;
                    if let MissionOrigin::Http {
                        url,
                        template,
                        expected_blake3,
                    } = &entry.origin
                    {
                        validate_acquisition(url, template, expected_blake3, action)?;
                        return Ok(success(
                            action,
                            "Remote mission refresh inputs are valid. Dry run performed no download, temporary staging, ownership claim, or library write.".into(),
                            json!({
                                "entry": entry,
                                "dry_run": true,
                                "downloads_started": false,
                                "temporary_download": false,
                                "writes_performed": false,
                                "database_opened": false,
                                "ownership_claimed": false
                            }),
                        ));
                    }
                    let plan = store
                        .plan_mission_refresh(&mission_id, &control)
                        .map_err(|error| store_error(action, error))?;
                    Ok(success(
                        action,
                        format!(
                            "Local mission refresh plan {}. Dry run performed no library writes.",
                            plan.fingerprint
                        ),
                        json!({
                            "plan": plan,
                            "dry_run": true,
                            "downloads_started": false,
                            "temporary_download": false,
                            "writes_performed": false,
                            "database_opened": false
                        }),
                    ))
                })
            } else {
                controlled(cli, action, move |control| {
                    let plan = store
                        .plan_mission_refresh(&mission_id, &control)
                        .map_err(|error| store_error(action, error))?;
                    let downloaded = matches!(&plan.entry.origin, MissionOrigin::Http { .. });
                    finish_import(dry_run, &store, plan, apply, downloaded, control, action)
                })
            }
        }
        ServerMissionCommand::Acquire {
            url,
            template,
            expected_blake3,
            apply,
        } => {
            require_confirmation(cli, apply, action)?;
            validate_acquisition(&url, &template, &expected_blake3, action)?;
            controlled(cli, action, move |control| {
                let plan = store
                    .plan_mission_acquisition(&url, &template, &expected_blake3, &control)
                    .map_err(|error| store_error(action, error))?;
                finish_import(dry_run, &store, plan, apply, true, control, action)
            })
        }
        ServerMissionCommand::Cycle {
            id,
            expected_revision,
            file,
            apply,
        } => cycle(cli, &store, id, expected_revision, file, apply, action),
    }
}

fn finish_import(
    dry_run: bool,
    store: &ServerStore,
    plan: MissionImportPlan,
    apply: bool,
    downloaded: bool,
    control: ContentStageControl,
    action: &'static str,
) -> Result<CommandSuccess, CommandError> {
    let fingerprint = plan.fingerprint.clone();
    if apply && !dry_run {
        let _claim = claim(store, action)?;
        let entry = store
            .apply_mission_import(plan, &control)
            .map_err(|error| store_error(action, error))?;
        return Ok(success(
            action,
            format!(
                "Mission {} saved to the reviewed mission library at revision {}.",
                entry.payload.template, entry.revision_id
            ),
            json!({
                "entry": entry,
                "fingerprint": fingerprint,
                "dry_run": false,
                "downloads_started": downloaded,
                "temporary_download": downloaded,
                "writes_performed": true,
                "database_opened": false
            }),
        ));
    }

    Ok(success(
        action,
        format!(
            "Mission {} reviewed as {}. No mission library files were written.",
            plan.entry.payload.template, plan.fingerprint
        ),
        json!({
            "plan": plan,
            "dry_run": dry_run,
            "downloads_started": downloaded,
            "temporary_download": downloaded,
            "writes_performed": false,
            "database_opened": false,
            "requires_apply_and_yes": true
        }),
    ))
}

fn cycle(
    cli: &CliArgs,
    store: &ServerStore,
    id: String,
    expected_revision: u64,
    file: Option<std::path::PathBuf>,
    apply: bool,
    action: &'static str,
) -> Result<CommandSuccess, CommandError> {
    let id = ServerInstanceId::try_from(id)
        .map_err(|message| CommandError::validation(action, message))?;
    let instance = store
        .show(&id)
        .map_err(|error| store_error(action, error))?;
    let Some(file) = file else {
        if apply {
            return Err(CommandError::validation(
                action,
                "Cycle --apply requires --file",
            ));
        }
        let ServerDefinition::Arma3(arma) = &instance.definition else {
            return Err(CommandError::validation(
                action,
                "Mission cycles are available for Arma 3 server drafts only",
            ));
        };
        return Ok(success(
            action,
            format!(
                "Mission cycle entries: {}; server draft revision: {}. No files were written.",
                arma.mission_cycle.len(),
                instance.revision
            ),
            json!({
                "instance_id": id.as_str(),
                "revision": instance.revision,
                "expected_revision_matches": expected_revision == instance.revision,
                "mission_cycle": arma.mission_cycle,
                "writes_performed": false,
                "database_opened": false
            }),
        ));
    };
    require_confirmation(cli, apply, action)?;
    if instance.revision != expected_revision {
        return Err(store_error(action, ServerStoreError::StaleRevision));
    }
    if instance
        .configuration_import
        .as_deref()
        .is_some_and(|imported| {
            imported.files.iter().any(|file| {
                file.role == ConfigurationRole::Server
                    && file.ownership == ConfigurationOwnership::Operator
            })
        })
    {
        return Err(CommandError::validation(
            action,
            "The operator-owned server configuration file controls the mission cycle; edit that source through configuration import instead.",
        ));
    }
    let bytes = crate::core::server::read_bounded(&file).map_err(|_| {
        CommandError::validation(
            action,
            "Cannot read a mission-cycle JSON array (maximum 4 MiB)",
        )
    })?;
    let cycle: Vec<Arma3Mission> = serde_json::from_slice(&bytes).map_err(|_| {
        CommandError::validation(action, "Expected a JSON array of Arma 3 mission entries")
    })?;
    let mut changed = instance;
    let ServerDefinition::Arma3(arma) = &mut changed.definition else {
        return Err(CommandError::validation(
            action,
            "Mission cycles are available for Arma 3 server drafts only",
        ));
    };
    arma.mission_cycle = cycle;
    let draft = store
        .plan(DraftChange::Replace {
            instance: changed,
            expected_revision,
        })
        .map_err(|error| store_error(action, error))?;
    if apply && !cli.dry_run {
        let _claim = claim(store, action)?;
        let updated = draft.instance.clone();
        let catalog_revision = draft.catalog_revision();
        store
            .apply(draft)
            .map_err(|error| store_error(action, error))?;
        return Ok(success(
            action,
            format!(
                "Mission cycle saved at server draft revision {}.",
                updated.revision
            ),
            json!({
                "instance": updated,
                "catalog_revision": catalog_revision,
                "dry_run": false,
                "writes_performed": true,
                "database_opened": false
            }),
        ));
    }
    let instance = draft.instance.clone();
    let catalog_revision = draft.catalog_revision();
    Ok(success(
        action,
        format!(
            "Mission cycle reviewed for server draft revision {}. No files were written.",
            instance.revision
        ),
        json!({
            "instance": instance,
            "catalog_revision": catalog_revision,
            "dry_run": cli.dry_run,
            "writes_performed": false,
            "database_opened": false,
            "requires_apply_and_yes": true
        }),
    ))
}

fn validate_acquisition(
    url: &str,
    template: &str,
    expected_blake3: &str,
    action: &str,
) -> Result<(), CommandError> {
    crate::core::server::validate_acquisition_url(url)
        .map_err(|error| store_error(action, error))?;
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 128
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    let mut parts = template.split('.');
    let name = parts.next().unwrap_or_default();
    let world = parts.next().unwrap_or_default();
    if parts.next().is_some() || !valid_part(name) || !valid_part(world) {
        return Err(CommandError::validation(
            action,
            "Mission template must use ASCII Name.World components",
        ));
    }
    if expected_blake3.len() != 64
        || !expected_blake3
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CommandError::validation(
            action,
            "Expected mission BLAKE3 digest must contain 64 lowercase hexadecimal characters",
        ));
    }
    Ok(())
}

fn validate_mission_id(value: &str, action: &str) -> Result<(), CommandError> {
    let Some(suffix) = value.strip_prefix("mission_") else {
        return Err(CommandError::validation(
            action,
            "Invalid mission library id",
        ));
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CommandError::validation(
            action,
            "Invalid mission library id",
        ));
    }
    Ok(())
}

fn require_confirmation(cli: &CliArgs, apply: bool, action: &str) -> Result<(), CommandError> {
    if apply && !cli.dry_run && !cli.yes {
        return Err(CommandError::validation(
            action,
            "Saving a mission library or server draft change requires --apply --yes",
        ));
    }
    Ok(())
}
