#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod build_info;
mod cli;
mod ui;

use crate::ui::app::agent_driver::AgentGuiLaunchConfig;
use std::process::Command;
use std::sync::Once;
use std::{backtrace::Backtrace, fs};

// TODO: @YetheSamartaka Temporary fix for console not showing up on Windows.
#[cfg(target_os = "windows")]
fn attach_console() {
    use std::fs::File;

    use winapi::um::consoleapi::AllocConsole;
    use winapi::um::wincon::ATTACH_PARENT_PROCESS;
    use winapi::um::wincon::AttachConsole;

    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            AllocConsole();
        }

        // redirect stdout
        if let Err(e) = File::create("CONOUT$") {
            eprintln!("WARNING: Failed to redirect console output: {}", e);
        }
    }
}

#[cfg(target_os = "windows")]
fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(target_os = "windows")]
fn register_start_menu_shortcut() {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let exe_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            log::warn!("Failed to resolve current executable path: {err}");
            return;
        }
    };

    let working_dir = exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let shortcut_path = match std::env::var("APPDATA") {
        Ok(appdata) => std::path::PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Foxy.lnk"),
        Err(err) => {
            log::warn!("APPDATA environment variable is not available: {err}");
            return;
        }
    };

    let exe_path_str = exe_path.to_string_lossy();
    let working_dir_str = working_dir.to_string_lossy();
    let shortcut_path_str = shortcut_path.to_string_lossy();

    let script = format!(
        "$wsh = New-Object -ComObject WScript.Shell; \
         $lnk = $wsh.CreateShortcut('{shortcut}'); \
         $lnk.TargetPath = '{target}'; \
         $lnk.WorkingDirectory = '{working_dir}'; \
         $lnk.IconLocation = '{target},0'; \
         $lnk.Description = 'Foxy - Arma 3 mod updater'; \
         $lnk.Save();",
        shortcut = escape_powershell_single_quoted(&shortcut_path_str),
        target = escape_powershell_single_quoted(&exe_path_str),
        working_dir = escape_powershell_single_quoted(&working_dir_str),
    );

    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    match status {
        Ok(exit_status) if exit_status.success() => {
            log::info!("Start Menu shortcut registration complete.");
        }
        Ok(exit_status) => {
            log::warn!(
                "Start Menu shortcut registration failed with exit code: {:?}",
                exit_status.code()
            );
        }
        Err(err) => {
            log::warn!("Failed to run PowerShell for Start Menu registration: {err}");
        }
    }
}

#[cfg(target_os = "linux")]
const FOXY_DESKTOP_ENTRY_MARKER: &str = "X-Foxy-Generated=true";

#[cfg(target_os = "linux")]
fn desktop_entry_escape_path(path: &std::path::Path, quote: bool) -> String {
    let raw = path.to_string_lossy();
    let escaped = raw
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    if quote {
        format!("\"{}\"", escaped)
    } else {
        escaped
    }
}

#[cfg(target_os = "linux")]
fn register_linux_desktop_entry() {
    let desktop_dir = dirs::data_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"))
        .join("applications");

    let desktop_path = desktop_dir.join("foxy.desktop");

    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let existing = std::fs::read_to_string(&desktop_path).ok();
    if existing
        .as_deref()
        .is_some_and(|content| !content.contains(FOXY_DESKTOP_ENTRY_MARKER))
    {
        return;
    }

    let icon_path = exe_path
        .parent()
        .map(|parent| parent.join("foxy.png"))
        .filter(|path| path.is_file());
    let icon_line = icon_path
        .as_ref()
        .map(|path| format!("Icon={}\n", desktop_entry_escape_path(path, false)))
        .unwrap_or_default();

    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Foxy\n\
         Comment=Arma 3 Mod Updater\n\
         Exec={}\n\
         {}\
         Terminal=false\n\
         Categories=Game;Utility;\n\
         Keywords=arma;mod;updater;\n\
         StartupNotify=true\n\
         StartupWMClass=Foxy\n\
         {}\n",
        desktop_entry_escape_path(&exe_path, true),
        icon_line,
        FOXY_DESKTOP_ENTRY_MARKER
    );

    if existing.as_deref() == Some(content.as_str()) {
        return;
    }

    if let Err(err) = std::fs::create_dir_all(&desktop_dir) {
        log::warn!("Failed to create applications directory: {}", err);
        return;
    }

    if let Err(err) = std::fs::write(&desktop_path, content) {
        log::warn!("Failed to write desktop entry: {}", err);
        return;
    }

    log::info!("Desktop entry registered: {}", desktop_path.display());

    // Update desktop database if available
    let _ = Command::new("update-desktop-database")
        .arg(&desktop_dir)
        .status();
}

