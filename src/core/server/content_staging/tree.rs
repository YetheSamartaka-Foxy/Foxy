use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::super::ServerStoreError as Error;
use super::{
    ContentStageControl, ContentStageManifest, ContentStagePhase, MAX_ENTRIES, StageEntry,
    StageEntryKind, StageSource,
};

#[cfg(test)]
mod tests;

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 8192;
const MAX_TREE_PATH_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::core::server) struct SourceSnapshot {
    entries: Vec<StageEntry>,
    stamps: BTreeMap<String, Stamp>,
}

pub(in crate::core::server) fn safe_component(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name == "."
        || name == ".."
        || name.ends_with([' ', '.'])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*/\\".contains(c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "COM¹"
            | "COM²"
            | "COM³"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
            | "LPT¹"
            | "LPT²"
            | "LPT³"
    ) {
        return false;
    }
    true
}

pub(in crate::core::server) fn parse_relative(value: &str) -> Result<PathBuf, Error> {
    if value.len() > 8192 || value.is_empty() {
        return Err(Error::Invalid("Invalid staging path".into()));
    }
    let mut out = PathBuf::new();
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() > 128 || parts.iter().any(|p| !safe_component(p)) {
        return Err(Error::Invalid("Invalid staging path".into()));
    }
    for part in parts {
        out.push(part);
    }
    Ok(out)
}

fn portable_key(path: &str) -> String {
    path.chars().flat_map(char::to_uppercase).collect()
}

fn metadata(path: &Path) -> Result<fs::Metadata, Error> {
    let m = fs::symlink_metadata(path).map_err(|_| Error::Io)?;
    if m.file_type().is_symlink() || !m.is_file() && !m.is_dir() {
        return Err(Error::UnsafePath);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err(Error::UnsafePath);
        }
    }
    Ok(m)
}

fn checked_dir(path: &Path) -> Result<(), Error> {
    super::super::store::check_directory_chain(path)?;
    let m = metadata(path)?;
    if !m.is_dir() {
        return Err(Error::UnsafePath);
    }
    Ok(())
}

pub(super) fn open_read(path: &Path) -> Result<File, Error> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(0x00200000).share_mode(0x00000001);
    }
    let f = opts.open(path).map_err(|_| Error::UnsafePath)?;
    let m = f.metadata().map_err(|_| Error::Io)?;
    if !m.is_file() {
        return Err(Error::UnsafePath);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err(Error::UnsafePath);
        }
    }
    Ok(f)
}

struct ScanState {
    hash_files: bool,
    exclude_manifest: bool,
    path_bytes: usize,
    entry_count: usize,
}

