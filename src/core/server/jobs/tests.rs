use super::ledger::{OccurrenceListing, RETAINED_TERMINAL_OCCURRENCES, occurrence_id};
use super::model::MaintenanceCatalog;
use super::status::job_statuses;
use super::*;

#[test]
fn arma_job_options_validate_real_actions_and_keep_old_policy_documents_readable() {
    let example: MaintenanceCatalog = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_arma_maintenance_jobs.json"
    ))
    .unwrap();
    example.validate(&example.instance_id).unwrap();
    let command: crate::core::server::rcon::arma3::ArmaAdminCommand =
        serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/server_arma_administration_command.json"
        ))
        .unwrap();
    assert_eq!(command.wire_text().unwrap(), "kick 0 Operator review");
    let legacy: MaintenancePolicy = serde_json::from_value(serde_json::json!({"grace_minutes":10,"max_defer_minutes":30,"max_attempts":2,"retry_delay_seconds":60,"stop_timeout_seconds":60})).unwrap();
    assert!(legacy.arma.is_none());
    let mut definition = definition("ArmaRotation");
    definition.action = MaintenanceAction::RotateMission;
    assert!(definition.validate().is_err());
    definition.policy.arma = Some(ArmaMaintenancePolicy {
        mission_template: Some("mission_alpha.vr".into()),
        ..Default::default()
    });
    assert!(definition.validate().is_ok());
    definition.action = MaintenanceAction::Restart;
    assert!(definition.validate().is_err());
    let mut policy = ArmaMaintenancePolicy {
        announcement_delay_seconds: 5,
        ..Default::default()
    };
    assert!(policy.validate().is_err());
    policy.announcement = Some("Restart in five seconds".into());
    assert!(policy.validate().is_ok());
    policy.announcement = Some("message\n#shutdown".into());
    assert!(policy.validate().is_err());
}
use crate::core::schedule::{JobSchedule, ScheduleClock};
use crate::core::server::ServerInstanceId;

fn instance() -> ServerInstanceId {
    ServerInstanceId::try_from(format!("server_{}", "a".repeat(32))).unwrap()
}

fn definition(name: &str) -> MaintenanceJobDefinition {
    MaintenanceJobDefinition {
        name: name.into(),
        enabled: true,
        action: MaintenanceAction::Restart,
        schedule: JobSchedule::Recurring {
            minutes_of_day: 4 * 60,
            weekdays: 0b0111_1111,
        },
        clock: ScheduleClock::Utc,
        policy: MaintenancePolicy::default(),
    }
}

#[test]
fn catalog_changes_assign_ids_and_advance_policy_revisions_only_on_real_edits() {
    let empty = MaintenanceCatalog::empty(instance());
    let (added, id) =
        model::apply_change(&empty, MaintenanceChange::Add(definition("Nightly")), 1_000).unwrap();
    let id = id.unwrap();
    assert!(valid_job_id(&id));
    assert_eq!(added.revision, 1);
    assert_eq!(added.job(&id).unwrap().revision, 1);
    assert_eq!(added.job(&id).unwrap().revised_unix_ms, 1_000);

    let (same, _) = model::apply_change(
        &added,
        MaintenanceChange::Set {
            id: id.clone(),
            definition: definition("Nightly"),
        },
        2_000,
    )
    .unwrap();
    assert_eq!(same.revision, 2);
    assert_eq!(same.job(&id).unwrap().revision, 1);
    assert_eq!(same.job(&id).unwrap().revised_unix_ms, 1_000);

    let mut edited = definition("Nightly");
    edited.policy.max_attempts = 3;
    let (changed, _) = model::apply_change(
        &same,
        MaintenanceChange::Set {
            id: id.clone(),
            definition: edited,
        },
        3_000,
    )
    .unwrap();
    assert_eq!(changed.job(&id).unwrap().revision, 2);
    assert_eq!(changed.job(&id).unwrap().revised_unix_ms, 3_000);

    assert!(
        model::apply_change(
            &changed,
            MaintenanceChange::Add(definition("nightly")),
            4_000
        )
        .is_err(),
        "names are unique without case"
    );
    let (removed, affected) = model::apply_change(
        &changed,
        MaintenanceChange::Remove { id: id.clone() },
        5_000,
    )
    .unwrap();
    assert!(removed.jobs.is_empty() && affected.is_none());
    assert!(model::apply_change(&removed, MaintenanceChange::Remove { id }, 6_000).is_err());
}

