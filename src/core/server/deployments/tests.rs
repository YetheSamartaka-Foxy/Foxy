use super::*;

#[test]
fn shipped_manifest_and_empty_selection_examples_match_their_formats() {
    for bytes in [
        include_str!("../../../../examples/json/server_backend/arma3_configuration_revision.json"),
        include_str!(
            "../../../../examples/json/server_backend/reforger_configuration_revision.json"
        ),
    ] {
        let bundle: Bundle = serde_json::from_str(bytes).unwrap();
        assert_eq!(bundle.schema_version, 1);
        assert_eq!(
            bundle.preview,
            super::super::configuration::render(&bundle.instance).unwrap()
        );
        assert!(!bundle.preview.launch_ready);
        assert_eq!(bundle.game_space_id, bundle.instance.game_space_id);
    }
    for bytes in [
        include_str!(
            "../../../../examples/json/appdata/games/arma3/server/instances/server_00000000000000000000000000000001/configuration/state.json"
        ),
        include_str!(
            "../../../../examples/json/appdata/games/reforger/server/instances/server_00000000000000000000000000000002/configuration/state.json"
        ),
    ] {
        let state: ConfigurationRevisions = serde_json::from_str(bytes).unwrap();
        assert_eq!(state.schema_version, 1);
        assert_eq!(state.generation, 0);
        assert!(state.active.is_none() && state.revisions.is_empty());
    }
}

fn fixture(root: &Path) -> ServerStore {
    fs::write(
        root.join("games.json"),
        include_str!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let directory = root.join("games/arma3/server");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("servers.json"),
        include_str!("../../../../examples/json/appdata/games/arma3/server/servers.json"),
    )
    .unwrap();
    ServerStore::new(ServerContext::capture(root, Some("arma3")).unwrap()).unwrap()
}

#[test]
fn revision_ids_are_single_strict_components() {
    assert!(valid_revision_id(&format!("config_{}", "a".repeat(64))));
    for value in [
        "../config",
        "config_1",
        &format!("config_{}", "A".repeat(64)),
        &format!("config_{}", "g".repeat(64)),
    ] {
        assert!(!valid_revision_id(value));
    }
}

#[test]
fn plans_and_unowned_apply_do_not_create_configuration_directories() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    let id = store.load().unwrap().instances[0].id.clone();
    let before = fs::read(store.path()).unwrap();
    let plan = store.plan_configuration_commit(&id).unwrap();
    assert_eq!(plan.expected_generation, 0);
    assert_eq!(
        store.apply_configuration_commit(plan),
        Err(ServerStoreError::Ownership)
    );
    assert_eq!(fs::read(store.path()).unwrap(), before);
    assert!(
        !store
            .context()
            .space_dir()
            .join("server/instances")
            .exists()
    );
}

#[test]
fn owned_configuration_plans_reject_stale_state_input_and_modified_artifacts() {
    const PROBE: &str = "FOXY_TEST_CONFIGURATION_STORE_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::server::deployments::tests::owned_configuration_plans_reject_stale_state_input_and_modified_artifacts"])
            .env(PROBE, "1").output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let store = fixture(root.path());
    let id = store.load().unwrap().instances[0].id.clone();
    let claim = store.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let mut forged = store.plan_configuration_commit(&id).unwrap();
    forged.preview.files[0].relative_path = "../outside.cfg".into();
    assert_eq!(
        store.apply_configuration_commit(forged),
        Err(ServerStoreError::StaleRevision)
    );
    assert!(!store.configuration_directory(&id).exists());
    let stale = store.plan_configuration_commit(&id).unwrap();
    let edit = store
        .plan(super::super::DraftChange::Rename {
            id: id.clone(),
            name: "SecondServer".into(),
            expected_revision: 1,
        })
        .unwrap();
    store.apply(edit).unwrap();
    assert_eq!(
        store.apply_configuration_commit(stale),
        Err(ServerStoreError::StaleRevision)
    );
    let first = store.plan_configuration_commit(&id).unwrap();
    let second = first.clone();
    let state = store.apply_configuration_commit(first).unwrap();
    assert_eq!(state.generation, 1);
    assert_eq!(
        store.apply_configuration_commit(second),
        Err(ServerStoreError::StaleRevision)
    );
    let revision = state.active.unwrap();
    let rollback = store.plan_configuration_rollback(&id, &revision).unwrap();
    let commit = store.plan_configuration_commit(&id).unwrap();
    store.apply_configuration_commit(commit).unwrap();
    assert_eq!(
        store.apply_configuration_rollback(rollback),
        Err(ServerStoreError::StaleRevision)
    );
    let pointer = store.configuration_directory(&id).join("state.json");
    let bytes = fs::read(&pointer).unwrap();
    let mut wrong: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    wrong["instance_id"] = serde_json::to_value(ServerInstanceId::new()).unwrap();
    fs::write(&pointer, serde_json::to_vec(&wrong).unwrap()).unwrap();
    assert_eq!(
        store.configuration_revisions(&id),
        Err(ServerStoreError::Corrupt)
    );
    fs::write(&pointer, &bytes).unwrap();
    let unexpected = store
        .configuration_directory(&id)
        .join("revisions")
        .join(&revision)
        .join("unowned.cfg");
    fs::write(&unexpected, b"retained").unwrap();
    assert!(!store.verify_configuration(&id, &revision).unwrap().valid);
    assert!(store.plan_configuration_rollback(&id, &revision).is_err());
    assert_eq!(fs::read(&unexpected).unwrap(), b"retained");
    assert_eq!(fs::read(pointer).unwrap(), bytes);
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
}
