use super::{InstallationId, ServerInstallation, ServerStore, ServerStoreError};
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::recovery::ConsumerRecord;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct InstallationConsumerInspection {
    pub installation_id: InstallationId,
    pub inspection_complete: bool,
    pub recorded_consumers: Vec<ConsumerRecord>,
    pub cleanup_required: Option<bool>,
    pub inspection_error: Option<&'static str>,
    pub liveness_verified: bool,
    pub exclusive_maintenance_authorized: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrphanDisposition {
    InspectionFailed,
    Retained,
    Eligible,
}

impl InstallationConsumerInspection {
    /// Classification only. This does not delete a registration or authorize maintenance.
    pub fn orphan_disposition(&self) -> OrphanDisposition {
        if !self.inspection_complete
            || self.inspection_error.is_some()
            || self.cleanup_required.is_none()
        {
            return OrphanDisposition::InspectionFailed;
        }
        if self.cleanup_required == Some(true) || !self.recorded_consumers.is_empty() {
            return OrphanDisposition::Retained;
        }
        OrphanDisposition::Eligible
    }

    pub(super) fn inspect(installation: &ServerInstallation) -> Self {
        let result = InstallationLocks::capture(&installation.directory)
            .and_then(|locks| locks.inspect_consumers());
        let (recorded_consumers, inspection_error) = match result {
            Ok(records) => (records, None),
            Err(_) => (
                vec![],
                Some(
                    "Consumer registrations could not be verified; installation maintenance remains blocked.",
                ),
            ),
        };
        Self {
            installation_id: installation.id.clone(),
            inspection_complete: inspection_error.is_none(),
            cleanup_required: inspection_error
                .is_none()
                .then_some(!recorded_consumers.is_empty()),
            recorded_consumers,
            inspection_error,
            liveness_verified: false,
            exclusive_maintenance_authorized: false,
            writes_performed: false,
        }
    }
}

impl ServerStore {
    pub fn installation_consumers(
        &self,
        id: &InstallationId,
    ) -> Result<InstallationConsumerInspection, ServerStoreError> {
        if !self.capabilities().installation_adoption {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let catalog = self.load()?;
        let installation = catalog
            .installations
            .iter()
            .find(|installation| &installation.id == id)
            .ok_or(ServerStoreError::InstallationNotFound)?;
        Ok(InstallationConsumerInspection::inspect(installation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspection(
        complete: bool,
        cleanup_required: Option<bool>,
        records: usize,
    ) -> InstallationConsumerInspection {
        InstallationConsumerInspection {
            installation_id: InstallationId::new(),
            inspection_complete: complete,
            recorded_consumers: (0..records)
                .map(|index| foxy_supervisor::recovery::ConsumerRecord {
                    cleanup: None,
                    schema_version: 1,
                    installation_key: format!("key-{index}"),
                    session: foxy_supervisor::session::SessionIdentity {
                        owner_id: "owner-a".into(),
                        owner_generation: 1,
                        game_space_id: "alpha".into(),
                        instance_id: "server_0000000000000000000000000000000a".into(),
                        operation_id: "op-1".into(),
                        run_generation: 1,
                    },
                })
                .collect(),
            cleanup_required,
            inspection_error: (!complete).then_some("unverified"),
            liveness_verified: false,
            exclusive_maintenance_authorized: false,
            writes_performed: false,
        }
    }

    #[test]
    fn orphan_disposition_retains_unverified_or_recorded_consumers() {
        assert_eq!(
            inspection(false, None, 0).orphan_disposition(),
            OrphanDisposition::InspectionFailed
        );
        assert_eq!(
            inspection(true, Some(true), 0).orphan_disposition(),
            OrphanDisposition::Retained
        );
        assert_eq!(
            inspection(true, Some(false), 1).orphan_disposition(),
            OrphanDisposition::Retained
        );
        let eligible = inspection(true, Some(false), 0);
        assert_eq!(eligible.orphan_disposition(), OrphanDisposition::Eligible);
        assert!(!eligible.liveness_verified);
        assert!(!eligible.exclusive_maintenance_authorized);
        assert!(!eligible.writes_performed);
    }
}