#[test]
fn definitions_reject_empty_schedules_names_and_out_of_range_policy() {
    let mut bad = definition(" padded ");
    assert!(bad.validate().is_err());
    bad = definition("Weekly");
    bad.schedule = JobSchedule::Recurring {
        minutes_of_day: 1440,
        weekdays: 1,
    };
    assert!(bad.validate().is_err());
    bad.schedule = JobSchedule::Recurring {
        minutes_of_day: 60,
        weekdays: 0,
    };
    assert!(bad.validate().is_err());
    bad.schedule = JobSchedule::OnceAt { unix_ms: 0 };
    assert!(bad.validate().is_err());
    for edit in [
        |policy: &mut MaintenancePolicy| policy.grace_minutes = 1441,
        |policy: &mut MaintenancePolicy| policy.max_defer_minutes = 721,
        |policy: &mut MaintenancePolicy| policy.max_attempts = 0,
        |policy: &mut MaintenancePolicy| policy.max_attempts = 6,
        |policy: &mut MaintenancePolicy| policy.retry_delay_seconds = 9,
        |policy: &mut MaintenancePolicy| policy.stop_timeout_seconds = 121,
    ] {
        let mut candidate = definition("Weekly");
        edit(&mut candidate.policy);
        assert!(candidate.validate().is_err());
    }
    let json = serde_json::to_value(definition("Weekly")).unwrap();
    let decoded: MaintenanceJobDefinition = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(decoded, definition("Weekly"));
    let mut unknown = json;
    unknown["shell"] = "rm".into();
    assert!(serde_json::from_value::<MaintenanceJobDefinition>(unknown).is_err());
}

#[test]
fn maintenance_adapters_require_their_native_capabilities() {
    let mut capabilities = crate::core::server::ServerCapabilities::default();
    assert!(
        MaintenanceAction::Restart
            .unavailable_reason(&capabilities)
            .is_some()
    );
    capabilities.lifecycle = true;
    assert!(
        MaintenanceAction::RotateMission
            .unavailable_reason(&capabilities)
            .is_some()
    );
    capabilities.arma_administration = true;
    assert!(
        MaintenanceAction::RotateMission
            .unavailable_reason(&capabilities)
            .is_none()
    );
    assert!(
        MaintenanceAction::Restart
            .unavailable_reason(&capabilities)
            .is_none()
    );
    assert!(
        MaintenanceAction::Update
            .unavailable_reason(&capabilities)
            .is_some()
    );
    assert!(
        MaintenanceAction::Backup
            .unavailable_reason(&capabilities)
            .is_some()
    );
}

fn occurrence(trigger: OccurrenceTrigger, owner: &str) -> Occurrence {
    Occurrence::new(
        instance(),
        "job_0000000000000001",
        3,
        MaintenanceAction::Restart,
        trigger,
        owner,
        7,
        10_000,
    )
}

#[test]
fn occurrence_ids_are_deterministic_per_schedule_revision_or_manual_request() {
    let scheduled = OccurrenceTrigger::Scheduled {
        scheduled_unix_ms: 99,
    };
    let first = occurrence(scheduled.clone(), "owner_a");
    let again = occurrence(scheduled, "owner_b");
    assert_eq!(first.occurrence_id, again.occurrence_id);
    assert!(valid_occurrence_id(&first.occurrence_id));
    let later = occurrence(
        OccurrenceTrigger::Scheduled {
            scheduled_unix_ms: 100,
        },
        "owner_a",
    );
    assert_ne!(first.occurrence_id, later.occurrence_id);
    let manual = |request: &str, at| {
        occurrence(
            OccurrenceTrigger::Manual {
                requested_unix_ms: at,
                request: request.into(),
            },
            "owner_a",
        )
        .occurrence_id
    };
    assert_eq!(manual("client/5", 1), manual("client/5", 2));
    assert_ne!(manual("client/5", 1), manual("client/6", 1));
}

