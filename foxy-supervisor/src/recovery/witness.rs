use super::{ConsumerRecord, MAX_RECORD_BYTES, invalid, options, read_record, record_name};
use crate::leases::{InstallationLocks, directory_chain, ordinary};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, Instant},
};

pub const MAX_RECEIPTS: usize = 1024;
pub const RETAINED_COMPLETED_RECEIPTS: usize = 128;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize)]
pub struct ConsumerCleanupInspection {
    pub consumer: ConsumerRecord,
    pub consumer_fingerprint: String,
    pub registration_present: bool,
    pub receipt_fingerprint: Option<String>,
    pub receipt_state: &'static str,
    pub observation: Option<CleanupObservation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupObservation {
    pub backend: String,
    pub owner_exit_observed: bool,
    pub tree_exited: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupReceipt {
    pub schema_version: u32,
    pub consumer_fingerprint: String,
    pub consumer: ConsumerRecord,
    pub observation: Option<CleanupObservation>,
}

impl CleanupReceipt {
    pub fn validates(&self) -> bool {
        self.schema_version == 1
            && self.consumer.validates()
            && self.consumer.schema_version == 2
            && self.consumer.cleanup.is_some()
            && self.consumer.session.validate().is_ok()
            && self.consumer_fingerprint.len() == 64
            && serde_json::to_vec(&self.consumer).is_ok_and(|bytes| {
                blake3::hash(&bytes).to_hex().as_str() == self.consumer_fingerprint
            })
            && self.observation.as_ref().is_none_or(|observation| {
                observation.tree_exited
                    && matches!(
                        observation.backend.as_str(),
                        "windows_job" | "windows_job_waited_members" | "linux_pid_namespace"
                    )
            })
    }

    fn proves_cleanup(&self) -> bool {
        self.observation.as_ref().is_some_and(|observation| {
            matches!(
                observation.backend.as_str(),
                "windows_job_waited_members" | "linux_pid_namespace"
            )
        })
    }
}

fn hex_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn arm(
    installation: &InstallationLocks,
    consumer: &ConsumerRecord,
    consumer_bytes: &[u8],
) -> io::Result<File> {
    let directory = installation.namespace.join("cleanup");
    directory_chain(&directory, true)?;
    prune_history(installation)?;
    if fs::read_dir(&directory)?.take(MAX_RECEIPTS + 1).count() >= MAX_RECEIPTS {
        return Err(invalid());
    }
    let consumer_fingerprint = blake3::hash(consumer_bytes).to_hex().to_string();
    let pending = CleanupReceipt {
        schema_version: 1,
        consumer_fingerprint: consumer_fingerprint.clone(),
        consumer: consumer.clone(),
        observation: None,
    };
    if !pending.validates() {
        return Err(invalid());
    }
    let bytes = serde_json::to_vec(&pending).map_err(|_| invalid())?;
    let mut file = options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.join(format!("{consumer_fingerprint}.json")))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(file)
}

fn read_receipt(path: &Path, installation_key: &str) -> io::Result<(CleanupReceipt, Vec<u8>)> {
    let metadata = fs::symlink_metadata(path)?;
    if !ordinary(&metadata) || !metadata.is_file() {
        return Err(invalid());
    }
    let mut file = options().read(true).open(path)?;
    file.try_lock_shared().map_err(io::Error::from)?;
    let result = (|| {
        let metadata = file.metadata()?;
        if !ordinary(&metadata) || !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_RECORD_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let receipt: CleanupReceipt = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if !receipt.validates()
            || receipt.consumer.installation_key != installation_key
            || path.file_name()
                != Some(std::ffi::OsStr::new(&format!(
                    "{}.json",
                    receipt.consumer_fingerprint
                )))
        {
            return Err(invalid());
        }
        Ok((receipt, bytes))
    })();
    let unlocked = file.unlock();
    let result = result?;
    unlocked?;
    Ok(result)
}

pub(super) fn discard_unspawned_pending(
    consumer_path: &Path,
    consumer: &ConsumerRecord,
    consumer_bytes: &[u8],
) -> io::Result<()> {
    let namespace = consumer_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(invalid)?;
    let fingerprint = blake3::hash(consumer_bytes).to_hex().to_string();
    let path = namespace
        .join("cleanup")
        .join(format!("{fingerprint}.json"));
    let expected = CleanupReceipt {
        schema_version: 1,
        consumer_fingerprint: fingerprint,
        consumer: consumer.clone(),
        observation: None,
    };
    let expected = serde_json::to_vec(&expected).map_err(|_| invalid())?;
    let (_, bytes) = read_receipt(&path, &consumer.installation_key)?;
    if bytes != expected || read_record(consumer_path)? != consumer_bytes {
        return Err(invalid());
    }
    fs::remove_file(path)
}

fn prune_history(installation: &InstallationLocks) -> io::Result<()> {
    let directory = installation.namespace.join("cleanup");
    let key = installation.coordination_key()?;
    let mut completed = Vec::new();
    let mut count = 0;
    for entry in fs::read_dir(&directory)? {
        count += 1;
        if count > MAX_RECEIPTS {
            return Err(invalid());
        }
        let entry = entry?;
        let Ok((receipt, bytes)) = read_receipt(&entry.path(), &key) else {
            continue;
        };
        if receipt.proves_cleanup()
            && fs::symlink_metadata(
                installation
                    .namespace
                    .join("consumers")
                    .join(record_name(&receipt.consumer)?),
            )
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        {
            completed.push((entry.path(), bytes));
        }
    }
    completed.sort_by(|left, right| left.0.cmp(&right.0));
    let remove = completed.len().saturating_sub(RETAINED_COMPLETED_RECEIPTS);
    for (path, bytes) in completed.into_iter().take(remove) {
        if read_record(&path)? != bytes {
            return Err(invalid());
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

impl InstallationLocks {
    pub fn inspect_cleanup(&self) -> io::Result<Vec<ConsumerCleanupInspection>> {
        self.revalidate()?;
        let key = self.coordination_key()?;
        let records = self.inspect_consumers()?;
        let mut rows = BTreeMap::new();
        for consumer in records {
            let path = self
                .namespace
                .join("consumers")
                .join(record_name(&consumer)?);
            let bytes = read_record(&path)?;
            let fingerprint = blake3::hash(&bytes).to_hex().to_string();
            let mut row = ConsumerCleanupInspection {
                receipt_state: if consumer.cleanup.is_some() {
                    "pending_or_unavailable"
                } else {
                    "legacy_unverified"
                },
                consumer,
                consumer_fingerprint: fingerprint.clone(),
                registration_present: true,
                receipt_fingerprint: None,
                observation: None,
            };
            if row.consumer.cleanup.is_some()
                && let Ok((receipt, receipt_bytes)) = read_receipt(
                    &self
                        .namespace
                        .join("cleanup")
                        .join(format!("{fingerprint}.json")),
                    &key,
                )
                && receipt.consumer == row.consumer
                && receipt.consumer_fingerprint == fingerprint
            {
                row.receipt_fingerprint = Some(blake3::hash(&receipt_bytes).to_hex().to_string());
                row.receipt_state = if receipt.proves_cleanup() {
                    "verified_exited"
                } else if receipt.observation.is_some() {
                    "legacy_unverified"
                } else {
                    "pending"
                };
                row.observation = receipt.observation;
            }
            rows.insert(fingerprint, row);
        }
        let directory = self.namespace.join("cleanup");
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(rows.into_values().collect());
            }
            Err(error) => return Err(error),
            Ok(_) => directory_chain(&directory, false)?,
        }
        for (count, entry) in fs::read_dir(&directory)?.enumerate() {
            if count >= MAX_RECEIPTS {
                return Err(invalid());
            }
            let entry = entry?;
            let Some(stem) = entry
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                return Err(invalid());
            };
            if rows.contains_key(&stem) {
                continue;
            }
            let (receipt, bytes) = read_receipt(&entry.path(), &key)?;
            if !fs::symlink_metadata(
                self.namespace
                    .join("consumers")
                    .join(record_name(&receipt.consumer)?),
            )
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
            {
                return Err(invalid());
            }
            let verified = receipt.proves_cleanup();
            rows.insert(
                stem,
                ConsumerCleanupInspection {
                    consumer_fingerprint: receipt.consumer_fingerprint,
                    consumer: receipt.consumer,
                    registration_present: false,
                    receipt_fingerprint: Some(blake3::hash(&bytes).to_hex().to_string()),
                    receipt_state: if verified {
                        "verified_history"
                    } else {
                        "unverified_history"
                    },
                    observation: receipt.observation,
                },
            );
        }
        self.revalidate()?;
        Ok(rows.into_values().collect())
    }

    pub fn retire_verified_consumer(
        &self,
        consumer_fingerprint: &str,
        receipt_fingerprint: &str,
    ) -> io::Result<bool> {
        if !hex_fingerprint(consumer_fingerprint) || !hex_fingerprint(receipt_fingerprint) {
            return Err(invalid());
        }
        let _admission = self.admit_for_recovery()?;
        self.inspect_consumers()?;
        let path = self
            .namespace
            .join("consumers")
            .join(format!("{consumer_fingerprint}.json"));
        let (receipt, bytes) = read_receipt(
            &self
                .namespace
                .join("cleanup")
                .join(format!("{consumer_fingerprint}.json")),
            &self.coordination_key()?,
        )?;
        if receipt.consumer_fingerprint != consumer_fingerprint
            || !receipt.proves_cleanup()
            || blake3::hash(&bytes).to_hex().as_str() != receipt_fingerprint
        {
            return Err(invalid());
        }
        let current = match read_record(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if blake3::hash(&current).to_hex().as_str() != consumer_fingerprint
            || serde_json::from_slice::<ConsumerRecord>(&current).map_err(|_| invalid())?
                != receipt.consumer
        {
            return Err(invalid());
        }
        self.revalidate()?;
        if read_record(&path)? != current {
            return Err(invalid());
        }
        fs::remove_file(path)?;
        Ok(true)
    }
}

pub(crate) struct WitnessReceiptWriter {
    file: File,
    pending: CleanupReceipt,
    pending_bytes: Vec<u8>,
}

impl WitnessReceiptWriter {
    pub(crate) fn open(mut file: File) -> io::Result<Self> {
        file.try_lock_shared().map_err(io::Error::from)?;
        let result = (|| {
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            (&mut file)
                .take(MAX_RECORD_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_RECORD_BYTES {
                return Err(invalid());
            }
            let pending: CleanupReceipt = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if !pending.validates() || pending.observation.is_some() {
                return Err(invalid());
            }
            Ok((pending, bytes))
        })();
        let unlocked = file.unlock();
        let (pending, pending_bytes) = result?;
        unlocked?;
        Ok(Self {
            file,
            pending,
            pending_bytes,
        })
    }

    pub(crate) fn complete(&mut self, backend: &str, owner_exit_observed: bool) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(io::Error::from(error)),
            }
        }
        let result = (|| {
            self.file.seek(SeekFrom::Start(0))?;
            let mut current = Vec::new();
            (&mut self.file)
                .take(MAX_RECORD_BYTES + 1)
                .read_to_end(&mut current)?;
            if current != self.pending_bytes {
                return Err(invalid());
            }
            let mut completed = self.pending.clone();
            completed.observation = Some(CleanupObservation {
                backend: backend.into(),
                owner_exit_observed,
                tree_exited: true,
            });
            if !completed.validates() || !completed.proves_cleanup() {
                return Err(invalid());
            }
            let bytes = serde_json::to_vec(&completed).map_err(|_| invalid())?;
            if bytes.len() as u64 > MAX_RECORD_BYTES {
                return Err(invalid());
            }
            self.file.seek(SeekFrom::Start(0))?;
            self.file.write_all(&bytes)?;
            self.file.set_len(bytes.len() as u64)?;
            self.file.sync_all()
        })();
        let unlocked = self.file.unlock();
        result.and(unlocked)
    }
}
