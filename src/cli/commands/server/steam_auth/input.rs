use foxy_steam::{AuthError, Secret};
use serde::Deserialize;
use std::io;
use std::path::Path;
use zeroize::Zeroize;

#[cfg(windows)]
#[path = "input/windows_file.rs"]
mod windows_file;

const MAX_INPUT: usize = 4096;
const MAX_CREDENTIAL_FILE: usize = 16 * 1024;

pub(super) struct PrivateInput {
    bytes: Vec<u8>,
    skip_line_feed: bool,
    terminal: TerminalMode,
    windows_handle: Option<*mut std::ffi::c_void>,
    windows_mode: u32,
    windows_console: bool,
}

enum TerminalMode {
    #[cfg(unix)]
    Unix(libc::termios),
    Other,
}

impl PrivateInput {
    pub(super) fn new() -> Result<Self, AuthError> {
        #[cfg(unix)]
        {
            let terminal = unsafe { libc::isatty(0) } == 1;
            if terminal {
                let mut previous = std::mem::MaybeUninit::<libc::termios>::uninit();
                if unsafe { libc::tcgetattr(0, previous.as_mut_ptr()) } != 0 {
                    return Err(AuthError::Storage);
                }
                let previous = unsafe { previous.assume_init() };
                let mut hidden = previous;
                hidden.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON);
                hidden.c_cc[libc::VMIN] = 0;
                hidden.c_cc[libc::VTIME] = 0;
                if unsafe { libc::tcsetattr(0, libc::TCSANOW, &hidden) } != 0 {
                    return Err(AuthError::Storage);
                }
                return Ok(Self {
                    bytes: Vec::new(),
                    skip_line_feed: false,
                    terminal: TerminalMode::Unix(previous),
                    windows_handle: None,
                    windows_mode: 0,
                    windows_console: false,
                });
            }
            Ok(Self {
                bytes: Vec::new(),
                skip_line_feed: false,
                terminal: TerminalMode::Other,
                windows_handle: None,
                windows_mode: 0,
                windows_console: false,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use winapi::um::consoleapi::{GetConsoleMode, SetConsoleMode};
            let handle: winapi::um::winnt::HANDLE = io::stdin().as_raw_handle().cast();
            if handle.is_null() || handle == winapi::um::handleapi::INVALID_HANDLE_VALUE {
                return Err(AuthError::Storage);
            }
            let mut mode = 0;
            if unsafe { GetConsoleMode(handle, &mut mode) } != 0 {
                let hidden = mode & !(0x0004 | 0x0002 | 0x0001);
                if unsafe { SetConsoleMode(handle, hidden) } == 0 {
                    return Err(AuthError::Storage);
                }
                Ok(Self {
                    bytes: Vec::new(),
                    skip_line_feed: false,
                    terminal: TerminalMode::Other,
                    windows_handle: Some(handle.cast()),
                    windows_mode: mode,
                    windows_console: true,
                })
            } else {
                Ok(Self {
                    bytes: Vec::new(),
                    skip_line_feed: false,
                    terminal: TerminalMode::Other,
                    windows_handle: Some(handle.cast()),
                    windows_mode: 0,
                    windows_console: false,
                })
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(AuthError::Storage)
        }
    }

    pub(super) fn poll_line(&mut self) -> Result<Option<Secret>, AuthError> {
        #[cfg(unix)]
        {
            let mut pollfd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
            if ready < 0 {
                return match io::Error::last_os_error().kind() {
                    io::ErrorKind::Interrupted => Err(AuthError::Cancelled),
                    _ => Err(AuthError::Storage),
                };
            }
            if ready == 0 {
                return Ok(None);
            }
            let mut byte = 0u8;
            let count = unsafe { libc::read(0, (&mut byte as *mut u8).cast(), 1) };
            if count == 0 {
                return Err(AuthError::Cancelled);
            }
            if count < 0 {
                return match io::Error::last_os_error().kind() {
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => Ok(None),
                    _ => Err(AuthError::Storage),
                };
            }
            self.push_byte(byte)
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use winapi::shared::minwindef::{DWORD, FALSE};
            use winapi::um::consoleapi::{GetNumberOfConsoleInputEvents, ReadConsoleInputW};
            use winapi::um::namedpipeapi::PeekNamedPipe;
            use winapi::um::wincon::{INPUT_RECORD, KEY_EVENT};
            let handle = self
                .windows_handle
                .unwrap_or_else(|| io::stdin().as_raw_handle().cast());
            let mut mode = 0;
            if unsafe { winapi::um::consoleapi::GetConsoleMode(handle.cast(), &mut mode) } != 0 {
                let mut pending = 0;
                if unsafe { GetNumberOfConsoleInputEvents(handle.cast(), &mut pending) } == 0
                    || pending == 0
                {
                    return Ok(None);
                }
                let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
                let mut read = 0;
                if unsafe { ReadConsoleInputW(handle.cast(), &mut record, 1, &mut read) } == 0
                    || read == 0
                {
                    return Err(AuthError::Storage);
                }
                if record.EventType != KEY_EVENT || unsafe { record.Event.KeyEvent().bKeyDown } == 0
                {
                    return Ok(None);
                }
                let key = unsafe { record.Event.KeyEvent() };
                let control = key.dwControlKeyState & 0x000c != 0;
                if control && matches!(key.wVirtualKeyCode, 0x43 | 0x44) {
                    return Err(AuthError::Cancelled);
                }
                match key.wVirtualKeyCode {
                    0x0d => self.finish_line(),
                    0x08 => {
                        self.pop_character();
                        Ok(None)
                    }
                    _ => {
                        let ch = unsafe { *key.uChar.UnicodeChar() };
                        if ch == 0 {
                            return Ok(None);
                        }
                        let scalar = char::from_u32(ch as u32).ok_or(AuthError::InvalidInput)?;
                        self.push_bytes(scalar.encode_utf8(&mut [0; 4]).as_bytes())
                    }
                }
            } else {
                let mut available: DWORD = 0;
                let mut ignored = [0u8; 1];
                if unsafe {
                    PeekNamedPipe(
                        handle.cast(),
                        ignored.as_mut_ptr().cast(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                } == FALSE
                {
                    return if unsafe { winapi::um::errhandlingapi::GetLastError() } == 109 {
                        Err(AuthError::Cancelled)
                    } else {
                        Err(AuthError::Storage)
                    };
                }
                if available == 0 {
                    return Ok(None);
                }
                let mut byte = 0u8;
                let mut read = 0;
                if unsafe {
                    winapi::um::fileapi::ReadFile(
                        handle.cast(),
                        (&mut byte as *mut u8).cast(),
                        1,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(AuthError::Storage);
                }
                if read == 0 {
                    return Err(AuthError::Cancelled);
                }
                self.push_byte(byte)
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(AuthError::Storage)
        }
    }

    fn push_byte(&mut self, byte: u8) -> Result<Option<Secret>, AuthError> {
        if self.skip_line_feed {
            self.skip_line_feed = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }
        match byte {
            b'\n' => self.finish_line(),
            b'\r' => {
                self.skip_line_feed = true;
                self.finish_line()
            }
            3 | 4 => Err(AuthError::Cancelled),
            8 | 127 => {
                self.pop_character();
                Ok(None)
            }
            0 => Err(AuthError::InvalidInput),
            _ => self.push_bytes(&[byte]),
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) -> Result<Option<Secret>, AuthError> {
        if self.bytes.len().saturating_add(bytes.len()) > MAX_INPUT {
            self.bytes.zeroize();
            return Err(AuthError::InvalidInput);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(None)
    }

    fn pop_character(&mut self) {
        if let Some(mut byte) = self.bytes.pop() {
            while byte & 0xc0 == 0x80 {
                let Some(previous) = self.bytes.pop() else {
                    break;
                };
                byte = previous;
            }
        }
    }

    fn finish_line(&mut self) -> Result<Option<Secret>, AuthError> {
        let bytes = std::mem::take(&mut self.bytes);
        let value = match String::from_utf8(bytes) {
            Ok(value) => value,
            Err(error) => {
                error.into_bytes().zeroize();
                return Err(AuthError::InvalidInput);
            }
        };
        if value.is_empty() {
            return Err(AuthError::InvalidInput);
        }
        Ok(Some(Secret::new(value)))
    }
}

impl Drop for PrivateInput {
    fn drop(&mut self) {
        let _ = &self.terminal;
        let _ = (self.windows_handle, self.windows_mode, self.windows_console);
        self.bytes.zeroize();
        #[cfg(unix)]
        if let TerminalMode::Unix(previous) = &self.terminal {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, previous) };
        }
        #[cfg(windows)]
        if let Some(handle) = self.windows_handle
            && self.windows_console
        {
            unsafe { winapi::um::consoleapi::SetConsoleMode(handle.cast(), self.windows_mode) };
        }
    }
}

pub(super) fn parse_credentials(input: &str) -> Result<(Secret, Secret), AuthError> {
    if input.len() > MAX_CREDENTIAL_FILE {
        return Err(AuthError::InvalidInput);
    }
    let parsed: JsonCredentials =
        serde_json::from_str(input).map_err(|_| AuthError::InvalidInput)?;
    validate_credentials(parsed.account_name, parsed.password)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonCredentials {
    account_name: String,
    password: String,
}

pub(super) fn load_credentials(path: &Path) -> Result<(Secret, Secret), AuthError> {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let before = std::fs::symlink_metadata(path).map_err(|_| AuthError::Storage)?;
        if !before.is_file()
            || before.file_type().is_symlink()
            || before.len() > MAX_CREDENTIAL_FILE as u64
        {
            return Err(AuthError::InvalidInput);
        }
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options.open(path).map_err(|_| AuthError::Storage)?;
        let metadata = file.metadata().map_err(|_| AuthError::Storage)?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
            || metadata.dev() != before.dev()
            || metadata.ino() != before.ino()
        {
            return Err(AuthError::Storage);
        }
        let mut text = String::new();
        file.take((MAX_CREDENTIAL_FILE + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|_| AuthError::InvalidInput)?;
        if text.len() > MAX_CREDENTIAL_FILE {
            return Err(AuthError::InvalidInput);
        }
        parse_config(&text)
    }
    #[cfg(windows)]
    {
        let text = windows_file::read_private(path)?;
        parse_config(&text)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(AuthError::Storage)
    }
}

fn parse_config(input: &str) -> Result<(Secret, Secret), AuthError> {
    if input.trim_start().starts_with('{') {
        return parse_credentials(input);
    }
    let mut username = None;
    let mut password = None;
    for line in input.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line.split_once('=').ok_or(AuthError::InvalidInput)?;
        let slot = match key.trim() {
            "FOXY_STEAMCMD_USERNAME" => &mut username,
            "FOXY_STEAMCMD_PASSWORD" => &mut password,
            _ => return Err(AuthError::InvalidInput),
        };
        if slot.is_some() {
            return Err(AuthError::InvalidInput);
        }
        let mut value = raw.trim();
        if value.starts_with(['\'', '"']) {
            if value.len() < 2 || value.as_bytes().first() != value.as_bytes().last() {
                return Err(AuthError::InvalidInput);
            }
            value = &value[1..value.len() - 1];
        }
        if value.is_empty() || value.len() > MAX_INPUT || value.chars().any(char::is_control) {
            return Err(AuthError::InvalidInput);
        }
        *slot = Some(value.to_owned());
    }
    let username = username.ok_or(AuthError::InvalidInput)?;
    if !username
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(AuthError::InvalidInput);
    }
    validate_credentials(username, password.ok_or(AuthError::InvalidInput)?)
}

fn validate_credentials(username: String, password: String) -> Result<(Secret, Secret), AuthError> {
    if username.is_empty()
        || username.len() > MAX_INPUT
        || password.is_empty()
        || password.len() > MAX_INPUT
        || username.chars().any(char::is_control)
        || password.chars().any(char::is_control)
    {
        return Err(AuthError::InvalidInput);
    }
    Ok((Secret::new(username), Secret::new(password)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_json_accepts_escaped_unicode_and_literal_shell_characters() {
        let (user, password) =
            parse_credentials(r#"{"account_name":"fixture_user","password":"a=b#c$`\\\"\u00e9"}"#)
                .unwrap();
        assert_eq!(user.expose(), "fixture_user");
        assert_eq!(password.expose(), "a=b#c$`\\\"\u{e9}");
    }

    #[test]
    fn credentials_reject_duplicate_empty_unknown_and_control_character_fields() {
        for input in [
            r#"{"account_name":"fixture","account_name":"other","password":"synthetic"}"#,
            r#"{"account_name":"fixture","password":""}"#,
            r#"{"account_name":"fixture\nquit","password":"synthetic"}"#,
            r#"{"account_name":"fixture","password":"synthetic","command":"quit"}"#,
        ] {
            assert!(matches!(
                parse_credentials(input),
                Err(AuthError::InvalidInput)
            ));
        }
        let (account, _) =
            parse_credentials(r#"{"account_name":"fixture;quit","password":"synthetic"}"#).unwrap();
        assert_eq!(account.expose(), "fixture;quit");
    }

    #[test]
    fn local_config_keeps_shell_metacharacters_literal_and_rejects_duplicates() {
        let (user, password) = parse_config(
            "FOXY_STEAMCMD_USERNAME=fixture_user\nFOXY_STEAMCMD_PASSWORD=\"a=b#c$`\\value\"",
        )
        .unwrap();
        assert_eq!(user.expose(), "fixture_user");
        assert_eq!(password.expose(), "a=b#c$`\\value");
        assert!(matches!(
            parse_config(
                "FOXY_STEAMCMD_USERNAME=fixture\nFOXY_STEAMCMD_PASSWORD=one\nFOXY_STEAMCMD_PASSWORD=two"
            ),
            Err(AuthError::InvalidInput)
        ));
    }
}
