use super::*;
use serde_json::json;

const TRANSITION_DEADLINE: Duration = Duration::from_secs(180);
const SYNTHETIC_IDENTITY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE=";

#[test]
#[ignore = "Requires an explicitly prepared private native Reforger server configuration and runtime"]
fn native_reforger_rcon_catalog_and_restart_shutdown() -> io::Result<()> {
    let config = required_directory("FOXY_NATIVE_RCON_CONFIG")?;
    let space = required_identifier("FOXY_NATIVE_RCON_SPACE")?;
    if space != "reforger" {
        return Err(io::Error::other(
            "This fixture requires the reforger game space",
        ));
    }
    let case = Case {
        config,
        space,
        instance: required_identifier("FOXY_NATIVE_RCON_INSTANCE")?,
        port: std::env::var("FOXY_NATIVE_RCON_PORT")
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?,
        password: format!("fx{:024x}", rand::random::<u128>() >> 32),
    };
    let output = required_directory("FOXY_NATIVE_RCON_OUTPUT")?;
    let executable_before = installation_hash(&case)?;
    let mut evidence = Evidence::new(&output, &case.password);
    let mut report = Vec::new();

    let (owner_code, owner_state) = case.raw(&["server", "owner"], None)?;
    evidence.push(owner_state)?;
    if owner_code != 4 {
        return Err(io::Error::other(
            "Refusing to run with a pre-existing owner",
        ));
    }
    let shown = case.ok(&["server", "instance", "show", &case.instance])?;
    evidence.push(shown.clone())?;
    let settings = &shown["data"]["instance"]["definition"]["settings"];
    if settings["rcon"]["port"] != case.port || settings["rcon"]["permission"] != "admin" {
        return Err(io::Error::other(
            "Prepared runtime must have the supplied RCon port and admin permission",
        ));
    }
    let scenario = string(&settings["scenario_id"], "committed Reforger scenario")?.to_owned();
    let plan = case.ok(&["server", "instance", "start-plan", &case.instance])?;
    evidence.push(plan.clone())?;
    if plan["data"]["plan"]["launch"]["rcon"]["endpoint"] != format!("127.0.0.1:{}", case.port)
        || plan["data"]["plan"]["launch"]["reforger_scenario"] != scenario
    {
        return Err(io::Error::other(
            "Prepared start plan does not bind the requested RCon endpoint and committed scenario",
        ));
    }

    let mut owner = start_parent_bound_owner(&case)?;
    wait_owner(&case, &mut owner)?;
    let owner_snapshot = case.ok(&["server", "owner"])?;
    evidence.push(owner_snapshot.clone())?;
    let owner_id = string(
        &owner_snapshot["data"]["owner"]["scope"]["owner_id"],
        "native owner id",
    )?
    .to_owned();
    validate_owner_log_component(&owner_id)?;
    let mut cleanup = Cleanup {
        case: &case,
        run: None,
        active: true,
    };
    let mut owned_jobs = AdminJobCleanup::new(&case);
    recover_leftovers(&case, &mut evidence)?;
    let mut run = start_and_wait(&case, &mut evidence)?;
    cleanup.run = Some(run);
    wait_session(&case, run, 1, &mut evidence)?;
    let initial_monitor = case.ok(&["server", "instance", "monitor", &case.instance])?;
    evidence.push(initial_monitor.clone())?;
    let initial_status = &initial_monitor["data"]["monitoring"]["status"];
    let pid = number(&initial_status["process_id"], "initial Reforger process id")?;
    if initial_status["run_generation"] != run {
        return Err(io::Error::other(
            "Initial monitoring did not match the started run",
        ));
    }
    let before_players = fresh_session(&case, run)?;
    let (players_code, players) = typed_admin(&case, "players", run, &[], false)?;
    evidence.push(json!({"typed_players": {"exit_code": players_code, "result": players}}))?;
    if players_code != 5
        || players["data"]["parsed"]["complete"] != false
        || !list_record_matches(
            &players,
            "#players",
            run,
            number(
                &before_players["session_generation"],
                "players session generation",
            )? as u32,
        )
        || !record_transcript(&players["data"]["record"]).contains("Players on server:")
    {
        return Err(io::Error::other(format!(
            "Typed players did not retain a fresh incomplete native listing transcript: {players}"
        )));
    }

    let before_bans = fresh_session(&case, run)?;
    let (bans_code, bans) = typed_admin(&case, "bans", run, &["--page", "1"], false)?;
    evidence.push(json!({"typed_bans_page_one": {"exit_code": bans_code, "result": bans}}))?;
    let ban_record_ok = list_record_matches(
        &bans,
        "#ban list 1",
        run,
        number(
            &before_bans["session_generation"],
            "bans session generation",
        )? as u32,
    );
    let ban_transcript = record_transcript(&bans["data"]["record"]);
    let explicit_empty_bans = ban_transcript
        .lines()
        .any(|line| line.trim() == "Server has no bans to list.");
    let meaningful_incomplete_page = bans["data"]["parsed"]["complete"] != true
        && bans["data"]["parsed"]["bans"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty());
    if !matches!(bans_code, 0 | 5)
        || !ban_record_ok
        || !(explicit_empty_bans && bans["data"]["parsed"]["complete"] == true
            || meaningful_incomplete_page)
    {
        return Err(io::Error::other(format!(
            "Ban page one was neither an explicit empty-list response nor a meaningful incomplete page: {bans}"
        )));
    }

    let before_synthetic_ban = fresh_session(&case, run)?;
    let (ban_code, synthetic_ban) = typed_admin(
        &case,
        "ban",
        run,
        &[
            "--identity",
            SYNTHETIC_IDENTITY,
            "--duration-seconds",
            "60",
            "--reason",
            "Foxy synthetic native fixture",
            "--expected-run-generation",
            &run.to_string(),
            "--yes",
        ],
        false,
    )?;
    evidence.push(
        json!({"synthetic_native_ban_rejection": {"exit_code": ban_code, "result": synthetic_ban}}),
    )?;
    if ban_code != 4
        || synthetic_ban["data"]["state"] != "replied"
        || !record_matches(
            &synthetic_ban["data"]["record"],
            &format!("#ban create {SYNTHETIC_IDENTITY} 60 Foxy synthetic native fixture"),
            run,
            number(
                &before_synthetic_ban["session_generation"],
                "synthetic ban session generation",
            )? as u32,
        )
        || !record_transcript(&synthetic_ban["data"]["record"])
            .lines()
            .any(|line| line.trim() == "Failed to create ban.")
    {
        return Err(io::Error::other(format!(
            "Synthetic ban identity did not receive an explicit native rejection: {synthetic_ban}"
        )));
    }
    report.push("synthetic Base64 identity ban received explicit native rejection; no successful ban is claimed".into());

    let history_before_kick = command_history(&case)?;
    let (unknown_kick_code, unknown_kick) = typed_admin(
        &case,
        "kick",
        run,
        &[
            "--player-id",
            "4294967295",
            "--expected-identity",
            SYNTHETIC_IDENTITY,
            "--expected-run-generation",
            &run.to_string(),
            "--yes",
        ],
        false,
    )?;
    evidence.push(json!({"unknown_player_kick_refusal": {"exit_code": unknown_kick_code, "result": unknown_kick}}))?;
    if unknown_kick_code != 4
        || unknown_kick["data"]["state"] != "not_sent"
        || unknown_kick["data"]["reason"] != "target_changed"
        || !unknown_kick["data"]["record"]["sent_unix_ms"].is_null()
    {
        return Err(io::Error::other(format!(
            "Unknown player id was not refused before kick: {unknown_kick}"
        )));
    }
    let refused = new_commands(
        &history_before_kick,
        &command_history(&case)?,
        run,
        "#kick 4294967295",
    );
    if refused.len() != 1
        || refused[0]["state"] != "not_sent"
        || !refused[0]["sent_unix_ms"].is_null()
    {
        return Err(io::Error::other(
            "Refused kick was not durably recorded without transmission",
        ));
    }

    let history_before_stale_kick = command_history(&case)?;
    let stale_run = run.saturating_add(99);
    let (stale_kick_code, stale_kick) = typed_admin(
        &case,
        "kick",
        run,
        &[
            "--player-id",
            "1",
            "--expected-identity",
            SYNTHETIC_IDENTITY,
            "--expected-run-generation",
            &stale_run.to_string(),
            "--yes",
        ],
        false,
    )?;
    evidence.push(
        json!({"stale_run_kick_refusal": {"exit_code": stale_kick_code, "result": stale_kick}}),
    )?;
    if stale_kick_code != 4 || stale_kick["ok"].as_bool().is_none_or(|ok| ok) {
        return Err(io::Error::other(format!(
            "Stale-run kick was not refused: {stale_kick}"
        )));
    }
    assert_no_new_command(
        &history_before_stale_kick,
        &command_history(&case)?,
        run,
        "#kick 1",
    )?;

    let stale_reload_cursor = logs_next_sequence(&case, &mut evidence)?;
    let history_before_stale_reload = command_history(&case)?;
    let stale_reload_generation = run.saturating_add(99).to_string();
    let (stale_reload_code, stale_reload) = typed_admin(
        &case,
        "reload-scenario",
        run,
        &[
            "--expected-run-generation",
            &stale_reload_generation,
            "--timeout-ms",
            "60000",
            "--yes",
        ],
        false,
    )?;
    evidence.push(json!({"stale_generation_scenario_reload": {"exit_code": stale_reload_code, "result": stale_reload}}))?;
    if stale_reload_code != 4 {
        return Err(io::Error::other(format!(
            "Stale-generation scenario reload was not refused: {stale_reload}"
        )));
    }
    assert_no_new_command(
        &history_before_stale_reload,
        &command_history(&case)?,
        run,
        "#restart",
    )?;
    assert_no_fresh_scenario_load(&case, run, stale_reload_cursor, &scenario, &mut evidence)?;

    let reload_cursor = logs_next_sequence(&case, &mut evidence)?;
    let reload_started_ms = unix_ms();
    let history_before_explicit_reload = command_history(&case)?;
    let (reload_code, reload) = typed_admin(
        &case,
        "reload-scenario",
        run,
        &[
            "--expected-run-generation",
            &run.to_string(),
            "--timeout-ms",
            "60000",
            "--yes",
        ],
        false,
    )?;
    evidence
        .push(json!({"typed_scenario_reload": {"exit_code": reload_code, "result": reload}}))?;
    let history_after_explicit_reload = command_history(&case)?;
    let explicit_reload_records = new_commands(
        &history_before_explicit_reload,
        &history_after_explicit_reload,
        run,
        "#restart",
    );
    if explicit_reload_records.len() != 1
        || explicit_reload_records[0]["sent_unix_ms"].is_null()
        || !matches!(
            explicit_reload_records[0]["state"].as_str(),
            Some("replied" | "unknown")
        )
    {
        return Err(io::Error::other(format!(
            "Explicit scenario reload did not retain exactly one sent command record: {explicit_reload_records:?}"
        )));
    }
    evidence.push(json!({"explicit_reload_command_records": explicit_reload_records}))?;
    let live_after_explicit = current_run_query(&case, run, pid, reload_started_ms, &mut evidence)?;
    let explicit_message = reload["data"]["outcome"]["message"]
        .as_str()
        .unwrap_or_default();
    if reload["data"]["process_restarted"] != false
        || reload["data"]["outcome"]["status"]["run_generation"] != run
        || reload["data"]["outcome"]["status"]["process_id"] != pid
    {
        return Err(io::Error::other(format!(
            "Scenario reload changed its owned process or run: {reload}"
        )));
    }
    if reload_code == 0 && reload["data"]["outcome"]["completed"] == true {
        let reloaded = wait_fresh_scenario_and_query(
            &case,
            run,
            pid,
            reload_cursor,
            reload_started_ms,
            &scenario,
            &mut evidence,
        )?;
        evidence.push(json!({"explicit_reload_proof_phase": "live_verified"}))?;
        report.push(format!(
            "typed reload live-verified in same process/run ({})",
            reloaded["observation"]["query"]["last_success_unix_ms"]
        ));
    } else if reload_code == 5
        && reload["data"]["outcome"]["completed"] == false
        && explicit_message.contains("unconfirmed")
        && explicit_message.contains("will not be repeated automatically")
        && live_after_explicit["data"]["monitoring"]["query_ready"] == true
    {
        evidence.push(json!({"explicit_reload_proof_phase": "pending_retrospective_native_log", "live_effect_claimed": false}))?;
        report.push("typed reload outcome remained unknown live; exactly one sent command and same-run fresh query retained for retrospective native-log verification".into());
    } else {
        return Err(io::Error::other(format!(
            "Typed reload returned neither live proof nor the explicit unconfirmed outcome: code={reload_code}, result={reload}"
        )));
    }

    let logout_before = fresh_session(&case, run)?;
    let logout_generation = number(
        &logout_before["session_generation"],
        "logout session generation",
    )?;
    let (logout_code, logout) = typed_admin(
        &case,
        "logout",
        run,
        &[
            "--expected-run-generation",
            &run.to_string(),
            "--timeout-ms",
            "10000",
            "--yes",
        ],
        false,
    )?;
    evidence.push(json!({"typed_logout": {"exit_code": logout_code, "result": logout}}))?;
    if !matches!(logout_code, 0 | 5)
        || !record_command_bound(
            &logout["data"]["record"],
            "@logout",
            run,
            logout_generation as u32,
        )
    {
        return Err(io::Error::other(format!(
            "Typed logout did not produce its current-session command record: {logout}"
        )));
    }
    let reconnected = wait_session(
        &case,
        run,
        number(
            &logout["data"]["record"]["session_generation"],
            "logout command session",
        )? + 1,
        &mut evidence,
    )?;
    report.push(format!(
        "typed logout reconnected same run as session {}",
        reconnected["session_generation"]
    ));

    let suffix = format!("{:08x}", rand::random::<u32>());
    let cancel_job = add_reforger_reload_job(
        &case,
        &mut owned_jobs,
        &mut evidence,
        &format!("D29_Cancel_{suffix}"),
    )?;
    let active_job = add_reforger_reload_job(
        &case,
        &mut owned_jobs,
        &mut evidence,
        &format!("D29_Reload_{suffix}"),
    )?;

    let before_cancel = case.ok(&["server", "instance", "monitor", &case.instance])?;
    evidence.push(before_cancel.clone())?;
    let cancel_status = &before_cancel["data"]["monitoring"]["status"];
    let cancel_pid = number(&cancel_status["process_id"], "pre-cancel process id")?;
    let cancel_cursor = logs_next_sequence(&case, &mut evidence)?;
    let cancel_history = command_history(&case)?;
    let cancellation_gate = hold_owner_operation(&case)?;
    let cancel_result = run_reforger_job(&case, &cancel_job, &mut evidence)?;
    let cancel_occurrence = string(
        &cancel_result["data"]["occurrence_id"],
        "cancelled occurrence id",
    )?
    .to_owned();
    let (cancel_code, cancel_reply) = case.raw(
        &[
            "--yes",
            "server",
            "maintenance",
            "cancel",
            &case.instance,
            &cancel_occurrence,
        ],
        None,
    )?;
    evidence
        .push(json!({"pre_dispatch_cancel": {"exit_code": cancel_code, "result": cancel_reply}}))?;
    if cancel_code != 0 {
        return Err(io::Error::other(format!(
            "Pre-dispatch maintenance cancellation was refused: {cancel_reply}"
        )));
    }
    let cancelled = wait_occurrence(&case, &cancel_occurrence, &mut evidence)?;
    drop(cancellation_gate);
    if cancelled["state"] != "cancelled" {
        return Err(io::Error::other(format!(
            "Scenario reload was dispatched before cancellation: {cancelled}"
        )));
    }
    assert_no_new_command(&cancel_history, &command_history(&case)?, run, "#restart")?;
    assert_no_fresh_scenario_load(&case, run, cancel_cursor, &scenario, &mut evidence)?;
    let after_cancel = case.ok(&["server", "instance", "monitor", &case.instance])?;
    evidence.push(after_cancel.clone())?;
    if after_cancel["data"]["monitoring"]["status"]["run_generation"] != run
        || after_cancel["data"]["monitoring"]["status"]["process_id"] != cancel_pid
        || after_cancel["data"]["monitoring"]["query_ready"] != true
    {
        return Err(io::Error::other(
            "Cancelled pre-dispatch reload changed or degraded the active process",
        ));
    }

    let maintenance_cursor = logs_next_sequence(&case, &mut evidence)?;
    let maintenance_started_ms = unix_ms();
    let history_before_maintenance_reload = command_history(&case)?;
    let first_run_request = run_reforger_job(&case, &active_job, &mut evidence)?;
    let occurrence = string(
        &first_run_request["data"]["occurrence_id"],
        "maintenance occurrence id",
    )?
    .to_owned();
    let duplicate_run_request = run_reforger_job(&case, &active_job, &mut evidence)?;
    if duplicate_run_request["data"]["occurrence_id"] != occurrence
        || duplicate_run_request["data"]["created"] != false
    {
        return Err(io::Error::other(format!(
            "Repeated manual run did not deduplicate to the same open request: {duplicate_run_request}"
        )));
    }
    let maintenance_result = wait_occurrence(&case, &occurrence, &mut evidence)?;
    let history_after_maintenance_reload = command_history(&case)?;
    let maintenance_reload_records = new_commands(
        &history_before_maintenance_reload,
        &history_after_maintenance_reload,
        run,
        "#restart",
    );
    if maintenance_reload_records.len() != 1
        || maintenance_reload_records[0]["sent_unix_ms"].is_null()
        || !matches!(
            maintenance_reload_records[0]["state"].as_str(),
            Some("replied" | "unknown")
        )
    {
        return Err(io::Error::other(format!(
            "Maintenance reload did not retain exactly one sent command record: {maintenance_reload_records:?}"
        )));
    }
    evidence.push(json!({"maintenance_reload_command_records": maintenance_reload_records}))?;
    let reload_records_for_run = new_commands(
        &history_before_explicit_reload,
        &history_after_maintenance_reload,
        run,
        "#restart",
    );
    if reload_records_for_run.len() != 2 {
        return Err(io::Error::other(format!(
            "Expected exactly one sent #restart record for each explicit/job request and no replay: {reload_records_for_run:?}"
        )));
    }
    evidence.push(json!({"reload_records_before_process_restart": reload_records_for_run}))?;
    let live_after_maintenance =
        current_run_query(&case, run, pid, maintenance_started_ms, &mut evidence)?;
    let maintenance_message = maintenance_result["message"].as_str().unwrap_or_default();
    if maintenance_result["state"] == "succeeded"
        && maintenance_message.contains("answered a fresh query")
        && live_after_maintenance["data"]["monitoring"]["query_ready"] == true
    {
        let maintenance_reload = wait_fresh_scenario_and_query(
            &case,
            run,
            pid,
            maintenance_cursor,
            maintenance_started_ms,
            &scenario,
            &mut evidence,
        )?;
        if maintenance_reload["status"]["run_generation"] != run
            || maintenance_reload["status"]["process_id"] != pid
        {
            return Err(io::Error::other(
                "Scheduled Reforger scenario reload changed the process or run",
            ));
        }
        evidence.push(json!({"maintenance_reload_proof_phase": "live_verified"}))?;
        report.push("maintenance reload live-verified in same process/run".into());
    } else if maintenance_result["state"] == "unknown"
        && maintenance_message.contains("unconfirmed")
        && maintenance_message.contains("will not be repeated automatically")
        && live_after_maintenance["data"]["monitoring"]["query_ready"] == true
    {
        evidence.push(json!({"maintenance_reload_proof_phase": "pending_retrospective_native_log", "live_effect_claimed": false}))?;
        report.push("maintenance reload outcome remained unknown live; exactly one sent command and same-run fresh query retained for retrospective native-log verification".into());
    } else {
        return Err(io::Error::other(format!(
            "Maintenance reload returned neither live proof nor the explicit unknown outcome: {maintenance_result}"
        )));
    }

    owned_jobs.remove_all(&mut evidence)?;
    let history_before_restart = command_history(&case)?;
    let start_plan = case.ok(&["server", "instance", "start-plan", &case.instance])?;
    evidence.push(start_plan.clone())?;
    let activation = number(
        &start_plan["data"]["plan"]["activation_generation"],
        "reviewed activation generation",
    )?
    .to_string();
    let fingerprint = string(
        &start_plan["data"]["plan"]["fingerprint"],
        "reviewed start-plan fingerprint",
    )?
    .to_owned();
    let prior_run = run;
    let startup_started_ms = unix_ms();
    let restarted = case.ok(&[
        "server",
        "instance",
        "restart",
        &case.instance,
        "--expected-run-generation",
        &prior_run.to_string(),
        "--expected-activation-generation",
        &activation,
        "--expected-plan-fingerprint",
        &fingerprint,
        "--timeout-ms",
        "60000",
        "--yes",
    ])?;
    evidence.push(restarted.clone())?;
    let restart_outcome = &restarted["data"]["outcome"];
    if restart_outcome["completed"] != true
        || restart_outcome["status"]["run_generation"] != prior_run + 1
    {
        return Err(io::Error::other(format!(
            "Reviewed process restart did not complete a new run: {restarted}"
        )));
    }
    run = number(
        &restart_outcome["status"]["run_generation"],
        "restarted run",
    )?;
    cleanup.run = Some(run);
    let restarted_ready = wait_new_ready_run(&case, prior_run, &mut evidence)?;
    let new_status = &restarted_ready["data"]["monitoring"]["status"];
    let new_pid = number(&new_status["process_id"], "restarted process id")?;
    if new_pid == pid || run != prior_run + 1 {
        return Err(io::Error::other(format!(
            "Process restart did not produce a distinct native process and run: {restarted_ready}"
        )));
    }
    let new_session = wait_session(&case, run, 1, &mut evidence)?;
    let startup = current_run_query(&case, run, new_pid, startup_started_ms, &mut evidence)?;
    if number(&new_session["run_generation"], "new session run")? != run
        || startup["data"]["monitoring"]["status"]["process_id"] != new_pid
    {
        return Err(io::Error::other(
            "Restarted process did not regain its own RCon session and scenario readiness",
        ));
    }
    let restart_shutdowns = new_commands(
        &history_before_restart,
        &command_history(&case)?,
        prior_run,
        "#shutdown",
    );
    if restart_shutdowns.len() != 1
        || !matches!(
            restart_shutdowns[0]["state"].as_str(),
            Some("replied" | "unknown")
        )
        || restart_shutdowns[0]["sent_unix_ms"].is_null()
    {
        return Err(io::Error::other(format!(
            "Reviewed restart did not retain exactly one sent shutdown record for the prior run: {restart_shutdowns:?}"
        )));
    }
    let retrospective = read_retrospective_reload_log(&case, &owner_id, prior_run, &scenario)?;
    let expected_world = retrospective.world.clone();
    evidence.push(json!({
        "retrospective_native_reload_proof": {
            "proof_phase": "post_shutdown_native_stdout",
            "owner_id": owner_id,
            "run_generation": prior_run,
            "engine_initialization_count": retrospective.engine_initialization_count,
            "committed_playthrough_count": retrospective.committed_playthrough_count,
            "reload_command_count": retrospective.reload_command_count,
            "reloads": retrospective.reloads,
            "stdout_path": retrospective.path,
            "live_ui_logs_were_not_used_as_effect_proof": true
        }
    }))?;
    report.push("post-shutdown native stdout retrospectively verified exactly two committed same-process scenario reloads; no live success was inferred from unknown outcomes".into());
    report.push(format!(
        "reviewed restart {prior_run}->{run} changed process {pid}->{new_pid}, completed RCon shutdown and reached fresh committed scenario/query readiness"
    ));

    let final_history = command_history(&case)?;
    let (stop_code, stopped) = case.raw(
        &[
            "server",
            "instance",
            "stop",
            &case.instance,
            "--expected-run-generation",
            &run.to_string(),
            "--timeout-ms",
            "60000",
            "--yes",
        ],
        None,
    )?;
    evidence.push(json!({"final_product_stop": {"exit_code": stop_code, "result": stopped}}))?;
    if stop_code != 0
        || stopped["data"]["outcome"]["completed"] != true
        || stopped["data"]["outcome"]["status"]["run_generation"] != run
        || stopped["data"]["outcome"]["status"]["phase"] != "exited"
        || stopped["data"]["outcome"]["status"]["cleanup_verified"] != true
        || stopped["data"]["outcome"]["status"]["process_exists"] != false
        || stopped["data"]["outcome"]["status"]["forced"] != false
    {
        return Err(io::Error::other(format!(
            "Final product Stop did not verify graceful exact-run cleanup: {stopped}"
        )));
    }
    cleanup.run = None;
    let (started_log, started_path) = read_native_run_log(&case, &owner_id, run)?;
    verify_started_run_log(&started_log, &expected_world)?;
    evidence.push(json!({"restarted_run_native_proof": {
        "proof_phase": "post_shutdown_native_stdout", "stdout_path": started_path,
        "expected_world_from_verified_scenario_reload": expected_world,
        "live_query_and_rcon_verified": true, "live_scenario_completion_claimed": false,
    }}))?;
    let final_shutdowns = new_commands(&final_history, &command_history(&case)?, run, "#shutdown");
    if final_shutdowns.len() != 1
        || !matches!(
            final_shutdowns[0]["state"].as_str(),
            Some("replied" | "unknown")
        )
        || final_shutdowns[0]["sent_unix_ms"].is_null()
    {
        return Err(io::Error::other(format!(
            "Final Stop did not retain exactly one replied RCon shutdown record for the stopped run: {final_shutdowns:?}"
        )));
    }
    let final_status = case.ok(&["server", "instance", "status", &case.instance])?;
    evidence.push(final_status.clone())?;
    if final_status["data"]["status"]["run_generation"] != run
        || final_status["data"]["status"]["cleanup_verified"] != true
        || final_status["data"]["status"]["process_exists"] != false
        || final_status["data"]["status"]["forced"] != false
    {
        return Err(io::Error::other(format!(
            "Final idle status does not prove graceful cleanup of run {run}: {final_status}"
        )));
    }
    let stopped_owner = case.ok(&["server", "supervise", "--shutdown", "--yes"])?;
    evidence.push(stopped_owner)?;
    cleanup.active = false;
    drop(owner);
    if installation_hash(&case)? != executable_before {
        return Err(io::Error::other("The adopted executable changed"));
    }
    report.push(
        "final product Stop retained one sent #shutdown record and exact-run graceful cleanup"
            .into(),
    );
    evidence.push(json!({"observations": report}))?;
    evidence.finish()
}

