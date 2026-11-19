use super::*;
use crate::core::server::ServerDefinitionKind;

fn content_plan(instance_id: ServerInstanceId) -> Box<ContentDeploymentPlan> {
    Box::new(ContentDeploymentPlan {
        game_space_id: "alpha".into(),
        instance_id,
        instance_revision: 1,
        catalog_revision: 1,
        content_sources_fingerprint: Some("fingerprint".into()),
        fingerprint: "plan-fingerprint".into(),
        bindings: vec![],
        blockers: vec![],
        deployment_ready: false,
        downloads_started: false,
        writes_performed: false,
    })
}

fn scope(request_id: &str) -> ServerTaskScope {
    ServerTaskScope {
        root: "root".into(),
        space_id: "alpha".into(),
        game_id: "arma3".into(),
        request_id: request_id.into(),
        owner: None,
    }
}

fn monitoring_report(
    mut status: crate::core::server::InstanceRuntimeStatus,
    query_state: crate::core::server::QueryState,
    log_text: &str,
    cpu_percent: f32,
) -> crate::core::server::InstanceMonitoring {
    use crate::core::server::monitoring::{QueryObservation, ResourcePoint, ResourceSample};
    use crate::core::server::{
        InstanceMonitoring, LogLine, LogSnapshot, MonitoringHealth, MonitoringSnapshot, QueryState,
        ResourceState,
    };

    status.process_exists = true;
    status.phase = crate::core::server::InstanceRuntimePhase::Running;
    status.observation_age_ms = 0;
    InstanceMonitoring {
        observation_unavailable: false,
        status: status.clone(),
        observation: Some(MonitoringSnapshot {
            schema_version: 1,
            context_fingerprint: "a".repeat(64),
            instance_id: status.instance_id.clone(),
            owner_id: status.owner_id.clone(),
            owner_generation: status.owner_generation,
            run_generation: status.run_generation,
            activation_generation: status.activation_generation,
            runtime_id: status.runtime_id.clone(),
            sampled_unix_ms: 10_000,
            query: QueryObservation {
                state: query_state,
                attempted_unix_ms: Some(10_000),
                last_success_unix_ms: (query_state == QueryState::Responding).then_some(10_000),
                next_attempt_unix_ms: Some(12_000),
                consecutive_failures: 0,
                last_good: None,
                attempt_history: Vec::new(),
            },
            resources: vec![ResourcePoint {
                sampled_unix_ms: 10_000,
                sample: ResourceSample {
                    pid: Some(12),
                    process_start_time_secs: Some(10),
                    cpu_percent: Some(cpu_percent),
                    resident_bytes: Some(2048),
                    uptime_ms: Some(1000),
                    state: ResourceState::Available,
                },
            }],
            logs: LogSnapshot {
                entries: vec![LogLine {
                    sequence: 1,
                    stream: "engine_0".into(),
                    text: log_text.into(),
                }],
                next_sequence: 2,
                truncated: false,
                error: None,
            },
            headless: None,
        }),
        observation_age_ms: Some(0),
        query_age_ms: Some(0),
        query_ready: query_state == QueryState::Responding,
        health: if query_state == QueryState::Responding {
            MonitoringHealth::Healthy
        } else {
            MonitoringHealth::Degraded
        },
    }
}

