pub(in crate::core::server) mod launch;
mod profile;
mod reforger;
pub(in crate::core::server) use launch::{NativeRconPlan, NativeRuntimeLaunchPlan};
pub use profile::NativeProfileSeed;
mod runtime;
mod store;
mod verification;

use serde::{Deserialize, Serialize};

use super::runtime_payload::PayloadInventory;
use super::{ServerInstallation, ServerInstanceId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRuntimeReference {
    pub runtime_id: String,
    pub manifest_fingerprint: String,
}

impl NativeRuntimeReference {
    pub(in crate::core::server) fn directory_name(&self) -> String {
        format!(
            "runtime_{}",
            &blake3::hash(self.runtime_id.as_bytes()).to_hex()[..32]
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRuntimeManifest {
    pub schema_version: u32,
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub preparation_id: String,
    pub installation: ServerInstallation,
    pub payload: PayloadInventory,
    pub realized_entries: Vec<super::content_staging::StageEntry>,
    pub file_sources: std::collections::BTreeMap<String, String>,
    pub mission_templates: Vec<String>,
    pub server_mods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_mods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_trust: Option<Box<super::ServerKeyTrust>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_bindings: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reforger_addons: Option<super::ReforgerAddonLock>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headless_clients: Vec<super::NativeHeadlessClient>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentActivationPlan {
    #[serde(skip)]
    context: super::ServerContext,
    pub instance_id: ServerInstanceId,
    pub preparation_id: String,
    pub expected_selection_generation: u64,
    pub expected_activation_generation: u64,
    pub catalog_fingerprint: String,
    pub selection_fingerprint: String,
    pub installation: ServerInstallation,
    pub native_runtime: NativeRuntimeManifest,
    pub runtime: NativeRuntimeReference,
    pub runtime_directory: std::path::PathBuf,
    pub fingerprint: String,
    pub writes_performed: bool,
    pub process_started: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentOperationOutcome {
    pub instance_id: ServerInstanceId,
    pub status: String,
    pub activation_generation: Option<u64>,
    pub runtime_id: Option<String>,
    pub runtime_verified: bool,
    pub journal_pending: bool,
    pub intent_pending: bool,
    pub writes_performed: bool,
    pub process_started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentActivationStatus {
    pub instance_id: ServerInstanceId,
    pub activation_generation: u64,
    pub runtime: Option<NativeRuntimeReference>,
    pub runtime_directory: Option<std::path::PathBuf>,
    pub preparation_id: Option<String>,
    pub integrity_verified: bool,
    pub recovery_required: bool,
    pub writes_performed: bool,
    pub process_started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentResolutionReview {
    pub instance_id: ServerInstanceId,
    pub fingerprint: String,
    pub expected_outcome: String,
    pub activation_generation: u64,
    pub runtime_id: Option<String>,
    pub journal_present: bool,
    pub writes_performed: bool,
    pub process_started: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_runtime_example_is_strict_reference_data_without_compatibility() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/native_runtime_manifest.json"
        ))
        .unwrap();
        let manifest: NativeRuntimeManifest = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&manifest).unwrap(), value);
        assert!(
            super::super::engine_layout::observe_tested_layout(&manifest.installation).is_none()
        );
        let mut invalid = value;
        invalid["payload"]["authority"] = true.into();
        assert!(serde_json::from_value::<NativeRuntimeManifest>(invalid).is_err());
    }

    #[test]
    fn runtime_directory_names_are_bounded_and_keep_untrusted_data_out_of_paths() {
        let reference = |id: &str| NativeRuntimeReference {
            runtime_id: id.into(),
            manifest_fingerprint: "a".repeat(64),
        };
        let name = reference("runtime_alpha").directory_name();
        assert_eq!(name.len(), 40);
        assert_eq!(name, reference("runtime_alpha").directory_name());
        assert_ne!(name, reference("runtime_beta").directory_name());
        assert!(
            !reference("../outside")
                .directory_name()
                .contains(['/', '\\', '.'])
        );
    }
}
