use std::collections::{HashSet, VecDeque};
use std::mem::size_of;
use std::path::Path;

use crate::core::api::{self, FileDiffSummary, ModDiffSummary, ProgressEvent, SyncMode};
use crate::core::utils::addon_backup;
use crate::ui::app::{
    AddonFolderStructure, AddonInventoryPathCacheEntry, ExternalAddonRowCache, Foxy,
    RepositoryAddonListCache, RepositoryExternalAddonsListCache, RepositoryListRow,
};
use crate::ui::types::*;

impl Foxy {
    pub(super) fn heap_bytes_of_string(value: &String) -> usize {
        value.capacity()
    }

    pub(super) fn heap_bytes_of_option_string(value: &Option<String>) -> usize {
        value.as_ref().map_or(0, Self::heap_bytes_of_string)
    }

    pub(super) fn heap_bytes_of_path(path: &Path) -> usize {
        path.as_os_str().to_string_lossy().len()
    }

    pub(super) fn heap_bytes_of_repository_profile(profile: &RepositoryProfile) -> usize {
        let mut total = Self::heap_bytes_of_string(&profile.name)
            + Self::heap_bytes_of_string(&profile.additional_params);

        total += profile.addons.capacity() * size_of::<(String, bool)>();
        total += profile
            .addons
            .iter()
            .map(|(name, _)| Self::heap_bytes_of_string(name))
            .sum::<usize>();

        total += profile.optional_addons.capacity() * size_of::<(String, bool)>();
        total += profile
            .optional_addons
            .iter()
            .map(|(name, _)| Self::heap_bytes_of_string(name))
            .sum::<usize>();

        total += profile.optional_addon_favorites.capacity() * size_of::<String>();
        total += profile
            .optional_addon_favorites
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += profile.optional_addon_client_side.capacity() * size_of::<String>();
        total += profile
            .optional_addon_client_side
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += profile.external_addons.capacity() * size_of::<(String, bool, String)>();
        total += profile
            .external_addons
            .iter()
            .map(|(name, _, path)| {
                Self::heap_bytes_of_string(name) + Self::heap_bytes_of_string(path)
            })
            .sum::<usize>();

        total += profile.external_addon_favorites.capacity() * size_of::<String>();
        total += profile
            .external_addon_favorites
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += profile.external_addon_client_side.capacity() * size_of::<String>();
        total += profile
            .external_addon_client_side
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total
    }

    pub(super) fn heap_bytes_of_repository_server(server: &RepositoryServer) -> usize {
        Self::heap_bytes_of_string(&server.name)
            + Self::heap_bytes_of_string(&server.address)
            + Self::heap_bytes_of_string(&server.port)
            + Self::heap_bytes_of_string(&server.password)
    }

    pub(super) fn heap_bytes_of_repository(repo: &Repository) -> usize {
        let mut total = Self::heap_bytes_of_string(&repo.name)
            + Self::heap_bytes_of_string(&repo.address)
            + Self::heap_bytes_of_string(&repo.path)
            + Self::heap_bytes_of_string(&repo.additional_params)
            + Self::heap_bytes_of_string(&repo.icon_image_path)
            + Self::heap_bytes_of_string(&repo.icon_image_checksum)
            + Self::heap_bytes_of_string(&repo.repo_image_path)
            + Self::heap_bytes_of_string(&repo.repo_image_checksum)
            + Self::heap_bytes_of_string(&repo.app_update_url)
            + Self::heap_bytes_of_option_string(&repo.selected_profile)
            + Self::heap_bytes_of_option_string(&repo.repository_space_id)
            + Self::heap_bytes_of_option_string(&repo.repository_space_entry_address);

        total += repo.profiles.capacity() * size_of::<RepositoryProfile>();
        total += repo
            .profiles
            .iter()
            .map(Self::heap_bytes_of_repository_profile)
            .sum::<usize>();

        total += repo.addons.capacity() * size_of::<(String, bool)>();
        total += repo
            .addons
            .iter()
            .map(|(name, _)| Self::heap_bytes_of_string(name))
            .sum::<usize>();

        total += repo.optional_addons.capacity() * size_of::<(String, bool)>();
        total += repo
            .optional_addons
            .iter()
            .map(|(name, _)| Self::heap_bytes_of_string(name))
            .sum::<usize>();

        total += repo.optional_addon_favorites.capacity() * size_of::<String>();
        total += repo
            .optional_addon_favorites
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += repo.optional_addon_client_side.capacity() * size_of::<String>();
        total += repo
            .optional_addon_client_side
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += repo.remote_client_side_addons.capacity() * size_of::<String>();
        total += repo
            .remote_client_side_addons
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += repo.external_addons.capacity() * size_of::<(String, bool, String)>();
        total += repo
            .external_addons
            .iter()
            .map(|(name, _, path)| {
                Self::heap_bytes_of_string(name) + Self::heap_bytes_of_string(path)
            })
            .sum::<usize>();

        total += repo.external_addon_favorites.capacity() * size_of::<String>();
        total += repo
            .external_addon_favorites
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += repo.external_addon_client_side.capacity() * size_of::<String>();
        total += repo
            .external_addon_client_side
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += repo.servers.capacity() * size_of::<RepositoryServer>();
        total += repo
            .servers
            .iter()
            .map(Self::heap_bytes_of_repository_server)
            .sum::<usize>();

        total
    }

