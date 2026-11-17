use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

const MAX_FILES: usize = 64;
const MAX_DEPTH: usize = 3;
const MAX_LINE_BYTES: usize = 4096;
const MAX_HISTORY_LINES: usize = 128;
const MAX_HISTORY_BYTES: usize = 32 * 1024;
const MAX_SAMPLE_BYTES: usize = 32 * 1024;
const MAX_FILE_SAMPLE_BYTES: usize = 8 * 1024;
const MAX_DISCOVERY_ENTRIES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogSnapshot {
    pub entries: Vec<LogLine>,
    pub next_sequence: u64,
    pub truncated: bool,
    pub error: Option<LogReadError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub sequence: u64,
    pub stream: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogReadError {
    DiscoveryLimit,
    ReadFailed,
    UnsafePath,
}

pub(super) struct LogCollector {
    roots: Vec<PathBuf>,
    run_started_unix_ms: u64,
    cursors: HashMap<PathBuf, FileCursor>,
    streams: HashMap<PathBuf, String>,
    next_engine_stream: u32,
    entries: VecDeque<LogLine>,
    history_bytes: usize,
    next_sequence: u64,
    next_file_index: usize,
    truncated: bool,
    secrets: Vec<zeroize::Zeroizing<String>>,
}

#[derive(Default)]
struct FileCursor {
    offset: u64,
    pending: Vec<u8>,
    dropping_line: bool,
    identity: Option<FileIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(not(any(unix, windows)))]
    len: u64,
    #[cfg(not(any(unix, windows)))]
    modified_ms: Option<u64>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    file_id: Option<(u32, u64)>,
}

impl LogCollector {
    pub(super) fn new(roots: Vec<PathBuf>, run_started_unix_ms: u64) -> Self {
        let roots_truncated = roots.len() > MAX_FILES;
        let roots: Vec<_> = roots.into_iter().take(MAX_FILES).collect();
        let mut collector = Self {
            roots,
            run_started_unix_ms,
            cursors: HashMap::new(),
            streams: HashMap::new(),
            next_engine_stream: 0,
            entries: VecDeque::new(),
            history_bytes: 0,
            next_sequence: 1,
            next_file_index: 0,
            truncated: roots_truncated,
            secrets: Vec::new(),
        };
        let (files, _) = collector.discover();
        for file in files {
            if let Ok((metadata, file_identity)) = baseline_file(&file.path) {
                collector.cursors.insert(
                    file.path.clone(),
                    FileCursor {
                        offset: metadata.len(),
                        identity: Some(file_identity),
                        ..FileCursor::default()
                    },
                );
                collector.assign_stream(file);
            }
        }
        collector
    }

    pub(super) fn sample(&mut self) -> LogSnapshot {
        let (files, mut error) = self.discover();
        self.cursors
            .retain(|path, _| files.iter().any(|file| &file.path == path));
        self.streams
            .retain(|path, _| files.iter().any(|file| &file.path == path));
        if error == Some(LogReadError::DiscoveryLimit) {
            self.truncated = true;
        }
        let file_count = files.len();
        let start_index = if file_count == 0 {
            0
        } else {
            self.next_file_index % file_count
        };
        let mut remaining = MAX_SAMPLE_BYTES;
        for offset in 0..file_count {
            let discovered = files[(start_index + offset) % file_count].clone();
            if remaining == 0 {
                self.truncated = true;
                break;
            }
            if !self.cursors.contains_key(&discovered.path) {
                let Ok(metadata) = fs::symlink_metadata(&discovered.path) else {
                    error.get_or_insert(LogReadError::ReadFailed);
                    continue;
                };
                if !is_regular_nofollow(&metadata) {
                    error.get_or_insert(LogReadError::UnsafePath);
                    continue;
                }
                if modified_ms(&metadata).is_none_or(|mtime| mtime < self.run_started_unix_ms) {
                    continue;
                }
                let Ok((opened_metadata, file_identity)) = baseline_file(&discovered.path) else {
                    error.get_or_insert(LogReadError::UnsafePath);
                    continue;
                };
                if modified_ms(&opened_metadata)
                    .is_none_or(|mtime| mtime < self.run_started_unix_ms)
                {
                    continue;
                }
                self.cursors.insert(
                    discovered.path.clone(),
                    FileCursor {
                        identity: Some(file_identity),
                        ..FileCursor::default()
                    },
                );
                self.assign_stream(discovered.clone());
            }
            match self.read_new_bytes(&discovered.path, remaining.min(MAX_FILE_SAMPLE_BYTES)) {
                Ok((bytes_read, chunks)) => {
                    remaining = remaining.saturating_sub(bytes_read);
                    for chunk in chunks {
                        self.consume_chunk(&discovered.path, &chunk);
                    }
                }
                Err(ReadFailure::Unsafe) => {
                    error.get_or_insert(LogReadError::UnsafePath);
                }
                Err(ReadFailure::Io) => {
                    error.get_or_insert(LogReadError::ReadFailed);
                }
            }
        }
        if file_count > 0 {
            self.next_file_index = (start_index + 1) % file_count;
        }
        LogSnapshot {
            entries: self.entries.iter().cloned().collect(),
            next_sequence: self.next_sequence,
            truncated: self.truncated,
            error,
        }
    }