#[test]
fn claims_are_exclusive_updates_are_owner_bound_and_abandoned_work_becomes_unknown() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("occurrences");
    let mut mine = occurrence(
        OccurrenceTrigger::Scheduled {
            scheduled_unix_ms: 5,
        },
        "owner_a",
    );
    assert!(claim(&dir, &mine).unwrap());
    assert!(
        !claim(&dir, &mine).unwrap(),
        "a second claim must not succeed"
    );
    mine.state = OccurrenceState::Running;
    mine.attempts = 1;
    update(&dir, &mine).unwrap();
    let mut foreign = mine.clone();
    foreign.owner_id = "owner_b".into();
    foreign.state = OccurrenceState::Succeeded;
    assert!(update(&dir, &foreign).is_err());

    assert_eq!(recover_abandoned(&dir, "owner_a", 20_000).unwrap(), 0);
    assert_eq!(recover_abandoned(&dir, "owner_b", 20_000).unwrap(), 1);
    let recovered = read(&dir, &mine.occurrence_id).unwrap().unwrap();
    assert_eq!(recovered.state, OccurrenceState::Unknown);
    assert_eq!(recovered.reason, Some(OccurrenceReason::OwnerLost));
    assert!(
        update(&dir, &mine).is_err(),
        "terminal occurrences stay final"
    );
    assert_eq!(recover_abandoned(&dir, "owner_c", 30_000).unwrap(), 0);
}

#[test]
fn tampered_or_renamed_records_are_unreadable_and_retention_keeps_open_work() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("occurrences");
    let base = occurrence(
        OccurrenceTrigger::Scheduled {
            scheduled_unix_ms: 1,
        },
        "owner_a",
    );
    claim(&dir, &base).unwrap();
    let mut tampered = base.clone();
    tampered.job_id = "job_0000000000000002".into();
    std::fs::write(
        dir.join(format!("{}.json", base.occurrence_id)),
        serde_json::to_vec(&tampered).unwrap(),
    )
    .unwrap();
    assert!(read(&dir, &base.occurrence_id).is_err());
    assert_eq!(ledger::list(&dir).unwrap().unreadable, 1);
    std::fs::remove_file(dir.join(format!("{}.json", base.occurrence_id))).unwrap();

    for index in 0..(RETAINED_TERMINAL_OCCURRENCES as u64 + 5) {
        let mut done = occurrence(
            OccurrenceTrigger::Scheduled {
                scheduled_unix_ms: 1_000 + index,
            },
            "owner_a",
        );
        done.state = OccurrenceState::Succeeded;
        done.updated_unix_ms = 1_000 + index;
        claim(&dir, &done).unwrap();
    }
    let open = occurrence(
        OccurrenceTrigger::Scheduled {
            scheduled_unix_ms: 1,
        },
        "owner_a",
    );
    claim(&dir, &open).unwrap();
    assert_eq!(prune(&dir).unwrap(), 5);
    let listing = ledger::list(&dir).unwrap();
    assert_eq!(listing.occurrences.len(), RETAINED_TERMINAL_OCCURRENCES + 1);
    assert!(
        listing
            .occurrences
            .iter()
            .any(|item| item.occurrence_id == open.occurrence_id)
    );
}

#[test]
fn retention_keeps_each_jobs_latest_scheduled_record_and_skips_unreadable_files() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("occurrences");
    let mut other_job = Occurrence::new(
        instance(),
        "job_0000000000000002",
        1,
        MaintenanceAction::Restart,
        OccurrenceTrigger::Scheduled {
            scheduled_unix_ms: 5,
        },
        "owner_a",
        7,
        10,
    );
    other_job.state = OccurrenceState::Succeeded;
    claim(&dir, &other_job).unwrap();
    for index in 0..(RETAINED_TERMINAL_OCCURRENCES as u64 + 3) {
        let mut manual = occurrence(
            OccurrenceTrigger::Manual {
                requested_unix_ms: 100 + index,
                request: format!("owner_a/7/cli/{index}"),
            },
            "owner_a",
        );
        manual.state = OccurrenceState::Succeeded;
        manual.updated_unix_ms = 100 + index;
        claim(&dir, &manual).unwrap();
    }
    std::fs::write(
        dir.join(format!("occ_{}.json", "f".repeat(32))),
        b"not an occurrence",
    )
    .unwrap();
    assert_eq!(
        prune(&dir).unwrap(),
        3,
        "unreadable files no longer stop retention"
    );
    let listing = ledger::list(&dir).unwrap();
    assert_eq!(listing.unreadable, 1);
    assert!(
        listing
            .occurrences
            .iter()
            .any(|item| item.occurrence_id == other_job.occurrence_id),
        "deleting a job's latest scheduled record would let its time be claimed again"
    );
}