#[test]
fn runtime_results_require_the_selected_instance_and_captured_owner_generation() {
    let id = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainRepo")
        .unwrap()
        .id;
    let other = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "RepoAlpha")
        .unwrap()
        .id;
    let owner = foxy_supervisor::control::Scope {
        context_key: "a".repeat(64),
        space_id: "alpha".into(),
        instance_id: "space_catalog".into(),
        owner_id: "owner_alpha".into(),
        generation: 3,
    };
    let mut captured = scope("lifecycle");
    captured.owner = Some((owner.owner_id.clone(), owner.generation));
    let result = |monitoring| {
        Ok(ServerTaskResult {
            scope: captured.clone(),
            output: ServerTaskOutput::InstanceMonitoring(monitoring),
        })
    };
    let status = crate::core::server::InstanceRuntimeStatus::not_owned(id.clone(), &owner);
    let monitoring = monitoring_report(
        status.clone(),
        crate::core::server::QueryState::Responding,
        "server ready",
        12.5,
    );
    let mut state = ServerViewState {
        selected: Some(id.clone()),
        ..Default::default()
    };
    assert!(!state.finish(
        &captured,
        result(monitoring_report(
            crate::core::server::InstanceRuntimeStatus::not_owned(other.clone(), &owner),
            crate::core::server::QueryState::Responding,
            "server ready",
            12.5,
        ))
    ));
    let mut stale = monitoring.clone();
    stale.status.owner_generation += 1;
    assert!(!state.finish(&captured, result(stale)));
    assert!(state.lifecycle_status.is_none());
    assert!(state.finish(&captured, result(monitoring)));
    assert!(state.lifecycle_status.is_some());
    assert_eq!(
        state.lifecycle_status.as_ref().unwrap().owner_id,
        owner.owner_id
    );
    assert_eq!(
        state
            .lifecycle_monitoring
            .as_ref()
            .unwrap()
            .status
            .instance_id,
        id
    );
    state.selected = Some(other.clone());
    assert!(!state.finish(
        &captured,
        Ok(ServerTaskResult {
            scope: captured.clone(),
            output: ServerTaskOutput::InstanceRuntimeLogsOpened(id),
        })
    ));
}

#[test]
fn dirty_edits_block_selection_and_discard_restores_the_baseline() {
    let first = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let second =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "SecondServer").unwrap();
    let mut state = ServerViewState {
        catalog: Some(ServerCatalog {
            schema_version: 1,
            game_space_id: "alpha".into(),
            game_id: "arma3".into(),
            revision: 1,
            installations: vec![],
            instances: vec![first.clone(), second.clone()],
        }),
        ..Default::default()
    };
    assert!(state.select(&first.id));
    state.editor.as_mut().unwrap().draft.display_name = "UnsavedServer".into();
    assert!(state.dirty());
    assert!(!state.select(&second.id));
    assert_eq!(state.selected.as_ref(), Some(&first.id));
    state.discard();
    assert!(!state.dirty());
    assert_eq!(state.editor.as_ref().unwrap().draft, first);
    state.monitoring_search = "failed".into();
    state.monitoring_paused = true;
    state.monitoring_auto_scroll = false;
    assert!(state.select(&second.id));
    assert!(state.lifecycle_monitoring.is_none());
    assert!(state.monitoring_search.is_empty());
    assert!(!state.monitoring_paused);
    assert!(state.monitoring_auto_scroll);
}

#[test]
fn paused_monitoring_freezes_only_same_run_logs_and_new_runs_resume_them() {
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let owner = foxy_supervisor::control::Scope {
        context_key: "a".repeat(64),
        space_id: "alpha".into(),
        instance_id: "space_catalog".into(),
        owner_id: "owner_alpha".into(),
        generation: 3,
    };
    let mut status =
        crate::core::server::InstanceRuntimeStatus::not_owned(instance.id.clone(), &owner);
    status.run_generation = 4;
    let previous = monitoring_report(
        status.clone(),
        crate::core::server::QueryState::Responding,
        "old log line",
        1.0,
    );
    let mut state = ServerViewState {
        lifecycle_monitoring: Some(previous),
        monitoring_search: "old".into(),
        monitoring_paused: true,
        monitoring_auto_scroll: false,
        ..Default::default()
    };

    state.update_monitoring(monitoring_report(
        status.clone(),
        crate::core::server::QueryState::Timeout,
        "new same-run log line",
        25.0,
    ));
    let same_run = state.lifecycle_monitoring.as_ref().unwrap();
    let observation = same_run.observation.as_ref().unwrap();
    assert_eq!(observation.logs.entries[0].text, "old log line");
    assert_eq!(
        observation.query.state,
        crate::core::server::QueryState::Timeout
    );
    assert_eq!(observation.resources[0].sample.cpu_percent, Some(25.0));
    assert!(state.monitoring_paused);

    status.run_generation += 1;
    state.update_monitoring(monitoring_report(
        status,
        crate::core::server::QueryState::Responding,
        "new run log line",
        2.0,
    ));
    let new_run = state.lifecycle_monitoring.as_ref().unwrap();
    assert_eq!(
        new_run.observation.as_ref().unwrap().logs.entries[0].text,
        "new run log line"
    );
    assert!(!state.monitoring_paused);
    assert!(state.monitoring_search.is_empty());
    assert!(state.monitoring_auto_scroll);
}

