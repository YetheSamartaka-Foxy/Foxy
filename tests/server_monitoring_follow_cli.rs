#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use foxy_supervisor::process::CommandSpec;
#[cfg(windows)]
use foxy_supervisor::process::OwnedProcess;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
#[cfg(target_os = "linux")]
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    process::{Child, Command, ExitStatus, Stdio},
};

const MAX_STREAM_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STDERR_BYTES: u64 = 64 * 1024;
const MAX_FRAMES: usize = 256;

#[path = "server_reforger_native/lifecycle/transport.rs"]
mod reforger_transport;

#[test]
#[ignore = "requires an already-running private server instance and owner; Windows uses Ctrl+Break"]
fn monitoring_follow_emits_cancelled_ndjson_without_stopping_server() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other("Retained process control is unavailable"));
    }
    let config = required_directory("FOXY_MONITOR_FOLLOW_CONFIG")?;
    let space = required_identifier("FOXY_MONITOR_FOLLOW_SPACE")?;
    let instance = required_identifier("FOXY_MONITOR_FOLLOW_INSTANCE")?;
    let before = monitor_snapshot(&config, &space, &instance)?;
    if before["ok"] != true {
        return Err(io::Error::other(
            "Before monitor snapshot was not successful",
        ));
    }
    let before_status = before
        .pointer("/data/monitoring/status")
        .ok_or_else(|| io::Error::other("Snapshot response omitted monitoring status"))?;
    let identity = run_identity(before_status)?;
    if before_status["process_exists"] != true {
        return Err(io::Error::other("Configured server process is not present"));
    }

    let evidence = tempfile::Builder::new()
        .prefix("foxy-monitor-follow-")
        .tempdir_in(&config)?
        .keep();
    private_directory(&evidence)?;
    fs::write(
        evidence.join("before.json"),
        serde_json::to_vec_pretty(&before)?,
    )?;
    let stream_path = evidence.join("stream.ndjson");
    let stderr_path = evidence.join("stderr.log");
    let output = private_output(&stream_path)?;
    let stderr = private_output(&stderr_path)?;
    let arguments = vec![
        "--quiet".into(),
        "--no-progress".into(),
        "--config-dir".into(),
        config.as_os_str().to_owned(),
        "server".into(),
        "instance".into(),
        "monitor".into(),
        instance.clone().into(),
        "--follow".into(),
        "--stream-json".into(),
        "--interval-ms".into(),
        "250".into(),
        "--game-space".into(),
        space.clone().into(),
    ];
    let spec = CommandSpec {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_Foxy")),
        directory: std::env::current_dir()?,
        arguments,
        environment: Vec::new(),
    };
    let mut child = RetainedCli::spawn(&spec, &output, &stderr)?;
    drop(output);
    drop(stderr);

    let snapshots = wait_for_snapshots(&mut child, &stream_path, &stderr_path, &identity)?;
    let stop_result = child.deliver_soft_stop();
    if let Err(error) = stop_result {
        child.cleanup_failed();
        return Err(io::Error::other(format!(
            "Could not deliver retained soft stop: {error}"
        )));
    }
    let exit = match child.wait(Duration::from_secs(5))? {
        Some(exit) if !exit.forced => exit,
        Some(_) => {
            return Err(io::Error::other(
                "Follow CLI required forced termination after soft stop",
            ));
        }
        None => {
            child.cleanup_failed();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Follow CLI did not exit within five seconds after soft stop",
            ));
        }
    };
    if exit.root_exit_code != Some(0) {
        return Err(io::Error::other(format!(
            "Follow CLI exited unsuccessfully: {exit:?}"
        )));
    }
    let bytes = read_bounded(&stream_path)?;
    let frames = parse_stream(&bytes)?;
    validate_stream(&frames, &identity, snapshots)?;

    let after = monitor_snapshot(&config, &space, &instance)?;
    if after["ok"] != true {
        return Err(io::Error::other(
            "After monitor snapshot was not successful",
        ));
    }
    let after_status = after
        .pointer("/data/monitoring/status")
        .ok_or_else(|| io::Error::other("After snapshot omitted monitoring status"))?;
    if run_identity(after_status)? != identity
        || after_status["process_exists"] != true
        || after_status["process_id"] != before_status["process_id"]
    {
        return Err(io::Error::other(
            "Server owner/run identity or process presence changed during read-only follow",
        ));
    }
    fs::write(
        evidence.join("after.json"),
        serde_json::to_vec_pretty(&after)?,
    )?;
    Ok(())
}