fn hold_owner_operation(case: &Case) -> io::Result<File> {
    let owner = case.ok(&["server", "owner"])?;
    let key = string(
        &owner["data"]["owner"]["scope"]["context_key"],
        "owner context key",
    )?;
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("Invalid private owner context key"));
    }
    #[cfg(windows)]
    let directory = std::env::temp_dir().join(format!("foxy-owner-{}", &key[..24]));
    #[cfg(target_os = "linux")]
    let directory = Path::new("/tmp").join(format!(
        "foxy-owner-{}-{}",
        unsafe { libc::geteuid() },
        &key[..16]
    ));
    let directory = foxy_supervisor::control::handoff::ProtectedDirectory::open(&directory)?;
    if directory.read("operation.lock")? != b"foxy-metadata-operation-v1" {
        return Err(io::Error::other("Owner operation marker changed"));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("operation.lock"))?;
    file.try_lock().map_err(io::Error::other)?;
    Ok(file)
}

fn typed_admin(
    case: &Case,
    verb: &str,
    _run: u64,
    extra: &[&str],
    _unused: bool,
) -> io::Result<(i32, Value)> {
    let mut args = vec!["server", "reforger-admin", verb, &case.instance];
    args.extend_from_slice(extra);
    case.raw(&args, None)
}

