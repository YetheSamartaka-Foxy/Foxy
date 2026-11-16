mod activation;
mod preparation;
mod recovery_inspection;
mod resolution;
mod selection;

use std::fs;
use std::path::PathBuf;

use crate::core::server::content_sources::{AddonSupply, ContentSources, RepositoryProfile};
use crate::core::server::{
    Arma3Difficulty, Arma3Mission, ContentPlanRequest, ContentStageControl,
    DeploymentCompositionRequest, DeploymentRecordPlan, ServerAddonBinding, ServerAddonRole,
    ServerCatalog, ServerContext, ServerDefinition, ServerDefinitionKind, ServerInstance,
    ServerInstanceId, ServerStore, ServerStoreError,
};

const CLAIM_TEST: &str = "FOXY_DEPLOYMENT_RECORD_CLAIM_TEST";

fn run_isolated(name: &str) -> bool {
    if std::env::var(CLAIM_TEST).as_deref() == Ok(name) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("core::server::deployment_records::tests::{name}"),
            "--nocapture",
        ])
        .env(CLAIM_TEST, name)
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
    fs::write(root.path().join("games.json"), serde_json::json!({
        "active_game_space_id":"alpha", "last_opened_game_space_id":"alpha",
        "game_spaces":[{"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0},{"id":"beta","game_id":"reforger","display_name":"Beta","created_at":0}]
    }).to_string()).unwrap();
    for id in ["alpha", "beta"] {
        fs::create_dir_all(root.path().join("games").join(id)).unwrap();
    }
    let (game, kind) = if space == "alpha" {
        ("arma3", ServerDefinitionKind::Arma3)
    } else {
        ("reforger", ServerDefinitionKind::Reforger)
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
    } else if let ServerDefinition::Reforger(settings) = &mut instance.definition {
        settings.scenario_id = Some("{0123456789ABCDEF}Missions/Example.conf".into());
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
    fs::write(addon.join("mpmissions/Mission.Altis.pbo"), b"mission").unwrap();
    fs::create_dir_all(addon.join("keys")).unwrap();
    fs::write(addon.join("keys/server.bikey"), b"key").unwrap();
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

fn claim(f: &Fixture) {
    assert_eq!(
        crate::core::tasks::db_process_lock::acquire(&f.claim),
        crate::core::tasks::db_process_lock::LockOutcome::Acquired
    );
}
fn release(f: &Fixture) {
    crate::core::tasks::db_process_lock::release_for(&f.claim).unwrap();
}

fn prepare_inputs(f: &Fixture) -> String {
    let catalog = f.store.load().unwrap();
    let instance = catalog.instances.iter().find(|i| i.id == f.id).unwrap();
    let req = ContentPlanRequest {
        instance_id: f.id.clone(),
        expected_instance_revision: instance.revision,
        expected_catalog_revision: catalog.revision,
        content_sources_fingerprint: f.store.content_sources().unwrap().fingerprint,
        bindings: vec![ServerAddonBinding {
            source_id: "RepoAlpha".into(),
            role: ServerAddonRole::Required,
            order: 0,
        }],
    };
    let old = f.store.content_bindings(&f.id).unwrap();
    let bp = f
        .store
        .plan_content_bindings(req, old.fingerprint.as_deref())
        .unwrap();
    claim(f);
    let saved = f.store.apply_content_bindings(bp).unwrap();
    release(f);
    let stage = f
        .store
        .plan_content_stage(
            &f.id,
            saved.fingerprint.as_deref().unwrap(),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(f);
    f.store
        .apply_content_stage(stage.clone(), &ContentStageControl::default())
        .unwrap();
    release(f);
    let config = f.store.plan_configuration_commit(&f.id).unwrap();
    claim(f);
    f.store.apply_configuration_commit(config).unwrap();
    release(f);
    stage.stage_id
}

fn request(f: &Fixture, stage: &str) -> DeploymentCompositionRequest {
    DeploymentCompositionRequest {
        instance_id: f.id.clone(),
        stage_id: Some(stage.into()),
        expected_revision: Some(1),
        expected_configuration_generation: Some(1),
    }
}

fn records_root(f: &Fixture) -> PathBuf {
    f.store
        .context()
        .space_dir()
        .join("server/instances")
        .join(f.id.as_str())
        .join("deployments")
}

fn record_plan(f: &Fixture, stage: &str, generation: Option<u64>) -> DeploymentRecordPlan {
    f.store
        .plan_deployment_record(
            request(f, stage),
            generation,
            &ContentStageControl::default(),
        )
        .unwrap()
}

fn commit(f: &Fixture, plan: DeploymentRecordPlan) -> crate::core::server::DeploymentRecordCommit {
    claim(f);
    let result = f
        .store
        .apply_deployment_record(plan, &ContentStageControl::default());
    release(f);
    result.unwrap()
}

#[test]
fn prepare_commit_list_verify_are_deterministic_and_keep_runtime_authority_false() {
    if run_isolated("prepare_commit_list_verify_are_deterministic_and_keep_runtime_authority_false")
    {
        return;
    }
    for space in ["alpha", "beta"] {
        let f = fixture(space);
        let stage = prepare_inputs(&f);
        let before = fs::read(f.store.path()).unwrap();
        let p = record_plan(&f, &stage, Some(0));
        let repeated = record_plan(&f, &stage, Some(0));
        assert_eq!(p.fingerprint, repeated.fingerprint);
        assert_eq!(p.record_id, repeated.record_id);
        assert!(!p.directory().exists());
        assert_eq!(fs::read(f.store.path()).unwrap(), before);
        let record_id = p.record_id.clone();
        let committed = commit(&f, p);
        assert_eq!(committed.state.generation, 1);
        assert_eq!(
            committed.state.selected.as_deref(),
            Some(record_id.as_str())
        );
        assert!(committed.published);
        assert!(committed.selection_changed);
        assert!(!committed.active_deployment_changed);
        assert!(!committed.process_started);
        let state = f.store.deployment_records(&f.id).unwrap();
        assert_eq!(state, committed.state);
        let verified = f
            .store
            .verify_deployment_record(&f.id, &record_id, &ContentStageControl::default())
            .unwrap();
        assert!(
            verified.integrity_verified
                && verified.configuration_verified
                && verified.content_verified
        );
        assert!(
            !verified.deployment_ready
                && !verified.writes_performed
                && !verified.active_deployment_changed
        );
        let reused = record_plan(&f, &stage, Some(1));
        let reused = commit(&f, reused);
        assert!(!reused.published);
        assert_eq!(reused.state.generation, 1);
        assert_eq!(reused.state.selected.as_deref(), Some(record_id.as_str()));
    }
}

#[test]
fn stale_generation_or_inputs_and_tampered_plans_do_not_change_selection() {
    if run_isolated("stale_generation_or_inputs_and_tampered_plans_do_not_change_selection") {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = record_plan(&f, &stage, Some(0));
    let saved = commit(&f, first);
    let state = saved.state.clone();
    assert!(
        f.store
            .plan_deployment_record(
                request(&f, &stage),
                Some(0),
                &ContentStageControl::default()
            )
            .is_err()
    );
    let mut tampered = record_plan(&f, &stage, Some(1));
    tampered.manifest.content_stage_id = "stage_".to_string();
    claim(&f);
    assert!(
        f.store
            .apply_deployment_record(tampered, &ContentStageControl::default())
            .is_err()
    );
    release(&f);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), state);
    let mut tampered_review = record_plan(&f, &stage, Some(1));
    tampered_review.review.content.current_sources_fingerprint = None;
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record(tampered_review, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&f);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), state);
    let mut bytes = fs::read(
        f.store
            .context()
            .space_dir()
            .join("server/content-sources.json"),
    )
    .unwrap();
    bytes.push(b'\n');
    fs::write(
        f.store
            .context()
            .space_dir()
            .join("server/content-sources.json"),
        bytes,
    )
    .unwrap();
    let stale = f.store.plan_deployment_record(
        request(&f, &stage),
        Some(1),
        &ContentStageControl::default(),
    );
    assert!(matches!(stale, Err(ServerStoreError::Invalid(_))));
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), state);
}

