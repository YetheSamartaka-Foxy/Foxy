use super::*;
use crate::core::server::installation::tests::create_native_install;

fn store(root: &Path, game: &str) -> ServerStore {
    fs::write(
        root.join("games.json"),
        format!(
            r#"{{"active_game_space_id":"alpha","last_opened_game_space_id":"alpha","game_spaces":[{{"id":"alpha","game_id":"{game}","display_name":"Alpha","created_at":0}}]}}"#
        ),
    )
    .unwrap();
    ServerStore::new(ServerContext::capture(root, Some("alpha")).unwrap()).unwrap()
}

fn manifest_for(installation: &ServerInstallation) -> ManagedManifest {
    ManagedManifest {
        schema_version: 1,
        game: installation.game,
        platform: installation.platform,
        app_id: 233_780,
        channel: ContentChannel::Stable,
        branch: "public".into(),
        build_id: Some(1),
        change_number: Some(2),
        depots: vec![ManagedDepot {
            depot_id: 233_781,
            manifest_gid: u64::MAX,
        }],
        files: 1,
        bytes: installation.evidence.bytes,
        inventory_blake3: "0".repeat(64),
        executable: ManagedExecutable {
            path: installation.executable.clone(),
            bytes: installation.evidence.bytes,
            blake3: installation.evidence.blake3.clone(),
        },
    }
}

#[test]
fn managed_manifest_marks_only_the_exact_executable_as_managed() {
    let external = tempfile::tempdir().unwrap();
    create_native_install(external.path(), ServerDefinitionKind::Arma3);
    let adopted =
        ServerInstallation::inspect(external.path(), ServerDefinitionKind::Arma3).unwrap();
    assert_eq!(adopted.ownership, InstallationOwnership::Adopted);
    assert_eq!(
        adopted.build_evidence,
        BuildEvidence::ExecutableFingerprintOnly
    );

    let manifest = manifest_for(&adopted);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains(&format!("\"{}\"", u64::MAX)));
    assert_eq!(
        serde_json::from_slice::<ManagedManifest>(&bytes).unwrap(),
        manifest
    );
    fs::write(external.path().join(MANIFEST_FILE), &bytes).unwrap();
    let managed =
        ServerInstallation::inspect(external.path(), ServerDefinitionKind::Arma3).unwrap();
    assert_eq!(managed.ownership, InstallationOwnership::Managed);
    assert_eq!(managed.build_evidence, BuildEvidence::NativeDepotManifest);
    assert!(managed.validate(ServerDefinitionKind::Arma3));
    managed.verify().unwrap();

    let mut foreign = manifest.clone();
    foreign.executable.blake3 = "f".repeat(64);
    fs::write(
        external.path().join(MANIFEST_FILE),
        serde_json::to_vec(&foreign).unwrap(),
    )
    .unwrap();
    assert_eq!(
        ServerInstallation::inspect(external.path(), ServerDefinitionKind::Arma3)
            .unwrap()
            .ownership,
        InstallationOwnership::Adopted
    );
    fs::write(
        external.path().join(MANIFEST_FILE),
        b"{\"schema_version\":1}",
    )
    .unwrap();
    assert_eq!(
        ServerInstallation::inspect(external.path(), ServerDefinitionKind::Arma3)
            .unwrap()
            .ownership,
        InstallationOwnership::Adopted
    );
}

#[test]
fn managed_receipt_channel_matches_its_dedicated_app_and_old_arma_receipts_default_stable() {
    let external = tempfile::tempdir().unwrap();
    create_native_install(external.path(), ServerDefinitionKind::Reforger);
    let installation =
        ServerInstallation::inspect(external.path(), ServerDefinitionKind::Reforger).unwrap();
    let mut receipt = manifest_for(&installation);
    for channel in [ContentChannel::Stable, ContentChannel::Experimental] {
        receipt.channel = channel;
        receipt.app_id = dedicated_app(ServerDefinitionKind::Reforger, channel).unwrap();
        fs::write(
            external.path().join(MANIFEST_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ServerInstallation::inspect(external.path(), ServerDefinitionKind::Reforger)
                .unwrap()
                .ownership,
            InstallationOwnership::Managed
        );
        receipt.channel = if channel == ContentChannel::Stable {
            ContentChannel::Experimental
        } else {
            ContentChannel::Stable
        };
        fs::write(
            external.path().join(MANIFEST_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ServerInstallation::inspect(external.path(), ServerDefinitionKind::Reforger)
                .unwrap()
                .ownership,
            InstallationOwnership::Adopted
        );
    }
    let old: ManagedManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/managed_installation_manifest.json"
    ))
    .unwrap();
    assert_eq!(old.channel, ContentChannel::Stable);
    assert!(
        !serde_json::to_value(&old)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("channel")
    );
    assert!(dedicated_app(ServerDefinitionKind::Arma3, ContentChannel::Experimental).is_err());
}