fn fresh_session(case: &Case, run: u64) -> io::Result<Value> {
    let status = session(case)?;
    if status["connection"] != "connected" || status["run_generation"] != run {
        return Err(io::Error::other(format!(
            "Typed RCon command was not bound to a fresh connected session for run {run}: {status}"
        )));
    }
    Ok(status)
}

fn record_matches(record: &Value, text: &str, run: u64, session_generation: u32) -> bool {
    record_command_bound(record, text, run, session_generation) && record["state"] == "replied"
}

fn record_command_bound(record: &Value, text: &str, run: u64, session_generation: u32) -> bool {
    record["text"] == text
        && record["run_generation"] == run
        && record["session_generation"]
            .as_u64()
            .is_some_and(|generation| generation > u64::from(session_generation))
        && matches!(record["state"].as_str(), Some("replied" | "unknown"))
}

fn list_record_matches(result: &Value, text: &str, run: u64, session_generation: u32) -> bool {
    result["data"]["record"].is_object()
        && record_matches(&result["data"]["record"], text, run, session_generation)
        && result["data"]["record"]["messages"]
            .as_array()
            .is_some_and(|messages| !messages.is_empty())
}

fn record_transcript(record: &Value) -> String {
    record["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn command_history(case: &Case) -> io::Result<Value> {
    let value = case.ok(&["server", "rcon", "history", &case.instance])?;
    Ok(value["data"]["history"]["records"]
        .as_array()
        .map(|records| json!(records))
        .unwrap_or_else(|| json!([])))
}

fn new_commands(before: &Value, after: &Value, run: u64, text: &str) -> Vec<Value> {
    let old_ids = before
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["command_id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    after
        .as_array()
        .into_iter()
        .flatten()
        .filter(|record| {
            record["run_generation"] == run
                && record["text"] == text
                && record["command_id"]
                    .as_str()
                    .is_some_and(|id| !old_ids.contains(id))
        })
        .cloned()
        .collect()
}

fn assert_no_new_command(before: &Value, after: &Value, run: u64, text: &str) -> io::Result<()> {
    let found = new_commands(before, after, run, text);
    if found.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Refused {text} unexpectedly created a command record: {found:?}"
        )))
    }
}

