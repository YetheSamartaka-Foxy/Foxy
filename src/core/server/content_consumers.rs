//! Repository identity is (URL, local path); physical sharing requires canonical addon directories, never URL alone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    ContentSources, ContentStageControl, RepositoryProfile, ServerInstanceId, ServerStore,
    ServerStoreError,
};

const MAX_SPACES: usize = 256;
const MAX_REPORT_ENTRIES: usize = 4096;
const MAX_REPOSITORIES_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ContentConsumerReport {
    pub game_space_id: String,
    pub sources: Vec<SourceConsumers>,
    pub diagnostics: Vec<String>,
    pub inspection_complete: bool,
    pub source_discovery_scope: &'static str,
    pub installation_consumers: Vec<super::InstallationConsumerInspection>,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceConsumers {
    pub profile: String,
    pub remote_url: String,
    pub local_path: String,
    pub folder_id: String,
    pub physical_addons: Vec<String>,
    pub instances: Vec<InstanceContentUse>,
    pub client_repositories: Vec<String>,
    pub other_space_profiles: Vec<String>,
    pub shared_with_profiles: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstanceContentUse {
    pub instance_id: ServerInstanceId,
    pub display_name: String,
    pub saved_binding_role: Option<String>,
    pub retained_stages: Vec<String>,
    pub active_record: Option<String>,
    pub pinned_records: Vec<String>,
}

fn normalized_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.ends_with('/') {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/")
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (physical(left), physical(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn physical(path: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    super::content_staging::physical_directory(path).ok()
}

fn physical_addons(sources: &ContentSources, profile: &RepositoryProfile) -> Vec<PathBuf> {
    let Some(folder) = sources
        .folders
        .iter()
        .find(|folder| folder.id == profile.folder_id)
    else {
        return Vec::new();
    };
    let directories: Vec<PathBuf> = if profile.addons.is_empty() {
        vec![folder.path.clone()]
    } else {
        profile
            .addons
            .iter()
            .map(|addon| folder.path.join(addon))
            .collect()
    };
    directories
        .into_iter()
        .filter_map(|path| physical(&path))
        .collect()
}

fn client_repositories(
    space_dir: &Path,
    control: &ContentStageControl,
) -> Result<Vec<(String, String, PathBuf)>, String> {
    let path = space_dir.join("repositories.json");
    super::store::check_directory_chain(space_dir)
        .map_err(|_| "Client repository directory is not ordinary.".to_string())?;
    super::store::check_regular_file(&path)
        .map_err(|_| "Client repository registry is not an ordinary file.".to_string())?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("Client repository registry could not be inspected.".into()),
    };
    if !metadata.is_file() || metadata.len() as usize > MAX_REPOSITORIES_BYTES {
        return Err("Client repository registry is not an ordinary bounded file.".into());
    }
    let bytes =
        super::content_staging::read_bounded_controlled(&path, MAX_REPOSITORIES_BYTES, control)
            .map_err(|_| {
                "Client repository registry could not be read within its limit.".to_string()
            })?;
    let serde_json::Value::Array(entries) = serde_json::from_slice(&bytes)
        .map_err(|_| "Client repository registry is malformed.".to_string())?
    else {
        return Err("Client repository registry is malformed.".into());
    };
    if entries.len() > MAX_REPORT_ENTRIES {
        return Err("Client repository registry exceeds its entry limit.".into());
    }
    entries
        .iter()
        .map(|entry| {
            Some((
                entry.get("name")?.as_str()?.to_owned(),
                entry.get("address")?.as_str()?.to_owned(),
                PathBuf::from(entry.get("path")?.as_str()?),
            ))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| "Client repository registry contains an invalid entry.".into())
}

fn profile_matches(
    left: &RepositoryProfile,
    left_folder: &super::content_sources::AddonFolder,
    right: &RepositoryProfile,
    left_sources: &ContentSources,
    right_sources: &ContentSources,
) -> bool {
    let left_addons = physical_addons(left_sources, left);
    let right_addons = physical_addons(right_sources, right);
    left.remote_url == right.remote_url
        && same_path(&left.local_path, &right.local_path)
        && physical(&left_folder.path)
            == right_sources
                .folders
                .iter()
                .find(|folder| folder.id == right.folder_id)
                .and_then(|folder| physical(&folder.path))
        && !left_addons.is_empty()
        && left_addons == right_addons
        && left.addons == right.addons
}

fn client_repository_matches(url: &str, local_path: &Path, address: &str, path: &Path) -> bool {
    !url.is_empty() && normalized_url(address) == url && same_path(local_path, path)
}

impl ServerStore {
    pub fn content_consumers(&self) -> Result<ContentConsumerReport, ServerStoreError> {
        let space_dir = self.context().space_dir();
        let sources = self.content_sources()?.sources;
        let catalog = self.load()?;
        let control = ContentStageControl::default();
        let mut inspection_complete = true;
        let mut diagnostics = Vec::new();
        let mut repositories = Vec::new();
        match client_repositories(&space_dir, &control) {
            Ok(entries) => repositories.extend(entries.into_iter().map(|(name, address, path)| {
                (self.context().space_id().to_owned(), name, address, path)
            })),
            Err(message) => {
                inspection_complete = false;
                diagnostics.push(message);
            }
        }
        let other_spaces =
            self.other_space_sources(&space_dir, &mut diagnostics, &mut inspection_complete);
        let mut inspected_repositories = repositories.len();
        for entry in &other_spaces {
            if inspected_repositories >= MAX_REPORT_ENTRIES {
                inspection_complete = false;
                diagnostics.push("Combined client repository inventory exceeds its entry limit; remaining registries are unknown.".into());
                break;
            }
            if let Ok(mut entries) = client_repositories(
                &space_dir.parent().unwrap_or(&space_dir).join(&entry.0),
                &control,
            ) {
                let remaining = MAX_REPORT_ENTRIES - inspected_repositories;
                if entries.len() > remaining {
                    entries.truncate(remaining);
                    inspection_complete = false;
                    diagnostics.push("Combined client repository inventory exceeds its entry limit; remaining entries are unknown.".into());
                }
                inspected_repositories += entries.len();
                repositories.extend(
                    entries
                        .into_iter()
                        .map(|(name, address, path)| (entry.0.clone(), name, address, path)),
                );
            } else {
                inspection_complete = false;
                diagnostics.push(format!(
                    "Client repository registry for another space ({}) could not be inspected.",
                    entry.0
                ));
            }
        }
        for (space_id, name, _, path) in &repositories {
            if physical(path).is_none() {
                inspection_complete = false;
                diagnostics.push(format!(
                    "Client repository {name} in space {space_id} has an unresolved physical path."
                ));
            }
        }
        let mut profile_directories: Vec<(String, Vec<PathBuf>)> = Vec::new();
        for profile in &sources.profiles {
            profile_directories.push((profile.name.clone(), physical_addons(&sources, profile)));
        }

        let mut reports = Vec::new();
        let mut instance_scan_budget = MAX_REPORT_ENTRIES;
        'profiles: for profile in &sources.profiles {
            let addons = profile_directories
                .iter()
                .find(|(name, _)| name == &profile.name)
                .map(|(_, addons)| addons.clone())
                .unwrap_or_default();
            if addons.is_empty() {
                inspection_complete = false;
                diagnostics.push(format!(
                    "Profile {} has no existing physical addon directory.",
                    profile.name
                ));
            }
            if physical(&profile.local_path).is_none() {
                inspection_complete = false;
                diagnostics.push(format!(
                    "Profile {} repository path could not be physically resolved.",
                    profile.name
                ));
            }
            let shared_with_profiles = profile_directories
                .iter()
                .filter(|(name, other)| {
                    name != &profile.name && other.iter().any(|path| addons.contains(path))
                })
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>();
            let url = normalized_url(&profile.remote_url);
            let mut client = Vec::new();
            for (space_id, name, address, path) in &repositories {
                if physical(path).is_none() {
                    continue;
                }
                if client_repository_matches(&url, &profile.local_path, address, path) {
                    client.push(if space_id == self.context().space_id() {
                        name.clone()
                    } else {
                        format!("{space_id}/{name}")
                    });
                } else {
                    if same_path(path, &profile.local_path) {
                        diagnostics.push(format!(
                            "Client repository {name} uses the local path of profile {} with a different URL; they are independent repository instances.",
                            profile.name
                        ));
                    }
                }
            }
            let mut other_space_profiles = Vec::new();
            for (space, other) in &other_spaces {
                for candidate in &other.profiles {
                    if physical_addons(other, candidate)
                        .iter()
                        .any(|path| addons.contains(path))
                    {
                        other_space_profiles.push(format!("{space}/{}", candidate.name));
                    }
                }
            }
            let mut instances = Vec::new();
            for instance in &catalog.instances {
                if instance_scan_budget == 0 {
                    inspection_complete = false;
                    break 'profiles;
                }
                instance_scan_budget -= 1;
                let (document, _) = match self.read_content_bindings(&instance.id) {
                    Ok(value) => value,
                    Err(_) => {
                        inspection_complete = false;
                        diagnostics.push(format!(
                            "Bindings for server {} could not be inspected.",
                            instance.display_name
                        ));
                        (None, None)
                    }
                };
                let saved_binding_role = document.as_ref().and_then(|document| {
                    document
                        .bindings
                        .iter()
                        .find(|binding| {
                            profile_matches(
                                &binding.profile,
                                &binding.folder,
                                profile,
                                &ContentSources {
                                    schema_version: sources.schema_version,
                                    folders: vec![binding.folder.clone()],
                                    profiles: vec![binding.profile.clone()],
                                },
                                &sources,
                            )
                        })
                        .map(|binding| {
                            serde_json::to_value(binding.binding.role)
                                .ok()
                                .and_then(|value| value.as_str().map(str::to_owned))
                                .unwrap_or_default()
                        })
                });
                let mut retained_stages = Vec::new();
                match self.content_stages(&instance.id) {
                    Ok(listing) => {
                        if listing.unrecognized_entries > 0 {
                            inspection_complete = false;
                            diagnostics.push(format!("{} unrecognized retained-stage entries for server {} were not inspected.", listing.unrecognized_entries, instance.display_name));
                        }
                        for stage in listing.stage_ids {
                            match self.read_stage_manifest(&instance.id, &stage, &control) {
                                Ok(manifest)
                                    if manifest.bindings.bindings.iter().any(|binding| {
                                        profile_matches(
                                            &binding.profile,
                                            &binding.folder,
                                            profile,
                                            &ContentSources {
                                                schema_version: sources.schema_version,
                                                folders: manifest
                                                    .bindings
                                                    .bindings
                                                    .iter()
                                                    .map(|b| b.folder.clone())
                                                    .collect(),
                                                profiles: manifest
                                                    .bindings
                                                    .bindings
                                                    .iter()
                                                    .map(|b| b.profile.clone())
                                                    .collect(),
                                            },
                                            &sources,
                                        )
                                    }) =>
                                {
                                    retained_stages.push(stage)
                                }
                                Ok(_) => {}
                                Err(_) => {
                                    inspection_complete = false;
                                    diagnostics.push(format!("Retained stage {stage} for server {} could not be inspected.", instance.display_name));
                                }
                            }
                        }
                    }
                    Err(_) => {
                        inspection_complete = false;
                        diagnostics.push(format!(
                            "Retained stages for server {} could not be inventoried.",
                            instance.display_name
                        ));
                    }
                }
                if saved_binding_role.is_none() && retained_stages.is_empty() {
                    continue;
                }
                let active_record = match self.read_activation_state(&instance.id, &control) {
                    Ok(state) => state.map(|(state, _)| state.deployment.record_id),
                    Err(_) => {
                        inspection_complete = false;
                        diagnostics.push(format!(
                            "Active record for server {} could not be inspected.",
                            instance.display_name
                        ));
                        None
                    }
                };
                let pinned_records = match self.deployment_pins(&instance.id) {
                    Ok(pins) => pins.records,
                    Err(_) => {
                        inspection_complete = false;
                        diagnostics.push(format!(
                            "Pinned records for server {} could not be inspected.",
                            instance.display_name
                        ));
                        Vec::new()
                    }
                };
                instances.push(InstanceContentUse {
                    instance_id: instance.id.clone(),
                    display_name: instance.display_name.clone(),
                    saved_binding_role,
                    retained_stages,
                    active_record,
                    pinned_records,
                });
            }
            reports.push(SourceConsumers {
                profile: profile.name.clone(),
                remote_url: profile.remote_url.clone(),
                local_path: profile.local_path.display().to_string(),
                folder_id: profile.folder_id.clone(),
                physical_addons: addons
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                instances,
                client_repositories: client,
                other_space_profiles,
                shared_with_profiles,
            });
        }
        if instance_scan_budget == 0 {
            diagnostics.push("Server source-consumer combinations exceed the inspection limit; remaining combinations are unknown.".into());
        }
        let mut seen_urls = BTreeMap::new();
        for profile in &sources.profiles {
            let url = normalized_url(&profile.remote_url);
            if url.is_empty() {
                continue;
            }
            seen_urls
                .entry(url)
                .or_insert_with(Vec::new)
                .push((profile.name.as_str(), physical(&profile.local_path)));
        }
        for (url, names) in seen_urls.into_iter().filter(|(_, names)| {
            names.iter().enumerate().any(|(index, (_, path))| {
                names
                    .iter()
                    .skip(index + 1)
                    .any(|(_, other)| path.is_some() && other.is_some() && path != other)
            })
        }) {
            diagnostics.push(format!(
                "Profiles {} share URL {url} but are separate repository instances because their local paths differ.",
                names.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            ));
        }
        let mut installation_consumers = Vec::new();
        if catalog.installations.len() > MAX_REPORT_ENTRIES {
            inspection_complete = false;
            diagnostics.push("Installation consumer inventory exceeds its entry limit.".into());
        }
        for installation in catalog.installations.iter().take(MAX_REPORT_ENTRIES) {
            let inspection =
                super::consumers::InstallationConsumerInspection::inspect(installation);
            if !inspection.inspection_complete {
                inspection_complete = false;
                diagnostics.push(format!(
                    "Consumer registrations for installation {} could not be verified.",
                    installation.id.as_str()
                ));
            }
            installation_consumers.push(inspection);
        }
        Ok(ContentConsumerReport {
            game_space_id: self.context().space_id().into(),
            sources: reports,
            diagnostics,
            inspection_complete,
            source_discovery_scope: "Registered source and client repository registries under this configuration root; other roots are unknown.",
            installation_consumers,
            writes_performed: false,
        })
    }

    fn other_space_sources(
        &self,
        space_dir: &Path,
        diagnostics: &mut Vec<String>,
        complete: &mut bool,
    ) -> Vec<(String, ContentSources)> {
        let Some(games) = space_dir.parent() else {
            *complete = false;
            diagnostics.push("Other registered game spaces could not be inventoried.".into());
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(games) else {
            *complete = false;
            diagnostics.push("Other registered game spaces could not be inventoried.".into());
            return Vec::new();
        };
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        let Ok(mut entries) = entries.collect::<Result<Vec<_>, _>>() else {
            *complete = false;
            diagnostics.push("Other registered game spaces could not be inventoried.".into());
            return result;
        };
        entries.sort_by_key(|entry| entry.file_name());
        if entries.len() > MAX_SPACES {
            *complete = false;
            diagnostics.push("Other game-space inventory exceeds its inspection limit.".into());
        }
        for entry in entries.into_iter().take(MAX_SPACES) {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if path == space_dir || !seen.insert(name.to_owned()) {
                continue;
            }
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            match crate::core::server::ServerContext::capture(
                self.context().data_root(),
                Some(name),
            ) {
                Ok(context) if context.game_id() == self.context().game_id() => {
                    match ContentSources::load(&path) {
                        Ok(sources) => {
                            if sources
                                .profiles
                                .iter()
                                .any(|profile| physical_addons(&sources, profile).is_empty())
                            {
                                *complete = false;
                                diagnostics.push(format!("One or more source paths in another space ({name}) could not be physically resolved."));
                            }
                            result.push((name.to_owned(), sources));
                        }
                        Err(_) => {
                            *complete = false;
                            diagnostics.push(format!("Content source registry for another space ({name}) could not be inspected."));
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    *complete = false;
                    diagnostics
                        .push("Registered game-space identity could not be verified.".into());
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_normalize_with_one_trailing_slash_only() {
        assert_eq!(
            normalized_url("https://repo.example.invalid:8080"),
            "https://repo.example.invalid:8080/"
        );
        assert_eq!(
            normalized_url("https://repo.example.invalid:8080/"),
            "https://repo.example.invalid:8080/"
        );
        assert_eq!(normalized_url("  "), "");
    }

    #[test]
    fn client_repositories_require_name_address_and_path() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("repositories.json"),
            br#"[{"name":"MainRepo","address":"https://repo.example.invalid:8080/","path":"R:/Mods/MainRepo"}]"#,
        )
        .unwrap();
        let repositories =
            client_repositories(temp.path(), &ContentStageControl::default()).unwrap();
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].0, "MainRepo");
        assert_eq!(repositories[0].2, PathBuf::from("R:/Mods/MainRepo"));
        fs::write(temp.path().join("repositories.json"),br#"[{"name":"MainRepo","address":"https://repo.example.invalid:8080/","path":"R:/Mods/MainRepo"},{"name":"Broken"}]"#).unwrap();
        assert!(client_repositories(temp.path(), &ContentStageControl::default()).is_err());
    }

    #[test]
    fn client_repository_identity_requires_url_and_physical_path() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("RepoAlpha");
        let other = temp.path().join("RepoBeta");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&other).unwrap();
        let url = "https://repo.example.invalid:8080/";
        assert!(client_repository_matches(
            url,
            &repo,
            "https://repo.example.invalid:8080",
            &repo
        ));
        assert!(!client_repository_matches(url, &repo, url, &other));
        assert!(!client_repository_matches(
            url,
            &repo,
            "https://other.example.invalid/",
            &repo
        ));
    }

    #[test]
    fn malformed_client_registry_is_reported_as_incomplete_input() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("repositories.json"), b"not-json").unwrap();
        assert!(
            client_repositories(temp.path(), &ContentStageControl::default())
                .unwrap_err()
                .contains("malformed")
        );
    }
}
