use super::*;

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

fn planned(item_id: &str, action: WorkshopItemAction, gid: &str) -> PlannedWorkshopItem {
    PlannedWorkshopItem {
        item_id: item_id.into(),
        title: "Synthetic item".into(),
        action,
        manifest_gid: Some(gid.into()),
        pinned_manifest: None,
        file_size: 5,
        time_updated: 7,
        issue: None,
    }
}

fn claim(store: &ServerStore) -> PathBuf {
    let claim = store.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    claim
}

fn publish_fixture(store: &ServerStore, item_id: &str, gid: &str, payload: &[u8]) -> PathBuf {
    let revision = store.workshop_revision(item_id, gid);
    let addon = revision.join(addon_name(item_id));
    fs::create_dir_all(addon.join("addons")).unwrap();
    fs::write(addon.join("addons/a.pbo"), payload).unwrap();
    let mut entries = BTreeMap::new();
    entries.insert(
        "addons".to_owned(),
        ExpectedEntry {
            depot_id: ARMA3_CLIENT_APP,
            size: 0,
            sha1: None,
            directory: true,
            executable: false,
        },
    );
    entries.insert(
        "addons/a.pbo".to_owned(),
        ExpectedEntry {
            depot_id: ARMA3_CLIENT_APP,
            size: payload.len() as u64,
            sha1: None,
            directory: false,
            executable: false,
        },
    );
    let verified = content::verify_tree(&addon, &entries, &|| false).unwrap();
    let inventory = RevisionInventory::new(item_id, gid, 7, &entries, &verified);
    write_new(&revision.join(INVENTORY), &inventory).unwrap();
    revision
}

#[test]
fn pinned_revisions_move_profiles_and_verify_without_writing() {
    const PROBE: &str = "FOXY_WORKSHOP_PIN_TEST_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "core::server::workshop_store::tests::pinned_revisions_move_profiles_and_verify_without_writing"])
            .env(PROBE, "1").output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("running 1 test"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    pinning_registers_an_already_present_profile_and_moves_with_updates();
    verification_detects_changed_extra_and_missing_files_without_writing();
}

fn pinning_registers_an_already_present_profile_and_moves_with_updates() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "arma3");
    let claim = claim(&store);
    let first = publish_fixture(&store, "123", "11", b"first");
    store
        .pin_workshop_revision(&planned("123", WorkshopItemAction::Acquire, "11"), "11", 7)
        .unwrap();
    let snapshot = store.workshop_items().unwrap();
    assert_eq!(snapshot.generation, 1);
    assert_eq!(snapshot.items[0].pinned_manifest, "11");
    assert_eq!(snapshot.items[0].profile, "workshop-123");
    let sources = store.content_sources().unwrap().sources;
    let resolved = sources.resolve("workshop-123").unwrap();
    assert!(!resolved.download);
    assert_eq!(resolved.addon_paths, vec![first.join("@workshop_123")]);

    let second = publish_fixture(&store, "123", "12", b"second");
    store
        .pin_workshop_revision(&planned("123", WorkshopItemAction::Update, "12"), "12", 8)
        .unwrap();
    let snapshot = store.workshop_items().unwrap();
    assert_eq!(snapshot.generation, 2);
    assert_eq!(snapshot.items[0].pinned_manifest, "12");
    assert_eq!(snapshot.items[0].revisions, vec!["11", "12"]);
    let sources = store.content_sources().unwrap().sources;
    assert_eq!(sources.profiles.len(), 1);
    assert_eq!(sources.folders.len(), 1);
    assert_eq!(
        sources.resolve("workshop-123").unwrap().addon_paths,
        vec![second.join("@workshop_123")]
    );
    assert!(first.join("@workshop_123/addons/a.pbo").exists());

    assert!(matches!(
        store.pin_workshop_revision(&planned("124", WorkshopItemAction::Acquire, "1"), "1", 1),
        Err(ServerStoreError::NotFound | ServerStoreError::Corrupt)
    ));
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
}

fn verification_detects_changed_extra_and_missing_files_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path(), "arma3");
    let claim = claim(&store);
    let revision = publish_fixture(&store, "200", "21", b"payload");
    store
        .pin_workshop_revision(&planned("200", WorkshopItemAction::Acquire, "21"), "21", 7)
        .unwrap();
    let control = ContentStageControl::default();
    let report = store.verify_workshop_item("200", &control).unwrap();
    assert!(report.verified, "{report:?}");
    assert_eq!(report.files, 1);
    assert!(!report.writes_performed);

    let file = revision.join("@workshop_200/addons/a.pbo");
    fs::write(&file, b"payloaD").unwrap();
    assert!(
        !store
            .verify_workshop_item("200", &control)
            .unwrap()
            .verified
    );
    fs::write(&file, b"payload").unwrap();
    fs::write(revision.join("@workshop_200/extra.txt"), b"x").unwrap();
    assert!(
        !store
            .verify_workshop_item("200", &control)
            .unwrap()
            .verified
    );
    fs::remove_file(revision.join("@workshop_200/extra.txt")).unwrap();
    fs::remove_file(&file).unwrap();
    assert!(
        !store
            .verify_workshop_item("200", &control)
            .unwrap()
            .verified
    );
    assert!(matches!(
        store.verify_workshop_item("999", &control),
        Err(ServerStoreError::NotFound)
    ));
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
}

