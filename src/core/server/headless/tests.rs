use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::plan::{HeadlessPlanInputs, PARAMETER_FILE};
use super::runtime::parameter_bytes;
use super::*;
use crate::core::server::ServerStoreError;

fn client(id: &str, name: &str, order: u32) -> Arma3HeadlessClient {
    Arma3HeadlessClient {
        id: id.into(),
        name: name.into(),
        order,
        enabled: true,
        addons: HeadlessAddonSelection::default(),
        process: HeadlessProcessOverrides::default(),
        lifecycle: HeadlessLifecyclePolicy::default(),
    }
}

const FIRST: &str = "hc_0000000000000001";
const SECOND: &str = "hc_0000000000000002";

#[test]
fn definitions_serialize_compactly_and_reject_unknown_fields() {
    let value = serde_json::to_value(client(FIRST, "HC1", 0)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"id": FIRST, "name": "HC1", "order": 0})
    );
    let parsed: Arma3HeadlessClient = serde_json::from_value(value.clone()).unwrap();
    assert!(parsed.enabled && parsed.addons.inherit_required);
    let mut unknown = value;
    unknown["password"] = "secret".into();
    assert!(serde_json::from_value::<Arma3HeadlessClient>(unknown).is_err());
    let detailed = Arma3HeadlessClient {
        enabled: false,
        addons: HeadlessAddonSelection {
            inherit_required: false,
            include: vec!["RepoAlpha".into()],
            exclude: Vec::new(),
        },
        process: HeadlessProcessOverrides {
            cpu_count: Some(2),
            ex_threads: Some(3),
            max_mem: Some(4096),
            limit_fps: Some(60),
        },
        ..client(FIRST, "HC1", 0)
    };
    let round_trip: Arma3HeadlessClient =
        serde_json::from_str(&serde_json::to_string(&detailed).unwrap()).unwrap();
    assert_eq!(round_trip, detailed);
}

#[test]
fn validation_requires_unique_ids_profile_names_bounded_overrides_and_coherent_selection() {
    assert!(
        validate_headless_clients(&[client(FIRST, "HC1", 0), client(SECOND, "HC2", 1)], None)
            .is_ok()
    );
    for clients in [
        vec![client(FIRST, "HC1", 0), client(FIRST, "HC2", 1)],
        vec![client(FIRST, "HC1", 0), client(SECOND, "hc1", 1)],
        vec![client("hc_1", "HC1", 0)],
        vec![client("HC_000000000000000A", "HC1", 0)],
        vec![client(FIRST, "bad name", 0)],
        vec![client(FIRST, "../HC", 0)],
        vec![client(FIRST, "", 0)],
    ] {
        assert!(validate_headless_clients(&clients, None).is_err());
    }
    let many: Vec<_> = (0..=MAX_HEADLESS_CLIENTS)
        .map(|index| client(&format!("hc_{index:016x}"), &format!("HC{index}"), 0))
        .collect();
    assert!(validate_headless_clients(&many[..MAX_HEADLESS_CLIENTS], None).is_ok());
    assert!(validate_headless_clients(&many, None).is_err());
    for process in [
        HeadlessProcessOverrides {
            cpu_count: Some(0),
            ..Default::default()
        },
        HeadlessProcessOverrides {
            ex_threads: Some(2),
            ..Default::default()
        },
        HeadlessProcessOverrides {
            max_mem: Some(255),
            ..Default::default()
        },
        HeadlessProcessOverrides {
            limit_fps: Some(4),
            ..Default::default()
        },
    ] {
        let invalid = Arma3HeadlessClient {
            process,
            ..client(FIRST, "HC1", 0)
        };
        assert!(validate_headless_clients(&[invalid], None).is_err());
    }
    for addons in [
        HeadlessAddonSelection {
            inherit_required: false,
            include: Vec::new(),
            exclude: vec!["RepoAlpha".into()],
        },
        HeadlessAddonSelection {
            inherit_required: true,
            include: vec!["RepoAlpha".into()],
            exclude: vec!["repoalpha".into()],
        },
        HeadlessAddonSelection {
            inherit_required: true,
            include: vec!["RepoAlpha".into(), "REPOALPHA".into()],
            exclude: Vec::new(),
        },
        HeadlessAddonSelection {
            inherit_required: true,
            include: vec!["@mod_01".into()],
            exclude: Vec::new(),
        },
    ] {
        let invalid = Arma3HeadlessClient {
            addons,
            ..client(FIRST, "HC1", 0)
        };
        assert!(validate_headless_clients(&[invalid], None).is_err());
    }
}

