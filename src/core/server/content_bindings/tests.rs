use super::*;
use crate::core::server::content_sources::{AddonSupply, ContentSources};
use crate::core::server::{ServerAddonRole, ServerCatalog, ServerDefinitionKind, ServerInstance};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

const CLAIM_TEST: &str = "FOXY_CONTENT_BINDINGS_CLAIM_TEST";

fn fixture(root: &Path) {
    fs::write(
        root.join("games.json"),
        serde_json::to_vec(&json!({
            "active_game_space_id": "alpha",
            "last_opened_game_space_id": "alpha",
            "game_spaces": [
                {"id":"alpha", "game_id":"arma3", "display_name":"Alpha", "created_at":0},
                {"id":"beta", "game_id":"reforger", "display_name":"Beta", "created_at":0}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    for space in ["alpha", "beta"] {
        fs::create_dir_all(root.join("games").join(space)).unwrap();
    }
}

fn prepare_store(root: &Path, space: &str) -> (ServerStore, ServerInstanceId) {
    let store = ServerStore::new(ServerContext::capture(root, Some(space)).unwrap()).unwrap();
    let kind = if space == "alpha" {
        ServerDefinitionKind::Arma3
    } else {
        ServerDefinitionKind::Reforger
    };
    let instance = ServerInstance::draft(space, kind, "MainServer").unwrap();
    let catalog = ServerCatalog {
        schema_version: super::super::definition::SERVER_SCHEMA_VERSION,
        game_space_id: space.into(),
        game_id: if space == "alpha" {
            "arma3"
        } else {
            "reforger"
        }
        .into(),
        revision: 1,
        instances: vec![instance.clone()],
        installations: vec![],
    };
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();

    let source_root = root.join(format!("source-{space}"));
    fs::create_dir_all(&source_root).unwrap();
    let mut sources = ContentSources::default();
    sources.add_folder("hosted", &source_root).unwrap();
    sources
        .add_profile(crate::core::server::content_sources::RepositoryProfile {
            name: "RepoAlpha".into(),
            remote_url: "https://repo.example.invalid/main".into(),
            local_path: root.join(format!("client-{space}")),
            folder_id: "hosted".into(),
            supply: AddonSupply::AlreadyPresent,
            addons: vec!["@mod_01".into()],
        })
        .unwrap();
    sources.save(&store.context().space_dir()).unwrap();
    (store, instance.id)
}

fn request(
    store: &ServerStore,
    id: &ServerInstanceId,
    bindings: Vec<ServerAddonBinding>,
) -> ContentPlanRequest {
    let catalog = store.load().unwrap();
    let instance = catalog
        .instances
        .iter()
        .find(|item| &item.id == id)
        .unwrap();
    ContentPlanRequest {
        instance_id: id.clone(),
        expected_instance_revision: instance.revision,
        expected_catalog_revision: catalog.revision,
        content_sources_fingerprint: store.content_sources().unwrap().fingerprint,
        bindings,
    }
}

fn selected() -> Vec<ServerAddonBinding> {
    vec![ServerAddonBinding {
        source_id: "RepoAlpha".into(),
        role: ServerAddonRole::Required,
        order: 0,
    }]
}

fn binding_path(store: &ServerStore, id: &ServerInstanceId) -> PathBuf {
    store
        .context()
        .space_dir()
        .join("server/instances")
        .join(id.as_str())
        .join(CONTENT_BINDINGS_FILE)
}

#[test]
fn absent_snapshot_and_plan_are_read_only_and_keep_game_spaces_separate() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    for space in ["alpha", "beta"] {
        let (store, id) = prepare_store(root.path(), space);
        let catalog_before = fs::read(store.path()).unwrap();
        let source_path = store
            .context()
            .space_dir()
            .join("server/content-sources.json");
        let sources_before = fs::read(&source_path).unwrap();
        let snapshot = store.content_bindings(&id).unwrap();
        assert!(snapshot.document.is_none());
        assert!(snapshot.fingerprint.is_none());
        assert!(!snapshot.review_current);
        let plan = store
            .plan_content_bindings(request(&store, &id, selected()), None)
            .unwrap();
        assert_eq!(plan.expected_generation, 0);
        assert_eq!(plan.document.generation, 1);
        assert_eq!(plan.document.game_space_id, space);
        assert!(!plan.preview.deployment_ready);
        assert!(!plan.preview.downloads_started);
        assert!(!plan.preview.writes_performed);
        assert!(!binding_path(&store, &id).exists());
        assert_eq!(fs::read(store.path()).unwrap(), catalog_before);
        assert_eq!(fs::read(source_path).unwrap(), sources_before);
    }
}

#[test]
fn claimed_save_reload_generation_and_empty_selection_work_for_both_games() {
    if std::env::var_os(CLAIM_TEST).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::server::content_bindings::tests::claimed_save_reload_generation_and_empty_selection_work_for_both_games",
                "--nocapture",
            ])
            .env(CLAIM_TEST, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    for space in ["alpha", "beta"] {
        let (store, id) = prepare_store(root.path(), space);
        let path = binding_path(&store, &id);
        let initial = store.content_bindings(&id).unwrap();
        let plan = store
            .plan_content_bindings(
                request(&store, &id, selected()),
                initial.fingerprint.as_deref(),
            )
            .unwrap();
        let claim = store.validate_claim_path().unwrap();
        assert_eq!(
            crate::core::tasks::db_process_lock::acquire(&claim),
            crate::core::tasks::db_process_lock::LockOutcome::Acquired
        );
        let mut changed_report = plan.clone();
        changed_report.document.bindings[0].binding.role = ServerAddonRole::ServerOnly;
        assert_eq!(
            store.apply_content_bindings(changed_report).unwrap_err(),
            ServerStoreError::StaleRevision
        );
        assert!(!path.exists());
        let saved = store.apply_content_bindings(plan).unwrap();
        assert!(saved.review_current);
        assert_eq!(saved.document.as_ref().unwrap().generation, 1);
        assert!(saved.fingerprint.is_some());
        assert!(saved.document.as_ref().unwrap().bindings.len() == 1);
        assert_eq!(
            store.content_bindings(&id).unwrap().document,
            saved.document
        );

        let clear = store
            .plan_content_bindings(request(&store, &id, vec![]), saved.fingerprint.as_deref())
            .unwrap();
        assert_eq!(clear.expected_generation, 1);
        assert_eq!(clear.document.generation, 2);
        let cleared = store.apply_content_bindings(clear).unwrap();
        assert!(cleared.review_current);
        assert!(cleared.document.as_ref().unwrap().bindings.is_empty());
        assert_eq!(cleared.document.as_ref().unwrap().generation, 2);
        assert!(!fs::read(&path).unwrap().is_empty());
        crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
    }
}

#[test]
fn shipped_empty_binding_documents_match_the_owned_schema() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    for (space, json) in [
        (
            "arma3",
            include_str!(
                "../../../../examples/json/appdata/games/arma3/server/instances/server_00000000000000000000000000000001/content-bindings.json"
            ),
        ),
        (
            "reforger",
            include_str!(
                "../../../../examples/json/appdata/games/reforger/server/instances/server_00000000000000000000000000000002/content-bindings.json"
            ),
        ),
    ] {
        let store =
            ServerStore::new(ServerContext::capture(root.path(), Some(space)).unwrap()).unwrap();
        let document: ContentBindingsDocument = serde_json::from_str(json).unwrap();
        store
            .validate_content_bindings(&document.instance_id, &document)
            .unwrap();
        assert_eq!(document.generation, 1);
        assert!(document.bindings.is_empty());
    }
}

#[test]
fn a_small_registry_cannot_expand_into_an_oversized_binding_document() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (store, id) = prepare_store(root.path(), "alpha");
    let mut sources = store.content_sources().unwrap().sources;
    for _ in 0..120 {
        sources.folders[0].path.push("x".repeat(200));
    }
    for index in 1..180 {
        let mut profile = sources.profiles[0].clone();
        profile.name = format!("Repo{index:02}");
        profile.local_path = root.path().join(format!("client-{index}"));
        sources.add_profile(profile).unwrap();
    }
    sources.save(&store.context().space_dir()).unwrap();
    let selected = sources
        .profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| ServerAddonBinding {
            source_id: profile.name.clone(),
            role: ServerAddonRole::Required,
            order: index as u32,
        })
        .collect();
    assert_eq!(
        store
            .plan_content_bindings(request(&store, &id, selected), None)
            .unwrap_err(),
        ServerStoreError::Invalid("Content bindings exceed 4 MiB".into())
    );
    assert!(!binding_path(&store, &id).exists());
}