#[test]
fn planning_is_gated_and_validates_items_before_contacting_steam() {
    let root = tempfile::tempdir().unwrap();
    let reforger = store(root.path(), "reforger");
    let request = WorkshopRequest {
        items: vec!["123".into()],
        account: None,
        repair: false,
    };
    assert!(matches!(
        reforger.plan_workshop(request.clone(), &ContentStageControl::default()),
        Err(ServerStoreError::UnsupportedGame)
    ));
    if !cfg!(all(
        target_arch = "x86_64",
        any(windows, target_os = "linux")
    )) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let arma = store(root.path(), "arma3");
    for request in [
        WorkshopRequest {
            items: Vec::new(),
            ..request.clone()
        },
        WorkshopRequest {
            items: vec!["../1".into()],
            ..request.clone()
        },
        WorkshopRequest {
            items: vec!["0".into()],
            ..request.clone()
        },
        WorkshopRequest {
            account: Some("../alias".into()),
            ..request.clone()
        },
        WorkshopRequest {
            repair: true,
            ..request.clone()
        },
        WorkshopRequest {
            account: Some("default".into()),
            ..request.clone()
        },
    ] {
        assert!(matches!(
            arma.plan_workshop(request, &ContentStageControl::default()),
            Err(ServerStoreError::Invalid(_))
        ));
    }
    assert!(!root.path().join("games/alpha/server/workshop").exists());
}

#[test]
fn documents_and_item_plans_follow_pins() {
    let detail = |gid: Option<u64>| WorkshopItem {
        item_id: 5,
        result: 1,
        consumer_app_id: ARMA3_CLIENT_APP,
        creator_app_id: ARMA3_CLIENT_APP,
        title: "Synthetic".into(),
        file_size: 9,
        manifest_gid: gid,
        legacy_file: false,
        time_updated: 3,
        visibility: 0,
        banned: false,
        file_type: 0,
    };
    let mut document = WorkshopDocument {
        schema_version: SCHEMA_VERSION,
        game_space_id: "alpha".into(),
        generation: 1,
        items: Vec::new(),
    };
    assert_eq!(
        plan_item(&detail(Some(8)), &document, false).action,
        WorkshopItemAction::Acquire
    );
    document.items.push(TrackedWorkshopItem {
        item_id: "5".into(),
        title: "Synthetic".into(),
        consumer_app_id: ARMA3_CLIENT_APP,
        pinned_manifest: "8".into(),
        pinned_time_updated: 2,
        revisions: vec!["8".into()],
        profile: "workshop-5".into(),
    });
    assert_eq!(
        plan_item(&detail(Some(8)), &document, false).action,
        WorkshopItemAction::Current
    );
    let update = plan_item(&detail(Some(9)), &document, false);
    assert_eq!(update.action, WorkshopItemAction::Update);
    assert_eq!(update.pinned_manifest.as_deref(), Some("8"));
    let repair = plan_item(&detail(Some(9)), &document, true);
    assert_eq!(repair.action, WorkshopItemAction::Repair);
    assert_eq!(repair.manifest_gid.as_deref(), Some("8"));
    let unavailable = plan_item(&detail(None), &document, false);
    assert_eq!(unavailable.action, WorkshopItemAction::Unavailable);
    assert_eq!(unavailable.issue, Some(ItemIssue::NoContent));
}

#[test]
fn batch_success_requires_every_actionable_item() {
    let control = ContentStageControl::default();
    let item = |status| WorkshopItemOutcome {
        item_id: "1".into(),
        status,
        manifest_gid: None,
        files: 0,
        bytes: 0,
        pinned: false,
        staging_removed: None,
        content_error: None,
        error: None,
    };
    use WorkshopItemStatus::*;
    assert_eq!(
        batch_status(&[item(Acquired), item(Reused), item(Current)], &control),
        WorkshopBatchStatus::Complete
    );
    assert_eq!(
        batch_status(&[item(Acquired), item(Failed)], &control),
        WorkshopBatchStatus::Partial
    );
    assert_eq!(
        batch_status(&[item(Failed), item(Unavailable)], &control),
        WorkshopBatchStatus::Failed
    );
    assert_eq!(
        batch_status(&[item(Acquired), item(Cancelled)], &control),
        WorkshopBatchStatus::Cancelled
    );
    assert_eq!(
        batch_status(&[item(Current)], &control),
        WorkshopBatchStatus::Failed
    );
    control.cancel();
    assert_eq!(
        batch_status(&[item(Failed)], &control),
        WorkshopBatchStatus::Cancelled
    );
}

#[test]
fn committed_workshop_examples_match_the_schemas() {
    let document: WorkshopDocument = serde_json::from_str(include_str!(
        "../../../../examples/json/appdata/games/arma3/server/workshop/workshop.json"
    ))
    .unwrap();
    assert_eq!(document.schema_version, SCHEMA_VERSION);
    let item = &document.items[0];
    assert!(valid_item_id(&item.item_id));
    assert!(item.revisions.contains(&item.pinned_manifest));
    assert_eq!(item.profile, profile_name(&item.item_id));
    assert_eq!(item.consumer_app_id, ARMA3_CLIENT_APP);
    let inventory: RevisionInventory = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/workshop_revision_inventory.json"
    ))
    .unwrap();
    assert_eq!(inventory.item_id, item.item_id);
    assert_eq!(inventory.manifest_gid, item.pinned_manifest);
    assert_eq!(inventory.expected().len(), 2);
}
