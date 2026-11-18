use super::instances::{capture, store_error, success};
use super::{CommandError, CommandSuccess};
use crate::cli::args::{CliArgs, ServerContextArgs, ServerServiceCommand};
use crate::core::server::{ServiceExportFormat, plan_service_export, write_service_export};
use serde_json::json;

pub(super) fn run(
    cli: &CliArgs,
    context: ServerContextArgs,
    command: ServerServiceCommand,
) -> Result<CommandSuccess, CommandError> {
    const ACTION: &str = "server.service.export";
    let context = capture(context, ACTION)?;
    let ServerServiceCommand::Export {
        format,
        executable,
        output,
    } = command;
    let format = match format.as_deref() {
        Some("systemd-user") => ServiceExportFormat::SystemdUser,
        Some("windows-user-task") => ServiceExportFormat::WindowsUserTask,
        None if cfg!(windows) => ServiceExportFormat::WindowsUserTask,
        None => ServiceExportFormat::SystemdUser,
        Some(_) => {
            return Err(CommandError::validation(
                ACTION,
                "Unsupported service export format",
            ));
        }
    };
    let executable = executable
        .map(Ok)
        .unwrap_or_else(std::env::current_exe)
        .map_err(|_| CommandError::operation(ACTION, "Cannot locate the Foxy executable"))?;
    let plan = plan_service_export(&context, &executable, format)
        .map_err(|error| store_error(ACTION, error))?;
    let writes = output.is_some() && !cli.dry_run;
    if let Some(path) = output.as_ref().filter(|_| writes) {
        let path = std::path::absolute(path).map_err(|_| {
            CommandError::validation(ACTION, "Cannot resolve the export destination")
        })?;
        write_service_export(&plan, &path).map_err(|error| store_error(ACTION, error))?;
    }
    let message = if writes {
        "Saved a new definition. Registration is separate; no owner or server was started."
            .to_owned()
    } else {
        format!("{}\n{}", plan.definition, plan.warnings.join("\n"))
    };
    Ok(success(
        ACTION,
        message,
        json!({
            "plan":plan, "output":output, "writes_performed":writes,
            "registration_performed":false,"database_opened":false,"dry_run":cli.dry_run,
        }),
    ))
}
