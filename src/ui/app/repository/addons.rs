use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use log::{info, warn};

use crate::ui::app::{
    AddonInventoryEntry, AddonInventoryViewCache, Foxy, RepositoryAddonListCache,
    RepositoryExternalAddonsListCache, RepositorySettingsAddonPreloadResult,
};
use crate::ui::types::{
    Repository, RepositoryProfile, RepositoryServer, UpdateSummaryNotice,
    additional_folder_alias_key, sanitize_additional_folder_alias, selected_creator_dlc_codes,
    split_additional_launch_params,
};

impl Foxy {
    pub(crate) fn invalidate_addon_inventory_cache(&mut self) {
        self.cached_all_addons = None;
        self.addon_inventory_generation = self.addon_inventory_generation.wrapping_add(1);
        if self.addon_inventory_generation == 0 {
            self.addon_inventory_generation = 1;
        }
        self.addon_inventory_view_cache = AddonInventoryViewCache::default();
        self.repository_addons_list_cache = RepositoryAddonListCache::default();
        self.repository_optional_addons_list_cache = RepositoryAddonListCache::default();
        self.repository_external_addons_list_cache = RepositoryExternalAddonsListCache::default();
        self.invalidate_repository_addon_size_cache();
    }

    pub fn apply_profile_to_repository(repo: &mut Repository, profile: &RepositoryProfile) {
        repo.csla = profile.csla;
        repo.ef = profile.ef;
        repo.gm = profile.gm;
        repo.rf = profile.rf;
        repo.spe = profile.spe;
        repo.vn = profile.vn;
        repo.ws = profile.ws;
        repo.skip_intro = profile.skip_intro;
        repo.no_splash = profile.no_splash;
        repo.world_empty = profile.world_empty;
        repo.load_mission_to_memory = profile.load_mission_to_memory;
        repo.enable_ht = profile.enable_ht;
        repo.huge_pages = profile.huge_pages;
        repo.no_logs = profile.no_logs;
        repo.include_steam_addons = profile.include_steam_addons;
        repo.addons = profile.addons.clone();
        repo.optional_addons = profile.optional_addons.clone();
        repo.optional_addon_favorites = profile.optional_addon_favorites.clone();
        repo.optional_addon_client_side = profile.optional_addon_client_side.clone();
        repo.external_addons = profile.external_addons.clone();
        repo.external_addon_favorites = profile.external_addon_favorites.clone();
        repo.external_addon_client_side = profile.external_addon_client_side.clone();
        repo.additional_params = profile.additional_params.clone();
    }

