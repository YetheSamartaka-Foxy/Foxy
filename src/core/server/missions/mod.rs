mod acquisition;
mod metadata;
mod payload;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{ContentStageControl, ServerContext, ServerStoreError};
use payload::MissionPayload;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Arma3MissionSource {
    Library {
        mission_id: String,
        revision_id: String,
    },
    AddonClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MissionOrigin {
    Local {
        path: PathBuf,
    },
    Http {
        url: String,
        template: String,
        expected_blake3: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionLibraryEntry {
    pub schema_version: u32,
    pub game_space_id: String,
    pub mission_id: String,
    pub revision_id: String,
    pub origin: MissionOrigin,
    pub payload: MissionPayload,
}

#[derive(Clone, Debug, Serialize)]
pub struct MissionLibrarySnapshot {
    pub entries: Vec<MissionLibraryEntry>,
    pub fingerprint: String,
    pub unrecognized_entries: u64,
    pub writes_performed: bool,
}

#[derive(Debug, Serialize)]
pub struct MissionImportPlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    source: PathBuf,
    #[serde(skip)]
    before: Option<String>,
    #[serde(skip)]
    temporary: Option<Arc<tempfile::TempDir>>,
    #[serde(skip)]
    proof: String,
    pub entry: MissionLibraryEntry,
    pub fingerprint: String,
    pub writes_performed: bool,
}

pub(super) fn valid_id(value: &str, prefix: &str, digits: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == digits
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub(super) fn valid_template(value: &str) -> bool {
    value.len() <= 256
        && !value.contains("..")
        && value
            .rsplit_once('.')
            .is_some_and(|(name, world)| !name.is_empty() && !world.is_empty())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !value.to_ascii_lowercase().ends_with(".sqf")
}

impl Arma3MissionSource {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Library {
                mission_id,
                revision_id,
            } if !valid_id(mission_id, "mission_", 32)
                || !valid_id(revision_id, "revision_", 64) =>
            {
                Err("Invalid mission library reference".into())
            }
            _ => Ok(()),
        }
    }
}

pub(crate) fn validate_acquisition_url(url: &str) -> Result<(), ServerStoreError> {
    acquisition::validate_url(url).map(drop)
}

pub(super) fn cycle_library_sources(
    definition: &super::ServerDefinition,
) -> Vec<Arma3MissionSource> {
    let mut sources = Vec::new();
    if let super::ServerDefinition::Arma3(arma) = definition {
        for mission in arma.mission_cycle.iter().filter(|mission| mission.enabled) {
            if let Some(source @ Arma3MissionSource::Library { .. }) = &mission.source
                && !sources.contains(source)
            {
                sources.push(source.clone());
            }
        }
    }
    sources
}

#[cfg(test)]
mod payload_tests;
#[cfg(test)]
mod tests;
