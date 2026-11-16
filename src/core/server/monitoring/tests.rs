use super::*;
use crate::core::server::{InstanceRuntimePhase, ServerInstanceId};

fn status() -> InstanceRuntimeStatus {
    let id = ServerInstanceId::try_from(format!("server_{}", "a".repeat(32))).unwrap();
    let scope = foxy_supervisor::control::Scope {
        context_key: "a".repeat(64),
        space_id: "alpha".into(),
        instance_id: "space_catalog".into(),
        owner_id: "owner_alpha".into(),
        generation: 2,
    };
    let mut status = InstanceRuntimeStatus::not_owned(id, &scope);
    status.run_generation = 4;
    status.phase = InstanceRuntimePhase::Running;
    status.process_exists = true;
    status
}

fn snapshot() -> MonitoringSnapshot {
    let status = status();
    MonitoringSnapshot {
        schema_version: 1,
        context_fingerprint: "a".repeat(64),
        instance_id: status.instance_id,
        owner_id: status.owner_id,
        owner_generation: 2,
        run_generation: 4,
        activation_generation: None,
        runtime_id: None,
        sampled_unix_ms: 20_000,
        query: QueryObservation {
            state: QueryState::Responding,
            attempted_unix_ms: Some(20_000),
            last_success_unix_ms: Some(20_000),
            next_attempt_unix_ms: Some(23_000),
            consecutive_failures: 0,
            attempt_history: Vec::new(),
            last_good: Some(QuerySummary {
                name: "Example server".into(),
                map: "Altis".into(),
                players: 0,
                max_players: 16,
                bots: 0,
                version: "2.22".into(),
            }),
        },
        resources: Vec::new(),
        logs: LogSnapshot {
            entries: Vec::new(),
            next_sequence: 1,
            truncated: false,
            error: None,
        },
        headless: None,
    }
}

#[test]
fn query_timeout_preserves_running_process_and_last_good_without_readiness() {
    let mut observation = snapshot();
    observation.query.state = QueryState::Timeout;
    observation.query.consecutive_failures = 1;
    let report = InstanceMonitoring::new(status(), Some(observation), 21_000);
    assert_eq!(report.health, MonitoringHealth::Degraded);
    assert!(report.status.process_exists);
    assert!(!report.query_ready);
    assert!(!report.status.readiness_verified);
    assert_eq!(report.query_age_ms, Some(1000));
    assert_eq!(
        report.observation.unwrap().query.last_good.unwrap().map,
        "Altis"
    );
}

#[test]
fn process_query_and_snapshot_freshness_are_independent() {
    let report = InstanceMonitoring::new(status(), Some(snapshot()), 21_000);
    assert!(report.query_ready);
    assert_eq!(report.health, MonitoringHealth::Healthy);
    assert!(!report.status.readiness_verified);
    let stale = InstanceMonitoring::new(status(), Some(snapshot()), 31_000);
    assert_eq!(stale.health, MonitoringHealth::Stale);
    assert!(!stale.query_ready);
    let mut exited = status();
    exited.process_exists = false;
    exited.phase = InstanceRuntimePhase::Exited;
    let stopped = InstanceMonitoring::new(exited, Some(snapshot()), 21_000);
    assert_eq!(stopped.health, MonitoringHealth::NotRunning);
    assert!(!stopped.query_ready);
    let missing = InstanceMonitoring::new(status(), None, 21_000);
    assert_eq!(missing.health, MonitoringHealth::Unknown);
}

#[test]
fn an_unreachable_owner_cannot_leave_cached_desktop_health_fresh() {
    let mut report = InstanceMonitoring::new(status(), Some(snapshot()), 21_000);
    assert!(report.query_ready);
    report.age_by(11_000);
    assert_eq!(report.health, MonitoringHealth::Stale);
    assert!(!report.query_ready);
    assert!(report.status.process_exists);
    assert_eq!(report.observation_age_ms, Some(12_000));
}

#[test]
fn old_or_future_clock_samples_cannot_establish_fresh_health() {
    assert!(!InstanceMonitoring::new(status(), Some(snapshot()), 19_000).query_ready);
    let mut old_query = snapshot();
    old_query.sampled_unix_ms = 40_000;
    assert!(!InstanceMonitoring::new(status(), Some(old_query), 40_000).query_ready);
    let mut stale_owner = status();
    stale_owner.observation_age_ms = 11_000;
    assert_eq!(
        InstanceMonitoring::new(stale_owner, Some(snapshot()), 21_000).health,
        MonitoringHealth::Stale
    );
}