    fn discover(&self) -> (Vec<DiscoveredFile>, Option<LogReadError>) {
        let mut found = Vec::new();
        let mut visited = 0usize;
        let mut error = None;
        let mut roots = self.roots.clone();
        roots.sort();
        for root in &roots {
            let Ok(metadata) = fs::symlink_metadata(root) else {
                continue;
            };
            if is_regular_nofollow(&metadata) {
                if supported_log_file(root) {
                    found.push(DiscoveredFile {
                        path: root.clone(),
                        stdout: true,
                    });
                }
                continue;
            }
            if !is_directory_nofollow(&metadata) {
                error.get_or_insert(LogReadError::UnsafePath);
                continue;
            }
            if denied_log_directory(root) {
                continue;
            }
            let mut pending = vec![(root.clone(), 0usize)];
            while let Some((directory, depth)) = pending.pop() {
                if visited >= MAX_DISCOVERY_ENTRIES {
                    error.get_or_insert(LogReadError::DiscoveryLimit);
                    break;
                }
                visited += 1;
                let Ok(entries) = fs::read_dir(&directory) else {
                    error.get_or_insert(LogReadError::ReadFailed);
                    continue;
                };
                let budget = MAX_DISCOVERY_ENTRIES
                    .saturating_sub(visited)
                    .saturating_add(1);
                let mut children: Vec<_> = entries.take(budget).filter_map(Result::ok).collect();
                if children.len() >= budget {
                    error.get_or_insert(LogReadError::DiscoveryLimit);
                }
                children.sort_by_key(|entry| entry.file_name());
                for entry in children {
                    if visited >= MAX_DISCOVERY_ENTRIES {
                        error.get_or_insert(LogReadError::DiscoveryLimit);
                        break;
                    }
                    visited += 1;
                    let path = entry.path();
                    let Ok(metadata) = fs::symlink_metadata(&path) else {
                        continue;
                    };
                    if is_directory_nofollow(&metadata) {
                        if denied_log_directory(&path) {
                            continue;
                        }
                        if depth < MAX_DEPTH {
                            pending.push((path, depth + 1));
                        }
                    } else if is_regular_nofollow(&metadata)
                        && supported_log_file(&path)
                        && !denied_log_file(&path)
                    {
                        found.push(DiscoveredFile {
                            path,
                            stdout: false,
                        });
                        if found.len() > MAX_FILES {
                            found.truncate(MAX_FILES);
                            error.get_or_insert(LogReadError::DiscoveryLimit);
                            return (found, error);
                        }
                    }
                }
            }
        }
        found.sort_by(|left, right| left.path.cmp(&right.path));
        if found.len() > MAX_FILES {
            found.truncate(MAX_FILES);
            error.get_or_insert(LogReadError::DiscoveryLimit);
        }
        (found, error)
    }

    fn assign_stream(&mut self, file: DiscoveredFile) {
        self.streams.entry(file.path).or_insert_with(|| {
            if file.stdout {
                "stdout".to_string()
            } else {
                let stream = format!("engine_{}", self.next_engine_stream);
                self.next_engine_stream = self.next_engine_stream.saturating_add(1);
                stream
            }
        });
    }