#[test]
#[ignore = "requires an existing private Reforger configuration and native runtime"]
fn monitoring_follow_private_reforger_owner_lifecycle() -> io::Result<()> {
    if !foxy_supervisor::process::support().available {
        return Err(io::Error::other("Retained process control is unavailable"));
    }
    let config = required_directory("FOXY_MONITOR_FOLLOW_CONFIG")?;
    let space = required_identifier("FOXY_MONITOR_FOLLOW_SPACE")?;
    if space != "reforger" {
        return Err(io::Error::other(
            "Private lifecycle wrapper requires FOXY_MONITOR_FOLLOW_SPACE=reforger",
        ));
    }
    let instance = required_identifier("FOXY_MONITOR_FOLLOW_INSTANCE")?;
    let owner = reforger_transport::start_owner(&config)?;
    let plan =
        reforger_transport::invoke(&config, &["server", "instance", "start-plan", &instance], 0)?;
    let activation_generation = plan["data"]["plan"]["activation_generation"]
        .as_u64()
        .filter(|generation| *generation > 0)
        .ok_or_else(|| io::Error::other("Fresh start plan omitted activation generation"))?
        .to_string();
    let fingerprint = plan["data"]["plan"]["fingerprint"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other("Fresh start plan omitted fingerprint"))?;
    let started = reforger_transport::invoke(
        &config,
        &[
            "server",
            "instance",
            "start",
            &instance,
            "--expected-activation-generation",
            &activation_generation,
            "--expected-plan-fingerprint",
            fingerprint,
            "--yes",
        ],
        0,
    )?;
    let run = &started["data"]["outcome"]["status"];
    let run_generation = run["run_generation"]
        .as_u64()
        .filter(|generation| *generation > 0)
        .ok_or_else(|| io::Error::other("Start outcome omitted the current run generation"))?;
    if run["phase"] != "running" || run["process_exists"] != true {
        return Err(io::Error::other(
            "Start did not report a live foreground Reforger process",
        ));
    }

    monitoring_follow_emits_cancelled_ndjson_without_stopping_server()?;

    let stopped = reforger_transport::invoke(
        &config,
        &[
            "server",
            "instance",
            "stop",
            &instance,
            "--expected-run-generation",
            &run_generation.to_string(),
            "--timeout-ms",
            "120000",
            "--yes",
        ],
        0,
    )?;
    let stopped_status = &stopped["data"]["outcome"]["status"];
    if stopped_status["run_generation"] != run_generation
        || stopped_status["process_exists"] != false
        || stopped_status["cleanup_verified"] != true
        || stopped_status["forced"] != false
    {
        return Err(io::Error::other(
            "Separate fixture teardown did not confirm graceful process cleanup",
        ));
    }
    owner.finish()
}

fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} must be set explicitly")))?;
    if !path.is_absolute()
        || !path.is_dir()
        || fs::symlink_metadata(&path)?.file_type().is_symlink()
    {
        return Err(io::Error::other(format!(
            "{name} must be an absolute ordinary directory"
        )));
    }
    path.canonicalize()
}

fn required_identifier(name: &str) -> io::Result<String> {
    let value = std::env::var(name)
        .map_err(|_| io::Error::other(format!("{name} must be set explicitly")))?;
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(io::Error::other(format!(
            "{name} is not a valid identifier"
        )));
    }
    Ok(value)
}

