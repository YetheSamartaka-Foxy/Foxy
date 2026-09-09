use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub type Environment = BTreeMap<String, Option<String>>;

pub fn environment(command: &mut Command, values: &Environment) {
    for (key, value) in values {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
}

pub struct ManagedChild {
    pub child: Child,
    #[cfg(windows)]
    _job: Job,
}

impl ManagedChild {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::{io::AsRawHandle, process::CommandExt};
            use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
            let job = Job::new()?;
            command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
            let mut child = command.spawn().context("Could not start child process")?;
            if let Err(error) = job.assign_and_resume(child.as_raw_handle()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self { child, _job: job })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {
                child: command.spawn().context("Could not start child process")?,
            })
        }
    }

    pub fn wait(&mut self, timeout: Duration) -> Result<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                self.child.wait()?;
                bail!("Process timed out after {} seconds", timeout.as_secs());
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    fn new() -> Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let job = Self(handle);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(job)
        }
    }

    fn assign_and_resume(&self, process: windows_sys::Win32::Foundation::HANDLE) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtResumeProcess(process: windows_sys::Win32::Foundation::HANDLE) -> i32;
        }
        unsafe {
            if AssignProcessToJobObject(self.0, process) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let status = NtResumeProcess(process);
            if status < 0 {
                bail!("Could not resume job-owned process: NTSTATUS {status:#x}");
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub fn process(command: &mut Command, timeout: Duration) -> Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut process = ManagedChild::spawn(command)?;
    let mut stdout = process
        .child
        .stdout
        .take()
        .context("Missing child stdout")?;
    let mut stderr = process
        .child
        .stderr
        .take()
        .context("Missing child stderr")?;
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let status = process.wait(timeout);
    // Close the job before joining readers so descendants cannot keep pipes open.
    drop(process);
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

pub fn executable(root: &Path, profile: &str, binary: &str) -> PathBuf {
    root.join("target")
        .join(profile)
        .join(format!("{binary}{}", std::env::consts::EXE_SUFFIX))
}

pub fn build(root: &Path, profile: &str, binary: &str, timeout: Duration) -> Result<PathBuf> {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["build", "-p", binary, "--bin", binary]);
    if profile == "release" {
        command.arg("--release");
    }
    let result = process(&mut command, timeout)?;
    if !result.status.success() {
        bail!("Build failed: {}", String::from_utf8_lossy(&result.stderr));
    }
    let path = executable(root, profile, binary);
    anyhow::ensure!(path.is_file(), "Built executable is missing");
    Ok(path)
}

pub fn foxy(
    exe: &Path,
    config: &Path,
    arguments: &[String],
    env: &Environment,
    timeout: Duration,
) -> Result<Value> {
    let mut command = Command::new(exe);
    command
        .arg("--config-dir")
        .arg(config)
        .arg("--json")
        .args(arguments);
    environment(&mut command, env);
    let result = process(&mut command, timeout)?;
    if !result.status.success() {
        bail!(
            "Foxy exited {}: {} {}",
            result.status,
            String::from_utf8_lossy(&result.stderr),
            String::from_utf8_lossy(&result.stdout)
        );
    }
    serde_json::from_slice(&result.stdout).context("Foxy returned invalid JSON")
}

pub fn start_gui(
    exe: &Path,
    config: &Path,
    run: &Path,
    env: &Environment,
    timeout: Duration,
) -> Result<ManagedChild> {
    let mut command = Command::new(exe);
    command
        .arg("--config-dir")
        .arg(config)
        .args(["ui", "--agent-gui", "--agent-port", "0"])
        .stdout(File::create(run.join("app.out"))?)
        .stderr(File::create(run.join("app.log"))?);
    environment(&mut command, env);
    let mut process = ManagedChild::spawn(&mut command)?;
    let deadline = Instant::now() + timeout.min(Duration::from_secs(120));
    while Instant::now() < deadline {
        anyhow::ensure!(
            process.child.try_wait()?.is_none(),
            "Foxy exited during GUI startup"
        );
        if let Ok(response) = crate::driver::call(
            exe,
            config,
            &["status".into()],
            env,
            Duration::from_secs(10),
        ) && response.data["startup_frame_rendered"].as_bool() == Some(true)
        {
            return Ok(process);
        }
        thread::sleep(Duration::from_millis(500));
    }
    bail!("Foxy agent GUI did not become ready before the startup timeout")
}

pub fn stop_gui(process: &mut ManagedChild, exe: &Path, config: &Path, env: &Environment) {
    if process.child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = crate::driver::call(exe, config, &["close".into()], env, Duration::from_secs(15));
    let _ = process.wait(Duration::from_secs(15));
}

pub fn assert_database_mode(log: &str, expected: &str, gate: u32) -> Result<Value> {
    let re = regex::Regex::new(
        r"STARTUP: Turso journal_mode=(\w+) mvcc_enabled=(true|false) write_gate_permits=(\d+)",
    )?;
    let last = re
        .captures_iter(log)
        .last()
        .context("Could not confirm the effective Turso startup mode")?;
    let actual = last[1].to_ascii_lowercase();
    let enabled = &last[2] == "true";
    let actual_gate: u32 = last[3].parse()?;
    anyhow::ensure!(
        actual == expected && enabled == (expected == "mvcc") && (gate == 0 || gate == actual_gate),
        "Database profile mismatch: expected mode={expected} gate={gate}, got mode={actual} mvcc={enabled} gate={actual_gate}"
    );
    Ok(
        serde_json::json!({"journal_mode":actual,"mvcc_enabled":enabled,"write_gate_permits":actual_gate}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn database_profile_requires_consistent_mode_and_gate() {
        let log = "STARTUP: Turso journal_mode=wal mvcc_enabled=false write_gate_permits=4";
        assert!(assert_database_mode(log, "wal", 0).is_ok());
        assert!(assert_database_mode(log, "wal", 1).is_err());
        assert!(assert_database_mode(log, "mvcc", 4).is_err());
        assert!(assert_database_mode("", "wal", 0).is_err());
    }
}
