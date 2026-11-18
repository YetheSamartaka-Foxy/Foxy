use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use foxy_supervisor::leases::InstallationLocks;
use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::model::{
    BACKUP_SCHEMA_VERSION, BackupArchiveEntry, BackupConfigurationSet, BackupConsistencyMode,
    BackupConsistencyWitness, BackupContentSet, BackupEngineSet, BackupProviderSet, BackupSaveSet,
    BackupSetKind, MAX_BACKUP_FILES, MAX_BACKUP_MANIFEST_BYTES, MAX_BACKUP_SINGLE_FILE_BYTES,
    MAX_BACKUP_TOTAL_BYTES, MAX_BACKUPS_PER_INSTANCE, ServerBackupListing, ServerBackupManifest,
    ServerBackupOutcome, ServerBackupPlan, ServerBackupSummary, ServerBackupVerificationReport,
    is_forbidden_secret_archive_path, persistence_providers, valid_archive_relative_path,
    valid_backup_id, valid_lower_hex,
};
use super::retention::{
    BACKUP_RETENTION_FILE, load_retention_doc, plan_prune_from_listing, valid_quarantine_name,
    valid_retention_receipt_name,
};
use crate::core::server::content_staging::{
    ContentStageControl, publish_no_replace, read_bounded_controlled,
};
use crate::core::server::store::{check_directory_chain, check_regular_file};
use crate::core::server::{
    InstanceRuntimePhase, InstanceRuntimeStatus, ServerInstallation, ServerInstance,
    ServerInstanceId, ServerStore, ServerStoreError,
};

const BACKUP_STAGE_PREFIX: &str = ".backup-build-";
const MAX_TREE_DEPTH: usize = 64;

enum ArchivePayload {
    File(PathBuf),
    Bytes(Vec<u8>),
}

pub(super) struct PlannedArchiveItem {
    entry: BackupArchiveEntry,
    payload: ArchivePayload,
}

#[derive(Serialize)]
struct EngineMetadataSnapshot<'a> {
    installation: Option<&'a ServerInstallation>,
    build_observation: Option<crate::core::server::InstallationBuildObservation>,
    engine_set: &'a BackupEngineSet,
}

#[derive(Serialize)]
struct PlanFingerprintInput<'a> {
    game_space_id: &'a str,
    game_id: &'a str,
    instance_id: &'a ServerInstanceId,
    consistency: &'a BackupConsistencyWitness,
    engine_set: &'a BackupEngineSet,
    configuration_set: &'a BackupConfigurationSet,
    content_set: &'a BackupContentSet,
    provider_set: &'a BackupProviderSet,
    save_set: &'a BackupSaveSet,
    entries: &'a [BackupArchiveEntry],
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn ensure_ordinary_dir(path: &Path) -> Result<(), ServerStoreError> {
    check_directory_chain(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| ServerStoreError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

pub(super) fn hash_and_measure_file(
    path: &Path,
    control: &ContentStageControl,
) -> Result<(u64, String), ServerStoreError> {
    check_regular_file(path)?;
    let mut file = File::open(path).map_err(|_| ServerStoreError::Io)?;
    let metadata = file.metadata().map_err(|_| ServerStoreError::Io)?;
    if !metadata.is_file() {
        return Err(ServerStoreError::UnsafePath);
    }
    if metadata.len() > MAX_BACKUP_SINGLE_FILE_BYTES {
        return Err(ServerStoreError::Invalid(format!(
            "File {} exceeds the single-file backup limit of {MAX_BACKUP_SINGLE_FILE_BYTES} bytes.",
            path.display()
        )));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        control.checkpoint()?;
        let read = file.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(ServerStoreError::Corrupt)?;
        if total > MAX_BACKUP_SINGLE_FILE_BYTES {
            return Err(ServerStoreError::Invalid(
                "Backup input grew past the single-file size limit while reading.".into(),
            ));
        }
        hasher.update(&buffer[..read]);
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

fn count_secret_references(bytes: &[u8]) -> usize {
    let text = String::from_utf8_lossy(bytes);
    text.matches("foxy-secret:").count()
}

fn push_synthetic(
    items: &mut BTreeMap<String, PlannedArchiveItem>,
    archive_path: &str,
    set: BackupSetKind,
    bytes: Vec<u8>,
) -> Result<(), ServerStoreError> {
    if !valid_archive_relative_path(archive_path) || is_forbidden_secret_archive_path(archive_path)
    {
        return Err(ServerStoreError::UnsafePath);
    }
    let size_bytes = bytes.len() as u64;
    if size_bytes > MAX_BACKUP_SINGLE_FILE_BYTES {
        return Err(ServerStoreError::Invalid(
            "Backup metadata snapshot exceeds the single-file size limit.".into(),
        ));
    }
    let blake3 = blake3::hash(&bytes).to_hex().to_string();
    items.insert(
        archive_path.to_owned(),
        PlannedArchiveItem {
            entry: BackupArchiveEntry {
                archive_path: archive_path.to_owned(),
                set,
                size_bytes,
                blake3,
            },
            payload: ArchivePayload::Bytes(bytes),
        },
    );
    Ok(())
}

fn push_file(
    items: &mut BTreeMap<String, PlannedArchiveItem>,
    archive_path: String,
    set: BackupSetKind,
    path: PathBuf,
    control: &ContentStageControl,
) -> Result<(u64, usize), ServerStoreError> {
    if !valid_archive_relative_path(&archive_path)
        || is_forbidden_secret_archive_path(&archive_path)
    {
        return Err(ServerStoreError::UnsafePath);
    }
    if items.len() >= MAX_BACKUP_FILES {
        return Err(ServerStoreError::Invalid(format!(
            "Backup exceeds the maximum of {MAX_BACKUP_FILES} files."
        )));
    }
    let (size_bytes, blake3) = hash_and_measure_file(&path, control)?;
    let secret_refs = if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "json" | "cfg" | "txt" | "Arma3Profile"))
        && size_bytes <= 4 * 1024 * 1024
    {
        fs::read(&path)
            .map(|bytes| count_secret_references(&bytes))
            .unwrap_or(0)
    } else {
        0
    };
    items.insert(
        archive_path.clone(),
        PlannedArchiveItem {
            entry: BackupArchiveEntry {
                archive_path,
                set,
                size_bytes,
                blake3,
            },
            payload: ArchivePayload::File(path),
        },
    );
    Ok((size_bytes, secret_refs))
}

fn collect_tree(
    root: &Path,
    archive_prefix: &str,
    set: BackupSetKind,
    items: &mut BTreeMap<String, PlannedArchiveItem>,
    secret_refs: &mut usize,
    filter: &dyn Fn(&str) -> bool,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    if !root.try_exists().map_err(|_| ServerStoreError::Io)? {
        return Ok(());
    }
    ensure_ordinary_dir(root)?;
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel_dir, current_depth)) = stack.pop() {
        control.checkpoint()?;
        if current_depth > MAX_TREE_DEPTH {
            return Err(ServerStoreError::UnsafePath);
        }
        let mut children = fs::read_dir(&dir)
            .map_err(|_| ServerStoreError::Io)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ServerStoreError::Io)?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            control.checkpoint()?;
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| ServerStoreError::UnsafePath)?;
            if name.starts_with('.') {
                continue;
            }
            let rel = if rel_dir.is_empty() {
                name
            } else {
                format!("{rel_dir}/{name}")
            };
            if !filter(&rel) {
                continue;
            }
            let path = child.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(ServerStoreError::UnsafePath);
            }
            if metadata.is_dir() {
                stack.push((path, rel, current_depth + 1));
            } else if metadata.is_file() {
                let archive_path = if archive_prefix.is_empty() {
                    rel
                } else {
                    format!("{archive_prefix}/{rel}")
                };
                let (_, refs) = push_file(items, archive_path, set, path, control)?;
                *secret_refs = secret_refs.saturating_add(refs);
            } else {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    Ok(())
}