    fn read_new_bytes(
        &mut self,
        path: &Path,
        byte_limit: usize,
    ) -> Result<(usize, Vec<Vec<u8>>), ReadFailure> {
        let metadata = fs::symlink_metadata(path).map_err(|_| ReadFailure::Io)?;
        if !is_regular_nofollow(&metadata) {
            return Err(ReadFailure::Unsafe);
        }
        let mut file = open_read_nofollow(path)?;
        let opened_metadata = file.metadata().map_err(|_| ReadFailure::Io)?;
        if !is_regular_nofollow(&opened_metadata) {
            return Err(ReadFailure::Unsafe);
        }
        let current_identity = identity(&opened_metadata, &file);
        if !has_stable_file_identity(current_identity) {
            return Err(ReadFailure::Io);
        }
        let cursor = self.cursors.get_mut(path).ok_or(ReadFailure::Io)?;
        if cursor.identity.is_some_and(|old| {
            opened_metadata.len() < cursor.offset
                || (cursor.offset > 0 && old.replaced_by(current_identity))
        }) {
            cursor.offset = 0;
            cursor.pending.clear();
            cursor.dropping_line = false;
        }
        if opened_metadata.len() < cursor.offset {
            cursor.offset = 0;
            cursor.pending.clear();
            cursor.dropping_line = false;
        }
        if opened_metadata.len().saturating_sub(cursor.offset) > byte_limit as u64 {
            cursor.offset = opened_metadata.len().saturating_sub(byte_limit as u64);
            cursor.pending.clear();
            cursor.dropping_line = true;
            self.truncated = true;
        }
        file.seek(SeekFrom::Start(cursor.offset))
            .map_err(|_| ReadFailure::Io)?;
        let to_read = opened_metadata
            .len()
            .saturating_sub(cursor.offset)
            .min(byte_limit as u64) as usize;
        let mut bytes = vec![0; to_read];
        let count = file.read(&mut bytes).map_err(|_| ReadFailure::Io)?;
        bytes.truncate(count);
        cursor.offset = cursor.offset.saturating_add(count as u64);
        cursor.identity = Some(current_identity);
        Ok((
            count,
            if bytes.is_empty() {
                Vec::new()
            } else {
                vec![bytes]
            },
        ))
    }

    /// Exact values the engine may echo, such as a protected RCon password, are replaced
    /// before a line is sanitized or stored.
    pub(super) fn redact_values(&mut self, values: Vec<zeroize::Zeroizing<String>>) {
        self.secrets
            .extend(values.into_iter().filter(|value| value.len() >= 3));
    }

    fn consume_chunk(&mut self, path: &Path, bytes: &[u8]) {
        let Some(cursor) = self.cursors.get_mut(path) else {
            return;
        };
        let mut complete = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if !cursor.dropping_line {
                    if cursor.pending.last() == Some(&b'\r') {
                        cursor.pending.pop();
                    }
                    complete.push(std::mem::take(&mut cursor.pending));
                }
                cursor.pending.clear();
                cursor.dropping_line = false;
            } else if !cursor.dropping_line {
                if cursor.pending.len() < MAX_LINE_BYTES {
                    cursor.pending.push(*byte);
                } else {
                    cursor.dropping_line = true;
                    self.truncated = true;
                    complete.push(std::mem::take(&mut cursor.pending));
                }
            }
        }
        let stream = self
            .streams
            .get(path)
            .cloned()
            .unwrap_or_else(|| "engine".into());
        for line in complete {
            let mut decoded = zeroize::Zeroizing::new(String::from_utf8_lossy(&line).into_owned());
            for secret in &self.secrets {
                if decoded.contains(secret.as_str()) {
                    *decoded = decoded.replace(secret.as_str(), "[redacted]");
                }
            }
            let sanitized = sanitize_text(&decoded);
            self.push_line(stream.clone(), sanitized);
        }
    }

    fn push_line(&mut self, stream: String, text: String) {
        let bytes = stream.len().saturating_add(text.len());
        if bytes > MAX_HISTORY_BYTES {
            self.truncated = true;
            return;
        }
        while self.entries.len() >= MAX_HISTORY_LINES
            || self.history_bytes.saturating_add(bytes) > MAX_HISTORY_BYTES
        {
            if let Some(removed) = self.entries.pop_front() {
                self.history_bytes = self
                    .history_bytes
                    .saturating_sub(removed.stream.len().saturating_add(removed.text.len()));
                self.truncated = true;
            } else {
                break;
            }
        }
        self.history_bytes = self.history_bytes.saturating_add(bytes);
        self.entries.push_back(LogLine {
            sequence: self.next_sequence,
            stream,
            text,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);
    }
}

#[derive(Clone)]
struct DiscoveredFile {
    path: PathBuf,
    stdout: bool,
}

