use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::GAME;

pub(super) fn invoke(config: &Path, args: &[&str], expected: i32) -> io::Result<Value> {
    let (code, value) = invoke_raw(config, args)?;
    if code != expected {
        return Err(io::Error::other(format!(
            "Lifecycle CLI returned exit {code}, expected {expected}; args={args:?}; JSON={value}"
        )));
    }
    if expected == 0 && value["ok"] != true {
        return Err(io::Error::other(format!(
            "Lifecycle CLI response was not successful; exit={code}, expected={expected}; args={args:?}; JSON={value}",
        )));
    }
    Ok(value)
}

pub(super) fn invoke_raw(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(config)
        .args(["--json"])
        .args(args)
        .args(["--game-space", GAME]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing CLI stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing CLI stderr"))?;
    let out_reader = thread::spawn(move || read_limited(stdout, 1024 * 1024));
    let err_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let (stdout, stderr) = join_readers(out_reader, err_reader);
                return Err(io::Error::other(format!(
                    "Lifecycle CLI wait failed; args={args:?}; error={error}; stdout={}; stderr={}",
                    format_reader(stdout),
                    format_reader(stderr),
                )));
            }
        }
        if Instant::now() >= deadline {
            let kill = child.kill();
            let wait = child.wait();
            let (stdout, stderr) = join_readers(out_reader, err_reader);
            return Err(io::Error::other(format!(
                "Lifecycle CLI exceeded 180-second deadline; args={args:?}; kill={kill:?}; wait={wait:?}; stdout={}; stderr={}",
                format_reader(stdout),
                format_reader(stderr),
            )));
        }
        thread::sleep(Duration::from_millis(20));
    };
    let (stdout, stderr) = join_readers(out_reader, err_reader);
    let stdout = stdout?;
    let stderr = stderr?;
    if stdout.len() > 1024 * 1024 || stderr.len() > 64 * 1024 {
        return Err(io::Error::other("Lifecycle CLI output exceeded bounds"));
    }
    let value: Value = serde_json::from_slice(&stdout).map_err(|error| {
        io::Error::other(format!(
            "Lifecycle CLI returned invalid JSON; exit={}; args={args:?}; error={error}; stdout={}; stderr={}",
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ))
    })?;
    Ok((status.code().unwrap_or(-1), value))
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>, name: &str) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other(format!("CLI {name} reader failed")))?
}

fn join_readers(
    stdout: thread::JoinHandle<io::Result<Vec<u8>>>,
    stderr: thread::JoinHandle<io::Result<Vec<u8>>>,
) -> (io::Result<Vec<u8>>, io::Result<Vec<u8>>) {
    (join_reader(stdout, "stdout"), join_reader(stderr, "stderr"))
}

fn format_reader(output: io::Result<Vec<u8>>) -> String {
    match output {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => format!("<reader error: {error}>"),
    }
}

pub(super) struct Owner(Child);

impl std::ops::Deref for Owner {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Owner {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.stdin.take();
        let _ = wait_child(&mut self.0, Duration::from_secs(30));
    }
}

pub(super) fn start_owner(config: &Path) -> io::Result<Owner> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--config-dir"])
        .arg(config)
        .args([
            "--json",
            "server",
            "supervise",
            "--parent-bound",
            "--game-space",
            GAME,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = Owner(command.spawn()?);
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Err(io::Error::other(
                "Foreground owner exited before becoming available",
            ));
        }
        match invoke(config, &["server", "owner"], 0) {
            Ok(_) => return Ok(child),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    Err(io::Error::other(
        "Foreground owner did not become available before deadline",
    ))
}

pub(super) fn wait_child(child: &mut Child, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    child.kill()?;
    let _ = child.wait()?;
    Err(io::Error::other(
        "Foreground owner did not exit within deadline",
    ))
}

fn read_limited(mut reader: impl io::Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > limit {
            return Err(io::Error::other("Lifecycle CLI output exceeded bound"));
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok(output)
}
