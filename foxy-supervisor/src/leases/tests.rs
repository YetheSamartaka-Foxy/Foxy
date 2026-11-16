use super::*;

use std::collections::BTreeMap;
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
        fs::create_dir(installation.join("content")).unwrap();
        fs::write(installation.join("content/package.bin"), b"base content").unwrap();
        fs::write(installation.join("manual.cfg"), b"operator configuration").unwrap();
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

    fn probe(&self, mode: &str) -> Probe {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "leases::tests::independent_process_lease_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("FOXY_TEST_INSTALLATION_DIRECTORY", &self.installation)
            .env("FOXY_TEST_INSTALLATION_REGISTRY", &self.registry)
            .env("FOXY_TEST_INSTALLATION_PROBE", mode)
            .env("FOXY_TEST_INSTALLATION_READY", self.base.join("ready"))
            .env("FOXY_CONFIG_DIR", self.base.join(format!("config-{mode}")))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        Probe(Some(command.spawn().unwrap()))
    }
}

struct Probe(Option<Child>);

impl Probe {
    fn finish(mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "Lease probe exceeded its deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = self.0.take().unwrap().wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "Lease probe failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn terminate_and_wait(&mut self) {
        let child = self.0.as_mut().unwrap();
        child.kill().unwrap();
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

fn assert_admission_busy(locks: &InstallationLocks) {
    match locks.try_admit() {
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
        Ok(_) => panic!("A second admission guard was granted"),
    }
}

fn maintenance_busy(admission: Admission<'_>) -> Admission<'_> {
    match admission.try_maintenance() {
        Err((admission, error)) => {
            assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
            admission
        }
        Ok(_) => panic!("Maintenance was granted while a runtime lease was held"),
    }
}

fn maintenance(admission: Admission<'_>) -> MaintenanceLease<'_> {
    match admission.try_maintenance() {
        Ok(lease) => lease,
        Err((_, error)) => panic!("Maintenance lease was refused: {error}"),
    }
}

fn file_identity(file: &File) -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().unwrap();
        (metadata.dev(), metadata.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use winapi::um::fileapi::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        assert_ne!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) },
            0
        );
        (
            information.dwVolumeSerialNumber as u64,
            (information.nFileIndexHigh as u64) << 32 | information.nFileIndexLow as u64,
        )
    }
}

#[test]
fn capture_and_revalidation_do_not_create_the_registry_or_edit_the_installation() {
    let fixture = Fixture::new();
    let before = inventory(&fixture.base);
    let locks = fixture.capture();
    locks.revalidate().unwrap();
    assert!(!fixture.registry.exists());
    assert!(!locks.namespace.exists());
    assert_eq!(inventory(&fixture.base), before);

    let adopted = inventory(&fixture.installation);
    let admission = locks.try_admit().unwrap();
    assert!(locks.namespace.join("admission.lock").is_file());
    assert!(!locks.namespace.join("content.lock").exists());
    let runtime = admission.try_runtime().unwrap();
    drop(admission);
    assert_eq!(inventory(&fixture.installation), adopted);
    drop(runtime);
    drop(maintenance(locks.try_admit().unwrap()));
    assert_eq!(inventory(&fixture.installation), adopted);
}

#[test]
fn shared_guards_release_independently_while_maintenance_retains_admission() {
    let fixture = Fixture::new();
    let first = fixture.capture();
    let second = fixture.capture();
    let admission = first.try_admit().unwrap();
    let first_runtime = admission.try_runtime().unwrap();
    drop(admission);
    let admission = second.try_admit().unwrap();
    let second_runtime = admission.try_runtime().unwrap();
    drop(admission);
    drop(first);

    let admission = maintenance_busy(second.try_admit().unwrap());
    assert_admission_busy(&second);
    drop(first_runtime);
    let admission = maintenance_busy(admission);
    assert_admission_busy(&second);
    drop(second_runtime);
    let lease = maintenance(admission);
    assert_admission_busy(&second);
    drop(lease);
    drop(second.try_admit().unwrap());
}

#[test]
fn canonical_and_dot_aliases_share_the_physical_installation_namespace() {
    let fixture = Fixture::new();
    let original = fixture.capture();
    let dotted =
        InstallationLocks::capture_in(&fixture.installation.join("."), &fixture.registry).unwrap();
    let canonical = InstallationLocks::capture_in(&original.directory, &fixture.registry).unwrap();
    assert_eq!(original.identity, dotted.identity);
    assert_eq!(original.identity, canonical.identity);
    assert_eq!(original.namespace, dotted.namespace);
    assert_eq!(original.namespace, canonical.namespace);
    let before = inventory(&fixture.base);
    assert_eq!(
        original.coordination_key().unwrap(),
        dotted.coordination_key().unwrap()
    );
    assert_eq!(
        original.coordination_key().unwrap(),
        canonical.coordination_key().unwrap()
    );
    assert_eq!(inventory(&fixture.base), before);
    let admission = original.try_admit().unwrap();
    assert_admission_busy(&dotted);
    assert_admission_busy(&canonical);
    drop(admission);
    drop(dotted.try_admit().unwrap());
}

#[test]
fn maintenance_identity_is_the_retained_physical_installation_key() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let key = locks.coordination_key().unwrap();
    assert!(!locks.namespace.exists());
    let maintenance = maintenance(locks.try_admit().unwrap());
    assert_eq!(maintenance.installation_key().unwrap(), key);
    assert_admission_busy(&fixture.capture());
    let other_directory = fixture.base.join("other-installation");
    fs::create_dir(&other_directory).unwrap();
    let other = InstallationLocks::capture_in(&other_directory, &fixture.registry).unwrap();
    assert_ne!(other.coordination_key().unwrap(), key);
    drop(maintenance);
    drop(locks.try_admit().unwrap());
}

