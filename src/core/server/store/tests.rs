use super::*;
use serde_json::json;

fn fixture(root: &Path) {
    fs::write(
        root.join("games.json"),
        serde_json::to_vec(&json!({
            "active_game_space_id": "alpha", "last_opened_game_space_id": "alpha",
            "game_spaces": [
                {"id":"alpha", "game_id":"arma3", "display_name":"Alpha", "created_at":0},
                {"id":"beta", "game_id":"reforger", "display_name":"Beta", "created_at":0},
                {"id":"other", "game_id":"generic", "display_name":"Other", "created_at":0}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn shipped_catalog_examples_match_the_runtime_schema() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    for (space, bytes) in [
        (
            "arma3",
            include_str!("../../../../examples/json/appdata/games/arma3/server/servers.json"),
        ),
        (
            "reforger",
            include_str!("../../../../examples/json/appdata/games/reforger/server/servers.json"),
        ),
    ] {
        let store = make_store(root.path(), space);
        let catalog: ServerCatalog = serde_json::from_str(bytes).unwrap();
        store.validate_catalog(&catalog).unwrap();
        assert_eq!(catalog.instances.len(), 1);
    }
}

fn make_store(root: &Path, space: &str) -> ServerStore {
    ServerStore::new(ServerContext::capture(root, Some(space)).unwrap()).unwrap()
}

fn legacy_catalog(store: &ServerStore) -> Vec<u8> {
    let mut catalog = store.empty();
    catalog.revision = 7;
    catalog
        .instances
        .push(ServerInstance::draft(store.context.space_id(), store.kind, "MainServer").unwrap());
    let mut value = serde_json::to_value(catalog).unwrap();
    value["schema_version"] = 1.into();
    value.as_object_mut().unwrap().remove("installations");
    for instance in value["instances"].as_array_mut().unwrap() {
        instance["schema_version"] = 1.into();
        instance.as_object_mut().unwrap().remove("installation_id");
    }
    serde_json::to_vec_pretty(&value).unwrap()
}

#[test]
fn legacy_projection_is_strict_and_never_writes_during_reads_or_plans() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    for space in ["alpha", "beta"] {
        let store = make_store(root.path(), space);
        let bytes = legacy_catalog(&store);
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        fs::write(store.path(), &bytes).unwrap();
        let (catalog, legacy) = store.load_with_status().unwrap();
        assert!(legacy);
        assert_eq!(catalog.schema_version, 2);
        assert_eq!(catalog.revision, 7);
        assert_eq!(catalog.instances[0].revision, 1);
        assert!(catalog.instances[0].installation_id.is_none());
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let original = store
            .parse_instance(&serde_json::to_vec(&value["instances"][0]).unwrap())
            .unwrap();
        assert_eq!(original, catalog.instances[0]);
        assert!(
            store
                .plan(DraftChange::Rename {
                    id: original.id.clone(),
                    name: "RenamedServer".into(),
                    expected_revision: 1
                })
                .unwrap()
                .migrates_from_v1
        );
        assert_eq!(fs::read(store.path()).unwrap(), bytes);
        assert_eq!(
            fs::read_dir(store.path().parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        value["instances"][0]["installation_id"] = serde_json::Value::Null;
        fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(store.load(), Err(ServerStoreError::Corrupt));
    }
}

#[test]
fn adoption_rechecks_executables_and_migration_backup_failure_preserves_v1() {
    const PROBE: &str = "FOXY_TEST_ADOPTION_STORE_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::server::store::tests::adoption_rechecks_executables_and_migration_backup_failure_preserves_v1"])
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
    let external = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = make_store(root.path(), "alpha");
    let bytes = legacy_catalog(&store);
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), &bytes).unwrap();
    let instance = store.load().unwrap().instances.remove(0);
    let executable =
        super::super::installation::tests::create_native_install(external.path(), store.kind);
    let executable_bytes = fs::read(&executable).unwrap();
    let installation = store.inspect_installation(external.path()).unwrap();
    let make_plan = || {
        store
            .plan(DraftChange::Adopt {
                id: instance.id.clone(),
                expected_revision: 1,
                installation: installation.clone(),
            })
            .unwrap()
    };
    let stale = make_plan();
    let mut changed = executable_bytes.clone();
    *changed.last_mut().unwrap() ^= 1;
    fs::write(&executable, &changed).unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&store.validate_claim_path().unwrap()),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    assert_eq!(store.apply(stale), Err(ServerStoreError::StaleRevision));
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    fs::write(&executable, &executable_bytes).unwrap();
    let backup = store
        .path()
        .parent()
        .unwrap()
        .join(format!("servers.v1.{}.json", blake3::hash(&bytes).to_hex()));
    fs::create_dir(&backup).unwrap();
    assert_eq!(store.apply(make_plan()), Err(ServerStoreError::UnsafePath));
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    fs::remove_dir(&backup).unwrap();
    store.apply(make_plan()).unwrap();
    assert_eq!(fs::read(&backup).unwrap(), bytes);
    let (catalog, legacy) = store.load_with_status().unwrap();
    assert!(!legacy);
    assert_eq!(
        catalog.instances[0].installation_id,
        Some(installation.id.clone())
    );
    assert_eq!(catalog.instances[0].revision, 2);
    assert_eq!(catalog.installations, vec![installation]);
    assert_eq!(fs::read(executable).unwrap(), executable_bytes);
    crate::core::tasks::db_process_lock::release_for(&store.context.database_path()).unwrap();
}

#[test]
fn empty_inspection_planning_and_unowned_apply_never_create_files() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = make_store(root.path(), "alpha");
    assert!(store.load().unwrap().instances.is_empty());
    let plan = store
        .plan(DraftChange::Add {
            name: "MainServer".into(),
        })
        .unwrap();
    assert_eq!(plan.catalog_revision(), 1);
    assert_eq!(store.apply(plan), Err(ServerStoreError::Ownership));
    assert!(!root.path().join("games").exists());
    assert!(matches!(
        ServerStore::new(ServerContext::capture(root.path(), Some("other")).unwrap()),
        Err(ServerStoreError::UnsupportedGame)
    ));
}

