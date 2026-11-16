use super::resolution::{find_file_named, find_first_file};
use super::*;
use crate::core::server::activation::{
    ActivationAdmission, ActivationError, ActivationRecoveryAdmission, ActivationRequest,
    ResolutionBoundary, TransactionBoundary,
};
use crate::core::server::consumers::InstallationConsumerInspection;
use crate::core::server::{InstallationRecoveryInspection, ServerInstallation};
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::maintenance::{MaintenanceIntent, MaintenanceReceipt, ResolutionOutcome};
use foxy_supervisor::session::SessionIdentity;

fn identity(f: &Fixture, operation_id: &str) -> SessionIdentity {
    SessionIdentity {
        owner_id: "owner_a".into(),
        owner_generation: 1,
        game_space_id: f.store.context().space_id().into(),
        instance_id: f.id.as_str().into(),
        operation_id: operation_id.into(),
        run_generation: 1,
    }
}

fn installation_and_locks(f: &Fixture) -> (ServerInstallation, InstallationLocks) {
    let directory = f._root.path().join("synthetic-installation");
    fs::create_dir_all(&directory).unwrap();
    let locks =
        InstallationLocks::capture_in(&directory, &f._root.path().join("lease-registry")).unwrap();
    let installation: ServerInstallation = serde_json::from_value(serde_json::json!({
        "id": crate::core::server::InstallationId::new(),
        "game": f.store.context().game_id(),
        "platform": if cfg!(windows) { "windows_x64" } else { "linux_x64" },
        "directory": directory,
        "executable": "fixture-server",
        "evidence": { "bytes": 64, "blake3": "0000000000000000000000000000000000000000000000000000000000000000" },
        "ownership": "adopted",
        "build_evidence": "executable_fingerprint_only"
    }))
    .unwrap();
    (installation, locks)
}

fn file_snapshot(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
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
    let mut files = std::collections::BTreeMap::new();
    if root.exists() {
        visit(root, root, &mut files);
    }
    files
}

fn consumers(installation: &ServerInstallation) -> InstallationConsumerInspection {
    InstallationConsumerInspection {
        installation_id: installation.id.clone(),
        inspection_complete: true,
        recorded_consumers: vec![],
        cleanup_required: Some(false),
        inspection_error: None,
        liveness_verified: false,
        exclusive_maintenance_authorized: false,
        writes_performed: false,
    }
}

fn inspect(
    f: &Fixture,
    installation: &ServerInstallation,
    locks: Option<&InstallationLocks>,
) -> InstallationRecoveryInspection {
    f.store
        .inspect_installation_recovery(installation, consumers(installation), locks)
}

fn activation_request(
    f: &Fixture,
    preparation_id: String,
    locks: &InstallationLocks,
    operation_id: &str,
) -> ActivationRequest {
    ActivationRequest {
        runtime: None,
        instance_id: f.id.clone(),
        preparation_id,
        expected_selection_generation: 1,
        expected_activation_generation: 0,
        owner: identity(f, operation_id),
        installation_key: locks.coordination_key().unwrap(),
    }
}

