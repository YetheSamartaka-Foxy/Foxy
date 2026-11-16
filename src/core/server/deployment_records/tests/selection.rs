use super::*;
use std::collections::BTreeMap;

fn inventory(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for item in std::fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                out.insert(path.strip_prefix(base).unwrap().into(), Vec::new());
                visit(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().into(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

pub(super) fn add_second_record(f: &Fixture) -> String {
    std::fs::write(
        f._root.path().join(format!(
            "source-{}/@mod_01/mpmissions/Mission.Altis.pbo",
            f.store.context().space_id()
        )),
        b"new mission bytes",
    )
    .unwrap();
    let bindings = f.store.content_bindings(&f.id).unwrap();
    let next_stage = f
        .store
        .plan_content_stage(
            &f.id,
            bindings.fingerprint.as_deref().unwrap(),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(f);
    f.store
        .apply_content_stage(next_stage.clone(), &ContentStageControl::default())
        .unwrap();
    release(f);
    let plan = record_plan(f, &next_stage.stage_id, Some(1));
    let id = plan.record_id.clone();
    commit(f, plan);
    id
}

#[test]
fn selecting_old_record_ignores_current_draft_source_and_config_selection() {
    if run_isolated(
        "selection::selecting_old_record_ignores_current_draft_source_and_config_selection",
    ) {
        return;
    }
    for space in ["alpha", "beta"] {
        let f = fixture(space);
        let old_stage = prepare_inputs(&f);
        let first = commit(&f, record_plan(&f, &old_stage, Some(0)));
        let old_record = first.record.record_id.clone();
        let newer_record = add_second_record(&f);
        if space == "alpha" {
            let mut catalog = f.store.load().unwrap();
            catalog.revision += 1;
            catalog.instances[0].revision += 1;
            catalog.instances[0].display_name = "Changed Draft".into();
            std::fs::write(f.store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();
            let config = f.store.plan_configuration_commit(&f.id).unwrap();
            claim(&f);
            f.store.apply_configuration_commit(config).unwrap();
            release(&f);
        }
        let before = inventory(f._root.path());
        let plan = f
            .store
            .plan_deployment_record_selection(
                &f.id,
                &old_record,
                2,
                &ContentStageControl::default(),
            )
            .unwrap();
        let stable = f
            .store
            .plan_deployment_record_selection(
                &f.id,
                &old_record,
                2,
                &ContentStageControl::default(),
            )
            .unwrap();
        assert_eq!(plan.fingerprint, stable.fingerprint);
        assert_eq!(
            plan.previous_record_id.as_deref(),
            Some(newer_record.as_str())
        );
        assert_eq!(inventory(f._root.path()), before);
        claim(&f);
        let applied = f
            .store
            .apply_deployment_record_selection(plan, &ContentStageControl::default())
            .unwrap();
        release(&f);
        assert_eq!(applied.state.selected.as_deref(), Some(old_record.as_str()));
        assert!(applied.selection_changed);
        assert!(
            !applied.published && !applied.active_deployment_changed && !applied.process_started
        );
        let after = inventory(f._root.path());
        assert_eq!(after.len(), before.len());
        let selected_state = PathBuf::from(format!(
            "games/{space}/server/instances/{}/deployments/state.json",
            f.id.as_str()
        ));
        for (path, bytes) in before {
            if path == selected_state {
                assert_ne!(after.get(&path), Some(&bytes));
            } else {
                assert_eq!(after.get(&path), Some(&bytes));
            }
        }
        assert!(
            f.store
                .verify_deployment_record(&f.id, &old_record, &ContentStageControl::default())
                .unwrap()
                .integrity_verified
        );
    }
}

#[test]
fn selection_refuses_stale_raw_state_tampering_unclaimed_and_unindexed_targets() {
    if run_isolated(
        "selection::selection_refuses_stale_raw_state_tampering_unclaimed_and_unindexed_targets",
    ) {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = commit(&f, record_plan(&f, &stage, Some(0)));
    let old = first.record.record_id.clone();
    let second = add_second_record(&f);
    let state = f.store.deployment_records(&f.id).unwrap();
    let unindexed = format!("deployment_{}", "a".repeat(64));
    assert_eq!(
        f.store
            .plan_deployment_record_selection(&f.id, &unindexed, 2, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::NotFound
    );
    let index_path = records_root(&f).join("state.json");
    let index_bytes = fs::read(&index_path).unwrap();
    let mut state_value: serde_json::Value = serde_json::from_slice(&index_bytes).unwrap();
    state_value["records"] = serde_json::json!([second.clone()]);
    let unlisted_bytes = serde_json::to_vec(&state_value).unwrap();
    fs::write(&index_path, &unlisted_bytes).unwrap();
    assert!(
        f.store
            .verify_deployment_record(&f.id, &old, &ContentStageControl::default())
            .unwrap()
            .integrity_verified
    );
    assert_eq!(
        f.store
            .plan_deployment_record_selection(&f.id, &old, 2, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::NotFound
    );
    fs::write(&index_path, &index_bytes).unwrap();
    let other = fixture("beta");
    assert_eq!(
        other
            .store
            .plan_deployment_record_selection(
                &other.id,
                &first.record.record_id,
                0,
                &ContentStageControl::default()
            )
            .unwrap_err(),
        ServerStoreError::NotFound
    );

    let plan = f
        .store
        .plan_deployment_record_selection(&f.id, &old, 2, &ContentStageControl::default())
        .unwrap();
    assert_eq!(
        f.store
            .apply_deployment_record_selection(plan.clone(), &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::Ownership
    );
    let mut tampered = plan.clone();
    tampered.target.record_id = second;
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record_selection(tampered, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&f);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), state);

    let path = records_root(&f).join("state.json");
    let mut changed = std::fs::read(&path).unwrap();
    changed.push(b'\n');
    std::fs::write(&path, &changed).unwrap();
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record_selection(plan, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(&f);
    assert_eq!(std::fs::read(path).unwrap(), changed);
}

#[test]
fn selecting_already_selected_is_noop_and_cancellation_preserves_index_bytes() {
    if run_isolated(
        "selection::selecting_already_selected_is_noop_and_cancellation_preserves_index_bytes",
    ) {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = commit(&f, record_plan(&f, &stage, Some(0)));
    let old = first.record.record_id.clone();
    let second = add_second_record(&f);
    let plan = f
        .store
        .plan_deployment_record_selection(&f.id, &second, 2, &ContentStageControl::default())
        .unwrap();
    let path = records_root(&f).join("state.json");
    let before_inventory = inventory(f._root.path());
    let before = std::fs::read(&path).unwrap();
    claim(&f);
    let cancelled = ContentStageControl::default();
    cancelled.cancel_on_phase_for_test(crate::core::server::ContentStagePhase::Checking);
    assert_eq!(
        f.store
            .apply_deployment_record_selection(plan.clone(), &cancelled)
            .unwrap_err(),
        ServerStoreError::RecordCancelled
    );
    release(&f);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(inventory(f._root.path()), before_inventory);
    let cancelled_before_guard = ContentStageControl::default();
    cancelled_before_guard.cancel_before_record_selection_for_test();
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record_selection(plan.clone(), &cancelled_before_guard)
            .unwrap_err(),
        ServerStoreError::RecordCancelled
    );
    release(&f);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    claim(&f);
    let applied = f
        .store
        .apply_deployment_record_selection(plan, &ContentStageControl::default())
        .unwrap();
    release(&f);
    assert_eq!(applied.state.generation, 2);
    let noop = f
        .store
        .plan_deployment_record_selection(&f.id, &second, 2, &ContentStageControl::default())
        .unwrap();
    let no_bytes = std::fs::read(&path).unwrap();
    claim(&f);
    let noop = f
        .store
        .apply_deployment_record_selection(noop, &ContentStageControl::default())
        .unwrap();
    release(&f);
    assert!(!noop.selection_changed);
    assert_eq!(noop.state.generation, 2);
    assert_eq!(std::fs::read(path).unwrap(), no_bytes);
    assert!(
        f.store
            .verify_deployment_record(&f.id, &old, &ContentStageControl::default())
            .unwrap()
            .integrity_verified
    );
}

#[test]
fn selection_plan_rejects_corrupt_referenced_archives_without_repair() {
    if run_isolated("selection::selection_plan_rejects_corrupt_referenced_archives_without_repair")
    {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let first = commit(&f, record_plan(&f, &stage, Some(0)));
    let old = first.record.record_id.clone();
    let second = add_second_record(&f);
    let verified = f
        .store
        .verify_deployment_record(&f.id, &old, &ContentStageControl::default())
        .unwrap();
    let content = f
        .store
        .context()
        .space_dir()
        .join("server/instances")
        .join(f.id.as_str())
        .join("content-stages")
        .join(&verified.manifest.content_stage_id)
        .join("addons/@mod_01/keys/server.bikey");
    let bytes = std::fs::read(&content).unwrap();
    std::fs::write(&content, b"altered retained key").unwrap();
    let index = records_root(&f).join("state.json");
    let index_bytes = std::fs::read(&index).unwrap();
    assert_eq!(
        f.store
            .plan_deployment_record_selection(&f.id, &old, 2, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::Corrupt
    );
    assert_eq!(std::fs::read(&index).unwrap(), index_bytes);
    assert_eq!(std::fs::read(&content).unwrap(), b"altered retained key");
    std::fs::write(&content, &bytes).unwrap();

    let plan = f
        .store
        .plan_deployment_record_selection(&f.id, &old, 2, &ContentStageControl::default())
        .unwrap();
    let reviewed_index = fs::read(&index).unwrap();
    fs::write(&content, b"changed after review").unwrap();
    let changed_content = fs::read(&content).unwrap();
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_record_selection(plan, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::Corrupt
    );
    release(&f);
    assert_eq!(fs::read(&index).unwrap(), reviewed_index);
    assert_eq!(fs::read(&content).unwrap(), changed_content);
    fs::write(&content, &bytes).unwrap();

    let stage_dir = f
        .store
        .context()
        .space_dir()
        .join("server/instances")
        .join(f.id.as_str())
        .join("content-stages")
        .join(&verified.manifest.content_stage_id);
    let stage_backup = stage_dir.with_extension("selection-test-backup");
    fs::rename(&stage_dir, &stage_backup).unwrap();
    assert_eq!(
        f.store
            .plan_deployment_record_selection(&f.id, &old, 2, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::Corrupt
    );
    assert_eq!(fs::read(&index).unwrap(), reviewed_index);
    fs::rename(stage_backup, stage_dir).unwrap();
    assert!(
        f.store
            .verify_deployment_record(&f.id, &second, &ContentStageControl::default())
            .unwrap()
            .integrity_verified
    );
}

#[test]
fn selection_plan_preserves_corrupt_or_future_index_bytes() {
    if run_isolated("selection::selection_plan_preserves_corrupt_or_future_index_bytes") {
        return;
    }
    let f = fixture("alpha");
    let stage = prepare_inputs(&f);
    let committed = commit(&f, record_plan(&f, &stage, Some(0)));
    let path = records_root(&f).join("state.json");
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
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            f.store
                .plan_deployment_record_selection(
                    &f.id,
                    &committed.record.record_id,
                    1,
                    &ContentStageControl::default()
                )
                .unwrap_err(),
            error
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
