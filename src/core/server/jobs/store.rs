use std::io::Write;
use std::path::PathBuf;

use super::ledger::{self, OccurrenceListing};
use super::model::{MaintenanceCatalog, MaintenanceChange, apply_change};
use super::status::{MaintenanceOverview, job_statuses};
use crate::core::server::{ServerInstanceId, ServerStore, ServerStoreError};

const CATALOG_FILE: &str = "jobs.json";

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| {
            u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
        })
}

impl ServerStore {
    pub(in crate::core::server) fn maintenance_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("maintenance")
    }

    pub(in crate::core::server) fn occurrence_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.maintenance_root(id).join("occurrences")
    }

    fn checked_maintenance_root(&self, id: &ServerInstanceId) -> Result<PathBuf, ServerStoreError> {
        self.show(id)?;
        let root = self.maintenance_root(id);
        if root.exists() {
            crate::core::server::store::check_directory_chain(&root)?;
        }
        Ok(root)
    }

    pub fn load_maintenance(
        &self,
        id: &ServerInstanceId,
    ) -> Result<MaintenanceCatalog, ServerStoreError> {
        let path = self.checked_maintenance_root(id)?.join(CATALOG_FILE);
        let bytes = match crate::core::server::store::read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MaintenanceCatalog::empty(id.clone()));
            }
            Err(_) => return Err(ServerStoreError::Io),
        };
        let catalog: MaintenanceCatalog =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        catalog
            .validate(id)
            .map_err(|_| ServerStoreError::Corrupt)?;
        Ok(catalog)
    }

    /// Applies one revision-checked change and returns the saved catalog plus the affected job id.
    pub fn change_maintenance(
        &self,
        id: &ServerInstanceId,
        expected_revision: u64,
        change: MaintenanceChange,
    ) -> Result<(MaintenanceCatalog, Option<String>), ServerStoreError> {
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok(self.change_maintenance_owned(id, expected_revision, change))
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    pub fn preview_maintenance(
        &self,
        id: &ServerInstanceId,
        expected_revision: u64,
        change: MaintenanceChange,
    ) -> Result<(MaintenanceCatalog, Option<String>), ServerStoreError> {
        let current = self.load_maintenance(id)?;
        if current.revision != expected_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        apply_change(&current, change, unix_ms()).map_err(ServerStoreError::Invalid)
    }

    fn change_maintenance_owned(
        &self,
        id: &ServerInstanceId,
        expected_revision: u64,
        change: MaintenanceChange,
    ) -> Result<(MaintenanceCatalog, Option<String>), ServerStoreError> {
        let (next, affected) = self.preview_maintenance(id, expected_revision, change)?;
        let root = self.checked_maintenance_root(id)?;
        std::fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        crate::core::server::store::check_directory_chain(&root)?;
        let bytes = serde_json::to_vec_pretty(&next).map_err(|_| ServerStoreError::Corrupt)?;
        let mut file = tempfile::NamedTempFile::new_in(&root).map_err(|_| ServerStoreError::Io)?;
        file.write_all(&bytes).map_err(|_| ServerStoreError::Io)?;
        file.as_file()
            .sync_all()
            .map_err(|_| ServerStoreError::Io)?;
        if self.load_maintenance(id)?.revision != expected_revision {
            return Err(ServerStoreError::StaleRevision);
        }
        file.persist(root.join(CATALOG_FILE))
            .map_err(|_| ServerStoreError::Io)?;
        Ok((next, affected))
    }

    /// Job statuses plus the bounded occurrence listing they were derived from.
    pub fn maintenance_overview(
        &self,
        id: &ServerInstanceId,
    ) -> Result<(MaintenanceOverview, OccurrenceListing), ServerStoreError> {
        let catalog = self.load_maintenance(id)?;
        let history = self.maintenance_history(id)?;
        let owner_running = matches!(
            crate::core::server::owner::attach(self.context()),
            Ok(Some(_))
        );
        let capabilities = crate::core::game::registry()
            .get(self.context().game_id())
            .map(|game| game.server_capabilities())
            .unwrap_or_default();
        let overview = job_statuses(&catalog, &history, &capabilities, owner_running, unix_ms());
        Ok((overview, history))
    }

    pub fn maintenance_history(
        &self,
        id: &ServerInstanceId,
    ) -> Result<OccurrenceListing, ServerStoreError> {
        let root = self.checked_maintenance_root(id)?.join("occurrences");
        if root.exists() {
            crate::core::server::store::check_directory_chain(&root)?;
        }
        ledger::list(&root).map_err(|_| ServerStoreError::Io)
    }
}
