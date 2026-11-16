use super::*;
use crate::core::server::content_sources::{AddonSupply, ContentSources, RepositoryProfile};
use crate::core::server::{
    ContentPlanRequest, ServerAddonBinding, ServerAddonRole, ServerCatalog, ServerContext,
    ServerDefinitionKind, ServerInstance, ServerStore, ServerStoreError,
};
use std::fs;
use std::path::{Path, PathBuf};

const CLAIM_TEST: &str = "FOXY_CONTENT_STAGING_CLAIM_TEST";

#[test]
fn acquired_manifest_keeps_download_identity_and_requires_exact_provenance() {
    let fixture = make_fixture("alpha", AddonSupply::Download);
    let request = binding_request(&fixture.store, &fixture.id);
    let document = fixture
        .store
        .plan_content_bindings(request, None)
        .unwrap()
        .document;
    let profile = &document.bindings[0].profile;
    let snapshot = StageRepositorySnapshot {
        profile_name: profile.name.clone(),
        repository_url: profile.remote_url.clone(),
        repository_local_path: profile.local_path.clone(),
        publication_checksum: "1".repeat(64),
        addons: vec![StageRepositoryAddon {
            name: "@mod_01".into(),
            checksum: "2".repeat(64),
        }],
    };
    assert!(validation::expected_sources(&document).is_err());
    let sources =
        validation::expected_sources_with_acquisitions(&document, std::slice::from_ref(&snapshot))
            .unwrap();
    assert_eq!(sources[0].source, profile.local_path.join("@mod_01"));
    assert_eq!(sources[0].destination, "addons/@mod_01");
    assert_eq!(profile.supply, AddonSupply::Download);
    for drift in ["url", "path", "addon", "duplicate"] {
        let mut changed = snapshot.clone();
        match drift {
            "url" => changed.repository_url = "https://other.example.invalid/".into(),
            "path" => changed.repository_local_path = profile.local_path.join("other"),
            "addon" => changed.addons[0].name = "@mod_02".into(),
            _ => changed.addons.push(changed.addons[0].clone()),
        }
        assert!(
            validation::expected_sources_with_acquisitions(&document, &[changed]).is_err(),
            "{drift}"
        );
    }
    let legacy: ContentStageManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/content_stage_manifest.json"
    ))
    .unwrap();
    assert!(legacy.acquisitions.is_empty());
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("acquisitions")
            .is_none()
    );
}

#[test]
fn documented_empty_manifest_matches_the_strict_archive_schema() {
    let manifest: ContentStageManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/content_stage_manifest.json"
    ))
    .unwrap();
    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert!(manifest.bindings.bindings.is_empty());
    assert_eq!(
        validation::expected_sources(&manifest.bindings).unwrap(),
        manifest.sources
    );
    tree::validate_entries(&manifest).unwrap();
    assert_eq!(manifest.entries.len(), 1);
    assert_eq!(manifest.entries[0].path, "addons");
    assert_eq!(manifest.total_bytes, 0);
}

#[test]
fn acquired_example_binds_original_repository_identity_without_temporary_paths() {
    let manifest: ContentStageManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/acquired_content_stage_manifest.json"
    ))
    .unwrap();
    let sources =
        validation::expected_sources_with_acquisitions(&manifest.bindings, &manifest.acquisitions)
            .unwrap();
    assert_eq!(sources, manifest.sources);
    assert_eq!(
        manifest.bindings.bindings[0].profile.supply,
        AddonSupply::Download
    );
    assert_ne!(
        manifest.bindings.bindings[0].folder.path,
        manifest.acquisitions[0].repository_local_path
    );
    tree::validate_entries(&manifest).unwrap();
}

struct Fixture {
    _root: tempfile::TempDir,
    store: ServerStore,
    id: ServerInstanceId,
    claim: PathBuf,
    source: PathBuf,
}

