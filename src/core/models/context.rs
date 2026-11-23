use crate::core::db::{DbHandle, FoxyDb};
use crate::core::models::recheck_level::RecheckLevel;
use reqwest::Client;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct StagedManifestResponse {
    pub(crate) value: serde_json::Value,
    pub(crate) timing: crate::core::utils::fetch_json::FetchJsonTiming,
}

#[derive(Clone)]
pub(crate) struct DeferredPartInsert {
    pub(crate) file_id: i64,
    pub(crate) path: String,
    pub(crate) remote_length: i64,
    pub(crate) remote_start: i64,
    pub(crate) remote_checksum: String,
    pub(crate) data_order: i64,
}

#[derive(Clone)]
pub(crate) struct FoxyContext {
    pub(crate) database: DbHandle,
    pub(crate) client: Client,
    pub(crate) recheck_level: RecheckLevel,
    pub(crate) forced_mod_refreshes: HashSet<String>,
    /// Revalidation keeps unchanged part graphs; forced refresh instead refetches the remote manifest.
    pub(crate) reapplied_mod_manifests: HashSet<String>,
    pub(crate) queue_download_targets: bool,
    pub(crate) patch_plan_metadata_refresh: bool,
    pub(crate) force_download_targets: bool,
    pub(crate) force_full_downloads: bool,
    pub(crate) target_local_path: Option<String>,
    pub(crate) repository_space_shared_path: Option<String>,
    /// Carry the initiating action's op_id so saved benchmarks cannot claim another action's SOL records.
    pub(crate) operation_id: Option<Arc<str>>,
    transient_metadata_failure: Arc<AtomicBool>,
    /// When the table starts empty, use plain inserts into its index-deferred state.
    /// Shared across spawned mod tasks; defaults to the safe `ON CONFLICT` path.
    pub(crate) fresh_subfiles_load: Arc<AtomicBool>,
    /// Empty part rows do not prove addon links are fresh; retain a separate repository-link freshness flag.
    fresh_addon_file_links: Arc<AtomicBool>,
    /// When set, part rows are buffered for a background flush that can overlap
    /// downloads. Shared across spawned tasks and context clones.
    pub(crate) defer_part_inserts: Arc<AtomicBool>,
    /// Buffer of part rows staged by the deferred-insert path; drained once by the
    /// background flush task (awaited before the incremental hasher loads its tree).
    pub(crate) deferred_part_inserts: Arc<Mutex<Vec<DeferredPartInsert>>>,
    /// Metadata fan-out clears fresh-load mode before deferred flushing, so buffered rows retain their own marker.
    pub(crate) deferred_part_inserts_fresh_load: Arc<AtomicBool>,
    /// Retain committed deferred rows for the in-memory tree; later flushes must skip that committed prefix.
    deferred_parts_persisted: Arc<AtomicUsize>,

    stream_part_inserts: Arc<AtomicBool>,
    /// Held while one group is applied and written, so the rows it appends to
    /// the deferred buffer form one contiguous range.
    part_stream_lock: Arc<tokio::sync::Mutex<()>>,
    pending_addon_file_links: Arc<Mutex<Vec<(i64, i64)>>>,
    pending_download_targets: Arc<Mutex<Vec<PendingDownloadTarget>>>,
    pending_patch_clear_ids: Arc<Mutex<Vec<i64>>>,
    remote_addon_empty_states: Arc<Mutex<HashMap<String, bool>>>,
    staged_manifest_responses: Arc<Mutex<HashMap<String, StagedManifestResponse>>>,
    /// Reuse same-pass fingerprints while their files are cached instead of paying cold-disk sampling seeks.
    fresh_file_content_hashes: Arc<Mutex<HashMap<u64, String>>>,

    pub(crate) verified_hash_record:
        Option<crate::core::tasks::calculate_hashes::VerifiedHashRecordUse>,

    pub(crate) manifest_cache: Option<Arc<crate::core::utils::manifest_cache::ManifestCache>>,
    pub(crate) hash_evidence: Arc<crate::core::tasks::calculate_hashes::HashEvidenceState>,
    hash_cancel_rx: Option<watch::Receiver<bool>>,
    pinned_hash_targets:
        Arc<Mutex<Option<Arc<crate::core::tasks::download_files::PinnedDownloadTargets>>>>,
}

