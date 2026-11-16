use super::*;

#[test]
fn shipped_consumer_registration_matches_the_strict_schema() {
    let record: ConsumerRecord = serde_json::from_str(include_str!(
        "../../../examples/json/server_backend/server_consumer_registration.json"
    ))
    .unwrap();
    assert_eq!(record.schema_version, 1);
    record.session.validate().unwrap();
    assert!(!record_name(&record).unwrap().contains('/'));
}
use crate::leases::{MaintenanceLease, RuntimeLease};
use std::collections::BTreeMap;
use std::fs::File;
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
        let installation = base.join("installation");
        fs::create_dir(&installation).unwrap();
        fs::write(installation.join("package.bin"), b"unchanged package").unwrap();
        let registry = base.join("private/lease-registry");
        Self {
            _root: root,
            base,
            installation,
            registry,
        }
    }

    fn capture(&self) -> InstallationLocks {
        InstallationLocks::capture_in(&self.installation, &self.registry).unwrap()
    }
}

fn identity(instance: &str) -> SessionIdentity {
    SessionIdentity {
        owner_id: "owner_alpha".into(),
        owner_generation: 1,
        game_space_id: "alpha".into(),
        instance_id: instance.into(),
        operation_id: "operation_1".into(),
        run_generation: 1,
    }
}

#[test]
fn consumer_slot_review_preserves_records_and_rejects_duplicate_before_spawn() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let consumers = locks.namespace.join("consumers");
    assert!(!consumers.exists());
    admission
        .check_consumer_slot("alpha", "instance_1")
        .unwrap();
    assert!(!consumers.exists());
    let registration = Registration::create(&admission, &identity("instance_1")).unwrap();
    let before = inventory(&consumers);
    assert_eq!(
        admission
            .check_consumer_slot("alpha", "instance_1")
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(Registration::create(&admission, &identity("instance_1")).is_err());
    admission
        .check_consumer_slot("alpha", "instance_2")
        .unwrap();
    assert_eq!(inventory(&consumers), before);
    assert_eq!(
        fs::read(fixture.installation.join("package.bin")).unwrap(),
        b"unchanged package"
    );
    drop(registration);
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let name = PathBuf::from(path.file_name().unwrap());
        if path.is_dir() {
            result.insert(name.clone(), None);
            for (relative, bytes) in inventory(&path) {
                result.insert(name.join(relative), bytes);
            }
        } else {
            result.insert(name, Some(fs::read(path).unwrap()));
        }
    }
    result
}

fn register(locks: &InstallationLocks, session: &SessionIdentity) -> (RuntimeLease, Registration) {
    let admission = locks.try_admit().unwrap();
    let runtime = admission.try_runtime().unwrap();
    let registration = Registration::create(&admission, session).unwrap();
    drop(admission);
    (runtime, registration)
}