#[test]
fn lock_files_persist_as_the_same_objects_after_all_guards_are_released() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let runtime = admission.try_runtime().unwrap();
    drop(admission);
    drop(runtime);
    let admission_path = locks.namespace.join("admission.lock");
    let content_path = locks.namespace.join("content.lock");
    let admission_file = File::open(&admission_path).unwrap();
    let content_file = File::open(&content_path).unwrap();
    let identities = (file_identity(&admission_file), file_identity(&content_file));

    drop(maintenance(locks.try_admit().unwrap()));
    let admission = locks.try_admit().unwrap();
    let runtime = admission.try_runtime().unwrap();
    drop(admission);
    drop(runtime);
    assert_eq!(
        (
            file_identity(&File::open(&admission_path).unwrap()),
            file_identity(&File::open(&content_path).unwrap()),
        ),
        identities
    );
    assert_eq!(file_identity(&admission_file), identities.0);
    assert_eq!(file_identity(&content_file), identities.1);
    assert!(fs::read(admission_path).unwrap().is_empty());
    assert!(fs::read(content_path).unwrap().is_empty());
}

#[test]
fn replaced_installation_roots_are_refused_or_pinned_against_replacement() {
    let fixture = Fixture::new();
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let moved = fixture.base.join("original-installation");
    match fs::rename(&fixture.installation, &moved) {
        Ok(()) => {}
        Err(error) => {
            assert!(
                cfg!(windows)
                    && (matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                    ) || matches!(error.raw_os_error(), Some(5 | 32 | 33))),
                "Unexpected installation rename refusal: {error}"
            );
            locks.revalidate().unwrap();
            drop(admission.try_runtime().unwrap());
            return;
        }
    }
    fs::create_dir(&fixture.installation).unwrap();
    fs::write(fixture.installation.join("replacement.bin"), b"replacement").unwrap();
    let replacement = inventory(&fixture.installation);
    assert_eq!(
        locks.revalidate().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        locks.coordination_key().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    match admission.try_runtime() {
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::InvalidData),
        Ok(_) => panic!("A runtime lease accepted a replaced installation"),
    }
    match admission.try_maintenance() {
        Err((retained, error)) => {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            drop(retained);
        }
        Ok(_) => panic!("Maintenance accepted a replaced installation"),
    }
    assert!(locks.try_admit().is_err());
    assert_eq!(inventory(&fixture.installation), replacement);
    assert_eq!(
        fs::read(moved.join("manual.cfg")).unwrap(),
        b"operator configuration"
    );
}

