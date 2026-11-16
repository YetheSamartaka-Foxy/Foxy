use super::*;
use crate::core::server::activation::{
    ActivationAdmission, ActivationError, ActivationRecoveryAdmission, ResolutionBoundary,
    TransactionBoundary,
};
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::maintenance::ResolutionOutcome;
use foxy_supervisor::session::SessionIdentity;

fn resolver(f: &Fixture, operation_id: &str) -> SessionIdentity {
    SessionIdentity {
        owner_id: "owner_a".into(),
        owner_generation: 1,
        game_space_id: f.store.context().space_id().into(),
        instance_id: f.id.as_str().into(),
        operation_id: operation_id.into(),
        run_generation: 1,
    }
}

fn activation_owner(f: &Fixture) -> SessionIdentity {
    resolver(f, "op_1")
}

fn activation_request(
    f: &Fixture,
    preparation_id: String,
    locks: &InstallationLocks,
    operation_id: &str,
) -> crate::core::server::activation::ActivationRequest {
    crate::core::server::activation::ActivationRequest {
        runtime: None,
        instance_id: f.id.clone(),
        preparation_id,
        expected_selection_generation: 1,
        expected_activation_generation: 0,
        owner: resolver(f, operation_id),
        installation_key: locks.coordination_key().unwrap(),
    }
}

fn installation(f: &Fixture) -> InstallationLocks {
    let path = f._root.path().join("synthetic-installation");
    fs::create_dir_all(&path).unwrap();
    InstallationLocks::capture_in(&path, &f._root.path().join("lease-registry")).unwrap()
}

pub(super) fn find_file_named(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.file_name().is_some_and(|file_name| file_name == name) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file_named(&path, name)
        {
            return Some(found);
        }
    }
    None
}

pub(super) fn find_first_file(root: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.is_file() {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_first_file(&path)
        {
            return Some(found);
        }
    }
    None
}

