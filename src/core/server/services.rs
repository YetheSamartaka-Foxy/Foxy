use std::path::Path;

use serde::Serialize;

use super::{ServerContext, ServerDefinitionKind, ServerStoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceExportFormat {
    SystemdUser,
    WindowsUserTask,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceExportPlan {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub format: ServiceExportFormat,
    pub file_name: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub definition: String,
    pub fingerprint: String,
    pub warnings: Vec<String>,
    pub registration_performed: bool,
    pub writes_performed: bool,
    pub stop_arguments: Vec<String>,
}

pub fn plan_service_export(
    context: &ServerContext,
    executable: &Path,
    format: ServiceExportFormat,
) -> Result<ServiceExportPlan, ServerStoreError> {
    let caps = crate::core::game::registry()
        .get(context.game_id())
        .ok_or(ServerStoreError::UnsupportedGame)?
        .server_capabilities();
    if !cfg!(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    )) || !caps.lifecycle
        || !matches!(
            caps.definition_kind,
            Some(ServerDefinitionKind::Arma3 | ServerDefinitionKind::Reforger)
        )
    {
        return Err(ServerStoreError::UnsupportedGame);
    }
    match format {
        ServiceExportFormat::SystemdUser if !cfg!(target_os = "linux") => {
            return Err(ServerStoreError::UnsupportedGame);
        }
        ServiceExportFormat::WindowsUserTask if !cfg!(target_os = "windows") => {
            return Err(ServerStoreError::UnsupportedGame);
        }
        _ => {}
    }
    let root =
        std::fs::canonicalize(context.data_root()).map_err(|_| ServerStoreError::UnsafePath)?;
    let executable = std::fs::canonicalize(executable).map_err(|_| ServerStoreError::UnsafePath)?;
    if !executable.is_file() {
        return Err(ServerStoreError::UnsafePath);
    }
    let root = export_path_text(&root, format)?;
    let executable = export_path_text(&executable, format)?;
    validate_text(&root)?;
    validate_text(&executable)?;
    validate_text(context.space_id())?;
    validate_absolute_path(Path::new(&executable))?;
    validate_absolute_path(Path::new(&root))?;
    let mut arguments = vec![
        "--config-dir".to_string(),
        root,
        "server".to_string(),
        "supervise".to_string(),
        "--game-space".to_string(),
        context.space_id().to_string(),
    ];
    let base_arguments = vec![
        "--config-dir".to_string(),
        arguments[1].clone(),
        "server".to_string(),
        "supervise".to_string(),
        "--game-space".to_string(),
        context.space_id().to_string(),
    ];
    let mut stop_arguments = base_arguments;
    stop_arguments.extend(["--shutdown".to_string(), "--yes".to_string()]);
    let mut warnings = vec![
        "The stop command is idle-only. Stop all server instances before using it.".to_string(),
    ];
    let (file_name, definition) = match format {
        ServiceExportFormat::SystemdUser => {
            warnings.push(
                "Systemd stops after 15 minutes; a timeout leaves save state uncertain."
                    .to_string(),
            );
            (
                "foxy-server.service".to_string(),
                render_systemd(&executable, &arguments),
            )
        }
        ServiceExportFormat::WindowsUserTask => {
            arguments.push("--background".to_string());
            warnings.push("InteractiveToken runs only after the registering user is logged in; this is not a boot-before-login service.".to_string());
            (
                "FoxyServerTask.xml".to_string(),
                render_task_xml(&executable, &arguments),
            )
        }
    };
    let fingerprint = fingerprint(
        &arguments[1],
        context.space_id(),
        context.game_id(),
        format,
        &definition,
    );
    Ok(ServiceExportPlan {
        schema_version: 1,
        game_space_id: context.space_id().to_string(),
        game_id: context.game_id().to_string(),
        format,
        file_name,
        executable,
        arguments,
        definition,
        fingerprint,
        warnings,
        registration_performed: false,
        writes_performed: false,
        stop_arguments,
    })
}

pub fn write_service_export(
    plan: &ServiceExportPlan,
    destination: &Path,
) -> Result<(), ServerStoreError> {
    validate_plan(plan)?;
    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err(ServerStoreError::UnsafePath);
    }
    let parent = destination.parent().ok_or(ServerStoreError::UnsafePath)?;
    super::store::check_directory_chain(parent)?;
    let parent_metadata =
        std::fs::symlink_metadata(parent).map_err(|_| ServerStoreError::UnsafePath)?;
    if !parent_metadata.is_dir() {
        return Err(ServerStoreError::UnsafePath);
    }
    super::store::check_regular_file(destination)?;
    match std::fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(ServerStoreError::Invalid(
                "Export destination already exists; choose a new file".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ServerStoreError::Io),
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| ServerStoreError::Io)?;
    }
    use std::io::Write;
    temp.write_all(plan.definition.as_bytes())
        .map_err(|_| ServerStoreError::Io)?;
    temp.as_file()
        .sync_all()
        .map_err(|_| ServerStoreError::Io)?;
    temp.persist_noclobber(destination).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            ServerStoreError::Invalid("Export destination already exists; choose a new file".into())
        } else {
            ServerStoreError::Io
        }
    })?;
    Ok(())
}

