//! Supply choices never change client addon enablement or contact the network; repository identity remains (URL, local path).

#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ServerStore, ServerStoreError};

pub const CONTENT_SOURCES_FILE: &str = "server/content-sources.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_SOURCE_RECORDS: usize = 1024;
const MAX_PROFILE_ADDONS: usize = 4096;
const MAX_FOLDER_ENTRIES: usize = 65_536;

#[derive(Clone, Debug, Serialize)]
pub struct ContentSourceSnapshot {
    pub sources: ContentSources,
    pub fingerprint: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonSupply {
    Download,

    AlreadyPresent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddonFolder {
    pub id: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryProfile {
    pub name: String,
    pub remote_url: String,
    pub local_path: PathBuf,
    pub folder_id: String,
    pub supply: AddonSupply,

    pub addons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentSources {
    pub schema_version: u32,
    pub folders: Vec<AddonFolder>,
    pub profiles: Vec<RepositoryProfile>,
}

impl Default for ContentSources {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            folders: Vec::new(),
            profiles: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub download: bool,
    pub root: PathBuf,
    pub addon_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceError {
    Identity,
    DuplicateFolder,
    DuplicateProfile,
    DuplicateName,
    UnknownFolder,
    DownloadOwnCopy,
    Escape,
    InvalidUrl,
    Limit,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Identity => "Content profiles require valid names and absolute source paths.",
            Self::DuplicateFolder => "A content folder with this ID is already registered.",
            Self::DuplicateProfile => "This repository URL and local path are already registered.",
            Self::DuplicateName => "Content profile names must be unique.",
            Self::UnknownFolder => "The content profile refers to an unknown folder.",
            Self::DownloadOwnCopy => "A repository already in the selected folder cannot be downloaded into itself.",
            Self::Escape => "Addon names must be safe single directory names without duplicates.",
            Self::InvalidUrl => "Use an HTTP or HTTPS repository URL without credentials, query or fragment. An empty URL is allowed only for already-present local content.",
            Self::Limit => "The content source registry exceeds its entry limit.",
        })
    }
}

impl ContentSources {
    pub fn addon_candidates(path: &Path) -> Result<Vec<String>, String> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| "The selected content folder could not be inspected.".to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Choose an ordinary local folder without a symbolic link.".into());
        }
        let entries = std::fs::read_dir(path)
            .map_err(|_| "The selected content folder could not be read.".to_string())?;
        let mut addons = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= MAX_FOLDER_ENTRIES {
                return Err(
                    "The selected content folder has too many entries to scan safely.".into(),
                );
            }
            let entry =
                entry.map_err(|_| "The selected content folder could not be read.".to_string())?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_addon_name(&name) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|_| "A content folder entry could not be inspected.".to_string())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                if addons.len() >= MAX_PROFILE_ADDONS {
                    return Err(
                        "The selected content folder has too many addon directories.".into(),
                    );
                }
                addons.push(name);
            }
        }
        addons.sort_by(|left, right| {
            left.to_ascii_lowercase()
                .cmp(&right.to_ascii_lowercase())
                .then_with(|| left.cmp(right))
        });
        Ok(addons)
    }

    pub fn load(space_dir: &Path) -> Result<Self, String> {
        Self::read_snapshot(space_dir)
            .map(|snapshot| snapshot.sources)
            .map_err(|error| error.to_string())
    }

    pub(super) fn read_snapshot(
        space_dir: &Path,
    ) -> Result<ContentSourceSnapshot, ServerStoreError> {
        let path = space_dir.join(CONTENT_SOURCES_FILE);
        super::store::check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
        super::store::check_regular_file(&path)?;
        let bytes = match super::read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ContentSourceSnapshot {
                    sources: Self::default(),
                    fingerprint: None,
                });
            }
            Err(_) => return Err(ServerStoreError::Io),
        };
        let sources: Self =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if sources.schema_version != SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let sources = sources
            .normalized()
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
        Ok(ContentSourceSnapshot {
            sources,
            fingerprint: Some(blake3::hash(&bytes).to_hex().to_string()),
        })
    }

    pub fn save(&self, space_dir: &Path) -> Result<(), String> {
        let path = space_dir.join(CONTENT_SOURCES_FILE);
        super::store::check_directory_chain(path.parent().ok_or("Invalid content registry path")?)
            .map_err(|error| error.to_string())?;
        super::store::check_regular_file(&path).map_err(|error| error.to_string())?;
        let normalized = self
            .clone()
            .normalized()
            .map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(&normalized).map_err(|error| error.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("Content source registry exceeds 4 MiB".into());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        crate::core::utils::fs_safety::atomic_write(&path, &bytes)
            .map_err(|error| error.to_string())
    }

    fn normalized(self) -> Result<Self, SourceError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SourceError::Identity);
        }
        let mut normalized = Self::default();
        for folder in self.folders {
            normalized.add_folder(&folder.id, &folder.path)?;
        }
        for profile in self.profiles {
            normalized.add_profile(profile)?;
        }
        Ok(normalized)
    }

    pub fn add_folder(&mut self, id: &str, path: &Path) -> Result<(), SourceError> {
        validate_id(id)?;
        if !path.is_absolute() || has_parent_segment(path) {
            return Err(SourceError::Identity);
        }
        if self.folders.len() >= MAX_SOURCE_RECORDS {
            return Err(SourceError::Limit);
        }
        if self
            .folders
            .iter()
            .any(|folder| folder.id.eq_ignore_ascii_case(id))
        {
            return Err(SourceError::DuplicateFolder);
        }
        self.folders.push(AddonFolder {
            id: id.to_string(),
            path: std::path::absolute(path).map_err(|_| SourceError::Identity)?,
        });
        Ok(())
    }

    pub fn add_profile(&mut self, mut profile: RepositoryProfile) -> Result<(), SourceError> {
        validate_id(&profile.name)?;
        validate_id(&profile.folder_id)?;
        profile.remote_url = normalize_repository_url(&profile.remote_url)?;
        if profile.supply == AddonSupply::Download && profile.remote_url.is_empty() {
            return Err(SourceError::InvalidUrl);
        }
        if !profile.local_path.is_absolute() || has_parent_segment(&profile.local_path) {
            return Err(SourceError::Identity);
        }
        if self.profiles.len() >= MAX_SOURCE_RECORDS || profile.addons.len() > MAX_PROFILE_ADDONS {
            return Err(SourceError::Limit);
        }
        let mut addon_names = std::collections::HashSet::new();
        for addon in &profile.addons {
            if !is_addon_name(addon) || !addon_names.insert(addon.to_ascii_lowercase()) {
                return Err(SourceError::Escape);
            }
        }
        profile.local_path =
            std::path::absolute(&profile.local_path).map_err(|_| SourceError::Identity)?;
        let folder = self
            .folders
            .iter()
            .find(|folder| folder.id == profile.folder_id)
            .ok_or(SourceError::UnknownFolder)?;
        if profile.supply == AddonSupply::Download && same_path(&folder.path, &profile.local_path) {
            return Err(SourceError::DownloadOwnCopy);
        }
        if self
            .profiles
            .iter()
            .any(|existing| existing.name.eq_ignore_ascii_case(&profile.name))
        {
            return Err(SourceError::DuplicateName);
        }
        if self.profiles.iter().any(|existing| {
            existing.remote_url == profile.remote_url
                && same_path(&existing.local_path, &profile.local_path)
                && !(existing.remote_url.is_empty()
                    && profile.remote_url.is_empty()
                    && existing.supply == AddonSupply::AlreadyPresent
                    && profile.supply == AddonSupply::AlreadyPresent)
        }) {
            return Err(SourceError::DuplicateProfile);
        }
        self.profiles.push(profile);
        Ok(())
    }

    pub fn resolve(&self, name: &str) -> Result<ResolvedProfile, SourceError> {
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.name == name)
            .ok_or(SourceError::Identity)?;
        let folder = self
            .folders
            .iter()
            .find(|folder| folder.id == profile.folder_id)
            .ok_or(SourceError::UnknownFolder)?;
        if profile.supply == AddonSupply::Download && same_path(&folder.path, &profile.local_path) {
            return Err(SourceError::DownloadOwnCopy);
        }
        let addon_paths = if profile.addons.is_empty() {
            vec![folder.path.clone()]
        } else {
            profile
                .addons
                .iter()
                .map(|addon| folder.path.join(addon))
                .collect()
        };
        Ok(ResolvedProfile {
            download: profile.supply == AddonSupply::Download,
            root: folder.path.clone(),
            addon_paths,
        })
    }
}

