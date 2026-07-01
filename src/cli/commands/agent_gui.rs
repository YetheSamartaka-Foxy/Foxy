use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::cli::args::{
    AgentGuiAddonsArgs, AgentGuiAssertArgs, AgentGuiBackupsArgs, AgentGuiBatchArgs,
    AgentGuiCheckpointArgs, AgentGuiClickArgs, AgentGuiClockArgs, AgentGuiCommand,
    AgentGuiContextMenuArgs, AgentGuiDialogArgs, AgentGuiDiffArgs, AgentGuiDownloadSummaryArgs,
    AgentGuiDragArgs, AgentGuiElementArgs, AgentGuiEventsArgs, AgentGuiExecArgs, AgentGuiFillArgs,
    AgentGuiFindArgs, AgentGuiFixtureArgs, AgentGuiFocusArgs, AgentGuiHoverArgs,
    AgentGuiInventoryArgs, AgentGuiInvokeArgs, AgentGuiKeyArgs, AgentGuiLogsArgs,
    AgentGuiMemoryArgs, AgentGuiMenuSelectArgs, AgentGuiMissionsArgs, AgentGuiModifierArgs,
    AgentGuiMouseArgs, AgentGuiNavArgs, AgentGuiOpenViewArgs, AgentGuiPendingUpdatesArgs,
    AgentGuiProfilesArgs, AgentGuiQueryArgs, AgentGuiRepositoriesArgs, AgentGuiResizeArgs,
    AgentGuiRestoreArgs, AgentGuiScaleArgs, AgentGuiScenarioArgs, AgentGuiScreenshotArgs,
    AgentGuiScrollArgs, AgentGuiSelectArgs, AgentGuiSetFilterArgs, AgentGuiSetSettingArgs,
    AgentGuiSettleArgs, AgentGuiSnapshotArgs, AgentGuiSpacesArgs, AgentGuiStableRenderArgs,
    AgentGuiTextArgs, AgentGuiTypeArgs, AgentGuiWaitArgs, AgentGuiWindowArgs, CliArgs,
};
use crate::cli::exit_codes;
use crate::cli::toon;
use crate::ui::app::Foxy;
use crate::ui::app::agent_driver::{
    AgentGuiCommand as DriverCommand, AgentGuiModifiers, AgentGuiResponse, AgentGuiWaitCondition,
    AgentGuiWireRequest, read_session, send_command_to_session,
};

use super::{CommandError, CommandSuccess};

/// Read timeout for the persistent `exec` connection. Generous so a long
/// `wait`/`download-complete` step does not trip it.
const EXEC_READ_TIMEOUT: Duration = Duration::from_secs(180);

pub fn run_agent_gui_command(
    cli: &CliArgs,
    command: AgentGuiCommand,
) -> Result<CommandSuccess, CommandError> {
    // Client-only orchestration commands never reach the driver.
    match command {
        AgentGuiCommand::Exec(args) => run_exec(cli, args),
        AgentGuiCommand::Scenario(args) => run_scenario(cli, args),
        AgentGuiCommand::Fixture(args) => run_fixture(args),
        other => run_single_driver_command(cli, other),
    }
}

fn run_single_driver_command(
    cli: &CliArgs,
    command: AgentGuiCommand,
) -> Result<CommandSuccess, CommandError> {
    let action = format!("agent-gui.{}", command_name(&command));
    let driver_command = to_driver_command(command, cli.toon)?;
    let session = read_session(&Foxy::get_config_directory()).map_err(|e| {
        CommandError::operation(
            &action,
            format!("Could not load agent GUI session. Start Foxy with `ui --agent-gui`: {e}"),
        )
    })?;
    let response = send_command_to_session(&session, driver_command)
        .map_err(|e| CommandError::operation(&action, e))?;
    response_to_success(&action, response, cli.flat, cli.field.as_deref())
}

fn response_to_success(
    action: &str,
    response: AgentGuiResponse,
    flat: bool,
    field: Option<&str>,
) -> Result<CommandSuccess, CommandError> {
    if !response.ok {
        let message = response
            .errors
            .first()
            .map(|err| err.message.clone())
            .unwrap_or_else(|| "Agent GUI command failed".to_string());
        return Err(CommandError {
            action: action.to_string(),
            message,
            code: exit_codes::OPERATION_FAILED,
        });
    }

    let data = project_response_data(&response, flat, field).map_err(|e| {
        CommandError::operation(action, format!("Failed to project driver response: {e}"))
    })?;

    Ok(CommandSuccess {
        action: action.to_string(),
        message: human_message(&response),
        data,
        exit_code: exit_codes::SUCCESS,
    })
}

/// Apply client-side `--flat` / `--field` projection. Default keeps the current
/// double-envelope shape (the full `AgentGuiResponse`) so existing recipes work.
fn project_response_data(
    response: &AgentGuiResponse,
    flat: bool,
    field: Option<&str>,
) -> Result<Value, serde_json::Error> {
    if let Some(path) = field {
        // `--field` projects into the inner payload (and implies flat).
        return Ok(dotted_lookup(&response.data, path).unwrap_or(Value::Null));
    }
    if flat {
        return Ok(response.data.clone());
    }
    serde_json::to_value(response)
}

