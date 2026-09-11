//! Filesystem compatibility rules for the paths Foxy reads and writes.
//!
//! Foxy stages multi-gigabyte addon files beside their targets (`.foxy.part`,
//! `.foxy.bak` sidecars), preallocates full-length part files, and keeps a
//! Turso database plus WAL under the game space. Each of those has a known
//! failure mode on some filesystem: FAT cannot hold a file of 4 GiB or more,
//! FAT/exFAT are not journaled, a database on a network share has no reliable
//! locking, and Windows applications that are not long-path aware cannot open
//! paths of 260 characters or more. The rules here turn a probed volume plus a
//! path role into a list of issues; callers decide how to surface them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::utils::format::sanitize_log_path;

/// Largest file FAT12/16/32 can store (4 GiB minus one byte).
pub const FAT_MAX_FILE_BYTES: u64 = 0xFFFF_FFFF;

/// `MAX_PATH` on Windows, including the terminating NUL: a usable path is at
/// most 259 characters for applications without the long-path manifest flag.
pub const WINDOWS_MAX_PATH_CHARS: usize = 260;

/// Longest sidecar suffix Foxy appends next to a target file while it is being
/// downloaded or replaced (`<file>.foxy.part.meta.tmp`). A target path that is
/// legal on its own can still fail once the sidecar name is added.
pub const SIDECAR_SUFFIX_RESERVE: usize = ".foxy.part.meta.tmp".len();

/// Longest single path component accepted by every filesystem Foxy targets
/// (NTFS/exFAT count UTF-16 units, ext4/XFS/Btrfs count bytes; both are 255).
pub const MAX_NAME_CHARS: usize = 255;

/// Filesystem family, classified from the name the operating system reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilesystemFamily {
    Ntfs,
    ReFs,
    /// FAT12, FAT16 and FAT32 (`FAT32`, `vfat`, `msdos`).
    Fat,
    ExFat,
    Apfs,
    HfsPlus,
    /// ext2, ext3 and ext4.
    Ext,
    Xfs,
    Btrfs,
    Zfs,
    F2fs,
    /// SMB/CIFS, NFS, AFP, WebDAV and hypervisor shared folders.
    Network,
    /// tmpfs, ramfs and RAM disks: contents do not survive a reboot.
    Volatile,
    /// An opaque FUSE mount (`fuseblk` is how Linux reports ntfs-3g and
    /// exfat-fuse volumes).
    Fuse,
    Unknown,
}

impl FilesystemFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ntfs => "ntfs",
            Self::ReFs => "refs",
            Self::Fat => "fat",
            Self::ExFat => "exfat",
            Self::Apfs => "apfs",
            Self::HfsPlus => "hfsplus",
            Self::Ext => "ext",
            Self::Xfs => "xfs",
            Self::Btrfs => "btrfs",
            Self::Zfs => "zfs",
            Self::F2fs => "f2fs",
            Self::Network => "network",
            Self::Volatile => "volatile",
            Self::Fuse => "fuse",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify a filesystem name as reported by `GetVolumeInformationW`,
/// `/proc/mounts`, or `statfs`.
pub fn classify_filesystem_name(name: &str) -> FilesystemFamily {
    let name = name.trim().to_ascii_lowercase();
    match name.as_str() {
        "ntfs" | "ntfs3" | "ntfs-3g" => FilesystemFamily::Ntfs,
        "refs" => FilesystemFamily::ReFs,
        "fat" | "fat12" | "fat16" | "fat32" | "vfat" | "msdos" => FilesystemFamily::Fat,
        "exfat" => FilesystemFamily::ExFat,
        "apfs" => FilesystemFamily::Apfs,
        "hfs" | "hfs+" | "hfsplus" => FilesystemFamily::HfsPlus,
        "ext2" | "ext3" | "ext4" => FilesystemFamily::Ext,
        "xfs" => FilesystemFamily::Xfs,
        "btrfs" => FilesystemFamily::Btrfs,
        "zfs" => FilesystemFamily::Zfs,
        "f2fs" => FilesystemFamily::F2fs,
        "cifs" | "smb" | "smb2" | "smb3" | "smbfs" | "nfs" | "nfs4" | "afp" | "afpfs" | "9p"
        | "vboxsf" | "virtiofs" | "sshfs" | "davfs" | "webdav" | "prl_fs" | "vmhgfs" => {
            FilesystemFamily::Network
        }
        "tmpfs" | "ramfs" | "devtmpfs" => FilesystemFamily::Volatile,
        "fuseblk" | "fuse" => FilesystemFamily::Fuse,
        other if other.starts_with("fuse.") => FilesystemFamily::Fuse,
        _ => FilesystemFamily::Unknown,
    }
}

/// What the operating system reports about the volume behind a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Mount point or drive root the path resolves to.
    pub root: PathBuf,
    /// Filesystem name exactly as reported.
    pub filesystem: String,
    pub family: FilesystemFamily,
    pub removable: bool,
    /// A network drive or UNC path. Windows reports the *remote* filesystem
    /// name for mapped shares (an SMB share on an NTFS server reads "NTFS"), so
    /// this flag is what identifies a share, not `family`.
    pub remote: bool,
    pub read_only: bool,
}

impl VolumeInfo {
    /// Largest single file the filesystem can hold, when it has a limit that a
    /// repository file could realistically hit.
    pub fn max_file_bytes(&self) -> Option<u64> {
        match self.family {
            FilesystemFamily::Fat => Some(FAT_MAX_FILE_BYTES),
            _ => None,
        }
    }

