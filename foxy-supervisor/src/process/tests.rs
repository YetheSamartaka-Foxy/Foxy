#![cfg(windows)]

use super::{CommandSpec, OwnedProcess, TreeExit};
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use winapi::um::processthreadsapi::OpenProcess;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{WAIT_FAILED, WAIT_OBJECT_0};
use winapi::um::winnt::{PROCESS_QUERY_LIMITED_INFORMATION, SYNCHRONIZE};

const FIXTURE: &str = "process::tests::fixture";
const MODE: &str = "FOXY_SUPERVISOR_TEST_MODE";
const ROOT: &str = "FOXY_SUPERVISOR_TEST_ROOT";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const FIXTURE_LIFETIME: Duration = Duration::from_secs(30);

pub(super) fn spec(root: &Path, mode: &str) -> CommandSpec {
    CommandSpec {
        executable: std::env::current_exe().unwrap(),
        directory: root.to_path_buf(),
        arguments: ["--exact", FIXTURE, "--ignored", "--nocapture"]
            .map(OsString::from)
            .to_vec(),
        environment: vec![
            (MODE.into(), mode.into()),
            (ROOT.into(), root.as_os_str().to_os_string()),
        ],
    }
}

fn supported() {
    let support = super::support();
    assert!(support.available, "{support:?}");
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !path.is_file() {
        assert!(Instant::now() < deadline, "Fixture did not become ready");
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn read_pid(root: &Path, file: &str) -> u32 {
    let path = root.join(file);
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Ok(value) = std::fs::read_to_string(&path)
            && let Ok(pid) = value.parse()
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "Fixture did not publish its pid");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Observation(OwnedHandle);

impl Observation {
    fn open(pid: u32) -> Self {
        let raw = unsafe { OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        assert!(
            !raw.is_null(),
            "Cannot retain process: {}",
            io::Error::last_os_error()
        );
        Self(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
    }

    fn running(&self) -> bool {
        let status = unsafe { WaitForSingleObject(self.0.as_raw_handle().cast(), 0) };
        assert_ne!(status, WAIT_FAILED, "{}", io::Error::last_os_error());
        status != WAIT_OBJECT_0
    }

    fn wait_for_exit(&self) {
        let status = unsafe {
            WaitForSingleObject(
                self.0.as_raw_handle().cast(),
                TEST_TIMEOUT.as_millis() as u32,
            )
        };
        assert_eq!(status, WAIT_OBJECT_0, "Retained process did not exit");
    }
}

struct FixtureChild(Child);

impl FixtureChild {
    fn spawn(root: &Path, mode: &str) -> Self {
        let spec = spec(root, mode);
        let child = Command::new(&spec.executable)
            .current_dir(&spec.directory)
            .args(&spec.arguments)
            .envs(spec.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self(child)
    }
}

impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn assert_invalid(spec: &CommandSpec) {
    match OwnedProcess::spawn(spec) {
        Ok(_) => panic!("Invalid process input was accepted"),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
    }
}

#[test]
fn normal_exit_is_waited_and_cached() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let mut process = OwnedProcess::spawn(&spec(root.path(), "exit")).unwrap();
    assert_ne!(process.id(), 0);
    let expected = Some(TreeExit {
        root_exit_code: Some(23),
        forced: false,
    });
    assert_eq!(process.wait(TEST_TIMEOUT).unwrap(), expected);
    assert_eq!(process.observe().unwrap(), expected);
    assert_eq!(process.wait(Duration::ZERO).unwrap(), expected);
    assert_eq!(process.terminate_and_wait(TEST_TIMEOUT).unwrap(), expected);
}

#[test]
fn exit_code_259_is_a_completed_exit() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let mut process = OwnedProcess::spawn(&spec(root.path(), "exit_259")).unwrap();
    assert_eq!(
        process.wait(TEST_TIMEOUT).unwrap(),
        Some(TreeExit {
            root_exit_code: Some(259),
            forced: false
        }),
    );
}

#[test]
fn wait_timeout_retains_the_live_process() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let mut process = OwnedProcess::spawn(&spec(root.path(), "hold")).unwrap();
    wait_for(&root.path().join("root.ready"));
    let observation = Observation::open(process.id());
    assert_eq!(process.wait(Duration::from_millis(20)).unwrap(), None);
    assert!(observation.running());
    assert_eq!(process.observe().unwrap(), None);
    std::fs::write(root.path().join("release"), b"release").unwrap();
    assert_eq!(
        process.wait(TEST_TIMEOUT).unwrap(),
        Some(TreeExit {
            root_exit_code: Some(0),
            forced: false
        }),
    );
    observation.wait_for_exit();
}

#[test]
fn root_exit_does_not_complete_a_tree_with_a_live_descendant() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let mut process = OwnedProcess::spawn(&spec(root.path(), "root_exits")).unwrap();
    let root_observation = Observation::open(process.id());
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    root_observation.wait_for_exit();
    assert!(leaf.running());
    assert_eq!(process.observe().unwrap(), None);
    assert_eq!(process.wait(Duration::from_millis(20)).unwrap(), None);
    let expected = Some(TreeExit {
        root_exit_code: Some(27),
        forced: true,
    });
    assert_eq!(process.terminate_and_wait(TEST_TIMEOUT).unwrap(), expected);
    leaf.wait_for_exit();
    assert_eq!(process.observe().unwrap(), expected);
}

#[test]
fn explicit_force_waits_for_the_complete_tree() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let mut process = OwnedProcess::spawn(&spec(root.path(), "tree")).unwrap();
    let root_observation = Observation::open(process.id());
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    assert!(root_observation.running());
    assert!(leaf.running());
    let exit = process.terminate_and_wait(TEST_TIMEOUT).unwrap().unwrap();
    assert!(exit.forced);
    assert!(!root_observation.running());
    assert!(!leaf.running());
    assert_eq!(
        process.terminate_and_wait(TEST_TIMEOUT).unwrap(),
        Some(exit)
    );
}

