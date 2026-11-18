mod battleye;
mod mapping;
mod packs;
pub(super) mod parser;
mod review;
mod runtime;
mod secrets;
pub use battleye::NativeBattlEyeSeed;
pub(crate) use battleye::{BattlEyeRcon, battleye_names};
pub use runtime::NativeConfigurationSeed;
pub(in crate::core::server) use runtime::private_configuration_name;

use super::{ServerInstance, ServerInstanceId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use packs::{InstancePackImportPlan, PortableInstancePack};
pub use review::ConfigurationImportPlan;
pub(in crate::core::server) use review::render_imported;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationRole {
    Server,
    Basic,
    Profile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationOwnership {
    Managed,
    Operator,
}

#[derive(Clone, Debug)]
pub struct ConfigurationImportInput {
    pub role: ConfigurationRole,
    pub ownership: ConfigurationOwnership,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedConfigurationFile {
    pub role: ConfigurationRole,
    pub ownership: ConfigurationOwnership,
    pub template: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedConfiguration {
    pub files: Vec<ImportedConfigurationFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_paths: Vec<String>,
    #[serde(default)]
    pub paths_reviewed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_references: Vec<super::content_bindings::ReviewedContentBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pack_omissions: Vec<String>,
}

impl ImportedConfiguration {
    pub(crate) fn validate(&self) -> Result<(), String> {
        review::validate_imported(self)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigurationImportReport {
    pub instance_id: ServerInstanceId,
    pub expected_revision: u64,
    pub fingerprint: String,
    pub instance: ServerInstance,
    pub warnings: Vec<String>,
    pub unresolved_paths: Vec<String>,
    pub secret_references: Vec<String>,
    pub diff: Vec<String>,
    pub writes_performed: bool,
}

#[cfg(test)]
mod tests;
