use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::archive::verify_backup_directory;
use super::model::{
    BACKUP_PRUNE_RECEIPT_SCHEMA_VERSION, BACKUP_RETENTION_SCHEMA_VERSION, BackupConsistencyMode,
    BackupPruneCandidate, BackupPruneOutcome, BackupPrunePlan, BackupRetainedEntry,
    BackupRetentionPolicy, BackupRetentionState, MAX_BACKUP_KEEP_LAST, MAX_BACKUP_RETENTION_BYTES,
    MAX_BACKUPS_PER_INSTANCE, ServerBackupListing, valid_backup_id, valid_lower_hex,
    valid_prune_receipt_id,
};
use crate::core::server::content_staging::{ContentStageControl, read_bounded_controlled};
use crate::core::server::store::{check_directory_chain, check_regular_file};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};

pub(super) const BACKUP_RETENTION_FILE: &str = "retention.json";
pub(super) const BACKUP_PRUNE_PREFIX: &str = ".backup-prune-";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RetentionDocument {
    pub schema_version: u32,
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub policy: BackupRetentionPolicy,
    pub pinned: Vec<String>,
}

#[derive(Serialize)]
struct PruneFingerprintInput<'a> {
    game_space_id: &'a str,
    instance_id: &'a ServerInstanceId,
    keep_last: u16,
    max_total_bytes: Option<u64>,
    pinned: &'a [String],
    retained: &'a [BackupRetainedEntry],
    candidates: &'a [BackupPruneCandidate],
    corrupt_backups: &'a [String],
    unrecognized_entries: &'a [String],
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

pub(super) fn valid_quarantine_name(value: &str) -> bool {
    value
        .strip_prefix(BACKUP_PRUNE_PREFIX)
        .is_some_and(|suffix| valid_lower_hex(suffix, 64))
}

pub(super) fn valid_retention_receipt_name(value: &str) -> bool {
    value
        .strip_prefix("prune_")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|suffix| valid_lower_hex(suffix, 64))
}

pub(super) fn load_retention_doc(
    store: &ServerStore,
    id: &ServerInstanceId,
) -> Result<RetentionDocument, ServerStoreError> {
    let path = store.backups_root(id).join(BACKUP_RETENTION_FILE);
    check_directory_chain(&store.backups_root(id))?;
    if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
        return Ok(RetentionDocument {
            schema_version: BACKUP_RETENTION_SCHEMA_VERSION,
            game_space_id: store.context().space_id().into(),
            instance_id: id.clone(),
            generation: 0,
            policy: BackupRetentionPolicy::default(),
            pinned: Vec::new(),
        });
    }
    check_regular_file(&path)?;
    let control = ContentStageControl::default();
    let bytes = read_bounded_controlled(&path, MAX_BACKUP_RETENTION_BYTES, &control)?;
    let doc: RetentionDocument =
        serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if doc.schema_version != BACKUP_RETENTION_SCHEMA_VERSION
        || doc.game_space_id != store.context().space_id()
        || &doc.instance_id != id
        || doc.generation == 0
        || doc.policy.validate().is_err()
        || doc.pinned.len() > MAX_BACKUPS_PER_INSTANCE
        || doc.pinned.iter().any(|entry| !valid_backup_id(entry))
        || doc.pinned.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(doc)
}

fn save_retention_doc(
    store: &ServerStore,
    id: &ServerInstanceId,
    doc: &RetentionDocument,
) -> Result<(), ServerStoreError> {
    let root = store.backups_root(id);
    check_directory_chain(&root)?;
    fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
    let bytes = serde_json::to_vec_pretty(doc).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_BACKUP_RETENTION_BYTES {
        return Err(ServerStoreError::Invalid(
            "Retention policy document exceeds size limit.".into(),
        ));
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".retention-")
        .tempfile_in(&root)
        .map_err(|_| ServerStoreError::Io)?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    temporary
        .persist(root.join(BACKUP_RETENTION_FILE))
        .map_err(|_| ServerStoreError::Io)?;
    Ok(())
}

