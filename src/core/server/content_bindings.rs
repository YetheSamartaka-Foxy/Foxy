use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::addons::order_bindings;
use super::content_sources::{AddonFolder, ContentSources, RepositoryProfile};
use super::{
    ContentDeploymentPlan, ContentPlanRequest, ServerAddonBinding, ServerContext, ServerInstanceId,
    ServerStore, ServerStoreError,
};

const SCHEMA_VERSION: u32 = 1;
pub(super) const CONTENT_BINDINGS_FILE: &str = "content-bindings.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedContentBinding {
    pub binding: ServerAddonBinding,
    pub profile: RepositoryProfile,
    pub folder: AddonFolder,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentBindingsDocument {
    pub schema_version: u32,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub instance_revision: u64,
    pub catalog_revision: u64,
    pub catalog_fingerprint: String,
    pub content_sources_fingerprint: Option<String>,
    pub bindings: Vec<ReviewedContentBinding>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentBindingsSnapshot {
    pub document: Option<ContentBindingsDocument>,
    pub fingerprint: Option<String>,
    pub review_current: bool,
    pub issues: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentBindingsPlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    request: ContentPlanRequest,
    #[serde(skip)]
    before: Option<String>,
    pub document: ContentBindingsDocument,
    pub preview: ContentDeploymentPlan,
    pub fingerprint: String,
    pub expected_generation: u64,
}

impl ServerStore {
    pub fn content_bindings(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ContentBindingsSnapshot, ServerStoreError> {
        self.validate_bindings_context()?;
        let (catalog, catalog_hash, _) = self.read()?;
        let instance = catalog
            .instances
            .iter()
            .find(|instance| &instance.id == id)
            .ok_or(ServerStoreError::NotFound)?;
        let sources = self.content_sources()?;
        let (document, fingerprint) = self.read_content_bindings(id)?;
        let catalog_fingerprint = catalog_hash.map(|hash| hash.to_hex().to_string());
        let mut issues = Vec::new();
        if let Some(document) = &document {
            if document.instance_revision != instance.revision
                || document.catalog_revision != catalog.revision
                || Some(document.catalog_fingerprint.as_str()) != catalog_fingerprint.as_deref()
            {
                issues.push("The saved content bindings were reviewed against a different server catalog. Review them again before saving.");
            }
            if document.content_sources_fingerprint != sources.fingerprint {
                issues.push("Content source registrations changed since these bindings were reviewed. Review their identities and paths again.");
            }
            if document.bindings.iter().any(|item| {
                sources
                    .sources
                    .profiles
                    .iter()
                    .find(|profile| profile.name == item.profile.name)
                    != Some(&item.profile)
                    || sources
                        .sources
                        .folders
                        .iter()
                        .find(|folder| folder.id == item.folder.id)
                        != Some(&item.folder)
            }) {
                issues.push("Saved content source identities differ from current registrations. Reload and review their paths again.");
            }
        }
        let (_, after_catalog_hash, _) = self.read()?;
        if after_catalog_hash != catalog_hash
            || self.content_sources()?.fingerprint != sources.fingerprint
            || self.read_content_bindings(id)?.1 != fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(ContentBindingsSnapshot {
            review_current: document.is_some() && issues.is_empty(),
            document,
            fingerprint,
            issues,
        })
    }

    pub fn plan_content_bindings(
        &self,
        request: ContentPlanRequest,
        expected_bindings_fingerprint: Option<&str>,
    ) -> Result<ContentBindingsPlan, ServerStoreError> {
        let before = self.content_bindings(&request.instance_id)?;
        if before.fingerprint.as_deref() != expected_bindings_fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let expected_generation = before.document.as_ref().map_or(0, |item| item.generation);
        let generation = expected_generation.checked_add(1).ok_or_else(|| {
            ServerStoreError::Invalid("Content binding generation limit reached".into())
        })?;
        let (_, catalog_hash, _) = self.read()?;
        let sources = self.content_sources()?;
        if sources.fingerprint != request.content_sources_fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let ordered = order_bindings(request.bindings.clone()).map_err(|_| {
            ServerStoreError::Invalid("Content plan contains invalid or duplicate bindings".into())
        })?;
        let mut bindings = Vec::with_capacity(ordered.len());
        let mut binding_bytes = 0usize;
        for binding in ordered {
            let profile = sources
                .sources
                .profiles
                .iter()
                .find(|profile| profile.name == binding.source_id)
                .ok_or(ServerStoreError::StaleRevision)?;
            let folder = sources
                .sources
                .folders
                .iter()
                .find(|folder| folder.id == profile.folder_id)
                .ok_or(ServerStoreError::Corrupt)?;
            binding_bytes = binding_bytes.saturating_add(
                serde_json::to_vec(&(&binding, profile, folder))
                    .map_err(|_| ServerStoreError::Corrupt)?
                    .len(),
            );
            if binding_bytes > 4 * 1024 * 1024 {
                return Err(ServerStoreError::Invalid(
                    "Content bindings exceed 4 MiB".into(),
                ));
            }
            bindings.push(ReviewedContentBinding {
                binding,
                profile: profile.clone(),
                folder: folder.clone(),
            });
        }
        let document = ContentBindingsDocument {
            schema_version: SCHEMA_VERSION,
            game_space_id: self.context().space_id().into(),
            game_id: self.context().game_id().into(),
            instance_id: request.instance_id.clone(),
            generation,
            instance_revision: request.expected_instance_revision,
            catalog_revision: request.expected_catalog_revision,
            catalog_fingerprint: catalog_hash
                .ok_or(ServerStoreError::NotFound)?
                .to_hex()
                .to_string(),
            content_sources_fingerprint: sources.fingerprint.clone(),
            bindings,
        };
        self.validate_content_bindings(&request.instance_id, &document)?;
        if serde_json::to_vec_pretty(&document)
            .map_err(|_| ServerStoreError::Corrupt)?
            .len()
            > 4 * 1024 * 1024
        {
            return Err(ServerStoreError::Invalid(
                "Content bindings exceed 4 MiB".into(),
            ));
        }
        let preview = self.plan_content(request.clone())?;
        let (_, after_catalog_hash, _) = self.read()?;
        if after_catalog_hash != catalog_hash
            || self.content_sources()?.fingerprint != sources.fingerprint
            || self.read_content_bindings(&request.instance_id)?.1 != before.fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let identity = serde_json::to_vec(&(&document, &before.fingerprint, &preview.fingerprint))
            .map_err(|_| ServerStoreError::Corrupt)?;
        Ok(ContentBindingsPlan {
            context: self.context().clone(),
            request,
            before: before.fingerprint,
            document,
            preview,
            fingerprint: blake3::hash(&identity).to_hex().to_string(),
            expected_generation,
        })
    }

    pub fn apply_content_bindings(
        &self,
        plan: ContentBindingsPlan,
    ) -> Result<ContentBindingsSnapshot, ServerStoreError> {
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                self.validate_bindings_context()?;
                if self.context() != &plan.context {
                    return Err(ServerStoreError::StaleRevision);
                }
                let fresh = self.plan_content_bindings(plan.request, plan.before.as_deref())?;
                if fresh.fingerprint != plan.fingerprint
                    || fresh.document != plan.document
                    || fresh.expected_generation != plan.expected_generation
                    || fresh.preview.fingerprint != plan.preview.fingerprint
                {
                    return Err(ServerStoreError::StaleRevision);
                }
                self.write_content_bindings(&fresh.document, fresh.before.as_deref())?;
                let bytes = serde_json::to_vec_pretty(&fresh.document)
                    .map_err(|_| ServerStoreError::Corrupt)?;
                Ok(ContentBindingsSnapshot {
                    document: Some(fresh.document),
                    fingerprint: Some(blake3::hash(&bytes).to_hex().to_string()),
                    review_current: true,
                    issues: vec![],
                })
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    fn validate_bindings_context(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().content_bindings {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let current =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &current != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }

    fn binding_path(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join(CONTENT_BINDINGS_FILE)
    }

    pub(in crate::core::server) fn read_content_bindings(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(Option<ContentBindingsDocument>, Option<String>), ServerStoreError> {
        let path = self.binding_path(id);
        super::store::check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
        super::store::check_regular_file(&path)?;
        let bytes = match super::read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
            Err(_) => return Err(ServerStoreError::Io),
        };
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
        }
        let version: Version =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if version.schema_version != SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let document: ContentBindingsDocument =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        self.validate_content_bindings(id, &document)?;
        Ok((
            Some(document),
            Some(blake3::hash(&bytes).to_hex().to_string()),
        ))
    }

    pub(super) fn validate_content_bindings(
        &self,
        id: &ServerInstanceId,
        document: &ContentBindingsDocument,
    ) -> Result<(), ServerStoreError> {
        if document.schema_version != SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        if document.game_space_id != self.context().space_id()
            || document.game_id != self.context().game_id()
            || &document.instance_id != id
            || document.generation == 0
            || document.instance_revision == 0
            || document.catalog_revision == 0
            || !valid_fingerprint(&document.catalog_fingerprint)
            || document
                .content_sources_fingerprint
                .as_ref()
                .is_some_and(|fingerprint| !valid_fingerprint(fingerprint))
            || (!document.bindings.is_empty() && document.content_sources_fingerprint.is_none())
            || document.bindings.len() > 256
        {
            return Err(ServerStoreError::Corrupt);
        }
        let input = document
            .bindings
            .iter()
            .map(|item| item.binding.clone())
            .collect::<Vec<_>>();
        if order_bindings(input.clone()).map_err(|_| ServerStoreError::Corrupt)? != input {
            return Err(ServerStoreError::Corrupt);
        }
        let mut sources = ContentSources::default();
        for item in &document.bindings {
            if item.binding.source_id != item.profile.name
                || item.profile.folder_id != item.folder.id
            {
                return Err(ServerStoreError::Corrupt);
            }
            if let Some(existing) = sources
                .folders
                .iter()
                .find(|folder| folder.id == item.folder.id)
            {
                if existing != &item.folder {
                    return Err(ServerStoreError::Corrupt);
                }
            } else {
                sources
                    .add_folder(&item.folder.id, &item.folder.path)
                    .map_err(|_| ServerStoreError::Corrupt)?;
                if sources.folders.last() != Some(&item.folder) {
                    return Err(ServerStoreError::Corrupt);
                }
            }
            sources
                .add_profile(item.profile.clone())
                .map_err(|_| ServerStoreError::Corrupt)?;
            if sources.profiles.last() != Some(&item.profile) {
                return Err(ServerStoreError::Corrupt);
            }
        }
        Ok(())
    }

    fn write_content_bindings(
        &self,
        document: &ContentBindingsDocument,
        before: Option<&str>,
    ) -> Result<(), ServerStoreError> {
        let path = self.binding_path(&document.instance_id);
        let (catalog, _, _) = self.read()?;
        if catalog
            .installations
            .iter()
            .any(|installation| super::installation::paths_overlap(&installation.directory, &path))
        {
            return Err(ServerStoreError::UnsafePath);
        }
        let bytes = serde_json::to_vec_pretty(document).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(ServerStoreError::Invalid(
                "Content bindings exceed 4 MiB".into(),
            ));
        }
        let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
        super::store::check_directory_chain(parent)?;
        super::store::check_regular_file(&path)?;
        fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
        super::store::check_directory_chain(parent)?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| ServerStoreError::Io)?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        if self
            .read_content_bindings(&document.instance_id)?
            .1
            .as_deref()
            != before
        {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_bindings_context()?;
        super::store::check_directory_chain(parent)?;
        super::store::check_regular_file(&path)?;
        temporary.persist(&path).map_err(|_| ServerStoreError::Io)?;
        Ok(())
    }
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests;