    pub fn journaled(&self) -> bool {
        !matches!(self.family, FilesystemFamily::Fat | FilesystemFamily::ExFat)
    }

    pub fn is_network(&self) -> bool {
        self.remote || self.family == FilesystemFamily::Network
    }

    /// Whether two names differing only by letter case resolve to the same
    /// file. Windows treats every volume that way (per-directory case
    /// sensitivity is opt-in and rare); elsewhere FAT/exFAT always do and
    /// Apple filesystems do by default.
    pub fn case_insensitive_lookup(&self) -> bool {
        if cfg!(windows) {
            return true;
        }
        match self.family {
            FilesystemFamily::Fat | FilesystemFamily::ExFat => true,
            FilesystemFamily::Apfs | FilesystemFamily::HfsPlus => cfg!(target_os = "macos"),
            _ => false,
        }
    }
}

/// How Foxy uses a storage path, derived from the startup storage role names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageRoleKind {
    /// Foxy's own state: database, settings, logs, temp staging, backups.
    State,
    /// Download destinations Foxy writes addon content into.
    Content,
    /// Game installs and folders Foxy only reads or lightly touches.
    GameInstall,
    Other,
}

pub fn storage_role_kind(role: &str) -> StorageRoleKind {
    match role {
        "database" | "app_data" | "game_space" | "logs" | "temp" | "backups" => {
            StorageRoleKind::State
        }
        "repository" | "repository_space" => StorageRoleKind::Content,
        "arma3" | "twwh3" | "reforger" | "generic" | "steam" | "arma3_profiles"
        | "additional_folder" | "cleanup_folder" => StorageRoleKind::GameInstall,
        _ => StorageRoleKind::Other,
    }
}

/// Roles whose files are Foxy's local state and cannot be recovered by a
/// redownload.
fn role_holds_database(role: &str) -> bool {
    matches!(role, "database" | "game_space" | "app_data")
}

/// Roles that receive multi-gigabyte files (addon content, patch staging,
/// addon backups) and therefore care about a per-file size ceiling.
fn role_writes_large_files(role: &str) -> bool {
    storage_role_kind(role) == StorageRoleKind::Content || matches!(role, "temp" | "backups")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StorageIssueSeverity {
    /// Logged only; useful for support, not worth interrupting the user.
    Advisory,
    /// Works, but with a real risk the user should know about.
    Warning,
    /// Foxy cannot do its job safely on this path.
    Blocking,
}

impl StorageIssueSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Warning => "warning",
            Self::Blocking => "blocking",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageIssueCode {
    ReadOnlyVolume,
    DatabaseOnNetworkShare,
    NetworkShare,
    VolatileStorage,
    FatFileSizeLimit,
    NotJournaled,
    RemovableStateDrive,
    CopyOnWriteDatabase,
    CaseSensitiveContent,
    /// A repository file is larger than the destination filesystem allows.
    FileExceedsFilesystemLimit,
    /// A repository path (plus sidecar) reaches the Windows `MAX_PATH` limit.
    PathTooLongForWindows,
    /// A repository file name cannot be created on Windows.
    InvalidWindowsName,
    /// Two repository files differ only by letter case on a case-insensitive
    /// volume, so one overwrites the other.
    CaseCollision,
}

impl StorageIssueCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnlyVolume => "read_only_volume",
            Self::DatabaseOnNetworkShare => "database_on_network_share",
            Self::NetworkShare => "network_share",
            Self::VolatileStorage => "volatile_storage",
            Self::FatFileSizeLimit => "fat_file_size_limit",
            Self::NotJournaled => "not_journaled",
            Self::RemovableStateDrive => "removable_state_drive",
            Self::CopyOnWriteDatabase => "copy_on_write_database",
            Self::CaseSensitiveContent => "case_sensitive_content",
            Self::FileExceedsFilesystemLimit => "file_exceeds_filesystem_limit",
            Self::PathTooLongForWindows => "path_too_long_for_windows",
            Self::InvalidWindowsName => "invalid_windows_name",
            Self::CaseCollision => "case_collision",
        }
    }
}

/// One compatibility finding for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageIssue {
    pub code: StorageIssueCode,
    pub severity: StorageIssueSeverity,
    pub role: String,
    pub path: PathBuf,
    /// Mount point or drive root of the volume behind `path`, so several
    /// paths on one drive can be collapsed into one finding.
    pub volume_root: PathBuf,
    pub filesystem: String,
    /// Files affected by a limit-type issue (over the size ceiling, too long,
    /// invalid name); zero for volume-level issues.
    pub affected_files: usize,
    /// Largest offending file in bytes for [`StorageIssueCode::FileExceedsFilesystemLimit`].
    pub largest_file_bytes: u64,
    /// Longest offending path in characters for [`StorageIssueCode::PathTooLongForWindows`].
    pub longest_path_chars: usize,
    /// One offending name or path pair, for the user to recognize the file.
    pub example: String,
}

impl StorageIssue {
    fn volume(
        code: StorageIssueCode,
        severity: StorageIssueSeverity,
        role: &str,
        path: &Path,
        volume: &VolumeInfo,
    ) -> Self {
        Self {
            code,
            severity,
            role: role.to_string(),
            path: path.to_path_buf(),
            volume_root: volume.root.clone(),
            filesystem: volume.filesystem.clone(),
            affected_files: 0,
            largest_file_bytes: 0,
            longest_path_chars: 0,
            example: String::new(),
        }
    }