#[test]
fn unclaimed_apply_and_other_context_plans_cannot_write() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (alpha, alpha_id) = prepare_store(root.path(), "alpha");
    let (beta, beta_id) = prepare_store(root.path(), "beta");
    let plan = alpha
        .plan_content_bindings(request(&alpha, &alpha_id, selected()), None)
        .unwrap();
    assert!(matches!(
        alpha.apply_content_bindings(plan.clone()),
        Err(ServerStoreError::Ownership)
    ));
    assert!(!binding_path(&alpha, &alpha_id).exists());
    assert!(matches!(
        beta.apply_content_bindings(plan),
        Err(ServerStoreError::Ownership)
    ));
    assert!(!binding_path(&beta, &beta_id).exists());
}

#[test]
fn raw_binding_source_and_catalog_edits_stale_plans_without_replacement() {
    const STALE_TEST: &str = "FOXY_CONTENT_BINDINGS_STALE_TEST";
    if std::env::var_os(STALE_TEST).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::server::content_bindings::tests::raw_binding_source_and_catalog_edits_stale_plans_without_replacement",
                "--nocapture",
            ])
            .env(STALE_TEST, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (store, id) = prepare_store(root.path(), "alpha");
    let claim = store.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    let initial = store
        .plan_content_bindings(request(&store, &id, selected()), None)
        .unwrap();
    let saved = store.apply_content_bindings(initial).unwrap();
    let path = binding_path(&store, &id);

    let plan = store
        .plan_content_bindings(request(&store, &id, vec![]), saved.fingerprint.as_deref())
        .unwrap();
    let mut edited = fs::read(&path).unwrap();
    edited.push(b'\n');
    fs::write(&path, &edited).unwrap();
    assert!(matches!(
        store.apply_content_bindings(plan),
        Err(ServerStoreError::StaleRevision)
    ));
    assert_eq!(fs::read(&path).unwrap(), edited);

    let current = store.content_bindings(&id).unwrap();
    let plan = store
        .plan_content_bindings(request(&store, &id, vec![]), current.fingerprint.as_deref())
        .unwrap();
    let source_path = store
        .context()
        .space_dir()
        .join("server/content-sources.json");
    let mut source_bytes = fs::read(&source_path).unwrap();
    source_bytes.push(b'\n');
    fs::write(&source_path, &source_bytes).unwrap();
    assert!(matches!(
        store.apply_content_bindings(plan),
        Err(ServerStoreError::StaleRevision)
    ));
    assert_eq!(fs::read(&path).unwrap(), edited);

    let current = store.content_bindings(&id).unwrap();
    let plan = store
        .plan_content_bindings(request(&store, &id, vec![]), current.fingerprint.as_deref())
        .unwrap();
    let mut catalog_bytes = fs::read(store.path()).unwrap();
    catalog_bytes.push(b'\n');
    fs::write(store.path(), &catalog_bytes).unwrap();
    assert!(matches!(
        store.apply_content_bindings(plan),
        Err(ServerStoreError::StaleRevision)
    ));
    assert_eq!(fs::read(&path).unwrap(), edited);
    assert_eq!(fs::read(store.path()).unwrap(), catalog_bytes);
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
}

