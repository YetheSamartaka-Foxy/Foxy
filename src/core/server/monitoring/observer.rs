use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::*;
use crate::core::server_query::{QueryError, query_info};

pub(in crate::core::server) struct PreparedObserver {
    input: NativeMonitoringInput,
    snapshot: MonitoringSnapshot,
    logs: logs::LogCollector,
    cache: cache::CacheWriter,
}

pub(in crate::core::server) struct RunObserver {
    cancel: Arc<AtomicBool>,
    query_endpoint: bool,
    last_query_success: Arc<Mutex<Option<Instant>>>,
    latest: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<cache::CacheWriter>>,
    stopped_cache: Option<cache::CacheWriter>,
}

impl PreparedObserver {
    pub(in crate::core::server) fn new(
        store: &super::super::ServerStore,
        status: &InstanceRuntimeStatus,
        mut input: NativeMonitoringInput,
        stdout: PathBuf,
    ) -> Self {
        let started = unix_ms();
        input.log_roots.insert(0, stdout);
        let mut logs = logs::LogCollector::new(input.log_roots.clone(), started);
        let snapshot = MonitoringSnapshot {
            schema_version: 1,
            context_fingerprint: store.activation_context_fingerprint(),
            instance_id: status.instance_id.clone(),
            owner_id: status.owner_id.clone(),
            owner_generation: status.owner_generation,
            run_generation: status.run_generation,
            activation_generation: status.activation_generation,
            runtime_id: status.runtime_id.clone(),
            sampled_unix_ms: started,
            query: QueryObservation {
                state: if input.endpoint.is_some() && input.expected_name.is_some() {
                    QueryState::Pending
                } else {
                    QueryState::Unavailable
                },
                attempted_unix_ms: None,
                last_success_unix_ms: None,
                next_attempt_unix_ms: None,
                consecutive_failures: 0,
                last_good: None,
                attempt_history: Vec::new(),
            },
            resources: Vec::new(),
            logs: logs.sample(),
            headless: None,
        };
        let cache = cache::CacheWriter::new(
            store.record_root(&status.instance_id).join("runtime-state"),
            status,
        );
        Self {
            input,
            snapshot,
            logs,
            cache,
        }
    }