    pub(super) fn heap_bytes_of_repository_space_entry(entry: &RepositorySpaceEntry) -> usize {
        Self::heap_bytes_of_string(&entry.name) + Self::heap_bytes_of_string(&entry.address)
    }

    pub(super) fn heap_bytes_of_repository_space(space: &RepositorySpace) -> usize {
        let mut total = Self::heap_bytes_of_string(&space.id)
            + Self::heap_bytes_of_string(&space.name)
            + Self::heap_bytes_of_option_string(&space.local_name_override)
            + Self::heap_bytes_of_string(&space.source_address)
            + Self::heap_bytes_of_string(&space.source_base_url)
            + Self::heap_bytes_of_string(&space.shared_path)
            + Self::heap_bytes_of_string(&space.icon_image_path)
            + Self::heap_bytes_of_string(&space.icon_image_checksum)
            + Self::heap_bytes_of_string(&space.repo_image_path)
            + Self::heap_bytes_of_string(&space.repo_image_checksum)
            + Self::heap_bytes_of_string(&space.app_update_url);

        total += space.entries.capacity() * size_of::<RepositorySpaceEntry>();
        total += space
            .entries
            .iter()
            .map(Self::heap_bytes_of_repository_space_entry)
            .sum::<usize>();

        total
    }

    pub(super) fn heap_bytes_of_download_summary(_summary: &DownloadSummary) -> usize {
        0
    }

    pub(super) fn heap_bytes_of_update_summary_notice(notice: &UpdateSummaryNotice) -> usize {
        let mut total = Self::heap_bytes_of_string(&notice.repository_url)
            + Self::heap_bytes_of_download_summary(&notice.summary);
        total += notice.mods.capacity() * size_of::<ModDiffSummary>();
        total += notice
            .mods
            .iter()
            .map(Self::heap_bytes_of_mod_diff_summary)
            .sum::<usize>();
        total
    }

