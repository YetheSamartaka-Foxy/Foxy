//! A record of files whose every part was read and matched the remote, kept
//! beside `database.db` so a whole-database wipe (a schema bump) does not
//! cost a full re-read of a payload nothing has touched. A file is restored
//! from it only while its NTFS identity, size, write and change times and
//! change-journal USN are exactly those captured around the hash that
//! verified it, and only where the database holds no local state for it. An
//! integrity recheck never restores; every hash run refreshes the record.

use super::part_hashes::PartSpanSource;
use super::scheduling::{FileHashJob, FileHashResult};
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

const RECORD_FILE: &str = "verified_hashes.json";
const RECORD_VERSION: u32 = 1;
/// Entries not verified or restored for this long are dropped on save.
const STALE_AFTER_SECS: u64 = 180 * 24 * 60 * 60;

/// Serialises load-modify-save of the record between concurrent hash runs.
static RECORD_WRITE: Mutex<()> = Mutex::new(());

/// Where one operation's record lives and whether it may restore from it.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedHashRecordUse {
    path: PathBuf,
    trust: bool,
}

impl VerifiedHashRecordUse {
    /// The active game space's record. `trust` is false for flows that must
    /// read every byte (integrity recheck, force redownload).
    pub(crate) fn for_active_space(trust: bool) -> Self {
        Self::at(
            record_path(&crate::core::game::spaces::active_game_space_dir()),
            trust,
        )
    }

    pub(crate) fn at(path: PathBuf, trust: bool) -> Self {
        Self { path, trust }
    }

    /// Whether this operation may restore any file under `folder`: a wipe
    /// forgets a repository's entries but keeps the record file.
    pub(crate) async fn may_restore_under(&self, folder: &str) -> bool {
        if !self.trust {
            return false;
        }
        let path = self.path.clone();
        let prefix = format!("{}/", path_key(folder));
        tokio::task::spawn_blocking(move || {
            path.is_file()
                && load(&path)
                    .entries
                    .keys()
                    .any(|key| key.starts_with(&prefix))
        })
        .await
        .unwrap_or(false)
    }
}

pub(crate) fn record_path(space_dir: &Path) -> PathBuf {
    space_dir.join(RECORD_FILE)
}

/// Everything about a file that a write, truncation, replacement or restore
/// from backup changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FileIdentity {
    volume: u32,
    file_id: u64,
    size: u64,
    modified: i64,
    changed: i64,
    /// The file's last change-journal record; `None` when the volume keeps
    /// no journal (the query then answers 0).
    usn: Option<i64>,
}

