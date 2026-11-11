use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct Journal {
    target: PathBuf,
    stage_name: String,
    backup_name: String,
    had_original: bool,
}

#[derive(Debug)]
pub struct DirectReplace {
    target: PathBuf,
    journal_path: PathBuf,
    _lock: File,
    stage_path: Option<PathBuf>,
    backup_path: Option<PathBuf>,
    preserve_recovery: bool,
}

pub fn has_pending_recovery(target: &Path) -> bool {
    append_suffix(target, ".foxy.direct-replace.json").exists()
}

impl DirectReplace {
    pub fn acquire(target: &Path) -> io::Result<Self> {
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = fs::canonicalize(parent)?;
        let name = target.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "replacement target has no file name",
            )
        })?;
        let target = parent.join(name);
        let lock_path = append_suffix(&target, ".foxy.direct-replace.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another Foxy direct download owns this target",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }

        let replacement = Self {
            journal_path: append_suffix(&target, ".foxy.direct-replace.json"),
            target,
            _lock: lock,
            stage_path: None,
            backup_path: None,
            preserve_recovery: false,
        };
        Ok(replacement)
    }

    pub fn recover(&mut self) -> io::Result<()> {
        self.recover_journal()
    }

    pub fn create_staging_file(&mut self) -> io::Result<(PathBuf, File)> {
        if self.stage_path.is_some() || self.journal_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "replacement transaction already has a journal",
            ));
        }

        let had_original = match fs::symlink_metadata(&self.target) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => true,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "replacement target is not a file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let name = self
            .target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "replacement target name is not UTF-8",
                )
            })?;
        let (stage_name, backup_name) = unique_names(name);
        let journal = Journal {
            target: self.target.clone(),
            stage_name: stage_name.clone(),
            backup_name: backup_name.clone(),
            had_original,
        };
        let mut journal_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.journal_path)?;
        journal_file.write_all(&serde_json::to_vec(&journal).map_err(io::Error::other)?)?;
        journal_file.sync_all()?;
        sync_parent(&self.journal_path);

        let stage_path = self.target.with_file_name(stage_name);
        let backup_path = self.target.with_file_name(backup_name);
        let stage = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage_path)
        {
            Ok(stage) => stage,
            Err(error) => {
                let _ = fs::remove_file(&self.journal_path);
                return Err(error);
            }
        };
        self.stage_path = Some(stage_path.clone());
        self.backup_path = Some(backup_path);
        Ok((stage_path, stage))
    }

    pub fn promote(&mut self) -> io::Result<()> {
        self.promote_with(|| {})
    }

    fn promote_with(&mut self, after_backup: impl FnOnce()) -> io::Result<()> {
        let stage = self.stage_path.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "replacement has no staged file",
            )
        })?;
        let backup = self
            .backup_path
            .as_ref()
            .expect("staging creates backup path");
        OpenOptions::new().write(true).open(stage)?.sync_all()?;

        let journal: Journal = read_journal(&self.journal_path)?;
        if journal.had_original {
            if !self.target.exists() && fs::symlink_metadata(&self.target).is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "original target disappeared before promotion",
                ));
            }
            fs::rename(&self.target, backup)?;
            sync_parent(&self.target);
            after_backup();
        } else if fs::symlink_metadata(&self.target).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "target appeared after collision approval",
            ));
        }

        if let Err(error) = fs::rename(stage, &self.target) {
            if journal.had_original {
                match restore_backup(backup, &self.target) {
                    Ok(()) => {
                        sync_parent(&self.target);
                        self.cleanup_uncommitted();
                    }
                    Err(restore_error) => {
                        self.preserve_recovery = true;
                        return Err(io::Error::new(
                            restore_error.kind(),
                            format!(
                                "promotion failed ({error}); original remains recoverable at {} but could not be restored ({restore_error})",
                                backup.display()
                            ),
                        ));
                    }
                }
            } else {
                self.cleanup_uncommitted();
            }
            return Err(error);
        }
        sync_parent(&self.target);
        self.stage_path = None;
        if let Err(error) = remove_if_exists(backup) {
            log::warn!(
                "Direct replacement committed; previous file remains in recoverable backup: {}",
                error
            );
            self.preserve_recovery = true;
            return Ok(());
        }
        if let Err(error) = remove_if_exists(&self.journal_path) {
            log::warn!(
                "Direct replacement committed; recovery journal cleanup failed: {}",
                error
            );
        }
        self.backup_path = None;
        Ok(())
    }

    fn recover_journal(&mut self) -> io::Result<()> {
        if !self.journal_path.exists() {
            return Ok(());
        }
        let journal = read_journal(&self.journal_path)?;
        if journal.target != self.target {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "direct replacement journal belongs to a different target",
            ));
        }
        let target_name = self
            .target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "replacement target name is not UTF-8",
                )
            })?;
        if !journal
            .stage_name
            .starts_with(&format!("{target_name}.foxy.direct-replace."))
            || !journal.stage_name.ends_with(".part")
            || !journal
                .backup_name
                .starts_with(&format!("{target_name}.foxy.direct-replace."))
            || !journal.backup_name.ends_with(".bak")
            || Path::new(&journal.stage_name).file_name()
                != Some(std::ffi::OsStr::new(&journal.stage_name))
            || Path::new(&journal.backup_name).file_name()
                != Some(std::ffi::OsStr::new(&journal.backup_name))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "direct replacement journal contains unsafe sibling names",
            ));
        }
        let stage = self.target.with_file_name(journal.stage_name);
        let backup = self.target.with_file_name(journal.backup_name);
        let stage_exists = stage.exists();
        let backup_exists = backup.exists();
        let target_exists = fs::symlink_metadata(&self.target).is_ok();

        if backup_exists && target_exists && stage_exists {
            self.preserve_recovery = true;
            self.stage_path = Some(stage);
            self.backup_path = Some(backup);
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "replacement recovery found both a target and original backup; all files were preserved",
            ));
        }
        if backup_exists && !target_exists {
            fs::rename(&backup, &self.target)?;
            sync_parent(&self.target);
        } else if backup_exists && !stage_exists {
            remove_if_exists(&backup)?;
        } else if !backup_exists && !stage_exists && !target_exists {
            self.preserve_recovery = true;
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "replacement recovery found no target, staging file, or backup",
            ));
        }
        remove_if_exists(&stage)?;
        remove_if_exists(&self.journal_path)?;
        sync_parent(&self.target);
        Ok(())
    }

    fn cleanup_uncommitted(&mut self) {
        if let Some(stage) = &self.stage_path {
            let _ = remove_if_exists(stage);
        }
        if self
            .backup_path
            .as_ref()
            .is_none_or(|backup| !backup.exists())
        {
            let _ = remove_if_exists(&self.journal_path);
            self.backup_path = None;
        } else {
            self.preserve_recovery = true;
        }
        self.stage_path = None;
    }
}

