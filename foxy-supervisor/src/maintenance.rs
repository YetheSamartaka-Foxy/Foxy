use crate::leases::{Admission, InstallationLocks, MaintenanceLease, directory_chain};
use crate::session::SessionIdentity;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

const MAX_RECEIPTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceIntent {
    pub schema_version: u32,
    pub installation_key: String,
    pub context_fingerprint: String,
    pub transaction_fingerprint: String,
    pub transaction_evidence: String,
    pub previous_state_fingerprint: Option<String>,
    pub next_state_fingerprint: String,
    pub owner: SessionIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceSnapshot {
    pub intent: MaintenanceIntent,
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionOutcome {
    Aborted,
    Committed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceReceipt {
    pub schema_version: u32,
    pub intent: MaintenanceSnapshot,
    pub outcome: ResolutionOutcome,
    pub evidence_fingerprint: String,
    pub resolver: SessionIdentity,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MaintenanceInventory {
    pub pending: Option<MaintenanceSnapshot>,
    pub receipts: Vec<MaintenanceReceipt>,
}

pub struct MaintenanceRecoveryLease<'a> {
    admission: Admission<'a>,
    _content: File,
    expected: MaintenanceSnapshot,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Installation maintenance recovery metadata is invalid or unverified.",
    )
}

fn blocked() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "Installation maintenance requires explicit verified recovery.",
    )
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid())?;
    if bytes.len() > 16 * 1024 {
        return Err(invalid());
    }
    Ok(bytes)
}

fn fingerprint<T: Serialize>(value: &T) -> io::Result<String> {
    Ok(blake3::hash(&encode(value)?).to_hex().to_string())
}

fn read<T: serde::de::DeserializeOwned + Serialize>(path: &Path) -> io::Result<T> {
    let bytes = crate::recovery::read_record(path)?;
    let value: T = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if encode(&value)? != bytes {
        return Err(invalid());
    }
    Ok(value)
}

fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    directory_chain(parent, true)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

fn validate_snapshot(locks: &InstallationLocks, snapshot: &MaintenanceSnapshot) -> io::Result<()> {
    let intent = &snapshot.intent;
    intent.owner.validate()?;
    if intent.schema_version != 1
        || intent.installation_key != locks.coordination_key()?
        || !hash(&intent.context_fingerprint)
        || !hash(&intent.transaction_fingerprint)
        || blake3::hash(intent.transaction_evidence.as_bytes())
            .to_hex()
            .as_str()
            != intent.transaction_fingerprint
        || !hash(&intent.next_state_fingerprint)
        || intent
            .previous_state_fingerprint
            .as_deref()
            .is_some_and(|value| !hash(value))
        || snapshot.fingerprint != fingerprint(intent)?
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate_receipt(locks: &InstallationLocks, receipt: &MaintenanceReceipt) -> io::Result<()> {
    validate_snapshot(locks, &receipt.intent)?;
    receipt.resolver.validate()?;
    if receipt.schema_version != 1
        || !hash(&receipt.evidence_fingerprint)
        || receipt.resolver.game_space_id != receipt.intent.intent.owner.game_space_id
        || receipt.resolver.instance_id != receipt.intent.intent.owner.instance_id
    {
        return Err(invalid());
    }
    Ok(())
}

// Held admission keeps validated history stable while commit checks read only owned records.
fn verify_pending(locks: &InstallationLocks, expected: &MaintenanceSnapshot) -> io::Result<()> {
    validate_snapshot(locks, expected)?;
    let root = locks.namespace.join("maintenance");
    directory_chain(&root, false)?;
    for (index, entry) in fs::read_dir(&root)?.enumerate() {
        let entry = entry?;
        if index >= 2 || (entry.file_name() != "intent.json" && entry.file_name() != "receipts") {
            return Err(invalid());
        }
        if entry.file_name() == "receipts" {
            directory_chain(&entry.path(), false)?;
        }
    }
    let intent: MaintenanceIntent = read(&root.join("intent.json"))?;
    if intent != expected.intent {
        return Err(blocked());
    }
    let consumers = locks.namespace.join("consumers");
    match directory_chain(&consumers, false) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(()) => {}
    }
    if let Some(entry) = fs::read_dir(consumers)?.next() {
        entry?;
        return Err(blocked());
    }
    Ok(())
}

