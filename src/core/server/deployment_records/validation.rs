use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use super::super::{
    ContentBindingsDocument, ContentStageControl, DeploymentCompositionPlan, ServerContext,
    ServerInstanceId, ServerStore, ServerStoreError,
};
use super::{DeploymentRecordManifest, DeploymentRecordState, MAX_BYTES, MAX_RECORDS};

pub(super) fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

pub(super) fn record_id(value: &str) -> bool {
    value.strip_prefix("deployment_").is_some_and(hash)
}

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ServerStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_BYTES {
        return Err(ServerStoreError::Invalid(
            "Deployment record document exceeds 8 MiB".into(),
        ));
    }
    Ok(bytes)
}

pub(super) fn manifest_from(
    review: &DeploymentCompositionPlan,
) -> Result<DeploymentRecordManifest, ServerStoreError> {
    let runtime_only = |code: &str| {
        matches!(
            code,
            "key_trust_unverified"
                | "installation_missing"
                | "arma_layout_unverified"
                | "reforger_layout_unverified"
                | "installation_not_verified"
                | "installation_leases_not_acquired"
                | "owner_recovery_unavailable"
                | "runtime_activation_unavailable"
                | "consumer_inspection_incomplete"
                | "consumer_cleanup_required"
                | "reforger_resources_unverified"
        )
    };
    if !review.configuration.integrity_verified
        || review.configuration.matches_current_draft != Some(true)
        || !review.content.integrity_verified
        || review.content.matches_saved_bindings != Some(true)
        || !review.content.current_bindings.review_current
        || review.artifacts.truncated
        || review
            .blockers
            .iter()
            .any(|issue| !runtime_only(issue.code))
    {
        return Err(ServerStoreError::Invalid("Deployment record requires complete verified archives and current reviewed intent without key or mission conflicts. Review the composition blockers.".into()));
    }
    let required = |value: &Option<String>| value.clone().ok_or(ServerStoreError::Corrupt);
    let stage = review
        .content
        .stage
        .as_ref()
        .ok_or(ServerStoreError::Corrupt)?;
    Ok(DeploymentRecordManifest {
        schema_version: 1,
        game_space_id: review.game_space_id.clone(),
        game_id: review.game_id.clone(),
        instance_id: review.instance_id.clone(),
        instance_revision: review.instance_revision,
        catalog_revision: review.catalog_revision,
        catalog_fingerprint: required(&review.catalog_fingerprint)?,
        configuration_revision: required(&review.configuration.revision_id)?,
        configuration_manifest_fingerprint: required(&review.configuration.manifest_fingerprint)?,
        configuration_fingerprint: review
            .configuration
            .preview
            .as_ref()
            .ok_or(ServerStoreError::Corrupt)?
            .fingerprint
            .clone(),
        content_stage_id: stage.stage_id.clone(),
        content_manifest_fingerprint: stage.fingerprint.clone(),
        content_bindings_fingerprint: required(&review.content.current_bindings.fingerprint)?,
        content_bindings: review
            .content
            .reviewed_bindings
            .clone()
            .ok_or(ServerStoreError::Corrupt)?,
    })
}

impl ServerStore {
    pub(in crate::core::server) fn record_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("deployments")
    }

    pub(in crate::core::server) fn validate_record_context(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().deployment_records {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let current =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &current != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }

    pub(super) fn check_record_destination(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(), ServerStoreError> {
        let bindings = self.content_bindings(id)?;
        self.check_record_destination_for_bindings(id, bindings.document.as_ref())
    }

    pub(super) fn check_record_destination_for_bindings(
        &self,
        id: &ServerInstanceId,
        bindings: Option<&ContentBindingsDocument>,
    ) -> Result<(), ServerStoreError> {
        self.check_deployment_destination(&self.record_root(id), bindings)
    }

    pub(in crate::core::server) fn check_deployment_destination(
        &self,
        destination: &Path,
        bindings: Option<&ContentBindingsDocument>,
    ) -> Result<(), ServerStoreError> {
        let root = super::super::content_staging::physical_directory(destination)?;
        for installation in self.load()?.installations {
            let path = super::super::content_staging::physical_directory(&installation.directory)?;
            if super::super::installation::paths_overlap(&root, &path) {
                return Err(ServerStoreError::UnsafePath);
            }
        }
        if let Some(document) = bindings {
            for binding in &document.bindings {
                let mut inputs = vec![binding.profile.local_path.clone()];
                if binding.profile.addons.is_empty() {
                    inputs.push(binding.folder.path.clone());
                } else {
                    inputs.extend(
                        binding
                            .profile
                            .addons
                            .iter()
                            .map(|name| binding.folder.path.join(name)),
                    );
                }
                for input in inputs {
                    let path = super::super::content_staging::physical_directory(&input)?;
                    if super::super::installation::paths_overlap(&root, &path) {
                        return Err(ServerStoreError::UnsafePath);
                    }
                }
            }
        }
        Ok(())
    }

    pub(in crate::core::server) fn validate_record_manifest(
        &self,
        id: &ServerInstanceId,
        manifest: &DeploymentRecordManifest,
    ) -> Result<(), ServerStoreError> {
        if manifest.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        self.validate_content_bindings(id, &manifest.content_bindings)?;
        if manifest.game_space_id != self.context().space_id()
            || manifest.game_id != self.context().game_id()
            || &manifest.instance_id != id
            || manifest.instance_revision == 0
            || manifest.catalog_revision == 0
            || [
                &manifest.catalog_fingerprint,
                &manifest.configuration_manifest_fingerprint,
                &manifest.configuration_fingerprint,
                &manifest.content_manifest_fingerprint,
                &manifest.content_bindings_fingerprint,
            ]
            .iter()
            .any(|value| !hash(value))
            || manifest.configuration_revision
                != format!("config_{}", manifest.configuration_fingerprint)
            || manifest.content_stage_id
                != format!("stage_{}", manifest.content_manifest_fingerprint)
            || manifest.content_bindings.instance_revision != manifest.instance_revision
            || manifest.content_bindings.catalog_revision != manifest.catalog_revision
            || manifest.content_bindings.catalog_fingerprint != manifest.catalog_fingerprint
        {
            return Err(ServerStoreError::Corrupt);
        }
        encode(manifest)?;
        Ok(())
    }

    pub(super) fn validate_record_state(
        &self,
        id: &ServerInstanceId,
        state: &DeploymentRecordState,
    ) -> Result<(), ServerStoreError> {
        if state.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let unique: HashSet<_> = state.records.iter().collect();
        if state.game_space_id != self.context().space_id()
            || &state.instance_id != id
            || state.records.len() > MAX_RECORDS
            || unique.len() != state.records.len()
            || state.records.iter().any(|id| !record_id(id))
            || state
                .selected
                .as_ref()
                .is_some_and(|id| !unique.contains(id))
            || state.selected.is_none() != state.records.is_empty()
            || state.generation < state.records.len() as u64
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(())
    }

    pub(super) fn check_record_inputs(
        &self,
        review: &DeploymentCompositionPlan,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        self.validate_record_context()?;
        if self.read()?.1.map(|hash| hash.to_hex().to_string()) != review.catalog_fingerprint
            || self
                .read_configuration_state_controlled(&review.instance_id, control)?
                .1
                .map(|hash| hash.to_hex().to_string())
                != review.configuration.selection_fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let bindings = self.content_bindings(&review.instance_id)?;
        if bindings.document != review.content.current_bindings.document
            || bindings.fingerprint != review.content.current_bindings.fingerprint
            || !bindings.review_current
            || self.content_sources()?.fingerprint != review.content.current_sources_fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }
}