fn make_fixture(space: &str, supply: AddonSupply) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        serde_json::json!({
            "active_game_space_id":"alpha",
            "last_opened_game_space_id":"alpha",
            "game_spaces":[
                {"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0},
                {"id":"beta","game_id":"reforger","display_name":"Beta","created_at":0}
            ]
        })
        .to_string(),
    )
    .unwrap();
    for space in ["alpha", "beta"] {
        fs::create_dir_all(root.path().join("games").join(space)).unwrap();
    }
    let game_id = if space == "alpha" {
        "arma3"
    } else {
        "reforger"
    };
    let kind = if space == "alpha" {
        ServerDefinitionKind::Arma3
    } else {
        ServerDefinitionKind::Reforger
    };
    let store =
        ServerStore::new(ServerContext::capture(root.path(), Some(space)).unwrap()).unwrap();
    let instance = ServerInstance::draft(space, kind, "MainServer").unwrap();
    let catalog = ServerCatalog {
        schema_version: super::super::definition::SERVER_SCHEMA_VERSION,
        game_space_id: space.into(),
        game_id: game_id.into(),
        revision: 1,
        instances: vec![instance.clone()],
        installations: vec![],
    };
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();

    let folder = root.path().join(format!("source-{space}"));
    let source = folder.join("@mod_01");
    fs::create_dir_all(source.join("empty-directory")).unwrap();
    fs::create_dir_all(root.path().join(format!("client-{space}"))).unwrap();
    fs::write(source.join("empty.bin"), b"").unwrap();
    fs::write(source.join("nested.bin"), b"initial local content").unwrap();
    let mut sources = ContentSources::default();
    sources.add_folder("hosted", &folder).unwrap();
    sources
        .add_profile(RepositoryProfile {
            name: "RepoAlpha".into(),
            remote_url: "https://repo.example.invalid/main".into(),
            local_path: root.path().join(format!("client-{space}")),
            folder_id: "hosted".into(),
            supply,
            addons: vec!["@mod_01".into()],
        })
        .unwrap();
    sources.save(&store.context().space_dir()).unwrap();
    let claim = store.context().database_path();
    Fixture {
        _root: root,
        store,
        id: instance.id,
        claim,
        source,
    }
}

fn binding_request(store: &ServerStore, id: &ServerInstanceId) -> ContentPlanRequest {
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
        bindings: vec![ServerAddonBinding {
            source_id: "RepoAlpha".into(),
            role: ServerAddonRole::Required,
            order: 0,
        }],
    }
}

fn claim(path: &Path) {
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(path),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
}

fn release(path: &Path) {
    crate::core::tasks::db_process_lock::release_for(path).unwrap();
}

fn save_binding(fixture: &Fixture) -> String {
    let before = fixture.store.content_bindings(&fixture.id).unwrap();
    let plan = fixture
        .store
        .plan_content_bindings(
            binding_request(&fixture.store, &fixture.id),
            before.fingerprint.as_deref(),
        )
        .unwrap();
    claim(&fixture.claim);
    let saved = fixture.store.apply_content_bindings(plan).unwrap();
    release(&fixture.claim);
    saved.fingerprint.unwrap()
}

fn stage_plan(fixture: &Fixture) -> ContentStagePlan {
    let bindings = fixture.store.content_bindings(&fixture.id).unwrap();
    fixture
        .store
        .plan_content_stage(
            &fixture.id,
            bindings.fingerprint.as_deref().unwrap(),
            &ContentStageControl::default(),
        )
        .unwrap()
}

fn owned_stage(fixture: &Fixture, plan: ContentStagePlan) -> ContentStageReport {
    claim(&fixture.claim);
    let report = fixture
        .store
        .apply_content_stage(plan, &ContentStageControl::default())
        .unwrap();
    release(&fixture.claim);
    report
}

fn assert_no_owned_temporary_stages(fixture: &Fixture, preserved: Option<&Path>) {
    let root = fixture.store.stage_root(&fixture.id);
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if preserved.is_some_and(|preserved| preserved == path) {
            continue;
        }
        assert!(
            !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".stage-build-")
        );
    }
}

