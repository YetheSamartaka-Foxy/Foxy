use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::metadata::{MissionMetadata, parse_metadata};
use super::{ContentStageControl, ServerStoreError};

const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MEMBERS: usize = 10_000;
const MAX_PATH: usize = 1024;
const MAX_PATHS: usize = 4 * 1024 * 1024;
const BUFFER: usize = 64 * 1024;
const MAX_HEADER: u64 = 4 * 1024 * 1024;
const MAX_META: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionPayload {
    pub template: String,
    pub world: String,
    pub metadata: MissionMetadata,
    pub source_kind: MissionSourceKind,
    pub files: Vec<MissionInputFile>,
    pub payload_bytes: u64,
    pub payload_blake3: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionSourceKind {
    Pbo,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionInputFile {
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Clone)]
struct Member {
    path: String,
    bytes: u64,
    uncompressed_bytes: u64,
    compressed: bool,
    data: Option<(u64, u64)>,
}

pub fn inspect_payload(
    source: &Path,
    control: &ContentStageControl,
) -> Result<MissionPayload, ServerStoreError> {
    control.checkpoint()?;
    let meta = fs::symlink_metadata(source).map_err(|_| ServerStoreError::Io)?;
    if meta.file_type().is_symlink() || is_reparse(&meta) {
        return Err(ServerStoreError::UnsafePath);
    }
    if meta.is_file() {
        reject_hardlink(&meta)?;
    }
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(ServerStoreError::UnsafePath)?;
    let stem = if name
        .get(name.len().saturating_sub(4)..)
        .is_some_and(|ext| ext.eq_ignore_ascii_case(".pbo"))
    {
        &name[..name.len() - 4]
    } else {
        name
    };
    let (template, world) = mission_name(stem)?;
    let (kind, members) = if meta.is_file() {
        if !name.to_ascii_lowercase().ends_with(".pbo") || meta.len() > MAX_BYTES {
            return Err(invalid("Expected an ordinary PBO within the size limit"));
        }
        (MissionSourceKind::Pbo, parse_pbo(source)?)
    } else if meta.is_dir() {
        (MissionSourceKind::Folder, scan_folder(source, control)?)
    } else {
        return Err(ServerStoreError::UnsafePath);
    };
    validate_members(&members)?;
    if !members
        .iter()
        .any(|m| m.path.eq_ignore_ascii_case("mission.sqm"))
    {
        return Err(invalid("Mission payload must contain mission.sqm"));
    }
    let (files, metadata, member_bytes) = inventory(source, kind, &members, control)?;
    let (payload_bytes, payload_blake3) = match kind {
        MissionSourceKind::Pbo => hash_file(source, meta.len(), control)?,
        MissionSourceKind::Folder => {
            let (bytes, digest) = write_pbo(&mut io::sink(), &members, source, control)?;
            if bytes > MAX_BYTES {
                return Err(invalid("Mission PBO exceeds 512 MiB"));
            }
            (bytes, digest)
        }
    };
    if member_bytes > MAX_BYTES {
        return Err(invalid("Mission payload exceeds 512 MiB"));
    }
    Ok(MissionPayload {
        template,
        world,
        metadata,
        source_kind: kind,
        files,
        payload_bytes,
        payload_blake3,
    })
}

pub fn copy_payload(
    source: &Path,
    reviewed: &MissionPayload,
    destination: &Path,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let current = inspect_payload(source, control)?;
    if current != *reviewed {
        return Err(ServerStoreError::StaleRevision);
    }
    control.checkpoint()?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| ServerStoreError::Io)?;
    let result = (|| {
        match reviewed.source_kind {
            MissionSourceKind::Pbo => {
                let length = fs::metadata(source)
                    .map_err(|_| ServerStoreError::Io)?
                    .len();
                if length > MAX_BYTES {
                    return Err(ServerStoreError::StaleRevision);
                }
                copy_exact(source, &mut out, length, control)?;
            }
            MissionSourceKind::Folder => {
                let members = scan_folder(source, control)?;
                write_pbo(&mut out, &members, source, control)?;
            }
        }
        out.sync_all().map_err(|_| ServerStoreError::Io)?;
        let check = inspect_payload(destination, control)?;
        if check.template != reviewed.template
            || check.world != reviewed.world
            || check.files != reviewed.files
            || check.payload_bytes != reviewed.payload_bytes
            || check.payload_blake3 != reviewed.payload_blake3
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let final_source = inspect_payload(source, control)?;
        if final_source != *reviewed {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    })();
    if result.is_err() {
        drop(out);
        let _ = fs::remove_file(destination);
    }
    result
}

fn mission_name(value: &str) -> Result<(String, String), ServerStoreError> {
    let stem = value.strip_suffix(".pbo").unwrap_or(value);
    let (name, world) = stem
        .rsplit_once('.')
        .ok_or_else(|| invalid("Mission name must be Template.World"))?;
    let safe = |v: &str| {
        !v.is_empty()
            && v.len() <= 128
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if !safe(name) || !safe(world) || name.contains('.') {
        return Err(invalid(
            "Mission template and world must use ASCII letters, digits, underscore, or hyphen",
        ));
    }
    Ok((stem.to_owned(), world.to_owned()))
}

fn scan_folder(
    root: &Path,
    control: &ContentStageControl,
) -> Result<Vec<Member>, ServerStoreError> {
    let mut out = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = dirs.pop() {
        control.checkpoint()?;
        if depth > 32 {
            return Err(invalid("Mission folder nesting exceeds limit"));
        }
        let mut entries = fs::read_dir(&dir)
            .map_err(|_| ServerStoreError::Io)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ServerStoreError::Io)?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            control.checkpoint()?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ServerStoreError::UnsafePath)?;
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            safe_member_path(&rel)?;
            let meta = fs::symlink_metadata(entry.path()).map_err(|_| ServerStoreError::Io)?;
            if meta.file_type().is_symlink() || is_reparse(&meta) {
                return Err(ServerStoreError::UnsafePath);
            }
            if meta.is_dir() {
                dirs.push((entry.path(), rel, depth + 1));
            } else if meta.is_file() {
                reject_hardlink(&meta)?;
                out.push(Member {
                    path: rel,
                    bytes: meta.len(),
                    uncompressed_bytes: meta.len(),
                    compressed: false,
                    data: None,
                });
                if out.len() > MAX_MEMBERS {
                    return Err(invalid("Mission has too many files"));
                }
            } else {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn parse_pbo(path: &Path) -> Result<Vec<Member>, ServerStoreError> {
    let mut file = File::open(path).map_err(|_| ServerStoreError::Io)?;
    let len = file.metadata().map_err(|_| ServerStoreError::Io)?.len();
    let mut first = [0; 5];
    file.read_exact(&mut first)
        .map_err(|_| invalid("Malformed PBO header"))?;
    if first != *b"\0sreV" {
        return Err(invalid("Unsupported PBO header"));
    }
    let mut ext = [0; 16];
    file.read_exact(&mut ext)
        .map_err(|_| invalid("Truncated PBO header"))?;
    let mut consumed = 21u64;
    let mut initial = [0; 1];
    file.read_exact(&mut initial)
        .map_err(|_| invalid("Truncated PBO header"))?;
    consumed += 1;
    if initial[0] != 0 {
        file.seek(SeekFrom::Start(21))
            .map_err(|_| ServerStoreError::Io)?;
        let mut reads = 0usize;
        loop {
            let s = read_cstring(&mut file, &mut consumed)?;
            reads += 1;
            if s.is_empty() {
                break;
            }
            if reads > MAX_MEMBERS * 2
                || file.stream_position().map_err(|_| ServerStoreError::Io)? > MAX_HEADER
            {
                return Err(invalid("PBO headers exceed limit"));
            }
        }
    }
    let mut headers = Vec::new();
    let mut header_bytes = 0u64;
    loop {
        let name = read_cstring(&mut file, &mut consumed)?;
        if consumed > MAX_HEADER {
            return Err(invalid("PBO headers exceed limit"));
        }
        let mut fields = [0; 20];
        file.read_exact(&mut fields)
            .map_err(|_| invalid("Truncated PBO member header"))?;
        consumed += 20;
        let packing = u32::from_le_bytes(fields[0..4].try_into().unwrap());
        let original = u32::from_le_bytes(fields[4..8].try_into().unwrap()) as u64;
        let size = u32::from_le_bytes(fields[16..20].try_into().unwrap()) as u64;
        if name.is_empty() {
            break;
        }
        if packing == 0 && original != 0 && original != size {
            return Err(invalid("Invalid uncompressed PBO member size"));
        }
        let name = name.replace('\\', "/");
        safe_member_path(&name)?;
        header_bytes = header_bytes
            .checked_add(name.len() as u64 + 21)
            .ok_or(ServerStoreError::Corrupt)?;
        if header_bytes > MAX_HEADER {
            return Err(invalid("PBO headers exceed limit"));
        }
        headers.push((name, size, original, packing));
        if headers.len() > MAX_MEMBERS {
            return Err(invalid("PBO has too many members"));
        }
    }
    let data_start = file.stream_position().map_err(|_| ServerStoreError::Io)?;
    if data_start > MAX_HEADER {
        return Err(invalid("PBO headers exceed limit"));
    }
    let mut offset = data_start;
    let mut out = Vec::with_capacity(headers.len());
    for (name, bytes, original, packing) in headers {
        let end = offset.checked_add(bytes).ok_or(ServerStoreError::Corrupt)?;
        if end > len {
            return Err(invalid("Truncated PBO member data"));
        }
        out.push(Member {
            path: name,
            bytes,
            uncompressed_bytes: if packing == 0 || original == 0 {
                bytes
            } else {
                original
            },
            compressed: packing != 0,
            data: Some((offset, bytes)),
        });
        offset = end;
    }
    let remaining = len.saturating_sub(offset);
    if remaining != 0 {
        if remaining != 21 {
            return Err(invalid("PBO has trailing or truncated data"));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| ServerStoreError::Io)?;
        let mut footer = [0u8; 21];
        file.read_exact(&mut footer)
            .map_err(|_| invalid("Truncated PBO checksum footer"))?;
        if footer[0] != 0 {
            return Err(invalid("Malformed PBO checksum footer"));
        }
    }
    if offset > len {
        return Err(invalid("PBO has trailing or truncated data"));
    }
    Ok(out)
}

fn read_cstring(file: &mut File, consumed: &mut u64) -> Result<String, ServerStoreError> {
    let mut bytes = Vec::new();
    loop {
        let mut b = [0];
        file.read_exact(&mut b)
            .map_err(|_| invalid("Truncated PBO header"))?;
        *consumed += 1;
        if b[0] == 0 {
            break;
        }
        bytes.push(b[0]);
        if bytes.len() > MAX_PATH {
            return Err(invalid("PBO path exceeds limit"));
        }
    }
    String::from_utf8(bytes).map_err(|_| invalid("PBO paths must be UTF-8"))
}

fn inventory(
    source: &Path,
    kind: MissionSourceKind,
    members: &[Member],
    control: &ContentStageControl,
) -> Result<(Vec<MissionInputFile>, MissionMetadata, u64), ServerStoreError> {
    let mut files = Vec::new();
    let mut meta_files = Vec::new();
    let mut total = 0u64;
    for member in members {
        control.checkpoint()?;
        total = total
            .checked_add(member.uncompressed_bytes)
            .ok_or_else(|| invalid("Mission payload size overflow"))?;
        if total > MAX_BYTES {
            return Err(invalid("Mission payload exceeds 512 MiB"));
        }
        let mut file = match kind {
            MissionSourceKind::Folder => {
                File::open(source.join(&member.path)).map_err(|_| ServerStoreError::Io)?
            }
            MissionSourceKind::Pbo => File::open(source).map_err(|_| ServerStoreError::Io)?,
        };
        if let Some((offset, _)) = member.data {
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| ServerStoreError::Io)?;
        }
        let mut left = member.bytes;
        let mut hash = blake3::Hasher::new();
        let mut meta_bytes = Vec::new();
        let is_meta = member.path.rsplit('/').next().is_some_and(|n| {
            n.eq_ignore_ascii_case("description.ext") || n.eq_ignore_ascii_case("mission.sqm")
        });
        if is_meta && member.bytes <= MAX_META {
            meta_bytes.reserve(member.bytes as usize);
        }
        let mut buf = [0; BUFFER];
        while left > 0 {
            control.checkpoint()?;
            let chunk = left.min(buf.len() as u64) as usize;
            let count = file
                .read(&mut buf[..chunk])
                .map_err(|_| ServerStoreError::Io)?;
            if count == 0 {
                return Err(ServerStoreError::StaleRevision);
            }
            hash.update(&buf[..count]);
            if is_meta && member.bytes <= MAX_META {
                meta_bytes.extend_from_slice(&buf[..count]);
            }
            left -= count as u64;
        }
        let digest = hash.finalize().to_hex().to_string();
        if is_meta {
            if member.compressed || member.bytes > MAX_META {
                meta_files.push((member.path.clone(), Vec::new()));
            } else {
                meta_files.push((member.path.clone(), meta_bytes));
            }
        }
        files.push(MissionInputFile {
            path: member.path.clone(),
            bytes: member.bytes,
            blake3: digest,
        });
    }
    let mut metadata = parse_metadata(&meta_files);
    for (path, bytes) in meta_files {
        if bytes.is_empty() {
            let reason = members.iter().find(|member| member.path == path).map_or(
                "metadata exceeds 1 MiB",
                |member| {
                    if member.compressed {
                        "metadata member is compressed; ignored"
                    } else {
                        "metadata exceeds 1 MiB; ignored"
                    }
                },
            );
            metadata.warnings.push(format!("{path}: {reason}"));
        }
    }
    Ok((files, metadata, total))
}

fn write_pbo<W: Write>(
    out: &mut W,
    members: &[Member],
    root: &Path,
    control: &ContentStageControl,
) -> Result<(u64, String), ServerStoreError> {
    let mut sha = Sha1::new();
    let mut blake = blake3::Hasher::new();
    let mut total = 0u64;
    write_archive_bytes(out, &mut sha, &mut blake, &mut total, b"\0sreV")?;
    write_archive_bytes(out, &mut sha, &mut blake, &mut total, &[0; 16])?;
    write_archive_bytes(out, &mut sha, &mut blake, &mut total, &[0])?;
    for m in members {
        control.checkpoint()?;
        write_archive_bytes(out, &mut sha, &mut blake, &mut total, m.path.as_bytes())?;
        write_archive_bytes(out, &mut sha, &mut blake, &mut total, &[0])?;
        write_archive_bytes(out, &mut sha, &mut blake, &mut total, &[0; 16])?;
        write_archive_bytes(
            out,
            &mut sha,
            &mut blake,
            &mut total,
            &(m.bytes as u32).to_le_bytes(),
        )?;
    }
    write_archive_bytes(out, &mut sha, &mut blake, &mut total, &[0; 21])?;
    for m in members {
        control.checkpoint()?;
        let mut input = File::open(root.join(&m.path)).map_err(|_| ServerStoreError::Io)?;
        let mut left = m.bytes;
        let mut buf = [0; BUFFER];
        while left > 0 {
            control.checkpoint()?;
            let n = input
                .read(&mut buf[..left.min(BUFFER as u64) as usize])
                .map_err(|_| ServerStoreError::Io)?;
            if n == 0 {
                return Err(ServerStoreError::StaleRevision);
            }
            write_archive_bytes(out, &mut sha, &mut blake, &mut total, &buf[..n])?;
            left -= n as u64;
        }
    }
    let checksum = sha.finalize();
    out.write_all(&[0]).map_err(|_| ServerStoreError::Io)?;
    out.write_all(&checksum).map_err(|_| ServerStoreError::Io)?;
    blake.update(&[0]);
    blake.update(&checksum);
    total = total.checked_add(21).ok_or(ServerStoreError::Corrupt)?;
    Ok((total, blake.finalize().to_hex().to_string()))
}

fn write_archive_bytes<W: Write>(
    out: &mut W,
    sha: &mut Sha1,
    blake: &mut blake3::Hasher,
    total: &mut u64,
    bytes: &[u8],
) -> Result<(), ServerStoreError> {
    out.write_all(bytes).map_err(|_| ServerStoreError::Io)?;
    sha.update(bytes);
    blake.update(bytes);
    *total = total
        .checked_add(bytes.len() as u64)
        .ok_or(ServerStoreError::Corrupt)?;
    Ok(())
}

fn hash_file(
    source: &Path,
    bytes: u64,
    control: &ContentStageControl,
) -> Result<(u64, String), ServerStoreError> {
    let mut file = File::open(source).map_err(|_| ServerStoreError::Io)?;
    let mut left = bytes;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; BUFFER];
    while left > 0 {
        control.checkpoint()?;
        let count = file
            .read(&mut buffer[..left.min(BUFFER as u64) as usize])
            .map_err(|_| ServerStoreError::Io)?;
        if count == 0 {
            return Err(ServerStoreError::StaleRevision);
        }
        hasher.update(&buffer[..count]);
        left -= count as u64;
    }
    Ok((bytes, hasher.finalize().to_hex().to_string()))
}

fn copy_exact(
    source: &Path,
    out: &mut File,
    bytes: u64,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let mut input = File::open(source).map_err(|_| ServerStoreError::Io)?;
    copy_exact_reader(&mut input, out, bytes, control)
}
fn copy_exact_reader(
    input: &mut File,
    out: &mut File,
    bytes: u64,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let mut left = bytes;
    let mut buf = [0; BUFFER];
    while left > 0 {
        control.checkpoint()?;
        let n = input
            .read(&mut buf[..left.min(BUFFER as u64) as usize])
            .map_err(|_| ServerStoreError::Io)?;
        if n == 0 {
            return Err(ServerStoreError::StaleRevision);
        }
        out.write_all(&buf[..n]).map_err(|_| ServerStoreError::Io)?;
        left -= n as u64;
    }
    Ok(())
}
fn validate_members(members: &[Member]) -> Result<(), ServerStoreError> {
    let mut seen = HashSet::new();
    let mut total = 0usize;
    if members.len() > MAX_MEMBERS {
        return Err(invalid("Mission has too many files"));
    }
    for m in members {
        safe_member_path(&m.path)?;
        total = total.saturating_add(m.path.len());
        if total > MAX_PATHS || !seen.insert(m.path.to_ascii_lowercase()) {
            return Err(invalid("Mission contains colliding member paths"));
        }
    }
    Ok(())
}
fn safe_member_path(value: &str) -> Result<(), ServerStoreError> {
    if value.is_empty()
        || value.len() > MAX_PATH
        || value.contains('\\')
        || value.starts_with('/')
        || value.bytes().any(|b| {
            b == 0 || !b.is_ascii() || matches!(b, b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|')
        })
    {
        return Err(ServerStoreError::UnsafePath);
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || value
            .split('/')
            .any(|part| part.is_empty() || part.ends_with(' ') || part.ends_with('.'))
    {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}
fn reject_hardlink(_meta: &fs::Metadata) -> Result<(), ServerStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if _meta.nlink() > 1 {
            return Err(ServerStoreError::UnsafePath);
        }
    }
    Ok(())
}
#[cfg(windows)]
fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &fs::Metadata) -> bool {
    false
}
fn invalid(value: &str) -> ServerStoreError {
    ServerStoreError::Invalid(value.to_owned())
}