    pub(super) fn heap_bytes_of_settings(settings: &SettingsViewState) -> usize {
        let mut total = Self::heap_bytes_of_string(&settings.current_tab)
            + Self::heap_bytes_of_string(&settings.arma3_directory)
            + Self::heap_bytes_of_string(&settings.arma3_profiles_directory)
            + Self::heap_bytes_of_string(&settings.steam_directory)
            + Self::heap_bytes_of_string(&settings.temp_directory)
            + Self::heap_bytes_of_string(&settings.backup_directory)
            + Self::heap_bytes_of_string(&settings.locale)
            + Self::heap_bytes_of_string(&settings.additional_folders_filter)
            + Self::heap_bytes_of_string(&settings.cleanup_folders_filter)
            + Self::heap_bytes_of_string(&settings.saved_theme_name_draft)
            + Self::heap_bytes_of_string(&settings.new_theme_name_draft);

        total += settings.saved_themes.capacity() * size_of::<crate::ui::theme::Theme>();
        total += settings
            .saved_themes
            .iter()
            .map(|theme| Self::heap_bytes_of_string(&theme.name))
            .sum::<usize>();

        total += settings.additional_folders.capacity() * size_of::<String>();
        total += settings
            .additional_folders
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();
        total += settings.additional_folder_aliases.capacity() * size_of::<(String, String)>();
        total += settings
            .additional_folder_aliases
            .iter()
            .map(|(path, alias)| {
                Self::heap_bytes_of_string(path) + Self::heap_bytes_of_string(alias)
            })
            .sum::<usize>();

        total += settings.cleanup_folders.capacity() * size_of::<(String, bool)>();
        total += settings
            .cleanup_folders
            .iter()
            .map(|(path, _)| Self::heap_bytes_of_string(path))
            .sum::<usize>();

        total += settings.update_summary_notices.capacity() * size_of::<UpdateSummaryNotice>();
        total += settings
            .update_summary_notices
            .iter()
            .map(Self::heap_bytes_of_update_summary_notice)
            .sum::<usize>();
        total += settings.active_update_sessions.capacity() * size_of::<ActiveUpdateSession>();
        total += settings
            .active_update_sessions
            .iter()
            .map(Self::heap_bytes_of_active_update_session)
            .sum::<usize>();

        total
    }

    pub(super) fn heap_bytes_of_active_update_session(session: &ActiveUpdateSession) -> usize {
        Self::heap_bytes_of_string(&session.repository_url)
            + Self::heap_bytes_of_string(&session.session_id)
            + session.mods.capacity() * size_of::<ModDiffSummary>()
            + session
                .mods
                .iter()
                .map(Self::heap_bytes_of_mod_diff_summary)
                .sum::<usize>()
    }

    pub(super) fn heap_bytes_of_file_diff_summary(summary: &FileDiffSummary) -> usize {
        Self::heap_bytes_of_string(&summary.name)
    }

    pub(super) fn heap_bytes_of_mod_diff_summary(summary: &ModDiffSummary) -> usize {
        let mut total = Self::heap_bytes_of_string(&summary.name);
        total += summary.files.capacity() * size_of::<FileDiffSummary>();
        total += summary
            .files
            .iter()
            .map(Self::heap_bytes_of_file_diff_summary)
            .sum::<usize>();
        total
    }

    pub(super) fn heap_bytes_of_log_entry(entry: &api::LogEntry) -> usize {
        Self::heap_bytes_of_string(&entry.timestamp)
            + Self::heap_bytes_of_string(&entry.level)
            + Self::heap_bytes_of_string(&entry.source)
            + Self::heap_bytes_of_string(&entry.message)
    }

    pub(super) fn heap_bytes_of_progress_event(evt: &ProgressEvent) -> usize {
        match evt {
            ProgressEvent::Stage { label, .. } => Self::heap_bytes_of_string(label),
            ProgressEvent::DownloadMod { mod_name, .. } => Self::heap_bytes_of_string(mod_name),
            ProgressEvent::Failed(message) => Self::heap_bytes_of_string(message),
            ProgressEvent::Diff { mods } => {
                mods.capacity() * size_of::<ModDiffSummary>()
                    + mods
                        .iter()
                        .map(Self::heap_bytes_of_mod_diff_summary)
                        .sum::<usize>()
            }
            ProgressEvent::RepositoryFoxyMode { app_update_url, .. } => {
                Self::heap_bytes_of_option_string(app_update_url)
            }
            ProgressEvent::SiblingPropagation { repo_urls } => {
                repo_urls.capacity() * size_of::<String>()
                    + repo_urls
                        .iter()
                        .map(Self::heap_bytes_of_string)
                        .sum::<usize>()
            }
            ProgressEvent::RecheckHashProgress { .. }
            | ProgressEvent::DownloadPlan { .. }
            | ProgressEvent::DownloadTelemetry { .. }
            | ProgressEvent::HashTelemetry { .. }
            | ProgressEvent::HashSummary { .. }
            | ProgressEvent::Finished
            | ProgressEvent::Cancelled => 0,
        }
    }

    pub(super) fn heap_bytes_of_backup_record(record: &addon_backup::AddonBackupRecord) -> usize {
        Self::heap_bytes_of_string(&record.addon_name)
            + Self::heap_bytes_of_string(&record.content_hash)
            + Self::heap_bytes_of_string(&record.folder_name)
            + Self::heap_bytes_of_path(&record.path)
    }