enum ReadFailure {
    Io,
    Unsafe,
}

fn supported_log_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    extension.eq_ignore_ascii_case("rpt")
        || (extension.eq_ignore_ascii_case("log")
            && (!name.to_ascii_lowercase().starts_with("console")
                || path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|value| value.to_str())
                    .is_some_and(|parent| parent.to_ascii_lowercase().starts_with("logs_"))))
}

fn denied_log_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "config" | "configs" | "secret" | "secrets" | "credential" | "credentials"
            )
        })
}

fn denied_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            ["secret", "credential", "password", "token"]
                .iter()
                .any(|needle| lower.contains(needle))
        })
}

pub(in crate::core::server) fn open_regular_log(path: &Path) -> Option<(File, Metadata)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !is_regular_nofollow(&metadata) {
        return None;
    }
    let file = open_read_nofollow(path).ok()?;
    let opened = file.metadata().ok()?;
    is_regular_nofollow(&opened).then_some((file, opened))
}

fn baseline_file(path: &Path) -> Result<(Metadata, FileIdentity), ReadFailure> {
    let path_metadata = fs::symlink_metadata(path).map_err(|_| ReadFailure::Io)?;
    if !is_regular_nofollow(&path_metadata) {
        return Err(ReadFailure::Unsafe);
    }
    let file = open_read_nofollow(path)?;
    let metadata = file.metadata().map_err(|_| ReadFailure::Io)?;
    if !is_regular_nofollow(&metadata) {
        return Err(ReadFailure::Unsafe);
    }
    #[cfg(unix)]
    if identity(&path_metadata, &file) != identity(&metadata, &file) {
        return Err(ReadFailure::Io);
    }
    let identity = identity(&metadata, &file);
    if !has_stable_file_identity(identity) {
        return Err(ReadFailure::Io);
    }
    Ok((metadata, identity))
}

fn identity(_metadata: &Metadata, _file: &File) -> FileIdentity {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        #[cfg(not(any(unix, windows)))]
        len: _metadata.len(),
        #[cfg(not(any(unix, windows)))]
        modified_ms: modified_ms(_metadata),
        #[cfg(unix)]
        device: _metadata.dev(),
        #[cfg(unix)]
        inode: _metadata.ino(),
        #[cfg(windows)]
        file_id: windows_file_id(_file),
    }
}

#[cfg(windows)]
fn windows_file_id(file: &File) -> Option<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    use winapi::um::fileapi::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return None;
    }
    Some((
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

impl FileIdentity {
    fn replaced_by(self, current: Self) -> bool {
        #[cfg(unix)]
        {
            self.device != current.device || self.inode != current.inode
        }
        #[cfg(windows)]
        {
            self.file_id != current.file_id
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.len == current.len && self.modified_ms != current.modified_ms
        }
    }
}

fn has_stable_file_identity(identity: FileIdentity) -> bool {
    #[cfg(windows)]
    {
        identity.file_id.is_some()
    }
    #[cfg(not(windows))]
    {
        let _ = identity;
        true
    }
}

fn modified_ms(metadata: &Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}

#[cfg(unix)]
fn is_regular_nofollow(metadata: &Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_file() && !metadata.file_type().is_block_device()
}

#[cfg(windows)]
fn is_regular_nofollow(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

#[cfg(not(any(unix, windows)))]
fn is_regular_nofollow(metadata: &Metadata) -> bool {
    metadata.file_type().is_file()
}

#[cfg(unix)]
fn is_directory_nofollow(metadata: &Metadata) -> bool {
    metadata.file_type().is_dir()
}

#[cfg(windows)]
fn is_directory_nofollow(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

#[cfg(not(any(unix, windows)))]
fn is_directory_nofollow(metadata: &Metadata) -> bool {
    metadata.file_type().is_dir()
}

#[cfg(unix)]
fn open_read_nofollow(path: &Path) -> Result<File, ReadFailure> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ReadFailure::Io)
}

#[cfg(windows)]
fn open_read_nofollow(path: &Path) -> Result<File, ReadFailure> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| ReadFailure::Unsafe)
}

#[cfg(not(any(unix, windows)))]
fn open_read_nofollow(path: &Path) -> Result<File, ReadFailure> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|_| ReadFailure::Io)
}