fn add_reforger_reload_job(
    case: &Case,
    cleanup: &mut AdminJobCleanup<'_>,
    evidence: &mut Evidence,
    name: &str,
) -> io::Result<Value> {
    let before = case.ok(&["server", "maintenance", "list", &case.instance])?;
    evidence.push(before.clone())?;
    let revision = number(
        &before["data"]["overview"]["catalog_revision"],
        "maintenance catalog revision",
    )?
    .to_string();
    let added = case.ok(&[
        "server",
        "maintenance",
        "add",
        &case.instance,
        "--expected-revision",
        &revision,
        "--name",
        name,
        "--action",
        "reload-scenario",
        "--weekly",
        "daily",
        "--time",
        "00:00",
        "--disabled",
    ])?;
    evidence.push(added.clone())?;
    let job_id = string(&added["data"]["job_id"], "created Reforger maintenance job")?.to_owned();
    cleanup.jobs.push(job_id.clone());
    let listed = case.ok(&["server", "maintenance", "list", &case.instance])?;
    evidence.push(listed.clone())?;
    let job = listed["data"]["overview"]["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|item| item["job"]["id"] == job_id))
        .cloned()
        .ok_or_else(|| io::Error::other("Saved Reforger reload job is missing"))?;
    let definition = &job["job"]["definition"];
    if definition["enabled"] != false
        || definition["action"] != "reload_scenario"
        || definition["policy"].get("arma").is_some()
    {
        return Err(io::Error::other(format!(
            "Saved Reforger reload job has unexpected shape: {job}"
        )));
    }
    Ok(job)
}

fn run_reforger_job(case: &Case, job: &Value, evidence: &mut Evidence) -> io::Result<Value> {
    let id = string(&job["job"]["id"], "Reforger job id")?;
    let revision = number(&job["job"]["revision"], "Reforger job policy revision")?.to_string();
    let result = case.ok(&[
        "server",
        "maintenance",
        "run",
        &case.instance,
        id,
        "--expected-policy-revision",
        &revision,
        "--yes",
    ])?;
    evidence.push(result.clone())?;
    Ok(result)
}

fn wait_occurrence(case: &Case, id: &str, evidence: &mut Evidence) -> io::Result<Value> {
    let deadline = Instant::now() + TRANSITION_DEADLINE;
    loop {
        let history = case.ok(&["server", "maintenance", "history", &case.instance])?;
        let occurrence = history["data"]["occurrences"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["occurrence_id"] == id))
            .cloned();
        evidence.push(json!({"maintenance_history": history, "occurrence": occurrence}))?;
        if let Some(item) = occurrence
            && item["state"].as_str().is_some_and(|state| {
                matches!(
                    state,
                    "succeeded" | "blocked" | "cancelled" | "failed" | "unknown" | "skipped"
                )
            })
        {
            return Ok(item);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Maintenance occurrence {id} did not reach a terminal state"),
            ));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_fresh_scenario_and_query(
    case: &Case,
    run: u64,
    pid: u64,
    after_sequence: u64,
    requested_after_ms: u64,
    scenario: &str,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let deadline = Instant::now() + TRANSITION_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (logs_code, logs) = case.raw(
            &[
                "server",
                "instance",
                "logs",
                &case.instance,
                "--after-sequence",
                &after_sequence.to_string(),
            ],
            None,
        )?;
        let entries = logs["data"]["logs"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut start = None;
        let mut world = None;
        for entry in &entries {
            let sequence = entry["sequence"].as_u64().unwrap_or(0);
            let text = entry["text"].as_str().unwrap_or_default();
            if sequence >= after_sequence
                && (after_sequence == 0
                    || text.contains("[SaveGameManager] Starting new playthrough"))
                && text.contains(&format!("for mission '{scenario}'"))
            {
                start = Some(sequence);
                world = None;
            } else if start.is_some_and(|start_sequence| start_sequence < sequence)
                && text.contains("WORLD")
                && text.contains("Entities load '")
            {
                world = Some(sequence);
                break;
            }
        }
        let (monitor_code, monitor) =
            case.raw(&["server", "instance", "monitor", &case.instance], None)?;
        let query_ms =
            monitor["data"]["monitoring"]["observation"]["query"]["last_success_unix_ms"]
                .as_u64()
                .unwrap_or(0);
        let exact_ready = monitor_code == 0
            && monitor["data"]["monitoring"]["query_ready"] == true
            && monitor["data"]["monitoring"]["status"]["run_generation"] == run
            && monitor["data"]["monitoring"]["status"]["process_id"] == pid
            && query_ms > requested_after_ms;
        last = json!({"logs": logs, "monitor": monitor, "scenario_start_sequence": start, "world_load_sequence": world});
        if logs_code == 0
            && logs["data"]["run_generation"] == run
            && start.is_some()
            && world.is_some()
            && exact_ready
        {
            evidence.push(last.clone())?;
            return Ok(last["monitor"]["data"]["monitoring"].clone());
        }
        thread::sleep(Duration::from_millis(200));
    }
    evidence.push(last.clone())?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("No current-run committed scenario/world load followed by a fresh query: {last}"),
    ))
}