fn inspect_runtime_state_files(
    runtime_state_root: &Path,
    items: &mut BTreeMap<String, PlannedArchiveItem>,
    excluded_secrets: &mut BTreeSet<String>,
    excluded_transients: &mut BTreeSet<String>,
    control: &ContentStageControl,
) -> Result<(bool, usize, u64), ServerStoreError> {
    if !runtime_state_root
        .try_exists()
        .map_err(|_| ServerStoreError::Io)?
    {
        return Ok((false, 0, 0));
    }
    ensure_ordinary_dir(runtime_state_root)?;
    let mut save_files = 0usize;
    let mut save_bytes = 0u64;
    let mut stack = vec![(runtime_state_root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, rel_dir, depth)) = stack.pop() {
        control.checkpoint()?;
        if depth > MAX_TREE_DEPTH {
            return Err(ServerStoreError::UnsafePath);
        }
        let mut children = fs::read_dir(&dir)
            .map_err(|_| ServerStoreError::Io)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ServerStoreError::Io)?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            control.checkpoint()?;
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| ServerStoreError::UnsafePath)?;
            let rel = if rel_dir.is_empty() {
                name.clone()
            } else {
                format!("{rel_dir}/{name}")
            };
            let lower = rel.to_ascii_lowercase();
            let lower_name = name.to_ascii_lowercase();
            let path = child.path();
            let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
                return Err(ServerStoreError::UnsafePath);
            }
            if is_runtime_secret_rel(&lower, &lower_name) {
                excluded_secrets.insert(format!("deployments/runtime-state/{rel}"));
                continue;
            }
            if is_runtime_transient_rel(&lower, &lower_name) {
                excluded_transients.insert(format!("deployments/runtime-state/{rel}"));
                continue;
            }
            if metadata.is_dir() {
                stack.push((path, rel, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                return Err(ServerStoreError::UnsafePath);
            }
            if should_archive_runtime_state_rel(&lower) {
                let archive_path = format!("saves/{rel}");
                let (bytes, _) =
                    push_file(items, archive_path, BackupSetKind::Save, path, control)?;
                save_files = save_files.saturating_add(1);
                save_bytes = save_bytes
                    .checked_add(bytes)
                    .ok_or(ServerStoreError::Corrupt)?;
            } else {
                excluded_transients.insert(format!("deployments/runtime-state/{rel}"));
            }
        }
    }
    Ok((true, save_files, save_bytes))
}

fn is_runtime_secret_rel(lower_rel: &str, lower_name: &str) -> bool {
    lower_name == "ownertoken.bin"
        || lower_name == "join.par"
        || (lower_name.starts_with("beserver") && lower_name.ends_with(".cfg"))
        || lower_rel == "home/private-configuration"
        || lower_rel.starts_with("home/private-configuration/")
        || lower_rel == "home/battleye"
        || lower_rel.starts_with("home/battleye/")
        || (lower_rel.starts_with("headless/")
            && (lower_rel.ends_with("/private") || lower_rel.contains("/private/")))
}

fn is_runtime_transient_rel(lower_rel: &str, lower_name: &str) -> bool {
    lower_rel == "home/cache"
        || lower_rel.starts_with("home/cache/")
        || lower_rel == "home/runtime"
        || lower_rel.starts_with("home/runtime/")
        || lower_rel == "home/data"
        || lower_rel.starts_with("home/data/")
        || lower_rel == "monitoring"
        || lower_rel.starts_with("monitoring/")
        || (lower_name.starts_with("run_") && lower_name.ends_with(".log"))
        || (lower_rel.starts_with("headless/")
            && (lower_rel.contains("/home/cache")
                || lower_rel.contains("/home/runtime")
                || lower_rel.contains("/home/data")))
}

fn should_archive_runtime_state_rel(lower_rel: &str) -> bool {
    lower_rel == "ownership.json"
        || lower_rel.starts_with("home/profiles/")
        || lower_rel.starts_with("home/config/")
        || (lower_rel.starts_with("headless/")
            && (lower_rel.contains("/home/profiles/") || lower_rel.contains("/home/config/")))
}