fn job(schedule: JobSchedule, revised: u64) -> MaintenanceJob {
    let mut definition = definition("Nightly");
    definition.schedule = schedule;
    MaintenanceJob {
        id: "job_0000000000000001".into(),
        revision: 1,
        revised_unix_ms: revised,
        definition,
    }
}

#[test]
fn due_occurrences_respect_policy_revision_and_grace() {
    let at = 10_000_000;
    let once = job(JobSchedule::OnceAt { unix_ms: at }, 0);
    assert_eq!(scheduled_due(&once, at - 1), None);
    assert_eq!(
        scheduled_due(&once, at + 60_000),
        Some(ScheduledDue::Run {
            scheduled_unix_ms: at
        })
    );
    assert_eq!(
        scheduled_due(&once, at + 11 * 60_000),
        Some(ScheduledDue::Missed {
            scheduled_unix_ms: at
        })
    );
    let revised_later = job(JobSchedule::OnceAt { unix_ms: at }, at + 1);
    assert_eq!(scheduled_due(&revised_later, at + 60_000), None);
    let mut disabled = once.clone();
    disabled.definition.enabled = false;
    assert_eq!(scheduled_due(&disabled, at + 60_000), None);

    let mut no_grace = once.clone();
    no_grace.definition.policy.grace_minutes = 0;
    assert_eq!(
        scheduled_due(&no_grace, at + 5_000),
        Some(ScheduledDue::Run {
            scheduled_unix_ms: at
        }),
        "a zero grace still covers the owner's scan interval"
    );
    assert_eq!(
        scheduled_due(&no_grace, at + 61_000),
        Some(ScheduledDue::Missed {
            scheduled_unix_ms: at
        })
    );
}

#[test]
fn claimed_occurrences_expire_after_grace_and_deferral_allowance() {
    let mut policy = MaintenancePolicy::default();
    let allowance =
        (u64::from(policy.grace_minutes) + u64::from(policy.max_defer_minutes)) * 60_000;
    assert!(!claim_expired(1_000, &policy, 1_000 + allowance));
    assert!(claim_expired(1_000, &policy, 1_000 + allowance + 1));
    policy.grace_minutes = 0;
    policy.max_defer_minutes = 0;
    assert!(!claim_expired(1_000, &policy, 1_000 + 60_000));
    assert!(claim_expired(1_000, &policy, 1_000 + 60_001));
}

fn inputs() -> RestartInputs {
    RestartInputs {
        server_running: true,
        server_settled: true,
        stop_completed_by_occurrence: false,
        started_since_stop: false,
        players: PlayerEvidence::Unknown,
        operation_busy: false,
    }
}

#[test]
fn restarts_never_start_a_server_they_did_not_stop_and_defer_within_limits() {
    let mut policy = MaintenancePolicy::default();
    assert_eq!(
        restart_step(inputs(), &policy, None, 0),
        RestartStep::Restart
    );
    let stopped = RestartInputs {
        server_running: false,
        ..inputs()
    };
    assert_eq!(
        restart_step(stopped, &policy, None, 0),
        RestartStep::Skip(OccurrenceReason::ServerNotRunning)
    );
    assert_eq!(
        restart_step(stopped, &policy, Some(0), 1),
        RestartStep::Skip(OccurrenceReason::StoppedOutsideJob)
    );
    let ours = RestartInputs {
        server_running: false,
        stop_completed_by_occurrence: true,
        ..inputs()
    };
    assert_eq!(restart_step(ours, &policy, None, 0), RestartStep::StartOnly);
    let restarted_elsewhere = RestartInputs {
        stop_completed_by_occurrence: true,
        ..inputs()
    };
    assert_eq!(
        restart_step(restarted_elsewhere, &policy, None, 0),
        RestartStep::Skip(OccurrenceReason::StartedOutsideJob)
    );
    let started_and_stopped_elsewhere = RestartInputs {
        started_since_stop: true,
        ..ours
    };
    assert_eq!(
        restart_step(started_and_stopped_elsewhere, &policy, None, 0),
        RestartStep::Skip(OccurrenceReason::StartedOutsideJob),
        "a retry never starts a run that an operator started and stopped in the meantime"
    );

    policy.player_policy = PlayerPolicy::RequireEmpty;
    assert_eq!(
        restart_step(inputs(), &policy, None, 0),
        RestartStep::Defer(OccurrenceReason::PlayerCountUnknown)
    );
    let occupied = RestartInputs {
        players: PlayerEvidence::Occupied,
        ..inputs()
    };
    let limit = u64::from(policy.max_defer_minutes) * 60_000;
    assert_eq!(
        restart_step(occupied, &policy, Some(0), limit - 1),
        RestartStep::Defer(OccurrenceReason::PlayersPresent)
    );
    assert_eq!(
        restart_step(occupied, &policy, Some(0), limit),
        RestartStep::Skip(OccurrenceReason::PlayersPresent)
    );
    let empty = RestartInputs {
        players: PlayerEvidence::Empty,
        ..inputs()
    };
    assert_eq!(
        restart_step(empty, &policy, Some(0), 1),
        RestartStep::Restart
    );
    let busy = RestartInputs {
        operation_busy: true,
        players: PlayerEvidence::Empty,
        ..inputs()
    };
    assert_eq!(
        restart_step(busy, &policy, None, 0),
        RestartStep::Defer(OccurrenceReason::OperationBusy)
    );
    policy.max_defer_minutes = 0;
    assert_eq!(
        restart_step(busy, &policy, None, 0),
        RestartStep::Skip(OccurrenceReason::OperationBusy)
    );
}

