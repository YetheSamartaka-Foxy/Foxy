use serde_json::json;

use super::instances::{capture, save_change, store_error, success};
use crate::cli::args::{
    CliArgs, ServerContextArgs, ServerReforgerExportArgs, ServerReforgerImportArgs,
    ServerScenarioCommand,
};
use crate::cli::commands::{CommandError, CommandSuccess};
use crate::core::server::{
    DraftChange, ServerDefinition, ServerDefinitionKind, ServerInstanceId, ServerStore,
    import_server_json, parse_scenario_listing, read_scenario_log, render_server_json,
    scenario_listing_note,
};

pub(super) fn import(
    cli: &CliArgs,
    context: ServerContextArgs,
    args: ServerReforgerImportArgs,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.configuration.import-reforger";
    let store =
        ServerStore::new(capture(context, action)?).map_err(|error| store_error(action, error))?;
    let id = ServerInstanceId::try_from(args.id)
        .map_err(|message| CommandError::validation(action, message))?;
    let mut instance = store
        .show(&id)
        .map_err(|error| store_error(action, error))?;
    if instance.definition.kind() != ServerDefinitionKind::Reforger {
        return Err(CommandError::validation(
            action,
            "Reforger server.json import requires a Reforger server draft",
        ));
    }
    if instance.revision != args.expected_revision {
        return Err(CommandError::validation(
            action,
            "Server draft changed; reload it and retry with the current revision",
        ));
    }
    let bytes = crate::core::server::read_bounded(&args.file).map_err(|_| {
        CommandError::validation(action, "Cannot read Reforger server.json (maximum 4 MiB)")
    })?;
    let review =
        import_server_json(&bytes).map_err(|message| CommandError::validation(action, message))?;
    instance.definition = ServerDefinition::Reforger(Box::new(review.definition.clone()));
    let saved = if args.apply && !cli.dry_run {
        if !cli.yes {
            return Err(CommandError::validation(
                action,
                "Applying a Reforger configuration import requires --yes. Review without --apply first.",
            ));
        }
        Some(save_change(
            cli,
            &store,
            store.context(),
            DraftChange::Replace {
                instance,
                expected_revision: args.expected_revision,
            },
            action,
        )?)
    } else {
        None
    };
    Ok(success(
        action,
        "Reforger server.json reviewed. Secret values were removed. A scenario resource name does not prove the scenario is installed.".into(),
        json!({
            "review": review,
            "writes_performed": saved.is_some(),
            "database_opened": false
        }),
    ))
}

pub(super) fn export(
    cli: &CliArgs,
    context: ServerContextArgs,
    args: ServerReforgerExportArgs,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.configuration.export-reforger";
    let store =
        ServerStore::new(capture(context, action)?).map_err(|error| store_error(action, error))?;
    let id = ServerInstanceId::try_from(args.id)
        .map_err(|message| CommandError::validation(action, message))?;
    let instance = store
        .show(&id)
        .map_err(|error| store_error(action, error))?;
    let ServerDefinition::Reforger(settings) = &instance.definition else {
        return Err(CommandError::validation(
            action,
            "Reforger server.json export requires a Reforger server draft",
        ));
    };
    let value = render_server_json(settings);
    let warnings = crate::core::server::preview_warnings(settings);
    if !cli.dry_run {
        crate::core::server::write_server_json(&args.file, &value)
            .map_err(|message| CommandError::validation(action, message))?;
    }
    Ok(success(
        action,
        "Reforger server.json export contains the saved draft and preserved fields, without secret values.".into(),
        json!({
            "server_json": value,
            "warnings": warnings,
            "writes_performed": !cli.dry_run,
            "database_opened": false
        }),
    ))
}

pub(super) fn scenario(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerScenarioCommand,
) -> Result<CommandSuccess, CommandError> {
    match command {
        ServerScenarioCommand::ParseLog { file } => {
            let action = "server.configuration.scenario.parse-log";
            let listing = read_scenario_log(&file)
                .and_then(|output| parse_scenario_listing(&output))
                .map_err(|message| CommandError::validation(action, message))?;
            let count = listing.scenarios.len();
            Ok(success(
                action,
                "Official scenario listing parsed. No scenario was selected and no files were written.".into(),
                json!({
                    "listing": listing,
                    "count": count,
                    "writes_performed": false,
                    "database_opened": false
                }),
            ))
        }
        ServerScenarioCommand::ImportLog { id, file } => {
            let action = "server.configuration.scenario.import-log";
            let store = ServerStore::new(capture(context, action)?)
                .map_err(|error| store_error(action, error))?;
            let id = ServerInstanceId::try_from(id)
                .map_err(|message| CommandError::validation(action, message))?;
            let instance = store
                .show(&id)
                .map_err(|error| store_error(action, error))?;
            if instance.definition.kind() != ServerDefinitionKind::Reforger {
                return Err(CommandError::validation(
                    action,
                    "Scenario listing import requires a Reforger server draft",
                ));
            }
            let selected = scenario_of(&instance.definition);
            let listing = read_scenario_log(&file)
                .and_then(|output| parse_scenario_listing(&output))
                .map_err(|message| CommandError::validation(action, message))?;
            let stored = if cli.dry_run {
                None
            } else if !cli.yes {
                return Err(CommandError::validation(
                    action,
                    "Storing a scenario listing requires --yes. parse-log reviews a log without writing.",
                ));
            } else {
                let _claim = super::content::claim(&store, action)?;
                Some(
                    store
                        .write_scenario_discovery(&id, &listing)
                        .map_err(|error| store_error(action, error))?,
                )
            };
            let after = store
                .show(&id)
                .map_err(|error| store_error(action, error))?;
            if scenario_of(&after.definition) != selected || after.revision != instance.revision {
                return Err(CommandError::operation(
                    action,
                    "Scenario listing storage changed the selected scenario",
                ));
            }
            Ok(success(
                action,
                "Scenario listing stored. The selected scenario was not changed, and a listed name does not prove the file exists.".into(),
                json!({
                    "discovery": stored,
                    "selected_scenario": selected,
                    "count": listing.scenarios.len(),
                    "writes_performed": stored.is_some(),
                    "database_opened": false
                }),
            ))
        }
        ServerScenarioCommand::List { id } => {
            let action = "server.configuration.scenario.list";
            let store = ServerStore::new(capture(context, action)?)
                .map_err(|error| store_error(action, error))?;
            let id = ServerInstanceId::try_from(id)
                .map_err(|message| CommandError::validation(action, message))?;
            let instance = store
                .show(&id)
                .map_err(|error| store_error(action, error))?;
            if instance.definition.kind() != ServerDefinitionKind::Reforger {
                return Err(CommandError::validation(
                    action,
                    "Scenario listing requires a Reforger server draft",
                ));
            }
            let selected = scenario_of(&instance.definition);
            let discovery = store
                .read_scenario_discovery(&id)
                .map_err(|error| store_error(action, error))?;
            let note = scenario_listing_note(selected.as_deref(), discovery.as_ref());
            Ok(success(
                action,
                note.to_string(),
                json!({
                    "discovery": discovery,
                    "selected_scenario": selected,
                    "note": note,
                    "writes_performed": false,
                    "database_opened": false
                }),
            ))
        }
    }
}

fn scenario_of(definition: &ServerDefinition) -> Option<String> {
    match definition {
        ServerDefinition::Reforger(settings) => settings.scenario_id.clone(),
        ServerDefinition::Arma3(_) => None,
    }
}