#[test]
fn enabled_local_clients_need_loopback_admission_when_the_list_is_explicit() {
    let enabled = [client(FIRST, "HC1", 0)];
    assert!(validate_headless_clients(&enabled, Some(&["127.0.0.1".to_owned()])).is_ok());
    assert!(validate_headless_clients(&enabled, Some(&["10.0.0.5".to_owned()])).is_err());
    assert!(validate_headless_clients(&enabled, Some(&[])).is_err());
    let disabled = [Arma3HeadlessClient {
        enabled: false,
        ..client(FIRST, "HC1", 0)
    }];
    assert!(validate_headless_clients(&disabled, Some(&[])).is_ok());
}

#[test]
fn order_is_explicit_then_stable_by_id() {
    let clients = [
        client(SECOND, "HC2", 1),
        client("hc_0000000000000003", "HC3", 0),
        client(FIRST, "HC1", 1),
    ];
    let ordered: Vec<_> = ordered_headless_clients(&clients)
        .into_iter()
        .map(|client| client.name.as_str())
        .collect();
    assert_eq!(ordered, ["HC3", "HC1", "HC2"]);
}

#[test]
fn consumer_identity_binds_each_client_to_its_instance_only() {
    let instance = "server_0123456789abcdef0123456789abcdef";
    let consumer = consumer_instance_id(instance, FIRST);
    assert!(consumer_belongs_to(&consumer, instance));
    assert!(consumer_belongs_to(instance, instance));
    assert!(!consumer_belongs_to(
        &consumer,
        "server_0123456789abcdef0123456789abcde0"
    ));
    assert!(!consumer_belongs_to(&format!("{instance}-other"), instance));
    assert!(!consumer_belongs_to(&format!("{instance}x"), instance));
}

fn native(id: &str, name: &str, mods: &[&str]) -> NativeHeadlessClient {
    NativeHeadlessClient {
        id: id.into(),
        name: name.into(),
        mods: mods.iter().map(|item| (*item).into()).collect(),
        process: HeadlessProcessOverrides::default(),
        lifecycle: HeadlessLifecyclePolicy::default(),
    }
}

fn plans(
    clients: &[NativeHeadlessClient],
    password: Option<&str>,
) -> Result<Vec<HeadlessLaunchPlan>, ServerStoreError> {
    headless_launch_plans(
        clients,
        HeadlessPlanInputs {
            executable: Path::new("R:/Runtime/engine/arma3server_x64.exe"),
            directory: Path::new("R:/Runtime/engine"),
            state_root: Path::new("R:/Instance/runtime-state"),
            game_port: 2302,
            password_reference: password.map(str::to_owned),
        },
        |path| Ok(path.to_string_lossy().replace('\\', "/")),
        |root| {
            Ok(vec![(
                "HOME".to_owned(),
                root.join("home").to_string_lossy().replace('\\', "/"),
            )])
        },
        |value| {
            if value.contains([';', '/']) && !value.starts_with("foxy-content/") {
                Err(ServerStoreError::UnsafePath)
            } else {
                Ok(())
            }
        },
    )
}

