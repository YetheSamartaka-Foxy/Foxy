use crate::core::db::{DbHandle, FoxyDb};
use crate::core::models::recheck_level::RecheckLevel;
use reqwest::Client;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// A brand-new `subfiles` row staged for the deferred background insert on the
/// force-redownload path (after_turso_regression_analysis7.md, "A++"). Mirrors the
/// 6 bound columns of the part upsert; the local_* columns use inline defaults.
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
    pub(crate) queue_download_targets: bool,
    pub(crate) patch_plan_metadata_refresh: bool,
    pub(crate) force_download_targets: bool,
    pub(crate) target_local_path: Option<String>,
    pub(crate) repository_space_shared_path: Option<String>,
    /// Set by the metadata rebuild when the `subfiles` table is globally empty at
    /// rebuild start (the post-whole-wipe force-redownload / first-download case),
    /// so the per-mod part insert can use a plain `INSERT` into an index-deferred
    /// table instead of `INSERT … ON CONFLICT` (after_turso_regression_analysis5.md
    /// P0-b). Shared (`Arc`) so the flag set before the parallel fan-out is visible
    /// to every spawned mod task. Default `false` (safe `ON CONFLICT` path).
    pub(crate) fresh_subfiles_load: Arc<AtomicBool>,
    /// A++ (after_turso_regression_analysis7.md): when set (force-redownload), the
    /// per-mod part insert buffers its brand-new rows into `deferred_part_inserts`
    /// instead of writing them inline, so the ~22s 66k-row insert can run as one
    /// background transaction overlapped with the download instead of blocking
    /// `remote_repository`'s critical path. `Arc` so the flag set before the parallel
    /// fan-out is visible to every spawned mod task and survives context clones.
    pub(crate) defer_part_inserts: Arc<AtomicBool>,
    /// Buffer of part rows staged by the deferred-insert path; drained once by the
    /// background flush task (awaited before the incremental hasher loads its tree).
    pub(crate) deferred_part_inserts: Arc<Mutex<Vec<DeferredPartInsert>>>,
}

/// Constructor
impl FoxyContext {
    pub(crate) fn new(database: DbHandle, client: Client) -> Self {
        FoxyContext {
            database,
            client,
            recheck_level: RecheckLevel::DEFAULT,
            forced_mod_refreshes: HashSet::new(),
            queue_download_targets: true,
            patch_plan_metadata_refresh: false,
            force_download_targets: false,
            target_local_path: None,
            repository_space_shared_path: None,
            fresh_subfiles_load: Arc::new(AtomicBool::new(false)),
            defer_part_inserts: Arc::new(AtomicBool::new(false)),
            deferred_part_inserts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Enable/disable the deferred part-insert path for the current sync (set by the
    /// pipeline before `remote_repository` on a force-redownload).
    pub(crate) fn set_defer_part_inserts(&self, value: bool) {
        self.defer_part_inserts.store(value, Ordering::Relaxed);
    }

    /// Whether the per-mod part insert should buffer its rows for the background
    /// flush instead of writing them inline. See [`FoxyContext::defer_part_inserts`].
    pub(crate) fn should_defer_part_inserts(&self) -> bool {
        self.defer_part_inserts.load(Ordering::Relaxed)
    }

    /// Stage brand-new part rows for the background flush.
    pub(crate) fn buffer_deferred_parts(&self, rows: Vec<DeferredPartInsert>) {
        if rows.is_empty() {
            return;
        }
        if let Ok(mut buffer) = self.deferred_part_inserts.lock() {
            buffer.extend(rows);
        }
    }

    /// Drain every staged part row (clears the buffer). Returns them for the
    /// background flush; empty when nothing was deferred.
    pub(crate) fn take_deferred_parts(&self) -> Vec<DeferredPartInsert> {
        self.deferred_part_inserts
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
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

    /// Whether the current metadata rebuild is loading into a globally-empty,
    /// index-deferred `subfiles` table (set by the rebuild, read by the part
    /// insert). See [`FoxyContext::fresh_subfiles_load`].
    pub(crate) fn is_fresh_subfiles_load(&self) -> bool {
        self.fresh_subfiles_load.load(Ordering::Relaxed)
    }

    /// Mark (or clear) the index-deferred fresh-load mode for the current rebuild.
    pub(crate) fn set_fresh_subfiles_load(&self, value: bool) {
        self.fresh_subfiles_load.store(value, Ordering::Relaxed);
    }

    pub(crate) fn with_forced_mod_refreshes(
        mut self,
        forced_mod_refreshes: HashSet<String>,
    ) -> Self {
        self.forced_mod_refreshes = forced_mod_refreshes;
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

    pub(crate) fn with_target_local_path(mut self, local_path: impl Into<String>) -> Self {
        self.target_local_path = Some(local_path.into());
        self
    }

    pub(crate) fn with_repository_space_shared_path(mut self, shared_path: Option<String>) -> Self {
        self.repository_space_shared_path = shared_path;
        self
    }

    /// Storage-neutral DB handle for the seam (plan.md §5.1). Converted call
    /// sites use `context.db()` instead of `context.database.as_ref()`. The arm
    /// returned follows the active storage feature: SeaORM by default, Turso
    /// under `--features turso` (the Phase-2 cutover wiring).
    pub(crate) fn db(&self) -> FoxyDb {
        FoxyDb::from_handle(self.database.clone())
    }
}
