use super::*;
use crate::leases::{Admission, InstallationLocks, MaintenanceLease};
use crate::session::SessionIdentity;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    installation: PathBuf,
    registry: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let installation = base.join("game-installation");
        fs::create_dir(&installation).unwrap();
        fs::write(installation.join("package.bin"), b"adopted package bytes").unwrap();
        fs::write(installation.join("operator.cfg"), b"operator bytes").unwrap();
        let registry = base.join("private/lock-registry");
        Self {
            _root: root,
            base,
            installation,
            registry,
        }
    }

    fn locks(&self) -> InstallationLocks {
        InstallationLocks::capture_in(&self.installation, &self.registry).unwrap()
    }
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        owner_id: "owner_alpha".into(),
        owner_generation: 1,
        game_space_id: "alpha".into(),
        instance_id: "instance_1".into(),
        operation_id: "operation_1".into(),
        run_generation: 1,
    }
}

fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}

fn intent(locks: &InstallationLocks, owner: SessionIdentity) -> MaintenanceIntent {
    MaintenanceIntent {
        schema_version: 1,
        installation_key: locks.coordination_key().unwrap(),
        context_fingerprint: digest("configuration context"),
        transaction_fingerprint: digest("transaction evidence"),
        transaction_evidence: "transaction evidence".into(),
        previous_state_fingerprint: Some(digest("previous state")),
        next_state_fingerprint: digest("next state"),
        owner,
    }
}

fn maintenance(locks: &InstallationLocks) -> MaintenanceLease<'_> {
    let admission = locks.try_admit().unwrap();
    match admission.try_maintenance() {
        Ok(lease) => lease,
        Err((_admission, error)) => panic!("Expected exclusive maintenance admission: {error}"),
    }
}

fn begin(locks: &InstallationLocks, value: MaintenanceIntent) -> MaintenanceSnapshot {
    maintenance(locks).begin_intent(value).unwrap()
}

fn recovery<'a>(
    locks: &'a InstallationLocks,
    snapshot: &MaintenanceSnapshot,
) -> crate::maintenance::MaintenanceRecoveryLease<'a> {
    match locks.try_recover_maintenance(snapshot) {
        Ok(lease) => lease,
        Err(error) => panic!("Expected explicit maintenance recovery admission: {error}"),
    }
}

