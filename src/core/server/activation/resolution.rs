use std::fs;
use std::io::Write;

use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::maintenance::{MaintenanceReceipt, MaintenanceSnapshot, ResolutionOutcome};
use foxy_supervisor::session::SessionIdentity;

use super::manifest::{encode, fingerprint, validate_owner};
use super::{
    ActivationError, ActivationJournal, ActivationRecoveryAdmission, ActivationResolutionPlan,
    ActivationResolutionRecord, ActivationResolutionReport, ResolutionBoundary, maintenance_intent,
};
use crate::core::server::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};

fn record_name(intent: &MaintenanceSnapshot) -> String {
    format!("resolutions/resolution_{}.json", intent.fingerprint)
}

fn receipt(record: &ActivationResolutionRecord) -> Result<MaintenanceReceipt, ActivationError> {
    Ok(MaintenanceReceipt {
        schema_version: 1,
        intent: record.intent.clone(),
        outcome: record.outcome,
        evidence_fingerprint: fingerprint(record)?,
        resolver: record.resolver.clone(),
    })
}

impl ServerStore {
    pub(crate) fn plan_activation_resolution(
        &self,
        id: &ServerInstanceId,
        installation: &InstallationLocks,
        resolver: SessionIdentity,
        control: &ContentStageControl,
    ) -> Result<ActivationResolutionPlan, ActivationError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        self.show(id)?;
        validate_owner(&resolver, self.context().space_id(), id)?;
        let catalog = self
            .read()?
            .1
            .ok_or(ServerStoreError::Corrupt)?
            .to_hex()
            .to_string();
        let selection = self.read_record_state(id, control)?.1;
        let inventory = installation
            .inspect_maintenance()
            .map_err(|_| ActivationError::RecoveryRequired)?;
        let intent = inventory
            .pending
            .as_ref()
            .ok_or(ActivationError::RecoveryRequired)?;
        let journal: ActivationJournal = serde_json::from_str(&intent.intent.transaction_evidence)
            .map_err(|_| ServerStoreError::Corrupt)?;
        self.validate_activation_journal(id, &journal)?;
        if intent.intent != maintenance_intent(&journal)? {
            return Err(ActivationError::RecoveryRequired);
        }
        self.verify_preparation_reference(id, &journal.next.deployment, control)?;
        if let Some(previous) = &journal.previous {
            self.verify_activation_reference(id, &previous.deployment, control)?;
        }
        let actual_journal: Option<(ActivationJournal, String)> =
            self.read_activation_document(id, "journal.json", control)?;
        if actual_journal.as_ref().is_some_and(|(value, hash)| {
            value != &journal || hash != &intent.intent.transaction_fingerprint
        }) {
            return Err(ActivationError::RecoveryRequired);
        }
        let active = self.read_activation_state(id, control)?;
        let outcome = if active.as_ref().map(|(state, hash)| (state, hash))
            == journal
                .previous
                .as_ref()
                .zip(journal.previous_fingerprint.as_ref())
        {
            ResolutionOutcome::Aborted
        } else if active.as_ref().map(|(state, hash)| (state, hash.as_str()))
            == Some((&journal.next, intent.intent.next_state_fingerprint.as_str()))
        {
            ResolutionOutcome::Committed
        } else {
            return Err(ActivationError::RecoveryRequired);
        };
        if outcome == ResolutionOutcome::Committed {
            self.verify_activation_reference(id, &journal.next.deployment, control)?;
        }
        let stored: Option<(ActivationResolutionRecord, String)> =
            self.read_activation_document(id, &record_name(intent), control)?;
        let record = if let Some((record, _)) = stored {
            self.validate_resolution_record(id, &record, control)?;
            if &record.intent != intent
                || record.outcome != outcome
                || record.active.as_ref() != active.as_ref().map(|(state, _)| state)
                || record.active_fingerprint.as_ref() != active.as_ref().map(|(_, hash)| hash)
            {
                return Err(ActivationError::RecoveryRequired);
            }
            record
        } else {
            ActivationResolutionRecord {
                schema_version: 1,
                intent: intent.clone(),
                journal,
                active: active.as_ref().map(|(state, _)| state.clone()),
                active_fingerprint: active.as_ref().map(|(_, hash)| hash.clone()),
                outcome,
                resolver: resolver.clone(),
            }
        };
        self.validate_resolution_record(id, &record, control)?;
        let existing_receipt = inventory
            .receipts
            .iter()
            .find(|value| &value.intent == intent)
            .cloned();
        if existing_receipt
            .as_ref()
            .is_some_and(|value| receipt(&record).as_ref() != Ok(value))
        {
            return Err(ActivationError::RecoveryRequired);
        }
        let mut plan = ActivationResolutionPlan {
            context: self.context().clone(),
            proof: String::new(),
            record,
            fingerprint: String::new(),
            resolver,
            journal_fingerprint: actual_journal.as_ref().map(|(_, hash)| hash.clone()),
            catalog_fingerprint: catalog,
            selection_fingerprint: selection,
            receipt: existing_receipt,
        };
        self.check_resolution_inputs(&plan, control, false)?;
        if installation
            .inspect_maintenance()
            .map_err(|_| ActivationError::RecoveryRequired)?
            != inventory
        {
            return Err(ServerStoreError::StaleRevision.into());
        }
        plan.fingerprint = fingerprint(&plan)?;
        plan.proof = plan.fingerprint.clone();
        Ok(plan)
    }

    fn validate_resolution_record(
        &self,
        id: &ServerInstanceId,
        record: &ActivationResolutionRecord,
        control: &ContentStageControl,
    ) -> Result<(), ActivationError> {
        self.validate_resolution_metadata(id, record)?;
        self.verify_preparation_reference(id, &record.journal.next.deployment, control)?;
        if record.outcome == ResolutionOutcome::Committed {
            self.verify_activation_reference(id, &record.journal.next.deployment, control)?;
        }
        if let Some(previous) = &record.journal.previous {
            self.verify_activation_reference(id, &previous.deployment, control)?;
        }
        Ok(())
    }

    pub(super) fn validate_resolution_metadata(
        &self,
        id: &ServerInstanceId,
        record: &ActivationResolutionRecord,
    ) -> Result<(), ActivationError> {
        if record.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion.into());
        }
        self.validate_activation_journal(id, &record.journal)?;
        validate_owner(&record.resolver, self.context().space_id(), id)?;
        let intent = maintenance_intent(&record.journal)?;
        let intent_hash = blake3::hash(&encode(&intent)?).to_hex().to_string();
        if record.intent.intent != intent || record.intent.fingerprint != intent_hash {
            return Err(ServerStoreError::Corrupt.into());
        }
        let (state, hash) = match record.outcome {
            ResolutionOutcome::Aborted => (
                record.journal.previous.as_ref(),
                record.journal.previous_fingerprint.as_ref(),
            ),
            ResolutionOutcome::Committed => (
                Some(&record.journal.next),
                Some(&record.intent.intent.next_state_fingerprint),
            ),
        };
        if record.active.as_ref() != state || record.active_fingerprint.as_ref() != hash {
            return Err(ServerStoreError::Corrupt.into());
        }
        encode(record)?;
        Ok(())
    }

    fn check_resolution_inputs(
        &self,
        plan: &ActivationResolutionPlan,
        control: &ContentStageControl,
        journal_retired: bool,
    ) -> Result<(), ActivationError> {
        self.validate_record_context()?;
        let id = &plan.record.journal.instance_id;
        self.show(id)?;
        let current = self.read_activation_state(id, control)?;
        let current_journal: Option<(ActivationJournal, String)> =
            self.read_activation_document(id, "journal.json", control)?;
        let expected_journal = if journal_retired {
            None
        } else {
            plan.journal_fingerprint.as_ref()
        };
        if self
            .read()?
            .1
            .map(|value| value.to_hex().to_string())
            .as_ref()
            != Some(&plan.catalog_fingerprint)
            || self.read_record_state(id, control)?.1 != plan.selection_fingerprint
            || current.as_ref().map(|(state, _)| state) != plan.record.active.as_ref()
            || current.as_ref().map(|(_, hash)| hash) != plan.record.active_fingerprint.as_ref()
            || current_journal.as_ref().map(|(_, hash)| hash) != expected_journal
            || current_journal
                .as_ref()
                .is_some_and(|(journal, _)| journal != &plan.record.journal)
        {
            return Err(ServerStoreError::StaleRevision.into());
        }
        control.checkpoint()?;
        Ok(())
    }

    pub(crate) fn admit_activation_resolution<'a>(
        &self,
        plan: &ActivationResolutionPlan,
        _installation: &'a InstallationLocks,
    ) -> Result<ActivationRecoveryAdmission<'a>, ActivationError> {
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision.into());
        }
        self.capabilities()
            .definition_kind
            .ok_or(ServerStoreError::UnsupportedGame)?;
        Err(ActivationError::Unavailable)
    }

    pub(in crate::core::server) fn admit_native_resolution<'a>(
        &self,
        plan: &ActivationResolutionPlan,
        installation: &'a InstallationLocks,
        execution: &super::super::owner::DeploymentExecution,
        bound: &super::super::ServerInstallation,
        control: &ContentStageControl,
    ) -> Result<ActivationRecoveryAdmission<'a>, ActivationError> {
        execution
            .validate(self.context())
            .map_err(|_| ServerStoreError::Ownership)?;
        let identity = execution.identity();
        if &plan.context != self.context()
            || identity.target_instance != plan.record.journal.instance_id
            || identity.owner_id != plan.resolver.owner_id
            || identity.owner_generation != plan.resolver.owner_generation
            || identity.operation_id != plan.resolver.operation_id
            || identity.run_generation != plan.resolver.run_generation
            || self.native_installation(&plan.record.journal.instance_id)? != *bound
            || InstallationLocks::capture(&bound.directory)
                .and_then(|locks| locks.coordination_key())
                .map_err(|_| ServerStoreError::Ownership)?
                != plan.record.journal.installation_key
            || installation
                .coordination_key()
                .map_err(|_| ServerStoreError::Ownership)?
                != plan.record.journal.installation_key
        {
            return Err(ActivationError::OwnerMismatch);
        }
        let claim =
            crate::core::tasks::db_process_lock::pin_held_claim(&self.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
        let recovery = installation
            .try_recover_maintenance(&plan.record.intent)
            .map_err(|_| ServerStoreError::Ownership)?;
        bound.verify()?;
        control.checkpoint()?;
        Ok(ActivationRecoveryAdmission {
            context: self.context().clone(),
            _claim: claim,
            recovery,
            resolver: plan.resolver.clone(),
            installation: Some(bound.clone()),
        })
    }

    pub(crate) fn apply_activation_resolution(
        &self,
        plan: ActivationResolutionPlan,
        installation: &InstallationLocks,
        admission: &ActivationRecoveryAdmission<'_>,
        control: &ContentStageControl,
    ) -> Result<ActivationResolutionReport, ActivationError> {
        self.resolve_activation(plan, installation, admission, control, |_| Ok(()))
    }

    fn resolve_activation(
        &self,
        plan: ActivationResolutionPlan,
        installation: &InstallationLocks,
        admission: &ActivationRecoveryAdmission<'_>,
        control: &ContentStageControl,
        mut boundary: impl FnMut(ResolutionBoundary) -> Result<(), ActivationError>,
    ) -> Result<ActivationResolutionReport, ActivationError> {
        if &plan.context != self.context() || plan.fingerprint != plan.proof {
            return Err(ServerStoreError::StaleRevision.into());
        }
        admission.validate(self, &plan)?;
        let id = &plan.record.journal.instance_id;
        let fresh =
            self.plan_activation_resolution(id, installation, plan.resolver.clone(), control)?;
        if fresh.fingerprint != plan.proof || encode(&fresh)? != encode(&plan)? {
            return Err(ServerStoreError::StaleRevision.into());
        }
        self.publish_resolution_record(&plan.record, control)?;
        boundary(ResolutionBoundary::AfterRecord)?;
        let receipt = receipt(&plan.record)?;
        admission
            .recovery
            .publish_receipt(&receipt)
            .map_err(|_| ActivationError::RecoveryRequired)?;
        boundary(ResolutionBoundary::AfterReceipt)?;
        self.validate_resolution_record(id, &plan.record, control)?;
        self.with_activation_claim(|| {
            admission.validate(self, &plan)?;
            self.check_resolution_inputs(&plan, control, false)?;
            self.verify_resolution_record(&plan.record, control)?;
            admission
                .recovery
                .verify_receipt(&receipt)
                .map_err(|_| ActivationError::RecoveryRequired)?;
            control.checkpoint()?;
            if plan.journal_fingerprint.is_some() {
                fs::remove_file(self.activation_root(id).join("journal.json"))
                    .map_err(|_| ServerStoreError::Io)?;
            }
            Ok(())
        })?;
        boundary(ResolutionBoundary::AfterJournalRetire)?;
        boundary(ResolutionBoundary::BeforeIntentRetire)?;
        self.with_activation_claim(|| {
            admission.validate(self, &plan)?;
            self.check_resolution_inputs(&plan, control, true)?;
            self.verify_resolution_record(&plan.record, control)?;
            control.checkpoint()?;
            admission
                .recovery
                .retire_intent(&receipt)
                .map_err(|_| ActivationError::RecoveryRequired)?;
            Ok(())
        })?;
        boundary(ResolutionBoundary::AfterIntentRetire)?;
        Ok(ActivationResolutionReport {
            record: plan.record,
            receipt,
            journal_retired: plan.journal_fingerprint.is_some(),
            intent_retired: true,
            engine_admission_authorized: false,
            writes_performed: true,
        })
    }

    pub(super) fn check_resolution_capacity(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(), ActivationError> {
        let root = self.activation_root(id).join("resolutions");
        super::super::store::check_directory_chain(&root)?;
        match fs::read_dir(&root) {
            Ok(entries) => {
                for (count, entry) in entries.enumerate() {
                    entry.map_err(|_| ServerStoreError::Io)?;
                    if count >= 1023 {
                        return Err(ServerStoreError::Invalid(
                            "Activation resolution retention limit reached".into(),
                        )
                        .into());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ServerStoreError::Io.into()),
        }
        Ok(())
    }

    fn publish_resolution_record(
        &self,
        record: &ActivationResolutionRecord,
        control: &ContentStageControl,
    ) -> Result<(), ActivationError> {
        let id = &record.journal.instance_id;
        let path = self.activation_root(id).join(record_name(&record.intent));
        if path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return self.verify_resolution_record(record, control);
        }
        self.check_resolution_capacity(id)?;
        let root = path.parent().ok_or(ServerStoreError::UnsafePath)?;
        fs::create_dir_all(root).map_err(|_| ServerStoreError::Io)?;
        super::super::store::check_directory_chain(root)?;
        let mut file = tempfile::NamedTempFile::new_in(root).map_err(|_| ServerStoreError::Io)?;
        file.write_all(&encode(record)?)
            .map_err(|_| ServerStoreError::Io)?;
        file.as_file()
            .sync_all()
            .map_err(|_| ServerStoreError::Io)?;
        control.checkpoint()?;
        file.persist_noclobber(&path)
            .map_err(|_| ActivationError::RecoveryRequired)?;
        self.verify_resolution_record(record, control)
    }

    fn verify_resolution_record(
        &self,
        record: &ActivationResolutionRecord,
        control: &ContentStageControl,
    ) -> Result<(), ActivationError> {
        let stored: Option<(ActivationResolutionRecord, String)> = self.read_activation_document(
            &record.journal.instance_id,
            &record_name(&record.intent),
            control,
        )?;
        if stored.as_ref().map(|(value, hash)| (value, hash.as_str()))
            != Some((record, fingerprint(record)?.as_str()))
        {
            return Err(ActivationError::RecoveryRequired);
        }
        Ok(())
    }
}

impl ActivationRecoveryAdmission<'_> {
    fn validate(
        &self,
        store: &ServerStore,
        plan: &ActivationResolutionPlan,
    ) -> Result<(), ActivationError> {
        if let Some(installation) = &self.installation {
            installation.verify()?;
        }
        if &self.context != store.context() || self.resolver != plan.resolver {
            return Err(ActivationError::OwnerMismatch);
        }
        self.recovery
            .verify_intent(&plan.record.intent)
            .map_err(|_| ActivationError::OwnerMismatch)
    }
}

#[cfg(test)]
impl ServerStore {
    pub(in crate::core::server) fn test_activation_resolution(
        &self,
        plan: ActivationResolutionPlan,
        installation: &InstallationLocks,
        admission: &ActivationRecoveryAdmission<'_>,
        control: &ContentStageControl,
        boundary: impl FnMut(ResolutionBoundary) -> Result<(), ActivationError>,
    ) -> Result<ActivationResolutionReport, ActivationError> {
        self.resolve_activation(plan, installation, admission, control, boundary)
    }
}