/// A file the hash run verified end to end, ready to record.
#[derive(Clone, Debug)]
pub(super) struct VerifiedFile {
    pub(super) identity: FileIdentity,
    pub(super) signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    identity: FileIdentity,
    signature: String,
    fingerprint: Option<String>,
    seen: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecordFile {
    version: u32,
    entries: HashMap<String, Entry>,
}

fn path_key(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn load(path: &Path) -> RecordFile {
    let parsed = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RecordFile>(&bytes).ok());
    match parsed {
        Some(record) if record.version == RECORD_VERSION => record,
        _ => RecordFile {
            version: RECORD_VERSION,
            entries: HashMap::new(),
        },
    }
}

fn save(path: &Path, record: &RecordFile) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

/// What the record binds a verification to: the manifest's parts for the
/// file (order, path, remote span and checksum) and its whole-file checksum,
/// so a new version of the file never matches an old entry.
pub(super) fn parts_signature<'a>(
    file_remote_checksum: &str,
    parts: impl IntoIterator<Item = &'a FoxyModFilePart>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"FOXY_VERIFIED_PARTS_V1");
    let mut field = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(file_remote_checksum.to_ascii_uppercase().as_bytes());
    for part in parts {
        field(&part.data_order.to_le_bytes());
        field(part.path.as_bytes());
        field(&part.remote_start.to_le_bytes());
        field(&part.remote_length.to_le_bytes());
        field(part.remote_checksum.to_ascii_uppercase().as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// True when every part matched the remote at the remote offsets, or the
/// whole-file checksum matched for a file without parts.
pub(super) fn result_is_clean(
    file_remote_checksum: &str,
    parts: &[(usize, FoxyModFilePart)],
    whole_file_checksum: Option<&str>,
) -> bool {
    if parts.is_empty() {
        return !file_remote_checksum.is_empty()
            && whole_file_checksum
                .is_some_and(|checksum| checksum.eq_ignore_ascii_case(file_remote_checksum));
    }
    parts.iter().all(|(_, part)| {
        !part.local_checksum.is_empty()
            && part
                .local_checksum
                .eq_ignore_ascii_case(&part.remote_checksum)
            && part.local_start == part.remote_start
            && part.local_length == part.remote_length
    })
}

/// Splits `jobs` into results restored from the record and the jobs that
/// still need hashing. Only runs whose context trusts the record restore.
pub(super) async fn restore(
    record: &VerifiedHashRecordUse,
    jobs: Vec<FileHashJob>,
) -> (Vec<FileHashResult>, Vec<FileHashJob>) {
    if !record.trust || jobs.is_empty() {
        return (Vec::new(), jobs);
    }
    let started = Instant::now();
    let path = record.path.clone();
    let Ok(stored) = tokio::task::spawn_blocking(move || load(&path)).await else {
        return (Vec::new(), jobs);
    };
    if stored.entries.is_empty() {
        return (Vec::new(), jobs);
    }
    // Jobs the database knows nothing about, whose manifest parts are the ones
    // the record verified.
    let candidates: Vec<(usize, String)> = jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| {
            job.span_source == PartSpanSource::DetectLocalLayout && !job.has_local_baseline
        })
        .filter_map(|(index, job)| {
            let entry = stored.entries.get(&path_key(&job.file_path))?;
            let signature = parts_signature(&job.file_remote_checksum, job.indexed_parts.parts());
            (entry.signature == signature).then_some((index, signature))
        })
        .collect();
    let paths: Vec<String> = candidates
        .iter()
        .map(|(index, _)| jobs[*index].file_path.clone())
        .collect();
    let identities = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|path| capture_identity(Path::new(path)))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let mut unchanged: HashMap<usize, String> = HashMap::new();
    for ((index, signature), identity) in candidates.iter().zip(&identities) {
        let entry = &stored.entries[&path_key(&jobs[*index].file_path)];
        if identity.as_ref() == Some(&entry.identity) {
            unchanged.insert(*index, signature.clone());
        }
    }
    let mut restored = Vec::new();
    let mut remaining = Vec::new();
    for (index, job) in jobs.into_iter().enumerate() {
        match unchanged.remove(&index) {
            Some(signature) => {
                let entry = &stored.entries[&path_key(&job.file_path)];
                restored.push(restored_result(job, entry, signature));
            }
            None => remaining.push(job),
        }
    }
    let candidates = candidates.len();
    let bytes: u64 = restored
        .iter()
        .map(|result| {
            result
                .updated_parts
                .iter()
                .map(|(_, part)| part.remote_length)
                .sum::<u64>()
        })
        .sum();
    info!(
        "Verified hash record: restored_files={} restored_bytes={} candidates={} hashing_files={} elapsed={:.3}s",
        restored.len(),
        bytes,
        candidates,
        remaining.len(),
        started.elapsed().as_secs_f64()
    );
    (restored, remaining)
}

fn restored_result(job: FileHashJob, entry: &Entry, signature: String) -> FileHashResult {
    let parts_count = job.indexed_parts.len();
    let whole_file_checksum = job
        .indexed_parts
        .is_empty()
        .then(|| job.file_remote_checksum.clone());
    let updated_parts = job
        .indexed_parts
        .iter()
        .map(|(part_idx, part)| {
            let mut part = part.clone();
            part.local_checksum = part.remote_checksum.clone();
            part.local_start = part.remote_start;
            part.local_length = part.remote_length;
            (part_idx, part)
        })
        .collect();
    FileHashResult {
        file_idx: job.file_idx,
        updated_parts,
        whole_file_checksum,
        content_hash: entry.fingerprint.clone(),
        file_path: job.file_path,
        elapsed: std::time::Duration::ZERO,
        parts_count,
        missing_file: false,
        part_metrics: Default::default(),
        verified: Some(VerifiedFile {
            identity: entry.identity.clone(),
            signature,
        }),
    }
}

/// Records every verified file of a finished run and forgets the files the
/// run found changed, unreadable or not matching the remote.
pub(super) async fn update<'a>(
    record: &VerifiedHashRecordUse,
    results: impl Iterator<Item = &'a FileHashResult>,
) {
    let updates: Vec<(String, Option<Entry>)> = results
        .map(|result| {
            let entry = result.verified.as_ref().map(|verified| Entry {
                identity: verified.identity.clone(),
                signature: verified.signature.clone(),
                fingerprint: result.content_hash.clone(),
                seen: 0,
            });
            (path_key(&result.file_path), entry)
        })
        .collect();
    if updates.is_empty() {
        return;
    }
    let path = record.path.clone();
    let started = Instant::now();
    let saved =
        tokio::task::spawn_blocking(move || update_blocking(&path, updates, now_secs())).await;
    match saved {
        Ok(Ok((recorded, forgotten, total))) => info!(
            "Verified hash record: recorded={} forgotten={} entries={} elapsed={:.3}s",
            recorded,
            forgotten,
            total,
            started.elapsed().as_secs_f64()
        ),
        Ok(Err(err)) => warn!("Verified hash record could not be saved: {}", err),
        Err(err) => error!("Verified hash record save task failed: {}", err),
    }
}

