use super::*;
use crate::core::{
    server::ServerInstanceId,
    server::{ContentStageControl, DeploymentOperationOutcome},
    tasks::db_process_lock::{self, HeldClaimPin},
};
use foxy_supervisor::control::OperationResult;
use std::fs::File;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const UNKNOWN_OPERATION_GRACE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionIdentity {
    pub target_instance: ServerInstanceId,
    pub owner_id: String,
    pub owner_generation: u64,
    pub operation_id: String,
    pub run_generation: u64,
}

pub(crate) struct DeploymentExecution {
    context: ServerContext,
    scope: Scope,
    identity: SessionIdentity,
    _claim: HeldClaimPin,
    _operation_file: File,
}

impl DeploymentExecution {
    pub(super) fn new(
        context: &ServerContext,
        scope: &Scope,
        client_id: &str,
        request_id: u64,
        target_instance: ServerInstanceId,
        operation_file: File,
    ) -> io::Result<Self> {
        if request_id == 0
            || client_id.is_empty()
            || client_id.len() > 128
            || !client_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(invalid("Invalid deployment operation identity"));
        }
        if scope.context_key != super::context_key(context)?
            || scope.space_id != context.space_id()
            || scope.instance_id != IDENTITY
        {
            return Err(invalid("Deployment owner scope mismatch"));
        }
        let claim = db_process_lock::pin_held_claim(&context.database_path())
            .map_err(|_| invalid("An exclusive game-space claim is required"))?;
        Ok(Self {
            context: context.clone(),
            scope: scope.clone(),
            identity: SessionIdentity {
                target_instance,
                owner_id: scope.owner_id.clone(),
                owner_generation: scope.generation,
                operation_id: operation_identifier(client_id, request_id),
                run_generation: request_id,
            },
            _claim: claim,
            _operation_file: operation_file,
        })
    }

    pub(crate) fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    pub(crate) fn validate(&self, context: &ServerContext) -> io::Result<()> {
        if self.context != *context
            || self.scope.context_key != super::context_key(context)?
            || self.scope.space_id != context.space_id()
            || self.scope.instance_id != IDENTITY
            || self.identity.owner_id != self.scope.owner_id
            || self.identity.owner_generation != self.scope.generation
            || self.identity.run_generation == 0
        {
            return Err(invalid("Deployment execution context changed"));
        }
        Ok(())
    }
}