#[test]
fn launch_plans_use_client_mode_with_isolated_roots_and_no_dedicated_arguments() {
    let mut tuned = native(SECOND, "HC2", &[]);
    tuned.process.limit_fps = Some(60);
    tuned.process.cpu_count = Some(2);
    let built = plans(
        &[
            native(
                FIRST,
                "HC1",
                &["foxy-content/@mod_01", "foxy-content/@mod_02"],
            ),
            tuned,
        ],
        None,
    )
    .unwrap();
    assert_eq!(
        built[0].arguments,
        [
            "-client",
            "-connect=127.0.0.1",
            "-port=2302",
            "-profiles=R:/Instance/runtime-state/headless/hc_0000000000000001/home/profiles",
            "-name=HC1",
            "-mod=foxy-content/@mod_01;foxy-content/@mod_02",
        ]
    );
    assert_eq!(
        built[1].arguments[5..],
        ["-cpuCount=2".to_owned(), "-limitFPS=60".to_owned()]
    );
    assert_ne!(built[0].state_root, built[1].state_root);
    assert_eq!(
        built[1].environment[0].1,
        "R:/Instance/runtime-state/headless/hc_0000000000000002/home"
    );
    for plan in &built {
        assert!(!plan.join_password && plan.parameter_file.is_none());
        for argument in &plan.arguments {
            for dedicated in [
                "-config=",
                "-cfg=",
                "-serverMod",
                "-autoInit",
                "-keysFolder",
                "-password",
                "-par=",
            ] {
                assert!(!argument.starts_with(dedicated), "{argument}");
            }
        }
    }
}

#[test]
fn join_passwords_travel_only_through_the_private_parameter_file() {
    let built = plans(&[native(FIRST, "HC1", &[])], Some("secret_reference")).unwrap();
    let plan = &built[0];
    assert!(plan.join_password);
    let parameter = plan.parameter_file.clone().unwrap();
    assert_eq!(
        parameter,
        PathBuf::from("R:/Instance/runtime-state/headless/hc_0000000000000001/private")
            .join(PARAMETER_FILE)
    );
    assert_eq!(
        plan.arguments.last().unwrap(),
        &format!("-par={}", parameter.to_string_lossy().replace('\\', "/"))
    );
    let rendered = serde_json::to_string(plan).unwrap();
    assert!(!rendered.contains("secret_reference"));
    assert!(!plan.arguments.iter().any(|item| item.contains("password")));
}

#[test]
fn launch_plans_refuse_corrupt_identities_and_unsafe_mods() {
    for clients in [
        vec![native(FIRST, "HC1", &[]), native(FIRST, "HC2", &[])],
        vec![native(FIRST, "HC1", &[]), native(SECOND, "hc1", &[])],
        vec![native("hc_bad", "HC1", &[])],
        vec![native(FIRST, "HC1", &["mods;-serverMod=x"])],
    ] {
        assert!(plans(&clients, None).is_err());
    }
}

#[test]
fn join_password_reference_reads_only_protected_forms() {
    let bindings = [("password".to_owned(), "secret_bound".to_owned())];
    assert_eq!(
        join_password_reference("hostname = \"A\";", &bindings).unwrap(),
        None
    );
    assert_eq!(
        join_password_reference("password = \"\";", &bindings).unwrap(),
        None
    );
    assert_eq!(
        join_password_reference("password = \"foxy-secret:secret_inline\";", &[]).unwrap(),
        Some("secret_inline".into())
    );
    assert_eq!(
        join_password_reference("Password = \"<redacted secret reference>\";", &bindings).unwrap(),
        Some("secret_bound".into())
    );
    assert_eq!(
        join_password_reference("password = \"<redacted secret reference>\";", &[]),
        Err(ServerStoreError::Corrupt)
    );
    assert_eq!(
        join_password_reference("password = \"plain\";", &bindings),
        Err(ServerStoreError::Corrupt)
    );
    assert!(join_password_reference("password = \"a\"; password = \"b\";", &bindings).is_err());
    assert!(join_password_reference("#include \"other.cfg\"", &bindings).is_err());
}

#[test]
fn readiness_needs_a_fresh_query_answer() {
    let now = Instant::now();
    assert_eq!(
        HeadlessReadiness::evaluate(false, Some(now), now),
        HeadlessReadiness::NoQueryEndpoint
    );
    assert_eq!(
        HeadlessReadiness::evaluate(true, None, now),
        HeadlessReadiness::NotResponding
    );
    assert_eq!(
        HeadlessReadiness::evaluate(true, Some(now), now + Duration::from_secs(5)),
        HeadlessReadiness::Ready
    );
    assert_eq!(
        HeadlessReadiness::evaluate(true, Some(now), now + Duration::from_secs(16)),
        HeadlessReadiness::Stale
    );
    assert!(HeadlessReadiness::Ready.refusal().is_none());
    assert!(HeadlessReadiness::NotResponding.refusal().is_some());
}

