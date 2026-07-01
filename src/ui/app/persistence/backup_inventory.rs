use log::{error, warn};

use crate::core::utils::addon_backup;
use crate::ui::app::{BackupManagerNotice, Foxy, PersistenceRequest};

impl Foxy {
    pub(in crate::ui::app) fn dispatch_backup_inventory_refresh_request(&mut self) {
        if !self.backup_inventory_refresh_requested || self.backup_inventory_refresh_in_progress {
            return;
        }

        let Some(backup_root) = self.configured_backup_directory() else {
            let message = self.t("Addon backup directory is not configured.");
            self.backup_manager_notice = Some(BackupManagerNotice {
                success: false,
                message: message.clone(),
            });
            self.backup_inventory_refresh_requested = false;
            warn!("{}", message);
            return;
        };

        self.backup_inventory_request_id = self.backup_inventory_request_id.saturating_add(1);
        let request_id = self.backup_inventory_request_id;
        match self
            .persistence_request_tx
            .send(PersistenceRequest::RefreshBackupInventory {
                request_id,
                backup_root,
            }) {
            Ok(()) => {
                self.backup_inventory_refresh_requested = false;
                self.backup_inventory_refresh_in_progress = true;
                self.backup_inventory_in_flight_request_id = Some(request_id);
            }
            Err(err) => {
                error!("Failed to queue backup inventory refresh request: {}", err);
            }
        }
    }

    pub(in crate::ui::app) fn handle_backup_inventory_refresh_result(
        &mut self,
        request_id: u64,
        result: Result<Vec<addon_backup::AddonBackupRecord>, String>,
    ) {
        if self.backup_inventory_in_flight_request_id == Some(request_id) {
            self.backup_inventory_in_flight_request_id = None;
        }
        self.backup_inventory_refresh_in_progress = false;

        match result {
            Ok(records) => {
                self.backup_manager_records = records;
                self.backup_manager_records_version =
                    self.backup_manager_records_version.wrapping_add(1);
                self.backup_manager_loaded = true;
            }
            Err(err) => {
                let message =
                    self.t_fmt("Failed to load addon backups: {error}", &[("error", err)]);
                self.backup_manager_notice = Some(BackupManagerNotice {
                    success: false,
                    message: message.clone(),
                });
                self.backup_manager_loaded = false;
                warn!("{}", message);
            }
        }

        self.needs_repaint = true;
    }
}
