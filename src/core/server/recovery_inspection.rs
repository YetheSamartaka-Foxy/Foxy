use foxy_supervisor::leases::InstallationLocks;
use serde::Serialize;

use super::{
    InstallationConsumerInspection, InstallationId, ServerContext, ServerInstallation, ServerStore,
    ServerStoreError,
};

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryMetadataEvidence {
    pub intent_fingerprint: String,
    pub transaction_fingerprint: String,
    pub instance_id: String,
    pub pending: bool,
    pub context_status: &'static str,
    pub journal_status: &'static str,
    pub active_endpoint: &'static str,
    pub resolution_status: &'static str,
    pub receipt_status: &'static str,
    pub catalog_unchanged: Option<bool>,
    pub selection_unchanged: Option<bool>,
    pub inspection_error: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstallationRecoveryInspection {
    pub installation_id: InstallationId,
    pub inspection_complete: bool,
    pub maintenance_status: &'static str,
    pub receipt_count: Option<usize>,
    pub retention_limit_reached: Option<bool>,
    pub evidence: Vec<RecoveryMetadataEvidence>,
    pub evidence_truncated: bool,
    pub inspection_error: Option<&'static str>,
    pub consumers: InstallationConsumerInspection,
    pub native_layout: &'static str,
    pub stopped_owner: &'static str,
    pub content_trust: &'static str,
    pub owner_handoff: &'static str,
    pub activation_available: bool,
    pub resolution_available: bool,
    pub liveness_verified: bool,
    pub installation_leases_acquired: bool,
    pub archives_verified: bool,
    pub writes_performed: bool,
}

impl ServerStore {
    pub fn installation_recovery(
        &self,
        id: &InstallationId,
    ) -> Result<InstallationRecoveryInspection, ServerStoreError> {
        if !self.capabilities().installation_adoption {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let context =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        let (catalog, before, _) = self.read()?;
        let installation = catalog
            .installations
            .iter()
            .find(|value| &value.id == id)
            .ok_or(ServerStoreError::InstallationNotFound)?;
        let locks = InstallationLocks::capture(&installation.directory);
        let consumers = self.installation_consumers(id)?;
        let result =
            self.inspect_installation_recovery(installation, consumers, locks.as_ref().ok());
        if self.read()?.1 != before {
            return Err(ServerStoreError::StaleRevision);
        }
        let context =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(result)
    }

    pub(in crate::core::server) fn inspect_installation_recovery(
        &self,
        installation: &ServerInstallation,
        consumers: InstallationConsumerInspection,
        locks: Option<&InstallationLocks>,
    ) -> InstallationRecoveryInspection {
        let mut result = InstallationRecoveryInspection {
            installation_id: installation.id.clone(),
            inspection_complete: false,
            maintenance_status: "unknown",
            receipt_count: None,
            retention_limit_reached: None,
            evidence: vec![],
            evidence_truncated: false,
            inspection_error: None,
            consumers,
            native_layout: "unsupported",
            stopped_owner: "unverified",
            content_trust: "unverified",
            owner_handoff: "not_implemented",
            activation_available: false,
            resolution_available: false,
            liveness_verified: false,
            installation_leases_acquired: false,
            archives_verified: false,
            writes_performed: false,
        };
        let inspected = (|| {
            let locks = locks.ok_or("installation_identity_unverified")?;
            let inventory = locks
                .inspect_maintenance()
                .map_err(|_| "maintenance_metadata_unverified")?;
            result.maintenance_status = if inventory.pending.is_some() {
                "pending"
            } else {
                "clear"
            };
            result.receipt_count = Some(inventory.receipts.len());
            result.retention_limit_reached = Some(inventory.receipts.len() >= 1024);
            if let Some(pending) = &inventory.pending {
                result.evidence.push(
                    self.inspect_recovery_metadata(
                        pending,
                        inventory
                            .receipts
                            .iter()
                            .find(|receipt| &receipt.intent == pending),
                        true,
                    ),
                );
            }
            let historical = inventory
                .receipts
                .iter()
                .filter(|receipt| inventory.pending.as_ref() != Some(&receipt.intent))
                .collect::<Vec<_>>();
            result.evidence_truncated = historical.len() > 16;
            for receipt in historical.into_iter().take(16) {
                result.evidence.push(self.inspect_recovery_metadata(
                    &receipt.intent,
                    Some(receipt),
                    false,
                ));
            }
            if locks
                .inspect_maintenance()
                .map_err(|_| "maintenance_metadata_unverified")?
                != inventory
            {
                return Err("maintenance_changed_during_inspection");
            }
            if locks
                .inspect_consumers()
                .map_err(|_| "consumer_metadata_unverified")?
                != result.consumers.recorded_consumers
            {
                return Err("consumers_changed_during_inspection");
            }
            Ok::<(), &'static str>(())
        })();
        if let Err(error) = inspected {
            result.inspection_error = Some(error);
        }
        result.inspection_complete = result.inspection_error.is_none()
            && result.consumers.inspection_complete
            && result
                .evidence
                .iter()
                .all(|evidence| evidence.inspection_error.is_none());
        result
    }
}
