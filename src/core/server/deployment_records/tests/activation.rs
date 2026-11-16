use super::*;
use crate::core::server::activation::{
    ActivationAdmission, ActivationError, ActivationRequest, ActivationState, RecoveryDisposition,
    TransactionBoundary,
};
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::session::SessionIdentity;

fn owner(f: &Fixture) -> SessionIdentity {
    SessionIdentity {
        owner_id: "owner_a".into(),
        owner_generation: 1,
        game_space_id: f.store.context().space_id().into(),
        instance_id: f.id.as_str().into(),
        operation_id: "op_1".into(),
        run_generation: 1,
    }
}

fn locks(f: &Fixture) -> InstallationLocks {
    let installation = f._root.path().join("synthetic-installation");
    fs::create_dir_all(&installation).unwrap();
    InstallationLocks::capture_in(&installation, &f._root.path().join("lease-registry")).unwrap()
}

fn select_and_prepare(f: &Fixture) -> String {
    let stage = prepare_inputs(f);
    commit(f, record_plan(f, &stage, Some(0)));
    let preparation = f
        .store
        .plan_deployment_preparation(&f.id, 1, &ContentStageControl::default())
        .unwrap();
    claim(f);
    f.store
        .apply_deployment_preparation(preparation.clone(), &ContentStageControl::default())
        .unwrap();
    release(f);
    preparation.preparation_id
}

fn activation_request(
    f: &Fixture,
    preparation_id: String,
    selection_generation: u64,
    activation_generation: u64,
    locks: &InstallationLocks,
) -> ActivationRequest {
    ActivationRequest {
        runtime: None,
        instance_id: f.id.clone(),
        preparation_id,
        expected_selection_generation: selection_generation,
        expected_activation_generation: activation_generation,
        owner: owner(f),
        installation_key: locks.coordination_key().unwrap(),
    }
}

fn seed_predecessor(f: &Fixture, installation: &InstallationLocks) -> ActivationState {
    let preparation_id = select_and_prepare(f);
    let plan = f
        .store
        .plan_activation(
            activation_request(f, preparation_id, 1, 0, installation),
            &ContentStageControl::default(),
        )
        .unwrap();
    let previous = plan.journal.next;
    let root = records_root(f).join("activation");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("active.json"),
        serde_json::to_vec(&previous).unwrap(),
    )
    .unwrap();
    previous
}

fn replacement_plan(
    f: &Fixture,
    installation: &InstallationLocks,
) -> (crate::core::server::activation::ActivationPlan, String) {
    let record = super::selection::add_second_record(f);
    let preparation = f
        .store
        .plan_deployment_preparation(&f.id, 2, &ContentStageControl::default())
        .unwrap();
    claim(f);
    f.store
        .apply_deployment_preparation(preparation.clone(), &ContentStageControl::default())
        .unwrap();
    release(f);
    let plan = f
        .store
        .plan_activation(
            activation_request(f, preparation.preparation_id, 2, 1, installation),
            &ContentStageControl::default(),
        )
        .unwrap();
    (plan, record)
}

fn deployment_bytes(f: &Fixture) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let root = records_root(f);
    let mut files = std::collections::BTreeMap::new();
    visit(&root, &root, &mut files);
    files.retain(|path, _| {
        path.components()
            .next()
            .is_some_and(|c| c.as_os_str() != "activation")
    });
    files
}

