use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::server::content_sources::{AddonSupply, ContentSources, RepositoryProfile};
use crate::core::server::{
    Arma3Difficulty, Arma3Mission, ContentPlanRequest, ContentStageControl, ContentStagePhase,
    DeploymentCompositionRequest, ServerAddonBinding, ServerAddonRole, ServerCatalog,
    ServerContext, ServerDefinition, ServerDefinitionKind, ServerInstance, ServerInstanceId,
    ServerStore, ServerStoreError,
};

fn run_isolated(name: &str) -> bool {
    const CHILD: &str = "FOXY_COMPOSITION_REVIEW_TEST";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("core::server::deployment_composition::tests::{name}"),
            "--nocapture",
        ])
        .env(CHILD, name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

struct Fixture {
    _root: tempfile::TempDir,
    store: ServerStore,
    id: ServerInstanceId,
    claim: PathBuf,
}

fn fixture(space: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        serde_json::json!({
            "active_game_space_id":"alpha", "last_opened_game_space_id":"alpha",
            "game_spaces":[
                {"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0},
                {"id":"beta","game_id":"reforger","display_name":"Beta","created_at":0}
            ]
        })
        .to_string(),
    )
    .unwrap();
    for id in ["alpha", "beta"] {
        fs::create_dir_all(root.path().join("games").join(id)).unwrap();
    }
    let game = if space == "alpha" {
        "arma3"
    } else {
        "reforger"
    };
    let kind = if game == "arma3" {
        ServerDefinitionKind::Arma3
    } else {
        ServerDefinitionKind::Reforger
    };
    let store =
        ServerStore::new(ServerContext::capture(root.path(), Some(space)).unwrap()).unwrap();
    let mut instance = ServerInstance::draft(space, kind, "Fixture").unwrap();
    if let ServerDefinition::Arma3(settings) = &mut instance.definition {
        settings.mission_cycle.push(Arma3Mission {
            enabled: true,
            source: None,
            params: Default::default(),
            template: "Mission.Altis".into(),
            difficulty: Arma3Difficulty::Regular,
        });
    }
    let catalog = ServerCatalog {
        schema_version: super::super::definition::SERVER_SCHEMA_VERSION,
        game_space_id: space.into(),
        game_id: game.into(),
        revision: 1,
        instances: vec![instance.clone()],
        installations: vec![],
    };
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();
    let addon = root.path().join(format!("source-{space}/@mod_01"));
    fs::create_dir_all(addon.join("mpmissions")).unwrap();
    fs::write(addon.join("mpmissions/Mission.Altis.pbo"), b"mission bytes").unwrap();
    fs::create_dir_all(addon.join("keys")).unwrap();
    fs::write(addon.join("keys/server.bikey"), b"key bytes").unwrap();
    fs::create_dir_all(root.path().join(format!("client-{space}"))).unwrap();
    let mut sources = ContentSources::default();
    sources
        .add_folder("hosted", addon.parent().unwrap())
        .unwrap();
    sources
        .add_profile(RepositoryProfile {
            name: "RepoAlpha".into(),
            remote_url: "https://repo.example.invalid/main".into(),
            local_path: root.path().join(format!("client-{space}")),
            folder_id: "hosted".into(),
            supply: AddonSupply::AlreadyPresent,
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
    }
}

fn claim(fixture: &Fixture) {
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&fixture.claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
}

fn release(fixture: &Fixture) {
    crate::core::tasks::db_process_lock::release_for(&fixture.claim).unwrap();
}

fn bind_and_stage(fixture: &Fixture) -> String {
    let catalog = fixture.store.load().unwrap();
    let instance = catalog
        .instances
        .iter()
        .find(|item| item.id == fixture.id)
        .unwrap();
    let request = ContentPlanRequest {
        instance_id: fixture.id.clone(),
        expected_instance_revision: instance.revision,
        expected_catalog_revision: catalog.revision,
        content_sources_fingerprint: fixture.store.content_sources().unwrap().fingerprint,
        bindings: vec![ServerAddonBinding {
            source_id: "RepoAlpha".into(),
            role: ServerAddonRole::Required,
            order: 0,
        }],
    };
    let prior = fixture.store.content_bindings(&fixture.id).unwrap();
    let plan = fixture
        .store
        .plan_content_bindings(request, prior.fingerprint.as_deref())
        .unwrap();
    claim(fixture);
    let saved = fixture.store.apply_content_bindings(plan).unwrap();
    release(fixture);
    let binding_fp = saved.fingerprint.unwrap();
    let stage = fixture
        .store
        .plan_content_stage(&fixture.id, &binding_fp, &ContentStageControl::default())
        .unwrap();
    claim(fixture);
    fixture
        .store
        .apply_content_stage(stage.clone(), &ContentStageControl::default())
        .unwrap();
    release(fixture);
    stage.stage_id
}

fn save_configuration(fixture: &Fixture) {
    let plan = fixture
        .store
        .plan_configuration_commit(&fixture.id)
        .unwrap();
    claim(fixture);
    fixture.store.apply_configuration_commit(plan).unwrap();
    release(fixture);
}

fn request(fixture: &Fixture, stage: Option<String>) -> DeploymentCompositionRequest {
    DeploymentCompositionRequest {
        instance_id: fixture.id.clone(),
        stage_id: stage,
        expected_revision: Some(1),
        expected_configuration_generation: None,
    }
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                out.insert(path.strip_prefix(base).unwrap().to_path_buf(), Vec::new());
                visit(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn composition_is_deterministic_read_only_and_game_scoped() {
    if run_isolated("composition_is_deterministic_read_only_and_game_scoped") {
        return;
    }
    for space in ["alpha", "beta"] {
        let fixture = fixture(space);
        let stage = bind_and_stage(&fixture);
        save_configuration(&fixture);
        let before = inventory(fixture._root.path());
        let plan = fixture
            .store
            .plan_deployment_composition(
                request(&fixture, Some(stage.clone())),
                &ContentStageControl::default(),
            )
            .unwrap();
        let again = fixture
            .store
            .plan_deployment_composition(
                request(&fixture, Some(stage)),
                &ContentStageControl::default(),
            )
            .unwrap();
        assert_eq!(plan.fingerprint, again.fingerprint);
        assert!(
            !plan.deployment_ready
                && !plan.writes_performed
                && !plan.active_deployment_changed
                && !plan.downloads_started
                && !plan.installation_leases_acquired
                && !plan.process_started
        );
        assert!(plan.configuration.integrity_verified && plan.content.integrity_verified);
        if space == "alpha" {
            assert_eq!(plan.artifacts.mission_files, 1);
            assert_eq!(plan.artifacts.key_files, 1);
            assert!(
                plan.artifacts
                    .missions
                    .iter()
                    .any(|item| item.template == "Mission.Altis" && item.state == "candidate")
            );
        } else {
            assert!(plan.artifacts.keys.is_empty() && plan.artifacts.missions.is_empty());
        }
        assert_eq!(inventory(fixture._root.path()), before);
    }
}

#[test]
fn missing_and_corrupt_archives_are_blockers_without_repair_or_deletion() {
    if run_isolated("missing_and_corrupt_archives_are_blockers_without_repair_or_deletion") {
        return;
    }
    let fixture = fixture("alpha");
    let stage = bind_and_stage(&fixture);
    let missing = fixture
        .store
        .plan_deployment_composition(request(&fixture, None), &ContentStageControl::default())
        .unwrap();
    assert!(
        missing
            .blockers
            .iter()
            .any(|issue| issue.code == "configuration_missing")
    );
    assert!(
        missing
            .blockers
            .iter()
            .any(|issue| issue.code == "content_missing")
    );
    save_configuration(&fixture);
    let baseline = fixture
        .store
        .plan_deployment_composition(
            request(&fixture, Some(stage.clone())),
            &ContentStageControl::default(),
        )
        .unwrap();
    let config_dir = fixture
        .store
        .context()
        .space_dir()
        .join("server/instances")
        .join(fixture.id.as_str())
        .join("configuration/revisions")
        .join(baseline.configuration.revision_id.as_deref().unwrap());
    fs::write(config_dir.join("server.cfg"), b"corrupt config").unwrap();
    let stage_path = fixture
        .store
        .context()
        .space_dir()
        .join("server/instances")
        .join(fixture.id.as_str())
        .join("content-stages")
        .join(&stage)
        .join("addons/@mod_01/keys/server.bikey");
    fs::write(&stage_path, b"corrupt key").unwrap();
    let before = inventory(fixture._root.path());
    let report = fixture
        .store
        .plan_deployment_composition(
            request(&fixture, Some(stage)),
            &ContentStageControl::default(),
        )
        .unwrap();
    assert!(!report.configuration.integrity_verified);
    assert!(!report.content.integrity_verified);
    assert!(
        report
            .blockers
            .iter()
            .any(|issue| issue.code == "configuration_invalid")
    );
    assert!(
        report
            .blockers
            .iter()
            .any(|issue| issue.code == "content_unverified")
    );
    assert_eq!(inventory(fixture._root.path()), before);
}

#[test]
fn stale_tokens_bindings_drift_and_cancellation_are_reported_without_writes() {
    if run_isolated("stale_tokens_bindings_drift_and_cancellation_are_reported_without_writes") {
        return;
    }
    let fixture = fixture("alpha");
    let stage = bind_and_stage(&fixture);
    save_configuration(&fixture);
    let mut stale = request(&fixture, Some(stage.clone()));
    stale.expected_revision = Some(99);
    assert_eq!(
        fixture
            .store
            .plan_deployment_composition(stale, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    let mut stale = request(&fixture, Some(stage.clone()));
    stale.expected_configuration_generation = Some(99);
    assert_eq!(
        fixture
            .store
            .plan_deployment_composition(stale, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    let mut missing = request(&fixture, Some(stage.clone()));
    missing.instance_id = ServerInstanceId::new();
    assert_eq!(
        fixture
            .store
            .plan_deployment_composition(missing, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::NotFound
    );
    assert!(matches!(
        fixture.store.plan_deployment_composition(
            request(&fixture, Some("bad".into())),
            &ContentStageControl::default()
        ),
        Err(ServerStoreError::Invalid(_))
    ));

    let catalog = fixture.store.load().unwrap();
    let instance = catalog
        .instances
        .iter()
        .find(|item| item.id == fixture.id)
        .unwrap();
    let replacement = ContentPlanRequest {
        instance_id: fixture.id.clone(),
        expected_instance_revision: instance.revision,
        expected_catalog_revision: catalog.revision,
        content_sources_fingerprint: fixture.store.content_sources().unwrap().fingerprint,
        bindings: vec![ServerAddonBinding {
            source_id: "RepoAlpha".into(),
            role: ServerAddonRole::Optional,
            order: 0,
        }],
    };
    let prior = fixture.store.content_bindings(&fixture.id).unwrap();
    let plan = fixture
        .store
        .plan_content_bindings(replacement, prior.fingerprint.as_deref())
        .unwrap();
    claim(&fixture);
    fixture.store.apply_content_bindings(plan).unwrap();
    release(&fixture);
    let before = inventory(fixture._root.path());
    let report = fixture
        .store
        .plan_deployment_composition(
            request(&fixture, Some(stage)),
            &ContentStageControl::default(),
        )
        .unwrap();
    assert!(report.content.integrity_verified);
    assert_eq!(report.content.matches_saved_bindings, Some(false));
    assert!(
        report
            .blockers
            .iter()
            .any(|issue| issue.code == "content_intent_changed")
    );
    assert_eq!(inventory(fixture._root.path()), before);

    let sources_path = fixture
        .store
        .context()
        .space_dir()
        .join("server/content-sources.json");
    let mut source_bytes = fs::read(&sources_path).unwrap();
    source_bytes.push(b'\n');
    fs::write(&sources_path, &source_bytes).unwrap();
    let before = inventory(fixture._root.path());
    let report = fixture
        .store
        .plan_deployment_composition(
            request(&fixture, Some(report.content.requested_stage_id.unwrap())),
            &ContentStageControl::default(),
        )
        .unwrap();
    assert!(report.content.integrity_verified);
    assert!(!report.content.current_bindings.review_current);
    assert!(
        report
            .blockers
            .iter()
            .any(|issue| issue.code == "bindings_review_required")
    );
    assert_eq!(inventory(fixture._root.path()), before);

    let cancelled_before = ContentStageControl::default();
    cancelled_before.cancel();
    assert_eq!(
        fixture
            .store
            .plan_deployment_composition(
                request(
                    &fixture,
                    Some(report.content.requested_stage_id.clone().unwrap())
                ),
                &cancelled_before
            )
            .unwrap_err(),
        ServerStoreError::ReviewCancelled
    );
    let cancelled = ContentStageControl::default();
    cancelled.cancel_on_phase_for_test(ContentStagePhase::Checking);
    assert_eq!(
        fixture
            .store
            .plan_deployment_composition(
                request(&fixture, Some(report.content.requested_stage_id.unwrap())),
                &cancelled
            )
            .unwrap_err(),
        ServerStoreError::ReviewCancelled
    );
}