fn maintenance_denied(admission: Admission<'_>, kind: io::ErrorKind) -> Admission<'_> {
    match admission.try_maintenance() {
        Err((retained, error)) => {
            assert_eq!(error.kind(), kind);
            retained
        }
        Ok(_) => panic!("Maintenance accepted an unreconciled consumer"),
    }
}

fn maintenance(mut admission: Admission<'_>) -> MaintenanceLease<'_> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match admission.try_maintenance() {
            Ok(lease) => return lease,
            Err((retained, error)) => {
                assert!(
                    error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline,
                    "Maintenance refused an empty consumer registry: {error}"
                );
                // Concurrent fork fixtures can retain CLOEXEC locks until exec completes.
                admission = retained;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn absent_consumer_inspection_is_write_free() {
    let fixture = Fixture::new();
    let before = inventory(&fixture.base);
    let locks = fixture.capture();
    assert!(locks.inspect_consumers().unwrap().is_empty());
    assert!(!fixture.registry.exists());
    assert!(!locks.namespace.exists());
    assert_eq!(inventory(&fixture.base), before);
}

#[test]
fn registration_is_serialized_by_admission_and_binds_the_exact_record_name() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admitted = locks.try_admit().unwrap();
    let registration = Registration::create(&admitted, &identity("instance_1")).unwrap();
    match locks.try_admit() {
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
        Ok(_) => panic!("A second admission entered registration creation"),
    }
    let record = ConsumerRecord {
        cleanup: None,
        schema_version: 1,
        installation_key: locks
            .namespace
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        session: identity("instance_1"),
    };
    assert_eq!(locks.inspect_consumers().unwrap(), vec![record.clone()]);
    let bytes = serde_json::to_vec(&record).unwrap();
    assert_eq!(fs::read(&registration.path).unwrap(), bytes);
    assert_eq!(
        registration.path.file_name().unwrap(),
        std::ffi::OsStr::new(&format!("{}.json", blake3::hash(&bytes).to_hex())),
    );
    drop(admitted);
    drop(registration);
    assert_eq!(locks.inspect_consumers().unwrap(), vec![record]);
}

#[test]
fn invalid_registration_identity_does_not_create_a_consumer_directory() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let mut invalid = identity("instance_1");
    invalid.game_space_id = "../alpha".into();
    assert!(Registration::create(&admission, &invalid).is_err());
    assert!(!locks.namespace.join("consumers").exists());
    assert!(locks.inspect_consumers().unwrap().is_empty());
    drop(maintenance(admission));
}

#[test]
fn distinct_instances_share_runtime_but_duplicate_instance_generations_are_refused() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let (first_runtime, mut first) = register(&locks, &identity("instance_1"));
    let (second_runtime, mut second) = register(&locks, &identity("instance_2"));
    let admission = locks.try_admit().unwrap();
    let mut duplicate = identity("instance_1");
    duplicate.owner_id = "owner_beta".into();
    duplicate.owner_generation = 2;
    duplicate.operation_id = "operation_2".into();
    duplicate.run_generation = 2;
    match Registration::create(&admission, &duplicate) {
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
        Ok(_) => panic!("Changed owner identity bypassed the live instance registration"),
    }
    assert_eq!(locks.inspect_consumers().unwrap().len(), 2);
    let admission = maintenance_denied(admission, io::ErrorKind::WouldBlock);
    first.retire().unwrap();
    drop(first_runtime);
    let admission = maintenance_denied(admission, io::ErrorKind::WouldBlock);
    second.retire().unwrap();
    drop(second_runtime);
    drop(maintenance(admission));
}

#[test]
fn dropping_registration_and_runtime_does_not_authorize_maintenance() {
    let fixture = Fixture::new();
    let before = inventory(&fixture.installation);
    let locks = fixture.capture();
    let (runtime, registration) = register(&locks, &identity("instance_1"));
    let path = registration.path.clone();
    let bytes = fs::read(&path).unwrap();
    let admission = maintenance_denied(locks.try_admit().unwrap(), io::ErrorKind::WouldBlock);
    drop(registration);
    drop(runtime);
    let content = OpenOptions::new()
        .read(true)
        .write(true)
        .open(locks.namespace.join("content.lock"))
        .unwrap();
    content.try_lock().unwrap();
    drop(content);
    drop(maintenance_denied(admission, io::ErrorKind::WouldBlock));
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(locks.inspect_consumers().unwrap().len(), 1);
    assert_eq!(inventory(&fixture.installation), before);
}

#[test]
fn retiring_the_exact_owned_marker_preserves_runtime_lock_until_release() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let (runtime, mut registration) = register(&locks, &identity("instance_1"));
    let path = registration.path.clone();
    registration.retire().unwrap();
    assert!(!path.exists());
    assert!(locks.inspect_consumers().unwrap().is_empty());
    let admission = maintenance_denied(locks.try_admit().unwrap(), io::ErrorKind::WouldBlock);
    drop(runtime);
    drop(maintenance(admission));
    assert!(locks.namespace.join("admission.lock").is_file());
    assert!(locks.namespace.join("content.lock").is_file());
}