fn update_blocking(
    path: &Path,
    updates: Vec<(String, Option<Entry>)>,
    now: u64,
) -> std::io::Result<(usize, usize, usize)> {
    let _guard = RECORD_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut record = load(path);
    let (mut recorded, mut forgotten) = (0, 0);
    for (key, entry) in updates {
        match entry {
            Some(mut entry) => {
                entry.seen = now;
                record.entries.insert(key, entry);
                recorded += 1;
            }
            None => forgotten += usize::from(record.entries.remove(&key).is_some()),
        }
    }
    record
        .entries
        .retain(|_, entry| now.saturating_sub(entry.seen) < STALE_AFTER_SECS);
    save(path, &record)?;
    Ok((recorded, forgotten, record.entries.len()))
}

/// Drops the active game space's entries under `folder`, for a repository
/// whose database state the user asked Foxy to forget.
pub(crate) fn forget_verified_hashes_under(folder: &str) {
    let path = record_path(&crate::core::game::spaces::active_game_space_dir());
    match forget_under(&path, folder) {
        Ok(0) => {}
        Ok(dropped) => info!(
            "Verified hash record: forgot {} entries under a wiped repository folder",
            dropped
        ),
        Err(err) => warn!("Verified hash record could not be updated: {}", err),
    }
}

fn forget_under(record_path: &Path, folder: &str) -> std::io::Result<usize> {
    let _guard = RECORD_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !record_path.exists() {
        return Ok(0);
    }
    let mut record = load(record_path);
    let prefix = format!("{}/", path_key(folder));
    let before = record.entries.len();
    record.entries.retain(|key, _| !key.starts_with(&prefix));
    let dropped = before - record.entries.len();
    if dropped > 0 {
        save(record_path, &record)?;
    }
    Ok(dropped)
}

pub(super) async fn capture_identity_async(path: &str) -> Option<FileIdentity> {
    let path = PathBuf::from(path);
    tokio::task::spawn_blocking(move || capture_identity(&path))
        .await
        .ok()
        .flatten()
}