// TODO: Rewrite:
mod core;

#[allow(dead_code)]
fn wipe_db() {
    core::tasks::init_database::wipe_database_sync();
}

fn install_panic_hook() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        std::panic::set_hook(Box::new(|panic_info| {
            let location = panic_info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_else(|| "<unknown>".to_string());
            let message = if let Some(msg) = panic_info.payload().downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
                msg.clone()
            } else {
                "non-string panic payload".to_string()
            };
            let backtrace = Backtrace::force_capture();

            eprintln!("Foxy panic at {location}: {message}\n{backtrace}");
            log::error!("Foxy panic at {}: {}\n{}", location, message, backtrace);
            if location.contains("egui-wgpu") || message.contains("egui-wgpu") {
                mark_wgpu_panic_for_next_launch(&message);
            }
        }));
    });
}

fn mark_wgpu_panic_for_next_launch(message: &str) {
    let marker_path = core::utils::renderer_fallback::wgpu_crash_marker_path();
    let contents = format!(
        "Foxy detected an egui-wgpu panic on the previous run.\n\
         Next UI launch will switch the UI renderer setting to Glow unless FOXY_RENDERER is set.\n\
         Panic: {message}\n"
    );
    if let Some(parent) = marker_path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        eprintln!(
            "WARNING: Failed to create WGPU crash marker directory {}: {}",
            parent.display(),
            err
        );
        return;
    }
    if let Err(err) = fs::write(&marker_path, contents) {
        eprintln!(
            "WARNING: Failed to write WGPU crash marker {}: {}",
            marker_path.display(),
            err
        );
    }
}

fn main() {
    install_panic_hook();

    let raw_args: Vec<String> = std::env::args().collect();
    let launched_from_terminal = is_terminal_invocation();
    let no_args = raw_args.len() <= 1;

    if no_args && (cfg!(debug_assertions) || !launched_from_terminal) {
        launch_ui(false, default_no_arg_agent_gui_config());
        return;
    }

    #[cfg(target_os = "windows")]
    {
        let likely_cli = raw_args.len() <= 1
            || raw_args
                .get(1)
                .map(|arg| !arg.eq_ignore_ascii_case("ui"))
                .unwrap_or(true);
        if (launched_from_terminal && likely_cli) || cfg!(debug_assertions) {
            attach_console();
        }
    }

    match cli::run_from_env() {
        cli::CliExecution::RunUi {
            debug_mode,
            agent_gui,
        } => {
            launch_ui(debug_mode, agent_gui);
        }
        cli::CliExecution::Exit(code) => {
            std::process::exit(code);
        }
    }
}

fn is_terminal_invocation() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
        || std::io::stdout().is_terminal()
        || std::io::stderr().is_terminal()
}

fn default_no_arg_agent_gui_config() -> AgentGuiLaunchConfig {
    AgentGuiLaunchConfig {
        enabled: cfg!(debug_assertions),
        port: 0,
    }
}

fn launch_ui(debug_mode: bool, agent_gui: AgentGuiLaunchConfig) {
    core::api::ensure_logger_with_terminal();

    // Agent GUI (driver/test) sessions must not mutate the user's desktop
    // environment: skip Start Menu / .desktop registration so a throwaway dev
    // build launched with `ui --agent-gui` never repoints the real shortcut.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let skip_desktop_integration = agent_gui.enabled;

    #[cfg(target_os = "windows")]
    {
        // Skip Start Menu shortcut registration if installed via the Inno Setup installer
        // (the installer handles shortcuts). Detect by checking for unins000.exe next to the binary.
        let installed_via_installer = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.join("unins000.exe").exists()))
            .unwrap_or(false);
        if !installed_via_installer && !skip_desktop_integration {
            register_start_menu_shortcut();
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Register .desktop entry if not installed via the .sh installer.
        // Detect by checking for a marker file left by the installer.
        let installed_via_installer = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .map(|p| p.join(".installed_by_foxy_installer").exists())
            })
            .unwrap_or(false);
        if !installed_via_installer && !skip_desktop_integration {
            register_linux_desktop_entry();
        }
    }

    core::game::spaces::ensure_game_spaces_layout();
    core::tasks::init_database::check_and_wipe_database();
    ui::window::main(debug_mode, agent_gui);
}