    pub fn contains_addons_subfolder(folder: &Path) -> bool {
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(_) => return false,
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.path().is_dir() && name.eq_ignore_ascii_case("addons") {
                return true;
            }
        }
        false
    }

    pub(crate) fn normalize_origin_lookup_path(path: &str) -> String {
        let normalized = path
            .trim()
            .replace('\\', "/")
            .trim_start_matches("//?/")
            .trim_end_matches('/')
            .to_string();

        if cfg!(windows) {
            normalized.to_lowercase()
        } else {
            normalized
        }
    }

    pub(crate) fn path_is_within_root(path: &str, root: &str) -> bool {
        path == root
            || path
                .strip_prefix(root)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    /// Find the actual `steamapps` directory under a parent, handling case
    /// differences between platforms (e.g. `SteamApps` vs `steamapps`).
    fn find_steamapps_dir(parent: &Path) -> Option<PathBuf> {
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str()
                    && name.eq_ignore_ascii_case("steamapps")
                    && entry.path().is_dir()
                {
                    return Some(entry.path());
                }
            }
        }
        None
    }

    /// Build candidate paths where Steam Workshop content for Arma 3 might live.
    /// The !Workshop junction is tried first; then we derive the real path from
    /// the Arma 3 install location (assumes standard `steamapps/common/<game>`
    /// layout) and the Steam directory setting.
    fn build_workshop_candidate_paths(arma_dir: &str, steam_dir: &str) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if !arma_dir.is_empty() {
            candidates.push(Path::new(arma_dir).join("!Workshop"));

            // Arma 3 is typically at <steam>/steamapps/common/Arma 3, so
            // parent().parent() yields the steamapps directory.
            let arma_path = Path::new(arma_dir);
            if let Some(steamapps) = arma_path.parent().and_then(|p| p.parent())
                && steamapps
                    .file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("steamapps"))
            {
                candidates.push(steamapps.join("workshop").join("content").join("107410"));
            }
        }

        if !steam_dir.is_empty() {
            // Use the actual directory name on disk to handle case differences
            let steam_path = Path::new(steam_dir);
            if let Some(actual_steamapps) = Self::find_steamapps_dir(steam_path) {
                candidates.push(
                    actual_steamapps
                        .join("workshop")
                        .join("content")
                        .join("107410"),
                );
            } else {
                // Fallback to lowercase if directory listing fails
                candidates.push(
                    steam_path
                        .join("steamapps")
                        .join("workshop")
                        .join("content")
                        .join("107410"),
                );
            }
        }

        candidates
    }

    pub(crate) fn normalized_steam_workshop_root_path_for(
        arma3_directory: &str,
        steam_directory: &str,
    ) -> Option<String> {
        let candidates =
            Self::build_workshop_candidate_paths(arma3_directory.trim(), steam_directory.trim());

        for candidate in &candidates {
            if let Ok(resolved) = candidate.canonicalize() {
                return Some(Self::normalize_origin_lookup_path(
                    &resolved.to_string_lossy(),
                ));
            }
        }

        None
    }

    pub(crate) fn normalized_steam_workshop_root_path(&self) -> Option<String> {
        Self::normalized_steam_workshop_root_path_for(
            &self.settings_view_state.arma3_directory,
            &self.settings_view_state.steam_directory,
        )
    }

    pub(crate) fn is_steam_workshop_path_with_root(
        path: &str,
        workshop_root: Option<&str>,
    ) -> bool {
        let normalized_path = Self::normalize_origin_lookup_path(path);
        // Common Steam workshop location on Windows/Linux for Arma 3.
        // Use case-insensitive check to handle SteamApps vs steamapps on any platform.
        if normalized_path
            .to_ascii_lowercase()
            .contains("/steamapps/workshop/content/107410")
        {
            return true;
        }

        let Some(workshop_root) = workshop_root else {
            return false;
        };
        Self::path_is_within_root(&normalized_path, workshop_root)
    }

    fn addon_repo_origins_from(
        repositories: &[Repository],
        addon_name: &str,
        absolute_path: &str,
    ) -> Vec<String> {
        let addon_name = addon_name.trim();
        if addon_name.is_empty() {
            return Vec::new();
        }

        let absolute_path = Self::normalize_origin_lookup_path(absolute_path);

        let mut matches: Vec<String> = repositories
            .iter()
            .filter(|repo| {
                let repo_path = Self::normalize_origin_lookup_path(&repo.path);
                if repo_path.is_empty() {
                    return false;
                }

                if !Self::path_is_within_root(&absolute_path, &repo_path) {
                    return false;
                }

                repo.addons
                    .iter()
                    .chain(repo.optional_addons.iter())
                    .any(|(name, _)| name.eq_ignore_ascii_case(addon_name))
            })
            .map(|repo| repo.name.clone())
            .collect();

        matches.sort();
        matches.dedup();
        matches
    }

    pub(crate) fn addon_is_repo_defined_client_side(
        &self,
        addon_name: &str,
        absolute_path: &str,
    ) -> bool {
        let addon_name = addon_name.trim();
        if addon_name.is_empty() {
            return false;
        }

        let absolute_path = Self::normalize_origin_lookup_path(absolute_path);
        self.repository_view_state.repositories.iter().any(|repo| {
            let repo_path = Self::normalize_origin_lookup_path(&repo.path);
            if repo_path.is_empty() || !Self::path_is_within_root(&absolute_path, &repo_path) {
                return false;
            }

            repo.remote_client_side_addons
                .iter()
                .any(|name| name.eq_ignore_ascii_case(addon_name))
        })
    }

    fn read_mod_name_from_meta<P: AsRef<Path>>(mod_path: P) -> Option<String> {
        let mod_cpp_path = mod_path.as_ref().join("mod.cpp");
        let content = std::fs::read_to_string(mod_cpp_path).ok()?;
        let mut in_single = false;
        let mut in_double = false;
        let mut result = String::new();
        for c in content.chars() {
            if c == '"' && !in_single {
                in_double = !in_double;
                continue;
            } else if c == '\'' && !in_double {
                in_single = !in_single;
                continue;
            }
            if in_single || in_double {
                result.push(c);
            }
        }
        if result.trim().is_empty() {
            None
        } else {
            Some(result.trim().to_string())
        }
    }

    pub fn discover_addons_in_path<P: AsRef<Path>>(root_path: P) -> Vec<(String, String)> {
        let mut results = Vec::new();
        let path = root_path.as_ref();
        if !path.is_dir() {
            return results;
        }

        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => return results,
        };

        for entry in entries.flatten() {
            let subfolder_path = entry.path();

            if !subfolder_path.is_dir() {
                continue;
            }

            let subfolder_name = match subfolder_path.file_name() {
                Some(os_str) => os_str.to_string_lossy().to_string(),
                None => continue,
            };

            if !Foxy::contains_addons_subfolder(&subfolder_path) {
                continue;
            }

            let display_name = if subfolder_name.chars().all(|c| c.is_ascii_digit()) {
                Self::read_mod_name_from_meta(&subfolder_path).unwrap_or(subfolder_name)
            } else {
                subfolder_name
            };

            let absolute = match subfolder_path.canonicalize() {
                Ok(abs) => abs
                    .display()
                    .to_string()
                    .trim_start_matches(r"\\?\")
                    .to_string(),
                Err(_) => subfolder_path.display().to_string(),
            };

            results.push((display_name, absolute));
        }

        results
    }

    fn additional_folder_origin_map_from(
        additional_folders: &[String],
        additional_folder_aliases: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for folder in additional_folders {
            let trimmed = folder.trim();
            if trimmed.is_empty() {
                continue;
            }
            let key = additional_folder_alias_key(trimmed);
            let alias = additional_folder_aliases
                .get(&key)
                .map(String::as_str)
                .unwrap_or(trimmed);
            let label = sanitize_additional_folder_alias(alias);
            map.insert(key, label);
        }
        map
    }

    pub fn gather_all_addon_origins_from(
        repositories: &[Repository],
        additional_folders: &[String],
        additional_folder_aliases: &HashMap<String, String>,
        arma3_directory: &str,
        steam_directory: &str,
    ) -> Vec<AddonInventoryEntry> {
        let mut discovered = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let additional_origin_by_folder =
            Self::additional_folder_origin_map_from(additional_folders, additional_folder_aliases);

        for repo in repositories {
            let repo_path = repo.path.trim();
            if repo_path.is_empty() {
                continue;
            }
            let scanned = Foxy::discover_addons_in_path(repo_path);
            for (addon_name, absolute_path) in scanned {
                let repo_origins =
                    Self::addon_repo_origins_from(repositories, &addon_name, &absolute_path);
                if repo_origins.is_empty() {
                    continue;
                }

                if seen.insert(absolute_path.clone()) {
                    let origin = match repo_origins.as_slice() {
                        [single] => single.clone(),
                        many => many.join(", "),
                    };
                    let size_bytes = addon_directory_total_size(Path::new(&absolute_path)).ok();
                    discovered.push((addon_name, absolute_path, origin, size_bytes));
                }
            }
        }

        let workshop_root =
            Self::normalized_steam_workshop_root_path_for(arma3_directory, steam_directory);
        for folder in additional_folders {
            let folder = folder.trim();
            if folder.is_empty() {
                continue;
            }
            let folder_key = Self::normalize_origin_lookup_path(folder);
            let additional_origin = additional_origin_by_folder
                .get(&folder_key)
                .cloned()
                .unwrap_or_else(|| "Additional folders".to_string());
            let scanned = Foxy::discover_addons_in_path(folder);
            for (addon_name, absolute_path) in scanned {
                if seen.insert(absolute_path.clone()) {
                    let origin = if Self::is_steam_workshop_path_with_root(
                        &absolute_path,
                        workshop_root.as_deref(),
                    ) {
                        "Steam Workshop".to_string()
                    } else {
                        additional_origin.clone()
                    };
                    let size_bytes = addon_directory_total_size(Path::new(&absolute_path)).ok();
                    discovered.push((addon_name, absolute_path, origin, size_bytes));
                }
            }
        }

        let workshop_candidates =
            Self::build_workshop_candidate_paths(arma3_directory.trim(), steam_directory.trim());

        let mut scanned_roots: HashSet<PathBuf> = HashSet::new();
        for workshop_folder in &workshop_candidates {
            let canonical = workshop_folder
                .canonicalize()
                .unwrap_or_else(|_| workshop_folder.clone());
            if !scanned_roots.insert(canonical) {
                continue;
            }
            let scanned = Foxy::discover_addons_in_path(workshop_folder);
            for (addon_name, absolute_path) in scanned {
                if seen.insert(absolute_path.clone()) {
                    let size_bytes = addon_directory_total_size(Path::new(&absolute_path)).ok();
                    discovered.push((
                        addon_name,
                        absolute_path,
                        "Steam Workshop".to_string(),
                        size_bytes,
                    ));
                }
            }
        }

        discovered.sort_by(|a, b| {
            let (a_name, a_path, a_origin, _) = a;
            let (b_name, b_path, b_origin, _) = b;
            a_name
                .cmp(b_name)
                .then(a_origin.cmp(b_origin))
                .then(a_path.cmp(b_path))
        });

        discovered
    }

    pub fn gather_all_addon_origins(&self) -> Vec<AddonInventoryEntry> {
        Self::gather_all_addon_origins_from(
            &self.repository_view_state.repositories,
            &self.settings_view_state.additional_folders,
            &self.settings_view_state.additional_folder_aliases,
            &self.settings_view_state.arma3_directory,
            &self.settings_view_state.steam_directory,
        )
    }

    pub fn get_or_generate_all_addons(&mut self) -> &Vec<AddonInventoryEntry> {
        if self.cached_all_addons.is_none() {
            let new_data = self.gather_all_addon_origins();
            self.cached_all_addons = Some(new_data);
        }
        self.cached_all_addons.as_ref().unwrap()
    }

    pub(crate) fn ensure_repository_settings_addon_caches(&mut self, repo_index: usize) {
        if self
            .repository_view_state
            .repositories
            .get(repo_index)
            .is_none()
        {
            return;
        }

        self.ensure_repository_addon_list_cache_cached(
            repo_index,
            crate::ui::app::RepositoryAddonListKind::Addons,
        );
        self.ensure_repository_addon_list_cache_cached(
            repo_index,
            crate::ui::app::RepositoryAddonListKind::OptionalAddons,
        );
        self.ensure_repository_external_addons_base_cache_cached(repo_index);
    }

    pub(crate) fn preload_repository_settings_addon_caches(&mut self, repo_index: usize) {
        if self
            .repository_view_state
            .repositories
            .get(repo_index)
            .is_none()
        {
            return;
        }

        if self.cached_all_addons.is_some() {
            self.ensure_repository_settings_addon_caches(repo_index);
            return;
        }

        if self.repository_settings_addon_preload_worker.is_some() {
            return;
        }

        let repositories = self.repository_view_state.repositories.clone();
        let additional_folders = self.settings_view_state.additional_folders.clone();
        let additional_folder_aliases = self.settings_view_state.additional_folder_aliases.clone();
        let arma3_directory = self.settings_view_state.arma3_directory.clone();
        let steam_directory = self.settings_view_state.steam_directory.clone();
        let inventory_generation = self.addon_inventory_generation;
        let result_tx = self.repository_settings_addon_preload_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();

        self.repository_settings_addon_preload_worker = Some(std::thread::spawn(move || {
            let started_at = std::time::Instant::now();
            let addons = Foxy::gather_all_addon_origins_from(
                &repositories,
                &additional_folders,
                &additional_folder_aliases,
                &arma3_directory,
                &steam_directory,
            );
            info!(
                "Preloaded addon inventory for repository settings in {:.2?} ({} addons)",
                started_at.elapsed(),
                addons.len()
            );
            if result_tx
                .send(RepositorySettingsAddonPreloadResult {
                    repo_index,
                    inventory_generation,
                    addons,
                })
                .is_err()
            {
                warn!("Failed to send repository settings addon preload result: UI channel closed");
            }
            Self::request_background_repaint(repaint_ctx.as_ref());
        }));
    }

    pub(crate) fn poll_repository_settings_addon_preload_results(&mut self) {
        loop {
            match self.repository_settings_addon_preload_rx.try_recv() {
                Ok(result) => {
                    if let Some(worker) = self.repository_settings_addon_preload_worker.take()
                        && worker.join().is_err()
                    {
                        warn!("Repository settings addon preload worker panicked");
                    }
                    if result.inventory_generation != self.addon_inventory_generation {
                        info!(
                            "Discarding stale repository settings addon preload result for repo index {}",
                            result.repo_index
                        );
                        continue;
                    }

                    if self.cached_all_addons.is_none() {
                        self.cached_all_addons = Some(result.addons);
                    }

                    let repo_index = self
                        .selected_repository_for_settings
                        .unwrap_or(result.repo_index);
                    self.ensure_repository_settings_addon_caches(repo_index);
                    self.needs_repaint = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    warn!("Repository settings addon preload result channel disconnected");
                    break;
                }
            }
        }
    }

    pub fn create_launch_command(
        &self,
        repo: &Repository,
        server: Option<&RepositoryServer>,
    ) -> Option<std::process::Command> {
        let arma3_directory = self.settings_view_state.arma3_directory.trim();

        #[cfg(target_os = "windows")]
        if arma3_directory.is_empty() {
            log::warn!(
                "Cannot create launch command: Arma 3 directory is not configured (raw value {:?})",
                self.settings_view_state.arma3_directory
            );
            return None;
        }

        let arma3_dir_path = if arma3_directory.is_empty() {
            std::path::Path::new(".")
        } else {
            std::path::Path::new(arma3_directory)
        };
        #[cfg(target_os = "windows")]
        if !arma3_dir_path.exists() {
            log::warn!(
                "Cannot create launch command: Arma 3 directory does not exist: {}",
                arma3_directory
            );
            return None;
        }

        #[cfg(target_os = "windows")]
        if !crate::core::steam::is_valid_arma3_dir(arma3_dir_path) {
            log::warn!(
                "Cannot create launch command: Arma 3 directory is not valid: {}",
                arma3_directory
            );
            return None;
        }

        let mut args = Vec::new();

        crate::ui::types::push_arma3_profile_launch_args(
            &self.settings_view_state,
            repo,
            &mut args,
        );

        if repo.skip_intro {
            args.push("-skipIntro".to_string());
        }
        if repo.no_splash {
            args.push("-noSplash".to_string());
        }
        if repo.world_empty {
            args.push("-world=empty".to_string());
        }
        if repo.load_mission_to_memory {
            args.push("-loadMissionToMemory".to_string());
        }
        if repo.enable_ht {
            args.push("-enableHT".to_string());
        }
        if repo.huge_pages {
            args.push("-hugePages".to_string());
        }
        if repo.no_logs {
            args.push("-noLogs".to_string());
        }

        if !repo.additional_params.is_empty() {
            args.extend(split_additional_launch_params(&repo.additional_params));
        }

        let resolved_addons = resolve_launch_mod_paths(repo, arma3_directory);
        if !resolved_addons.is_empty() {
            let mod_param = format!("-mod={}", resolved_addons.join(";"));
            args.push(mod_param);
        }

        if let Some(server) = server {
            args.push(format!("-connect={}", server.address));
            args.push(format!("-port={}", server.port));
            if !server.password.is_empty() {
                args.push(format!("-password={}", server.password));
            }
        }

        let steam_directory = self.settings_view_state.steam_directory.trim();
        let Some(launch) =
            crate::core::steam::arma3_launch_command(arma3_dir_path, steam_directory)
        else {
            log::warn!("Cannot create launch command: Steam launch command is unavailable");
            return None;
        };
        let mut command = std::process::Command::new(launch.program);
        command.args(launch.args);
        command.args(&args);
        if !arma3_directory.is_empty() && arma3_dir_path.exists() {
            command.current_dir(arma3_directory);
        }

        Some(command)
    }

    fn normalized_repo_url_for_index(&self, repo_index: usize) -> Option<String> {
        self.repository_view_state
            .repositories
            .get(repo_index)
            .map(|repo| Self::normalize_repo_url(&repo.address))
    }

    pub(crate) fn open_update_summary_for_repo(&mut self, repo_index: usize) -> bool {
        let Some(normalized_url) = self.normalized_repo_url_for_index(repo_index) else {
            return false;
        };
        let Some(notice) = self
            .settings_view_state
            .update_summary_notices
            .iter()
            .find(|notice| notice.repository_url == normalized_url)
            .cloned()
        else {
            return false;
        };

        // Restore the per-mod snapshot captured when this repo finished
        // downloading so the modal shows this repo's list, not whichever
        // repo happened to be the last one cached in `mod_diff_cache`.
        if notice.mods.is_empty() {
            self.clear_mod_diff_cache();
            self.update_ready_repo = None;
        } else {
            self.set_mod_diff_cache(notice.mods);
            self.update_ready_repo = Some(repo_index);
        }
        self.download_summary = Some(notice.summary);
        self.download_finished = true;
        self.download_finished_repo = Some(repo_index);
        self.download_progress = Some(("Finished".to_string(), 1.0));
        self.direct_download_update_view = false;
        self.update_modal_open = true;
        self.needs_repaint = true;
        true
    }

    pub(crate) fn acknowledge_update_summary_for_repo(&mut self, repo_index: usize) {
        let Some(normalized_url) = self.normalized_repo_url_for_index(repo_index) else {
            return;
        };
        let repo_path = self
            .repository_view_state
            .repositories
            .get(repo_index)
            .map(|repo| repo.path.clone())
            .unwrap_or_default();
        let previous_len = self.settings_view_state.update_summary_notices.len();
        self.settings_view_state
            .update_summary_notices
            .retain(|notice| notice.repository_url != normalized_url);
        if self.settings_view_state.update_summary_notices.len() != previous_len {
            self.save_settings();
        }

        let instance_key = Self::repo_instance_key(&normalized_url, &repo_path);
        let has_cached_pending_updates = self
            .pending_update_cache
            .get(&instance_key)
            .is_some_and(|mods| mods.iter().any(|m| m.needs_update));
        if has_cached_pending_updates {
            return;
        }

        if self.update_ready_repo == Some(repo_index) {
            self.update_ready_repo = None;
            self.clear_mod_diff_cache();
        }
        if self.download_finished_repo == Some(repo_index) {
            self.download_finished = false;
            self.download_finished_repo = None;
            self.download_progress = None;
            self.download_summary = None;
        }
        self.clear_pending_update_cache_for_url(&normalized_url, &repo_path);
        self.set_repo_state_for_address(
            &normalized_url,
            &repo_path,
            crate::ui::types::RepoState::Synced,
        );
        self.clear_cached_pending_update(repo_index);
    }

    pub(in crate::ui::app) fn register_update_summary_notice_for_repo(
        &mut self,
        repo_index: usize,
    ) {
        let Some(normalized_url) = self.normalized_repo_url_for_index(repo_index) else {
            return;
        };
        let Some(summary) = self.download_summary.clone() else {
            return;
        };

        // Do not persist a notice when the download completed with nothing
        // actually updated (e.g. after a DB wipe + recheck cycle).
        if !summary.has_meaningful_content() {
            info!(
                "Skipping update summary notice for {} - no mods, files, or parts were updated",
                normalized_url
            );
            return;
        }

        // Snapshot the per-mod diff so the modal can reconstruct this repo's
        // list later, even after further updates overwrite `mod_diff_cache`
        // during a bulk space update.
        let mods_snapshot: Vec<_> = self
            .mod_diff_cache
            .iter()
            .filter(|m| {
                m.needs_update
                    || self
                        .mod_download_progress
                        .get(&m.name)
                        .is_some_and(|(pct, _, _, _, _)| *pct >= 1.0)
            })
            .cloned()
            .collect();

        if let Some(existing) = self
            .settings_view_state
            .update_summary_notices
            .iter_mut()
            .find(|notice| notice.repository_url == normalized_url)
        {
            existing.pending_ack_count = existing.pending_ack_count.saturating_add(1).max(1);
            existing.summary = summary;
            existing.mods = mods_snapshot;
        } else {
            self.settings_view_state
                .update_summary_notices
                .push(UpdateSummaryNotice {
                    repository_url: normalized_url,
                    pending_ack_count: 1,
                    summary,
                    mods: mods_snapshot,
                });
        }
        self.save_settings();
    }

    pub(in crate::ui::app) fn set_repo_app_update_url_for_address(
        &mut self,
        repository_url: &str,
        app_update_url: Option<&str>,
    ) -> bool {
        let normalized_url = Self::normalize_repo_url(repository_url);
        let sanitized_url = app_update_url
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_default();
        let mut changed = false;

        for repo in &mut self.repository_view_state.repositories {
            if Self::normalize_repo_url(&repo.address) != normalized_url {
                continue;
            }
            if repo.app_update_url != sanitized_url {
                repo.app_update_url = sanitized_url.clone();
                changed = true;
            }
        }

        if changed {
            self.save_repositories();
        }

        changed
    }

    pub(in crate::ui) fn maybe_auto_fill_app_update_url_from_metadata(&mut self) -> bool {
        if self.settings_view_state.app_update_url_user_override
            || !self.settings_view_state.app_update_url.trim().is_empty()
        {
            return false;
        }

        let space_url = self
            .repository_spaces
            .iter()
            .map(|space| space.app_update_url.trim())
            .find(|candidate| !candidate.is_empty())
            .map(str::to_string);
        let (detected_url, source_label) = if let Some(url) = space_url {
            (Some(url), "repository-space metadata")
        } else {
            (
                self.repository_view_state
                    .repositories
                    .iter()
                    .map(|repo| repo.app_update_url.trim())
                    .find(|candidate| !candidate.is_empty())
                    .map(str::to_string),
                "repository metadata",
            )
        };

        let Some(url) = detected_url else {
            return false;
        };

        self.settings_view_state.app_update_url = url.clone();
        self.settings_view_state.app_update_url_user_override = false;
        if !self.settings_view_state.app_update_mode_user_override {
            self.settings_view_state.app_update_mode = crate::ui::types::AppUpdateMode::Server;
        }
        self.save_settings();
        info!("Auto-filled app update URL from {}: {}", source_label, url);
        true
    }
}

