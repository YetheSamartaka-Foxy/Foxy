use crate::ui::app::{AddonBackupNotice, Foxy};

impl Foxy {
    pub fn invalidate_backup_manager_inventory(&mut self) {
        self.backup_manager_loaded = false;
    }

    pub fn is_backup_manager_inventory_refresh_pending(&self) -> bool {
        self.backup_inventory_refresh_requested || self.backup_inventory_refresh_in_progress
    }

    pub(in crate::ui::app) fn set_addon_backup_notice(
        &mut self,
        repo_index: usize,
        success: bool,
        message: String,
    ) {
        self.addon_backup_notice = Some(AddonBackupNotice {
            repo_index,
            success,
            message,
        });
        self.needs_repaint = true;
    }
}