fn assert_no_fresh_scenario_load(
    case: &Case,
    run: u64,
    after_sequence: u64,
    scenario: &str,
    evidence: &mut Evidence,
) -> io::Result<()> {
    let (code, logs) = case.raw(
        &[
            "server",
            "instance",
            "logs",
            &case.instance,
            "--after-sequence",
            &after_sequence.to_string(),
        ],
        None,
    )?;
    evidence.push(logs.clone())?;
    let found = code == 0
        && logs["data"]["run_generation"] == run
        && logs["data"]["logs"]["entries"]
            .as_array()
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry["sequence"]
                        .as_u64()
                        .is_some_and(|sequence| sequence >= after_sequence)
                        && entry["text"].as_str().is_some_and(|text| {
                            text.contains("[SaveGameManager] Starting new playthrough")
                                && text.contains(&format!("for mission '{scenario}'"))
                        })
                })
            });
    if found {
        Err(io::Error::other(format!(
            "A refused or cancelled reload produced a fresh committed scenario start: {logs}"
        )))
    } else {
        Ok(())
    }
}

fn current_run_query(
    case: &Case,
    run: u64,
    pid: u64,
    requested_after_ms: u64,
    evidence: &mut Evidence,
) -> io::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (code, monitor) = case.raw(&["server", "instance", "monitor", &case.instance], None)?;
        let query_ms =
            monitor["data"]["monitoring"]["observation"]["query"]["last_success_unix_ms"]
                .as_u64()
                .unwrap_or(0);
        let ready = code == 0
            && monitor["data"]["monitoring"]["query_ready"] == true
            && monitor["data"]["monitoring"]["status"]["run_generation"] == run
            && monitor["data"]["monitoring"]["status"]["process_id"] == pid
            && query_ms > requested_after_ms;
        last = monitor;
        if ready {
            evidence.push(json!({"same_run_fresh_query_after_unconfirmed_reload": last}))?;
            return Ok(last);
        }
        thread::sleep(Duration::from_millis(250));
    }
    evidence.push(last.clone())?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("Unconfirmed reload did not retain a fresh query for the same process/run: {last}"),
    ))
}