fn resolve_launch_mod_paths(repo: &Repository, arma3_directory: &str) -> Vec<String> {
    let creator_dlc_codes = selected_creator_dlc_codes(repo);
    let enabled_addons: Vec<String> = repo
        .addons
        .iter()
        .map(|(addon, enabled)| (addon, *enabled))
        .chain(
            repo.optional_addons
                .iter()
                .map(|(addon, enabled)| (addon, *enabled)),
        )
        .filter_map(|(addon, enabled)| if enabled { Some(addon.clone()) } else { None })
        .collect();
    let enabled_external_addons = repo
        .external_addons
        .iter()
        .filter(|(_, enabled, _)| *enabled)
        .collect::<Vec<_>>();

    if creator_dlc_codes.is_empty()
        && enabled_addons.is_empty()
        && enabled_external_addons.is_empty()
    {
        return Vec::new();
    }

    let mut resolved_addons: Vec<String> = Vec::new();
    let repo_path = repo.path.trim();

    for creator_dlc_code in creator_dlc_codes {
        resolved_addons.push(creator_dlc_code.to_string());
    }

    for addon in &enabled_addons {
        let addon_path = std::path::Path::new(repo_path).join(addon);
        if addon_path.exists() {
            resolved_addons.push(addon_path.to_string_lossy().to_string());
        } else {
            let arma3_addon_path = std::path::Path::new(arma3_directory).join(addon);
            if arma3_addon_path.exists() {
                resolved_addons.push(arma3_addon_path.to_string_lossy().to_string());
            } else {
                log::error!(
                    "Addon not found in repository or Arma 3 directory: {}",
                    addon
                );
            }
        }
    }

    for (addon, _, path) in enabled_external_addons {
        if let Some(external_path) = resolve_external_launch_addon_path(addon, path) {
            resolved_addons.push(external_path.to_string_lossy().to_string());
        } else {
            log::error!(
                "External addon not found at configured path: addon={} path={}",
                addon,
                path
            );
        }
    }

    resolved_addons
}

