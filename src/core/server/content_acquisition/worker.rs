use std::collections::HashSet;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::core::api::{self, ModDiffSummary, ProgressEvent, RepositorySyncOptions, SyncMode};
use crate::core::db::{FoxyDb, params};
use crate::core::models::modification::{ADDON_COLUMNS, FoxyMod};
use crate::core::models::repository::normalize_repository_local_path_identity;
use crate::core::server::ServerStoreError;
use crate::core::server::content_staging::ContentStageProgress;
use crate::ui::types::SettingsViewState;

pub const MAX_WORKER_DOCUMENT: usize = 4 * 1024 * 1024;
const REQUEST_FILE: &str = "worker-request.json";
const STATE_FILE: &str = "worker-state.json";
const CANCEL_FILE: &str = "worker-cancel";
const MAX_ADDONS: usize = 2048;
const MAX_ADDON_NAME: usize = 255;
const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub schema_version: u32,
    pub context_fingerprint: String,
    pub game_space_id: String,
    pub game_id: String,
    pub repository_url: String,
    pub addons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerState {
    pub schema_version: u32,
    pub request_fingerprint: String,
    pub status: WorkerStatus,
    pub progress: ContentStageProgress,
    pub publication_checksum: Option<String>,
    pub addons: Vec<WorkerAddon>,
    pub planned_transfer_bytes: u64,
    pub full_download_bytes: u64,
    pub patch_files_planned: u64,
    pub message: Option<String>,
    #[serde(default)]
    pub database_opened: bool,
    #[serde(default)]
    pub downloads_started: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Preparing,
    Downloading,
    Verifying,
    Complete,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerAddon {
    pub name: String,
    pub checksum: String,
    pub verified: bool,
}

pub(crate) fn run() -> Result<(), ServerStoreError> {
    let root = crate::core::utils::app_paths::foxy_data_dir();
    let workspace = root
        .join("games")
        .join(crate::core::game::spaces::active_game_space().space_id);
    let request = read_request(&workspace.join(REQUEST_FILE))?;
    let fingerprint = request_fingerprint(&request)?;
    let mut state = WorkerState {
        schema_version: STATE_SCHEMA_VERSION,
        request_fingerprint: fingerprint,
        status: WorkerStatus::Preparing,
        progress: ContentStageProgress::default(),
        publication_checksum: None,
        addons: Vec::new(),
        planned_transfer_bytes: 0,
        full_download_bytes: 0,
        patch_files_planned: 0,
        message: None,
        database_opened: false,
        downloads_started: false,
    };
    persist_state(&workspace, &state)?;

    let result = execute(&root, &workspace, &request, &mut state);
    match result {
        Ok(()) => {
            state.status = WorkerStatus::Complete;
            state.progress.phase = crate::core::server::ContentStagePhase::Complete;
            persist_state(&workspace, &state)
        }
        Err(ServerStoreError::Cancelled) => {
            state.status = WorkerStatus::Cancelled;
            state.progress.phase = crate::core::server::ContentStagePhase::Cancelled;
            state
                .addons
                .iter_mut()
                .for_each(|addon| addon.verified = false);
            state.message = Some("Content acquisition was cancelled".to_owned());
            persist_state(&workspace, &state)
        }
        Err(error) => {
            state.status = WorkerStatus::Failed;
            state.progress.phase = crate::core::server::ContentStagePhase::Failed;
            state
                .addons
                .iter_mut()
                .for_each(|addon| addon.verified = false);
            state.message = Some(error.to_string());
            persist_state(&workspace, &state)?;
            if matches!(error, ServerStoreError::Io | ServerStoreError::Corrupt) {
                Err(error)
            } else {
                Ok(())
            }
        }
    }
}

fn execute(
    root: &Path,
    workspace: &Path,
    request: &WorkerRequest,
    state: &mut WorkerState,
) -> Result<(), ServerStoreError> {
    validate_workspace(root, workspace, request)?;
    check_cancel(workspace)?;
    let content = workspace.join("content");
    if !content.is_dir() {
        return Err(ServerStoreError::Invalid(
            "Private content directory is missing".into(),
        ));
    }
    let settings = SettingsViewState::default();
    let db_handle = crate::core::api::background_runtime()
        .ok_or_else(|| ServerStoreError::Invalid("Worker runtime is unavailable".into()))?
        .block_on(crate::core::tasks::init_database::init_database());
    state.database_opened = true;
    persist_state(workspace, state)?;
    let db = FoxyDb::from_handle(db_handle);

    let empty_selection = Vec::new();
    let (refresh_events, refresh_worker, refresh_cancel, refresh_pause) = spawn_stage(
        request,
        &content,
        &empty_selection,
        SyncMode::RemoteRefreshOnly,
        &settings,
        workspace,
    );
    let refresh = collect_stage(
        refresh_events,
        refresh_worker,
        refresh_cancel,
        refresh_pause,
        workspace,
        state,
        None,
    )?;
    if !refresh.finished {
        return Err(ServerStoreError::Invalid(
            "Repository refresh did not complete".into(),
        ));
    }

    let discovered = load_addons(&db, &request.repository_url, &content)?;
    let selection = select_addons(&discovered, &request.addons, &content)?;
    state.addons = selection
        .iter()
        .map(|addon| WorkerAddon {
            name: addon.name.clone(),
            checksum: addon.remote_checksum.clone(),
            verified: false,
        })
        .collect();
    let persisted = discovered
        .iter()
        .map(|addon| {
            (
                addon.name.clone(),
                selection.iter().any(|selected| selected.name == addon.name),
            )
        })
        .collect::<Vec<_>>();
    run_verification(
        request,
        &content,
        workspace,
        state,
        &settings,
        persisted.clone(),
        true,
    )?;

    state.downloads_started = true;
    persist_state(workspace, state)?;
    let (download_events, download_worker, download_cancel, download_pause) = spawn_stage(
        request,
        &content,
        &persisted,
        SyncMode::Download,
        &settings,
        workspace,
    );
    let downloaded = collect_stage(
        download_events,
        download_worker,
        download_cancel,
        download_pause,
        workspace,
        state,
        Some(WorkerStatus::Downloading),
    )?;
    if !downloaded.finished {
        return Err(ServerStoreError::Invalid(
            "Repository download did not complete".into(),
        ));
    }
    run_verification(
        request, &content, workspace, state, &settings, persisted, false,
    )?;

    let verified = load_addons(&db, &request.repository_url, &content)?;
    for selected in &mut state.addons {
        let addon = verified
            .iter()
            .find(|addon| addon.name == selected.name)
            .ok_or(ServerStoreError::NotFound)?;
        if addon.local_checksum.is_empty()
            || addon.remote_checksum.is_empty()
            || addon.local_checksum != addon.remote_checksum
        {
            return Err(ServerStoreError::Invalid(
                "Selected addon checksums do not match".into(),
            ));
        }
        selected.checksum = addon.remote_checksum.clone();
        selected.verified = true;
    }

    let repo_path = content.display().to_string();
    let repo_rows = crate::core::api::background_runtime()
        .ok_or(ServerStoreError::Corrupt)?
        .block_on(async {
            db.query_all(
                "SELECT local_path, remote_checksum FROM repositories WHERE remote_url = ?",
                params![request.repository_url.clone()],
            )
            .await
        })
        .map_err(|_| ServerStoreError::Corrupt)?;
    state.publication_checksum = repo_rows
        .iter()
        .find(|row| {
            row.get_string("local_path").is_ok_and(|path| {
                normalize_repository_local_path_identity(&path)
                    == normalize_repository_local_path_identity(&repo_path)
            })
        })
        .and_then(|row| row.get_string("remote_checksum").ok())
        .filter(|checksum| !checksum.is_empty());
    if state.publication_checksum.is_none()
        || state.addons.is_empty()
        || state.addons.iter().any(|addon| !addon.verified)
    {
        return Err(ServerStoreError::Invalid(
            "Published content could not be verified".into(),
        ));
    }
    Ok(())
}

fn run_verification(
    request: &WorkerRequest,
    content: &Path,
    workspace: &Path,
    state: &mut WorkerState,
    settings: &SettingsViewState,
    selection: Vec<(String, bool)>,
    allow_missing: bool,
) -> Result<(), ServerStoreError> {
    let (events, worker, cancel, pause) = spawn_stage(
        request,
        content,
        &selection,
        SyncMode::RecheckIntegrity,
        settings,
        workspace,
    );
    let result = collect_stage(
        events,
        worker,
        cancel,
        pause,
        workspace,
        state,
        Some(WorkerStatus::Verifying),
    )?;
    if !result.finished
        || result.verification_basis != Some((true, true))
        || !result.hash_evidence_seen
        || result.hash_incomplete
        || (!allow_missing && result.hash_issue_count != 0)
        || (!allow_missing && result.diff_needs_update)
        || !result.diff_seen
    {
        return Err(ServerStoreError::Invalid(format!(
            "Complete content verification was not established (seed={allow_missing}, finished={}, basis={:?}, evidence={}, incomplete={}, issues={}, diff={}, update={})",
            result.finished,
            result.verification_basis,
            result.hash_evidence_seen,
            result.hash_incomplete,
            result.hash_issue_count,
            result.diff_seen,
            result.diff_needs_update,
        )));
    }
    Ok(())
}

struct StageResult {
    finished: bool,
    verification_basis: Option<(bool, bool)>,
    hash_incomplete: bool,
    hash_evidence_seen: bool,
    hash_issue_count: usize,
    diff_seen: bool,
    diff_needs_update: bool,
}

fn spawn_stage(
    request: &WorkerRequest,
    content: &Path,
    selection: &[(String, bool)],
    mode: SyncMode,
    settings: &SettingsViewState,
    workspace: &Path,
) -> (
    broadcast::Receiver<ProgressEvent>,
    std::thread::JoinHandle<()>,
    watch::Sender<bool>,
    watch::Sender<bool>,
) {
    let (tx, rx) = broadcast::channel(512);
    let (_pause_tx, pause_rx) = watch::channel(false);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let options = RepositorySyncOptions {
        operation_id: api::next_operation_id("content-worker"),
        prepare_download_plan: false,
        repository_space_shared_path: None,
        repository_space_members: Vec::new(),
        auto_backup_directory: None,
        backup_local_drift: false,
        enrolled_link_targets: Vec::new(),
        interactive_hash_checks: false,
        rollback_temp_directory: Some(workspace.display().to_string()),
        download_speed_limit_mbps: None,
        recent_local_path_reset: false,
        discard_prepared_queue: true,
        force_redownload: false,
        force_full_downloads: false,
        allow_suspect_full_redownload: false,
        download_pause_rx: pause_rx,
        cancel_rx,
        hash_algorithm_preference: crate::ui::types::HashAlgorithmPreference::default(),
        hash_io_profile: settings.hash_io_profile,
        trust_verified_hashes: false,
        persisted_addon_selection: if mode == SyncMode::RemoteRefreshOnly {
            None
        } else {
            Some(selection.to_vec())
        },
    };
    let worker = api::spawn_repository_sync(
        request.repository_url.clone(),
        content.display().to_string(),
        selection.to_vec(),
        tx,
        mode,
        options,
        None,
    );
    (rx, worker, _cancel_tx, _pause_tx)
}

fn collect_stage(
    mut rx: broadcast::Receiver<ProgressEvent>,
    worker: std::thread::JoinHandle<()>,
    cancel_tx: watch::Sender<bool>,
    _pause_tx: watch::Sender<bool>,
    workspace: &Path,
    state: &mut WorkerState,
    status: Option<WorkerStatus>,
) -> Result<StageResult, ServerStoreError> {
    let selected_addons = state
        .addons
        .iter()
        .map(|addon| addon.name.clone())
        .collect::<Vec<_>>();
    let started = Instant::now();
    let mut finished = false;
    let mut failed = false;
    let mut cancelled = false;
    let mut verification_basis = None;
    let mut hash_incomplete = false;
    let mut hash_evidence_seen = false;
    let mut hash_issue_count = 0;
    let mut diff_seen = false;
    let mut diff_needs_update = false;
    let mut event_stream_lagged = false;
    let mut persistence_failed = false;
    let mut last_write = Instant::now();
    let mut final_hash_evidence = None;
    while !worker.is_finished() {
        if started.elapsed() > Duration::from_secs(15 * 60) || workspace.join(CANCEL_FILE).exists()
        {
            cancelled = true;
            let _ = cancel_tx.send(true);
        }
        if let Some(status) = status {
            state.status = status;
        }
        state.progress.phase = match state.status {
            WorkerStatus::Downloading => crate::core::server::ContentStagePhase::Copying,
            WorkerStatus::Verifying => crate::core::server::ContentStagePhase::Verifying,
            _ => crate::core::server::ContentStagePhase::Preparing,
        };
        if cancelled {
            state.status = WorkerStatus::Cancelled;
        }
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    if let ProgressEvent::HashEvidence { state } = &event {
                        final_hash_evidence = Some(state.clone());
                    }
                    observe_event(
                        event,
                        state,
                        &selected_addons,
                        &mut finished,
                        &mut failed,
                        &mut cancelled,
                        &mut verification_basis,
                        &mut hash_incomplete,
                        &mut hash_evidence_seen,
                        &mut hash_issue_count,
                        &mut diff_seen,
                        &mut diff_needs_update,
                    )
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    event_stream_lagged = true;
                    break;
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => break,
            }
        }
        if last_write.elapsed() >= Duration::from_millis(100) && !is_terminal(state.status) {
            if persist_state(workspace, state).is_err() {
                persistence_failed = true;
                cancelled = true;
                let _ = cancel_tx.send(true);
            }
            last_write = Instant::now();
        }
        if cancelled {
            let _ = cancel_tx.send(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let joined = worker.join();
    loop {
        match rx.try_recv() {
            Ok(event) => {
                if let ProgressEvent::HashEvidence { state } = &event {
                    final_hash_evidence = Some(state.clone());
                }
                observe_event(
                    event,
                    state,
                    &selected_addons,
                    &mut finished,
                    &mut failed,
                    &mut cancelled,
                    &mut verification_basis,
                    &mut hash_incomplete,
                    &mut hash_evidence_seen,
                    &mut hash_issue_count,
                    &mut diff_seen,
                    &mut diff_needs_update,
                )
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                event_stream_lagged = true;
                continue;
            }
            Err(broadcast::error::TryRecvError::Closed | broadcast::error::TryRecvError::Empty) => {
                break;
            }
        }
    }
    if joined.is_err() {
        return Err(ServerStoreError::Invalid(
            "Repository worker stopped unexpectedly".into(),
        ));
    }
    if persistence_failed {
        return Err(ServerStoreError::Io);
    }
    if event_stream_lagged {
        return Err(ServerStoreError::Invalid(
            "Repository progress evidence was incomplete".into(),
        ));
    }
    if failed {
        return Err(ServerStoreError::Invalid(
            "Repository pipeline failed".into(),
        ));
    }
    if cancelled {
        return Err(ServerStoreError::Cancelled);
    }
    if let Some(evidence) = final_hash_evidence {
        hash_incomplete = evidence.is_incomplete() || evidence.has_retry_save_pending();
        hash_issue_count = evidence.issues_snapshot().len();
    }
    Ok(StageResult {
        finished,
        verification_basis,
        hash_incomplete,
        hash_evidence_seen,
        hash_issue_count,
        diff_seen,
        diff_needs_update,
    })
}

#[allow(clippy::too_many_arguments)]
fn observe_event(
    event: ProgressEvent,
    state: &mut WorkerState,
    selected_addons: &[String],
    finished: &mut bool,
    failed: &mut bool,
    cancelled: &mut bool,
    verification_basis: &mut Option<(bool, bool)>,
    hash_incomplete: &mut bool,
    hash_evidence_seen: &mut bool,
    hash_issue_count: &mut usize,
    diff_seen: &mut bool,
    diff_needs_update: &mut bool,
) {
    match event {
        ProgressEvent::RecheckHashProgress {
            checked_files,
            total_files,
            checked_bytes,
            total_bytes,
            ..
        } => {
            state.progress.files_done = checked_files as u64;
            state.progress.total_files = total_files as u64;
            state.progress.bytes_done = checked_bytes;
            state.progress.total_bytes = total_bytes;
        }
        ProgressEvent::DownloadPlan {
            files_total,
            planned_bytes,
            full_bytes,
            patch_files,
        } => {
            state.progress.total_files = files_total as u64;
            state.progress.total_bytes = full_bytes;
            state.planned_transfer_bytes = planned_bytes;
            state.full_download_bytes = full_bytes;
            state.patch_files_planned = patch_files as u64;
        }
        ProgressEvent::DownloadMod {
            files_done,
            files_total,
            bytes_done,
            bytes_total,
            ..
        } => {
            state.progress.files_done = files_done as u64;
            state.progress.total_files = files_total as u64;
            state.progress.bytes_done = bytes_done;
            state.progress.total_bytes = bytes_total;
        }
        ProgressEvent::VerificationBasis { full, remote_fresh } => {
            *verification_basis = Some((full, remote_fresh))
        }
        ProgressEvent::HashEvidence { state: evidence } => {
            *hash_evidence_seen = true;
            let issues = evidence.issues_snapshot();
            *hash_issue_count = issues.len();
            *hash_incomplete = evidence.is_incomplete() || evidence.has_retry_save_pending();
        }
        ProgressEvent::Diff { mods } => {
            *diff_seen = true;
            *diff_needs_update = mods.iter().any(|addon: &ModDiffSummary| {
                selected_addons.contains(&addon.name) && addon.needs_update
            });
        }
        ProgressEvent::Finished => *finished = true,
        ProgressEvent::Failed(_) => *failed = true,
        ProgressEvent::Cancelled => *cancelled = true,
        _ => {}
    }
}

fn load_addons(db: &FoxyDb, url: &str, content: &Path) -> Result<Vec<FoxyMod>, ServerStoreError> {
    let path = content.display().to_string();
    let future = async {
        let repositories = db
            .query_all(
                "SELECT id, local_path FROM repositories WHERE remote_url = ?",
                params![url.to_owned()],
            )
            .await
            .map_err(|_| ServerStoreError::Corrupt)?;
        let repository_id = repositories
            .iter()
            .find(|row| {
                row.get_string("local_path").is_ok_and(|stored_path| {
                    normalize_repository_local_path_identity(&stored_path)
                        == normalize_repository_local_path_identity(&path)
                })
            })
            .ok_or(ServerStoreError::NotFound)?
            .get_i64("id")
            .map_err(|_| ServerStoreError::Corrupt)?;
        let qualified_columns = ADDON_COLUMNS
            .split(',')
            .map(|column| format!("a.{}", column.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = db
            .query_all(
                &format!(
                    "SELECT {qualified_columns} FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id WHERE ra.repository_id = ? ORDER BY a.name"
                ),
                params![repository_id],
            )
            .await
            .map_err(|_| ServerStoreError::Corrupt)?;
        rows.iter()
            .map(FoxyMod::from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ServerStoreError::Corrupt)
    };
    crate::core::api::background_runtime()
        .ok_or(ServerStoreError::Corrupt)?
        .block_on(future)
}

fn select_addons(
    discovered: &[FoxyMod],
    requested: &[String],
    content: &Path,
) -> Result<Vec<FoxyMod>, ServerStoreError> {
    if discovered.is_empty() || discovered.len() > MAX_ADDONS || requested.len() > MAX_ADDONS {
        return Err(ServerStoreError::Invalid(
            "Published addon set is empty or too large".into(),
        ));
    }
    let mut seen = HashSet::new();
    for addon in discovered {
        if !safe_addon_name(&addon.name)
            || addon.remote_checksum.trim().is_empty()
            || normalize_repository_local_path_identity(&addon.local_path)
                != normalize_repository_local_path_identity(
                    &content.join(&addon.name).display().to_string(),
                )
        {
            return Err(ServerStoreError::Invalid(
                "Published addon metadata is invalid".into(),
            ));
        }
        if !seen.insert(addon.name.as_str()) {
            return Err(ServerStoreError::Invalid(
                "Published addon names are duplicated".into(),
            ));
        }
    }
    if requested.is_empty() {
        return Ok(discovered.to_vec());
    }
    let mut requested_seen = HashSet::new();
    let mut selected = Vec::with_capacity(requested.len());
    for name in requested {
        if !safe_addon_name(name) || !requested_seen.insert(name.as_str()) {
            return Err(ServerStoreError::Invalid(
                "Requested addon selection is invalid".into(),
            ));
        }
        selected.push(
            discovered
                .iter()
                .find(|addon| addon.name == *name)
                .cloned()
                .ok_or(ServerStoreError::NotFound)?,
        );
    }
    if selected.is_empty() {
        return Err(ServerStoreError::Invalid("No addons were selected".into()));
    }
    Ok(selected)
}

fn safe_addon_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ADDON_NAME
        && name != "."
        && name != ".."
        && Path::new(name).components().count() == 1
        && Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && !name
            .chars()
            .any(|character| matches!(character, '/' | '\\' | ':'))
        && !name.chars().any(char::is_control)
}

fn validate_workspace(
    root: &Path,
    workspace: &Path,
    request: &WorkerRequest,
) -> Result<(), ServerStoreError> {
    let active = crate::core::game::spaces::active_game_space();
    if request.schema_version != STATE_SCHEMA_VERSION
        || request.context_fingerprint.is_empty()
        || request.game_space_id != active.space_id
        || request.game_id != active.game_id
        || workspace != root.join("games").join(&request.game_space_id)
        || crate::core::game::spaces::active_game_space_dir() != workspace
        || request.repository_url.trim().is_empty()
        || !request.repository_url.ends_with('/')
    {
        return Err(ServerStoreError::Ownership);
    }
    Ok(())
}

fn read_request(path: &Path) -> Result<WorkerRequest, ServerStoreError> {
    let bytes = crate::core::server::content_staging::read_bounded_controlled(
        path,
        MAX_WORKER_DOCUMENT,
        &crate::core::server::ContentStageControl::default(),
    )?;
    serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)
}

fn request_fingerprint(request: &WorkerRequest) -> Result<String, ServerStoreError> {
    let bytes = serde_json::to_vec(request).map_err(|_| ServerStoreError::Corrupt)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn persist_state(workspace: &Path, state: &WorkerState) -> Result<(), ServerStoreError> {
    let bytes = serde_json::to_vec(state).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_WORKER_DOCUMENT {
        return Err(ServerStoreError::Corrupt);
    }
    let path = workspace.join(STATE_FILE);
    crate::core::utils::fs_safety::atomic_write(&path, &bytes).map_err(|_| ServerStoreError::Io)
}

fn check_cancel(workspace: &Path) -> Result<(), ServerStoreError> {
    if workspace.join(CANCEL_FILE).exists() {
        Err(ServerStoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn is_terminal(status: WorkerStatus) -> bool {
    matches!(
        status,
        WorkerStatus::Complete | WorkerStatus::Failed | WorkerStatus::Cancelled
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_exact_and_rejects_duplicate_or_missing_names() {
        let addon = FoxyMod {
            name: "MainRepo".into(),
            remote_checksum: "abc".into(),
            local_path: "R:/worker/content/MainRepo".into(),
            ..Default::default()
        };
        let content = Path::new("R:/worker/content");
        assert_eq!(
            select_addons(std::slice::from_ref(&addon), &["MainRepo".into()], content)
                .unwrap()
                .len(),
            1
        );
        assert!(
            select_addons(
                std::slice::from_ref(&addon),
                &["MainRepo".into(), "MainRepo".into()],
                content
            )
            .is_err()
        );
        assert!(select_addons(&[addon], &["Other".into()], content).is_err());
    }

    #[test]
    fn addon_name_must_be_one_safe_basename() {
        assert!(safe_addon_name("RepoAlpha"));
        assert!(!safe_addon_name("../RepoAlpha"));
        assert!(!safe_addon_name(""));
    }
}
