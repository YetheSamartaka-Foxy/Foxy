use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::CommandSpec;
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};
use serde_json::Value;

pub(super) struct EngineProof {
    pub mission_seen: bool,
    pub content_isolated: bool,
    pub save_requested: bool,
    pub profile_readback: bool,
    pub mission_readback: bool,
    pub runtime_state: PathBuf,
}

struct SessionRun<'a> {
    source: &'a Path,
    id: &'a str,
    generation: u64,
    profiles: &'a Path,
    identity: &'a str,
    hostname: &'a str,
}

pub(super) fn consume(
    native: &Value,
    source: &Path,
    id: &str,
    identity: &str,
    hostname: &str,
    generation: u64,
    _logs: &Path,
) -> io::Result<EngineProof> {
    if native["spawned"] != false {
        return Err(io::Error::other(
            "Launch plan unexpectedly spawned a process",
        ));
    }
    let executable = PathBuf::from(string(&native["executable"], "candidate executable")?);
    let directory = PathBuf::from(string(&native["directory"], "candidate directory")?);
    if executable.starts_with(source) || directory.starts_with(source) {
        return Err(io::Error::other("Launch plan points into adopted source"));
    }
    if !executable.is_file() || !directory.is_dir() {
        return Err(io::Error::other(
            "Active native runtime candidate is missing",
        ));
    }
    let profiles = PathBuf::from(string(&native["profiles"], "private profiles path")?);
    if !profiles.is_absolute() || !profiles.is_dir() {
        return Err(io::Error::other(
            "Native runtime profile directory is unavailable",
        ));
    }
    let runtime_state = profiles
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Native runtime state root is missing"))?
        .to_path_buf();
    let reserved = reserve_port()?;
    let arguments = native["arguments"]
        .as_array()
        .ok_or_else(|| io::Error::other("Missing native launch arguments"))?;
    let mut args = Vec::new();
    for argument in arguments {
        let argument = string(argument, "launch argument")?;
        if !argument.starts_with("-port=") {
            args.push(argument.to_owned());
        }
    }
    args.extend([
        format!("-port={reserved}"),
        "-ip=127.0.0.1".to_owned(),
        "-noSound".to_owned(),
        "-noSplash".to_owned(),
        "-noPause".to_owned(),
        "-world=empty".to_owned(),
    ]);
    let mut environment = Vec::new();
    if let Some(entries) = native["environment"].as_array() {
        for entry in entries {
            let pair = entry
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| io::Error::other("Invalid launch environment entry"))?;
            environment.push((
                OsString::from(string(&pair[0], "environment name")?),
                OsString::from(string(&pair[1], "environment value")?),
            ));
        }
    }
    #[cfg(target_os = "linux")]
    {
        fs::create_dir_all(_logs)?;
        let log = _logs.join(format!("generation-{generation}-{identity}.log"));
        let request = super::console::Request {
            executable: executable.clone(),
            directory: directory.clone(),
            arguments: args,
            log: log.clone(),
        };
        let request_path = _logs.join(format!("request-{generation}-{identity}.json"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&request_path)?;
        serde_json::to_writer(&mut file, &request).map_err(io::Error::other)?;
        file.sync_all()?;
        environment.push((
            OsString::from("FOXY_ARMA_ACTIVATION_CONSOLE_REQUEST"),
            request_path.into_os_string(),
        ));
        run_contained(
            SessionRun {
                source,
                id,
                generation,
                profiles: &runtime_state,
                identity,
                hostname,
            },
            CommandSpec {
                executable: std::env::current_exe()?,
                directory,
                arguments: vec![
                    "native_activation_console_fixture".into(),
                    "--exact".into(),
                    "--ignored".into(),
                    "--test-threads=1".into(),
                ],
                environment,
            },
            &log,
        )
    }
    #[cfg(windows)]
    run_contained(
        SessionRun {
            source,
            id,
            generation,
            profiles: &runtime_state,
            identity,
            hostname,
        },
        CommandSpec {
            executable,
            directory,
            arguments: args.into_iter().map(OsString::from).collect(),
            environment,
        },
        Path::new(""),
    )
}