fn monitor_snapshot(config: &Path, space: &str, instance: &str) -> io::Result<Value> {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--json", "--config-dir"])
        .arg(config)
        .args([
            "server",
            "instance",
            "monitor",
            instance,
            "--game-space",
            space,
        ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return Err(io::Error::other(
            "Read-only monitor snapshot command failed",
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(io::Error::other)
}

fn run_identity(status: &Value) -> io::Result<(String, u64, u64)> {
    let owner = status["owner_id"]
        .as_str()
        .ok_or_else(|| io::Error::other("Monitor status omitted owner identity"))?;
    let owner_generation = status["owner_generation"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Monitor status omitted owner generation"))?;
    let run_generation = status["run_generation"]
        .as_u64()
        .ok_or_else(|| io::Error::other("Monitor status omitted run generation"))?;
    Ok((owner.to_owned(), owner_generation, run_generation))
}

fn wait_for_snapshots(
    child: &mut RetainedCli,
    path: &Path,
    stderr_path: &Path,
    identity: &(String, u64, u64),
) -> io::Result<usize> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let bytes = read_bounded(path)?;
        let complete_length = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let frames = parse_stream(&bytes[..complete_length])?;
        let count = frames
            .iter()
            .filter(|frame| frame["type"] == "snapshot")
            .count();
        for frame in frames.iter().filter(|frame| frame["type"] == "snapshot") {
            if run_identity(&frame["monitoring"]["status"])? != *identity {
                return Err(io::Error::other(
                    "Follow snapshot identity differs from current run",
                ));
            }
        }
        if count >= 3 {
            return Ok(count);
        }
        if child.observe()?.is_some() {
            let stderr = read_bounded_limit(stderr_path, MAX_STDERR_BYTES)?;
            return Err(io::Error::other(format!(
                "Follow CLI exited before three snapshots; stderr={}",
                String::from_utf8_lossy(&stderr)
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Three snapshots not observed",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn parse_stream(bytes: &[u8]) -> io::Result<Vec<Value>> {
    if bytes.len() as u64 > MAX_STREAM_BYTES {
        return Err(io::Error::other("Follow output exceeded 4 MiB"));
    }
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err(io::Error::other(
            "Follow output ended with an unframed partial record",
        ));
    }
    let mut frames = Vec::new();
    for line in bytes[..bytes.len().saturating_sub(1)].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            return Err(io::Error::other(
                "Follow output contains an empty NDJSON frame",
            ));
        }
        if frames.len() == MAX_FRAMES {
            return Err(io::Error::other("Follow output exceeded 256 frames"));
        }
        let frame: Value = serde_json::from_slice(line).map_err(io::Error::other)?;
        if frame["type"] != "snapshot" && frame["type"] != "end" && frame["type"] != "error" {
            return Err(io::Error::other(
                "Follow output contains a non-stream envelope",
            ));
        }
        frames.push(frame);
    }
    Ok(frames)
}

fn validate_stream(
    frames: &[Value],
    identity: &(String, u64, u64),
    minimum_snapshots: usize,
) -> io::Result<()> {
    let snapshots: Vec<_> = frames
        .iter()
        .filter(|frame| frame["type"] == "snapshot")
        .collect();
    if snapshots.len() < minimum_snapshots {
        return Err(io::Error::other(
            "Follow stream lost snapshots before cancellation",
        ));
    }
    let mut sequences = HashSet::new();
    for frame in &snapshots {
        if run_identity(&frame["monitoring"]["status"])? != *identity {
            return Err(io::Error::other("Follow stream changed owner/run identity"));
        }
        if let Some(entries) = frame
            .pointer("/monitoring/observation/logs/entries")
            .and_then(Value::as_array)
        {
            for entry in entries {
                let sequence = entry["sequence"]
                    .as_u64()
                    .ok_or_else(|| io::Error::other("Current-run log entry omitted sequence"))?;
                if !sequences.insert(sequence) {
                    return Err(io::Error::other("Current-run log sequence was repeated"));
                }
            }
        }
    }
    let terminals: Vec<_> = frames
        .iter()
        .filter(|frame| frame["type"] == "end")
        .collect();
    if terminals.len() != 1 || terminals[0]["reason"] != "cancelled" {
        return Err(io::Error::other(
            "Follow stream lacks one cancelled terminal frame",
        ));
    }
    if frames.last().is_none_or(|frame| frame["type"] != "end")
        || frames.iter().any(|frame| frame["type"] == "error")
    {
        return Err(io::Error::other(
            "Follow stream ended with an error or trailing frame",
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    read_bounded_limit(path, MAX_STREAM_BYTES)
}

fn read_bounded_limit(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    if length > limit {
        return Err(io::Error::other("Captured CLI output exceeded its bound"));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("Captured CLI output exceeded its bound"));
    }
    Ok(bytes)
}

fn private_output(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct FollowExit {
    root_exit_code: Option<u32>,
    forced: bool,
}

struct RetainedCli {
    #[cfg(windows)]
    process: OwnedProcess,
    #[cfg(target_os = "linux")]
    child: Child,
    #[cfg(target_os = "linux")]
    pidfd: OwnedFd,
    exit: Option<FollowExit>,
}

impl RetainedCli {
    fn spawn(spec: &CommandSpec, output: &File, stderr: &File) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let process = OwnedProcess::spawn_controlled(spec, output)?;
            let _ = stderr;
            Ok(Self {
                process,
                exit: None,
            })
        }
        #[cfg(target_os = "linux")]
        {
            let mut command = Command::new(&spec.executable);
            command
                .current_dir(&spec.directory)
                .args(&spec.arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::from(output.try_clone()?))
                .stderr(Stdio::from(stderr.try_clone()?));
            for (name, value) in &spec.environment {
                command.env(name, value);
            }
            let child = command.spawn()?;
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0u32) as i32 };
            if raw < 0 {
                let error = io::Error::last_os_error();
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            let pidfd = unsafe { OwnedFd::from_raw_fd(raw) };
            Ok(Self {
                child,
                pidfd,
                exit: None,
            })
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (spec, output, stderr);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Retained follow process control is unavailable",
            ))
        }
    }

    fn observe(&mut self) -> io::Result<Option<FollowExit>> {
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        #[cfg(windows)]
        let exit = self.process.observe()?.map(|exit| FollowExit {
            root_exit_code: exit.root_exit_code,
            forced: exit.forced,
        });
        #[cfg(target_os = "linux")]
        let exit = self.child.try_wait()?.map(follow_exit_from_status);
        self.exit = exit;
        Ok(exit)
    }

    fn deliver_soft_stop(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            let helper = foxy_supervisor::process::ConsoleSignalHelper {
                executable: PathBuf::from(env!("CARGO_BIN_EXE_Foxy")),
                arguments: vec![
                    "server".into(),
                    "console-signal".into(),
                    "--ctrl-break".into(),
                ],
            };
            self.process.deliver_soft_stop(Some(&helper))
        }
        #[cfg(target_os = "linux")]
        {
            self.send_signal(libc::SIGINT)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Retained follow soft stop is unavailable",
            ))
        }
    }

    fn wait(&mut self, timeout: Duration) -> io::Result<Option<FollowExit>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(exit) = self.observe()? {
                return Ok(Some(exit));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn cleanup_failed(&mut self) {
        if self.exit.is_some() {
            return;
        }
        #[cfg(windows)]
        {
            let _ = self.process.terminate_and_wait(Duration::from_secs(5));
        }
        #[cfg(target_os = "linux")]
        {
            let _ = self.send_signal(libc::SIGKILL);
            if self.wait(Duration::from_secs(5)).ok().flatten().is_none() {
                let _ = self.child.kill();
                if let Ok(status) = self.child.wait() {
                    self.exit = Some(follow_exit_from_status(status));
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn send_signal(&self, signal: i32) -> io::Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn follow_exit_from_status(status: ExitStatus) -> FollowExit {
    FollowExit {
        root_exit_code: status.code().and_then(|code| u32::try_from(code).ok()),
        forced: false,
    }
}

impl Drop for RetainedCli {
    fn drop(&mut self) {
        self.cleanup_failed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_parser_accepts_snapshot_and_cancelled_end_frames() {
        let bytes = br#"{"type":"snapshot","monitoring":{"status":{"owner_id":"owner_fixture","owner_generation":2,"run_generation":4}}}
{"type":"end","reason":"cancelled"}
"#;
        let frames = parse_stream(bytes).unwrap();
        validate_stream(&frames, &("owner_fixture".into(), 2, 4), 1).unwrap();
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn stream_parser_rejects_outer_success_envelopes_and_duplicate_sequences() {
        assert!(parse_stream(b"{\"ok\":true,\"data\":{}}\n").is_err());
        let entry = r#"{"sequence":7}"#;
        let bytes = format!(
            "{{\"type\":\"snapshot\",\"monitoring\":{{\"status\":{{\"owner_id\":\"owner_fixture\",\"owner_generation\":2,\"run_generation\":4}},\"observation\":{{\"logs\":{{\"entries\":[{entry}]}}}}}}}}\n{{\"type\":\"snapshot\",\"monitoring\":{{\"status\":{{\"owner_id\":\"owner_fixture\",\"owner_generation\":2,\"run_generation\":4}},\"observation\":{{\"logs\":{{\"entries\":[{entry}]}}}}}}}}\n{{\"type\":\"end\",\"reason\":\"cancelled\"}}\n"
        );
        let frames = parse_stream(bytes.as_bytes()).unwrap();
        assert!(validate_stream(&frames, &("owner_fixture".into(), 2, 4), 2).is_err());
    }
}