pub(super) fn inspect_consistency_witness(
    store: &ServerStore,
    id: &ServerInstanceId,
    instance: &ServerInstance,
    all_instances: &[ServerInstance],
    installation: Option<&ServerInstallation>,
    mode: BackupConsistencyMode,
    runtime_witness: Option<&InstanceRuntimeStatus>,
) -> Result<(BackupConsistencyWitness, Vec<String>), ServerStoreError> {
    let mut blockers = Vec::new();
    let mut notes = Vec::new();

    let (
        owner_running,
        owner_id,
        owner_generation,
        observed_phase,
        observed_run_generation,
        cleanup_verified,
        headless_running,
        process_active,
        owner_inspection_ok,
    ) = if let Some(status) = runtime_witness {
        if &status.instance_id != id {
            return Err(ServerStoreError::Invalid(
                "Runtime status witness belongs to a different server instance.".into(),
            ));
        }
        let active = status.process_exists
            || status.headless_running > 0
            || !matches!(
                status.phase,
                InstanceRuntimePhase::NotOwned | InstanceRuntimePhase::Exited
            );
        let cleanup_ok = status.phase == InstanceRuntimePhase::NotOwned || status.cleanup_verified;
        (
            true,
            Some(status.owner_id.clone()),
            Some(status.owner_generation),
            Some(status.phase),
            (status.run_generation > 0).then_some(status.run_generation),
            cleanup_ok,
            status.headless_running as usize,
            active,
            true,
        )
    } else {
        match crate::core::server::owner::attach(store.context()) {
            Ok(Some(attachment)) => match attachment.instance_status(id) {
                Ok(status) => {
                    let active = status.process_exists
                        || status.headless_running > 0
                        || !matches!(
                            status.phase,
                            InstanceRuntimePhase::NotOwned | InstanceRuntimePhase::Exited
                        );
                    let cleanup_ok =
                        status.phase == InstanceRuntimePhase::NotOwned || status.cleanup_verified;
                    (
                        true,
                        Some(status.owner_id.clone()),
                        Some(status.owner_generation),
                        Some(status.phase),
                        (status.run_generation > 0).then_some(status.run_generation),
                        cleanup_ok,
                        status.headless_running as usize,
                        active,
                        true,
                    )
                }
                Err(_) => {
                    blockers.push(
                        "The active server owner did not confirm runtime status for this instance."
                            .into(),
                    );
                    (false, None, None, None, None, false, 0, true, false)
                }
            },
            Ok(None) => (
                false,
                None,
                None,
                Some(InstanceRuntimePhase::NotOwned),
                None,
                true,
                0,
                false,
                true,
            ),
            Err(_) => {
                notes.push(
                    "Recorded owner is unreachable; stopped state cannot be verified without crash recovery."
                        .into(),
                );
                (false, None, None, None, None, false, 0, false, false)
            }
        }
    };

    let journal_path = store.record_root(id).join("activation/journal.json");
    check_regular_file(&journal_path)?;
    let pending_activation_journal = journal_path
        .try_exists()
        .map_err(|_| ServerStoreError::Io)?;
    if pending_activation_journal {
        blockers.push(
            "A deployment activation journal is pending recovery. Resolve it before taking a backup."
                .into(),
        );
    }

    let mut active_consumers = 0usize;
    let mut pending_installation_maintenance = false;
    let mut locks_inspection_ok = true;
    let mut consumers_by_instance: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(installation) = installation {
        match InstallationLocks::capture(&installation.directory) {
            Ok(locks) => {
                match locks.inspect_consumers() {
                    Ok(records) => {
                        for record in records {
                            if record.session.game_space_id == store.context().space_id() {
                                *consumers_by_instance
                                    .entry(record.session.instance_id.clone())
                                    .or_default() += 1;
                            }
                            if record.session.game_space_id == store.context().space_id()
                                && record.session.instance_id == id.as_str()
                            {
                                active_consumers += 1;
                            }
                        }
                    }
                    Err(_) => {
                        locks_inspection_ok = false;
                        notes.push(
                            "Physical installation consumer registrations could not be verified."
                                .into(),
                        );
                    }
                }
                match locks.inspect_maintenance() {
                    Ok(inventory) => {
                        pending_installation_maintenance = inventory.pending.is_some();
                    }
                    Err(_) => {
                        locks_inspection_ok = false;
                        notes.push(
                            "Physical installation maintenance lock state could not be verified."
                                .into(),
                        );
                    }
                }
            }
            Err(_) => {
                locks_inspection_ok = false;
                notes.push(
                    "Adopted installation directory is unavailable for consumer lock verification."
                        .into(),
                );
            }
        }
    }

    let mut shared_save_path_conflicts = Vec::new();
    let own_state_root = store.record_root(id).join("runtime-state");
    let own_physical_state = own_state_root.canonicalize().ok();
    for other in all_instances {
        if &other.id == id {
            continue;
        }
        let other_state_root = store.record_root(&other.id).join("runtime-state");
        if other_state_root == own_state_root
            || (own_physical_state.is_some()
                && other_state_root.canonicalize().ok() == own_physical_state)
        {
            shared_save_path_conflicts.push(format!(
                "Shared runtime-state directory with {} ({})",
                other.display_name,
                other.id.as_str()
            ));
        }
    }
    if let Some(other_active) = consumers_by_instance
        .iter()
        .find(|(other_id, count)| **count > 0 && other_id.as_str() != id.as_str())
        && !shared_save_path_conflicts.is_empty()
    {
        notes.push(format!(
            "Another instance ({}) holds an active installation consumer while sharing save paths.",
            other_active.0
        ));
    }
    let _ = instance;

    let stopped_verified = owner_inspection_ok
        && locks_inspection_ok
        && !process_active
        && cleanup_verified
        && headless_running == 0
        && active_consumers == 0
        && !pending_activation_journal
        && !pending_installation_maintenance
        && shared_save_path_conflicts.is_empty();

    if mode == BackupConsistencyMode::StoppedVerified && !stopped_verified {
        if !owner_inspection_ok {
            blockers.push(
                "Stopped consistency could not be verified because owner state is unreadable or crashed."
                    .into(),
            );
        }
        if process_active {
            blockers.push(
                "The server process or headless client is still active. Stop the instance first or explicitly choose best-effort live backup."
                    .into(),
            );
        }
        if !cleanup_verified {
            blockers.push(
                "Previous process cleanup is not verified. Inspect recovery before taking a stopped-consistent backup."
                    .into(),
            );
        }
        if active_consumers > 0 || !locks_inspection_ok {
            blockers.push(
                "Physical installation consumer registrations remain active or unverified; resolve them before taking a stopped-consistent backup."
                    .into(),
            );
        }
        if pending_installation_maintenance {
            blockers.push(
                "Physical installation maintenance is pending; resolve it before taking a stopped-consistent backup."
                    .into(),
            );
        }
        if !shared_save_path_conflicts.is_empty() {
            blockers.push(
                "Another instance shares mutable save storage; resolve shared save paths before taking a stopped-consistent backup."
                    .into(),
            );
        }
    } else if mode == BackupConsistencyMode::BestEffortLive && !stopped_verified {
        notes.push(
            "Best-effort live backup does not freeze active writers; saves may reflect mid-write engine state."
                .into(),
        );
    }

    Ok((
        BackupConsistencyWitness {
            mode,
            stopped_verified,
            owner_running,
            owner_id,
            owner_generation,
            observed_phase,
            observed_run_generation,
            cleanup_verified,
            headless_running,
            active_consumers,
            pending_activation_journal,
            pending_installation_maintenance,
            shared_save_path_conflicts,
            notes,
        },
        blockers,
    ))
}

