//! Stop deadlines retain ownership; only explicit force_stop terminates, and its outcome stays forced.

#![cfg_attr(not(test), allow(dead_code))]

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use foxy_supervisor::leases::Admission;
use foxy_supervisor::process::{CommandSpec, TreeExit};
use foxy_supervisor::session::{ForegroundSession, SessionIdentity, SessionPhase};

#[derive(Debug)]
pub(crate) enum RunError {
    Busy,
    Idle,
    GameExecutable,
    Spawn(#[allow(dead_code)] std::io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RunView {
    Idle,
    Owned {
        instance_id: String,
        phase: SessionPhase,
        pid: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopWait {
    Exited { forced: bool },
    StillOwned,
}

#[derive(Default)]
pub(crate) struct ForegroundRuns {
    active: Option<ActiveRun>,
}

struct ActiveRun {
    session: ForegroundSession,
}

impl ForegroundRuns {
    pub(crate) fn view(&self) -> RunView {
        match &self.active {
            None => RunView::Idle,
            Some(run) => RunView::Owned {
                instance_id: run.session.event().identity.instance_id.clone(),
                phase: run.session.event().phase,
                pid: run.session.process_id(),
            },
        }
    }

    pub(crate) fn start(
        &mut self,
        identity: SessionIdentity,
        spec: &CommandSpec,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
    ) -> Result<u32, RunError> {
        if self.active.is_some() {
            return Err(RunError::Busy);
        }
        if is_game_executable(spec) {
            return Err(RunError::GameExecutable);
        }
        self.spawn(identity, spec, admission, cancelled)
    }

    pub(crate) fn start_adopted(
        &mut self,
        identity: SessionIdentity,
        spec: &CommandSpec,
        expected_executable: &str,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
    ) -> Result<u32, RunError> {
        if !adopted_executable_matches(spec, expected_executable) {
            return Err(RunError::GameExecutable);
        }
        self.spawn(identity, spec, admission, cancelled)
    }

    pub(crate) fn poll(&mut self) -> Result<Option<TreeExit>, RunError> {
        let run = self.active.as_mut().ok_or(RunError::Idle)?;
        match run.session.observe().map_err(RunError::Spawn)? {
            Some(exit) => {
                self.active = None;
                Ok(Some(exit))
            }
            None => Ok(None),
        }
    }

    fn spawn(
        &mut self,
        identity: SessionIdentity,
        spec: &CommandSpec,
        admission: Admission<'_>,
        cancelled: &AtomicBool,
    ) -> Result<u32, RunError> {
        if self.active.is_some() {
            return Err(RunError::Busy);
        }
        let session = ForegroundSession::spawn(identity, spec, admission, cancelled, || Ok(()))
            .map_err(RunError::Spawn)?;
        let pid = session.process_id();
        self.active = Some(ActiveRun { session });
        Ok(pid)
    }

    pub(crate) fn request_stop_and_wait(
        &mut self,
        timeout: Duration,
    ) -> Result<StopWait, RunError> {
        let run = self.active.as_mut().ok_or(RunError::Idle)?;
        run.session.request_stop();
        match run.session.wait(timeout).map_err(RunError::Spawn)? {
            Some(exit) => {
                self.active = None;
                Ok(StopWait::Exited {
                    forced: exit.forced,
                })
            }
            None => Ok(StopWait::StillOwned),
        }
    }

    pub(crate) fn force_stop(&mut self, timeout: Duration) -> Result<TreeExit, RunError> {
        let run = self.active.as_mut().ok_or(RunError::Idle)?;
        let exit = run
            .session
            .force_stop(timeout)
            .map_err(RunError::Spawn)?
            .ok_or(RunError::Idle)?;
        self.active = None;
        Ok(exit)
    }
}

fn is_game_executable(spec: &CommandSpec) -> bool {
    spec.executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower.contains("arma3server") || lower.contains("armareforgerserver")
        })
}

pub(crate) fn adopted_executable_matches(spec: &CommandSpec, expected: &str) -> bool {
    spec.executable.file_name().and_then(|name| name.to_str()) == Some(expected)
        && is_game_executable(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use foxy_supervisor::leases::InstallationLocks;
    #[cfg(windows)]
    use foxy_supervisor::process::support;
    #[cfg(windows)]
    use std::ffi::OsString;
    #[cfg(windows)]
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn a_game_executable_name_is_refused_before_spawn() {
        let spec = CommandSpec {
            executable: PathBuf::from("C:/Games/arma3server_x64.exe"),
            directory: PathBuf::from("C:/Games"),
            arguments: Vec::new(),
            environment: Vec::new(),
        };
        let mut runs = ForegroundRuns::default();
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("installation");
        let registry = root.path().join("registry");
        std::fs::create_dir(&installation).unwrap();
        let locks = InstallationLocks::capture_in(&installation, &registry).unwrap();
        let error = runs
            .start(
                identity(),
                &spec,
                locks.try_admit().unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap_err();
        assert!(matches!(error, RunError::GameExecutable));
        assert!(matches!(runs.view(), RunView::Idle));
        assert!(adopted_executable_matches(&spec, "arma3server_x64.exe"));
        assert!(!adopted_executable_matches(&spec, "ArmaReforgerServer.exe"));
        assert!(matches!(
            runs.start_adopted(
                identity(),
                &spec,
                "ArmaReforgerServer.exe",
                locks.try_admit().unwrap(),
                &AtomicBool::new(false)
            ),
            Err(RunError::GameExecutable)
        ));
        assert!(matches!(runs.poll(), Err(RunError::Idle)));
        assert!(matches!(
            runs.request_stop_and_wait(Duration::ZERO),
            Err(RunError::Idle)
        ));
        assert!(matches!(
            runs.force_stop(Duration::ZERO),
            Err(RunError::Idle)
        ));
    }

    fn identity() -> SessionIdentity {
        SessionIdentity {
            owner_id: "owner_a".into(),
            owner_generation: 1,
            game_space_id: "alpha".into(),
            instance_id: "server_0000000000000000000000000000000a".into(),
            operation_id: "op_1".into(),
            run_generation: 1,
        }
    }

    #[cfg(windows)]
    fn spec(root: &Path) -> CommandSpec {
        CommandSpec {
            executable: std::env::current_exe().unwrap(),
            directory: root.to_path_buf(),
            arguments: [
                "--exact",
                "core::server::runs::tests::hold_fixture",
                "--ignored",
            ]
            .map(OsString::from)
            .to_vec(),
            environment: vec![(
                "FOXY_RUN_FIXTURE_ROOT".into(),
                root.as_os_str().to_os_string(),
            )],
        }
    }

    #[test]
    #[cfg(windows)]
    fn one_contained_nongame_run_rejects_a_second_start_and_a_deadline_does_not_kill() {
        assert!(support().available, "{:?}", support());
        let root = tempfile::tempdir().unwrap();
        let installation = root.path().join("installation");
        let registry = root.path().join("registry");
        std::fs::create_dir(&installation).unwrap();
        let locks = InstallationLocks::capture_in(&installation, &registry).unwrap();
        let mut runs = ForegroundRuns::default();
        let pid = runs
            .start(
                identity(),
                &spec(root.path()),
                locks.try_admit().unwrap(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_ne!(pid, 0);
        wait_for(&root.path().join("ready"));
        assert!(runs.poll().unwrap().is_none());
        let second = runs.start(
            identity(),
            &spec(root.path()),
            locks.try_admit().unwrap(),
            &AtomicBool::new(false),
        );
        assert!(matches!(second, Err(RunError::Busy)));
        assert_eq!(
            runs.request_stop_and_wait(Duration::from_millis(40))
                .unwrap(),
            StopWait::StillOwned
        );
        assert!(root.path().join("ready").is_file());
        let exit = runs.force_stop(Duration::from_secs(10)).unwrap();
        assert!(exit.forced);
        assert!(matches!(runs.view(), RunView::Idle));
    }

    #[cfg(windows)]
    fn wait_for(path: &Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !path.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "Invoked only by the foreground run test"]
    fn hold_fixture() {
        let root = PathBuf::from(std::env::var_os("FOXY_RUN_FIXTURE_ROOT").unwrap());
        std::fs::write(root.join("ready"), b"ready").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !root.join("release").is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
