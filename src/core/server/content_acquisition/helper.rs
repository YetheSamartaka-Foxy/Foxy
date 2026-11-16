use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use foxy_supervisor::process::{CommandSpec, OwnedProcess};

use super::worker::{MAX_WORKER_DOCUMENT, WorkerRequest, WorkerState, WorkerStatus};
use crate::core::server::{ContentStageControl, ContentStagePhase, ServerStoreError};

pub(super) fn run(
    root: &Path,
    request: &WorkerRequest,
    control: &ContentStageControl,
    profile: &str,
) -> Result<WorkerState, ServerStoreError> {
    let space = root.join("games").join(&request.game_space_id);
    let request_bytes = serde_json::to_vec(request).map_err(|_| ServerStoreError::Corrupt)?;
    if request_bytes.len() > MAX_WORKER_DOCUMENT {
        return Err(ServerStoreError::Corrupt);
    }
    let fingerprint = blake3::hash(&request_bytes).to_hex().to_string();
    fs::write(space.join("worker-request.json"), request_bytes)
        .map_err(|_| ServerStoreError::Io)?;
    let spec = CommandSpec {
        executable: std::env::current_exe().map_err(|_| ServerStoreError::Io)?,
        directory: root.to_path_buf(),
        arguments: vec![
            "--config-dir".into(),
            root.as_os_str().to_owned(),
            "--quiet".into(),
            "content-acquisition-worker".into(),
        ],
        environment: vec![
            ("FOXY_CONFIG_DIR".into(), root.as_os_str().to_owned()),
            ("EGUI_INSPECTION".into(), "0".into()),
            ("FOXY_AGENT_TOON".into(), "0".into()),
        ],
    };
    let mut process = OwnedProcess::spawn(&spec).map_err(|_| {
        ServerStoreError::Invalid("Cannot start the contained repository acquisition worker".into())
    })?;
    let started = Instant::now();
    let mut cancelled_at = None;
    let mut failure = None;
    loop {
        let state = read_state(&space);
        match &state {
            Ok(Some(state))
                if state.schema_version == 1 && state.request_fingerprint == fingerprint =>
            {
                let mut progress = state.progress.clone();
                progress.source = Some(profile.to_owned());
                progress.phase = match state.status {
                    WorkerStatus::Preparing => ContentStagePhase::Preparing,
                    WorkerStatus::Downloading => ContentStagePhase::Copying,
                    _ => ContentStagePhase::Verifying,
                };
                control.receive_progress(progress);
            }
            Ok(None) => {}
            _ => {
                failure = Some(ServerStoreError::Corrupt);
            }
        }
        let exit = process
            .observe()
            .map_err(|_| ServerStoreError::StageCleanupIncomplete)?;
        if exit.is_some() {
            if let Some(error) = failure {
                return Err(error);
            }
            let mut state = read_state(&space)?.ok_or(ServerStoreError::Io)?;
            if state.schema_version != 1
                || state.request_fingerprint != fingerprint
                || !matches!(
                    state.status,
                    WorkerStatus::Complete | WorkerStatus::Failed | WorkerStatus::Cancelled
                )
            {
                return Err(ServerStoreError::Corrupt);
            }
            if state.status == WorkerStatus::Complete
                && exit.is_none_or(|exit| exit.forced || exit.root_exit_code != Some(0))
            {
                state.status = WorkerStatus::Failed;
                state.message=Some("The repository worker exited without a clean contained outcome. Previous complete stages were retained.".into());
            }
            return Ok(state);
        }
        if control.snapshot().cancel_requested
            || failure.is_some()
            || started.elapsed() >= Duration::from_secs(15 * 60)
        {
            if cancelled_at.is_none() {
                if fs::write(space.join("worker-cancel"), b"cancel").is_err() {
                    failure = Some(ServerStoreError::Io);
                }
                cancelled_at = Some(Instant::now());
            }
            if cancelled_at.is_some_and(|time: Instant| time.elapsed() >= Duration::from_secs(20)) {
                if process
                    .terminate_and_wait(Duration::from_secs(10))
                    .map_err(|_| ServerStoreError::StageCleanupIncomplete)?
                    .is_none()
                {
                    return Err(ServerStoreError::StageCleanupIncomplete);
                }
                return Err(failure.unwrap_or(ServerStoreError::Cancelled));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_state(space: &Path) -> Result<Option<WorkerState>, ServerStoreError> {
    let path = space.join("worker-state.json");
    if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
        return Ok(None);
    }
    let bytes = crate::core::server::content_staging::read_bounded_controlled(
        &path,
        MAX_WORKER_DOCUMENT,
        &ContentStageControl::default(),
    )?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| ServerStoreError::Corrupt)
}