#[test]
fn invalid_registry_destinations_fail_without_modifying_adopted_files() {
    let fixture = Fixture::new();
    let before = inventory(&fixture.installation);
    let registry = fixture.base.join("registry-file");
    fs::write(&registry, b"unowned registry file").unwrap();
    assert!(InstallationLocks::capture_in(&fixture.installation, &registry).is_err());
    assert_eq!(fs::read(registry).unwrap(), b"unowned registry file");
    assert_eq!(inventory(&fixture.installation), before);
    assert!(InstallationLocks::capture_in(Path::new("relative"), &fixture.registry).is_err());
    assert!(InstallationLocks::capture_in(&fixture.installation, Path::new("relative")).is_err());
}

#[test]
fn registries_cannot_overlap_installations_or_create_paths_before_rejecting_parent_segments() {
    let fixture = Fixture::new();
    let before = inventory(&fixture.base);
    for registry in [
        &fixture.installation,
        &fixture.installation.join("locks"),
        &fixture.base,
    ] {
        assert!(InstallationLocks::capture_in(&fixture.installation, registry).is_err());
    }
    let mut raw = fixture.base.as_os_str().to_os_string();
    for part in ["must-not-exist", "..", "registry"] {
        raw.push(std::path::MAIN_SEPARATOR_STR);
        raw.push(part);
    }
    let parent_segment = PathBuf::from(raw);
    assert!(InstallationLocks::capture_in(&fixture.installation, &parent_segment).is_err());
    assert_eq!(inventory(&fixture.base), before);
}

#[cfg(unix)]
#[test]
fn linked_installation_and_registry_directories_are_refused() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let before = inventory(&fixture.installation);
    let installation_alias = fixture.base.join("installation-alias");
    symlink(&fixture.installation, &installation_alias).unwrap();
    assert!(InstallationLocks::capture_in(&installation_alias, &fixture.registry).is_err());
    let target = fixture.base.join("external-registry");
    fs::create_dir(&target).unwrap();
    let registry_alias = fixture.base.join("registry-alias");
    symlink(&target, &registry_alias).unwrap();
    assert!(InstallationLocks::capture_in(&fixture.installation, &registry_alias).is_err());
    assert!(fs::read_dir(target).unwrap().next().is_none());
    assert_eq!(inventory(&fixture.installation), before);
}

#[cfg(unix)]
#[test]
fn linked_lock_files_are_refused_without_touching_their_targets() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let locks = fixture.capture();
    let target = fixture.base.join("unowned-file");
    fs::write(&target, b"unchanged").unwrap();
    fs::create_dir_all(&locks.namespace).unwrap();
    let admission_path = locks.namespace.join("admission.lock");
    symlink(&target, admission_path).unwrap();
    assert!(locks.try_admit().is_err());
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");

    let content_fixture = Fixture::new();
    let locks = content_fixture.capture();
    let target = content_fixture.base.join("unowned-file");
    fs::write(&target, b"unchanged").unwrap();
    let admission = locks.try_admit().unwrap();
    let content_path = locks.namespace.join("content.lock");
    symlink(&target, content_path).unwrap();
    assert!(admission.try_runtime().is_err());
    match admission.try_maintenance() {
        Err((retained, error)) => {
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_admission_busy(&locks);
            drop(retained);
        }
        Ok(_) => panic!("Maintenance accepted a linked content lock"),
    }
    assert_eq!(fs::read(target).unwrap(), b"unchanged");
}

