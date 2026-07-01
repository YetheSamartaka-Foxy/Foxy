use crate::core::ts3_plugin;
use crate::core::utils::format::{sanitize_log_path, sanitize_log_path_str};
use crate::ui::app::{Foxy, Ts3PluginUpdatePrompt};
use log::info;

impl Foxy {
    /// After a download completes for a repository, scan its addon path
    /// for TS3 plugins whose hash differs from the last installed version.
    /// If found, set `ts3_plugin_update_prompt` so the UI can offer installation.
    pub(in crate::ui::app) fn check_ts3_plugin_updates_for_repo(&mut self, repo_index: usize) {
        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            info!(
                "Skipping TS3 plugin update check because repository index is missing: repo_index={}",
                repo_index
            );
            return;
        };

        let repo_path = repo.path.clone();
        if repo_path.is_empty() {
            info!(
                "Skipping TS3 plugin update check because repository path is empty: repo_index={}",
                repo_index
            );
            return;
        }

        info!(
            "Checking TS3 plugin updates for repository: repo_index={} path={} tracked_installed_plugins={}",
            repo_index,
            sanitize_log_path_str(&repo_path),
            self.settings_view_state.ts3_installed_plugin_hashes.len()
        );

        // Invalidate the cached scan so the settings tab picks up changes.
        self.invalidate_ts3_plugin_cache();

        let plugins = ts3_plugin::scan_repository_for_ts3_plugins(&repo_path);
        info!(
            "TS3 plugin update check scan finished: repo_index={} plugin_count={}",
            repo_index,
            plugins.len()
        );

        for plugin in plugins {
            let path_key = plugin.plugin_path.display().to_string();
            let installed_hash = self
                .settings_view_state
                .ts3_installed_plugin_hashes
                .get(&path_key);
            let ts3_lookup = ts3_plugin::lookup_installed_teamspeak_plugin(&plugin.plugin_path);
            info!(
                "Evaluating TS3 plugin update candidate: addon={} path={} detected_hash={} foxy_stored_hash_present={} foxy_stored_hash_matches={} ts3_search_name={} ts3_expected_files={} ts3_candidate_plugin_dirs={} ts3_existing_plugin_dirs={} ts3_installed_matches={} ts3_missing_files={} ts3_hash_mismatches={} ts3_installed={} ts3_up_to_date={}",
                plugin.addon_name,
                sanitize_log_path(&plugin.plugin_path),
                plugin.file_hash,
                installed_hash.is_some(),
                installed_hash == Some(&plugin.file_hash),
                ts3_lookup.search_name,
                ts3_lookup.expected_files.len(),
                ts3_lookup.checked_dirs.len(),
                ts3_lookup.existing_dirs.len(),
                ts3_lookup.matched_files.len(),
                ts3_lookup.missing_files.len(),
                ts3_lookup.hash_mismatched_files.len(),
                ts3_lookup.is_installed,
                ts3_lookup.is_up_to_date
            );

            if installed_hash != Some(&plugin.file_hash) && ts3_lookup.is_up_to_date {
                self.mark_ts3_plugin_installed(&path_key, &plugin.file_hash);
                continue;
            }

            // Only prompt when the plugin was previously installed through the
            // app or detected in TeamSpeak, and the local package hash differs
            // from that installed copy.
            if (installed_hash.is_some() || ts3_lookup.is_installed) && !ts3_lookup.is_up_to_date {
                let previous_hash = installed_hash.map_or("<unknown>", String::as_str);
                info!(
                    "TS3 plugin update detected: addon={} path={} previous_hash={} current_hash={}",
                    plugin.addon_name,
                    sanitize_log_path(&plugin.plugin_path),
                    previous_hash,
                    plugin.file_hash
                );
                self.ts3_plugin_update_prompt = Some(Ts3PluginUpdatePrompt {
                    plugin_path: plugin.plugin_path,
                    addon_name: plugin.addon_name,
                    file_hash: plugin.file_hash,
                });
                // Show the first updated plugin prompt; additional ones
                // will be surfaced via the settings tab.
                return;
            }
        }
        info!(
            "No TS3 plugin update prompt needed for repository: repo_index={}",
            repo_index
        );
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