fn receipt(snapshot: &MaintenanceSnapshot) -> MaintenanceReceipt {
    let mut resolver = snapshot.intent.owner.clone();
    resolver.owner_id = "resolver_beta".into();
    resolver.operation_id = "resolve_1".into();
    MaintenanceReceipt {
        schema_version: 1,
        intent: snapshot.clone(),
        outcome: ResolutionOutcome::Committed,
        evidence_fingerprint: digest("verified evidence"),
        resolver,
    }
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, dir: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                result.insert(relative.clone(), None);
                visit(root, &path, result);
            } else {
                result.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn maintenance_denied(admission: Admission<'_>, expected: io::ErrorKind) -> Admission<'_> {
    match admission.try_maintenance() {
        Err((retained, error)) => {
            assert_eq!(error.kind(), expected);
            retained
        }
        Ok(_) => panic!("Maintenance admitted an unresolved installation intent"),
    }
}

#[test]
fn inspection_and_physical_aliases_are_write_free_and_share_the_intent_barrier() {
    let fixture = Fixture::new();
    let before = inventory(&fixture.base);
    let locks = fixture.locks();
    assert!(locks.inspect_maintenance().unwrap().pending.is_none());
    assert!(!locks.namespace.exists());
    assert_eq!(inventory(&fixture.base), before);

    let alias =
        InstallationLocks::capture_in(&fixture.installation.join("."), &fixture.registry.join("."))
            .unwrap();
    assert_eq!(alias.namespace, locks.namespace);
    let snapshot = begin(&locks, intent(&locks, identity()));
    assert_eq!(locks.inspect_maintenance().unwrap().pending, Some(snapshot));
    assert!(matches!(alias.try_admit(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));

    let preheld = locks.admit_for_recovery().unwrap();
    assert!(
        matches!(preheld.try_runtime(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    let preheld = maintenance_denied(preheld, io::ErrorKind::WouldBlock);
    drop(preheld);
    assert!(matches!(locks.try_admit(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

#[test]
fn explicit_receipt_retirement_is_ordered_idempotent_and_reenables_normal_leases() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let before = inventory(&fixture.installation);
    let snapshot = begin(&locks, intent(&locks, identity()));
    let resolver = recovery(&locks, &snapshot);
    assert!(resolver.verify_intent(&snapshot).is_ok());
    let receipt = receipt(&snapshot);
    assert!(resolver.retire_intent(&receipt).is_err());
    assert!(resolver.publish_receipt(&receipt).unwrap());
    assert!(!resolver.publish_receipt(&receipt).unwrap());
    let mut conflict = receipt.clone();
    conflict.evidence_fingerprint = digest("different evidence");
    assert!(
        matches!(resolver.publish_receipt(&conflict), Err(error) if error.kind() == io::ErrorKind::InvalidData)
    );
    assert!(resolver.retire_intent(&conflict).is_err());
    assert!(locks.inspect_maintenance().unwrap().pending.is_some());
    assert_eq!(
        locks.inspect_maintenance().unwrap().receipts,
        vec![receipt.clone()]
    );
    assert!(matches!(locks.try_admit(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));

    resolver.retire_intent(&receipt).unwrap();
    assert!(locks.inspect_maintenance().unwrap().pending.is_none());
    assert_eq!(locks.inspect_maintenance().unwrap().receipts, vec![receipt]);
    assert!(matches!(locks.try_admit(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    drop(resolver);

    let admission = locks.try_admit().unwrap();
    let runtime = admission.try_runtime().unwrap();
    drop(runtime);
    match admission.try_maintenance() {
        Ok(lease) => drop(lease),
        Err((_admission, error)) => {
            panic!("Maintenance remained blocked after retirement: {error}")
        }
    }
    assert_eq!(inventory(&fixture.installation), before);
}

#[test]
fn changed_context_physical_key_or_receipt_owner_cannot_retire_an_intent() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let snapshot = begin(&locks, intent(&locks, identity()));

    let mut changed_context = snapshot.clone();
    changed_context.intent.context_fingerprint = digest("other context");
    changed_context.fingerprint =
        blake3::hash(&serde_json::to_vec(&changed_context.intent).unwrap())
            .to_hex()
            .to_string();
    assert!(
        matches!(locks.try_recover_maintenance(&changed_context), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );

    let other = Fixture::new();
    let other_locks = other.locks();
    assert!(
        matches!(other_locks.try_recover_maintenance(&snapshot), Err(error) if error.kind() == io::ErrorKind::InvalidData)
    );

    let guard = recovery(&locks, &snapshot);
    let mut wrong_context_receipt = receipt(&snapshot);
    wrong_context_receipt.intent = changed_context;
    assert!(matches!(
        guard.publish_receipt(&wrong_context_receipt),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
    let mut invalid_resolver = receipt(&snapshot);
    invalid_resolver.resolver.game_space_id = "beta".into();
    assert!(
        matches!(guard.publish_receipt(&invalid_resolver), Err(error) if error.kind() == io::ErrorKind::InvalidData)
    );
    assert!(guard.retire_intent(&invalid_resolver).is_err());
    assert_eq!(locks.inspect_maintenance().unwrap().pending, Some(snapshot));
    assert!(locks.inspect_maintenance().unwrap().receipts.is_empty());
    drop(guard);
}

#[test]
fn retained_consumers_and_tampered_intent_block_recovery_without_deletion() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let snapshot = begin(&locks, intent(&locks, identity()));

    let record = crate::recovery::ConsumerRecord {
        cleanup: None,
        schema_version: 1,
        installation_key: locks.coordination_key().unwrap(),
        session: identity(),
    };
    let record_bytes = serde_json::to_vec(&record).unwrap();
    let record_path = locks
        .namespace
        .join("consumers")
        .join(format!("{}.json", blake3::hash(&record_bytes).to_hex()));
    fs::create_dir_all(record_path.parent().unwrap()).unwrap();
    fs::write(&record_path, &record_bytes).unwrap();
    assert!(
        matches!(locks.try_recover_maintenance(&snapshot), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    assert_eq!(fs::read(&record_path).unwrap(), record_bytes);
    assert_eq!(locks.inspect_consumers().unwrap(), vec![record]);
    fs::remove_file(&record_path).unwrap();

    let intent_path = locks.namespace.join("maintenance/intent.json");
    let original = fs::read(&intent_path).unwrap();
    let mut tampered: serde_json::Value = serde_json::from_slice(&original).unwrap();
    tampered["transaction_fingerprint"] = digest("tampered transaction").into();
    let tampered = serde_json::to_vec(&tampered).unwrap();
    fs::write(&intent_path, &tampered).unwrap();
    assert!(
        matches!(locks.try_recover_maintenance(&snapshot), Err(error) if error.kind() == io::ErrorKind::InvalidData)
    );
    assert_eq!(fs::read(&intent_path).unwrap(), tampered);
    assert!(matches!(locks.try_admit(), Err(error) if error.kind() == io::ErrorKind::InvalidData));
}

#[test]
fn malformed_future_unknown_residue_and_oversized_inventories_fail_closed() {
    for case in [
        "noncanonical",
        "future",
        "unknown",
        "oversized",
        "root_residue",
        "receipt_residue",
    ] {
        let fixture = Fixture::new();
        let locks = fixture.locks();
        let _snapshot = begin(&locks, intent(&locks, identity()));
        let maintenance_root = locks.namespace.join("maintenance");
        let intent_path = maintenance_root.join("intent.json");
        match case {
            "noncanonical" => {
                let mut bytes = fs::read(&intent_path).unwrap();
                bytes.push(b' ');
                fs::write(&intent_path, bytes).unwrap();
            }
            "future" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&intent_path).unwrap()).unwrap();
                value["schema_version"] = 2.into();
                fs::write(&intent_path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "unknown" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&intent_path).unwrap()).unwrap();
                value["unknown_field"] = true.into();
                fs::write(&intent_path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "oversized" => fs::write(&intent_path, vec![b' '; 16 * 1024 + 1]).unwrap(),
            "root_residue" => fs::write(maintenance_root.join("operator-file"), b"keep").unwrap(),
            "receipt_residue" => {
                fs::create_dir_all(maintenance_root.join("receipts")).unwrap();
                fs::write(maintenance_root.join("receipts/unowned.json"), b"{}").unwrap();
            }
            _ => unreachable!(),
        }
        let before = inventory(&fixture.base);
        assert!(locks.inspect_maintenance().is_err(), "{case}");
        assert!(
            matches!(
                locks.try_admit(),
                Err(error) if error.kind() == io::ErrorKind::InvalidData
            ),
            "{case}"
        );
        assert!(locks.try_recover_maintenance(&_snapshot).is_err(), "{case}");
        let preheld = locks.admit_for_recovery().unwrap();
        assert!(
            matches!(preheld.try_runtime(), Err(error) if error.kind() == io::ErrorKind::InvalidData)
        );
        drop(maintenance_denied(preheld, io::ErrorKind::InvalidData));
        assert_eq!(inventory(&fixture.base), before, "{case}");
    }
}

#[cfg(unix)]
#[test]
fn linked_intent_and_receipt_directories_are_rejected_without_touching_targets() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let locks = fixture.locks();
    let snapshot = begin(&locks, intent(&locks, identity()));
    let path = locks.namespace.join("maintenance/intent.json");
    let target = fixture.base.join("external-intent");
    let original = fs::read(&path).unwrap();
    fs::write(&target, &original).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&target, &path).unwrap();
    assert!(locks.inspect_maintenance().is_err());
    assert!(locks.try_admit().is_err());
    assert!(locks.try_recover_maintenance(&snapshot).is_err());
    assert_eq!(fs::read(&target).unwrap(), original);

    let fixture = Fixture::new();
    let locks = fixture.locks();
    let snapshot = begin(&locks, intent(&locks, identity()));
    let guard = recovery(&locks, &snapshot);
    let receipt = receipt(&snapshot);
    let receipt_target = fixture.base.join("external-receipt-directory");
    fs::create_dir(&receipt_target).unwrap();
    let receipt_root = locks.namespace.join("maintenance/receipts");
    symlink(&receipt_target, &receipt_root).unwrap();
    assert!(guard.publish_receipt(&receipt).is_err());
    assert!(locks.inspect_maintenance().is_err());
    assert!(locks.try_admit().is_err());
    assert!(fs::read_dir(receipt_target).unwrap().next().is_none());
    assert!(locks.namespace.join("maintenance/intent.json").is_file());
    drop(guard);
}

#[test]
fn receipt_quota_refuses_a_new_intent_before_publishing_its_marker() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let root = locks.namespace.join("maintenance/receipts");
    fs::create_dir_all(&root).unwrap();
    for index in 0..1024 {
        let evidence = format!("transaction evidence {index}");
        let mut transaction = intent(&locks, identity());
        transaction.transaction_evidence = evidence.clone();
        transaction.transaction_fingerprint = digest(&evidence);
        let snapshot = MaintenanceSnapshot {
            fingerprint: super::fingerprint(&transaction).unwrap(),
            intent: transaction,
        };
        let receipt = receipt(&snapshot);
        fs::write(
            root.join(format!("{}.json", snapshot.fingerprint)),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(locks.inspect_maintenance().unwrap().receipts.len(), 1024);
    let lease = maintenance(&locks);
    let result = lease.begin_intent(intent(&locks, identity()));
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    assert!(!locks.namespace.join("maintenance/intent.json").exists());
    assert_eq!(locks.inspect_maintenance().unwrap().receipts.len(), 1024);
}

struct Probe(Option<Child>);

impl Probe {
    fn kill_and_wait(&mut self) {
        let child = self.0.as_mut().unwrap();
        child.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Maintenance owner did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait().unwrap();
        self.0 = None;
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn killed_owner_intent_blocks_other_config_root_until_explicit_recovery_and_receipt() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let ready = fixture.base.join("owner-ready");
    let config = fixture.base.join("independent-config-root");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "maintenance::tests::owner_intent_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("FOXY_TEST_MAINT_INSTALL", &fixture.installation)
        .env("FOXY_TEST_MAINT_REGISTRY", &fixture.registry)
        .env("FOXY_TEST_MAINT_READY", &ready)
        .env("FOXY_CONFIG_DIR", &config)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut probe = Probe(Some(command.spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready.exists() {
        assert!(probe.0.as_mut().unwrap().try_wait().unwrap().is_none());
        assert!(
            Instant::now() < deadline,
            "Maintenance owner did not persist intent"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let snapshot = locks.inspect_maintenance().unwrap().pending.unwrap();
    assert!(matches!(locks.try_admit(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    let root_alias =
        InstallationLocks::capture_in(&fixture.installation, &fixture.registry).unwrap();
    assert_eq!(
        root_alias.inspect_maintenance().unwrap().pending,
        Some(snapshot.clone())
    );
    probe.kill_and_wait();
    assert_eq!(
        locks.inspect_maintenance().unwrap().pending,
        Some(snapshot.clone())
    );
    drop(locks.admit_for_recovery().unwrap());
    assert!(matches!(locks.try_admit(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    assert!(!config.exists());

    let guard = recovery(&locks, &snapshot);
    let receipt = receipt(&snapshot);
    assert!(guard.publish_receipt(&receipt).unwrap());
    guard.retire_intent(&receipt).unwrap();
    drop(guard);
    let admission = locks.try_admit().unwrap();
    drop(admission.try_runtime().unwrap());
    match admission.try_maintenance() {
        Ok(lease) => drop(lease),
        Err((_admission, error)) => {
            panic!("Maintenance remained blocked after retirement: {error}")
        }
    }
}

#[test]
#[ignore = "Invoked only by the independent-process maintenance-intent test"]
fn owner_intent_probe() {
    let install = PathBuf::from(std::env::var_os("FOXY_TEST_MAINT_INSTALL").unwrap());
    let registry = PathBuf::from(std::env::var_os("FOXY_TEST_MAINT_REGISTRY").unwrap());
    let ready = PathBuf::from(std::env::var_os("FOXY_TEST_MAINT_READY").unwrap());
    let locks = InstallationLocks::capture_in(&install, &registry).unwrap();
    let lease = maintenance(&locks);
    let snapshot = lease.begin_intent(intent(&locks, identity())).unwrap();
    fs::write(ready, snapshot.fingerprint).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(lease);
}
