use super::{ContentStageControl, InstallationId, ServerInstanceId, ServerStore, ServerStoreError};
use foxy_supervisor::{leases::InstallationLocks, recovery::witness::ConsumerCleanupInspection};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsumerRecoveryEntry {
    pub consumer_fingerprint: String,
    pub session: foxy_supervisor::session::SessionIdentity,
    pub registration_present: bool,
    pub receipt_state: String,
    pub eligible: bool,
    pub reason: String,
    #[serde(skip)]
    receipt_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsumerRecoveryPlan {
    pub instance_id: ServerInstanceId,
    pub installation_id: InstallationId,
    pub fingerprint: String,
    pub owner_authenticated: bool,
    pub inspection_complete: bool,
    pub eligible_count: usize,
    pub pending_count: usize,
    pub completed_history: usize,
    pub entries: Vec<ConsumerRecoveryEntry>,
    pub blockers: Vec<String>,
    pub writes_performed: bool,
    #[serde(skip)]
    owner_scope: Option<foxy_supervisor::control::Scope>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsumerRecoveryOutcome {
    pub instance_id: ServerInstanceId,
    pub installation_id: InstallationId,
    pub retired_count: usize,
    pub remaining_records: usize,
    pub writes_performed: bool,
    pub cancelled: bool,
    pub inspection_complete: bool,
    pub error: Option<String>,
}

#[derive(Serialize)]
struct FingerprintInput<'a> {
    catalog_fingerprint: &'a str,
    instance: &'a ServerInstanceId,
    installation: &'a InstallationId,
    space: &'a str,
    game: &'a str,
    owner: Option<&'a foxy_supervisor::control::Scope>,
    inspection_complete: bool,
    entries: &'a [ConsumerRecoveryEntry],
    raw: &'a [RawFingerprint],
}

#[derive(Serialize)]
struct RawFingerprint {
    consumer: String,
    receipt: Option<String>,
    registration_present: bool,
    state: String,
}

impl ServerStore {
    pub fn plan_consumer_recovery(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ConsumerRecoveryPlan, ServerStoreError> {
        if !self.capabilities().consumer_recovery {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let catalog = self.load()?;
        let instance = catalog
            .instances
            .iter()
            .find(|item| &item.id == id)
            .ok_or(ServerStoreError::NotFound)?;
        let installation_id = instance
            .installation_id
            .clone()
            .ok_or(ServerStoreError::InstallationNotFound)?;
        let installation = catalog
            .installations
            .iter()
            .find(|item| item.id == installation_id)
            .ok_or(ServerStoreError::InstallationNotFound)?;
        let owner = super::owner::attach(self.context())
            .ok()
            .flatten()
            .map(|attachment| attachment.scope().clone());
        let mut blockers = Vec::new();
        if owner.is_none() {
            blockers.push("Authenticated owner scope could not be verified".into());
        }
        let inspections = InstallationLocks::capture(&installation.directory)
            .and_then(|locks| locks.inspect_cleanup());
        let (rows, inspection_complete) = match inspections {
            Ok(rows) => (rows, true),
            Err(_) => {
                blockers.push(
                    "Consumer or receipt inspection is incomplete; no records can be retired"
                        .into(),
                );
                (Vec::new(), false)
            }
        };
        let (mut entries, raw) = classify_rows(
            &rows,
            id,
            self.context().space_id(),
            owner.as_ref(),
            inspection_complete,
        );
        if restrict_unverified_group(
            &rows,
            id,
            self.context().space_id(),
            owner.as_ref(),
            &mut entries,
        ) {
            blockers.push("The selected consumer group still contains unverified cleanup; all group records are preserved".into());
        }
        let eligible_count = entries.iter().filter(|entry| entry.eligible).count();
        let pending_count = entries
            .iter()
            .filter(|entry| entry.registration_present && entry.receipt_state != "verified_exited")
            .count();
        let completed_history = entries
            .iter()
            .filter(|entry| {
                !entry.registration_present && entry.receipt_state == "verified_history"
            })
            .count();
        let catalog_fingerprint =
            blake3::hash(&serde_json::to_vec(&catalog).map_err(|_| ServerStoreError::Corrupt)?)
                .to_hex()
                .to_string();
        let fingerprint = plan_fingerprint(FingerprintInput {
            catalog_fingerprint: &catalog_fingerprint,
            instance: id,
            installation: &installation_id,
            space: self.context().space_id(),
            game: self.context().game_id(),
            owner: owner.as_ref(),
            inspection_complete,
            entries: &entries,
            raw: &raw,
        })?;
        Ok(ConsumerRecoveryPlan {
            instance_id: id.clone(),
            installation_id,
            fingerprint,
            owner_authenticated: owner.is_some(),
            inspection_complete,
            eligible_count,
            pending_count,
            completed_history,
            entries,
            blockers,
            writes_performed: false,
            owner_scope: owner,
        })
    }

    pub fn recover_consumers(
        &self,
        id: &ServerInstanceId,
        expected_fingerprint: &str,
        control: &ContentStageControl,
    ) -> Result<ConsumerRecoveryOutcome, ServerStoreError> {
        let plan = self.plan_consumer_recovery(id)?;
        if plan.fingerprint != expected_fingerprint
            || !plan.owner_authenticated
            || !plan.inspection_complete
            || plan.eligible_count == 0
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let path = self.context().database_path();
        let scope = super::owner::scope_for_write(&path).map_err(ServerStoreError::Invalid)?;
        if plan.owner_scope.as_ref() != Some(&scope) {
            return Err(ServerStoreError::StaleRevision);
        }
        let mut retired_count = 0;
        let mut cancelled = false;
        let mut writes_performed = false;
        let mut remaining_records = plan
            .entries
            .iter()
            .filter(|entry| entry.registration_present)
            .count();
        let mut inspection_complete = true;
        let mut operation_error = None;
        let result = super::owner::with_write(&path, || {
            let current = self
                .plan_consumer_recovery(id)
                .map_err(|error| error.to_string())?;
            if current.fingerprint != expected_fingerprint
                || !current.inspection_complete
                || !current.owner_authenticated
                || current.owner_scope.as_ref() != Some(&scope)
            {
                return Err("Consumer recovery review changed".into());
            }
            let installation = self
                .load()
                .map_err(|error| error.to_string())?
                .installations
                .into_iter()
                .find(|item| item.id == current.installation_id)
                .ok_or_else(|| "Installation disappeared".to_string())?;
            let locks = InstallationLocks::capture(&installation.directory)
                .map_err(|error| error.to_string())?;
            for entry in current.entries.iter().filter(|entry| entry.eligible) {
                if control.snapshot().cancel_requested {
                    cancelled = true;
                    break;
                }
                let attempt = (|| {
                    let rows = locks.inspect_cleanup().map_err(|error| error.to_string())?;
                    let row = rows
                        .iter()
                        .find(|row| row.consumer_fingerprint == entry.consumer_fingerprint)
                        .ok_or_else(|| "Reviewed consumer disappeared".to_string())?;
                    if row.receipt_state != "verified_exited"
                        || !row.registration_present
                        || row.receipt_fingerprint != entry.receipt_fingerprint
                    {
                        return Err("Reviewed consumer receipt changed".into());
                    }
                    let receipt = entry
                        .receipt_fingerprint
                        .as_deref()
                        .ok_or_else(|| "Verified receipt fingerprint missing".to_string())?;
                    let removed = locks
                        .retire_verified_consumer(&entry.consumer_fingerprint, receipt)
                        .map_err(|error| error.to_string())?;
                    Ok::<bool, String>(removed)
                })();
                match attempt {
                    Ok(true) => {
                        retired_count += 1;
                        remaining_records = remaining_records.saturating_sub(1);
                        writes_performed = true;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        operation_error = Some(error);
                        break;
                    }
                }
            }
            Ok(())
        });
        result.map_err(ServerStoreError::Invalid)?;
        match self.plan_consumer_recovery(id) {
            Ok(after) if after.inspection_complete => {
                remaining_records = after
                    .entries
                    .iter()
                    .filter(|entry| entry.registration_present)
                    .count()
            }
            Ok(_) => {
                inspection_complete = false;
                operation_error.get_or_insert_with(|| "Post-recovery inspection is incomplete; the remaining count is last observed".into());
            }
            Err(error) => {
                inspection_complete = false;
                operation_error
                    .get_or_insert_with(|| format!("Post-recovery inspection failed: {error}"));
            }
        }
        Ok(ConsumerRecoveryOutcome {
            instance_id: id.clone(),
            installation_id: plan.installation_id,
            retired_count,
            remaining_records,
            writes_performed,
            cancelled,
            inspection_complete,
            error: operation_error,
        })
    }
}

fn plan_fingerprint(input: FingerprintInput<'_>) -> Result<String, ServerStoreError> {
    Ok(
        blake3::hash(&serde_json::to_vec(&input).map_err(|_| ServerStoreError::Corrupt)?)
            .to_hex()
            .to_string(),
    )
}

fn restrict_unverified_group(
    rows: &[ConsumerCleanupInspection],
    id: &ServerInstanceId,
    space: &str,
    owner: Option<&foxy_supervisor::control::Scope>,
    entries: &mut [ConsumerRecoveryEntry],
) -> bool {
    let pending = rows.iter().any(|row| {
        row.registration_present
            && row.receipt_state != "verified_exited"
            && row.consumer.session.game_space_id == space
            && owner.is_some_and(|scope| {
                row.consumer.cleanup.as_ref().is_some_and(|binding| {
                    binding.group_id == id.as_str() && binding.context_key == scope.context_key
                })
            })
    });
    if pending {
        for entry in entries.iter_mut().filter(|entry| entry.eligible) {
            entry.eligible = false;
            entry.reason = "selected group contains unverified cleanup".into();
        }
    }
    pending
}

fn classify_rows(
    rows: &[ConsumerCleanupInspection],
    id: &ServerInstanceId,
    space: &str,
    owner: Option<&foxy_supervisor::control::Scope>,
    complete: bool,
) -> (Vec<ConsumerRecoveryEntry>, Vec<RawFingerprint>) {
    let mut entries = Vec::with_capacity(rows.len());
    let mut raw = Vec::with_capacity(rows.len());
    for row in rows {
        let same_instance = row
            .consumer
            .cleanup
            .as_ref()
            .is_some_and(|binding| binding.group_id == id.as_str());
        let same_context = owner.is_some_and(|scope| {
            row.consumer
                .cleanup
                .as_ref()
                .is_some_and(|binding| binding.context_key == scope.context_key)
        });
        let same_space = row.consumer.session.game_space_id == space;
        let is_current_owner = owner.is_some_and(|scope| {
            row.consumer.session.owner_id == scope.owner_id
                && row.consumer.session.owner_generation == scope.generation
        });
        let eligible = complete
            && owner.is_some()
            && row.registration_present
            && row.receipt_state == "verified_exited"
            && same_instance
            && same_context
            && same_space
            && !is_current_owner;
        let reason = if !complete {
            "inspection incomplete"
        } else if owner.is_none() {
            "owner is not authenticated"
        } else if !same_instance {
            "foreign or legacy group"
        } else if !same_context {
            "foreign configuration root"
        } else if !same_space {
            "foreign game space"
        } else if is_current_owner {
            "current owner record"
        } else if !row.registration_present {
            "receipt history without a registration"
        } else if row.receipt_state != "verified_exited" {
            "cleanup receipt is not verified complete"
        } else {
            "verified old-owner cleanup receipt"
        }
        .to_string();
        entries.push(ConsumerRecoveryEntry {
            consumer_fingerprint: row.consumer_fingerprint.clone(),
            session: row.consumer.session.clone(),
            registration_present: row.registration_present,
            receipt_state: row.receipt_state.into(),
            eligible,
            reason,
            receipt_fingerprint: row.receipt_fingerprint.clone(),
        });
        raw.push(RawFingerprint {
            consumer: row.consumer_fingerprint.clone(),
            receipt: row.receipt_fingerprint.clone(),
            registration_present: row.registration_present,
            state: row.receipt_state.into(),
        });
    }
    (entries, raw)
}

#[cfg(test)]
mod tests;
