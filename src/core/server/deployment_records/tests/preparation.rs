use super::selection::add_second_record;
use super::*;
use std::collections::BTreeMap;

fn inventory(root: &std::path::Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &std::path::Path, dir: &std::path::Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                out.insert(path.strip_prefix(base).unwrap().into(), Vec::new());
                visit(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn selected_fixture(space: &str) -> (Fixture, String) {
    let f = fixture(space);
    let stage = prepare_inputs(&f);
    let record = commit(&f, record_plan(&f, &stage, Some(0)));
    (f, record.record.record_id)
}

#[test]
fn preparation_plan_is_deterministic_read_only_and_generation_bound() {
    if run_isolated("preparation::preparation_plan_is_deterministic_read_only_and_generation_bound")
    {
        return;
    }
    for space in ["alpha", "beta"] {
        let (f, record) = selected_fixture(space);
        let before = inventory(f._root.path());
        let first = f
            .store
            .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
            .unwrap();
        let second = f
            .store
            .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
            .unwrap();
        assert_eq!(first.preparation_id, second.preparation_id);
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.manifest.record_id, record);
        assert_eq!(first.manifest.schema_version, 1);
        assert_eq!(first.manifest.layout_version, 1);
        assert!(first.preparation_id.starts_with("prepared_"));
        assert_eq!(inventory(f._root.path()), before);
        let mut tampered = first.clone();
        tampered.directory = tampered.directory.with_file_name("prepared_operator");
        assert_tampered(&f, tampered);
        let mut tampered = first.clone();
        tampered.preparation_id.push('x');
        assert_tampered(&f, tampered);
        let mut tampered = first.clone();
        tampered.manifest.total_bytes += 1;
        assert_tampered(&f, tampered);
        let mut tampered = first.clone();
        tampered.fingerprint.push('x');
        assert_tampered(&f, tampered);
        let mut tampered = first.clone();
        tampered.expected_generation += 1;
        assert_tampered(&f, tampered);
        let state_path = f.store.record_root(&f.id).join("state.json");
        let original = fs::read(&state_path).unwrap();
        let mut altered = original.clone();
        altered.push(b'\n');
        fs::write(&state_path, &altered).unwrap();
        claim(&f);
        assert_eq!(
            f.store
                .apply_deployment_preparation(first.clone(), &ContentStageControl::default())
                .unwrap_err(),
            ServerStoreError::StaleRevision
        );
        release(&f);
        assert_eq!(fs::read(&state_path).unwrap(), altered);
        fs::write(&state_path, original).unwrap();
        assert_eq!(
            f.store
                .apply_deployment_preparation(first.clone(), &ContentStageControl::default())
                .unwrap_err(),
            ServerStoreError::Ownership
        );
        assert_eq!(
            f.store
                .plan_deployment_preparation(&f.id, 0, &ContentStageControl::default())
                .unwrap_err(),
            ServerStoreError::StaleRevision
        );
    }
}

fn assert_tampered(f: &Fixture, plan: crate::core::server::DeploymentPreparationPlan) {
    claim(f);
    assert_eq!(
        f.store
            .apply_deployment_preparation(plan, &ContentStageControl::default())
            .unwrap_err(),
        ServerStoreError::StaleRevision
    );
    release(f);
}

#[test]
fn preparation_build_copies_verified_archives_and_reuses_without_activation() {
    if run_isolated(
        "preparation::preparation_build_copies_verified_archives_and_reuses_without_activation",
    ) {
        return;
    }
    let (f, record) = selected_fixture("alpha");
    let selected_before = f.store.deployment_records(&f.id).unwrap();
    let plan = f
        .store
        .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
        .unwrap();
    claim(&f);
    let report = f
        .store
        .apply_deployment_preparation(plan.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    assert_eq!(report.preparation_id, plan.preparation_id);
    assert_eq!(report.manifest.record_id, record);
    assert!(report.integrity_verified && report.published && report.writes_performed);
    assert!(
        !report.deployment_ready
            && !report.active_deployment_changed
            && !report.selection_changed
            && !report.process_started
    );
    let root = plan.directory.clone();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(root.join("manifest.json")).unwrap())
            .unwrap(),
        serde_json::to_value(&report.manifest).unwrap()
    );
    assert!(root.join("configuration/manifest.json").is_file());
    assert!(root.join("content/manifest.json").is_file());
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), selected_before);
    claim(&f);
    let reused = f
        .store
        .apply_deployment_preparation(plan, &ContentStageControl::default())
        .unwrap();
    release(&f);
    assert!(reused.integrity_verified && reused.reused_existing);
    assert!(!reused.published);
}