#[test]
fn independent_processes_share_runtime_and_contend_on_maintenance_and_admission() {
    let fixture = Fixture::new();
    let adopted = inventory(&fixture.installation);
    let locks = fixture.capture();
    let admission = locks.try_admit().unwrap();
    let runtime = admission.try_runtime().unwrap();
    fixture.probe("admission_busy").finish();
    drop(admission);
    fixture.probe("runtime").finish();
    fixture.probe("maintenance_busy").finish();
    let admission = maintenance_busy(locks.try_admit().unwrap());
    fixture.probe("admission_busy").finish();
    drop(runtime);
    let lease = maintenance(admission);
    fixture.probe("admission_busy").finish();
    drop(lease);
    fixture.probe("maintenance").finish();
    fixture.probe("runtime").finish();
    assert_eq!(inventory(&fixture.installation), adopted);
    for mode in [
        "admission_busy",
        "runtime",
        "maintenance_busy",
        "maintenance",
    ] {
        assert!(!fixture.base.join(format!("config-{mode}")).exists());
    }
}

#[test]
fn abrupt_lease_holder_exit_releases_the_os_lock_without_removing_lock_files() {
    let fixture = Fixture::new();
    let adopted = inventory(&fixture.installation);
    let mut owner = fixture.probe("hold_runtime");
    let ready = fixture.base.join("ready");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(owner.0.as_mut().unwrap().try_wait().unwrap().is_none());
        assert!(
            Instant::now() < deadline,
            "Lease holder did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let locks = fixture.capture();
    let admission = maintenance_busy(locks.try_admit().unwrap());
    let content = File::open(locks.namespace.join("content.lock")).unwrap();
    let identity = file_identity(&content);
    owner.terminate_and_wait();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut admission = admission;
    let lease = loop {
        match admission.try_maintenance() {
            Ok(lease) => break lease,
            Err((retained, error)) => {
                assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                assert!(
                    Instant::now() < deadline,
                    "Exited holder's lock remained busy"
                );
                admission = retained;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    drop(lease);
    assert_eq!(
        file_identity(&File::open(locks.namespace.join("content.lock")).unwrap()),
        identity
    );
    assert!(locks.namespace.join("admission.lock").is_file());
    assert_eq!(inventory(&fixture.installation), adopted);
}

#[test]
#[ignore = "Invoked only by the independent-process installation lease tests"]
fn independent_process_lease_probe() {
    let directory = PathBuf::from(std::env::var_os("FOXY_TEST_INSTALLATION_DIRECTORY").unwrap());
    let registry = PathBuf::from(std::env::var_os("FOXY_TEST_INSTALLATION_REGISTRY").unwrap());
    let locks = InstallationLocks::capture_in(&directory, &registry).unwrap();
    match std::env::var("FOXY_TEST_INSTALLATION_PROBE")
        .unwrap()
        .as_str()
    {
        "admission_busy" => assert_admission_busy(&locks),
        "runtime" => {
            let admission = locks.try_admit().unwrap();
            let runtime = admission.try_runtime().unwrap();
            drop(admission);
            drop(runtime);
        }
        "maintenance_busy" => {
            let admission = maintenance_busy(locks.try_admit().unwrap());
            assert_admission_busy(&locks);
            drop(admission);
        }
        "maintenance" => drop(maintenance(locks.try_admit().unwrap())),
        "hold_runtime" => {
            let admission = locks.try_admit().unwrap();
            let runtime = admission.try_runtime().unwrap();
            drop(admission);
            fs::write(
                PathBuf::from(std::env::var_os("FOXY_TEST_INSTALLATION_READY").unwrap()),
                b"ready",
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(runtime);
        }
        _ => panic!("Unknown installation lease probe"),
    }
}