#[test]
fn editor_field_identity_is_distinct_between_spaces_and_instances() {
    let first = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let second =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "SecondServer").unwrap();
    let mut other_space = first.clone();
    other_space.game_space_id = "beta".into();
    assert_ne!(
        ServerEditor::new(first.clone()).field_scope(),
        ServerEditor::new(second).field_scope()
    );
    assert_ne!(
        ServerEditor::new(first).field_scope(),
        ServerEditor::new(other_space).field_scope()
    );
}

#[test]
fn errors_preserve_edits_and_finished_workers_remain_busy_until_consumed() {
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let mut editor = ServerEditor::new(instance);
    editor.draft.display_name = "UnsavedServer".into();
    let scope = ServerTaskScope {
        root: "root".into(),
        space_id: "alpha".into(),
        game_id: "arma3".into(),
        request_id: "new-request".into(),
        owner: None,
    };
    let worker = std::thread::spawn(|| Err(ServerStoreError::StaleRevision));
    while !worker.is_finished() {
        std::thread::yield_now();
    }
    let mut state = ServerViewState {
        editor: Some(editor),
        task: Some(ServerTask {
            scope: scope.clone(),
            passive: false,
            restore_focus: None,
            worker,
            stage_control: None,
            kind: None,
            maintenance_poll: false,
            rcon_poll: false,
        }),
        ..Default::default()
    };
    assert!(state.busy());
    assert!(state.controls_busy());
    state.discard();
    assert!(state.dirty());
    let task = state.task.take().unwrap();
    state.finish(&scope, task.worker.join().unwrap());
    assert!(!state.busy());
    assert!(state.dirty());
    assert_eq!(state.error, Some(ServerStoreError::StaleRevision));
    assert_eq!(
        state.editor.as_ref().unwrap().draft.display_name,
        "UnsavedServer"
    );
}

#[test]
fn passive_monitoring_keeps_controls_available_and_queues_only_first_action() {
    use crate::ui::app::server::ServerTaskAction;

    let scope = scope("poll");
    let worker = std::thread::spawn(|| Err(ServerStoreError::Cancelled));
    let mut state = ServerViewState {
        task: Some(ServerTask {
            scope,
            passive: true,
            restore_focus: None,
            worker,
            stage_control: None,
            kind: None,
            maintenance_poll: false,
            rcon_poll: false,
        }),
        ..Default::default()
    };

    assert!(state.busy());
    assert!(!state.controls_busy());
    assert!(state.queue_passive_action(ServerTaskAction::LoadMissionLibrary));
    assert!(state.controls_busy());
    assert!(!state.queue_passive_action(ServerTaskAction::Catalog(None)));
    assert!(matches!(
        state.queued_action.as_deref(),
        Some(ServerTaskAction::LoadMissionLibrary)
    ));
    assert!(state.busy());
    let _ = state.task.take().unwrap().worker.join();
}

#[test]
fn a_prior_visit_result_cannot_replace_the_newer_editor() {
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let scope = ServerTaskScope {
        root: "root".into(),
        space_id: "alpha".into(),
        game_id: "arma3".into(),
        request_id: "new-request".into(),
        owner: None,
    };
    let mut old = scope.clone();
    old.request_id = "old-request".into();
    let mut state = ServerViewState {
        editor: Some(ServerEditor::new(instance.clone())),
        ..Default::default()
    };
    let result = ServerTaskResult {
        scope: old,
        output: ServerTaskOutput::Catalog {
            catalog: ServerCatalog {
                schema_version: 1,
                game_space_id: "alpha".into(),
                game_id: "arma3".into(),
                revision: 0,
                installations: vec![],
                instances: vec![],
            },
            selected: None,
            saved: false,
            migration_pending: false,
        },
    };
    assert!(!state.finish(&scope, Ok(result)));
    assert_eq!(state.editor.as_ref().unwrap().draft, instance);
    assert!(!state.loaded);
}

