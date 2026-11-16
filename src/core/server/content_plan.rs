use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::addons::{ServerAddonBinding, ServerAddonRole, order_bindings};
use super::content_sources::AddonSupply;
use super::{ServerInstanceId, ServerStore, ServerStoreError};

const MAX_BINDINGS: usize = 256;
const MAX_PATHS: usize = 2048;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentPlanRequest {
    pub instance_id: ServerInstanceId,
    pub expected_instance_revision: u64,
    pub expected_catalog_revision: u64,
    pub content_sources_fingerprint: Option<String>,
    pub bindings: Vec<ServerAddonBinding>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentDeploymentPlan {
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub instance_revision: u64,
    pub catalog_revision: u64,
    pub content_sources_fingerprint: Option<String>,
    pub fingerprint: String,
    pub bindings: Vec<PlannedContentBinding>,
    pub blockers: Vec<ContentPlanBlocker>,
    pub deployment_ready: bool,
    pub downloads_started: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlannedContentBinding {
    pub profile_name: String,
    pub repository_url: String,
    pub repository_local_path: PathBuf,
    pub supply: AddonSupply,
    pub role: ServerAddonRole,
    pub order: u32,
    pub addon_paths: Vec<PlannedContentPath>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PlannedContentPath {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub state: ContentSourceState,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSourceState {
    PresentUnverified,
    Missing,
    Unsafe,
    AcquisitionRequired,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentPlanBlocker {
    pub code: &'static str,
    pub message: String,
}

impl ServerStore {
    pub fn plan_content(
        &self,
        request: ContentPlanRequest,
    ) -> Result<ContentDeploymentPlan, ServerStoreError> {
        if request.bindings.len() > MAX_BINDINGS {
            return Err(ServerStoreError::Invalid(
                "Content plan exceeds its binding limit".into(),
            ));
        }
        let (catalog, catalog_fingerprint, _) = self.read()?;
        if catalog.revision != request.expected_catalog_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        let instance = catalog
            .instances
            .iter()
            .find(|instance| instance.id == request.instance_id)
            .ok_or(ServerStoreError::NotFound)?;
        if instance.revision != request.expected_instance_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        let snapshot = self.content_sources()?;
        if snapshot.fingerprint != request.content_sources_fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let ordered = order_bindings(request.bindings).map_err(|_| {
            ServerStoreError::Invalid("Content plan contains invalid or duplicate bindings".into())
        })?;
        let mut blockers = Vec::new();
        let mut planned = Vec::with_capacity(ordered.len());
        let mut destinations = Vec::<String>::new();
        let mut path_count = 0usize;
        let mut path_bytes = 0usize;
        let installation = instance
            .installation_id
            .as_ref()
            .and_then(|id| catalog.installations.iter().find(|item| &item.id == id));
        let content_root = self
            .context()
            .space_dir()
            .join("server/instances")
            .join(request.instance_id.as_str())
            .join("content");

        for binding in ordered {
            let profile = snapshot
                .sources
                .profiles
                .iter()
                .find(|profile| profile.name == binding.source_id)
                .ok_or_else(|| {
                    ServerStoreError::Invalid(format!(
                        "Unknown content source profile: {}",
                        binding.source_id
                    ))
                })?;
            let folder = snapshot
                .sources
                .folders
                .iter()
                .find(|folder| folder.id == profile.folder_id)
                .ok_or(ServerStoreError::Corrupt)?;
            let expanded_bytes = folder
                .path
                .as_os_str()
                .len()
                .saturating_mul(profile.addons.len().max(1))
                .saturating_add(profile.addons.iter().map(String::len).sum::<usize>());
            path_bytes = path_bytes.saturating_add(expanded_bytes);
            if path_bytes > 4 * 1024 * 1024 {
                return Err(ServerStoreError::Invalid(
                    "Content plan exceeds its path size limit".into(),
                ));
            }
            let names = if profile.addons.is_empty() {
                vec![folder.path.clone()]
            } else {
                profile
                    .addons
                    .iter()
                    .map(|name| folder.path.join(name))
                    .collect()
            };
            path_count = path_count.saturating_add(names.len());
            if path_count > MAX_PATHS {
                return Err(ServerStoreError::Invalid(
                    "Content plan exceeds its path limit".into(),
                ));
            }
            let mut addon_paths = Vec::with_capacity(names.len());
            for source in names {
                let basename = source.file_name().and_then(|name| name.to_str());
                let valid_basename = basename.is_some_and(super::content_sources::is_addon_name);
                let destination = content_root.join(
                    basename
                        .filter(|_| valid_basename)
                        .unwrap_or("invalid-addon-name"),
                );
                if !valid_basename {
                    push_blocker(
                        &mut blockers,
                        "invalid_addon_basename",
                        "An addon source does not have a safe directory name.",
                    );
                }
                if check_destination(&destination).is_err() {
                    push_blocker(
                        &mut blockers,
                        "unsafe_destination",
                        "A proposed content destination contains a link or non-directory.",
                    );
                }
                if destination.exists() {
                    push_blocker(
                        &mut blockers,
                        "destination_exists",
                        "A proposed content destination already exists and will not be overwritten.",
                    );
                }
                let key = conservative_path_key(&destination);
                for previous in &destinations {
                    if path_keys_overlap(previous, &key) {
                        push_blocker(
                            &mut blockers,
                            "destination_collision",
                            "Two selected content sources propose overlapping destinations.",
                        );
                    }
                }
                destinations.push(key);
                if installation.is_some_and(|installation| {
                    super::installation::paths_overlap(&installation.directory, &destination)
                }) {
                    push_blocker(
                        &mut blockers,
                        "installation_overlap",
                        "Content destinations must remain separate from the adopted installation.",
                    );
                }
                if super::installation::paths_overlap(&source, &content_root)
                    || super::installation::paths_overlap(&source, &destination)
                {
                    push_blocker(
                        &mut blockers,
                        "content_source_destination_overlap",
                        "A content source overlaps its proposed destination tree and cannot be copied safely.",
                    );
                }
                if installation.is_some_and(|installation| {
                    super::installation::paths_overlap(&installation.directory, &source)
                }) {
                    push_blocker(
                        &mut blockers,
                        "content_source_installation_overlap",
                        "Content sources must remain separate from the adopted installation.",
                    );
                }
                let observed_source = observe_directory(&source);
                let state = match (profile.supply, observed_source) {
                    (AddonSupply::Download, ContentSourceState::Unsafe) => {
                        ContentSourceState::Unsafe
                    }
                    (AddonSupply::Download, _) => ContentSourceState::AcquisitionRequired,
                    (AddonSupply::AlreadyPresent, state) => state,
                };
                if profile.supply == AddonSupply::Download {
                    push_blocker(
                        &mut blockers,
                        "content_acquisition_required",
                        "This profile requires acquisition; preview does not download or verify content.",
                    );
                }
                match (profile.supply, state) {
                    (_, ContentSourceState::Unsafe) => push_blocker(
                        &mut blockers,
                        "content_source_unsafe",
                        "An addon source is not an ordinary directory or could not be safely observed.",
                    ),
                    (AddonSupply::AlreadyPresent, ContentSourceState::Missing) => push_blocker(
                        &mut blockers,
                        "content_source_missing",
                        "An already-present addon directory is missing.",
                    ),
                    _ => {}
                }
                addon_paths.push(PlannedContentPath {
                    source,
                    destination,
                    state,
                });
            }
            planned.push(PlannedContentBinding {
                profile_name: profile.name.clone(),
                repository_url: profile.remote_url.clone(),
                repository_local_path: profile.local_path.clone(),
                supply: profile.supply,
                role: binding.role,
                order: binding.order,
                addon_paths,
            });
        }
        push_blocker(
            &mut blockers,
            "runtime_deployment_unavailable",
            "Content preview does not stage or activate files; use the separate reviewed deployment workflow.",
        );

        let (after_catalog, after_catalog_fingerprint, _) = self.read()?;
        let after_sources = self.content_sources()?;
        if after_catalog.revision != catalog.revision
            || after_catalog_fingerprint != catalog_fingerprint
            || after_sources.fingerprint != snapshot.fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let identity = serde_json::to_vec(&(
            self.context().space_id(),
            &request.instance_id,
            request.expected_instance_revision,
            request.expected_catalog_revision,
            &snapshot.fingerprint,
            &planned,
        ))
        .map_err(|_| ServerStoreError::Corrupt)?;
        Ok(ContentDeploymentPlan {
            game_space_id: self.context().space_id().to_string(),
            instance_id: request.instance_id,
            instance_revision: instance.revision,
            catalog_revision: catalog.revision,
            content_sources_fingerprint: snapshot.fingerprint,
            fingerprint: blake3::hash(&identity).to_hex().to_string(),
            bindings: planned,
            blockers,
            deployment_ready: false,
            downloads_started: false,
            writes_performed: false,
        })
    }
}

fn push_blocker(blockers: &mut Vec<ContentPlanBlocker>, code: &'static str, message: &str) {
    if !blockers
        .iter()
        .any(|blocker| blocker.code == code && blocker.message == message)
    {
        blockers.push(ContentPlanBlocker {
            code,
            message: message.into(),
        });
    }
}

fn observe_directory(path: &Path) -> ContentSourceState {
    if super::store::check_directory_chain(path).is_err() {
        return ContentSourceState::Unsafe;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if path.canonicalize().is_ok() {
                ContentSourceState::PresentUnverified
            } else {
                ContentSourceState::Unsafe
            }
        }
        Ok(_) => ContentSourceState::Unsafe,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ContentSourceState::Missing,
        Err(_) => ContentSourceState::Unsafe,
    }
}

fn check_destination(path: &Path) -> Result<(), ServerStoreError> {
    super::store::check_directory_chain(path)
}

fn conservative_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn path_keys_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|tail| tail.starts_with('\\'))
        || right
            .strip_prefix(left)
            .is_some_and(|tail| tail.starts_with('\\'))
}

#[cfg(test)]
#[path = "content_plan/tests.rs"]
mod tests;