pub(super) fn build_backup_plan_internal(
    store: &ServerStore,
    id: &ServerInstanceId,
    mode: BackupConsistencyMode,
    runtime_witness: Option<&InstanceRuntimeStatus>,
    control: &ContentStageControl,
) -> Result<(ServerBackupPlan, Vec<PlannedArchiveItem>), ServerStoreError> {
    if !store.capabilities().server_backups {
        return Err(ServerStoreError::UnsupportedGame);
    }
    store.require_no_server_restore(id)?;
    store.validate_record_context()?;
    let (catalog, catalog_hash, _) = store.read()?;
    let catalog_fingerprint = catalog_hash
        .map(|hash| hash.to_hex().to_string())
        .ok_or(ServerStoreError::NotFound)?;
    let instance = catalog
        .instances
        .iter()
        .find(|item| &item.id == id)
        .ok_or(ServerStoreError::NotFound)?
        .clone();
    let installation = instance.installation_id.as_ref().and_then(|inst_id| {
        catalog
            .installations
            .iter()
            .find(|item| &item.id == inst_id)
            .cloned()
    });

    let (consistency, mut blockers) = inspect_consistency_witness(
        store,
        id,
        &instance,
        &catalog.instances,
        installation.as_ref(),
        mode,
        runtime_witness,
    )?;

    let instance_root = store
        .context()
        .space_dir()
        .join("server/instances")
        .join(id.as_str());
    check_directory_chain(&instance_root)?;

    let (config_state, _) = store.read_configuration_state_controlled(id, control)?;
    let (record_state, _) = store.read_record_state(id, control)?;
    let pins = store.deployment_pins(id)?;
    let active_state = store
        .read_activation_state(id, control)
        .map_err(|_| ServerStoreError::Corrupt)?
        .map(|(state, _)| state);

    let mut stage_ids = BTreeSet::new();
    let active_record_id = active_state
        .as_ref()
        .map(|state| state.deployment.record_id.clone());
    if let Some(record_id) = active_record_id
        .as_deref()
        .or(record_state.selected.as_deref())
        && let Ok(manifest) = store.read_record(id, record_id, control)
    {
        stage_ids.insert(manifest.content_stage_id);
    }
    let reforger_addon_lock_digest = store
        .selected_reforger_addon_lock(id)
        .ok()
        .flatten()
        .and_then(|lock| serde_json::to_vec(&lock).ok())
        .map(|encoded| blake3::hash(&encoded).to_hex().to_string());

    let active_preparation_id = active_state
        .as_ref()
        .map(|state| state.deployment.preparation_id.clone());
    let active_runtime_id = active_state
        .as_ref()
        .and_then(|state| state.deployment.runtime.as_ref())
        .map(|runtime| runtime.runtime_id.clone());
    let runtime_manifest_digest = active_state
        .as_ref()
        .and_then(|state| state.deployment.runtime.as_ref())
        .map(|runtime| runtime.manifest_fingerprint.clone());

    let build_observation = installation
        .as_ref()
        .filter(|inst| inst.directory.exists())
        .map(crate::core::server::installation::observe_build);
    let engine_build_id = build_observation
        .as_ref()
        .and_then(|obs| obs.steam_manifest.as_ref().map(|m| m.build_id.clone()))
        .or_else(|| {
            installation
                .as_ref()
                .map(|inst| inst.evidence.blake3.clone())
        });

    let channel = installation
        .as_ref()
        .map(|installation| store.backup_installation_channel(installation))
        .transpose()?
        .flatten();
    let engine_set = BackupEngineSet {
        installation_id: installation.as_ref().map(|inst| inst.id.clone()),
        installation_mode: installation
            .as_ref()
            .map(|inst| format!("{:?}", inst.ownership).to_ascii_lowercase()),
        engine_build_id,
        active_deployment_record_id: active_record_id.clone(),
        active_preparation_id,
        active_runtime_id,
        runtime_manifest_digest,
        payload_mode: "reference_metadata_only".into(),
        channel: channel.as_ref().map(|(channel, _)| *channel),
        installation_receipt_digest: channel.map(|(_, digest)| digest),
    };

    let content_set = BackupContentSet {
        selected_deployment_record_id: record_state.selected.clone(),
        active_deployment_record_id: active_record_id,
        pinned_deployment_records: pins.records,
        content_stage_ids: stage_ids.into_iter().collect(),
        reforger_addon_lock_digest,
    };

    let (mut covered_providers, mut uncovered_providers) =
        persistence_providers(instance.definition.kind());
    let mut reforger_save_tree_version = None;
    if instance.definition.kind() == crate::core::server::ServerDefinitionKind::Reforger {
        let config = if let Some(runtime) = active_state
            .as_ref()
            .and_then(|state| state.deployment.runtime.as_ref())
        {
            store.verify_native_runtime(id, runtime, control)?;
            read_bounded_controlled(
                &store
                    .runtime_directory(id, runtime)?
                    .join("configuration/server.json"),
                1024 * 1024,
                control,
            )?
        } else {
            crate::core::server::configuration::render(&instance)?
                .files
                .into_iter()
                .find(|file| file.relative_path == "server.json")
                .ok_or(ServerStoreError::Corrupt)?
                .contents
                .into_bytes()
        };
        let coverage = crate::core::server::saves::provider_coverage(&config)?;
        if coverage.local_default {
            reforger_save_tree_version = Some(1);
        } else {
            covered_providers.clear();
            uncovered_providers.push("reforger_custom_storage_configuration".into());
            blockers.extend(coverage.blockers);
        }
    }
    let provider_set = BackupProviderSet {
        covered_providers,
        uncovered_providers,
        writer_stopped_verified: consistency.stopped_verified,
        reforger_save_tree_version,
        shared_save_path_conflicts: consistency.shared_save_path_conflicts.clone(),
    };

    let mut items: BTreeMap<String, PlannedArchiveItem> = BTreeMap::new();
    let instance_bytes =
        serde_json::to_vec_pretty(&instance).map_err(|_| ServerStoreError::Corrupt)?;
    let mut secret_reference_count = count_secret_references(&instance_bytes);
    push_synthetic(
        &mut items,
        "metadata/instance.json",
        BackupSetKind::Configuration,
        instance_bytes,
    )?;

    let engine_snapshot_bytes = serde_json::to_vec_pretty(&EngineMetadataSnapshot {
        installation: installation.as_ref(),
        build_observation,
        engine_set: &engine_set,
    })
    .map_err(|_| ServerStoreError::Corrupt)?;
    push_synthetic(
        &mut items,
        "metadata/installation.json",
        BackupSetKind::Engine,
        engine_snapshot_bytes,
    )?;

    let provider_bytes =
        serde_json::to_vec_pretty(&provider_set).map_err(|_| ServerStoreError::Corrupt)?;
    push_synthetic(
        &mut items,
        "metadata/provider_coverage.json",
        BackupSetKind::Provider,
        provider_bytes,
    )?;

    collect_tree(
        &instance_root.join("configuration"),
        "configuration",
        BackupSetKind::Configuration,
        &mut items,
        &mut secret_reference_count,
        &|rel| !is_forbidden_secret_archive_path(rel),
        control,
    )?;

    let bindings_path = instance_root.join("content-bindings.json");
    if bindings_path
        .try_exists()
        .map_err(|_| ServerStoreError::Io)?
    {
        let (_, refs) = push_file(
            &mut items,
            "content/content-bindings.json".into(),
            BackupSetKind::Content,
            bindings_path,
            control,
        )?;
        secret_reference_count = secret_reference_count.saturating_add(refs);
    }

    let deployments_root = store.record_root(id);
    if deployments_root
        .try_exists()
        .map_err(|_| ServerStoreError::Io)?
    {
        ensure_ordinary_dir(&deployments_root)?;
        for name in ["state.json", "pins.json"] {
            let path = deployments_root.join(name);
            if path.try_exists().map_err(|_| ServerStoreError::Io)? {
                push_file(
                    &mut items,
                    format!("content/deployments/{name}"),
                    BackupSetKind::Content,
                    path,
                    control,
                )?;
            }
        }
        collect_tree(
            &deployments_root.join("records"),
            "content/deployments/records",
            BackupSetKind::Content,
            &mut items,
            &mut secret_reference_count,
            &|rel| rel.ends_with("/manifest.json") || rel == "manifest.json",
            control,
        )?;
        collect_tree(
            &deployments_root.join("prepared"),
            "content/deployments/prepared",
            BackupSetKind::Content,
            &mut items,
            &mut secret_reference_count,
            &|rel| rel.ends_with("/manifest.json"),
            control,
        )?;
        let active_path = deployments_root.join("activation/active.json");
        if active_path.try_exists().map_err(|_| ServerStoreError::Io)? {
            push_file(
                &mut items,
                "content/deployments/activation/active.json".into(),
                BackupSetKind::Content,
                active_path,
                control,
            )?;
        }
        collect_tree(
            &deployments_root.join("runtimes"),
            "content/deployments/runtimes",
            BackupSetKind::Engine,
            &mut items,
            &mut secret_reference_count,
            &|rel| rel.ends_with("/manifest.json"),
            control,
        )?;
    }

    let mut excluded_secrets = BTreeSet::new();
    excluded_secrets.insert("deployments/runtime-state/home/private-configuration/**".into());
    excluded_secrets.insert("deployments/runtime-state/home/battleye/**".into());
    excluded_secrets.insert("deployments/runtime-state/headless/*/join.par".into());
    excluded_secrets.insert("**/ownerToken.bin".into());

    let mut excluded_transients = BTreeSet::new();
    excluded_transients.insert("deployments/runtime-state/home/cache/**".into());
    excluded_transients.insert("deployments/runtime-state/home/runtime/**".into());
    excluded_transients.insert("deployments/runtime-state/home/data/**".into());
    excluded_transients.insert("deployments/runtime-state/run_*.log".into());

    let runtime_state_root = deployments_root.join("runtime-state");
    let (runtime_state_present, save_file_count, save_total_bytes) = inspect_runtime_state_files(
        &runtime_state_root,
        &mut items,
        &mut excluded_secrets,
        &mut excluded_transients,
        control,
    )?;

    let (runtime_state_owner_generation, runtime_state_run_generation) = consistency
        .owner_generation
        .map_or((None, None), |owner_gen| {
            (Some(owner_gen), consistency.observed_run_generation)
        });

    let configuration_set = BackupConfigurationSet {
        instance_revision: instance.revision,
        catalog_fingerprint,
        active_configuration_revision_id: config_state.active,
        secret_reference_count,
        excluded_secret_paths: excluded_secrets.into_iter().collect(),
    };
    let save_set = BackupSaveSet {
        runtime_state_present,
        runtime_state_owner_generation,
        runtime_state_run_generation,
        save_file_count,
        save_total_bytes,
        excluded_transient_paths: excluded_transients.into_iter().collect(),
    };

    let planned: Vec<PlannedArchiveItem> = items.into_values().collect();
    if planned.len() > MAX_BACKUP_FILES {
        blockers.push(format!(
            "Backup file count ({}) exceeds the limit of {MAX_BACKUP_FILES}.",
            planned.len()
        ));
    }
    let total_uncompressed_bytes = planned
        .iter()
        .try_fold(0u64, |sum, item| sum.checked_add(item.entry.size_bytes))
        .ok_or(ServerStoreError::Corrupt)?;
    if total_uncompressed_bytes > MAX_BACKUP_TOTAL_BYTES {
        blockers.push(format!(
            "Backup uncompressed size ({total_uncompressed_bytes} bytes) exceeds the limit of {MAX_BACKUP_TOTAL_BYTES} bytes."
        ));
    }
    let entries: Vec<BackupArchiveEntry> = planned.iter().map(|item| item.entry.clone()).collect();
    let fp_bytes = serde_json::to_vec(&PlanFingerprintInput {
        game_space_id: store.context().space_id(),
        game_id: store.context().game_id(),
        instance_id: id,
        consistency: &consistency,
        engine_set: &engine_set,
        configuration_set: &configuration_set,
        content_set: &content_set,
        provider_set: &provider_set,
        save_set: &save_set,
        entries: &entries,
    })
    .map_err(|_| ServerStoreError::Corrupt)?;
    let fingerprint = blake3::hash(&fp_bytes).to_hex().to_string();

    Ok((
        ServerBackupPlan {
            game_space_id: store.context().space_id().into(),
            game_id: store.context().game_id().into(),
            instance_id: id.clone(),
            instance_name: instance.display_name,
            consistency,
            engine_set,
            configuration_set,
            content_set,
            provider_set,
            save_set,
            planned_files: entries.len(),
            planned_uncompressed_bytes: total_uncompressed_bytes,
            entries,
            blockers,
            fingerprint,
            writes_performed: false,
        },
        planned,
    ))
}

