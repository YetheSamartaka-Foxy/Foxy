use super::*;
use crate::{recovery::Registration, session::SessionIdentity};
use std::{fs, path::PathBuf};

struct Fixture {
    _root: tempfile::TempDir,
    installation: PathBuf,
    registry: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let installation = base.join("installation");
        fs::create_dir(&installation).unwrap();
        fs::write(installation.join("operator.cfg"), b"keep me").unwrap();
        Self {
            _root: root,
            installation,
            registry: base.join("private/registry"),
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

fn witnessed(locks: &InstallationLocks) -> (String, Registration) {
    let admission = locks.try_admit().unwrap();
    let registration =
        Registration::create_witnessed(&admission, &identity(), "group_alpha", &"a".repeat(64))
            .unwrap();
    let fingerprint = blake3::hash(&registration.bytes).to_hex().to_string();
    drop(admission);
    (fingerprint, registration)
}

fn receipt_path(locks: &InstallationLocks, fingerprint: &str) -> PathBuf {
    locks
        .namespace
        .join("cleanup")
        .join(format!("{fingerprint}.json"))
}

#[test]
fn legacy_job_accounting_receipt_never_authorizes_retirement() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let (fingerprint, registration) = witnessed(&locks);
    let path = receipt_path(&locks, &fingerprint);
    let mut legacy: CleanupReceipt = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy.observation = Some(CleanupObservation {
        backend: "windows_job".into(),
        owner_exit_observed: true,
        tree_exited: true,
    });
    assert!(legacy.validates());
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let row = locks.inspect_cleanup().unwrap().remove(0);
    assert_eq!(row.receipt_state, "legacy_unverified");
    assert!(
        locks
            .retire_verified_consumer(&fingerprint, row.receipt_fingerprint.as_deref().unwrap())
            .is_err()
    );
    assert_eq!(read_record(&registration.path).unwrap(), registration.bytes);
}

#[test]
fn shipped_cleanup_receipt_has_a_bound_strict_consumer() {
    let receipt: CleanupReceipt = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_consumer_cleanup_receipt.json"
    ))
    .unwrap();
    let canonical = serde_json::to_vec(&receipt.consumer).unwrap();
    assert_eq!(
        receipt.consumer_fingerprint,
        blake3::hash(&canonical).to_hex().as_str()
    );
    assert!(receipt.validates());
}

#[test]
fn only_exact_completed_receipt_retires_registration_and_duplicate_ack_is_idempotent() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let (fingerprint, registration) = witnessed(&locks);
    let mut writer =
        WitnessReceiptWriter::open(registration.receipt().unwrap().try_clone().unwrap()).unwrap();
    writer.complete("linux_pid_namespace", false).unwrap();
    let rows = locks.inspect_cleanup().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].receipt_state, "verified_exited");
    let receipt_fingerprint = rows[0].receipt_fingerprint.as_deref().unwrap();
    assert!(
        locks
            .retire_verified_consumer(&fingerprint, receipt_fingerprint)
            .unwrap()
    );
    assert!(
        !locks
            .retire_verified_consumer(&fingerprint, receipt_fingerprint)
            .unwrap()
    );
    let history = locks.inspect_cleanup().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].receipt_state, "verified_history");
    assert_eq!(
        fs::read(fixture.installation.join("operator.cfg")).unwrap(),
        b"keep me"
    );
}

