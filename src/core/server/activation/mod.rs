#![cfg_attr(not(test), allow(dead_code))]

mod inspection;
mod manifest;
mod resolution;
mod store;

use foxy_supervisor::leases::{InstallationLocks, MaintenanceLease};
use foxy_supervisor::maintenance::{
    MaintenanceIntent, MaintenanceReceipt, MaintenanceRecoveryLease, MaintenanceSnapshot,
    ResolutionOutcome,
};
use foxy_supervisor::session::SessionIdentity;
use serde::{Deserialize, Serialize};

use super::{ServerContext, ServerInstanceId, ServerStore, ServerStoreError};
use crate::core::tasks::db_process_lock::HeldClaimPin;

const MAX_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActivationError {
    Store(ServerStoreError),
    Unavailable,
    RecoveryRequired,
    OwnerMismatch,
    Interrupted,
}

impl From<ServerStoreError> for ActivationError {
    fn from(value: ServerStoreError) -> Self {
        Self::Store(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparedReference {
    pub preparation_id: String,
    pub record_id: String,
    pub manifest_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<super::NativeRuntimeReference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationState {
    pub schema_version: u32,
    pub context_fingerprint: String,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub installation_key: String,
    pub deployment: PreparedReference,
    pub owner: SessionIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationDestinations {
    pub active_state: String,
    pub journal: String,
    pub candidate: String,
    pub predecessor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationJournal {
    pub schema_version: u32,
    pub context_fingerprint: String,
    pub game_space_id: String,
    pub game_id: String,
    pub instance_id: ServerInstanceId,
    pub owner: SessionIdentity,
    pub installation_key: String,
    pub catalog_fingerprint: String,
    pub selection_generation: u64,
    pub selection_fingerprint: String,
    pub previous: Option<ActivationState>,
    pub previous_fingerprint: Option<String>,
    pub next: ActivationState,
    pub destinations: ActivationDestinations,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivationRequest {
    pub instance_id: ServerInstanceId,
    pub preparation_id: String,
    pub expected_selection_generation: u64,
    pub expected_activation_generation: u64,
    pub owner: SessionIdentity,
    pub installation_key: String,
    pub runtime: Option<super::NativeRuntimeReference>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivationPlan {
    context: ServerContext,
    proof: String,
    pub journal: ActivationJournal,
    pub fingerprint: String,
}

pub(crate) struct ActivationAdmission<'a> {
    context: ServerContext,
    _claim: HeldClaimPin,
    maintenance: MaintenanceLease<'a>,
    owner: SessionIdentity,
    installation: Option<super::ServerInstallation>,
}

impl ActivationAdmission<'_> {
    fn validate(
        &self,
        context: &ServerContext,
        journal: &ActivationJournal,
    ) -> Result<(), ActivationError> {
        if let Some(installation) = &self.installation {
            installation.verify()?;
        }
        if &self.context != context
            || self.owner != journal.owner
            || self
                .maintenance
                .installation_key()
                .map_err(|_| ServerStoreError::Ownership)?
                != journal.installation_key
        {
            return Err(ActivationError::OwnerMismatch);
        }
        Ok(())
    }
}

impl ServerStore {
    pub(in crate::core::server) fn admit_native_activation<'a>(
        &self,
        plan: &ActivationPlan,
        installation: &'a InstallationLocks,
        execution: &super::owner::DeploymentExecution,
        bound: &super::ServerInstallation,
        control: &super::ContentStageControl,
    ) -> Result<ActivationAdmission<'a>, ActivationError> {
        execution
            .validate(self.context())
            .map_err(|_| ServerStoreError::Ownership)?;
        let identity = execution.identity();
        if &plan.context != self.context()
            || plan.journal.next.deployment.runtime.is_none()
            || identity.target_instance != plan.journal.instance_id
            || identity.owner_id != plan.journal.owner.owner_id
            || identity.owner_generation != plan.journal.owner.owner_generation
            || identity.operation_id != plan.journal.owner.operation_id
            || identity.run_generation != plan.journal.owner.run_generation
            || self.native_installation(&plan.journal.instance_id)? != *bound
            || InstallationLocks::capture(&bound.directory)
                .and_then(|locks| locks.coordination_key())
                .map_err(|_| ServerStoreError::Ownership)?
                != plan.journal.installation_key
            || installation
                .coordination_key()
                .map_err(|_| ServerStoreError::Ownership)?
                != plan.journal.installation_key
        {
            return Err(ActivationError::OwnerMismatch);
        }
        let claim =
            crate::core::tasks::db_process_lock::pin_held_claim(&self.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
        let maintenance = installation
            .try_admit()
            .and_then(|admission| admission.try_maintenance().map_err(|(_, error)| error))
            .map_err(|_| ServerStoreError::Ownership)?;
        installation
            .revalidate()
            .map_err(|_| ServerStoreError::Ownership)?;
        bound.verify()?;
        control.checkpoint()?;
        Ok(ActivationAdmission {
            context: self.context().clone(),
            _claim: claim,
            maintenance,
            owner: plan.journal.owner.clone(),
            installation: Some(bound.clone()),
        })
    }

    pub(crate) fn admit_activation<'a>(
        &self,
        plan: &ActivationPlan,
        _installation: &'a InstallationLocks,
    ) -> Result<ActivationAdmission<'a>, ActivationError> {
        self.capabilities()
            .definition_kind
            .ok_or(ServerStoreError::UnsupportedGame)?;
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision.into());
        }
        Err(ActivationError::Unavailable)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryDisposition {
    NoJournal,
    PredecessorRetained,
    CandidateCommitted,
    Ambiguous,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivationRecovery {
    pub disposition: RecoveryDisposition,
    pub journal: Option<ActivationJournal>,
    pub active: Option<ActivationState>,
    pub selection_unchanged: Option<bool>,
    pub catalog_unchanged: Option<bool>,
    pub journal_retained: bool,
    pub protection_release_authorized: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransactionBoundary {
    BeforeJournal,
    AfterIntent,
    AfterJournal,
    BeforePointer,
    AfterPointer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationResolutionRecord {
    pub schema_version: u32,
    pub intent: MaintenanceSnapshot,
    pub journal: ActivationJournal,
    pub active: Option<ActivationState>,
    pub active_fingerprint: Option<String>,
    pub outcome: ResolutionOutcome,
    pub resolver: SessionIdentity,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActivationResolutionPlan {
    #[serde(skip)]
    context: ServerContext,
    #[serde(skip)]
    proof: String,
    pub record: ActivationResolutionRecord,
    pub fingerprint: String,
    pub resolver: SessionIdentity,
    pub journal_fingerprint: Option<String>,
    pub catalog_fingerprint: String,
    pub selection_fingerprint: Option<String>,
    pub receipt: Option<MaintenanceReceipt>,
}

pub(crate) struct ActivationRecoveryAdmission<'a> {
    context: ServerContext,
    _claim: HeldClaimPin,
    recovery: MaintenanceRecoveryLease<'a>,
    resolver: SessionIdentity,
    installation: Option<super::ServerInstallation>,
}

#[derive(Clone, Debug)]
pub(crate) struct ActivationResolutionReport {
    pub record: ActivationResolutionRecord,
    pub receipt: MaintenanceReceipt,
    pub journal_retired: bool,
    pub intent_retired: bool,
    pub engine_admission_authorized: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolutionBoundary {
    AfterRecord,
    AfterReceipt,
    AfterJournalRetire,
    BeforeIntentRetire,
    AfterIntentRetire,
}

pub(super) fn maintenance_intent(
    journal: &ActivationJournal,
) -> Result<MaintenanceIntent, ActivationError> {
    Ok(MaintenanceIntent {
        schema_version: 1,
        installation_key: journal.installation_key.clone(),
        context_fingerprint: journal.context_fingerprint.clone(),
        transaction_fingerprint: manifest::fingerprint(journal)?,
        transaction_evidence: String::from_utf8(manifest::encode(journal)?)
            .map_err(|_| ServerStoreError::Corrupt)?,
        previous_state_fingerprint: journal.previous_fingerprint.clone(),
        next_state_fingerprint: manifest::fingerprint(&journal.next)?,
        owner: journal.owner.clone(),
    })
}

#[cfg(test)]
impl<'a> ActivationRecoveryAdmission<'a> {
    pub(in crate::core::server) fn fixture(
        store: &ServerStore,
        installation: &'a InstallationLocks,
        resolver: SessionIdentity,
        expected: &MaintenanceSnapshot,
    ) -> Result<Self, ActivationError> {
        let claim =
            crate::core::tasks::db_process_lock::pin_held_claim(&store.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
        let recovery = installation
            .try_recover_maintenance(expected)
            .map_err(|_| ServerStoreError::Ownership)?;
        Ok(Self {
            context: store.context().clone(),
            _claim: claim,
            recovery,
            resolver,
            installation: None,
        })
    }
}

#[cfg(test)]
impl<'a> ActivationAdmission<'a> {
    pub(in crate::core::server) fn fixture(
        store: &ServerStore,
        installation: &'a InstallationLocks,
        owner: SessionIdentity,
    ) -> Result<Self, ActivationError> {
        let claim =
            crate::core::tasks::db_process_lock::pin_held_claim(&store.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
        let maintenance = installation
            .try_admit()
            .map_err(|_| ServerStoreError::Ownership)?
            .try_maintenance()
            .map_err(|_| ServerStoreError::Ownership)?;
        Ok(Self {
            context: store.context().clone(),
            _claim: claim,
            maintenance,
            owner,
            installation: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_example_preserves_strict_evidence_shape() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/activation_resolution.json"
        ))
        .unwrap();
        let record: ActivationResolutionRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(record.schema_version, 1);
        assert_eq!(serde_json::to_value(record).unwrap(), value);
        let mut invalid = value;
        invalid["unrecognized"] = true.into();
        assert!(serde_json::from_value::<ActivationResolutionRecord>(invalid).is_err());
    }

    #[test]
    fn journal_example_preserves_strict_nested_state_and_destination_shapes() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/activation_journal.json"
        ))
        .unwrap();
        let journal: ActivationJournal = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(journal.schema_version, 1);
        assert_eq!(journal.next.schema_version, 1);
        assert_eq!(serde_json::to_value(&journal).unwrap(), value);
        for target in ["root", "next", "destinations", "owner", "deployment"] {
            let mut invalid = value.clone();
            let object = match target {
                "root" => &mut invalid,
                "deployment" => &mut invalid["next"]["deployment"],
                name => &mut invalid[name],
            };
            object["unrecognized"] = true.into();
            assert!(serde_json::from_value::<ActivationJournal>(invalid).is_err());
        }
    }

    #[test]
    fn recovery_metadata_from_another_config_root_is_refused_with_matching_ids() {
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let store = |root: &std::path::Path| {
            std::fs::write(root.join("games.json"), serde_json::json!({
                "active_game_space_id":"alpha", "last_opened_game_space_id":"alpha",
                "game_spaces":[{"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0}]
            }).to_string()).unwrap();
            ServerStore::new(ServerContext::capture(root, Some("alpha")).unwrap()).unwrap()
        };
        let first = store(first_root.path());
        let second = store(second_root.path());
        let mut journal: ActivationJournal = serde_json::from_str(include_str!(
            "../../../../examples/json/server_backend/activation_journal.json"
        ))
        .unwrap();
        journal.context_fingerprint = first.activation_context_fingerprint();
        journal.next.context_fingerprint = journal.context_fingerprint.clone();
        let id = &journal.instance_id;
        first.validate_activation_journal(id, &journal).unwrap();
        let source = first.activation_root(id);
        let target = second.activation_root(id);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        for (name, bytes) in [
            ("journal.json", serde_json::to_vec(&journal).unwrap()),
            ("active.json", serde_json::to_vec(&journal.next).unwrap()),
        ] {
            std::fs::write(source.join(name), &bytes).unwrap();
            std::fs::copy(source.join(name), target.join(name)).unwrap();
        }
        let control = super::super::ContentStageControl::default();
        assert!(first.read_activation_state(id, &control).unwrap().is_some());
        assert_eq!(
            second.read_activation_state(id, &control).unwrap_err(),
            ActivationError::Store(ServerStoreError::Corrupt)
        );
        let copied: ActivationJournal = second
            .read_activation_document(id, "journal.json", &control)
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(copied, journal);
        assert_eq!(
            second.validate_activation_journal(id, &copied).unwrap_err(),
            ActivationError::Store(ServerStoreError::Corrupt)
        );
        for name in ["journal.json", "active.json"] {
            assert_eq!(
                std::fs::read(source.join(name)).unwrap(),
                std::fs::read(target.join(name)).unwrap()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_and_plain_config_roots_share_one_activation_context() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("games.json"),
            serde_json::json!({
                "active_game_space_id":"alpha", "last_opened_game_space_id":"alpha",
                "game_spaces":[{"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0}]
            })
            .to_string(),
        )
        .unwrap();
        let absolute = std::path::absolute(root.path()).unwrap();
        let text = absolute.to_string_lossy().replace('/', "\\");
        let plain = text
            .strip_prefix(r"\\?\UNC\")
            .map(|tail| format!(r"\\{tail}"))
            .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
            .unwrap_or(text);
        let prefixed = if plain.starts_with(r"\\") {
            format!(r"\\?\UNC\{}", plain.trim_start_matches(r"\\"))
        } else {
            format!(r"\\?\{plain}")
        };
        let context = |path: &str| {
            ServerStore::new(
                ServerContext::capture(std::path::Path::new(path), Some("alpha")).unwrap(),
            )
            .unwrap()
            .activation_context_fingerprint()
        };
        assert_eq!(context(&plain), context(&prefixed));
    }
}