#[test]
fn add_server_opens_setup_only_after_a_current_catalog_save() {
    use crate::ui::app::server::ServerPage;

    let instance = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer")
        .expect("valid server draft");
    let current = scope("new-draft");
    let result = |scope, saved| ServerTaskResult {
        scope,
        output: ServerTaskOutput::Catalog {
            catalog: ServerCatalog {
                schema_version: 1,
                game_space_id: "alpha".into(),
                game_id: "arma3".into(),
                revision: 1,
                installations: vec![],
                instances: vec![instance.clone()],
            },
            selected: Some(instance.id.clone()),
            saved,
            migration_pending: false,
        },
    };
    let mut state = ServerViewState {
        page: ServerPage::Add,
        new_name: instance.display_name.clone(),
        ..Default::default()
    };
    assert!(!state.finish(&current, Ok(result(scope("prior-draft"), true))));
    assert!(state.finish(&current, Ok(result(current.clone(), false))));
    assert_eq!(state.page, ServerPage::Add);
    assert_eq!(state.new_name, instance.display_name);
    assert!(state.finish(&current, Ok(result(current.clone(), true))));
    assert_eq!(state.page, ServerPage::Setup);
    assert_eq!(state.selected, Some(instance.id));
    assert!(state.new_name.is_empty());
}

#[test]
fn read_only_results_retain_entered_fields_and_the_loaded_catalog() {
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let scope = ServerTaskScope {
        root: "root".into(),
        space_id: "alpha".into(),
        game_id: "arma3".into(),
        request_id: "request".into(),
        owner: None,
    };
    let mut editor = ServerEditor::new(instance.clone());
    editor.draft.display_name = "UnsavedServer".into();
    let mut state = ServerViewState {
        editor: Some(editor),
        installation_directory: "operator-input".into(),
        ..Default::default()
    };
    let installation = serde_json::from_value(serde_json::json!({
            "id": crate::core::server::InstallationId::new(), "game": "arma3", "platform": "windows_x64",
            "directory": "R:/Mods/Server", "executable": "arma3server_x64.exe", "ownership": "adopted",
            "build_evidence": "executable_fingerprint_only", "evidence": {"bytes":128,"blake3":"a".repeat(64)}
        })).unwrap();
    state.finish(
        &scope,
        Ok(ServerTaskResult {
            scope: scope.clone(),
            output: ServerTaskOutput::Inspection(installation, Box::default()),
        }),
    );
    assert!(state.inspection.is_some());
    state.finish(
        &scope,
        Ok(ServerTaskResult {
            scope: scope.clone(),
            output: ServerTaskOutput::Preflight(Box::new(DeploymentPreflight {
                game_space_id: "alpha".into(),
                instance_id: instance.id,
                instance_revision: 1,
                catalog_revision: 1,
                installation: None,
                tested_engine_layout: None,
                installation_consumers: None,
                installation_recovery: None,
                configuration: crate::core::server::ActiveConfigurationEvidence {
                    status: "not_selected",
                    generation: Some(0),
                    revision_id: None,
                    integrity_valid: None,
                    matches_current_draft: None,
                },
                runtime_prerequisites: crate::core::server::RuntimePrerequisites::inspect(),
                paths: vec![],
                blockers: vec![],
                deployment_ready: false,
                writes_performed: false,
            })),
        }),
    );
    assert!(state.preflight.is_some());
    assert!(state.catalog.is_none());
    assert!(state.dirty());
    assert_eq!(
        state.editor.as_ref().unwrap().draft.display_name,
        "UnsavedServer"
    );
    assert_eq!(state.installation_directory, "operator-input");
}

