use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crate::ui::app::Foxy;

#[derive(Clone, Debug, PartialEq, Eq)]
/// Results are valid only for the captured game space and worker generation.
pub(crate) struct WorkerScope {
    space_id: String,
    generation: u64,
}

pub(crate) struct WorkerGeneration(Option<u64>);

impl Default for WorkerGeneration {
    fn default() -> Self {
        Self(Some(1))
    }
}

impl WorkerGeneration {
    pub(crate) fn advance(&mut self) {
        self.0 = self.0.and_then(|generation| generation.checked_add(1));
    }

    pub(crate) fn capture(&self, space_id: String) -> Option<WorkerScope> {
        self.0.map(|generation| WorkerScope {
            space_id,
            generation,
        })
    }

    pub(crate) fn accepts(&self, active_space: &str, scope: &WorkerScope) -> bool {
        self.accepts_parts(active_space, &scope.space_id, scope.generation)
    }

    pub(crate) fn accepts_parts(
        &self,
        active_space: &str,
        space_id: &str,
        generation: u64,
    ) -> bool {
        self.0 == Some(generation) && active_space == space_id
    }

    #[cfg(test)]
    pub(crate) fn from_counter(generation: u64) -> Self {
        Self(Some(generation))
    }
}

impl WorkerScope {
    pub(crate) fn space_id(&self) -> &str {
        &self.space_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Close admission before a game-space handoff; workers already admitted must join before database ownership is released.
#[derive(Clone, Debug)]
pub(crate) struct DbAdmission(Arc<AtomicBool>);

impl Default for DbAdmission {
    fn default() -> Self {
        Self::open()
    }
}

impl DbAdmission {
    pub(crate) fn open() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    pub(crate) fn allows(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn close(&self) {
        self.0.store(false, Ordering::Release);
    }

    pub(crate) fn resume(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Return no database handle after handoff closes admission.
pub(crate) fn admit_database_section(admission: &DbAdmission) -> bool {
    admission.allows()
}

pub(crate) fn image_cache_file(directory: &Path, checksum_hex: &str) -> PathBuf {
    directory.join(format!("{checksum_hex}.png"))
}

/// A stale result must not consume a newer join-preflight request.
pub(crate) fn take_current_preflight<T>(
    generation: &WorkerGeneration,
    active_space: &str,
    scope_space: &str,
    scope_generation: u64,
    pending: &mut Option<T>,
) -> Option<T> {
    if !generation.accepts_parts(active_space, scope_space, scope_generation) {
        return None;
    }
    pending.take()
}

pub(crate) fn current_worker_result(
    generation: &WorkerGeneration,
    active_space: &str,
    scope_space: &str,
    scope_generation: u64,
) -> bool {
    generation.accepts_parts(active_space, scope_space, scope_generation)
}

pub(crate) fn reap_worker(worker: &mut Option<JoinHandle<()>>, failed: &str) {
    let finished = worker.as_ref().is_some_and(|handle| handle.is_finished());
    if !finished {
        return;
    }
    if let Some(handle) = worker.take()
        && handle.join().is_err()
    {
        log::warn!("{failed}");
    }
}

pub(crate) fn reap_tagged_worker(worker: &mut Option<(u64, JoinHandle<()>)>, failed: &str) {
    let finished = worker
        .as_ref()
        .is_some_and(|(_, handle)| handle.is_finished());
    if !finished {
        return;
    }
    if let Some((_, handle)) = worker.take()
        && handle.join().is_err()
    {
        log::warn!("{failed}");
    }
}

pub(crate) fn reap_workers(workers: &mut Vec<JoinHandle<()>>, failed: &str) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            if workers.swap_remove(index).join().is_err() {
                log::warn!("{failed}");
            }
        } else {
            index += 1;
        }
    }
}

#[cfg(test)]
pub(crate) struct RetainedClientWorkers {
    pub queries: Vec<JoinHandle<()>>,
    pub preflight: Option<JoinHandle<()>>,
    pub mission: Option<JoinHandle<()>>,
    pub images: Vec<JoinHandle<()>>,
}

#[cfg(test)]
impl RetainedClientWorkers {
    pub(crate) fn reap_finished(&mut self) {
        reap_workers(&mut self.queries, "Server query worker failed.");
        reap_worker(&mut self.preflight, "Join preflight worker failed.");
        reap_worker(&mut self.mission, "Mission scan worker failed.");
        reap_workers(&mut self.images, "Image worker failed.");
    }

    pub(crate) fn is_drained(&self) -> bool {
        self.queries.is_empty()
            && self.preflight.is_none()
            && self.mission.is_none()
            && self.images.is_empty()
    }
}

/// This barrier joins only its listed workers before database release; other workers remain outside it.
#[cfg(test)]
pub(crate) const HANDOFF_RETAINED_HANDLES: &[&str] = &[
    "fs_watch_worker",
    "backup_cleanup_worker",
    "startup_recovery_rx",
    "pending_queries",
    "join_preflight_worker",
    "mission_scan_worker",
    "image_workers",
    "startup_pending_restore_worker",
    "startup_quick_scan_filter_worker",
    "startup_probe_retry_worker",
    "cached_update_load_workers",
    "quick_scan_worker",
    "backend_worker",
    "repository_settings_addon_preload_worker",
    "maintenance_workers",
    "force_redownload_workers",
    "addon_hash_recalc_worker",
    "repo_metadata_workers",
    "repository_space_freshness_worker",
    "repository_addon_size_load_worker",
    "direct_download_worker",
    "workshop_task_worker",
    "database_wipe_worker",
    "addon_backup_worker",
    "pending_update_clear_workers",
    "overview_worker",
    "ts3_plugin_scan_worker",
    "repository_space_import_worker",
    "benchmark_workers",
];

impl Foxy {
    pub(crate) fn capture_worker_scope(&self) -> Option<WorkerScope> {
        if !self.client_db_admission.allows() {
            return None;
        }
        self.server_query_generation
            .capture(crate::core::game::spaces::active_game_space().space_id)
    }

    pub(in crate::ui::app) fn begin_client_worker_handoff(&mut self) {
        if self.client_worker_handoff_active {
            return;
        }
        self.client_db_admission.close();
        self.client_worker_handoff_active = true;
        self.server_query_generation.advance();
        if let Some(stop) = self.fs_watch_stop.as_ref() {
            stop.store(true, Ordering::Release);
        }
    }

    pub(in crate::ui::app) fn client_worker_handoff_ready(&mut self) -> bool {
        self.reap_client_worker_handles();
        self.startup_recovery_rx.is_none()
            && !self.profile_backup_work_in_flight()
            && self.fs_watch_worker.is_none()
            && self.backup_cleanup_worker.is_none()
            && self.pending_queries.is_empty()
            && self.join_preflight_worker.is_none()
            && self.mission_scan_worker.is_none()
            && self.image_workers.is_empty()
            && self.startup_pending_restore_worker.is_none()
            && self.startup_quick_scan_filter_worker.is_none()
            && self.startup_probe_retry_worker.is_none()
            && self.cached_update_load_workers.is_empty()
            && self.quick_scan_worker.is_none()
            && self.backend_worker.is_none()
            && self.repository_settings_addon_preload_worker.is_none()
            && self.maintenance_workers.is_empty()
            && self.force_redownload_workers.is_empty()
            && self.addon_hash_recalc_worker.is_none()
            && self.repo_metadata_workers.is_empty()
            && self.repository_space_freshness_worker.is_none()
            && self.repository_addon_size_load_worker.is_none()
            && self.direct_download_worker.is_none()
            && self.workshop_task_worker.is_none()
            && self.database_wipe_worker.is_none()
            && self.addon_backup_worker.is_none()
            && self.pending_update_clear_workers.is_empty()
            && self.overview_worker.is_none()
            && self.ts3_plugin_scan_worker.is_none()
            && self.repository_space_import_worker.is_none()
            && self.benchmark_workers.is_empty()
    }

    pub(crate) fn scoped_result_applies(&self, space_id: &str, generation: u64) -> bool {
        let active_space = crate::core::game::spaces::active_game_space().space_id;
        current_worker_result(
            &self.server_query_generation,
            &active_space,
            space_id,
            generation,
        )
    }

    pub(in crate::ui::app) fn end_client_worker_handoff(&mut self) {
        self.client_db_admission.resume();
        self.client_worker_handoff_active = false;
    }

    pub(in crate::ui::app) fn reap_client_worker_handles(&mut self) {
        reap_worker(
            &mut self.fs_watch_worker,
            "Filesystem watcher worker failed.",
        );
        reap_worker(
            &mut self.backup_cleanup_worker,
            "Backup cleanup worker failed.",
        );
        reap_workers(&mut self.pending_queries, "Server query worker failed.");
        reap_worker(
            &mut self.join_preflight_worker,
            "Join preflight worker failed.",
        );
        reap_worker(&mut self.mission_scan_worker, "Mission scan worker failed.");
        reap_workers(&mut self.image_workers, "Image worker failed.");
        reap_worker(
            &mut self.startup_pending_restore_worker,
            "Startup pending-update restore worker failed.",
        );
        reap_worker(
            &mut self.startup_quick_scan_filter_worker,
            "Startup quick scan eligibility worker failed.",
        );
        reap_worker(
            &mut self.startup_probe_retry_worker,
            "Startup remote probe retry worker failed.",
        );
        reap_workers(
            &mut self.cached_update_load_workers,
            "Cached update load worker failed.",
        );
        reap_worker(&mut self.quick_scan_worker, "Quick scan worker failed.");
        reap_worker(&mut self.backend_worker, "Repository sync worker failed.");
        reap_tagged_worker(
            &mut self.repository_settings_addon_preload_worker,
            "Repository settings addon preload worker failed.",
        );
        reap_workers(
            &mut self.maintenance_workers,
            "Repository maintenance worker failed.",
        );
        reap_workers(
            &mut self.force_redownload_workers,
            "Addon force redownload probe failed.",
        );
        reap_worker(
            &mut self.addon_hash_recalc_worker,
            "Addon hash recalculation worker failed.",
        );
        reap_workers(
            &mut self.repo_metadata_workers,
            "Repository metadata worker failed.",
        );
        reap_worker(
            &mut self.repository_space_freshness_worker,
            "Repository space freshness worker failed.",
        );
        reap_worker(
            &mut self.repository_addon_size_load_worker,
            "Repository addon size worker failed.",
        );
        reap_worker(
            &mut self.direct_download_worker,
            "Direct download worker failed.",
        );
        reap_worker(
            &mut self.workshop_task_worker,
            "Workshop task worker failed.",
        );
        reap_worker(
            &mut self.database_wipe_worker,
            "Database wipe worker failed.",
        );
        reap_worker(&mut self.addon_backup_worker, "Addon backup worker failed.");
        reap_workers(
            &mut self.pending_update_clear_workers,
            "Pending-update clear worker failed.",
        );
        reap_worker(
            &mut self.overview_worker,
            "Game space overview worker failed.",
        );
        reap_worker(
            &mut self.ts3_plugin_scan_worker,
            "TeamSpeak plugin scan worker failed.",
        );
        reap_worker(
            &mut self.repository_space_import_worker,
            "Repository space import worker failed.",
        );
        reap_workers(&mut self.benchmark_workers, "Benchmark worker failed.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn stale_preflight_results_do_not_consume_a_newer_request() {
        let mut generation = WorkerGeneration::default();
        let old = generation.capture("alpha".into()).unwrap();
        generation.advance();
        let current = generation.capture("alpha".into()).unwrap();
        let mut pending = Some("new-request");
        assert!(
            take_current_preflight(
                &generation,
                "alpha",
                old.space_id(),
                old.generation(),
                &mut pending,
            )
            .is_none()
        );
        assert_eq!(pending, Some("new-request"));
        assert_eq!(
            take_current_preflight(
                &generation,
                "alpha",
                current.space_id(),
                current.generation(),
                &mut pending,
            ),
            Some("new-request")
        );
        assert!(pending.is_none());
    }

    #[test]
    fn mission_and_image_results_require_the_current_space_and_generation() {
        let mut generation = WorkerGeneration::default();
        let alpha = generation.capture("alpha".into()).unwrap();
        assert!(current_worker_result(
            &generation,
            "alpha",
            alpha.space_id(),
            alpha.generation(),
        ));
        let (tx, rx) = mpsc::channel();
        tx.send(alpha.clone()).unwrap();
        generation.advance();
        let buffered = rx.recv().unwrap();
        assert!(!current_worker_result(
            &generation,
            "alpha",
            buffered.space_id(),
            buffered.generation(),
        ));
        assert!(!current_worker_result(
            &generation,
            "alpha",
            alpha.space_id(),
            alpha.generation(),
        ));
        let beta = generation.capture("beta".into()).unwrap();
        assert!(!current_worker_result(
            &generation,
            "beta",
            alpha.space_id(),
            alpha.generation(),
        ));
        assert!(current_worker_result(
            &generation,
            "beta",
            beta.space_id(),
            beta.generation(),
        ));
        generation.advance();
        assert!(!current_worker_result(
            &generation,
            "alpha",
            alpha.space_id(),
            alpha.generation(),
        ));
    }

    #[test]
    fn image_cache_files_stay_in_the_directory_captured_at_dispatch() {
        let captured = Path::new("games/alpha/images");
        let active_after_switch = Path::new("games/beta/images");
        let file = image_cache_file(captured, "abc");
        assert_eq!(file, PathBuf::from("games/alpha/images/abc.png"));
        assert_ne!(file.parent(), Some(active_after_switch));
    }

    #[test]
    fn closed_admission_skips_the_database_section_and_join_releases_it() {
        let admission = DbAdmission::open();
        let users = Arc::new(AtomicUsize::new(0));
        let (release, wait) = mpsc::channel();
        let admission_for_worker = admission.clone();
        let users_for_worker = Arc::clone(&users);
        let worker = std::thread::spawn(move || {
            wait.recv().unwrap();
            if !admit_database_section(&admission_for_worker) {
                return;
            }
            users_for_worker.fetch_add(1, Ordering::SeqCst);
            users_for_worker.fetch_sub(1, Ordering::SeqCst);
        });
        let mut retained = RetainedClientWorkers {
            queries: Vec::new(),
            preflight: Some(worker),
            mission: None,
            images: Vec::new(),
        };
        admission.close();
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !retained.is_drained() {
            assert!(std::time::Instant::now() < deadline, "worker did not exit");
            retained.reap_finished();
            if !retained.is_drained() {
                std::thread::yield_now();
            }
        }
        assert_eq!(users.load(Ordering::SeqCst), 0);
        assert!(!admission.allows());
    }

    #[test]
    fn an_entered_database_section_blocks_release_until_it_is_joined() {
        let admission = DbAdmission::open();
        let users = Arc::new(AtomicUsize::new(0));
        let (entered, entered_wait) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let users_for_worker = Arc::clone(&users);
        let worker = std::thread::spawn(move || {
            users_for_worker.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            wait.recv().unwrap();
            users_for_worker.fetch_sub(1, Ordering::SeqCst);
        });
        let mut retained = RetainedClientWorkers {
            queries: Vec::new(),
            preflight: Some(worker),
            mission: None,
            images: Vec::new(),
        };
        entered_wait.recv().unwrap();
        admission.close();
        retained.reap_finished();
        assert!(!retained.is_drained());
        assert_eq!(users.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !retained.is_drained() {
            assert!(std::time::Instant::now() < deadline, "worker did not exit");
            retained.reap_finished();
            if !retained.is_drained() {
                std::thread::yield_now();
            }
        }
        assert_eq!(users.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn space_switch_drains_retained_client_workers_before_database_release() {
        let source = include_str!("game_space_switch.rs");
        let body = source
            .split_once("fn process_pending_game_space_switch")
            .expect("switch wait")
            .1
            .split_once("fn finish_game_space_switch")
            .expect("finish follows the wait")
            .0;
        let ready = body
            .find("client_worker_handoff_ready")
            .expect("handoff wait");
        let release = body
            .find("finish_game_space_switch")
            .expect("database release call");
        assert!(ready < release);
        for field in HANDOFF_RETAINED_HANDLES {
            assert!(
                include_str!("../mod.rs").contains(&format!("{field}:")),
                "missing retained handle {field}"
            );
        }
        let workers = include_str!("client_workers.rs");
        let begin = workers
            .split_once("fn begin_client_worker_handoff")
            .expect("handoff begin")
            .1
            .split_once("fn client_worker_handoff_ready")
            .expect("handoff readiness follows begin")
            .0;
        let admission_close = begin
            .find("client_db_admission.close()")
            .expect("admission closes at handoff start");
        let generation_advance = begin
            .find("server_query_generation.advance()")
            .expect("handoff invalidates earlier results");
        assert!(admission_close < generation_advance);
        assert!(begin.contains("fs_watch_stop"));
        let ready = workers
            .split_once("fn client_worker_handoff_ready")
            .expect("handoff readiness")
            .1
            .split_once("fn scoped_result_applies")
            .expect("scoped result follows readiness")
            .0;
        for required in [
            "startup_recovery_rx",
            "fs_watch_worker",
            "backup_cleanup_worker",
        ] {
            assert!(
                ready.contains(required),
                "handoff readiness omits {required}"
            );
        }
        let switch = include_str!("game_space_switch.rs");
        let reset = switch
            .split_once("fn reset_space_scoped_state")
            .expect("reset")
            .1;
        assert!(
            !reset.contains("self.quick_scan_worker = None"),
            "reset detaches a quick scan that can still hold the database"
        );
        assert!(
            !reset.contains("self.backend_worker = None"),
            "reset detaches a sync worker"
        );
        assert!(
            !reset.contains("self.addon_backup_worker = None"),
            "reset detaches the addon backup worker"
        );
    }
}