fn start_activation(
    f: &Fixture,
    locks: &InstallationLocks,
    boundary: TransactionBoundary,
) -> Result<(), ActivationError> {
    let stage = prepare_inputs(f);
    let committed = commit(f, record_plan(f, &stage, Some(0)));
    let preparation = f
        .store
        .plan_deployment_preparation(
            &f.id,
            committed.state.generation,
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
    let admission = ActivationAdmission::fixture(&f.store, locks, identity(f, "op_1")).unwrap();
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
    drop(admission);
    release(f);
    result.map(|_| ())
}

fn resolve_after_receipt(f: &Fixture, locks: &InstallationLocks) {
    let plan = f
        .store
        .plan_activation_resolution(
            &f.id,
            locks,
            identity(f, "resolver_a"),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(f);
    let admission = ActivationRecoveryAdmission::fixture(
        &f.store,
        locks,
        plan.resolver.clone(),
        &plan.record.intent,
    )
    .unwrap();
    let result = f.store.test_activation_resolution(
        plan,
        locks,
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
    release(f);
}

fn assert_never_authorizes(report: &InstallationRecoveryInspection) {
    assert!(!report.activation_available);
    assert!(!report.resolution_available);
    assert!(!report.liveness_verified);
    assert!(!report.installation_leases_acquired);
    assert!(!report.archives_verified);
    assert!(!report.writes_performed);
    assert!(!report.consumers.liveness_verified);
    assert!(!report.consumers.exclusive_maintenance_authorized);
    assert!(!report.consumers.writes_performed);
}

#[test]
fn clear_inspection_for_both_games_is_read_only_and_does_not_create_namespace() {
    if run_isolated(
        "recovery_inspection::clear_inspection_for_both_games_is_read_only_and_does_not_create_namespace",
    ) {
        return;
    }
    for space in ["alpha", "beta"] {
        let f = fixture(space);
        let (installation, locks) = installation_and_locks(&f);
        let registry = f._root.path().join("lease-registry");
        let before = file_snapshot(&registry);
        let report = inspect(&f, &installation, Some(&locks));
        assert!(report.inspection_complete);
        assert_eq!(report.maintenance_status, "clear");
        assert_eq!(report.receipt_count, Some(0));
        assert_eq!(report.evidence.len(), 0);
        assert_never_authorizes(&report);
        assert_eq!(file_snapshot(&registry), before);
    }
}

#[test]
fn malformed_pending_metadata_is_unknown_and_retained_consumers_are_not_live_verified() {
    if run_isolated(
        "recovery_inspection::malformed_pending_metadata_is_unknown_and_retained_consumers_are_not_live_verified",
    ) {
        return;
    }
    for malformed in ["invalid", "future", "unknown"] {
        let f = fixture("alpha");
        let (installation, locks) = installation_and_locks(&f);
        assert_eq!(
            start_activation(&f, &locks, TransactionBoundary::AfterIntent).unwrap_err(),
            ActivationError::Interrupted
        );
        let intent_path =
            find_file_named(&f._root.path().join("lease-registry"), "intent.json").unwrap();
        let bytes = match malformed {
            "invalid" => b"{".to_vec(),
            "future" | "unknown" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&intent_path).unwrap()).unwrap();
                if malformed == "future" {
                    value["schema_version"] = 99.into();
                } else {
                    value["unrecognized"] = true.into();
                }
                serde_json::to_vec(&value).unwrap()
            }
            _ => unreachable!(),
        };
        fs::write(&intent_path, &bytes).unwrap();
        let report = inspect(&f, &installation, Some(&locks));
        assert!(!report.inspection_complete);
        assert_eq!(report.maintenance_status, "unknown");
        assert_eq!(
            report.inspection_error,
            Some("maintenance_metadata_unverified")
        );
        assert_eq!(fs::read(&intent_path).unwrap(), bytes);
        assert_never_authorizes(&report);
    }

    let f = fixture("alpha");
    let (installation, locks) = installation_and_locks(&f);
    let record = foxy_supervisor::recovery::ConsumerRecord {
        cleanup: None,
        schema_version: 1,
        installation_key: locks.coordination_key().unwrap(),
        session: identity(&f, "live_owner"),
    };
    let bytes = serde_json::to_vec(&record).unwrap();
    let directory = f
        ._root
        .path()
        .join("lease-registry")
        .join(locks.coordination_key().unwrap())
        .join("consumers");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(format!("{}.json", blake3::hash(&bytes).to_hex())),
        &bytes,
    )
    .unwrap();
    let consumer = InstallationConsumerInspection {
        installation_id: installation.id.clone(),
        inspection_complete: true,
        recorded_consumers: vec![record],
        cleanup_required: Some(true),
        inspection_error: None,
        liveness_verified: false,
        exclusive_maintenance_authorized: false,
        writes_performed: false,
    };
    let report = f
        .store
        .inspect_installation_recovery(&installation, consumer, Some(&locks));
    assert!(report.inspection_complete);
    assert_eq!(report.consumers.recorded_consumers.len(), 1);
    assert_eq!(report.consumers.cleanup_required, Some(true));
    assert_never_authorizes(&report);
}