    pub(super) fn heap_bytes_of_string_set(set: &HashSet<String>) -> usize {
        set.capacity() * size_of::<String>()
            + set.iter().map(Self::heap_bytes_of_string).sum::<usize>()
    }

    pub(super) fn heap_bytes_of_string_pair_queue(queue: &VecDeque<(String, String)>) -> usize {
        queue.capacity() * size_of::<(String, String)>()
            + queue
                .iter()
                .map(|(left, right)| {
                    Self::heap_bytes_of_string(left) + Self::heap_bytes_of_string(right)
                })
                .sum::<usize>()
    }

    pub(super) fn heap_bytes_of_startup_sync_queue(
        queue: &VecDeque<(String, String, SyncMode)>,
    ) -> usize {
        queue.capacity() * size_of::<(String, String, SyncMode)>()
            + queue
                .iter()
                .map(|(address, path, _)| {
                    Self::heap_bytes_of_string(address) + Self::heap_bytes_of_string(path)
                })
                .sum::<usize>()
    }

    pub(super) fn heap_bytes_of_repo_space_sync_queue(
        queue: &VecDeque<(String, usize, SyncMode)>,
    ) -> usize {
        queue.capacity() * size_of::<(String, usize, SyncMode)>()
            + queue
                .iter()
                .map(|(repo_url, _, _)| Self::heap_bytes_of_string(repo_url))
                .sum::<usize>()
    }

    pub(super) fn heap_bytes_of_repository_list_cache(&self) -> usize {
        let mut total = Self::heap_bytes_of_string(&self.repository_list_cache.filter_raw)
            + Self::heap_bytes_of_string(&self.repository_list_cache.filter_lower);

        total += self.repository_list_cache.repository_names_lower.capacity() * size_of::<String>();
        total += self
            .repository_list_cache
            .repository_names_lower
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();
        total += self
            .repository_list_cache
            .repository_addresses_lower
            .capacity()
            * size_of::<String>();
        total += self
            .repository_list_cache
            .repository_addresses_lower
            .iter()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total +=
            self.repository_list_cache.space_index_by_id.capacity() * size_of::<(String, usize)>();
        total += self
            .repository_list_cache
            .space_index_by_id
            .keys()
            .map(Self::heap_bytes_of_string)
            .sum::<usize>();

        total += self.repository_list_cache.filtered_indices.capacity() * size_of::<usize>();
        total += self.repository_list_cache.rows.capacity() * size_of::<RepositoryListRow>();

        total
    }

    pub(super) fn heap_bytes_of_addon_inventory_view_cache(&self) -> usize {
        let mut total = self
            .addon_inventory_view_cache
            .addon_paths_by_name
            .capacity()
            * size_of::<(String, Vec<AddonInventoryPathCacheEntry>)>();

        total += self
            .addon_inventory_view_cache
            .addon_paths_by_name
            .iter()
            .map(|(addon_name, paths)| {
                Self::heap_bytes_of_string(addon_name)
                    + paths.capacity() * size_of::<AddonInventoryPathCacheEntry>()
                    + paths
                        .iter()
                        .map(|entry| {
                            Self::heap_bytes_of_string(&entry.path)
                                + Self::heap_bytes_of_string(&entry.normalized_path_lower)
                        })
                        .sum::<usize>()
            })
            .sum::<usize>();

        total
    }