pub(super) fn sanitize_text(input: &str) -> String {
    let mut chars: Vec<char> = input.chars().collect();
    redact_sensitive_assignments(&mut chars);
    redact_spaced_password_keys(&mut chars);
    redact_user_paths(&mut chars);
    redact_endpoints_and_ids(&mut chars);
    let sanitized: String = chars.into_iter().collect();
    if sanitized.len() <= MAX_LINE_BYTES {
        return sanitized;
    }
    let mut end = MAX_LINE_BYTES;
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    sanitized[..end].to_string()
}

fn redact_sensitive_assignments(chars: &mut Vec<char>) {
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '=' && chars[index] != ':' {
            index += 1;
            continue;
        }
        let mut key_end = index;
        while key_end > 0 && chars[key_end - 1].is_whitespace() {
            key_end -= 1;
        }
        if key_end > 0 && matches!(chars[key_end - 1], '"' | '\'') {
            key_end -= 1;
        }
        let mut key_start = key_end;
        while key_start > 0
            && (chars[key_start - 1].is_ascii_alphanumeric() || chars[key_start - 1] == '_')
        {
            key_start -= 1;
        }
        let key: String = chars[key_start..key_end]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        if !sensitive_key(&key) {
            index += 1;
            continue;
        }
        let mut value_start = index + 1;
        while value_start < chars.len() && chars[value_start].is_whitespace() {
            value_start += 1;
        }
        let quote = chars
            .get(value_start)
            .copied()
            .filter(|value| *value == '"' || *value == '\'');
        let mut end = value_start + usize::from(quote.is_some());
        if let Some(quote) = quote {
            while end < chars.len() {
                if chars[end] == '\\' {
                    end = end.saturating_add(2).min(chars.len());
                    continue;
                }
                if chars[end] == quote {
                    if chars.get(end + 1) == Some(&quote) {
                        end += 2;
                        continue;
                    }
                    break;
                }
                end += 1;
            }
            end = (end + usize::from(end < chars.len())).min(chars.len());
        } else {
            while end < chars.len() && !matches!(chars[end], ',' | ';' | '}' | ' ' | '\t') {
                end += 1;
            }
        }
        if value_start < end {
            chars.splice(value_start..end, "[redacted]".chars());
            index = value_start + "[redacted]".len();
            continue;
        }
        index = end.max(index + 1);
    }
}

/// Config echoes such as `RConPassword value` separate key and value by whitespace only.
fn redact_spaced_password_keys(chars: &mut Vec<char>) {
    let mut index = 0;
    while index < chars.len() {
        if !(chars[index].is_ascii_alphanumeric() || chars[index] == '_')
            || (index > 0 && (chars[index - 1].is_ascii_alphanumeric() || chars[index - 1] == '_'))
        {
            index += 1;
            continue;
        }
        let mut key_end = index;
        while key_end < chars.len()
            && (chars[key_end].is_ascii_alphanumeric() || chars[key_end] == '_')
        {
            key_end += 1;
        }
        let key: String = chars[index..key_end]
            .iter()
            .collect::<String>()
            .to_ascii_lowercase();
        let compound =
            key.contains("password") && !matches!(key.as_str(), "password" | "passwords");
        let mut value_start = key_end;
        while value_start < chars.len() && matches!(chars[value_start], ' ' | '\t') {
            value_start += 1;
        }
        if !compound
            || value_start == key_end
            || value_start >= chars.len()
            || matches!(chars[value_start], '=' | ':')
        {
            index = key_end;
            continue;
        }
        let mut end = value_start;
        while end < chars.len() && !chars[end].is_whitespace() && !matches!(chars[end], ',' | ';') {
            end += 1;
        }
        chars.splice(value_start..end, "[redacted]".chars());
        index = value_start + "[redacted]".len();
    }
}

