use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;

pub(super) fn is_missing_remote_file(error: &anyhow::Error) -> bool {
    error.downcast_ref::<reqwest::Error>().is_some_and(|error| {
        matches!(
            error.status(),
            Some(reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE)
        )
    })
}

/// Another process holds the file open without delete sharing (a running game
/// keeps its PBOs open this way), so it cannot be renamed aside or replaced.
pub(super) fn is_target_in_use(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(is_in_use_io_error)
}

fn is_in_use_io_error(error: &std::io::Error) -> bool {
    #[cfg(target_os = "windows")]
    {
        use winapi::shared::winerror::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
        matches!(
            error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok()),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        )
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = error;
        false
    }
}

/// Whether another process holds `path` open in a way that refuses the rename
/// a promote performs. Opening for delete access asks the same question the
/// rename does without changing the file.
#[cfg(target_os = "windows")]
fn target_is_held_open(path: &std::path::Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use winapi::um::winnt::{DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};
    match std::fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
    {
        Ok(_) => false,
        Err(error) => is_in_use_io_error(&error),
    }
}

#[cfg(not(target_os = "windows"))]
fn target_is_held_open(_path: &std::path::Path) -> bool {
    false
}

/// Existing target files that another process is holding open. A second probe
/// after a short pause drops brief holds such as an antivirus scan, so only a
/// lasting holder (a running game) blocks the update.
pub(super) fn find_targets_in_use(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let held: Vec<_> = paths
        .iter()
        .filter(|path| target_is_held_open(path))
        .cloned()
        .collect();
    if held.is_empty() {
        return held;
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    held.into_iter()
        .filter(|path| target_is_held_open(path))
        .collect()
}

/// After a promote failed with a sharing violation: whether the existing target
/// is what another process holds. A brief hold on the staged file (an antivirus
/// scan of the fresh download) leaves the target free and is retried instead.
pub(super) async fn target_still_held(target_path: &str) -> bool {
    let path = std::path::PathBuf::from(target_path);
    tokio::task::spawn_blocking(move || {
        !find_targets_in_use(std::slice::from_ref(&path)).is_empty()
    })
    .await
    .unwrap_or(false)
}

#[derive(Debug)]
pub(crate) struct TargetsInUse {
    pub(super) count: usize,
    pub(super) example: Option<String>,
}

impl TargetsInUse {
    pub(super) fn from_paths(paths: &[std::path::PathBuf]) -> Self {
        Self {
            count: paths.len(),
            example: paths
                .first()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned()),
        }
    }
}

impl std::fmt::Display for TargetsInUse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.example, self.count) {
            (Some(example), count) if count > 0 => write!(
                formatter,
                "{count} file(s) to update are in use by another program, for example {example}. Close the game and any program using the repository files, then retry the update."
            ),
            _ => formatter.write_str(
                "Files to update are in use by another program. Close the game and any program using the repository files, then retry the update.",
            ),
        }
    }
}

impl std::error::Error for TargetsInUse {}

pub(super) struct DownloadStop {
    missing_remote_file: AtomicBool,
    target_in_use: AtomicBool,
    cancel_tx: watch::Sender<bool>,
}

impl DownloadStop {
    pub(super) fn new(cancel_tx: watch::Sender<bool>) -> Self {
        Self {
            missing_remote_file: AtomicBool::new(false),
            target_in_use: AtomicBool::new(false),
            cancel_tx,
        }
    }

    pub(super) fn remote_file_missing(&self) {
        self.missing_remote_file.store(true, Ordering::SeqCst);
        self.cancel_tx.send_replace(true);
    }

    pub(super) fn has_missing_remote_file(&self) -> bool {
        self.missing_remote_file.load(Ordering::SeqCst)
    }

    /// The update is rolled back anyway once a file cannot be replaced, so the
    /// remaining transfers stop instead of finishing for nothing.
    pub(super) fn target_held_open(&self) {
        self.target_in_use.store(true, Ordering::SeqCst);
        self.cancel_tx.send_replace(true);
    }

    pub(super) fn has_target_in_use(&self) -> bool {
        self.target_in_use.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub(crate) struct MissingRemoteFiles;

impl std::fmt::Display for MissingRemoteFiles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("The server no longer provides a queued file (HTTP 404 or 410). Retry the update to refresh the server's file list.")
    }
}

impl std::error::Error for MissingRemoteFiles {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_remote_file_stops_transfers_without_user_cancellation() {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let stop = DownloadStop::new(cancel_tx);
        assert!(!stop.has_missing_remote_file());
        assert!(!*cancel_rx.borrow());
        stop.remote_file_missing();
        assert!(stop.has_missing_remote_file());
        assert!(*cancel_rx.borrow());
    }

    #[test]
    fn ordinary_errors_do_not_invalidate_remote_metadata() {
        assert!(!is_missing_remote_file(&anyhow::anyhow!("timeout")));
    }

    #[test]
    fn a_held_target_stops_transfers_and_names_the_cause() {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let stop = DownloadStop::new(cancel_tx);
        stop.target_held_open();
        assert!(stop.has_target_in_use());
        assert!(!stop.has_missing_remote_file());
        assert!(*cancel_rx.borrow());

        let message = TargetsInUse::from_paths(&[std::path::PathBuf::from(
            "R:/Mods/@mod_01/addons/mod_01.pbo",
        )])
        .to_string();
        assert!(message.starts_with("1 file(s) to update are in use"));
        assert!(message.contains("mod_01.pbo"));
        assert!(!message.contains("R:/Mods"));
    }

    #[test]
    fn missing_and_free_targets_are_not_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let free = dir.path().join("free.pbo");
        std::fs::write(&free, b"free").unwrap();
        let _reader = std::fs::File::open(&free).unwrap();
        let missing = dir.path().join("missing.pbo");
        assert!(find_targets_in_use(&[free, missing]).is_empty());
        assert!(!is_target_in_use(&anyhow::anyhow!("timeout")));
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn a_target_held_without_delete_sharing_is_reported_and_its_promote_fails_in_use() {
        use std::os::windows::fs::OpenOptionsExt;
        use winapi::um::winnt::FILE_SHARE_READ;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("held.pbo");
        std::fs::write(&target, b"old").unwrap();
        let _game_handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&target)
            .unwrap();

        assert_eq!(
            find_targets_in_use(std::slice::from_ref(&target)),
            vec![target.clone()]
        );

        let staged = dir.path().join("held.pbo.foxy.part");
        std::fs::write(&staged, b"new").unwrap();
        let mut session = super::super::UpdateRollbackSession::new(
            dir.path().join("rollback"),
            "https://repo.example.invalid/",
        )
        .await
        .unwrap();
        let error = session.promote_file(1, &staged, &target).await.unwrap_err();
        assert!(is_target_in_use(&error), "{error:#}");
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(target_still_held(&target.to_string_lossy()).await);
        assert!(!target_still_held(&staged.to_string_lossy()).await);
    }
}
