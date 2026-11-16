mod manifest;
mod store;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::ServerContext;
use super::content_staging::StageEntry;
use super::deployment_records::DeploymentRecordManifest;

const MAX_BYTES: usize = 48 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentPreparationManifest {
    pub schema_version: u32,
    pub layout_version: u32,
    pub record_id: String,
    pub record: DeploymentRecordManifest,
    pub entries: Vec<StageEntry>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentPreparationPlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    proof: String,
    pub manifest: DeploymentPreparationManifest,
    pub preparation_id: String,
    pub fingerprint: String,
    pub expected_generation: u64,
    pub selection_fingerprint: String,
    pub inventory_fingerprint: String,
    pub directory: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentPreparationReport {
    pub preparation_id: String,
    pub manifest: DeploymentPreparationManifest,
    pub directory: PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub integrity_verified: bool,
    pub published: bool,
    pub reused_existing: bool,
    pub writes_performed: bool,
    pub deployment_ready: bool,
    pub active_deployment_changed: bool,
    pub selection_changed: bool,
    pub process_started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentPreparationList {
    pub preparation_ids: Vec<String>,
    pub unrecognized_entries: u64,
    pub integrity_verified: bool,
    pub writes_performed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_example_preserves_the_strict_versioned_shape() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/deployment_preparation_manifest.json"
        ))
        .unwrap();
        let manifest: DeploymentPreparationManifest =
            serde_json::from_value(value.clone()).unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.layout_version, 1);
        assert_eq!(serde_json::to_value(manifest).unwrap(), value);
        let mut future_field = value;
        future_field["unexpected"] = true.into();
        assert!(serde_json::from_value::<DeploymentPreparationManifest>(future_field).is_err());
    }
}
