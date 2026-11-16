use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::definition::SERVER_SCHEMA_VERSION;
use super::{
    InstallationId, ServerContext, ServerDefinitionKind, ServerInstallation, ServerInstance,
    ServerInstanceId,
};

const MAX_DOCUMENT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStoreError {
    Invalid(String),
    Corrupt,
    UnsupportedVersion,
    UnsupportedGame,
    NotFound,
    InstallationNotFound,
    StaleRevision,
    Ownership,
    Io,
    UnsafePath,
    Cancelled,
    StageCleanupIncomplete,
    ReviewCancelled,
    RecordCancelled,
    RecordCleanupIncomplete,
    PreparationCancelled,
    DeploymentCancelled,
    PreparationCleanupIncomplete,
}

impl std::fmt::Display for ServerStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Invalid(message) => message,
            Self::Corrupt => "Server store is invalid; existing data was preserved",
            Self::UnsupportedVersion => {
                "Unsupported server schema version; existing data was preserved"
            }
            Self::UnsupportedGame => "Server draft management is unavailable for this game module",
            Self::NotFound => "Server instance was not found in the selected game space",
            Self::InstallationNotFound => {
                "Server installation was not found in the selected game space"
            }
            Self::StaleRevision => {
                "Server draft changed; reload it and retry with the current revision"
            }
            Self::Ownership => "An exclusive claim on the selected game space is required",
            Self::Io => {
                "Cannot read or write server definitions; check permissions and available storage"
            }
            Self::UnsafePath => {
                "Server storage requires ordinary directories and files without links or reparse points"
            }
            Self::Cancelled => {
                "Content staging cancelled; previously published stages were retained"
            }
            Self::StageCleanupIncomplete => {
                "Content staging stopped, but temporary cleanup is incomplete. Unselected temporary data was retained."
            }
            Self::ReviewCancelled => {
                "Deployment composition review cancelled; no files were changed"
            }
            Self::RecordCancelled => {
                "Deployment record operation cancelled; the saved selection was retained"
            }
            Self::PreparationCancelled => {
                "Deployment preparation cancelled; previous generations and offline selection were retained"
            }
            Self::DeploymentCancelled => {
                "Native deployment cancelled; inspect activation status and recovery before retrying"
            }
            Self::PreparationCleanupIncomplete => {
                "Deployment preparation stopped, but temporary cleanup is incomplete. Previous generations and offline selection were retained."
            }
            Self::RecordCleanupIncomplete => {
                "Deployment record operation stopped, but temporary cleanup is incomplete. The saved selection was retained."
            }
        };
        f.write_str(message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerCatalog {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub revision: u64,
    pub instances: Vec<ServerInstance>,
    pub installations: Vec<ServerInstallation>,
}

pub enum DraftChange {
    Import {
        instance: ServerInstance,
    },
    Adopt {
        id: ServerInstanceId,
        expected_revision: u64,
        installation: ServerInstallation,
    },
    Bind {
        id: ServerInstanceId,
        expected_revision: u64,
        installation_id: Option<InstallationId>,
    },
    Add {
        name: String,
    },
    Replace {
        instance: ServerInstance,
        expected_revision: u64,
    },
    Rename {
        id: ServerInstanceId,
        name: String,
        expected_revision: u64,
    },
    Clone {
        id: ServerInstanceId,
        name: String,
        expected_revision: u64,
    },
    Remove {
        id: ServerInstanceId,
        expected_revision: u64,
    },
}

pub struct DraftPlan {
    context: ServerContext,
    fingerprint: Option<blake3::Hash>,
    after: ServerCatalog,
    pub instance: ServerInstance,
    pub removes_instance: bool,
    pub migrates_from_v1: bool,
    installation_check: Option<ServerInstallation>,
}

impl DraftPlan {
    pub(super) fn set_import_paths_reviewed(&mut self, reviewed: bool) {
        if let Some(imported) = &mut self.instance.configuration_import {
            imported.paths_reviewed = reviewed;
        }
        if let Some(instance) = self
            .after
            .instances
            .iter_mut()
            .find(|instance| instance.id == self.instance.id)
        {
            instance.configuration_import = self.instance.configuration_import.clone();
        }
    }
    pub fn migration_backup_relative_path(&self) -> Option<PathBuf> {
        self.fingerprint
            .filter(|_| self.migrates_from_v1)
            .map(|hash| PathBuf::from(format!("server/servers.v1.{}.json", hash.to_hex())))
    }
    pub fn catalog(&self) -> &ServerCatalog {
        &self.after
    }

    pub fn catalog_revision(&self) -> u64 {
        self.after.revision
    }
}

pub struct ServerStore {
    context: ServerContext,
    kind: ServerDefinitionKind,
}

impl ServerStore {
    pub(super) fn capabilities(&self) -> super::ServerCapabilities {
        crate::core::game::registry()
            .get(self.context.game_id())
            .map(|module| module.server_capabilities())
            .unwrap_or_default()
    }
    pub(crate) fn context(&self) -> &ServerContext {
        &self.context
    }
    pub fn new(context: ServerContext) -> Result<Self, ServerStoreError> {
        let caps = crate::core::game::registry()
            .get(context.game_id())
            .ok_or(ServerStoreError::UnsupportedGame)?
            .server_capabilities();
        let kind = caps
            .definition_kind
            .filter(|_| caps.draft_definitions)
            .ok_or(ServerStoreError::UnsupportedGame)?;
        Ok(Self { context, kind })
    }

    pub fn path(&self) -> PathBuf {
        self.context.space_dir().join("server/servers.json")
    }

    pub fn validate_claim_path(&self) -> Result<PathBuf, ServerStoreError> {
        let path = self.context.database_path();
        check_directory_chain(&self.context.space_dir())?;
        check_regular_file(&path.with_extension("lock"))?;
        check_regular_file(&path.with_extension("owner"))?;
        Ok(path)
    }

    pub fn load(&self) -> Result<ServerCatalog, ServerStoreError> {
        self.load_with_status().map(|(catalog, _)| catalog)
    }

    pub fn load_with_status(&self) -> Result<(ServerCatalog, bool), ServerStoreError> {
        self.read().map(|(catalog, _, legacy)| (catalog, legacy))
    }

    pub fn inspect_installation(
        &self,
        directory: &Path,
    ) -> Result<ServerInstallation, ServerStoreError> {
        self.require_adoption()?;
        let installation = ServerInstallation::inspect(directory, self.kind)?;
        self.check_installation_root(&installation)?;
        Ok(installation)
    }

    pub fn inspect_installation_build(
        &self,
        installation: &ServerInstallation,
    ) -> Result<super::installation::InstallationBuildObservation, ServerStoreError> {
        self.require_adoption()?;
        if !installation.validate(self.kind) {
            return Err(ServerStoreError::StaleRevision);
        }
        self.check_installation_root(installation)?;
        installation.verify()?;
        let observation = super::installation::observe_build(installation);
        installation.verify()?;
        Ok(observation)
    }

    fn require_adoption(&self) -> Result<(), ServerStoreError> {
        if !crate::core::game::registry()
            .get(self.context.game_id())
            .is_some_and(|module| module.server_capabilities().installation_adoption)
        {
            return Err(ServerStoreError::UnsupportedGame);
        }
        Ok(())
    }

    pub(super) fn check_installation_root(
        &self,
        installation: &ServerInstallation,
    ) -> Result<(), ServerStoreError> {
        let root = self
            .context
            .data_root()
            .canonicalize()
            .map_err(|_| ServerStoreError::UnsafePath)?;
        if super::installation::paths_overlap(&installation.directory, &root) {
            return Err(ServerStoreError::Invalid(
                "An adopted installation must be separate from Foxy's data directory".into(),
            ));
        }
        Ok(())
    }

    pub fn show(&self, id: &ServerInstanceId) -> Result<ServerInstance, ServerStoreError> {
        self.load()?
            .instances
            .into_iter()
            .find(|instance| &instance.id == id)
            .ok_or(ServerStoreError::NotFound)
    }

    pub fn parse_instance(&self, bytes: &[u8]) -> Result<ServerInstance, ServerStoreError> {
        let instance: ServerInstance = if check_version(bytes)? == 1 {
            super::migration::instance(bytes)?
        } else {
            serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?
        };
        instance
            .validate(self.context.space_id(), self.kind)
            .map_err(ServerStoreError::Invalid)?;
        Ok(instance)
    }

    pub fn plan(&self, change: DraftChange) -> Result<DraftPlan, ServerStoreError> {
        self.validate_claim_path()?;
        let (mut after, fingerprint, migrates_from_v1) = self.read()?;
        let mut installation_check = None;
        let removes_instance = matches!(change, DraftChange::Remove { .. });
        let instance = match change {
            DraftChange::Import { mut instance } => {
                instance.id = ServerInstanceId::new();
                instance.revision = 1;
                instance.game_space_id = self.context.space_id().into();
                instance.installation_id = None;
                instance
                    .validate(self.context.space_id(), self.kind)
                    .map_err(ServerStoreError::Invalid)?;
                after.instances.push(instance.clone());
                instance
            }
            DraftChange::Adopt {
                id,
                expected_revision,
                mut installation,
            } => {
                self.require_adoption()?;
                let index = find_revision(&after, &id, expected_revision)?;
                if !installation.validate(self.kind) {
                    return Err(ServerStoreError::Corrupt);
                }
                self.check_installation_root(&installation)?;
                installation.verify()?;
                if let Some(existing) = after
                    .installations
                    .iter()
                    .find(|existing| existing.directory == installation.directory)
                {
                    if existing.evidence != installation.evidence
                        || existing.platform != installation.platform
                    {
                        return Err(ServerStoreError::Invalid("Installation evidence changed; a new managed revision is required before rebinding".into()));
                    }
                    installation = existing.clone();
                } else {
                    after.installations.push(installation.clone());
                }
                after.instances[index].installation_id = Some(installation.id.clone());
                after.instances[index].revision = next_revision(expected_revision)?;
                installation_check = Some(installation);
                after.instances[index].clone()
            }
            DraftChange::Bind {
                id,
                expected_revision,
                installation_id,
            } => {
                self.require_adoption()?;
                let index = find_revision(&after, &id, expected_revision)?;
                if let Some(id) = &installation_id {
                    let installation = after
                        .installations
                        .iter()
                        .find(|item| &item.id == id)
                        .ok_or(ServerStoreError::NotFound)?
                        .clone();
                    self.check_installation_root(&installation)?;
                    installation.verify()?;
                    installation_check = Some(installation);
                }
                after.instances[index].installation_id = installation_id;
                after.instances[index].revision = next_revision(expected_revision)?;
                after.instances[index].clone()
            }
            DraftChange::Add { name } => {
                let instance = ServerInstance::draft(self.context.space_id(), self.kind, &name)
                    .map_err(ServerStoreError::Invalid)?;
                after.instances.push(instance.clone());
                instance
            }
            DraftChange::Replace {
                mut instance,
                expected_revision,
            } => {
                instance
                    .validate(self.context.space_id(), self.kind)
                    .map_err(ServerStoreError::Invalid)?;
                let index = find_revision(&after, &instance.id, expected_revision)?;
                if instance.revision != expected_revision {
                    return Err(ServerStoreError::StaleRevision);
                }
                if instance.installation_id != after.instances[index].installation_id {
                    return Err(ServerStoreError::Invalid(
                        "Use the installation binding operation to change installation identity"
                            .into(),
                    ));
                }
                instance.revision = next_revision(expected_revision)?;
                after.instances[index] = instance.clone();
                instance
            }
            DraftChange::Rename {
                id,
                name,
                expected_revision,
            } => {
                let index = find_revision(&after, &id, expected_revision)?;
                let instance = &mut after.instances[index];
                instance.display_name = name;
                instance.revision = next_revision(expected_revision)?;
                instance
                    .validate(self.context.space_id(), self.kind)
                    .map_err(ServerStoreError::Invalid)?;
                instance.clone()
            }
            DraftChange::Clone {
                id,
                name,
                expected_revision,
            } => {
                let index = find_revision(&after, &id, expected_revision)?;
                let mut instance = after.instances[index].clone();
                instance.id = ServerInstanceId::new();
                instance.display_name = name;
                instance.revision = 1;
                instance
                    .validate(self.context.space_id(), self.kind)
                    .map_err(ServerStoreError::Invalid)?;
                after.instances.push(instance.clone());
                instance
            }
            DraftChange::Remove {
                id,
                expected_revision,
            } => {
                let index = find_revision(&after, &id, expected_revision)?;
                after.instances.remove(index)
            }
        };
        after.revision = next_revision(after.revision)?;
        self.validate_catalog(&after)?;
        encode_catalog(&after)?;
        Ok(DraftPlan {
            context: self.context.clone(),
            fingerprint,
            after,
            instance,
            removes_instance,
            migrates_from_v1,
            installation_check,
        })
    }

    pub fn apply(&self, plan: DraftPlan) -> Result<(), ServerStoreError> {
        if plan.context != self.context {
            return Err(ServerStoreError::Invalid(
                "Draft plan belongs to a different context".to_string(),
            ));
        }
        crate::core::server::owner::with_write(&self.context.database_path(), || {
            Ok(self.apply_owned(plan))
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    pub(super) fn apply_owned(&self, plan: DraftPlan) -> Result<(), ServerStoreError> {
        let current_context =
            ServerContext::capture(self.context.data_root(), Some(self.context.space_id()))
                .map_err(|_| {
                    ServerStoreError::Invalid("Selected game space is no longer valid".to_string())
                })?;
        if current_context.game_id() != self.context.game_id() {
            return Err(ServerStoreError::Invalid(
                "Selected game space changed game module".to_string(),
            ));
        }
        let (before, fingerprint, legacy) = self.read()?;
        if fingerprint != plan.fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        for instance in &before.instances {
            if plan.after.instances.iter().any(|next| {
                next.id == instance.id && next.installation_id == instance.installation_id
            }) {
                continue;
            }
            if let Some(installation) = instance.installation_id.as_ref().and_then(|id| {
                before
                    .installations
                    .iter()
                    .find(|installation| &installation.id == id)
            }) {
                let locks =
                    foxy_supervisor::leases::InstallationLocks::capture(&installation.directory)
                        .map_err(|_| ServerStoreError::Ownership)?;
                if locks
                    .inspect_consumers()
                    .map_err(|_| ServerStoreError::Ownership)?
                    .iter()
                    .any(|record| {
                        record.session.game_space_id == self.context.space_id()
                            && super::headless::consumer_belongs_to(
                                &record.session.instance_id,
                                instance.id.as_str(),
                            )
                    })
                {
                    return Err(ServerStoreError::Invalid("Stop the retained instance and verify consumer cleanup before removing or rebinding it.".into()));
                }
            }
        }
        if let Some(installation) = &plan.installation_check {
            self.check_installation_root(installation)?;
            installation.verify()?;
        }
        let bytes = encode_catalog(&plan.after)?;
        let path = self.path();
        let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
        check_directory_chain(parent)?;
        fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
        check_directory_chain(parent)?;
        check_regular_file(&path)?;
        if legacy {
            let original = read_bounded(&path).map_err(|_| ServerStoreError::Io)?;
            if Some(blake3::hash(&original)) != plan.fingerprint {
                return Err(ServerStoreError::StaleRevision);
            }
            let backup_path = parent.join(format!(
                "servers.v1.{}.json",
                blake3::hash(&original).to_hex()
            ));
            check_regular_file(&backup_path)?;
            if backup_path.exists() {
                if read_bounded(&backup_path).map_err(|_| ServerStoreError::Io)? != original {
                    return Err(ServerStoreError::Corrupt);
                }
            } else {
                let mut backup =
                    tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
                backup
                    .write_all(&original)
                    .and_then(|()| backup.as_file().sync_all())
                    .map_err(|_| ServerStoreError::Io)?;
                backup
                    .persist_noclobber(&backup_path)
                    .map_err(|_| ServerStoreError::Io)?;
            }
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
        temp.write_all(&bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        temp.persist(&path).map_err(|_| ServerStoreError::Io)?;
        Ok(())
    }

    pub(super) fn read(
        &self,
    ) -> Result<(ServerCatalog, Option<blake3::Hash>, bool), ServerStoreError> {
        let path = self.path();
        check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
        check_regular_file(&path)?;
        let bytes = match read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((self.empty(), None, false));
            }
            Err(_) => return Err(ServerStoreError::Io),
        };
        let legacy = check_version(&bytes)? == 1;
        let catalog: ServerCatalog = if legacy {
            super::migration::catalog(&bytes)?
        } else {
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?
        };
        self.validate_catalog(&catalog)?;
        Ok((catalog, Some(blake3::hash(&bytes)), legacy))
    }

    fn empty(&self) -> ServerCatalog {
        ServerCatalog {
            schema_version: SERVER_SCHEMA_VERSION,
            game_space_id: self.context.space_id().to_string(),
            game_id: self.context.game_id().to_string(),
            revision: 0,
            instances: Vec::new(),
            installations: Vec::new(),
        }
    }

    fn validate_catalog(&self, catalog: &ServerCatalog) -> Result<(), ServerStoreError> {
        if catalog.schema_version != SERVER_SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        if catalog.game_space_id != self.context.space_id()
            || catalog.game_id != self.context.game_id()
            || (catalog.revision == 0
                && (!catalog.instances.is_empty() || !catalog.installations.is_empty()))
        {
            return Err(ServerStoreError::Corrupt);
        }
        let mut ids = HashSet::new();
        let mut installation_ids = HashSet::new();
        let mut directories = HashSet::new();
        for installation in &catalog.installations {
            if !installation.validate(self.kind)
                || !installation_ids.insert(&installation.id)
                || !directories.insert(&installation.directory)
            {
                return Err(ServerStoreError::Corrupt);
            }
        }
        for instance in &catalog.instances {
            instance
                .validate(self.context.space_id(), self.kind)
                .map_err(|_| ServerStoreError::Corrupt)?;
            if !ids.insert(&instance.id) {
                return Err(ServerStoreError::Corrupt);
            }
            if instance
                .installation_id
                .as_ref()
                .is_some_and(|id| !installation_ids.contains(id))
            {
                return Err(ServerStoreError::Corrupt);
            }
        }
        Ok(())
    }
}

pub fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        return Err(std::io::Error::other("Expected a regular JSON document"));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_DOCUMENT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(std::io::Error::other("Server document exceeds 4 MiB"));
    }
    Ok(bytes)
}

fn encode_catalog(catalog: &ServerCatalog) -> Result<Vec<u8>, ServerStoreError> {
    let bytes = serde_json::to_vec_pretty(catalog).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(ServerStoreError::Invalid(
            "Server store exceeds its 4 MiB limit".to_string(),
        ));
    }
    Ok(bytes)
}

fn check_version(bytes: &[u8]) -> Result<u64, ServerStoreError> {
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(ServerStoreError::Corrupt);
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) if version == 1 || version == u64::from(SERVER_SCHEMA_VERSION) => Ok(version),
        Some(_) => Err(ServerStoreError::UnsupportedVersion),
        None => Err(ServerStoreError::Corrupt),
    }
}

fn find_revision(
    catalog: &ServerCatalog,
    id: &ServerInstanceId,
    expected: u64,
) -> Result<usize, ServerStoreError> {
    let index = catalog
        .instances
        .iter()
        .position(|instance| &instance.id == id)
        .ok_or(ServerStoreError::NotFound)?;
    if catalog.instances[index].revision != expected {
        return Err(ServerStoreError::StaleRevision);
    }
    Ok(index)
}

fn next_revision(revision: u64) -> Result<u64, ServerStoreError> {
    revision
        .checked_add(1)
        .ok_or_else(|| ServerStoreError::Invalid("Server revision limit reached".to_string()))
}

pub(super) fn check_directory_chain(path: &Path) -> Result<(), ServerStoreError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(ServerStoreError::UnsafePath);
        }
        current.push(component);
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {}
            Ok(_) => return Err(ServerStoreError::UnsafePath),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(ServerStoreError::Io),
        }
    }
    Ok(())
}

pub(super) fn check_regular_file(path: &Path) -> Result<(), ServerStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => Ok(()),
        Ok(_) => Err(ServerStoreError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ServerStoreError::Io),
    }
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests;