fn inventory(
    root: &Path,
    prefix: &str,
    out: &mut Vec<StageEntry>,
    stamps: &mut BTreeMap<String, Stamp>,
    control: &ContentStageControl,
    scan: &mut ScanState,
) -> Result<(), Error> {
    control.checkpoint()?;
    let dir = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(parse_relative(prefix)?)
    };
    checked_dir(&dir)?;
    let iter = fs::read_dir(&dir).map_err(|_| Error::Io)?;
    let mut children = Vec::new();
    for item in iter {
        let item = item.map_err(|_| Error::Io)?;
        if !(scan.exclude_manifest && prefix.is_empty() && item.file_name() == "manifest.json") {
            if scan.entry_count >= MAX_ENTRIES {
                return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
            }
            scan.entry_count += 1;
        }
        children.push(item);
    }
    children.sort_by_key(|e| e.file_name());
    for child in children {
        control.checkpoint()?;
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| Error::UnsafePath)?;
        if !safe_component(&name) {
            return Err(Error::UnsafePath);
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let m = metadata(&child.path())?;
        if scan.exclude_manifest && prefix.is_empty() && rel == "manifest.json" {
            if !m.is_file() {
                return Err(Error::Corrupt);
            }
            continue;
        }
        if rel.len() > MAX_PATH_BYTES || rel.split('/').count() > 128 {
            return Err(Error::Invalid("Staging tree exceeds path limits".into()));
        }
        scan.path_bytes = scan
            .path_bytes
            .checked_add(rel.len())
            .ok_or(Error::Invalid("Staging tree exceeds path limits".into()))?;
        if scan.path_bytes > MAX_TREE_PATH_BYTES {
            return Err(Error::Invalid("Staging tree exceeds path limits".into()));
        }
        if m.is_dir() {
            out.push(StageEntry {
                path: rel.clone(),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            });
            stamps.insert(
                rel.clone(),
                Stamp {
                    len: m.len(),
                    modified: m.modified().ok(),
                },
            );
            inventory(root, &rel, out, stamps, control, scan)?;
        } else {
            let mut f = open_read(&child.path())?;
            let before = f.metadata().map_err(|_| Error::Io)?;
            let stamp = Stamp {
                len: before.len(),
                modified: before.modified().ok(),
            };
            let mut hasher = blake3::Hasher::new();
            let count = if scan.hash_files {
                let mut count = 0u64;
                let mut buf = [0u8; BUFFER_SIZE];
                loop {
                    control.checkpoint()?;
                    let n = f.read(&mut buf).map_err(|_| Error::Io)?;
                    if n == 0 {
                        break;
                    }
                    count = count
                        .checked_add(n as u64)
                        .ok_or(Error::Invalid("Staging byte limit exceeded".into()))?;
                    if count > stamp.len {
                        return Err(Error::StaleRevision);
                    }
                    hasher.update(&buf[..n]);
                    control.record_bytes(n as u64);
                }
                count
            } else {
                before.len()
            };
            let after = f.metadata().map_err(|_| Error::Io)?;
            let path_after = metadata(&child.path())?;
            let after_stamp = Stamp {
                len: after.len(),
                modified: after.modified().ok(),
            };
            if stamp != after_stamp || path_after.len() != count || count != stamp.len {
                return Err(Error::StaleRevision);
            }
            stamps.insert(rel.clone(), stamp);
            out.push(StageEntry {
                path: rel,
                kind: StageEntryKind::File,
                bytes: count,
                blake3: if scan.hash_files {
                    Some(hasher.finalize().to_hex().to_string())
                } else {
                    None
                },
            });
            if scan.hash_files {
                control.record_file();
            }
        }
        if out.len() > MAX_ENTRIES {
            return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
        }
    }
    Ok(())
}

fn scan_sources(
    sources: &[StageSource],
    control: &ContentStageControl,
    hash_files: bool,
) -> Result<(Vec<StageEntry>, BTreeMap<String, Stamp>), Error> {
    let mut all = vec![StageEntry {
        path: "addons".into(),
        kind: StageEntryKind::Directory,
        bytes: 0,
        blake3: None,
    }];
    let mut stamps = BTreeMap::new();
    let mut scan = ScanState {
        hash_files,
        exclude_manifest: false,
        path_bytes: "addons".len(),
        entry_count: 1,
    };
    for source in sources {
        control.checkpoint()?;
        checked_dir(&source.source)?;
        let base = source
            .source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(Error::UnsafePath)?;
        if !safe_component(base)
            || !source
                .destination
                .strip_prefix("addons/")
                .is_some_and(safe_component)
        {
            return Err(Error::Invalid("Invalid content source destination".into()));
        }
        let dest = source.destination.clone();
        scan.entry_count += 1;
        if scan.entry_count > MAX_ENTRIES {
            return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
        }
        scan.path_bytes = scan
            .path_bytes
            .checked_add(dest.len())
            .ok_or(Error::Invalid("Staging tree exceeds path limits".into()))?;
        if scan.path_bytes > MAX_TREE_PATH_BYTES {
            return Err(Error::Invalid("Staging tree exceeds path limits".into()));
        }
        if all.len() >= MAX_ENTRIES {
            return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
        }
        all.push(StageEntry {
            path: dest.clone(),
            kind: StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        });
        let mut local = Vec::new();
        let mut local_stamps = BTreeMap::new();
        inventory(
            &source.source,
            "",
            &mut local,
            &mut local_stamps,
            control,
            &mut scan,
        )?;
        for mut e in local {
            let old = e.path.clone();
            e.path = format!("{dest}/{old}");
            if e.path.len() > MAX_PATH_BYTES || e.path.split('/').count() > 128 {
                return Err(Error::Invalid("Staging tree exceeds path limits".into()));
            }
            scan.path_bytes = scan
                .path_bytes
                .checked_add(dest.len() + 1)
                .ok_or(Error::Invalid("Staging tree exceeds path limits".into()))?;
            if scan.path_bytes > MAX_TREE_PATH_BYTES {
                return Err(Error::Invalid("Staging tree exceeds path limits".into()));
            }
            if let Some(stamp) = local_stamps.remove(&old) {
                stamps.insert(e.path.clone(), stamp);
            }
            if all.len() >= MAX_ENTRIES {
                return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
            }
            all.push(e);
        }
    }
    all.sort_by(|a, b| a.path.cmp(&b.path));
    validate_paths(&all, hash_files)?;
    Ok((all, stamps))
}

