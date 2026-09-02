//! Cross-process exclusive claim on a game space's database.
//!
//! Turso's manual states plainly that it has "no multi-process access": two
//! processes opening the same file is undefined behavior, not a supported
//! concurrent-writer setup the way stock SQLite's WAL is. Foxy ships one binary
//! that runs as both a GUI and a CLI against the same data root, and nothing
//! stopped a second launch (or a CLI command run while the window is open) from
//! opening `database.db` alongside the first. The failure mode is the worst
//! kind: no error, just a database that quietly stops agreeing with itself.
//!
//! The claim is an advisory whole-file lock on `database.lock` held for the
//! life of the process. The OS drops it when the process exits, crash included,
//! so there is no stale-lock state to repair. The owning PID is kept in an
//! unlocked `database.owner` sidecar purely so the "already running" message can
//! name the process; on Windows the lock itself blocks reads, so the PID cannot
//! live in the locked file.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use log::{error, info, warn};

use crate::core::utils::format::sanitize_log_path;

/// How long to keep retrying a contended lock before giving up.
///
/// Not just politeness: the Windows installer runs with `/CLOSEAPPLICATIONS` and
/// can start the new Foxy while the old one is still tearing down, so a fresh
/// launch must be able to wait out the handoff instead of refusing to start.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_POLL: Duration = Duration::from_millis(100);

/// Result of trying to claim a game space's database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    /// This process now owns the database.
    Acquired,
    /// Another Foxy process owns it. Carries the owner's PID when the sidecar
    /// could be read.
    Busy { holder_pid: Option<u32> },
    /// The lock could not be evaluated (permissions, read-only volume, an
    /// filesystem with no lock support). Never treated as contention - refusing
    /// to start over an unreadable lock file would be worse than the risk it
    /// guards against.
    Unavailable(String),
}

/// The lock currently held by this process, with the database path it covers.
/// Dropping the `File` releases the OS lock, so a game-space switch releases the
/// previous space by replacing this slot.
static HELD: Mutex<Option<(PathBuf, File)>> = Mutex::new(None);

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

/// Claim `db_path` for this process, waiting out a brief handoff window.
///
/// Idempotent: calling it again for a path this process already holds returns
/// [`LockOutcome::Acquired`] without touching the filesystem. Claiming a
/// different path releases the previous claim, which is what a game-space
/// switch wants.
pub fn acquire(db_path: &Path) -> LockOutcome {
    let Ok(mut held) = HELD.lock() else {
        return LockOutcome::Unavailable("lock slot poisoned".to_string());
    };
    if held.as_ref().is_some_and(|(path, _)| path == db_path) {
        return LockOutcome::Acquired;
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

    // Safe to publish the PID now: nobody else can hold the lock, and the
    // sidecar is only ever read while a live process owns it.
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
    *held = Some((db_path.to_path_buf(), file));
    LockOutcome::Acquired
}

/// Claim the active game space's database.
pub fn acquire_for_active_space() -> LockOutcome {
    acquire(&crate::core::tasks::db_turso::database_file_path())
}

/// Whether this process holds the claim on `db_path`.
pub fn holds(db_path: &Path) -> bool {
    HELD.lock()
        .ok()
        .is_some_and(|held| held.as_ref().is_some_and(|(path, _)| path == db_path))
}

/// Release whatever claim this process holds, if any. Used when closing a game
/// space's database so another process (or another space) can take it.
pub fn release() {
    if let Ok(mut held) = HELD.lock()
        && let Some((path, _)) = held.take()
    {
        info!(
            "Released exclusive access to database {}",
            sanitize_log_path(&path)
        );
        let _ = std::fs::remove_file(owner_path_for(&path));
    }
}

/// One-line lock state for the diagnostics manifest.
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

    /// `HELD` is process-wide, so these tests cannot run concurrently with each
    /// other. Poisoning is ignored: a failing test must not cascade.
    static SERIALIZE: Mutex<()> = Mutex::new(());

    fn serialized() -> std::sync::MutexGuard<'static, ()> {
        SERIALIZE.lock().unwrap_or_else(|err| err.into_inner())
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

    /// Switching game spaces claims the new space and drops the old one.
    #[test]
    fn claiming_a_second_path_releases_the_first() {
        let _guard = serialized();
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a").join("database.db");
        let second = dir.path().join("b").join("database.db");

        assert_eq!(acquire(&first), LockOutcome::Acquired);
        assert_eq!(acquire(&second), LockOutcome::Acquired);

        assert!(!holds(&first));
        assert!(holds(&second));
        release();
    }

    /// The owning PID is published where a contending process can read it even
    /// though the lock file itself is unreadable while locked.
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
}
