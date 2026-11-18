use super::*;
use crate::core::server::{
    ContentStageControl, InstanceRuntimeStatus, RuntimeOperationOutcome, ServerInstanceId,
};
use foxy_supervisor::control::MAX_PAYLOAD_BYTES;
use serde::{Deserialize, Serialize};

const UNKNOWN_OPERATION_GRACE: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LifecycleReply {
    pub(super) outcome: Option<RuntimeOperationOutcome>,
    pub(super) error: Option<String>,
}

impl Attachment {
    pub(crate) fn instance_monitoring(
        &self,
        store: &crate::core::server::ServerStore,
        instance_id: &ServerInstanceId,
    ) -> io::Result<crate::core::server::InstanceMonitoring> {
        if context_key(store.context())? != self.scope().context_key
            || store.context().space_id() != self.scope().space_id
        {
            return Err(invalid(
                "Monitoring context does not match the authenticated owner",
            ));
        }
        for attempt in 0..3 {
            let status = self.instance_status(instance_id)?;
            let observation = store.read_monitoring_snapshot(&status);
            let after = self.instance_status(instance_id)?;
            if status.owner_id != after.owner_id
                || status.owner_generation != after.owner_generation
                || status.run_generation != after.run_generation
                || status.monitoring_fingerprint != after.monitoring_fingerprint
                || (observation.is_err() && attempt < 2)
            {
                continue;
            }
            let unavailable = observation.is_err();
            let mut report = crate::core::server::monitoring::InstanceMonitoring::new(
                after,
                observation.unwrap_or(None),
                crate::core::server::monitoring::unix_ms(),
            );
            report.observation_unavailable = unavailable;
            return Ok(report);
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Monitoring changed while reading; refresh the current run",
        ))
    }

    pub(crate) fn instance_status(
        &self,
        instance_id: &ServerInstanceId,
    ) -> io::Result<InstanceRuntimeStatus> {
        let cancelled = AtomicBool::new(false);
        let budget = IoBudget {
            deadline: Instant::now() + CONNECT_TIMEOUT,
            cancelled: &cancelled,
        };
        let reply = self.deployment_message(
            "metadata_controller",
            ClientMessage::Submit {
                request: Request {
                    request_id: 1,
                    operation: Operation::InspectInstance {
                        instance_id: instance_id.as_str().into(),
                    },
                },
            },
            budget,
        )?;
        let ServerMessage::Request {
            reply:
                RequestReply::Complete {
                    request_id: 1,
                    result,
                },
        } = reply
        else {
            return Err(invalid("Owner lifecycle inspection was not completed"));
        };
        if !result.succeeded || result.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("Owner refused lifecycle inspection"));
        }
        let status: InstanceRuntimeStatus = serde_json::from_slice(&result.payload)
            .map_err(|_| invalid("Invalid owner lifecycle status"))?;
        validate_status_identity(&status, instance_id, self.scope())?;
        Ok(status)
    }

    pub(crate) fn run_instance(
        &self,
        operation: Operation,
        control: &ContentStageControl,
    ) -> io::Result<RuntimeOperationOutcome> {
        let instance_id = match &operation {
            Operation::StartInstance { instance_id, .. }
            | Operation::StopInstance { instance_id, .. }
            | Operation::RestartInstance { instance_id, .. }
            | Operation::RotateArmaMission { instance_id, .. }
            | Operation::ReloadReforgerScenario { instance_id, .. }
            | Operation::StartHeadlessClients { instance_id, .. }
            | Operation::StopHeadlessClients { instance_id, .. }
            | Operation::RestartHeadlessClients { instance_id, .. }
            | Operation::CreateServerBackup { instance_id, .. }
            | Operation::PruneServerBackups { instance_id, .. }
            | Operation::RestoreServerBackup { instance_id, .. }
            | Operation::RollbackServerRestore { instance_id, .. } => {
                ServerInstanceId::try_from(instance_id.clone())
                    .map_err(|_| invalid("Invalid lifecycle instance id"))?
            }
            _ => return Err(invalid("Invalid lifecycle operation")),
        };
        if control.snapshot().cancel_requested {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Lifecycle operation cancelled before owner submission",
            ));
        }

        let identity = self.request_identity()?;
        let client_id = identity.client_id.to_owned();
        let request_id = identity.request_id;
        let cancel_id = request_id
            .checked_add(1)
            .ok_or_else(|| invalid("Lifecycle request identities are exhausted"))?;
        let cancelled = AtomicBool::new(false);
        let mut submitted = false;
        let mut cancellation_sent = false;
        let mut unknown_deadline = Instant::now() + UNKNOWN_OPERATION_GRACE;
        let cancel_operation = if matches!(
            operation,
            Operation::CreateServerBackup { .. }
                | Operation::PruneServerBackups { .. }
                | Operation::RestoreServerBackup { .. }
                | Operation::RollbackServerRestore { .. }
        ) {
            Operation::CancelServerBackup { request_id }
        } else {
            Operation::CancelLifecycle { request_id }
        };
        let result = loop {
            if submitted && control.snapshot().cancel_requested {
                cancellation_sent = true;
                let budget = IoBudget {
                    deadline: Instant::now() + CONNECT_TIMEOUT,
                    cancelled: &cancelled,
                };
                let _ = self.deployment_request(
                    &client_id,
                    cancel_id,
                    cancel_operation.clone(),
                    budget,
                );
            }
            let budget = IoBudget {
                deadline: Instant::now() + CONNECT_TIMEOUT,
                cancelled: &cancelled,
            };
            if !submitted && control.snapshot().cancel_requested {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Lifecycle operation cancelled before owner submission",
                ));
            }
            submitted = true;
            match self.deployment_request(&client_id, request_id, operation.clone(), budget) {
                Ok(Some(result)) => break result,
                Ok(None) => {
                    unknown_deadline = Instant::now() + UNKNOWN_OPERATION_GRACE;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) if transient_connection(&error) => {
                    if Instant::now() >= unknown_deadline {
                        return Err(unknown_lifecycle_operation(error.kind()));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(unknown_lifecycle_operation(error.kind())),
            }
        };

        if result.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(invalid("Lifecycle operation reply is too large"));
        }
        let reply: LifecycleReply = serde_json::from_slice(&result.payload)
            .map_err(|_| invalid("Invalid lifecycle operation reply"))?;
        let terminal_outcome = if let Some(outcome) = reply.outcome {
            validate_lifecycle_outcome(&operation, &instance_id, self.scope(), &outcome)?;
            Some(outcome)
        } else {
            None
        };
        acknowledge_lifecycle(self, &client_id, request_id, &cancelled);
        if cancellation_sent {
            acknowledge_lifecycle(self, &client_id, cancel_id, &cancelled);
        }
        if let Some(outcome) = terminal_outcome {
            return Ok(outcome);
        }
        if !result.succeeded {
            return Err(io::Error::other(
                reply
                    .error
                    .unwrap_or_else(|| "Owner lifecycle operation failed".into()),
            ));
        }
        Err(invalid("Lifecycle operation outcome is missing"))
    }
}

