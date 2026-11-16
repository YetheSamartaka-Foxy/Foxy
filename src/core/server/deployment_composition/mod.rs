mod artifacts;
mod store;
pub(super) use artifacts::inspect as inspect_artifacts;

use serde::Serialize;

use super::{
    ConfigurationPreview, ContentBindingsDocument, ContentBindingsSnapshot, ContentStageReport,
    InstallationConsumerInspection, InstallationId, RuntimePrerequisites, ServerInstanceId,
};

#[derive(Clone, Debug)]
pub struct DeploymentCompositionRequest {
    pub instance_id: ServerInstanceId,
    pub stage_id: Option<String>,
    pub expected_revision: Option<u64>,
    pub expected_configuration_generation: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompositionIssue {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompositionConfiguration {
    pub status: &'static str,
    pub generation: u64,
    pub selection_fingerprint: Option<String>,
    pub revision_id: Option<String>,
    pub manifest_fingerprint: Option<String>,
    pub integrity_verified: bool,
    pub matches_current_draft: Option<bool>,
    pub preview: Option<ConfigurationPreview>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompositionContent {
    pub status: &'static str,
    pub requested_stage_id: Option<String>,
    pub integrity_verified: bool,
    pub stage: Option<ContentStageReport>,
    pub reviewed_bindings: Option<ContentBindingsDocument>,
    pub matches_saved_bindings: Option<bool>,
    pub current_bindings: ContentBindingsSnapshot,
    pub current_sources_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentCompositionPlan {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub instance_revision: u64,
    pub catalog_revision: u64,
    pub catalog_fingerprint: Option<String>,
    pub configuration: CompositionConfiguration,
    pub content: CompositionContent,
    pub artifacts: artifacts::ArtifactEvidence,
    pub installation_id: Option<InstallationId>,
    pub installation_verified: bool,
    pub installation_consumers: Option<InstallationConsumerInspection>,
    pub runtime_prerequisites: RuntimePrerequisites,
    pub blockers: Vec<CompositionIssue>,
    pub fingerprint: String,
    pub deployment_ready: bool,
    pub writes_performed: bool,
    pub active_deployment_changed: bool,
    pub downloads_started: bool,
    pub installation_leases_acquired: bool,
    pub process_started: bool,
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests;
