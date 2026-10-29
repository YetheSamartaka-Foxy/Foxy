use std::collections::HashMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, TryLockError as MutexTryLockError};

use crate::core::utils::fs_safety::{atomic_write, is_safe_child_path};

use super::{extra_files, reforger, workshop};

pub(super) const IMPORT_RECOVERY_DIR: &str = ".foxypack-import-recovery";
const IMPORT_RECOVERY_FILES: &[&str] = &[
    "repositories.json",
    "repository_spaces.json",
    extra_files::EXTRA_FILES_FILE,
    workshop::WORKSHOP_FILE,
    reforger::REFORGER_ADDONS_FILE,
    extra_files::EXTRA_FILES_DIR,
];
const IMPORT_LOCK_FILE: &str = ".foxypack-import.lock";
static ACTIVE_IMPORTS: LazyLock<Mutex<HashMap<PathBuf, ActiveImport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct ActiveImport {
    _file: File,
    operation: Arc<Mutex<()>>,
}

pub(super) struct ImportClaim {
    pub(super) space_dir: PathBuf,
    retained: bool,
}

impl ImportClaim {
    pub(super) fn acquire(space_dir: &Path, create: bool) -> Result<Self, String> {
        if let Ok(metadata) = fs::symlink_metadata(space_dir)
            && metadata.file_type().is_symlink()
        {
            return Err("Game-space directory must not be a symbolic link".to_string());
        }
        if create {
            fs::create_dir_all(space_dir)
                .map_err(|err| format!("Failed to create game-space directory: {err}"))?;
        } else if !space_dir.exists() {
            return Err("Game-space directory does not exist".to_string());
        }
        let space_dir = canonical_space_dir(space_dir)?;
        let mut active = ACTIVE_IMPORTS
            .lock()
            .map_err(|_| "Config-pack import ownership lock is poisoned".to_string())?;
        if active.contains_key(&space_dir) {
            return Err(
                "A config-pack operation is already active for this game space".to_string(),
            );
        }
        let lock_path = space_dir.join(IMPORT_LOCK_FILE);
        if let Ok(metadata) = fs::symlink_metadata(&lock_path)
            && metadata.file_type().is_symlink()
        {
            return Err("Config-pack import lock must not be a symbolic link".to_string());
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|err| format!("Failed to open config-pack import lock: {err}"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(
                    "A config-pack operation is already active for this game space".to_string(),
                );
            }
            Err(TryLockError::Error(err)) => {
                return Err(format!("Failed to lock config-pack import state: {err}"));
            }
        }
        active.insert(
            space_dir.clone(),
            ActiveImport {
                _file: file,
                operation: Arc::new(Mutex::new(())),
            },
        );
        Ok(Self {
            space_dir,
            retained: false,
        })
    }

    pub(super) fn retain(mut self) {
        self.retained = true;
    }
}

impl Drop for ImportClaim {
    fn drop(&mut self) {
        if !self.retained {
            release_import_claim(&self.space_dir);
        }
    }
}

fn release_import_claim(space_dir: &Path) {
    if let Ok(mut active) = ACTIVE_IMPORTS.lock() {
        active.remove(space_dir);
    }
}

fn canonical_space_dir(space_dir: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(space_dir)
        .map_err(|err| format!("Failed to resolve game-space directory: {err}"))
}

fn active_import_operation_lock(space_dir: &Path) -> Result<Arc<Mutex<()>>, String> {
    let active = ACTIVE_IMPORTS
        .lock()
        .map_err(|_| "Config-pack import ownership lock is poisoned".to_string())?;
    active
        .get(space_dir)
        .map(|claim| claim.operation.clone())
        .ok_or_else(|| "No active config-pack import is owned for this game space".to_string())
}