fn validate_status_identity(
    status: &InstanceRuntimeStatus,
    target: &ServerInstanceId,
    scope: &Scope,
) -> io::Result<()> {
    if &status.instance_id != target
        || status.owner_id != scope.owner_id
        || status.owner_generation != scope.generation
    {
        return Err(invalid("Lifecycle status does not match the owner request"));
    }
    Ok(())
}

fn validate_lifecycle_outcome(
    operation: &Operation,
    target: &ServerInstanceId,
    scope: &Scope,
    outcome: &RuntimeOperationOutcome,
) -> io::Result<()> {
    validate_status_identity(&outcome.status, target, scope)?;
    match operation {
        Operation::StartInstance {
            expected_activation_generation,
            ..
        } if outcome.status.run_generation == 0
            || outcome.status.activation_generation != Some(*expected_activation_generation) =>
        {
            Err(invalid("Lifecycle start outcome has no matching run"))
        }
        Operation::StopInstance {
            expected_run_generation,
            ..
        }
        | Operation::RotateArmaMission {
            expected_run_generation,
            ..
        }
        | Operation::ReloadReforgerScenario {
            expected_run_generation,
            ..
        } if outcome.status.run_generation != *expected_run_generation => Err(invalid(
            "Lifecycle stop outcome run does not match the request",
        )),
        Operation::StartHeadlessClients {
            expected_run_generation,
            ..
        }
        | Operation::StopHeadlessClients {
            expected_run_generation,
            ..
        }
        | Operation::RestartHeadlessClients {
            expected_run_generation,
            ..
        } if outcome.status.run_generation != *expected_run_generation => Err(invalid(
            "Headless client outcome run does not match the request",
        )),
        Operation::RestartInstance {
            expected_run_generation,
            expected_activation_generation,
            ..
        } => {
            let matches = if outcome.completed {
                outcome.status.run_generation > *expected_run_generation
                    && outcome.status.activation_generation == Some(*expected_activation_generation)
            } else {
                outcome.status.run_generation == *expected_run_generation
            };
            if matches {
                Ok(())
            } else {
                Err(invalid(
                    "Lifecycle restart outcome does not match the request",
                ))
            }
        }
        Operation::StartInstance { .. }
        | Operation::StopInstance { .. }
        | Operation::StartHeadlessClients { .. }
        | Operation::StopHeadlessClients { .. }
        | Operation::RestartHeadlessClients { .. }
        | Operation::CreateServerBackup { .. }
        | Operation::PruneServerBackups { .. }
        | Operation::RestoreServerBackup { .. }
        | Operation::RollbackServerRestore { .. } => Ok(()),
        Operation::RotateArmaMission { .. } | Operation::ReloadReforgerScenario { .. } => Ok(()),
        _ => Err(invalid("Invalid lifecycle operation")),
    }
}