#[test]
fn attempts_retry_within_budget_and_cancellation_never_retries() {
    let policy = MaintenancePolicy::default();
    let failed = AttemptResult {
        completed: false,
        cancelled: false,
        stop_completed: true,
    };
    assert_eq!(
        after_attempt(failed, 1, &policy, 1_000),
        AfterAttempt::Retry {
            next_unix_ms: 1_000 + u64::from(policy.retry_delay_seconds) * 1000
        }
    );
    assert_eq!(
        after_attempt(failed, 2, &policy, 1_000),
        AfterAttempt::Failed
    );
    let cancelled = AttemptResult {
        cancelled: true,
        ..failed
    };
    assert_eq!(
        after_attempt(cancelled, 1, &policy, 0),
        AfterAttempt::Cancelled
    );
    let done = AttemptResult {
        completed: true,
        ..failed
    };
    assert_eq!(after_attempt(done, 1, &policy, 0), AfterAttempt::Succeeded);
}

#[test]
fn status_reports_inactive_owner_blocked_adapter_and_next_run() {
    let capabilities = crate::core::server::ServerCapabilities {
        lifecycle: true,
        ..Default::default()
    };
    let mut catalog = MaintenanceCatalog::empty(instance());
    catalog.revision = 4;
    catalog.jobs.push(job(
        JobSchedule::Recurring {
            minutes_of_day: 0,
            weekdays: 0b0111_1111,
        },
        0,
    ));
    let mut backup = job(JobSchedule::OnceAt { unix_ms: 5 }, 0);
    backup.id = "job_0000000000000002".into();
    backup.definition.name = "Backup".into();
    backup.definition.action = MaintenanceAction::Backup;
    catalog.jobs.push(backup);
    let history = OccurrenceListing {
        occurrences: Vec::new(),
        unreadable: 0,
        truncated: false,
    };
    let inactive = job_statuses(&catalog, &history, &capabilities, false, 1_000);
    assert!(
        inactive.jobs[0]
            .blocker
            .as_deref()
            .is_some_and(|text| text.contains("inactive"))
    );
    assert!(inactive.jobs[0].next_run_unix_ms.is_some());
    assert!(
        inactive.jobs[1]
            .blocker
            .as_deref()
            .is_some_and(|text| text.contains("not available"))
    );
    let active = job_statuses(&catalog, &history, &capabilities, true, 1_000);
    assert_eq!(active.jobs[0].blocker, None);
    assert_eq!(active.catalog_revision, 4);
}

#[test]
fn public_examples_are_valid_catalog_and_occurrence_records() {
    let catalog: MaintenanceCatalog = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_scheduled_jobs.json"
    ))
    .unwrap();
    catalog.validate(&catalog.instance_id).unwrap();
    let occurrence: Occurrence = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/server_scheduled_occurrence.json"
    ))
    .unwrap();
    let job = catalog.job(&occurrence.job_id).unwrap();
    assert_eq!(occurrence.instance_id, catalog.instance_id);
    assert_eq!(occurrence.policy_revision, job.revision);
    assert_eq!(
        occurrence.occurrence_id,
        occurrence_id(
            &occurrence.instance_id,
            &occurrence.job_id,
            occurrence.policy_revision,
            &occurrence.trigger
        )
    );
    assert!(occurrence.state.terminal());
}