#[test]
fn selecting_another_instance_clears_loaded_sources_bindings_and_preview() {
    let first = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let second =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "SecondServer").unwrap();
    let mut state = ServerViewState {
        catalog: Some(ServerCatalog {
            schema_version: 1,
            game_space_id: "alpha".into(),
            game_id: "arma3".into(),
            revision: 1,
            installations: vec![],
            instances: vec![first.clone(), second.clone()],
        }),
        content_stage_selection: Some("stage_previous".into()),
        content_stage_list: Some(crate::core::server::ContentStageList {
            stage_ids: vec!["stage_previous".into()],
            unrecognized_entries: 0,
            integrity_verified: false,
            writes_performed: false,
        }),
        ..Default::default()
    };
    assert!(state.select(&first.id));
    state.content_sources = Some(ContentSourceSnapshot {
        sources: crate::core::server::ContentSources::default(),
        fingerprint: Some("fingerprint".into()),
    });
    state.content_plan = Some(content_plan(first.id));
    state.content_bindings.push(ServerAddonBinding {
        source_id: "RepoAlpha".into(),
        role: crate::core::server::ServerAddonRole::Required,
        order: 0,
    });

    assert!(state.select(&second.id));
    assert!(state.content_sources.is_none());
    assert!(state.content_bindings.is_empty());
    assert!(state.content_plan.is_none());
    assert!(state.content_stage_list.is_none());
    assert!(state.content_stage_selection.is_none());
}

#[test]
fn refreshing_sources_preserves_bindings_and_marks_source_drift_stale() {
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let binding = ServerAddonBinding {
        source_id: "RepoAlpha".into(),
        role: crate::core::server::ServerAddonRole::Required,
        order: 0,
    };
    let saved_fingerprint = "binding-fingerprint".to_string();
    let mut state = ServerViewState {
        editor: Some(ServerEditor::new(instance.clone())),
        content_plan: Some(content_plan(instance.id.clone())),
        saved_content_bindings: Some(ContentBindingsSnapshot {
            document: Some(crate::core::server::ContentBindingsDocument {
                schema_version: 1,
                game_space_id: "alpha".into(),
                game_id: "arma3".into(),
                instance_id: instance.id.clone(),
                generation: 1,
                instance_revision: 1,
                catalog_revision: 1,
                catalog_fingerprint: "catalog-fingerprint".into(),
                content_sources_fingerprint: Some("old-fingerprint".into()),
                bindings: vec![],
            }),
            fingerprint: Some(saved_fingerprint.clone()),
            review_current: true,
            issues: vec![],
        }),
        ..Default::default()
    };
    state.content_bindings.push(binding.clone());
    let task_scope = scope("refresh");

    assert!(state.finish(
        &task_scope,
        Ok(ServerTaskResult {
            scope: task_scope.clone(),
            output: ServerTaskOutput::ContentSources(ContentSourceSnapshot {
                sources: crate::core::server::ContentSources::default(),
                fingerprint: Some("new-fingerprint".into()),
            }),
        }),
    ));
    assert_eq!(state.content_bindings, [binding]);
    assert!(state.content_plan.is_none());
    let saved = state.saved_content_bindings.as_ref().unwrap();
    assert_eq!(
        saved.fingerprint.as_deref(),
        Some(saved_fingerprint.as_str())
    );
    assert!(!saved.review_current);
    assert!(saved.issues.contains(
        &"Content source registrations changed since these bindings were reviewed. Review their identities and paths again."
    ));
}

#[test]
fn reloading_an_empty_binding_snapshot_retains_the_save_baseline() {
    let task_scope = scope("reload");
    let snapshot = ContentBindingsSnapshot {
        document: None,
        fingerprint: None,
        review_current: false,
        issues: vec![],
    };
    let mut state = ServerViewState::default();

    assert!(state.finish(
        &task_scope,
        Ok(ServerTaskResult {
            scope: task_scope.clone(),
            output: ServerTaskOutput::ContentBindings {
                sources: ContentSourceSnapshot {
                    sources: crate::core::server::ContentSources::default(),
                    fingerprint: Some("current-sources".into()),
                },
                snapshot,
            },
        }),
    ));
    assert!(state.content_bindings.is_empty());
    assert!(state.saved_content_bindings.is_some());
    assert!(state.content_sources.is_some());
}

