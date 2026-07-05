use super::normalize_local_path_for_compare;
use crate::core::db::{FoxyDb, params};
use crate::ui::app::{
    AddonFolderEntry, AddonFolderStructure, AddonInventoryPathCacheEntry, Foxy,
    RepositoryAddonListKind, RepositoryAddonSizeLoadResult,
};
use crate::ui::i18n::locale_compare;
use crate::ui::search_filter::MultiEntryFilter;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;

const MAX_ADDON_STRUCTURE_FILES: usize = 5000;

impl Foxy {
    pub(crate) fn ensure_repository_addon_size_cache_loaded(&mut self) {
        if self.repository_addon_size_load_pending
            || !self
                .repository_addon_size_bytes_by_repo_and_addon
                .is_empty()
        {
            return;
        }

        self.repository_addon_size_load_pending = true;
        let tx = self.repository_addon_size_load_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let result = load_repository_addon_sizes();
            if tx.send(result).is_ok() {
                Foxy::request_background_repaint(repaint_ctx.as_ref());
            }
        });
    }

    pub(crate) fn poll_repository_addon_size_load_results(&mut self) {
        while let Ok(result) = self.repository_addon_size_load_rx.try_recv() {
            self.repository_addon_size_load_pending = false;
            self.repository_addon_size_bytes_by_repo_and_addon = result.sizes_by_repo_and_addon;
            self.repository_addons_list_cache = crate::ui::app::RepositoryAddonListCache::default();
            self.repository_optional_addons_list_cache =
                crate::ui::app::RepositoryAddonListCache::default();
            self.needs_repaint = true;
        }
    }

    pub(crate) fn invalidate_repository_addon_size_cache(&mut self) {
        self.repository_addon_size_bytes_by_repo_and_addon.clear();
        self.repository_addons_list_cache = crate::ui::app::RepositoryAddonListCache::default();
        self.repository_optional_addons_list_cache =
            crate::ui::app::RepositoryAddonListCache::default();
    }

    pub(crate) fn repository_addon_size_key(
        repo_address: &str,
        addon_name: &str,
    ) -> (String, String) {
        (
            Self::normalize_repo_url(repo_address),
            addon_name.to_lowercase(),
        )
    }

    pub(crate) fn repository_addon_remote_size_bytes(
        &self,
        repo_address: &str,
        addon_name: &str,
    ) -> u64 {
        self.repository_addon_size_bytes_by_repo_and_addon
            .get(&Self::repository_addon_size_key(repo_address, addon_name))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn repository_remote_size_bytes_by_address(&self, repo_address: &str) -> u64 {
        let normalized_url = Self::normalize_repo_url(repo_address);
        self.repository_addon_size_bytes_by_repo_and_addon
            .iter()
            .filter_map(|((repo_url, _), size)| (repo_url == &normalized_url).then_some(*size))
            .sum()
    }

    pub(super) fn repository_addon_list_enabled_size_bytes_cached(
        &self,
        kind: RepositoryAddonListKind,
    ) -> u64 {
        let cache = self.repository_addon_list_cache_cached(kind);
        cache
            .remote_size_bytes_by_source
            .iter()
            .zip(&cache.enabled_by_source)
            .filter(|(_, enabled)| **enabled)
            .map(|(size, _)| *size)
            .sum()
    }

    pub(crate) fn repository_space_remote_size_bytes(&self, space_id: &str) -> u64 {
        repository_space_remote_size_bytes_for_repositories(
            space_id,
            &self.repository_view_state.repositories,
            &self.repository_addon_size_bytes_by_repo_and_addon,
        )
    }

    pub(super) fn current_repository_profile_name_cached(
        &self,
        repo_index: usize,
    ) -> Option<String> {
        self.repository_view_state
            .repositories
            .get(repo_index)
            .and_then(|repo| repo.selected_profile.clone())
    }

    pub(super) fn current_repository_addons_cached(
        &self,
        repo_index: usize,
        kind: RepositoryAddonListKind,
    ) -> Option<&Vec<(String, bool)>> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(match kind {
                RepositoryAddonListKind::Addons => &profile.addons,
                RepositoryAddonListKind::OptionalAddons => &profile.optional_addons,
            });
        }

        Some(match kind {
            RepositoryAddonListKind::Addons => &repo.addons,
            RepositoryAddonListKind::OptionalAddons => &repo.optional_addons,
        })
    }

    pub(super) fn current_repository_addons_mut_cached(
        &mut self,
        repo_index: usize,
        kind: RepositoryAddonListKind,
    ) -> Option<&mut Vec<(String, bool)>> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(repo_index)?;
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            return Some(match kind {
                RepositoryAddonListKind::Addons => &mut profile.addons,
                RepositoryAddonListKind::OptionalAddons => &mut profile.optional_addons,
            });
        }

        Some(match kind {
            RepositoryAddonListKind::Addons => &mut repo.addons,
            RepositoryAddonListKind::OptionalAddons => &mut repo.optional_addons,
        })
    }

    pub(super) fn current_repository_optional_addon_favorites_cached(
        &self,
        repo_index: usize,
    ) -> Option<&Vec<String>> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(&profile.optional_addon_favorites);
        }

        Some(&repo.optional_addon_favorites)
    }

    pub(super) fn current_repository_optional_addon_favorites_mut_cached(
        &mut self,
        repo_index: usize,
    ) -> Option<&mut Vec<String>> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(repo_index)?;
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            return Some(&mut profile.optional_addon_favorites);
        }

        Some(&mut repo.optional_addon_favorites)
    }

    pub(super) fn current_repository_optional_addon_client_side_cached(
        &self,
        repo_index: usize,
    ) -> Option<&Vec<String>> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(&profile.optional_addon_client_side);
        }

        Some(&repo.optional_addon_client_side)
    }

    pub(super) fn current_repository_optional_addon_client_side_mut_cached(
        &mut self,
        repo_index: usize,
    ) -> Option<&mut Vec<String>> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(repo_index)?;
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            return Some(&mut profile.optional_addon_client_side);
        }

        Some(&mut repo.optional_addon_client_side)
    }

    pub(super) fn repository_addon_list_cache_cached(
        &self,
        kind: RepositoryAddonListKind,
    ) -> &crate::ui::app::RepositoryAddonListCache {
        match kind {
            RepositoryAddonListKind::Addons => &self.repository_addons_list_cache,
            RepositoryAddonListKind::OptionalAddons => &self.repository_optional_addons_list_cache,
        }
    }

    pub(super) fn repository_addon_list_cache_mut_cached(
        &mut self,
        kind: RepositoryAddonListKind,
    ) -> &mut crate::ui::app::RepositoryAddonListCache {
        match kind {
            RepositoryAddonListKind::Addons => &mut self.repository_addons_list_cache,
            RepositoryAddonListKind::OptionalAddons => {
                &mut self.repository_optional_addons_list_cache
            }
        }
    }

    pub(super) fn ensure_addon_inventory_view_cache_cached(&mut self) {
        if self.addon_inventory_view_cache.inventory_generation_seen
            == self.addon_inventory_generation
        {
            return;
        }

        let addon_paths_by_name = {
            let all_addons = self.get_or_generate_all_addons();
            let mut addon_paths_by_name: HashMap<String, Vec<AddonInventoryPathCacheEntry>> =
                HashMap::new();
            for (addon_name, path, _origin, _size_bytes) in all_addons {
                addon_paths_by_name
                    .entry(addon_name.to_lowercase())
                    .or_default()
                    .push(AddonInventoryPathCacheEntry {
                        path: path.clone(),
                        normalized_path_lower: normalize_local_path_for_compare(path)
                            .to_lowercase(),
                    });
            }
            addon_paths_by_name
        };

        self.addon_inventory_view_cache.inventory_generation_seen = self.addon_inventory_generation;
        self.addon_inventory_view_cache.addon_paths_by_name = addon_paths_by_name;
    }

    pub(super) fn resolve_preferred_addon_path_cached(
        paths: &[AddonInventoryPathCacheEntry],
        repo_path_normalized: &str,
    ) -> Option<String> {
        if repo_path_normalized.is_empty() {
            return paths.first().map(|entry| entry.path.clone());
        }

        paths
            .iter()
            .find(|entry| {
                entry
                    .normalized_path_lower
                    .starts_with(repo_path_normalized)
            })
            .map(|entry| entry.path.clone())
            .or_else(|| paths.first().map(|entry| entry.path.clone()))
    }

    pub(super) fn ensure_repository_addon_file_structure_cached(
        &mut self,
        kind: RepositoryAddonListKind,
        source_index: usize,
    ) {
        let Some(addon_path) = self
            .repository_addon_list_cache_cached(kind)
            .preferred_paths
            .get(source_index)
            .and_then(|path| path.clone())
        else {
            return;
        };
        let path_key = normalize_local_path_for_compare(&addon_path).to_lowercase();
        let already_loaded = self
            .repository_addon_list_cache_cached(kind)
            .file_entries_by_source
            .get(source_index)
            .and_then(|structure| structure.as_ref())
            .is_some_and(|structure| structure.path_key == path_key);
        if already_loaded {
            return;
        }

        let structure = scan_addon_folder_structure(&addon_path, &path_key);
        let cache = self.repository_addon_list_cache_mut_cached(kind);
        if let Some(slot) = cache.file_entries_by_source.get_mut(source_index) {
            *slot = Some(structure);
        }
    }

    pub(super) fn ensure_repository_external_addon_file_structure_cached(
        &mut self,
        row_index: usize,
    ) {
        let Some(addon_path) = self
            .repository_external_addons_list_cache
            .rows
            .get(row_index)
            .map(|row| row.path.clone())
        else {
            return;
        };
        let path_key = normalize_local_path_for_compare(&addon_path).to_lowercase();
        let already_loaded = self
            .repository_external_addons_list_cache
            .file_entries_by_row
            .get(row_index)
            .and_then(|structure| structure.as_ref())
            .is_some_and(|structure| structure.path_key == path_key);
        if already_loaded {
            return;
        }

        let structure = scan_addon_folder_structure(&addon_path, &path_key);
        if let Some(slot) = self
            .repository_external_addons_list_cache
            .file_entries_by_row
            .get_mut(row_index)
        {
            *slot = Some(structure);
        }
    }

    pub(crate) fn ensure_repository_addon_list_cache_cached(
        &mut self,
        repo_index: usize,
        kind: RepositoryAddonListKind,
    ) {
        self.ensure_repository_addon_size_cache_loaded();
        self.ensure_addon_inventory_view_cache_cached();

        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            return;
        };
        let repo_address = repo.address.clone();
        let Some(source) = self.current_repository_addons_cached(repo_index, kind) else {
            return;
        };
        let source_len = source.len();

        let selected_profile = self.current_repository_profile_name_cached(repo_index);
        let optional_favorite_names: HashSet<String> = match kind {
            RepositoryAddonListKind::Addons => HashSet::new(),
            RepositoryAddonListKind::OptionalAddons => self
                .current_repository_optional_addon_favorites_cached(repo_index)
                .map(|favorites| favorites.iter().map(|name| name.to_lowercase()).collect())
                .unwrap_or_default(),
        };
        let optional_client_side_names: HashSet<String> = match kind {
            RepositoryAddonListKind::Addons => HashSet::new(),
            RepositoryAddonListKind::OptionalAddons => self
                .current_repository_optional_addon_client_side_cached(repo_index)
                .map(|client_side| client_side.iter().map(|name| name.to_lowercase()).collect())
                .unwrap_or_default(),
        };
        let forced_client_side_names: HashSet<String> = match kind {
            RepositoryAddonListKind::Addons => HashSet::new(),
            RepositoryAddonListKind::OptionalAddons => repo
                .remote_client_side_addons
                .iter()
                .map(|name| name.to_lowercase())
                .collect(),
        };
        let repo_path_normalized = normalize_local_path_for_compare(&repo.path).to_lowercase();
        let needs_rebuild = {
            let cache = self.repository_addon_list_cache_cached(kind);
            cache.repo_index != Some(repo_index)
                || cache.selected_profile != selected_profile
                || cache.inventory_generation_seen != self.addon_inventory_generation
                || cache.repo_path_normalized != repo_path_normalized
                || cache.source_names.len() != source.len()
                || cache
                    .source_names
                    .iter()
                    .zip(source.iter())
                    .any(|(cached, (name, _))| cached != name)
        };

        if needs_rebuild {
            let inventory_generation = self.addon_inventory_generation;
            let source_names: Vec<String> = source.iter().map(|(name, _)| name.clone()).collect();
            let normalized_names: Vec<String> =
                source.iter().map(|(name, _)| name.to_lowercase()).collect();
            let enabled_by_source: Vec<bool> = source.iter().map(|(_, enabled)| *enabled).collect();
            let favorite_by_source: Vec<bool> = normalized_names
                .iter()
                .map(|name| optional_favorite_names.contains(name))
                .collect();
            let client_side_by_source: Vec<bool> = normalized_names
                .iter()
                .map(|name| {
                    optional_client_side_names.contains(name)
                        || forced_client_side_names.contains(name)
                })
                .collect();
            let forced_client_side_by_source: Vec<bool> = normalized_names
                .iter()
                .map(|name| forced_client_side_names.contains(name))
                .collect();
            let remote_size_bytes_by_source: Vec<u64> = source_names
                .iter()
                .map(|name| self.repository_addon_remote_size_bytes(&repo_address, name))
                .collect();
            let mut sorted_indices: Vec<usize> = (0..source.len()).collect();
            sorted_indices.sort_by(|&left, &right| {
                favorite_by_source[right]
                    .cmp(&favorite_by_source[left])
                    .then_with(|| locale_compare(&normalized_names[left], &normalized_names[right]))
                    .then_with(|| locale_compare(&source_names[left], &source_names[right]))
            });
            let preferred_paths: Vec<Option<String>> = normalized_names
                .iter()
                .map(|name_lower| {
                    self.addon_inventory_view_cache
                        .addon_paths_by_name
                        .get(name_lower)
                        .and_then(|paths| {
                            Self::resolve_preferred_addon_path_cached(paths, &repo_path_normalized)
                        })
                })
                .collect();

            let cache = self.repository_addon_list_cache_mut_cached(kind);
            cache.repo_index = Some(repo_index);
            cache.selected_profile = selected_profile;
            cache.inventory_generation_seen = inventory_generation;
            cache.repo_path_normalized = repo_path_normalized;
            cache.source_names = source_names;
            cache.normalized_names = normalized_names;
            cache.enabled_by_source = enabled_by_source;
            cache.favorite_by_source = favorite_by_source;
            cache.client_side_by_source = client_side_by_source;
            cache.forced_client_side_by_source = forced_client_side_by_source;
            cache.remote_size_bytes_by_source = remote_size_bytes_by_source;
            cache.sorted_indices = sorted_indices;
            cache.preferred_paths = preferred_paths;
            cache.file_entries_by_source = vec![None; source_len];
            cache.expanded_source_indices.clear();
            cache.file_search_matches_by_source = vec![false; source_len];
            cache.filtered_indices.clear();
            cache.filter_lower.clear();
            cache.state_filter.clear();
            cache.favorites_only_filter = false;
            cache.client_side_only_filter = false;
            cache.include_file_search_filter = false;
            cache.filters_dirty = true;
            // Names and resolved paths changed, so the shaped galleys are stale.
            cache.galleys = Default::default();
            cache.galley_prewarm_cursor = 0;
            cache.galley_prewarm_path_width = None;
            return;
        }

        let enabled_changed = {
            let cache = self.repository_addon_list_cache_cached(kind);
            cache.enabled_by_source.len() != source.len()
                || cache
                    .enabled_by_source
                    .iter()
                    .zip(source.iter())
                    .any(|(cached, (_, enabled))| *cached != *enabled)
        };

        if enabled_changed {
            let enabled_by_source: Vec<bool> = source.iter().map(|(_, enabled)| *enabled).collect();
            let cache = self.repository_addon_list_cache_mut_cached(kind);
            cache.enabled_by_source = enabled_by_source;
            cache.filters_dirty = true;
        }

        let favorites_changed = {
            let cache = self.repository_addon_list_cache_cached(kind);
            cache.favorite_by_source.len() != source_len
                || cache
                    .favorite_by_source
                    .iter()
                    .zip(cache.normalized_names.iter())
                    .any(|(cached, name)| *cached != optional_favorite_names.contains(name))
        };

        if favorites_changed {
            let favorite_by_source: Vec<bool> = self
                .repository_addon_list_cache_cached(kind)
                .normalized_names
                .iter()
                .map(|name| optional_favorite_names.contains(name))
                .collect();
            let mut sorted_indices = self
                .repository_addon_list_cache_cached(kind)
                .sorted_indices
                .clone();
            let normalized_names = self
                .repository_addon_list_cache_cached(kind)
                .normalized_names
                .clone();
            let source_names = self
                .repository_addon_list_cache_cached(kind)
                .source_names
                .clone();
            sorted_indices.sort_by(|&left, &right| {
                favorite_by_source[right]
                    .cmp(&favorite_by_source[left])
                    .then_with(|| locale_compare(&normalized_names[left], &normalized_names[right]))
                    .then_with(|| locale_compare(&source_names[left], &source_names[right]))
            });
            let cache = self.repository_addon_list_cache_mut_cached(kind);
            cache.favorite_by_source = favorite_by_source;
            cache.sorted_indices = sorted_indices;
            cache.filters_dirty = true;
        }

        let client_side_changed = {
            let cache = self.repository_addon_list_cache_cached(kind);
            cache.client_side_by_source.len() != source_len
                || cache.forced_client_side_by_source.len() != source_len
                || cache
                    .client_side_by_source
                    .iter()
                    .zip(cache.normalized_names.iter())
                    .any(|(cached, name)| {
                        *cached
                            != (optional_client_side_names.contains(name)
                                || forced_client_side_names.contains(name))
                    })
                || cache
                    .forced_client_side_by_source
                    .iter()
                    .zip(cache.normalized_names.iter())
                    .any(|(cached, name)| *cached != forced_client_side_names.contains(name))
        };

        if client_side_changed {
            let client_side_by_source: Vec<bool> = self
                .repository_addon_list_cache_cached(kind)
                .normalized_names
                .iter()
                .map(|name| {
                    optional_client_side_names.contains(name)
                        || forced_client_side_names.contains(name)
                })
                .collect();
            let forced_client_side_by_source: Vec<bool> = self
                .repository_addon_list_cache_cached(kind)
                .normalized_names
                .iter()
                .map(|name| forced_client_side_names.contains(name))
                .collect();
            let cache = self.repository_addon_list_cache_mut_cached(kind);
            cache.client_side_by_source = client_side_by_source;
            cache.forced_client_side_by_source = forced_client_side_by_source;
            cache.filters_dirty = true;
        }
    }

    pub(super) fn set_repository_addon_row_enabled_cached(
        &mut self,
        kind: RepositoryAddonListKind,
        source_index: usize,
        enabled: bool,
    ) -> bool {
        let cache = self.repository_addon_list_cache_mut_cached(kind);
        let Some(current_enabled) = cache.enabled_by_source.get_mut(source_index) else {
            return false;
        };
        if *current_enabled == enabled {
            return false;
        }
        *current_enabled = enabled;
        cache.filters_dirty = true;
        true
    }

    pub(super) fn set_all_repository_addon_rows_enabled_cached(
        &mut self,
        kind: RepositoryAddonListKind,
        enabled: bool,
    ) -> bool {
        let cache = self.repository_addon_list_cache_mut_cached(kind);
        let mut changed = false;
        for current_enabled in &mut cache.enabled_by_source {
            if *current_enabled != enabled {
                *current_enabled = enabled;
                changed = true;
            }
        }
        if changed {
            cache.filters_dirty = true;
        }
        changed
    }

    pub(super) fn set_repository_optional_addon_row_favorite_cached(
        &mut self,
        source_index: usize,
        favorite: bool,
    ) -> bool {
        let cache =
            self.repository_addon_list_cache_mut_cached(RepositoryAddonListKind::OptionalAddons);
        let Some(current_favorite) = cache.favorite_by_source.get_mut(source_index) else {
            return false;
        };
        if *current_favorite == favorite {
            return false;
        }
        *current_favorite = favorite;
        cache.sorted_indices.sort_by(|&left, &right| {
            cache.favorite_by_source[right]
                .cmp(&cache.favorite_by_source[left])
                .then_with(|| {
                    locale_compare(
                        &cache.normalized_names[left],
                        &cache.normalized_names[right],
                    )
                })
                .then_with(|| locale_compare(&cache.source_names[left], &cache.source_names[right]))
        });
        cache.filters_dirty = true;
        true
    }

    pub(super) fn set_repository_optional_addon_row_client_side_cached(
        &mut self,
        source_index: usize,
        client_side: bool,
    ) -> bool {
        let cache =
            self.repository_addon_list_cache_mut_cached(RepositoryAddonListKind::OptionalAddons);
        let Some(current_client_side) = cache.client_side_by_source.get_mut(source_index) else {
            return false;
        };
        if cache
            .forced_client_side_by_source
            .get(source_index)
            .copied()
            .unwrap_or(false)
        {
            return false;
        }
        if *current_client_side == client_side {
            return false;
        }
        *current_client_side = client_side;
        cache.filters_dirty = true;
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn ensure_filtered_repository_addon_indices_cached(
        &mut self,
        repo_index: usize,
        kind: RepositoryAddonListKind,
        filter: &str,
        addon_state_filter: &str,
        favorites_only: bool,
        client_side_only: bool,
        include_file_search: bool,
    ) {
        self.ensure_repository_addon_list_cache_cached(repo_index, kind);

        let filter_lower = filter.to_lowercase();
        let multi_filter = MultiEntryFilter::parse(filter);
        let needs_rebuild = {
            let cache = self.repository_addon_list_cache_cached(kind);
            cache.filters_dirty
                || cache.filter_lower != filter_lower
                || cache.state_filter != addon_state_filter
                || cache.favorites_only_filter != favorites_only
                || cache.client_side_only_filter != client_side_only
                || cache.include_file_search_filter != include_file_search
        };

        if !needs_rebuild {
            return;
        }

        if include_file_search && !multi_filter.is_empty() {
            let indices = self
                .repository_addon_list_cache_cached(kind)
                .sorted_indices
                .clone();
            for source_index in indices {
                self.ensure_repository_addon_file_structure_cached(kind, source_index);
            }
        }

        let (filtered_indices, file_search_matches_by_source) = {
            let cache = self.repository_addon_list_cache_cached(kind);
            let mut file_search_matches_by_source = vec![false; cache.source_names.len()];
            let include_file_search = include_file_search && !multi_filter.is_empty();
            let filtered_indices = cache
                .sorted_indices
                .iter()
                .copied()
                .filter(|&source_index| {
                    let matches_text_filter = multi_filter
                        .matches_any_normalized(&[cache.normalized_names[source_index].as_str()]);
                    let matches_file_filter = include_file_search
                        && cache
                            .file_entries_by_source
                            .get(source_index)
                            .and_then(|structure| structure.as_ref())
                            .is_some_and(|structure| {
                                structure.files.iter().any(|file| {
                                    multi_filter.matches_any_normalized(&[
                                        file.name_lower.as_str(),
                                        file.path_lower.as_str(),
                                    ])
                                })
                            });
                    file_search_matches_by_source[source_index] = matches_file_filter;
                    let matches_state_filter = match addon_state_filter {
                        "Enabled" => cache.enabled_by_source[source_index],
                        "Disabled" => !cache.enabled_by_source[source_index],
                        _ => true,
                    };
                    let matches_favorite_filter =
                        !favorites_only || cache.favorite_by_source[source_index];
                    let matches_client_side_filter =
                        !client_side_only || cache.client_side_by_source[source_index];
                    (matches_text_filter || matches_file_filter)
                        && matches_state_filter
                        && matches_favorite_filter
                        && matches_client_side_filter
                })
                .collect::<Vec<_>>();
            (filtered_indices, file_search_matches_by_source)
        };

        let cache = self.repository_addon_list_cache_mut_cached(kind);
        cache.filtered_indices = filtered_indices;
        cache.file_search_matches_by_source = file_search_matches_by_source;
        cache.galley_prewarm_cursor = 0;
        cache.filter_lower = filter_lower;
        cache.state_filter = addon_state_filter.to_string();
        cache.favorites_only_filter = favorites_only;
        cache.client_side_only_filter = client_side_only;
        cache.include_file_search_filter = include_file_search;
        cache.filters_dirty = false;
    }

    pub(super) fn persist_repository_addon_row_state_cached(
        &mut self,
        repo_index: usize,
        kind: RepositoryAddonListKind,
    ) {
        let enabled_by_source = self
            .repository_addon_list_cache_cached(kind)
            .enabled_by_source
            .clone();
        let Some(addons) = self.current_repository_addons_mut_cached(repo_index, kind) else {
            return;
        };
        for ((_, enabled), cached_enabled) in addons.iter_mut().zip(enabled_by_source) {
            *enabled = cached_enabled;
        }
        self.save_repositories();
    }

    pub(super) fn persist_repository_optional_addon_favorite_state_cached(
        &mut self,
        repo_index: usize,
    ) {
        let favorite_names: Vec<String> = self
            .repository_optional_addons_list_cache
            .source_names
            .iter()
            .zip(
                self.repository_optional_addons_list_cache
                    .favorite_by_source
                    .iter(),
            )
            .filter_map(|(name, favorite)| favorite.then_some(name.clone()))
            .collect();
        let Some(favorites) =
            self.current_repository_optional_addon_favorites_mut_cached(repo_index)
        else {
            return;
        };
        *favorites = favorite_names;
        self.save_repositories();
    }

    pub(super) fn persist_repository_optional_addon_client_side_state_cached(
        &mut self,
        repo_index: usize,
    ) {
        let client_side_names: Vec<String> = self
            .repository_optional_addons_list_cache
            .source_names
            .iter()
            .zip(
                self.repository_optional_addons_list_cache
                    .client_side_by_source
                    .iter(),
            )
            .zip(
                self.repository_optional_addons_list_cache
                    .forced_client_side_by_source
                    .iter(),
            )
            .filter_map(|((name, client_side), forced)| {
                (*client_side && !*forced).then_some(name.clone())
            })
            .collect();
        let Some(client_side) =
            self.current_repository_optional_addon_client_side_mut_cached(repo_index)
        else {
            return;
        };
        *client_side = client_side_names;
        self.save_repositories();
    }

    pub(super) fn current_repository_external_addons_cached(
        &self,
        repo_index: usize,
    ) -> Option<&Vec<(String, bool, String)>> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(&profile.external_addons);
        }

        Some(&repo.external_addons)
    }

    pub(super) fn current_repository_external_addons_mut_cached(
        &mut self,
        repo_index: usize,
    ) -> Option<&mut Vec<(String, bool, String)>> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(repo_index)?;
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            return Some(&mut profile.external_addons);
        }

        Some(&mut repo.external_addons)
    }

    pub(super) fn current_repository_external_addon_favorites_cached(
        &self,
        repo_index: usize,
    ) -> Option<&Vec<String>> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(&profile.external_addon_favorites);
        }

        Some(&repo.external_addon_favorites)
    }

    pub(super) fn current_repository_external_addon_favorites_mut_cached(
        &mut self,
        repo_index: usize,
    ) -> Option<&mut Vec<String>> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(repo_index)?;
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            return Some(&mut profile.external_addon_favorites);
        }

        Some(&mut repo.external_addon_favorites)
    }

    pub(super) fn current_repository_external_addon_client_side_cached(
        &self,
        repo_index: usize,
    ) -> Option<&Vec<String>> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(&profile.external_addon_client_side);
        }

        Some(&repo.external_addon_client_side)
    }

    pub(super) fn current_repository_external_addon_client_side_mut_cached(
        &mut self,
        repo_index: usize,
    ) -> Option<&mut Vec<String>> {
        let repo = self
            .repository_view_state
            .repositories
            .get_mut(repo_index)?;
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            return Some(&mut profile.external_addon_client_side);
        }

        Some(&mut repo.external_addon_client_side)
    }

    pub(super) fn current_repository_include_steam_addons_cached(
        &self,
        repo_index: usize,
    ) -> Option<bool> {
        let repo = self.repository_view_state.repositories.get(repo_index)?;
        if let Some(selected_name) = repo.selected_profile.as_deref()
            && let Some(profile) = repo.profiles.iter().find(|p| p.name == selected_name)
        {
            return Some(profile.include_steam_addons);
        }

        Some(repo.include_steam_addons)
    }

    pub(super) fn set_current_repository_include_steam_addons_cached(
        &mut self,
        repo_index: usize,
        enabled: bool,
    ) -> bool {
        let Some(repo) = self.repository_view_state.repositories.get_mut(repo_index) else {
            return false;
        };
        let selected_name = repo.selected_profile.clone();
        if let Some(selected_name) = selected_name.as_deref()
            && let Some(profile) = repo.profiles.iter_mut().find(|p| p.name == selected_name)
        {
            if profile.include_steam_addons == enabled {
                return false;
            }
            profile.include_steam_addons = enabled;
            return true;
        }

        if repo.include_steam_addons == enabled {
            return false;
        }
        repo.include_steam_addons = enabled;
        true
    }

    pub(super) fn current_external_addon_lookup_keys_cached(
        &self,
        repo_index: usize,
    ) -> HashSet<(String, String)> {
        self.current_repository_external_addons_cached(repo_index)
            .map(|addons| {
                addons
                    .iter()
                    .filter(|(_, enabled, _)| *enabled)
                    .map(|(name, _, path)| {
                        (
                            name.to_lowercase(),
                            normalize_local_path_for_compare(path).to_lowercase(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn current_external_addon_favorite_lookup_keys_cached(
        &self,
        repo_index: usize,
    ) -> HashSet<String> {
        self.current_repository_external_addon_favorites_cached(repo_index)
            .map(|paths| {
                paths
                    .iter()
                    .map(|path| normalize_local_path_for_compare(path).to_lowercase())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn current_external_addon_client_side_lookup_keys_cached(
        &self,
        repo_index: usize,
    ) -> HashSet<String> {
        self.current_repository_external_addon_client_side_cached(repo_index)
            .map(|paths| {
                paths
                    .iter()
                    .map(|path| normalize_local_path_for_compare(path).to_lowercase())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn ensure_repository_external_addons_base_cache_cached(
        &mut self,
        repo_index: usize,
    ) {
        self.ensure_addon_inventory_view_cache_cached();

        let Some(repo) = self.repository_view_state.repositories.get(repo_index) else {
            return;
        };
        let include_steam_addons = self
            .current_repository_include_steam_addons_cached(repo_index)
            .unwrap_or(false);
        let selected_profile = self.current_repository_profile_name_cached(repo_index);
        let workshop_root = self.normalized_steam_workshop_root_path();
        let scope_key =
            repository_external_addon_scope_key(&self.repository_view_state.repositories);

        // Detect changes without allocating: this runs every frame while the
        // External Addons tab is open, so cloning the local addon name vectors
        // here (only to compare) would churn allocations during scrolling.
        let local_names_changed = |cached: &[String], source: &[(String, bool)]| -> bool {
            cached.len() != source.len()
                || cached
                    .iter()
                    .zip(source.iter())
                    .any(|(cached_name, (name, _))| cached_name != name)
        };

        let (needs_rebuild, reset_collapsed_origins) = {
            let cache = &self.repository_external_addons_list_cache;
            (
                cache.repo_index != Some(repo_index)
                    || cache.inventory_generation_seen != self.addon_inventory_generation
                    || cache.scope_key != scope_key
                    || cache.include_steam_addons != include_steam_addons
                    || local_names_changed(&cache.local_addon_names, &repo.addons)
                    || local_names_changed(
                        &cache.local_optional_addon_names,
                        &repo.optional_addons,
                    )
                    || cache.remote_client_side_addon_names != repo.remote_client_side_addons,
                cache.repo_index != Some(repo_index),
            )
        };

        if needs_rebuild {
            let local_addon_names: Vec<String> =
                repo.addons.iter().map(|(name, _)| name.clone()).collect();
            let local_optional_addon_names: Vec<String> = repo
                .optional_addons
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            let remote_client_side_addon_names = repo.remote_client_side_addons.clone();
            let local_name_lowers: HashSet<String> = repo
                .addons
                .iter()
                .map(|(name, _)| name.to_lowercase())
                .chain(
                    repo.optional_addons
                        .iter()
                        .map(|(name, _)| name.to_lowercase()),
                )
                .collect();

            let (rows, origin_options) = {
                let all_addons = self.get_or_generate_all_addons().clone();
                let mut rows = Vec::new();
                let mut origins = HashSet::new();

                for (addon_name, path, origin, local_size_bytes) in &all_addons {
                    let addon_name_lower = addon_name.to_lowercase();
                    if local_name_lowers.contains(&addon_name_lower) {
                        continue;
                    }

                    if !repository_external_addon_visible_for_repo(
                        &self.repository_view_state.repositories,
                        repo_index,
                        path,
                    ) {
                        continue;
                    }

                    if !include_steam_addons
                        && Foxy::is_steam_workshop_path_with_root(path, workshop_root.as_deref())
                    {
                        continue;
                    }
                    let path_lower = path.to_lowercase();

                    origins.insert(origin.clone());
                    rows.push(crate::ui::app::ExternalAddonRowCache {
                        addon_name: addon_name.clone(),
                        path: path.clone(),
                        origin: origin.clone(),
                        addon_name_lower,
                        path_lower,
                        origin_lower: origin.to_lowercase(),
                        path_lookup_key: normalize_local_path_for_compare(path).to_lowercase(),
                        local_size_bytes: *local_size_bytes,
                    });
                }

                let mut origin_options: Vec<String> = origins.into_iter().collect();
                origin_options.sort_by(|a, b| locale_compare(a, b));
                (rows, origin_options)
            };

            let enabled_lookup = self.current_external_addon_lookup_keys_cached(repo_index);
            let favorite_lookup =
                self.current_external_addon_favorite_lookup_keys_cached(repo_index);
            let client_side_lookup =
                self.current_external_addon_client_side_lookup_keys_cached(repo_index);
            let enabled_by_row: Vec<bool> = rows
                .iter()
                .map(|row| {
                    enabled_lookup
                        .contains(&(row.addon_name_lower.clone(), row.path_lookup_key.clone()))
                })
                .collect();
            let favorite_by_row = rows
                .iter()
                .map(|row| favorite_lookup.contains(&row.path_lookup_key))
                .collect();
            let client_side_by_row = rows
                .iter()
                .map(|row| client_side_lookup.contains(&row.path_lookup_key))
                .collect::<Vec<_>>();
            let forced_client_side_by_row = rows
                .iter()
                .map(|row| self.addon_is_repo_defined_client_side(&row.addon_name, &row.path))
                .collect::<Vec<_>>();
            let client_side_by_row = rows
                .iter()
                .zip(client_side_by_row)
                .zip(&forced_client_side_by_row)
                .map(|((_, client_side), forced)| client_side || *forced)
                .collect();
            let enabled_count = enabled_by_row.iter().filter(|enabled| **enabled).count();
            let enabled_size_bytes = rows
                .iter()
                .zip(&enabled_by_row)
                .filter(|(_, enabled)| **enabled)
                .filter_map(|(row, _)| row.local_size_bytes)
                .sum::<u64>();
            let total_size_bytes = rows
                .iter()
                .filter_map(|row| row.local_size_bytes)
                .sum::<u64>();

            let cache = &mut self.repository_external_addons_list_cache;
            cache.repo_index = Some(repo_index);
            cache.selected_profile = selected_profile;
            cache.include_steam_addons = include_steam_addons;
            cache.inventory_generation_seen = self.addon_inventory_generation;
            cache.scope_key = scope_key;
            cache.local_addon_names = local_addon_names;
            cache.local_optional_addon_names = local_optional_addon_names;
            cache.remote_client_side_addon_names = remote_client_side_addon_names;
            cache.rows = rows;
            cache.origin_options = origin_options;
            if reset_collapsed_origins {
                cache.collapsed_origins.clear();
            } else {
                cache.collapsed_origins.retain(|origin| {
                    cache
                        .origin_options
                        .iter()
                        .any(|current_origin| current_origin == origin)
                });
            }
            cache.enabled_by_row = enabled_by_row;
            cache.favorite_by_row = favorite_by_row;
            cache.client_side_by_row = client_side_by_row;
            cache.forced_client_side_by_row = forced_client_side_by_row;
            cache.file_entries_by_row = vec![None; cache.rows.len()];
            cache.expanded_row_indices.clear();
            cache.file_search_matches_by_row = vec![false; cache.rows.len()];
            cache.enabled_count = enabled_count;
            cache.enabled_size_bytes = enabled_size_bytes;
            cache.filtered_size_bytes = 0;
            cache.total_size_bytes = total_size_bytes;
            cache.filtered_indices.clear();
            cache.grouped_filtered_indices.clear();
            cache.filter_lower.clear();
            cache.origin_filter.clear();
            cache.state_filter.clear();
            cache.favorites_only_filter = false;
            cache.client_side_only_filter = false;
            cache.include_file_search_filter = false;
            cache.filters_dirty = true;
            // Row names, paths and origins changed, so the shaped galleys are stale.
            cache.galleys = Default::default();
            cache.galley_prewarm_cursor = 0;
            cache.galley_prewarm_path_width = None;
            cache.galley_prewarm_include_origin = false;
            return;
        }

        if self.repository_external_addons_list_cache.selected_profile != selected_profile {
            let enabled_lookup = self.current_external_addon_lookup_keys_cached(repo_index);
            let favorite_lookup =
                self.current_external_addon_favorite_lookup_keys_cached(repo_index);
            let client_side_lookup =
                self.current_external_addon_client_side_lookup_keys_cached(repo_index);
            let enabled_by_row: Vec<bool> = self
                .repository_external_addons_list_cache
                .rows
                .iter()
                .map(|row| {
                    enabled_lookup
                        .contains(&(row.addon_name_lower.clone(), row.path_lookup_key.clone()))
                })
                .collect();
            let favorite_by_row = self
                .repository_external_addons_list_cache
                .rows
                .iter()
                .map(|row| favorite_lookup.contains(&row.path_lookup_key))
                .collect();
            let client_side_by_row = self
                .repository_external_addons_list_cache
                .rows
                .iter()
                .map(|row| client_side_lookup.contains(&row.path_lookup_key))
                .collect::<Vec<_>>();
            let forced_client_side_by_row = self
                .repository_external_addons_list_cache
                .rows
                .iter()
                .map(|row| self.addon_is_repo_defined_client_side(&row.addon_name, &row.path))
                .collect::<Vec<_>>();
            let client_side_by_row = self
                .repository_external_addons_list_cache
                .rows
                .iter()
                .zip(client_side_by_row)
                .zip(&forced_client_side_by_row)
                .map(|((_, client_side), forced)| client_side || *forced)
                .collect();
            let enabled_count = enabled_by_row.iter().filter(|enabled| **enabled).count();
            let enabled_size_bytes = self
                .repository_external_addons_list_cache
                .rows
                .iter()
                .zip(&enabled_by_row)
                .filter(|(_, enabled)| **enabled)
                .filter_map(|(row, _)| row.local_size_bytes)
                .sum::<u64>();
            let cache = &mut self.repository_external_addons_list_cache;
            cache.selected_profile = selected_profile;
            cache.enabled_by_row = enabled_by_row;
            cache.favorite_by_row = favorite_by_row;
            cache.client_side_by_row = client_side_by_row;
            cache.forced_client_side_by_row = forced_client_side_by_row;
            cache.enabled_count = enabled_count;
            cache.enabled_size_bytes = enabled_size_bytes;
            cache.filters_dirty = true;
        }
    }

    pub(super) fn set_repository_external_addon_row_enabled_cached(
        &mut self,
        row_index: usize,
        enabled: bool,
    ) -> bool {
        let cache = &mut self.repository_external_addons_list_cache;
        let Some(current_enabled) = cache.enabled_by_row.get_mut(row_index) else {
            return false;
        };
        if *current_enabled == enabled {
            return false;
        }
        *current_enabled = enabled;
        let row_size = cache
            .rows
            .get(row_index)
            .and_then(|row| row.local_size_bytes)
            .unwrap_or(0);
        if enabled {
            cache.enabled_count += 1;
            cache.enabled_size_bytes = cache.enabled_size_bytes.saturating_add(row_size);
        } else {
            cache.enabled_count = cache.enabled_count.saturating_sub(1);
            cache.enabled_size_bytes = cache.enabled_size_bytes.saturating_sub(row_size);
        }
        cache.filters_dirty = true;
        true
    }

    pub(super) fn set_all_repository_external_addon_rows_enabled_cached(
        &mut self,
        enabled: bool,
    ) -> bool {
        let cache = &mut self.repository_external_addons_list_cache;
        let mut changed = false;
        for current_enabled in &mut cache.enabled_by_row {
            if *current_enabled != enabled {
                *current_enabled = enabled;
                changed = true;
            }
        }
        if changed {
            cache.enabled_count = if enabled {
                cache.enabled_by_row.len()
            } else {
                0
            };
            cache.enabled_size_bytes = if enabled {
                cache
                    .rows
                    .iter()
                    .filter_map(|row| row.local_size_bytes)
                    .sum()
            } else {
                0
            };
            cache.filters_dirty = true;
        }
        changed
    }

    pub(super) fn set_repository_external_addon_row_favorite_cached(
        &mut self,
        row_index: usize,
        favorite: bool,
    ) -> bool {
        let cache = &mut self.repository_external_addons_list_cache;
        let Some(current_favorite) = cache.favorite_by_row.get_mut(row_index) else {
            return false;
        };
        if *current_favorite == favorite {
            return false;
        }
        *current_favorite = favorite;
        cache.filters_dirty = true;
        true
    }

    pub(super) fn set_repository_external_addon_row_client_side_cached(
        &mut self,
        row_index: usize,
        client_side: bool,
    ) -> bool {
        let cache = &mut self.repository_external_addons_list_cache;
        if cache
            .forced_client_side_by_row
            .get(row_index)
            .copied()
            .unwrap_or(false)
        {
            return false;
        }
        let Some(current_client_side) = cache.client_side_by_row.get_mut(row_index) else {
            return false;
        };
        if *current_client_side == client_side {
            return false;
        }
        *current_client_side = client_side;
        cache.filters_dirty = true;
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn ensure_filtered_repository_external_addon_indices_cached(
        &mut self,
        repo_index: usize,
        filter: &str,
        origin_filter: &str,
        addon_state_filter: &str,
        favorites_only: bool,
        client_side_only: bool,
        include_file_search: bool,
    ) {
        self.ensure_repository_external_addons_base_cache_cached(repo_index);

        let filter_lower = filter.to_lowercase();
        let needs_rebuild = {
            let cache = &self.repository_external_addons_list_cache;
            cache.filters_dirty
                || cache.filter_lower != filter_lower
                || cache.origin_filter != origin_filter
                || cache.state_filter != addon_state_filter
                || cache.favorites_only_filter != favorites_only
                || cache.client_side_only_filter != client_side_only
                || cache.include_file_search_filter != include_file_search
        };

        if !needs_rebuild {
            return;
        }

        let multi_filter = MultiEntryFilter::parse(filter);
        if include_file_search && !multi_filter.is_empty() {
            let row_count = self.repository_external_addons_list_cache.rows.len();
            for row_index in 0..row_count {
                self.ensure_repository_external_addon_file_structure_cached(row_index);
            }
        }

        let (
            filtered_indices,
            grouped_filtered_indices,
            filtered_size_bytes,
            file_search_matches_by_row,
        ) = {
            let cache = &self.repository_external_addons_list_cache;
            let mut filtered_size_bytes = 0;
            let mut file_search_matches_by_row = vec![false; cache.rows.len()];
            let include_file_search = include_file_search && !multi_filter.is_empty();
            let filtered_indices: Vec<usize> = cache
                .rows
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    let matches_text_filter = multi_filter.matches_any_normalized(&[
                        row.addon_name_lower.as_str(),
                        row.path_lower.as_str(),
                        row.origin_lower.as_str(),
                    ]);
                    let matches_file_filter = include_file_search
                        && cache
                            .file_entries_by_row
                            .get(index)
                            .and_then(|structure| structure.as_ref())
                            .is_some_and(|structure| {
                                structure.files.iter().any(|file| {
                                    multi_filter.matches_any_normalized(&[
                                        file.name_lower.as_str(),
                                        file.path_lower.as_str(),
                                    ])
                                })
                            });
                    file_search_matches_by_row[index] = matches_file_filter;
                    let matches_origin_filter =
                        origin_filter == "All" || row.origin.as_str() == origin_filter;
                    let matches_state_filter = match addon_state_filter {
                        "Enabled" => cache.enabled_by_row[index],
                        "Disabled" => !cache.enabled_by_row[index],
                        _ => true,
                    };
                    let matches_favorite_filter = !favorites_only || cache.favorite_by_row[index];
                    let matches_client_side_filter =
                        !client_side_only || cache.client_side_by_row[index];

                    let matches = (matches_text_filter || matches_file_filter)
                        && matches_origin_filter
                        && matches_state_filter
                        && matches_favorite_filter
                        && matches_client_side_filter;
                    if matches {
                        filtered_size_bytes += row.local_size_bytes.unwrap_or(0);
                        Some(index)
                    } else {
                        None
                    }
                })
                .collect();

            let mut grouped_filtered_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for entry_index in &filtered_indices {
                let origin = if cache.favorite_by_row[*entry_index] {
                    "Favorites".to_string()
                } else {
                    cache.rows[*entry_index].origin.clone()
                };
                grouped_filtered_indices
                    .entry(origin)
                    .or_default()
                    .push(*entry_index);
            }

            let mut grouped_filtered_indices =
                grouped_filtered_indices.into_iter().collect::<Vec<_>>();
            if let Some(favorites_index) = grouped_filtered_indices
                .iter()
                .position(|(origin, _)| origin == "Favorites")
            {
                let favorites_group = grouped_filtered_indices.remove(favorites_index);
                grouped_filtered_indices.insert(0, favorites_group);
            }

            (
                filtered_indices,
                grouped_filtered_indices,
                filtered_size_bytes,
                file_search_matches_by_row,
            )
        };

        let cache = &mut self.repository_external_addons_list_cache;
        cache.filtered_indices = filtered_indices;
        cache.grouped_filtered_indices = grouped_filtered_indices;
        cache.filtered_size_bytes = filtered_size_bytes;
        cache.file_search_matches_by_row = file_search_matches_by_row;
        cache.galley_prewarm_cursor = 0;
        cache.filter_lower = filter_lower;
        cache.origin_filter = origin_filter.to_string();
        cache.state_filter = addon_state_filter.to_string();
        cache.favorites_only_filter = favorites_only;
        cache.client_side_only_filter = client_side_only;
        cache.include_file_search_filter = include_file_search;
        cache.filters_dirty = false;
    }

    pub(super) fn persist_repository_external_addon_row_state_cached(&mut self, repo_index: usize) {
        let new_external_addons: Vec<(String, bool, String)> = self
            .repository_external_addons_list_cache
            .rows
            .iter()
            .zip(
                self.repository_external_addons_list_cache
                    .enabled_by_row
                    .iter(),
            )
            .filter_map(|(row, enabled)| {
                enabled.then_some((row.addon_name.clone(), true, row.path.clone()))
            })
            .collect();

        let Some(external_addons) = self.current_repository_external_addons_mut_cached(repo_index)
        else {
            return;
        };
        *external_addons = new_external_addons;
        self.save_repositories();
    }

    pub(super) fn persist_repository_external_addon_favorite_state_cached(
        &mut self,
        repo_index: usize,
    ) {
        let favorite_paths: Vec<String> = self
            .repository_external_addons_list_cache
            .rows
            .iter()
            .zip(
                self.repository_external_addons_list_cache
                    .favorite_by_row
                    .iter(),
            )
            .filter_map(|(row, favorite)| favorite.then_some(row.path.clone()))
            .collect();
        let Some(favorites) =
            self.current_repository_external_addon_favorites_mut_cached(repo_index)
        else {
            return;
        };
        *favorites = favorite_paths;
        self.save_repositories();
    }

    pub(super) fn persist_repository_external_addon_client_side_state_cached(
        &mut self,
        repo_index: usize,
    ) {
        let client_side_paths: Vec<String> = self
            .repository_external_addons_list_cache
            .rows
            .iter()
            .zip(
                self.repository_external_addons_list_cache
                    .client_side_by_row
                    .iter(),
            )
            .zip(
                self.repository_external_addons_list_cache
                    .forced_client_side_by_row
                    .iter(),
            )
            .filter_map(|((row, client_side), forced)| {
                (*client_side && !*forced).then_some(row.path.clone())
            })
            .collect();
        let Some(client_side) =
            self.current_repository_external_addon_client_side_mut_cached(repo_index)
        else {
            return;
        };
        *client_side = client_side_paths;
        self.save_repositories();
    }
}

fn load_repository_addon_sizes() -> RepositoryAddonSizeLoadResult {
    let mut result = RepositoryAddonSizeLoadResult::default();
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            log::warn!(
                "Failed to start repository addon size loader runtime: {}",
                err
            );
            return result;
        }
    };

    let rows = rt.block_on(async {
        let db = FoxyDb::from_handle(crate::core::tasks::init_database::init_database().await);
        db.query_all(
            r#"
            SELECT r.remote_url AS repo_url,
                   a.name AS addon_name,
                   COALESCE(SUM(CASE WHEN f.length > 0 THEN f.length ELSE 0 END), 0) AS size_bytes
              FROM repositories r
              JOIN repository_addons ra ON ra.repository_id = r.id
              JOIN addons a ON a.id = ra.addon_id
              LEFT JOIN addon_files af ON af.addon_id = a.id
              LEFT JOIN files f ON f.id = af.file_id
             GROUP BY r.remote_url, a.name
            "#,
            params![],
        )
        .await
    });

    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            log::warn!("Failed to load repository addon sizes: {}", err);
            return result;
        }
    };

    for row in rows {
        let Ok(repo_url) = row.get_string("repo_url") else {
            continue;
        };
        let Ok(addon_name) = row.get_string("addon_name") else {
            continue;
        };
        let size_bytes = row.get_i64("size_bytes").unwrap_or(0).max(0) as u64;
        result.sizes_by_repo_and_addon.insert(
            Foxy::repository_addon_size_key(&repo_url, &addon_name),
            size_bytes,
        );
    }

    result
}

fn repository_space_remote_size_bytes_for_repositories(
    space_id: &str,
    repositories: &[crate::ui::types::Repository],
    sizes_by_repo_and_addon: &HashMap<(String, String), u64>,
) -> u64 {
    let mut size_by_path_and_addon: HashMap<(String, String), u64> = HashMap::new();

    for repo in repositories
        .iter()
        .filter(|repo| repo.repository_space_id.as_deref() == Some(space_id))
    {
        let repo_url = Foxy::normalize_repo_url(&repo.address);
        let path_key = {
            let normalized_path = normalize_local_path_for_compare(&repo.path);
            if normalized_path.is_empty() {
                format!("repo:{repo_url}")
            } else {
                normalized_path
            }
        };

        for ((size_repo_url, addon_name), size_bytes) in sizes_by_repo_and_addon {
            if size_repo_url != &repo_url {
                continue;
            }

            size_by_path_and_addon
                .entry((path_key.clone(), addon_name.clone()))
                .and_modify(|existing_size| *existing_size = (*existing_size).max(*size_bytes))
                .or_insert(*size_bytes);
        }
    }

    size_by_path_and_addon.values().sum()
}

fn repository_external_addon_scope_key(repositories: &[crate::ui::types::Repository]) -> String {
    let mut parts = repositories
        .iter()
        .map(|repo| {
            format!(
                "{}|{}|{}",
                repo.repository_space_id.as_deref().unwrap_or_default(),
                Foxy::normalize_repo_url(&repo.address),
                normalize_local_path_for_compare(&repo.path)
            )
        })
        .collect::<Vec<_>>();
    parts.sort();
    parts.join("\n")
}

fn repository_external_addon_visible_for_repo(
    repositories: &[crate::ui::types::Repository],
    repo_index: usize,
    addon_path: &str,
) -> bool {
    let Some(current_repo) = repositories.get(repo_index) else {
        return false;
    };

    let normalized_addon_path = Foxy::normalize_origin_lookup_path(addon_path);
    if normalized_addon_path.is_empty() {
        return true;
    }

    let matching_repo_roots = repositories
        .iter()
        .enumerate()
        .filter_map(|(idx, repo)| {
            let repo_path = Foxy::normalize_origin_lookup_path(&repo.path);
            if repo_path.is_empty()
                || !Foxy::path_is_within_root(&normalized_addon_path, &repo_path)
            {
                return None;
            }
            Some((idx, repo))
        })
        .collect::<Vec<_>>();

    if matching_repo_roots.is_empty() {
        return true;
    }

    if matching_repo_roots
        .iter()
        .any(|(idx, _)| *idx == repo_index)
    {
        return true;
    }

    let Some(current_space_id) = current_repo.repository_space_id.as_deref() else {
        return false;
    };

    matching_repo_roots
        .iter()
        .any(|(_, repo)| repo.repository_space_id.as_deref() == Some(current_space_id))
}

fn scan_addon_folder_structure(addon_path: &str, path_key: &str) -> AddonFolderStructure {
    let root = Path::new(addon_path);
    let mut structure = AddonFolderStructure {
        path_key: path_key.to_string(),
        files: Vec::new(),
        truncated: false,
    };

    let Ok(metadata) = std::fs::metadata(root) else {
        return structure;
    };
    if metadata.is_file() {
        let display_path = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| addon_path.to_string());
        push_addon_folder_entry(&mut structure.files, display_path);
        return structure;
    }
    if !metadata.is_dir() {
        return structure;
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }

            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let display_path = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            push_addon_folder_entry(&mut structure.files, display_path);
            if structure.files.len() >= MAX_ADDON_STRUCTURE_FILES {
                structure.truncated = true;
                stack.clear();
                break;
            }
        }
    }

    structure
        .files
        .sort_by(|left, right| locale_compare(&left.path_lower, &right.path_lower));
    structure
}

fn push_addon_folder_entry(files: &mut Vec<AddonFolderEntry>, display_path: String) {
    let name_lower = PathBuf::from(&display_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| display_path.to_lowercase());
    files.push(AddonFolderEntry {
        path_lower: display_path.to_lowercase(),
        display_path,
        name_lower,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::types::Repository;

    fn repo(address: &str, path: &str, space_id: Option<&str>) -> Repository {
        Repository {
            address: address.to_string(),
            path: path.to_string(),
            repository_space_id: space_id.map(str::to_string),
            ..Repository::default()
        }
    }

    fn size_key(repo_address: &str, addon_name: &str) -> (String, String) {
        Foxy::repository_addon_size_key(repo_address, addon_name)
    }

    #[test]
    fn repository_space_size_deduplicates_shared_addon_paths() {
        let repositories = vec![
            repo(
                "https://example.test/alpha",
                "C:/shared/mods",
                Some("space"),
            ),
            repo(
                "https://example.test/bravo",
                "C:/shared/mods/",
                Some("space"),
            ),
        ];
        let sizes = HashMap::from([
            (size_key("https://example.test/alpha", "@ace"), 100),
            (size_key("https://example.test/alpha", "@rhs"), 200),
            (size_key("https://example.test/bravo", "@ace"), 100),
            (size_key("https://example.test/bravo", "@cba"), 50),
        ]);

        assert_eq!(
            repository_space_remote_size_bytes_for_repositories("space", &repositories, &sizes),
            350
        );
    }

    #[test]
    fn repository_space_size_keeps_same_addon_on_different_paths() {
        let repositories = vec![
            repo("https://example.test/alpha", "C:/mods/alpha", Some("space")),
            repo("https://example.test/bravo", "C:/mods/bravo", Some("space")),
        ];
        let sizes = HashMap::from([
            (size_key("https://example.test/alpha", "@ace"), 100),
            (size_key("https://example.test/bravo", "@ace"), 100),
        ]);

        assert_eq!(
            repository_space_remote_size_bytes_for_repositories("space", &repositories, &sizes),
            200
        );
    }

    #[test]
    fn repository_space_size_uses_largest_size_for_shared_addon_name() {
        let repositories = vec![
            repo(
                "https://example.test/alpha",
                "C:/shared/mods",
                Some("space"),
            ),
            repo(
                "https://example.test/bravo",
                "C:/shared/mods",
                Some("space"),
            ),
        ];
        let sizes = HashMap::from([
            (size_key("https://example.test/alpha", "@ace"), 100),
            (size_key("https://example.test/bravo", "@ace"), 125),
        ]);

        assert_eq!(
            repository_space_remote_size_bytes_for_repositories("space", &repositories, &sizes),
            125
        );
    }

    #[test]
    fn external_addon_scope_allows_same_space_repositories_with_different_roots() {
        let repositories = vec![
            repo("https://example.test/alpha", "C:/mods/alpha", Some("space")),
            repo("https://example.test/bravo", "D:/mods/bravo", Some("space")),
            repo(
                "https://example.test/other",
                "E:/mods/other",
                Some("other-space"),
            ),
        ];

        assert!(repository_external_addon_visible_for_repo(
            &repositories,
            0,
            "D:/mods/bravo/@shared"
        ));
        assert!(!repository_external_addon_visible_for_repo(
            &repositories,
            0,
            "E:/mods/other/@unrelated"
        ));
    }

    #[test]
    fn external_addon_scope_keeps_unrelated_standalone_repositories_separate() {
        let repositories = vec![
            repo(
                "https://example.test/alpha",
                "S:/Swifty/foxy_test/40k",
                None,
            ),
            repo(
                "https://example.test/bravo",
                "S:/Swifty/TFR_Repository",
                None,
            ),
        ];

        assert!(!repository_external_addon_visible_for_repo(
            &repositories,
            0,
            "S:/Swifty/TFR_Repository/@ace"
        ));
    }

    #[test]
    fn external_addon_scope_allows_standalone_physical_overlap() {
        let repositories = vec![
            repo("https://example.test/alpha", "S:/Swifty/shared", None),
            repo(
                "https://example.test/bravo",
                "S:/Swifty/shared",
                Some("space"),
            ),
        ];

        assert!(repository_external_addon_visible_for_repo(
            &repositories,
            0,
            "S:/Swifty/shared/@ace"
        ));
    }

    #[test]
    fn external_addon_scope_allows_non_repository_inventory_sources() {
        let repositories = vec![repo(
            "https://example.test/alpha",
            "S:/Swifty/foxy_test/40k",
            None,
        )];

        assert!(repository_external_addon_visible_for_repo(
            &repositories,
            0,
            "D:/Steam/steamapps/workshop/content/107410/123"
        ));
    }
}