#[test]
fn parameter_files_refuse_values_the_engine_cannot_parse_safely() {
    assert_eq!(
        parameter_bytes("probe pass 1").unwrap(),
        b"-password=probe pass 1\n"
    );
    for bad in ["", " leading", "trailing ", "line\nbreak", "nul\0", "tab\t"] {
        assert!(parameter_bytes(bad).is_err(), "{bad:?}");
    }
    assert!(parameter_bytes(&"a".repeat(1025)).is_err());
}

#[cfg(any(windows, target_os = "linux"))]
#[test]
fn state_preparation_isolates_each_client_and_rewrites_only_its_parameter_file() {
    let directory = tempfile::tempdir().unwrap();
    let state_root = directory.path().join("runtime-state");
    std::fs::create_dir(&state_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let built = headless_launch_plans(
        &[native(FIRST, "HC1", &[])],
        HeadlessPlanInputs {
            executable: Path::new("engine/arma3server_x64.exe"),
            directory: Path::new("engine"),
            state_root: &state_root,
            game_port: 2302,
            password_reference: Some("secret_reference".into()),
        },
        |path| Ok(path.to_string_lossy().into_owned()),
        |_| Ok(Vec::new()),
        |_| Ok(()),
    )
    .unwrap();
    let plan = &built[0];
    prepare_headless_state(&state_root, plan, Some("first pass")).unwrap();
    let parameter = plan.parameter_file.clone().unwrap();
    assert_eq!(
        std::fs::read(&parameter).unwrap(),
        b"-password=first pass\n"
    );
    assert!(plan.profiles.is_dir());
    std::fs::write(plan.profiles.join("kept.txt"), b"profile").unwrap();
    prepare_headless_state(&state_root, plan, Some("second pass")).unwrap();
    assert_eq!(
        std::fs::read(&parameter).unwrap(),
        b"-password=second pass\n"
    );
    assert_eq!(
        std::fs::read(plan.profiles.join("kept.txt")).unwrap(),
        b"profile"
    );
    let mut unprotected = plan.clone();
    unprotected.join_password = false;
    unprotected.parameter_file = None;
    prepare_headless_state(&state_root, &unprotected, None).unwrap();
    assert!(!parameter.exists());
    let mut foreign = plan.clone();
    foreign.state_root = directory.path().join("elsewhere");
    assert_eq!(
        prepare_headless_state(&state_root, &foreign, Some("first pass")),
        Err(ServerStoreError::UnsafePath)
    );
    assert!(prepare_headless_state(&state_root, plan, None).is_err());
}

#[test]
fn headless_example_is_a_valid_draft_and_frozen_runtime_shape() {
    let example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/arma3_headless_clients.json"
    ))
    .unwrap();
    let instance: crate::core::server::ServerInstance =
        serde_json::from_value(example["instance"].clone()).unwrap();
    instance
        .validate("arma3", crate::core::server::ServerDefinitionKind::Arma3)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&instance).unwrap(),
        example["instance"]
    );
    let preview = crate::core::server::configuration::render(&instance).unwrap();
    assert!(
        preview.files[0]
            .contents
            .contains("headlessClients[] = {\"127.0.0.1\"};")
    );
    let frozen: Vec<NativeHeadlessClient> =
        serde_json::from_value(example["native_runtime_headless_clients"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&frozen).unwrap(),
        example["native_runtime_headless_clients"]
    );
    assert!(plans(&frozen, None).is_ok());
}

