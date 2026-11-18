use super::model::ServerBackupManifest;
use super::*;
use crate::core::server::content_staging::ContentStageControl;
use crate::core::server::{
    DraftChange, InstanceRuntimePhase, InstanceRuntimeStatus, ServerCatalog, ServerContext,
    ServerDefinitionKind, ServerInstanceId, ServerStore, ServerStoreError,
};
use std::fs;
use std::io::Write;
use std::path::Path;

fn ready() -> BackupEvidence {
    BackupEvidence {
        archive_present: false,
        engine_compatible: true,
        config_compatible: true,
        content_compatible: true,
        provider_compatible: true,
    }
}

#[test]
fn missing_compatibility_refuses_the_backup() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("runtime");
    let backup = root.path().join("backups");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("server.cfg"), b"hostname = \"Main\";\n").unwrap();
    let mut evidence = ready();
    evidence.engine_compatible = false;
    assert_eq!(
        backup_owned_files(&source, &backup, &evidence),
        Err(BackupError::Incompatible)
    );
    assert!(!backup.exists());
    backup_owned_files(&source, &backup, &ready()).unwrap();
    assert_eq!(
        fs::read_to_string(backup.join("server.cfg")).unwrap(),
        "hostname = \"Main\";\n"
    );
}

fn run_claimed_test(test_name: &str) -> bool {
    const CLAIM_ENV: &str = "FOXY_SERVER_BACKUP_CLAIM_TEST";
    if std::env::var_os(CLAIM_ENV).is_some() {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env(CLAIM_ENV, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn setup_store(
    game_id: &str,
) -> (
    tempfile::TempDir,
    ServerStore,
    ServerInstanceId,
    crate::core::tasks::db_process_lock::HeldClaimPin,
) {
    let root = tempfile::tempdir().unwrap();
    let space_id = format!("space-{game_id}");
    fs::write(
        root.path().join("games.json"),
        serde_json::to_vec(&serde_json::json!({
            "active_game_space_id": space_id,
            "last_opened_game_space_id": space_id,
            "game_spaces": [{
                "id": space_id,
                "game_id": game_id,
                "display_name": format!("Test {game_id}"),
                "created_at": 0
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::create_dir_all(root.path().join("games").join(&space_id)).unwrap();
    let context = ServerContext::capture(root.path(), Some(&space_id)).unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&context.database_path()),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let pin =
        crate::core::tasks::db_process_lock::pin_held_claim(&context.database_path()).unwrap();
    let store = ServerStore::new(context).unwrap();
    let plan = store
        .plan(DraftChange::Add {
            name: format!("Main {game_id}"),
        })
        .unwrap();
    let id = plan.instance.id.clone();
    store.apply(plan).unwrap();
    (root, store, id, pin)
}

fn seed_runtime_files(root: &Path, space_id: &str, id: &ServerInstanceId, game_id: &str) {
    let base = root
        .join("games")
        .join(space_id)
        .join("server/instances")
        .join(id.as_str());
    let state = base.join("deployments/runtime-state");
    fs::create_dir_all(state.join("home/profiles/Users/FoxyServer")).unwrap();
    fs::create_dir_all(state.join("home/profiles/profile/Saved")).unwrap();
    fs::create_dir_all(state.join("home/config")).unwrap();
    fs::create_dir_all(state.join("home/cache")).unwrap();
    fs::create_dir_all(state.join("home/runtime")).unwrap();
    fs::create_dir_all(state.join("home/data")).unwrap();
    fs::create_dir_all(state.join("home/private-configuration/runtime_a")).unwrap();
    fs::create_dir_all(state.join("home/battleye")).unwrap();
    fs::create_dir_all(state.join("headless/hc_0000000000000001/home/profiles")).unwrap();
    fs::create_dir_all(state.join("headless/hc_0000000000000001/private")).unwrap();

    fs::write(
        state.join("home/profiles/Users/FoxyServer/FoxyServer.Arma3Profile"),
        format!("// profile for {game_id}\n").as_bytes(),
    )
    .unwrap();
    fs::write(
        state.join("home/profiles/profile/Saved/session.save"),
        b"save-payload-v1",
    )
    .unwrap();
    for relative in [
        "home/profiles/profile/.save/game/Scenario/playthrough001/savepoint000/meta-info.json",
        "home/profiles/profile/.save/game/Scenario/playthrough001/savepoint000/WorldState/11111111-1111-1111-1111-111111111111.blob",
        "home/profiles/profile/.save/gamemode/shared-state.blob",
        "home/profiles/profile/.save/settings/ReforgerGameSettings.conf",
    ] {
        let path = state.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"opaque-native-persistence-fixture").unwrap();
    }
    fs::write(state.join("home/config/settings.json"), b"{\"a\":1}\n").unwrap();
    fs::write(state.join("home/cache/temp.bin"), b"transient-cache").unwrap();
    fs::write(state.join("home/runtime/server.log"), b"transient-log").unwrap();
    fs::write(state.join("home/data/addon_pack.pak"), b"downloaded-addon").unwrap();
    fs::write(state.join("run_owner_1.log"), b"transient-owner-log").unwrap();

    // Plaintext materialized secret files must always be excluded from backups.
    fs::write(
        state.join("home/private-configuration/runtime_a/server.cfg"),
        b"passwordAdmin = \"secret-plaintext-admin\";\n",
    )
    .unwrap();
    fs::write(
        state.join("home/battleye/BEServer_x64.cfg"),
        b"RConPassword secret-rcon-pw\n",
    )
    .unwrap();
    fs::write(
        state.join("headless/hc_0000000000000001/join.par"),
        b"-password=secret-join-pw\n",
    )
    .unwrap();
    fs::write(
        state.join("headless/hc_0000000000000001/private/ownerToken.bin"),
        b"token-secret-bytes",
    )
    .unwrap();
    fs::write(
        state.join("home/profiles/ownerToken.bin"),
        b"owner-token-secret",
    )
    .unwrap();
    fs::write(
        state.join("headless/hc_0000000000000001/home/profiles/hc.Arma3Profile"),
        b"hc-profile\n",
    )
    .unwrap();
}

#[test]
fn stopped_consistency_is_required_by_default_and_excludes_secrets_and_transients() {
    if !run_claimed_test(
        "core::server::backup::tests::stopped_consistency_is_required_by_default_and_excludes_secrets_and_transients",
    ) {
        return;
    }
    let (root, store, id, _pin) = setup_store("arma3");
    seed_runtime_files(root.path(), store.context().space_id(), &id, "arma3");
    let control = ContentStageControl::default();

    let running_status = InstanceRuntimeStatus {
        instance_id: id.clone(),
        owner_id: "owner_test_1".into(),
        owner_generation: 3,
        run_generation: 7,
        activation_generation: Some(2),
        runtime_id: Some("runtime_test".into()),
        phase: InstanceRuntimePhase::Running,
        process_id: Some(4242),
        process_exists: true,
        readiness_verified: true,
        cleanup_verified: false,
        stop_timed_out: false,
        forced: false,
        root_exit_code: None,
        observation_age_ms: 0,
        monitoring_fingerprint: None,
        headless_running: 0,
    };

    let blocked_plan = store
        .plan_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            Some(&running_status),
            &control,
        )
        .unwrap();
    assert!(!blocked_plan.consistency.stopped_verified);
    assert!(!blocked_plan.blockers.is_empty());
    assert!(matches!(
        store.create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            None,
            false,
            Some(&running_status),
            &control,
        ),
        Err(ServerStoreError::Invalid(_))
    ));

    // Explicit best-effort live backup succeeds while running and clearly marks stopped_verified=false.
    let live = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::BestEffortLive,
            None,
            false,
            Some(&running_status),
            &control,
        )
        .unwrap();
    assert_eq!(
        live.manifest.consistency.mode,
        BackupConsistencyMode::BestEffortLive
    );
    assert!(!live.manifest.consistency.stopped_verified);

    // Exited + verified cleanup allows default StoppedVerified backup.
    let stopped_status = InstanceRuntimeStatus {
        phase: InstanceRuntimePhase::Exited,
        process_id: None,
        process_exists: false,
        readiness_verified: false,
        cleanup_verified: true,
        root_exit_code: Some(0),
        ..running_status
    };
    let plan = store
        .plan_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            Some(&stopped_status),
            &control,
        )
        .unwrap();
    assert!(plan.consistency.stopped_verified);
    assert!(plan.blockers.is_empty());

    let outcome = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            Some(&plan.fingerprint),
            false,
            Some(&stopped_status),
            &control,
        )
        .unwrap();
    assert!(outcome.verified);
    assert_ne!(outcome.manifest.backup_id, live.manifest.backup_id);

    let archive_paths: Vec<&str> = outcome
        .manifest
        .entries
        .iter()
        .map(|entry| entry.archive_path.as_str())
        .collect();
    assert!(archive_paths.contains(&"metadata/instance.json"));
    assert!(archive_paths.contains(&"metadata/installation.json"));
    assert!(archive_paths.contains(&"metadata/provider_coverage.json"));
    assert!(
        archive_paths.contains(&"saves/home/profiles/Users/FoxyServer/FoxyServer.Arma3Profile")
    );
    assert!(
        archive_paths.contains(&"saves/headless/hc_0000000000000001/home/profiles/hc.Arma3Profile")
    );

    for forbidden in [
        "private-configuration",
        "BEServer",
        "join.par",
        "ownerToken",
        "home/cache",
        "home/runtime",
        "home/data",
        "run_owner_1.log",
    ] {
        assert!(
            archive_paths.iter().all(|path| !path.contains(forbidden)),
            "archive unexpectedly contained {forbidden}: {archive_paths:?}"
        );
    }

    let report = store
        .verify_server_backup(&id, &outcome.manifest.backup_id, &control)
        .unwrap();
    assert!(report.valid);
    assert_eq!(report.verified_files, outcome.manifest.total_files);
}

#[test]
fn verify_rejects_tampered_archive_and_path_traversal() {
    if !run_claimed_test(
        "core::server::backup::tests::verify_rejects_tampered_archive_and_path_traversal",
    ) {
        return;
    }
    let (root, store, id, _pin) = setup_store("reforger");
    seed_runtime_files(root.path(), store.context().space_id(), &id, "reforger");
    let control = ContentStageControl::default();

    let outcome = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            None,
            false,
            None,
            &control,
        )
        .unwrap();
    assert_eq!(
        outcome.manifest.provider_set.covered_providers,
        vec![
            "reforger_local_session_storage",
            "reforger_local_gamemode_storage",
        ]
    );
    assert_eq!(
        outcome.manifest.provider_set.reforger_save_tree_version,
        Some(1)
    );
    for suffix in [
        ".save/game/Scenario/playthrough001/savepoint000/meta-info.json",
        ".save/game/Scenario/playthrough001/savepoint000/WorldState/11111111-1111-1111-1111-111111111111.blob",
        ".save/gamemode/shared-state.blob",
        ".save/settings/ReforgerGameSettings.conf",
    ] {
        assert!(
            outcome.manifest.entries.iter().any(|entry| {
                entry.set == super::model::BackupSetKind::Save
                    && entry.archive_path.ends_with(suffix)
            }),
            "hidden local save tree missing {suffix}"
        );
    }

    let backup_dir = root
        .path()
        .join("games")
        .join(store.context().space_id())
        .join("server/instances")
        .join(id.as_str())
        .join("backups")
        .join(&outcome.manifest.backup_id);

    // Replace archive.zip with an archive containing a traversal entry.
    let archive_path = backup_dir.join("archive.zip");
    let mut bad_bytes = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bad_bytes));
        let opts = zip::write::SimpleFileOptions::default();
        writer.start_file("../escape.txt", opts).unwrap();
        writer.write_all(b"evil").unwrap();
        writer.finish().unwrap();
    }
    fs::write(&archive_path, &bad_bytes).unwrap();
    assert!(matches!(
        store.verify_server_backup(&id, &outcome.manifest.backup_id, &control),
        Err(ServerStoreError::Corrupt | ServerStoreError::UnsafePath)
    ));
}