fn validate_paths(entries: &[StageEntry], require_hashes: bool) -> Result<(), Error> {
    if entries.len() > MAX_ENTRIES || entries.windows(2).any(|w| w[0].path >= w[1].path) {
        return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
    }
    let mut seen = HashSet::new();
    for e in entries {
        parse_relative(&e.path)?;
        let key = portable_key(&e.path);
        if !seen.insert(key) {
            return Err(Error::Invalid("Duplicate portable staging path".into()));
        }
        if e.path != "addons" && !e.path.starts_with("addons/") {
            return Err(Error::UnsafePath);
        }
        match e.kind {
            StageEntryKind::Directory if e.bytes == 0 && e.blake3.is_none() => {}
            StageEntryKind::File
                if (!require_hashes && e.blake3.is_none())
                    || e.blake3.as_ref().is_some_and(|h| {
                        h.len() == 64
                            && h.bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    }) => {}
            _ => return Err(Error::Invalid("Invalid staging entry".into())),
        }
    }
    if entries
        .first()
        .is_none_or(|e| e.path != "addons" || e.kind != StageEntryKind::Directory)
    {
        return Err(Error::Invalid("Missing addons root".into()));
    }
    let kinds: BTreeMap<_, _> = entries.iter().map(|e| (e.path.as_str(), e.kind)).collect();
    for e in entries.iter().filter(|e| e.path != "addons") {
        let parent = e
            .path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .ok_or(Error::Invalid("Invalid staging entry".into()))?;
        if kinds.get(parent) != Some(&StageEntryKind::Directory) {
            return Err(Error::Invalid("Invalid staging directory hierarchy".into()));
        }
    }
    Ok(())
}