/// Look up a dotted path (`nodes.0.rect`) inside a JSON value via JSON Pointer.
fn dotted_lookup(value: &Value, dotted: &str) -> Option<Value> {
    if dotted.is_empty() {
        return Some(value.clone());
    }
    let pointer = format!("/{}", dotted.split('.').collect::<Vec<_>>().join("/"));
    value.pointer(&pointer).cloned()
}

fn parse_document_value(
    action: &str,
    raw: &str,
    json_message: impl Fn(&serde_json::Error) -> String,
    toon_enabled: bool,
) -> Result<Value, CommandError> {
    if toon_enabled {
        match toon::decode(raw) {
            Ok(value) => return Ok(value),
            Err(toon_err) => {
                return serde_json::from_str::<Value>(raw).map_err(|json_err| {
                    CommandError::validation(
                        action,
                        format!(
                            "{}; TOON decode also failed: {toon_err}",
                            json_message(&json_err)
                        ),
                    )
                });
            }
        }
    }

    serde_json::from_str::<Value>(raw)
        .map_err(|e| CommandError::validation(action, json_message(&e)))
}

fn path_has_toon_extension(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toon"))
}

fn human_message(response: &AgentGuiResponse) -> String {
    match response.command.as_str() {
        "screenshot" => response
            .data
            .get("screenshot_path")
            .and_then(Value::as_str)
            .map(|path| format!("Screenshot written to {path}"))
            .unwrap_or_else(|| "Screenshot captured".to_string()),
        "status" => format!("Foxy agent GUI is running on view {}", response.view),
        "health" => response
            .data
            .get("version_label")
            .and_then(Value::as_str)
            .map(|v| format!("Foxy {v} healthy"))
            .unwrap_or_else(|| "Foxy agent GUI healthy".to_string()),
        "open-view" => format!("Opened view {}", response.view),
        "logs" => count_message(response, "count", "log entries"),
        "repositories" => count_message(response, "returned", "repositories"),
        "addons" => count_message(response, "returned", "addons"),
        "inventory" => count_message(response, "returned", "inventory addons"),
        "backups" => count_message(response, "returned", "backups"),
        "scale" => response
            .data
            .get("ui_scale_percent")
            .and_then(Value::as_u64)
            .map(|percent| format!("UI scale set to {percent}%"))
            .unwrap_or_else(|| "UI scale updated".to_string()),
        "resize" => "Window resize requested".to_string(),
        "profiles" => count_message(response, "returned", "profiles"),
        "missions" => count_message(response, "returned", "missions"),
        "spaces" => count_message(response, "returned", "repository spaces"),
        "arma-profiles" => count_message(response, "total", "Arma 3 profiles"),
        "download-summary" => match response.data.get("present").and_then(Value::as_bool) {
            Some(true) => "Returned the last download summary".to_string(),
            _ => "No download summary recorded yet".to_string(),
        },
        "app-update" => response
            .data
            .get("status")
            .and_then(Value::as_str)
            .map(|status| format!("App update status: {status}"))
            .unwrap_or_else(|| "Returned app update status".to_string()),
        "pending-updates" => response
            .data
            .get("total_mod_count")
            .and_then(Value::as_u64)
            .map(|count| format!("{count} mods pending update"))
            .unwrap_or_else(|| "Returned pending updates".to_string()),
        "toasts" => match response.data.get("present").and_then(Value::as_bool) {
            Some(true) => response
                .data
                .get("message")
                .and_then(Value::as_str)
                .map(|message| format!("Toast showing: {message}"))
                .unwrap_or_else(|| "Toast showing".to_string()),
            _ => "No toast showing".to_string(),
        },
        "set-setting" => response
            .data
            .get("key")
            .and_then(Value::as_str)
            .map(|key| format!("Setting {key} updated"))
            .unwrap_or_else(|| "Setting updated".to_string()),
        "set-filter" => response
            .data
            .get("filter")
            .and_then(Value::as_str)
            .map(|filter| format!("Filter {filter} updated"))
            .unwrap_or_else(|| "Filter updated".to_string()),
        "fill" => response
            .data
            .get("target")
            .and_then(Value::as_str)
            .map(|target| format!("Filled {target}"))
            .unwrap_or_else(|| "Filled text field".to_string()),
        "focus" => response
            .data
            .get("focused")
            .and_then(Value::as_str)
            .map(|focus| format!("Focused {focus}"))
            .unwrap_or_else(|| "Focus updated".to_string()),
        "assert" => match response.data.get("ok").and_then(Value::as_bool) {
            Some(true) => "Assertion passed".to_string(),
            _ => "Assertion failed".to_string(),
        },
        "window" => response
            .data
            .get("action")
            .and_then(Value::as_str)
            .map(|action| format!("Window {action} requested"))
            .unwrap_or_else(|| "Window action requested".to_string()),
        _ => format!("Agent GUI command {} completed", response.command),
    }
}

fn count_message(response: &AgentGuiResponse, key: &str, noun: &str) -> String {
    response
        .data
        .get(key)
        .and_then(Value::as_u64)
        .map(|count| format!("Returned {count} {noun}"))
        .unwrap_or_else(|| format!("Returned {noun}"))
}

