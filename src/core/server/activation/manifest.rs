use std::path::{Path, PathBuf};

use foxy_supervisor::session::SessionIdentity;
use serde::de::DeserializeOwned;

use super::{
    ActivationDestinations, ActivationError, ActivationJournal, ActivationState, MAX_BYTES,
    PreparedReference,
};
use crate::core::server::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ActivationError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_BYTES {
        return Err(ServerStoreError::Corrupt.into());
    }
    Ok(bytes)
}

pub(super) fn fingerprint<T: serde::Serialize>(value: &T) -> Result<String, ActivationError> {
    Ok(blake3::hash(&encode(value)?).to_hex().to_string())
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn installation_key(value: &str) -> bool {
    value.strip_prefix("v1_").is_some_and(|key| {
        key.len() == 49
            && key.as_bytes()[16] == b'_'
            && key
                .bytes()
                .enumerate()
                .all(|(i, byte)| i == 16 || byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

pub(super) fn validate_owner(
    owner: &SessionIdentity,
    space: &str,
    id: &ServerInstanceId,
) -> Result<(), ActivationError> {
    if owner.owner_generation == 0
        || owner.run_generation == 0
        || owner.game_space_id != space
        || owner.instance_id != id.as_str()
        || ![
            &owner.owner_id,
            &owner.operation_id,
            &owner.game_space_id,
            &owner.instance_id,
        ]
        .into_iter()
        .all(|value| identifier(value))
    {
        return Err(ActivationError::OwnerMismatch);
    }
    Ok(())
}

pub(super) fn destinations(journal: &ActivationJournal) -> ActivationDestinations {
    ActivationDestinations {
        active_state: "activation/active.json".into(),
        journal: "activation/journal.json".into(),
        candidate: journal
            .next
            .deployment
            .runtime
            .as_ref()
            .map(|runtime| format!("runtimes/{}", runtime.directory_name()))
            .unwrap_or_else(|| format!("prepared/{}", journal.next.deployment.preparation_id)),
        predecessor: journal.previous.as_ref().map(|state| {
            state
                .deployment
                .runtime
                .as_ref()
                .map(|runtime| format!("runtimes/{}", runtime.directory_name()))
                .unwrap_or_else(|| format!("prepared/{}", state.deployment.preparation_id))
        }),
    }
}

fn activation_context_root_bytes(root: &Path) -> Vec<u8> {
    #[cfg(windows)]
    {
        let text = root.to_string_lossy().replace('/', "\\");
        let normalized = if text.starts_with("\\\\?\\") {
            text
        } else if let Some(tail) = text.strip_prefix("\\\\") {
            format!("\\\\?\\UNC\\{tail}")
        } else {
            format!("\\\\?\\{text}")
        };
        std::ffi::OsStr::new(&normalized)
            .as_encoded_bytes()
            .to_vec()
    }
    #[cfg(not(windows))]
    {
        root.as_os_str().as_encoded_bytes().to_vec()
    }
}

impl ServerStore {
    pub(in crate::core::server) fn activation_context_fingerprint(&self) -> String {
        let mut hash = blake3::Hasher::new_derive_key("foxy.server.activation.context.v1");
        for bytes in [
            activation_context_root_bytes(self.context().data_root()).as_slice(),
            self.context().space_id().as_bytes(),
            self.context().game_id().as_bytes(),
        ] {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        hash.finalize().to_hex().to_string()
    }

    pub(super) fn activation_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.record_root(id).join("activation")
    }

    pub(super) fn validate_activation_state(
        &self,
        id: &ServerInstanceId,
        state: &ActivationState,
    ) -> Result<(), ActivationError> {
        if state.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion.into());
        }
        validate_owner(&state.owner, self.context().space_id(), id)?;
        if state.context_fingerprint != self.activation_context_fingerprint()
            || state.game_space_id != self.context().space_id()
            || state.game_id != self.context().game_id()
            || &state.instance_id != id
            || state.generation == 0
            || !installation_key(&state.installation_key)
            || !hash(&state.deployment.manifest_fingerprint)
            || state.deployment.preparation_id
                != format!("prepared_{}", state.deployment.manifest_fingerprint)
            || !state
                .deployment
                .record_id
                .strip_prefix("deployment_")
                .is_some_and(hash)
            || state.deployment.runtime.as_ref().is_some_and(|runtime| {
                !hash(&runtime.manifest_fingerprint)
                    || runtime.runtime_id != format!("runtime_{}", runtime.manifest_fingerprint)
            })
        {
            return Err(ServerStoreError::Corrupt.into());
        }
        Ok(())
    }

    pub(super) fn validate_activation_journal(
        &self,
        id: &ServerInstanceId,
        journal: &ActivationJournal,
    ) -> Result<(), ActivationError> {
        if journal.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion.into());
        }
        self.validate_activation_state(id, &journal.next)?;
        if let Some(previous) = &journal.previous {
            self.validate_activation_state(id, previous)?;
        }
        let previous_generation = journal
            .previous
            .as_ref()
            .map_or(0, |state| state.generation);
        if journal.context_fingerprint != self.activation_context_fingerprint()
            || journal.game_space_id != self.context().space_id()
            || journal.game_id != self.context().game_id()
            || &journal.instance_id != id
            || journal.owner != journal.next.owner
            || !installation_key(&journal.installation_key)
            || journal.next.installation_key != journal.installation_key
            || journal
                .previous
                .as_ref()
                .is_some_and(|state| state.installation_key != journal.installation_key)
            || !hash(&journal.catalog_fingerprint)
            || !hash(&journal.selection_fingerprint)
            || journal.selection_generation == 0
            || journal.previous.is_some() != journal.previous_fingerprint.is_some()
            || journal
                .previous_fingerprint
                .as_deref()
                .is_some_and(|value| !hash(value))
            || previous_generation.checked_add(1) != Some(journal.next.generation)
            || journal.destinations != destinations(journal)
        {
            return Err(ServerStoreError::Corrupt.into());
        }
        encode(journal)?;
        Ok(())
    }

    pub(super) fn read_activation_document<T: DeserializeOwned>(
        &self,
        id: &ServerInstanceId,
        name: &str,
        control: &ContentStageControl,
    ) -> Result<Option<(T, String)>, ActivationError> {
        let root = self.activation_root(id);
        super::super::store::check_directory_chain(&root)?;
        let path = root.join(name);
        super::super::store::check_directory_chain(
            path.parent().ok_or(ServerStoreError::UnsafePath)?,
        )?;
        super::super::store::check_regular_file(&path)?;
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok(None);
        }
        let bytes =
            super::super::content_staging::read_bounded_controlled(&path, MAX_BYTES, control)?;
        #[derive(serde::Deserialize)]
        struct Version {
            schema_version: u32,
        }
        let version: Version =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if version.schema_version != 1 {
            return Err(ServerStoreError::UnsupportedVersion.into());
        }
        let document = serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        Ok(Some((document, blake3::hash(&bytes).to_hex().to_string())))
    }

    pub(in crate::core::server) fn read_activation_state(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<Option<(ActivationState, String)>, ActivationError> {
        let value = self.read_activation_document(id, "active.json", control)?;
        if let Some((state, _)) = &value {
            self.validate_activation_state(id, state)?;
        }
        Ok(value)
    }

    pub(in crate::core::server) fn settled_activation_state(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<(ActivationState, String), ActivationError> {
        if self
            .read_activation_document::<ActivationJournal>(id, "journal.json", control)?
            .is_some()
        {
            return Err(ActivationError::RecoveryRequired);
        }
        self.read_activation_state(id, control)?
            .ok_or_else(|| ServerStoreError::NotFound.into())
    }

    pub(in crate::core::server) fn verify_activation_reference(
        &self,
        id: &ServerInstanceId,
        reference: &PreparedReference,
        control: &ContentStageControl,
    ) -> Result<(), ActivationError> {
        self.verify_preparation_reference(id, reference, control)?;
        if let Some(runtime) = &reference.runtime {
            let manifest = self.verify_native_runtime(id, runtime, control)?;
            if manifest.preparation_id != reference.preparation_id {
                return Err(ServerStoreError::Corrupt.into());
            }
        }
        Ok(())
    }

    pub(super) fn verify_preparation_reference(
        &self,
        id: &ServerInstanceId,
        reference: &PreparedReference,
        control: &ContentStageControl,
    ) -> Result<(), ActivationError> {
        let report = self.verify_deployment_preparation(id, &reference.preparation_id, control)?;
        if report.manifest.record_id != reference.record_id
            || report.preparation_id != format!("prepared_{}", reference.manifest_fingerprint)
        {
            return Err(ServerStoreError::Corrupt.into());
        }
        self.check_deployment_destination(
            &self.activation_root(id),
            Some(&report.manifest.record.content_bindings),
        )?;
        Ok(())
    }
}