#[test]
fn corruption_cancellation_and_unowned_entries_preserve_prior_selection() {
    if run_isolated("corruption_cancellation_and_unowned_entries_preserve_prior_selection") {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = commit(&f, record_plan(&f, &stage, Some(0)));
    let old = first.state.clone();
    let root = f
        .store
        .context()
        .space_dir()
        .join("server/instances")
        .join(f.id.as_str())
        .join("deployments/records");
    fs::write(
        root.join(&first.record.record_id).join("operator-data"),
        b"keep",
    )
    .unwrap();
    let cancelled = ContentStageControl::default();
    cancelled.cancel();
    let plan = record_plan(&f, &stage, Some(1));
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record(plan, &cancelled)
            .unwrap_err(),
        ServerStoreError::RecordCancelled
    );
    release(&f);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), old);
    assert_eq!(
        fs::read(root.join(&first.record.record_id).join("operator-data")).unwrap(),
        b"keep"
    );
    let verification = f.store.verify_deployment_record(
        &f.id,
        &first.record.record_id,
        &ContentStageControl::default(),
    );
    assert!(verification.is_err());
}

#[test]
fn old_records_verify_after_source_and_draft_selection_drift() {
    if run_isolated("old_records_verify_after_source_and_draft_selection_drift") {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let committed = commit(&f, record_plan(&f, &stage, Some(0)));
    let source_path = f
        .store
        .context()
        .space_dir()
        .join("server/content-sources.json");
    let mut source = fs::read(&source_path).unwrap();
    source.push(b'\n');
    fs::write(source_path, source).unwrap();
    let mut catalog = f.store.load().unwrap();
    catalog.revision += 1;
    catalog.instances[0].revision += 1;
    catalog.instances[0].display_name = "New draft".into();
    fs::write(f.store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();
    let state_path = records_root(&f).join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["generation"] = 2.into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let verified = f
        .store
        .verify_deployment_record(
            &f.id,
            &committed.record.record_id,
            &ContentStageControl::default(),
        )
        .unwrap();
    assert!(
        verified.integrity_verified && verified.configuration_verified && verified.content_verified
    );
    assert_eq!(f.store.deployment_records(&f.id).unwrap().generation, 2);
}

#[test]
fn raw_selection_bytes_and_unclaimed_apply_cannot_replace_a_selection() {
    if run_isolated("raw_selection_bytes_and_unclaimed_apply_cannot_replace_a_selection") {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = commit(&f, record_plan(&f, &stage, Some(0)));
    let old = first.state.clone();
    let plan = record_plan(&f, &stage, Some(1));
    assert_eq!(
        f.store
            .apply_deployment_record(plan, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::Ownership
    );
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), old);
    let plan = record_plan(&f, &stage, Some(1));
    let path = records_root(&f).join("state.json");
    let mut altered = fs::read(&path).unwrap();
    altered.push(b'\n');
    fs::write(&path, &altered).unwrap();
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record(plan, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&f);
    assert_eq!(fs::read(&path).unwrap(), altered);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), old);
}