fn resolve_external_launch_addon_path(addon: &str, path: &str) -> Option<std::path::PathBuf> {
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return None;
    }

    let base_path = std::path::Path::new(trimmed_path);
    let nested_path = base_path.join(addon.trim());
    if nested_path.is_dir() {
        return Some(nested_path);
    }

    if base_path.is_dir() {
        if workshop_id_from_launch_path(trimmed_path).is_some() {
            return Some(base_path.to_path_buf());
        }
        let base_name = base_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if base_name.trim_start().starts_with('@') {
            return Some(base_path.to_path_buf());
        }
        let base_name = normalize_launch_addon_name(base_name);
        let addon_key = normalize_launch_addon_name(addon);
        if !base_name.is_empty() && base_name == addon_key {
            return Some(base_path.to_path_buf());
        }
    }

    None
}

fn workshop_id_from_launch_path(path: &str) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for window in parts.windows(4) {
        if window[0].eq_ignore_ascii_case("workshop")
            && window[1].eq_ignore_ascii_case("content")
            && window[2] == "107410"
            && window[3].chars().all(|ch| ch.is_ascii_digit())
        {
            return Some(window[3].to_string());
        }
    }

    for pair in parts.windows(2) {
        if pair[0] == "107410" && pair[1].chars().all(|ch| ch.is_ascii_digit()) {
            return Some(pair[1].to_string());
        }
    }

    None
}

