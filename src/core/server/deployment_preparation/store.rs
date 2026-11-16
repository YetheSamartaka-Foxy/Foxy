use std::fs;
use std::io::Write;
use std::path::Path;

use super::super::content_staging::{
    StageEntryKind, copy_verified_file, parse_relative, publish_no_replace, verify_tree,
};
use super::super::{
    ContentStageControl, ContentStagePhase, ServerInstanceId, ServerStore, ServerStoreError,
};
use super::manifest::{encode, hash_id};
use super::{DeploymentPreparationList, DeploymentPreparationPlan, DeploymentPreparationReport};

impl ServerStore {
    pub fn plan_deployment_preparation(
        &self,
        id: &ServerInstanceId,
        expected_generation: u64,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationPlan, ServerStoreError> {
        let result = self
            .prepare_deployment(id, expected_generation, control)
            .map_err(preparation_error);
        control.finish(&result);
        result
    }

    fn prepare_deployment(
        &self,
        id: &ServerInstanceId,
        expected_generation: u64,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationPlan, ServerStoreError> {
        control.checkpoint()?;
        self.validate_preparation_context()?;
        self.show(id)?;
        let (state, before) = self.read_record_state(id, control)?;
        if state.generation != expected_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        let record_id = state.selected.as_deref().ok_or_else(|| {
            ServerStoreError::Invalid(
                "Select an offline deployment record before preparing runtime files.".into(),
            )
        })?;
        let record = self.read_record(id, record_id, control)?;
        self.check_preparation_destination(id, &record)?;
        let (configuration, content) = self.preparation_inputs(id, &record);
        let manifest =
            self.preparation_manifest(id, record_id, &record, &configuration, &content, control)?;
        if self.read_record(id, record_id, control)? != record
            || self.read_record_state(id, control)?.1 != before
        {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_preparation_context()?;
        let preparation_id = format!("prepared_{}", blake3::hash(&encode(&manifest)?).to_hex());
        let directory = self.preparation_root(id).join(&preparation_id);
        super::super::store::check_directory_chain(&directory)?;
        let mut plan = DeploymentPreparationPlan {
            context: self.context().clone(),
            proof: String::new(),
            manifest,
            preparation_id,
            fingerprint: String::new(),
            expected_generation,
            selection_fingerprint: before.ok_or(ServerStoreError::Corrupt)?,
            inventory_fingerprint: blake3::hash(&encode(&state)?).to_hex().to_string(),
            directory,
        };
        plan.fingerprint = blake3::hash(&encode(&plan)?).to_hex().to_string();
        plan.proof = plan.fingerprint.clone();
        control.checkpoint()?;
        Ok(plan)
    }

    pub fn apply_deployment_preparation(
        &self,
        plan: DeploymentPreparationPlan,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationReport, ServerStoreError> {
        let result = self
            .build_deployment(plan, control)
            .map_err(preparation_error);
        control.finish(&result);
        result
    }

    fn build_deployment(
        &self,
        plan: DeploymentPreparationPlan,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationReport, ServerStoreError> {
        control.checkpoint()?;
        let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if &plan.context != self.context() || plan.proof != plan.fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let id = &plan.manifest.record.instance_id;
        let fresh = self.prepare_deployment(id, plan.expected_generation, control)?;
        if fresh.fingerprint != plan.proof || encode(&fresh)? != encode(&plan)? {
            return Err(ServerStoreError::StaleRevision);
        }
        if plan
            .directory
            .try_exists()
            .map_err(|_| ServerStoreError::Io)?
        {
            let mut report =
                self.inspect_preparation(id, &plan.preparation_id, &plan.directory, control)?;
            if report.manifest != plan.manifest {
                return Err(ServerStoreError::Corrupt);
            }
            self.commit_preparation_check(&plan, control, || Ok(()))?;
            report.reused_existing = true;
            return Ok(report);
        }
        let list = self.deployment_preparations(id)?;
        if list.preparation_ids.len() as u64 + list.unrecognized_entries >= 1024 {
            return Err(ServerStoreError::Invalid(
                "Deployment preparation listing exceeds 1024 entries".into(),
            ));
        }
        let root = self.preparation_root(id);
        self.check_preparation_destination(id, &plan.manifest.record)?;
        super::super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        super::super::store::check_directory_chain(&root)?;
        let temporary = tempfile::Builder::new()
            .prefix(".preparation-build-")
            .tempdir_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        let result = (|| {
            let files = plan
                .manifest
                .entries
                .iter()
                .filter(|entry| entry.kind == StageEntryKind::File)
                .count() as u64;
            control.start_phase(ContentStagePhase::Copying, files, plan.manifest.total_bytes);
            let (configuration, content) = self.preparation_inputs(id, &plan.manifest.record);
            for entry in &plan.manifest.entries {
                control.checkpoint()?;
                let relative = parse_relative(&entry.path)?;
                let destination = temporary.path().join(relative);
                super::super::store::check_directory_chain(
                    destination.parent().ok_or(ServerStoreError::UnsafePath)?,
                )?;
                match entry.kind {
                    StageEntryKind::Directory => {
                        fs::create_dir(&destination).map_err(|_| ServerStoreError::Io)?
                    }
                    StageEntryKind::File => {
                        let (root, suffix) =
                            if let Some(path) = entry.path.strip_prefix("configuration/") {
                                (&configuration, path)
                            } else if let Some(path) = entry.path.strip_prefix("content/") {
                                (&content, path)
                            } else {
                                return Err(ServerStoreError::Corrupt);
                            };
                        copy_verified_file(
                            &root.join(parse_relative(suffix)?),
                            &destination,
                            entry,
                            control,
                        )?;
                    }
                }
            }
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(temporary.path().join("manifest.json"))
                .map_err(|_| ServerStoreError::Io)?;
            file.write_all(&encode(&plan.manifest)?)
                .and_then(|()| file.sync_all())
                .map_err(|_| ServerStoreError::Io)?;
            drop(file);
            let report =
                self.inspect_preparation(id, &plan.preparation_id, temporary.path(), control)?;
            if report.manifest != plan.manifest {
                return Err(ServerStoreError::Corrupt);
            }
            let confirmed = self.prepare_deployment(id, plan.expected_generation, control)?;
            if encode(&confirmed)? != encode(&plan)? {
                return Err(ServerStoreError::StaleRevision);
            }
            let published = self.commit_preparation_check(&plan, control, || {
                publish_no_replace(temporary.path(), &plan.directory)
            })?;
            if !published {
                let existing =
                    self.inspect_preparation(id, &plan.preparation_id, &plan.directory, control)?;
                if existing.manifest != plan.manifest {
                    return Err(ServerStoreError::Corrupt);
                }
                self.commit_preparation_check(&plan, control, || Ok(()))?;
            }
            Ok((report, published))
        })();
        match result {
            Ok((mut report, true)) => {
                let _ = temporary.keep();
                report.directory = plan.directory;
                report.published = true;
                report.writes_performed = true;
                Ok(report)
            }
            result => {
                temporary
                    .close()
                    .map_err(|_| ServerStoreError::PreparationCleanupIncomplete)?;
                result.map(|(mut report, _)| {
                    report.directory = plan.directory;
                    report.reused_existing = true;
                    report
                })
            }
        }
    }

    fn commit_preparation_check<T>(
        &self,
        plan: &DeploymentPreparationPlan,
        control: &ContentStageControl,
        publish: impl FnOnce() -> Result<T, ServerStoreError>,
    ) -> Result<T, ServerStoreError> {
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                self.validate_preparation_context()?;
                let id = &plan.manifest.record.instance_id;
                self.show(id)?;
                self.check_preparation_destination(id, &plan.manifest.record)?;
                if self.read_record_state(id, control)?.1.as_deref()
                    != Some(plan.selection_fingerprint.as_str())
                    || self.read_record(id, &plan.manifest.record_id, control)?
                        != plan.manifest.record
                {
                    return Err(ServerStoreError::StaleRevision);
                }
                super::super::store::check_directory_chain(&plan.directory)?;
                control.checkpoint()?;
                publish()
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    pub fn verify_deployment_preparation(
        &self,
        id: &ServerInstanceId,
        preparation_id: &str,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationReport, ServerStoreError> {
        let result = (|| {
            self.validate_preparation_context()?;
            self.show(id)?;
            self.inspect_preparation(
                id,
                preparation_id,
                &self.preparation_root(id).join(preparation_id),
                control,
            )
        })()
        .map_err(preparation_error);
        control.finish(&result);
        result
    }

    fn inspect_preparation(
        &self,
        id: &ServerInstanceId,
        preparation_id: &str,
        directory: &Path,
        control: &ContentStageControl,
    ) -> Result<DeploymentPreparationReport, ServerStoreError> {
        control.checkpoint()?;
        let manifest = self.read_preparation_manifest(id, preparation_id, directory, control)?;
        verify_tree(&manifest.entries, manifest.total_bytes, directory, control)?;
        let expected = self.preparation_manifest(
            id,
            &manifest.record_id,
            &manifest.record,
            &directory.join("configuration"),
            &directory.join("content"),
            control,
        )?;
        if expected != manifest {
            return Err(ServerStoreError::Corrupt);
        }
        if self.read_preparation_manifest(id, preparation_id, directory, control)? != manifest {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_preparation_context()?;
        control.checkpoint()?;
        Ok(DeploymentPreparationReport {
            preparation_id: preparation_id.into(),
            directory: directory.into(),
            files: manifest
                .entries
                .iter()
                .filter(|e| e.kind == StageEntryKind::File)
                .count() as u64,
            bytes: manifest.total_bytes,
            manifest,
            integrity_verified: true,
            published: false,
            reused_existing: false,
            writes_performed: false,
            deployment_ready: false,
            active_deployment_changed: false,
            selection_changed: false,
            process_started: false,
        })
    }

    pub fn deployment_preparations(
        &self,
        id: &ServerInstanceId,
    ) -> Result<DeploymentPreparationList, ServerStoreError> {
        self.validate_preparation_context()?;
        self.show(id)?;
        let root = self.preparation_root(id);
        super::super::store::check_directory_chain(&root)?;
        let directory = match fs::read_dir(&root) {
            Ok(directory) => Some(directory),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(ServerStoreError::Io),
        };
        let mut ids = vec![];
        let mut unrecognized_entries = 0;
        if let Some(directory) = directory {
            for (index, item) in directory.enumerate() {
                if index >= 1024 {
                    return Err(ServerStoreError::Invalid(
                        "Deployment preparation listing exceeds 1024 entries".into(),
                    ));
                }
                let item = item.map_err(|_| ServerStoreError::Io)?;
                if let Some(name) = item
                    .file_name()
                    .to_str()
                    .filter(|name| hash_id(name, "prepared_"))
                    && super::super::store::check_directory_chain(&item.path()).is_ok()
                {
                    ids.push(name.to_owned());
                } else {
                    unrecognized_entries += 1;
                }
            }
        }
        ids.sort();
        Ok(DeploymentPreparationList {
            preparation_ids: ids,
            unrecognized_entries,
            integrity_verified: false,
            writes_performed: false,
        })
    }
}

fn preparation_error(error: ServerStoreError) -> ServerStoreError {
    if matches!(
        error,
        ServerStoreError::Cancelled
            | ServerStoreError::ReviewCancelled
            | ServerStoreError::RecordCancelled
    ) {
        ServerStoreError::PreparationCancelled
    } else {
        error
    }
}