#[test]
fn retention_preserves_pinned_and_latest_stopped_consistent_backup() {
    if !run_claimed_test(
        "core::server::backup::tests::retention_preserves_pinned_and_latest_stopped_consistent_backup",
    ) {
        return;
    }
    let (_root, store, id, _pin) = setup_store("arma3");
    let control = ContentStageControl::default();

    let b1_stopped = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            None,
            false,
            None,
            &control,
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b2_stopped = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            None,
            false,
            None,
            &control,
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b3_live = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::BestEffortLive,
            None,
            false,
            None,
            &control,
        )
        .unwrap();

    // Pin b1_stopped (oldest).
    let retention = store
        .set_backup_pin(&id, &b1_stopped.manifest.backup_id, true, 0)
        .unwrap();
    assert_eq!(retention.generation, 1);

    // Configure keep_last = 1.
    let retention = store
        .set_backup_retention_policy(
            &id,
            BackupRetentionPolicy {
                keep_last: 1,
                max_total_bytes: None,
            },
            retention.generation,
        )
        .unwrap();
    assert_eq!(retention.generation, 2);

    // Even with keep_last=1, b1 (pinned), b2 (latest stopped-consistent), and b3 (newest) stay!
    let prune_plan = store.plan_backup_prune(&id, None, &control).unwrap();
    assert!(prune_plan.candidates.is_empty());
    let retained_ids: Vec<&str> = prune_plan
        .retained
        .iter()
        .map(|item| item.backup_id.as_str())
        .collect();
    assert!(retained_ids.contains(&b1_stopped.manifest.backup_id.as_str()));
    assert!(retained_ids.contains(&b2_stopped.manifest.backup_id.as_str()));
    assert!(retained_ids.contains(&b3_live.manifest.backup_id.as_str()));

    // Now create a newer StoppedVerified backup b4_stopped: with keep_last=1, b2 & b3 become prunable!
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b4_stopped = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            None,
            false,
            None,
            &control,
        )
        .unwrap();
    let prune_plan = store.plan_backup_prune(&id, None, &control).unwrap();
    let candidate_ids: Vec<&str> = prune_plan
        .candidates
        .iter()
        .map(|item| item.backup_id.as_str())
        .collect();
    assert_eq!(candidate_ids.len(), 2);
    assert!(candidate_ids.contains(&b2_stopped.manifest.backup_id.as_str()));
    assert!(candidate_ids.contains(&b3_live.manifest.backup_id.as_str()));

    // Stale fingerprint fails safely.
    assert_eq!(
        store.apply_backup_prune(&id, &"0".repeat(64), None, &control),
        Err(ServerStoreError::StaleRevision)
    );

    let outcome = store
        .apply_backup_prune(&id, &prune_plan.fingerprint, None, &control)
        .unwrap();
    assert_eq!(outcome.removed.len(), 2);
    assert_eq!(
        store
            .load_prune_receipt(&id, &outcome.receipt_id, &control)
            .unwrap(),
        outcome
    );

    let listing = store.list_server_backups(&id).unwrap();
    let remaining_ids: Vec<&str> = listing
        .backups
        .iter()
        .map(|item| item.backup_id.as_str())
        .collect();
    assert_eq!(remaining_ids.len(), 2);
    assert!(remaining_ids.contains(&b1_stopped.manifest.backup_id.as_str()));
    assert!(remaining_ids.contains(&b4_stopped.manifest.backup_id.as_str()));

    // Unknown or corrupt directory left untouched by prune.
    let unknown_dir = store.backups_root(&id).join("backup_custom_note");
    fs::create_dir_all(&unknown_dir).unwrap();
    let listing_with_unknown = store.list_server_backups(&id).unwrap();
    assert_eq!(
        listing_with_unknown.unrecognized_entries,
        vec!["backup_custom_note"]
    );
}

#[test]
fn public_backup_example_matches_manifest_contract() {
    let manifest: ServerBackupManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_backup_manifest.json"
    ))
    .unwrap();
    manifest.validate().unwrap();
}

#[test]
fn unused_catalog_helper_keeps_type_check() {
    let _ = ServerCatalog {
        schema_version: 2,
        game_space_id: "space".into(),
        game_id: "arma3".into(),
        revision: 0,
        instances: Vec::new(),
        installations: Vec::new(),
    };
    let _ = ServerDefinitionKind::Arma3;
}