pub fn client_enablement_changes() -> bool {
    false
}

fn validate_id(value: &str) -> Result<(), SourceError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(SourceError::Identity);
    }
    Ok(())
}

pub(super) fn is_addon_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.ends_with(['.', ' '])
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
                )
        })
}

fn has_parent_segment(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn normalize_repository_url(url: &str) -> Result<String, SourceError> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(String::new());
    }
    if url
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(SourceError::InvalidUrl);
    }
    let mut parsed = reqwest::Url::parse(url).map_err(|_| SourceError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(SourceError::InvalidUrl);
    }
    if !parsed.path().ends_with('/') {
        parsed.set_path(&format!("{}/", parsed.path()));
    }
    Ok(parsed.into())
}

fn same_path(left: &Path, right: &Path) -> bool {
    if !cfg!(windows) {
        return left == right;
    }
    let normalize = |path: &Path| {
        let text = path.to_string_lossy().replace('/', "\\");
        let text = text.trim_end_matches('\\').to_string();
        if cfg!(windows) {
            text.to_ascii_lowercase()
        } else {
            text
        }
    };
    normalize(left) == normalize(right)
}

impl ServerStore {
    pub fn content_sources(&self) -> Result<ContentSourceSnapshot, ServerStoreError> {
        if !self.capabilities().content_planning {
            return Err(ServerStoreError::UnsupportedGame);
        }
        ContentSources::read_snapshot(&self.context().space_dir())
    }

    pub fn save_content_sources(
        &self,
        sources: &ContentSources,
        expected_fingerprint: Option<&str>,
    ) -> Result<(), ServerStoreError> {
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                let current = super::ServerContext::capture(
                    self.context().data_root(),
                    Some(self.context().space_id()),
                )
                .map_err(ServerStoreError::Invalid)?;
                if &current != self.context() {
                    return Err(ServerStoreError::StaleRevision);
                }
                if self.content_sources()?.fingerprint.as_deref() != expected_fingerprint {
                    return Err(ServerStoreError::StaleRevision);
                }
                sources
                    .save(&self.context().space_dir())
                    .map_err(|_| ServerStoreError::Io)
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }
}

#[cfg(test)]
mod tests;