#[test]
fn cancellation_during_archive_check_and_unindexed_orphan_are_recoverable() {
    if run_isolated("cancellation_during_archive_check_and_unindexed_orphan_are_recoverable") {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = commit(&f, record_plan(&f, &stage, Some(0)));
    let old = first.state.clone();
    fs::write(
        f._root
            .path()
            .join("source-alpha/@mod_01/mpmissions/Mission.Altis.pbo"),
        b"new mission bytes",
    )
    .unwrap();
    let binding = f.store.content_bindings(&f.id).unwrap();
    let next_stage = f
        .store
        .plan_content_stage(
            &f.id,
            binding.fingerprint.as_deref().unwrap(),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(&f);
    f.store
        .apply_content_stage(next_stage.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    let second = record_plan(&f, &next_stage.stage_id, Some(1));
    let cancelled = ContentStageControl::default();
    cancelled.cancel_on_phase_for_test(crate::core::server::ContentStagePhase::Checking);
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record(second.clone(), &cancelled)
            .unwrap_err(),
        ServerStoreError::RecordCancelled
    );
    release(&f);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), old);
    let dir = second.directory();
    let state_path = records_root(&f).join("state.json");
    let state_bytes = fs::read(&state_path).unwrap();
    let after_publication = ContentStageControl::default();
    after_publication.cancel_on_record_publication_for_test();
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record(second.clone(), &after_publication)
            .unwrap_err(),
        ServerStoreError::RecordCancelled
    );
    release(&f);
    assert_eq!(fs::read(&state_path).unwrap(), state_bytes);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    assert!(fs::read_dir(dir.parent().unwrap()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".record-build-")
    }));
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), old);
    assert!(
        f.store
            .verify_deployment_record(&f.id, &second.record_id, &ContentStageControl::default())
            .unwrap()
            .integrity_verified
    );
    claim(&f);
    let committed = f
        .store
        .apply_deployment_record(second, &ContentStageControl::default())
        .unwrap();
    release(&f);
    assert!(!committed.published);
    assert!(committed.selection_changed);
    assert!(committed.state.records.contains(&first.record.record_id));
    assert!(
        committed
            .state
            .records
            .contains(&committed.record.record_id)
    );
}

