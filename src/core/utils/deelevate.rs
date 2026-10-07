//! Launching child processes without the current process's elevated token.
//!
//! A game started from an elevated Foxy inherits the admin token, and Windows
//! then blocks unelevated apps (Discord, TeamSpeak, OBS) from sending it input
//! or capturing its window. Foxy no longer asks for elevation, but users who
//! start it elevated themselves, or who still carry the old "Run as
//! administrator" AppCompat flag, must not pay for it with a broken overlay.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::process::Command;

/// Whether this process runs with an elevated token.
///
/// `None` means the query itself failed, or the platform has no such concept.
pub fn is_process_elevated() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        windows_impl::is_process_elevated()
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Start a process and return its pid, dropping elevation when this process is
/// elevated. Falls back to a plain spawn whenever de-elevation is unavailable.
pub fn spawn_unelevated(program: &OsStr, args: &[OsString], cwd: Option<&Path>) -> io::Result<u32> {
    #[cfg(target_os = "windows")]
    {
        if is_process_elevated() == Some(true) {
            match windows_impl::spawn_with_shell_token(program, args, cwd) {
                Ok(pid) => {
                    log::info!("Started child process with the unelevated shell token (pid={pid})");
                    return Ok(pid);
                }
                Err(err) => {
                    log::warn!(
                        "Could not drop elevation for the child process ({err}); starting it \
                         elevated, which can break Discord/TeamSpeak hotkeys and OBS capture"
                    );
                }
            }
        }
    }

    spawn_inheriting(program, args, cwd)
}

fn spawn_inheriting(program: &OsStr, args: &[OsString], cwd: Option<&Path>) -> io::Result<u32> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    command.spawn().map(|child| child.id())
}

/// Quote one argument for a Windows command line, per the `CommandLineToArgvW`
/// rules: backslashes are only special in front of a quote.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                quoted.push(ch);
            }
            '"' => {
                for _ in 0..=backslashes {
                    quoted.push('\\');
                }
                backslashes = 0;
                quoted.push('"');
            }
            _ => {
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    for _ in 0..backslashes {
        quoted.push('\\');
    }
    quoted.push('"');
    quoted
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::quote_windows_arg;
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use winapi::shared::minwindef::DWORD;
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use winapi::um::securitybaseapi::{DuplicateTokenEx, GetTokenInformation};
    use winapi::um::winbase::CreateProcessWithTokenW;
    use winapi::um::winnt::{
        HANDLE, MAXIMUM_ALLOWED, PROCESS_QUERY_LIMITED_INFORMATION, SecurityImpersonation,
        TOKEN_DUPLICATE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation, TokenPrimary,
    };
    use winapi::um::winuser::{GetShellWindow, GetWindowThreadProcessId};

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub(super) fn is_process_elevated() -> Option<bool> {
        unsafe {
            let mut raw = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) == 0 {
                return None;
            }
            let token = OwnedHandle(raw);

            let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
            let mut returned: DWORD = 0;
            let ok = GetTokenInformation(
                token.0,
                TokenElevation,
                &mut elevation as *mut _ as *mut _,
                std::mem::size_of::<TOKEN_ELEVATION>() as DWORD,
                &mut returned,
            );
            (ok != 0).then_some(elevation.TokenIsElevated != 0)
        }
    }

    /// Start the process with a primary token duplicated from the desktop shell,
    /// which runs at the interactive user's normal integrity level.
    pub(super) fn spawn_with_shell_token(
        program: &OsStr,
        args: &[OsString],
        cwd: Option<&Path>,
    ) -> io::Result<u32> {
        let shell_token = shell_primary_token()?;

        let mut command_line: Vec<u16> = wide(build_command_line(program, args));
        let application = wide(program);
        let working_dir = cwd.map(wide);

        unsafe {
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as DWORD;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();

            let ok = CreateProcessWithTokenW(
                shell_token.0,
                0,
                application.as_ptr(),
                command_line.as_mut_ptr(),
                0,
                std::ptr::null_mut(),
                working_dir
                    .as_ref()
                    .map_or(std::ptr::null(), |dir| dir.as_ptr()),
                &mut startup,
                &mut info,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }

            let _process = OwnedHandle(info.hProcess);
            let _thread = OwnedHandle(info.hThread);
            Ok(info.dwProcessId)
        }
    }

    fn shell_primary_token() -> io::Result<OwnedHandle> {
        unsafe {
            let shell_window = GetShellWindow();
            if shell_window.is_null() {
                return Err(io::Error::other("no desktop shell window"));
            }

            let mut shell_pid: DWORD = 0;
            GetWindowThreadProcessId(shell_window, &mut shell_pid);
            if shell_pid == 0 {
                return Err(io::Error::other("no desktop shell process"));
            }

            let raw_process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, shell_pid);
            if raw_process.is_null() {
                return Err(io::Error::last_os_error());
            }
            let shell_process = OwnedHandle(raw_process);

            let mut raw_token = std::ptr::null_mut();
            if OpenProcessToken(shell_process.0, TOKEN_DUPLICATE, &mut raw_token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let shell_token = OwnedHandle(raw_token);

            let mut raw_primary = std::ptr::null_mut();
            let ok = DuplicateTokenEx(
                shell_token.0,
                MAXIMUM_ALLOWED,
                std::ptr::null_mut(),
                SecurityImpersonation,
                TokenPrimary,
                &mut raw_primary,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(OwnedHandle(raw_primary))
        }
    }

    fn build_command_line(program: &OsStr, args: &[OsString]) -> OsString {
        let mut parts = vec![quote_windows_arg(&program.to_string_lossy())];
        parts.extend(
            args.iter()
                .map(|arg| quote_windows_arg(&arg.to_string_lossy())),
        );
        OsString::from(parts.join(" "))
    }

    fn wide<S: AsRef<OsStr>>(value: S) -> Vec<u16> {
        value
            .as_ref()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::quote_windows_arg;

    #[test]
    fn plain_arguments_are_not_quoted() {
        assert_eq!(quote_windows_arg("-nosplash"), "-nosplash");
    }

    #[test]
    fn arguments_with_spaces_are_quoted() {
        assert_eq!(
            quote_windows_arg(r"-mod=C:\Program Files\Arma 3"),
            r#""-mod=C:\Program Files\Arma 3""#
        );
    }

    #[test]
    fn empty_argument_survives_as_an_empty_quoted_string() {
        assert_eq!(quote_windows_arg(""), "\"\"");
    }

    #[test]
    fn backslashes_are_only_escaped_before_a_quote() {
        assert_eq!(quote_windows_arg(r"a\\b c"), r#""a\\b c""#);
        assert_eq!(quote_windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_windows_arg(r"trailing\"), r"trailing\");
        assert_eq!(quote_windows_arg(r"a b\"), r#""a b\\""#);
    }
}
