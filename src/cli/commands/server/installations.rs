use serde_json::json;

use super::instances::{capture, store_error, success};
use crate::cli::args::{ServerContextArgs, ServerInstallationCommand};
use crate::cli::commands::{CommandError, CommandSuccess};
use crate::core::server::ServerStore;

pub(super) fn run(
    context: ServerContextArgs,
    command: ServerInstallationCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = match command {
        ServerInstallationCommand::List => "server.installation.list",
        ServerInstallationCommand::Inspect(_) => "server.installation.inspect",
        ServerInstallationCommand::Consumers(_) => "server.installation.consumers",
        ServerInstallationCommand::Install { .. }
        | ServerInstallationCommand::Update { .. }
        | ServerInstallationCommand::Repair { .. }
        | ServerInstallationCommand::Check(_)
        | ServerInstallationCommand::Validate(_) => unreachable!(),
    };
    let context = capture(context, action)?;
    let store = ServerStore::new(context).map_err(|error| store_error(action, error))?;
    match command {
        ServerInstallationCommand::Consumers(args) => {
            let id = crate::core::server::InstallationId::try_from(args.id)
                .map_err(|error| CommandError::validation(action, error))?;
            let recovery = store
                .installation_recovery(&id)
                .map_err(|error| store_error(action, error))?;
            let inspection = recovery.consumers.clone();
            let complete = inspection.inspection_complete && recovery.inspection_complete;
            let message = if !complete {
                "Consumer or recovery metadata could not be fully verified. Maintenance remains blocked."
                    .to_string()
            } else {
                format!(
                    "{} registered consumer(s). Maintenance: {}. Liveness is unverified; this inspection does not authorize maintenance.",
                    inspection.recorded_consumers.len(),
                    recovery.maintenance_status
                )
            };
            let mut result = success(
                action,
                message,
                json!({"consumer_inspection":inspection,"recovery_inspection":recovery,"database_opened":false,"writes_performed":false}),
            );
            if !complete {
                result.exit_code = crate::cli::exit_codes::OPERATION_FAILED;
            }
            Ok(result)
        }
        ServerInstallationCommand::List => {
            let (catalog, migration_pending) = store
                .load_with_status()
                .map_err(|error| store_error(action, error))?;
            let message = if catalog.installations.is_empty() {
                "No adopted installations in this game space".into()
            } else {
                catalog
                    .installations
                    .iter()
                    .map(|item| {
                        format!(
                            "{}  {:?}  {}",
                            item.id.as_str(),
                            item.platform,
                            item.directory.display()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(success(
                action,
                message,
                json!({"installations":catalog.installations,"migration_pending":migration_pending,"database_opened":false}),
            ))
        }
        ServerInstallationCommand::Install { .. }
        | ServerInstallationCommand::Update { .. }
        | ServerInstallationCommand::Repair { .. }
        | ServerInstallationCommand::Check(_)
        | ServerInstallationCommand::Validate(_) => unreachable!(),
        ServerInstallationCommand::Inspect(args) => {
            let installation = store
                .inspect_installation(&args.directory)
                .map_err(|error| store_error(action, error))?;
            let build = store
                .inspect_installation_build(&installation)
                .map_err(|error| store_error(action, error))?;
            let version = build
                .executable_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "unknown".into());
            let steam = build
                .steam_manifest
                .as_ref()
                .map(|steam| {
                    format!(
                        "app {}; build {}; branch {}",
                        steam.app_id,
                        steam.build_id,
                        if steam.branch_conflict {
                            "conflicting"
                        } else {
                            steam.branch.as_deref().unwrap_or("unknown")
                        }
                    )
                })
                .unwrap_or_else(|| "unknown".into());
            let layout = build.tested_engine_layout.as_ref().map(|layout| {
                if installation.game == crate::core::server::ServerDefinitionKind::Reforger {
                    format!("Reforger {}; isolated runtime copy and private profile, download, temporary and log paths", layout.tested_runtime_version)
                } else {
                    format!("Arma {}; shared root-relative directories and isolated-copy fallback; base keys retained", layout.tested_runtime_version)
                }
            }).unwrap_or_else(|| "unsupported executable build".into());
            let message = format!(
                "Installation: {}\nNative executable inspected: {}\nPlatform: {:?}; bytes: {}; BLAKE3: {}\nExecutable version metadata: {version}\nLocal Steam manifest: {steam}\nExact native-tested layout match: {layout}\nLayout compatibility does not authorize deployment or Start. Package completeness and vendor authenticity remain unverified. Nothing was executed or written.",
                installation.directory.display(),
                installation.executable,
                installation.platform,
                installation.evidence.bytes,
                installation.evidence.blake3
            );
            Ok(success(
                action,
                message,
                json!({"installation":installation,"build_observation":build,"build_version_verified":false,"writes_performed":false,"database_opened":false}),
            ))
        }
    }
}