pub(super) fn plan_prune_from_listing(
    store: &ServerStore,
    id: &ServerInstanceId,
    listing: &ServerBackupListing,
    keep_last_override: Option<u16>,
    auto_retention: bool,
    control: &ContentStageControl,
) -> Result<BackupPrunePlan, ServerStoreError> {
    let keep_last = keep_last_override.unwrap_or(listing.retention.policy.keep_last);
    if !(1..=MAX_BACKUP_KEEP_LAST).contains(&keep_last) {
        return Err(ServerStoreError::Invalid(format!(
            "Backup retention keep-last count must be 1-{MAX_BACKUP_KEEP_LAST}."
        )));
    }
    let max_total_bytes = listing.retention.policy.max_total_bytes;
    let pinned_set: BTreeSet<&str> = listing
        .retention
        .pinned
        .iter()
        .map(String::as_str)
        .collect();

    struct VerifiedSnapshot {
        backup_id: String,
        created_unix_ms: u64,
        mode: BackupConsistencyMode,
        stopped_verified: bool,
        archive_bytes: u64,
        manifest_fingerprint: String,
        pinned: bool,
    }

    let mut verified = Vec::new();
    let mut corrupt_backups: BTreeSet<String> = listing.corrupt_backups.iter().cloned().collect();
    let backups_root = store.backups_root(id);

    for summary in &listing.backups {
        control.checkpoint()?;
        let dir = backups_root.join(&summary.backup_id);
        match verify_backup_directory(store, id, &summary.backup_id, &dir, control) {
            Ok((report, manifest)) => {
                verified.push(VerifiedSnapshot {
                    backup_id: manifest.backup_id,
                    created_unix_ms: manifest.created_unix_ms,
                    mode: manifest.consistency.mode,
                    stopped_verified: manifest.consistency.stopped_verified,
                    archive_bytes: report.archive_bytes,
                    manifest_fingerprint: report.archive_blake3,
                    pinned: pinned_set.contains(summary.backup_id.as_str()),
                });
            }
            Err(ServerStoreError::Cancelled) => return Err(ServerStoreError::Cancelled),
            Err(_) => {
                corrupt_backups.insert(summary.backup_id.clone());
            }
        }
    }

    let newest_stopped_index = verified.iter().position(|item| {
        item.stopped_verified && item.mode == BackupConsistencyMode::StoppedVerified
    });

    let mut retained_reasons: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut demote_order = Vec::new();

    for (index, item) in verified.iter().enumerate() {
        let mut reasons = Vec::new();
        if item.pinned {
            reasons.push("pinned".into());
        }
        if Some(index) == newest_stopped_index {
            reasons.push("latest_stopped_consistent".into());
        }
        if index < usize::from(keep_last) {
            reasons.push("keep_last".into());
        }
        if !reasons.is_empty() {
            let protected = item.pinned || Some(index) == newest_stopped_index || index == 0;
            retained_reasons.insert(index, reasons);
            if !protected {
                demote_order.push(index);
            }
        }
    }

    if let Some(budget) = max_total_bytes {
        let mut retained_bytes: u64 = retained_reasons
            .keys()
            .map(|index| verified[*index].archive_bytes)
            .sum();
        for index in demote_order.into_iter().rev() {
            if retained_bytes <= budget {
                break;
            }
            retained_bytes = retained_bytes.saturating_sub(verified[index].archive_bytes);
            retained_reasons.remove(&index);
        }
    }

    let mut retained = Vec::new();
    let mut candidates = Vec::new();
    let mut reclaimable_bytes = 0u64;

    for (index, item) in verified.into_iter().enumerate() {
        if let Some(reasons) = retained_reasons.remove(&index) {
            retained.push(BackupRetainedEntry {
                backup_id: item.backup_id,
                created_unix_ms: item.created_unix_ms,
                mode: item.mode,
                archive_bytes: item.archive_bytes,
                reasons,
            });
        } else {
            reclaimable_bytes = reclaimable_bytes.saturating_add(item.archive_bytes);
            let reason = if index < usize::from(keep_last) {
                "exceeds_max_total_bytes".into()
            } else {
                "exceeds_keep_last".into()
            };
            candidates.push(BackupPruneCandidate {
                backup_id: item.backup_id,
                created_unix_ms: item.created_unix_ms,
                mode: item.mode,
                archive_bytes: item.archive_bytes,
                manifest_fingerprint: item.manifest_fingerprint,
                reason,
            });
        }
    }

    let mut blockers = Vec::new();
    if auto_retention && retained.is_empty() {
        blockers.push("Retention stopped because no verified backup would remain.".into());
    }
    if !corrupt_backups.is_empty() && auto_retention {
        // Auto-retention still leaves corrupt directories untouched without failing backup creation.
    }

    let corrupt_list: Vec<String> = corrupt_backups.into_iter().collect();
    let fp_bytes = serde_json::to_vec(&PruneFingerprintInput {
        game_space_id: store.context().space_id(),
        instance_id: id,
        keep_last,
        max_total_bytes,
        pinned: &listing.retention.pinned,
        retained: &retained,
        candidates: &candidates,
        corrupt_backups: &corrupt_list,
        unrecognized_entries: &listing.unrecognized_entries,
    })
    .map_err(|_| ServerStoreError::Corrupt)?;
    let fingerprint = blake3::hash(&fp_bytes).to_hex().to_string();

    Ok(BackupPrunePlan {
        instance_id: id.clone(),
        keep_last,
        max_total_bytes,
        pinned: listing.retention.pinned.clone(),
        retained,
        candidates,
        reclaimable_bytes,
        corrupt_backups: corrupt_list,
        unrecognized_entries: listing.unrecognized_entries.clone(),
        blockers,
        fingerprint,
        writes_performed: false,
    })
}