#[test]
fn cached_observations_are_bounded_and_preserve_replaced_unknown_files() {
    let directory = tempfile::tempdir().unwrap();
    let mut writer = cache::CacheWriter::new(directory.path().to_path_buf(), &status());
    let first = writer.write(&snapshot()).unwrap();
    let first_path = directory
        .path()
        .join(format!("observation_owner_alpha_4_{first}.json"));
    std::fs::write(&first_path, b"unknown replacement").unwrap();
    let mut sample = snapshot();
    for tick in 1..(cache::RETAINED_SNAPSHOTS as u64 + 4) {
        sample.sampled_unix_ms += tick;
        writer.write(&sample).unwrap();
    }
    assert_eq!(std::fs::read(first_path).unwrap(), b"unknown replacement");
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        cache::RETAINED_SNAPSHOTS + 1
    );
    let repeated = writer.write(&sample).unwrap();
    assert_eq!(writer.write(&sample).unwrap(), repeated);
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        cache::RETAINED_SNAPSHOTS + 1
    );
    drop(writer);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn cache_publication_supports_ordinary_windows_paths_above_the_legacy_limit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("a".repeat(120));
    std::fs::create_dir(&root).unwrap();
    let mut writer = cache::CacheWriter::new(root.clone(), &status());
    let digest = writer.write(&snapshot()).unwrap();
    assert!(
        root.join(format!("observation_owner_alpha_4_{digest}.json"))
            .to_string_lossy()
            .len()
            > 260
    );
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn cache_digest_does_not_replace_full_context_and_run_identity_validation() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("games.json"),
        include_bytes!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let context = super::super::ServerContext::capture(directory.path(), Some("arma3")).unwrap();
    let store = super::super::ServerStore::new(context).unwrap();
    let mut status = status();
    let root = store.record_root(&status.instance_id).join("runtime-state");
    std::fs::create_dir_all(&root).unwrap();
    let mut writer = cache::CacheWriter::new(root, &status);
    let mut sample = snapshot();
    sample.context_fingerprint = store.activation_context_fingerprint();
    status.monitoring_fingerprint = Some(writer.write(&sample).unwrap());
    assert_eq!(cache::read(&store, &status).unwrap(), Some(sample.clone()));
    sample.run_generation += 1;
    status.monitoring_fingerprint = Some(writer.write(&sample).unwrap());
    assert!(cache::read(&store, &status).is_err());
    sample.run_generation -= 1;
    sample.context_fingerprint = "b".repeat(64);
    status.monitoring_fingerprint = Some(writer.write(&sample).unwrap());
    assert!(cache::read(&store, &status).is_err());
}

#[test]
fn disabled_query_emits_no_datagram_and_observer_stop_retains_only_its_snapshot() {
    use std::io::Write;
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("games.json"),
        include_bytes!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let store = super::super::ServerStore::new(
        super::super::ServerContext::capture(directory.path(), Some("arma3")).unwrap(),
    )
    .unwrap();
    let mut status = status();
    let root = store.record_root(&status.instance_id).join("runtime-state");
    std::fs::create_dir_all(&root).unwrap();
    let stdout = root.join("run.log");
    std::fs::write(&stdout, b"previous\n").unwrap();
    let listener = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    listener
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let observer = PreparedObserver::new(
        &store,
        &status,
        NativeMonitoringInput {
            endpoint: Some(listener.local_addr().unwrap()),
            expected_name: None,
            executable: std::env::current_exe().unwrap(),
            log_roots: Vec::new(),
        },
        stdout.clone(),
    );
    let mut observer = observer
        .start(std::process::id(), Default::default())
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&stdout)
        .unwrap()
        .write_all(b"current password=hidden\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while observer.fingerprint().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let stopped = Instant::now();
    observer.stop();
    assert!(stopped.elapsed() < Duration::from_millis(500));
    status.monitoring_fingerprint = observer.fingerprint();
    assert!(status.monitoring_fingerprint.is_some());
    let snapshot = cache::read(&store, &status).unwrap().unwrap();
    assert_eq!(snapshot.query.state, QueryState::Unavailable);
    assert!(snapshot.query.attempt_history.is_empty());
    assert!(
        snapshot
            .logs
            .entries
            .iter()
            .any(|line| line.text == "current password=[redacted]")
    );
    let mut packet = [0; 64];
    assert!(listener.recv_from(&mut packet).is_err());
    drop(observer);
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
    assert_eq!(
        std::fs::read(stdout).unwrap(),
        b"previous\ncurrent password=hidden\n"
    );
}