#[test]
fn changed_marker_bytes_cannot_be_retired_by_the_original_registration() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let (runtime, mut registration) = register(&locks, &identity("instance_1"));
    let mut changed = registration.bytes.clone();
    changed.push(b'\n');
    fs::write(&registration.path, &changed).unwrap();
    assert_eq!(
        registration.retire().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&registration.path).unwrap(), changed);
    drop(runtime);
    drop(maintenance_denied(
        locks.try_admit().unwrap(),
        io::ErrorKind::WouldBlock,
    ));
}

#[test]
fn malformed_oversized_unknown_and_misbound_consumer_records_block_maintenance() {
    for case in [
        "malformed",
        "oversized",
        "schema",
        "field",
        "session_field",
        "identity",
        "installation",
    ] {
        let fixture = Fixture::new();
        let locks = fixture.capture();
        let admission = locks.try_admit().unwrap();
        let registration = Registration::create(&admission, &identity("instance_1")).unwrap();
        let mut record: ConsumerRecord = serde_json::from_slice(&registration.bytes).unwrap();
        let mut path = registration.path.clone();
        let bytes = match case {
            "malformed" => b"{".to_vec(),
            "oversized" => vec![b' '; MAX_RECORD_BYTES as usize + 1],
            "field" | "session_field" => {
                let mut value = serde_json::to_value(&record).unwrap();
                if case == "field" {
                    value["unexpected"] = true.into();
                } else {
                    value["session"]["unexpected"] = true.into();
                }
                serde_json::to_vec(&value).unwrap()
            }
            "schema" | "identity" | "installation" => {
                match case {
                    "schema" => record.schema_version = 2,
                    "identity" => record.session.owner_generation = 0,
                    "installation" => record.installation_key = "other_installation".into(),
                    _ => unreachable!(),
                }
                let renamed = path.parent().unwrap().join(record_name(&record).unwrap());
                fs::rename(&path, &renamed).unwrap();
                path = renamed;
                serde_json::to_vec(&record).unwrap()
            }
            _ => unreachable!(),
        };
        fs::write(&path, &bytes).unwrap();
        assert!(locks.inspect_consumers().is_err());
        match admission.try_maintenance() {
            Err((retained, _)) => drop(retained),
            Ok(_) => panic!("Maintenance accepted an invalid consumer record"),
        }
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn renamed_records_and_extra_files_or_directories_are_not_ignored() {
    for extra in ["renamed", "file", "directory"] {
        let fixture = Fixture::new();
        let locks = fixture.capture();
        let admission = locks.try_admit().unwrap();
        let registration = Registration::create(&admission, &identity("instance_1")).unwrap();
        let directory = registration.path.parent().unwrap();
        match extra {
            "renamed" => fs::rename(&registration.path, directory.join("unexpected.json")).unwrap(),
            "file" => fs::write(directory.join("unowned.txt"), b"unowned file").unwrap(),
            "directory" => fs::create_dir(directory.join("unowned-directory")).unwrap(),
            _ => unreachable!(),
        }
        assert!(locks.inspect_consumers().is_err());
        match admission.try_maintenance() {
            Err((retained, _)) => drop(retained),
            Ok(_) => panic!("Maintenance ignored an unexpected registry entry"),
        }
    }
}

#[test]
fn consumer_count_is_bounded_before_maintenance() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let registration = Registration::create(&admission, &identity("instance_0")).unwrap();
    let directory = registration.path.parent().unwrap();
    let mut record: ConsumerRecord = serde_json::from_slice(&registration.bytes).unwrap();
    for index in 1..MAX_CONSUMERS {
        record.session.instance_id = format!("instance_{index}");
        fs::write(
            directory.join(record_name(&record).unwrap()),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(locks.inspect_consumers().unwrap().len(), MAX_CONSUMERS);
    assert!(Registration::create(&admission, &identity("instance_extra")).is_err());
    assert_eq!(fs::read_dir(directory).unwrap().count(), MAX_CONSUMERS);
    record.session.instance_id = "instance_extra".into();
    fs::write(
        directory.join(record_name(&record).unwrap()),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        locks.inspect_consumers().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    drop(maintenance_denied(admission, io::ErrorKind::InvalidData));
}

#[cfg(unix)]
#[test]
fn a_fifo_consumer_entry_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;

    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let registration = Registration::create(&admission, &identity("instance_1")).unwrap();
    let path = registration.path.parent().unwrap().join("fifo");
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    assert!(locks.inspect_consumers().is_err());
    drop(maintenance_denied(admission, io::ErrorKind::InvalidData));
}

#[cfg(unix)]
#[test]
fn linked_consumer_directory_or_marker_is_refused_without_touching_its_target() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let target = fixture.base.join("unowned-consumers");
    fs::create_dir(&target).unwrap();
    symlink(&target, locks.namespace.join("consumers")).unwrap();
    assert!(locks.inspect_consumers().is_err());
    assert!(Registration::create(&admission, &identity("instance_1")).is_err());
    assert!(fs::read_dir(&target).unwrap().next().is_none());
    drop(admission);

    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let mut registration = Registration::create(&admission, &identity("instance_1")).unwrap();
    let target = fixture.base.join("unowned-marker");
    fs::write(&target, &registration.bytes).unwrap();
    fs::remove_file(&registration.path).unwrap();
    symlink(&target, &registration.path).unwrap();
    assert!(locks.inspect_consumers().is_err());
    assert!(registration.retire().is_err());
    assert_eq!(fs::read(target).unwrap(), registration.bytes);
    match admission.try_maintenance() {
        Err((retained, _)) => drop(retained),
        Ok(_) => panic!("Maintenance accepted a linked consumer marker"),
    }
}

struct Probe(Option<Child>);

impl Probe {
    fn kill_and_wait(&mut self) {
        let child = self.0.as_mut().unwrap();
        child.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "Registration probe did not exit");
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
fn killed_owner_leaves_a_consumer_marker_that_blocks_released_content_maintenance() {
    let fixture = Fixture::new();
    let adopted = inventory(&fixture.installation);
    let ready = fixture.base.join("ready");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "recovery::tests::crash_registration_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("FOXY_TEST_CONSUMER_INSTALLATION", &fixture.installation)
        .env("FOXY_TEST_CONSUMER_REGISTRY", &fixture.registry)
        .env("FOXY_TEST_CONSUMER_READY", &ready)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut probe = Probe(Some(command.spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(probe.0.as_mut().unwrap().try_wait().unwrap().is_none());
        assert!(
            Instant::now() < deadline,
            "Registration probe did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let locks = fixture.capture();
    let records = locks.inspect_consumers().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session, identity("instance_1"));
    let path = locks
        .namespace
        .join("consumers")
        .join(record_name(&records[0]).unwrap());
    let bytes = fs::read(&path).unwrap();
    let admission = maintenance_denied(locks.try_admit().unwrap(), io::ErrorKind::WouldBlock);
    let content = File::options()
        .read(true)
        .write(true)
        .open(locks.namespace.join("content.lock"))
        .unwrap();
    match content.try_lock() {
        Err(error) => assert_eq!(io::Error::from(error).kind(), io::ErrorKind::WouldBlock),
        Ok(_) => panic!("Live registration probe did not retain its content lease"),
    }
    probe.kill_and_wait();
    content.try_lock().unwrap();
    drop(content);
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(locks.inspect_consumers().unwrap(), records);
    drop(maintenance_denied(admission, io::ErrorKind::WouldBlock));
    assert_eq!(inventory(&fixture.installation), adopted);
}

#[test]
#[ignore = "Invoked only by the independent-process consumer registration test"]
fn crash_registration_probe() {
    let directory = PathBuf::from(std::env::var_os("FOXY_TEST_CONSUMER_INSTALLATION").unwrap());
    let registry = PathBuf::from(std::env::var_os("FOXY_TEST_CONSUMER_REGISTRY").unwrap());
    let locks = InstallationLocks::capture_in(&directory, &registry).unwrap();
    let (runtime, registration) = register(&locks, &identity("instance_1"));
    fs::write(
        PathBuf::from(std::env::var_os("FOXY_TEST_CONSUMER_READY").unwrap()),
        b"ready",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(registration);
    drop(runtime);
}
