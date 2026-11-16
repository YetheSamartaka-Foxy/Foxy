use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::headless::{self, Prepared, SECRET};
use super::{invoke, lifecycle, number, start, start_plan, status, string, transport};

const TERMINATED_EXIT_CODE: u32 = 240;

pub(in super::super) fn run() -> io::Result<()> {
    let Prepared {
        started_at,
        source,
        source_hash,
        case,
        config,
        id,
        installation,
        identity,
        first,
        second,
        profiles,
    } = headless::prepare(
        "FOXY_ARMA_HEADLESS_LIFECYCLE_SCRATCH",
        [
            &[
                "--name",
                "HC1",
                "--order",
                "0",
                "--limit-fps",
                "50",
                "--max-restarts",
                "2",
                "--join-timeout",
                "180",
            ],
            &[
                "--name",
                "HC2",
                "--order",
                "1",
                "--no-inherit-required",
                "--include",
                "RepoBeta",
                "--start-delay",
                "5",
            ],
        ],
        [&["-limitFPS=50"], &[]],
    )?;
    let hc1 = string(&first["id"], "HC1 id")?.to_owned();
    let hc2 = string(&second["id"], "HC2 id")?.to_owned();
    let hc_roots = [
        PathBuf::from(string(&first["state_root"], "HC1 root")?),
        PathBuf::from(string(&second["state_root"], "HC2 root")?),
    ];
    let hc1_profiles = first["arguments"]
        .as_array()
        .and_then(|arguments| {
            arguments
                .iter()
                .filter_map(Value::as_str)
                .find(|argument| argument.starts_with("-profiles="))
        })
        .ok_or_else(|| io::Error::other("HC1 plan has no profile argument"))?
        .to_owned();
    let mut report = Vec::new();

    let owner = transport::start_owner(&config)?;
    let plan = start_plan(&config, &id)?;
    start(&config, &id, &plan)?;
    let run = number(
        &status(&config, &id)?["data"]["status"]["run_generation"],
        "run generation",
    )?;
    let run_arg = run.to_string();

    let mut waiting_seen = false;
    let ready = wait(&config, &id, "automatic start", 300, |current| {
        if current["query_ready"] == false
            && clients(current).len() == 2
            && clients(current)
                .iter()
                .all(|client| client["state"] == "waiting_for_server" && client["starts"] == 0)
        {
            waiting_seen = true;
        }
        Ok(joined(current, &hc1)? && joined(current, &hc2)?)
    })?;
    if !waiting_seen {
        return Err(io::Error::other(
            "Never observed both clients waiting without a process before query readiness",
        ));
    }
    let ready_ms = number(&ready["ready_unix_ms"], "ready time")?;
    let started_1 = number(&client(&ready, &hc1)?["started_unix_ms"], "HC1 start")?;
    let started_2 = number(&client(&ready, &hc2)?["started_unix_ms"], "HC2 start")?;
    if started_1 < ready_ms || started_2 < ready_ms + 5_000 {
        return Err(io::Error::other(format!(
            "A client started before readiness or its delay: ready {ready_ms}, HC1 {started_1}, HC2 {started_2}"
        )));
    }
    for hc in [&hc1, &hc2] {
        expect(&ready, hc, "starts", 1)?;
    }
    expect_consumers(&config, &installation, &id, &[&hc1, &hc2])?;
    let evidence =
        headless::wait_for_evidence(&profiles, &hc_roots, &identity, Duration::from_secs(240))?;
    report.push(format!(
        "auto start after readiness at {:.1}s (HC2 +{} ms after ready, delay 5 s), {evidence}",
        headless::elapsed(started_at),
        started_2 - ready_ms
    ));

    let stopped = hc_operation(&config, "stop", &id, &run_arg, Some(&hc1))?;
    no_change_beyond(&stopped, "HC1 stop")?;
    let after_stop = headless_status(&config, &id)?;
    let stopped_client = client(&after_stop, &hc1)?;
    if stopped_client["state"] != "stopped"
        || stopped_client["reason"] != "stopped_by_operator"
        || stopped_client["last_exit"]["requested"] != true
        || !joined(&after_stop, &hc2)?
    {
        return Err(io::Error::other(format!(
            "Individual stop did not leave HC1 stopped and HC2 joined: {after_stop}"
        )));
    }
    expect_consumers(&config, &installation, &id, &[&hc2])?;
    std::thread::sleep(Duration::from_secs(15));
    let held = headless_status(&config, &id)?;
    if client(&held, &hc1)?["state"] != "stopped" {
        return Err(io::Error::other(format!(
            "A manually stopped client restarted: {held}"
        )));
    }
    expect(&held, &hc1, "starts", 1)?;
    report.push("HC1 manual stop held for 15 s without restart while HC2 stayed joined".into());

    hc_operation(&config, "start", &id, &run_arg, Some(&hc1))?;
    let restarted_1 = wait(&config, &id, "HC1 explicit start", 240, |current| {
        joined(current, &hc1)
    })?;
    expect(&restarted_1, &hc1, "starts", 2)?;

    let before_restart = number(
        &client(&restarted_1, &hc2)?["started_unix_ms"],
        "HC2 start before restart",
    )?;
    hc_operation(&config, "restart", &id, &run_arg, Some(&hc2))?;
    let restarted_2 = wait(&config, &id, "HC2 individual restart", 240, |current| {
        Ok(joined(current, &hc2)? && client(current, &hc2)?["starts"] == 2)
    })?;
    if number(
        &client(&restarted_2, &hc2)?["started_unix_ms"],
        "HC2 restarted",
    )? <= before_restart
    {
        return Err(io::Error::other("HC2 restart did not start a new process"));
    }
    expect_consumers(&config, &installation, &id, &[&hc1, &hc2])?;
    report.push("HC1 explicit start and HC2 individual restart rejoined".into());

    let mut starts = 2;
    for attempt in 1..=3u64 {
        let current = wait(&config, &id, "HC1 running before crash", 240, |current| {
            Ok(client(current, &hc1)?["state"] == "running")
        })?;
        let pid = number(&client(&current, &hc1)?["process_id"], "HC1 pid")?;
        let killed_at = unix_ms();
        terminate(u32::try_from(pid).unwrap_or(u32::MAX), &case, &hc1_profiles)?;
        if attempt < 3 {
            starts += 1;
            let recovered = wait(&config, &id, "HC1 automatic restart", 240, |current| {
                let entry = client(current, &hc1)?;
                Ok(entry["state"] == "running" && entry["starts"] == starts)
            })?;
            let entry = client(&recovered, &hc1)?;
            let restarted_at = number(&entry["started_unix_ms"], "HC1 restart time")?;
            let minimum = if attempt == 1 { 5_000 } else { 10_000 };
            if entry["automatic_restarts"] != attempt
                || entry["last_exit"]["requested"] != false
                || restarted_at < killed_at + minimum - 500
            {
                return Err(io::Error::other(format!(
                    "Automatic restart {attempt} did not back off as configured: {recovered}"
                )));
            }
            #[cfg(windows)]
            if entry["last_exit"]["exit_code"] != TERMINATED_EXIT_CODE {
                return Err(io::Error::other(format!(
                    "Unexpected exit code after termination: {recovered}"
                )));
            }
            report.push(format!(
                "unexpected exit {attempt} restarted after {} ms",
                restarted_at - killed_at
            ));
        } else {
            let suspended = wait(&config, &id, "HC1 suspension", 60, |current| {
                let entry = client(current, &hc1)?;
                Ok(entry["state"] == "restart_suspended")
            })?;
            if client(&suspended, &hc1)?["reason"] != "restart_budget_exhausted" {
                return Err(io::Error::other(format!(
                    "Suspension reason is wrong: {suspended}"
                )));
            }
            std::thread::sleep(Duration::from_secs(30));
            let held = headless_status(&config, &id)?;
            let entry = client(&held, &hc1)?;
            if entry["state"] != "restart_suspended"
                || entry["starts"] != starts
                || !entry["process_id"].is_null()
                || !joined(&held, &hc2)?
            {
                return Err(io::Error::other(format!(
                    "The restart budget did not stop the loop: {held}"
                )));
            }
            expect_consumers(&config, &installation, &id, &[&hc2])?;
            report.push("third exit suspended automatic restarts; nothing started for 30 s".into());
        }
    }

    let restarted = lifecycle(&config, "restart", &id, run, 60_000, false)?;
    let outcome = &restarted["data"]["outcome"];
    let next_run = number(&outcome["status"]["run_generation"], "next run")?;
    if outcome["completed"] != true || next_run <= run {
        return Err(io::Error::other(format!(
            "Server restart did not complete: {restarted}"
        )));
    }
    let rejoined = wait(
        &config,
        &id,
        "reconnect after server restart",
        300,
        |current| {
            Ok(number(&current["run_generation"], "run")? == next_run
                && joined(current, &hc1)?
                && joined(current, &hc2)?)
        },
    )?;
    for hc in [&hc1, &hc2] {
        expect(&rejoined, hc, "starts", 1)?;
        expect(&rejoined, hc, "automatic_restarts", 0)?;
    }
    expect_consumers(&config, &installation, &id, &[&hc1, &hc2])?;
    report.push(format!(
        "server restart to run {next_run} reconnected both clients once each"
    ));

    let stopped = lifecycle(&config, "stop", &id, next_run, 60_000, false)?;
    let outcome = &stopped["data"]["outcome"];
    if outcome["completed"] != true
        || outcome["status"]["cleanup_verified"] != true
        || outcome["status"]["forced"] != false
        || outcome["status"].get("headless_running").is_some()
    {
        return Err(io::Error::other(format!(
            "Graceful server stop did not clean up: {stopped}"
        )));
    }
    expect_consumers(&config, &installation, &id, &[])?;
    let runtime_state = profiles
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Server profiles have no runtime-state root"))?;
    for file in headless::logs(&profiles)?
        .into_iter()
        .chain(headless::logs(runtime_state)?)
        .chain(hc_roots.iter().flat_map(|root| {
            headless::logs(&root.join("home/profiles"))
                .unwrap_or_default()
                .into_iter()
                .chain(headless::logs(root).unwrap_or_default())
        }))
    {
        if String::from_utf8_lossy(&fs::read(&file)?).contains(SECRET) {
            return Err(io::Error::other(format!(
                "Join password leaked into {}",
                file.display()
            )));
        }
    }
    drop(owner);
    if super::inventory::tree_hash(&source)? != source_hash {
        return Err(io::Error::other("Native source changed during acceptance"));
    }
    println!(
        "Native headless lifecycle acceptance passed for case {} in {:.1}s: {}",
        case.display(),
        headless::elapsed(started_at),
        report.join("; ")
    );
    Ok(())
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

/// An observation can be briefly unavailable while the owner replaces its snapshot; that is not a
/// lifecycle result, so a bounded retry reads the next one.
fn headless_status(config: &Path, id: &str) -> io::Result<Value> {
    let mut attempts = 0;
    loop {
        let value = invoke(
            config,
            &["server", "headless", "status", id],
            "headless status",
        )?;
        headless::no_secret(&value, "headless status")?;
        attempts += 1;
        if value["data"]["observation_unavailable"] != true || attempts >= 5 {
            return Ok(value["data"].clone());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn clients(status: &Value) -> Vec<Value> {
    status["clients"].as_array().cloned().unwrap_or_default()
}

fn client<'a>(status: &'a Value, id: &str) -> io::Result<&'a Value> {
    status["clients"]
        .as_array()
        .and_then(|clients| clients.iter().find(|client| client["id"] == id))
        .ok_or_else(|| io::Error::other(format!("Headless status has no client {id}: {status}")))
}

fn joined(status: &Value, id: &str) -> io::Result<bool> {
    let entry = client(status, id)?;
    Ok(entry["state"] == "running" && entry["connection"] == "joined")
}

fn expect(status: &Value, id: &str, field: &str, value: u64) -> io::Result<()> {
    if client(status, id)?[field] != value {
        return Err(io::Error::other(format!(
            "Expected {field} {value} for {id}: {status}"
        )));
    }
    Ok(())
}

fn wait(
    config: &Path,
    id: &str,
    label: &str,
    seconds: u64,
    mut done: impl FnMut(&Value) -> io::Result<bool>,
) -> io::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let current = headless_status(config, id)?;
        if done(&current)? {
            return Ok(current);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!("{label} timed out: {current}")));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn hc_operation(
    config: &Path,
    operation: &str,
    id: &str,
    run: &str,
    client: Option<&str>,
) -> io::Result<Value> {
    let mut args = vec![
        "--yes",
        "server",
        "headless",
        operation,
        id,
        "--expected-run-generation",
        run,
        "--timeout-ms",
        "60000",
    ];
    if operation == "start" {
        args.truncate(7);
    }
    if let Some(client) = client {
        args.extend(["--client", client]);
    }
    let value = invoke(config, &args, operation)?;
    headless::no_secret(&value, operation)?;
    if value["data"]["outcome"]["completed"] != true {
        return Err(io::Error::other(format!(
            "Headless {operation} did not complete: {value}"
        )));
    }
    Ok(value)
}

fn no_change_beyond(value: &Value, label: &str) -> io::Result<()> {
    if value["data"]["outcome"]["status"]["phase"] != "running" {
        return Err(io::Error::other(format!(
            "{label} changed the server run: {value}"
        )));
    }
    Ok(())
}

fn expect_consumers(
    config: &Path,
    installation: &str,
    id: &str,
    clients: &[&str],
) -> io::Result<()> {
    let consumers = invoke(
        config,
        &["server", "installation", "consumers", installation],
        "inspect consumers",
    )?;
    let mut registered: Vec<String> =
        consumers["data"]["consumer_inspection"]["recorded_consumers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|record| record["session"]["instance_id"].as_str().map(str::to_owned))
            .collect();
    registered.sort();
    let mut expected: Vec<String> = if clients.is_empty() {
        Vec::new()
    } else {
        std::iter::once(id.to_owned())
            .chain(clients.iter().map(|client| format!("{id}-{client}")))
            .collect()
    };
    expected.sort();
    if registered != expected {
        return Err(io::Error::other(format!(
            "Unexpected consumer registrations {registered:?}, expected {expected:?}"
        )));
    }
    Ok(())
}

/// Simulates an engine crash of one owner-reported client after confirming the process is that
/// client's engine inside this case, never a process found by name.
#[cfg(windows)]
fn terminate(pid: u32, case: &Path, _profiles: &str) -> io::Result<()> {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{OpenProcess, TerminateProcess};
    use winapi::um::winbase::QueryFullProcessImageNameW;
    use winapi::um::winnt::{PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u16; 4096];
    let mut size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let named =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) } != 0;
    let image = String::from_utf16_lossy(&buffer[..size as usize]).to_ascii_lowercase();
    let root = case
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_ascii_lowercase();
    if !named || !image.starts_with(&root) || !image.ends_with("arma3server_x64.exe") {
        unsafe { CloseHandle(handle) };
        return Err(io::Error::other(
            "The reported client process is not this case's engine",
        ));
    }
    let terminated = unsafe { TerminateProcess(handle, TERMINATED_EXIT_CODE) } != 0;
    unsafe { CloseHandle(handle) };
    if terminated {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// On Linux the owner reports the namespace init; the engine is its child with this client's
/// own profile argument.
#[cfg(unix)]
fn terminate(pid: u32, _case: &Path, profiles: &str) -> io::Result<()> {
    let _ = TERMINATED_EXIT_CODE;
    for entry in fs::read_dir("/proc")? {
        let name = entry?.file_name();
        let Ok(child) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{child}/stat")) else {
            continue;
        };
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u32>().ok());
        if parent != Some(pid) {
            continue;
        }
        let Ok(arguments) = fs::read(format!("/proc/{child}/cmdline")) else {
            continue;
        };
        if arguments
            .split(|byte| *byte == 0)
            .any(|argument| argument == profiles.as_bytes())
        {
            if unsafe { libc::kill(child, libc::SIGKILL) } != 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
    }
    Err(io::Error::other(
        "No engine child of the reported client process carries its profile argument",
    ))
}