#[derive(Clone)]
pub(crate) struct PendingDownloadTarget {
    pub(crate) file_id: i64,
    pub(crate) download_remote_url: String,
    pub(crate) download_local_path: String,
    pub(crate) size: i64,
}

impl FoxyContext {
    pub(crate) fn new(database: DbHandle, client: Client) -> Self {
        let hash_evidence =
            Arc::new(crate::core::tasks::calculate_hashes::HashEvidenceState::default());
        hash_evidence.set_retry_database(FoxyDb::from_handle(database.clone()));
        FoxyContext {
            database,
            client,
            recheck_level: RecheckLevel::DEFAULT,
            forced_mod_refreshes: HashSet::new(),
            reapplied_mod_manifests: HashSet::new(),
            queue_download_targets: true,
            patch_plan_metadata_refresh: false,
            force_download_targets: false,
            force_full_downloads: false,
            target_local_path: None,
            repository_space_shared_path: None,
            operation_id: None,
            transient_metadata_failure: Arc::new(AtomicBool::new(false)),
            fresh_subfiles_load: Arc::new(AtomicBool::new(false)),
            fresh_addon_file_links: Arc::new(AtomicBool::new(false)),
            defer_part_inserts: Arc::new(AtomicBool::new(false)),
            deferred_part_inserts: Arc::new(Mutex::new(Vec::new())),
            deferred_part_inserts_fresh_load: Arc::new(AtomicBool::new(false)),
            deferred_parts_persisted: Arc::new(AtomicUsize::new(0)),
            stream_part_inserts: Arc::new(AtomicBool::new(false)),
            part_stream_lock: Arc::new(tokio::sync::Mutex::new(())),
            pending_addon_file_links: Arc::new(Mutex::new(Vec::new())),
            pending_download_targets: Arc::new(Mutex::new(Vec::new())),
            pending_patch_clear_ids: Arc::new(Mutex::new(Vec::new())),
            remote_addon_empty_states: Arc::new(Mutex::new(HashMap::new())),
            staged_manifest_responses: Arc::new(Mutex::new(HashMap::new())),
            fresh_file_content_hashes: Arc::new(Mutex::new(HashMap::new())),
            verified_hash_record: None,
            manifest_cache: None,
            hash_evidence,
            hash_cancel_rx: None,
            pinned_hash_targets: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn pin_hash_targets(
        &self,
        targets: Arc<crate::core::tasks::download_files::PinnedDownloadTargets>,
    ) {
        if let Ok(mut pinned) = self.pinned_hash_targets.lock() {
            *pinned = Some(targets);
        }
    }

    pub(crate) fn hash_target_path(&self, path: &str) -> Result<String, String> {
        let targets = self
            .pinned_hash_targets
            .lock()
            .map_err(|_| "Write target state is unavailable".to_owned())?
            .clone();
        match targets {
            Some(targets) => targets
                .resolve(std::path::Path::new(path))
                .map(|path| path.to_string_lossy().into_owned()),
            None => Ok(path.to_owned()),
        }
    }

    pub(crate) fn record_fresh_file_content_hashes(
        &self,
        hashes: impl IntoIterator<Item = (u64, String)>,
    ) {
        if let Ok(mut fresh) = self.fresh_file_content_hashes.lock() {
            fresh.extend(hashes);
        }
    }

    /// Take the fingerprint a hash pass recorded for `file_id`, if any. Taking
    /// it means a second refresh in the same operation samples the disk again.
    pub(crate) fn take_fresh_file_content_hash(&self, file_id: u64) -> Option<String> {
        self.fresh_file_content_hashes
            .lock()
            .ok()
            .and_then(|mut fresh| fresh.remove(&file_id))
    }

    pub(crate) fn set_defer_part_inserts(&self, value: bool) {
        self.defer_part_inserts.store(value, Ordering::Relaxed);
    }

    pub(crate) fn should_defer_part_inserts(&self) -> bool {
        self.defer_part_inserts.load(Ordering::Relaxed)
    }

    pub(crate) fn buffer_deferred_parts(&self, rows: Vec<DeferredPartInsert>) {
        if rows.is_empty() {
            return;
        }
        if self.is_fresh_subfiles_load() {
            self.set_deferred_part_inserts_fresh_load(true);
        }
        if let Ok(mut buffer) = self.deferred_part_inserts.lock() {
            buffer.extend(rows);
        }
    }

    pub(crate) fn take_deferred_parts(&self) -> Vec<DeferredPartInsert> {
        let rows = self
            .deferred_part_inserts
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default();
        self.deferred_parts_persisted.store(0, Ordering::Relaxed);
        rows
    }

    pub(crate) fn take_unpersisted_deferred_parts(&self) -> (Vec<DeferredPartInsert>, usize) {
        let persisted = self.deferred_parts_persisted.load(Ordering::Relaxed);
        let mut rows = self.take_deferred_parts();
        let persisted = persisted.min(rows.len());
        rows.drain(..persisted);
        (rows, persisted)
    }

    pub(crate) fn deferred_parts_persisted(&self) -> usize {
        self.deferred_parts_persisted.load(Ordering::Relaxed)
    }

    pub(crate) fn set_deferred_parts_persisted(&self, rows: usize) {
        self.deferred_parts_persisted.store(rows, Ordering::Relaxed);
    }

    pub(crate) fn deferred_parts_from(&self, start: usize) -> Vec<DeferredPartInsert> {
        self.deferred_part_inserts
            .lock()
            .map(|buffer| buffer.get(start..).map(<[_]>::to_vec).unwrap_or_default())
            .unwrap_or_default()
    }

    pub(crate) fn set_stream_part_inserts(&self, value: bool) {
        self.stream_part_inserts.store(value, Ordering::Relaxed);
    }

    pub(crate) fn should_stream_part_inserts(&self) -> bool {
        self.stream_part_inserts.load(Ordering::Relaxed)
    }

    pub(crate) async fn lock_part_stream(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.part_stream_lock.lock().await
    }

    pub(crate) fn deferred_part_count(&self) -> usize {
        self.deferred_part_inserts
            .lock()
            .map(|buffer| buffer.len())
            .unwrap_or(0)
    }

    pub(crate) fn deferred_parts_snapshot(&self) -> Vec<DeferredPartInsert> {
        self.deferred_part_inserts
            .lock()
            .map(|buffer| buffer.clone())
            .unwrap_or_default()
    }

    pub(crate) fn is_fresh_subfiles_load(&self) -> bool {
        self.fresh_subfiles_load.load(Ordering::Relaxed)
    }

    pub(crate) fn set_fresh_subfiles_load(&self, value: bool) {
        self.fresh_subfiles_load.store(value, Ordering::Relaxed);
    }

    pub(crate) fn has_fresh_addon_file_links(&self) -> bool {
        self.fresh_addon_file_links.load(Ordering::Relaxed)
    }

    pub(crate) fn set_fresh_addon_file_links(&self, value: bool) {
        self.fresh_addon_file_links.store(value, Ordering::Relaxed);
    }

    pub(crate) fn deferred_part_inserts_are_fresh_load(&self) -> bool {
        self.deferred_part_inserts_fresh_load
            .load(Ordering::Relaxed)
    }

    pub(crate) fn set_deferred_part_inserts_fresh_load(&self, value: bool) {
        self.deferred_part_inserts_fresh_load
            .store(value, Ordering::Relaxed);
    }

    pub(crate) fn buffer_addon_file_links(&self, links: impl IntoIterator<Item = (i64, i64)>) {
        let mut links = links.into_iter().peekable();
        if links.peek().is_none() {
            return;
        }
        if let Ok(mut buffer) = self.pending_addon_file_links.lock() {
            buffer.extend(links);
        }
    }

    pub(crate) fn take_pending_addon_file_links(&self) -> Vec<(i64, i64)> {
        self.pending_addon_file_links
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
    }

    pub(crate) fn buffer_download_targets(
        &self,
        rows: impl IntoIterator<Item = PendingDownloadTarget>,
    ) {
        let mut rows = rows.into_iter().peekable();
        if rows.peek().is_none() {
            return;
        }
        if let Ok(mut buffer) = self.pending_download_targets.lock() {
            buffer.extend(rows);
        }
    }

    pub(crate) fn take_pending_download_targets(&self) -> Vec<PendingDownloadTarget> {
        self.pending_download_targets
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
    }

    pub(crate) fn buffer_patch_clear_ids(&self, file_ids: impl IntoIterator<Item = i64>) {
        let mut file_ids = file_ids.into_iter().peekable();
        if file_ids.peek().is_none() {
            return;
        }
        if let Ok(mut buffer) = self.pending_patch_clear_ids.lock() {
            buffer.extend(file_ids);
        }
    }

    pub(crate) fn take_pending_patch_clear_ids(&self) -> Vec<i64> {
        self.pending_patch_clear_ids
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
    }

    pub(crate) fn record_remote_addon_empty(&self, name: &str, empty: bool) {
        if let Ok(mut states) = self.remote_addon_empty_states.lock() {
            states.insert(name.to_owned(), empty);
        }
    }

    pub(crate) fn remote_addon_empty_states(&self) -> HashMap<String, bool> {
        self.remote_addon_empty_states
            .lock()
            .map(|states| states.clone())
            .unwrap_or_default()
    }

    pub(crate) fn stage_manifest_response(&self, url: String, response: StagedManifestResponse) {
        if let Ok(mut staged) = self.staged_manifest_responses.lock() {
            staged.insert(url, response);
        }
    }

    pub(crate) fn staged_manifest_response(&self, url: &str) -> Option<StagedManifestResponse> {
        self.staged_manifest_responses
            .lock()
            .ok()
            .and_then(|staged| staged.get(url).cloned())
    }

    pub(crate) fn clear_staged_manifest_responses(&self) {
        if let Ok(mut staged) = self.staged_manifest_responses.lock() {
            staged.clear();
        }
    }

    pub(crate) fn empty_remote_addon_names(&self) -> HashSet<String> {
        self.remote_addon_empty_states
            .lock()
            .map(|states| {
                states
                    .iter()
                    .filter(|(_, empty)| **empty)
                    .map(|(name, _)| name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn with_forced_mod_refreshes(
        mut self,
        forced_mod_refreshes: HashSet<String>,
    ) -> Self {
        self.forced_mod_refreshes = forced_mod_refreshes;
        self
    }

    pub(crate) fn with_recheck_level(mut self, level: RecheckLevel) -> Self {
        self.recheck_level = level;
        self
    }

    pub(crate) fn with_reapplied_mod_manifests(mut self, addons: HashSet<String>) -> Self {
        self.reapplied_mod_manifests = addons;
        self
    }

    pub(crate) fn with_download_target_queueing(mut self, enabled: bool) -> Self {
        self.queue_download_targets = enabled;
        self
    }

    pub(crate) fn with_patch_plan_metadata_refresh(mut self, enabled: bool) -> Self {
        self.patch_plan_metadata_refresh = enabled;
        self
    }

    pub(crate) fn with_force_download_targets(mut self, enabled: bool) -> Self {
        self.force_download_targets = enabled;
        self
    }

    pub(crate) fn with_force_full_downloads(mut self, enabled: bool) -> Self {
        self.force_full_downloads = enabled;
        self
    }

    pub(crate) fn with_target_local_path(mut self, local_path: impl Into<String>) -> Self {
        self.target_local_path = Some(local_path.into());
        self
    }

    pub(crate) fn with_repository_space_shared_path(mut self, shared_path: Option<String>) -> Self {
        self.repository_space_shared_path = shared_path;
        self
    }

    pub(crate) fn with_verified_hash_record(
        mut self,
        record: crate::core::tasks::calculate_hashes::VerifiedHashRecordUse,
    ) -> Self {
        self.verified_hash_record = Some(record);
        self
    }

    pub(crate) fn with_manifest_cache(
        mut self,
        cache: crate::core::utils::manifest_cache::ManifestCache,
    ) -> Self {
        self.manifest_cache = Some(Arc::new(cache));
        self
    }

    pub(crate) fn with_hash_cancel_rx(mut self, cancel_rx: watch::Receiver<bool>) -> Self {
        self.hash_cancel_rx = Some(cancel_rx);
        self
    }

    pub(crate) fn hash_cancel_rx(&self) -> Option<watch::Receiver<bool>> {
        self.hash_cancel_rx.clone()
    }

    pub(crate) fn clone_for_hash_retry(&self) -> Self {
        let mut retry_context = self.clone();
        retry_context.hash_evidence =
            Arc::new(crate::core::tasks::calculate_hashes::HashEvidenceState::default());
        retry_context
            .hash_evidence
            .set_retry_database(retry_context.db());
        retry_context
    }

    pub(crate) fn with_operation_id(mut self, operation_id: impl Into<Arc<str>>) -> Self {
        self.pinned_hash_targets = Arc::new(Mutex::new(None));
        self.transient_metadata_failure = Arc::new(AtomicBool::new(false));
        self.hash_evidence =
            Arc::new(crate::core::tasks::calculate_hashes::HashEvidenceState::default());
        self.hash_evidence.set_retry_database(self.db());
        self.remote_addon_empty_states = Arc::new(Mutex::new(HashMap::new()));
        self.staged_manifest_responses = Arc::new(Mutex::new(HashMap::new()));
        self.hash_cancel_rx = None;
        self.operation_id = Some(operation_id.into());
        self
    }

    pub(crate) fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    pub(crate) fn mark_transient_metadata_failure(&self) {
        self.transient_metadata_failure
            .store(true, Ordering::Relaxed);
    }

    pub(crate) fn has_transient_metadata_failure(&self) -> bool {
        self.transient_metadata_failure.load(Ordering::Relaxed)
    }

    pub(crate) fn clear_transient_metadata_failure(&self) {
        self.transient_metadata_failure
            .store(false, Ordering::Relaxed);
    }

    pub(crate) fn db(&self) -> FoxyDb {
        FoxyDb::from_handle(self.database.clone())
    }
}

#[cfg(test)]
mod retry_scope_tests {
    use super::FoxyContext;
    use std::sync::Arc;

    #[tokio::test]
    async fn metadata_retry_evidence_is_shared_only_within_one_operation() {
        let database = crate::core::tasks::db_turso::build_test_database().await;
        let base = FoxyContext::new(database, reqwest::Client::new());
        let first = base.clone().with_operation_id("first");
        let fetch = first.clone();
        fetch.mark_transient_metadata_failure();
        first.record_remote_addon_empty("@mod_01", true);
        first
            .hash_evidence
            .record_issue(crate::core::tasks::calculate_hashes::HashIssue {
                file_path: "repo".to_owned(),
                kind: crate::core::tasks::calculate_hashes::HashIssueKind::Unreadable,
                detail: None,
            });
        assert!(first.has_transient_metadata_failure());
        assert_eq!(
            first.remote_addon_empty_states().get("@mod_01"),
            Some(&true)
        );
        assert!(!base.has_transient_metadata_failure());

        let second = first.clone().with_operation_id("second");
        assert!(!second.has_transient_metadata_failure());
        assert!(second.remote_addon_empty_states().is_empty());
        assert!(!second.hash_evidence.is_incomplete());
        assert!(first.has_transient_metadata_failure());
        assert!(first.hash_evidence.is_incomplete());
    }

    #[tokio::test]
    async fn hash_retry_context_does_not_retain_owning_evidence() {
        let database = crate::core::tasks::db_turso::build_test_database().await;
        let context = FoxyContext::new(database, reqwest::Client::new());
        let evidence = context.hash_evidence.clone();
        let weak_evidence = Arc::downgrade(&evidence);
        let retry_context = context.clone_for_hash_retry();

        evidence.set_retry_save(move || {
            let retry_context = retry_context.clone();
            async move {
                drop(retry_context);
                Ok(())
            }
        });

        drop(context);
        drop(evidence);
        assert!(weak_evidence.upgrade().is_none());
    }
}
