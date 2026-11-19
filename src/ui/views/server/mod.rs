mod arma_admin;
mod addon_profiles;
mod backups;
mod configuration;
mod consumer_recovery;
mod content;
mod content_acquisition;
mod content_stage;
mod deployment_activation;
mod deployment_composition;
mod deployment_preparation;
mod deployment_records;
mod dialogs;
mod editor;
mod headless;
mod import_export;
mod installation;
mod keys;
mod lifecycle;
mod maintenance;
mod missions;
mod monitoring;
mod native_install;
mod publication;
mod rcon;
mod reconciliation;
mod recovery;
mod reforger_addons;
mod reforger_admin;
mod services;
mod sessions;
mod sidebar;
mod steam_auth;
mod workshop;
mod workspace;

use crate::ui::app::Foxy;
use crate::ui::app::server::ServerDialog;

impl Foxy {
    pub(crate) fn open_server_draft_dialog(&mut self, dialog: ServerDialog) {
        if self.server_view.busy() || self.server_view.dirty() {
            return;
        }
        self.server_view.dialog_name = self
            .server_view
            .editor
            .as_ref()
            .map(|editor| editor.draft.display_name.clone())
            .unwrap_or_default();
        self.server_view.open_dialog(dialog);
    }
}