#[test]
fn strict_record_dtos_and_unknown_future_storage_are_preserved() {
    if run_isolated("strict_record_dtos_and_unknown_future_storage_are_preserved") {
        return;
    }
    let mut manifest: super::DeploymentRecordManifest = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/deployment_record_manifest.json"
    ))
    .unwrap();
    assert_eq!(manifest.schema_version, 1);
    let f = fixture("alpha");
    manifest.game_space_id = "alpha".into();
    manifest.instance_id = f.id.clone();
    manifest.content_bindings.game_space_id = "alpha".into();
    manifest.content_bindings.instance_id = f.id.clone();
    f.store.validate_record_manifest(&f.id, &manifest).unwrap();
    let stage = prepare_inputs(&f);
    let committed = commit(&f, record_plan(&f, &stage, Some(0)));
    let state_path = records_root(&f).join("state.json");
    let original_state = fs::read(&state_path).unwrap();
    for (bytes, error) in [
        (
            br#"{"schema_version":1,"unexpected":true}"#.to_vec(),
            ServerStoreError::Corrupt,
        ),
        (
            br#"{"schema_version":99}"#.to_vec(),
            ServerStoreError::UnsupportedVersion,
        ),
    ] {
        fs::write(&state_path, &bytes).unwrap();
        assert_eq!(f.store.deployment_records(&f.id).unwrap_err(), error);
        assert_eq!(fs::read(&state_path).unwrap(), bytes);
    }
    fs::write(&state_path, &original_state).unwrap();
    let manifest_path = records_root(&f)
        .join("records")
        .join(&committed.record.record_id)
        .join("manifest.json");
    let original = fs::read(&manifest_path).unwrap();
    for (bytes, error) in [
        (
            br#"{"schema_version":1,"unexpected":true}"#.to_vec(),
            ServerStoreError::Corrupt,
        ),
        (
            br#"{"schema_version":99}"#.to_vec(),
            ServerStoreError::UnsupportedVersion,
        ),
    ] {
        fs::write(&manifest_path, &bytes).unwrap();
        assert_eq!(
            f.store
                .verify_deployment_record(
                    &f.id,
                    &committed.record.record_id,
                    &ContentStageControl::default()
                )
                .unwrap_err(),
            error
        );
        assert_eq!(fs::read(&manifest_path).unwrap(), bytes);
    }
    fs::write(&manifest_path, &original).unwrap();
    assert_eq!(
        f.store
            .verify_deployment_record(
                &f.id,
                "deployment_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &ContentStageControl::default()
            )
            .unwrap_err(),
        ServerStoreError::NotFound
    );
}

#[test]
fn record_destination_cannot_write_inside_a_client_installation() {
    if run_isolated("record_destination_cannot_write_inside_a_client_installation") {
        return;
    }
    let f = fixture("alpha");
    let root = f.store.record_root(&f.id);
    let mut sources = f.store.content_sources().unwrap().sources;
    sources.profiles[0].local_path = root.clone();
    sources.save(&f.store.context().space_dir()).unwrap();
    let stage = prepare_inputs(&f);
    assert_eq!(
        f.store
            .plan_deployment_record(request(&f, &stage), None, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::UnsafePath
    );
    assert!(!root.exists());
}