fn command_name(command: &AgentGuiCommand) -> &'static str {
    match command {
        AgentGuiCommand::Status => "status",
        AgentGuiCommand::OpenView(_) => "open-view",
        AgentGuiCommand::Snapshot(_) => "snapshot",
        AgentGuiCommand::Text(_) => "text",
        AgentGuiCommand::Find(_) => "find",
        AgentGuiCommand::Click(_) => "click",
        AgentGuiCommand::Scroll(_) => "scroll",
        AgentGuiCommand::Hover(_) => "hover",
        AgentGuiCommand::MouseDown(_) => "mouse-down",
        AgentGuiCommand::MouseUp(_) => "mouse-up",
        AgentGuiCommand::Key(_) => "key",
        AgentGuiCommand::Type(_) => "type",
        AgentGuiCommand::Screenshot(_) => "screenshot",
        AgentGuiCommand::Fps => "fps",
        AgentGuiCommand::Wait(_) => "wait",
        AgentGuiCommand::Logs(_) => "logs",
        AgentGuiCommand::Repositories(_) => "repositories",
        AgentGuiCommand::Addons(_) => "addons",
        AgentGuiCommand::Settings => "settings",
        AgentGuiCommand::Progress => "progress",
        AgentGuiCommand::Scale(_) => "scale",
        AgentGuiCommand::Resize(_) => "resize",
        AgentGuiCommand::Profiles(_) => "profiles",
        AgentGuiCommand::Missions(_) => "missions",
        AgentGuiCommand::Spaces(_) => "spaces",
        AgentGuiCommand::DownloadSummary(_) => "download-summary",
        AgentGuiCommand::Toasts => "toasts",
        AgentGuiCommand::SetSetting(_) => "set-setting",
        AgentGuiCommand::Health => "health",
        AgentGuiCommand::Focus(_) => "focus",
        AgentGuiCommand::Nav(_) => "nav",
        AgentGuiCommand::Fill(_) => "fill",
        AgentGuiCommand::Filters => "filters",
        AgentGuiCommand::SetFilter(_) => "set-filter",
        AgentGuiCommand::Select(_) => "select",
        AgentGuiCommand::Window(_) => "window",
        AgentGuiCommand::Settle(_) => "settle",
        AgentGuiCommand::StableRender(_) => "stable-render",
        AgentGuiCommand::Assert(_) => "assert",
        AgentGuiCommand::ContextMenu(_) => "context-menu",
        AgentGuiCommand::MenuSelect(_) => "menu-select",
        AgentGuiCommand::Inventory(_) => "inventory",
        AgentGuiCommand::PendingUpdates(_) => "pending-updates",
        AgentGuiCommand::AppUpdate => "app-update",
        AgentGuiCommand::Memory(_) => "memory",
        AgentGuiCommand::ArmaProfiles => "arma-profiles",
        AgentGuiCommand::Backups(_) => "backups",
        AgentGuiCommand::Invoke(_) => "invoke",
        AgentGuiCommand::Batch(_) => "batch",
        AgentGuiCommand::Diff(_) => "diff",
        AgentGuiCommand::Drag(_) => "drag",
        AgentGuiCommand::Query(_) => "query",
        AgentGuiCommand::Checkpoint(_) => "checkpoint",
        AgentGuiCommand::Restore(_) => "restore",
        AgentGuiCommand::Element(_) => "element",
        AgentGuiCommand::Events(_) => "events",
        AgentGuiCommand::Clock(_) => "clock",
        AgentGuiCommand::Dialog(_) => "dialog",
        AgentGuiCommand::Exec(_) => "exec",
        AgentGuiCommand::Scenario(_) => "scenario",
        AgentGuiCommand::Fixture(_) => "fixture",
        AgentGuiCommand::Close => "close",
    }
}

