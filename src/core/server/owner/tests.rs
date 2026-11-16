use super::*;
use std::{
    process::{Command, Stdio},
    sync::atomic::Ordering,
};

#[test]
fn public_owner_snapshot_example_contains_scope_but_no_connection_capability() {
    let bytes = include_str!("../../../../examples/json/server_backend/server_owner_snapshot.json");
    let snapshot: OwnerSnapshot = serde_json::from_str(bytes).unwrap();
    snapshot.scope.validate().unwrap();
    assert!(!snapshot.database_opened);
    assert!(snapshot.lifecycle_available);
    assert_eq!(snapshot.active_sessions, 0);
    assert!(valid_lifetime_snapshot(&snapshot));
    assert!(!bytes.contains("capability"));
}

#[test]
fn app_updates_wait_for_supervised_sessions_and_detached_owners() {
    let mut snapshot: OwnerSnapshot = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_owner_snapshot.json"
    ))
    .unwrap();
    snapshot.lifetime = OwnerLifetime::ParentBound;
    assert_eq!(
        update_gate::blocks_update(&snapshot),
        None,
        "an idle app-bound owner ends with the app"
    );
    snapshot.active_sessions = 2;
    assert_eq!(
        update_gate::blocks_update(&snapshot),
        Some(UpdateBlockerKind::Sessions(2))
    );
    snapshot.active_sessions = 0;
    snapshot.lifetime = OwnerLifetime::Detached;
    assert_eq!(
        update_gate::blocks_update(&snapshot),
        Some(UpdateBlockerKind::Detached),
        "a detached owner outlives the app"
    );

    let root = tempfile::tempdir().unwrap();
    assert!(update_gate::update_blockers(root.path()).is_empty());
    let registry = root
        .path()
        .join(crate::core::game::spaces::GAMES_REGISTRY_FILE);
    std::fs::write(&registry, b"{ not json").unwrap();
    assert_eq!(
        update_gate::update_blockers(root.path())
            .into_iter()
            .map(|blocker| blocker.kind)
            .collect::<Vec<_>>(),
        vec![UpdateBlockerKind::RegistryUnreadable],
        "an unreadable space list cannot prove that no server runs"
    );
    std::fs::write(
        &registry,
        include_str!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    assert!(
        update_gate::update_blockers(root.path()).is_empty(),
        "spaces without a running owner never block"
    );
}

#[test]
fn detached_lifetime_requires_a_prepared_launch_identity() {
    let mut snapshot: OwnerSnapshot = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_owner_snapshot.json"
    ))
    .unwrap();
    snapshot.lifetime = OwnerLifetime::Detached;
    assert!(valid_lifetime_snapshot(&snapshot));
    snapshot.launch_id = None;
    assert!(!valid_lifetime_snapshot(&snapshot));
    snapshot.lifetime = OwnerLifetime::Foreground;
    assert!(valid_lifetime_snapshot(&snapshot));
    snapshot.launch_id = Some("untrusted_launch".into());
    assert!(!valid_lifetime_snapshot(&snapshot));
}

#[test]
fn metadata_permit_keeps_owner_claim_until_retained_work_finishes() {
    if std::env::var_os("FOXY_OWNER_PERMIT_PROBE").is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::server::owner::tests::metadata_permit_keeps_owner_claim_until_retained_work_finishes", "--nocapture"])
            .env("FOXY_OWNER_PERMIT_PROBE", "1").output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("games.json"), br#"{"active_game_space_id":"alpha","last_opened_game_space_id":"alpha","game_spaces":[{"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0}]}"#).unwrap();
    std::fs::create_dir_all(root.path().join("games/alpha")).unwrap();
    let context = ServerContext::capture(root.path(), Some("alpha")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "core::server::owner::tests::owner_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("FOXY_OWNER_FIXTURE_ROOT", root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lifetime = child.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if attach(&context).is_ok_and(|owner| owner.is_some()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owner fixture did not become ready"
        );
        assert!(child.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    let permit = permit(&context).unwrap().unwrap();
    let pin = pin_write(&context.database_path()).unwrap();
    assert!(!crate::core::tasks::db_process_lock::holds(
        &context.database_path()
    ));
    assert!(!context.database_path().exists());
    assert!(permit::permit(&context).is_err());
    drop(lifetime);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child.try_wait().unwrap().is_none(),
        "owner released an in-flight operation"
    );
    drop(permit);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().unwrap().is_none(),
        "write pin did not retain operation authority"
    );
    drop(pin);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owner did not drain after permit release"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pin_write(&context.database_path()).is_err());
    assert!(attach(&context).unwrap().is_none());
    let (directory, name, key) = location(&context, true).unwrap();
    let directory = directory.unwrap();
    let scope = Scope {
        context_key: key,
        space_id: "alpha".into(),
        instance_id: IDENTITY.into(),
        owner_id: "owner_fixture".into(),
        generation: 1,
    };
    for malformed in 0..2 {
        let mut record = OwnerRecord {
            version: RECORD_VERSION,
            scope: scope.clone(),
            endpoint: "unknown_endpoint".into(),
            capability: [1; 32],
        };
        if malformed == 0 {
            record.scope.generation = 0;
        }
        let bytes = serde_json::to_vec(&record).unwrap();
        directory.publish_new(&name, &bytes).unwrap();
        assert!(supervise(&context, false, Arc::new(AtomicBool::new(true))).is_err());
        assert_eq!(directory.read(&name).unwrap(), bytes);
        assert!(!crate::core::tasks::db_process_lock::holds(
            &context.database_path()
        ));
        directory.remove_exact(&name, &bytes).unwrap();
    }
}

#[test]
#[ignore = "Invoked only by the owner permit test"]
fn owner_fixture() {
    let root = std::env::var_os("FOXY_OWNER_FIXTURE_ROOT").unwrap();
    let context = ServerContext::capture(std::path::Path::new(&root), Some("alpha")).unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    supervise(&context, true, stopped.clone()).unwrap();
    stopped.store(true, Ordering::Release);
}

#[test]
fn a_dead_owner_endpoint_is_reported_as_not_running_without_hiding_other_failures() {
    for kind in [io::ErrorKind::NotFound, io::ErrorKind::ConnectionRefused] {
        let reported = unreachable_owner(io::Error::from(kind));
        assert_eq!(reported.kind(), io::ErrorKind::NotFound);
        assert!(reported.to_string().contains("recorded owner"));
        assert!(reported.to_string().contains("recovery-plan"));
    }
    let denied = unreachable_owner(io::Error::from(io::ErrorKind::PermissionDenied));
    assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
    assert!(!denied.to_string().contains("recorded owner"));
}