fn validate_plan(plan: &ServiceExportPlan) -> Result<(), ServerStoreError> {
    let base_arguments = [
        "--config-dir",
        plan.arguments
            .get(1)
            .map(String::as_str)
            .unwrap_or_default(),
        "server",
        "supervise",
        "--game-space",
        plan.game_space_id.as_str(),
    ];
    let expected_argument_count = match plan.format {
        ServiceExportFormat::SystemdUser => 6,
        ServiceExportFormat::WindowsUserTask => 7,
    };
    let valid_format_arguments = plan.arguments.len() == expected_argument_count
        && plan.arguments[..6]
            .iter()
            .map(String::as_str)
            .eq(base_arguments)
        && match plan.format {
            ServiceExportFormat::SystemdUser => true,
            ServiceExportFormat::WindowsUserTask => plan.arguments[6] == "--background",
        };
    let expected_definition = match plan.format {
        ServiceExportFormat::SystemdUser => render_systemd(&plan.executable, &plan.arguments),
        ServiceExportFormat::WindowsUserTask => render_task_xml(&plan.executable, &plan.arguments),
    };
    if plan.schema_version != 1
        || plan.registration_performed
        || plan.writes_performed
        || plan.definition.len() > 64 * 1024
        || !valid_format_arguments
        || plan.stop_arguments.len() != 8
        || plan.stop_arguments[..6]
            .iter()
            .map(String::as_str)
            .ne(base_arguments)
        || plan.stop_arguments[6..] != ["--shutdown", "--yes"]
        || validate_absolute_path(Path::new(&plan.executable)).is_err()
        || validate_text(&plan.executable).is_err()
        || validate_absolute_path(Path::new(&plan.arguments[1])).is_err()
        || !crate::core::game::spaces::is_valid_game_space_id(&plan.game_space_id)
        || !matches!(plan.game_id.as_str(), "arma3" | "reforger")
        || plan
            .arguments
            .iter()
            .any(|value| validate_text(value).is_err())
        || plan.fingerprint
            != fingerprint(
                plan.arguments[1].as_str(),
                &plan.game_space_id,
                &plan.game_id,
                plan.format,
                &plan.definition,
            )
        || plan.definition != expected_definition
    {
        return Err(ServerStoreError::Invalid(
            "Service export plan is invalid or changed".into(),
        ));
    }
    for value in plan.stop_arguments.iter().skip(6) {
        validate_text(value)?;
    }
    Ok(())
}