#[test]
fn corrupt_or_future_local_journal_and_resolution_are_per_evidence_errors() {
    if run_isolated(
        "recovery_inspection::corrupt_or_future_local_journal_and_resolution_are_per_evidence_errors",
    ) {
        return;
    }
    for corruption in ["journal_invalid", "journal_future", "journal_unknown"] {
        let f = fixture("alpha");
        let (installation, locks) = installation_and_locks(&f);
        assert_eq!(
            start_activation(&f, &locks, TransactionBoundary::AfterJournal).unwrap_err(),
            ActivationError::Interrupted
        );
        let path = records_root(&f).join("activation/journal.json");
        let bytes = match corruption {
            "journal_invalid" => b"{".to_vec(),
            "journal_future" | "journal_unknown" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if corruption == "journal_future" {
                    value["schema_version"] = 99.into();
                } else {
                    value["unrecognized"] = true.into();
                }
                serde_json::to_vec(&value).unwrap()
            }
            _ => unreachable!(),
        };
        fs::write(&path, &bytes).unwrap();
        let report = inspect(&f, &installation, Some(&locks));
        assert_eq!(report.maintenance_status, "pending");
        assert!(!report.inspection_complete);
        let evidence = report.evidence.first().unwrap();
        assert_eq!(evidence.inspection_error, Some("journal_unreadable"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_never_authorizes(&report);
    }

    for corruption in ["resolution_future", "resolution_unknown"] {
        let f = fixture("alpha");
        let (installation, locks) = installation_and_locks(&f);
        assert_eq!(
            start_activation(&f, &locks, TransactionBoundary::AfterPointer).unwrap_err(),
            ActivationError::Interrupted
        );
        resolve_after_receipt(&f, &locks);
        let resolution_dir = records_root(&f).join("activation/resolutions");
        let path = fs::read_dir(&resolution_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if corruption == "resolution_future" {
            value["schema_version"] = 99.into();
        } else {
            value["unrecognized"] = true.into();
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(&path, &bytes).unwrap();
        let report = inspect(&f, &installation, Some(&locks));
        assert_eq!(report.maintenance_status, "pending");
        assert!(!report.inspection_complete);
        let evidence = report.evidence.first().unwrap();
        assert_eq!(evidence.inspection_error, Some("resolution_unreadable"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_never_authorizes(&report);
    }
}

#[test]
fn resolution_history_is_bounded_to_sixteen_and_inspection_never_hashes_prepared_archives() {
    if run_isolated(
        "recovery_inspection::resolution_history_is_bounded_to_sixteen_and_inspection_never_hashes_prepared_archives",
    ) {
        return;
    }
    let f = fixture("alpha");
    let (installation, locks) = installation_and_locks(&f);
    for index in 0..18 {
        let admission = locks.try_admit().unwrap();
        let maintenance = match admission.try_maintenance() {
            Ok(maintenance) => maintenance,
            Err((_, error)) => panic!("could not create fixture maintenance lease: {error}"),
        };
        let evidence = format!("evidence-{index}");
        let intent = MaintenanceIntent {
            schema_version: 1,
            installation_key: locks.coordination_key().unwrap(),
            context_fingerprint: "a".repeat(64),
            transaction_fingerprint: blake3::hash(evidence.as_bytes()).to_hex().to_string(),
            transaction_evidence: evidence,
            previous_state_fingerprint: None,
            next_state_fingerprint: "b".repeat(64),
            owner: identity(&f, &format!("op_{index}")),
        };
        let snapshot = maintenance.begin_intent(intent).unwrap();
        drop(maintenance);
        let recovery = locks.try_recover_maintenance(&snapshot).unwrap();
        let receipt = MaintenanceReceipt {
            schema_version: 1,
            intent: snapshot.clone(),
            outcome: ResolutionOutcome::Aborted,
            evidence_fingerprint: "c".repeat(64),
            resolver: identity(&f, &format!("resolver_{index}")),
        };
        recovery.publish_receipt(&receipt).unwrap();
        recovery.retire_intent(&receipt).unwrap();
    }
    let report = inspect(&f, &installation, Some(&locks));
    assert!(report.inspection_complete);
    assert_eq!(report.maintenance_status, "clear");
    assert_eq!(report.receipt_count, Some(18));
    assert!(report.evidence_truncated);
    assert_eq!(report.evidence.len(), 16);
    assert!(report.evidence.iter().all(|evidence| !evidence.pending));
    assert_never_authorizes(&report);

    let stage = prepare_inputs(&f);
    let record = commit(&f, record_plan(&f, &stage, Some(0)));
    let preparation = f
        .store
        .plan_deployment_preparation(
            &f.id,
            record.state.generation,
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(&f);
    f.store
        .apply_deployment_preparation(preparation.clone(), &ContentStageControl::default())
        .unwrap();
    release(&f);
    let archive = find_first_file(
        &records_root(&f)
            .join("prepared")
            .join(&preparation.preparation_id)
            .join("content"),
    )
    .unwrap();
    let original = fs::read(&archive).unwrap();
    let report_before = inspect(&f, &installation, Some(&locks));
    assert!(!report_before.archives_verified);
    let evidence_before = serde_json::to_value(&report_before.evidence).unwrap();
    let mut changed = original.clone();
    changed.push(b'x');
    fs::write(&archive, &changed).unwrap();
    let report_after = inspect(&f, &installation, Some(&locks));
    assert!(!report_after.archives_verified);
    assert_eq!(
        serde_json::to_value(&report_after.evidence).unwrap(),
        evidence_before
    );
    assert_eq!(fs::read(&archive).unwrap(), changed);
}

#[test]
fn pending_inspection_reports_each_transaction_boundary_without_repair() {
    if run_isolated(
        "recovery_inspection::pending_inspection_reports_each_transaction_boundary_without_repair",
    ) {
        return;
    }
    for (boundary, journal, endpoint) in [
        (TransactionBoundary::AfterIntent, "absent", "predecessor"),
        (TransactionBoundary::AfterJournal, "matches", "predecessor"),
        (TransactionBoundary::AfterPointer, "matches", "candidate"),
    ] {
        let f = fixture("alpha");
        let (installation, locks) = installation_and_locks(&f);
        assert_eq!(
            start_activation(&f, &locks, boundary).unwrap_err(),
            ActivationError::Interrupted
        );
        let maintenance_before = locks.inspect_maintenance().unwrap();
        let report = inspect(&f, &installation, Some(&locks));
        assert!(report.inspection_complete);
        assert_eq!(report.maintenance_status, "pending");
        assert_eq!(report.receipt_count, Some(0));
        let evidence = report.evidence.first().unwrap();
        assert!(evidence.pending);
        assert_eq!(evidence.context_status, "local_context");
        assert_eq!(evidence.journal_status, journal);
        assert_eq!(evidence.active_endpoint, endpoint);
        assert_eq!(evidence.catalog_unchanged, Some(true));
        assert_eq!(evidence.selection_unchanged, Some(true));
        assert_never_authorizes(&report);
        assert_eq!(locks.inspect_maintenance().unwrap(), maintenance_before);
    }
}

#[test]
fn receipt_and_resolution_evidence_matches_before_and_after_intent_retirement() {
    if run_isolated(
        "recovery_inspection::receipt_and_resolution_evidence_matches_before_and_after_intent_retirement",
    ) {
        return;
    }
    let f = fixture("alpha");
    let (installation, locks) = installation_and_locks(&f);
    assert_eq!(
        start_activation(&f, &locks, TransactionBoundary::AfterPointer).unwrap_err(),
        ActivationError::Interrupted
    );
    resolve_after_receipt(&f, &locks);
    let pending = inspect(&f, &installation, Some(&locks));
    assert!(pending.inspection_complete);
    assert_eq!(pending.maintenance_status, "pending");
    let evidence = pending.evidence.first().unwrap();
    assert_eq!(evidence.journal_status, "matches");
    assert_eq!(evidence.active_endpoint, "candidate");
    assert_eq!(evidence.resolution_status, "matches");
    assert_eq!(evidence.receipt_status, "matches");
    assert_eq!(evidence.catalog_unchanged, Some(true));
    assert_eq!(evidence.selection_unchanged, Some(true));
    assert_never_authorizes(&pending);

    let retry = f
        .store
        .plan_activation_resolution(
            &f.id,
            &locks,
            identity(&f, "resolver_b"),
            &ContentStageControl::default(),
        )
        .unwrap();
    claim(&f);
    let admission = ActivationRecoveryAdmission::fixture(
        &f.store,
        &locks,
        retry.resolver.clone(),
        &retry.record.intent,
    )
    .unwrap();
    f.store
        .apply_activation_resolution(retry, &locks, &admission, &ContentStageControl::default())
        .unwrap();
    drop(admission);
    release(&f);

    let resolved = inspect(&f, &installation, Some(&locks));
    assert!(resolved.inspection_complete);
    assert_eq!(resolved.maintenance_status, "clear");
    assert_eq!(resolved.receipt_count, Some(1));
    assert_eq!(resolved.evidence.len(), 1);
    let evidence = &resolved.evidence[0];
    assert!(!evidence.pending);
    assert_eq!(evidence.journal_status, "absent");
    assert_eq!(evidence.active_endpoint, "candidate");
    assert_eq!(evidence.resolution_status, "matches");
    assert_eq!(evidence.receipt_status, "matches");
    assert_eq!(evidence.catalog_unchanged, None);
    assert_eq!(evidence.selection_unchanged, None);
    assert_never_authorizes(&resolved);
}

#[test]
fn missing_or_unverified_namespace_is_unknown_and_foreign_context_skips_local_state() {
    if run_isolated(
        "recovery_inspection::missing_or_unverified_namespace_is_unknown_and_foreign_context_skips_local_state",
    ) {
        return;
    }
    let f = fixture("alpha");
    let (installation, locks) = installation_and_locks(&f);
    let missing = inspect(&f, &installation, None);
    assert!(!missing.inspection_complete);
    assert_eq!(missing.maintenance_status, "unknown");
    assert_eq!(
        missing.inspection_error,
        Some("installation_identity_unverified")
    );
    assert_never_authorizes(&missing);

    assert_eq!(
        start_activation(&f, &locks, TransactionBoundary::AfterIntent).unwrap_err(),
        ActivationError::Interrupted
    );
    let foreign = fixture("beta");
    let report = inspect(&foreign, &installation, Some(&locks));
    assert!(report.inspection_complete);
    assert_eq!(report.maintenance_status, "pending");
    let evidence = report.evidence.first().unwrap();
    assert_eq!(evidence.context_status, "foreign_context");
    assert_eq!(evidence.journal_status, "not_inspected");
    assert_eq!(evidence.active_endpoint, "not_inspected");
    assert_eq!(evidence.resolution_status, "not_inspected");
    assert_eq!(evidence.catalog_unchanged, None);
    assert_eq!(evidence.selection_unchanged, None);
    assert_never_authorizes(&report);
}
