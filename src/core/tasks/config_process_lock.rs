//! One operational Foxy process per configuration root.

use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::core::tasks::db_process_lock::LockOutcome;

const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_POLL: Duration = Duration::from_millis(100);
const LOCK_FILE_NAME: &str = ".foxy-config.lock";

static HELD: OnceLock<Mutex<HashMap<PathBuf, File>>> = OnceLock::new();
static ROOT_GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn lock_path_for(root: &Path) -> PathBuf {
    root.join(LOCK_FILE_NAME)
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .map_err(|_| "could not prepare the configuration root for process ownership".to_owned())?;
    root.canonicalize()
        .map_err(|_| "could not resolve the configuration root for process ownership".to_owned())
}

/// Claim `root` for this process, waiting briefly for an installer handoff.
/// Claims for different roots are independent and remain held until process exit.
pub fn acquire(root: &Path) -> LockOutcome {
    acquire_with_timeout(root, ACQUIRE_TIMEOUT)
}

fn acquire_with_timeout(root: &Path, timeout: Duration) -> LockOutcome {
    let root = match canonical_root(root) {
        Ok(root) => root,
        Err(error) => return LockOutcome::Unavailable(error),
    };
    let gates = ROOT_GATES.get_or_init(|| Mutex::new(HashMap::new()));
    let gate = match gates.lock() {
        Ok(mut gates) => gates
            .entry(root.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone(),
        Err(_) => return LockOutcome::Unavailable("configuration lock registry poisoned".into()),
    };
    let _root_gate = match gate.lock() {
        Ok(gate) => gate,
        Err(_) => return LockOutcome::Unavailable("configuration root lock poisoned".into()),
    };
    if HELD
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .is_some_and(|held| held.contains_key(&root))
    {
        return LockOutcome::Acquired;
    }

    let file = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path_for(&root))
    {
        Ok(file) => file,
        Err(_) => {
            return LockOutcome::Unavailable(
                "could not open the configuration process lock".to_owned(),
            );
        }
    };
    let deadline = Instant::now() + timeout;
    let mut waited = false;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                if !waited {
                    info!(
                        "Configuration root is claimed by another Foxy process; waiting briefly for it to exit"
                    );
                    waited = true;
                }
                std::thread::sleep(ACQUIRE_POLL.min(timeout));
            }
            Err(TryLockError::WouldBlock) => {
                warn!("Configuration root is already in use by another Foxy process");
                return LockOutcome::Busy { holder_pid: None };
            }
            Err(TryLockError::Error(_)) => {
                return LockOutcome::Unavailable(
                    "could not evaluate the configuration process lock".to_owned(),
                );
            }
        }
    }
    let Ok(mut held) = HELD.get_or_init(|| Mutex::new(HashMap::new())).lock() else {
        return LockOutcome::Unavailable("configuration lock registry poisoned".into());
    };
    held.insert(root, file);
    info!("Claimed exclusive process ownership of the configuration root");
    LockOutcome::Acquired
}

pub fn acquire_for_config_root() -> LockOutcome {
    acquire(&crate::core::utils::app_paths::foxy_data_dir())
}

#[cfg(test)]
fn holds(root: &Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .ok()
        .is_some_and(|held| held.contains_key(&root))
}

#[cfg(test)]
fn release_for_test(root: &Path) {
    if let Ok(root) = root.canonicalize()
        && let Ok(mut held) = HELD.get_or_init(|| Mutex::new(HashMap::new())).lock()
    {
        held.remove(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_root_claim_is_stable_and_holds_one_claim() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("config");
        assert_eq!(acquire(&root), LockOutcome::Acquired);
        assert_eq!(acquire(&root), LockOutcome::Acquired);
        assert!(holds(&root));
        assert!(lock_path_for(&root).exists());
        release_for_test(&root);
    }

    #[test]
    fn distinct_roots_are_claimed_independently() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        assert_eq!(acquire(&first), LockOutcome::Acquired);
        assert_eq!(acquire(&second), LockOutcome::Acquired);
        assert!(holds(&first));
        assert!(holds(&second));
        release_for_test(&first);
        release_for_test(&second);
    }

    #[test]
    fn external_same_root_claim_is_reported_busy() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("config");
        std::fs::create_dir_all(&root).unwrap();
        let external = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path_for(&root))
            .unwrap();
        external.try_lock().unwrap();
        assert_eq!(
            acquire_with_timeout(&root, Duration::from_millis(1)),
            LockOutcome::Busy { holder_pid: None }
        );
    }
}