#[test]
fn malformed_future_unknown_and_mismatched_documents_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (store, id) = prepare_store(root.path(), "alpha");
    let path = binding_path(&store, &id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for (bytes, error) in [
        (b"{".to_vec(), ServerStoreError::Corrupt),
        (
            br#"{"schema_version":999}"#.to_vec(),
            ServerStoreError::UnsupportedVersion,
        ),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(store.content_bindings(&id).unwrap_err(), error);
        assert!(matches!(
            store.plan_content_bindings(request(&store, &id, vec![]), None),
            Err(ref actual) if actual == &error
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    let mut invalid = Vec::new();
    let mut unknown = json!({
        "schema_version": 1,
        "game_space_id": "alpha",
        "game_id": "arma3",
        "instance_id": id.clone(),
        "generation": 1,
        "instance_revision": 1,
        "catalog_revision": 1,
        "catalog_fingerprint": "0".repeat(64),
        "content_sources_fingerprint": null,
        "bindings": [],
        "unexpected": true
    });
    invalid.push(serde_json::to_vec(&unknown).unwrap());
    unknown.as_object_mut().unwrap().remove("unexpected");
    unknown["game_space_id"] = "beta".into();
    invalid.push(serde_json::to_vec(&unknown).unwrap());
    for bytes in invalid {
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            store.content_bindings(&id),
            Err(ServerStoreError::Corrupt)
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn generation_overflow_refuses_a_plan_without_touching_the_document() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (store, id) = prepare_store(root.path(), "alpha");
    let value = json!({
        "schema_version": 1,
        "game_space_id": "alpha",
        "game_id": "arma3",
        "instance_id": id.clone(),
        "generation": u64::MAX,
        "instance_revision": 1,
        "catalog_revision": 1,
        "catalog_fingerprint": blake3::hash(&fs::read(store.path()).unwrap()).to_hex().to_string(),
        "content_sources_fingerprint": store.content_sources().unwrap().fingerprint,
        "bindings": []
    });
    let path = binding_path(&store, &id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        store.plan_content_bindings(
            request(&store, &id, vec![]),
            Some(blake3::hash(&bytes).to_hex().as_str())
        ),
        Err(ServerStoreError::Invalid(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn embedded_source_drift_does_not_pass_current_review() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (store, id) = prepare_store(root.path(), "alpha");
    let mut document = store
        .plan_content_bindings(request(&store, &id, selected()), None)
        .unwrap()
        .document;
    document.bindings[0].folder.path = root.path().join("different-source");
    let path = binding_path(&store, &id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec(&document).unwrap();
    fs::write(&path, &bytes).unwrap();
    let snapshot = store.content_bindings(&id).unwrap();
    assert!(!snapshot.review_current);
    assert!(!snapshot.issues.is_empty());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn linked_binding_parent_is_refused_and_target_is_preserved() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fixture(root.path());
    let (store, id) = prepare_store(root.path(), "alpha");
    let plan = store
        .plan_content_bindings(request(&store, &id, selected()), None)
        .unwrap();
    let instance_dir = store.context().space_dir().join("server/instances");
    fs::create_dir_all(&instance_dir).unwrap();
    symlink(outside.path(), instance_dir.join(id.as_str())).unwrap();
    let claim = store.validate_claim_path().unwrap();
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
    assert!(matches!(
        store.apply_content_bindings(plan),
        Err(ServerStoreError::UnsafePath)
    ));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
}

#[test]
fn example_content_plan_bindings_parses() {
    let bindings: Vec<crate::core::server::ServerAddonBinding> = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/content_plan_bindings.json"
    ))
    .unwrap();
    assert!(!bindings.is_empty());
}