    pub(in crate::core::server) fn start(
        mut self,
        pid: u32,
        headless: crate::core::server::headless::SharedHeadlessStatus,
    ) -> std::io::Result<RunObserver> {
        let cancel = Arc::new(AtomicBool::new(false));
        let latest = Arc::new(Mutex::new(None));
        let last_query_success = Arc::new(Mutex::new(None));
        let query_endpoint = self.snapshot.query.state != QueryState::Unavailable;
        let worker_cancel = cancel.clone();
        let worker_latest = latest.clone();
        let worker_success = last_query_success.clone();
        let thread = std::thread::Builder::new()
            .name("server-observation".into())
            .spawn(move || {
                let mut resources =
                    resources::ResourceSampler::new(pid, self.input.executable.clone());
                let mut next_query = Instant::now();
                loop {
                    if worker_cancel.load(Ordering::Acquire) {
                        break;
                    }
                    if self.snapshot.query.state != QueryState::Unavailable
                        && Instant::now() >= next_query
                    {
                        let query = &mut self.snapshot.query;
                        query.attempted_unix_ms = Some(unix_ms());
                        let result = query_info(
                            self.input.endpoint.expect("query endpoint"),
                            Duration::from_millis(1500),
                            &worker_cancel,
                        );
                        match result {
                            Ok(info)
                                if Some(info.name.as_str())
                                    == self.input.expected_name.as_deref() =>
                            {
                                query.state = QueryState::Responding;
                                query.consecutive_failures = 0;
                                query.last_success_unix_ms = Some(unix_ms());
                                *worker_success
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner()) =
                                    Some(Instant::now());
                                query.last_good = Some(QuerySummary {
                                    name: logs::sanitize_text(&info.name),
                                    map: logs::sanitize_text(&info.map),
                                    players: info.players,
                                    max_players: info.max_players,
                                    bots: info.bots,
                                    version: version_label(&info.version),
                                });
                            }
                            result => {
                                query.state = match result {
                                    Ok(_) => QueryState::IdentityMismatch,
                                    Err(QueryError::Timeout) => QueryState::Timeout,
                                    Err(QueryError::Cancelled) => QueryState::Cancelled,
                                    Err(QueryError::Protocol(_)) => QueryState::InvalidResponse,
                                    Err(QueryError::Network(_)) => QueryState::NetworkError,
                                };
                                query.consecutive_failures =
                                    query.consecutive_failures.saturating_add(1);
                            }
                        }
                        query.attempt_history.push(QueryAttempt {
                            completed_unix_ms: unix_ms(),
                            state: query.state,
                        });
                        if query.attempt_history.len() > 16 {
                            query.attempt_history.remove(0);
                        }
                        let delay = Duration::from_secs(if query.consecutive_failures == 0 {
                            3
                        } else {
                            (1u64 << query.consecutive_failures.min(5)).min(30)
                        });
                        next_query = Instant::now() + delay;
                        query.next_attempt_unix_ms =
                            Some(unix_ms().saturating_add(delay.as_millis() as u64));
                    }
                    self.snapshot.sampled_unix_ms = unix_ms();
                    self.snapshot.resources.push(ResourcePoint {
                        sampled_unix_ms: self.snapshot.sampled_unix_ms,
                        sample: resources.sample(),
                    });
                    if self.snapshot.resources.len() > 60 {
                        self.snapshot.resources.remove(0);
                    }
                    self.snapshot.logs = self.logs.sample();
                    let mut written = self.write_with_headless(&headless, &worker_latest);
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while Instant::now() < deadline && !worker_cancel.load(Ordering::Acquire) {
                        if written && headless.pending() {
                            written = self.write_with_headless(&headless, &worker_latest);
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
                self.snapshot.sampled_unix_ms = unix_ms();
                self.snapshot.logs = self.logs.sample();
                self.write_with_headless(&headless, &worker_latest);
                self.cache
            })?;
        Ok(RunObserver {
            cancel,
            query_endpoint,
            last_query_success,
            latest,
            thread: Some(thread),
            stopped_cache: None,
        })
    }
}

impl PreparedObserver {
    fn write_with_headless(
        &mut self,
        headless: &crate::core::server::headless::SharedHeadlessStatus,
        latest: &Mutex<Option<String>>,
    ) -> bool {
        let (status, revision) = headless.snapshot();
        self.snapshot.headless = status;
        match self.cache.write(&self.snapshot) {
            Ok(digest) => {
                *latest.lock().unwrap_or_else(|error| error.into_inner()) = Some(digest);
                headless.mark_written(revision);
                true
            }
            Err(_) => false,
        }
    }
}

impl RunObserver {
    pub(in crate::core::server) fn fingerprint(&self) -> Option<String> {
        self.latest
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(in crate::core::server) fn headless_readiness(
        &self,
    ) -> crate::core::server::headless::HeadlessReadiness {
        let last = *self
            .last_query_success
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        crate::core::server::headless::HeadlessReadiness::evaluate(
            self.query_endpoint,
            last,
            Instant::now(),
        )
    }

    pub(in crate::core::server) fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            self.stopped_cache = thread.join().ok();
        }
    }
}

impl Drop for RunObserver {
    fn drop(&mut self) {
        self.stop();
    }
}

fn version_label(version: &str) -> String {
    let parts = version.split('.').collect::<Vec<_>>();
    if version.len() <= 64
        && (2..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        version.to_owned()
    } else {
        logs::sanitize_text(version)
    }
}

#[cfg(test)]
mod tests {
    use super::version_label;

    #[test]
    fn dotted_engine_versions_are_not_misclassified_as_network_endpoints() {
        assert_eq!(version_label("1.9.0.37"), "1.9.0.37");
        assert_eq!(version_label("2.22"), "2.22");
        assert!(!version_label("password=hidden").contains("hidden"));
    }
}