struct RetrospectiveReloadProof {
    path: String,
    world: String,
    engine_initialization_count: usize,
    committed_playthrough_count: usize,
    reload_command_count: usize,
    reloads: Vec<Value>,
}

fn validate_owner_log_component(owner_id: &str) -> io::Result<()> {
    if owner_id.is_empty()
        || owner_id.len() > 128
        || !owner_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(io::Error::other("Invalid owner id for native run log"));
    }
    Ok(())
}

fn read_retrospective_reload_log(
    case: &Case,
    owner_id: &str,
    run: u64,
    scenario: &str,
) -> io::Result<RetrospectiveReloadProof> {
    let (log, path_text) = read_native_run_log(case, owner_id, run)?;
    let mut proof = verify_retrospective_reload_log(&log, scenario)?;
    proof.path = path_text;
    Ok(proof)
}

fn read_native_run_log(case: &Case, owner_id: &str, run: u64) -> io::Result<(String, String)> {
    validate_owner_log_component(owner_id)?;
    let path = case
        .config
        .join("games")
        .join(&case.space)
        .join("server")
        .join("instances")
        .join(&case.instance)
        .join("deployments")
        .join("runtime-state")
        .join(format!("run_{owner_id}_{run}.log"));
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err(io::Error::other(
            "Retrospective native run log is not an ordinary file",
        ));
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&case.config) {
        return Err(io::Error::other(
            "Retrospective native run log escaped the fixture config root",
        ));
    }
    let log = fs::read_to_string(&canonical)?;
    let path_text = canonical
        .to_str()
        .ok_or_else(|| io::Error::other("Native run log path is not UTF-8"))?
        .to_owned();
    Ok((log, path_text))
}

