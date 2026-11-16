mod selection;
mod store;
mod validation;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    ContentBindingsDocument, DeploymentCompositionPlan, DeploymentCompositionRequest,
    ServerContext, ServerInstanceId,
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORDS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentRecordManifest {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub instance_revision: u64,
    pub catalog_revision: u64,
    pub catalog_fingerprint: String,
    pub configuration_revision: String,
    pub configuration_manifest_fingerprint: String,
    pub configuration_fingerprint: String,
    pub content_stage_id: String,
    pub content_manifest_fingerprint: String,
    pub content_bindings_fingerprint: String,
    pub content_bindings: ContentBindingsDocument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentRecordState {
    pub schema_version: u32,
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub selected: Option<String>,
    pub records: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentRecordPlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    request: DeploymentCompositionRequest,
    #[serde(skip)]
    before: Option<String>,
    #[serde(skip)]
    proof: String,
    pub review: DeploymentCompositionPlan,
    pub manifest: DeploymentRecordManifest,
    pub record_id: String,
    pub fingerprint: String,
    pub expected_generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentRecordVerification {
    pub record_id: String,
    pub manifest: DeploymentRecordManifest,
    pub integrity_verified: bool,
    pub configuration_verified: bool,
    pub content_verified: bool,
    pub deployment_ready: bool,
    pub writes_performed: bool,
    pub active_deployment_changed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentRecordCommit {
    pub state: DeploymentRecordState,
    pub record: DeploymentRecordVerification,
    pub published: bool,
    pub selection_changed: bool,
    pub active_deployment_changed: bool,
    pub process_started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentRecordSelectionPlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    proof: String,
    pub instance_id: ServerInstanceId,
    pub expected_generation: u64,
    pub selection_fingerprint: String,
    pub inventory_fingerprint: String,
    pub previous_record_id: Option<String>,
    pub target: DeploymentRecordVerification,
    pub fingerprint: String,
}

impl DeploymentRecordPlan {
    pub fn directory(&self) -> PathBuf {
        self.context
            .space_dir()
            .join("server/instances")
            .join(self.manifest.instance_id.as_str())
            .join("deployments/records")
            .join(&self.record_id)
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests;
