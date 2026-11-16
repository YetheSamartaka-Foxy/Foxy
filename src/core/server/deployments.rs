use super::{
    ConfigurationPreview, ServerContext, ServerInstallation, ServerInstance, ServerInstanceId,
    ServerStore, ServerStoreError,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationRevisions {
    pub schema_version: u32,
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub active: Option<String>,
    pub revisions: Vec<String>,
}

#[derive(Clone)]
pub struct ConfigurationCommitPlan {
    context: ServerContext,
    instance: ServerInstance,
    installation: Option<ServerInstallation>,
    before: Option<blake3::Hash>,
    pub preview: ConfigurationPreview,
    pub expected_generation: u64,
    pub revision_id: String,
}

impl ConfigurationCommitPlan {
    pub fn destination_directory(&self) -> PathBuf {
        self.context
            .space_dir()
            .join("server/instances")
            .join(self.instance.id.as_str())
            .join("configuration")
    }
}

#[derive(Clone)]
pub struct ConfigurationRollbackPlan {
    context: ServerContext,
    instance: ServerInstance,
    before: blake3::Hash,
    pub expected_generation: u64,
    pub revision_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigurationVerification {
    pub revision_id: String,
    pub valid: bool,
    pub issues: Vec<String>,
    pub launch_ready: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActiveConfigurationEvidence {
    pub status: &'static str,
    pub generation: Option<u64>,
    pub revision_id: Option<String>,
    pub integrity_valid: Option<bool>,
    pub matches_current_draft: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema_version: u32,
    game_space_id: String,
    instance: ServerInstance,
    preview: ConfigurationPreview,
}

impl ServerStore {
    pub(super) fn active_configuration_evidence(
        &self,
        instance: &ServerInstance,
    ) -> ActiveConfigurationEvidence {
        let mut evidence = ActiveConfigurationEvidence {
            status: "unreadable",
            generation: None,
            revision_id: None,
            integrity_valid: None,
            matches_current_draft: None,
        };
        let Ok((state, before)) = self.read_configuration_state(&instance.id) else {
            return evidence;
        };
        evidence.generation = Some(state.generation);
        let Some(revision) = &state.active else {
            evidence.status = "not_selected";
            return evidence;
        };
        evidence.revision_id = Some(revision.clone());
        let verified = self.read_verified_configuration(&instance.id, revision);
        let unchanged = self
            .read_configuration_state(&instance.id)
            .is_ok_and(|(after, hash)| after == state && hash == before);
        if !unchanged {
            evidence.status = "changed_during_inspection";
            return evidence;
        }
        evidence.integrity_valid = Some(verified.is_ok());
        evidence.status = if verified.is_ok() {
            "verified_configuration_only"
        } else {
            "invalid"
        };
        if verified.is_ok() {
            evidence.matches_current_draft = super::configuration::render(instance)
                .ok()
                .map(|preview| revision == &format!("config_{}", preview.fingerprint));
        }
        evidence
    }

    pub fn configuration_revisions(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ConfigurationRevisions, ServerStoreError> {
        if !self.capabilities().configuration_snapshots {
            return Err(ServerStoreError::UnsupportedGame);
        }
        self.show(id)?;
        self.read_configuration_state(id).map(|(state, _)| state)
    }

    pub fn plan_configuration_commit(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ConfigurationCommitPlan, ServerStoreError> {
        if !self.capabilities().configuration_snapshots {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let catalog = self.load()?;
        let instance = catalog
            .instances
            .into_iter()
            .find(|instance| &instance.id == id)
            .ok_or(ServerStoreError::NotFound)?;
        let installation = instance.installation_id.as_ref().and_then(|id| {
            catalog
                .installations
                .into_iter()
                .find(|item| &item.id == id)
        });
        if let Some(installation) = &installation {
            self.check_installation_root(installation)?;
            installation.verify()?;
        }
        let preview = super::configuration::render(&instance)?;
        let (state, before) = self.read_configuration_state(id)?;
        Ok(ConfigurationCommitPlan {
            context: self.context().clone(),
            instance,
            installation,
            before,
            expected_generation: state.generation,
            revision_id: format!("config_{}", preview.fingerprint),
            preview,
        })
    }

    pub fn apply_configuration_commit(
        &self,
        plan: ConfigurationCommitPlan,
    ) -> Result<ConfigurationRevisions, ServerStoreError> {
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok(self.commit_configuration_owned(plan))
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    fn commit_configuration_owned(
        &self,
        plan: ConfigurationCommitPlan,
    ) -> Result<ConfigurationRevisions, ServerStoreError> {
        self.validate_configuration_context(&plan.instance)?;
        let (mut state, fingerprint) = self.read_configuration_state(&plan.instance.id)?;
        if state.generation != plan.expected_generation
            || fingerprint != plan.before
            || super::configuration::render(&plan.instance)? != plan.preview
            || plan.revision_id != format!("config_{}", plan.preview.fingerprint)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let catalog = self.load()?;
        let installation = plan
            .instance
            .installation_id
            .as_ref()
            .and_then(|id| catalog.installations.iter().find(|item| &item.id == id));
        if installation != plan.installation.as_ref() {
            return Err(ServerStoreError::StaleRevision);
        }
        if let Some(installation) = installation {
            self.check_installation_root(installation)?;
            installation.verify()?;
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(ServerStoreError::StaleRevision)?;
        let directory = self
            .configuration_directory(&plan.instance.id)
            .join("revisions");
        super::store::check_directory_chain(&directory)?;
        fs::create_dir_all(&directory).map_err(|_| ServerStoreError::Io)?;
        super::store::check_directory_chain(&directory)?;
        let target = directory.join(&plan.revision_id);
        if target.exists() {
            self.read_verified_configuration(&plan.instance.id, &plan.revision_id)?;
        } else {
            let staging = tempfile::Builder::new()
                .prefix(".config-stage-")
                .tempdir_in(&directory)
                .map_err(|_| ServerStoreError::Io)?;
            for file in &plan.preview.files {
                write_new(
                    &staging.path().join(&file.relative_path),
                    file.contents.as_bytes(),
                )?;
            }
            let bundle = Bundle {
                schema_version: 1,
                game_space_id: self.context().space_id().into(),
                instance: plan.instance.clone(),
                preview: plan.preview,
            };
            write_new(
                &staging.path().join("manifest.json"),
                &serde_json::to_vec_pretty(&bundle).map_err(|_| ServerStoreError::Corrupt)?,
            )?;
            super::store::check_directory_chain(&target)?;
            fs::rename(staging.path(), &target).map_err(|_| ServerStoreError::Io)?;
            self.read_verified_configuration(&plan.instance.id, &plan.revision_id)?;
        }
        if self.read_configuration_state(&plan.instance.id)?.1 != plan.before {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_configuration_context(&plan.instance)?;
        if !state.revisions.contains(&plan.revision_id) {
            state.revisions.push(plan.revision_id.clone());
        }
        state.active = Some(plan.revision_id);
        self.write_configuration_state(&state)?;
        Ok(state)
    }

    pub fn verify_configuration(
        &self,
        id: &ServerInstanceId,
        revision: &str,
    ) -> Result<ConfigurationVerification, ServerStoreError> {
        let state = self.configuration_revisions(id)?;
        if !valid_revision_id(revision) {
            return Err(ServerStoreError::Invalid(
                "Invalid configuration revision id".into(),
            ));
        }
        if !state.revisions.iter().any(|item| item == revision) {
            return Err(ServerStoreError::NotFound);
        }
        let issues = self
            .read_verified_configuration(id, revision)
            .err()
            .map(|error| vec![error.to_string()])
            .unwrap_or_default();
        Ok(ConfigurationVerification {
            revision_id: revision.into(),
            valid: issues.is_empty(),
            issues,
            launch_ready: false,
        })
    }

    pub fn plan_configuration_rollback(
        &self,
        id: &ServerInstanceId,
        revision: &str,
    ) -> Result<ConfigurationRollbackPlan, ServerStoreError> {
        if !self.capabilities().configuration_rollback {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let instance = self.show(id)?;
        let (state, before) = self.read_configuration_state(id)?;
        if !valid_revision_id(revision) {
            return Err(ServerStoreError::Invalid(
                "Invalid configuration revision id".into(),
            ));
        }
        if !state.revisions.iter().any(|item| item == revision) {
            return Err(ServerStoreError::NotFound);
        }
        self.read_verified_configuration(id, revision)?;
        Ok(ConfigurationRollbackPlan {
            context: self.context().clone(),
            instance,
            before: before.ok_or(ServerStoreError::StaleRevision)?,
            expected_generation: state.generation,
            revision_id: revision.into(),
        })
    }

    pub fn apply_configuration_rollback(
        &self,
        plan: ConfigurationRollbackPlan,
    ) -> Result<ConfigurationRevisions, ServerStoreError> {
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok(self.rollback_configuration_owned(plan))
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    fn rollback_configuration_owned(
        &self,
        plan: ConfigurationRollbackPlan,
    ) -> Result<ConfigurationRevisions, ServerStoreError> {
        self.validate_configuration_context(&plan.instance)?;
        let (mut state, before) = self.read_configuration_state(&plan.instance.id)?;
        if before != Some(plan.before)
            || state.generation != plan.expected_generation
            || !state.revisions.contains(&plan.revision_id)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        self.read_verified_configuration(&plan.instance.id, &plan.revision_id)?;
        state.active = Some(plan.revision_id);
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or(ServerStoreError::StaleRevision)?;
        self.write_configuration_state(&state)?;
        Ok(state)
    }

    fn validate_configuration_context(
        &self,
        instance: &ServerInstance,
    ) -> Result<(), ServerStoreError> {
        let current =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if current.game_id() != self.context().game_id() || self.show(&instance.id)? != *instance {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }

    fn configuration_directory(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("configuration")
    }

    fn read_configuration_state(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(ConfigurationRevisions, Option<blake3::Hash>), ServerStoreError> {
        self.read_configuration_state_controlled(id, &super::ContentStageControl::default())
    }

    pub(super) fn read_configuration_state_controlled(
        &self,
        id: &ServerInstanceId,
        control: &super::ContentStageControl,
    ) -> Result<(ConfigurationRevisions, Option<blake3::Hash>), ServerStoreError> {
        control.checkpoint()?;
        let path = self.configuration_directory(id).join("state.json");
        super::store::check_directory_chain(path.parent().unwrap())?;
        super::store::check_regular_file(&path)?;
        let bytes = if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok((
                ConfigurationRevisions {
                    schema_version: 1,
                    game_space_id: self.context().space_id().into(),
                    instance_id: id.clone(),
                    generation: 0,
                    active: None,
                    revisions: vec![],
                },
                None,
            ));
        } else {
            super::content_staging::read_bounded_controlled(&path, 4 * 1024 * 1024, control)?
        };
        let state: ConfigurationRevisions =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if state.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let unique = state.revisions.iter().collect::<HashSet<_>>();
        if state.game_space_id != self.context().space_id()
            || &state.instance_id != id
            || unique.len() != state.revisions.len()
            || state
                .revisions
                .iter()
                .any(|revision| !valid_revision_id(revision))
            || state
                .active
                .as_ref()
                .is_some_and(|active| !unique.contains(active))
            || state.active.is_none() != state.revisions.is_empty()
            || state.generation < state.revisions.len() as u64
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok((state, Some(blake3::hash(&bytes))))
    }

    fn write_configuration_state(
        &self,
        state: &ConfigurationRevisions,
    ) -> Result<(), ServerStoreError> {
        let directory = self.configuration_directory(&state.instance_id);
        super::store::check_directory_chain(&directory)?;
        let path = directory.join("state.json");
        super::store::check_regular_file(&path)?;
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(ServerStoreError::Invalid(
                "Configuration revision index exceeds 4 MiB".into(),
            ));
        }
        let mut temporary =
            tempfile::NamedTempFile::new_in(&directory).map_err(|_| ServerStoreError::Io)?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        temporary.persist(path).map_err(|_| ServerStoreError::Io)?;
        Ok(())
    }

    fn read_verified_configuration(
        &self,
        id: &ServerInstanceId,
        revision: &str,
    ) -> Result<ConfigurationPreview, ServerStoreError> {
        self.read_verified_configuration_controlled(
            id,
            revision,
            &super::ContentStageControl::default(),
        )
        .map(|(_, preview, _)| preview)
    }

    pub(super) fn read_verified_configuration_controlled(
        &self,
        id: &ServerInstanceId,
        revision: &str,
        control: &super::ContentStageControl,
    ) -> Result<(ServerInstance, ConfigurationPreview, String), ServerStoreError> {
        self.read_verified_configuration_at(
            id,
            revision,
            &self
                .configuration_directory(id)
                .join("revisions")
                .join(revision),
            control,
        )
    }

    pub(super) fn read_verified_configuration_at(
        &self,
        id: &ServerInstanceId,
        revision: &str,
        directory: &Path,
        control: &super::ContentStageControl,
    ) -> Result<(ServerInstance, ConfigurationPreview, String), ServerStoreError> {
        control.checkpoint()?;
        if !valid_revision_id(revision) {
            return Err(ServerStoreError::Invalid(
                "Invalid configuration revision id".into(),
            ));
        }
        super::store::check_directory_chain(directory)?;
        let manifest = directory.join("manifest.json");
        super::store::check_regular_file(&manifest)?;
        let manifest_bytes =
            super::content_staging::read_bounded_controlled(&manifest, 4 * 1024 * 1024, control)?;
        let bundle: Bundle =
            serde_json::from_slice(&manifest_bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if bundle.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let kind = crate::core::game::registry()
            .get(self.context().game_id())
            .and_then(|module| module.server_capabilities().definition_kind)
            .ok_or(ServerStoreError::UnsupportedGame)?;
        bundle
            .instance
            .validate(self.context().space_id(), kind)
            .map_err(|_| ServerStoreError::Corrupt)?;
        if bundle.game_space_id != self.context().space_id()
            || &bundle.instance.id != id
            || bundle.preview != super::configuration::render(&bundle.instance)?
            || revision != format!("config_{}", bundle.preview.fingerprint)
        {
            return Err(ServerStoreError::Corrupt);
        }
        let expected = bundle
            .preview
            .files
            .iter()
            .map(|file| file.relative_path.as_str())
            .chain(std::iter::once("manifest.json"))
            .collect::<HashSet<_>>();
        let mut count = 0;
        for entry in fs::read_dir(directory).map_err(|_| ServerStoreError::Io)? {
            control.checkpoint()?;
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            count += 1;
            if count > expected.len()
                || !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| expected.contains(name))
            {
                return Err(ServerStoreError::Corrupt);
            }
        }
        if count != expected.len() {
            return Err(ServerStoreError::Corrupt);
        }
        for file in &bundle.preview.files {
            let path = directory.join(&file.relative_path);
            super::store::check_regular_file(&path)?;
            let contents =
                super::content_staging::read_bounded_controlled(&path, 4 * 1024 * 1024, control)?;
            if contents != file.contents.as_bytes() {
                return Err(ServerStoreError::Corrupt);
            }
        }
        if super::content_staging::read_bounded_controlled(&manifest, 4 * 1024 * 1024, control)?
            != manifest_bytes
        {
            return Err(ServerStoreError::StaleRevision);
        }
        control.checkpoint()?;
        Ok((
            bundle.instance,
            bundle.preview,
            blake3::hash(&manifest_bytes).to_hex().to_string(),
        ))
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ServerStoreError> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| ServerStoreError::Io)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ServerStoreError::Io)
}

fn valid_revision_id(id: &str) -> bool {
    id.strip_prefix("config_").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests;