impl Drop for DirectReplace {
    fn drop(&mut self) {
        if self.preserve_recovery {
            return;
        }
        if let Some(stage) = &self.stage_path {
            let _ = remove_if_exists(stage);
        }
        if self
            .backup_path
            .as_ref()
            .is_none_or(|backup| !backup.exists())
        {
            let _ = remove_if_exists(&self.journal_path);
        }
    }
}

fn read_journal(path: &Path) -> io::Result<Journal> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn restore_backup(backup: &Path, target: &Path) -> io::Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "target path is occupied; original backup was left untouched",
        ));
    }
    fs::rename(backup, target)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    value.into()
}

fn unique_names(target_name: &str) -> (String, String) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let identity = format!("{}.{nanos}.{counter}", std::process::id());
    (
        format!("{target_name}.foxy.direct-replace.{identity}.part"),
        format!("{target_name}.foxy.direct-replace.{identity}.bak"),
    )
}

#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_foxy_sidecars_are_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("addon.bin");
        let part = append_suffix(&target, ".foxy.part");
        let backup = append_suffix(&target, ".foxy.bak");
        fs::write(&part, b"unowned part").unwrap();
        fs::write(&backup, b"unowned backup").unwrap();

        let mut replacement = DirectReplace::acquire(&target).unwrap();
        let (_, mut stage) = replacement.create_staging_file().unwrap();
        stage.write_all(b"new bytes").unwrap();
        drop(stage);
        replacement.promote().unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new bytes");
        assert_eq!(fs::read(part).unwrap(), b"unowned part");
        assert_eq!(fs::read(backup).unwrap(), b"unowned backup");
    }

    #[test]
    fn failed_promotion_and_restore_keep_original_in_journaled_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("addon.bin");
        fs::write(&target, b"original bytes").unwrap();
        let mut replacement = DirectReplace::acquire(&target).unwrap();
        let (stage_path, mut stage) = replacement.create_staging_file().unwrap();
        stage.write_all(b"replacement bytes").unwrap();
        drop(stage);

        let error = replacement
            .promote_with(|| fs::create_dir(&target).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("remains recoverable"));
        let backup = replacement.backup_path.as_ref().unwrap().clone();
        let journal = replacement.journal_path.clone();
        assert_eq!(fs::read(&backup).unwrap(), b"original bytes");
        assert!(stage_path.exists());
        assert!(journal.exists());
        drop(replacement);

        fs::remove_dir(&target).unwrap();
        let mut replacement = DirectReplace::acquire(&target).unwrap();
        replacement.recover().unwrap();
        assert_eq!(fs::read(target).unwrap(), b"original bytes");
        assert!(!backup.exists());
        assert!(!journal.exists());
    }

    #[test]
    fn concurrent_owner_is_refused_without_unlinking_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("addon.bin");
        let owner = DirectReplace::acquire(&target).unwrap();
        let lock_path = append_suffix(&target, ".foxy.direct-replace.lock");
        assert!(lock_path.exists());
        let error = DirectReplace::acquire(&target).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(lock_path.exists());
        drop(owner);
    }
}
