use std::{fs, path::Path};

use super::*;
use crate::core::server::content_staging::{
    ContentStageManifest, StageEntryKind, StageRepositoryAddon, StageRepositorySnapshot,
    StageSource, copy_verified_file, parse_relative,
};

impl ServerStore {
    pub(super) fn acquire_content_inner(
        &self,
        plan: &ContentAcquisitionPlan,
        control: &ContentStageControl,
    ) -> Result<ContentAcquisitionOutcome, ServerStoreError> {
        let _pin = super::super::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        let current = self.plan_content_acquisition_inner(
            &plan.instance_id,
            plan.seed_stage.as_deref(),
            control,
        )?;
        control.defer_terminal();
        if current.fingerprint != plan.fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        if !current.blockers.is_empty() {
            return Err(ServerStoreError::Invalid(current.blockers.join(" ")));
        }
        let bindings = self
            .content_bindings(&plan.instance_id)?
            .document
            .ok_or(ServerStoreError::Corrupt)?;
        let seed = plan
            .seed_stage
            .as_deref()
            .map(|id| self.read_stage_manifest(&plan.instance_id, id, control))
            .transpose()?;
        let root = self.acquisition_root(&plan.instance_id);
        self.check_deployment_destination(&root, Some(&bindings))?;
        super::super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        let job = tempfile::Builder::new()
            .prefix(".acquire-")
            .tempdir_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        let mut outcome = ContentAcquisitionOutcome {
            instance_id: plan.instance_id.clone(),
            status: ContentAcquisitionStatus::Failed,
            stage: None,
            items: Vec::new(),
            message: String::new(),
            downloads_started: Some(false),
            writes_performed: true,
            scratch_database_opened: Some(false),
            client_database_opened: false,
        };
        let mut repositories = Vec::new();
        let mut sources = Vec::new();
        let operation = (|| {
            for (index, profile) in plan.profiles.iter().enumerate() {
                control.checkpoint()?;
                let config = job.path().join(format!("repo-{index}"));
                let space = config.join("games").join(self.context().space_id());
                let content = space.join("content");
                fs::create_dir_all(&content).map_err(|_| ServerStoreError::Io)?;
                let games = serde_json::json!({"active_game_space_id":self.context().space_id(),"last_opened_game_space_id":self.context().space_id(),"game_spaces":[{"id":self.context().space_id(),"game_id":self.context().game_id(),"display_name":"Acquisition","created_at":0}]});
                fs::write(
                    config.join("games.json"),
                    serde_json::to_vec(&games).map_err(|_| ServerStoreError::Corrupt)?,
                )
                .map_err(|_| ServerStoreError::Io)?;
                if let (Some(seed), Some(seed_id)) = (&seed, &plan.seed_stage) {
                    self.seed_repository(
                        &plan.instance_id,
                        seed_id,
                        seed,
                        profile,
                        &content,
                        control,
                    )?;
                }
                let request = worker::WorkerRequest {
                    schema_version: 1,
                    context_fingerprint: self.activation_context_fingerprint(),
                    game_space_id: self.context().space_id().into(),
                    game_id: self.context().game_id().into(),
                    repository_url: profile.repository_url.clone(),
                    addons: profile.addons.clone(),
                };
                let state = match helper::run(&config, &request, control, &profile.name) {
                    Ok(state) => state,
                    Err(error) => {
                        outcome.downloads_started = merge_witness(outcome.downloads_started, None);
                        outcome.scratch_database_opened =
                            merge_witness(outcome.scratch_database_opened, None);
                        outcome.items.push(ContentAcquisitionItem {
                            profile: profile.name.clone(),
                            status: if error == ServerStoreError::Cancelled {
                                ContentAcquisitionStatus::Cancelled
                            } else {
                                ContentAcquisitionStatus::Failed
                            },
                            addons: Vec::new(),
                            planned_transfer_bytes: 0,
                            full_download_bytes: 0,
                            patch_files_planned: 0,
                            message: Some(error.to_string()),
                        });
                        return Err(error);
                    }
                };
                outcome.downloads_started =
                    merge_witness(outcome.downloads_started, Some(state.downloads_started));
                outcome.scratch_database_opened =
                    merge_witness(outcome.scratch_database_opened, Some(state.database_opened));
                let status = match state.status {
                    worker::WorkerStatus::Complete => ContentAcquisitionStatus::Complete,
                    worker::WorkerStatus::Cancelled => ContentAcquisitionStatus::Cancelled,
                    _ => ContentAcquisitionStatus::Failed,
                };
                outcome.items.push(ContentAcquisitionItem {
                    profile: profile.name.clone(),
                    status,
                    addons: state
                        .addons
                        .iter()
                        .map(|addon| ContentAcquisitionAddon {
                            name: addon.name.clone(),
                            checksum: addon.checksum.clone(),
                            verified: addon.verified,
                        })
                        .collect(),
                    planned_transfer_bytes: state.planned_transfer_bytes,
                    full_download_bytes: state.full_download_bytes,
                    patch_files_planned: state.patch_files_planned,
                    message: state.message,
                });
                if status != ContentAcquisitionStatus::Complete {
                    return Err(if status == ContentAcquisitionStatus::Cancelled {
                        ServerStoreError::Cancelled
                    } else {
                        ServerStoreError::Invalid(
                            "Repository acquisition failed; the prior complete stage was retained"
                                .into(),
                        )
                    });
                }
                if state.addons.is_empty()
                    || state.addons.len() > 2048
                    || state.addons.iter().any(|addon| !addon.verified)
                {
                    return Err(ServerStoreError::Corrupt);
                }
                let mut addons = Vec::new();
                for addon in state.addons {
                    parse_relative(&addon.name)?;
                    if addon.name.contains('/') {
                        return Err(ServerStoreError::UnsafePath);
                    }
                    sources.push(StageSource {
                        profile_name: profile.name.clone(),
                        source: content.join(&addon.name),
                        destination: format!("addons/{}", addon.name),
                    });
                    addons.push(StageRepositoryAddon {
                        name: addon.name,
                        checksum: addon.checksum,
                    });
                }
                addons.sort_by(|left, right| left.name.cmp(&right.name));
                repositories.push(StageRepositorySnapshot {
                    profile_name: profile.name.clone(),
                    repository_url: profile.repository_url.clone(),
                    repository_local_path: profile.repository_local_path.clone(),
                    publication_checksum: state
                        .publication_checksum
                        .ok_or(ServerStoreError::Corrupt)?,
                    addons,
                });
            }
            control.checkpoint()?;
            outcome.stage = Some(self.publish_acquired_content_stage(
                &plan.instance_id,
                &plan.bindings_fingerprint,
                repositories,
                sources,
                control,
            )?);
            Ok(())
        })();
        let cleanup = if matches!(operation, Err(ServerStoreError::StageCleanupIncomplete)) {
            let _retained = job.keep();
            Err(ServerStoreError::StageCleanupIncomplete)
        } else {
            job.close()
                .map_err(|_| ServerStoreError::StageCleanupIncomplete)
        };
        let final_result = operation.and(cleanup);
        outcome.status = match &final_result {
            Ok(()) => ContentAcquisitionStatus::Complete,
            Err(ServerStoreError::Cancelled) => ContentAcquisitionStatus::Cancelled,
            Err(_) => ContentAcquisitionStatus::Failed,
        };
        outcome.message = match final_result {
            Ok(()) => {
                "Repository content was verified and published as an inactive retained stage."
                    .into()
            }
            Err(error) => error.to_string(),
        };
        for profile in plan.profiles.iter().skip(outcome.items.len()) {
            outcome.items.push(ContentAcquisitionItem {
                profile: profile.name.clone(),
                status: outcome.status,
                addons: Vec::new(),
                planned_transfer_bytes: 0,
                full_download_bytes: 0,
                patch_files_planned: 0,
                message: Some("Not started because acquisition did not complete.".into()),
            });
        }
        Ok(outcome)
    }