fn to_driver_command(
    command: AgentGuiCommand,
    toon_input: bool,
) -> Result<DriverCommand, CommandError> {
    Ok(match command {
        AgentGuiCommand::Status => DriverCommand::Status,
        AgentGuiCommand::OpenView(AgentGuiOpenViewArgs {
            view,
            repo_index,
            tab,
        }) => DriverCommand::OpenView {
            view,
            repository_index: repo_index,
            tab,
        },
        AgentGuiCommand::Snapshot(AgentGuiSnapshotArgs {
            fields,
            since_frame,
        }) => DriverCommand::Snapshot {
            fields,
            since_frame,
        },
        AgentGuiCommand::Text(AgentGuiTextArgs { contains, limit }) => {
            DriverCommand::Text { contains, limit }
        }
        AgentGuiCommand::Find(AgentGuiFindArgs {
            text,
            role,
            id,
            visible_only,
        }) => DriverCommand::Find {
            text,
            role,
            id,
            visible_only,
        },
        AgentGuiCommand::Click(AgentGuiClickArgs {
            text,
            id,
            x,
            y,
            button,
            double,
            modifiers,
        }) => DriverCommand::Click {
            text,
            id,
            x,
            y,
            modifiers: to_driver_modifiers(modifiers),
            button,
            double,
        },
        AgentGuiCommand::Scroll(AgentGuiScrollArgs {
            id,
            x,
            y,
            dx,
            dy,
            modifiers,
        }) => DriverCommand::Scroll {
            id,
            x,
            y,
            dx,
            dy,
            modifiers: to_driver_modifiers(modifiers),
        },
        AgentGuiCommand::Hover(AgentGuiHoverArgs { id, x, y }) => DriverCommand::Hover { id, x, y },
        AgentGuiCommand::MouseDown(AgentGuiMouseArgs {
            id,
            x,
            y,
            button,
            modifiers,
        }) => DriverCommand::MouseDown {
            id,
            x,
            y,
            modifiers: to_driver_modifiers(modifiers),
            button,
        },
        AgentGuiCommand::MouseUp(AgentGuiMouseArgs {
            id,
            x,
            y,
            button,
            modifiers,
        }) => DriverCommand::MouseUp {
            id,
            x,
            y,
            modifiers: to_driver_modifiers(modifiers),
            button,
        },
        AgentGuiCommand::Key(AgentGuiKeyArgs { key, modifiers }) => DriverCommand::Key {
            key,
            modifiers: to_driver_modifiers(modifiers),
        },
        AgentGuiCommand::Type(AgentGuiTypeArgs { text }) => DriverCommand::Type { text },
        AgentGuiCommand::Screenshot(AgentGuiScreenshotArgs { output, annotate }) => {
            DriverCommand::Screenshot {
                output: absolute_output_path(output)?,
                annotate,
            }
        }
        AgentGuiCommand::Fps => DriverCommand::Fps,
        AgentGuiCommand::Wait(args) => {
            let timeout_ms = args.timeout_ms;
            DriverCommand::Wait {
                condition: wait_condition(args)?,
                timeout_ms,
            }
        }
        AgentGuiCommand::Logs(AgentGuiLogsArgs {
            level,
            contains,
            limit,
            since_generation,
        }) => DriverCommand::Logs {
            level,
            contains,
            limit,
            since_generation,
        },
        AgentGuiCommand::Repositories(AgentGuiRepositoriesArgs { contains, limit }) => {
            DriverCommand::Repositories { contains, limit }
        }
        AgentGuiCommand::Addons(AgentGuiAddonsArgs {
            repo_index,
            tab,
            contains,
            enabled_only,
            limit,
        }) => DriverCommand::Addons {
            repository_index: repo_index,
            tab,
            contains,
            enabled_only,
            limit,
        },
        AgentGuiCommand::Settings => DriverCommand::Settings,
        AgentGuiCommand::Progress => DriverCommand::Progress,
        AgentGuiCommand::Scale(AgentGuiScaleArgs { percent }) => DriverCommand::Scale { percent },
        AgentGuiCommand::Resize(AgentGuiResizeArgs { width, height }) => {
            DriverCommand::Resize { width, height }
        }
        AgentGuiCommand::Profiles(AgentGuiProfilesArgs {
            repo_index,
            contains,
            limit,
        }) => DriverCommand::Profiles {
            repository_index: repo_index,
            contains,
            limit,
        },
        AgentGuiCommand::Missions(AgentGuiMissionsArgs { contains, limit }) => {
            DriverCommand::Missions { contains, limit }
        }
        AgentGuiCommand::Spaces(AgentGuiSpacesArgs { contains, limit }) => {
            DriverCommand::Spaces { contains, limit }
        }
        AgentGuiCommand::DownloadSummary(AgentGuiDownloadSummaryArgs { include_telemetry }) => {
            DriverCommand::DownloadSummary { include_telemetry }
        }
        AgentGuiCommand::Toasts => DriverCommand::Toasts,
        AgentGuiCommand::SetSetting(AgentGuiSetSettingArgs { key, value }) => {
            DriverCommand::SetSetting { key, value }
        }
        AgentGuiCommand::Health => DriverCommand::Health,
        AgentGuiCommand::Focus(AgentGuiFocusArgs { target, clear }) => {
            DriverCommand::Focus { target, clear }
        }
        AgentGuiCommand::Nav(AgentGuiNavArgs { count, reverse }) => {
            DriverCommand::Nav { count, reverse }
        }
        AgentGuiCommand::Fill(AgentGuiFillArgs { target, value }) => {
            DriverCommand::Fill { target, value }
        }
        AgentGuiCommand::Filters => DriverCommand::Filters,
        AgentGuiCommand::SetFilter(AgentGuiSetFilterArgs { name, value }) => {
            DriverCommand::SetFilter { name, value }
        }
        AgentGuiCommand::Select(AgentGuiSelectArgs {
            repository,
            server,
            mission,
            space,
        }) => DriverCommand::Select {
            repository,
            server,
            mission,
            space,
        },
        AgentGuiCommand::Window(AgentGuiWindowArgs { action }) => DriverCommand::Window { action },
        AgentGuiCommand::Settle(AgentGuiSettleArgs { frames }) => DriverCommand::Settle { frames },
        AgentGuiCommand::StableRender(AgentGuiStableRenderArgs { on }) => {
            DriverCommand::StableRender { on }
        }
        AgentGuiCommand::Assert(AgentGuiAssertArgs {
            field,
            equals,
            contains,
            repository_index,
        }) => DriverCommand::Assert {
            field,
            equals,
            contains,
            repository_index,
        },
        AgentGuiCommand::ContextMenu(AgentGuiContextMenuArgs { id, x, y }) => {
            DriverCommand::ContextMenu { id, x, y }
        }
        AgentGuiCommand::MenuSelect(AgentGuiMenuSelectArgs { item }) => {
            DriverCommand::MenuSelect { item }
        }
        AgentGuiCommand::Inventory(AgentGuiInventoryArgs {
            contains,
            folder,
            source,
            limit,
        }) => DriverCommand::Inventory {
            contains,
            folder,
            source,
            limit,
        },
        AgentGuiCommand::PendingUpdates(AgentGuiPendingUpdatesArgs {
            repo_index,
            contains,
            limit,
            include_files,
        }) => DriverCommand::PendingUpdates {
            repository_index: repo_index,
            contains,
            limit,
            include_files,
        },
        AgentGuiCommand::AppUpdate => DriverCommand::AppUpdate,
        AgentGuiCommand::Memory(AgentGuiMemoryArgs { history, textures }) => {
            DriverCommand::Memory { history, textures }
        }
        AgentGuiCommand::ArmaProfiles => DriverCommand::ArmaProfiles,
        AgentGuiCommand::Backups(AgentGuiBackupsArgs { contains, limit }) => {
            DriverCommand::Backups { contains, limit }
        }
        AgentGuiCommand::Invoke(AgentGuiInvokeArgs {
            action,
            repo_index,
            profile,
            params,
            allow_destructive,
            list_actions,
        }) => {
            let mut params_value = match params {
                Some(raw) => parse_document_value(
                    "agent-gui.invoke",
                    &raw,
                    |e| format!("--params must be a JSON object: {e}"),
                    toon_input,
                )?,
                None => Value::Object(serde_json::Map::new()),
            };
            // Merge the convenience flags into the params object.
            if let Value::Object(map) = &mut params_value {
                if let Some(index) = repo_index {
                    map.insert("repo-index".to_string(), serde_json::json!(index));
                }
                if let Some(name) = profile {
                    map.insert("profile".to_string(), serde_json::json!(name));
                }
            }
            DriverCommand::Invoke {
                action,
                params: params_value,
                allow_destructive,
                list_actions,
            }
        }
        AgentGuiCommand::Batch(AgentGuiBatchArgs {
            stdin,
            steps,
            stop_on_error,
        }) => {
            let raw = if stdin {
                let mut buffer = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer).map_err(|e| {
                    CommandError::operation("agent-gui.batch", format!("Failed to read stdin: {e}"))
                })?;
                buffer
            } else {
                steps.ok_or_else(|| {
                    CommandError::validation(
                        "agent-gui.batch",
                        "Provide --stdin or --steps <json-array>",
                    )
                })?
            };
            let value = parse_document_value(
                "agent-gui.batch",
                &raw,
                |e| format!("Batch must be a JSON array of command objects: {e}"),
                toon_input,
            )?;
            let steps: Vec<DriverCommand> = serde_json::from_value(value).map_err(|e| {
                CommandError::validation(
                    "agent-gui.batch",
                    format!("Batch must be an array of command objects: {e}"),
                )
            })?;
            DriverCommand::Batch {
                steps,
                stop_on_error,
            }
        }
        AgentGuiCommand::Diff(AgentGuiDiffArgs { baseline }) => DriverCommand::Diff { baseline },
        AgentGuiCommand::Drag(AgentGuiDragArgs {
            from_id,
            from_x,
            from_y,
            to_id,
            to_x,
            to_y,
            steps,
            button,
        }) => DriverCommand::Drag {
            from_id,
            from_x,
            from_y,
            to_id,
            to_x,
            to_y,
            steps,
            button,
        },
        AgentGuiCommand::Query(AgentGuiQueryArgs { expr }) => DriverCommand::Query { expr },
        AgentGuiCommand::Checkpoint(AgentGuiCheckpointArgs { name, list }) => {
            DriverCommand::Checkpoint { name, list }
        }
        AgentGuiCommand::Restore(AgentGuiRestoreArgs { name }) => DriverCommand::Restore { name },
        AgentGuiCommand::Element(AgentGuiElementArgs { id, x, y }) => {
            DriverCommand::Element { id, x, y }
        }
        AgentGuiCommand::Events(AgentGuiEventsArgs {
            kinds,
            since,
            limit,
        }) => DriverCommand::Events {
            kinds,
            since,
            limit,
        },
        AgentGuiCommand::Clock(AgentGuiClockArgs { action, ms }) => {
            DriverCommand::Clock { action, ms }
        }
        AgentGuiCommand::Dialog(AgentGuiDialogArgs {
            action,
            path,
            cancel,
        }) => DriverCommand::Dialog {
            action,
            path,
            cancel,
        },
        AgentGuiCommand::Close => DriverCommand::Close,
        // Client-only commands are intercepted before this point.
        AgentGuiCommand::Exec(_) | AgentGuiCommand::Scenario(_) | AgentGuiCommand::Fixture(_) => {
            return Err(CommandError::validation(
                "agent-gui",
                "exec/scenario/fixture are client-side commands and cannot be sent to the driver",
            ));
        }
    })
}