#[test]
fn activation_planning_is_read_only_for_both_games_and_admission_stays_unavailable() {
    if run_isolated(
        "activation::activation_planning_is_read_only_for_both_games_and_admission_stays_unavailable",
    ) {
        return;
    }
    for space in ["alpha", "beta"] {
        let f = fixture(space);
        let preparation = select_and_prepare(&f);
        let installation = locks(&f);
        let before = fs::read_dir(records_root(&f))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        let req = activation_request(&f, preparation, 1, 0, &installation);
        let mut stale_activation = req.clone();
        stale_activation.expected_activation_generation = 1;
        assert!(
            f.store
                .plan_activation(stale_activation, &ContentStageControl::default())
                .is_err()
        );
        let plan = f
            .store
            .plan_activation(req.clone(), &ContentStageControl::default())
            .unwrap();
        let repeated = f
            .store
            .plan_activation(req, &ContentStageControl::default())
            .unwrap();
        assert_eq!(plan.fingerprint, repeated.fingerprint);
        assert_eq!(plan.journal, repeated.journal);
        let mut stale = activation_request(
            &f,
            plan.journal.next.deployment.preparation_id.clone(),
            0,
            0,
            &installation,
        );
        assert!(
            f.store
                .plan_activation(stale.clone(), &ContentStageControl::default())
                .is_err()
        );
        stale.expected_selection_generation = 1;
        stale.owner.operation_id = "bad operation".into();
        assert_eq!(
            f.store
                .plan_activation(stale, &ContentStageControl::default())
                .unwrap_err(),
            ActivationError::OwnerMismatch
        );
        assert!(matches!(
            f.store.admit_activation(&plan, &installation),
            Err(ActivationError::Unavailable)
        ));
        assert_eq!(
            fs::read_dir(records_root(&f))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            before
        );
        assert!(!records_root(&f).join("activation").exists());
    }
}

#[test]
fn activation_transaction_commits_first_pointer_and_keeps_journal() {
    if run_isolated("activation::activation_transaction_commits_first_pointer_and_keeps_journal") {
        return;
    }
    let f = fixture("alpha");
    let installation = locks(&f);
    let preparation = select_and_prepare(&f);
    let plan = f
        .store
        .plan_activation(
            activation_request(&f, preparation, 1, 0, &installation),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
    let first = f
        .store
        .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);
    assert_eq!(first.disposition, RecoveryDisposition::CandidateCommitted);
    assert_eq!(first.active.as_ref().unwrap().generation, 1);
    assert!(first.writes_performed && first.journal_retained);
    assert!(!first.protection_release_authorized);

    assert!(records_root(&f).join("activation/journal.json").is_file());
    let inspected = f
        .store
        .inspect_activation_recovery(&f.id, &ContentStageControl::default())
        .unwrap();
    assert_eq!(
        inspected.disposition,
        RecoveryDisposition::CandidateCommitted
    );
    assert!(inspected.journal_retained && !inspected.writes_performed);
    assert!(!inspected.protection_release_authorized);
    assert_eq!(inspected.journal, first.journal);
    assert_eq!(inspected.selection_unchanged, Some(true));
    assert_eq!(inspected.catalog_unchanged, Some(true));

    let journal = inspected.journal.unwrap();
    assert_eq!(
        f.store
            .plan_activation(
                activation_request(
                    &f,
                    journal.next.deployment.preparation_id,
                    1,
                    1,
                    &installation,
                ),
                &ContentStageControl::default(),
            )
            .unwrap_err(),
        ActivationError::RecoveryRequired
    );
    let catalog_path = f.store.path().to_path_buf();
    let mut catalog = fs::read(&catalog_path).unwrap();
    catalog.push(b' ');
    fs::write(&catalog_path, &catalog).unwrap();
    let drifted = f
        .store
        .inspect_activation_recovery(&f.id, &ContentStageControl::default())
        .unwrap();
    assert_eq!(drifted.disposition, RecoveryDisposition::CandidateCommitted);
    assert_eq!(drifted.catalog_unchanged, Some(false));
    assert_eq!(drifted.selection_unchanged, Some(true));
    assert!(!drifted.writes_performed && !drifted.protection_release_authorized);
    assert_eq!(fs::read(catalog_path).unwrap(), catalog);
}

#[test]
fn replacement_activation_commits_over_a_seeded_predecessor() {
    if run_isolated("activation::replacement_activation_commits_over_a_seeded_predecessor") {
        return;
    }
    let f = fixture("alpha");
    let installation = locks(&f);
    let predecessor = seed_predecessor(&f, &installation);
    let (plan, next_record) = replacement_plan(&f, &installation);
    assert_eq!(plan.journal.previous.as_ref(), Some(&predecessor));
    assert_eq!(plan.journal.next.deployment.record_id, next_record);
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
    let committed = f
        .store
        .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);
    assert_eq!(
        committed.disposition,
        RecoveryDisposition::CandidateCommitted
    );
    assert_eq!(committed.active.unwrap().generation, 2);
    assert!(records_root(&f).join("activation/journal.json").is_file());
}