fn run_contained(
    run: SessionRun<'_>,
    spec: CommandSpec,
    console_log: &Path,
) -> io::Result<EngineProof> {
    let locks = InstallationLocks::capture(run.source)?;
    let admission = locks.try_admit()?;
    let session_identity = SessionIdentity {
        owner_id: "native_activation_acceptance".into(),
        owner_generation: 1,
        game_space_id: "arma3".into(),
        instance_id: run.id.to_owned(),
        operation_id: "consume_active_runtime".into(),
        run_generation: run.generation,
    };
    let previous_reports = report_files(run.profiles)?;
    let mut session = ForegroundSession::spawn(
        session_identity,
        &spec,
        admission,
        &AtomicBool::new(false),
        || Ok(()),
    )?;
    let deadline = Instant::now() + Duration::from_secs(180);
    let proof = loop {
        let report = read_engine_output(run.profiles, console_log, &previous_reports)?;
        if report.contains("ErrorMessage: Cannot open file") {
            break None;
        }
        let proof = super::proof::parse(&report, run.identity, run.hostname);
        if proof.mission_seen && proof.content_isolated && proof.save_requested {
            break Some(proof);
        }
        if session.observe()?.is_some() || Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stopped = session.force_stop(Duration::from_secs(5))?;
    if !stopped.is_some_and(|exit| exit.forced) {
        return Err(io::Error::other(
            "Native engine did not record a forced waited tree exit",
        ));
    }
    std::thread::sleep(Duration::from_millis(1100));
    let proof = proof.ok_or_else(|| {
        io::Error::other("Native engine did not report the exact mission marker before timeout")
    })?;
    eprintln!(
        "Native generation {}: fresh mission and addon markers verified",
        run.generation
    );
    Ok(EngineProof {
        mission_seen: proof.mission_seen,
        content_isolated: proof.content_isolated,
        save_requested: proof.save_requested,
        profile_readback: proof.profile_readback,
        mission_readback: proof.mission_readback,
        runtime_state: run.profiles.to_path_buf(),
    })
}

fn read_engine_output(
    runtime_state: &Path,
    console: &Path,
    previous: &[PathBuf],
) -> io::Result<String> {
    let mut output = String::new();
    if !console.as_os_str().is_empty() {
        output.push_str(&read_bounded(console, 32 * 1024 * 1024)?);
    }
    for report in report_files(runtime_state)? {
        if !previous.contains(&report) {
            output.push_str(&read_bounded(&report, 16 * 1024 * 1024)?);
        }
    }
    if output.len() > 64 * 1024 * 1024 {
        return Err(io::Error::other("Native output exceeds total bound"));
    }
    Ok(output)
}

fn report_files(runtime_state: &Path) -> io::Result<Vec<PathBuf>> {
    let mut reports = Vec::new();
    let mut count = 0;
    let mut stack = vec![runtime_state.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            count += 1;
            if count > 4096 {
                return Err(io::Error::other("Native profile inventory exceeded bound"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native profile contains a link"));
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rpt"))
            {
                reports.push(entry.path());
            }
        }
    }
    reports.sort();
    Ok(reports)
}

fn read_bounded(path: &Path, bound: u64) -> io::Result<String> {
    let mut bytes = Vec::new();
    match fs::File::open(path) {
        Ok(file) => file.take(bound + 1).read_to_end(&mut bytes)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    if bytes.len() as u64 > bound {
        return Err(io::Error::other("Native log exceeds bound"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn reserve_port() -> io::Result<u16> {
    for _ in 0..16 {
        let tcp = TcpListener::bind(("127.0.0.1", 0))?;
        let port = tcp.local_addr()?.port();
        if let Ok(udp) = UdpSocket::bind(("127.0.0.1", port)) {
            drop(udp);
            drop(tcp);
            return Ok(port);
        }
    }
    Err(io::Error::other("Could not reserve a loopback game port"))
}

fn string<'a>(value: &'a Value, name: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .ok_or_else(|| io::Error::other(format!("Missing {name}")))
}