pub(super) fn validate_managed_paths(space_dir: &Path) -> Result<(), String> {
    for name in IMPORT_RECOVERY_FILES {
        let path = space_dir.join(name);
        validate_no_symlinks(&path)?;
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            let expected_directory = *name == extra_files::EXTRA_FILES_DIR;
            if metadata.is_dir() != expected_directory {
                return Err(format!(
                    "Managed config-pack path has an unexpected type: {name}"
                ));
            }
        }
    }
    for name in ["extra_files.import.tmp", IMPORT_RECOVERY_DIR] {
        let path = space_dir.join(name);
        validate_no_symlinks(&path)?;
        if let Ok(metadata) = fs::symlink_metadata(&path)
            && !metadata.is_dir()
        {
            return Err(format!(
                "Config-pack staging path has an unexpected type: {name}"
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_no_symlinks(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Could not inspect managed config-pack path: {error}"
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err("Managed config-pack paths must not contain symbolic links".to_string());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)
            .map_err(|error| format!("Could not inspect managed config-pack directory: {error}"))?
        {
            let entry = entry
                .map_err(|error| format!("Could not inspect managed config-pack entry: {error}"))?;
            validate_no_symlinks(&entry.path())?;
        }
    }
    Ok(())
}

pub(super) fn begin_import_recovery(space_dir: &Path) -> Result<(), String> {
    validate_managed_paths(space_dir)?;
    recover_pending_import_inner(space_dir)?;
    let recovery_dir = space_dir.join(IMPORT_RECOVERY_DIR);
    let backups = recovery_dir.join("files");
    fs::create_dir_all(&backups)
        .map_err(|err| format!("Failed to create config-pack recovery data: {}", err))?;
    let mut present = Vec::new();
    for name in IMPORT_RECOVERY_FILES {
        let source = space_dir.join(name);
        if source.exists() {
            copy_path(&source, &backups.join(name))?;
        }
        present.push((*name).to_string());
    }
    let marker = recovery_dir.join("pending.json");
    atomic_write(
        &marker,
        &serde_json::to_vec(&present).map_err(|err| err.to_string())?,
    )
    .map_err(|err| format!("Failed to write config-pack recovery journal: {}", err))
}

/// Restores an interrupted config-pack import for this game space.
pub fn recover_pending_import(space_dir: &Path) -> Result<(), String> {
    let _claim = ImportClaim::acquire(space_dir, false)?;
    recover_pending_import_inner(&_claim.space_dir)
}

pub(super) fn recover_pending_import_inner(space_dir: &Path) -> Result<(), String> {
    validate_managed_paths(space_dir)?;
    let recovery_dir = space_dir.join(IMPORT_RECOVERY_DIR);
    let marker = recovery_dir.join("pending.json");
    if !marker.is_file() {
        if recovery_dir.exists() {
            fs::remove_dir_all(&recovery_dir).map_err(|err| {
                format!(
                    "Failed to remove incomplete config-pack recovery data: {}",
                    err
                )
            })?;
        }
        return Ok(());
    }

    let files: Vec<String> = serde_json::from_slice(
        &fs::read(&marker)
            .map_err(|err| format!("Failed to read config-pack recovery journal: {}", err))?,
    )
    .map_err(|err| format!("Invalid config-pack recovery journal: {}", err))?;
    let backups = recovery_dir.join("files");
    for name in files {
        if !is_safe_child_path(&name) || !IMPORT_RECOVERY_FILES.contains(&name.as_str()) {
            return Err("Invalid path in config-pack recovery journal".to_string());
        }
        let target = space_dir.join(&name);
        let backup = backups.join(&name);
        remove_path_if_exists(&target)?;
        if backup.exists() {
            copy_path(&backup, &target)?;
        }
    }
    let _ = fs::remove_dir_all(space_dir.join("extra_files.import.tmp"));
    fs::remove_dir_all(&recovery_dir)
        .map_err(|err| format!("Failed to clear config-pack recovery data: {}", err))
}

/// Rolls back a failed import that is still owned by this process.
pub fn rollback_pending_import(space_dir: &Path) -> Result<(), String> {
    let key = canonical_space_dir(space_dir)?;
    let operation = active_import_operation_lock(&key)?;
    let _operation_guard = operation.try_lock().map_err(|error| match error {
        MutexTryLockError::WouldBlock => {
            "Another config-pack finalization is already active for this game space".to_string()
        }
        MutexTryLockError::Poisoned(_) => "Config-pack operation lock is poisoned".to_string(),
    })?;
    let result = recover_pending_import_inner(&key);
    drop(_operation_guard);
    release_import_claim(&key);
    result
}

/// Marks the current import durable after the caller has saved its config files.
pub fn commit_pending_import(space_dir: &Path) -> Result<(), String> {
    let key = canonical_space_dir(space_dir)?;
    let operation = active_import_operation_lock(&key)?;
    let _operation_guard = operation.try_lock().map_err(|error| match error {
        MutexTryLockError::WouldBlock => {
            "Another config-pack finalization is already active for this game space".to_string()
        }
        MutexTryLockError::Poisoned(_) => "Config-pack operation lock is poisoned".to_string(),
    })?;
    if let Err(commit_error) = commit_pending_import_inner(&key) {
        let rollback_error = recover_pending_import_inner(&key).err();
        drop(_operation_guard);
        release_import_claim(&key);
        return Err(match rollback_error {
            Some(rollback_error) => format!(
                "{}; import rollback failed: {}",
                commit_error, rollback_error
            ),
            None => commit_error,
        });
    }
    drop(_operation_guard);
    release_import_claim(&key);
    Ok(())
}

fn commit_pending_import_inner(space_dir: &Path) -> Result<(), String> {
    let recovery_dir = space_dir.join(IMPORT_RECOVERY_DIR);
    if recovery_dir.exists() {
        let marker = recovery_dir.join("pending.json");
        if marker.exists() {
            fs::remove_file(&marker)
                .map_err(|err| format!("Failed to commit config-pack import: {}", err))?;
        }
        let _ = fs::remove_dir_all(&recovery_dir);
    }
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("Failed to inspect config-pack target: {err}")),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .map_err(|err| format!("Failed to restore config-pack target: {}", err))
}

fn copy_path(source: &Path, target: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|err| format!("Failed to inspect config-pack source: {err}"))?;
    if metadata.file_type().is_symlink() {
        return Err("Managed config-pack paths must not contain symbolic links".to_string());
    }
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|err| err.to_string())?;
        for entry in fs::read_dir(source).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            copy_path(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::copy(source, target).map_err(|err| err.to_string())?;
    } else {
        return Err("Unsupported filesystem entry in config-pack data".to_string());
    }
    Ok(())
}