    /// Whether the finding describes the volume as a whole (one finding per
    /// drive is enough) rather than the files of one folder.
    pub fn is_volume_level(&self) -> bool {
        !matches!(
            self.code,
            StorageIssueCode::FileExceedsFilesystemLimit
                | StorageIssueCode::PathTooLongForWindows
                | StorageIssueCode::InvalidWindowsName
                | StorageIssueCode::CaseCollision
        )
    }

    /// Stable identity of the finding for collapsing duplicates and for
    /// "do not show again" bookkeeping: the code, the drive (or the folder for
    /// per-file findings) and the filesystem. Adding a second repository on an
    /// already-acknowledged FAT32 drive raises nothing new; moving the drive
    /// to another filesystem or adding a new drive does.
    pub fn fingerprint_component(&self) -> String {
        let anchor = if self.is_volume_level() {
            &self.volume_root
        } else {
            &self.path
        };
        format!(
            "{}|{}|{}",
            self.code.as_str(),
            fold_path(&anchor.to_string_lossy()),
            self.filesystem.to_ascii_lowercase()
        )
    }

    pub fn log_line(&self) -> String {
        let mut line = format!(
            "storage_compat: severity={} code={} role={} path=\"{}\" fs=\"{}\"",
            self.severity.as_str(),
            self.code.as_str(),
            self.role,
            sanitize_log_path(&self.path),
            self.filesystem
        );
        if self.affected_files > 0 {
            line.push_str(&format!(" affected_files={}", self.affected_files));
        }
        if self.largest_file_bytes > 0 {
            line.push_str(&format!(" largest_file_bytes={}", self.largest_file_bytes));
        }
        if self.longest_path_chars > 0 {
            line.push_str(&format!(" longest_path_chars={}", self.longest_path_chars));
        }
        if !self.example.is_empty() {
            line.push_str(&format!(
                " example=\"{}\"",
                crate::core::utils::format::sanitize_log_path_str(&self.example)
            ));
        }
        line
    }
}

/// Separator- and case-insensitive form of a path, for comparing two spellings
/// of the same file.
fn fold_path(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// Volume-level findings for a path used in `role`.
pub fn evaluate_volume(role: &str, path: &Path, volume: &VolumeInfo) -> Vec<StorageIssue> {
    let kind = storage_role_kind(role);
    if matches!(kind, StorageRoleKind::Other | StorageRoleKind::GameInstall) {
        return Vec::new();
    }
    let issue = |code, severity| StorageIssue::volume(code, severity, role, path, volume);
    let mut issues = Vec::new();

    if volume.read_only {
        issues.push(issue(
            StorageIssueCode::ReadOnlyVolume,
            StorageIssueSeverity::Blocking,
        ));
    }

    if volume.is_network() {
        if role_holds_database(role) {
            issues.push(issue(
                StorageIssueCode::DatabaseOnNetworkShare,
                StorageIssueSeverity::Blocking,
            ));
        } else {
            issues.push(issue(
                StorageIssueCode::NetworkShare,
                StorageIssueSeverity::Warning,
            ));
        }
    }

    match volume.family {
        FilesystemFamily::Volatile => {
            let severity = if role_holds_database(role) {
                StorageIssueSeverity::Blocking
            } else {
                StorageIssueSeverity::Warning
            };
            issues.push(issue(StorageIssueCode::VolatileStorage, severity));
        }
        FilesystemFamily::Fat => {
            if role_writes_large_files(role) {
                issues.push(issue(
                    StorageIssueCode::FatFileSizeLimit,
                    StorageIssueSeverity::Warning,
                ));
            } else {
                issues.push(issue(
                    StorageIssueCode::NotJournaled,
                    StorageIssueSeverity::Warning,
                ));
            }
        }
        FilesystemFamily::ExFat => {
            issues.push(issue(
                StorageIssueCode::NotJournaled,
                StorageIssueSeverity::Warning,
            ));
        }
        FilesystemFamily::Btrfs if role == "database" => {
            issues.push(issue(
                StorageIssueCode::CopyOnWriteDatabase,
                StorageIssueSeverity::Advisory,
            ));
        }
        _ => {}
    }

    if volume.removable && role_holds_database(role) {
        issues.push(issue(
            StorageIssueCode::RemovableStateDrive,
            StorageIssueSeverity::Warning,
        ));
    }

    if kind == StorageRoleKind::Content && !volume.case_insensitive_lookup() {
        issues.push(issue(
            StorageIssueCode::CaseSensitiveContent,
            StorageIssueSeverity::Advisory,
        ));
    }

    issues
}

/// Why a path component cannot be created on a Windows volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameProblem {
    /// `CON`, `PRN`, `AUX`, `NUL`, `COM1`..`COM9`, `LPT1`..`LPT9` (any extension).
    ReservedName,
    InvalidCharacter(char),
    /// Windows strips trailing dots and spaces, so the file lands under a
    /// different name than the manifest expects.
    TrailingDotOrSpace,
    TooLong,
}

/// Validate one path component against the rules every Windows filesystem
/// enforces. Length is checked everywhere since the 255 ceiling is universal.
pub fn name_problem(component: &str) -> Option<NameProblem> {
    if component.is_empty() {
        return None;
    }
    if component.chars().count() > MAX_NAME_CHARS {
        return Some(NameProblem::TooLong);
    }
    if !cfg!(windows) {
        return None;
    }
    if let Some(invalid) = component
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (*c as u32) < 0x20)
    {
        return Some(NameProblem::InvalidCharacter(invalid));
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Some(NameProblem::TrailingDotOrSpace);
    }
    let stem = component.split('.').next().unwrap_or(component);
    if is_windows_reserved_stem(stem) {
        return Some(NameProblem::ReservedName);
    }
    None
}

