use super::*;
use crate::core::server::{
    ContentStageControl, ContentStagePhase, DeploymentOperationOutcome, ServerInstanceId,
};
use foxy_supervisor::control::{CompletionTicket, Ledger, Operation, OperationResult};
use serde::{Deserialize, Serialize};
use std::io;
use std::thread::JoinHandle;

pub(super) struct DeploymentWorker {
    pub(super) ticket: CompletionTicket,
    pub(super) cancel_ticket: Option<CompletionTicket>,
    pub(super) handle: JoinHandle<Result<DeploymentOperationOutcome, String>>,
    pub(super) control: ContentStageControl,
    pub(super) client_id: String,
    pub(super) request_id: u64,
    pub(super) instance_id: String,
    pub(super) last_progress: Option<Vec<u8>>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DeploymentProgressEvent<'a> {
    client_id: &'a str,
    request_id: u64,
    instance_id: &'a str,
    progress: serde_json::Value,
}

pub(super) fn publish_deployment_progress(
    deployment: &mut Option<DeploymentWorker>,
    ledger: &mut Ledger,
) -> io::Result<()> {
    let Some(worker) = deployment.as_mut() else {
        return Ok(());
    };
    let progress = serde_json::to_value(worker.control.snapshot())
        .map_err(|_| invalid("Deployment progress encoding failed"))?;
    let payload = serde_json::to_vec(&DeploymentProgressEvent {
        client_id: &worker.client_id,
        request_id: worker.request_id,
        instance_id: &worker.instance_id,
        progress,
    })
    .map_err(|_| invalid("Deployment progress encoding failed"))?;
    if worker.last_progress.as_ref() != Some(&payload) {
        ledger.publish("deployment_progress", payload.clone())?;
        worker.last_progress = Some(payload);
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentReply {
    outcome: Option<DeploymentOperationOutcome>,
    error: Option<String>,
    cancelled: bool,
}

pub(super) fn deployment_reply_error(error: &str) -> io::Result<OperationResult> {
    deployment_reply_failure(error, false)
}

fn deployment_reply_failure(error: &str, cancelled: bool) -> io::Result<OperationResult> {
    let payload = serde_json::to_vec(&DeploymentReply {
        outcome: None,
        error: Some(error.chars().take(160).collect()),
        cancelled,
    })
    .map_err(|_| invalid("Deployment reply encoding failed"))?;
    if payload.len() > foxy_supervisor::control::MAX_PAYLOAD_BYTES {
        return Err(invalid("Deployment reply too large"));
    }
    Ok(OperationResult {
        succeeded: false,
        payload,
    })
}

pub(super) fn operation_instance(operation: &Operation) -> io::Result<ServerInstanceId> {
    let value = match operation {
        Operation::ActivateDeployment { instance_id, .. }
        | Operation::ResolveDeployment { instance_id, .. } => instance_id,
        _ => return Err(invalid("Invalid deployment operation")),
    };
    ServerInstanceId::try_from(value.clone()).map_err(|_| invalid("Invalid server instance id"))
}

pub(super) fn finish_deployment(
    deployment: &mut Option<DeploymentWorker>,
    ledger: &mut Ledger,
) -> io::Result<()> {
    if !deployment
        .as_ref()
        .is_some_and(|worker| worker.handle.is_finished())
    {
        return Ok(());
    }
    let worker = deployment.take().expect("finished deployment exists");
    let result = match worker.handle.join() {
        Ok(Ok(outcome)) => {
            let payload = serde_json::to_vec(&DeploymentReply {
                outcome: Some(outcome),
                error: None,
                cancelled: false,
            })
            .map_err(|_| invalid("Deployment reply encoding failed"))?;
            if payload.len() > foxy_supervisor::control::MAX_PAYLOAD_BYTES {
                deployment_reply_error("Deployment outcome exceeded the reply limit")?
            } else {
                OperationResult {
                    succeeded: true,
                    payload,
                }
            }
        }
        Ok(Err(error)) => deployment_reply_failure(
            &error,
            worker.control.snapshot().phase == ContentStagePhase::Cancelled,
        )?,
        Err(_) => deployment_reply_error("Owner deployment worker stopped unexpectedly")?,
    };
    ledger.complete_pending(worker.ticket, result.clone())?;
    if let Some(ticket) = worker.cancel_ticket {
        ledger.complete_pending(ticket, result)?;
    }
    let _ = (worker.client_id, worker.request_id);
    Ok(())
}

#[cfg(test)]
mod deployment_tests {
    use super::*;

    #[test]
    fn deployment_error_payload_is_bounded_and_does_not_expose_long_details() {
        let result = deployment_reply_error(&"x".repeat(4096)).unwrap();
        assert!(result.payload.len() <= foxy_supervisor::control::MAX_PAYLOAD_BYTES);
        let reply: DeploymentReply = serde_json::from_slice(&result.payload).unwrap();
        assert!(!result.succeeded);
        assert_eq!(reply.outcome, None);
        assert_eq!(reply.error.unwrap().len(), 160);
    }
}
