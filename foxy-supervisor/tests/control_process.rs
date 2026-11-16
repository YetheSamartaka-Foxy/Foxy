#![cfg(any(windows, target_os = "linux"))]

use foxy_supervisor::control::{
    Capability, ClientMessage, ClientSession, Ledger, Operation, OperationResult, Request,
    RequestReply, Scope, ServerMessage, ServerSession,
    transport::{IoBudget, Listener, LocalEndpoint, connect},
};
use serde::{Deserialize, Serialize};
use std::io;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CAPABILITY: [u8; 32] = [0x5a; 32];

#[derive(Serialize, Deserialize)]
struct HelperConfig {
    capability: [u8; 32],
    scope: Scope,
    endpoint: String,
    ready_file: String,
    counter_file: String,
    drop_first_response: bool,
    connection_limit: usize,
}

struct ServerProcess {
    child: Child,
    _directory: TempDir,
    endpoint: LocalEndpoint,
    counter_file: std::path::PathBuf,
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scope() -> Scope {
    Scope {
        context_key: "a".repeat(64),
        space_id: "space_alpha".into(),
        instance_id: "instance_alpha".into(),
        owner_id: "owner_alpha".into(),
        generation: 7,
    }
}

fn with_budget<T>(f: impl FnOnce(IoBudget<'_>) -> io::Result<T>) -> io::Result<T> {
    let cancelled = AtomicBool::new(false);
    f(IoBudget {
        deadline: Instant::now() + Duration::from_secs(2),
        cancelled: &cancelled,
    })
}

fn start_server(drop_first_response: bool, connection_limit: usize) -> ServerProcess {
    let directory = tempfile::Builder::new()
        .prefix("f-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let suffix = format!("{:016x}", rand::random::<u64>());
    #[cfg(windows)]
    let endpoint = LocalEndpoint::NamedPipe(format!(r"\\.\pipe\foxy-control-{suffix}"));
    #[cfg(target_os = "linux")]
    let endpoint = LocalEndpoint::UnixSocket(directory.path().join(format!("c-{suffix}")));
    #[cfg(not(any(windows, target_os = "linux")))]
    let endpoint = LocalEndpoint::NamedPipe(String::new());
    let ready_file = directory.path().join("ready");
    let counter_file = directory.path().join("counter");
    let config = HelperConfig {
        capability: CAPABILITY,
        scope: scope(),
        endpoint: match &endpoint {
            LocalEndpoint::NamedPipe(value) => value.clone(),
            LocalEndpoint::UnixSocket(path) => path.to_string_lossy().into_owned(),
        },
        ready_file: ready_file.to_string_lossy().into_owned(),
        counter_file: counter_file.to_string_lossy().into_owned(),
        drop_first_response,
        connection_limit,
    };
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "server_helper", "--nocapture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    serde_json::to_writer(child.stdin.take().unwrap(), &config).unwrap();
    let started = Instant::now();
    while !ready_file.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("control test helper exited before readiness: {status}");
        }
        if started.elapsed() >= Duration::from_secs(3) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("control helper readiness timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    ServerProcess {
        child,
        _directory: directory,
        endpoint,
        counter_file,
    }
}

fn client<'a>(
    endpoint: &LocalEndpoint,
    requested_scope: &Scope,
    capability_bytes: [u8; 32],
    client_id: &str,
    resume_after: u64,
    budget: IoBudget<'_>,
) -> io::Result<ClientSession<'a>> {
    let connection = connect(endpoint, budget)?;
    let capability = Capability::from_bytes(capability_bytes)?;
    ClientSession::authenticate(
        connection,
        requested_scope,
        &capability,
        client_id,
        resume_after,
        budget,
    )
}

fn read_reply(session: &mut ClientSession<'_>) -> ServerMessage {
    with_budget(|budget| session.receive(budget)).unwrap()
}

fn send(session: &mut ClientSession<'_>, message: &ClientMessage) {
    with_budget(|budget| session.send(message, budget)).unwrap();
}

fn inspect(request_id: u64, revision: u64) -> ClientMessage {
    ClientMessage::Submit {
        request: Request {
            request_id,
            operation: Operation::Inspect {
                expected_revision: Some(revision),
            },
        },
    }
}

fn request_reply(message: ServerMessage) -> RequestReply {
    match message {
        ServerMessage::Request { reply } => reply,
        other => panic!("expected request reply, got {other:?}"),
    }
}