#[cfg(windows)]
pub(super) fn capture_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use winapi::um::fileapi::{
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, GetFileInformationByHandle,
    };
    use winapi::um::ioapiset::DeviceIoControl;
    use winapi::um::minwinbase::FileBasicInfo;
    use winapi::um::winbase::GetFileInformationByHandleEx;
    use winapi::um::winioctl::FSCTL_READ_FILE_USN_DATA;
    use winapi::um::winnt::FILE_READ_ATTRIBUTES;

    // Attribute access only: opening for data could start an on-access scan.
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .open(path)
        .ok()?;
    let handle = file.as_raw_handle().cast();
    // SAFETY: all-zero is valid for both structs, and each call writes only
    // into the struct it is given, whose size it is told.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return None;
    }
    let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&raw mut basic).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_ok == 0 {
        return None;
    }
    // A USN_RECORD_V2 or V3 with the file name; 1 KiB holds any name.
    let mut usn_record = [0u64; 128];
    let mut returned = 0u32;
    // SAFETY: the output buffer is 8-aligned and its size is passed.
    let usn_ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_READ_FILE_USN_DATA,
            std::ptr::null_mut(),
            0,
            usn_record.as_mut_ptr().cast(),
            size_of_val(&usn_record) as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    let usn = (usn_ok != 0)
        .then(|| usn_from_record(bytemuck_bytes(&usn_record), returned as usize))
        .flatten()
        .filter(|usn| *usn != 0);
    // SAFETY: LARGE_INTEGER is a union over the same 64 bits.
    let (modified, changed) = unsafe {
        (
            *basic.LastWriteTime.QuadPart(),
            *basic.ChangeTime.QuadPart(),
        )
    };
    Some(FileIdentity {
        volume: info.dwVolumeSerialNumber,
        file_id: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        size: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        modified,
        changed,
        usn,
    })
}

#[cfg(windows)]
fn bytemuck_bytes(words: &[u64; 128]) -> &[u8] {
    // SAFETY: u64 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), size_of_val(words)) }
}

