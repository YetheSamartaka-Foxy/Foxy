use super::instances::{capture, store_error, success};
use crate::cli::args::{
    CliArgs, ServerConfigurationImportArgs, ServerConfigurationPackExportArgs,
    ServerConfigurationPackImportArgs, ServerContextArgs,
};
use crate::cli::commands::{CommandError, CommandSuccess};
use crate::core::server::{
    ConfigurationImportInput, ConfigurationOwnership, ConfigurationRole, PortableInstancePack,
    ServerInstanceId, ServerStore,
};
use serde_json::json;

pub(super) fn import(
    cli: &CliArgs,
    context: ServerContextArgs,
    args: ServerConfigurationImportArgs,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.configuration.import";
    let store = ServerStore::new(capture(context, action)?).map_err(|e| store_error(action, e))?;
    let id =
        ServerInstanceId::try_from(args.id).map_err(|e| CommandError::validation(action, e))?;
    let ownership = if args.managed {
        ConfigurationOwnership::Managed
    } else {
        ConfigurationOwnership::Operator
    };
    let inputs: Vec<_> = [
        (ConfigurationRole::Server, args.server),
        (ConfigurationRole::Basic, args.basic),
        (ConfigurationRole::Profile, args.profile),
    ]
    .into_iter()
    .filter_map(|(role, path)| {
        path.map(|path| ConfigurationImportInput {
            role,
            ownership,
            path,
        })
    })
    .collect();
    let mut plan = store
        .plan_configuration_import(&id, args.expected_revision, &inputs)
        .map_err(|e| store_error(action, e))?;
    plan.set_paths_reviewed(args.review_paths || plan.report.unresolved_paths.is_empty());
    let report = plan.report.clone();
    let instance = if args.apply && !cli.dry_run {
        if !cli.yes {
            return Err(CommandError::validation(
                action,
                "Applying a configuration import requires --yes. Review without --apply first.",
            ));
        }
        let _claim = super::content::claim(&store, action)?;
        Some(
            store
                .apply_configuration_import(plan)
                .map_err(|e| store_error(action, e))?,
        )
    } else {
        None
    };
    Ok(success(
        action,
        format!(
            "Configuration import reviewed; {} secret references; {} unresolved paths; saved: {}",
            report.secret_references.len(),
            report.unresolved_paths.len(),
            instance.is_some()
        ),
        json!({"report":report,"writes_performed":instance.is_some(),"instance":instance,"database_opened":false}),
    ))
}

pub(super) fn export_pack(
    cli: &CliArgs,
    context: ServerContextArgs,
    args: ServerConfigurationPackExportArgs,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.configuration.export-pack";
    let store = ServerStore::new(capture(context, action)?).map_err(|e| store_error(action, e))?;
    let id =
        ServerInstanceId::try_from(args.id).map_err(|e| CommandError::validation(action, e))?;
    let pack = store
        .export_instance_pack(&id)
        .map_err(|e| store_error(action, e))?;
    let pack = if args.sanitized {
        pack.sanitized().map_err(|e| store_error(action, e))?
    } else {
        pack
    };
    if !cli.dry_run {
        pack.write_new(&args.file)
            .map_err(|e| store_error(action, e))?;
    }
    Ok(success(action, "Portable instance pack contains redacted configuration; installations, content, credential values and saves are excluded.".into(), json!({"pack":pack,"writes_performed":!cli.dry_run,"database_opened":false})))
}

pub(super) fn import_pack(
    cli: &CliArgs,
    context: ServerContextArgs,
    args: ServerConfigurationPackImportArgs,
) -> Result<CommandSuccess, CommandError> {
    let action = "server.configuration.import-pack";
    let store = ServerStore::new(capture(context, action)?).map_err(|e| store_error(action, e))?;
    let pack = PortableInstancePack::read(&args.file).map_err(|e| store_error(action, e))?;
    let mut plan = store
        .plan_instance_pack_import(pack, &args.name)
        .map_err(|e| store_error(action, e))?;
    plan.set_paths_reviewed(args.review_paths || plan.report.unresolved_paths.is_empty());
    let report = plan.report.clone();
    let instance = if args.apply && !cli.dry_run {
        if !cli.yes {
            return Err(CommandError::validation(
                action,
                "Importing an instance pack requires --yes. Review without --apply first.",
            ));
        }
        let _claim = super::content::claim(&store, action)?;
        Some(
            store
                .apply_instance_pack_import(plan)
                .map_err(|e| store_error(action, e))?,
        )
    } else {
        None
    };
    Ok(success(action, "Portable pack reviewed as a new unbound instance; review paths and bind credentials before activation.".into(), json!({"report":report,"writes_performed":instance.is_some(),"instance":instance,"database_opened":false})))
}