fn verify_retrospective_reload_log(
    log: &str,
    scenario: &str,
) -> io::Result<RetrospectiveReloadProof> {
    let lines: Vec<_> = log.lines().collect();
    let engine_initialization_count = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("initializing engine, version")
        })
        .count();
    let restart_indices: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.contains("Process Command: #restart").then_some(index))
        .collect();
    let all_playthrough_indices: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.contains("[SaveGameManager] Starting new playthrough")
                .then_some(index)
        })
        .collect();
    let initial_playthroughs = all_playthrough_indices
        .iter()
        .take_while(|index| **index < restart_indices.first().copied().unwrap_or(0))
        .count();
    if engine_initialization_count != 1
        || restart_indices.len() != 2
        || initial_playthroughs > 1
        || all_playthrough_indices.len() != initial_playthroughs + 2
        || all_playthrough_indices
            .iter()
            .any(|index| !lines[*index].contains(&format!("for mission '{scenario}'")))
    {
        return Err(io::Error::other(format!(
            "Retrospective log has unexpected engine/reload counts: init={engine_initialization_count}, restarts={}, playthroughs={}",
            restart_indices.len(),
            all_playthrough_indices.len(),
        )));
    }

    let mut reloads = Vec::with_capacity(2);
    let mut world = None;
    for (ordinal, restart_index) in restart_indices.iter().copied().enumerate() {
        let next_boundary = lines
            .iter()
            .enumerate()
            .skip(restart_index + 1)
            .find_map(|(index, line)| {
                (line.contains("Process Command: #restart")
                    || line.contains("Process Command: #shutdown"))
                .then_some(index)
            })
            .unwrap_or(lines.len());
        let expected_start = all_playthrough_indices[ordinal + initial_playthroughs];
        if expected_start <= restart_index || expected_start >= next_boundary {
            return Err(io::Error::other(format!(
                "Reload {} did not precede its exact committed playthrough before the next command",
                ordinal + 1
            )));
        }
        let world_load = lines
            .iter()
            .enumerate()
            .skip(expected_start + 1)
            .take(next_boundary.saturating_sub(expected_start + 1))
            .find_map(|(index, line)| {
                (line.contains("WORLD") && line.contains("Entities load '")).then_some(index)
            })
            .ok_or_else(|| {
                io::Error::other(format!(
                    "Reload {} had no following native world-load line",
                    ordinal + 1
                ))
            })?;
        let online = lines
            .iter()
            .enumerate()
            .skip(world_load + 1)
            .take(next_boundary.saturating_sub(world_load + 1))
            .find_map(|(index, line)| line.contains("Entered online game state.").then_some(index))
            .ok_or_else(|| {
                io::Error::other(format!(
                    "Reload {} had no native world-online line before the next RCon command",
                    ordinal + 1
                ))
            })?;
        let loaded_world = world_path(lines[world_load])
            .ok_or_else(|| io::Error::other("Reload has no exact native world resource"))?;
        if world
            .as_ref()
            .is_some_and(|expected| expected != &loaded_world)
        {
            return Err(io::Error::other(
                "Scenario reloads loaded different native worlds",
            ));
        }
        world = Some(loaded_world);
        reloads.push(json!({
            "operation": if ordinal == 0 { "typed_reload" } else { "maintenance_reload" },
            "rcon_restart_line": restart_index + 1,
            "committed_playthrough_line": expected_start + 1,
            "world_load_line": world_load + 1,
            "world_online_line": online + 1,
        }));
    }
    Ok(RetrospectiveReloadProof {
        path: String::new(),
        world: world.expect("two verified reloads"),
        engine_initialization_count,
        committed_playthrough_count: all_playthrough_indices.len(),
        reload_command_count: restart_indices.len(),
        reloads,
    })
}