fn new_backup_id(
    space_id: &str,
    instance_id: &ServerInstanceId,
    created_unix_ms: u64,
    plan_fp: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"foxy-server-backup-v1\0");
    hasher.update(space_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(instance_id.as_str().as_bytes());
    hasher.update(&created_unix_ms.to_le_bytes());
    let nonce: [u8; 16] = rand::random();
    hasher.update(&nonce);
    hasher.update(plan_fp.as_bytes());
    format!("backup_{}", hasher.finalize().to_hex())
}

fn write_zip_archive(
    archive_path: &Path,
    manifest_bytes: &[u8],
    items: &[PlannedArchiveItem],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(archive_path)
        .map_err(|_| ServerStoreError::Io)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut buffer = vec![0u8; 64 * 1024];
    writer
        .start_file("manifest.json", options)
        .map_err(|_| ServerStoreError::Io)?;
    writer
        .write_all(manifest_bytes)
        .map_err(|_| ServerStoreError::Io)?;
    for item in items {
        control.checkpoint()?;
        writer
            .start_file(&item.entry.archive_path, options)
            .map_err(|_| ServerStoreError::Io)?;
        match &item.payload {
            ArchivePayload::Bytes(bytes) => {
                writer.write_all(bytes).map_err(|_| ServerStoreError::Io)?;
            }
            ArchivePayload::File(source_path) => {
                check_regular_file(source_path)?;
                let mut reader = File::open(source_path).map_err(|_| ServerStoreError::Io)?;
                let mut total = 0u64;
                loop {
                    control.checkpoint()?;
                    let read = reader.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
                    if read == 0 {
                        break;
                    }
                    total = total
                        .checked_add(read as u64)
                        .ok_or(ServerStoreError::Corrupt)?;
                    if total > item.entry.size_bytes {
                        return Err(ServerStoreError::StaleRevision);
                    }
                    writer
                        .write_all(&buffer[..read])
                        .map_err(|_| ServerStoreError::Io)?;
                }
                if total != item.entry.size_bytes {
                    return Err(ServerStoreError::StaleRevision);
                }
            }
        }
    }
    let finished = writer.finish().map_err(|_| ServerStoreError::Io)?;
    finished.sync_all().map_err(|_| ServerStoreError::Io)?;
    Ok(())
}