fn remove_backup_directory(
    path: &Path,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    check_directory_chain(path)?;
    let mut dirs_to_remove = vec![path.to_path_buf()];
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        control.checkpoint()?;
        for entry in fs::read_dir(&dir).map_err(|_| ServerStoreError::Io)? {
            control.checkpoint()?;
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let entry_path = entry.path();
            let meta = fs::symlink_metadata(&entry_path).map_err(|_| ServerStoreError::Io)?;
            if meta.file_type().is_symlink() {
                return Err(ServerStoreError::UnsafePath);
            }
            if meta.is_dir() {
                dirs_to_remove.push(entry_path.clone());
                stack.push(entry_path);
            } else if meta.is_file() {
                fs::remove_file(&entry_path).map_err(|_| ServerStoreError::Io)?;
            } else {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    for dir in dirs_to_remove.into_iter().rev() {
        control.checkpoint()?;
        fs::remove_dir(&dir).map_err(|_| ServerStoreError::Io)?;
    }
    Ok(())
}

impl ServerStore {
    pub fn backup_retention(
        &self,
        id: &ServerInstanceId,
    ) -> Result<BackupRetentionState, ServerStoreError> {
        if !self.capabilities().server_backups {
            return Err(ServerStoreError::UnsupportedGame);
        }
        self.validate_record_context()?;
        self.show(id)?;
        let doc = load_retention_doc(self, id)?;
        Ok(BackupRetentionState {
            instance_id: id.clone(),
            generation: doc.generation,
            policy: doc.policy,
            pinned: doc.pinned,
        })
    }

    pub fn set_backup_pin(
        &self,
        id: &ServerInstanceId,
        backup_id: &str,
        pinned: bool,
        expected_generation: u64,
    ) -> Result<BackupRetentionState, ServerStoreError> {
        if !self.capabilities().server_backups {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        self.validate_record_context()?;
        self.show(id)?;
        if !valid_backup_id(backup_id) {
            return Err(ServerStoreError::Invalid(
                "Invalid backup ID format.".into(),
            ));
        }
        let control = ContentStageControl::default();
        if pinned {
            self.verify_server_backup(id, backup_id, &control)?;
        }
        let mut doc = load_retention_doc(self, id)?;
        if doc.generation != expected_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        let mut pins: BTreeSet<String> = doc.pinned.into_iter().collect();
        if pinned {
            pins.insert(backup_id.to_owned());
        } else {
            pins.remove(backup_id);
        }
        doc.pinned = pins.into_iter().collect();
        doc.generation = doc
            .generation
            .checked_add(1)
            .ok_or(ServerStoreError::Corrupt)?;
        save_retention_doc(self, id, &doc)?;
        Ok(BackupRetentionState {
            instance_id: id.clone(),
            generation: doc.generation,
            policy: doc.policy,
            pinned: doc.pinned,
        })
    }

    pub fn set_backup_retention_policy(
        &self,
        id: &ServerInstanceId,
        policy: BackupRetentionPolicy,
        expected_generation: u64,
    ) -> Result<BackupRetentionState, ServerStoreError> {
        if !self.capabilities().server_backups {
            return Err(ServerStoreError::UnsupportedGame);
        }
        policy.validate().map_err(ServerStoreError::Invalid)?;
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        self.validate_record_context()?;
        self.show(id)?;
        let mut doc = load_retention_doc(self, id)?;
        if doc.generation != expected_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        doc.policy = policy;
        doc.generation = doc
            .generation
            .checked_add(1)
            .ok_or(ServerStoreError::Corrupt)?;
        save_retention_doc(self, id, &doc)?;
        Ok(BackupRetentionState {
            instance_id: id.clone(),
            generation: doc.generation,
            policy: doc.policy,
            pinned: doc.pinned,
        })
    }

    pub fn plan_backup_prune(
        &self,
        id: &ServerInstanceId,
        keep_last_override: Option<u16>,
        control: &ContentStageControl,
    ) -> Result<BackupPrunePlan, ServerStoreError> {
        let result = (|| {
            if !self.capabilities().server_backups {
                return Err(ServerStoreError::UnsupportedGame);
            }
            let listing = self.list_server_backups(id)?;
            plan_prune_from_listing(self, id, &listing, keep_last_override, false, control)
        })();
        control.finish(&result);
        result
    }

    pub fn apply_backup_prune(
        &self,
        id: &ServerInstanceId,
        expected_plan_fingerprint: &str,
        keep_last_override: Option<u16>,
        control: &ContentStageControl,
    ) -> Result<BackupPruneOutcome, ServerStoreError> {
        let result = self.apply_backup_prune_internal(
            id,
            expected_plan_fingerprint,
            keep_last_override,
            false,
            control,
        );
        control.finish(&result);
        result
    }

    pub(super) fn apply_backup_prune_internal(
        &self,
        id: &ServerInstanceId,
        expected_plan_fingerprint: &str,
        keep_last_override: Option<u16>,
        auto_retention: bool,
        control: &ContentStageControl,
    ) -> Result<BackupPruneOutcome, ServerStoreError> {
        control.checkpoint()?;
        if !self.capabilities().server_backups {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if !valid_lower_hex(expected_plan_fingerprint, 64) {
            return Err(ServerStoreError::Invalid(
                "Invalid backup prune plan fingerprint.".into(),
            ));
        }
        let listing = self.list_server_backups(id)?;
        let plan = plan_prune_from_listing(
            self,
            id,
            &listing,
            keep_last_override,
            auto_retention,
            control,
        )?;
        if plan.fingerprint != expected_plan_fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        if let Some(blocker) = plan.blockers.first() {
            return Err(ServerStoreError::Invalid(blocker.clone()));
        }

        let created_unix_ms = unix_ms();
        let mut receipt_hasher = blake3::Hasher::new();
        receipt_hasher.update(b"foxy-backup-prune-receipt-v1\0");
        receipt_hasher.update(self.context().space_id().as_bytes());
        receipt_hasher.update(&[0]);
        receipt_hasher.update(id.as_str().as_bytes());
        receipt_hasher.update(&created_unix_ms.to_le_bytes());
        let nonce: [u8; 16] = rand::random();
        receipt_hasher.update(&nonce);
        receipt_hasher.update(plan.fingerprint.as_bytes());
        let receipt_id = format!("prune_{}", receipt_hasher.finalize().to_hex());

        let outcome = BackupPruneOutcome {
            schema_version: BACKUP_PRUNE_RECEIPT_SCHEMA_VERSION,
            receipt_id: receipt_id.clone(),
            game_space_id: self.context().space_id().into(),
            instance_id: id.clone(),
            created_unix_ms,
            plan_fingerprint: plan.fingerprint,
            removed: plan.candidates.clone(),
            reclaimed_bytes: plan.reclaimable_bytes,
            retained_backups: plan
                .retained
                .into_iter()
                .map(|item| item.backup_id)
                .collect(),
            writes_performed: !plan.candidates.is_empty(),
        };

        let backups_root = self.backups_root(id);
        check_directory_chain(&backups_root)?;
        let receipts_dir = backups_root.join("prune-receipts");
        check_directory_chain(&receipts_dir)?;
        fs::create_dir_all(&receipts_dir).map_err(|_| ServerStoreError::Io)?;
        let receipt_bytes =
            serde_json::to_vec_pretty(&outcome).map_err(|_| ServerStoreError::Corrupt)?;
        let mut temp = tempfile::Builder::new()
            .prefix(".prune-receipt-")
            .tempfile_in(&receipts_dir)
            .map_err(|_| ServerStoreError::Io)?;
        temp.write_all(&receipt_bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        temp.persist_noclobber(receipts_dir.join(format!("{receipt_id}.json")))
            .map_err(|_| ServerStoreError::Io)?;

        for candidate in &plan.candidates {
            control.checkpoint()?;
            let source = backups_root.join(&candidate.backup_id);
            if !source.try_exists().map_err(|_| ServerStoreError::Io)? {
                continue;
            }
            let quarantine_suffix =
                blake3::hash(format!("{receipt_id}:{}", candidate.backup_id).as_bytes())
                    .to_hex()
                    .to_string();
            let quarantine = backups_root.join(format!("{BACKUP_PRUNE_PREFIX}{quarantine_suffix}"));
            fs::rename(&source, &quarantine).map_err(|_| ServerStoreError::Io)?;
            remove_backup_directory(&quarantine, control)?;
        }

        Ok(outcome)
    }

    pub fn load_prune_receipt(
        &self,
        id: &ServerInstanceId,
        receipt_id: &str,
        control: &ContentStageControl,
    ) -> Result<BackupPruneOutcome, ServerStoreError> {
        if !valid_prune_receipt_id(receipt_id) {
            return Err(ServerStoreError::Invalid(
                "Invalid prune receipt ID.".into(),
            ));
        }
        let receipts_dir = self.backups_root(id).join("prune-receipts");
        let path = receipts_dir.join(format!("{receipt_id}.json"));
        check_directory_chain(&receipts_dir)?;
        check_regular_file(&path)?;
        let bytes = read_bounded_controlled(&path, MAX_BACKUP_RETENTION_BYTES, control)?;
        let outcome: BackupPruneOutcome =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if outcome.schema_version != BACKUP_PRUNE_RECEIPT_SCHEMA_VERSION
            || outcome.receipt_id != receipt_id
            || &outcome.instance_id != id
            || outcome.game_space_id != self.context().space_id()
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(outcome)
    }
}