pub(in crate::core::server) fn validate_generic_entries(
    entries: &[StageEntry],
    total_bytes: u64,
) -> Result<(), Error> {
    if entries.len() > MAX_ENTRIES || entries.windows(2).any(|w| w[0].path >= w[1].path) {
        return Err(Error::Invalid("Staging tree exceeds entry limit".into()));
    }
    let mut seen = HashSet::new();
    let mut kinds = BTreeMap::new();
    let mut path_bytes = 0usize;
    let mut total = 0u64;
    for entry in entries {
        parse_relative(&entry.path)?;
        if !seen.insert(portable_key(&entry.path)) {
            return Err(Error::Invalid("Duplicate portable staging path".into()));
        }
        path_bytes = path_bytes
            .checked_add(entry.path.len())
            .ok_or(Error::Invalid("Staging tree exceeds path limits".into()))?;
        if entry.path.len() > MAX_PATH_BYTES
            || entry.path.split('/').count() > 128
            || path_bytes > MAX_TREE_PATH_BYTES
        {
            return Err(Error::Invalid("Staging tree exceeds path limits".into()));
        }
        match entry.kind {
            StageEntryKind::Directory if entry.bytes == 0 && entry.blake3.is_none() => {}
            StageEntryKind::File
                if entry.blake3.as_ref().is_some_and(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                }) =>
            {
                total = total
                    .checked_add(entry.bytes)
                    .ok_or(Error::Invalid("Staging byte limit exceeded".into()))?;
            }
            _ => return Err(Error::Invalid("Invalid staging entry".into())),
        }
        kinds.insert(entry.path.as_str(), entry.kind);
    }
    for entry in entries {
        if let Some((parent, _)) = entry.path.rsplit_once('/') {
            if kinds.get(parent) != Some(&StageEntryKind::Directory) {
                return Err(Error::Invalid("Invalid staging directory hierarchy".into()));
            }
        } else if entry.kind != StageEntryKind::Directory {
            return Err(Error::Invalid("Invalid staging directory hierarchy".into()));
        }
    }
    if total != total_bytes {
        return Err(Error::Invalid("Invalid staging byte total".into()));
    }
    Ok(())
}

pub(super) fn validate_entries(manifest: &ContentStageManifest) -> Result<(), Error> {
    validate_paths(&manifest.entries, true)?;
    let mut total = 0u64;
    for e in &manifest.entries {
        if e.kind == StageEntryKind::File {
            total = total
                .checked_add(e.bytes)
                .ok_or(Error::Invalid("Staging byte limit exceeded".into()))?;
        }
    }
    if total != manifest.total_bytes {
        return Err(Error::Invalid("Invalid staging byte total".into()));
    }
    Ok(())
}

pub(in crate::core::server) fn inspect_sources(
    sources: &[StageSource],
    control: &ContentStageControl,
) -> Result<Vec<StageEntry>, Error> {
    control.start_phase(ContentStagePhase::Preparing, 0, 0);
    let (metadata_entries, before_stamps) = scan_sources(sources, control, false)?;
    let files = metadata_entries
        .iter()
        .filter(|e| e.kind == StageEntryKind::File)
        .count() as u64;
    let bytes = metadata_entries
        .iter()
        .filter(|e| e.kind == StageEntryKind::File)
        .try_fold(0u64, |total, entry| {
            total
                .checked_add(entry.bytes)
                .ok_or_else(|| Error::Invalid("Staging byte limit exceeded".into()))
        })?;
    control.start_phase(ContentStagePhase::Verifying, files, bytes);
    let (hashed_entries, copied_stamps) = scan_sources(sources, control, true)?;
    let (after_entries, after_stamps) = scan_sources(sources, control, false)?;
    if metadata_entries != after_entries
        || before_stamps != after_stamps
        || copied_stamps != after_stamps
    {
        return Err(Error::StaleRevision);
    }
    Ok(hashed_entries)
}

