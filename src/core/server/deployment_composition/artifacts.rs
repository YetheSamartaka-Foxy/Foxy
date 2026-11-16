use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use super::super::ServerDefinition;
use crate::core::server::content_staging::{StageEntry, StageEntryKind};

#[cfg(test)]
mod tests;

const MAX_REFERENCES: usize = 2048;
const MAX_STRINGS: usize = 1024 * 1024;
const MAX_TEMPLATES: usize = 100_000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ArtifactEvidence {
    pub keys: Vec<KeyCandidates>,
    pub missions: Vec<MissionCandidates>,
    pub key_files: u64,
    pub mission_files: u64,
    pub truncated: bool,
    pub issues: Vec<ArtifactIssue>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactIssue {
    pub code: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactFile {
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct KeyCandidates {
    pub name: String,
    pub state: &'static str,
    pub files: Vec<ArtifactFile>,
    pub flattened_collision: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct MissionCandidates {
    pub template: String,
    pub state: &'static str,
    pub files: Vec<ArtifactFile>,
    pub missing_required: bool,
    pub conflicting_bytes: bool,
}

#[derive(Default)]
struct CandidateGroup {
    display_name: String,
    files: Vec<ArtifactFile>,
    count: u64,
    first_bytes: Option<u64>,
    first_hash: Option<String>,
    conflicting: bool,
}

impl CandidateGroup {
    fn observe(
        &mut self,
        entry: &StageEntry,
        refs: &mut usize,
        strings: &mut usize,
        truncated: &mut bool,
    ) {
        self.count += 1;
        if let Some(first) = self.first_bytes {
            if first != entry.bytes
                || self
                    .first_hash
                    .as_deref()
                    .zip(entry.blake3.as_deref())
                    .is_some_and(|(a, b)| a != b)
            {
                self.conflicting = true;
            }
        } else {
            self.first_bytes = Some(entry.bytes);
            self.first_hash = entry.blake3.clone();
        }
        let needed = entry.path.len() + entry.blake3.as_ref().map_or(0, String::len);
        if *refs >= MAX_REFERENCES || strings.saturating_add(needed) > MAX_STRINGS {
            *truncated = true;
            return;
        }
        *refs += 1;
        *strings += needed;
        self.files.push(ArtifactFile {
            path: entry.path.clone(),
            bytes: entry.bytes,
            blake3: entry.blake3.clone().unwrap_or_default(),
        });
    }
}

fn path_parts(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn key_file(entry: &StageEntry) -> bool {
    entry.kind == StageEntryKind::File
        && basename(&entry.path)
            .to_ascii_lowercase()
            .ends_with(".bikey")
}

fn mission_file(entry: &StageEntry) -> Option<&str> {
    if entry.kind != StageEntryKind::File
        || !path_parts(&entry.path)
            .take(path_parts(&entry.path).count().saturating_sub(1))
            .any(|part| part.eq_ignore_ascii_case("mpmissions"))
    {
        return None;
    }
    let name = basename(&entry.path);
    let (stem, extension) = name.rsplit_once('.')?;
    extension.eq_ignore_ascii_case("pbo").then_some(stem)
}

fn push_issue(issues: &mut Vec<ArtifactIssue>, code: &'static str, message: &'static str) {
    if !issues.iter().any(|issue| issue.code == code) {
        issues.push(ArtifactIssue { code, message });
    }
}

pub(in crate::core::server) fn inspect(
    definition: &ServerDefinition,
    entries: &[StageEntry],
) -> ArtifactEvidence {
    let mut evidence = ArtifactEvidence {
        keys: Vec::new(),
        missions: Vec::new(),
        key_files: 0,
        mission_files: 0,
        truncated: false,
        issues: Vec::new(),
    };
    let ServerDefinition::Arma3(arma) = definition else {
        return evidence;
    };

    let mut key_groups: BTreeMap<String, CandidateGroup> = BTreeMap::new();
    let mut mission_groups: BTreeMap<String, CandidateGroup> = BTreeMap::new();
    let mut refs = 0usize;
    let mut string_bytes = 0usize;
    let mut mission_templates = BTreeMap::new();
    let mut template_names = HashSet::new();
    for mission in arma.mission_cycle.iter().filter(|mission| mission.enabled) {
        if matches!(
            mission.source,
            Some(super::super::Arma3MissionSource::AddonClass)
        ) {
            push_issue(
                &mut evidence.issues,
                "addon_mission_class_unverified",
                "Addon mission class identity is explicit; availability cannot be inferred from a filename PBO. Supply verified class metadata before activation.",
            );
            evidence.missions.push(MissionCandidates {
                template: mission.template.clone(),
                state: "unverified_addon_class",
                files: vec![],
                missing_required: true,
                conflicting_bytes: false,
            });
            continue;
        }
        let normalized = mission.template.to_uppercase();
        if template_names.contains(&normalized) {
            continue;
        }
        if template_names.len() >= MAX_TEMPLATES {
            evidence.truncated = true;
            break;
        }
        template_names.insert(normalized.clone());
        if string_bytes.saturating_add(mission.template.len()) > MAX_STRINGS {
            evidence.truncated = true;
            continue;
        }
        string_bytes += mission.template.len();
        mission_templates.insert(normalized, mission.template.clone());
    }
    for entry in entries {
        if key_file(entry) {
            evidence.key_files += 1;
            let name = basename(&entry.path);
            let key = name.to_uppercase();
            let group = key_groups.entry(key).or_default();
            if group.display_name.is_empty() {
                group.display_name = name.to_string();
            }
            group.observe(entry, &mut refs, &mut string_bytes, &mut evidence.truncated);
        }
        if let Some(stem) = mission_file(entry) {
            let key = stem.to_uppercase();
            if !mission_templates.contains_key(&key) {
                continue;
            }
            evidence.mission_files += 1;
            let group = mission_groups.entry(key).or_default();
            if group.display_name.is_empty() {
                group.display_name = stem.to_string();
            }
            group.observe(entry, &mut refs, &mut string_bytes, &mut evidence.truncated);
        }
    }

    for (_, group) in key_groups {
        if group.conflicting {
            push_issue(
                &mut evidence.issues,
                "flattened_key_collision",
                "Key files with the same case-insensitive basename have different recorded bytes.",
            );
        }
        if group.files.is_empty()
            || string_bytes.saturating_add(group.display_name.len()) > MAX_STRINGS
        {
            evidence.truncated = true;
            continue;
        }
        string_bytes += group.display_name.len();
        evidence.keys.push(KeyCandidates {
            name: group.display_name,
            state: if group.conflicting {
                "conflict"
            } else {
                "candidate"
            },
            files: group.files,
            flattened_collision: group.conflicting,
        });
    }

    for (normalized, template) in mission_templates {
        let group = mission_groups.remove(&normalized).unwrap_or_default();
        let transfer_limit = if arma.server.mission_http_download_base_url.is_some() {
            512u64
        } else {
            300u64
        } * 1024
            * 1024;
        if group.files.iter().any(|file| file.bytes > transfer_limit) {
            push_issue(
                &mut evidence.issues,
                "mission_transfer_limit",
                "A mission exceeds its configured engine transfer path limit. Review missionHTTPDownloadBaseURL and hosting before deployment.",
            );
        }
        let missing = group.count == 0;
        let files = group.files;
        let conflict = group.conflicting;
        if missing {
            push_issue(
                &mut evidence.issues,
                "mission_missing",
                "A configured mission template has no matching staged PBO candidate under mpmissions.",
            );
        }
        if conflict {
            push_issue(
                &mut evidence.issues,
                "mission_conflicting_bytes",
                "Matching mission PBO candidates have different recorded bytes.",
            );
        }
        let state = if missing {
            "missing"
        } else if conflict {
            "conflict"
        } else {
            "candidate"
        };
        evidence.missions.push(MissionCandidates {
            template,
            state,
            files,
            missing_required: missing,
            conflicting_bytes: conflict,
        });
    }

    if evidence.truncated {
        push_issue(
            &mut evidence.issues,
            "artifact_diagnostics_truncated",
            "Artifact diagnostics exceeded the bounded candidate output; composition remains incomplete.",
        );
    }
    evidence
}
