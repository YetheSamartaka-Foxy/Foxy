use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::super::ServerStoreError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentStagePhase {
    #[default]
    Preparing,
    Verifying,
    Copying,
    Checking,
    Complete,
    Cancelled,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_verification_cannot_finish_a_retained_owner_operation() {
        let control = ContentStageControl::default();
        control.defer_terminal();
        control.start_phase(ContentStagePhase::Copying, 1, 64);
        control.record_bytes(64);
        control.record_file();
        control.finish(&Ok(()));
        assert_eq!(control.snapshot().phase, ContentStagePhase::Copying);
        control.finish_operation(&Ok(()));
        assert_eq!(control.snapshot().phase, ContentStagePhase::Complete);
    }

    #[test]
    fn remote_progress_does_not_clear_local_cancellation() {
        let control = ContentStageControl::default();
        control.cancel();
        control.receive_progress(ContentStageProgress::default());
        assert!(control.snapshot().cancel_requested);
        control.finish_operation(&Err::<(), _>(ServerStoreError::Cancelled));
        assert_eq!(control.snapshot().phase, ContentStagePhase::Cancelled);
        control.finish_operation(&Err::<(), _>(ServerStoreError::DeploymentCancelled));
        assert_eq!(control.snapshot().phase, ContentStagePhase::Cancelled);
    }
}

#[derive(Clone, Debug, Default, Serialize, serde::Deserialize)]
pub struct ContentStageProgress {
    #[serde(default)]
    pub source: Option<String>,
    pub phase: ContentStagePhase,
    pub files_done: u64,
    pub bytes_done: u64,
    pub total_files: u64,
    pub total_bytes: u64,
    pub cancel_requested: bool,
}

#[derive(Debug, Default)]
struct Shared {
    cancelled: AtomicBool,
    defer_terminal: AtomicBool,
    progress: Mutex<ContentStageProgress>,
    #[cfg(test)]
    cancel_after_copy_bytes: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    cancel_on_phase: Mutex<Option<ContentStagePhase>>,
    #[cfg(test)]
    cancel_on_record_publication: AtomicBool,
    #[cfg(test)]
    cancel_before_record_selection: AtomicBool,
}

#[derive(Clone, Debug, Default)]
pub struct ContentStageControl {
    shared: Arc<Shared>,
}

impl ContentStageControl {
    pub(in crate::core::server) fn cancellation_flag(&self) -> &AtomicBool {
        &self.shared.cancelled
    }
    pub(in crate::core::server) fn defer_terminal(&self) {
        self.shared.defer_terminal.store(true, Ordering::Release);
    }

    pub(in crate::core::server) fn finish_operation<T>(
        &self,
        result: &Result<T, ServerStoreError>,
    ) {
        self.shared.defer_terminal.store(false, Ordering::Release);
        self.finish(result);
    }
    pub(crate) fn receive_progress(&self, mut progress: ContentStageProgress) {
        progress.cancel_requested |= self.shared.cancelled.load(Ordering::Acquire);
        *self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = progress;
    }
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Release);
    }

    pub fn snapshot(&self) -> ContentStageProgress {
        let mut progress = self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        progress.cancel_requested = self.shared.cancelled.load(Ordering::Acquire);
        progress
    }

    pub(in crate::core::server) fn checkpoint(&self) -> Result<(), ServerStoreError> {
        if self.shared.cancelled.load(Ordering::Acquire) {
            Err(ServerStoreError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub(in crate::core::server) fn start_phase(
        &self,
        phase: ContentStagePhase,
        total_files: u64,
        total_bytes: u64,
    ) {
        *self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = ContentStageProgress {
            source: None,
            phase,
            total_files,
            total_bytes,
            ..Default::default()
        };
        #[cfg(test)]
        if self.shared.cancel_on_phase.lock().unwrap().as_ref() == Some(&phase) {
            self.cancel();
        }
    }

    pub(in crate::core::server) fn record_bytes(&self, bytes: u64) {
        let mut progress = self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.bytes_done = progress.bytes_done.saturating_add(bytes);
        #[cfg(test)]
        {
            let threshold = self.shared.cancel_after_copy_bytes.load(Ordering::Relaxed);
            if progress.phase == ContentStagePhase::Copying
                && threshold > 0
                && progress.bytes_done >= threshold
            {
                self.cancel();
            }
        }
    }

    pub(in crate::core::server) fn record_file(&self) {
        let mut progress = self
            .shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.files_done = progress.files_done.saturating_add(1);
    }

    pub(in crate::core::server) fn finish<T>(&self, result: &Result<T, ServerStoreError>) {
        if self.shared.defer_terminal.load(Ordering::Acquire) {
            return;
        }
        self.shared
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .phase = match result {
            Ok(_) => ContentStagePhase::Complete,
            Err(
                ServerStoreError::Cancelled
                | ServerStoreError::ReviewCancelled
                | ServerStoreError::RecordCancelled
                | ServerStoreError::PreparationCancelled
                | ServerStoreError::DeploymentCancelled,
            ) => ContentStagePhase::Cancelled,
            Err(_) => ContentStagePhase::Failed,
        };
    }

    #[cfg(test)]
    pub(in crate::core::server) fn cancel_after_copy_bytes_for_test(&self, bytes: u64) {
        self.shared
            .cancel_after_copy_bytes
            .store(bytes, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::core::server) fn cancel_on_phase_for_test(&self, phase: ContentStagePhase) {
        *self.shared.cancel_on_phase.lock().unwrap() = Some(phase);
    }

    #[cfg(test)]
    pub(in crate::core::server) fn cancel_on_record_publication_for_test(&self) {
        self.shared
            .cancel_on_record_publication
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(in crate::core::server) fn record_published_for_test(&self) {
        if self
            .shared
            .cancel_on_record_publication
            .load(Ordering::Acquire)
        {
            self.cancel();
        }
    }

    #[cfg(test)]
    pub(in crate::core::server) fn cancel_before_record_selection_for_test(&self) {
        self.shared
            .cancel_before_record_selection
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(in crate::core::server) fn before_record_selection_for_test(&self) {
        if self
            .shared
            .cancel_before_record_selection
            .load(Ordering::Acquire)
        {
            self.cancel();
        }
    }
}
