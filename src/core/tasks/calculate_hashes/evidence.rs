use crate::core::db::{DbValue, FoxyDb};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HashIssueKind {
    Missing,
    Unreadable,
    Unstable,
    SaveFailed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HashIssue {
    pub(crate) file_path: String,
    pub(crate) kind: HashIssueKind,
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StalledReadDecision {
    Waiting,
    Continue,
    Skip,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StalledRead {
    pub(crate) file_path: String,
    pub(crate) no_progress_secs: u64,
}

type RetrySave =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync>;

#[derive(Clone)]
struct RetryFileEvidence {
    path: String,
    identity: Option<super::verified_record::FileIdentity>,
    manifest_signature: String,
}

#[derive(Default)]
struct EvidenceInner {
    issues: Vec<HashIssue>,
    stalled: HashMap<String, (StalledRead, watch::Sender<StalledReadDecision>)>,
    retry_save: Vec<RetrySave>,
    retry_db: Option<FoxyDb>,
    retry_files: HashMap<String, RetryFileEvidence>,
}

#[derive(Default)]
pub(crate) struct HashEvidenceState {
    inner: Mutex<EvidenceInner>,
    unattended: std::sync::atomic::AtomicBool,
}

impl HashEvidenceState {
    pub(crate) fn record_issue(&self, issue: HashIssue) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(existing) = inner
            .issues
            .iter_mut()
            .find(|existing| existing.file_path == issue.file_path)
        {
            *existing = issue;
        } else {
            inner.issues.push(issue);
        }
    }

    pub(super) fn record_retry_file(
        &self,
        path: &str,
        identity: Option<super::verified_record::FileIdentity>,
        manifest_signature: String,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.retry_files.insert(
                path.to_owned(),
                RetryFileEvidence {
                    path: path.to_owned(),
                    identity,
                    manifest_signature,
                },
            );
        }
    }

    pub(crate) fn set_retry_database(&self, db: FoxyDb) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.retry_db = Some(db);
        }
    }

    pub(crate) fn issues_snapshot(&self) -> Vec<HashIssue> {
        self.inner
            .lock()
            .map(|inner| inner.issues.clone())
            .unwrap_or_default()
    }

    pub(crate) fn clear_repaired_missing_files(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .issues
                .retain(|issue| issue.kind != HashIssueKind::Missing);
        }
    }

    pub(crate) fn is_incomplete(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .issues
                    .iter()
                    .any(|issue| issue.kind != HashIssueKind::Missing)
            })
            .unwrap_or(true)
    }

    pub(crate) fn set_unattended(&self, unattended: bool) {
        self.unattended
            .store(unattended, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn is_unattended(&self) -> bool {
        self.unattended.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn set_retry_save<F, Fut>(&self, retry: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        if let Ok(mut inner) = self.inner.lock() {
            inner.retry_save.push(Arc::new(move || Box::pin(retry())));
        }
    }

    pub(crate) fn has_retry_save_pending(&self) -> bool {
        self.inner
            .lock()
            .map(|inner| !inner.retry_save.is_empty())
            .unwrap_or(false)
    }

    pub(crate) async fn retry_save_pending(&self) -> Result<bool, String> {
        let (retries, db, files) = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| "Hash evidence state is unavailable".to_owned())?;
            (
                inner.retry_save.clone(),
                inner.retry_db.clone(),
                inner.retry_files.values().cloned().collect::<Vec<_>>(),
            )
        };
        if retries.is_empty() {
            return Ok(false);
        }
        let db =
            db.ok_or_else(|| "Verification context expired; run the check again".to_owned())?;
        if files.is_empty() || !retry_evidence_is_current(&db, &files).await {
            return Err("Files or accepted manifest changed; run the check again".to_owned());
        }
        for retry in &retries {
            retry().await?;
        }
        if let Ok(mut inner) = self.inner.lock() {
            inner.retry_save.clear();
            inner
                .issues
                .retain(|issue| issue.kind != HashIssueKind::SaveFailed);
        }
        Ok(true)
    }

    pub(crate) fn begin_stalled_read(
        &self,
        read: StalledRead,
    ) -> watch::Receiver<StalledReadDecision> {
        let (tx, rx) = watch::channel(StalledReadDecision::Waiting);
        if let Ok(mut inner) = self.inner.lock() {
            inner.stalled.insert(read.file_path.clone(), (read, tx));
        }
        rx
    }

    pub(crate) fn stalled_snapshot(&self) -> Vec<StalledRead> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .stalled
                    .values()
                    .map(|(read, _)| read.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn respond_to_stall(&self, file_path: &str, decision: StalledReadDecision) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.stalled.get(file_path).map(|(_, tx)| tx.clone()))
            .is_some_and(|tx| tx.send(decision).is_ok())
    }

    pub(crate) fn finish_stalled_read(&self, file_path: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.stalled.remove(file_path);
        }
    }
}