#[test]
fn corruption_future_versions_and_mismatched_context_are_not_repaired() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = make_store(root.path(), "alpha");
    let path = store.path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut wrong_space = serde_json::to_value(store.empty()).unwrap();
    wrong_space["game_space_id"] = "beta".into();
    let mut unknown_field = serde_json::to_value(store.empty()).unwrap();
    unknown_field["unexpected"] = true.into();
    for bytes in [
        b"{".to_vec(),
        b"{\"schema_version\":999}".to_vec(),
        serde_json::to_vec(&wrong_space).unwrap(),
        serde_json::to_vec(&unknown_field).unwrap(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(store.load().is_err());
        assert!(
            store
                .plan(DraftChange::Add {
                    name: "MainServer".into()
                })
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    let future = b"{\"schema_version\":999}";
    assert_eq!(
        store.parse_instance(future),
        Err(ServerStoreError::UnsupportedVersion)
    );
    assert!(!path.with_extension("json.bak").exists());
}

#[test]
fn non_file_store_and_non_directory_ancestors_are_refused() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = make_store(root.path(), "alpha");
    fs::create_dir_all(store.path()).unwrap();
    assert_eq!(store.load(), Err(ServerStoreError::UnsafePath));
    let beta = make_store(root.path(), "beta");
    fs::create_dir_all(beta.context.space_dir()).unwrap();
    fs::write(beta.context.space_dir().join("server"), b"keep").unwrap();
    assert_eq!(beta.load(), Err(ServerStoreError::UnsafePath));
    fs::write(
        beta.context.database_path().with_extension("lock"),
        b"ordinary",
    )
    .unwrap();
    assert!(beta.validate_claim_path().is_ok());
    assert_eq!(
        fs::read(beta.context.space_dir().join("server")).unwrap(),
        b"keep"
    );
}

#[test]
fn bounded_documents_and_duplicate_instance_ids_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let store = make_store(root.path(), "alpha");
    let mut catalog = store.empty();
    catalog.revision = 1;
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    catalog.instances = vec![instance.clone(), instance];
    assert_eq!(
        store.validate_catalog(&catalog),
        Err(ServerStoreError::Corrupt)
    );
    assert!(
        store
            .parse_instance(&vec![b' '; MAX_DOCUMENT_BYTES as usize + 1])
            .is_err()
    );
    let input = root.path().join("oversized.json");
    File::create(&input)
        .unwrap()
        .set_len(MAX_DOCUMENT_BYTES + 1)
        .unwrap();
    assert!(read_bounded(&input).is_err());
    assert_eq!(
        next_revision(u64::MAX),
        Err(ServerStoreError::Invalid(
            "Server revision limit reached".into()
        ))
    );
}