#[test]
fn pending_legacy_malformed_and_foreign_receipts_never_retire_consumer() {
    for mode in ["pending", "legacy", "malformed", "foreign"] {
        let fixture = Fixture::new();
        let locks = fixture.locks();
        let (fingerprint, registration) = if mode == "legacy" {
            let admission = locks.try_admit().unwrap();
            let registration = Registration::create(&admission, &identity()).unwrap();
            let fingerprint = blake3::hash(&registration.bytes).to_hex().to_string();
            drop(admission);
            (fingerprint, registration)
        } else {
            witnessed(&locks)
        };
        let path = receipt_path(&locks, &fingerprint);
        if mode == "malformed" {
            fs::write(&path, b"{").unwrap();
        }
        if mode == "foreign" {
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            value["consumer"]["installation_key"] = "foreign_key".into();
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let receipt_fp = locks.inspect_cleanup().ok().and_then(|rows| {
            rows.into_iter()
                .find(|row| row.consumer_fingerprint == fingerprint)
                .and_then(|row| row.receipt_fingerprint)
        });
        assert!(
            receipt_fp.is_none_or(|fp| locks.retire_verified_consumer(&fingerprint, &fp).is_err())
        );
        assert_eq!(locks.inspect_consumers().unwrap().len(), 1, "case: {mode}");
        drop(registration);
    }
}

#[test]
fn writer_rejects_changed_consumer_or_pending_receipt_bytes_and_unknown_fields() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let (fingerprint, registration) = witnessed(&locks);
    let file = registration.receipt().unwrap();
    let mut writer = WitnessReceiptWriter::open(file.try_clone().unwrap()).unwrap();
    let mut changed = registration.bytes.clone();
    changed.push(b' ');
    fs::write(&registration.path, changed).unwrap();
    assert_eq!(locks.inspect_consumers().unwrap().len(), 1);
    let path = receipt_path(&locks, &fingerprint);
    let mut pending_bytes = fs::read(&path).unwrap();
    pending_bytes.push(b' ');
    fs::write(&path, pending_bytes).unwrap();
    assert!(writer.complete("linux_pid_namespace", false).is_err());
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["unexpected"] = true.into();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    match WitnessReceiptWriter::open(file.try_clone().unwrap()) {
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::InvalidData),
        Ok(_) => panic!("Writer accepted an unknown receipt field"),
    }
}

#[test]
fn same_physical_installation_uses_shared_receipt_registry_across_config_roots() {
    let fixture = Fixture::new();
    let first = fixture.locks();
    fs::create_dir(fixture._root.path().join("config-alpha")).unwrap();
    fs::create_dir(fixture._root.path().join("config-beta")).unwrap();
    let second = InstallationLocks::capture_in(&fixture.installation, &fixture.registry).unwrap();
    assert_eq!(
        first.coordination_key().unwrap(),
        second.coordination_key().unwrap()
    );
    let (fingerprint, registration) = witnessed(&first);
    assert_eq!(
        second.inspect_cleanup().unwrap()[0].consumer_fingerprint,
        fingerprint
    );
    let mut writer =
        WitnessReceiptWriter::open(registration.receipt().unwrap().try_clone().unwrap()).unwrap();
    writer.complete("windows_job_waited_members", true).unwrap();
    let row = second.inspect_cleanup().unwrap().remove(0);
    assert!(
        second
            .retire_verified_consumer(&fingerprint, row.receipt_fingerprint.as_deref().unwrap())
            .unwrap()
    );
}

#[test]
fn replaced_registration_and_stale_receipt_fingerprint_are_refused() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let (fingerprint, registration) = witnessed(&locks);
    let mut writer =
        WitnessReceiptWriter::open(registration.receipt().unwrap().try_clone().unwrap()).unwrap();
    writer.complete("linux_pid_namespace", true).unwrap();
    let row = locks.inspect_cleanup().unwrap().remove(0);
    assert!(
        locks
            .retire_verified_consumer(&fingerprint, &"0".repeat(64))
            .is_err()
    );
    let replacement = locks.namespace.join("consumers").join("replacement.json");
    fs::rename(&registration.path, &replacement).unwrap();
    assert!(
        locks
            .retire_verified_consumer(&fingerprint, row.receipt_fingerprint.as_deref().unwrap())
            .is_err()
    );
    assert!(replacement.exists());
}

#[test]
fn unrelated_registry_files_are_preserved_and_block_receipt_retirement() {
    let fixture = Fixture::new();
    let locks = fixture.locks();
    let (fingerprint, registration) = witnessed(&locks);
    let mut writer =
        WitnessReceiptWriter::open(registration.receipt().unwrap().try_clone().unwrap()).unwrap();
    writer.complete("linux_pid_namespace", true).unwrap();
    let row = locks.inspect_cleanup().unwrap().remove(0);
    let unrelated = locks.namespace.join("cleanup").join("operator-note.txt");
    fs::write(&unrelated, b"preserve this file").unwrap();
    assert_eq!(
        locks.inspect_cleanup().unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&unrelated).unwrap(), b"preserve this file");
    assert_eq!(locks.inspect_consumers().unwrap().len(), 1);
    assert!(row.receipt_fingerprint.is_some());
    assert_eq!(fingerprint, row.consumer_fingerprint);
}
