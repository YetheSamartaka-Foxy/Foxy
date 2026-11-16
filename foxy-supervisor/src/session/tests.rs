use super::*;
use crate::leases::InstallationLocks;

fn admission(locks: &InstallationLocks) -> Admission<'_> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match locks.try_admit() {
            Ok(admission) => return admission,
            Err(error) => {
                assert!(
                    error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline,
                    "Admission did not become available: {error}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn assert_maintenance_available(locks: &InstallationLocks) {
    let mut admitted = admission(locks);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match admitted.try_maintenance() {
            Ok(_) => return,
            Err((retained, error)) => {
                assert!(
                    error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline,
                    "Maintenance did not become available: {error}"
                );
                // Fork fixtures can briefly inherit CLOEXEC locks before executing.
                admitted = retained;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        owner_id: "owner_alpha".into(),
        owner_generation: 1,
        game_space_id: "alpha".into(),
        instance_id: "server_alpha".into(),
        operation_id: "op_alpha".into(),
        run_generation: 1,
    }
}

fn event() -> SessionEvent {
    SessionEvent {
        identity: identity(),
        sequence: 1,
        phase: SessionPhase::Spawned,
        exit: None,
        readiness_verified: false,
    }
}

#[test]
fn cursor_rejects_stale_owner_space_instance_operation_run_and_sequence() {
    let current = event();
    let mut cursor = EventCursor::new(identity());
    for change in 0..6 {
        let mut stale = current.clone();
        match change {
            0 => stale.identity.owner_id = "other_owner".into(),
            1 => stale.identity.owner_generation += 1,
            2 => stale.identity.game_space_id = "beta".into(),
            3 => stale.identity.instance_id = "server_beta".into(),
            4 => stale.identity.operation_id = "op_beta".into(),
            _ => stale.identity.run_generation += 1,
        }
        assert!(!cursor.accept(&stale));
    }
    assert!(cursor.accept(&current));
    assert!(!cursor.accept(&current));
    let mut terminal = current.clone();
    terminal.sequence = 2;
    terminal.phase = SessionPhase::Exited;
    assert!(!cursor.accept(&terminal));
    terminal.exit = Some(TreeExit {
        root_exit_code: Some(0),
        forced: false,
    });
    assert!(cursor.accept(&terminal));
    terminal.sequence = 3;
    assert!(!cursor.accept(&terminal));
}

#[test]
fn identities_require_bounded_identifiers_and_positive_generations() {
    assert!(identity().validate().is_ok());
    for change in 0..4 {
        let mut invalid = identity();
        match change {
            0 => invalid.owner_generation = 0,
            1 => invalid.run_generation = 0,
            2 => invalid.operation_id = "../invalid".into(),
            _ => invalid.owner_id = "x".repeat(129),
        }
        assert!(invalid.validate().is_err());
    }
}

#[test]
fn cancelled_start_and_failed_revalidation_release_locks_without_spawning() {
    let root = tempfile::tempdir().unwrap();
    let installation = root.path().join("installation");
    std::fs::create_dir(&installation).unwrap();
    let locks =
        InstallationLocks::capture_in(&installation, &root.path().join("registry")).unwrap();
    let spec = CommandSpec {
        executable: root.path().join("absent"),
        directory: root.path().to_owned(),
        arguments: vec![],
        environment: vec![],
    };
    for cancel in [true, false] {
        let result = ForegroundSession::spawn(
            identity(),
            &spec,
            admission(&locks),
            &AtomicBool::new(cancel),
            || {
                if cancel {
                    Ok(())
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Stale reviewed evidence.",
                    ))
                }
            },
        );
        let error = match result {
            Ok(_) => panic!("Unexpected spawn"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            if cancel {
                io::ErrorKind::Interrupted
            } else {
                io::ErrorKind::InvalidData
            }
        );
        assert_maintenance_available(&locks);
    }
    assert_eq!(std::fs::read_dir(&installation).unwrap().count(), 0);
}

#[test]
fn spawn_failure_keeps_registration_as_a_maintenance_barrier() {
    let root = tempfile::tempdir().unwrap();
    let installation = root.path().join("installation");
    std::fs::create_dir(&installation).unwrap();
    let locks =
        InstallationLocks::capture_in(&installation, &root.path().join("registry")).unwrap();
    let spec = CommandSpec {
        executable: root.path().join("absent"),
        directory: root.path().to_owned(),
        arguments: vec![],
        environment: vec![],
    };
    assert!(
        ForegroundSession::spawn(
            identity(),
            &spec,
            locks.try_admit().unwrap(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .is_err()
    );
    assert_eq!(locks.inspect_consumers().unwrap()[0].session, identity());
    let (_, error) = locks.try_admit().unwrap().try_maintenance().err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
}

#[cfg(any(windows, target_os = "linux"))]
mod native {
    use super::*;
    use std::fs;
    use std::time::Instant;

    fn wait_file(path: &std::path::Path) {
        let start = Instant::now();
        while !path.exists() {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn spec(root: &std::path::Path) -> CommandSpec {
        CommandSpec {
            executable: std::env::current_exe().unwrap(),
            directory: root.to_owned(),
            arguments: ["--exact", "session::tests::native::fixture", "--ignored"]
                .map(Into::into)
                .to_vec(),
            environment: vec![(
                "FOXY_SESSION_FIXTURE_ROOT".into(),
                root.as_os_str().to_owned(),
            )],
        }
    }

    #[cfg(target_os = "linux")]
    fn controlled_spec(root: &std::path::Path) -> CommandSpec {
        let mut spec = spec(root);
        spec.arguments.push("--nocapture".into());
        spec.environment
            .push(("FOXY_SESSION_SIGNAL_FIXTURE".into(), "1".into()));
        spec
    }

    fn stdio_spec(root: &std::path::Path) -> CommandSpec {
        let mut spec = spec(root);
        spec.arguments.push("--nocapture".into());
        spec.environment
            .push(("FOXY_SESSION_STDIO_FIXTURE".into(), "1".into()));
        spec
    }

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Requires native unprivileged PID namespaces"
    )]
    fn controlled_session_redirects_output_and_releases_after_natural_exit() {
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("installation");
        fs::create_dir(&installation).unwrap();
        let locks =
            InstallationLocks::capture_in(&installation, &root.path().join("registry")).unwrap();
        let output_path = root.path().join("session.log");
        let output = fs::File::create(&output_path).unwrap();
        let mut session = ForegroundSession::spawn_controlled(
            identity(),
            &stdio_spec(root.path()),
            &output,
            locks.try_admit().unwrap(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            session.wait(Duration::from_secs(10)).unwrap(),
            Some(TreeExit {
                root_exit_code: Some(0),
                forced: false
            })
        );
        let captured = fs::read_to_string(output_path).unwrap();
        assert!(captured.contains("stdout fixture"));
        assert!(captured.contains("stderr fixture"));
        assert!(locks.inspect_consumers().unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "Requires native unprivileged PID namespaces"]
    fn controlled_soft_stop_exits_naturally_and_preserves_retained_lifetime_until_waited() {
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("installation");
        fs::create_dir(&installation).unwrap();
        let locks =
            InstallationLocks::capture_in(&installation, &root.path().join("registry")).unwrap();
        let output_path = root.path().join("session.log");
        let output = fs::File::create(&output_path).unwrap();
        let mut session = ForegroundSession::spawn_controlled(
            identity(),
            &controlled_spec(root.path()),
            &output,
            locks.try_admit().unwrap(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap();
        wait_file(&root.path().join("signal-ready"));
        assert_eq!(session.wait(Duration::from_millis(15)).unwrap(), None);
        assert_eq!(locks.inspect_consumers().unwrap()[0].session, identity());
        let peer_root = tempfile::tempdir().unwrap();
        let mut peer = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "session::tests::native::fixture", "--ignored"])
            .env("FOXY_SESSION_FIXTURE_ROOT", peer_root.path())
            .env("FOXY_SESSION_SIGNAL_FIXTURE", "1")
            .spawn()
            .unwrap();
        wait_file(&peer_root.path().join("signal-ready"));
        session.deliver_soft_stop(None).unwrap();
        session.deliver_soft_stop(None).unwrap();
        let exit = session.wait(Duration::from_secs(5)).unwrap().unwrap();
        assert!(!exit.forced);
        assert_eq!(exit.root_exit_code, Some(0));
        assert!(
            fs::read_to_string(output_path)
                .unwrap()
                .contains("signal-received")
        );
        assert!(peer.try_wait().unwrap().is_none());
        fs::write(peer_root.path().join("release-peer"), b"release").unwrap();
        assert!(peer.wait().unwrap().success());
        assert!(locks.inspect_consumers().unwrap().is_empty());
        assert!(locks.try_admit().unwrap().try_maintenance().is_ok());
    }

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Requires native unprivileged PID namespaces"
    )]
    fn native_session_stop_timeout_retains_lifetime_until_the_entire_tree_is_waited() {
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("installation");
        fs::create_dir(&installation).unwrap();
        let registry = root.path().join("registry");
        let owner = InstallationLocks::capture_in(&installation, &registry).unwrap();
        let updater = InstallationLocks::capture_in(&installation, &registry).unwrap();
        let mut session = ForegroundSession::spawn(
            identity(),
            &spec(root.path()),
            owner.try_admit().unwrap(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap();
        assert_ne!(session.process_id(), 0);
        assert!(!session.event().readiness_verified);
        assert_eq!(owner.inspect_consumers().unwrap()[0].session, identity());
        wait_file(&root.path().join("descendant-ready"));
        let admission = updater.try_admit().unwrap();
        let (admission, _) = match admission.try_maintenance() {
            Ok(_) => panic!("Live content unlocked"),
            Err(blocked) => blocked,
        };
        session.request_stop();
        assert_eq!(session.event().phase, SessionPhase::StopRequested);
        assert_eq!(session.wait(Duration::from_millis(15)).unwrap(), None);
        assert_eq!(session.event().phase, SessionPhase::StopRequested);
        let exit = session
            .force_stop(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert!(exit.forced);
        assert_eq!(session.event().phase, SessionPhase::Exited);
        assert!(!session.event().readiness_verified);
        assert!(owner.inspect_consumers().unwrap().is_empty());
        assert!(admission.try_maintenance().is_ok());
        let terminal_sequence = session.event().sequence;
        session.request_stop();
        assert_eq!(session.observe().unwrap(), Some(exit));
        assert_eq!(session.event().sequence, terminal_sequence);
    }

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "Requires native unprivileged PID namespaces"
    )]
    fn native_session_drop_waits_for_fixture_tree_before_releasing_content() {
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("installation");
        fs::create_dir(&installation).unwrap();
        let locks =
            InstallationLocks::capture_in(&installation, &root.path().join("registry")).unwrap();
        let session = ForegroundSession::spawn(
            identity(),
            &spec(root.path()),
            locks.try_admit().unwrap(),
            &AtomicBool::new(false),
            || Ok(()),
        )
        .unwrap();
        wait_file(&root.path().join("descendant-ready"));
        drop(session);
        assert!(locks.inspect_consumers().unwrap().is_empty());
        assert!(locks.try_admit().unwrap().try_maintenance().is_ok());
    }

    #[test]
    #[ignore = "Invoked by isolated foreground-session tests"]
    fn fixture() {
        let root = std::path::PathBuf::from(std::env::var_os("FOXY_SESSION_FIXTURE_ROOT").unwrap());
        if std::env::var_os("FOXY_SESSION_STDIO_FIXTURE").is_some() {
            println!("stdout fixture");
            eprintln!("stderr fixture");
            return;
        }
        #[cfg(target_os = "linux")]
        {
            if std::env::var_os("FOXY_SESSION_SIGNAL_FIXTURE").is_some() {
                unsafe extern "C" fn signal_handler(_: i32) {
                    SIGNAL_RECEIVED.store(true, Ordering::Release);
                }
                static SIGNAL_RECEIVED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                unsafe {
                    libc::signal(
                        libc::SIGINT,
                        signal_handler as *const () as libc::sighandler_t,
                    );
                }
                fs::write(root.join("signal-ready"), b"ready").unwrap();
                println!("signal-ready");
                let deadline = Instant::now() + Duration::from_secs(10);
                while !SIGNAL_RECEIVED.load(Ordering::Acquire)
                    && !root.join("release-peer").exists()
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if SIGNAL_RECEIVED.load(Ordering::Acquire) {
                    println!("signal-received");
                    return;
                }
                if root.join("release-peer").exists() {
                    return;
                }
                panic!("Soft stop signal was not delivered");
            }
        }
        if std::env::var_os("FOXY_SESSION_DESCENDANT").is_some() {
            fs::write(root.join("descendant-ready"), b"ready").unwrap();
            std::thread::sleep(Duration::from_secs(30));
        } else {
            #[expect(
                clippy::zombie_processes,
                reason = "The fixture exits before its descendant to prove tree lifetime."
            )]
            let _child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "session::tests::native::fixture", "--ignored"])
                .env("FOXY_SESSION_DESCENDANT", "1")
                .spawn()
                .unwrap();
            wait_file(&root.join("descendant-ready"));
        }
    }
}