#[test]
fn activation_rejects_tampered_plans_permits_and_changed_inputs_without_writes() {
    if run_isolated(
        "activation::activation_rejects_tampered_plans_permits_and_changed_inputs_without_writes",
    ) {
        return;
    }

    let f = fixture("alpha");
    let installation = locks(&f);
    let unclaimed = ActivationAdmission::fixture(&f.store, &installation, owner(&f));
    assert!(matches!(
        unclaimed,
        Err(ActivationError::Store(ServerStoreError::Ownership))
    ));

    for mutation in ["fingerprint", "journal", "destination"] {
        let f = fixture("alpha");
        let installation = locks(&f);
        seed_predecessor(&f, &installation);
        let (mut plan, _) = replacement_plan(&f, &installation);
        match mutation {
            "fingerprint" => plan.fingerprint.push('x'),
            "journal" => plan.journal.owner.operation_id.push('x'),
            "destination" => plan.journal.destinations.candidate = "../operator".into(),
            _ => unreachable!(),
        }
        claim(&f);
        let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
        assert_eq!(
            f.store
                .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
                .unwrap_err(),
            ActivationError::Store(ServerStoreError::StaleRevision)
        );
        drop(admission);
        release(&f);
        assert!(!records_root(&f).join("activation/journal.json").exists());
    }

    for changed in ["selection", "pointer", "catalog"] {
        let f = fixture("alpha");
        let installation = locks(&f);
        seed_predecessor(&f, &installation);
        let (plan, _) = replacement_plan(&f, &installation);
        let changed_path = match changed {
            "selection" => records_root(&f).join("state.json"),
            "pointer" => records_root(&f).join("activation/active.json"),
            "catalog" => f.store.path().to_path_buf(),
            _ => unreachable!(),
        };
        let mut bytes = fs::read(&changed_path).unwrap();
        bytes.push(b' ');
        fs::write(&changed_path, &bytes).unwrap();
        claim(&f);
        let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
        assert_eq!(
            f.store
                .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
                .unwrap_err(),
            ActivationError::Store(ServerStoreError::StaleRevision)
        );
        drop(admission);
        release(&f);
        assert_eq!(fs::read(changed_path).unwrap(), bytes);
        assert!(!records_root(&f).join("activation/journal.json").exists());
    }

    {
        let f = fixture("alpha");
        let installation = locks(&f);
        seed_predecessor(&f, &installation);
        let (plan, _) = replacement_plan(&f, &installation);
        claim(&f);
        let wrong_owner = SessionIdentity {
            operation_id: "other_op".into(),
            ..owner(&f)
        };
        let admission = ActivationAdmission::fixture(&f.store, &installation, wrong_owner).unwrap();
        assert_eq!(
            f.store
                .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
                .unwrap_err(),
            ActivationError::OwnerMismatch
        );
        drop(admission);
        release(&f);
    }
    {
        let f = fixture("alpha");
        let installation = locks(&f);
        seed_predecessor(&f, &installation);
        let (plan, _) = replacement_plan(&f, &installation);
        let path = f._root.path().join("different-installation");
        fs::create_dir_all(&path).unwrap();
        let other =
            InstallationLocks::capture_in(&path, &f._root.path().join("other-registry")).unwrap();
        claim(&f);
        let admission = ActivationAdmission::fixture(&f.store, &other, owner(&f)).unwrap();
        assert_eq!(
            f.store
                .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
                .unwrap_err(),
            ActivationError::OwnerMismatch
        );
        drop(admission);
        release(&f);
    }
    {
        let alpha = fixture("alpha");
        let beta = fixture("beta");
        let alpha_installation = locks(&alpha);
        let beta_installation = locks(&beta);
        let beta_preparation = select_and_prepare(&beta);
        let beta_plan = beta
            .store
            .plan_activation(
                activation_request(&beta, beta_preparation, 1, 0, &beta_installation),
                &ContentStageControl::default(),
            )
            .unwrap();
        claim(&alpha);
        let foreign_permit =
            ActivationAdmission::fixture(&alpha.store, &alpha_installation, owner(&beta)).unwrap();
        assert_eq!(
            beta.store
                .apply_activation_transaction(
                    beta_plan,
                    &foreign_permit,
                    &ContentStageControl::default(),
                )
                .unwrap_err(),
            ActivationError::OwnerMismatch
        );
        drop(foreign_permit);
        release(&alpha);
        assert!(!records_root(&beta).join("activation").exists());
    }
}