/// The `Usn` field of a USN_RECORD_V2 (64-bit file ids) or V3 (128-bit).
fn usn_from_record(bytes: &[u8], len: usize) -> Option<i64> {
    let bytes = bytes.get(..len)?;
    let major = u16::from_le_bytes(bytes.get(4..6)?.try_into().ok()?);
    let at = match major {
        2 => 24,
        3 => 40,
        _ => return None,
    };
    Some(i64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(not(windows))]
pub(super) fn capture_identity(_path: &Path) -> Option<FileIdentity> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::calculate_hashes::scheduling::JobParts;

    fn part(order: i64, start: u64, len: u64, checksum: &str) -> (usize, FoxyModFilePart) {
        (
            order as usize,
            FoxyModFilePart {
                path: format!("part{order}"),
                remote_start: start,
                remote_length: len,
                remote_checksum: checksum.to_owned(),
                local_checksum: checksum.to_owned(),
                local_start: start,
                local_length: len,
                data_order: order,
                ..Default::default()
            },
        )
    }

    #[test]
    fn signature_changes_with_any_part_field_or_the_file_checksum() {
        let parts = vec![part(0, 0, 10, "AA"), part(1, 10, 5, "BB")];
        let signature = |parts: &[(usize, FoxyModFilePart)], checksum: &str| {
            parts_signature(checksum, parts.iter().map(|(_, part)| part))
        };
        let base = signature(&parts, "FF");
        assert_eq!(base, signature(&parts, "ff"));
        assert_ne!(base, signature(&parts, "FE"));
        let mut moved = parts.clone();
        moved[1].1.remote_start = 11;
        assert_ne!(base, signature(&moved, "FF"));
        let mut rehashed = parts.clone();
        rehashed[0].1.remote_checksum = "AB".into();
        assert_ne!(base, signature(&rehashed, "FF"));
        assert_ne!(base, signature(&parts[..1], "FF"));
    }

    #[test]
    fn only_files_matching_the_remote_at_its_offsets_are_clean() {
        let clean = vec![part(0, 0, 10, "AA"), part(1, 10, 5, "BB")];
        assert!(result_is_clean("FF", &clean, None));
        let mut shifted = clean.clone();
        shifted[1].1.local_start = 12;
        assert!(!result_is_clean("FF", &shifted, None));
        let mut unread = clean.clone();
        unread[0].1.local_checksum.clear();
        assert!(!result_is_clean("FF", &unread, None));
        assert!(result_is_clean("FF", &[], Some("ff")));
        assert!(!result_is_clean("FF", &[], Some("FE")));
        assert!(!result_is_clean("FF", &[], None));
        assert!(!result_is_clean("", &[], Some("")));
    }

    #[test]
    fn usn_is_read_from_both_record_versions() {
        let mut v2 = vec![0u8; 64];
        v2[4] = 2;
        v2[24..32].copy_from_slice(&1234i64.to_le_bytes());
        assert_eq!(usn_from_record(&v2, 64), Some(1234));
        let mut v3 = vec![0u8; 80];
        v3[4] = 3;
        v3[40..48].copy_from_slice(&99i64.to_le_bytes());
        assert_eq!(usn_from_record(&v3, 80), Some(99));
        assert_eq!(usn_from_record(&v3, 20), None);
        v3[4] = 4;
        assert_eq!(usn_from_record(&v3, 80), None);
    }

    #[test]
    fn updates_record_forget_and_prune_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RECORD_FILE);
        let identity = FileIdentity {
            volume: 1,
            file_id: 2,
            size: 3,
            modified: 4,
            changed: 5,
            usn: Some(6),
        };
        let entry = |signature: &str| Entry {
            identity: identity.clone(),
            signature: signature.to_owned(),
            fingerprint: Some("F".into()),
            seen: 0,
        };
        let now = 1_000_000_000;
        update_blocking(
            &path,
            vec![
                (path_key("C:/Repo/@a/addons/x.pbo"), Some(entry("s1"))),
                (path_key("C:/Repo/@b/y.pbo"), Some(entry("s2"))),
                (path_key("C:/Other/z.pbo"), Some(entry("s3"))),
            ],
            now,
        )
        .unwrap();
        assert_eq!(load(&path).entries.len(), 3);

        let (recorded, forgotten, total) = update_blocking(
            &path,
            vec![(path_key("C:/Other/z.pbo"), None)],
            now + STALE_AFTER_SECS - 1,
        )
        .unwrap();
        assert_eq!((recorded, forgotten, total), (0, 1, 2));

        assert_eq!(forget_under(&path, "C:\\Repo\\@a").unwrap(), 1);
        assert_eq!(forget_under(&path, "C:/Rep").unwrap(), 0);
        let (_, _, total) =
            update_blocking(&path, vec![("k".into(), None)], now + STALE_AFTER_SECS).unwrap();
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn a_record_may_restore_only_a_folder_it_holds_entries_under() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RECORD_FILE);
        let trusted = VerifiedHashRecordUse::at(path.clone(), true);
        assert!(!trusted.may_restore_under("C:/Repo").await);
        let entry = Entry {
            identity: FileIdentity {
                volume: 1,
                file_id: 2,
                size: 3,
                modified: 4,
                changed: 5,
                usn: None,
            },
            signature: "s".into(),
            fingerprint: None,
            seen: 0,
        };
        update_blocking(
            &path,
            vec![(path_key("C:/Repo/@a/x.pbo"), Some(entry))],
            1_000_000_000,
        )
        .unwrap();
        assert!(trusted.may_restore_under("C:\\Repo").await);
        assert!(!trusted.may_restore_under("C:/Other").await);
        assert!(
            !VerifiedHashRecordUse::at(path.clone(), false)
                .may_restore_under("C:/Repo")
                .await
        );
        forget_under(&path, "C:/Repo").unwrap();
        assert!(!trusted.may_restore_under("C:/Repo").await);
    }

    #[test]
    fn an_unknown_version_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RECORD_FILE);
        std::fs::write(&path, br#"{"version":99,"entries":{"k":{}}}"#).unwrap();
        assert!(load(&path).entries.is_empty());
        std::fs::write(&path, b"not json").unwrap();
        assert!(load(&path).entries.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn identity_changes_when_the_file_is_written_and_not_when_it_is_read() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, vec![7u8; 100_000]).unwrap();
        let first = capture_identity(&path).unwrap();
        let mut read_back = Vec::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_end(&mut read_back)
            .unwrap();
        assert_eq!(capture_identity(&path).unwrap(), first);
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.write_all(&[8u8]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let written = capture_identity(&path).unwrap();
        assert_eq!(written.size, first.size);
        assert_ne!(written, first);
        assert!(capture_identity(&dir.path().join("missing.bin")).is_none());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_recorded_file_is_restored_only_while_nothing_about_it_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("addon.pbo");
        let bytes = vec![3u8; 50_000];
        std::fs::write(&file_path, &bytes).unwrap();
        let checksum = blake3::hash(&bytes).to_hex().to_uppercase();
        let job = FileHashJob {
            file_idx: 7,
            file_path: file_path.to_str().unwrap().to_owned(),
            file_length: bytes.len() as u64,
            file_remote_checksum: "WHOLE".into(),
            indexed_parts: JobParts::owned(vec![(3, {
                let mut part = part(0, 0, bytes.len() as u64, &checksum).1;
                part.local_checksum.clear();
                part
            })]),
            span_source: PartSpanSource::DetectLocalLayout,
            has_local_baseline: false,
            capture_identity: true,
        };
        let signature = parts_signature(&job.file_remote_checksum, job.indexed_parts.parts());
        let hashed = FileHashResult {
            file_idx: job.file_idx,
            updated_parts: Vec::new(),
            whole_file_checksum: None,
            content_hash: Some("FINGERPRINT".into()),
            file_path: job.file_path.clone(),
            elapsed: std::time::Duration::ZERO,
            parts_count: 1,
            missing_file: false,
            part_metrics: Default::default(),
            verified: Some(VerifiedFile {
                identity: capture_identity(&file_path).unwrap(),
                signature,
            }),
        };
        let record_file = dir.path().join(RECORD_FILE);
        let trusted = VerifiedHashRecordUse::at(record_file.clone(), true);
        update(&trusted, std::iter::once(&hashed)).await;

        let (restored, remaining) = restore(&trusted, vec![job.clone()]).await;
        assert!(remaining.is_empty());
        assert_eq!(restored[0].file_idx, 7);
        assert_eq!(restored[0].content_hash.as_deref(), Some("FINGERPRINT"));
        let (part_idx, part) = &restored[0].updated_parts[0];
        assert_eq!(*part_idx, 3);
        assert_eq!(part.local_checksum, checksum);
        assert_eq!((part.local_start, part.local_length), (0, 50_000));
        assert!(restored[0].verified.is_some());

        let untrusted = VerifiedHashRecordUse::at(record_file.clone(), false);
        assert!(restore(&untrusted, vec![job.clone()]).await.0.is_empty());
        let known = FileHashJob {
            has_local_baseline: true,
            ..job.clone()
        };
        assert!(restore(&trusted, vec![known]).await.0.is_empty());
        let mut changed_part = job.indexed_parts.iter().next().unwrap().1.clone();
        changed_part.remote_checksum = "OTHER".into();
        let new_version = FileHashJob {
            indexed_parts: JobParts::owned(vec![(3, changed_part)]),
            ..job.clone()
        };
        assert!(restore(&trusted, vec![new_version]).await.0.is_empty());
        let fresh_download = FileHashJob {
            span_source: PartSpanSource::RemoteLayout,
            ..job.clone()
        };
        assert!(restore(&trusted, vec![fresh_download]).await.0.is_empty());

        std::fs::write(&file_path, &bytes).unwrap();
        let (restored, remaining) = restore(&trusted, vec![job]).await;
        assert!(restored.is_empty());
        assert_eq!(remaining.len(), 1);
    }
}