#[test]
fn lifecycle_policy_defaults_stay_off_the_wire_and_ranges_are_enforced() {
    let legacy: Arma3HeadlessClient =
        serde_json::from_value(serde_json::json!({"id": FIRST, "name": "HC1", "order": 0}))
            .unwrap();
    assert_eq!(legacy.lifecycle, HeadlessLifecyclePolicy::default());
    assert!(legacy.lifecycle.auto_start);
    let mut custom = client(FIRST, "HC1", 0);
    custom.lifecycle = HeadlessLifecyclePolicy {
        auto_start: false,
        start_delay_seconds: 30,
        connect_timeout_seconds: 300,
        max_restarts: 0,
    };
    let value = serde_json::to_value(&custom).unwrap();
    assert_eq!(
        value["lifecycle"],
        serde_json::json!({
            "auto_start": false,
            "start_delay_seconds": 30,
            "connect_timeout_seconds": 300,
            "max_restarts": 0
        })
    );
    assert_eq!(
        serde_json::from_value::<Arma3HeadlessClient>(value).unwrap(),
        custom
    );
    validate_headless_clients(std::slice::from_ref(&custom), None).unwrap();
    for (delay, timeout, restarts) in [(601, 120, 3), (0, 14, 3), (0, 901, 3), (0, 120, 11)] {
        let mut invalid = custom.clone();
        invalid.lifecycle.start_delay_seconds = delay;
        invalid.lifecycle.connect_timeout_seconds = timeout;
        invalid.lifecycle.max_restarts = restarts;
        assert!(validate_headless_clients(&[invalid], None).is_err());
    }
    let mut frozen = native(FIRST, "HC1", &[]);
    frozen.lifecycle.connect_timeout_seconds = 5;
    assert_eq!(plans(&[frozen], None), Err(ServerStoreError::Corrupt));
}

#[test]
fn restart_backoff_doubles_from_five_seconds_up_to_one_minute() {
    let delays: Vec<u64> = (0..7)
        .map(|previous| HeadlessLifecyclePolicy::restart_delay(previous).as_secs())
        .collect();
    assert_eq!(delays, [5, 10, 20, 40, 60, 60, 60]);
}

fn policy(auto_start: bool, delay: u16, max_restarts: u8) -> HeadlessLifecyclePolicy {
    HeadlessLifecyclePolicy {
        auto_start,
        start_delay_seconds: delay,
        max_restarts,
        ..HeadlessLifecyclePolicy::default()
    }
}

#[test]
fn automatic_start_waits_for_fresh_readiness_and_its_delay() {
    let now = std::time::Instant::now();
    let seconds = std::time::Duration::from_secs;
    let schedule = ClientSchedule::new(policy(true, 10, 3), None);
    assert_eq!(schedule.due(false, None, false, now), Due::WaitingForServer);
    assert_eq!(
        schedule.due(false, Some(now), false, now + seconds(30)),
        Due::WaitingForServer
    );
    assert_eq!(
        schedule.due(false, Some(now), true, now + seconds(5)),
        Due::At(now + seconds(10))
    );
    assert_eq!(
        schedule.due(false, Some(now), true, now + seconds(10)),
        Due::Now
    );
    assert_eq!(
        schedule.due(true, Some(now), true, now + seconds(10)),
        Due::Idle
    );
    assert_eq!(
        schedule.state(false, false, true),
        (HeadlessClientState::WaitingForServer, None)
    );
    assert_eq!(
        schedule.state(false, true, false),
        (
            HeadlessClientState::Stopped,
            Some(HeadlessReason::ServerNotRunning)
        )
    );

    let mut manual = ClientSchedule::new(policy(false, 0, 3), None);
    assert_eq!(manual.due(false, Some(now), true, now), Due::Idle);
    assert_eq!(
        manual.state(false, true, true),
        (
            HeadlessClientState::Stopped,
            Some(HeadlessReason::ManualStartRequired)
        )
    );
    manual.operator_start();
    assert_eq!(manual.due(false, Some(now), true, now), Due::Now);
}

