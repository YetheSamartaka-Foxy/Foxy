use std::fs;
use std::io::Write;

use super::manifest::{destinations, encode, fingerprint, validate_owner};
use super::{
    ActivationAdmission, ActivationDestinations, ActivationError, ActivationJournal,
    ActivationPlan, ActivationRecovery, ActivationRequest, ActivationState, PreparedReference,
    RecoveryDisposition, TransactionBoundary,
};
use crate::core::server::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};

impl ServerStore {
    pub(crate) fn plan_activation(
        &self,
        request: ActivationRequest,
        control: &ContentStageControl,
    ) -> Result<ActivationPlan, ActivationError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        let id = &request.instance_id;
        self.show(id)?;
        validate_owner(&request.owner, self.context().space_id(), id)?;
        if self
            .read_activation_document::<ActivationJournal>(id, "journal.json", control)?
            .is_some()
        {
            return Err(ActivationError::RecoveryRequired);
        }
        let (_, catalog, _) = self.read()?;
        let catalog = catalog
            .ok_or(ServerStoreError::Corrupt)?
            .to_hex()
            .to_string();
        let (selection, selection_fingerprint) = self.read_record_state(id, control)?;
        if selection.generation != request.expected_selection_generation {
            return Err(ServerStoreError::StaleRevision.into());
        }
        let report = self.verify_deployment_preparation(id, &request.preparation_id, control)?;
        if selection.selected.as_deref() != Some(report.manifest.record_id.as_str()) {
            return Err(ServerStoreError::StaleRevision.into());
        }
        let previous = self.read_activation_state(id, control)?;
        let generation = previous.as_ref().map_or(0, |(state, _)| state.generation);
        if generation != request.expected_activation_generation {
            return Err(ServerStoreError::StaleRevision.into());
        }
        if let Some((state, _)) = &previous {
            self.verify_activation_reference(id, &state.deployment, control)?;
        }
        let reference = PreparedReference {
            manifest_fingerprint: report
                .preparation_id
                .strip_prefix("prepared_")
                .ok_or(ServerStoreError::Corrupt)?
                .into(),
            preparation_id: report.preparation_id,
            record_id: report.manifest.record_id.clone(),
            runtime: request.runtime,
        };
        self.check_deployment_destination(
            &self.activation_root(id),
            Some(&report.manifest.record.content_bindings),
        )?;
        let next = ActivationState {
            schema_version: 1,
            context_fingerprint: self.activation_context_fingerprint(),
            game_space_id: self.context().space_id().into(),
            game_id: self.context().game_id().into(),
            instance_id: id.clone(),
            generation: generation.checked_add(1).ok_or(ServerStoreError::Corrupt)?,
            installation_key: request.installation_key.clone(),
            deployment: reference,
            owner: request.owner.clone(),
        };
        let mut journal = ActivationJournal {
            schema_version: 1,
            context_fingerprint: self.activation_context_fingerprint(),
            game_space_id: next.game_space_id.clone(),
            game_id: next.game_id.clone(),
            instance_id: id.clone(),
            owner: request.owner,
            installation_key: request.installation_key,
            catalog_fingerprint: catalog,
            selection_generation: selection.generation,
            selection_fingerprint: selection_fingerprint.ok_or(ServerStoreError::Corrupt)?,
            previous_fingerprint: previous.as_ref().map(|(_, hash)| hash.clone()),
            previous: previous.map(|(state, _)| state),
            next,
            destinations: ActivationDestinations {
                active_state: String::new(),
                journal: String::new(),
                candidate: String::new(),
                predecessor: None,
            },
        };
        journal.destinations = destinations(&journal);
        self.validate_activation_journal(id, &journal)?;
        self.check_activation_inputs(&journal, control)?;
        let proof = fingerprint(&journal)?;
        Ok(ActivationPlan {
            context: self.context().clone(),
            proof: proof.clone(),
            fingerprint: proof,
            journal,
        })
    }

    fn check_activation_inputs(
        &self,
        journal: &ActivationJournal,
        control: &ContentStageControl,
    ) -> Result<(), ActivationError> {
        self.validate_record_context()?;
        self.show(&journal.instance_id)?;
        if self
            .read()?
            .1
            .map(|hash| hash.to_hex().to_string())
            .as_deref()
            != Some(journal.catalog_fingerprint.as_str())
            || self
                .read_record_state(&journal.instance_id, control)?
                .1
                .as_deref()
                != Some(journal.selection_fingerprint.as_str())
            || self
                .read_activation_state(&journal.instance_id, control)?
                .map(|(_, hash)| hash)
                != journal.previous_fingerprint
        {
            return Err(ServerStoreError::StaleRevision.into());
        }
        control.checkpoint()?;
        Ok(())
    }

    pub(crate) fn apply_activation_transaction(
        &self,
        plan: ActivationPlan,
        admission: &ActivationAdmission<'_>,
        control: &ContentStageControl,
    ) -> Result<ActivationRecovery, ActivationError> {
        self.execute_activation(plan, admission, control, |_| Ok(()))
    }

    pub(in crate::core::server) fn execute_activation(
        &self,
        plan: ActivationPlan,
        admission: &ActivationAdmission<'_>,
        control: &ContentStageControl,
        mut boundary: impl FnMut(TransactionBoundary) -> Result<(), ActivationError>,
    ) -> Result<ActivationRecovery, ActivationError> {
        if &plan.context != self.context()
            || plan.proof != plan.fingerprint
            || fingerprint(&plan.journal)? != plan.proof
        {
            return Err(ServerStoreError::StaleRevision.into());
        }
        admission.validate(self.context(), &plan.journal)?;
        let journal = &plan.journal;
        let fresh = self.plan_activation(
            ActivationRequest {
                instance_id: journal.instance_id.clone(),
                preparation_id: journal.next.deployment.preparation_id.clone(),
                expected_selection_generation: journal.selection_generation,
                expected_activation_generation: journal
                    .previous
                    .as_ref()
                    .map_or(0, |state| state.generation),
                owner: journal.owner.clone(),
                installation_key: journal.installation_key.clone(),
                runtime: journal.next.deployment.runtime.clone(),
            },
            control,
        )?;
        if fresh.journal != plan.journal || fresh.fingerprint != plan.proof {
            return Err(ServerStoreError::StaleRevision.into());
        }
        boundary(TransactionBoundary::BeforeJournal)?;
        control.checkpoint()?;
        self.check_resolution_capacity(&journal.instance_id)?;
        let protection = admission
            .maintenance
            .begin_intent(super::maintenance_intent(journal)?)
            .map_err(|_| ServerStoreError::Ownership)?;
        boundary(TransactionBoundary::AfterIntent)?;
        control.checkpoint()?;
        let root = self.activation_root(&journal.instance_id);
        super::super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        super::super::store::check_directory_chain(&root)?;
        let mut intent =
            tempfile::NamedTempFile::new_in(&root).map_err(|_| ServerStoreError::Io)?;
        intent
            .write_all(&encode(journal)?)
            .map_err(|_| ServerStoreError::Io)?;
        intent
            .as_file()
            .sync_all()
            .map_err(|_| ServerStoreError::Io)?;
        self.with_activation_claim(|| {
            admission.validate(self.context(), journal)?;
            admission
                .maintenance
                .verify_intent(&protection)
                .map_err(|_| ServerStoreError::Ownership)?;
            self.check_activation_inputs(journal, control)?;
            super::super::store::check_directory_chain(&root)?;
            super::super::store::check_regular_file(&root.join("journal.json"))?;
            intent
                .persist_noclobber(root.join("journal.json"))
                .map_err(|_| ActivationError::RecoveryRequired)?;
            Ok(())
        })?;
        boundary(TransactionBoundary::AfterJournal)?;
        self.verify_activation_reference(&journal.instance_id, &journal.next.deployment, control)?;
        if let Some(previous) = &journal.previous {
            self.verify_activation_reference(&journal.instance_id, &previous.deployment, control)?;
        }
        let mut pointer =
            tempfile::NamedTempFile::new_in(&root).map_err(|_| ServerStoreError::Io)?;
        pointer
            .write_all(&encode(&journal.next)?)
            .map_err(|_| ServerStoreError::Io)?;
        pointer
            .as_file()
            .sync_all()
            .map_err(|_| ServerStoreError::Io)?;
        boundary(TransactionBoundary::BeforePointer)?;
        self.with_activation_claim(|| {
            admission.validate(self.context(), journal)?;
            admission
                .maintenance
                .verify_intent(&protection)
                .map_err(|_| ServerStoreError::Ownership)?;
            self.check_activation_inputs(journal, control)?;
            let persisted: Option<(ActivationJournal, String)> =
                self.read_activation_document(&journal.instance_id, "journal.json", control)?;
            if persisted
                .as_ref()
                .map(|(value, hash)| (value, hash.as_str()))
                != Some((journal, plan.proof.as_str()))
            {
                return Err(ActivationError::RecoveryRequired);
            }
            super::super::store::check_regular_file(&root.join("active.json"))?;
            control.checkpoint()?;
            if journal.previous.is_some() {
                pointer
                    .persist(root.join("active.json"))
                    .map_err(|_| ServerStoreError::Io)?;
            } else {
                pointer
                    .persist_noclobber(root.join("active.json"))
                    .map_err(|_| ActivationError::RecoveryRequired)?;
            }
            Ok(())
        })?;
        boundary(TransactionBoundary::AfterPointer)?;
        Ok(ActivationRecovery {
            disposition: RecoveryDisposition::CandidateCommitted,
            journal: Some(plan.journal.clone()),
            active: Some(plan.journal.next),
            selection_unchanged: Some(true),
            catalog_unchanged: Some(true),
            journal_retained: true,
            protection_release_authorized: false,
            writes_performed: true,
        })
    }

    pub(super) fn with_activation_claim<T>(
        &self,
        operation: impl FnOnce() -> Result<T, ActivationError>,
    ) -> Result<T, ActivationError> {
        crate::core::tasks::db_process_lock::with_held_claim(
            &self.context().database_path(),
            || Ok(operation()),
        )
        .map_err(|_| ServerStoreError::Ownership)?
    }

    pub(crate) fn inspect_activation_recovery(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<ActivationRecovery, ActivationError> {
        self.validate_record_context()?;
        self.show(id)?;
        let catalog = self.read()?.1;
        let journal: Option<(ActivationJournal, String)> =
            self.read_activation_document(id, "journal.json", control)?;
        let active = self.read_activation_state(id, control)?;
        let mut result = ActivationRecovery {
            disposition: RecoveryDisposition::NoJournal,
            journal: journal.as_ref().map(|(journal, _)| journal.clone()),
            active: active.as_ref().map(|(state, _)| state.clone()),
            selection_unchanged: None,
            catalog_unchanged: None,
            journal_retained: journal.is_some(),
            protection_release_authorized: false,
            writes_performed: false,
        };
        let selection = if journal.is_some() {
            self.read_record_state(id, control)?.1
        } else {
            None
        };
        if let Some((journal, _)) = &journal {
            self.validate_activation_journal(id, journal)?;
            self.verify_activation_reference(id, &journal.next.deployment, control)?;
            if let Some(previous) = &journal.previous {
                self.verify_activation_reference(id, &previous.deployment, control)?;
            }
            result.selection_unchanged =
                Some(selection.as_deref() == Some(journal.selection_fingerprint.as_str()));
            result.catalog_unchanged = Some(
                catalog.map(|hash| hash.to_hex().to_string()).as_deref()
                    == Some(journal.catalog_fingerprint.as_str()),
            );
            result.disposition = if active.as_ref().map(|(_, hash)| hash)
                == journal.previous_fingerprint.as_ref()
                && active.as_ref().map(|(state, _)| state) == journal.previous.as_ref()
            {
                RecoveryDisposition::PredecessorRetained
            } else if active.as_ref().map(|(state, hash)| (state, hash.as_str()))
                == Some((&journal.next, fingerprint(&journal.next)?.as_str()))
            {
                RecoveryDisposition::CandidateCommitted
            } else {
                RecoveryDisposition::Ambiguous
            };
        } else if let Some((state, _)) = &active {
            self.verify_activation_reference(id, &state.deployment, control)?;
        }
        if self.read_activation_state(id, control)? != active
            || self.read_activation_document::<ActivationJournal>(id, "journal.json", control)?
                != journal
            || self.read()?.1 != catalog
            || (journal.is_some() && self.read_record_state(id, control)?.1 != selection)
        {
            return Err(ServerStoreError::StaleRevision.into());
        }
        self.validate_record_context()?;
        control.checkpoint()?;
        Ok(result)
    }
}

#[cfg(test)]
impl ServerStore {
    pub(in crate::core::server) fn test_activation_transaction(
        &self,
        plan: ActivationPlan,
        admission: &ActivationAdmission<'_>,
        control: &ContentStageControl,
        boundary: impl FnMut(TransactionBoundary) -> Result<(), ActivationError>,
    ) -> Result<ActivationRecovery, ActivationError> {
        self.execute_activation(plan, admission, control, boundary)
    }
}