    fn seed_repository(
        &self,
        id: &ServerInstanceId,
        seed_id: &str,
        seed: &ContentStageManifest,
        profile: &ContentAcquisitionProfile,
        content: &Path,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        if !seed.bindings.bindings.iter().any(|item| {
            item.profile.name == profile.name
                && item.profile.remote_url == profile.repository_url
                && item.profile.local_path == profile.repository_local_path
        }) {
            return Ok(());
        }
        let root = self.stage_root(id).join(seed_id);
        for source in seed
            .sources
            .iter()
            .filter(|source| source.profile_name == profile.name)
        {
            let name = source
                .destination
                .strip_prefix("addons/")
                .ok_or(ServerStoreError::Corrupt)?;
            if !profile.addons.is_empty() && !profile.addons.iter().any(|addon| addon == name) {
                continue;
            }
            parse_relative(name)?;
            let prefix = format!("{}/", source.destination);
            for entry in seed
                .entries
                .iter()
                .filter(|entry| entry.path == source.destination || entry.path.starts_with(&prefix))
            {
                control.checkpoint()?;
                let relative = entry
                    .path
                    .strip_prefix("addons/")
                    .ok_or(ServerStoreError::Corrupt)?;
                let destination = content.join(parse_relative(relative)?);
                match entry.kind {
                    StageEntryKind::Directory => {
                        fs::create_dir_all(&destination).map_err(|_| ServerStoreError::Io)?
                    }
                    StageEntryKind::File => copy_verified_file(
                        &root.join(parse_relative(&entry.path)?),
                        &destination,
                        entry,
                        control,
                    )?,
                }
            }
        }
        Ok(())
    }
}

fn merge_witness(previous: Option<bool>, current: Option<bool>) -> Option<bool> {
    match (previous, current) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::merge_witness;
    #[test]
    fn unknown_effects_never_erase_a_positive_witness_or_claim_absence() {
        assert_eq!(merge_witness(Some(true), None), Some(true));
        assert_eq!(merge_witness(Some(false), None), None);
        assert_eq!(merge_witness(None, Some(false)), None);
        assert_eq!(merge_witness(Some(false), Some(false)), Some(false));
    }
}
