use serde::Serialize;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[derive(Clone, Debug, Serialize)]
pub struct ProcessSupport {
    pub backend: &'static str,
    pub available: bool,
    pub owner_death_policy: &'static str,
    pub reason: Option<&'static str>,
}

pub fn support() -> ProcessSupport {
    #[cfg(windows)]
    {
        let available = windows::owner_is_unelevated();
        ProcessSupport {
            backend: "windows_creation_time_job",
            available,
            owner_death_policy: "forced_tree_termination_save_uncertain",
            reason: (!available).then_some("A verifiably unelevated owner is required."),
        }
    }
    #[cfg(target_os = "linux")]
    {
        linux::support()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        ProcessSupport {
            backend: "unavailable",
            available: false,
            owner_death_policy: "unverified",
            reason: Some(
                "Native process-tree containment and owner-death cleanup are not implemented on this platform.",
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

#[derive(Clone, Debug)]
pub struct ConsoleSignalHelper {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
}

#[derive(Clone, Debug)]
pub struct CleanupWitnessCommand {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub group_id: String,
    pub context_key: String,
}

pub struct CleanupWitnessSpec<'a> {
    pub command: &'a CleanupWitnessCommand,
    pub receipt: &'a std::fs::File,
}

pub fn run_cleanup_witness(owner: u64, tree: u64, receipt: u64, ready: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::run_cleanup_witness(owner, tree, receipt, ready)
    }
    #[cfg(target_os = "linux")]
    {
        linux::run_cleanup_witness(owner, tree, receipt, ready)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (owner, tree, receipt, ready);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Cleanup witnesses are unavailable",
        ))
    }
}

#[cfg(windows)]
pub fn run_console_signal_helper(handle: u64) -> io::Result<i32> {
    windows::run_console_signal_helper(handle, false)
}

#[cfg(windows)]
pub fn run_console_break_helper(handle: u64) -> io::Result<i32> {
    windows::run_console_signal_helper(handle, true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TreeExit {
    pub root_exit_code: Option<u32>,
    pub forced: bool,
}

pub struct OwnedProcess {
    _sealed: (),
    #[cfg(windows)]
    inner: windows::Process,
    #[cfg(target_os = "linux")]
    inner: linux::Process,
}

impl OwnedProcess {
    pub fn spawn(spec: &CommandSpec) -> io::Result<Self> {
        Self::spawn_inner(spec, None, None)
    }

    pub fn spawn_controlled(spec: &CommandSpec, output: &std::fs::File) -> io::Result<Self> {
        Self::spawn_inner(spec, Some(output), None)
    }

    pub fn spawn_controlled_witnessed(
        spec: &CommandSpec,
        output: &std::fs::File,
        witness: &CleanupWitnessSpec<'_>,
    ) -> io::Result<Self> {
        Self::spawn_inner(spec, Some(output), Some(witness))
    }

    fn spawn_inner(
        spec: &CommandSpec,
        output: Option<&std::fs::File>,
        witness: Option<&CleanupWitnessSpec<'_>>,
    ) -> io::Result<Self> {
        if !spec.executable.is_absolute() || !spec.directory.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Process paths must be absolute.",
            ));
        }
        #[cfg(windows)]
        {
            windows::Process::spawn(spec, output, witness).map(|inner| Self { inner, _sealed: () })
        }
        #[cfg(target_os = "linux")]
        {
            linux::Process::spawn(spec, output, witness).map(|inner| Self { inner, _sealed: () })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (output, witness);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Owned process containment is unavailable.",
            ))
        }
    }

    pub fn deliver_soft_stop(&mut self, helper: Option<&ConsoleSignalHelper>) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let _ = helper;
            self.inner.deliver_soft_stop()
        }
        #[cfg(windows)]
        {
            self.inner.deliver_soft_stop(helper)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = helper;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Soft stop is unavailable.",
            ))
        }
    }

    pub fn id(&self) -> u32 {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.inner.id()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            unreachable!("No owned process can be constructed on this platform")
        }
    }

    pub fn observe(&mut self) -> io::Result<Option<TreeExit>> {
        #[cfg(any(windows, target_os = "linux"))]
        {
            self.inner.observe()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Owned process containment is unavailable.",
            ))
        }
    }

    pub fn wait(&mut self, timeout: Duration) -> io::Result<Option<TreeExit>> {
        let start = Instant::now();
        loop {
            if let Some(exit) = self.observe()? {
                return Ok(Some(exit));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            std::thread::sleep(
                Duration::from_millis(5).min(timeout.saturating_sub(start.elapsed())),
            );
        }
    }

    pub fn terminate_and_wait(&mut self, timeout: Duration) -> io::Result<Option<TreeExit>> {
        #[cfg(any(windows, target_os = "linux"))]
        self.inner.terminate()?;
        self.wait(timeout)
    }
}

#[cfg(test)]
mod tests;
