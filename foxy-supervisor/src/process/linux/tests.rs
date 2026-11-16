use super::*;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

const FIXTURE: &str = "process::linux::tests::fixture";
const MODE: &str = "FOXY_NAMESPACE_TEST_MODE";
const ROOT: &str = "FOXY_NAMESPACE_TEST_ROOT";
const TIMEOUT: Duration = Duration::from_secs(10);

fn spec(root: &Path, mode: &str) -> CommandSpec {
    CommandSpec {
        executable: std::env::current_exe().unwrap(),
        directory: root.to_path_buf(),
        arguments: ["--exact", FIXTURE, "--ignored", "--nocapture"]
            .map(OsString::from)
            .to_vec(),
        environment: vec![
            (MODE.into(), mode.into()),
            (ROOT.into(), root.as_os_str().to_owned()),
        ],
    }
}

fn wait_for(path: &Path) {
    let until = Instant::now() + TIMEOUT;
    while !path.is_file() {
        assert!(Instant::now() < until, "Fixture did not become ready");
        thread::sleep(Duration::from_millis(5));
    }
}

fn read_pid(root: &Path, name: &str) -> u32 {
    let file = root.join(name);
    wait_for(&file);
    std::fs::read_to_string(file).unwrap().parse().unwrap()
}

struct Observation(OwnedFd);