async fn retry_evidence_is_current(db: &FoxyDb, files: &[RetryFileEvidence]) -> bool {
    for file in files {
        match (
            &file.identity,
            super::verified_record::capture_identity_async(&file.path).await,
        ) {
            (Some(expected), Some(current)) if *expected == current => {}
            (None, None) => {
                if !tokio::fs::metadata(&file.path)
                    .await
                    .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
                {
                    return false;
                }
            }
            _ => return false,
        }
        let Ok(rows) = db
            .query_all(
                "SELECT id, remote_checksum FROM files WHERE local_path = ?",
                vec![DbValue::from(file.path.clone())],
            )
            .await
        else {
            return false;
        };
        if rows.is_empty() {
            return false;
        }
        for row in rows {
            let (Ok(file_id), Ok(remote_checksum)) =
                (row.get_i64("id"), row.get_string("remote_checksum"))
            else {
                return false;
            };
            let Ok(parts) = db
                .query_all(
                    "SELECT id, file_id, path, remote_length, local_length, remote_start, local_start, remote_checksum, local_checksum, data_order FROM subfiles WHERE file_id = ? ORDER BY data_order",
                    vec![DbValue::from(file_id)],
                )
                .await
            else {
                return false;
            };
            let Ok(parts) = parts
                .iter()
                .map(crate::core::models::modification_file_part::FoxyModFilePart::from_row)
                .collect::<Result<Vec<_>, _>>()
            else {
                return false;
            };
            let signature = super::verified_record::parts_signature(&remote_checksum, parts.iter());
            if signature != file.manifest_signature {
                return false;
            }
        }
    }
    true
}

impl std::fmt::Debug for HashEvidenceState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HashEvidenceState")
            .field("issues", &self.issues_snapshot().len())
            .field("stalled_reads", &self.stalled_snapshot().len())
            .field("retry_save_pending", &self.has_retry_save_pending())
            .field("unattended", &self.is_unattended())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn incomplete_excludes_missing_but_retry_save_requires_revalidated_evidence() {
        let state = HashEvidenceState::default();
        state.record_issue(HashIssue {
            file_path: "R:/Mods/@mod_01/missing.bin".to_owned(),
            kind: HashIssueKind::Missing,
            detail: None,
        });
        assert!(!state.is_incomplete());

        state.record_issue(HashIssue {
            file_path: "R:/Mods/@mod_01/unreadable.bin".to_owned(),
            kind: HashIssueKind::SaveFailed,
            detail: None,
        });
        state.set_retry_save(|| async { Ok(()) });
        assert!(state.is_incomplete());
        assert!(state.has_retry_save_pending());
        assert!(state.retry_save_pending().await.is_err());
        assert!(state.has_retry_save_pending());
        assert!(state.is_incomplete());
    }

    #[tokio::test]
    async fn failed_retry_does_not_persist_parent_evidence_and_can_be_retried() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .join("file.bin")
            .to_string_lossy()
            .into_owned();
        std::fs::write(&path, b"data").unwrap();
        let db = FoxyDb::from_turso(crate::core::tasks::db_turso::build_test_database().await);
        db.execute("INSERT INTO files (name, remote_path, local_path, remote_checksum) VALUES ('file.bin', 'https://repo.example.invalid/file.bin', ?, 'REMOTE')", vec![DbValue::from(path.clone())]).await.unwrap();
        let state = HashEvidenceState::default();
        state.set_retry_database(db);
        state.record_retry_file(
            &path,
            super::super::verified_record::capture_identity_async(&path).await,
            super::super::verified_record::parts_signature("REMOTE", std::iter::empty()),
        );
        let fail = Arc::new(AtomicBool::new(true));
        let child_fail = fail.clone();
        state.set_retry_save(move || {
            let fail = child_fail.clone();
            async move {
                if fail.load(Ordering::Relaxed) {
                    Err("child write failed".into())
                } else {
                    Ok(())
                }
            }
        });
        let parent_writes = Arc::new(AtomicUsize::new(0));
        let writes = parent_writes.clone();
        state.set_retry_save(move || {
            let writes = writes.clone();
            async move {
                writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        });
        assert!(state.retry_save_pending().await.is_err());
        assert_eq!(parent_writes.load(Ordering::Relaxed), 0);
        assert!(state.has_retry_save_pending());
        fail.store(false, Ordering::Relaxed);
        assert_eq!(state.retry_save_pending().await, Ok(true));
        assert_eq!(parent_writes.load(Ordering::Relaxed), 1);
        assert!(!state.has_retry_save_pending());
    }

    #[tokio::test]
    async fn stalled_read_decisions_are_visible_to_worker() {
        let state = HashEvidenceState::default();
        let mut decision = state.begin_stalled_read(StalledRead {
            file_path: "R:/Mods/@mod_01/large.bin".to_owned(),
            no_progress_secs: 90,
        });
        assert_eq!(state.stalled_snapshot().len(), 1);
        assert!(state.respond_to_stall("R:/Mods/@mod_01/large.bin", StalledReadDecision::Skip));
        assert_eq!(*decision.borrow_and_update(), StalledReadDecision::Skip);
        state.finish_stalled_read("R:/Mods/@mod_01/large.bin");
        assert!(state.stalled_snapshot().is_empty());
    }

    #[test]
    fn completed_repair_clears_missing_findings_without_hiding_read_failures() {
        let state = HashEvidenceState::default();
        for (path, kind) in [
            ("missing.bin", HashIssueKind::Missing),
            ("locked.bin", HashIssueKind::Unreadable),
        ] {
            state.record_issue(HashIssue {
                file_path: path.into(),
                kind,
                detail: None,
            });
        }
        state.clear_repaired_missing_files();
        assert_eq!(state.issues_snapshot().len(), 1);
        assert_eq!(state.issues_snapshot()[0].kind, HashIssueKind::Unreadable);
        assert!(state.is_incomplete());
    }
}