#[test]
fn activation_fault_boundaries_preserve_predecessor_or_report_candidate_without_repair() {
    if run_isolated(
        "activation::activation_fault_boundaries_preserve_predecessor_or_report_candidate_without_repair",
    ) {
        return;
    }
    for (boundary, expected, journal_exists) in [
        (
            TransactionBoundary::BeforeJournal,
            RecoveryDisposition::NoJournal,
            false,
        ),
        (
            TransactionBoundary::AfterJournal,
            RecoveryDisposition::PredecessorRetained,
            true,
        ),
        (
            TransactionBoundary::BeforePointer,
            RecoveryDisposition::PredecessorRetained,
            true,
        ),
        (
            TransactionBoundary::AfterPointer,
            RecoveryDisposition::CandidateCommitted,
            true,
        ),
    ] {
        let f = fixture("alpha");
        let installation = locks(&f);
        let predecessor = seed_predecessor(&f, &installation);
        let (plan, _) = replacement_plan(&f, &installation);
        fs::write(records_root(&f).join("operator-data"), b"leave-this-alone").unwrap();
        let source_archive = f
            ._root
            .path()
            .join("source-alpha/@mod_01/mpmissions/Mission.Altis.pbo");
        let source_before = fs::read(&source_archive).unwrap();
        let original = fs::read(records_root(&f).join("activation/active.json")).unwrap();
        let prepared_before = deployment_bytes(&f);
        claim(&f);
        let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
        let result = f.store.test_activation_transaction(
            plan,
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
        let path = records_root(&f).join("activation");
        assert_eq!(path.join("journal.json").exists(), journal_exists);
        if boundary != TransactionBoundary::BeforeJournal {
            assert!(!fs::read(path.join("journal.json")).unwrap().is_empty());
        }
        let inspected = f
            .store
            .inspect_activation_recovery(&f.id, &ContentStageControl::default())
            .unwrap();
        assert_eq!(inspected.disposition, expected);
        assert_eq!(inspected.journal.is_some(), journal_exists);
        assert_eq!(
            inspected.selection_unchanged,
            journal_exists.then_some(true)
        );
        assert_eq!(inspected.catalog_unchanged, journal_exists.then_some(true));
        assert!(!inspected.writes_performed && !inspected.protection_release_authorized);
        assert_eq!(deployment_bytes(&f), prepared_before);
        assert_eq!(fs::read(source_archive).unwrap(), source_before);
        if expected == RecoveryDisposition::CandidateCommitted {
            assert_eq!(inspected.active.unwrap().generation, 2);
        } else {
            assert_eq!(inspected.active.unwrap(), predecessor);
            assert_eq!(fs::read(path.join("active.json")).unwrap(), original);
        }
    }
}

#[test]
fn activation_cancellation_respects_journal_boundary_and_completion_wins_late_cancel() {
    if run_isolated(
        "activation::activation_cancellation_respects_journal_boundary_and_completion_wins_late_cancel",
    ) {
        return;
    }
    for boundary in [
        TransactionBoundary::BeforeJournal,
        TransactionBoundary::AfterJournal,
        TransactionBoundary::BeforePointer,
        TransactionBoundary::AfterPointer,
    ] {
        let f = fixture("alpha");
        let installation = locks(&f);
        let predecessor = seed_predecessor(&f, &installation);
        let (plan, _) = replacement_plan(&f, &installation);
        let control = ContentStageControl::default();
        claim(&f);
        let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
        let result = f
            .store
            .test_activation_transaction(plan, &admission, &control, |at| {
                if at == boundary {
                    control.cancel();
                }
                Ok(())
            });
        drop(admission);
        release(&f);
        let journal = records_root(&f).join("activation/journal.json");
        let active_path = records_root(&f).join("activation/active.json");
        if boundary == TransactionBoundary::AfterPointer {
            let completed = result.unwrap();
            assert_eq!(
                completed.disposition,
                RecoveryDisposition::CandidateCommitted
            );
            assert!(completed.journal.is_some());
            assert_eq!(completed.selection_unchanged, Some(true));
            assert_eq!(completed.catalog_unchanged, Some(true));
            assert!(completed.journal_retained && completed.writes_performed);
            assert!(!completed.protection_release_authorized);
            assert!(journal.is_file());
            let recovery = f
                .store
                .inspect_activation_recovery(&f.id, &ContentStageControl::default())
                .unwrap();
            assert_eq!(
                recovery.disposition,
                RecoveryDisposition::CandidateCommitted
            );
            assert!(recovery.journal.is_some() && recovery.journal_retained);
            assert_eq!(recovery.selection_unchanged, Some(true));
            assert_eq!(recovery.catalog_unchanged, Some(true));
            assert!(!recovery.writes_performed && !recovery.protection_release_authorized);
        } else {
            assert!(matches!(
                result,
                Err(ActivationError::Store(
                    ServerStoreError::Cancelled | ServerStoreError::PreparationCancelled
                ))
            ));
            assert_eq!(active(&active_path), predecessor);
            if boundary == TransactionBoundary::BeforeJournal {
                assert!(!journal.exists());
                let recovery = f
                    .store
                    .inspect_activation_recovery(&f.id, &ContentStageControl::default())
                    .unwrap();
                assert_eq!(recovery.disposition, RecoveryDisposition::NoJournal);
                assert!(recovery.journal.is_none() && !recovery.journal_retained);
                assert!(recovery.selection_unchanged.is_none());
                assert!(recovery.catalog_unchanged.is_none());
                assert!(!recovery.writes_performed && !recovery.protection_release_authorized);
            } else {
                assert!(journal.is_file());
                let recovery = f
                    .store
                    .inspect_activation_recovery(&f.id, &ContentStageControl::default())
                    .unwrap();
                assert_eq!(
                    recovery.disposition,
                    RecoveryDisposition::PredecessorRetained
                );
                assert!(recovery.journal.is_some() && recovery.journal_retained);
                assert_eq!(recovery.selection_unchanged, Some(true));
                assert_eq!(recovery.catalog_unchanged, Some(true));
                assert!(!recovery.writes_performed && !recovery.protection_release_authorized);
            }
        }
    }
}

fn active(path: &std::path::Path) -> ActivationState {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn activation_recovery_inspection_preserves_corrupt_future_unknown_and_ambiguous_state() {
    if run_isolated(
        "activation::activation_recovery_inspection_preserves_corrupt_future_unknown_and_ambiguous_state",
    ) {
        return;
    }
    for target in [
        "journal_corrupt",
        "journal_future",
        "journal_unknown",
        "journal_destination",
        "journal_candidate",
        "journal_context",
        "state_corrupt",
        "state_future",
        "state_unknown",
        "state_context",
        "preparation_corrupt",
        "preparation_future",
        "preparation_unknown",
    ] {
        let f = fixture("alpha");
        let installation = locks(&f);
        let predecessor = seed_predecessor(&f, &installation);
        let (plan, _) = replacement_plan(&f, &installation);
        let preparation_id = plan.journal.next.deployment.preparation_id.clone();
        claim(&f);
        let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
        f.store
            .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
            .unwrap();
        drop(admission);
        release(&f);
        let activation = records_root(&f).join("activation");
        let (path, baseline) = if target.starts_with("journal") {
            let path = activation.join("journal.json");
            let baseline = fs::read(&path).unwrap();
            (path, baseline)
        } else if target.starts_with("state") {
            let path = activation.join("active.json");
            let baseline = fs::read(&path).unwrap();
            (path, baseline)
        } else {
            let path = records_root(&f)
                .join("prepared")
                .join(preparation_id)
                .join("manifest.json");
            let baseline = fs::read(&path).unwrap();
            (path, baseline)
        };
        let damaged = match target {
            "journal_corrupt" | "state_corrupt" | "preparation_corrupt" => b"{".to_vec(),
            "journal_future" | "state_future" | "preparation_future" => {
                let mut value: serde_json::Value = serde_json::from_slice(&baseline).unwrap();
                value["schema_version"] = 99.into();
                serde_json::to_vec(&value).unwrap()
            }
            "journal_destination" | "journal_candidate" => {
                let mut value: serde_json::Value = serde_json::from_slice(&baseline).unwrap();
                if target == "journal_destination" {
                    value["destinations"]["candidate"] = "../operator".into();
                } else {
                    value["next"]["deployment"]["preparation_id"] = "../operator".into();
                }
                serde_json::to_vec(&value).unwrap()
            }
            "journal_context" | "state_context" => {
                let mut value: serde_json::Value = serde_json::from_slice(&baseline).unwrap();
                value["context_fingerprint"] = "0".repeat(64).into();
                serde_json::to_vec(&value).unwrap()
            }
            _ => {
                let mut value: serde_json::Value = serde_json::from_slice(&baseline).unwrap();
                value["unknown_operator_field"] = true.into();
                serde_json::to_vec(&value).unwrap()
            }
        };
        fs::write(&path, &damaged).unwrap();
        assert!(
            f.store
                .inspect_activation_recovery(&f.id, &ContentStageControl::default())
                .is_err(),
            "{target}"
        );
        assert_eq!(fs::read(&path).unwrap(), damaged);
        assert!(
            !fs::read(records_root(&f).join("activation/journal.json"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(predecessor.generation, 1);
    }

    let f = fixture("alpha");
    let installation = locks(&f);
    seed_predecessor(&f, &installation);
    let (plan, _) = replacement_plan(&f, &installation);
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
    f.store
        .apply_activation_transaction(plan.clone(), &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);
    let journal = fs::read(records_root(&f).join("activation/journal.json")).unwrap();
    let state_path = records_root(&f).join("activation/active.json");
    let mut changed: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    changed["generation"] = 99.into();
    let changed = serde_json::to_vec(&changed).unwrap();
    fs::write(&state_path, &changed).unwrap();
    let recovery = f
        .store
        .inspect_activation_recovery(&f.id, &ContentStageControl::default())
        .unwrap();
    assert_eq!(recovery.disposition, RecoveryDisposition::Ambiguous);
    assert_eq!(recovery.journal, Some(plan.journal.clone()));
    assert_eq!(recovery.selection_unchanged, Some(true));
    assert_eq!(recovery.catalog_unchanged, Some(true));
    assert!(!recovery.writes_performed && !recovery.protection_release_authorized);
    assert_eq!(fs::read(&state_path).unwrap(), changed);
    assert_eq!(
        fs::read(records_root(&f).join("activation/journal.json")).unwrap(),
        journal
    );
}

#[cfg(windows)]
#[test]
fn activation_pointer_replace_failure_keeps_predecessor_and_journal() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    if run_isolated("activation::activation_pointer_replace_failure_keeps_predecessor_and_journal")
    {
        return;
    }
    let f = fixture("alpha");
    let installation = locks(&f);
    let predecessor = seed_predecessor(&f, &installation);
    let (plan, _) = replacement_plan(&f, &installation);
    let active_path = records_root(&f).join("activation/active.json");
    let original = fs::read(&active_path).unwrap();
    let prepared_before = deployment_bytes(&f);
    let locked = OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(&active_path)
        .unwrap();
    claim(&f);
    let admission = ActivationAdmission::fixture(&f.store, &installation, owner(&f)).unwrap();
    assert_eq!(
        f.store
            .apply_activation_transaction(plan, &admission, &ContentStageControl::default())
            .unwrap_err(),
        ActivationError::Store(ServerStoreError::Io)
    );
    drop(admission);
    release(&f);
    assert!(records_root(&f).join("activation/journal.json").is_file());
    assert_eq!(fs::read(&active_path).unwrap(), original);
    assert_eq!(active(&active_path), predecessor);
    assert_eq!(deployment_bytes(&f), prepared_before);
    let recovery = f
        .store
        .inspect_activation_recovery(&f.id, &ContentStageControl::default())
        .unwrap();
    assert_eq!(
        recovery.disposition,
        RecoveryDisposition::PredecessorRetained
    );
    assert!(recovery.journal.is_some() && recovery.journal_retained);
    assert_eq!(recovery.selection_unchanged, Some(true));
    assert_eq!(recovery.catalog_unchanged, Some(true));
    assert!(!recovery.writes_performed && !recovery.protection_release_authorized);
    drop(locked);
}