impl Attachment {
    pub(crate) fn run_deployment(
        &self,
        operation: Operation,
        control: &ContentStageControl,
    ) -> io::Result<DeploymentOperationOutcome> {
        if !matches!(
            &operation,
            Operation::ActivateDeployment { .. } | Operation::ResolveDeployment { .. }
        ) {
            return Err(invalid("Invalid deployment operation"));
        }
        let expected_instance = match &operation {
            Operation::ActivateDeployment { instance_id, .. }
            | Operation::ResolveDeployment { instance_id, .. } => instance_id.clone(),
            _ => return Err(invalid("Invalid deployment operation")),
        };
        if control.snapshot().cancel_requested {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Deployment cancelled before owner submission",
            ));
        }
        let identity = self.request_identity()?;
        let client_id = identity.client_id.to_owned();
        let request_id = identity.request_id;
        let cancelled = AtomicBool::new(false);
        let mut events_after = 0;
        let mut submitted = false;
        let mut cancellation_sent = false;
        let mut unknown_deadline = Instant::now() + UNKNOWN_OPERATION_GRACE;
        let result = loop {
            if submitted && control.snapshot().cancel_requested {
                cancellation_sent = true;
                let budget = IoBudget {
                    deadline: Instant::now() + CONNECT_TIMEOUT,
                    cancelled: &cancelled,
                };
                let _ = self.deployment_request(
                    &client_id,
                    request_id + 1,
                    Operation::CancelDeployment { request_id },
                    budget,
                );
            }
            let budget = IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            };
            if let Ok(ServerMessage::Events { batch }) = self.deployment_message(
                &client_id,
                ClientMessage::Events {
                    after: events_after,
                },
                budget,
            ) {
                unknown_deadline = Instant::now() + UNKNOWN_OPERATION_GRACE;
                for event in batch.events {
                    events_after = event.sequence;
                    if let Ok(progress) =
                        serde_json::from_slice::<RemoteDeploymentProgress>(&event.payload)
                        && progress.client_id == client_id
                        && progress.request_id == request_id
                        && progress.instance_id == expected_instance
                    {
                        control.receive_progress(progress.progress);
                    }
                }
            }
            let budget = IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            };
            if !submitted && control.snapshot().cancel_requested {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Deployment cancelled before owner submission",
                ));
            }
            submitted = true;
            match self.deployment_request(&client_id, request_id, operation.clone(), budget) {
                Ok(Some(reply)) => break reply,
                Ok(None) => {
                    unknown_deadline = Instant::now() + UNKNOWN_OPERATION_GRACE;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) if transient_connection(&error) => {
                    if Instant::now() >= unknown_deadline {
                        return Err(unknown_operation(error.kind()));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    return Err(unknown_operation(error.kind()));
                }
            }
        };
        let reply: DeploymentReply = serde_json::from_slice(&result.payload)
            .map_err(|_| invalid("Invalid deployment outcome"))?;
        if !result.succeeded {
            let _ = self.deployment_message(
                &client_id,
                ClientMessage::Acknowledge { request_id },
                IoBudget {
                    deadline: Instant::now() + CONNECT_TIMEOUT,
                    cancelled: &cancelled,
                },
            );
            if cancellation_sent {
                let _ = self.deployment_message(
                    &client_id,
                    ClientMessage::Acknowledge {
                        request_id: request_id + 1,
                    },
                    IoBudget {
                        deadline: Instant::now() + CONNECT_TIMEOUT,
                        cancelled: &cancelled,
                    },
                );
            }
            return Err(io::Error::new(
                if reply.cancelled {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::Other
                },
                if reply.cancelled {
                    crate::core::server::ServerStoreError::DeploymentCancelled.to_string()
                } else {
                    reply
                        .error
                        .unwrap_or_else(|| "Owner deployment operation failed".into())
                },
            ));
        }
        let outcome = reply
            .outcome
            .ok_or_else(|| invalid("Deployment outcome is missing"))?;
        if outcome.instance_id.as_str() != expected_instance || outcome.process_started {
            return Err(invalid("Deployment outcome does not match the request"));
        }
        let _ = self.deployment_message(
            &client_id,
            ClientMessage::Acknowledge { request_id },
            IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            },
        );
        if cancellation_sent {
            let _ = self.deployment_message(
                &client_id,
                ClientMessage::Acknowledge {
                    request_id: request_id + 1,
                },
                IoBudget {
                    deadline: Instant::now() + CONNECT_TIMEOUT,
                    cancelled: &cancelled,
                },
            );
        }
        Ok(outcome)
    }

    pub(super) fn deployment_request(
        &self,
        client_id: &str,
        request_id: u64,
        operation: Operation,
        budget: IoBudget<'_>,
    ) -> io::Result<Option<OperationResult>> {
        let reply = self.deployment_message(
            client_id,
            ClientMessage::Submit {
                request: Request {
                    request_id,
                    operation,
                },
            },
            budget,
        )?;
        let ServerMessage::Request { reply } = reply else {
            return Err(invalid("Invalid owner deployment reply"));
        };
        if reply.request_id() != request_id {
            return Err(invalid("Deployment reply identity mismatch"));
        }
        match reply {
            RequestReply::Accepted { .. } | RequestReply::Pending { .. } => Ok(None),
            RequestReply::Complete { result, .. } => Ok(Some(result)),
            _ => Err(invalid("Owner refused deployment operation")),
        }
    }

    pub(super) fn deployment_message(
        &self,
        client_id: &str,
        message: ClientMessage,
        budget: IoBudget<'_>,
    ) -> io::Result<ServerMessage> {
        let connection = transport::connect(&self.record.endpoint()?, budget)?;
        let capability = Capability::from_bytes(self.record.capability)?;
        let mut session = ClientSession::authenticate(
            connection,
            &self.record.scope,
            &capability,
            client_id,
            0,
            budget,
        )?;
        session.send(&message, budget)?;
        let response = session.receive(budget)?;
        if session.send(&ClientMessage::Close, budget).is_ok() {
            let _ = session.receive(budget);
        }
        Ok(response)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteDeploymentProgress {
    client_id: String,
    request_id: u64,
    instance_id: String,
    progress: crate::core::server::content_staging::ContentStageProgress,
}

fn operation_identifier(client_id: &str, request_id: u64) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(client_id.as_bytes());
    hash.update(&[0]);
    hash.update(&request_id.to_le_bytes());
    let encoded = hash.finalize().to_hex();
    format!("run_{}", &encoded[..32])
}