#[test]
fn destinations_must_be_new_absolute_and_outside_foxy_data() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "arma3");
    let external = tempfile::tempdir().unwrap();
    let fresh = external.path().join("ServerAlpha");
    assert_eq!(
        validate_destination(&store, &fresh).unwrap(),
        external.path().canonicalize().unwrap().join("ServerAlpha")
    );
    fs::create_dir(&fresh).unwrap();
    assert!(matches!(
        validate_destination(&store, &fresh),
        Err(ServerStoreError::Invalid(_))
    ));
    assert!(matches!(
        validate_destination(&store, Path::new("relative/server")),
        Err(ServerStoreError::Invalid(_))
    ));
    assert!(validate_destination(&store, &external.path().join("missing/child")).is_err());
    assert!(matches!(
        validate_destination(&store, &root.path().join("server-copy")),
        Err(ServerStoreError::Invalid(_))
    ));
    assert!(matches!(
        validate_destination(&store, &external.path().join(".hidden")),
        Err(ServerStoreError::UnsafePath)
    ));
}

#[test]
fn only_unlocked_owned_staging_is_reported_and_removed() {
    let external = tempfile::tempdir().unwrap();
    let destination = external.path().join("Server.Alpha");
    let prefix = staging_name(&destination).unwrap();
    let interrupted = external.path().join(format!("{prefix}{:016x}", 1));
    let live = external.path().join(format!("{prefix}{:016x}", 2));
    let foreign = external.path().join(format!("{prefix}not-hex-name"));
    let unlocked_without_marker = external.path().join(format!("{prefix}{:016x}", 3));
    for directory in [&interrupted, &live, &foreign, &unlocked_without_marker] {
        fs::create_dir(directory).unwrap();
        fs::write(directory.join("payload.bin"), b"x").unwrap();
    }
    fs::write(lock_file(&interrupted), b"").unwrap();
    fs::write(lock_file(&live), b"").unwrap();
    let colliding = external.path().join(format!("{prefix}{:016x}", 4));
    assert!(
        create_staging(&colliding, &lock_file(&live)).is_err(),
        "existing lock files are never reused for new staging"
    );
    assert!(!colliding.exists());
    assert!(lock_file(&live).exists());
    let live_lock = File::options()
        .read(true)
        .write(true)
        .open(lock_file(&live))
        .unwrap();
    live_lock.try_lock().unwrap();

    assert_eq!(
        interrupted_staging(&destination).unwrap(),
        vec![interrupted.clone()]
    );

    let control = ContentStageControl::default();
    control.cancel();
    let outcome = run_install(
        InstallSpec {
            app_id: 1007,
            channel: ContentChannel::Stable,
            branch: "public",
            platform: ServerPlatform::current().unwrap(),
            game: ServerDefinitionKind::Arma3,
            directory: &destination,
            expected: None,
            executable: None,
            interrupted: &[interrupted.clone(), live.clone(), foreign.clone()],
        },
        LoginInput::Anonymous,
        &control,
    );
    assert_eq!(outcome.status, NativeInstallStatus::Cancelled);
    assert!(!outcome.published);
    assert_eq!(outcome.writes_performed, Some(true));
    assert!(!outcome.downloads_started);
    assert!(!interrupted.exists() && !lock_file(&interrupted).exists());
    assert!(live.join("payload.bin").exists() && foreign.join("payload.bin").exists());
    assert!(unlocked_without_marker.join("payload.bin").exists());
    assert!(!destination.exists());
    drop(live_lock);
}

#[test]
fn native_install_is_gated_by_capability_branch_and_alias() {
    let root = tempfile::tempdir().unwrap();
    let reforger = store(root.path(), "reforger");
    let request = NativeInstallRequest {
        instance_id: ServerInstanceId::new(),
        directory: root.path().join("x"),
        branch: "public".into(),
        channel: None,
        account: None,
    };
    if !cfg!(all(
        target_arch = "x86_64",
        any(windows, target_os = "linux")
    )) {
        assert!(matches!(
            reforger.plan_native_install(request.clone(), &ContentStageControl::default()),
            Err(ServerStoreError::UnsupportedGame)
        ));
        return;
    }
    assert!(matches!(
        reforger.plan_native_install(request.clone(), &ContentStageControl::default()),
        Err(ServerStoreError::NotFound)
    ));
    let root = tempfile::tempdir().unwrap();
    let arma = store(root.path(), "arma3");
    for (branch, account) in [("../public", None), ("public", Some("../alias".to_owned()))] {
        let request = NativeInstallRequest {
            branch: branch.into(),
            account,
            ..request.clone()
        };
        assert!(matches!(
            arma.plan_native_install(request, &ContentStageControl::default()),
            Err(ServerStoreError::Invalid(_))
        ));
    }
    assert!(matches!(
        arma.plan_native_install(request, &ContentStageControl::default()),
        Err(ServerStoreError::NotFound)
    ));
    assert!(!root.path().join("games/alpha").exists());
}