pub(super) fn verify_backup_directory(
    store: &ServerStore,
    id: &ServerInstanceId,
    backup_id: &str,
    directory: &Path,
    control: &ContentStageControl,
) -> Result<(ServerBackupVerificationReport, ServerBackupManifest), ServerStoreError> {
    if !valid_backup_id(backup_id) {
        return Err(ServerStoreError::Invalid(
            "Invalid backup ID format.".into(),
        ));
    }
    ensure_ordinary_dir(directory)?;
    let manifest_path = directory.join("manifest.json");
    let archive_path = directory.join("archive.zip");
    check_regular_file(&manifest_path)?;
    check_regular_file(&archive_path)?;

    let manifest_bytes =
        read_bounded_controlled(&manifest_path, MAX_BACKUP_MANIFEST_BYTES, control)?;
    let manifest: ServerBackupManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| ServerStoreError::Corrupt)?;
    manifest.validate()?;
    if manifest.backup_id != backup_id
        || &manifest.instance_id != id
        || manifest.game_space_id != store.context().space_id()
        || manifest.game_id != store.context().game_id()
    {
        return Err(ServerStoreError::Corrupt);
    }

    let (archive_bytes, archive_blake3) = {
        let mut file = File::open(&archive_path).map_err(|_| ServerStoreError::Io)?;
        let meta = file.metadata().map_err(|_| ServerStoreError::Io)?;
        if !meta.is_file() || meta.len() > MAX_BACKUP_TOTAL_BYTES + 16 * 1024 * 1024 {
            return Err(ServerStoreError::Corrupt);
        }
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            control.checkpoint()?;
            let n = file.read(&mut buf).map_err(|_| ServerStoreError::Io)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            hasher.update(&buf[..n]);
        }
        (total, hasher.finalize().to_hex().to_string())
    };

    let archive_file = File::open(&archive_path).map_err(|_| ServerStoreError::Io)?;
    let mut archive = ZipArchive::new(archive_file).map_err(|_| ServerStoreError::Corrupt)?;
    let expected_zip_entries = manifest.entries.len() + 1;
    if archive.len() != expected_zip_entries {
        return Err(ServerStoreError::Corrupt);
    }

    let mut expected: BTreeMap<&str, &BackupArchiveEntry> = manifest
        .entries
        .iter()
        .map(|entry| (entry.archive_path.as_str(), entry))
        .collect();
    let mut saw_manifest = false;
    let mut verified_files = 0usize;
    let mut verified_uncompressed_bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];

    for index in 0..archive.len() {
        control.checkpoint()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|_| ServerStoreError::Corrupt)?;
        if entry.is_dir() || entry.is_symlink() {
            return Err(ServerStoreError::UnsafePath);
        }
        if let Some(mode) = entry.unix_mode()
            && (mode & 0o170_000) == 0o120_000
        {
            return Err(ServerStoreError::UnsafePath);
        }
        let name = entry
            .name()
            .map_err(|_| ServerStoreError::UnsafePath)?
            .into_owned();
        if !valid_archive_relative_path(&name)
            || entry.enclosed_name().is_none()
            || is_forbidden_secret_archive_path(&name)
        {
            return Err(ServerStoreError::UnsafePath);
        }
        if name == "manifest.json" {
            if saw_manifest || entry.size() > MAX_BACKUP_MANIFEST_BYTES as u64 {
                return Err(ServerStoreError::Corrupt);
            }
            let mut inner = Vec::new();
            let mut total = 0u64;
            loop {
                control.checkpoint()?;
                let n = entry.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                if total > MAX_BACKUP_MANIFEST_BYTES as u64 {
                    return Err(ServerStoreError::Corrupt);
                }
                inner.extend_from_slice(&buffer[..n]);
            }
            let inner_manifest: ServerBackupManifest =
                serde_json::from_slice(&inner).map_err(|_| ServerStoreError::Corrupt)?;
            if inner_manifest != manifest {
                return Err(ServerStoreError::Corrupt);
            }
            saw_manifest = true;
            continue;
        }
        let expected_entry = expected
            .remove(name.as_str())
            .ok_or(ServerStoreError::Corrupt)?;
        let mut hasher = blake3::Hasher::new();
        let mut streamed = 0u64;
        loop {
            control.checkpoint()?;
            let n = entry.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
            if n == 0 {
                break;
            }
            streamed = streamed
                .checked_add(n as u64)
                .ok_or(ServerStoreError::Corrupt)?;
            if streamed > MAX_BACKUP_SINGLE_FILE_BYTES || streamed > expected_entry.size_bytes {
                return Err(ServerStoreError::Corrupt);
            }
            hasher.update(&buffer[..n]);
        }
        verified_uncompressed_bytes = verified_uncompressed_bytes
            .checked_add(streamed)
            .ok_or(ServerStoreError::Corrupt)?;
        if verified_uncompressed_bytes > MAX_BACKUP_TOTAL_BYTES
            || streamed != expected_entry.size_bytes
            || hasher.finalize().to_hex().as_str() != expected_entry.blake3
        {
            return Err(ServerStoreError::Corrupt);
        }
        verified_files += 1;
    }

    if !saw_manifest || !expected.is_empty() || verified_files != manifest.total_files {
        return Err(ServerStoreError::Corrupt);
    }

    let report = ServerBackupVerificationReport {
        instance_id: id.clone(),
        backup_id: backup_id.to_owned(),
        valid: true,
        archive_bytes,
        archive_blake3,
        verified_files,
        verified_uncompressed_bytes,
        consistency: manifest.consistency.clone(),
        issues: Vec::new(),
        manifest: Some(manifest.clone()),
    };
    Ok((report, manifest))
}

