use std::collections::{HashMap, HashSet};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::ui::app::Foxy;
use crate::ui::types::{ServerOnlineStatus, ServerStatusCache};

pub(crate) use super::client_workers::WorkerGeneration as ServerQueryGeneration;
pub(crate) use super::client_workers::WorkerScope as ServerQueryScope;

pub(crate) struct ServerQueryResult {
    pub(crate) scope: ServerQueryScope,
    pub(crate) address: String,
    pub(crate) port: String,
    pub(crate) status: ServerOnlineStatus,
}

fn apply_result(
    generation: &ServerQueryGeneration,
    active_space: &str,
    pending: &mut HashSet<(String, String)>,
    statuses: &mut HashMap<(String, String), ServerStatusCache>,
    result: ServerQueryResult,
) -> bool {
    if !generation.accepts(active_space, &result.scope) {
        return false;
    }
    let key = (result.address, result.port);
    pending.remove(&key);
    statuses.insert(
        key,
        ServerStatusCache {
            last_check: Instant::now(),
            status: result.status,
        },
    );
    true
}

fn reap_finished(workers: &mut Vec<JoinHandle<()>>) {
    super::client_workers::reap_workers(workers, "Server query worker failed.");
}

impl Foxy {
    pub(in crate::ui::app) fn poll_server_query_results(&mut self) {
        let active_space = crate::core::game::spaces::active_game_space().space_id;
        while let Ok(result) = self.server_updates.try_recv() {
            apply_result(
                &self.server_query_generation,
                &active_space,
                &mut self.pending_server_queries,
                &mut self.server_statuses,
                result,
            );
        }
    }

    pub(in crate::ui::app) fn reap_server_query_workers(&mut self) {
        reap_finished(&mut self.pending_queries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> (String, String) {
        ("127.0.0.1".into(), "2302".into())
    }

    fn result(scope: ServerQueryScope, players: u32) -> ServerQueryResult {
        let (address, port) = endpoint();
        ServerQueryResult {
            scope,
            address,
            port,
            status: ServerOnlineStatus::Online { players },
        }
    }

    #[test]
    fn query_scope_requires_the_current_space_and_generation() {
        let mut generation = ServerQueryGeneration::default();
        let alpha = generation.capture("alpha".into()).unwrap();
        assert!(generation.accepts("alpha", &alpha));
        assert!(!generation.accepts("beta", &alpha));
        generation.advance();
        assert!(!generation.accepts("alpha", &alpha));
        assert!(generation.accepts("alpha", &generation.capture("alpha".into()).unwrap()));
    }

    #[test]
    fn exhausted_query_generations_never_wrap_or_accept_old_results() {
        let mut generation = ServerQueryGeneration::from_counter(u64::MAX);
        let last = generation.capture("alpha".into()).unwrap();
        generation.advance();
        assert!(generation.capture("alpha".into()).is_none());
        assert!(!generation.accepts("alpha", &last));
        generation.advance();
        assert!(generation.capture("alpha".into()).is_none());
    }

    #[test]
    fn current_query_results_clear_pending_and_replace_cached_status() {
        let generation = ServerQueryGeneration::default();
        let mut pending = HashSet::from([endpoint()]);
        let mut statuses = HashMap::from([(
            endpoint(),
            ServerStatusCache {
                last_check: Instant::now(),
                status: ServerOnlineStatus::Offline,
            },
        )]);
        assert!(apply_result(
            &generation,
            "alpha",
            &mut pending,
            &mut statuses,
            result(generation.capture("alpha".into()).unwrap(), 7),
        ));
        assert!(pending.is_empty());
        assert!(matches!(
            statuses[&endpoint()].status,
            ServerOnlineStatus::Online { players: 7 }
        ));
    }

    #[test]
    fn late_query_results_preserve_pending_and_cache_after_returning_to_the_same_space() {
        let mut generation = ServerQueryGeneration::default();
        let old_alpha = generation.capture("alpha".into()).unwrap();
        generation.advance();
        let old_beta = generation.capture("beta".into()).unwrap();
        generation.advance();
        let current = generation.capture("alpha".into()).unwrap();
        let mut pending = HashSet::from([endpoint()]);
        let checked = Instant::now();
        let mut statuses = HashMap::from([(
            endpoint(),
            ServerStatusCache {
                last_check: checked,
                status: ServerOnlineStatus::Offline,
            },
        )]);
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            sender.send(result(old_alpha, 1)).unwrap();
            sender.send(result(old_beta, 2)).unwrap();
        });
        worker.join().unwrap();
        for stale in receiver {
            assert!(!apply_result(
                &generation,
                "alpha",
                &mut pending,
                &mut statuses,
                stale,
            ));
            assert!(pending.contains(&endpoint()));
            assert_eq!(statuses[&endpoint()].last_check, checked);
            assert!(matches!(
                statuses[&endpoint()].status,
                ServerOnlineStatus::Offline
            ));
        }
        assert!(apply_result(
            &generation,
            "alpha",
            &mut pending,
            &mut statuses,
            result(current, 3),
        ));
        assert!(pending.is_empty());
        assert!(matches!(
            statuses[&endpoint()].status,
            ServerOnlineStatus::Online { players: 3 }
        ));
    }

    #[test]
    fn unfinished_query_handles_are_retained_until_they_can_be_joined() {
        let (release, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || wait.recv().unwrap());
        let mut workers = vec![worker];
        reap_finished(&mut workers);
        assert_eq!(workers.len(), 1);
        release.send(()).unwrap();
        while !workers[0].is_finished() {
            std::thread::yield_now();
        }
        reap_finished(&mut workers);
        assert!(workers.is_empty());
    }
}
