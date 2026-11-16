//! Turso supports no multi-process database access, so this lock is held for
//! the process lifetime; the PID sidecar stays readable while the lock is held.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{error, info, warn};

use crate::core::utils::format::sanitize_log_path;

/// Allow the new process to wait briefly while the installer closes the old one.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_POLL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,

    Busy { holder_pid: Option<u32> },

    Unavailable(String),
}

static HELD: Mutex<Option<(PathBuf, Arc<File>)>> = Mutex::new(None);

pub(crate) struct HeldClaimPin {
    _file: Arc<File>,
}

pub(crate) fn pin_held_claim(db_path: &Path) -> Result<HeldClaimPin, String> {
    let db_path = std::path::absolute(db_path)
        .map_err(|_| "Cannot resolve the database claim path".to_string())?;
    let held = HELD
        .lock()
        .map_err(|_| "Database ownership slot is unavailable".to_string())?;
    let Some((_, file)) = held.as_ref().filter(|(path, _)| path == &db_path) else {
        return Err("An exclusive claim on the selected game space is required".into());
    };
    Ok(HeldClaimPin {
        _file: Arc::clone(file),
    })
}

/// A target database claim prepared before the active database is closed.
/// Dropping the value releases the target without changing `HELD`.
pub(crate) struct ReservedClaim {
    db_path: PathBuf,
    file: File,
}

impl ReservedClaim {
    /// Install this claim after the old database handle and claim have closed.
    pub(crate) fn activate(self) -> LockOutcome {
        let Ok(mut held) = HELD.lock() else {
            return LockOutcome::Unavailable("lock slot poisoned".to_string());
        };
        if held
            .as_ref()
            .is_some_and(|(_, file)| Arc::strong_count(file) > 1)
        {
            return LockOutcome::Unavailable(
                "A running operation still pins the current database claim".into(),
            );
        }
        if held
            .as_ref()
            .is_some_and(|(held_path, _)| held_path != &self.db_path)
        {
            return LockOutcome::Unavailable(
                "current database claim must be released before activation".to_string(),
            );
        }
        let owner_path = owner_path_for(&self.db_path);
        if let Err(err) = File::create(&owner_path).and_then(|mut owner| {
            write!(owner, "{}", std::process::id()).and_then(|()| owner.flush())
        }) {
            warn!(
                "Could not record the database lock owner in {}: {}",
                sanitize_log_path(&owner_path),
                err
            );
        }
        info!(
            "Claimed exclusive access to database {}",
            sanitize_log_path(&self.db_path)
        );
        *held = Some((self.db_path, Arc::new(self.file)));
        LockOutcome::Acquired
    }
}

/// Keep the target locked until activation; publish owner metadata only after activation succeeds.
pub(crate) fn reserve(db_path: &Path) -> Result<ReservedClaim, LockOutcome> {
    let db_path = std::path::absolute(db_path).map_err(|_| {
        LockOutcome::Unavailable("Cannot resolve the database claim path".to_string())
    })?;
    let db_path = db_path.as_path();
    let held = HELD
        .lock()
        .map_err(|_| LockOutcome::Unavailable("lock slot poisoned".to_string()))?;
    if let Some((held_path, file)) = held.as_ref()
        && held_path == db_path
    {
        let file = file.try_clone().map_err(|err| {
            LockOutcome::Unavailable(format!(
                "could not retain the database claim for {}: {err}",
                sanitize_log_path(db_path)
            ))
        })?;
        return Ok(ReservedClaim {
            db_path: db_path.to_path_buf(),
            file,
        });
    }

    let lock_path = lock_path_for(db_path);
    if let Some(parent) = lock_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return Err(LockOutcome::Unavailable(format!(
            "could not create {}: {err}",
            sanitize_log_path(parent)
        )));
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| {
            LockOutcome::Unavailable(format!(
                "could not open {}: {err}",
                sanitize_log_path(&lock_path)
            ))
        })?;

    let deadline = Instant::now() + ACQUIRE_TIMEOUT;
    let mut waited = false;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                if !waited {
                    info!(
                        "Database {} is claimed by another Foxy process; waiting up to {}s for it to exit",
                        sanitize_log_path(db_path),
                        ACQUIRE_TIMEOUT.as_secs()
                    );
                    waited = true;
                }
                std::thread::sleep(ACQUIRE_POLL);
            }
            Err(TryLockError::WouldBlock) => {
                let holder_pid = read_holder_pid(db_path);
                error!(
                    "Database {} is already open in another Foxy process{}; this process will not reserve it",
                    sanitize_log_path(db_path),
                    holder_pid
                        .map(|pid| format!(" (PID {pid})"))
                        .unwrap_or_default()
                );
                return Err(LockOutcome::Busy { holder_pid });
            }
            Err(TryLockError::Error(err)) => {
                return Err(LockOutcome::Unavailable(format!(
                    "could not lock {}: {err}",
                    sanitize_log_path(&lock_path)
                )));
            }
        }
    }

    drop(held);
    Ok(ReservedClaim {
        db_path: db_path.to_path_buf(),
        file,
    })
}

