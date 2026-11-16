use super::native::{
    copy_ordinary_tree, fresh_child, invoke_saved, required_directory, tree_digest,
};
use super::*;
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::maintenance::{MaintenanceIntent, MaintenanceReceipt, ResolutionOutcome};
use foxy_supervisor::process::CommandSpec;
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};
use std::sync::atomic::AtomicBool;

#[test]
#[ignore = "native CLI admission/consumer acceptance with a complete managed source and prepared session"]
fn native_d16_physical_and_unresolved_gates_refuse_cli_maintenance() {
    #[cfg(target_os = "linux")]
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let config = required_directory("FOXY_NATIVE_MAINTENANCE_CONFIG");
    let source = required_directory("FOXY_NATIVE_MAINTENANCE_SOURCE");
    let scratch = required_directory("FOXY_NATIVE_MAINTENANCE_SCRATCH");
    assert!(!scratch.starts_with(&source) && !source.starts_with(&scratch));
    assert!(!scratch.starts_with(&config) && !config.starts_with(&scratch));
    let run = fresh_child(&scratch);
    let original = run.join("ServerOriginal");
    let destination = run.join("ServerSuccessor");
    let destination_path = destination.to_str().unwrap();
    let artifacts = run.join("artifacts");
    fs::create_dir(&artifacts).unwrap();
    copy_ordinary_tree(&source, &original);
    let before = tree_digest(&original).0;
    let (code, added) = invoke_saved(
        &config,
        &["server", "instance", "add", "--name", "D16Gate"],
        &artifacts,
        "add",
    );
    assert_eq!(code, 0, "{added}");
    let id = added["data"]["instance"]["id"].as_str().unwrap();
    let (code, adopted) = invoke_saved(
        &config,
        &[
            "--yes",
            "server",
            "instance",
            "adopt",
            id,
            "--directory",
            original.to_str().unwrap(),
            "--expected-revision",
            "1",
        ],
        &artifacts,
        "adopt",
    );
    assert_eq!(code, 0, "{adopted}");
    let plan_args = [
        "server",
        "installation",
        "repair",
        "plan",
        id,
        "--directory",
        destination_path,
    ];
    let (code, plan) = invoke_saved(&config, &plan_args, &artifacts, "plan");
    assert_eq!(code, 0, "{plan}");
    assert!(
        plan["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let fingerprint = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    let apply_args = |fingerprint| {
        [
            "--yes",
            "server",
            "installation",
            "repair",
            "apply",
            id,
            "--directory",
            destination_path,
            "--expected-plan-fingerprint",
            fingerprint,
        ]
    };
    let locks = InstallationLocks::capture(&original).unwrap();
    let admission = locks.try_admit().unwrap();
    let runtime = admission.try_runtime().unwrap();
    let (code, result) = invoke_saved(
        &config,
        &apply_args(fingerprint),
        &artifacts,
        "admission_refused",
    );
    assert_eq!(code, 2, "{result}");
    assert!(
        errors(&result).contains("maintenance is blocked"),
        "{result}"
    );
    drop(admission);
    let (code, result) = invoke_saved(
        &config,
        &apply_args(fingerprint),
        &artifacts,
        "runtime_refused",
    );
    assert_eq!(code, 2, "{result}");
    assert!(
        errors(&result).contains("maintenance is blocked"),
        "{result}"
    );
    drop(runtime);
    assert!(!destination.exists());
    let owner = SessionIdentity {
        owner_id: "d16_external_owner".into(),
        owner_generation: 1,
        game_space_id: "foreign_space".into(),
        instance_id: id.into(),
        operation_id: "gate_probe".into(),
        run_generation: 1,
    };
    let digest = |value: &str| blake3::hash(value.as_bytes()).to_hex().to_string();
    let intent = MaintenanceIntent {
        schema_version: 1,
        installation_key: locks.coordination_key().unwrap(),
        context_fingerprint: digest("D16 private gate context"),
        transaction_fingerprint: digest("D16 no content mutation"),
        transaction_evidence: "D16 no content mutation".into(),
        previous_state_fingerprint: Some(before.clone()),
        next_state_fingerprint: before.clone(),
        owner: owner.clone(),
    };
    let lease = locks
        .try_admit()
        .unwrap()
        .try_maintenance()
        .map_err(|(_, error)| error)
        .unwrap();
    let pending = lease.begin_intent(intent).unwrap();
    drop(lease);
    let (code, pending_plan) = invoke_saved(&config, &plan_args, &artifacts, "pending_plan");
    assert_eq!(code, 0, "{pending_plan}");
    assert!(
        !pending_plan["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (code, refused) = invoke_saved(
        &config,
        &apply_args(
            pending_plan["data"]["plan"]["fingerprint"]
                .as_str()
                .unwrap(),
        ),
        &artifacts,
        "pending_refused",
    );
    assert_eq!(code, 2, "{refused}");
    assert!(
        errors(&refused).contains("maintenance is blocked"),
        "{refused}"
    );
    assert_eq!(tree_digest(&original).0, before);
    let recovery = locks.try_recover_maintenance(&pending).unwrap();
    let receipt = MaintenanceReceipt {
        schema_version: 1,
        intent: pending.clone(),
        outcome: ResolutionOutcome::Aborted,
        evidence_fingerprint: before.clone(),
        resolver: owner.clone(),
    };
    recovery.publish_receipt(&receipt).unwrap();
    recovery.retire_intent(&receipt).unwrap();
    drop(recovery);
    let resolved = locks.inspect_maintenance().unwrap();
    assert!(resolved.pending.is_none());
    assert_eq!(resolved.receipts, vec![receipt]);

    let spec = CommandSpec {
        executable: run.join("intentionally_missing_native_probe"),
        directory: run.clone(),
        arguments: vec![],
        environment: vec![],
    };
    let result = ForegroundSession::spawn(
        owner,
        &spec,
        locks.try_admit().unwrap(),
        &AtomicBool::new(false),
        || Ok(()),
    );
    assert!(result.is_err());
    let consumers = locks.inspect_consumers().unwrap();
    assert_eq!(consumers.len(), 1);
    fs::write(
        artifacts.join("unresolved_consumer.json"),
        serde_json::to_vec_pretty(&consumers).unwrap(),
    )
    .unwrap();
    let (code, unresolved_plan) = invoke_saved(&config, &plan_args, &artifacts, "unresolved_plan");
    assert_eq!(code, 0, "{unresolved_plan}");
    assert!(
        !unresolved_plan["data"]["plan"]["blockers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (code, refused) = invoke_saved(
        &config,
        &apply_args(
            unresolved_plan["data"]["plan"]["fingerprint"]
                .as_str()
                .unwrap(),
        ),
        &artifacts,
        "unresolved_refused",
    );
    assert_eq!(code, 2, "{refused}");
    assert!(
        errors(&refused).contains("maintenance is blocked"),
        "{refused}"
    );
    assert!(!destination.exists());
    assert_eq!(tree_digest(&original).0, before);
    assert!(fs::read_dir(&run).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".foxy-install-")
    }));
    assert_eq!(locks.inspect_consumers().unwrap(), consumers);
}
