mod acquisition;
mod control;
mod store;
mod tree;
mod validation;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ContentBindingsDocument, ServerContext, ServerInstanceId};

pub(crate) use control::ContentStageProgress;
pub use control::{ContentStageControl, ContentStagePhase};
pub(super) use store::publish_no_replace;
pub(super) use store::read_bounded_controlled;
pub(super) use tree::verify_stage as verify_stage_tree;
pub(in crate::core::server) use tree::{copy_sources, inspect_sources};
pub(super) use tree::{
    copy_verified_file, parse_relative, validate_generic_entries, verify_subtree, verify_tree,
};
pub(super) use validation::physical_directory;

pub(super) const SCHEMA_VERSION: u32 = 1;
pub(super) const MAX_ENTRIES: usize = 100_000;
pub(super) const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MANIFEST_FILE: &str = "manifest.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageEntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageEntry {
    pub path: String,
    pub kind: StageEntryKind,
    pub bytes: u64,
    pub blake3: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageSource {
    pub profile_name: String,
    pub source: PathBuf,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentStageManifest {
    pub schema_version: u32,
    pub bindings_fingerprint: String,
    pub bindings: ContentBindingsDocument,
    pub sources: Vec<StageSource>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acquisitions: Vec<StageRepositorySnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missions: Vec<super::Arma3MissionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reforger_addons: Option<super::reforger_addons::ReforgerAddonLock>,
    pub entries: Vec<StageEntry>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageRepositorySnapshot {
    pub profile_name: String,
    pub repository_url: String,
    pub repository_local_path: PathBuf,
    pub publication_checksum: String,
    pub addons: Vec<StageRepositoryAddon>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageRepositoryAddon {
    pub name: String,
    pub checksum: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentStagePlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    instance_id: ServerInstanceId,
    #[serde(skip)]
    proof: String,
    pub manifest: ContentStageManifest,
    pub fingerprint: String,
    pub stage_id: String,
    pub directory: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentStageReport {
    pub stage_id: String,
    pub directory: PathBuf,
    pub fingerprint: String,
    pub files: u64,
    pub bytes: u64,
    pub verified: bool,
    pub published: bool,
    pub reused_existing: bool,
    pub deployment_ready: bool,
    pub active_deployment_changed: bool,
    pub downloads_started: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentStageList {
    pub stage_ids: Vec<String>,
    pub unrecognized_entries: u64,
    pub integrity_verified: bool,
    pub writes_performed: bool,
}

#[cfg(test)]
mod tests;
