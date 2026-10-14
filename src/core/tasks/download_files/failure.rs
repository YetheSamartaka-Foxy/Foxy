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

pub(super) struct DownloadStop {
    missing_remote_file: AtomicBool,
    cancel_tx: watch::Sender<bool>,
}

impl DownloadStop {
    pub(super) fn new(cancel_tx: watch::Sender<bool>) -> Self {
        Self {
            missing_remote_file: AtomicBool::new(false),
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
}