impl Observation {
    fn open(pid: u32) -> Self {
        Self(owned(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) } as i32).unwrap())
    }

    fn running(&self) -> bool {
        let mut descriptor = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
        assert!(result >= 0, "{}", io::Error::last_os_error());
        result == 0
    }

    fn exited(&self) {
        let until = Instant::now() + TIMEOUT;
        while self.running() {
            assert!(Instant::now() < until, "Retained process did not exit");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn wait(process: &mut Process) -> TreeExit {
    let until = Instant::now() + TIMEOUT;
    loop {
        if let Some(exit) = process.observe().unwrap() {
            return exit;
        }
        assert!(Instant::now() < until, "Namespace did not finish");
        thread::sleep(Duration::from_millis(5));
    }
}

struct FixtureChild(Child);

impl FixtureChild {
    fn spawn(root: &Path, mode: &str) -> Self {
        let spec = spec(root, mode);
        Self(
            Command::new(&spec.executable)
                .args(spec.arguments)
                .current_dir(spec.directory)
                .envs(spec.environment)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}

impl Drop for FixtureChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn nul_paths_arguments_and_invalid_environment_are_rejected_before_namespace_creation() {
    let root = tempfile::tempdir().unwrap();
    for field in 0..5 {
        let mut spec = spec(root.path(), "exit");
        let invalid = OsString::from_vec(vec![b'a', 0, b'b']);
        match field {
            0 => spec.executable = root.path().join(invalid),
            1 => spec.directory = root.path().join(invalid),
            2 => spec.arguments.push(invalid),
            3 => spec.environment.push(("VALUE".into(), invalid)),
            _ => spec.environment.push((invalid, "value".into())),
        }
        assert_eq!(
            input::Prepared::new(&spec).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    for key in ["", "INVALID=KEY"] {
        let mut spec = spec(root.path(), "exit");
        spec.environment.push((key.into(), "value".into()));
        assert_eq!(
            input::Prepared::new(&spec).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn root_owner_is_refused_without_executing_the_fixture() {
    if unsafe { libc::geteuid() } == 0 {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            Process::spawn(&spec(root.path(), "inspect"), None, None)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!root.path().join("root.ready").exists());
        assert!(!support().available);
    }
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_normal_exit_preserves_environment_and_drops_privileges() {
    let root = tempfile::tempdir().unwrap();
    let mut spec = spec(root.path(), "inspect");
    let bytes = vec![b'a', b' ', b'"', b'\\', 0xff];
    spec.environment.push((
        "FOXY_NAMESPACE_TEST_BYTES".into(),
        OsString::from_vec(bytes.clone()),
    ));
    let mut process = Process::spawn(&spec, None, None).unwrap();
    let exit = wait(&mut process);
    assert_eq!(exit.root_exit_code, Some(23));
    assert!(!exit.forced);
    assert_eq!(process.observe().unwrap(), Some(exit));
    assert_eq!(
        std::fs::read(root.path().join("environment")).unwrap(),
        bytes
    );
    let status = std::fs::read_to_string(root.path().join("status")).unwrap();
    for name in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap()
            .trim();
        assert_eq!(u64::from_str_radix(value, 16).unwrap(), 0, "{name}");
    }
    assert_eq!(
        status
            .lines()
            .find_map(|line| line.strip_prefix("NoNewPrivs:"))
            .unwrap()
            .trim(),
        "1"
    );
    let users: Vec<u32> = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    assert!(
        users
            .iter()
            .all(|value| *value == unsafe { libc::getuid() })
    );
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_root_exit_waits_for_setsid_double_fork_descendant() {
    let root = tempfile::tempdir().unwrap();
    let mut process = Process::spawn(&spec(root.path(), "root_exits"), None, None).unwrap();
    let init = Observation::open(process.id());
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    wait_for(&root.path().join("root.exited"));
    thread::sleep(Duration::from_millis(30));
    assert!(init.running());
    assert!(leaf.running());
    assert_eq!(process.observe().unwrap(), None);
    std::fs::write(root.path().join("release"), b"release").unwrap();
    let exit = wait(&mut process);
    assert_eq!(exit.root_exit_code, Some(27));
    assert!(!exit.forced);
    init.exited();
    leaf.exited();
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_force_waits_for_tree_and_unknown_root_status_stays_unknown() {
    let root = tempfile::tempdir().unwrap();
    let mut process = Process::spawn(&spec(root.path(), "tree"), None, None).unwrap();
    let init = Observation::open(process.id());
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    wait_for(&root.path().join("root.ready"));
    assert!(leaf.running());
    process.terminate().unwrap();
    let exit = wait(&mut process);
    assert!(exit.forced);
    assert_eq!(exit.root_exit_code, None);
    init.exited();
    leaf.exited();
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_creator_outlives_spawning_thread_and_unrelated_fds_do_not_leak() {
    let root = tempfile::tempdir().unwrap();
    let sentinel = root.path().join("sentinel");
    let file = std::fs::File::create(&sentinel).unwrap();
    assert_eq!(
        unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );
    let mut spec = spec(root.path(), "fd_probe");
    spec.environment.push((
        "FOXY_NAMESPACE_TEST_SENTINEL".into(),
        sentinel.as_os_str().to_owned(),
    ));
    let mut process = thread::spawn(move || Process::spawn(&spec, None, None).unwrap())
        .join()
        .unwrap();
    wait_for(&root.path().join("root.ready"));
    assert_eq!(process.observe().unwrap(), None);
    assert_eq!(
        std::fs::read(root.path().join("fd_leak")).unwrap(),
        b"false"
    );
    process.terminate().unwrap();
    assert!(wait(&mut process).forced);
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_drop_waits_for_namespace_tree() {
    let root = tempfile::tempdir().unwrap();
    let process = Process::spawn(&spec(root.path(), "tree"), None, None).unwrap();
    let init = Observation::open(process.id());
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    drop(process);
    init.exited();
    leaf.exited();
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_abrupt_controller_death_kills_namespace_descendants() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = FixtureChild::spawn(root.path(), "owner");
    wait_for(&root.path().join("owner.ready"));
    let init = Observation::open(read_pid(root.path(), "init.pid"));
    let leaf = Observation::open(read_pid(root.path(), "leaf.pid"));
    assert!(init.running());
    assert!(leaf.running());
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    init.exited();
    leaf.exited();
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_exec_failure_reports_error_before_returning_process_authority() {
    let root = tempfile::tempdir().unwrap();
    let mut spec = spec(root.path(), "inspect");
    spec.executable = root.path().join("missing-executable");
    assert_eq!(
        Process::spawn(&spec, None, None).err().unwrap().kind(),
        io::ErrorKind::NotFound
    );
    assert!(!root.path().join("root.ready").exists());
}

#[test]
#[ignore = "Requires native unprivileged clone3 user/PID namespace support"]
fn native_closed_stdio_preserves_exec_success_and_failure_reporting() {
    let root = tempfile::tempdir().unwrap();
    let mut owner = FixtureChild::spawn(root.path(), "closed_stdio_owner");
    wait_for(&root.path().join("closed_stdio.result"));
    assert_eq!(
        std::fs::read(root.path().join("closed_stdio.result")).unwrap(),
        b"passed"
    );
    let until = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = owner.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until, "Closed-stdio owner did not finish");
        thread::sleep(Duration::from_millis(5));
    }
}

fn hold(root: &Path) {
    let until = Instant::now() + Duration::from_secs(30);
    while !root.join("release").is_file() && Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
}

fn publish(root: &Path, name: &str) {
    let pid = std::fs::read_link("/proc/self").unwrap();
    let temporary = root.join(format!("{name}.pending"));
    std::fs::write(&temporary, pid.as_os_str().as_encoded_bytes()).unwrap();
    std::fs::rename(temporary, root.join(name)).unwrap();
}

#[test]
#[ignore = "Invoked only by the native namespace parent tests"]
fn fixture() {
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    match std::env::var(MODE).unwrap().as_str() {
        "plain_exit" => std::process::exit(23),
        "inspect" => {
            std::fs::write(
                root.join("status"),
                std::fs::read("/proc/self/status").unwrap(),
            )
            .unwrap();
            std::fs::write(
                root.join("environment"),
                std::env::var_os("FOXY_NAMESPACE_TEST_BYTES")
                    .unwrap()
                    .into_encoded_bytes(),
            )
            .unwrap();
            std::process::exit(23);
        }
        "leaf" => {
            publish(&root, "leaf.pid");
            hold(&root);
        }
        "orphan" => {
            assert!(unsafe { libc::setsid() } >= 0);
            let child = FixtureChild::spawn(&root, "leaf");
            std::mem::forget(child);
            std::process::exit(0);
        }
        mode @ ("tree" | "root_exits") => {
            let child = FixtureChild::spawn(&root, "orphan");
            std::mem::forget(child);
            wait_for(&root.join("leaf.pid"));
            if mode == "root_exits" {
                std::fs::write(root.join("root.exited"), b"ready").unwrap();
                std::process::exit(27);
            }
            std::fs::write(root.join("root.ready"), b"ready").unwrap();
            hold(&root);
        }
        "fd_probe" => {
            let sentinel = PathBuf::from(std::env::var_os("FOXY_NAMESPACE_TEST_SENTINEL").unwrap());
            let leaked = std::fs::read_dir("/proc/self/fd").unwrap().any(|entry| {
                std::fs::read_link(entry.unwrap().path()).is_ok_and(|path| path == sentinel)
            });
            std::fs::write(root.join("fd_leak"), leaked.to_string()).unwrap();
            std::fs::write(root.join("root.ready"), b"ready").unwrap();
            hold(&root);
        }
        "owner" => {
            let mut process = Process::spawn(&spec(&root, "tree"), None, None).unwrap();
            wait_for(&root.join("root.ready"));
            std::fs::write(root.join("init.pid"), process.id().to_string()).unwrap();
            std::fs::write(root.join("owner.ready"), b"ready").unwrap();
            hold(&root);
            process.terminate().unwrap();
            wait(&mut process);
        }
        "closed_stdio_owner" => {
            for descriptor in 0..=2 {
                unsafe { libc::close(descriptor) };
            }
            let mut process = Process::spawn(&spec(&root, "plain_exit"), None, None).unwrap();
            assert_eq!(wait(&mut process).root_exit_code, Some(23));
            let mut missing = spec(&root, "plain_exit");
            missing.executable = root.join("missing-executable");
            assert_eq!(
                Process::spawn(&missing, None, None).err().unwrap().kind(),
                io::ErrorKind::NotFound
            );
            std::fs::write(root.join("closed_stdio.result"), b"passed").unwrap();
        }
        _ => panic!("Unknown namespace fixture mode"),
    }
}