#[test]
fn unexpected_exits_back_off_then_suspend_instead_of_looping() {
    let start = std::time::Instant::now();
    let seconds = std::time::Duration::from_secs;
    let mut schedule = ClientSchedule::new(policy(true, 0, 2), None);
    schedule.started();
    assert_eq!(
        schedule.exited(false, start),
        ExitDecision::Restart(seconds(5))
    );
    assert_eq!(
        schedule.due(false, Some(start), true, start + seconds(1)),
        Due::At(start + seconds(5))
    );
    assert_eq!(
        schedule.due(false, Some(start), false, start + seconds(5)),
        Due::WaitingForServer
    );
    assert_eq!(
        schedule.due(false, Some(start), true, start + seconds(5)),
        Due::Now
    );
    schedule.started();
    assert_eq!(
        schedule.exited(false, start + seconds(20)),
        ExitDecision::Restart(seconds(10))
    );
    schedule.started();
    assert_eq!(schedule.automatic_restarts(start + seconds(40)), 2);
    assert_eq!(
        schedule.exited(false, start + seconds(40)),
        ExitDecision::Suspended
    );
    assert_eq!(
        schedule.due(false, Some(start), true, start + seconds(3600)),
        Due::Idle
    );
    assert_eq!(
        schedule.state(false, true, true),
        (
            HeadlessClientState::RestartSuspended,
            Some(HeadlessReason::RestartBudgetExhausted)
        )
    );
    schedule.operator_start();
    assert_eq!(schedule.automatic_restarts(start + seconds(41)), 0);
    assert_eq!(
        schedule.due(false, Some(start), true, start + seconds(41)),
        Due::Now
    );

    let mut windowed = ClientSchedule::new(policy(true, 0, 1), None);
    assert_eq!(
        windowed.exited(false, start),
        ExitDecision::Restart(seconds(5))
    );
    windowed.started();
    assert_eq!(
        windowed.exited(false, start + seconds(HEADLESS_RESTART_WINDOW_SECONDS)),
        ExitDecision::Restart(seconds(5))
    );

    let mut disabled = ClientSchedule::new(policy(true, 0, 0), None);
    assert_eq!(disabled.exited(false, start), ExitDecision::RestartDisabled);
    assert!(!disabled.wanted());
    assert_eq!(disabled.due(false, Some(start), true, start), Due::Idle);
}

#[test]
fn requested_and_operator_stops_never_restart_or_spend_the_budget() {
    let now = std::time::Instant::now();
    let mut schedule = ClientSchedule::new(policy(true, 0, 1), None);
    assert_eq!(schedule.exited(true, now), ExitDecision::Requested);
    assert_eq!(schedule.automatic_restarts(now), 0);
    schedule.operator_stop();
    assert_eq!(schedule.exited(false, now), ExitDecision::Requested);
    assert_eq!(schedule.due(false, Some(now), true, now), Due::Idle);
    assert_eq!(schedule.reason(), Some(HeadlessReason::StoppedByOperator));

    let carried_stop = ClientSchedule::new(policy(true, 0, 3), Some(false));
    assert!(!carried_stop.wanted());
    assert_eq!(
        carried_stop.reason(),
        Some(HeadlessReason::StoppedByOperator)
    );
    let carried_start = ClientSchedule::new(policy(false, 0, 3), Some(true));
    assert_eq!(carried_start.due(false, Some(now), true, now), Due::Now);
}

#[test]
fn group_status_validation_bounds_clients_and_identities() {
    let entry = HeadlessClientStatus {
        id: FIRST.into(),
        name: "HC1".into(),
        state: HeadlessClientState::Running,
        wanted: true,
        reason: None,
        process_id: Some(7),
        started_unix_ms: Some(1),
        connection: Some(HeadlessConnection::Joined),
        joined_unix_ms: Some(2),
        starts: 1,
        automatic_restarts: 0,
        next_start_unix_ms: None,
        last_exit: None,
    };
    let group = HeadlessGroupStatus {
        ready_unix_ms: Some(1),
        clients: vec![entry.clone()],
    };
    assert!(group.valid());
    let encoded = serde_json::to_value(&group).unwrap();
    assert_eq!(encoded["clients"][0]["state"], "running");
    assert_eq!(encoded["clients"][0]["connection"], "joined");
    let mut bad = group.clone();
    bad.clients[0].id = "../hc".into();
    assert!(!bad.valid());
    let mut oversized = group;
    oversized.clients = vec![entry; MAX_HEADLESS_CLIENTS + 1];
    assert!(!oversized.valid());

    let example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/arma3_headless_status.json"
    ))
    .unwrap();
    let parsed: HeadlessGroupStatus = serde_json::from_value(example.clone()).unwrap();
    assert!(parsed.valid());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), example);
    assert_eq!(
        parsed.clients[1].reason,
        Some(HeadlessReason::RestartBudgetExhausted)
    );
}