#[test]
fn owned_draft_workflow_is_atomic_revision_checked_and_space_isolated() {
    const PROBE: &str = "FOXY_TEST_SERVER_STORE_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::server::store::tests::owned_draft_workflow_is_atomic_revision_checked_and_space_isolated", "--nocapture"])
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
    fixture(root.path());
    let registry_before = fs::read(root.path().join("games.json")).unwrap();
    std::env::set_current_dir(root.path()).unwrap();
    let alpha = make_store(root.path(), "alpha");
    let beta = make_store(root.path(), "beta");
    for store in [&alpha, &beta] {
        std::env::set_current_dir(root.path()).unwrap();
        let claim = store.validate_claim_path().unwrap();
        assert_eq!(
            crate::core::tasks::db_process_lock::acquire(
                &PathBuf::from("games")
                    .join(store.context.space_id())
                    .join("database.db")
            ),
            crate::core::tasks::db_process_lock::LockOutcome::Acquired
        );
        std::env::set_current_dir(root.path().parent().unwrap()).unwrap();
        assert_eq!(
            crate::core::tasks::db_process_lock::held_database_path().as_deref(),
            Some(claim.as_path())
        );
        let foreign = if store.context.space_id() == "alpha" {
            &beta
        } else {
            &alpha
        };
        let foreign_plan = foreign
            .plan(DraftChange::Add {
                name: "OtherServer".into(),
            })
            .unwrap();
        assert_eq!(
            foreign.apply(foreign_plan),
            Err(ServerStoreError::Ownership)
        );
        let plan = store
            .plan(DraftChange::Add {
                name: "MainServer".into(),
            })
            .unwrap();
        let original = plan.instance.clone();
        assert!(crate::core::tasks::db_process_lock::holds(&claim));
        store.apply(plan).unwrap();
        assert_eq!(store.show(&original.id).unwrap(), original);
        let old = store
            .plan(DraftChange::Rename {
                id: original.id.clone(),
                name: "OldPlan".into(),
                expected_revision: 1,
            })
            .unwrap();
        let current = store
            .plan(DraftChange::Rename {
                id: original.id.clone(),
                name: "RenamedServer".into(),
                expected_revision: 1,
            })
            .unwrap();
        store.apply(current).unwrap();
        let committed = fs::read(store.path()).unwrap();
        assert_eq!(store.apply(old), Err(ServerStoreError::StaleRevision));
        assert!(matches!(
            store.plan(DraftChange::Remove {
                id: original.id.clone(),
                expected_revision: 1
            }),
            Err(ServerStoreError::StaleRevision)
        ));
        assert_eq!(fs::read(store.path()).unwrap(), committed);
        let mut edited = store.show(&original.id).unwrap();
        edited.display_name = "EditedServer".into();
        let plan = store
            .plan(DraftChange::Replace {
                instance: edited,
                expected_revision: 2,
            })
            .unwrap();
        store.apply(plan).unwrap();
        let edited = store.show(&original.id).unwrap();
        assert_eq!(edited.revision, 3);
        assert_eq!(edited.definition, original.definition);
        let plan = store
            .plan(DraftChange::Clone {
                id: original.id.clone(),
                name: "CopiedServer".into(),
                expected_revision: 3,
            })
            .unwrap();
        let copied = plan.instance.clone();
        store.apply(plan).unwrap();
        assert_ne!(copied.id, original.id);
        assert_eq!(copied.revision, 1);
        assert_eq!(copied.definition, original.definition);
        assert_eq!(store.load().unwrap().instances.len(), 2);
        let plan = store
            .plan(DraftChange::Remove {
                id: copied.id.clone(),
                expected_revision: 1,
            })
            .unwrap();
        store.apply(plan).unwrap();
        assert_eq!(store.show(&copied.id), Err(ServerStoreError::NotFound));
        assert_eq!(store.load().unwrap().instances, vec![edited]);
        assert!(!claim.exists());
        assert_eq!(
            fs::read_dir(store.path().parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
    }
    let alpha_bytes = fs::read(alpha.path()).unwrap();
    let beta_bytes = fs::read(beta.path()).unwrap();
    assert_ne!(alpha_bytes, beta_bytes);
    assert_eq!(
        fs::read(root.path().join("games.json")).unwrap(),
        registry_before
    );
    assert!(
        alpha
            .parse_instance(&serde_json::to_vec(&beta.load().unwrap().instances[0]).unwrap())
            .is_err()
    );
    let plan = alpha
        .plan(DraftChange::Add {
            name: "MainServer".into(),
        })
        .unwrap();
    assert!(matches!(
        beta.apply(plan),
        Err(ServerStoreError::Invalid(_))
    ));
    assert_eq!(fs::read(alpha.path()).unwrap(), alpha_bytes);
    assert_eq!(fs::read(beta.path()).unwrap(), beta_bytes);
    let claim = alpha.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let plan = alpha
        .plan(DraftChange::Add {
            name: "MainServer".into(),
        })
        .unwrap();
    let mut manual: ServerCatalog = serde_json::from_slice(&alpha_bytes).unwrap();
    manual.instances[0].display_name = "ExternalEdit".into();
    let changed = serde_json::to_vec(&manual).unwrap();
    fs::write(alpha.path(), &changed).unwrap();
    assert_eq!(alpha.apply(plan), Err(ServerStoreError::StaleRevision));
    assert_eq!(fs::read(alpha.path()).unwrap(), changed);
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
    std::env::set_current_dir(root.path().parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn linked_store_directories_and_documents_are_refused() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fixture(root.path());
    let alpha = make_store(root.path(), "alpha");
    fs::create_dir_all(alpha.context.space_dir()).unwrap();
    symlink(outside.path(), alpha.context.space_dir().join("server")).unwrap();
    assert_eq!(alpha.load(), Err(ServerStoreError::UnsafePath));
    let beta = make_store(root.path(), "beta");
    fs::create_dir_all(beta.path().parent().unwrap()).unwrap();
    let target = outside.path().join("keep.json");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, beta.path()).unwrap();
    assert_eq!(beta.load(), Err(ServerStoreError::UnsafePath));
    assert_eq!(fs::read(target).unwrap(), b"keep");
}