fn lock_path_for(db_path: &Path) -> PathBuf {
    db_path.with_extension("lock")
}

fn owner_path_for(db_path: &Path) -> PathBuf {
    db_path.with_extension("owner")
}

/// Best-effort read of the PID recorded by whoever holds the lock. The sidecar
/// is deliberately not locked, so this works while the owner is running.
fn read_holder_pid(db_path: &Path) -> Option<u32> {
    let mut contents = String::new();
    File::open(owner_path_for(db_path))
        .ok()?
        .read_to_string(&mut contents)
        .ok()?;
    contents.trim().parse().ok()
}

/// A different database path requires a successful close of the current database first.
pub fn acquire(db_path: &Path) -> LockOutcome {
    let db_path = match std::path::absolute(db_path) {
        Ok(path) => path,
        Err(_) => {
            return LockOutcome::Unavailable("Cannot resolve the database claim path".to_string());
        }
    };
    let db_path = db_path.as_path();
    let Ok(mut held) = HELD.lock() else {
        return LockOutcome::Unavailable("lock slot poisoned".to_string());
    };
    if held.as_ref().is_some_and(|(path, _)| path == db_path) {
        return LockOutcome::Acquired;
    }
    if held.is_some() {
        return LockOutcome::Unavailable("Another game space is still claimed by this process; close its database before switching".to_string());
    }

    let lock_path = lock_path_for(db_path);
    if let Some(parent) = lock_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return LockOutcome::Unavailable(format!(
            "could not create {}: {err}",
            sanitize_log_path(parent)
        ));
    }

    let file = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(err) => {
            return LockOutcome::Unavailable(format!(
                "could not open {}: {err}",
                sanitize_log_path(&lock_path)
            ));
        }
    };

    let deadline = Instant::now() + ACQUIRE_TIMEOUT;
    let mut waited = false;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                if !waited {
                    info!(
                        "Database {} is claimed by another Foxy process; waiting up to {}s for it to exit",
                        sanitize_log_path(db_path),
                        ACQUIRE_TIMEOUT.as_secs()
                    );
                    waited = true;
                }
                std::thread::sleep(ACQUIRE_POLL);
            }
            Err(TryLockError::WouldBlock) => {
                let holder_pid = read_holder_pid(db_path);
                error!(
                    "Database {} is already open in another Foxy process{}; Turso does not support multi-process access, so this process will not touch it",
                    sanitize_log_path(db_path),
                    holder_pid
                        .map(|pid| format!(" (PID {pid})"))
                        .unwrap_or_default()
                );
                return LockOutcome::Busy { holder_pid };
            }
            Err(TryLockError::Error(err)) => {
                return LockOutcome::Unavailable(format!(
                    "could not lock {}: {err}",
                    sanitize_log_path(&lock_path)
                ));
            }
        }
    }

    let owner_path = owner_path_for(db_path);
    if let Err(err) = File::create(&owner_path)
        .and_then(|mut owner| write!(owner, "{}", std::process::id()).and_then(|()| owner.flush()))
    {
        warn!(
            "Could not record the database lock owner in {}: {}",
            sanitize_log_path(&owner_path),
            err
        );
    }

    info!(
        "Claimed exclusive access to database {}",
        sanitize_log_path(db_path)
    );
    *held = Some((db_path.to_path_buf(), Arc::new(file)));
    LockOutcome::Acquired
}

pub fn acquire_for_active_space() -> LockOutcome {
    acquire(&crate::core::tasks::db_turso::database_file_path())
}

pub fn holds(db_path: &Path) -> bool {
    let Ok(db_path) = std::path::absolute(db_path) else {
        return false;
    };
    HELD.lock()
        .ok()
        .is_some_and(|held| held.as_ref().is_some_and(|(path, _)| path == &db_path))
}

pub(crate) fn held_database_path() -> Option<PathBuf> {
    HELD.lock().ok()?.as_ref().map(|(path, _)| path.clone())
}

pub(crate) fn with_held_claim<T>(
    db_path: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let db_path = std::path::absolute(db_path)
        .map_err(|_| "Cannot resolve the database claim path".to_string())?;
    let held = HELD
        .lock()
        .map_err(|_| "Database ownership slot is unavailable".to_string())?;
    if held.as_ref().is_none_or(|(path, _)| path != &db_path) {
        return Err("An exclusive claim on the selected game space is required".to_string());
    }
    // Keep release/space switching blocked until the synchronous write finishes.
    let result = operation();
    drop(held);
    result
}