#[test]
fn a_stale_content_plan_result_cannot_replace_the_current_preview() {
    let instance =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let current_scope = scope("current");
    let stale_scope = scope("stale");
    let mut state = ServerViewState {
        editor: Some(ServerEditor::new(instance.clone())),
        content_plan: Some(content_plan(instance.id.clone())),
        ..Default::default()
    };

    assert!(!state.finish(
        &current_scope,
        Ok(ServerTaskResult {
            scope: stale_scope,
            output: ServerTaskOutput::ContentPlan(content_plan(instance.id)),
        }),
    ));
    assert_eq!(
        state.content_plan.as_ref().unwrap().fingerprint,
        "plan-fingerprint"
    );
}

#[test]
fn stale_stage_completion_cannot_replace_the_current_report() {
    let current_scope = scope("current-stage");
    let stale_scope = scope("stale-stage");
    let current = crate::core::server::ContentStageReport {
        stage_id: "stage_current".into(),
        directory: "stage-current".into(),
        fingerprint: "current-fingerprint".into(),
        files: 1,
        bytes: 10,
        verified: true,
        published: true,
        reused_existing: false,
        deployment_ready: false,
        active_deployment_changed: false,
        downloads_started: false,
    };
    let stale = crate::core::server::ContentStageReport {
        stage_id: "stage_stale".into(),
        directory: "stage-stale".into(),
        fingerprint: "stale-fingerprint".into(),
        files: 2,
        bytes: 20,
        verified: true,
        published: true,
        reused_existing: false,
        deployment_ready: false,
        active_deployment_changed: false,
        downloads_started: false,
    };
    let mut state = ServerViewState {
        content_stage_report: Some(current),
        ..Default::default()
    };
    assert!(!state.finish(
        &current_scope,
        Ok(ServerTaskResult {
            scope: stale_scope,
            output: ServerTaskOutput::ContentStageReport(stale),
        }),
    ));
    assert_eq!(
        state.content_stage_report.as_ref().unwrap().stage_id,
        "stage_current"
    );
}

#[test]
fn invalidating_a_stage_clears_plan_report_and_selected_inventory() {
    let mut state = ServerViewState {
        content_stage_report: Some(crate::core::server::ContentStageReport {
            stage_id: "stage_test".into(),
            directory: "stage-test".into(),
            fingerprint: "fingerprint".into(),
            files: 0,
            bytes: 0,
            verified: false,
            published: false,
            reused_existing: false,
            deployment_ready: false,
            active_deployment_changed: false,
            downloads_started: false,
        }),
        content_stage_list: Some(crate::core::server::ContentStageList {
            stage_ids: vec!["stage_test".into()],
            unrecognized_entries: 0,
            integrity_verified: false,
            writes_performed: false,
        }),
        content_stage_selection: Some("stage_test".into()),
        ..Default::default()
    };
    state.invalidate_content_stage();
    assert!(state.content_stage_plan.is_none());
    assert!(state.content_stage_report.is_none());
    assert!(state.content_stage_list.is_none());
    assert!(state.content_stage_selection.is_none());
}

#[test]
fn selecting_another_instance_clears_deployment_record_scope() {
    let first = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer").unwrap();
    let second =
        ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "SecondServer").unwrap();
    let mut state = ServerViewState {
        catalog: Some(ServerCatalog {
            schema_version: 1,
            game_space_id: "alpha".into(),
            game_id: "arma3".into(),
            revision: 1,
            installations: vec![],
            instances: vec![first.clone(), second.clone()],
        }),
        deployment_record_state: Some(DeploymentRecordState {
            schema_version: 1,
            game_space_id: "alpha".into(),
            instance_id: first.id.clone(),
            generation: 3,
            selected: Some("deployment_old".into()),
            records: vec!["deployment_old".into()],
        }),
        deployment_record_inspection_selection: Some("deployment_old".into()),
        dialog: Some(ServerDialog::SelectDeploymentRecord),
        ..Default::default()
    };
    assert!(state.select(&first.id));
    assert!(state.select(&second.id));
    assert!(state.deployment_record_state.is_none());
    assert!(state.deployment_record_inspection_selection.is_none());
    assert!(state.deployment_record_plan.is_none());
    assert!(state.deployment_record_verification.is_none());
    assert!(state.deployment_record_commit.is_none());
    assert!(state.deployment_record_selection_plan.is_none());
    assert!(state.dialog.is_none());
}