    pub(super) fn heap_bytes_of_repository_addon_list_cache(
        cache: &RepositoryAddonListCache,
    ) -> usize {
        cache.source_names.capacity() * size_of::<String>()
            + cache
                .source_names
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.normalized_names.capacity() * size_of::<String>()
            + cache
                .normalized_names
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.enabled_by_source.capacity() * size_of::<bool>()
            + cache.favorite_by_source.capacity() * size_of::<bool>()
            + cache.client_side_by_source.capacity() * size_of::<bool>()
            + cache.forced_client_side_by_source.capacity() * size_of::<bool>()
            + cache.sorted_indices.capacity() * size_of::<usize>()
            + cache.preferred_paths.capacity() * size_of::<Option<String>>()
            + cache
                .preferred_paths
                .iter()
                .filter_map(|path| path.as_ref())
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.file_entries_by_source.capacity() * size_of::<Option<AddonFolderStructure>>()
            + cache
                .file_entries_by_source
                .iter()
                .filter_map(|structure| structure.as_ref())
                .map(Self::heap_bytes_of_addon_folder_structure)
                .sum::<usize>()
            + cache.expanded_source_indices.capacity() * size_of::<usize>()
            + cache.file_search_matches_by_source.capacity() * size_of::<bool>()
            + cache.filtered_indices.capacity() * size_of::<usize>()
            + Self::heap_bytes_of_option_string(&cache.selected_profile)
            + Self::heap_bytes_of_string(&cache.repo_path_normalized)
            + Self::heap_bytes_of_string(&cache.filter_lower)
            + Self::heap_bytes_of_string(&cache.state_filter)
    }

    pub(super) fn heap_bytes_of_repository_external_addons_list_cache(
        cache: &RepositoryExternalAddonsListCache,
    ) -> usize {
        let mut total = cache.local_addon_names.capacity() * size_of::<String>()
            + cache
                .local_addon_names
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.local_optional_addon_names.capacity() * size_of::<String>()
            + cache
                .local_optional_addon_names
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.remote_client_side_addon_names.capacity() * size_of::<String>()
            + cache
                .remote_client_side_addon_names
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.origin_options.capacity() * size_of::<String>()
            + cache
                .origin_options
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.collapsed_origins.capacity() * size_of::<String>()
            + cache
                .collapsed_origins
                .iter()
                .map(Self::heap_bytes_of_string)
                .sum::<usize>()
            + cache.file_entries_by_row.capacity() * size_of::<Option<AddonFolderStructure>>()
            + cache
                .file_entries_by_row
                .iter()
                .filter_map(|structure| structure.as_ref())
                .map(Self::heap_bytes_of_addon_folder_structure)
                .sum::<usize>()
            + cache.expanded_row_indices.capacity() * size_of::<usize>()
            + cache.file_search_matches_by_row.capacity() * size_of::<bool>()
            + cache.enabled_by_row.capacity() * size_of::<bool>()
            + cache.favorite_by_row.capacity() * size_of::<bool>()
            + cache.client_side_by_row.capacity() * size_of::<bool>()
            + cache.forced_client_side_by_row.capacity() * size_of::<bool>()
            + cache.filtered_indices.capacity() * size_of::<usize>()
            + cache.grouped_filtered_indices.capacity() * size_of::<(String, Vec<usize>)>()
            + cache
                .grouped_filtered_indices
                .iter()
                .map(|(origin, indices)| {
                    Self::heap_bytes_of_string(origin) + indices.capacity() * size_of::<usize>()
                })
                .sum::<usize>()
            + Self::heap_bytes_of_option_string(&cache.selected_profile)
            + Self::heap_bytes_of_string(&cache.filter_lower)
            + Self::heap_bytes_of_string(&cache.origin_filter)
            + Self::heap_bytes_of_string(&cache.state_filter);

        total += cache.rows.capacity() * size_of::<ExternalAddonRowCache>();
        total += cache
            .rows
            .iter()
            .map(|row| {
                Self::heap_bytes_of_string(&row.addon_name)
                    + Self::heap_bytes_of_string(&row.path)
                    + Self::heap_bytes_of_string(&row.origin)
                    + Self::heap_bytes_of_string(&row.addon_name_lower)
                    + Self::heap_bytes_of_string(&row.path_lower)
                    + Self::heap_bytes_of_string(&row.origin_lower)
                    + Self::heap_bytes_of_string(&row.path_lookup_key)
            })
            .sum::<usize>();

        total
    }

    fn heap_bytes_of_addon_folder_structure(structure: &AddonFolderStructure) -> usize {
        Self::heap_bytes_of_string(&structure.path_key)
            + structure.files.capacity() * size_of::<crate::ui::app::AddonFolderEntry>()
            + structure
                .files
                .iter()
                .map(|file| {
                    Self::heap_bytes_of_string(&file.display_path)
                        + Self::heap_bytes_of_string(&file.name_lower)
                        + Self::heap_bytes_of_string(&file.path_lower)
                })
                .sum::<usize>()
    }
}
