use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub(super) const MANIFEST: &str = "manifest.json";
pub(super) const RETAIN: usize = 3;
pub(super) const MAX_FILES: usize = 32;
pub(super) const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const EXTENSIONS: &[&str] = &["Arma3Profile", "vars.Arma3Profile", "3den.Arma3Profile"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BackupPolicy {
    pub enabled: bool,
    pub interval_days: u16,
}
impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_days: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub id: String,
    pub profile_name: String,
    pub original_path: PathBuf,
    pub created_at: i64,
    pub file_count: usize,
    pub bytes: u64,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct FileRecord {
    pub name: String,
    pub bytes: u64,
    pub blake3: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub id: String,
    pub profile_name: String,
    pub original_path: PathBuf,
    pub created_at: i64,
    pub files: Vec<FileRecord>,
}

pub fn list_backups(root: &Path) -> Result<Vec<BackupEntry>> {
    let mut result = Vec::new();
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    super::paths::check_path(root, true)?;
    for item in fs::read_dir(root)? {
        let item = item?;
        if !item.file_type()?.is_dir() {
            continue;
        }
        let dir = item.path();
        let manifest_path = dir.join(MANIFEST);
        if super::paths::check_path(&manifest_path, false).is_err() {
            continue;
        }
        let Ok(bytes) = read_bounded(&manifest_path, 1024 * 1024) else {
            continue;
        };
        if bytes.len() > 1024 * 1024 {
            continue;
        }
        let Ok(m) = serde_json::from_slice::<Manifest>(&bytes) else {
            continue;
        };
        if validate_manifest(&m).is_err() || m.id != item.file_name().to_string_lossy() {
            continue;
        }
        result.push(to_entry(dir, m));
    }
    result.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(result)
}

pub(super) fn validate_manifest(m: &Manifest) -> Result<()> {
    if !safe_id(&m.id)
        || m.profile_name.trim().is_empty()
        || !m.original_path.is_absolute()
        || m.files.is_empty()
        || m.files.len() > MAX_FILES
    {
        bail!("invalid profile backup manifest");
    }
    let mut names = std::collections::HashSet::new();
    let mut total = 0u64;
    let mut primary_stem: Option<String> = None;
    let mut primary_count = 0usize;
    for f in &m.files {
        if !safe_filename(&f.name)
            || !allowed_profile_file(&f.name)
            || f.blake3.len() != 64
            || !f.blake3.bytes().all(|b| b.is_ascii_hexdigit())
            || !names.insert(f.name.to_ascii_lowercase())
            || f.bytes > MAX_FILE_BYTES
        {
            bail!("invalid profile backup file record");
        }
        total = total
            .checked_add(f.bytes)
            .ok_or_else(|| anyhow::anyhow!("backup size overflow"))?;
    }
    for file in &m.files {
        let lower = file.name.to_ascii_lowercase();
        if lower.ends_with(".arma3profile")
            && !lower.ends_with(".vars.arma3profile")
            && !lower.ends_with(".3den.arma3profile")
        {
            if file.bytes == 0 {
                bail!("primary profile is empty");
            }
            primary_count += 1;
            primary_stem =
                Some(file.name[..file.name.len() - ".Arma3Profile".len()].to_ascii_lowercase());
        }
    }
    if primary_count != 1 {
        bail!("profile backup must contain exactly one primary profile");
    }
    let stem = primary_stem.ok_or_else(|| anyhow::anyhow!("missing primary profile"))?;
    for file in &m.files {
        let lower = file.name.to_ascii_lowercase();
        if !lower.starts_with(&stem) {
            bail!("profile backup filenames do not share a profile stem");
        }
        let suffix = &lower[stem.len()..];
        if !matches!(
            suffix,
            ".arma3profile" | ".vars.arma3profile" | ".3den.arma3profile"
        ) {
            bail!("invalid profile backup companion");
        }
    }
    if total > MAX_TOTAL_BYTES {
        bail!("profile backup exceeds size limit");
    }
    Ok(())
}
pub(super) fn safe_filename(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', ':', '\0'])
}
pub(super) fn allowed_profile_file(s: &str) -> bool {
    EXTENSIONS.iter().any(|suffix| {
        s.to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase())
    })
}
pub(super) fn safe_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(super) fn to_entry(directory: PathBuf, m: Manifest) -> BackupEntry {
    BackupEntry {
        id: m.id,
        profile_name: m.profile_name,
        original_path: m.original_path,
        created_at: m.created_at,
        file_count: m.files.len(),
        bytes: m.files.iter().map(|f| f.bytes).sum(),
        directory,
    }
}

pub(super) fn read_manifest(root: &Path, id: &str) -> Result<(PathBuf, Manifest)> {
    if !safe_id(id) {
        bail!("invalid backup id");
    }
    let dir = root.join(id);
    super::paths::check_path(&dir, true)?;
    let path = dir.join(MANIFEST);
    super::paths::check_path(&path, false)?;
    let bytes = read_bounded(&path, 1024 * 1024)?;
    let m: Manifest = serde_json::from_slice(&bytes)?;
    validate_manifest(&m)?;
    if m.id != id {
        bail!("backup id mismatch");
    }
    Ok((dir, m))
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("profile backup file exceeds size limit");
    }
    Ok(bytes)
}