fn fingerprint(
    root: &str,
    space: &str,
    game: &str,
    format: ServiceExportFormat,
    definition: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"foxy-service-export-v1\0");
    hasher.update(root.as_bytes());
    hasher.update(b"\0");
    hasher.update(space.as_bytes());
    hasher.update(b"\0");
    hasher.update(game.as_bytes());
    hasher.update(b"\0");
    hasher.update(format!("{format:?}").as_bytes());
    hasher.update(b"\0");
    hasher.update(definition.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn path_text(path: &Path) -> Result<String, ServerStoreError> {
    path.to_str()
        .map(str::to_string)
        .ok_or(ServerStoreError::UnsafePath)
}

fn export_path_text(path: &Path, format: ServiceExportFormat) -> Result<String, ServerStoreError> {
    let text = path_text(path)?;
    #[cfg(windows)]
    if format == ServiceExportFormat::WindowsUserTask {
        return Ok(normalize_windows_task_path(&text));
    }
    let _ = format;
    Ok(text)
}

fn normalize_windows_task_path(path: &str) -> String {
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(drive_path) = path.strip_prefix(r"\\?\")
        && drive_path.as_bytes().get(1) == Some(&b':')
    {
        return drive_path.to_string();
    }
    path.to_string()
}

fn validate_text(value: &str) -> Result<(), ServerStoreError> {
    if value.chars().any(char::is_control) {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

fn validate_absolute_path(path: &Path) -> Result<(), ServerStoreError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

fn systemd_quote(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '$' => escaped.push_str("$$"),
            '%' => escaped.push_str("%%"),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn render_systemd(executable: &str, arguments: &[String]) -> String {
    let command = std::iter::once(executable)
        .chain(arguments.iter().map(String::as_str))
        .map(systemd_quote)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=Foxy server supervisor\n\n[Service]\nType=exec\nExecStart={command}\nUMask=0077\nKillMode=mixed\nKillSignal=SIGTERM\nTimeoutStopSec=15min\nRestart=no\nStandardOutput=journal\nStandardError=journal\nUnsetEnvironment=DISPLAY WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS\n\n[Install]\nWantedBy=default.target\n"
    )
}

fn windows_quote(value: &str) -> String {
    let mut output = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        match ch {
            '\\' => slashes += 1,
            '"' => {
                output.push_str(&"\\".repeat(slashes * 2 + 1));
                output.push('"');
                slashes = 0;
            }
            _ => {
                output.push_str(&"\\".repeat(slashes));
                slashes = 0;
                output.push(ch);
            }
        }
    }
    output.push_str(&"\\".repeat(slashes * 2));
    output.push('"');
    output
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn render_task_xml(executable: &str, arguments: &[String]) -> String {
    let arguments = arguments
        .iter()
        .map(|arg| windows_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    // With InteractiveToken and no UserId, Task Scheduler uses the registering account.
    format!(
        "<?xml version=\"1.0\"?>\n<Task version=\"1.3\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <Principals>\n    <Principal id=\"Author\">\n      <LogonType>InteractiveToken</LogonType>\n      <RunLevel>LeastPrivilege</RunLevel>\n    </Principal>\n  </Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <AllowHardTerminate>false</AllowHardTerminate>\n    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <IdleSettings>\n      <StopOnIdleEnd>false</StopOnIdleEnd>\n      <RestartOnIdle>false</RestartOnIdle>\n    </IdleSettings>\n  </Settings>\n  <Actions Context=\"Author\">\n    <Exec>\n      <Command>{}</Command>\n      <Arguments>{}</Arguments>\n    </Exec>\n  </Actions>\n</Task>\n",
        xml_escape(executable),
        xml_escape(&arguments)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_escaping_is_literal_for_systemd_windows_and_xml() {
        assert_eq!(systemd_quote("x%$\\\"\u{e9}"), "\"x%%$$\\\\\\\"\u{e9}\"");
        assert_eq!(windows_quote("x\\\"y"), "\"x\\\\\\\"y\"");
        assert_eq!(
            xml_escape("<&>'\"\u{e9}"),
            "&lt;&amp;&gt;&apos;&quot;\u{e9}"
        );
        let xml = render_task_xml(
            "C:\\Foxy & <Test>.exe",
            &["--config-dir".into(), "C:\\One & Two".into()],
        );
        assert!(xml.starts_with("<?xml version=\"1.0\"?>\n<Task version=\"1.3\""));
        for setting in [
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>LeastPrivilege</RunLevel>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<AllowHardTerminate>false</AllowHardTerminate>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
        ] {
            assert!(xml.contains(setting));
        }
        assert!(xml.contains("<Command>C:\\Foxy &amp; &lt;Test&gt;.exe</Command>"));
        assert!(xml.contains("&quot;C:\\One &amp; Two&quot;"));
        let background_xml = render_task_xml(
            "C:\\Foxy.exe",
            &[
                "--config-dir".into(),
                "C:\\One & Two".into(),
                "server".into(),
                "supervise".into(),
                "--game-space".into(),
                "alpha".into(),
                "--background".into(),
            ],
        );
        assert!(background_xml.contains("&quot;C:\\One &amp; Two&quot; &quot;server&quot; &quot;supervise&quot; &quot;--game-space&quot; &quot;alpha&quot; &quot;--background&quot;"));
        assert!(!xml.contains("<Triggers>"));
        assert_eq!(
            normalize_windows_task_path(r"\\?\C:\Foxy.exe"),
            r"C:\Foxy.exe"
        );
        assert_eq!(
            normalize_windows_task_path(r"\\?\UNC\server\share\Foxy.exe"),
            r"\\server\share\Foxy.exe"
        );
        let unit = render_systemd("/opt/Foxy $100%", &["server".into()]);
        assert!(unit.contains("ExecStart=\"/opt/Foxy $$100%%\" \"server\"\n"));
        assert!(unit.contains("KillMode=mixed\nKillSignal=SIGTERM\nTimeoutStopSec=15min\n"));
        assert!(!unit.contains("ExecStop="));
    }

    #[test]
    fn export_write_refuses_existing_destination_and_symlink_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = test_plan(tmp.path());
        let existing = tmp.path().join("present.service");
        std::fs::write(&existing, "keep").unwrap();
        let error = write_service_export(&plan, &existing).unwrap_err();
        assert!(error.to_string().contains("already exists"));
        assert_eq!(std::fs::read(existing).unwrap(), b"keep");
        let changed = ServiceExportPlan {
            definition: "[Service]\nType=exec\n".into(),
            ..plan.clone()
        };
        assert!(write_service_export(&changed, &tmp.path().join("changed.service")).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link = tmp.path().join("linked");
            symlink(tmp.path(), &link).unwrap();
            assert!(write_service_export(&plan, &link.join("new.service")).is_err());
        }
    }

    #[test]
    fn valid_plan_writes_exact_definition_bytes_once() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = test_plan(tmp.path());
        let destination = tmp.path().join("foxy-server.service");
        write_service_export(&plan, &destination).unwrap();
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            plan.definition.as_bytes()
        );
        assert!(write_service_export(&plan, &destination).is_err());
    }

    #[test]
    fn windows_plan_starts_background_but_keeps_stop_arguments_foreground() {
        let tmp = tempfile::tempdir().unwrap();
        let mut plan = test_plan(tmp.path());
        plan.format = ServiceExportFormat::WindowsUserTask;
        plan.file_name = "FoxyServerTask.xml".into();
        plan.arguments.push("--background".into());
        plan.definition = render_task_xml(&plan.executable, &plan.arguments);
        plan.fingerprint = fingerprint(
            &plan.arguments[1],
            &plan.game_space_id,
            &plan.game_id,
            plan.format,
            &plan.definition,
        );
        assert_eq!(plan.stop_arguments.len(), 8);
        assert_eq!(&plan.stop_arguments[6..], ["--shutdown", "--yes"]);
        let destination = tmp.path().join("FoxyServerTask.xml");
        write_service_export(&plan, &destination).unwrap();
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            plan.definition.as_bytes()
        );
    }

    fn test_plan(root: &Path) -> ServiceExportPlan {
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec![
            "--config-dir".into(),
            root.to_string_lossy().into_owned(),
            "server".into(),
            "supervise".into(),
            "--game-space".into(),
            "alpha".into(),
        ];
        let mut stop = args.clone();
        stop.extend(["--shutdown".into(), "--yes".into()]);
        let definition = render_systemd(&executable, &args);
        ServiceExportPlan {
            schema_version: 1,
            game_space_id: "alpha".into(),
            game_id: "arma3".into(),
            format: ServiceExportFormat::SystemdUser,
            file_name: "foxy-server.service".into(),
            executable,
            arguments: args,
            fingerprint: fingerprint(
                &root.to_string_lossy(),
                "alpha",
                "arma3",
                ServiceExportFormat::SystemdUser,
                &definition,
            ),
            definition,
            warnings: Vec::new(),
            registration_performed: false,
            writes_performed: false,
            stop_arguments: stop,
        }
    }
}