#[test]
fn preparation_verification_is_self_contained_and_rejects_corruption() {
    if run_isolated(
        "preparation::preparation_verification_is_self_contained_and_rejects_corruption",
    ) {
        return;
    }
    let (f, _) = selected_fixture("alpha");
    let plan = f
        .store
        .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
        .unwrap();
    claim(&f);
    f.store
        .apply_deployment_preparation(plan.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    let record = f
        .store
        .verify_deployment_record(
            &f.id,
            &plan.manifest.record_id,
            &ContentStageControl::default(),
        )
        .unwrap();
    fs::remove_dir_all(
        f.store
            .record_root(&f.id)
            .join("records")
            .join(&record.record_id),
    )
    .unwrap();
    let inputs = f
        .store
        .context()
        .space_dir()
        .join("server/instances")
        .join(f.id.as_str());
    for relative in ["configuration/revisions", "content-stages"] {
        let path = inputs.join(relative);
        fs::rename(&path, path.with_extension("offline-backup")).unwrap();
    }
    let verified = f
        .store
        .verify_deployment_preparation(&f.id, &plan.preparation_id, &ContentStageControl::default())
        .unwrap();
    assert!(verified.integrity_verified);
    assert!(
        !verified.deployment_ready
            && !verified.active_deployment_changed
            && !verified.process_started
    );
    fs::write(
        plan.directory
            .join("content/addons/@mod_01/keys/server.bikey"),
        b"changed",
    )
    .unwrap();
    assert_eq!(
        f.store
            .verify_deployment_preparation(
                &f.id,
                &plan.preparation_id,
                &ContentStageControl::default()
            )
            .unwrap_err(),
        ServerStoreError::Corrupt
    );
    let manifest_path = plan.directory.join("manifest.json");
    let original_manifest = fs::read(&manifest_path).unwrap();
    let mut future: serde_json::Value = serde_json::from_slice(&original_manifest).unwrap();
    future["layout_version"] = 99.into();
    fs::write(&manifest_path, serde_json::to_vec(&future).unwrap()).unwrap();
    assert_eq!(
        f.store
            .verify_deployment_preparation(
                &f.id,
                &plan.preparation_id,
                &ContentStageControl::default()
            )
            .unwrap_err(),
        ServerStoreError::UnsupportedVersion
    );
    assert_eq!(
        fs::read(&manifest_path).unwrap(),
        serde_json::to_vec(&future).unwrap()
    );
    fs::write(&manifest_path, original_manifest).unwrap();
}

#[test]
fn preparation_list_and_cancel_preserve_completed_generations() {
    if run_isolated("preparation::preparation_list_and_cancel_preserve_completed_generations") {
        return;
    }
    let (f, _) = selected_fixture("alpha");
    let first_plan = f
        .store
        .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
        .unwrap();
    claim(&f);
    let first_report = f
        .store
        .apply_deployment_preparation(first_plan.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    let old_generation = inventory(&first_report.directory);
    add_second_record(&f);
    let plan = f
        .store
        .plan_deployment_preparation(&f.id, 2, &ContentStageControl::default())
        .unwrap();
    let listed = f.store.deployment_preparations(&f.id).unwrap();
    assert_eq!(
        listed.preparation_ids,
        vec![first_report.preparation_id.clone()]
    );
    assert!(!listed.integrity_verified && !listed.writes_performed);
    let cancelled = ContentStageControl::default();
    cancelled.cancel_after_copy_bytes_for_test(1);
    claim(&f);
    assert_eq!(
        f.store
            .apply_deployment_preparation(plan.clone(), &cancelled)
            .unwrap_err(),
        ServerStoreError::PreparationCancelled
    );
    release(&f);
    assert!(!plan.directory.exists());
    assert_eq!(inventory(&first_report.directory), old_generation);
    let parent = plan.directory.parent().unwrap();
    assert!(fs::read_dir(parent).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".preparation-build-")
    }));
    claim(&f);
    let report = f
        .store
        .apply_deployment_preparation(plan, &ContentStageControl::default())
        .unwrap();
    release(&f);
    assert_eq!(inventory(&first_report.directory), old_generation);
    let listed = f.store.deployment_preparations(&f.id).unwrap();
    assert!(
        listed
            .preparation_ids
            .contains(&first_report.preparation_id)
    );
    assert!(listed.preparation_ids.contains(&report.preparation_id));
}
