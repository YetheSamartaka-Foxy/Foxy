use super::*;
use crate::core::server::content_staging::ContentStageControl;
use crate::core::server::{DraftChange, ServerContext};
use std::fs;
use std::path::{Path, PathBuf};

const CLAIM_ENV: &str = "FOXY_SERVER_RESTORE_CLAIM_TEST";
const PROCESS_ROOT_ENV: &str = "FOXY_SERVER_RESTORE_PROCESS_ROOT";
const PROCESS_INSTANCE_ENV: &str = "FOXY_SERVER_RESTORE_PROCESS_INSTANCE";
const PROCESS_LOSS_EXIT: i32 = 83;
const PROCESS_LOSS_CHILD: &str =
    "core::server::backup::restore::tests::restore_process_loss_child_exits_after_prefix_promotion";

fn run_claimed_test(test_name: &str) -> bool {
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

fn setup_store() -> (
    tempfile::TempDir,
    ServerStore,
    ServerInstanceId,
    crate::core::tasks::db_process_lock::HeldClaimPin,
) {
    setup_store_for("arma3")
}

fn setup_store_for(
    game_id: &str,
) -> (
    tempfile::TempDir,
    ServerStore,
    ServerInstanceId,
    crate::core::tasks::db_process_lock::HeldClaimPin,
) {
    let root = tempfile::tempdir().unwrap();
    let space_id = "space-restore-test";
    fs::write(
        root.path().join("games.json"),
        serde_json::to_vec(&serde_json::json!({
            "active_game_space_id": space_id,
            "last_opened_game_space_id": space_id,
            "game_spaces": [{
                "id": space_id,
                "game_id": game_id,
                "display_name": "Restore test",
                "created_at": 0
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::create_dir_all(root.path().join("games").join(space_id)).unwrap();
    let context = ServerContext::capture(root.path(), Some(space_id)).unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&context.database_path()),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let pin =
        crate::core::tasks::db_process_lock::pin_held_claim(&context.database_path()).unwrap();
    let store = ServerStore::new(context).unwrap();
    let plan = store
        .plan(DraftChange::Add {
            name: "Restore test server".into(),
        })
        .unwrap();
    let id = plan.instance.id.clone();
    store.apply(plan).unwrap();
    (root, store, id, pin)
}

fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[test]
fn restore_paths_and_disk_budget_reject_ambiguous_or_overflowing_inputs() {
    for path in [
        "../escape",
        "home/profiles/NUL.save",
        "home/profiles/name. ",
        "home/profiles/a:b",
        "home/profiles/COM1",
        "home\\profiles\\file",
    ] {
        assert!(!portable_path(path), "{path}");
    }
    assert_eq!(save_target("saves/ownership.json"), Ok(None));
    assert_eq!(
        save_target("saves/home/profiles/profile/save.bin"),
        Ok(Some("home/profiles/profile/save.bin"))
    );
    assert!(save_target("saves/home/private-configuration/server.cfg").is_err());
    assert!(save_target("saves/headless/not-an-hc/home/profiles/save.bin").is_err());
    assert_eq!(required_budget(10, 20, 30, 40).unwrap(), DISK_MARGIN + 100);
    assert_eq!(
        required_budget(u64::MAX, 1, 1, 1),
        Err(ServerStoreError::Corrupt)
    );
}

#[test]
fn cancellation_after_one_atomic_promotion_keeps_a_recoverable_exact_prefix() {
    if !run_claimed_test(
        "core::server::backup::restore::tests::cancellation_after_one_atomic_promotion_keeps_a_recoverable_exact_prefix",
    ) {
        return;
    }
    let (_root, store, id, _pin) = setup_store();
    let state = store.record_root(&id).join("runtime-state");
    let first = "home/profiles/first.save";
    let second = "home/profiles/second.save";
    let before = b"prior";
    let after = b"restore";
    for path in [first, second] {
        write_bytes(&state, path, before);
    }
    let (journal, fingerprint, tx) = journal_for(
        &store,
        &id,
        vec![
            entry(first, after, Some(before)),
            entry(second, after, Some(before)),
        ],
    );
    for path in [first, second] {
        write_bytes(&tx.join("before"), path, before);
        write_bytes(&tx.join("validated/saves"), path, after);
    }
    let control = ContentStageControl::default();
    control.cancel_after_copy_bytes_for_test(after.len() as u64 + 1);
    assert_eq!(
        store.promote_restore(&journal, &tx, &control),
        Err(ServerStoreError::Cancelled)
    );
    assert_eq!(fs::read(state.join(first)).unwrap(), after);
    assert_eq!(fs::read(state.join(second)).unwrap(), before);
    assert!(store.require_no_server_restore(&id).is_err());
    assert!(store.server_restore_status(&id).unwrap().pending.is_some());
    store
        .rollback_server_restore(&id, &fingerprint, None, &ContentStageControl::default())
        .unwrap();
    for path in [first, second] {
        assert_eq!(fs::read(state.join(path)).unwrap(), before);
    }
    assert!(store.server_restore_status(&id).unwrap().pending.is_none());
}

#[test]
#[ignore = "Subprocess helper for process-loss restore recovery acceptance"]
fn restore_process_loss_child_exits_after_prefix_promotion() {
    let Ok(root) = std::env::var(PROCESS_ROOT_ENV) else {
        panic!("This ignored child requires {PROCESS_ROOT_ENV} from its parent test.");
    };
    let id = ServerInstanceId::try_from(
        std::env::var(PROCESS_INSTANCE_ENV)
            .expect("parent provides the instance id for the restore child"),
    )
    .expect("parent provides a valid instance id");
    let context = ServerContext::capture(Path::new(&root), Some("space-restore-test")).unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&context.database_path()),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let _pin =
        crate::core::tasks::db_process_lock::pin_held_claim(&context.database_path()).unwrap();
    let store = ServerStore::new(context).unwrap();
    store.show(&id).unwrap();

    let state = store.record_root(&id).join("runtime-state");
    let first = "home/profiles/first.save";
    let second = "home/profiles/second.save";
    let unknown = "home/profiles/operator.keep";
    let before = b"prior";
    let after = b"restore";
    write_bytes(&state, first, before);
    write_bytes(&state, second, before);
    write_bytes(&state, unknown, b"unknown sibling");
    let (journal, _, tx) = journal_for(
        &store,
        &id,
        vec![
            entry(first, after, Some(before)),
            entry(second, after, Some(before)),
        ],
    );
    for path in [first, second] {
        write_bytes(&tx.join("before"), path, before);
        write_bytes(&tx.join("validated/saves"), path, after);
    }

    let control = ContentStageControl::default();
    control.cancel_after_copy_bytes_for_test(after.len() as u64 + 1);
    assert_eq!(
        store.promote_restore(&journal, &tx, &control),
        Err(ServerStoreError::Cancelled)
    );
    assert_eq!(fs::read(state.join(first)).unwrap(), after);
    assert_eq!(fs::read(state.join(second)).unwrap(), before);
    assert_eq!(fs::read(state.join(unknown)).unwrap(), b"unknown sibling");
    assert!(store.server_restore_status(&id).unwrap().pending.is_some());

    std::process::exit(PROCESS_LOSS_EXIT);
}

#[test]
fn process_loss_after_promotion_prefix_recovers_from_persisted_journal() {
    let test_name = "core::server::backup::restore::tests::process_loss_after_promotion_prefix_recovers_from_persisted_journal";
    if !run_claimed_test(test_name) {
        return;
    }
    let (root, store, id, pin) = setup_store();
    let database_path = store.context().database_path();
    let root_path = root.path().to_path_buf();
    let instance_id = id.as_str().to_owned();
    drop(store);
    drop(pin);
    crate::core::tasks::db_process_lock::release_for(&database_path).unwrap();

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", PROCESS_LOSS_CHILD, "--ignored", "--nocapture"])
        .env(PROCESS_ROOT_ENV, &root_path)
        .env(PROCESS_INSTANCE_ENV, instance_id)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(PROCESS_LOSS_EXIT),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );

    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&database_path),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let pin = crate::core::tasks::db_process_lock::pin_held_claim(&database_path).unwrap();
    let context = ServerContext::capture(&root_path, Some("space-restore-test")).unwrap();
    let store = ServerStore::new(context).unwrap();
    let state = store.record_root(&id).join("runtime-state");
    let first = "home/profiles/first.save";
    let second = "home/profiles/second.save";
    let unknown = "home/profiles/operator.keep";
    let before = b"prior";
    let after = b"restore";
    assert_eq!(fs::read(state.join(first)).unwrap(), after);
    assert_eq!(fs::read(state.join(second)).unwrap(), before);
    assert_eq!(fs::read(state.join(unknown)).unwrap(), b"unknown sibling");

    let status = store.server_restore_status(&id).unwrap();
    let pending = status
        .pending
        .expect("child exit leaves a durable recovery journal");
    store
        .rollback_server_restore(
            &id,
            &pending.fingerprint,
            None,
            &ContentStageControl::default(),
        )
        .unwrap();
    for path in [first, second] {
        assert_eq!(fs::read(state.join(path)).unwrap(), before);
    }
    assert_eq!(fs::read(state.join(unknown)).unwrap(), b"unknown sibling");
    assert!(store.server_restore_status(&id).unwrap().pending.is_none());
    drop(pin);
    crate::core::tasks::db_process_lock::release_for(&database_path).unwrap();
}

fn journal_for(
    store: &ServerStore,
    id: &ServerInstanceId,
    entries: Vec<RestoreEntry>,
) -> (RestoreJournal, String, PathBuf) {
    let journal = RestoreJournal {
        schema_version: 1,
        game_space_id: store.context().space_id().to_owned(),
        game_id: store.context().game_id().to_owned(),
        instance_id: id.clone(),
        restore_id: format!("restore_{}", "a".repeat(64)),
        backup_id: format!("backup_{}", "b".repeat(64)),
        plan_fingerprint: "c".repeat(64),
        entries,
    };
    let bytes = serde_json::to_vec_pretty(&journal).unwrap();
    let fingerprint = blake3::hash(&bytes).to_hex().to_string();
    let restore_root = store.restore_root(id);
    let tx = restore_root.join(&journal.restore_id);
    fs::create_dir_all(&tx).unwrap();
    fs::write(restore_root.join("journal.json"), bytes).unwrap();
    (journal, fingerprint, tx)
}

fn entry(path: &str, restored: &[u8], prior: Option<&[u8]>) -> RestoreEntry {
    RestoreEntry {
        path: path.into(),
        bytes: restored.len() as u64,
        blake3: content_hash(restored),
        prior: prior.map(|bytes| (bytes.len() as u64, content_hash(bytes))),
    }
}

fn write_bytes(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn rollback_restores_preimages_removes_new_saves_and_preserves_unknown_siblings() {
    let test_name = "core::server::backup::restore::tests::rollback_restores_preimages_removes_new_saves_and_preserves_unknown_siblings";
    if !run_claimed_test(test_name) {
        return;
    }
    let (_root, store, id, _pin) = setup_store();
    let state = store.record_root(&id).join("runtime-state");
    let old_path = "home/profiles/profile/Saved/old.save";
    let new_path = "home/profiles/profile/Saved/new.save";
    let unknown_path = "home/profiles/profile/Saved/operator.keep";
    let old_before = b"operator's prior save";
    let old_restored = b"backup's replacement save";
    let new_restored = b"new save from backup";
    write_bytes(&state, old_path, old_restored);
    write_bytes(&state, new_path, new_restored);
    write_bytes(&state, unknown_path, b"operator-owned sibling");

    let (_, fingerprint, tx) = journal_for(
        &store,
        &id,
        vec![
            entry(old_path, old_restored, Some(old_before)),
            entry(new_path, new_restored, None),
        ],
    );
    write_bytes(&tx.join("before"), old_path, old_before);
    let pending = store.server_restore_status(&id).unwrap();
    assert!(pending.pending.is_some());

    let outcome = store
        .rollback_server_restore(&id, &fingerprint, None, &ContentStageControl::default())
        .unwrap();

    assert!(outcome.rolled_back);
    assert!(outcome.writes_performed);
    assert_eq!(fs::read(state.join(old_path)).unwrap(), old_before);
    assert!(!state.join(new_path).exists());
    assert_eq!(
        fs::read(state.join(unknown_path)).unwrap(),
        b"operator-owned sibling"
    );
    assert!(store.server_restore_status(&id).unwrap().pending.is_none());
    assert_eq!(
        store
            .load_server_restore_outcome(&id, &outcome.restore_id, &ContentStageControl::default())
            .unwrap(),
        outcome
    );
    let receipt = store
        .restore_root(&id)
        .join("receipts")
        .join(format!("{}.json", outcome.restore_id));
    let mut foreign = outcome.clone();
    foreign.game_space_id = "foreign-space".into();
    fs::write(&receipt, serde_json::to_vec_pretty(&foreign).unwrap()).unwrap();
    assert_eq!(
        store.load_server_restore_outcome(
            &id,
            &outcome.restore_id,
            &ContentStageControl::default()
        ),
        Err(ServerStoreError::Corrupt)
    );

    let retry =
        store.rollback_server_restore(&id, &fingerprint, None, &ContentStageControl::default());
    assert_eq!(retry, Err(ServerStoreError::NotFound));
    assert_eq!(fs::read(state.join(old_path)).unwrap(), old_before);
    assert_eq!(
        fs::read(state.join(unknown_path)).unwrap(),
        b"operator-owned sibling"
    );
}

#[test]
fn rollback_refuses_changed_destinations_without_mutation_and_keeps_recovery_pending() {
    let test_name = "core::server::backup::restore::tests::rollback_refuses_changed_destinations_without_mutation_and_keeps_recovery_pending";
    if !run_claimed_test(test_name) {
        return;
    }
    let (_root, store, id, _pin) = setup_store();
    let state = store.record_root(&id).join("runtime-state");
    let path = "home/profiles/profile/Saved/save.dat";
    let prior = b"pre-restore bytes";
    let restored = b"backup bytes";
    let external = b"changed by another writer";
    write_bytes(&state, path, external);
    let (_, fingerprint, tx) = journal_for(&store, &id, vec![entry(path, restored, Some(prior))]);
    write_bytes(&tx.join("before"), path, prior);

    assert!(matches!(
        store.rollback_server_restore(&id, &fingerprint, None, &ContentStageControl::default()),
        Err(ServerStoreError::Invalid(_))
    ));
    assert_eq!(fs::read(state.join(path)).unwrap(), external);
    assert!(store.server_restore_status(&id).unwrap().pending.is_some());
}

#[test]
fn rollback_refuses_a_missing_preimage_before_writing_any_destination() {
    let test_name = "core::server::backup::restore::tests::rollback_refuses_a_missing_preimage_before_writing_any_destination";
    if !run_claimed_test(test_name) {
        return;
    }
    let (_root, store, id, _pin) = setup_store();
    let state = store.record_root(&id).join("runtime-state");
    let path = "home/profiles/profile/Saved/save.dat";
    let prior = b"pre-restore bytes";
    let restored = b"backup bytes";
    write_bytes(&state, path, restored);
    let (_, fingerprint, _tx) = journal_for(&store, &id, vec![entry(path, restored, Some(prior))]);

    assert!(matches!(
        store.rollback_server_restore(&id, &fingerprint, None, &ContentStageControl::default()),
        Err(ServerStoreError::Corrupt)
    ));
    assert_eq!(fs::read(state.join(path)).unwrap(), restored);
    assert!(store.server_restore_status(&id).unwrap().pending.is_some());
}

#[test]
fn cancelled_recovery_keeps_journal_and_succeeds_when_retried() {
    let test_name = "core::server::backup::restore::tests::cancelled_recovery_keeps_journal_and_succeeds_when_retried";
    if !run_claimed_test(test_name) {
        return;
    }
    let (_root, store, id, _pin) = setup_store();
    let state = store.record_root(&id).join("runtime-state");
    let path = "home/profiles/profile/Saved/save.dat";
    let prior = b"pre-restore bytes";
    let restored = b"backup bytes";
    write_bytes(&state, path, restored);
    let (_, fingerprint, tx) = journal_for(&store, &id, vec![entry(path, restored, Some(prior))]);
    write_bytes(&tx.join("before"), path, prior);

    let cancelled = ContentStageControl::default();
    cancelled.cancel();
    assert_eq!(
        store.rollback_server_restore(&id, &fingerprint, None, &cancelled),
        Err(ServerStoreError::Cancelled)
    );
    assert_eq!(fs::read(state.join(path)).unwrap(), restored);
    assert!(store.server_restore_status(&id).unwrap().pending.is_some());

    store
        .rollback_server_restore(&id, &fingerprint, None, &ContentStageControl::default())
        .unwrap();
    assert_eq!(fs::read(state.join(path)).unwrap(), prior);
    assert!(store.server_restore_status(&id).unwrap().pending.is_none());
}

#[test]
fn restore_status_rejects_cross_instance_and_escaping_journals() {
    let test_name = "core::server::backup::restore::tests::restore_status_rejects_cross_instance_and_escaping_journals";
    if !run_claimed_test(test_name) {
        return;
    }
    let (_root, store, id, _pin) = setup_store();
    let root = store.restore_root(&id);
    fs::create_dir_all(&root).unwrap();
    let mut journal = RestoreJournal {
        schema_version: 1,
        game_space_id: store.context().space_id().into(),
        game_id: store.context().game_id().into(),
        instance_id: ServerInstanceId::new(),
        restore_id: format!("restore_{}", "a".repeat(64)),
        backup_id: format!("backup_{}", "b".repeat(64)),
        plan_fingerprint: "c".repeat(64),
        entries: vec![entry("home/profiles/profile/Saved/file", b"new", None)],
    };
    fs::write(
        root.join("journal.json"),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store.server_restore_status(&id),
        Err(ServerStoreError::Corrupt)
    ));

    journal.instance_id = id;
    journal.entries[0].path = "../outside".into();
    fs::write(
        root.join("journal.json"),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store.server_restore_status(&journal.instance_id),
        Err(ServerStoreError::UnsafePath | ServerStoreError::Corrupt)
    ));
}

#[test]
fn archive_review_refuses_changed_engine_channel_configuration_provider_and_live_consistency() {
    if !run_claimed_test(
        "core::server::backup::restore::tests::archive_review_refuses_changed_engine_channel_configuration_provider_and_live_consistency",
    ) {
        return;
    }
    let (_root, store, id, _pin) = setup_store_for("reforger");
    let state = store.record_root(&id).join("runtime-state");
    write_bytes(
        &state,
        "home/profiles/state.save",
        b"preserved current save",
    );
    let outcome = store
        .create_server_backup(
            &id,
            BackupConsistencyMode::StoppedVerified,
            None,
            false,
            None,
            &ContentStageControl::default(),
        )
        .unwrap();
    let backup_id = &outcome.manifest.backup_id;
    let dir = store.backups_root(&id).join(backup_id);
    let original_zip = fs::read(dir.join("archive.zip")).unwrap();
    let baseline = store
        .plan_server_restore(&id, backup_id, None, &ContentStageControl::default())
        .unwrap();
    assert!(
        !baseline
            .blockers
            .iter()
            .any(|reason| reason.starts_with("Engine,")
                || reason.starts_with("The active configuration")
                || reason.starts_with("Persistence provider"))
    );
    for boundary in [
        "engine",
        "channel",
        "configuration",
        "provider",
        "save_tree",
        "live",
    ] {
        let mut manifest = outcome.manifest.clone();
        let expected = match boundary {
            "engine" => {
                manifest.engine_set.engine_build_id = Some("different-build".into());
                "Engine,"
            }
            "channel" => {
                manifest.engine_set.channel =
                    Some(crate::core::server::ContentChannel::Experimental);
                "Engine,"
            }
            "configuration" => {
                manifest.configuration_set.active_configuration_revision_id =
                    Some(format!("config_{}", "d".repeat(64)));
                "The active configuration"
            }
            "provider" => {
                manifest
                    .provider_set
                    .covered_providers
                    .push("unsupported_external_store".into());
                "Persistence provider"
            }
            "save_tree" => {
                manifest.provider_set.reforger_save_tree_version = None;
                "This Reforger backup predates"
            }
            _ => {
                manifest.consistency.mode = BackupConsistencyMode::BestEffortLive;
                manifest.consistency.stopped_verified = false;
                manifest.provider_set.writer_stopped_verified = false;
                "This backup has no stopped-consistency witness"
            }
        };
        let mut source = ZipArchive::new(std::io::Cursor::new(&original_zip)).unwrap();
        let temp = tempfile::NamedTempFile::new_in(&dir).unwrap();
        let mut writer = zip::ZipWriter::new(temp);
        for index in 0..source.len() {
            let mut entry = source.by_index(index).unwrap();
            let name = entry.name().unwrap().into_owned();
            writer
                .start_file(&name, zip::write::SimpleFileOptions::default())
                .unwrap();
            if name == "manifest.json" {
                writer
                    .write_all(&serde_json::to_vec_pretty(&manifest).unwrap())
                    .unwrap();
            } else {
                std::io::copy(&mut entry, &mut writer).unwrap();
            }
        }
        writer
            .finish()
            .unwrap()
            .persist(dir.join("archive.zip"))
            .unwrap();
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let plan = store
            .plan_server_restore(&id, backup_id, None, &ContentStageControl::default())
            .unwrap();
        assert!(
            plan.blockers
                .iter()
                .any(|reason| reason.starts_with(expected)),
            "{boundary}: {:?}",
            plan.blockers
        );
        assert_eq!(
            fs::read(state.join("home/profiles/state.save")).unwrap(),
            b"preserved current save"
        );
        assert!(!store.restore_root(&id).exists());
    }
}
