#![cfg(all(target_arch = "x86_64", any(windows, target_os = "linux")))]

use foxy_supervisor::leases::InstallationLocks;
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const OWNER_START_DEADLINE: Duration = Duration::from_secs(30);
const READY_DEADLINE: Duration = Duration::from_secs(180);

#[test]
#[ignore = "Requires an explicitly prepared private native server configuration and runtime"]
fn native_crash_recovery_retires_only_exit_proven_consumer_group() -> io::Result<()> {
    run_native_recovery(false)
}

#[test]
#[ignore = "Prepares real crash receipts and intentionally leaves an idle detached owner for GUI acceptance"]
fn native_prepare_crash_receipts_for_gui_review() -> io::Result<()> {
    run_native_recovery(true)
}

fn run_native_recovery(gui_review: bool) -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_RECOVERY_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_RECOVERY_SPACE")?;
    if !matches!(space.as_str(), "arma3" | "reforger") {
        return Err(io::Error::other("Recovery space must be arma3 or reforger"));
    }
    let game = required_identifier("FOXY_NATIVE_RECOVERY_GAME")?;
    if game != space {
        return Err(io::Error::other(
            "Recovery game must match its Arma 3 or Reforger game space",
        ));
    }
    let instance = required_identifier("FOXY_NATIVE_RECOVERY_INSTANCE")?;
    let output = required_directory("FOXY_NATIVE_RECOVERY_OUTPUT")?;
    let mut evidence = Evidence::new(&output)?;
    let initial_owner = invoke_raw(&config, &["server", "owner", "--game-space", &space])?;
    if initial_owner.0 == 0 {
        return Err(io::Error::other(
            "Refusing to attach to a pre-existing owner",
        ));
    }
    if initial_owner.0 != 4 {
        return Err(io::Error::other(format!(
            "Could not establish an empty owner slot: exit {}",
            initial_owner.0
        )));
    }
    let installation = installation_evidence(&config, &space, &instance)?;

    let mut first_owner = start_parent_bound_owner(&config, &space)?;
    let first_snapshot = wait_owner(&config, &space, &mut first_owner, &mut evidence)?;
    let first_owner_data = owner_data(&first_snapshot)?.clone();
    evidence.push(first_snapshot.clone())?;
    let (start_code, started) = reviewed_start(&config, &space, &instance, false, &mut evidence)?;
    if start_code != 0 || started["ok"] != true {
        return Err(io::Error::other(format!(
            "Reviewed native start failed: {started}"
        )));
    }
    let run = number_string(
        started.pointer("/data/outcome/status/run_generation"),
        "first run generation",
    )?;
    evidence.push(started)?;
    wait_ready(
        &config,
        &space,
        &instance,
        &first_owner_data,
        &run,
        &mut evidence,
    )?;
    if game == "arma3" {
        wait_headless_joined(&config, &space, &instance, &mut evidence)?;
    }
    let first_consumers = current_group_receipts(
        &config,
        &installation,
        &space,
        &instance,
        &first_owner_data,
        false,
    )?;
    let expected_consumers = if game == "arma3" { 3 } else { 1 };
    if first_consumers.len() != expected_consumers {
        return Err(io::Error::other(format!(
            "Expected {expected_consumers} current server/headless consumer receipts, found {}",
            first_consumers.len()
        )));
    }

    let live_plan = recovery_plan(&config, &space, &instance)?;
    assert_live_v2_pending(
        &config,
        &installation,
        &live_plan,
        &game,
        &space,
        &instance,
        &first_owner_data,
    )?;
    let live_bytes = current_group_receipts(
        &config,
        &installation,
        &space,
        &instance,
        &first_owner_data,
        false,
    )?;
    let live_apply = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "recover",
            &instance,
            "--expected-plan-fingerprint",
            string(&live_plan["fingerprint"], "live recovery fingerprint")?,
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    if live_apply.0 == 0 {
        return Err(io::Error::other(
            "Recovery applied while the bound owner and native run were live",
        ));
    }
    evidence.push(live_plan)?;
    evidence.push(live_apply.1)?;
    if current_group_receipts(
        &config,
        &installation,
        &space,
        &instance,
        &first_owner_data,
        false,
    )? != live_bytes
    {
        return Err(io::Error::other(
            "Live recovery refusal changed consumer records",
        ));
    }

    let server_receipt = first_consumers
        .iter()
        .find(|row| row["consumer"]["session"]["instance_id"] == instance)
        .ok_or_else(|| {
            io::Error::other("Current consumer group omitted the server registration")
        })?;
    let locks = InstallationLocks::capture(&installation.directory)?;
    let registry_root = dirs::data_local_dir()
        .ok_or_else(|| io::Error::other("Per-user local data directory unavailable"))?
        .join("Foxy/server-installation-locks");
    let namespace = registry_root.join(locks.coordination_key()?);
    let lock_path = namespace.join("cleanup").join(format!(
        "{}.json",
        string(
            &server_receipt["consumer_fingerprint"],
            "server consumer fingerprint"
        )?
    ));
    let lock_started = Instant::now();
    let mut receipt_lock = ReceiptFileLock::open(&lock_path)?;
    let marker_bytes: Vec<(PathBuf, Vec<u8>)> = first_consumers
        .iter()
        .map(|row| {
            let fingerprint = string(&row["consumer_fingerprint"], "consumer fingerprint")?;
            let path = namespace
                .join("consumers")
                .join(format!("{fingerprint}.json"));
            Ok((path.clone(), fs::read(path)?))
        })
        .collect::<io::Result<_>>()?;
    first_owner.kill_and_wait()?;
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
    check_detached_owner(&replacement, &space)?;
    let mut detached_cleanup = DetachedCleanup {
        config: config.clone(),
        space: space.clone(),
        instance: instance.clone(),
        run: None,
        active: true,
    };
    let replacement_owner = owner_data(&replacement)?.clone();
    evidence.push(replacement.clone())?;

    let locked_plan = recovery_plan(&config, &space, &instance)?;
    let locked_entry = locked_plan["entries"].as_array().and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry["consumer_fingerprint"] == server_receipt["consumer_fingerprint"])
    });
    if locked_plan["eligible_count"] != 0
        || locked_plan["pending_count"].as_u64().unwrap_or(0) == 0
        || locked_entry.is_none_or(|entry| {
            entry["registration_present"] != true
                || entry["eligible"] != false
                || entry["receipt_state"] != "pending_or_unavailable"
        })
    {
        return Err(io::Error::other(format!(
            "Locked pending receipt did not block the exact selected group: {locked_plan}"
        )));
    }
    let locked_apply = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "recover",
            &instance,
            "--expected-plan-fingerprint",
            string(&locked_plan["fingerprint"], "locked recovery fingerprint")?,
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    if locked_apply.0 == 0 {
        return Err(io::Error::other(
            "Replacement owner recovered consumers while the server receipt was locked",
        ));
    }
    if marker_bytes
        .iter()
        .any(|(path, bytes)| fs::read(path).ok().as_ref() != Some(bytes))
    {
        return Err(io::Error::other(
            "Locked pending recovery changed a consumer registration",
        ));
    }
    let held_for = lock_started.elapsed();
    receipt_lock.release()?;
    if held_for > Duration::from_secs(8) {
        return Err(io::Error::other(format!(
            "Pending receipt negative case held the witness lock for {held_for:?}, exceeding the 8 second bound"
        )));
    }
    evidence.push(locked_plan)?;
    evidence.push(locked_apply.1)?;

    let (blocked_code, blocked_start) =
        reviewed_start(&config, &space, &instance, true, &mut evidence)?;
    if blocked_code == 0 || blocked_start["ok"] != false {
        return Err(io::Error::other(
            "A replacement owner started before old consumers were explicitly recovered",
        ));
    }
    evidence.push(blocked_start)?;
    let after_refusal = invoke(&config, &["server", "owner", "--game-space", &space])?;
    if after_refusal.pointer("/data/owner/active_sessions") != Some(&Value::from(0)) {
        return Err(io::Error::other(
            "A pre-spawn consumer refusal created a retained session",
        ));
    }
    evidence.push(after_refusal)?;

    let completed_rows = wait_completed_receipts(
        &config,
        &installation,
        &space,
        &instance,
        &first_owner_data,
        &mut evidence,
    )?;
    if completed_rows.len() != first_consumers.len() {
        return Err(io::Error::other(
            "The exact old server and two-headless receipt group did not complete",
        ));
    }
    let group_fingerprints: Vec<String> = completed_rows
        .iter()
        .map(|row| {
            string(
                &row["consumer_fingerprint"],
                "completed consumer fingerprint",
            )
            .map(str::to_owned)
        })
        .collect::<io::Result<_>>()?;
    let completed = wait_completed_recovery_plan(
        &config,
        &space,
        &game,
        &instance,
        &group_fingerprints,
        &mut evidence,
    )?;
    let expected = string(&completed["fingerprint"], "completed recovery fingerprint")?.to_owned();
    if gui_review {
        verify_installation_evidence(&installation)?;
        evidence.push(
            serde_json::json!({"gui_review_ready":true,"plan":completed,"owner":replacement_owner}),
        )?;
        detached_cleanup.active = false;
        return Ok(());
    }
    let stale = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "recover",
            &instance,
            "--expected-plan-fingerprint",
            &"0".repeat(64),
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    if stale.0 == 0 {
        return Err(io::Error::other("Stale recovery plan was accepted"));
    }
    evidence.push(stale.1)?;
    let applied = invoke(
        &config,
        &[
            "server",
            "instance",
            "recover",
            &instance,
            "--expected-plan-fingerprint",
            &expected,
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    let outcome = &applied["data"]["outcome"];
    if outcome["retired_count"].as_u64() != completed["eligible_count"].as_u64()
        || outcome["remaining_records"] != 0
        || outcome["writes_performed"] != true
        || outcome["cancelled"] != false
        || outcome["inspection_complete"] != true
        || !outcome["error"].is_null()
    {
        return Err(io::Error::other(format!(
            "Recovery did not retire exactly the reviewed group: {outcome}"
        )));
    }
    evidence.push(completed.clone())?;
    evidence.push(applied)?;

    let history = recovery_plan(&config, &space, &instance)?;
    if history["eligible_count"] != 0
        || history["entries"].as_array().is_none_or(|entries| {
            entries
                .iter()
                .filter(|entry| entry["receipt_state"] == "verified_history")
                .count()
                < completed["eligible_count"].as_u64().unwrap_or_default() as usize
        })
    {
        return Err(io::Error::other(format!(
            "Completed recovery history was not retained: {history}"
        )));
    }
    let stale_after = invoke_raw(
        &config,
        &[
            "server",
            "instance",
            "recover",
            &instance,
            "--expected-plan-fingerprint",
            &expected,
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    if stale_after.0 == 0 {
        return Err(io::Error::other(
            "Previously applied recovery plan remained current",
        ));
    }
    evidence.push(history)?;
    evidence.push(stale_after.1)?;

    let (restart_code, restarted) =
        reviewed_start(&config, &space, &instance, true, &mut evidence)?;
    if restart_code != 0 || restarted["ok"] != true {
        return Err(io::Error::other(format!(
            "Restart after recovery failed: {restarted}"
        )));
    }
    let new_run = number_string(
        restarted.pointer("/data/outcome/status/run_generation"),
        "replacement run generation",
    )?;
    detached_cleanup.run = Some(new_run.clone());
    evidence.push(restarted)?;
    wait_ready(
        &config,
        &space,
        &instance,
        &replacement_owner,
        &new_run,
        &mut evidence,
    )?;
    let stopped = invoke(
        &config,
        &[
            "server",
            "instance",
            "stop",
            &instance,
            "--expected-run-generation",
            &new_run,
            "--timeout-ms",
            "30000",
            "--yes",
            "--game-space",
            &space,
        ],
    )?;
    if stopped.pointer("/data/outcome/completed") != Some(&Value::Bool(true))
        || stopped.pointer("/data/outcome/status/cleanup_verified") != Some(&Value::Bool(true))
    {
        return Err(io::Error::other(format!(
            "Replacement run did not stop gracefully: {stopped}"
        )));
    }
    evidence.push(stopped)?;
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
    if shutdown.pointer("/data/released") != Some(&Value::Bool(true)) {
        return Err(io::Error::other(
            "Replacement owner did not shut down after the graceful stop",
        ));
    }
    detached_cleanup.active = false;
    evidence.push(shutdown)?;
    verify_installation_evidence(&installation)?;
    evidence.push(serde_json::json!({"installation_executable_bytes":installation.bytes,"installation_executable_blake3":installation.blake3}))?;
    evidence.finish()?;
    Ok(())
}

fn start_parent_bound_owner(config: &Path, space: &str) -> io::Result<OwnedChild> {
    let mut command = base_command(config);
    command.args([
        "server",
        "supervise",
        "--parent-bound",
        "--game-space",
        space,
    ]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(OwnedChild(Some(command.spawn()?)))
}

fn wait_owner(
    config: &Path,
    space: &str,
    child: &mut OwnedChild,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let deadline = Instant::now() + OWNER_START_DEADLINE;
    loop {
        if child
            .0
            .as_mut()
            .and_then(|process| process.try_wait().ok().flatten())
            .is_some()
        {
            return Err(io::Error::other(
                "Parent-bound owner exited before becoming ready",
            ));
        }
        let (code, value) = invoke_raw(config, &["server", "owner", "--game-space", space])?;
        evidence.push(value.clone())?;
        if code == 0 && value.pointer("/data/owner/database_claimed") == Some(&Value::Bool(true)) {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Parent-bound owner did not become ready",
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn reviewed_start(
    config: &Path,
    space: &str,
    instance: &str,
    detach: bool,
    evidence: &mut Evidence,
) -> io::Result<(i32, Value)> {
    let (plan_code, plan_value) = invoke_raw(
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
    evidence.push(plan_value.clone())?;
    if plan_code != 0 || plan_value["ok"] != true {
        return Ok((plan_code, plan_value));
    }
    let plan = &plan_value["data"]["plan"];
    let generation = number_string(plan.get("activation_generation"), "activation generation")?;
    let fingerprint = string(&plan["fingerprint"], "start plan fingerprint")?;
    let mut args = vec![
        "server",
        "instance",
        "start",
        instance,
        "--expected-activation-generation",
        &generation,
        "--expected-plan-fingerprint",
        fingerprint,
        "--yes",
        "--game-space",
        space,
    ];
    if detach {
        args.push("--detach");
    }
    let (code, value) = invoke_raw(config, &args)?;
    evidence.push(value.clone())?;
    Ok((code, value))
}

fn recovery_plan(config: &Path, space: &str, instance: &str) -> io::Result<Value> {
    invoke(
        config,
        &[
            "server",
            "instance",
            "recovery-plan",
            instance,
            "--game-space",
            space,
        ],
    )
    .map(|value| value["data"]["plan"].clone())
}

fn assert_live_v2_pending(
    config: &Path,
    installation: &InstallationEvidence,
    plan: &Value,
    game: &str,
    space: &str,
    instance: &str,
    owner: &Value,
) -> io::Result<()> {
    let entries = plan["entries"]
        .as_array()
        .ok_or_else(|| io::Error::other("Recovery plan omitted entries"))?;
    let current = current_group_receipts(config, installation, space, instance, owner, false)?;
    if current.is_empty() || plan["eligible_count"] != 0 {
        return Err(io::Error::other(format!(
            "Live consumers were not exclusively pending: {plan}"
        )));
    }
    let current_count = current.len();
    let fingerprints: Vec<&str> = current
        .iter()
        .filter_map(|row| row["consumer_fingerprint"].as_str())
        .collect();
    let relevant_plan = entries.iter().filter(|entry| {
        entry["registration_present"] == true
            && fingerprints.contains(&entry["consumer_fingerprint"].as_str().unwrap_or_default())
    });
    let relevant_plan: Vec<_> = relevant_plan.collect();
    if relevant_plan.len() != current_count
        || relevant_plan
            .iter()
            .any(|entry| entry["receipt_state"] != "pending" || entry["eligible"] != false)
    {
        return Err(io::Error::other(
            "Current server/headless consumer registrations were not strictly pending v2 receipts",
        ));
    }
    if game == "arma3" && current_count != 3 {
        return Err(io::Error::other(
            "Arma 3 pending group omitted the server or a configured headless client",
        ));
    }
    Ok(())
}

fn wait_completed_recovery_plan(
    config: &Path,
    space: &str,
    game: &str,
    instance: &str,
    fingerprints: &[String],
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let plan = recovery_plan(config, space, instance)?;
        let entries = plan["entries"]
            .as_array()
            .ok_or_else(|| io::Error::other("Recovery plan omitted entries"))?;
        let old = entries
            .iter()
            .filter(|entry| {
                fingerprints.contains(
                    &entry["consumer_fingerprint"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect::<Vec<_>>();
        if old.len() == fingerprints.len()
            && !old.is_empty()
            && old.iter().all(|entry| {
                entry["receipt_state"] == "verified_exited" && entry["eligible"] == true
            })
        {
            if old.len() != if game == "arma3" { 3 } else { 1 }
                || plan["eligible_count"].as_u64() != Some(fingerprints.len() as u64)
            {
                return Err(io::Error::other("Recovery included an unverified receipt"));
            }
            evidence.push(plan.clone())?;
            return Ok(plan);
        }
        if Instant::now() >= deadline {
            evidence.push(plan)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bound owner death did not produce completed cleanup receipts",
            ));
        }
        thread::sleep(Duration::from_millis(300));
    }
}

fn wait_ready(
    config: &Path,
    space: &str,
    instance: &str,
    owner: &Value,
    run: &str,
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
        if report["query_ready"] == true {
            if report["status"]["run_generation"]
                .as_u64()
                .map(|value| value.to_string())
                .as_deref()
                != Some(run)
                || report["status"]["owner_id"] != owner["scope"]["owner_id"]
                || report["status"]["process_exists"] != true
                || report["observation_age_ms"]
                    .as_u64()
                    .is_none_or(|age| age > 10_000)
            {
                return Err(io::Error::other(format!(
                    "Fresh monitoring query did not match active run: {report}"
                )));
            }
            evidence.push(value)?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            evidence.push(value)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Native server did not reach fresh query readiness",
            ));
        }
        evidence.push(value)?;
        thread::sleep(Duration::from_millis(500));
    }
}

fn wait_headless_joined(
    config: &Path,
    space: &str,
    instance: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let plan = invoke(
        config,
        &[
            "server",
            "headless",
            "plan",
            instance,
            "--game-space",
            space,
        ],
    )?;
    let expected: Vec<String> = plan["data"]["headless_clients"]
        .as_array()
        .ok_or_else(|| io::Error::other("Headless plan omitted configured clients"))?
        .iter()
        .map(|item| string(&item["id"], "headless client id").map(str::to_owned))
        .collect::<io::Result<_>>()?;
    if expected.len() != 2 {
        return Err(io::Error::other(format!(
            "Prepared Arma runtime must have two headless clients, found {}",
            expected.len()
        )));
    }
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        let value = invoke(
            config,
            &[
                "server",
                "headless",
                "status",
                instance,
                "--game-space",
                space,
            ],
        )?;
        let status = &value["data"];
        let clients = status["clients"]
            .as_array()
            .ok_or_else(|| io::Error::other("Headless status omitted clients"))?;
        if expected.iter().all(|id| {
            clients.iter().any(|client| {
                client["id"] == id.as_str()
                    && client["state"] == "running"
                    && client["connection"] == "joined"
            })
        }) {
            evidence.push(value)?;
            return Ok(());
        }
        evidence.push(value)?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Both prepared Arma headless clients did not reach Running/joined",
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn expected_consumer_ids(
    config: &Path,
    space: &str,
    instance: &str,
    game: &str,
) -> io::Result<Vec<String>> {
    let mut ids = vec![instance.to_owned()];
    if game == "arma3" {
        let plan = invoke(
            config,
            &[
                "server",
                "headless",
                "plan",
                instance,
                "--game-space",
                space,
            ],
        )?;
        let clients = plan["data"]["headless_clients"]
            .as_array()
            .ok_or_else(|| io::Error::other("Headless plan omitted clients"))?;
        for client in clients {
            ids.push(format!(
                "{}-{}",
                instance,
                string(&client["id"], "headless id")?
            ));
        }
    }
    Ok(ids)
}

fn current_group_receipts(
    config: &Path,
    installation: &InstallationEvidence,
    space: &str,
    instance: &str,
    owner: &Value,
    completed: bool,
) -> io::Result<Vec<Value>> {
    let expected_ids = expected_consumer_ids(
        config,
        space,
        instance,
        if space == "arma3" {
            "arma3"
        } else {
            "reforger"
        },
    )?;
    let locks = InstallationLocks::capture(&installation.directory)?;
    let rows = locks.inspect_cleanup()?;
    let selected: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            row.registration_present
                && row
                    .consumer
                    .cleanup
                    .as_ref()
                    .is_some_and(|binding| binding.group_id == instance)
                && row.consumer.session.owner_id
                    == owner["scope"]["owner_id"].as_str().unwrap_or_default()
                && row.consumer.session.owner_generation.to_string()
                    == owner["scope"]["generation"]
                        .as_u64()
                        .unwrap_or_default()
                        .to_string()
        })
        .collect();
    if selected.iter().any(|row| {
        row.consumer.schema_version != 2
            || row.consumer.session.game_space_id != space
            || !expected_ids.contains(&row.consumer.session.instance_id)
            || row
                .consumer
                .cleanup
                .as_ref()
                .is_none_or(|binding| binding.context_key.is_empty())
            || if !completed {
                row.receipt_state != "pending" || row.observation.is_some()
            } else if row.receipt_state == "verified_exited" {
                row.observation.as_ref().is_none_or(|observation| {
                    !observation.owner_exit_observed
                        || !observation.tree_exited
                        || !matches!(
                            observation.backend.as_str(),
                            "windows_job_waited_members" | "linux_pid_namespace"
                        )
                })
            } else {
                !matches!(row.receipt_state, "pending" | "pending_or_unavailable")
            }
    }) {
        return Err(io::Error::other(
            "Current consumer group did not have strict v2 receipt state and matching identity",
        ));
    }
    selected
        .iter()
        .map(|row| serde_json::to_value(row).map_err(io::Error::other))
        .collect()
}

fn wait_completed_receipts(
    config: &Path,
    installation: &InstallationEvidence,
    space: &str,
    instance: &str,
    owner: &Value,
    evidence: &mut Evidence,
) -> io::Result<Vec<Value>> {
    let deadline = Instant::now() + READY_DEADLINE;
    let mut previous = Value::Null;
    loop {
        let current = current_group_receipts(config, installation, space, instance, owner, true);
        match current {
            Ok(rows) if !rows.is_empty() => {
                let value = Value::Array(rows.clone());
                if value != previous {
                    evidence.push(serde_json::json!({"cleanup_receipts":value}))?;
                    previous = value.clone();
                }
                let expected = if space == "arma3" { 3 } else { 1 };
                let all_complete = rows.iter().all(|row| {
                    row["receipt_state"] == "verified_exited"
                        && row["observation"]["owner_exit_observed"] == true
                        && row["observation"]["tree_exited"] == true
                });
                if rows.len() == expected && all_complete {
                    return Ok(rows);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => return Err(error),
            Err(_) => {}
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Bound owner death did not produce complete owner-exit and whole-tree receipts",
            ));
        }
        thread::sleep(Duration::from_millis(300));
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
                    "Parent-bound owner did not exit after kill",
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

struct ReceiptFileLock(Option<File>);
impl ReceiptFileLock {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        Ok(Self(Some(file)))
    }
    fn release(&mut self) -> io::Result<()> {
        if let Some(file) = self.0.take() {
            file.unlock()?;
        }
        Ok(())
    }
}
impl Drop for ReceiptFileLock {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            let _ = file.unlock();
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
        if self.active {
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
}

struct InstallationEvidence {
    directory: PathBuf,
    executable: PathBuf,
    bytes: u64,
    blake3: String,
}
fn installation_evidence(
    config: &Path,
    space: &str,
    instance: &str,
) -> io::Result<InstallationEvidence> {
    let listing = invoke(
        config,
        &["server", "instance", "list", "--game-space", space],
    )?;
    let catalog = &listing["data"]["catalog"];
    let item = catalog["instances"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == instance))
        .ok_or_else(|| io::Error::other("Prepared instance missing from catalog"))?;
    let installation_id = string(&item["installation_id"], "installation id")?;
    let installation = catalog["installations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == installation_id))
        .ok_or_else(|| io::Error::other("Prepared installation missing from catalog"))?;
    let dir = PathBuf::from(string(
        &installation["directory"],
        "installation directory",
    )?)
    .canonicalize()?;
    let executable = dir.join(string(
        &installation["executable"],
        "installation executable",
    )?);
    if executable.canonicalize()? != executable
        || !executable.starts_with(&dir)
        || !fs::symlink_metadata(&executable)?.file_type().is_file()
    {
        return Err(io::Error::other(
            "Adopted executable is not a canonical ordinary file",
        ));
    }
    let (bytes, digest) = hash_file(&executable)?;
    if installation["evidence"]["bytes"].as_u64() != Some(bytes)
        || installation["evidence"]["blake3"] != digest
    {
        return Err(io::Error::other(
            "Catalog executable evidence did not match actual bytes",
        ));
    }
    Ok(InstallationEvidence {
        directory: dir,
        executable,
        bytes,
        blake3: digest,
    })
}
fn verify_installation_evidence(expected: &InstallationEvidence) -> io::Result<()> {
    let (bytes, digest) = hash_file(&expected.executable)?;
    if bytes != expected.bytes || digest != expected.blake3 {
        return Err(io::Error::other(
            "Adopted executable changed during recovery acceptance",
        ));
    }
    Ok(())
}
fn hash_file(path: &Path) -> io::Result<(u64, String)> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hasher = blake3::Hasher::new();
    let count = io::copy(&mut file.take(size.saturating_add(1)), &mut hasher)?;
    if count != size {
        return Err(io::Error::other("Executable changed while hashing"));
    }
    Ok((count, hasher.finalize().to_hex().to_string()))
}

struct Evidence {
    path: PathBuf,
    items: Vec<Value>,
}
impl Evidence {
    fn new(output: &Path) -> io::Result<Self> {
        Ok(Self {
            path: output.join("crash-recovery.json"),
            items: Vec::new(),
        })
    }
    fn push(&mut self, value: Value) -> io::Result<()> {
        self.items.push(value);
        self.persist()
    }
    fn persist(&self) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.items)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(io::Error::other("Recovery evidence exceeded output bound"));
        }
        fs::write(&self.path, bytes)
    }
    fn finish(&self) -> io::Result<()> {
        self.persist()
    }
}
impl Drop for Evidence {
    fn drop(&mut self) {
        let _ = self.persist();
    }
}
fn check_detached_owner(value: &Value, space: &str) -> io::Result<()> {
    let owner = owner_data(value)?;
    if owner["scope"]["space_id"] != space
        || owner["database_claimed"] != true
        || owner["database_opened"] != false
        || owner["lifetime"] != "detached"
    {
        return Err(io::Error::other(
            "Replacement detached owner snapshot was invalid",
        ));
    }
    Ok(())
}
fn owner_data(value: &Value) -> io::Result<&Value> {
    value
        .pointer("/data/owner")
        .filter(|owner| owner.is_object())
        .ok_or_else(|| io::Error::other("Owner snapshot omitted owner data"))
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
    let path = path.canonicalize()?;
    if name.ends_with("_OUTPUT") {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::other(
                    "Recovery output directory must be private to its owner",
                ));
            }
        }
    }
    Ok(path)
}
fn string<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other(format!("Missing {label}")))
}
fn number_string(value: Option<&Value>, label: &str) -> io::Result<String> {
    value
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
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