fn copy_file(
    src: &Path,
    dst: &Path,
    expected: &StageEntry,
    control: &ContentStageControl,
) -> Result<Stamp, Error> {
    let mut input = open_read(src)?;
    let before = input.metadata().map_err(|_| Error::Io)?;
    let stamp = Stamp {
        len: before.len(),
        modified: before.modified().ok(),
    };
    if stamp.len != expected.bytes {
        return Err(Error::StaleRevision);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
        .map_err(|_| Error::Io)?;
    let mut h = blake3::Hasher::new();
    let mut count = 0u64;
    let mut buf = [0u8; BUFFER_SIZE];
    loop {
        control.checkpoint()?;
        let n = input.read(&mut buf).map_err(|_| Error::Io)?;
        if n == 0 {
            break;
        }
        count = count.checked_add(n as u64).ok_or(Error::StaleRevision)?;
        if count > expected.bytes {
            return Err(Error::StaleRevision);
        }
        output.write_all(&buf[..n]).map_err(|_| Error::Io)?;
        h.update(&buf[..n]);
        control.record_bytes(n as u64);
    }
    output.sync_all().map_err(|_| Error::Io)?;
    let after = input.metadata().map_err(|_| Error::Io)?;
    let post = Stamp {
        len: after.len(),
        modified: after.modified().ok(),
    };
    let path_meta = metadata(src)?;
    let path_post = Stamp {
        len: path_meta.len(),
        modified: path_meta.modified().ok(),
    };
    if stamp != post
        || stamp != path_post
        || count != expected.bytes
        || Some(h.finalize().to_hex().to_string()) != expected.blake3
    {
        return Err(Error::StaleRevision);
    }
    control.record_file();
    Ok(stamp)
}

pub(in crate::core::server) fn copy_verified_file(
    src: &Path,
    dst: &Path,
    expected: &StageEntry,
    control: &ContentStageControl,
) -> Result<(), Error> {
    if expected.kind != StageEntryKind::File {
        return Err(Error::Invalid("Expected a file staging entry".into()));
    }
    super::super::store::check_directory_chain(src.parent().ok_or(Error::UnsafePath)?)?;
    super::super::store::check_directory_chain(dst.parent().ok_or(Error::UnsafePath)?)?;
    copy_file(src, dst, expected, control).map(|_| ())
}

pub(in crate::core::server) fn copy_sources(
    manifest: &ContentStageManifest,
    temp_root: &Path,
    control: &ContentStageControl,
) -> Result<SourceSnapshot, Error> {
    validate_entries(manifest)?;
    control.start_phase(
        ContentStagePhase::Copying,
        manifest
            .entries
            .iter()
            .filter(|e| e.kind == StageEntryKind::File)
            .count() as u64,
        manifest.total_bytes,
    );
    checked_dir(temp_root)?;
    let mut root_entries = fs::read_dir(temp_root).map_err(|_| Error::Io)?;
    if root_entries.next().is_some() {
        return Err(Error::UnsafePath);
    }
    let mut source_for = BTreeMap::new();
    for s in &manifest.sources {
        source_for.insert(s.destination.clone(), s);
    }
    let (_, initial_stamps) = scan_sources(&manifest.sources, control, false)?;
    let mut copied_stamps = BTreeMap::new();
    for e in &manifest.entries {
        control.checkpoint()?;
        let rel = parse_relative(&e.path)?;
        let dst = temp_root.join(&rel);
        match e.kind {
            StageEntryKind::Directory => {
                let parent = dst.parent().ok_or(Error::UnsafePath)?;
                super::super::store::check_directory_chain(parent)?;
                fs::create_dir(&dst).map_err(|_| Error::Io)?;
            }
            StageEntryKind::File => {
                let mut selected = None;
                for (index, _) in e.path.match_indices('/') {
                    if let Some(source) = source_for.get(&e.path[..index]) {
                        selected = Some((&e.path[..index], *source));
                    }
                }
                let (dest, source) = selected.ok_or(Error::Corrupt)?;
                let suffix = e
                    .path
                    .strip_prefix(&format!("{dest}/"))
                    .ok_or(Error::Corrupt)?;
                let src = source.source.join(suffix);
                let parent = dst.parent().ok_or(Error::UnsafePath)?;
                super::super::store::check_directory_chain(parent)?;
                super::super::store::check_directory_chain(src.parent().ok_or(Error::UnsafePath)?)?;
                let stamp = copy_file(&src, &dst, e, control)?;
                copied_stamps.insert(e.path.clone(), stamp);
            }
        }
    }
    let (current, current_stamps) = scan_sources(&manifest.sources, control, false)?;
    let expected_metadata: Vec<_> = manifest
        .entries
        .iter()
        .map(|e| StageEntry {
            blake3: None,
            ..e.clone()
        })
        .collect();
    let file_paths: HashSet<_> = manifest
        .entries
        .iter()
        .filter(|entry| entry.kind == StageEntryKind::File)
        .map(|entry| entry.path.as_str())
        .collect();
    let expected_file_stamps: BTreeMap<_, _> = initial_stamps
        .iter()
        .filter(|(path, _)| file_paths.contains(path.as_str()))
        .map(|(path, stamp)| (path.clone(), stamp.clone()))
        .collect();
    if current != expected_metadata
        || initial_stamps != current_stamps
        || copied_stamps != expected_file_stamps
    {
        return Err(Error::StaleRevision);
    }
    Ok(SourceSnapshot {
        entries: current,
        stamps: current_stamps,
    })
}

pub(super) fn check_sources_unchanged(
    sources: &[StageSource],
    snapshot: &SourceSnapshot,
    control: &ContentStageControl,
) -> Result<(), Error> {
    let (entries, stamps) = scan_sources(sources, control, false)?;
    if entries != snapshot.entries || stamps != snapshot.stamps {
        return Err(Error::StaleRevision);
    }
    Ok(())
}

pub(in crate::core::server) fn verify_stage(
    manifest: &ContentStageManifest,
    stage_root: &Path,
    control: &ContentStageControl,
) -> Result<(), Error> {
    validate_entries(manifest)?;
    verify_tree(&manifest.entries, manifest.total_bytes, stage_root, control)
}

pub(in crate::core::server) fn verify_subtree(
    entries: &[StageEntry],
    root: &Path,
    prefix: &str,
    control: &ContentStageControl,
) -> Result<(), Error> {
    parse_relative(prefix)?;
    let total = entries.iter().try_fold(0u64, |total, entry| {
        total.checked_add(entry.bytes).ok_or(Error::Corrupt)
    })?;
    validate_generic_entries(entries, total)?;
    let selected_prefix = format!("{prefix}/");
    let selected: Vec<_> = entries
        .iter()
        .filter(|entry| entry.path.starts_with(&selected_prefix))
        .collect();
    control.start_phase(
        ContentStagePhase::Checking,
        selected
            .iter()
            .filter(|entry| entry.kind == StageEntryKind::File)
            .count() as u64,
        selected.iter().map(|entry| entry.bytes).sum(),
    );
    let mut found = Vec::new();
    inventory(
        root,
        prefix,
        &mut found,
        &mut BTreeMap::new(),
        control,
        &mut ScanState {
            hash_files: true,
            exclude_manifest: false,
            path_bytes: prefix.len(),
            entry_count: 0,
        },
    )?;
    found.sort_by(|a, b| a.path.cmp(&b.path));
    if !found.iter().eq(selected) {
        return Err(Error::Corrupt);
    }
    Ok(())
}

pub(in crate::core::server) fn verify_tree(
    entries: &[StageEntry],
    total_bytes: u64,
    root: &Path,
    control: &ContentStageControl,
) -> Result<(), Error> {
    validate_generic_entries(entries, total_bytes)?;
    control.start_phase(
        ContentStagePhase::Checking,
        entries
            .iter()
            .filter(|e| e.kind == StageEntryKind::File)
            .count() as u64,
        total_bytes,
    );
    (|| {
        checked_dir(root)?;
        let mut found = Vec::new();
        let mut stamps = BTreeMap::new();
        inventory(
            root,
            "",
            &mut found,
            &mut stamps,
            control,
            &mut ScanState {
                hash_files: true,
                exclude_manifest: true,
                path_bytes: 0,
                entry_count: 0,
            },
        )
        .map_err(|e| {
            if e == Error::Cancelled {
                e
            } else {
                Error::Corrupt
            }
        })?;
        found.sort_by(|a, b| a.path.cmp(&b.path));
        if found != entries {
            return Err(Error::Corrupt);
        }
        Ok(())
    })()
}