fn sensitive_key(key: &str) -> bool {
    [
        "password",
        "passwd",
        "token",
        "secret",
        "credential",
        "auth",
        "key",
        "host",
        "server",
        "address",
        "serveraddress",
        "serverhost",
        "endpoint",
        "remoteaddress",
        "ip",
        "steamid",
        "identityid",
        "playerid",
        "uid",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn redact_user_paths(chars: &mut Vec<char>) {
    let text: String = chars.iter().collect();
    let lower = text.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut markers = vec![
        "c:\\users\\".to_string(),
        "c:/users/".to_string(),
        "/home/".to_string(),
        "/users/".to_string(),
        "/root/".to_string(),
    ];
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = home
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if !home.is_empty() {
            markers.push(home);
        }
    }
    for marker in markers {
        let mut from = 0;
        while let Some(offset) = lower[from..].find(&marker) {
            let start_byte = from + offset;
            let start = lower[..start_byte].chars().count();
            let mut end = start;
            while end < chars.len()
                && !matches!(chars[end], ' ' | '\t' | '"' | '\'' | ',' | ';' | ')' | ']')
            {
                end += 1;
            }
            spans.push((start, end));
            from = start_byte + marker.len();
        }
    }
    let mut index = 0;
    while index + 2 < chars.len() {
        if chars[index].is_ascii_alphabetic()
            && chars[index + 1] == ':'
            && matches!(chars[index + 2], '\\' | '/')
            && !(index + 3 < chars.len() && chars[index + 2] == '/' && chars[index + 3] == '/')
        {
            let start = index;
            index += 3;
            while index < chars.len()
                && !matches!(
                    chars[index],
                    ' ' | '\t' | '"' | '\'' | ',' | ';' | ')' | ']'
                )
            {
                index += 1;
            }
            spans.push((start, index));
        } else {
            index += 1;
        }
    }
    replace_spans(chars, spans, "<user-path>");
}

fn redact_endpoints_and_ids(chars: &mut Vec<char>) {
    let mut spans = Vec::new();
    spans.extend(url_userinfo_spans(chars));
    spans.extend(url_authority_spans(chars));
    spans.extend(ipv6_spans(chars));
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_ascii_digit() {
            let start = index;
            while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.') {
                index += 1;
            }
            let digit_dots = &chars[start..index];
            let digits = digit_dots.iter().filter(|ch| ch.is_ascii_digit()).count();
            if digits == 17 {
                spans.push((start, index));
                continue;
            }
            if digit_dots.iter().filter(|ch| **ch == '.').count() == 3 && valid_ipv4(digit_dots) {
                let mut end = index;
                if end < chars.len() && chars[end] == ':' {
                    end += 1;
                    while end < chars.len() && chars[end].is_ascii_digit() {
                        end += 1;
                    }
                }
                spans.push((start, end));
            }
        } else {
            index += 1;
        }
    }
    for (start, end) in host_port_spans(chars) {
        spans.push((start, end));
    }
    replace_spans(chars, spans, "<redacted-endpoint-or-id>");
}

fn url_authority_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index + 2 < chars.len() {
        if chars[index] == ':' && chars[index + 1] == '/' && chars[index + 2] == '/' {
            let start = index + 3;
            let mut end = start;
            while end < chars.len()
                && !matches!(
                    chars[end],
                    '/' | '?' | '#' | ' ' | '\t' | '"' | '\'' | ')' | ']'
                )
            {
                end += 1;
            }
            if end > start {
                spans.push((start, end));
            }
            index = end;
        } else {
            index += 1;
        }
    }
    spans
}

fn url_userinfo_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for (at, ch) in chars.iter().enumerate() {
        if *ch != '@' {
            continue;
        }
        let mut start = at;
        while start > 0 && !matches!(chars[start - 1], ' ' | '\t' | '"' | '\'' | '(' | '[') {
            start -= 1;
        }
        let authority_start = start;
        for scheme in authority_start..at.saturating_sub(2) {
            if chars[scheme] == ':' && chars[scheme + 1] == '/' && chars[scheme + 2] == '/' {
                start = scheme + 3;
            }
        }
        let userinfo = &chars[start..at];
        if userinfo.contains(&':') {
            spans.push((start, at));
        }
    }
    spans
}

fn ipv6_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let bracketed = chars[index] == '[';
        if !bracketed && !chars[index].is_ascii_hexdigit() && chars[index] != ':' {
            index += 1;
            continue;
        }
        let start = index;
        if bracketed {
            index += 1;
        }
        let address_start = index;
        while index < chars.len() && (chars[index].is_ascii_hexdigit() || chars[index] == ':') {
            index += 1;
        }
        let address_end = index;
        let colons = chars[address_start..address_end]
            .iter()
            .filter(|ch| **ch == ':')
            .count();
        let closed = bracketed && index < chars.len() && chars[index] == ']';
        let compressed = chars[address_start..address_end]
            .windows(2)
            .any(|pair| pair == [':', ':']);
        if (colons >= 3 || compressed) && (!bracketed || closed) {
            if closed {
                index += 1;
            }
            if index < chars.len() && chars[index] == ':' {
                index += 1;
                while index < chars.len() && chars[index].is_ascii_digit() {
                    index += 1;
                }
            }
            spans.push((start, index));
        } else {
            index = start + usize::from(bracketed);
            if index == start {
                index += 1;
            }
        }
    }
    spans
}