fn is_windows_reserved_stem(stem: &str) -> bool {
    let upper = stem.trim_end().to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            upper.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && upper.as_bytes()[3].is_ascii_digit()
                && upper.as_bytes()[3] != b'0'
        }
    }
}

/// Number of characters the full path would occupy once Foxy appends its
/// longest sidecar suffix.
pub fn path_chars_with_sidecar(path_chars: usize) -> usize {
    path_chars + SIDECAR_SUFFIX_RESERVE
}

/// Whether a path of `path_chars` characters (before any sidecar suffix) can
/// break a Windows application that is not long-path aware.
pub fn windows_path_too_long(path_chars: usize) -> bool {
    path_chars_with_sidecar(path_chars) >= WINDOWS_MAX_PATH_CHARS
}

/// Aggregate limits over a set of files bound for one volume.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathLimitReport {
    pub file_count: usize,
    pub largest_file_bytes: u64,
    pub files_over_size_limit: usize,
    pub longest_path_chars: usize,
    pub paths_too_long: usize,
    pub invalid_names: usize,
    pub invalid_name_example: String,
    /// Two paths that collide once letter case is ignored.
    pub case_collision: Option<(String, String)>,
}

impl PathLimitReport {
    /// Scan `(path, size)` pairs. Pure string work: no filesystem access.
    pub fn from_files<'a>(
        files: impl IntoIterator<Item = (&'a str, u64)>,
        volume: &VolumeInfo,
    ) -> Self {
        let size_limit = volume.max_file_bytes();
        let check_case = volume.case_insensitive_lookup();
        let mut seen_folded: HashMap<String, &'a str> = HashMap::new();
        let mut report = Self::default();

        for (path, size) in files {
            report.file_count += 1;
            report.largest_file_bytes = report.largest_file_bytes.max(size);
            if size_limit.is_some_and(|limit| size > limit) {
                report.files_over_size_limit += 1;
            }

            let chars = path.chars().count();
            report.longest_path_chars = report.longest_path_chars.max(chars);
            if cfg!(windows) && windows_path_too_long(chars) {
                report.paths_too_long += 1;
            }

            if let Some(component) = Path::new(path)
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(name) => name.to_str(),
                    _ => None,
                })
                .find(|component| name_problem(component).is_some())
            {
                report.invalid_names += 1;
                if report.invalid_name_example.is_empty() {
                    report.invalid_name_example = component.to_string();
                }
            }

            if check_case && report.case_collision.is_none() {
                let folded = fold_path(path);
                match seen_folded.get(folded.as_str()) {
                    Some(previous) if *previous != path => {
                        report.case_collision = Some((previous.to_string(), path.to_string()));
                    }
                    Some(_) => {}
                    None => {
                        seen_folded.insert(folded, path);
                    }
                }
            }
        }
        report
    }

    /// Build the per-file findings for a destination in `role`.
    pub fn issues(&self, role: &str, path: &Path, volume: &VolumeInfo) -> Vec<StorageIssue> {
        let mut issues = Vec::new();
        let base = |code, severity| StorageIssue::volume(code, severity, role, path, volume);

        if self.files_over_size_limit > 0 {
            let mut issue = base(
                StorageIssueCode::FileExceedsFilesystemLimit,
                StorageIssueSeverity::Blocking,
            );
            issue.affected_files = self.files_over_size_limit;
            issue.largest_file_bytes = self.largest_file_bytes;
            issues.push(issue);
        }
        if self.invalid_names > 0 {
            let mut issue = base(
                StorageIssueCode::InvalidWindowsName,
                StorageIssueSeverity::Blocking,
            );
            issue.affected_files = self.invalid_names;
            issue.example = self.invalid_name_example.clone();
            issues.push(issue);
        }
        if let Some((first, second)) = &self.case_collision {
            let mut issue = base(
                StorageIssueCode::CaseCollision,
                StorageIssueSeverity::Blocking,
            );
            issue.affected_files = 2;
            issue.example = format!("{first} / {second}");
            issues.push(issue);
        }
        if self.paths_too_long > 0 {
            let mut issue = base(
                StorageIssueCode::PathTooLongForWindows,
                StorageIssueSeverity::Warning,
            );
            issue.affected_files = self.paths_too_long;
            issue.longest_path_chars = self.longest_path_chars;
            issues.push(issue);
        }
        issues
    }
}

/// Resolves the volume behind a path. On Windows every probe is a pair of
/// Win32 calls; elsewhere the mount table is read once per prober.
pub struct VolumeProber {
    #[cfg(not(windows))]
    disks: sysinfo::Disks,
}

impl Default for VolumeProber {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumeProber {
    pub fn new() -> Self {
        Self {
            #[cfg(not(windows))]
            disks: sysinfo::Disks::new_with_refreshed_list(),
        }
    }

    /// Volume of `path`, probed on the nearest ancestor that exists so a
    /// destination folder that has not been created yet still resolves.
    pub fn probe(&self, path: &Path) -> Option<VolumeInfo> {
        let absolute = absolute_path(path);
        let anchor = absolute
            .ancestors()
            .find(|candidate| candidate.exists())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| absolute.clone());
        self.probe_existing(&anchor, &absolute)
    }

    #[cfg(windows)]
    fn probe_existing(&self, anchor: &Path, original: &Path) -> Option<VolumeInfo> {
        windows::probe(anchor, original)
    }