#[test]
fn dropping_the_owner_eventually_contains_the_tree() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let process = OwnedProcess::spawn(&spec(root.path(), "tree")).unwrap();
    let root_observation = Observation::open(process.id());
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    assert!(root_observation.running());
    assert!(leaf.running());
    drop(process);
    root_observation.wait_for_exit();
    leaf.wait_for_exit();
}

#[test]
fn abrupt_owner_death_eventually_contains_the_tree() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let mut owner = FixtureChild::spawn(root.path(), "owner");
    wait_for(&root.path().join("owner.ready"));
    let root_observation = Observation::open(read_pid(root.path(), "root.pid"));
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    assert!(root_observation.running());
    assert!(leaf.running());
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    root_observation.wait_for_exit();
    leaf.wait_for_exit();
}

#[test]
fn relative_process_paths_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut relative_executable = spec(root.path(), "exit");
    relative_executable.executable = PathBuf::from("fixture.exe");
    assert_invalid(&relative_executable);
    let mut relative_directory = spec(root.path(), "exit");
    relative_directory.directory = PathBuf::from("relative");
    assert_invalid(&relative_directory);
}

#[test]
fn nul_inputs_and_invalid_environment_keys_are_rejected() {
    supported();
    let root = tempfile::tempdir().unwrap();
    let nul = OsString::from_wide(&[b'a' as u16, 0, b'b' as u16]);
    let mut argument = spec(root.path(), "exit");
    argument.arguments.push(nul.clone());
    assert_invalid(&argument);
    let mut executable = spec(root.path(), "exit");
    executable.executable = root.path().join(&nul);
    assert_invalid(&executable);
    let mut directory = spec(root.path(), "exit");
    directory.directory = root.path().join(&nul);
    assert_invalid(&directory);
    let mut value = spec(root.path(), "exit");
    value
        .environment
        .push(("FOXY_SUPERVISOR_TEST_VALUE".into(), nul.clone()));
    assert_invalid(&value);
    for key in [OsString::new(), OsString::from("INVALID=KEY"), nul] {
        let mut environment = spec(root.path(), "exit");
        environment.environment.push((key, "value".into()));
        assert_invalid(&environment);
    }
}

fn fixture_hold(root: &Path) {
    let deadline = Instant::now() + FIXTURE_LIFETIME;
    while !root.join("release").is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn publish_pid(root: &Path, name: &str) {
    let path = root.join(name);
    let pending = path.with_extension("pending");
    std::fs::write(&pending, std::process::id().to_string()).unwrap();
    std::fs::rename(pending, path).unwrap();
}

#[test]
#[ignore = "Invoked only by the owned-process subprocess tests"]
fn fixture() {
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    match std::env::var(MODE).unwrap().as_str() {
        "exit" => std::process::exit(23),
        "exit_259" => std::process::exit(259),
        "hold" => {
            std::fs::write(root.join("root.ready"), b"ready").unwrap();
            fixture_hold(&root);
        }
        "leaf" => {
            publish_pid(&root, "leaf.pid");
            fixture_hold(&root);
        }
        mode @ ("tree" | "root_exits") => {
            publish_pid(&root, "root.pid");
            let mut leaf = FixtureChild::spawn(&root, "leaf");
            read_pid(&root, "leaf.pid");
            if mode == "root_exits" {
                std::process::exit(27);
            }
            std::fs::write(root.join("root.ready"), b"ready").unwrap();
            fixture_hold(&root);
            leaf.0.wait().unwrap();
        }
        "owner" => {
            let mut process = OwnedProcess::spawn(&spec(&root, "tree")).unwrap();
            wait_for(&root.join("root.ready"));
            std::fs::write(root.join("owner.ready"), b"ready").unwrap();
            fixture_hold(&root);
            process.terminate_and_wait(TEST_TIMEOUT).unwrap().unwrap();
        }
        _ => panic!("Unknown fixture mode"),
    }
}