fn deployment_files(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path
                .strip_prefix(root)
                .unwrap()
                .components()
                .next()
                .unwrap()
                .as_os_str()
                == "activation"
            {
                continue;
            }
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut files = std::collections::BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn begin_interrupted_activation(f: &Fixture, locks: &InstallationLocks) {
    begin_interrupted_activation_at(f, locks, TransactionBoundary::AfterIntent);
}

fn begin_interrupted_activation_at(
    f: &Fixture,
    locks: &InstallationLocks,
    stop_at: TransactionBoundary,
) {
    let stage = prepare_inputs(f);
    let selected = commit(f, record_plan(f, &stage, Some(0)));
    let preparation = f
        .store
        .plan_deployment_preparation(
            &f.id,
            selected.state.generation,
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(f);
    f.store
        .apply_deployment_preparation(preparation.clone(), &ContentStageControl::default())
        .unwrap();
    release(f);
    let plan = f
        .store
        .plan_activation(
            activation_request(f, preparation.preparation_id, locks, "op_1"),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(f);
    let admission = ActivationAdmission::fixture(&f.store, locks, activation_owner(f)).unwrap();
    let result = f.store.test_activation_transaction(
        plan,
        &admission,
        &ContentStageControl::default(),
        |boundary| {
            if boundary == stop_at {
                Err(ActivationError::Interrupted)
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(result.unwrap_err(), ActivationError::Interrupted);
    drop(admission);
    release(f);
}

fn resolution_plan(
    f: &Fixture,
    locks: &InstallationLocks,
    identity: SessionIdentity,
) -> crate::core::server::activation::ActivationResolutionPlan {
    f.store
        .plan_activation_resolution(&f.id, locks, identity, &ContentStageControl::default())
        .unwrap()
}

fn resolve(
    f: &Fixture,
    locks: &InstallationLocks,
    plan: crate::core::server::activation::ActivationResolutionPlan,
) -> crate::core::server::activation::ActivationResolutionReport {
    claim(f);
    let admission = ActivationRecoveryAdmission::fixture(
        &f.store,
        locks,
        plan.resolver.clone(),
        &plan.record.intent,
    )
    .unwrap();
    let result = f.store.apply_activation_resolution(
        plan,
        locks,
        &admission,
        &ContentStageControl::default(),
    );
    drop(admission);
    release(f);
    result.unwrap()
}

#[test]
fn intent_without_journal_resolves_as_aborted_and_blocks_ordinary_admission_until_retired() {
    if run_isolated(
        "resolution::intent_without_journal_resolves_as_aborted_and_blocks_ordinary_admission_until_retired",
    ) {
        return;
    }
    for space in ["alpha", "beta"] {
        let f = fixture(space);
        let locks = installation(&f);
        begin_interrupted_activation(&f, &locks);
        let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
        assert_eq!(plan.record.outcome, ResolutionOutcome::Aborted);
        assert!(plan.journal_fingerprint.is_none());
        assert!(
            matches!(locks.try_admit(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );

        let report = resolve(&f, &locks, plan);
        assert_eq!(report.record.outcome, ResolutionOutcome::Aborted);
        assert!(!report.journal_retired && report.intent_retired && report.writes_performed);
        assert!(!report.engine_admission_authorized);
        let inventory = locks.inspect_maintenance().unwrap();
        assert!(inventory.pending.is_none());
        assert_eq!(inventory.receipts, vec![report.receipt]);
        let ordinary = locks.try_admit().unwrap();
        drop(ordinary);
    }
}

#[test]
fn committed_activation_resolution_publishes_evidence_without_changing_selection_or_pointer() {
    if run_isolated(
        "resolution::committed_activation_resolution_publishes_evidence_without_changing_selection_or_pointer",
    ) {
        return;
    }
    let f = fixture("alpha");
    let locks = installation(&f);
    let stage = prepare_inputs(&f);
    let selected = commit(&f, record_plan(&f, &stage, Some(0)));
    let preparation = f
        .store
        .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
        .unwrap();
    claim(&f);
    f.store
        .apply_deployment_preparation(preparation.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    let plan = f
        .store
        .plan_activation(
            activation_request(&f, preparation.preparation_id, &locks, "op_1"),
            &ContentStageControl::default(),
        )
        .unwrap();
    let active_path = records_root(&f).join("activation/active.json");
    let state_path = records_root(&f).join("state.json");
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &locks, activation_owner(&f)).unwrap();
    f.store
        .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);
    let pointer = fs::read(&active_path).unwrap();
    let selection = fs::read(&state_path).unwrap();
    let resolution = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
    assert_eq!(resolution.record.outcome, ResolutionOutcome::Committed);
    let report = resolve(&f, &locks, resolution);
    assert!(report.journal_retired && report.intent_retired);
    assert!(!report.engine_admission_authorized);
    assert_eq!(fs::read(&active_path).unwrap(), pointer);
    assert_eq!(fs::read(&state_path).unwrap(), selection);
    assert_eq!(f.store.deployment_records(&f.id).unwrap(), selected.state);
}

#[test]
fn every_resolution_boundary_retains_protection_until_a_new_resolver_finishes() {
    if run_isolated(
        "resolution::every_resolution_boundary_retains_protection_until_a_new_resolver_finishes",
    ) {
        return;
    }
    for activation_stop in [
        TransactionBoundary::AfterJournal,
        TransactionBoundary::AfterPointer,
    ] {
        for boundary in [
            ResolutionBoundary::AfterRecord,
            ResolutionBoundary::AfterReceipt,
            ResolutionBoundary::AfterJournalRetire,
            ResolutionBoundary::BeforeIntentRetire,
            ResolutionBoundary::AfterIntentRetire,
        ] {
            let f = fixture("alpha");
            let locks = installation(&f);
            begin_interrupted_activation_at(&f, &locks, activation_stop);
            let first = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
            let intent = first.record.intent.clone();
            assert!(matches!(
                f.store.admit_activation_resolution(&first, &locks),
                Err(ActivationError::Unavailable)
            ));
            claim(&f);
            let admission = ActivationRecoveryAdmission::fixture(
                &f.store,
                &locks,
                first.resolver.clone(),
                &intent,
            )
            .unwrap();
            let result = f.store.test_activation_resolution(
                first,
                &locks,
                &admission,
                &ContentStageControl::default(),
                |at| {
                    if at == boundary {
                        Err(ActivationError::Interrupted)
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.unwrap_err(), ActivationError::Interrupted);
            drop(admission);
            release(&f);

            let inventory = locks.inspect_maintenance().unwrap();
            if boundary == ResolutionBoundary::AfterIntentRetire {
                assert!(inventory.pending.is_none());
                assert_eq!(inventory.receipts.len(), 1);
            } else {
                assert_eq!(inventory.pending.as_ref(), Some(&intent));
                assert!(
                    matches!(locks.try_admit(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
                );
                let journal = records_root(&f).join("activation/journal.json");
                if matches!(
                    boundary,
                    ResolutionBoundary::AfterJournalRetire | ResolutionBoundary::BeforeIntentRetire
                ) {
                    assert!(!journal.exists());
                } else {
                    assert!(journal.is_file());
                }
                let resume = resolution_plan(&f, &locks, resolver(&f, "resolver_b"));
                let report = resolve(&f, &locks, resume);
                assert_eq!(report.record.resolver.operation_id, "resolver_a");
                assert_eq!(report.receipt.resolver.operation_id, "resolver_a");
                assert!(report.intent_retired);
            }
        }
    }
}

#[test]
fn resolution_planning_is_read_only_and_rejects_tampered_plans_and_wrong_recovery_owners() {
    if run_isolated(
        "resolution::resolution_planning_is_read_only_and_rejects_tampered_plans_and_wrong_recovery_owners",
    ) {
        return;
    }
    let f = fixture("alpha");
    let locks = installation(&f);
    begin_interrupted_activation(&f, &locks);
    let root = records_root(&f).join("activation");
    let inventory_before = locks.inspect_maintenance().unwrap();
    let active_before = fs::read(root.join("active.json")).ok();
    let journal_before = fs::read(root.join("journal.json")).ok();
    let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
    let unclaimed = ActivationRecoveryAdmission::fixture(
        &f.store,
        &locks,
        plan.resolver.clone(),
        &plan.record.intent,
    );
    assert!(matches!(
        unclaimed,
        Err(ActivationError::Store(ServerStoreError::Ownership))
    ));
    assert_eq!(locks.inspect_maintenance().unwrap(), inventory_before);
    assert_eq!(fs::read(root.join("active.json")).ok(), active_before);
    assert_eq!(fs::read(root.join("journal.json")).ok(), journal_before);
    assert!(!root.join("resolutions").exists());

    let mut tampered = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
    tampered.fingerprint.push('x');
    claim(&f);
    let admission = ActivationRecoveryAdmission::fixture(
        &f.store,
        &locks,
        tampered.resolver.clone(),
        &tampered.record.intent,
    )
    .unwrap();
    assert_eq!(
        f.store
            .apply_activation_resolution(
                tampered,
                &locks,
                &admission,
                &ContentStageControl::default(),
            )
            .unwrap_err(),
        ActivationError::Store(ServerStoreError::StaleRevision)
    );
    drop(admission);
    release(&f);

    let wrong_resolver = resolver(&f, "resolver_other");
    claim(&f);
    let wrong_admission =
        ActivationRecoveryAdmission::fixture(&f.store, &locks, wrong_resolver, &plan.record.intent)
            .unwrap();
    assert_eq!(
        f.store
            .apply_activation_resolution(
                plan,
                &locks,
                &wrong_admission,
                &ContentStageControl::default(),
            )
            .unwrap_err(),
        ActivationError::OwnerMismatch
    );
    drop(wrong_admission);
    release(&f);
    assert_eq!(locks.inspect_maintenance().unwrap(), inventory_before);
    assert!(!root.join("resolutions").exists());
}

#[test]
fn changed_raw_inputs_refuse_reviewed_resolution_and_leave_the_intent_pending() {
    if run_isolated(
        "resolution::changed_raw_inputs_refuse_reviewed_resolution_and_leave_the_intent_pending",
    ) {
        return;
    }
    for input in ["catalog", "selection", "pointer"] {
        let f = fixture("alpha");
        let locks = installation(&f);
        begin_interrupted_activation_at(&f, &locks, TransactionBoundary::AfterJournal);
        let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
        let target = match input {
            "catalog" => f.store.path().to_path_buf(),
            "selection" => records_root(&f).join("state.json"),
            "pointer" => records_root(&f).join("activation/journal.json"),
            _ => unreachable!(),
        };
        claim(&f);
        let admission = ActivationRecoveryAdmission::fixture(
            &f.store,
            &locks,
            plan.resolver.clone(),
            &plan.record.intent,
        )
        .unwrap();
        let mut changed = fs::read(&target).unwrap();
        changed.push(b' ');
        fs::write(&target, &changed).unwrap();
        assert!(
            f.store
                .apply_activation_resolution(
                    plan,
                    &locks,
                    &admission,
                    &ContentStageControl::default(),
                )
                .is_err()
        );
        drop(admission);
        release(&f);
        assert_eq!(fs::read(&target).unwrap(), changed);
        assert!(locks.inspect_maintenance().unwrap().pending.is_some());
    }
}

#[test]
fn corrupt_journal_and_resolution_record_are_preserved_and_never_clear_the_intent() {
    if run_isolated(
        "resolution::corrupt_journal_and_resolution_record_are_preserved_and_never_clear_the_intent",
    ) {
        return;
    }
    for corrupt_record in [false, true] {
        let f = fixture("alpha");
        let locks = installation(&f);
        begin_interrupted_activation_at(&f, &locks, TransactionBoundary::AfterJournal);
        let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
        let activation = records_root(&f).join("activation");
        let target = if corrupt_record {
            claim(&f);
            let admission = ActivationRecoveryAdmission::fixture(
                &f.store,
                &locks,
                plan.resolver.clone(),
                &plan.record.intent,
            )
            .unwrap();
            let result = f.store.test_activation_resolution(
                plan.clone(),
                &locks,
                &admission,
                &ContentStageControl::default(),
                |boundary| {
                    if boundary == ResolutionBoundary::AfterRecord {
                        Err(ActivationError::Interrupted)
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.unwrap_err(), ActivationError::Interrupted);
            drop(admission);
            release(&f);
            activation.join("resolutions").join(format!(
                "resolution_{}.json",
                plan.record.intent.fingerprint
            ))
        } else {
            activation.join("journal.json")
        };
        let mut bytes = fs::read(&target).unwrap();
        bytes.push(b' ');
        fs::write(&target, &bytes).unwrap();
        let planned = f.store.plan_activation_resolution(
            &f.id,
            &locks,
            resolver(&f, "resolver_b"),
            &ContentStageControl::default(),
        );
        if corrupt_record {
            let plan = planned.unwrap();
            claim(&f);
            let admission = ActivationRecoveryAdmission::fixture(
                &f.store,
                &locks,
                plan.resolver.clone(),
                &plan.record.intent,
            )
            .unwrap();
            assert_eq!(
                f.store
                    .apply_activation_resolution(
                        plan,
                        &locks,
                        &admission,
                        &ContentStageControl::default(),
                    )
                    .unwrap_err(),
                ActivationError::RecoveryRequired
            );
            drop(admission);
            release(&f);
        } else {
            assert!(planned.is_err());
        }
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert!(locks.inspect_maintenance().unwrap().pending.is_some());
    }
}

#[test]
fn corrupt_intent_receipt_prepared_data_and_conflicting_pointer_are_never_repaired() {
    if run_isolated(
        "resolution::corrupt_intent_receipt_prepared_data_and_conflicting_pointer_are_never_repaired",
    ) {
        return;
    }
    for corruption in [
        "intent",
        "receipt",
        "prepared",
        "pointer_conflict",
        "journal_future",
        "journal_unknown",
    ] {
        let f = fixture("alpha");
        let locks = installation(&f);
        let activation_boundary = if corruption == "intent" {
            TransactionBoundary::AfterIntent
        } else {
            TransactionBoundary::AfterJournal
        };
        begin_interrupted_activation_at(&f, &locks, activation_boundary);
        let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
        let activation = records_root(&f).join("activation");
        let registry = f._root.path().join("lease-registry");
        if corruption == "receipt" {
            claim(&f);
            let admission = ActivationRecoveryAdmission::fixture(
                &f.store,
                &locks,
                plan.resolver.clone(),
                &plan.record.intent,
            )
            .unwrap();
            let result = f.store.test_activation_resolution(
                plan.clone(),
                &locks,
                &admission,
                &ContentStageControl::default(),
                |boundary| {
                    if boundary == ResolutionBoundary::AfterReceipt {
                        Err(ActivationError::Interrupted)
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result.unwrap_err(), ActivationError::Interrupted);
            drop(admission);
            release(&f);
        }

        let target = match corruption {
            "intent" => find_file_named(&registry, "intent.json").unwrap(),
            "receipt" => find_file_named(
                &registry,
                &format!("{}.json", plan.record.intent.fingerprint),
            )
            .unwrap(),
            "prepared" => find_first_file(
                &records_root(&f)
                    .join("prepared")
                    .join(&plan.record.journal.next.deployment.preparation_id)
                    .join("content"),
            )
            .unwrap(),
            "pointer_conflict" => {
                let mut conflict = plan.record.journal.next.clone();
                conflict.generation += 7;
                let path = activation.join("active.json");
                fs::write(&path, serde_json::to_vec(&conflict).unwrap()).unwrap();
                path
            }
            "journal_future" | "journal_unknown" => {
                let path = activation.join("journal.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if corruption == "journal_future" {
                    value["schema_version"] = 99.into();
                } else {
                    value["unrecognized"] = true.into();
                }
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
                path
            }
            _ => unreachable!(),
        };
        let mut bytes = fs::read(&target).unwrap();
        if !matches!(
            corruption,
            "pointer_conflict" | "journal_future" | "journal_unknown"
        ) {
            bytes.push(b' ');
            fs::write(&target, &bytes).unwrap();
        }
        assert!(
            f.store
                .plan_activation_resolution(
                    &f.id,
                    &locks,
                    resolver(&f, "resolver_b"),
                    &ContentStageControl::default(),
                )
                .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), bytes);
        if corruption == "intent" || corruption == "receipt" {
            assert!(locks.inspect_maintenance().is_err());
        } else {
            assert!(locks.inspect_maintenance().unwrap().pending.is_some());
        }
    }
}

#[test]
fn aborted_resolution_preserves_deployment_and_operator_data_and_allows_a_new_operation() {
    if run_isolated(
        "resolution::aborted_resolution_preserves_deployment_and_operator_data_and_allows_a_new_operation",
    ) {
        return;
    }
    let f = fixture("alpha");
    let locks = installation(&f);
    begin_interrupted_activation_at(&f, &locks, TransactionBoundary::AfterJournal);
    let deployment_root = records_root(&f);
    let operator_path = deployment_root.join("operator-data");
    fs::write(&operator_path, b"keep-operator-bytes").unwrap();
    let selection_path = deployment_root.join("state.json");
    let selection_before = fs::read(&selection_path).unwrap();
    let immutable_before = deployment_files(&deployment_root);
    let prepared_id = serde_json::from_slice::<crate::core::server::activation::ActivationJournal>(
        &fs::read(deployment_root.join("activation/journal.json")).unwrap(),
    )
    .unwrap()
    .next
    .deployment
    .preparation_id;

    let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
    let report = resolve(&f, &locks, plan);
    assert_eq!(report.record.outcome, ResolutionOutcome::Aborted);
    assert_eq!(fs::read(&selection_path).unwrap(), selection_before);
    assert_eq!(fs::read(&operator_path).unwrap(), b"keep-operator-bytes");
    assert_eq!(deployment_files(&deployment_root), immutable_before);

    let next = f
        .store
        .plan_activation(
            activation_request(&f, prepared_id, &locks, "op_2"),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &locks, resolver(&f, "op_2")).unwrap();
    f.store
        .apply_activation_transaction(next, &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);
    let pending = locks.inspect_maintenance().unwrap().pending.unwrap();
    assert_eq!(pending.intent.owner.operation_id, "op_2");
    assert_eq!(fs::read(&selection_path).unwrap(), selection_before);
    assert_eq!(fs::read(&operator_path).unwrap(), b"keep-operator-bytes");
    assert_eq!(deployment_files(&deployment_root), immutable_before);
}

#[cfg(windows)]
#[test]
fn journal_retirement_io_failure_keeps_barrier_until_a_new_resolver_retries() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    if run_isolated(
        "resolution::journal_retirement_io_failure_keeps_barrier_until_a_new_resolver_retries",
    ) {
        return;
    }
    let f = fixture("alpha");
    let locks = installation(&f);
    let stage = prepare_inputs(&f);
    commit(&f, record_plan(&f, &stage, Some(0)));
    let preparation = f
        .store
        .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
        .unwrap();
    claim(&f);
    f.store
        .apply_deployment_preparation(preparation.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    let activation = f
        .store
        .plan_activation(
            activation_request(&f, preparation.preparation_id, &locks, "op_1"),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &locks, activation_owner(&f)).unwrap();
    f.store
        .apply_activation_transaction(activation, &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);

    let activation_root = records_root(&f).join("activation");
    let pointer_before = fs::read(activation_root.join("active.json")).unwrap();
    let selection_path = records_root(&f).join("state.json");
    let selection_before = fs::read(&selection_path).unwrap();
    let immutable_before = deployment_files(&records_root(&f));
    let journal_path = activation_root.join("journal.json");
    let locked_journal = OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(&journal_path)
        .unwrap();
    let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
    claim(&f);
    let admission = ActivationRecoveryAdmission::fixture(
        &f.store,
        &locks,
        plan.resolver.clone(),
        &plan.record.intent,
    )
    .unwrap();
    assert_eq!(
        f.store
            .apply_activation_resolution(plan, &locks, &admission, &ContentStageControl::default(),)
            .unwrap_err(),
        ActivationError::Store(ServerStoreError::Io)
    );
    drop(admission);
    release(&f);
    assert!(journal_path.is_file());
    assert!(locks.inspect_maintenance().unwrap().pending.is_some());
    assert_eq!(
        fs::read(activation_root.join("active.json")).unwrap(),
        pointer_before
    );
    assert_eq!(fs::read(&selection_path).unwrap(), selection_before);
    assert_eq!(deployment_files(&records_root(&f)), immutable_before);
    assert!(matches!(
        locks.try_admit(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    drop(locked_journal);

    let retry = resolution_plan(&f, &locks, resolver(&f, "resolver_b"));
    let report = resolve(&f, &locks, retry);
    assert!(report.journal_retired && report.intent_retired);
    assert!(locks.inspect_maintenance().unwrap().pending.is_none());
    assert_eq!(
        fs::read(activation_root.join("active.json")).unwrap(),
        pointer_before
    );
    assert_eq!(fs::read(&selection_path).unwrap(), selection_before);
    assert_eq!(deployment_files(&records_root(&f)), immutable_before);
}

#[test]
fn cancellation_before_intent_retirement_preserves_barrier_and_late_cancel_completion_wins() {
    if run_isolated(
        "resolution::cancellation_before_intent_retirement_preserves_barrier_and_late_cancel_completion_wins",
    ) {
        return;
    }
    let f = fixture("alpha");
    let locks = installation(&f);
    begin_interrupted_activation(&f, &locks);
    let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_a"));
    let intent = plan.record.intent.clone();
    let control = ContentStageControl::default();
    claim(&f);
    let admission =
        ActivationRecoveryAdmission::fixture(&f.store, &locks, plan.resolver.clone(), &intent)
            .unwrap();
    let cancelled =
        f.store
            .test_activation_resolution(plan, &locks, &admission, &control, |boundary| {
                if boundary == ResolutionBoundary::BeforeIntentRetire {
                    control.cancel();
                }
                Ok(())
            });
    assert!(matches!(
        cancelled,
        Err(ActivationError::Store(
            ServerStoreError::Cancelled | ServerStoreError::PreparationCancelled
        ))
    ));
    drop(admission);
    release(&f);
    assert_eq!(
        locks.inspect_maintenance().unwrap().pending.as_ref(),
        Some(&intent)
    );
    assert!(
        records_root(&f)
            .join("activation/resolutions")
            .join(format!("resolution_{}.json", intent.fingerprint))
            .is_file()
    );

    let plan = resolution_plan(&f, &locks, resolver(&f, "resolver_b"));
    let late_cancel = ContentStageControl::default();
    claim(&f);
    let admission = ActivationRecoveryAdmission::fixture(
        &f.store,
        &locks,
        plan.resolver.clone(),
        &plan.record.intent,
    )
    .unwrap();
    let completed =
        f.store
            .test_activation_resolution(plan, &locks, &admission, &late_cancel, |boundary| {
                if boundary == ResolutionBoundary::AfterIntentRetire {
                    late_cancel.cancel();
                }
                Ok(())
            });
    assert!(completed.is_ok());
    assert!(locks.inspect_maintenance().unwrap().pending.is_none());
    drop(admission);
    release(&f);
}
