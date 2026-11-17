use super::*;

#[test]
fn owner_lifetime_requests_have_no_implicit_process_targets() {
    for (wire, operation) in [
        (r#"{"operation":"detach_owner"}"#, Operation::DetachOwner {}),
        (
            r#"{"operation":"shutdown_owner"}"#,
            Operation::ShutdownOwner {},
        ),
    ] {
        assert_eq!(serde_json::from_str::<Operation>(wire).unwrap(), operation);
        let broadened = wire.replace(
            '}',
            ",\"instance_id\":\"server_00000000000000000000000000000001\"}",
        );
        assert!(serde_json::from_str::<Operation>(&broadened).is_err());
    }
}

pub(super) fn scope() -> Scope {
    Scope {
        context_key: "a".repeat(64),
        space_id: "space_alpha".into(),
        instance_id: "instance_alpha".into(),
        owner_id: "owner_alpha".into(),
        generation: 4,
    }
}

#[test]
fn scope_and_capability_are_strict_and_secret_debug_is_redacted() {
    scope().validate().unwrap();
    let mut invalid = scope();
    invalid.generation = 0;
    assert!(invalid.validate().is_err());
    invalid = scope();
    invalid.instance_id = "../other".into();
    assert!(invalid.validate().is_err());
    assert!(Capability::from_bytes([0; 32]).is_err());
    assert_eq!(
        format!("{:?}", Capability::from_bytes([42; 32]).unwrap()),
        "Capability([redacted])"
    );
    assert!(
        serde_json::from_str::<Request>(r#"{"request_id":1,"operation":{"operation":"start"}}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<Scope>(&format!(r#"{{"context_key":"{}","space_id":"a","instance_id":"b","owner_id":"c","generation":1,"uid":0}}"#, "a".repeat(64))).is_err());
}

#[test]
fn deployment_operations_enforce_bounded_identifiers_and_generations() {
    use super::ledger::validate_client;

    let valid_activation = ClientMessage::Submit {
        request: Request {
            request_id: 1,
            operation: Operation::ActivateDeployment {
                instance_id: "instance_alpha".into(),
                preparation_id: format!("prepared_{}", "a".repeat(64)),
                expected_selection_generation: 1,
                expected_activation_generation: 0,
                review_fingerprint: "b".repeat(64),
            },
        },
    };
    validate_client(&valid_activation).unwrap();
    let valid_resolution = ClientMessage::Submit {
        request: Request {
            request_id: 2,
            operation: Operation::ResolveDeployment {
                instance_id: "instance_alpha".into(),
                review_fingerprint: "c".repeat(64),
            },
        },
    };
    validate_client(&valid_resolution).unwrap();
    validate_client(&ClientMessage::Submit {
        request: Request {
            request_id: 3,
            operation: Operation::CancelDeployment { request_id: 2 },
        },
    })
    .unwrap();

    let mut invalid_activation = valid_activation;
    if let ClientMessage::Submit { request } = &mut invalid_activation
        && let Operation::ActivateDeployment { preparation_id, .. } = &mut request.operation
    {
        *preparation_id = format!("prepared_{}", "A".repeat(64));
    }
    assert!(validate_client(&invalid_activation).is_err());
    assert!(
        validate_client(&ClientMessage::Submit {
            request: Request {
                request_id: 4,
                operation: Operation::CancelDeployment { request_id: 0 },
            },
        })
        .is_err()
    );
}

#[test]
fn lifecycle_operations_round_trip_and_validate_boundaries() {
    use super::ledger::validate_client;

    let instance_id = format!("server_{}", "a".repeat(32));
    let operations = [
        Operation::InspectInstance {
            instance_id: instance_id.clone(),
        },
        Operation::StartInstance {
            instance_id: instance_id.clone(),
            expected_activation_generation: 1,
            review_fingerprint: "b".repeat(64),
        },
        Operation::StopInstance {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            force: true,
            timeout_ms: 120_000,
        },
        Operation::RestartInstance {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            expected_activation_generation: 2,
            review_fingerprint: "c".repeat(64),
            force: false,
            timeout_ms: 1,
        },
        Operation::StartHeadlessClients {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            headless_id: None,
        },
        Operation::StartHeadlessClients {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            headless_id: Some("hc_0123456789abcdef".into()),
        },
        Operation::StopHeadlessClients {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            headless_id: None,
            force: true,
            timeout_ms: 120_000,
        },
        Operation::RestartHeadlessClients {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            headless_id: Some("hc_0123456789abcdef".into()),
            timeout_ms: 1,
        },
        Operation::RunMaintenanceJob {
            instance_id: instance_id.clone(),
            job_id: "job_0123456789abcdef".into(),
            expected_policy_revision: 1,
        },
        Operation::CancelMaintenanceOccurrence {
            instance_id: instance_id.clone(),
            occurrence_id: format!("occ_{}", "a".repeat(32)),
        },
        Operation::InspectRcon {
            instance_id: instance_id.clone(),
        },
        Operation::TestRcon {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
        },
        Operation::SendRconCommand {
            instance_id: instance_id.clone(),
            expected_run_generation: 1,
            text: "x".repeat(1024),
            timeout_ms: 60_000,
        },
        Operation::CancelLifecycle { request_id: 1 },
    ];
    for (index, operation) in operations.into_iter().enumerate() {
        let message = ClientMessage::Submit {
            request: Request {
                request_id: index as u64 + 1,
                operation,
            },
        };
        validate_client(&message).unwrap();
        let encoded = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&encoded).unwrap(),
            message
        );
    }

    let invalid_operations = [
        Operation::InspectInstance {
            instance_id: "server_ABC".into(),
        },
        Operation::StartInstance {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_activation_generation: 0,
            review_fingerprint: "b".repeat(64),
        },
        Operation::StartInstance {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_activation_generation: 1,
            review_fingerprint: "B".repeat(64),
        },
        Operation::StopInstance {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 0,
            force: false,
            timeout_ms: 1,
        },
        Operation::StopInstance {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            force: false,
            timeout_ms: 0,
        },
        Operation::StopInstance {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            force: false,
            timeout_ms: 120_001,
        },
        Operation::RestartInstance {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            expected_activation_generation: 0,
            review_fingerprint: "c".repeat(64),
            force: false,
            timeout_ms: 1,
        },
        Operation::StartHeadlessClients {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 0,
            headless_id: None,
        },
        Operation::StartHeadlessClients {
            instance_id: "server_ABC".into(),
            expected_run_generation: 1,
            headless_id: None,
        },
        Operation::StartHeadlessClients {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            headless_id: Some("../hc".into()),
        },
        Operation::StopHeadlessClients {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            headless_id: None,
            force: false,
            timeout_ms: 0,
        },
        Operation::StopHeadlessClients {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 0,
            headless_id: None,
            force: false,
            timeout_ms: 1,
        },
        Operation::RestartHeadlessClients {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            headless_id: Some(String::new()),
            timeout_ms: 1,
        },
        Operation::RestartHeadlessClients {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            headless_id: None,
            timeout_ms: 120_001,
        },
        Operation::RunMaintenanceJob {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            job_id: "job_0123456789ABCDEF".into(),
            expected_policy_revision: 1,
        },
        Operation::RunMaintenanceJob {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            job_id: "job_0123456789abcdef".into(),
            expected_policy_revision: 0,
        },
        Operation::CancelMaintenanceOccurrence {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            occurrence_id: "occ_../x".into(),
        },
        Operation::InspectRcon {
            instance_id: "server_x".into(),
        },
        Operation::TestRcon {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 0,
        },
        Operation::SendRconCommand {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            text: "say -1 a
b"
            .into(),
            timeout_ms: 10_000,
        },
        Operation::SendRconCommand {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            text: "   ".into(),
            timeout_ms: 10_000,
        },
        Operation::SendRconCommand {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            text: "x".repeat(1025),
            timeout_ms: 10_000,
        },
        Operation::SendRconCommand {
            instance_id: "server_0123456789abcdef0123456789abcdef".into(),
            expected_run_generation: 1,
            text: "players".into(),
            timeout_ms: 999,
        },
        Operation::CancelLifecycle { request_id: 0 },
    ];
    for operation in invalid_operations {
        assert!(
            validate_client(&ClientMessage::Submit {
                request: Request {
                    request_id: 1,
                    operation,
                },
            })
            .is_err()
        );
    }
}
