use std::fs;
use std::io::Write;

use serde::Deserialize;

use super::super::{
    ContentStageControl, ContentStagePhase, DeploymentCompositionRequest, ServerInstanceId,
    ServerStore, ServerStoreError,
};
use super::validation::{encode, manifest_from, record_id};
use super::{
    DeploymentRecordCommit, DeploymentRecordManifest, DeploymentRecordPlan, DeploymentRecordState,
    DeploymentRecordVerification, MAX_BYTES, MAX_RECORDS,
};

impl ServerStore {
    pub fn plan_deployment_record(
        &self,
        request: DeploymentCompositionRequest,
        expected_selection_generation: Option<u64>,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordPlan, ServerStoreError> {
        let result = self
            .prepare_record(request, expected_selection_generation, control)
            .map_err(record_error);
        control.finish(&result);
        result
    }

    fn prepare_record(
        &self,
        request: DeploymentCompositionRequest,
        expected_selection_generation: Option<u64>,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordPlan, ServerStoreError> {
        self.validate_record_context()?;
        control.checkpoint()?;
        let (state, before) = self.read_record_state(&request.instance_id, control)?;
        if expected_selection_generation.is_some_and(|generation| state.generation != generation) {
            return Err(ServerStoreError::StaleRevision);
        }
        let review = self.inspect_composition(request.clone(), control)?;
        let manifest = manifest_from(&review)?;
        self.validate_record_manifest(&request.instance_id, &manifest)?;
        self.check_record_destination(&request.instance_id)?;
        let record_id = format!("deployment_{}", blake3::hash(&encode(&manifest)?).to_hex());
        if !state.records.contains(&record_id) && state.records.len() >= MAX_RECORDS {
            return Err(ServerStoreError::Invalid(
                "Deployment record history exceeds 1024 records".into(),
            ));
        }
        if self.read_record_state(&request.instance_id, control)?.1 != before {
            return Err(ServerStoreError::StaleRevision);
        }
        let proof = blake3::hash(&encode(&(
            &manifest,
            &review.fingerprint,
            &before,
            state.generation,
        ))?)
        .to_hex()
        .to_string();
        control.checkpoint()?;
        Ok(DeploymentRecordPlan {
            context: self.context().clone(),
            request,
            before,
            proof: proof.clone(),
            review,
            manifest,
            record_id,
            fingerprint: proof,
            expected_generation: state.generation,
        })
    }

    pub fn deployment_records(
        &self,
        id: &ServerInstanceId,
    ) -> Result<DeploymentRecordState, ServerStoreError> {
        self.validate_record_context()?;
        self.show(id)?;
        self.read_record_state(id, &ContentStageControl::default())
            .map(|(state, _)| state)
    }

    pub(in crate::core::server) fn read_record_state(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<(DeploymentRecordState, Option<String>), ServerStoreError> {
        control.checkpoint()?;
        let path = self.record_root(id).join("state.json");
        super::super::store::check_directory_chain(
            path.parent().ok_or(ServerStoreError::UnsafePath)?,
        )?;
        super::super::store::check_regular_file(&path)?;
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok((
                DeploymentRecordState {
                    schema_version: 1,
                    game_space_id: self.context().space_id().into(),
                    instance_id: id.clone(),
                    generation: 0,
                    selected: None,
                    records: vec![],
                },
                None,
            ));
        }
        let bytes =
            super::super::content_staging::read_bounded_controlled(&path, MAX_BYTES, control)?;
        let state: DeploymentRecordState = decode(&bytes)?;
        self.validate_record_state(id, &state)?;
        Ok((state, Some(blake3::hash(&bytes).to_hex().to_string())))
    }

    pub fn verify_deployment_record(
        &self,
        id: &ServerInstanceId,
        record: &str,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordVerification, ServerStoreError> {
        let result = (|| {
            self.validate_record_context()?;
            self.show(id)?;
            self.inspect_record(id, record, control)
        })()
        .map_err(record_error);
        control.finish(&result);
        result
    }

    pub(in crate::core::server) fn read_record(
        &self,
        id: &ServerInstanceId,
        record: &str,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordManifest, ServerStoreError> {
        if !record_id(record) {
            return Err(ServerStoreError::Invalid(
                "Invalid deployment record ID".into(),
            ));
        }
        let directory = self.record_root(id).join("records").join(record);
        super::super::store::check_directory_chain(&directory)?;
        if !directory.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Err(ServerStoreError::NotFound);
        }
        let mut entries = fs::read_dir(&directory).map_err(|_| ServerStoreError::Io)?;
        let only = entries
            .next()
            .ok_or(ServerStoreError::Corrupt)?
            .map_err(|_| ServerStoreError::Io)?;
        if only.file_name() != "manifest.json" || entries.next().is_some() {
            return Err(ServerStoreError::Corrupt);
        }
        let bytes = super::super::content_staging::read_bounded_controlled(
            &directory.join("manifest.json"),
            MAX_BYTES,
            control,
        )?;
        let manifest: DeploymentRecordManifest = decode(&bytes)?;
        self.validate_record_manifest(id, &manifest)?;
        if record != format!("deployment_{}", blake3::hash(&encode(&manifest)?).to_hex()) {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(manifest)
    }

    pub(super) fn inspect_record(
        &self,
        id: &ServerInstanceId,
        record: &str,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordVerification, ServerStoreError> {
        control.checkpoint()?;
        let manifest = self.read_record(id, record, control)?;
        self.inspect_record_archives_at(
            id,
            &manifest,
            &self
                .context()
                .space_dir()
                .join("server/instances")
                .join(id.as_str())
                .join("configuration/revisions")
                .join(&manifest.configuration_revision),
            &self.stage_root(id).join(&manifest.content_stage_id),
            control,
        )?;
        if self.read_record(id, record, control)? != manifest {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_record_context()?;
        control.checkpoint()?;
        Ok(DeploymentRecordVerification {
            record_id: record.into(),
            manifest,
            integrity_verified: true,
            configuration_verified: true,
            content_verified: true,
            deployment_ready: false,
            writes_performed: false,
            active_deployment_changed: false,
        })
    }

    pub(in crate::core::server) fn inspect_record_archives_at(
        &self,
        id: &ServerInstanceId,
        manifest: &DeploymentRecordManifest,
        configuration_directory: &std::path::Path,
        content_directory: &std::path::Path,
        control: &ContentStageControl,
    ) -> Result<
        (
            super::super::ConfigurationPreview,
            super::super::content_staging::ContentStageManifest,
        ),
        ServerStoreError,
    > {
        self.validate_record_manifest(id, manifest)?;
        let (instance, config, config_hash) = self.read_verified_configuration_at(
            id,
            &manifest.configuration_revision,
            configuration_directory,
            control,
        )?;
        let content = self
            .read_stage_manifest_at(id, &manifest.content_stage_id, content_directory, control)
            .map_err(|error| {
                if error == ServerStoreError::NotFound {
                    ServerStoreError::Corrupt
                } else {
                    error
                }
            })?;
        if let super::super::ServerDefinition::Reforger(settings) = &instance.definition {
            match &content.reforger_addons {
                Some(lock) => {
                    super::super::reforger_addons::check_definition(&instance.definition, lock)
                        .map_err(|_| ServerStoreError::Corrupt)?
                }
                None if !settings.addons.is_empty() => return Err(ServerStoreError::Corrupt),
                None => {}
            }
        }
        if instance.revision != manifest.instance_revision
            || config.fingerprint != manifest.configuration_fingerprint
            || config_hash != manifest.configuration_manifest_fingerprint
            || content.bindings != manifest.content_bindings
            || content.bindings_fingerprint != manifest.content_bindings_fingerprint
            || content.missions
                != super::super::missions::cycle_library_sources(&instance.definition)
        {
            return Err(ServerStoreError::Corrupt);
        }
        super::super::content_staging::verify_stage_tree(&content, content_directory, control)?;
        if self.read_stage_manifest_at(
            id,
            &manifest.content_stage_id,
            content_directory,
            control,
        )? != content
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let artifacts = super::super::deployment_composition::inspect_artifacts(
            &instance.definition,
            &content.entries,
        );
        if artifacts.truncated
            || !artifacts.issues.is_empty()
            || match &instance.definition {
                super::super::ServerDefinition::Arma3(settings) => {
                    !settings.mission_cycle.iter().any(|mission| mission.enabled)
                }
                super::super::ServerDefinition::Reforger(settings) => {
                    settings.scenario_id.is_none()
                }
            }
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok((config, content))
    }

    pub fn apply_deployment_record(
        &self,
        plan: DeploymentRecordPlan,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordCommit, ServerStoreError> {
        let result = self.commit_record(plan, control).map_err(record_error);
        control.finish(&result);
        result
    }

    fn commit_record(
        &self,
        plan: DeploymentRecordPlan,
        control: &ContentStageControl,
    ) -> Result<DeploymentRecordCommit, ServerStoreError> {
        control.checkpoint()?;
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if &plan.context != self.context() || plan.proof != plan.fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let fresh = self.prepare_record(
            plan.request.clone(),
            Some(plan.expected_generation),
            control,
        )?;
        if fresh.fingerprint != plan.proof
            || fresh.manifest != plan.manifest
            || fresh.record_id != plan.record_id
            || fresh.before != plan.before
            || fresh.review.fingerprint != plan.review.fingerprint
            || serde_json::to_value(&fresh.review).map_err(|_| ServerStoreError::Corrupt)?
                != serde_json::to_value(&plan.review).map_err(|_| ServerStoreError::Corrupt)?
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let (mut state, _) = self.read_record_state(&plan.manifest.instance_id, control)?;
        let selection_changed = state.selected.as_deref() != Some(&plan.record_id);
        if selection_changed {
            state.generation = state
                .generation
                .checked_add(1)
                .ok_or(ServerStoreError::Corrupt)?;
            state.selected = Some(plan.record_id.clone());
        }
        if !state.records.contains(&plan.record_id) {
            state.records.push(plan.record_id.clone());
        }
        self.validate_record_state(&plan.manifest.instance_id, &state)?;
        let root = self.record_root(&plan.manifest.instance_id);
        self.check_record_destination(&plan.manifest.instance_id)?;
        super::super::store::check_directory_chain(&root)?;
        super::super::store::check_directory_chain(&root.join("records"))?;
        fs::create_dir_all(root.join("records")).map_err(|_| ServerStoreError::Io)?;
        super::super::store::check_directory_chain(&root.join("records"))?;
        let published = self.publish_record(&plan, control)?;
        #[cfg(test)]
        control.record_published_for_test();
        let record = self.inspect_record(&plan.manifest.instance_id, &plan.record_id, control)?;
        self.persist_record_state(
            &state,
            plan.before.as_deref(),
            selection_changed,
            control,
            || {
                self.check_record_inputs(&plan.review, control)?;
                self.check_record_destination(&plan.manifest.instance_id)
            },
        )?;
        Ok(DeploymentRecordCommit {
            state,
            record,
            published,
            selection_changed,
            active_deployment_changed: false,
            process_started: false,
        })
    }

    fn publish_record(
        &self,
        plan: &DeploymentRecordPlan,
        control: &ContentStageControl,
    ) -> Result<bool, ServerStoreError> {
        let destination = plan.directory();
        super::super::store::check_directory_chain(&destination)?;
        if destination.try_exists().map_err(|_| ServerStoreError::Io)? {
            if self
                .inspect_record(&plan.manifest.instance_id, &plan.record_id, control)?
                .manifest
                != plan.manifest
            {
                return Err(ServerStoreError::Corrupt);
            }
            return Ok(false);
        }
        let temp = tempfile::Builder::new()
            .prefix(".record-build-")
            .tempdir_in(destination.parent().ok_or(ServerStoreError::UnsafePath)?)
            .map_err(|_| ServerStoreError::Io)?;
        let result = (|| {
            control.start_phase(ContentStagePhase::Preparing, 0, 0);
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(temp.path().join("manifest.json"))
                .map_err(|_| ServerStoreError::Io)?;
            file.write_all(&encode(&plan.manifest)?)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServerStoreError::Io)?;
            drop(file);
            self.check_record_inputs(&plan.review, control)?;
            control.checkpoint()?;
            super::super::content_staging::publish_no_replace(temp.path(), &destination)
        })();
        match result {
            Ok(true) => {
                let _ = temp.keep();
                Ok(true)
            }
            result => {
                temp.close()
                    .map_err(|_| ServerStoreError::RecordCleanupIncomplete)?;
                result
            }
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ServerStoreError> {
    #[derive(Deserialize)]
    struct Version {
        schema_version: u32,
    }
    let version: Version = serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if version.schema_version != 1 {
        return Err(ServerStoreError::UnsupportedVersion);
    }
    serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)
}

pub(super) fn record_error(error: ServerStoreError) -> ServerStoreError {
    if matches!(
        error,
        ServerStoreError::Cancelled | ServerStoreError::ReviewCancelled
    ) {
        ServerStoreError::RecordCancelled
    } else {
        error
    }
}
