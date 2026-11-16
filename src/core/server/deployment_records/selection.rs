use std::io::Write;

use super::super::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};
use super::store::record_error;
use super::validation::{encode, record_id};
use super::{DeploymentRecordCommit, DeploymentRecordSelectionPlan, DeploymentRecordState};

impl ServerStore {
    pub fn plan_deployment_record_selection(
        &self,
        id: &ServerInstanceId,
        target: &str,
        expected_generation: u64,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordSelectionPlan, ServerStoreError> {
        let result = self
            .prepare_record_selection(id, target, expected_generation, control)
            .map_err(record_error);
        control.finish(&result);
        result
    }

    fn prepare_record_selection(
        &self,
        id: &ServerInstanceId,
        target: &str,
        expected_generation: u64,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordSelectionPlan, ServerStoreError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        self.show(id)?;
        if !record_id(target) {
            return Err(ServerStoreError::Invalid(
                "Invalid deployment record ID".into(),
            ));
        }
        let (state, before) = self.read_record_state(id, control)?;
        if state.generation != expected_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        if !state.records.iter().any(|record| record == target) {
            return Err(ServerStoreError::NotFound);
        }
        let record = self.inspect_record(id, target, control)?;
        self.check_record_destination_for_bindings(id, Some(&record.manifest.content_bindings))?;
        if self.read_record_state(id, control)?.1 != before {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_record_context()?;
        control.checkpoint()?;
        let mut plan = DeploymentRecordSelectionPlan {
            context: self.context().clone(),
            proof: String::new(),
            instance_id: id.clone(),
            expected_generation,
            selection_fingerprint: before.ok_or(ServerStoreError::Corrupt)?,
            inventory_fingerprint: blake3::hash(&encode(&state)?).to_hex().to_string(),
            previous_record_id: state.selected,
            target: record,
            fingerprint: String::new(),
        };
        plan.fingerprint = blake3::hash(&encode(&plan)?).to_hex().to_string();
        plan.proof = plan.fingerprint.clone();
        Ok(plan)
    }

    pub fn apply_deployment_record_selection(
        &self,
        plan: DeploymentRecordSelectionPlan,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordCommit, ServerStoreError> {
        let result = self.select_record(plan, control).map_err(record_error);
        control.finish(&result);
        result
    }

    fn select_record(
        &self,
        plan: DeploymentRecordSelectionPlan,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordCommit, ServerStoreError> {
        control.checkpoint()?;
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if &plan.context != self.context() || plan.proof != plan.fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let fresh = self.prepare_record_selection(
            &plan.instance_id,
            &plan.target.record_id,
            plan.expected_generation,
            control,
        )?;
        if fresh.fingerprint != plan.proof || encode(&fresh)? != encode(&plan)? {
            return Err(ServerStoreError::StaleRevision);
        }
        let (mut state, before) = self.read_record_state(&plan.instance_id, control)?;
        if before.as_deref() != Some(plan.selection_fingerprint.as_str()) {
            return Err(ServerStoreError::StaleRevision);
        }
        let selection_changed = state.selected.as_deref() != Some(&plan.target.record_id);
        if selection_changed {
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or(ServerStoreError::Corrupt)?;
            state.selected = Some(plan.target.record_id.clone());
        }
        self.validate_record_state(&plan.instance_id, &state)?;
        self.persist_record_state(
            &state,
            Some(&plan.selection_fingerprint),
            selection_changed,
            control,
            || {
                self.validate_record_context()?;
                self.show(&plan.instance_id)?;
                self.check_record_destination_for_bindings(
                    &plan.instance_id,
                    Some(&plan.target.manifest.content_bindings),
                )
            },
        )?;
        Ok(DeploymentRecordCommit {
            state,
            record: fresh.target,
            published: false,
            selection_changed,
            active_deployment_changed: false,
            process_started: false,
        })
    }

    pub(super) fn persist_record_state(
        &self,
        state: &DeploymentRecordState,
        before: Option<&str>,
        changed: bool,
        control: &ContentStageControl,
        validate: impl FnOnce() -> Result<(), ServerStoreError>,
    ) -> Result<(), ServerStoreError> {
        let root = self.record_root(&state.instance_id);
        super::super::store::check_directory_chain(&root)?;
        let mut temporary = if changed {
            let bytes = encode(state)?;
            let mut file =
                tempfile::NamedTempFile::new_in(&root).map_err(|_| ServerStoreError::Io)?;
            if file
                .write_all(&bytes)
                .and_then(|()| file.as_file().sync_all())
                .is_err()
            {
                file.close()
                    .map_err(|_| ServerStoreError::RecordCleanupIncomplete)?;
                return Err(ServerStoreError::Io);
            }
            Some(file)
        } else {
            None
        };
        #[cfg(test)]
        control.before_record_selection_for_test();
        let result =
            crate::core::server::owner::with_write(&self.context().database_path(), || {
                Ok((|| {
                    validate()?;
                    if self
                        .read_record_state(&state.instance_id, control)?
                        .1
                        .as_deref()
                        != before
                    {
                        return Err(ServerStoreError::StaleRevision);
                    }
                    let path = root.join("state.json");
                    super::super::store::check_regular_file(&path)?;
                    control.checkpoint()?;
                    if let Some(file) = temporary.take()
                        && let Err(error) = file.persist(path)
                    {
                        error
                            .file
                            .close()
                            .map_err(|_| ServerStoreError::RecordCleanupIncomplete)?;
                        return Err(ServerStoreError::Io);
                    }
                    Ok(())
                })())
            })
            .map_err(|_| ServerStoreError::Ownership)
            .and_then(|result| result);
        if let Some(file) = temporary {
            file.close()
                .map_err(|_| ServerStoreError::RecordCleanupIncomplete)?;
        }
        result
    }
}
