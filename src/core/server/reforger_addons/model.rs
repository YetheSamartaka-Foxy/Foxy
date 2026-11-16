use serde::{Deserialize, Serialize};

use super::super::ReforgerAddonRequirement;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReforgerAddonChannel {
    Stable,
    Experimental,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReforgerAddonProvider {
    Local,
    BohemiaWorkshop,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerAddonMetadata {
    pub addon_id: String,
    pub version: String,
    pub channel: ReforgerAddonChannel,
    pub provider: ReforgerAddonProvider,
    #[serde(default)]
    pub dependencies: Vec<ReforgerAddonDependency>,
    #[serde(default)]
    pub scenarios: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerAddonDependency {
    pub addon_id: String,
    pub version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerAddonLock {
    pub channel: ReforgerAddonChannel,
    pub requested: Vec<ReforgerAddonRequirement>,
    pub mods_required_by_default: Option<bool>,
    pub scenario_id: String,
    pub scenario_provider: Option<String>,
    pub realized: Vec<ReforgerAddonMetadata>,
    pub omissions: Vec<ReforgerAddonOmission>,
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub content_sources: std::collections::BTreeMap<String, std::path::PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerAddonOmission {
    pub addon_id: String,
    pub reason: String,
}