fn valid_ipv4(value: &[char]) -> bool {
    let value: String = value.iter().collect();
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.parse::<u8>().is_ok())
}

fn host_port_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !chars[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len()
            && (chars[index].is_ascii_alphanumeric() || matches!(chars[index], '.' | '-' | '_'))
        {
            index += 1;
        }
        let host_end = index;
        if index < chars.len() && chars[index] == ':' {
            let port_start = index + 1;
            let mut end = port_start;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            let host = &chars[start..host_end];
            if end > port_start
                && host.contains(&'.')
                && host.iter().any(|ch| ch.is_ascii_alphabetic())
            {
                spans.push((start, end));
                index = end;
            }
        }
        index = index.max(host_end);
    }
    spans
}

fn replace_spans(chars: &mut Vec<char>, mut spans: Vec<(usize, usize)>, replacement: &str) {
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        if start >= end || start >= chars.len() {
            continue;
        }
        let end = end.min(chars.len());
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    for (start, end) in merged.into_iter().rev() {
        chars.splice(start..end, replacement.chars());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("foxy-monitor-{label}-{stamp}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn snapshots_only_new_generation_bytes_and_handles_partial_lines() {
        let root = temp_dir("freshness");
        let file = root.join("server.rpt");
        fs::write(&file, b"previous run\n").unwrap();
        let mut collector = LogCollector::new(vec![root.clone()], now_ms());
        assert!(collector.sample().entries.is_empty());
        let mut output = OpenOptions::new().append(true).open(&file).unwrap();
        output.write_all(b"current ").unwrap();
        assert!(collector.sample().entries.is_empty());
        output.write_all(b"run\nnext\n").unwrap();
        let snapshot = collector.sample();
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["current run", "next"]
        );
        assert_eq!(snapshot.entries[0].stream, "engine_0");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delayed_new_log_files_are_read_from_the_current_run() {
        let root = temp_dir("delayed");
        let mut collector = LogCollector::new(vec![root.clone()], 1);
        let file = root.join("server.rpt");
        fs::write(&file, b"fresh\n").unwrap();
        let snapshot = collector.sample();
        assert_eq!(
            snapshot.entries.last().map(|line| line.text.as_str()),
            Some("fresh")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn engine_config_echoes_and_known_secrets_never_reach_log_history() {
        let echoed =
            sanitize_text(" 1:32:46 BattlEye Server: Config entry: RConPassword synthetic22value");
        assert!(echoed.ends_with("RConPassword [redacted]"), "{echoed}");
        assert!(
            !sanitize_text("passwordAdmin\tsynthetic22value; next").contains("synthetic22value")
        );
        assert_eq!(
            sanitize_text("Wrong password attempt"),
            "Wrong password attempt"
        );
        assert_eq!(sanitize_text("RConPassword"), "RConPassword");
        let root = temp_dir("known-secret");
        let file = root.join("run.log");
        fs::write(&file, b"").unwrap();
        let mut collector = LogCollector::new(vec![root.clone()], 1);
        collector.redact_values(vec![zeroize::Zeroizing::new("plain-value-77".into())]);
        fs::write(
            &file,
            b"engine echoed plain-value-77 twice: plain-value-77
",
        )
        .unwrap();
        let snapshot = collector.sample();
        let text = snapshot
            .entries
            .last()
            .map(|line| line.text.clone())
            .unwrap_or_default();
        assert!(
            !text.contains("plain-value-77") && text.contains("[redacted]"),
            "{text}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn filename_rotations_do_not_accumulate_reader_state() {
        let root = temp_dir("bounded-rotation");
        let mut collector = LogCollector::new(vec![root.clone()], 1);
        for index in 0..100 {
            let file = root.join(format!("run_{index}.log"));
            fs::write(&file, b"current line\n").unwrap();
            collector.sample();
            assert!(collector.cursors.len() <= 1);
            assert!(collector.streams.len() <= 1);
            fs::remove_file(file).unwrap();
        }
        collector.sample();
        assert!(collector.cursors.is_empty());
        assert!(collector.streams.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncation_and_rotation_restart_the_file_cursor() {
        let root = temp_dir("rotation");
        let file = root.join("server.log");
        fs::write(&file, b"old line\n").unwrap();
        let mut collector = LogCollector::new(vec![root.clone()], now_ms());
        fs::write(&file, b"new\n").unwrap();
        let snapshot = collector.sample();
        assert_eq!(
            snapshot.entries.last().map(|line| line.text.as_str()),
            Some("new")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_lines_are_bounded_and_marked_truncated() {
        let root = temp_dir("oversized");
        let file = root.join("server.rpt");
        fs::write(&file, b"").unwrap();
        let mut collector = LogCollector::new(vec![root.clone()], now_ms());
        let mut output = OpenOptions::new().append(true).open(&file).unwrap();
        output.write_all(&vec![b'x'; MAX_LINE_BYTES + 20]).unwrap();
        output.write_all(b"\nshort\n").unwrap();
        let snapshot = collector.sample();
        assert!(snapshot.truncated);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|line| line.text.len() <= MAX_LINE_BYTES)
        );
        assert!(snapshot.entries.iter().any(|line| line.text == "short"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exported_lines_redact_secrets_ids_endpoints_and_user_paths() {
        let line = sanitize_text(
            "password=hidden steamId=76561198000000000 host=server.example.invalid:2302 peer=192.0.2.44:2302 path=C:\\Users\\ExampleUser\\logs\\a.rpt",
        );
        for private in [
            "hidden",
            "76561198000000000",
            "server.example.invalid",
            "192.0.2.44",
            "ExampleUser",
        ] {
            assert!(!line.contains(private), "{line}");
        }
        assert!(line.contains("[redacted]"));
        assert!(line.contains("<user-path>"));
    }

    #[test]
    fn exported_urls_and_ipv6_endpoints_are_redacted() {
        let line =
            sanitize_text("connect=https://user:secret@example.invalid/private [2001:db8::1]:2302");
        for private in ["user", "secret", "example.invalid", "2001:db8::1"] {
            assert!(!line.contains(private), "{line}");
        }
    }

    #[test]
    fn quoted_keys_and_escaped_secret_values_are_redacted() {
        for input in [
            r#"{"password": "hidden\"suffix", "name": "public"}"#,
            r#"password = "hidden""suffix";"#,
            r#"{"accessToken": "hidden suffix"}"#,
        ] {
            let line = sanitize_text(input);
            assert!(!line.contains("hidden"), "{line}");
            assert!(!line.contains("suffix"), "{line}");
            assert!(line.contains("[redacted]"), "{line}");
        }
    }

    #[test]
    fn bursts_skip_older_bytes_and_incomplete_leading_line() {
        let root = temp_dir("burst");
        let file = root.join("server.rpt");
        fs::write(&file, b"").unwrap();
        let mut collector = LogCollector::new(vec![root.clone()], 1);
        let mut burst = vec![b'x'; MAX_FILE_SAMPLE_BYTES * 4];
        burst.extend_from_slice(b"\nlatest current line\n");
        fs::write(&file, burst).unwrap();
        let sample = collector.sample();
        assert!(sample.truncated);
        assert_eq!(sample.entries.len(), 1);
        assert_eq!(sample.entries[0].text, "latest current line");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_utf8_is_lossy_without_exposing_paths() {
        let root = temp_dir("utf8");
        let file = root.join("server.rpt");
        fs::write(&file, b"").unwrap();
        let mut collector = LogCollector::new(vec![root.clone()], 1);
        let mut output = OpenOptions::new().append(true).open(&file).unwrap();
        output.write_all(b"bad\xff\n").unwrap();
        let snapshot = collector.sample();
        assert_eq!(
            snapshot.entries.last().map(|line| line.text.as_str()),
            Some("bad\u{fffd}")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_stdout_file_uses_a_stable_safe_stream_label() {
        let root = temp_dir("stdout");
        let stdout = root.join("stdout.log");
        fs::write(&stdout, b"").unwrap();
        let mut collector = LogCollector::new(vec![stdout.clone()], now_ms());
        let mut output = OpenOptions::new().append(true).open(&stdout).unwrap();
        output.write_all(b"ready\n").unwrap();
        let snapshot = collector.sample();
        assert_eq!(snapshot.entries[0].stream, "stdout");
        assert!(
            !snapshot.entries[0]
                .stream
                .contains(root.to_string_lossy().as_ref())
        );
        fs::remove_dir_all(root).unwrap();
    }
}