fn normalize_launch_addon_name(name: &str) -> String {
    name.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .chars()
        .filter_map(|ch| {
            if ch.is_whitespace() || matches!(ch, '-' | '_' | '.') {
                Some('_')
            } else if ch == '@' {
                None
            } else if ch.is_ascii() {
                Some(ch.to_ascii_lowercase())
            } else {
                ch.to_lowercase().next()
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn addon_directory_total_size(path: &Path) -> std::io::Result<u64> {
    let metadata = std::fs::metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_launch_addon_resolves_repo_root_plus_addon_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_dir).expect("addon dir");

        let resolved =
            resolve_external_launch_addon_path("@burnem_redux", &dir.path().to_string_lossy())
                .expect("external addon path should resolve");

        assert_eq!(resolved, addon_dir);
    }

    #[test]
    fn external_launch_addon_rejects_repo_root_when_addon_folder_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");

        let resolved =
            resolve_external_launch_addon_path("@burnem_redux", &dir.path().to_string_lossy());

        assert!(resolved.is_none());
    }

    #[test]
    fn external_launch_addon_accepts_direct_at_folder_with_display_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_dir).expect("addon dir");

        let resolved =
            resolve_external_launch_addon_path("Burn Em Redux", &addon_dir.to_string_lossy())
                .expect("direct @addon path should resolve");

        assert_eq!(resolved, addon_dir);
    }

    #[test]
    fn external_launch_addon_accepts_direct_workshop_id_folder_with_display_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir
            .path()
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join("107410")
            .join("463939057");
        std::fs::create_dir_all(&addon_dir).expect("workshop addon dir");

        let resolved = resolve_external_launch_addon_path("ACE", &addon_dir.to_string_lossy())
            .expect("direct workshop ID folder path should resolve");

        assert_eq!(resolved, addon_dir);
    }

    #[test]
    fn launch_mod_paths_include_external_addons_without_repo_addons() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@client_mod");
        std::fs::create_dir(&addon_dir).expect("addon dir");
        let repo = Repository {
            external_addons: vec![(
                "@client_mod".to_string(),
                true,
                addon_dir.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };

        let resolved = resolve_launch_mod_paths(&repo, "");

        assert_eq!(resolved, vec![addon_dir.to_string_lossy().to_string()]);
    }
}