fn to_driver_modifiers(args: AgentGuiModifierArgs) -> AgentGuiModifiers {
    AgentGuiModifiers {
        ctrl: args.ctrl,
        shift: args.shift,
        alt: args.alt,
        command: args.command,
    }
}

fn absolute_output_path(path: PathBuf) -> Result<PathBuf, CommandError> {
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|e| {
            CommandError::operation(
                "agent-gui.screenshot",
                format!("Failed to resolve current directory: {e}"),
            )
        })
}

fn wait_condition(args: AgentGuiWaitArgs) -> Result<AgentGuiWaitCondition, CommandError> {
    if let Some(text) = args.text {
        return Ok(AgentGuiWaitCondition::Text { text });
    }
    if let Some(view) = args.view {
        return Ok(AgentGuiWaitCondition::View { view });
    }
    if args.idle {
        return Ok(AgentGuiWaitCondition::Idle);
    }
    if args.modal_open {
        return Ok(AgentGuiWaitCondition::Modal { open: true });
    }
    if args.modal_closed {
        return Ok(AgentGuiWaitCondition::Modal { open: false });
    }
    if let Some(text) = args.toast {
        return Ok(AgentGuiWaitCondition::Toast { text });
    }
    if let Some(reason) = args.busy_reason_cleared {
        return Ok(AgentGuiWaitCondition::BusyReasonCleared { reason });
    }
    if args.download_complete {
        return Ok(AgentGuiWaitCondition::DownloadComplete);
    }
    if let Some(fps) = args.fps_above {
        return Ok(AgentGuiWaitCondition::FpsAbove { fps });
    }
    if let Some(id) = args.node_visible {
        return Ok(AgentGuiWaitCondition::NodeVisible { id });
    }
    Err(CommandError::validation(
        "agent-gui.wait",
        "Provide --text, --view, --idle, --modal-open, --modal-closed, --toast, --busy-reason-cleared, --download-complete, --fps-above, or --node-visible",
    ))
}