impl InstallationLocks {
    pub fn inspect_maintenance(&self) -> io::Result<MaintenanceInventory> {
        self.revalidate()?;
        let root = self.namespace.join("maintenance");
        match directory_chain(&root, false) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(MaintenanceInventory::default());
            }
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        let mut result = MaintenanceInventory::default();
        for (index, entry) in fs::read_dir(&root)?.enumerate() {
            if index >= 2 {
                return Err(invalid());
            }
            let entry = entry?;
            if entry.file_name() == "intent.json" {
                let intent: MaintenanceIntent = read(&entry.path())?;
                let snapshot = MaintenanceSnapshot {
                    fingerprint: fingerprint(&intent)?,
                    intent,
                };
                validate_snapshot(self, &snapshot)?;
                result.pending = Some(snapshot);
            } else if entry.file_name() == "receipts" {
                directory_chain(&entry.path(), false)?;
                for (count, receipt) in fs::read_dir(entry.path())?.enumerate() {
                    if count >= MAX_RECEIPTS {
                        return Err(invalid());
                    }
                    let receipt = receipt?;
                    let value: MaintenanceReceipt = read(&receipt.path())?;
                    validate_receipt(self, &value)?;
                    if receipt.file_name()
                        != std::ffi::OsStr::new(&format!("{}.json", value.intent.fingerprint))
                    {
                        return Err(invalid());
                    }
                    result.receipts.push(value);
                }
            } else {
                return Err(invalid());
            }
        }
        result
            .receipts
            .sort_by(|a, b| a.intent.fingerprint.cmp(&b.intent.fingerprint));
        self.revalidate()?;
        Ok(result)
    }

    pub(crate) fn require_no_maintenance_intent(&self) -> io::Result<()> {
        if self.inspect_maintenance()?.pending.is_some() {
            return Err(blocked());
        }
        Ok(())
    }

    pub fn try_recover_maintenance(
        &self,
        expected: &MaintenanceSnapshot,
    ) -> io::Result<MaintenanceRecoveryLease<'_>> {
        validate_snapshot(self, expected)?;
        let admission = self.admit_for_recovery()?;
        if self.inspect_maintenance()?.pending.as_ref() != Some(expected) {
            return Err(blocked());
        }
        let content = admission.exclusive_content()?;
        Ok(MaintenanceRecoveryLease {
            admission,
            _content: content,
            expected: expected.clone(),
        })
    }
}

impl MaintenanceLease<'_> {
    pub fn begin_intent(&self, intent: MaintenanceIntent) -> io::Result<MaintenanceSnapshot> {
        let locks = self.installation();
        let snapshot = MaintenanceSnapshot {
            fingerprint: fingerprint(&intent)?,
            intent,
        };
        validate_snapshot(locks, &snapshot)?;
        let inventory = locks.inspect_maintenance()?;
        if inventory.pending.is_some()
            || inventory.receipts.len() >= MAX_RECEIPTS
            || inventory
                .receipts
                .iter()
                .any(|receipt| receipt.intent.fingerprint == snapshot.fingerprint)
        {
            return Err(blocked());
        }
        if !locks.inspect_consumers()?.is_empty() {
            return Err(blocked());
        }
        publish(
            &locks.namespace.join("maintenance/intent.json"),
            &encode(&snapshot.intent)?,
        )?;
        self.verify_intent(&snapshot)?;
        Ok(snapshot)
    }

    pub fn verify_intent(&self, expected: &MaintenanceSnapshot) -> io::Result<()> {
        verify_pending(self.installation(), expected)
    }
}

impl MaintenanceRecoveryLease<'_> {
    pub fn verify_intent(&self, expected: &MaintenanceSnapshot) -> io::Result<()> {
        let locks = self.admission.installation;
        if expected != &self.expected {
            return Err(blocked());
        }
        verify_pending(locks, expected)
    }

    pub fn publish_receipt(&self, receipt: &MaintenanceReceipt) -> io::Result<bool> {
        let locks = self.admission.installation;
        self.verify_intent(&receipt.intent)?;
        validate_receipt(locks, receipt)?;
        let inventory = locks.inspect_maintenance()?;
        if let Some(existing) = inventory
            .receipts
            .iter()
            .find(|value| value.intent == self.expected)
        {
            if existing != receipt {
                return Err(invalid());
            }
            return Ok(false);
        }
        if inventory.receipts.len() >= MAX_RECEIPTS {
            return Err(invalid());
        }
        publish(
            &locks
                .namespace
                .join("maintenance/receipts")
                .join(format!("{}.json", receipt.intent.fingerprint)),
            &encode(receipt)?,
        )?;
        Ok(true)
    }

    pub fn verify_receipt(&self, receipt: &MaintenanceReceipt) -> io::Result<()> {
        self.verify_intent(&receipt.intent)?;
        let locks = self.admission.installation;
        validate_receipt(locks, receipt)?;
        let directory = locks.namespace.join("maintenance/receipts");
        directory_chain(&directory, false)?;
        let stored: MaintenanceReceipt =
            read(&directory.join(format!("{}.json", receipt.intent.fingerprint)))?;
        if &stored != receipt {
            return Err(blocked());
        }
        Ok(())
    }

    pub fn retire_intent(&self, receipt: &MaintenanceReceipt) -> io::Result<()> {
        self.verify_receipt(receipt)?;
        let locks = self.admission.installation;
        fs::remove_file(locks.namespace.join("maintenance/intent.json"))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn maintenance_examples_preserve_strict_canonical_shapes() {
        let intent: serde_json::Value = serde_json::from_str(include_str!(
            "../../examples/json/server_backend/server_maintenance_intent.json"
        ))
        .unwrap();
        let receipt: serde_json::Value = serde_json::from_str(include_str!(
            "../../examples/json/server_backend/server_maintenance_receipt.json"
        ))
        .unwrap();
        let parsed: MaintenanceIntent = serde_json::from_value(intent.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), intent);
        let parsed: MaintenanceReceipt = serde_json::from_value(receipt.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), receipt);
        let mut invalid = intent;
        invalid["unrecognized"] = true.into();
        assert!(serde_json::from_value::<MaintenanceIntent>(invalid).is_err());
        let mut invalid = receipt;
        invalid["intent"]["unrecognized"] = true.into();
        assert!(serde_json::from_value::<MaintenanceReceipt>(invalid).is_err());
    }
}
