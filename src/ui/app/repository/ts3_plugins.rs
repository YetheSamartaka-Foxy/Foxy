use crate::core::utils::format::sanitize_log_path_str;
use crate::ui::app::Foxy;
use log::info;

impl Foxy {
    /// After a download completes for a repository, request a background scan
    /// for TS3 plugins whose package differs from the installed version. The
    /// scan result sets `ts3_plugin_update_prompt` so the UI can offer
    /// installation, and refreshes the persisted plugin state.
    pub(in crate::ui::app) fn check_ts3_plugin_updates_for_repo(&mut self, repo_index: usize) {
        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            info!(
                "Skipping TS3 plugin update check because repository index is missing: repo_index={}",
                repo_index
            );
            return;
        };

        if repo.path.is_empty() {
            info!(
                "Skipping TS3 plugin update check because repository path is empty: repo_index={}",
                repo_index
            );
            return;
        }

        // Stale scan results would describe the pre-download files.
        self.ts3_plugin_cache = None;
        self.start_ts3_plugin_scan("repository download finished", true);
    }

    /// Record that a TS3 plugin was successfully opened for installation,
    /// storing its current hash to suppress future prompts until it changes.
    pub(crate) fn mark_ts3_plugin_installed(&mut self, path_key: &str, hash: &str) {
        self.settings_view_state
            .ts3_installed_plugin_hashes
            .insert(path_key.to_string(), hash.to_string());
        self.save_settings();
        info!(
            "Marked TS3 plugin as installed in Foxy settings: path={} hash={}",
            sanitize_log_path_str(path_key),
            hash
        );
    }
}
