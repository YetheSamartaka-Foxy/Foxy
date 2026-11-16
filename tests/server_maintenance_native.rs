#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const OWNER_START_DEADLINE: Duration = Duration::from_secs(30);
const READY_DEADLINE: Duration = Duration::from_secs(240);
const OCCURRENCE_DEADLINE: Duration = Duration::from_secs(300);

#[test]
#[ignore = "Requires an explicitly prepared private native server configuration and runtime"]
fn native_maintenance_jobs_run_once_and_never_repeat_unknown_effects() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_MAINTENANCE_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_MAINTENANCE_SPACE")?;
    if !matches!(space.as_str(), "arma3" | "reforger") {
        return Err(io::Error::other(
            "Maintenance space must be arma3 or reforger",
        ));
    }
    let instance = required_identifier("FOXY_NATIVE_MAINTENANCE_INSTANCE")?;
    let output = required_directory("FOXY_NATIVE_MAINTENANCE_OUTPUT")?;
    let started_at = Instant::now();
    let mut evidence = Evidence::new(&output)?;
    let mut report = Vec::new();
    let (code, _) = invoke_raw(&config, &["server", "owner", "--game-space", &space])?;
    if code != 4 {
        return Err(io::Error::other(
            "Refusing to run with a pre-existing owner",
        ));
    }
    let executable_before = installation_hash(&config, &space, &instance)?;
    let initial_jobs = jobs(&config, &space, &instance)?;
    if initial_jobs["overview"]["jobs"]
        .as_array()
        .is_some_and(|jobs| !jobs.is_empty())
    {
        return Err(io::Error::other(
            "The prepared instance already has maintenance jobs; use a fresh preparation",
        ));
    }

    let mut owner = start_parent_bound_owner(&config, &space)?;
    wait_owner(&config, &space, &mut owner)?;
    let run_1 = start_and_wait(&config, &space, &instance, &mut evidence)?;

    let mut revision = number(
        &initial_jobs["overview"]["catalog_revision"],
        "catalog revision",
    )?;
    let restart_at = local_in(45);
    let scheduled = add_job(
        &config,
        &space,
        &instance,
        &mut revision,
        &[
            "--name",
            "Scheduled restart",
            "--action",
            "restart",
            "--at",
            &restart_at,
        ],
    )?;
    let backup = add_job(
        &config,
        &space,
        &instance,
        &mut revision,
        &[
            "--name",
            "Backup",
            "--action",
            "backup",
            "--at",
            &restart_at,
        ],
    )?;
    let manual = add_job(
        &config,
        &space,
        &instance,
        &mut revision,
        &[
            "--name",
            "Manual restart",
            "--action",
            "restart",
            "--weekly",
            "daily",
            "--time",
            &far_time_of_day(),
        ],
    )?;
    let queued = add_job(
        &config,
        &space,
        &instance,
        &mut revision,
        &[
            "--name",
            "Queued restart",
            "--action",
            "restart",
            "--weekly",
            "daily",
            "--time",
            &far_time_of_day(),
        ],
    )?;
    let listed = jobs(&config, &space, &instance)?;
    evidence.push(listed.clone())?;
    if listed["overview"]["owner_running"] != true {
        return Err(io::Error::other("List did not see the running owner"));
    }

    let done = wait_terminal(&config, &space, &instance, &scheduled, &mut evidence)?;
    expect_state(&done, "succeeded", None)?;
    let run_2 = number(&done["run_generation_after"], "scheduled restart run")?;
    if run_2 <= run_1 || number(&done["run_generation_before"], "run before")? != run_1 {
        return Err(io::Error::other(format!(
            "Scheduled restart did not replace run {run_1}: {done}"
        )));
    }
    wait_ready(&config, &space, &instance, run_2, &mut evidence)?;
    let blocked = wait_terminal(&config, &space, &instance, &backup, &mut evidence)?;
    expect_state(&blocked, "blocked", Some("adapter_unavailable"))?;
    thread::sleep(Duration::from_secs(8));
    if occurrences_of(&config, &space, &instance, &scheduled)?.len() != 1 {
        return Err(io::Error::other(
            "A one-time scheduled restart was recorded more than once",
        ));
    }
    report.push(format!(
        "scheduled restart run {run_1}->{run_2} once, backup blocked (adapter unavailable)"
    ));

    let first = run_now(&config, &space, &instance, &manual)?;
    if first["data"]["created"] != true {
        return Err(io::Error::other(format!(
            "Manual run was not recorded: {first}"
        )));
    }
    let occurrence = string(&first["data"]["occurrence_id"], "manual occurrence")?.to_owned();
    let repeated = run_now(&config, &space, &instance, &manual)?;
    if repeated["data"]["occurrence_id"] != occurrence.as_str()
        || repeated["data"]["created"] != false
    {
        return Err(io::Error::other(format!(
            "A repeated manual run created a second open occurrence: {repeated}"
        )));
    }
    let queued_run = run_now(&config, &space, &instance, &queued)?;
    let queued_occurrence =
        string(&queued_run["data"]["occurrence_id"], "queued occurrence")?.to_owned();
    let cancel = invoke(
        &config,
        &[
            "--yes",
            "server",
            "maintenance",
            "cancel",
            &instance,
            &queued_occurrence,
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(cancel.clone())?;
    if cancel["data"]["state"] != "cancelled" {
        return Err(io::Error::other(format!(
            "The queued occurrence was not cancelled while another ran: {cancel}"
        )));
    }
    let manual_done = wait_terminal(&config, &space, &instance, &manual, &mut evidence)?;
    expect_state(&manual_done, "succeeded", None)?;
    let run_3 = number(&manual_done["run_generation_after"], "manual run")?;
    wait_ready(&config, &space, &instance, run_3, &mut evidence)?;
    thread::sleep(Duration::from_secs(8));
    let status = invoke(
        &config,
        &[
            "server",
            "instance",
            "status",
            &instance,
            "--game-space",
            &space,
        ],
    )?;
    if number(&status["data"]["status"]["run_generation"], "current run")? != run_3 {
        return Err(io::Error::other(
            "The cancelled occurrence still restarted the server",
        ));
    }
    report.push(format!(
        "manual run deduplicated and restarted to run {run_3}; queued occurrence cancelled without effect"
    ));

    let crash = run_now(&config, &space, &instance, &manual)?;
    let crash_occurrence = string(&crash["data"]["occurrence_id"], "crash occurrence")?.to_owned();
    wait_state(
        &config,
        &space,
        &instance,
        &crash_occurrence,
        "running",
        &mut evidence,
    )?;
    owner.kill_and_wait()?;
    let replacement = invoke(
        &config,
        &[
            "server",
            "supervise",
            "--detach",
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(replacement)?;
    let mut cleanup = DetachedCleanup {
        config: config.clone(),
        space: space.clone(),
        instance: instance.clone(),
        run: None,
        active: true,
    };
    let lost = wait_occurrence(
        &config,
        &space,
        &instance,
        &crash_occurrence,
        &mut evidence,
        |occurrence| occurrence["state"] == "unknown",
    )?;
    expect_state(&lost, "unknown", Some("owner_lost"))?;
    recover_consumers(&config, &space, &instance, &mut evidence)?;
    thread::sleep(Duration::from_secs(8));
    let after_crash = invoke(
        &config,
        &[
            "server",
            "instance",
            "status",
            &instance,
            "--game-space",
            &space,
        ],
    )?;
    if after_crash["data"]["status"]["process_exists"] == true {
        return Err(io::Error::other(
            "The replacement owner re-executed an occurrence whose effect was unknown",
        ));
    }
    report
        .push("owner killed mid-restart: occurrence unknown (owner_lost), not re-executed".into());

    let run_4 = start_and_wait(&config, &space, &instance, &mut evidence)?;
    cleanup.run = Some(run_4.to_string());
    let skip_at = local_in(40);
    let skipped_job = add_job(
        &config,
        &space,
        &instance,
        &mut revision,
        &[
            "--name",
            "Restart after stop",
            "--action",
            "restart",
            "--at",
            &skip_at,
        ],
    )?;
    stop(&config, &space, &instance, run_4, &mut evidence)?;
    cleanup.run = None;
    let skipped = wait_terminal(&config, &space, &instance, &skipped_job, &mut evidence)?;
    expect_state(&skipped, "skipped", Some("server_not_running"))?;
    thread::sleep(Duration::from_secs(8));
    let stopped = invoke(
        &config,
        &[
            "server",
            "instance",
            "status",
            &instance,
            "--game-space",
            &space,
        ],
    )?;
    if stopped["data"]["status"]["process_exists"] == true {
        return Err(io::Error::other(
            "A scheduled restart started a server an operator had stopped",
        ));
    }
    report.push("operator stop before a scheduled restart: skipped, server stayed stopped".into());

    let history = invoke(
        &config,
        &[
            "server",
            "maintenance",
            "history",
            &instance,
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(history.clone())?;
    if history["data"]["unreadable"] != 0 {
        return Err(io::Error::other("History contains unreadable occurrences"));
    }
    for job in [&scheduled, &backup, &manual, &queued, &skipped_job] {
        let expected = revision.to_string();
        invoke(
            &config,
            &[
                "--yes",
                "server",
                "maintenance",
                "remove",
                &instance,
                job,
                "--expected-revision",
                &expected,
                "--game-space",
                &space,
            ],
        )?;
        revision += 1;
    }
    let emptied = jobs(&config, &space, &instance)?;
    if emptied["overview"]["jobs"]
        .as_array()
        .is_none_or(|jobs| !jobs.is_empty())
    {
        return Err(io::Error::other("Removed jobs are still listed"));
    }
    cleanup.active = false;
    let shutdown = invoke(
        &config,
        &[
            "server",
            "supervise",
            "--shutdown",
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    evidence.push(shutdown)?;
    if installation_hash(&config, &space, &instance)? != executable_before {
        return Err(io::Error::other("The adopted executable changed"));
    }
    evidence.finish()?;
    println!(
        "Native maintenance acceptance passed for {space} in {:.1}s: {}",
        started_at.elapsed().as_secs_f64(),
        report.join("; ")
    );
    Ok(())
}

fn local_in(seconds: i64) -> String {
    (chrono::Local::now() + chrono::Duration::seconds(seconds))
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// A daily time twelve hours away, so the recurring job never fires during the run.
fn far_time_of_day() -> String {
    (chrono::Local::now() + chrono::Duration::hours(12))
        .format("%H:%M")
        .to_string()
}

fn add_job(
    config: &Path,
    space: &str,
    instance: &str,
    revision: &mut u64,
    definition: &[&str],
) -> io::Result<String> {
    let expected = revision.to_string();
    let mut args = vec![
        "server",
        "maintenance",
        "add",
        instance,
        "--expected-revision",
        &expected,
        "--game-space",
        space,
    ];
    args.extend_from_slice(definition);
    let added = invoke(config, &args)?;
    *revision += 1;
    Ok(string(&added["data"]["job_id"], "job id")?.to_owned())
}

fn jobs(config: &Path, space: &str, instance: &str) -> io::Result<Value> {
    Ok(invoke(
        config,
        &[
            "server",
            "maintenance",
            "list",
            instance,
            "--game-space",
            space,
        ],
    )?["data"]
        .clone())
}

fn policy_revision(config: &Path, space: &str, instance: &str, job: &str) -> io::Result<String> {
    let listed = jobs(config, space, instance)?;
    listed["overview"]["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|status| status["job"]["id"] == job))
        .and_then(|status| status["job"]["revision"].as_u64())
        .map(|revision| revision.to_string())
        .ok_or_else(|| io::Error::other("Job missing from list"))
}

fn run_now(config: &Path, space: &str, instance: &str, job: &str) -> io::Result<Value> {
    let revision = policy_revision(config, space, instance, job)?;
    invoke(
        config,
        &[
            "--yes",
            "server",
            "maintenance",
            "run",
            instance,
            job,
            "--expected-policy-revision",
            &revision,
            "--game-space",
            space,
        ],
    )
}

fn occurrences_of(config: &Path, space: &str, instance: &str, job: &str) -> io::Result<Vec<Value>> {
    let history = invoke(
        config,
        &[
            "server",
            "maintenance",
            "history",
            instance,
            "--game-space",
            space,
        ],
    )?;
    Ok(history["data"]["occurrences"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["job_id"] == job)
                .cloned()
                .collect()
        })
        .unwrap_or_default())
}

fn wait_terminal(
    config: &Path,
    space: &str,
    instance: &str,
    job: &str,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let deadline = Instant::now() + OCCURRENCE_DEADLINE;
    loop {
        let found = occurrences_of(config, space, instance, job)?;
        if let Some(done) = found.iter().find(|item| {
            matches!(
                item["state"].as_str(),
                Some("succeeded" | "failed" | "skipped" | "blocked" | "cancelled" | "unknown")
            )
        }) {
            evidence.push(done.clone())?;
            return Ok(done.clone());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Job {job} did not finish: {found:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn wait_occurrence(
    config: &Path,
    space: &str,
    instance: &str,
    occurrence: &str,
    evidence: &mut Evidence,
    done: impl Fn(&Value) -> bool,
) -> io::Result<Value> {
    let deadline = Instant::now() + OCCURRENCE_DEADLINE;
    loop {
        let history = invoke(
            config,
            &[
                "server",
                "maintenance",
                "history",
                instance,
                "--game-space",
                space,
            ],
        )?;
        if let Some(item) = history["data"]["occurrences"].as_array().and_then(|items| {
            items
                .iter()
                .find(|item| item["occurrence_id"] == occurrence)
        }) && done(item)
        {
            evidence.push(item.clone())?;
            return Ok(item.clone());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Occurrence {occurrence} did not reach the expected state"),
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn wait_state(
    config: &Path,
    space: &str,
    instance: &str,
    occurrence: &str,
    state: &str,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    wait_occurrence(config, space, instance, occurrence, evidence, |item| {
        item["state"] == state
    })
}

fn expect_state(occurrence: &Value, state: &str, reason: Option<&str>) -> io::Result<()> {
    if occurrence["state"] != state || reason.is_some_and(|reason| occurrence["reason"] != reason) {
        return Err(io::Error::other(format!(
            "Expected {state} {reason:?}: {occurrence}"
        )));
    }
    Ok(())
}

fn start_and_wait(
    config: &Path,
    space: &str,
    instance: &str,
    evidence: &mut Evidence,
) -> io::Result<u64> {
    let plan = invoke(
        config,
        &[
            "server",
            "instance",
            "start-plan",
            instance,
            "--game-space",
            space,
        ],
    )?;
    let generation =
        number(&plan["data"]["plan"]["activation_generation"], "activation")?.to_string();
    let fingerprint = string(&plan["data"]["plan"]["fingerprint"], "fingerprint")?.to_owned();
    let started = invoke(
        config,
        &[
            "server",
            "instance",
            "start",
            instance,
            "--expected-activation-generation",
            &generation,
            "--expected-plan-fingerprint",
            &fingerprint,
            "--yes",
            "--game-space",
            space,
        ],
    )?;
    evidence.push(started.clone())?;
    let run = number(
        &started["data"]["outcome"]["status"]["run_generation"],
        "run",
    )?;
    wait_ready(config, space, instance, run, evidence)?;
    Ok(run)
}

fn wait_ready(
    config: &Path,
    space: &str,
    instance: &str,
    run: u64,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let value = invoke(
            config,
            &[
                "server",
                "instance",
                "monitor",
                instance,
                "--game-space",
                space,
            ],
        )?;
        let report = &value["data"]["monitoring"];
        if report["query_ready"] == true && report["status"]["run_generation"] == run {
            evidence.push(value)?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            evidence.push(value)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Run {run} did not become query-ready"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn stop(
    config: &Path,
    space: &str,
    instance: &str,
    run: u64,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let run = run.to_string();
    let stopped = invoke(
        config,
        &[
            "server",
            "instance",
            "stop",
            instance,
            "--expected-run-generation",
            &run,
            "--timeout-ms",
            "60000",
            "--yes",
            "--game-space",
            space,
        ],
    )?;
    evidence.push(stopped.clone())?;
    if stopped["data"]["outcome"]["completed"] != true {
        return Err(io::Error::other(format!(
            "Graceful stop did not complete: {stopped}"
        )));
    }
    Ok(())
}

fn recover_consumers(
    config: &Path,
    space: &str,
    instance: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let plan = invoke(
            config,
            &[
                "server",
                "instance",
                "recovery-plan",
                instance,
                "--game-space",
                space,
            ],
        )?;
        let plan = plan["data"]["plan"].clone();
        let pending = plan["pending_count"].as_u64().unwrap_or(1);
        let eligible = plan["eligible_count"].as_u64().unwrap_or(0);
        if pending == 0 {
            evidence.push(plan.clone())?;
            if eligible == 0 {
                return Ok(());
            }
            let applied = invoke(
                config,
                &[
                    "server",
                    "instance",
                    "recover",
                    instance,
                    "--expected-plan-fingerprint",
                    string(&plan["fingerprint"], "recovery fingerprint")?,
                    "--yes",
                    "--game-space",
                    space,
                ],
            )?;
            evidence.push(applied.clone())?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            evidence.push(plan)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owner death did not produce completed cleanup receipts",
            ));
        }
        thread::sleep(Duration::from_millis(300));
    }
}

fn installation_hash(config: &Path, space: &str, instance: &str) -> io::Result<String> {
    let listing = invoke(
        config,
        &["server", "instance", "list", "--game-space", space],
    )?;
    let catalog = &listing["data"]["catalog"];
    let item = catalog["instances"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == instance))
        .ok_or_else(|| io::Error::other("Prepared instance missing"))?;
    let installation_id = string(&item["installation_id"], "installation id")?;
    let installation = catalog["installations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == installation_id))
        .ok_or_else(|| io::Error::other("Prepared installation missing"))?;
    let executable = PathBuf::from(string(
        &installation["directory"],
        "installation directory",
    )?)
    .join(string(
        &installation["executable"],
        "installation executable",
    )?);
    let mut file = File::open(&executable)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn start_parent_bound_owner(config: &Path, space: &str) -> io::Result<OwnedChild> {
    let mut command = base_command(config);
    command
        .args([
            "server",
            "supervise",
            "--parent-bound",
            "--game-space",
            space,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(OwnedChild(Some(command.spawn()?)))
}

fn wait_owner(config: &Path, space: &str, child: &mut OwnedChild) -> io::Result<()> {
    let deadline = Instant::now() + OWNER_START_DEADLINE;
    loop {
        if child
            .0
            .as_mut()
            .and_then(|process| process.try_wait().ok().flatten())
            .is_some()
        {
            return Err(io::Error::other("Owner exited before becoming ready"));
        }
        let (code, value) = invoke_raw(config, &["server", "owner", "--game-space", space])?;
        if code == 0 && value.pointer("/data/owner/database_claimed") == Some(&Value::Bool(true)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owner did not become ready",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

struct OwnedChild(Option<Child>);
impl OwnedChild {
    fn kill_and_wait(&mut self) -> io::Result<()> {
        let Some(mut child) = self.0.take() else {
            return Ok(());
        };
        child.kill()?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Owner did not exit after kill",
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct DetachedCleanup {
    config: PathBuf,
    space: String,
    instance: String,
    run: Option<String>,
    active: bool,
}
impl Drop for DetachedCleanup {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(run) = &self.run {
            let _ = invoke_raw(
                &self.config,
                &[
                    "server",
                    "instance",
                    "stop",
                    &self.instance,
                    "--expected-run-generation",
                    run,
                    "--timeout-ms",
                    "30000",
                    "--yes",
                    "--game-space",
                    &self.space,
                ],
            );
        }
        let _ = invoke_raw(
            &self.config,
            &[
                "server",
                "supervise",
                "--shutdown",
                "--yes",
                "--game-space",
                &self.space,
            ],
        );
    }
}

struct Evidence {
    path: PathBuf,
    items: Vec<Value>,
}
impl Evidence {
    fn new(output: &Path) -> io::Result<Self> {
        Ok(Self {
            path: output.join("maintenance.json"),
            items: Vec::new(),
        })
    }
    fn push(&mut self, value: Value) -> io::Result<()> {
        self.items.push(value);
        if self.items.len() > 4000 {
            self.items.drain(..1000);
        }
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        fs::write(&self.path, serde_json::to_vec_pretty(&self.items)?)
    }
}
impl Drop for Evidence {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn required_identifier(name: &str) -> io::Result<String> {
    let value = std::env::var(name).map_err(io::Error::other)?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::other(format!("Invalid {name}")));
    }
    Ok(value)
}

fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(name).ok_or_else(|| io::Error::other(format!("Missing {name}")))?,
    );
    if !path.is_absolute() || !path.is_dir() {
        return Err(io::Error::other(format!(
            "{name} must be an existing absolute directory"
        )));
    }
    path.canonicalize()
}

fn string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

fn number(value: &Value, label: &str) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}

fn invoke(config: &Path, args: &[&str]) -> io::Result<Value> {
    let (code, value) = invoke_raw(config, args)?;
    if code != 0 || value["ok"] != true {
        return Err(io::Error::other(format!(
            "CLI exit {code}, args={args:?}: {value}"
        )));
    }
    Ok(value)
}

fn invoke_raw(config: &Path, args: &[&str]) -> io::Result<(i32, Value)> {
    let mut command = base_command(config);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output()?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        io::Error::other(format!(
            "CLI emitted invalid JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ))
    })?;
    Ok((output.status.code().unwrap_or(-1), value))
}

fn base_command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .args(["--quiet", "--no-progress", "--config-dir"])
        .arg(config)
        .args(["--json"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