#[test]
fn reviewed_record_selection_rejects_changed_inventory_scope_generation_and_ids() {
    let id = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer")
        .unwrap()
        .id;
    let inventory = DeploymentRecordState {
        schema_version: 1,
        game_space_id: "alpha".into(),
        instance_id: id.clone(),
        generation: 4,
        selected: Some("deployment_previous".into()),
        records: vec!["deployment_target".into(), "deployment_previous".into()],
    };
    let fingerprint = blake3::hash(&serde_json::to_vec(&inventory).unwrap())
        .to_hex()
        .to_string();
    let matches = |state: &DeploymentRecordState| {
        deployment_record_selection_matches(
            state,
            &id,
            "alpha",
            4,
            Some("deployment_previous"),
            "deployment_target",
            &fingerprint,
        )
    };
    assert!(matches(&inventory));
    let mut changed = inventory.clone();
    changed.generation += 1;
    assert!(!matches(&changed));
    changed = inventory.clone();
    changed.game_space_id = "beta".into();
    assert!(!matches(&changed));
    changed = inventory.clone();
    changed.selected = Some("deployment_changed".into());
    assert!(!matches(&changed));
    changed = inventory.clone();
    changed.records.remove(0);
    assert!(!matches(&changed));
    changed = inventory.clone();
    changed.records.push("deployment_other".into());
    assert!(!matches(&changed));
}

#[test]
fn cancelling_record_selection_review_closes_dialog_and_drops_plan() {
    let mut state = ServerViewState {
        dialog: Some(ServerDialog::SelectDeploymentRecord),
        ..Default::default()
    };
    state.invalidate_deployment_record_selection_plan();
    assert!(state.deployment_record_selection_plan.is_none());
    assert!(state.dialog.is_none());
}

#[test]
fn deployment_preparation_invalidation_clears_review_and_inspection_state() {
    let mut state = ServerViewState {
        preparation_selection: Some("prepared_test".into()),
        ..Default::default()
    };
    state.invalidate_deployment_preparation();
    assert!(state.preparation_plan.is_none());
    assert!(state.preparation_list.is_none());
    assert!(state.preparation_selection.is_none());
    assert!(state.preparation_report.is_none());
}

#[test]
fn native_activation_results_require_the_captured_owner_and_selected_instance() {
    let id = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "MainServer")
        .unwrap()
        .id;
    let other = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "SecondServer")
        .unwrap()
        .id;
    let mut captured = scope("activation");
    captured.owner = Some(("owner_alpha".into(), 3));
    let outcome = |target| DeploymentOperationOutcome {
        instance_id: target,
        status: "committed".into(),
        activation_generation: Some(1),
        runtime_id: Some(format!("runtime_{}", "a".repeat(64))),
        runtime_verified: true,
        journal_pending: false,
        intent_pending: false,
        writes_performed: true,
        process_started: false,
    };
    let result = |scope: ServerTaskScope, target| {
        Ok(ServerTaskResult {
            scope,
            output: ServerTaskOutput::NativeActivationOutcome(outcome(target)),
        })
    };
    let mut state = ServerViewState {
        selected: Some(id.clone()),
        ..Default::default()
    };
    assert!(!state.finish(&captured, result(captured.clone(), other)));
    let mut foreign = captured.clone();
    foreign.owner = Some(("owner_alpha".into(), 4));
    assert!(!state.finish(&captured, result(foreign, id.clone())));
    assert!(state.activation_outcome.is_none());
    assert!(state.finish(&captured, result(captured.clone(), id)));
    assert!(state.activation_outcome.is_some());
    state.invalidate_deployment_preparation();
    assert!(state.activation_outcome.is_none());
    assert!(state.activation_status.is_none());
    assert!(state.activation_plan.is_none());
    assert!(state.activation_resolution.is_none());
}