#[test]
#[ignore = "contacts Steam and downloads a small anonymous redistributable"]
fn native_anonymous_install_publishes_verified_content_and_refuses_missing_executables() {
    let external = tempfile::tempdir().unwrap();
    let platform = ServerPlatform::current().unwrap();
    let spec = |directory: &Path, executable: Option<&'static str>| InstallSpec {
        app_id: 1007,
        channel: ContentChannel::Stable,
        branch: "public",
        platform,
        game: ServerDefinitionKind::Arma3,
        directory: directory.to_path_buf().leak(),
        expected: None,
        executable,
        interrupted: &[],
    };

    let published = external.path().join("Redist");
    let control = ContentStageControl::default();
    let outcome = run_install(spec(&published, None), LoginInput::Anonymous, &control);
    assert_eq!(
        outcome.status,
        NativeInstallStatus::PublishedUnbound,
        "{outcome:?}"
    );
    assert!(outcome.published && outcome.files > 0 && outcome.bytes > 0);
    assert_eq!(outcome.staging_removed, Some(true));
    let manifest: ManagedManifest =
        serde_json::from_slice(&fs::read(published.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(manifest.app_id, 1007);
    assert_eq!(manifest.files, outcome.files);
    assert_eq!(control.snapshot().phase, ContentStagePhase::Verifying);
    assert_eq!(control.snapshot().total_bytes, outcome.bytes);
    let siblings: Vec<_> = fs::read_dir(external.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(siblings, vec![std::ffi::OsString::from("Redist")]);
    println!("published files={} bytes={}", outcome.files, outcome.bytes);

    let refused = external.path().join("Refused");
    let outcome = run_install(
        spec(&refused, Some("missing_server.exe")),
        LoginInput::Anonymous,
        &ContentStageControl::default(),
    );
    assert_eq!(outcome.status, NativeInstallStatus::Failed);
    assert_eq!(outcome.staging_removed, Some(true));
    assert!(!refused.exists());

    let cancelled = external.path().join("Cancelled");
    let control = ContentStageControl::default();
    let watcher = control.clone();
    let cancel = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let progress = watcher.snapshot();
            if (progress.phase == ContentStagePhase::Copying && progress.bytes_done > 0)
                || progress.phase == ContentStagePhase::Verifying
                || std::time::Instant::now() > deadline
            {
                watcher.cancel();
                return progress.phase;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    let outcome = run_install(spec(&cancelled, None), LoginInput::Anonymous, &control);
    assert_eq!(cancel.join().unwrap(), ContentStagePhase::Copying);
    assert_eq!(
        outcome.status,
        NativeInstallStatus::Cancelled,
        "{outcome:?}"
    );
    assert_eq!(outcome.staging_removed, Some(true));
    assert!(!cancelled.exists());
    let remaining: Vec<_> = fs::read_dir(external.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(remaining, vec![std::ffi::OsString::from("Redist")]);
}

#[test]
fn committed_managed_manifest_example_matches_the_schema() {
    let example: ManagedManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/managed_installation_manifest.json"
    ))
    .unwrap();
    assert_eq!(example.schema_version, 1);
    assert_eq!(
        example.app_id,
        crate::core::server::distribution::ARMA3_SERVER_APP
    );
    assert_eq!(example.depots.len(), 2);
    assert_eq!(example.executable.path, "arma3server_x64.exe");
    let experimental: ManagedManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/reforger_experimental_installation_manifest.json"
    ))
    .unwrap();
    assert_eq!(experimental.channel, ContentChannel::Experimental);
    assert_eq!(
        experimental.app_id,
        dedicated_app(experimental.game, experimental.channel).unwrap()
    );
    assert_eq!(experimental.platform, ServerPlatform::LinuxX64);
    assert_eq!(experimental.branch, "public");
    assert_eq!(
        experimental.executable.path,
        executable_name(experimental.game, experimental.platform)
    );
}

#[test]
fn publication_never_replaces_a_destination_created_after_review() {
    let external = tempfile::tempdir().unwrap();
    let staging = external
        .path()
        .join(".foxy-install-Server-0000000000000001");
    let destination = external.path().join("Server");
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("server.bin"), b"verified").unwrap();
    fs::create_dir(&destination).unwrap();
    assert!(publish_no_replace(&staging, &destination).is_err());
    assert_eq!(fs::read(staging.join("server.bin")).unwrap(), b"verified");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    fs::remove_dir(&destination).unwrap();
    publish_no_replace(&staging, &destination).unwrap();
    assert!(!staging.exists());
    assert_eq!(
        fs::read(destination.join("server.bin")).unwrap(),
        b"verified"
    );
}

#[cfg(unix)]
#[test]
fn dedicated_executable_gains_execute_bits_matching_its_read_bits() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    for (before, after) in [(0o600, 0o700), (0o644, 0o755), (0o400, 0o500)] {
        let path = root.path().join(format!("server-{before:o}"));
        fs::write(&path, b"elf").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(before)).unwrap();
        mark_executable(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            after
        );
        assert_eq!(fs::read(&path).unwrap(), b"elf");
    }
    assert!(mark_executable(root.path()).is_err());
}