    #[cfg(not(windows))]
    fn probe_existing(&self, anchor: &Path, _original: &Path) -> Option<VolumeInfo> {
        let disk = self
            .disks
            .iter()
            .filter(|disk| anchor.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().as_os_str().len())?;
        let filesystem = disk.file_system().to_string_lossy().to_string();
        let family = classify_filesystem_name(&filesystem);
        Some(VolumeInfo {
            root: disk.mount_point().to_path_buf(),
            filesystem,
            family,
            removable: disk.is_removable(),
            remote: family == FilesystemFamily::Network,
            read_only: disk.is_read_only(),
        })
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path, PathBuf, Prefix};

    use winapi::um::fileapi::{GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW};
    use winapi::um::winbase::{DRIVE_CDROM, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE};
    use winapi::um::winnt::FILE_READ_ONLY_VOLUME;

    use super::{FilesystemFamily, VolumeInfo, classify_filesystem_name};

    fn is_unc(path: &Path) -> bool {
        matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))
        )
    }

    pub(super) fn probe(anchor: &Path, original: &Path) -> Option<VolumeInfo> {
        let wide: Vec<u16> = anchor
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Mount points can sit deep inside a folder tree, so give the root
        // buffer the full extended-length budget rather than MAX_PATH.
        let mut root = vec![0u16; 32_768];
        let resolved =
            unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
        if resolved == 0 {
            return None;
        }
        let root_len = root.iter().position(|&c| c == 0)?;
        root.truncate(root_len + 1);
        let root_path = PathBuf::from(OsString::from_wide(&root[..root_len]));

        let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
        let mut serial = 0u32;
        let mut max_component = 0u32;
        let mut flags = 0u32;
        let mut fs_name = [0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                &mut max_component,
                &mut flags,
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let fs_len = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        let filesystem = String::from_utf16_lossy(&fs_name[..fs_len]);
        let mut family = classify_filesystem_name(&filesystem);
        if drive_type == DRIVE_RAMDISK {
            family = FilesystemFamily::Volatile;
        }
        Some(VolumeInfo {
            root: root_path,
            filesystem,
            family,
            removable: matches!(drive_type, DRIVE_REMOVABLE | DRIVE_CDROM),
            remote: drive_type == DRIVE_REMOTE || is_unc(original) || is_unc(anchor),
            read_only: flags & FILE_READ_ONLY_VOLUME != 0 || drive_type == DRIVE_CDROM,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(family: FilesystemFamily) -> VolumeInfo {
        VolumeInfo {
            root: PathBuf::from("X:\\"),
            filesystem: family.as_str().to_uppercase(),
            family,
            removable: false,
            remote: false,
            read_only: false,
        }
    }

    fn codes(issues: &[StorageIssue]) -> Vec<StorageIssueCode> {
        issues.iter().map(|issue| issue.code).collect()
    }

    #[test]
    fn classifies_common_filesystem_names() {
        assert_eq!(classify_filesystem_name("NTFS"), FilesystemFamily::Ntfs);
        assert_eq!(classify_filesystem_name("ReFS"), FilesystemFamily::ReFs);
        assert_eq!(classify_filesystem_name("FAT32"), FilesystemFamily::Fat);
        assert_eq!(classify_filesystem_name("vfat"), FilesystemFamily::Fat);
        assert_eq!(classify_filesystem_name("exFAT"), FilesystemFamily::ExFat);
        assert_eq!(classify_filesystem_name("apfs"), FilesystemFamily::Apfs);
        assert_eq!(classify_filesystem_name("ext4"), FilesystemFamily::Ext);
        assert_eq!(classify_filesystem_name("xfs"), FilesystemFamily::Xfs);
        assert_eq!(classify_filesystem_name("btrfs"), FilesystemFamily::Btrfs);
        assert_eq!(classify_filesystem_name("cifs"), FilesystemFamily::Network);
        assert_eq!(classify_filesystem_name("nfs4"), FilesystemFamily::Network);
        assert_eq!(
            classify_filesystem_name("tmpfs"),
            FilesystemFamily::Volatile
        );
        assert_eq!(classify_filesystem_name("fuseblk"), FilesystemFamily::Fuse);
        assert_eq!(
            classify_filesystem_name("fuse.sshfs"),
            FilesystemFamily::Fuse
        );
        assert_eq!(classify_filesystem_name("wtf"), FilesystemFamily::Unknown);
        assert_eq!(classify_filesystem_name(""), FilesystemFamily::Unknown);
    }

    #[test]
    fn fat_limits_file_size_and_nothing_else_does() {
        assert_eq!(
            volume(FilesystemFamily::Fat).max_file_bytes(),
            Some(FAT_MAX_FILE_BYTES)
        );
        for family in [
            FilesystemFamily::Ntfs,
            FilesystemFamily::ExFat,
            FilesystemFamily::Ext,
            FilesystemFamily::Apfs,
            FilesystemFamily::Unknown,
        ] {
            assert_eq!(volume(family).max_file_bytes(), None, "{family:?}");
        }
    }

    #[test]
    fn journaling_is_absent_only_on_fat_families() {
        assert!(!volume(FilesystemFamily::Fat).journaled());
        assert!(!volume(FilesystemFamily::ExFat).journaled());
        assert!(volume(FilesystemFamily::Ntfs).journaled());
        assert!(volume(FilesystemFamily::Btrfs).journaled());
        assert!(volume(FilesystemFamily::Unknown).journaled());
    }

    #[test]
    fn roles_map_to_kinds() {
        assert_eq!(storage_role_kind("database"), StorageRoleKind::State);
        assert_eq!(storage_role_kind("temp"), StorageRoleKind::State);
        assert_eq!(storage_role_kind("repository"), StorageRoleKind::Content);
        assert_eq!(
            storage_role_kind("repository_space"),
            StorageRoleKind::Content
        );
        assert_eq!(storage_role_kind("arma3"), StorageRoleKind::GameInstall);
        assert_eq!(
            storage_role_kind("additional_folder"),
            StorageRoleKind::GameInstall
        );
        assert_eq!(storage_role_kind("whatever"), StorageRoleKind::Other);
    }

    #[test]
    fn ntfs_content_and_state_are_clean() {
        let vol = volume(FilesystemFamily::Ntfs);
        assert!(evaluate_volume("repository", Path::new("X:\\mods"), &vol).is_empty());
        assert!(evaluate_volume("database", Path::new("X:\\db"), &vol).is_empty());
        assert!(evaluate_volume("temp", Path::new("X:\\tmp"), &vol).is_empty());
    }

    #[test]
    fn game_install_and_unknown_roles_are_never_flagged() {
        let mut vol = volume(FilesystemFamily::Fat);
        vol.read_only = true;
        vol.remote = true;
        assert!(evaluate_volume("arma3", Path::new("X:\\Arma 3"), &vol).is_empty());
        assert!(evaluate_volume("cleanup_folder", Path::new("X:\\x"), &vol).is_empty());
        assert!(evaluate_volume("mystery", Path::new("X:\\x"), &vol).is_empty());
    }

    #[test]
    fn fat_content_warns_about_file_size_and_state_about_journaling() {
        let vol = volume(FilesystemFamily::Fat);
        let content = evaluate_volume("repository", Path::new("X:\\mods"), &vol);
        assert_eq!(codes(&content), vec![StorageIssueCode::FatFileSizeLimit]);
        assert_eq!(content[0].severity, StorageIssueSeverity::Warning);
        // Temp holds patch staging of full addon size, so it shares the ceiling.
        let temp = evaluate_volume("temp", Path::new("X:\\tmp"), &vol);
        assert_eq!(codes(&temp), vec![StorageIssueCode::FatFileSizeLimit]);
        let logs = evaluate_volume("logs", Path::new("X:\\logs"), &vol);
        assert_eq!(codes(&logs), vec![StorageIssueCode::NotJournaled]);
    }

    #[test]
    fn exfat_warns_about_journaling_everywhere_foxy_writes() {
        let vol = volume(FilesystemFamily::ExFat);
        for role in ["repository", "database", "temp", "backups"] {
            let issues = evaluate_volume(role, Path::new("X:\\p"), &vol);
            assert_eq!(
                codes(&issues),
                vec![StorageIssueCode::NotJournaled],
                "{role}"
            );
            assert_eq!(issues[0].severity, StorageIssueSeverity::Warning);
        }
    }

    #[test]
    fn network_share_blocks_database_roles_and_warns_elsewhere() {
        let mut vol = volume(FilesystemFamily::Ntfs);
        vol.remote = true;
        for role in ["database", "game_space", "app_data"] {
            let issues = evaluate_volume(role, Path::new("\\\\nas\\foxy"), &vol);
            assert_eq!(
                codes(&issues),
                vec![StorageIssueCode::DatabaseOnNetworkShare],
                "{role}"
            );
            assert_eq!(issues[0].severity, StorageIssueSeverity::Blocking);
        }
        for role in ["repository", "temp", "logs", "backups"] {
            let issues = evaluate_volume(role, Path::new("\\\\nas\\mods"), &vol);
            assert_eq!(
                codes(&issues),
                vec![StorageIssueCode::NetworkShare],
                "{role}"
            );
            assert_eq!(issues[0].severity, StorageIssueSeverity::Warning);
        }
        // A cifs mount on Linux is a share even without the drive-type flag.
        let cifs = volume(FilesystemFamily::Network);
        let issues = evaluate_volume("database", Path::new("/mnt/nas"), &cifs);
        assert_eq!(
            codes(&issues),
            vec![StorageIssueCode::DatabaseOnNetworkShare]
        );
    }

    #[test]
    fn read_only_volume_blocks_every_written_role() {
        let mut vol = volume(FilesystemFamily::Ntfs);
        vol.read_only = true;
        for role in ["database", "repository", "temp"] {
            let issues = evaluate_volume(role, Path::new("X:\\p"), &vol);
            assert_eq!(codes(&issues), vec![StorageIssueCode::ReadOnlyVolume]);
            assert_eq!(issues[0].severity, StorageIssueSeverity::Blocking);
        }
    }

    #[test]
    fn volatile_storage_blocks_database_and_warns_content() {
        let vol = volume(FilesystemFamily::Volatile);
        let db = evaluate_volume("database", Path::new("/tmp/foxy"), &vol);
        assert_eq!(codes(&db), vec![StorageIssueCode::VolatileStorage]);
        assert_eq!(db[0].severity, StorageIssueSeverity::Blocking);
        let content = evaluate_volume("repository", Path::new("/tmp/mods"), &vol);
        assert_eq!(codes(&content), vec![StorageIssueCode::VolatileStorage]);
        assert_eq!(content[0].severity, StorageIssueSeverity::Warning);
    }

    #[test]
    fn removable_drive_warns_only_for_state_roles() {
        let mut vol = volume(FilesystemFamily::Ntfs);
        vol.removable = true;
        let db = evaluate_volume("database", Path::new("E:\\Foxy"), &vol);
        assert_eq!(codes(&db), vec![StorageIssueCode::RemovableStateDrive]);
        // External drives full of mods are a normal setup.
        assert!(evaluate_volume("repository", Path::new("E:\\mods"), &vol).is_empty());
    }

    #[test]
    fn btrfs_database_is_only_an_advisory() {
        let vol = volume(FilesystemFamily::Btrfs);
        let db = evaluate_volume("database", Path::new("/home/x/foxy"), &vol);
        assert_eq!(codes(&db), vec![StorageIssueCode::CopyOnWriteDatabase]);
        assert_eq!(db[0].severity, StorageIssueSeverity::Advisory);
        assert!(evaluate_volume("game_space", Path::new("/home/x/foxy"), &vol).is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn case_sensitive_content_is_an_advisory_off_windows() {
        let vol = volume(FilesystemFamily::Ext);
        let issues = evaluate_volume("repository", Path::new("/srv/mods"), &vol);
        assert_eq!(codes(&issues), vec![StorageIssueCode::CaseSensitiveContent]);
        assert_eq!(issues[0].severity, StorageIssueSeverity::Advisory);
        assert!(volume(FilesystemFamily::Fat).case_insensitive_lookup());
    }

    #[cfg(windows)]
    #[test]
    fn every_volume_is_case_insensitive_on_windows() {
        assert!(volume(FilesystemFamily::Ext).case_insensitive_lookup());
        assert!(
            evaluate_volume(
                "repository",
                Path::new("X:\\mods"),
                &volume(FilesystemFamily::Ext)
            )
            .is_empty()
        );
    }

    #[test]
    fn combined_findings_stack_in_a_stable_order() {
        let mut vol = volume(FilesystemFamily::Fat);
        vol.removable = true;
        vol.read_only = true;
        let issues = evaluate_volume("database", Path::new("E:\\Foxy"), &vol);
        assert_eq!(
            codes(&issues),
            vec![
                StorageIssueCode::ReadOnlyVolume,
                StorageIssueCode::NotJournaled,
                StorageIssueCode::RemovableStateDrive,
            ]
        );
    }

    #[test]
    fn sidecar_reserve_matches_the_longest_suffix_foxy_writes() {
        assert_eq!(SIDECAR_SUFFIX_RESERVE, 19);
        assert!(!windows_path_too_long(240));
        assert!(windows_path_too_long(241));
        assert_eq!(path_chars_with_sidecar(100), 119);
    }

    #[test]
    fn name_length_is_checked_on_every_platform() {
        assert_eq!(name_problem(&"a".repeat(255)), None);
        assert_eq!(name_problem(&"a".repeat(256)), Some(NameProblem::TooLong));
        assert_eq!(name_problem(""), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_reserved_names_and_characters_are_rejected() {
        assert_eq!(name_problem("aux.pbo"), Some(NameProblem::ReservedName));
        assert_eq!(name_problem("CON"), Some(NameProblem::ReservedName));
        assert_eq!(name_problem("com1.txt"), Some(NameProblem::ReservedName));
        assert_eq!(name_problem("lpt9"), Some(NameProblem::ReservedName));
        assert_eq!(name_problem("com0"), None);
        assert_eq!(name_problem("com10"), None);
        assert_eq!(name_problem("console.pbo"), None);
        assert_eq!(name_problem("auxiliary.pbo"), None);
        assert_eq!(
            name_problem("what?.pbo"),
            Some(NameProblem::InvalidCharacter('?'))
        );
        assert_eq!(
            name_problem("a:b"),
            Some(NameProblem::InvalidCharacter(':'))
        );
        assert_eq!(name_problem("bad."), Some(NameProblem::TrailingDotOrSpace));
        assert_eq!(name_problem("bad "), Some(NameProblem::TrailingDotOrSpace));
        assert_eq!(name_problem("@CBA_A3"), None);
        assert_eq!(name_problem("file (1).pbo"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_only_name_rules_do_not_apply_elsewhere() {
        assert_eq!(name_problem("aux.pbo"), None);
        assert_eq!(name_problem("what?.pbo"), None);
        assert_eq!(name_problem("bad."), None);
    }

    #[test]
    fn report_counts_files_over_the_fat_limit() {
        let vol = volume(FilesystemFamily::Fat);
        let files = [
            ("X:\\mods\\@a\\addons\\small.pbo", 10u64),
            ("X:\\mods\\@a\\addons\\exact.pbo", FAT_MAX_FILE_BYTES),
            ("X:\\mods\\@a\\addons\\big.pbo", FAT_MAX_FILE_BYTES + 1),
            ("X:\\mods\\@b\\addons\\huge.pbo", 6 * 1024 * 1024 * 1024),
        ];
        let report = PathLimitReport::from_files(files.iter().copied(), &vol);
        assert_eq!(report.file_count, 4);
        assert_eq!(report.files_over_size_limit, 2);
        assert_eq!(report.largest_file_bytes, 6 * 1024 * 1024 * 1024);

        let issues = report.issues("repository", Path::new("X:\\mods"), &vol);
        let size = issues
            .iter()
            .find(|issue| issue.code == StorageIssueCode::FileExceedsFilesystemLimit)
            .expect("size issue");
        assert_eq!(size.severity, StorageIssueSeverity::Blocking);
        assert_eq!(size.affected_files, 2);
        assert_eq!(size.largest_file_bytes, 6 * 1024 * 1024 * 1024);

        // The same files on NTFS raise nothing.
        let ntfs = volume(FilesystemFamily::Ntfs);
        let report = PathLimitReport::from_files(files.iter().copied(), &ntfs);
        assert_eq!(report.files_over_size_limit, 0);
        assert!(
            report
                .issues("repository", Path::new("X:\\mods"), &ntfs)
                .iter()
                .all(|issue| issue.code != StorageIssueCode::FileExceedsFilesystemLimit)
        );
    }

    #[test]
    fn report_detects_case_collisions_only_where_lookup_folds_case() {
        let files = [
            ("X:\\mods\\@a\\addons\\Weapons.pbo", 1u64),
            ("X:\\mods\\@a\\addons\\weapons.pbo", 1),
            ("X:\\mods\\@a\\addons\\weapons.pbo", 1),
        ];
        let mut vol = volume(FilesystemFamily::Fat);
        let report = PathLimitReport::from_files(files.iter().copied(), &vol);
        let collision = report.case_collision.clone().expect("collision");
        assert_eq!(collision.0, "X:\\mods\\@a\\addons\\Weapons.pbo");
        assert_eq!(collision.1, "X:\\mods\\@a\\addons\\weapons.pbo");
        let issues = report.issues("repository", Path::new("X:\\mods"), &vol);
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == StorageIssueCode::CaseCollision
                    && issue.severity == StorageIssueSeverity::Blocking)
        );

        if !cfg!(windows) {
            vol.family = FilesystemFamily::Ext;
            let report = PathLimitReport::from_files(files.iter().copied(), &vol);
            assert!(report.case_collision.is_none());
        }
    }

    #[test]
    fn report_tracks_the_longest_path() {
        let vol = volume(FilesystemFamily::Ntfs);
        let long = format!("X:\\{}\\file.pbo", "d".repeat(300));
        let files = [("X:\\short.pbo", 1u64), (long.as_str(), 1)];
        let report = PathLimitReport::from_files(files.iter().copied(), &vol);
        assert_eq!(report.longest_path_chars, long.chars().count());
        if cfg!(windows) {
            assert_eq!(report.paths_too_long, 1);
            let issues = report.issues("repository", Path::new("X:\\"), &vol);
            let too_long = issues
                .iter()
                .find(|issue| issue.code == StorageIssueCode::PathTooLongForWindows)
                .expect("long path issue");
            assert_eq!(too_long.severity, StorageIssueSeverity::Warning);
            assert_eq!(too_long.longest_path_chars, long.chars().count());
        } else {
            assert_eq!(report.paths_too_long, 0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn report_flags_invalid_windows_names_with_an_example() {
        let vol = volume(FilesystemFamily::Ntfs);
        let files = [
            ("X:\\mods\\@a\\addons\\ok.pbo", 1u64),
            ("X:\\mods\\@a\\addons\\nul.pbo", 1),
            ("X:\\mods\\@a\\keys\\bad?.bikey", 1),
        ];
        let report = PathLimitReport::from_files(files.iter().copied(), &vol);
        assert_eq!(report.invalid_names, 2);
        assert_eq!(report.invalid_name_example, "nul.pbo");
        let issues = report.issues("repository", Path::new("X:\\mods"), &vol);
        let invalid = issues
            .iter()
            .find(|issue| issue.code == StorageIssueCode::InvalidWindowsName)
            .expect("invalid name issue");
        assert_eq!(invalid.affected_files, 2);
        assert_eq!(invalid.example, "nul.pbo");
    }

    #[test]
    fn empty_report_raises_nothing() {
        let vol = volume(FilesystemFamily::Fat);
        let report = PathLimitReport::from_files(std::iter::empty(), &vol);
        assert_eq!(report, PathLimitReport::default());
        assert!(
            report
                .issues("repository", Path::new("X:\\"), &vol)
                .is_empty()
        );
    }

    #[test]
    fn fingerprint_ignores_path_separator_and_case_noise() {
        let vol = volume(FilesystemFamily::Fat);
        let a = evaluate_volume("repository", Path::new("X:\\Mods\\Repo"), &vol);
        let b = evaluate_volume("repository", Path::new("x:/mods/repo/"), &vol);
        assert_eq!(a[0].fingerprint_component(), b[0].fingerprint_component());
        let mut other = vol.clone();
        other.filesystem = "exFAT".to_string();
        other.family = FilesystemFamily::ExFat;
        let c = evaluate_volume("repository", Path::new("X:\\Mods\\Repo"), &other);
        assert_ne!(a[0].fingerprint_component(), c[0].fingerprint_component());
    }

    #[test]
    fn log_line_carries_the_limit_details() {
        let vol = volume(FilesystemFamily::Fat);
        let files = [("X:\\mods\\big.pbo", FAT_MAX_FILE_BYTES + 5)];
        let report = PathLimitReport::from_files(files.iter().copied(), &vol);
        let issues = report.issues("repository", Path::new("X:\\mods"), &vol);
        let line = issues[0].log_line();
        assert!(line.starts_with(
            "storage_compat: severity=blocking code=file_exceeds_filesystem_limit role=repository"
        ));
        assert!(line.contains("affected_files=1"));
        assert!(line.contains(&format!("largest_file_bytes={}", FAT_MAX_FILE_BYTES + 5)));
    }

    #[test]
    fn prober_resolves_the_temp_directory() {
        let prober = VolumeProber::new();
        let info = prober.probe(&std::env::temp_dir());
        if let Some(info) = info {
            assert!(!info.filesystem.is_empty());
            assert!(!info.root.as_os_str().is_empty());
            // A folder that does not exist yet resolves through its parent.
            let unborn = std::env::temp_dir().join("foxy-storage-compat-nonexistent");
            let nested = prober.probe(&unborn.join("deeper")).expect("nested probe");
            assert_eq!(nested.root, info.root);
            assert_eq!(nested.family, info.family);
        }
    }
}