fn run_claimed_scenarios() {
    assert!(
        crate::core::game::registry()
            .get("arma3")
            .unwrap()
            .server_capabilities()
            .content_staging
    );
    assert!(
        crate::core::game::registry()
            .get("reforger")
            .unwrap()
            .server_capabilities()
            .content_staging
    );

    for space in ["alpha", "beta"] {
        let fixture = make_fixture(space, AddonSupply::AlreadyPresent);
        let binding_fp = save_binding(&fixture);
        let catalog_before = fs::read(fixture.store.path()).unwrap();
        let sources_path = fixture
            .store
            .context()
            .space_dir()
            .join("server/content-sources.json");
        let sources_before = fs::read(&sources_path).unwrap();
        let plan_a = fixture
            .store
            .plan_content_stage(&fixture.id, &binding_fp, &ContentStageControl::default())
            .unwrap();
        let plan_repeat = fixture
            .store
            .plan_content_stage(&fixture.id, &binding_fp, &ContentStageControl::default())
            .unwrap();
        assert_eq!(plan_a.fingerprint, plan_repeat.fingerprint);
        assert_eq!(plan_a.stage_id, plan_repeat.stage_id);
        assert!(!plan_a.directory.exists());
        assert_eq!(fs::read(fixture.store.path()).unwrap(), catalog_before);
        assert_eq!(fs::read(sources_path).unwrap(), sources_before);
        assert!(
            plan_a
                .manifest
                .entries
                .iter()
                .any(|entry| entry.path.ends_with("/empty-directory")
                    && entry.kind == StageEntryKind::Directory)
        );
        assert!(
            plan_a
                .manifest
                .entries
                .iter()
                .any(|entry| entry.path.ends_with("/empty.bin") && entry.bytes == 0)
        );
        assert!(
            plan_a
                .manifest
                .entries
                .iter()
                .any(|entry| entry.path.ends_with("/nested.bin") && entry.blake3.is_some())
        );

        let reusable = plan_a.clone();
        let report = owned_stage(&fixture, plan_a);
        assert!(report.verified && report.published && !report.reused_existing);
        assert!(
            !report.deployment_ready
                && !report.active_deployment_changed
                && !report.downloads_started
        );
        assert_eq!(
            fs::read(report.directory.join("addons/@mod_01/nested.bin")).unwrap(),
            b"initial local content"
        );
        assert!(
            report
                .directory
                .join("addons/@mod_01/empty-directory")
                .is_dir()
        );
        assert!(report.directory.join("addons/@mod_01/empty.bin").is_file());
        claim(&fixture.claim);
        let reused = fixture
            .store
            .apply_content_stage(reusable, &ContentStageControl::default())
            .unwrap();
        release(&fixture.claim);
        assert!(reused.verified && !reused.published && reused.reused_existing);
        let verified = fixture
            .store
            .verify_content_stage(
                &fixture.id,
                &report.stage_id,
                &ContentStageControl::default(),
            )
            .unwrap();
        assert!(verified.verified && !verified.published && !verified.reused_existing);
        let listed = fixture.store.content_stages(&fixture.id).unwrap();
        assert_eq!(listed.stage_ids, vec![report.stage_id.clone()]);
        assert!(!listed.integrity_verified && !listed.writes_performed);
    }

    let fixture = make_fixture("alpha", AddonSupply::AlreadyPresent);
    save_binding(&fixture);
    let first = owned_stage(&fixture, stage_plan(&fixture));
    let stale = stage_plan(&fixture);
    fs::write(fixture.source.join("nested.bin"), b"changed after review").unwrap();
    claim(&fixture.claim);
    assert_eq!(
        fixture
            .store
            .apply_content_stage(stale, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&fixture.claim);
    assert_eq!(
        fs::read(first.directory.join("addons/@mod_01/nested.bin")).unwrap(),
        b"initial local content"
    );
    let fresh = stage_plan(&fixture);
    assert_ne!(fresh.stage_id, first.stage_id);
    let second = owned_stage(&fixture, fresh);
    assert!(second.published);
    assert_eq!(
        fs::read(second.directory.join("addons/@mod_01/nested.bin")).unwrap(),
        b"changed after review"
    );
    fs::write(fixture.source.join("nested.bin"), b"later live change").unwrap();
    let archived = fixture
        .store
        .verify_content_stage(
            &fixture.id,
            &first.stage_id,
            &ContentStageControl::default(),
        )
        .unwrap();
    assert!(archived.verified);
    assert_eq!(archived.fingerprint, first.fingerprint);

    for drift in ["catalog", "sources", "bindings"] {
        let fixture = make_fixture("alpha", AddonSupply::AlreadyPresent);
        save_binding(&fixture);
        let plan = stage_plan(&fixture);
        let path = match drift {
            "catalog" => fixture.store.path(),
            "sources" => fixture
                .store
                .context()
                .space_dir()
                .join("server/content-sources.json"),
            _ => fixture
                .store
                .context()
                .space_dir()
                .join("server/instances")
                .join(fixture.id.as_str())
                .join("content-bindings.json"),
        };
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(b'\n');
        fs::write(&path, &bytes).unwrap();
        claim(&fixture.claim);
        assert_eq!(
            fixture
                .store
                .apply_content_stage(plan.clone(), &ContentStageControl::default())
                .unwrap_err(),
            ServerStoreError::StaleRevision,
            "{drift}"
        );
        release(&fixture.claim);
        assert!(!plan.directory.exists(), "{drift}");
        assert_eq!(fs::read(path).unwrap(), bytes);
    }

    let fixture = make_fixture("alpha", AddonSupply::AlreadyPresent);
    save_binding(&fixture);
    let plan = stage_plan(&fixture);
    let mut tampered = plan.clone();
    tampered.fingerprint.push_str("changed");
    claim(&fixture.claim);
    assert_eq!(
        fixture
            .store
            .apply_content_stage(tampered, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&fixture.claim);
    assert!(!plan.directory.exists());
    assert_eq!(
        fixture
            .store
            .apply_content_stage(plan, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::Ownership
    );

    let fixture = make_fixture("alpha", AddonSupply::AlreadyPresent);
    save_binding(&fixture);
    let plan = stage_plan(&fixture);
    let beta = ServerStore::new(
        ServerContext::capture(fixture.store.context().data_root(), Some("beta")).unwrap(),
    )
    .unwrap();
    let beta_claim = beta.context().database_path();
    claim(&beta_claim);
    assert_eq!(
        beta.apply_content_stage(plan.clone(), &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&beta_claim);
    assert!(!plan.directory.exists());

    let fixture = make_fixture("alpha", AddonSupply::Download);
    let binding_fp = save_binding(&fixture);
    assert!(matches!(
        fixture
            .store
            .plan_content_stage(&fixture.id, &binding_fp, &ContentStageControl::default()),
        Err(ServerStoreError::Invalid(_))
    ));

    let missing = make_fixture("alpha", AddonSupply::AlreadyPresent);
    save_binding(&missing);
    fs::remove_dir_all(&missing.source).unwrap();
    let missing_fingerprint = missing
        .store
        .content_bindings(&missing.id)
        .unwrap()
        .fingerprint
        .unwrap();
    assert!(
        missing
            .store
            .plan_content_stage(
                &missing.id,
                &missing_fingerprint,
                &ContentStageControl::default()
            )
            .is_err()
    );

    let overlapping = make_fixture("alpha", AddonSupply::AlreadyPresent);
    save_binding(&overlapping);
    let sources = overlapping.store.content_sources().unwrap().sources;
    let overlap_folder = overlapping
        .store
        .stage_root(&overlapping.id)
        .join("registered-source");
    let overlap_source = overlap_folder.join("@mod_01");
    fs::create_dir_all(&overlap_source).unwrap();
    fs::write(overlap_source.join("file.bin"), b"source in output root").unwrap();
    let mut updated = sources;
    updated.folders[0].path = overlap_folder;
    updated
        .save(&overlapping.store.context().space_dir())
        .unwrap();
    save_binding(&overlapping);
    let overlapping_fingerprint = overlapping
        .store
        .content_bindings(&overlapping.id)
        .unwrap()
        .fingerprint
        .unwrap();
    assert_eq!(
        overlapping
            .store
            .plan_content_stage(
                &overlapping.id,
                &overlapping_fingerprint,
                &ContentStageControl::default(),
            )
            .unwrap_err(),
        ServerStoreError::UnsafePath,
    );

    for corrupt in ["file", "extra", "arbitrary"] {
        let fixture = make_fixture("alpha", AddonSupply::AlreadyPresent);
        save_binding(&fixture);
        let plan = stage_plan(&fixture);
        if corrupt == "arbitrary" {
            fs::create_dir_all(&plan.directory).unwrap();
            fs::write(plan.directory.join("operator-data"), b"keep").unwrap();
        } else {
            let report = owned_stage(&fixture, plan.clone());
            if corrupt == "file" {
                fs::write(
                    report.directory.join("addons/@mod_01/nested.bin"),
                    b"tampered",
                )
                .unwrap();
            } else {
                fs::write(report.directory.join("addons/@mod_01/extra.bin"), b"extra").unwrap();
            }
        }
        let target = plan.directory.clone();
        claim(&fixture.claim);
        assert!(
            fixture
                .store
                .apply_content_stage(plan.clone(), &ContentStageControl::default())
                .is_err(),
            "{corrupt}"
        );
        release(&fixture.claim);
        if corrupt == "arbitrary" {
            assert_eq!(fs::read(target.join("operator-data")).unwrap(), b"keep");
        } else {
            assert!(
                fixture
                    .store
                    .verify_content_stage(
                        &fixture.id,
                        &plan.stage_id,
                        &ContentStageControl::default()
                    )
                    .is_err()
            );
            assert!(target.exists());
        }
    }

    for cancel_check in [false, true] {
        let fixture = make_fixture("alpha", AddonSupply::AlreadyPresent);
        save_binding(&fixture);
        let original = owned_stage(&fixture, stage_plan(&fixture));
        fs::write(fixture.source.join("nested.bin"), vec![b'x'; 256 * 1024]).unwrap();
        let plan = stage_plan(&fixture);
        let control = ContentStageControl::default();
        if cancel_check {
            control.cancel_on_phase_for_test(ContentStagePhase::Checking);
        } else {
            control.cancel_after_copy_bytes_for_test(1);
        }
        let preserved = if cancel_check {
            None
        } else {
            let path = fixture
                .store
                .stage_root(&fixture.id)
                .join(".stage-build-preserved");
            fs::create_dir(&path).unwrap();
            fs::write(path.join("operator-data"), b"keep").unwrap();
            Some(path)
        };
        claim(&fixture.claim);
        assert_eq!(
            fixture
                .store
                .apply_content_stage(plan.clone(), &control)
                .unwrap_err(),
            ServerStoreError::Cancelled
        );
        release(&fixture.claim);
        assert!(!plan.directory.exists());
        assert!(original.directory.exists());
        assert_no_owned_temporary_stages(&fixture, preserved.as_deref());
        if let Some(path) = preserved {
            assert_eq!(fs::read(path.join("operator-data")).unwrap(), b"keep");
        }
        assert_eq!(
            fixture.store.content_stages(&fixture.id).unwrap().stage_ids,
            vec![original.stage_id]
        );
    }
}

#[test]
fn staging_contracts_are_checked_under_an_isolated_database_claim() {
    if std::env::var_os(CLAIM_TEST).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::server::content_staging::tests::staging_contracts_are_checked_under_an_isolated_database_claim",
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
    run_claimed_scenarios();
}