// ── Client-only orchestration commands ─────────────────────────────────────

/// Persistent connection: stream newline-delimited command JSON from stdin over
/// one socket, printing one response line per command. Amortizes the
/// per-process connect + token handshake across a whole interactive session.
fn run_exec(cli: &CliArgs, args: AgentGuiExecArgs) -> Result<CommandSuccess, CommandError> {
    let action = "agent-gui.exec".to_string();
    if !args.stdin {
        return Err(CommandError::validation(
            &action,
            "exec currently requires --stdin",
        ));
    }
    let session = read_session(&Foxy::get_config_directory()).map_err(|e| {
        CommandError::operation(
            &action,
            format!("Could not load agent GUI session. Start Foxy with `ui --agent-gui`: {e}"),
        )
    })?;
    let stream = TcpStream::connect((session.host.as_str(), session.port))
        .map_err(|e| CommandError::operation(&action, format!("Failed to connect: {e}")))?;
    stream.set_read_timeout(Some(EXEC_READ_TIMEOUT)).ok();
    let mut writer = stream
        .try_clone()
        .map_err(|e| CommandError::operation(&action, format!("Failed to clone stream: {e}")))?;
    let mut reader = BufReader::new(stream);

    let stdin = std::io::stdin();
    let mut count: u64 = 0;
    for line in stdin.lock().lines() {
        let line = line
            .map_err(|e| CommandError::operation(&action, format!("Failed to read stdin: {e}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let command: DriverCommand = serde_json::from_str(trimmed).map_err(|e| {
            CommandError::validation(&action, format!("Invalid command JSON '{trimmed}': {e}"))
        })?;
        let request = AgentGuiWireRequest {
            token: session.token.clone(),
            command,
        };
        let wire = serde_json::to_string(&request).map_err(|e| {
            CommandError::operation(&action, format!("Failed to serialize request: {e}"))
        })?;
        writer
            .write_all(wire.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .map_err(|e| CommandError::operation(&action, format!("Failed to write: {e}")))?;

        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .map_err(|e| CommandError::operation(&action, format!("Failed to read: {e}")))?;
        if response_line.trim().is_empty() {
            return Err(CommandError::operation(
                &action,
                "Driver closed the connection",
            ));
        }
        // Print each response immediately so a long-lived caller can interleave
        // reads and reactions; honor --flat/--field per line.
        let printed = project_exec_line(
            response_line.trim(),
            cli.flat,
            cli.field.as_deref(),
            cli.toon,
        );
        println!("{printed}");
        count += 1;
    }

    Ok(CommandSuccess {
        action,
        message: format!("exec session complete ({count} commands)"),
        data: serde_json::json!({ "commands": count }),
        exit_code: exit_codes::SUCCESS,
    })
}

/// Project one streamed exec response line for `--flat`/`--field`. Falls back to
/// the raw line if it cannot be parsed (so the caller still sees the driver
/// output verbatim).
fn project_exec_line(line: &str, flat: bool, field: Option<&str>, toon_output: bool) -> String {
    if !flat && field.is_none() && !toon_output {
        return line.to_string();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    let projected = if flat || field.is_some() {
        let inner = value.get("data").cloned().unwrap_or(Value::Null);
        match field {
            Some(path) => dotted_lookup(&inner, path).unwrap_or(Value::Null),
            None => inner,
        }
    } else {
        value
    };
    if toon_output {
        return toon::encode(&projected).unwrap_or_else(|_| line.to_string());
    }
    serde_json::to_string(&projected).unwrap_or_else(|_| line.to_string())
}

/// Run a scenario file: an array of driver-command step objects executed in
/// order, returning a structured pass/fail transcript. A step fails when the
/// driver reports `ok:false`, or when an `assert` step's payload reports
/// `ok:false`.
fn run_scenario(cli: &CliArgs, args: AgentGuiScenarioArgs) -> Result<CommandSuccess, CommandError> {
    let action = "agent-gui.scenario".to_string();
    let content = std::fs::read_to_string(&args.file).map_err(|e| {
        CommandError::operation(
            &action,
            format!("Failed to read {}: {e}", args.file.display()),
        )
    })?;
    let toon_input = cli.toon || path_has_toon_extension(&args.file);
    let value = parse_document_value(
        &action,
        &content,
        |e| format!("Scenario must be a JSON array of command objects: {e}"),
        toon_input,
    )?;
    let steps: Vec<DriverCommand> = serde_json::from_value(value).map_err(|e| {
        CommandError::validation(
            &action,
            format!("Scenario must be an array of command objects: {e}"),
        )
    })?;
    let session = read_session(&Foxy::get_config_directory()).map_err(|e| {
        CommandError::operation(
            &action,
            format!("Could not load agent GUI session. Start Foxy with `ui --agent-gui`: {e}"),
        )
    })?;

    let mut transcript: Vec<Value> = Vec::new();
    let mut failures = 0usize;
    for (index, step) in steps.into_iter().enumerate() {
        let name = step.name().to_string();
        match send_command_to_session(&session, step) {
            Ok(response) => {
                // `assert` carries its result inside the payload.
                let step_ok = response.ok
                    && response
                        .data
                        .get("ok")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);
                if !step_ok {
                    failures += 1;
                }
                transcript.push(serde_json::json!({
                    "step": index,
                    "command": name,
                    "ok": step_ok,
                    "data": response.data,
                    "errors": response.errors,
                }));
            }
            Err(message) => {
                failures += 1;
                transcript.push(serde_json::json!({
                    "step": index,
                    "command": name,
                    "ok": false,
                    "error": message,
                }));
            }
        }
    }

    let passed = failures == 0;
    let data = serde_json::json!({
        "ok": passed,
        "total": transcript.len(),
        "failures": failures,
        "steps": transcript,
    });
    if passed {
        Ok(CommandSuccess {
            action,
            message: format!("Scenario passed ({} steps)", data["total"]),
            data,
            exit_code: exit_codes::SUCCESS,
        })
    } else {
        Err(CommandError {
            action,
            message: format!("Scenario failed: {failures} step(s) did not pass"),
            code: exit_codes::OPERATION_FAILED,
        })
    }
}

/// Allowed config basenames a fixture may write. Never the multi-GB database or
/// anything outside the isolated config dir (harness security rule).
const FIXTURE_ALLOWED_FILES: &[&str] = &[
    "settings.json",
    "repositories.json",
    "repository_spaces.json",
];

/// Seed the isolated `FOXY_CONFIG_DIR` with a known set of small JSON config
/// files. Format: `{ "files": { "settings.json": <json>, ... } }`.
fn run_fixture(args: AgentGuiFixtureArgs) -> Result<CommandSuccess, CommandError> {
    let action = "agent-gui.fixture".to_string();
    let content = std::fs::read_to_string(&args.file).map_err(|e| {
        CommandError::operation(
            &action,
            format!("Failed to read {}: {e}", args.file.display()),
        )
    })?;
    let spec: Value = serde_json::from_str(&content)
        .map_err(|e| CommandError::validation(&action, format!("Invalid fixture JSON: {e}")))?;
    let files = spec
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CommandError::validation(&action, "Fixture must have a top-level `files` object")
        })?;

    let config_dir = Foxy::get_config_directory();
    std::fs::create_dir_all(&config_dir).map_err(|e| {
        CommandError::operation(&action, format!("Failed to create config dir: {e}"))
    })?;

    let mut written: Vec<String> = Vec::new();
    for (name, value) in files {
        if !FIXTURE_ALLOWED_FILES.contains(&name.as_str()) {
            return Err(CommandError::validation(
                &action,
                format!(
                    "Fixture file '{name}' is not allowed (only {})",
                    FIXTURE_ALLOWED_FILES.join(", ")
                ),
            ));
        }
        let serialized = serde_json::to_string_pretty(value).map_err(|e| {
            CommandError::operation(&action, format!("Failed to serialize {name}: {e}"))
        })?;
        let path = config_dir.join(name);
        std::fs::write(&path, serialized).map_err(|e| {
            CommandError::operation(&action, format!("Failed to write {}: {e}", path.display()))
        })?;
        written.push(name.clone());
    }

    Ok(CommandSuccess {
        action,
        message: format!("Seeded {} fixture file(s)", written.len()),
        data: serde_json::json!({ "config_dir": config_dir, "written": written }),
        exit_code: exit_codes::SUCCESS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_wait_args() -> AgentGuiWaitArgs {
        AgentGuiWaitArgs {
            text: None,
            view: None,
            idle: false,
            modal_open: false,
            modal_closed: false,
            toast: None,
            busy_reason_cleared: None,
            download_complete: false,
            fps_above: None,
            node_visible: None,
            timeout_ms: 1000,
        }
    }

    #[test]
    fn wait_condition_prefers_text() {
        let condition = wait_condition(AgentGuiWaitArgs {
            text: Some("Settings".to_string()),
            ..empty_wait_args()
        })
        .unwrap();

        assert!(matches!(condition, AgentGuiWaitCondition::Text { .. }));
    }

    #[test]
    fn wait_condition_supports_modal_state() {
        let condition = wait_condition(AgentGuiWaitArgs {
            modal_open: true,
            ..empty_wait_args()
        })
        .unwrap();

        assert!(matches!(
            condition,
            AgentGuiWaitCondition::Modal { open: true }
        ));
    }

    #[test]
    fn wait_condition_supports_new_predicates() {
        assert!(matches!(
            wait_condition(AgentGuiWaitArgs {
                toast: Some("Saved".to_string()),
                ..empty_wait_args()
            })
            .unwrap(),
            AgentGuiWaitCondition::Toast { .. }
        ));
        assert!(matches!(
            wait_condition(AgentGuiWaitArgs {
                download_complete: true,
                ..empty_wait_args()
            })
            .unwrap(),
            AgentGuiWaitCondition::DownloadComplete
        ));
    }

    #[test]
    fn response_failure_becomes_command_error() {
        let response = AgentGuiResponse {
            ok: false,
            command: "status".to_string(),
            view: "settings".to_string(),
            elapsed_ms: 1,
            data: serde_json::json!({}),
            errors: vec![crate::ui::app::agent_driver::AgentGuiError {
                code: "not-found".to_string(),
                message: "No session".to_string(),
            }],
        };

        let err = response_to_success("agent-gui.status", response, false, None).unwrap_err();

        assert_eq!(err.message, "No session");
    }

    #[test]
    fn flat_projection_drops_outer_envelope() {
        let response = AgentGuiResponse {
            ok: true,
            command: "snapshot".to_string(),
            view: "settings".to_string(),
            elapsed_ms: 1,
            data: serde_json::json!({ "view": "settings", "fps": 60.0 }),
            errors: Vec::new(),
        };
        let data = project_response_data(&response, true, None).unwrap();
        assert_eq!(data["view"], "settings");
        assert!(data.get("command").is_none());
    }

    #[test]
    fn field_projection_extracts_dotted_path() {
        let response = AgentGuiResponse {
            ok: true,
            command: "snapshot".to_string(),
            view: "settings".to_string(),
            elapsed_ms: 1,
            data: serde_json::json!({ "nodes": [{ "rect": { "x": 12.0 } }] }),
            errors: Vec::new(),
        };
        let data = project_response_data(&response, false, Some("nodes.0.rect.x")).unwrap();
        assert_eq!(data, serde_json::json!(12.0));
    }

    #[test]
    fn exec_line_toon_encodes_full_response() {
        let line = r#"{"ok":true,"command":"snapshot","view":"settings","elapsed_ms":1,"data":{"view":"settings","nodes":[{"id":"footer.settings","rect":{"x":12.5,"y":8.25}}]},"errors":[]}"#;

        let encoded = project_exec_line(line, false, None, true);
        let decoded = toon::decode(&encoded).unwrap();

        assert_eq!(decoded["ok"], true);
        assert_eq!(decoded["data"]["view"], "settings");
        assert_eq!(decoded["data"]["nodes"][0]["rect"]["x"], 12.5);
    }

    #[test]
    fn exec_line_toon_respects_field_projection() {
        let line = r#"{"ok":true,"command":"snapshot","view":"settings","elapsed_ms":1,"data":{"nodes":[{"rect":{"x":12.5}}]},"errors":[]}"#;

        let encoded = project_exec_line(line, false, Some("nodes.0.rect.x"), true);
        let decoded = toon::decode(&encoded).unwrap();

        assert_eq!(decoded, serde_json::json!(12.5));
    }

    #[test]
    fn toon_enabled_document_parser_falls_back_to_json() {
        let raw = r#"[{"command":"health"}]"#;

        let parsed = parse_document_value(
            "agent-gui.batch",
            raw,
            |e| format!("Batch must be a JSON array of command objects: {e}"),
            true,
        )
        .unwrap();

        assert_eq!(parsed[0]["command"], "health");
    }

    #[test]
    fn toon_enabled_document_parser_accepts_toon() {
        let raw = "[1]{command}:\n  health";

        let parsed = parse_document_value(
            "agent-gui.batch",
            raw,
            |e| format!("Batch must be a JSON array of command objects: {e}"),
            true,
        )
        .unwrap();

        assert_eq!(parsed[0]["command"], "health");
    }

    #[test]
    fn fixture_rejects_disallowed_files() {
        assert!(!FIXTURE_ALLOWED_FILES.contains(&"database.db"));
        assert!(FIXTURE_ALLOWED_FILES.contains(&"settings.json"));
    }
}