fn counter(server: &ServerProcess) -> usize {
    std::fs::read_to_string(&server.counter_file)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn wait_for_counter(server: &ServerProcess, expected: usize) {
    let started = Instant::now();
    while counter(server) != expected {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "control operation did not complete"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn separate_process_authentication_ledger_reconnect_and_event_resume() {
    let server = start_server(true, 32);

    let mut alpha = with_budget(|budget| {
        client(
            &server.endpoint,
            &scope(),
            CAPABILITY,
            "client_alpha",
            0,
            budget,
        )
    })
    .unwrap();
    send(&mut alpha, &inspect(1, 5));
    assert_eq!(
        request_reply(read_reply(&mut alpha)),
        RequestReply::Accepted { request_id: 1 }
    );
    send(&mut alpha, &inspect(1, 5));
    assert_eq!(
        request_reply(read_reply(&mut alpha)),
        RequestReply::Pending { request_id: 1 }
    );
    let lost_response = with_budget(|budget| {
        alpha.send(&inspect(1, 5), budget)?;
        alpha.receive(budget)
    });
    assert!(lost_response.is_err());
    drop(alpha);

    let mut alpha = with_budget(|budget| {
        client(
            &server.endpoint,
            &scope(),
            CAPABILITY,
            "client_alpha",
            0,
            budget,
        )
    })
    .unwrap();
    send(&mut alpha, &inspect(1, 5));
    assert_eq!(
        request_reply(read_reply(&mut alpha)),
        RequestReply::Complete {
            request_id: 1,
            result: OperationResult {
                succeeded: true,
                payload: b"inspection".to_vec()
            },
        }
    );
    send(&mut alpha, &inspect(1, 5));
    assert!(matches!(
        request_reply(read_reply(&mut alpha)),
        RequestReply::Complete { request_id: 1, .. }
    ));
    send(&mut alpha, &inspect(1, 6));
    assert_eq!(
        request_reply(read_reply(&mut alpha)),
        RequestReply::Conflict { request_id: 1 }
    );
    send(&mut alpha, &ClientMessage::Acknowledge { request_id: 1 });
    assert_eq!(
        read_reply(&mut alpha),
        ServerMessage::Acknowledged { request_id: 1 }
    );
    send(&mut alpha, &inspect(1, 5));
    assert_eq!(
        request_reply(read_reply(&mut alpha)),
        RequestReply::Expired { request_id: 1 }
    );
    drop(alpha);

    let mut beta = with_budget(|budget| {
        client(
            &server.endpoint,
            &scope(),
            CAPABILITY,
            "client_beta",
            0,
            budget,
        )
    })
    .unwrap();
    send(&mut beta, &inspect(1, 5));
    assert!(matches!(
        request_reply(read_reply(&mut beta)),
        RequestReply::Complete { request_id: 1, .. }
    ));
    wait_for_counter(&server, 2);
    drop(beta);

    let mut resumed = with_budget(|budget| {
        client(
            &server.endpoint,
            &scope(),
            CAPABILITY,
            "events_client",
            2,
            budget,
        )
    })
    .unwrap();
    send(&mut resumed, &ClientMessage::Events { after: 2 });
    match read_reply(&mut resumed) {
        ServerMessage::Events { batch } => {
            assert_eq!(batch.after, 2);
            assert_eq!(batch.events.first().map(|event| event.sequence), Some(3));
        }
        other => panic!("expected resumed events, got {other:?}"),
    }
    send(&mut resumed, &ClientMessage::Close);
    assert_eq!(read_reply(&mut resumed), ServerMessage::Closed);
    assert_eq!(counter(&server), 2);
}

#[test]
fn mismatched_scope_and_capability_and_oversized_prefix_are_rejected() {
    let server = start_server(false, 32);
    for mutate in 0..5 {
        let mut invalid_scope = scope();
        match mutate {
            0 => invalid_scope.context_key = "b".repeat(64),
            1 => invalid_scope.space_id = "space_other".into(),
            2 => invalid_scope.owner_id = "owner_other".into(),
            3 => invalid_scope.instance_id = "instance_other".into(),
            _ => invalid_scope.generation += 1,
        }
        let result = with_budget(|budget| {
            client(
                &server.endpoint,
                &invalid_scope,
                CAPABILITY,
                "bad_scope",
                0,
                budget,
            )
        });
        assert!(result.is_err(), "scope variant {mutate} authenticated");
    }
    assert!(
        with_budget(|budget| client(&server.endpoint, &scope(), [0x33; 32], "bad_key", 0, budget))
            .is_err()
    );

    with_budget(|budget| {
        let mut connection = connect(&server.endpoint, budget)?;
        connection.write_all(
            &(foxy_supervisor::control::MAX_MESSAGE_BYTES as u32 + 1).to_le_bytes(),
            budget,
        )?;
        assert!(connection.read_exact(&mut [0], budget).is_err());
        Ok(())
    })
    .unwrap();
    let mut valid = with_budget(|budget| {
        client(
            &server.endpoint,
            &scope(),
            CAPABILITY,
            "valid_after_reject",
            0,
            budget,
        )
    })
    .unwrap();
    send(&mut valid, &inspect(1, 9));
    assert!(matches!(
        request_reply(read_reply(&mut valid)),
        RequestReply::Complete { request_id: 1, .. }
    ));
    wait_for_counter(&server, 1);
}

#[test]
fn endpoint_collision_and_cancelled_io_fail_without_claiming_a_connection() {
    let _directory = tempfile::Builder::new()
        .prefix("f-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_directory.path(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let suffix = format!("{:016x}", rand::random::<u64>());
    #[cfg(target_os = "linux")]
    let endpoint = LocalEndpoint::UnixSocket(_directory.path().join(format!("c-{suffix}")));
    #[cfg(windows)]
    let endpoint = LocalEndpoint::NamedPipe(format!(r"\\.\pipe\foxy-control-{suffix}"));
    let mut listener = Listener::bind(&endpoint).unwrap();
    assert!(Listener::bind(&endpoint).is_err());
    let not_cancelled = AtomicBool::new(false);
    let timeout = listener.accept(IoBudget {
        deadline: Instant::now() + Duration::from_millis(30),
        cancelled: &not_cancelled,
    });
    assert_eq!(timeout.err().unwrap().kind(), io::ErrorKind::TimedOut);
    let cancelled = AtomicBool::new(true);
    let result = connect(
        &endpoint,
        IoBudget {
            deadline: Instant::now() + Duration::from_secs(1),
            cancelled: &cancelled,
        },
    );
    assert!(result.is_err());
    drop(listener);
}

#[test]
#[ignore]
fn server_helper() {
    let mut config: HelperConfig = serde_json::from_reader(io::stdin()).unwrap();
    let endpoint = if config.endpoint.starts_with(r"\\.\pipe\foxy-control-") {
        LocalEndpoint::NamedPipe(config.endpoint.clone())
    } else {
        LocalEndpoint::UnixSocket(config.endpoint.clone().into())
    };
    let mut listener = Listener::bind(&endpoint).unwrap();
    let capability = Capability::from_bytes(config.capability).unwrap();
    let mut ledger = Ledger::new(config.scope.clone()).unwrap();
    for index in 1..=4 {
        ledger
            .publish(&format!("event_{index}"), vec![index as u8])
            .unwrap();
    }
    let mut executions = 0usize;
    let mut pending_observed = false;
    std::fs::write(&config.counter_file, "0").unwrap();
    std::fs::write(&config.ready_file, "ready").unwrap();
    for _ in 0..config.connection_limit {
        let connection = match with_budget(|budget| listener.accept(budget)) {
            Ok(connection) => connection,
            Err(_) => continue,
        };
        let mut session = match with_budget(|budget| {
            ServerSession::authenticate(connection, &config.scope, &capability, budget)
        }) {
            Ok(session) => session,
            Err(_) => continue,
        };
        while let Ok(message) = with_budget(|budget| session.receive(budget)) {
            if let ClientMessage::Events { after } = &message {
                assert_eq!(*after, session.resume_after());
            }
            let response = ledger.handle(&session, message.clone()).unwrap();
            if let (
                ClientMessage::Submit { .. },
                ServerMessage::Request {
                    reply: RequestReply::Accepted { request_id },
                },
            ) = (&message, &response)
            {
                executions += 1;
                std::fs::write(&config.counter_file, executions.to_string()).unwrap();
                if config.drop_first_response {
                    with_budget(|budget| session.send(&response, budget)).unwrap();
                    continue;
                }
                let reply = ledger
                    .complete(
                        &session,
                        *request_id,
                        OperationResult {
                            succeeded: true,
                            payload: b"inspection".to_vec(),
                        },
                    )
                    .unwrap();
                with_budget(|budget| session.send(&ServerMessage::Request { reply }, budget))
                    .unwrap();
            } else if let (
                ClientMessage::Submit { .. },
                ServerMessage::Request {
                    reply: RequestReply::Pending { request_id },
                },
            ) = (&message, &response)
            {
                if config.drop_first_response && !pending_observed {
                    pending_observed = true;
                    with_budget(|budget| session.send(&response, budget)).unwrap();
                    continue;
                }
                let reply = ledger
                    .complete(
                        &session,
                        *request_id,
                        OperationResult {
                            succeeded: true,
                            payload: b"inspection".to_vec(),
                        },
                    )
                    .unwrap();
                if config.drop_first_response {
                    config.drop_first_response = false;
                    break;
                }
                with_budget(|budget| session.send(&ServerMessage::Request { reply }, budget))
                    .unwrap();
            } else {
                let close = matches!(message, ClientMessage::Close);
                with_budget(|budget| session.send(&response, budget)).unwrap();
                if close {
                    return;
                }
            }
        }
    }
}