fn acknowledge_lifecycle(
    attachment: &Attachment,
    client_id: &str,
    request_id: u64,
    cancelled: &AtomicBool,
) {
    let _ = attachment.deployment_message(
        client_id,
        ClientMessage::Acknowledge { request_id },
        IoBudget {
            deadline: Instant::now() + CONNECT_TIMEOUT,
            cancelled,
        },
    );
}

fn unknown_lifecycle_operation(kind: io::ErrorKind) -> io::Error {
    io::Error::new(
        kind,
        "Lifecycle outcome is unknown; inspect runtime status before retrying",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::InstanceRuntimePhase;

    fn status() -> InstanceRuntimeStatus {
        InstanceRuntimeStatus {
            instance_id: ServerInstanceId::try_from(format!("server_{}", "a".repeat(32))).unwrap(),
            owner_id: "owner_a".into(),
            owner_generation: 2,
            run_generation: 4,
            activation_generation: Some(3),
            runtime_id: Some("runtime_a".into()),
            phase: InstanceRuntimePhase::Running,
            process_id: Some(7),
            process_exists: true,
            readiness_verified: true,
            cleanup_verified: false,
            stop_timed_out: false,
            forced: false,
            root_exit_code: None,
            observation_age_ms: 1,
            monitoring_fingerprint: Some("a".repeat(64)),
            headless_running: 0,
        }
    }

    #[test]
    fn lifecycle_reply_is_bounded_and_owner_instance_and_run_are_checked() {
        let example: InstanceRuntimeStatus = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/server_instance_runtime_status.json"
        ))
        .unwrap();
        assert_eq!(example.phase, InstanceRuntimePhase::Running);
        assert!(example.process_exists);
        assert!(!example.readiness_verified);
        let scope = Scope {
            context_key: "a".repeat(64),
            space_id: "alpha".into(),
            instance_id: "space_catalog".into(),
            owner_id: "owner_a".into(),
            generation: 2,
        };
        let target = status().instance_id;
        let outcome = RuntimeOperationOutcome {
            status: status(),
            completed: true,
            cancelled: false,
            message: None,
            backup_id: None,
            backup_consistency: None,
        };
        let payload = serde_json::to_vec(&LifecycleReply {
            outcome: Some(outcome.clone()),
            error: None,
        })
        .unwrap();
        assert!(payload.len() <= MAX_PAYLOAD_BYTES);
        let mut with_backup = outcome.clone();
        with_backup.backup_id = Some(format!("backup_{}", "f".repeat(64)));
        with_backup.backup_consistency =
            Some(crate::core::server::BackupConsistencyMode::StoppedVerified);
        with_backup.status.runtime_id = Some(format!("runtime_{}", "a".repeat(64)));
        with_backup.status.owner_id = format!("owner_{}", "b".repeat(32));
        with_backup.message = Some("The same owned server read the selected mission and answered a fresh query. Its process was not restarted.".into());
        let backup_payload = serde_json::to_vec(&LifecycleReply {
            outcome: Some(with_backup),
            error: None,
        })
        .unwrap();
        assert!(
            backup_payload.len() <= MAX_PAYLOAD_BYTES,
            "{}",
            backup_payload.len()
        );
        let mut headless = outcome.clone();
        headless.status.headless_running =
            u32::try_from(crate::core::server::MAX_HEADLESS_CLIENTS).unwrap();
        headless.status.runtime_id = Some(format!("runtime_{}", "a".repeat(64)));
        headless.status.owner_id = format!("owner_{}", "b".repeat(32));
        headless.message = Some("Started 32 headless client process(es). Join evidence appears in headless status once the client reports a started mission.".into());
        let headless_payload = serde_json::to_vec(&LifecycleReply {
            outcome: Some(headless),
            error: None,
        })
        .unwrap();
        assert!(
            headless_payload.len() <= MAX_PAYLOAD_BYTES,
            "{}",
            headless_payload.len()
        );
        let decoded: LifecycleReply = serde_json::from_slice(&payload).unwrap();
        let decoded = decoded.outcome.unwrap();
        validate_lifecycle_outcome(
            &Operation::StopInstance {
                instance_id: target.as_str().into(),
                expected_run_generation: 4,
                force: false,
                timeout_ms: 1,
            },
            &target,
            &scope,
            &decoded,
        )
        .unwrap();
        assert!(
            validate_lifecycle_outcome(
                &Operation::StopInstance {
                    instance_id: target.as_str().into(),
                    expected_run_generation: 3,
                    force: false,
                    timeout_ms: 1,
                },
                &target,
                &scope,
                &decoded,
            )
            .is_err()
        );
        let restart = Operation::RestartInstance {
            instance_id: target.as_str().into(),
            expected_run_generation: 4,
            expected_activation_generation: 3,
            review_fingerprint: "b".repeat(64),
            force: false,
            timeout_ms: 1000,
        };
        let mut interrupted = decoded.clone();
        interrupted.completed = false;
        interrupted.cancelled = true;
        interrupted.status.phase = InstanceRuntimePhase::Exited;
        interrupted.status.process_exists = false;
        interrupted.status.cleanup_verified = true;
        validate_lifecycle_outcome(&restart, &target, &scope, &interrupted).unwrap();
        interrupted.completed = true;
        assert!(validate_lifecycle_outcome(&restart, &target, &scope, &interrupted).is_err());
        let mut mismatched = decoded;
        mismatched.status.owner_generation += 1;
        assert!(
            validate_lifecycle_outcome(
                &Operation::StopInstance {
                    instance_id: target.as_str().into(),
                    expected_run_generation: 4,
                    force: false,
                    timeout_ms: 1,
                },
                &target,
                &scope,
                &mismatched,
            )
            .is_err()
        );
    }
}