/// Release only the claim whose engine has been closed.
pub(crate) fn release_for(db_path: &Path) -> Result<(), String> {
    let db_path = std::path::absolute(db_path)
        .map_err(|_| "Cannot resolve the database claim path".to_string())?;
    let mut held = HELD
        .lock()
        .map_err(|_| "Database ownership slot is unavailable".to_string())?;
    if held.as_ref().is_some_and(|(path, _)| path != &db_path) {
        return Err("Cannot release a different game space's database claim".to_string());
    }
    if held
        .as_ref()
        .is_some_and(|(_, file)| Arc::strong_count(file) > 1)
    {
        return Err("A running operation still pins the database claim".into());
    }
    if let Some((path, file)) = held.take() {
        let _ = std::fs::remove_file(owner_path_for(&path));
        drop(file);
    }
    Ok(())
}

#[cfg(test)]
fn release() {
    let path = HELD.lock().unwrap().as_ref().map(|(path, _)| path.clone());
    if let Some(path) = path {
        release_for(&path).unwrap();
    }
}

pub fn diagnostics_state() -> String {
    let db_path = crate::core::tasks::db_turso::database_file_path();
    if holds(&db_path) {
        return "held_by_this_process".to_string();
    }
    match read_holder_pid(&db_path) {
        Some(pid) => format!("not_held (owner sidecar reports PID {pid})"),
        None => "not_held".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static SERIALIZE: Mutex<()> = Mutex::new(());

    fn serialized() -> std::sync::MutexGuard<'static, ()> {
        SERIALIZE.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn assert_lock_busy(db_path: &Path) {
        if let Ok(file) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path_for(db_path))
        {
            assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
        }
    }

    fn assert_lock_free(db_path: &Path) {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path_for(db_path))
            .unwrap();
        assert!(file.try_lock().is_ok());
    }

    /// A second claim on a path this process already owns is a no-op, so the
    /// lazy database funnel can call it on every open.
    #[test]
    fn reacquiring_the_same_path_is_idempotent() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("database.db");

        assert_eq!(acquire(&db), LockOutcome::Acquired);
        assert_eq!(acquire(&db), LockOutcome::Acquired);
        assert!(holds(&db));

        release();
        assert!(!holds(&db));
    }

    #[test]
    fn claiming_a_second_path_keeps_the_first_until_explicit_release() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a").join("database.db");
        let second = dir.path().join("b").join("database.db");

        assert_eq!(acquire(&first), LockOutcome::Acquired);
        assert!(matches!(acquire(&second), LockOutcome::Unavailable(_)));
        assert!(holds(&first));
        assert!(!holds(&second));
        assert!(!second.parent().unwrap().exists());
        release();
        assert_eq!(acquire(&second), LockOutcome::Acquired);
        release();
    }

    #[test]
    fn synchronous_owner_work_pins_the_claim_until_completion() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let other = dir.path().join("other/database.db");
        assert!(with_held_claim(&path, || Ok(())).is_err());
        assert_eq!(acquire(&path), LockOutcome::Acquired);
        assert!(with_held_claim(&other, || Ok(())).is_err());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let mut releasing = None;
        with_held_claim(&path, || {
            assert!(matches!(
                HELD.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ));
            let path = path.clone();
            releasing = Some(std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                release_for(&path).unwrap();
                finished_tx.send(()).unwrap();
            }));
            started_rx.recv().unwrap();
            assert!(finished_rx.try_recv().is_err());
            Ok(())
        })
        .unwrap();
        releasing.unwrap().join().unwrap();
        finished_rx.recv().unwrap();
        assert!(!holds(&path));
    }

    #[test]
    fn long_lived_pins_preserve_ownership_without_blocking_readers() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let other = dir.path().join("other/database.db");
        assert!(pin_held_claim(&path).is_err());
        assert_eq!(acquire(&path), LockOutcome::Acquired);
        assert!(pin_held_claim(&other).is_err());
        let pin = pin_held_claim(&path).unwrap();
        assert!(HELD.try_lock().is_ok());
        assert!(holds(&path));
        assert_eq!(held_database_path(), Some(path.clone()));
        assert!(release_for(&path).is_err());
        assert!(matches!(acquire(&other), LockOutcome::Unavailable(_)));
        let replacement = reserve(&path).unwrap();
        assert!(matches!(
            replacement.activate(),
            LockOutcome::Unavailable(_)
        ));
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path_for(&path))
            .unwrap();
        assert!(matches!(probe.try_lock(), Err(TryLockError::WouldBlock)));
        drop(probe);
        with_held_claim(&path, || Ok(())).unwrap();
        drop(pin);
        release_for(&path).unwrap();
        assert!(!holds(&path));
    }

    #[test]
    fn unavailable_lock_does_not_claim_or_create_a_database() {
        let _guard = serialized();
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("file");
        std::fs::write(&parent, b"unchanged").unwrap();
        let db = parent.join("database.db");
        assert!(matches!(acquire(&db), LockOutcome::Unavailable(_)));
        assert!(!holds(&db));
        assert!(!db.exists());
        assert_eq!(std::fs::read(&parent).unwrap(), b"unchanged");
    }

    #[test]
    fn an_independent_process_is_refused_until_release() {
        let _guard = serialized();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("database.db");
        assert_eq!(acquire(&path), LockOutcome::Acquired);
        let run_probe = |busy: bool| {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "core::tasks::db_process_lock::tests::independent_process_lock_probe",
                    "--ignored",
                ])
                .env("FOXY_TEST_DATABASE_LOCK_PATH", &path)
                .env("FOXY_TEST_LOCK_EXPECT_BUSY", if busy { "1" } else { "0" })
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "lock probe failed: {}",
                String::from_utf8_lossy(&result.stdout)
            );
        };
        run_probe(true);
        assert!(holds(&path));
        release();
        run_probe(false);
        assert!(!path.exists());
    }

    #[test]
    #[ignore = "Invoked only by the independent-process ownership test"]
    fn independent_process_lock_probe() {
        let path = PathBuf::from(std::env::var_os("FOXY_TEST_DATABASE_LOCK_PATH").unwrap());
        if std::env::var("FOXY_TEST_LOCK_EXPECT_BUSY").unwrap() == "1" {
            assert!(matches!(acquire(&path), LockOutcome::Busy { .. }));
        } else {
            assert_eq!(acquire(&path), LockOutcome::Acquired);
            release();
        }
    }

    /// Keep owner PID readable from a sidecar because Windows locks the lock file against reads.
    #[test]
    fn owner_sidecar_names_the_holding_process() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("database.db");

        assert_eq!(acquire(&db), LockOutcome::Acquired);
        assert_eq!(read_holder_pid(&db), Some(std::process::id()));

        release();
        assert_eq!(read_holder_pid(&db), None);
    }

    /// The lock lives beside the database, not on top of it.
    #[test]
    fn lock_and_owner_paths_do_not_collide_with_the_database() {
        let db = Path::new("C:/games/space/database.db");
        assert_eq!(lock_path_for(db), Path::new("C:/games/space/database.lock"));
        assert_eq!(
            owner_path_for(db),
            Path::new("C:/games/space/database.owner")
        );
    }

    #[test]
    fn reservation_keeps_current_claim_and_holds_target_until_activation() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current/database.db");
        let target = dir.path().join("target/database.db");

        assert_eq!(acquire(&current), LockOutcome::Acquired);
        let reservation = reserve(&target).unwrap();
        assert!(holds(&current));
        assert!(!holds(&target));
        assert_lock_busy(&current);
        assert_lock_busy(&target);

        release();
        assert_lock_free(&current);
        assert_lock_busy(&target);
        assert_eq!(reservation.activate(), LockOutcome::Acquired);
        assert!(!holds(&current));
        assert!(holds(&target));
        assert_lock_busy(&target);

        release();
        assert_lock_free(&target);
    }

    #[test]
    fn dropping_unused_reservation_releases_target_claim() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target/database.db");

        let reservation = reserve(&target).unwrap();
        assert!(!holds(&target));
        assert_lock_busy(&target);
        drop(reservation);

        assert_lock_free(&target);
    }

    #[test]
    fn reserving_current_database_does_not_release_its_active_claim() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("current/database.db");

        assert_eq!(acquire(&db_path), LockOutcome::Acquired);
        let reservation = reserve(&db_path).unwrap();
        drop(reservation);

        assert!(holds(&db_path));
        assert_lock_busy(&db_path);
        release();
        assert_lock_free(&db_path);
    }

    #[test]
    fn reservation_uses_the_same_normalized_identity_as_an_active_claim() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("database.db");
        let alias = dir.path().join(".").join("database.db");

        assert_eq!(acquire(&db_path), LockOutcome::Acquired);
        let reservation = reserve(&alias).unwrap();
        assert_eq!(reservation.activate(), LockOutcome::Acquired);
        assert!(holds(&db_path));
        release_for(&alias).unwrap();
        assert_lock_free(&db_path);
    }

    #[test]
    fn activation_refuses_to_drop_an_unreleased_current_claim() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("current/database.db");
        let target = dir.path().join("target/database.db");

        assert_eq!(acquire(&current), LockOutcome::Acquired);
        let reservation = reserve(&target).unwrap();
        assert!(matches!(
            reservation.activate(),
            LockOutcome::Unavailable(_)
        ));
        assert!(holds(&current));
        assert!(!holds(&target));
        assert_lock_free(&target);
        release();
    }
}