impl ServerStore {
    pub(super) fn backups_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("backups")
    }

    pub fn plan_server_backup(
        &self,
        id: &ServerInstanceId,
        mode: BackupConsistencyMode,
        runtime_witness: Option<&InstanceRuntimeStatus>,
        control: &ContentStageControl,
    ) -> Result<ServerBackupPlan, ServerStoreError> {
        let result = build_backup_plan_internal(self, id, mode, runtime_witness, control)
            .map(|(plan, _)| plan);
        control.finish(&result);
        result
    }

    pub fn create_server_backup(
        &self,
        id: &ServerInstanceId,
        mode: BackupConsistencyMode,
        expected_plan_fingerprint: Option<&str>,
        apply_retention: bool,
        runtime_witness: Option<&InstanceRuntimeStatus>,
        control: &ContentStageControl,
    ) -> Result<ServerBackupOutcome, ServerStoreError> {
        let result = (|| {
            control.checkpoint()?;
            self.require_no_server_restore(id)?;
            let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
            let (plan, items) =
                build_backup_plan_internal(self, id, mode, runtime_witness, control)?;
            if let Some(reason) = plan.blockers.first() {
                return Err(ServerStoreError::Invalid(reason.clone()));
            }
            if let Some(expected) = expected_plan_fingerprint {
                if !valid_lower_hex(expected, 64) {
                    return Err(ServerStoreError::Invalid(
                        "Invalid backup plan fingerprint.".into(),
                    ));
                }
                if plan.fingerprint != expected {
                    return Err(ServerStoreError::StaleRevision);
                }
            }
            let listing = self.list_server_backups(id)?;
            if listing.backups.len() >= MAX_BACKUPS_PER_INSTANCE {
                return Err(ServerStoreError::Invalid(format!(
                    "Instance already stores {MAX_BACKUPS_PER_INSTANCE} backups; prune before creating another."
                )));
            }

            let created_unix_ms = unix_ms();
            let backup_id = new_backup_id(
                self.context().space_id(),
                id,
                created_unix_ms,
                &plan.fingerprint,
            );
            let backups_root = self.backups_root(id);
            check_directory_chain(&backups_root)?;
            fs::create_dir_all(&backups_root).map_err(|_| ServerStoreError::Io)?;
            ensure_ordinary_dir(&backups_root)?;

            let manifest = ServerBackupManifest {
                schema_version: BACKUP_SCHEMA_VERSION,
                backup_id: backup_id.clone(),
                game_space_id: plan.game_space_id,
                game_id: plan.game_id,
                instance_id: id.clone(),
                instance_name: plan.instance_name,
                created_unix_ms,
                plan_fingerprint: plan.fingerprint,
                consistency: plan.consistency,
                engine_set: plan.engine_set,
                configuration_set: plan.configuration_set,
                content_set: plan.content_set,
                provider_set: plan.provider_set,
                save_set: plan.save_set,
                total_files: items.len(),
                total_uncompressed_bytes: plan.planned_uncompressed_bytes,
                entries: plan.entries,
            };
            manifest.validate()?;
            let manifest_bytes =
                serde_json::to_vec_pretty(&manifest).map_err(|_| ServerStoreError::Corrupt)?;
            if manifest_bytes.len() > MAX_BACKUP_MANIFEST_BYTES {
                return Err(ServerStoreError::Invalid(
                    "Backup manifest exceeds size limit.".into(),
                ));
            }

            let staged = tempfile::Builder::new()
                .prefix(BACKUP_STAGE_PREFIX)
                .tempdir_in(&backups_root)
                .map_err(|_| ServerStoreError::Io)?;
            let staged_manifest_path = staged.path().join("manifest.json");
            let mut mf = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged_manifest_path)
                .map_err(|_| ServerStoreError::Io)?;
            mf.write_all(&manifest_bytes)
                .and_then(|()| mf.sync_all())
                .map_err(|_| ServerStoreError::Io)?;
            drop(mf);

            let staged_archive_path = staged.path().join("archive.zip");
            write_zip_archive(&staged_archive_path, &manifest_bytes, &items, control)?;

            let (verification, _) =
                verify_backup_directory(self, id, &backup_id, staged.path(), control)?;

            let final_dir = backups_root.join(&backup_id);
            control.checkpoint()?;
            if !publish_no_replace(staged.path(), &final_dir)? {
                return Err(ServerStoreError::Invalid(
                    "Backup directory collided with an existing snapshot.".into(),
                ));
            }
            let _ = staged.keep();

            let mut pruned_backups = Vec::new();
            let mut retention_receipt_id = None;
            if apply_retention {
                let refreshed = self.list_server_backups(id)?;
                let prune_plan =
                    plan_prune_from_listing(self, id, &refreshed, None, true, control)?;
                if prune_plan.blockers.is_empty() && !prune_plan.candidates.is_empty() {
                    let outcome = self.apply_backup_prune_internal(
                        id,
                        &prune_plan.fingerprint,
                        None,
                        true,
                        control,
                    )?;
                    retention_receipt_id = Some(outcome.receipt_id);
                    pruned_backups = outcome
                        .removed
                        .into_iter()
                        .map(|candidate| candidate.backup_id)
                        .collect();
                }
            }

            Ok(ServerBackupOutcome {
                instance_id: id.clone(),
                manifest,
                archive_bytes: verification.archive_bytes,
                archive_blake3: verification.archive_blake3,
                verified: verification.valid,
                pruned_backups,
                retention_receipt_id,
                writes_performed: true,
            })
        })();
        control.finish(&result);
        result
    }

    pub fn load_server_backup_outcome(
        &self,
        id: &ServerInstanceId,
        backup_id: &str,
        pruned_backups: Vec<String>,
        control: &ContentStageControl,
    ) -> Result<ServerBackupOutcome, ServerStoreError> {
        let dir = self.backups_root(id).join(backup_id);
        let (verification, manifest) = verify_backup_directory(self, id, backup_id, &dir, control)?;
        Ok(ServerBackupOutcome {
            instance_id: id.clone(),
            manifest,
            archive_bytes: verification.archive_bytes,
            archive_blake3: verification.archive_blake3,
            verified: verification.valid,
            pruned_backups,
            retention_receipt_id: None,
            writes_performed: true,
        })
    }

    pub fn verify_server_backup(
        &self,
        id: &ServerInstanceId,
        backup_id: &str,
        control: &ContentStageControl,
    ) -> Result<ServerBackupVerificationReport, ServerStoreError> {
        let result = (|| {
            if !self.capabilities().server_backups {
                return Err(ServerStoreError::UnsupportedGame);
            }
            self.validate_record_context()?;
            self.show(id)?;
            if !valid_backup_id(backup_id) {
                return Err(ServerStoreError::Invalid(
                    "Invalid backup ID format.".into(),
                ));
            }
            let dir = self.backups_root(id).join(backup_id);
            if !dir.try_exists().map_err(|_| ServerStoreError::Io)? {
                return Err(ServerStoreError::NotFound);
            }
            verify_backup_directory(self, id, backup_id, &dir, control).map(|(report, _)| report)
        })();
        control.finish(&result);
        result
    }

    pub fn list_server_backups(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ServerBackupListing, ServerStoreError> {
        if !self.capabilities().server_backups {
            return Err(ServerStoreError::UnsupportedGame);
        }
        self.validate_record_context()?;
        self.show(id)?;
        let retention_doc = load_retention_doc(self, id)?;
        let pinned_set: BTreeSet<&str> = retention_doc.pinned.iter().map(String::as_str).collect();
        let root = self.backups_root(id);
        check_directory_chain(&root)?;
        let mut backups = Vec::new();
        let mut corrupt_backups = Vec::new();
        let mut unrecognized_entries = Vec::new();
        let mut total_archive_bytes = 0u64;

        if root.try_exists().map_err(|_| ServerStoreError::Io)? {
            ensure_ordinary_dir(&root)?;
            let mut dir_entries = fs::read_dir(&root)
                .map_err(|_| ServerStoreError::Io)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ServerStoreError::Io)?;
            dir_entries.sort_by_key(|entry| entry.file_name());
            for entry in dir_entries {
                let name = match entry.file_name().into_string() {
                    Ok(name) => name,
                    Err(_) => {
                        unrecognized_entries.push("<non-UTF-8 entry>".into());
                        continue;
                    }
                };
                let metadata =
                    fs::symlink_metadata(entry.path()).map_err(|_| ServerStoreError::Io)?;
                let ordinary_dir = metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && !is_reparse_point(&metadata);
                if name == BACKUP_RETENTION_FILE
                    && metadata.is_file()
                    && !is_reparse_point(&metadata)
                {
                    continue;
                }
                if name == "prune-receipts" && ordinary_dir {
                    continue;
                }
                if valid_retention_receipt_name(&name) && metadata.is_file() {
                    continue;
                }
                if ordinary_dir
                    && (name.starts_with(BACKUP_STAGE_PREFIX) || valid_quarantine_name(&name))
                {
                    unrecognized_entries.push(name);
                    continue;
                }
                if !(ordinary_dir && valid_backup_id(&name)) {
                    unrecognized_entries.push(name);
                    continue;
                }
                let dir = entry.path();
                let manifest_path = dir.join("manifest.json");
                let archive_path = dir.join("archive.zip");
                let control = ContentStageControl::default();
                let summary = (|| -> Result<ServerBackupSummary, ServerStoreError> {
                    check_regular_file(&manifest_path)?;
                    check_regular_file(&archive_path)?;
                    let bytes = read_bounded_controlled(
                        &manifest_path,
                        MAX_BACKUP_MANIFEST_BYTES,
                        &control,
                    )?;
                    let manifest: ServerBackupManifest =
                        serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
                    manifest.validate()?;
                    if manifest.backup_id != name
                        || &manifest.instance_id != id
                        || manifest.game_space_id != self.context().space_id()
                    {
                        return Err(ServerStoreError::Corrupt);
                    }
                    let archive_meta = fs::symlink_metadata(&archive_path)
                        .map_err(|_| ServerStoreError::Corrupt)?;
                    if !archive_meta.is_file()
                        || archive_meta.file_type().is_symlink()
                        || is_reparse_point(&archive_meta)
                    {
                        return Err(ServerStoreError::UnsafePath);
                    }
                    Ok(ServerBackupSummary {
                        backup_id: manifest.backup_id,
                        created_unix_ms: manifest.created_unix_ms,
                        mode: manifest.consistency.mode,
                        stopped_verified: manifest.consistency.stopped_verified,
                        pinned: pinned_set.contains(name.as_str()),
                        instance_revision: manifest.configuration_set.instance_revision,
                        active_deployment_record_id: manifest
                            .content_set
                            .active_deployment_record_id,
                        total_files: manifest.total_files,
                        total_uncompressed_bytes: manifest.total_uncompressed_bytes,
                        archive_bytes: archive_meta.len(),
                        covered_providers: manifest.provider_set.covered_providers,
                        uncovered_providers: manifest.provider_set.uncovered_providers,
                    })
                })();
                match summary {
                    Ok(summary) => {
                        total_archive_bytes =
                            total_archive_bytes.saturating_add(summary.archive_bytes);
                        backups.push(summary);
                    }
                    Err(_) => corrupt_backups.push(name),
                }
            }
        }

        backups.sort_by(|left, right| {
            right
                .created_unix_ms
                .cmp(&left.created_unix_ms)
                .then_with(|| left.backup_id.cmp(&right.backup_id))
        });

        Ok(ServerBackupListing {
            instance_id: id.clone(),
            backups,
            retention: super::model::BackupRetentionState {
                instance_id: id.clone(),
                generation: retention_doc.generation,
                policy: retention_doc.policy,
                pinned: retention_doc.pinned,
            },
            total_archive_bytes,
            corrupt_backups,
            unrecognized_entries,
        })
    }
}