fn world_path(line: &str) -> Option<String> {
    let (_, resource) = line.split_once("Entities load ")?;
    let start = resource.find("worlds/")?;
    let rest = &resource[start..];
    let end = rest.find(".ent")? + 4;
    Some(rest[..end].to_owned())
}

fn verify_started_run_log(log: &str, expected_world: &str) -> io::Result<()> {
    let lines: Vec<_> = log.lines().collect();
    let init = lines
        .iter()
        .filter(|line| {
            line.to_ascii_lowercase()
                .contains("initializing engine, version")
        })
        .count();
    let restart = lines
        .iter()
        .filter(|line| line.contains("Process Command: #restart"))
        .count();
    let shutdowns: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.contains("Process Command: #shutdown").then_some(index))
        .collect();
    let world = lines
        .iter()
        .position(|line| world_path(line).as_deref() == Some(expected_world));
    if init != 1
        || restart != 0
        || shutdowns.len() != 1
        || world.is_none_or(|world| world >= shutdowns[0])
    {
        return Err(io::Error::other(
            "Restarted native run did not load the independently verified world before its single shutdown",
        ));
    }
    Ok(())
}

#[test]
fn restarted_run_requires_one_engine_and_the_verified_world_before_shutdown() {
    let log = "ENGINE : Initializing engine, version synthetic\nWORLD : Entities load @\"{0123456789ABCDEF}worlds/Example.ent\"\nDEFAULT : Entered onlineSCRIPT : diagnostic\ngame state.\nBACKEND : [RCON] Process Command: #shutdown\n";
    assert!(verify_started_run_log(log, "worlds/Example.ent").is_ok());
    assert!(verify_started_run_log(log, "worlds/Other.ent").is_err());
    assert!(
        verify_started_run_log(&log.replace("#shutdown", "#restart"), "worlds/Example.ent")
            .is_err()
    );
    assert!(
        verify_started_run_log(
            &format!("{log}ENGINE : Initializing engine, version synthetic\n"),
            "worlds/Example.ent"
        )
        .is_err()
    );
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
