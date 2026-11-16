use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const GAME_SPACE: &str = "reforger";

const COMMAND_DEADLINE: Duration = Duration::from_secs(360);

pub(super) fn invoke(config: &Path, args: &[&str], expected: i32) -> io::Result<Value> {
    let (code, value) = invoke_raw(config, args)?;
    if code != expected || (expected == 0 && value["ok"] != true) {
        return Err(io::Error::other(format!(
            "Reforger lifecycle CLI returned unexpected exit status {code} for {args:?}: {value}"
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
        .args(["--game-space", GAME_SPACE]);
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
    let out_reader = thread::spawn(move || read_limited(stdout, 2 * 1024 * 1024));
    let err_reader = thread::spawn(move || read_limited(stderr, 64 * 1024));
    let deadline = Instant::now() + COMMAND_DEADLINE;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("Reforger lifecycle CLI exceeded its deadline; args={args:?}"),
                ));
            }
        }
    };
    let stdout = out_reader
        .join()
        .map_err(|_| io::Error::other("CLI stdout reader failed"))??;
    let stderr = err_reader
        .join()
        .map_err(|_| io::Error::other("CLI stderr reader failed"))??;
    if stdout.len() > 2 * 1024 * 1024 || stderr.len() > 64 * 1024 {
        return Err(io::Error::other(
            "Reforger lifecycle CLI output exceeded bound",
        ));
    }
    let value: Value = serde_json::from_slice(&stdout)
        .map_err(|_| io::Error::other("Reforger lifecycle CLI returned invalid JSON"))?;
    std::fs::write(
        config.join("native-last-command.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"args":args,"exit":status.code(),"result":value}),
        )?,
    )?;
    Ok((status.code().unwrap_or(-1), value))
}

pub(super) struct Owner(Child);

impl Owner {
    pub(super) fn finish(mut self) -> io::Result<()> {
        self.0.stdin.take();
        wait_child(&mut self.0, Duration::from_secs(30))?;
        if !self.0.try_wait()?.is_some_and(|status| status.success()) {
            return Err(io::Error::other(
                "Foreground Reforger owner exited unsuccessfully",
            ));
        }
        Ok(())
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
            GAME_SPACE,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Err(io::Error::other(
                "Foreground Reforger owner exited during startup",
            ));
        }
        if invoke(config, &["server", "owner"], 0).is_ok() {
            return Ok(Owner(child));
        }
        thread::sleep(Duration::from_millis(100));
    }
    child.kill()?;
    let _ = child.wait()?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Foreground Reforger owner did not become available",
    ))
}

fn wait_child(child: &mut Child, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    child.kill()?;
    let _ = child.wait()?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Foreground Reforger owner did not exit",
    ))
}

fn read_limited(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
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