fn unknown_operation(kind: io::ErrorKind) -> io::Error {
    io::Error::new(
        kind,
        "Deployment outcome is unknown; inspect activation status and reviewed recovery before retrying",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn a_validated_terminal_reply_survives_a_lost_close_receipt() {
        let _directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(_directory.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        #[cfg(windows)]
        let endpoint = LocalEndpoint::NamedPipe(format!(
            r"\\.\pipe\foxy-control-{:032x}",
            rand::random::<u128>()
        ));
        #[cfg(not(windows))]
        let endpoint = LocalEndpoint::UnixSocket(_directory.path().join("owner.sock"));
        let scope = Scope {
            context_key: "a".repeat(64),
            space_id: "alpha".into(),
            instance_id: IDENTITY.into(),
            owner_id: "owner_alpha".into(),
            generation: 1,
        };
        let capability = Capability::generate();
        let attachment = Attachment {
            runtime_directory: _directory.path().to_path_buf(),
            record: Arc::new(OwnerRecord {
                version: RECORD_VERSION,
                scope: scope.clone(),
                capability: *capability.expose_for_handoff(),
                endpoint: match &endpoint {
                    LocalEndpoint::NamedPipe(name) => name.clone(),
                    LocalEndpoint::UnixSocket(path) => path.to_str().unwrap().into(),
                },
            }),
        };
        let mut listener = transport::Listener::bind(&endpoint).unwrap();
        let worker = std::thread::spawn(move || {
            let stopped = AtomicBool::new(false);
            let budget = IoBudget {
                deadline: Instant::now() + Duration::from_secs(5),
                cancelled: &stopped,
            };
            let stream = listener.accept(budget).unwrap();
            let mut session = foxy_supervisor::control::ServerSession::authenticate(
                stream,
                &scope,
                &capability,
                budget,
            )
            .unwrap();
            assert!(matches!(
                session.receive(budget).unwrap(),
                ClientMessage::Submit { .. }
            ));
            session
                .send(
                    &ServerMessage::Request {
                        reply: RequestReply::Complete {
                            request_id: 7,
                            result: OperationResult {
                                succeeded: true,
                                payload: b"retained".to_vec(),
                            },
                        },
                    },
                    budget,
                )
                .unwrap();
            assert_eq!(session.receive(budget).unwrap(), ClientMessage::Close);
        });
        let stopped = AtomicBool::new(false);
        let result = attachment
            .deployment_request(
                "controller_alpha",
                7,
                Operation::Inspect {
                    expected_revision: None,
                },
                IoBudget {
                    deadline: Instant::now() + Duration::from_secs(5),
                    cancelled: &stopped,
                },
            )
            .unwrap()
            .unwrap();
        assert!(result.succeeded);
        assert_eq!(result.payload, b"retained");
        worker.join().unwrap();
    }

    #[test]
    fn operation_identifier_is_stable_bounded_and_scoped_to_request() {
        let first = operation_identifier("deployment_abc", 1);
        assert_eq!(first, operation_identifier("deployment_abc", 1));
        assert_ne!(first, operation_identifier("deployment_abc", 2));
        assert_ne!(first, operation_identifier("deployment_def", 1));
        assert!(first.len() <= 128);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentReply {
    outcome: Option<DeploymentOperationOutcome>,
    error: Option<String>,
    cancelled: bool,
}
