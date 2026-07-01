use std::time::Duration;

use std::sync::LazyLock;

use log::{debug, info, warn};
use reqwest::blocking::Client;
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::ui::app::{
    FetchedRepositorySpace, Foxy, PendingRepositoryDuplicateAddAction,
    PendingRepositoryDuplicateAddState, RepoMetadataFetchResult, RepoMetadataPayload,
    RepositorySpaceImportContinuation, RepositorySpaceImportResult, RepositorySpaceManifest,
};
use crate::ui::i18n::tr;
use crate::ui::types::{
    Repository, RepositoryServer, RepositorySpace, RepositorySpaceEntry,
    apply_repo_client_parameters, apply_repo_dlc_content_from_repo_json, merge_remote_addon_list,
};

/// Shared HTTP client for repository-metadata fetches. A bounded timeout keeps
/// a slow or unreachable server from blocking the worker thread indefinitely
/// (the previous default-`reqwest::blocking::get` had no timeout, which is what
/// let a dead server hang for ~20s before erroring).
static REPO_METADATA_HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("failed to build repository metadata HTTP client: check TLS/system configuration")
});

const REPO_METADATA_MAX_ATTEMPTS: u32 = 3;
const REPO_METADATA_RETRY_BACKOFF: Duration = Duration::from_millis(750);

/// Fetch and parse a JSON document, retrying a few times on transient failure.
/// Runs on a background thread, so the blocking sleep between attempts never
/// touches the UI thread.
fn fetch_json_with_retry(url: &str) -> Result<Value, String> {
    let mut last_error = String::new();
    for attempt in 1..=REPO_METADATA_MAX_ATTEMPTS {
        match REPO_METADATA_HTTP_CLIENT
            .get(url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
        {
            Ok(response) => match response.json::<Value>() {
                Ok(json) => return Ok(json),
                Err(err) => last_error = format!("parse error: {err}"),
            },
            Err(err) => last_error = format!("request error: {err}"),
        }

        if attempt < REPO_METADATA_MAX_ATTEMPTS {
            warn!(
                "Repository metadata fetch attempt {}/{} failed for {}: {}; retrying",
                attempt, REPO_METADATA_MAX_ATTEMPTS, url, last_error
            );
            std::thread::sleep(REPO_METADATA_RETRY_BACKOFF);
        }
    }
    Err(last_error)
}

fn parse_remote_addon_list(
    mods: Option<&[Value]>,
    default_enabled: bool,
) -> (Vec<(String, bool)>, Vec<String>) {
    let mut addons = Vec::new();
    let mut client_side_addons = Vec::new();

    let Some(mods) = mods else {
        return (addons, client_side_addons);
    };

    for mod_info in mods {
        let Some(mod_name) = mod_info["modName"].as_str() else {
            continue;
        };
        let enabled = mod_info["enabled"].as_bool().unwrap_or(default_enabled);
        addons.push((mod_name.to_string(), enabled));
        if mod_info["clientSide"].as_bool().unwrap_or(false) {
            client_side_addons.push(mod_name.to_string());
        }
    }

    (addons, client_side_addons)
}

impl Foxy {
    pub fn normalize_repository_address_input(address: &str) -> String {
        let mut normalized = address.trim().replace('\\', "/");
        while normalized.ends_with('/') {
            normalized.pop();
        }

        let lower = normalized.to_ascii_lowercase();
        if lower.ends_with("/repo.json") {
            let keep_len = normalized.len() - "/repo.json".len();
            normalized.truncate(keep_len);
        }
        let lower = normalized.to_ascii_lowercase();
        if lower.ends_with("/repository_space.json") {
            let keep_len = normalized.len() - "/repository_space.json".len();
            normalized.truncate(keep_len);
        }

        normalized
    }

    pub(in crate::ui::app) fn repository_space_manifest_candidates(input: &str) -> Vec<String> {
        let normalized = input.trim().replace('\\', "/");
        if normalized.is_empty() {
            return Vec::new();
        }

        let lower = normalized.to_ascii_lowercase();
        if lower.ends_with("/repository_space.json") {
            return vec![normalized];
        }
        if lower.ends_with("/repo.json") {
            let root = normalized
                .trim_end_matches('/')
                .trim_end_matches("repo.json")
                .trim_end_matches('/');
            return vec![format!("{root}/repository_space.json")];
        }
        if lower.ends_with(".json") {
            return vec![normalized];
        }

        let root = normalized.trim_end_matches('/');
        vec![format!("{root}/repository_space.json")]
    }

    pub(in crate::ui::app) fn repository_space_base_url(manifest_url: &str) -> String {
        let normalized = manifest_url.trim().replace('\\', "/");
        let lower = normalized.to_ascii_lowercase();
        if lower.ends_with("/repository_space.json") {
            let keep_len = normalized.len() - "/repository_space.json".len();
            return normalized[..keep_len].to_string();
        }
        if let Some((base, _)) = normalized.rsplit_once('/') {
            return base.to_string();
        }
        normalized
    }

    pub(in crate::ui::app) fn repository_space_id_from_source(source_address: &str) -> String {
        let mut hasher = Sha1::new();
        hasher.update(source_address.to_ascii_lowercase().as_bytes());
        format!("space-{}", hex::encode(hasher.finalize()))
    }

    pub(in crate::ui::app) fn default_repository_name_from_address(address: &str) -> String {
        let trimmed = address.trim_end_matches('/');
        let candidate = trimmed.rsplit('/').next().unwrap_or_default().trim();
        if candidate.is_empty() {
            tr("New Repository")
        } else {
            candidate.to_string()
        }
    }

    /// Normalize a local download folder for identity comparison. Two
    /// repositories share core database / pending-update state only when they
    /// resolve to the same folder, so comparison must ignore separator style,
    /// trailing slashes and (on Windows) case. A blank folder means the download
    /// location has not been chosen yet and can never collide.
    fn normalize_repo_path_identity(path: &str) -> String {
        let mut normalized = path.trim().replace('\\', "/");
        while normalized.ends_with('/') {
            normalized.pop();
        }
        if cfg!(windows) {
            normalized = normalized.to_ascii_lowercase();
        }
        normalized
    }

    /// Collect existing repositories that would share core database and
    /// pending-update state with a repository about to be added.
    ///
    /// Identity is bound to the repository space and the local download folder -
    /// never to the remote URL alone. The same URL added under a different space
    /// or to a different folder is an independent install and does not collide.
    /// The URL only acts as a tiebreaker between repositories that deliberately
    /// share one space folder (where shared addons are deduplicated and each repo
    /// keeps its own unique ones), so two *different* repositories in the same
    /// folder are not reported as duplicates of each other.
    fn collect_duplicate_repository_bindings(
        &self,
        normalized_address: &str,
        space_id: Option<&str>,
        folder: &str,
    ) -> Vec<(String, Option<String>)> {
        let folder_key = Self::normalize_repo_path_identity(folder);
        if folder_key.is_empty() {
            return Vec::new();
        }
        self.repository_view_state
            .repositories
            .iter()
            .filter(|repo| {
                Self::normalize_repo_url(&repo.address) == normalized_address
                    && repo.repository_space_id.as_deref() == space_id
                    && Self::normalize_repo_path_identity(&repo.path) == folder_key
            })
            .map(|repo| {
                let space_name = repo
                    .repository_space_id
                    .as_deref()
                    .and_then(|space_id| self.repository_space_name_by_id(space_id))
                    .map(str::to_string);
                (repo.name.clone(), space_name)
            })
            .collect()
    }

    fn queue_duplicate_repository_add_confirmation(
        &mut self,
        normalized_address: &str,
        action: PendingRepositoryDuplicateAddAction,
        adding_to_space_id: Option<&str>,
        folder: &str,
    ) {
        let existing_repos = self.collect_duplicate_repository_bindings(
            normalized_address,
            adding_to_space_id,
            folder,
        );
        let adding_to_space_name = adding_to_space_id
            .and_then(|space_id| self.repository_space_name_by_id(space_id))
            .map(str::to_string);
        self.pending_repository_duplicate_add = Some(PendingRepositoryDuplicateAddState {
            normalized_url: normalized_address.to_string(),
            action,
            existing_repos,
            adding_to_space_name,
        });
    }

    fn add_repository_from_address_input_internal(
        &mut self,
        address_input: &str,
        name_input: &str,
        path_input: &str,
        ctx: &egui::Context,
        allow_duplicate: bool,
    ) -> Result<usize, String> {
        let address = Self::normalize_repository_address_input(address_input);
        if address.is_empty() {
            return Err("Address is required".to_string());
        }

        let name_input = name_input.trim();
        let path_input = path_input.trim();

        let normalized_with_trailing = Self::normalize_repo_url(&address);
        if !allow_duplicate {
            // A repository added from the address input shares a (space, folder)
            // install only when the user picks a download folder here; otherwise
            // it has no folder yet and is never flagged as a duplicate.
            let duplicates = self.collect_duplicate_repository_bindings(
                &normalized_with_trailing,
                None,
                path_input,
            );
            if !duplicates.is_empty() {
                self.queue_duplicate_repository_add_confirmation(
                    &normalized_with_trailing,
                    PendingRepositoryDuplicateAddAction::FromAddressInput {
                        address_input: address_input.to_string(),
                        name: name_input.to_string(),
                        path: path_input.to_string(),
                    },
                    None,
                    path_input,
                );
                return Err(self.t("This repository is already added in the same folder"));
            }
        }

        let name = if name_input.is_empty() {
            Self::default_repository_name_from_address(&address)
        } else {
            name_input.to_string()
        };
        let mut repo = Repository {
            name,
            address,
            path: path_input.to_string(),
            ..Repository::default()
        };

        repo.repository_space_id = None;
        repo.repository_space_entry_address = None;

        self.repository_view_state.repositories.push(repo);
        let repo_idx = self.repository_view_state.repositories.len() - 1;
        self.repository_view_state.selected_repository = Some(repo_idx);
        self.selected_repository_space_id = None;
        self.clear_completed_repository_check_banner_for_repo_change(Some(repo_idx));
        self.save_repositories();
        self.update_repository_from_url(repo_idx, ctx);
        let added_message = self.t_fmt(
            "Repository added: {name}",
            &[(
                "name",
                self.repository_view_state.repositories[repo_idx]
                    .name
                    .clone(),
            )],
        );
        self.show_success_toast(added_message);
        Ok(repo_idx)
    }

    pub fn add_repository_from_address_input(
        &mut self,
        address_input: &str,
        name_input: &str,
        path_input: &str,
        ctx: &egui::Context,
    ) -> Result<usize, String> {
        self.add_repository_from_address_input_internal(
            address_input,
            name_input,
            path_input,
            ctx,
            false,
        )
    }

    /// Dispatch a background fetch of a repository-space manifest. The network
    /// I/O runs on a worker thread (with a bounded-timeout client); the parsed
    /// result is applied on the UI thread by
    /// [`Self::poll_repository_space_import_results`], so a slow or unreachable
    /// server can no longer freeze the app. The previous synchronous version
    /// used a timeout-less `reqwest::blocking::get` on the UI thread.
    pub(crate) fn dispatch_repository_space_import(
        &mut self,
        input: &str,
        continuation: RepositorySpaceImportContinuation,
    ) {
        if self.repository_space_import_in_flight {
            debug!("Repository space import already in flight; ignoring duplicate request");
            return;
        }

        let input = input.trim().to_string();
        let tx = self.repository_space_import_result_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        self.repository_space_import_in_flight = true;
        self.needs_repaint = true;
        std::thread::spawn(move || {
            let outcome = Self::fetch_repository_space_manifest(&input);
            if tx
                .send(RepositorySpaceImportResult {
                    continuation,
                    outcome,
                })
                .is_ok()
            {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });
    }

    /// Worker-thread fetch + parse of a repository-space manifest. Tries each
    /// candidate URL in turn and returns the first valid manifest, `Ok(None)`
    /// if none of the candidates yields one, or `Err` on a parse failure.
    fn fetch_repository_space_manifest(
        input: &str,
    ) -> Result<Option<FetchedRepositorySpace>, String> {
        let candidates = Self::repository_space_manifest_candidates(input);
        let mut parse_error: Option<String> = None;

        for manifest_url in candidates {
            let response = match REPO_METADATA_HTTP_CLIENT.get(&manifest_url).send() {
                Ok(response) => response,
                Err(err) => {
                    debug!(
                        "Repository space manifest fetch failed for {}: {}",
                        manifest_url, err
                    );
                    continue;
                }
            };

            if !response.status().is_success() {
                debug!(
                    "Repository space manifest not available at {} (status {})",
                    manifest_url,
                    response.status()
                );
                continue;
            }

            match response.json::<RepositorySpaceManifest>() {
                Ok(manifest) => {
                    if manifest.entries.is_empty() {
                        parse_error = Some(format!(
                            "Repository space manifest has no entries: {}",
                            manifest_url
                        ));
                        continue;
                    }

                    let source_address = manifest_url.trim().to_string();
                    let source_base_url = Self::repository_space_base_url(&source_address);
                    let space_id = Self::repository_space_id_from_source(&source_address);
                    let app_update_url = manifest.app_update_url.trim().to_string();
                    let entries: Vec<RepositorySpaceEntry> = manifest
                        .entries
                        .into_iter()
                        .filter_map(|entry| {
                            let normalized_address =
                                Self::normalize_repository_address_input(&entry.address);
                            if normalized_address.is_empty() {
                                return None;
                            }
                            Some(RepositorySpaceEntry {
                                name: if entry.name.trim().is_empty() {
                                    Self::default_repository_name_from_address(&normalized_address)
                                } else {
                                    entry.name.trim().to_string()
                                },
                                address: normalized_address,
                                required: entry.required,
                            })
                        })
                        .collect();

                    if entries.is_empty() {
                        return Err("Repository space manifest entries are empty".to_string());
                    }

                    return Ok(Some(FetchedRepositorySpace {
                        source_address,
                        source_base_url,
                        space_id,
                        manifest_name: manifest.name.trim().to_string(),
                        icon_image_path: manifest.icon,
                        icon_image_checksum: manifest.icon_checksum,
                        repo_image_path: manifest.image,
                        repo_image_checksum: manifest.image_checksum,
                        app_update_url,
                        entries,
                    }));
                }
                Err(err) => {
                    parse_error = Some(format!(
                        "Failed to parse repository space manifest from {}: {}",
                        manifest_url, err
                    ));
                }
            }
        }

        if let Some(err) = parse_error {
            Err(err)
        } else {
            Ok(None)
        }
    }

    /// Merge a fetched repository-space manifest into app state on the UI
    /// thread, preserving existing local overrides, and return its space id.
    fn apply_fetched_repository_space(
        &mut self,
        fetched: FetchedRepositorySpace,
        ctx: &egui::Context,
    ) -> String {
        let FetchedRepositorySpace {
            source_address,
            source_base_url,
            space_id,
            manifest_name,
            icon_image_path,
            icon_image_checksum,
            repo_image_path,
            repo_image_checksum,
            app_update_url,
            entries,
        } = fetched;

        let space_name = if manifest_name.is_empty() {
            Self::default_repository_name_from_address(&source_base_url)
        } else {
            manifest_name.clone()
        };

        let existing_idx = self.repository_spaces.iter().position(|s| s.id == space_id);
        let existing_space = existing_idx.and_then(|idx| self.repository_spaces.get(idx));
        let shared_path = existing_space
            .map(|s| s.shared_path.clone())
            .unwrap_or_default();
        let local_name_override = existing_space.and_then(|s| {
            let override_name = s.local_name_override.as_deref()?.trim();
            if override_name.is_empty() {
                return None;
            }

            let existing_name = s.name.trim();
            let source_default = Self::default_repository_name_from_address(&s.source_base_url);
            let looks_like_auto_name =
                override_name == existing_name || override_name == source_default.trim();
            let should_clear_for_remote_name = !manifest_name.is_empty() && looks_like_auto_name;

            if should_clear_for_remote_name {
                None
            } else {
                Some(override_name.to_string())
            }
        });
        let collapsed = existing_space.map(|s| s.collapsed).unwrap_or(false);

        let space = RepositorySpace {
            id: space_id.clone(),
            name: space_name,
            local_name_override,
            collapsed,
            source_address,
            source_base_url,
            shared_path,
            icon_image_path,
            icon_image_checksum,
            repo_image_path,
            repo_image_checksum,
            app_update_url,
            entries,
        };

        if let Some(idx) = existing_idx {
            self.repository_spaces[idx] = space.clone();
        } else {
            self.repository_spaces.push(space.clone());
        }

        self.save_repository_spaces();
        self.reconcile_repository_space_paths();
        self.maybe_auto_fill_app_update_url_from_metadata();

        if !space.icon_image_checksum.is_empty() {
            self.download_and_load_image(
                ctx,
                &space.source_base_url,
                &space.icon_image_path,
                &space.icon_image_checksum,
                true,
            );
        }
        if !space.repo_image_checksum.is_empty() {
            self.download_and_load_image(
                ctx,
                &space.source_base_url,
                &space.repo_image_path,
                &space.repo_image_checksum,
                false,
            );
        }

        info!("Imported repository space {}", space.name);
        self.show_success_toast(self.t_fmt(
            "Repository space imported: {name}",
            &[("name", space.name.clone())],
        ));
        space_id
    }

    /// Drain completed background repository-space manifest fetches and apply
    /// them on the UI thread, then run each fetch's queued continuation.
    pub(in crate::ui::app) fn poll_repository_space_import_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.repository_space_import_result_rx.try_recv() {
            self.repository_space_import_in_flight = false;
            let RepositorySpaceImportResult {
                continuation,
                outcome,
            } = result;
            match continuation {
                RepositorySpaceImportContinuation::AddRepositoryDialog {
                    address_input,
                    name,
                    path,
                } => {
                    self.complete_add_repository_dialog_import(
                        address_input,
                        name,
                        path,
                        outcome,
                        ctx,
                    );
                }
                RepositorySpaceImportContinuation::SwiftyMigration { selected } => {
                    let space_id = match outcome {
                        Ok(Some(fetched)) => {
                            info!(
                                "Migration: imported repository space from {}",
                                fetched.source_address
                            );
                            Some(self.apply_fetched_repository_space(fetched, ctx))
                        }
                        Ok(None) => {
                            info!("Migration: no repository space manifest found");
                            self.swifty_migration_state.space_import_failed = true;
                            None
                        }
                        Err(err) => {
                            info!("Migration: repository space import failed: {}", err);
                            self.swifty_migration_state.space_import_failed = true;
                            None
                        }
                    };
                    self.finish_swifty_import(selected, space_id, ctx);
                }
            }
            self.needs_repaint = true;
        }
    }

    /// Apply the result of an add-repository dialog space import: open the space
    /// selector when a manifest was found, otherwise fall back to adding the
    /// input as a plain repository.
    fn complete_add_repository_dialog_import(
        &mut self,
        address_input: String,
        name_input: String,
        path_input: String,
        outcome: Result<Option<FetchedRepositorySpace>, String>,
        ctx: &egui::Context,
    ) {
        // The user closed the dialog (Cancel) while the fetch was in flight;
        // treat that as aborting the import, matching the original behavior
        // where Cancel returned before any import work ran.
        if !self.show_add_repository_modal {
            debug!("Discarding repository-space import result for a closed add-repository dialog");
            return;
        }

        match outcome {
            Ok(Some(fetched)) => {
                let space_id = self.apply_fetched_repository_space(fetched, ctx);
                self.selected_repository_space_id = Some(space_id.clone());
                self.repository_view_state.selected_repository = None;
                self.clear_completed_repository_check_banner_for_repo_change(None);
                self.open_repository_space_selector(space_id);
                self.show_add_repository_modal = false;
                self.add_repository_input_error = None;
                self.pending_repository_duplicate_add = None;
                info!("Imported repository space via add repository dialog");
            }
            Ok(None) => match self.add_repository_from_address_input(
                &address_input,
                &name_input,
                &path_input,
                ctx,
            ) {
                Ok(_) => {
                    self.show_add_repository_modal = false;
                    self.add_repository_input_error = None;
                    self.pending_repository_duplicate_add = None;
                    info!("Added repository via add repository dialog");
                }
                Err(err) => {
                    self.add_repository_input_error = Some(err);
                }
            },
            Err(err) => {
                self.add_repository_input_error = Some(err);
            }
        }
    }

    pub fn add_repository_from_space_entry(
        &mut self,
        space_id: &str,
        entry_address: &str,
        entry_name: &str,
        ctx: &egui::Context,
    ) -> Option<usize> {
        self.add_repository_from_space_entry_internal(
            space_id,
            entry_address,
            entry_name,
            ctx,
            false,
        )
    }

    fn add_repository_from_space_entry_internal(
        &mut self,
        space_id: &str,
        entry_address: &str,
        entry_name: &str,
        ctx: &egui::Context,
        allow_duplicate: bool,
    ) -> Option<usize> {
        let shared_path = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.shared_path.clone())?;
        let normalized_address = Self::normalize_repository_address_input(entry_address);
        if normalized_address.is_empty() {
            return None;
        }
        let normalized_with_trailing = Self::normalize_repo_url(&normalized_address);
        if !allow_duplicate {
            let duplicates = self.collect_duplicate_repository_bindings(
                &normalized_with_trailing,
                Some(space_id),
                &shared_path,
            );
            if !duplicates.is_empty() {
                self.queue_duplicate_repository_add_confirmation(
                    &normalized_with_trailing,
                    PendingRepositoryDuplicateAddAction::FromSpaceEntry {
                        space_id: space_id.to_string(),
                        entry_address: entry_address.to_string(),
                        entry_name: entry_name.to_string(),
                    },
                    Some(space_id),
                    &shared_path,
                );
                return None;
            }
        }

        let mut repo = Repository {
            name: if entry_name.trim().is_empty() {
                Self::default_repository_name_from_address(&normalized_address)
            } else {
                entry_name.to_string()
            },
            address: normalized_address.clone(),
            path: shared_path,
            ..Repository::default()
        };
        repo.repository_space_id = Some(space_id.to_string());
        repo.repository_space_entry_address = Some(normalized_address.clone());

        self.repository_view_state.repositories.push(repo);
        let repo_idx = self.repository_view_state.repositories.len() - 1;
        self.save_repositories();
        self.update_repository_from_url(repo_idx, ctx);
        let added_message = self.t_fmt(
            "Repository added: {name}",
            &[(
                "name",
                self.repository_view_state.repositories[repo_idx]
                    .name
                    .clone(),
            )],
        );
        self.show_success_toast(added_message);
        Some(repo_idx)
    }

    pub fn confirm_pending_duplicate_repository_add(
        &mut self,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let Some(pending) = self.pending_repository_duplicate_add.clone() else {
            return Ok(());
        };

        self.pending_repository_duplicate_add = None;
        match pending.action {
            PendingRepositoryDuplicateAddAction::FromAddressInput {
                address_input,
                name,
                path,
            } => {
                self.add_repository_from_address_input_internal(
                    &address_input,
                    &name,
                    &path,
                    ctx,
                    true,
                )?;
                self.show_add_repository_modal = false;
                self.add_repository_input_error = None;
            }
            PendingRepositoryDuplicateAddAction::FromSpaceEntry {
                space_id,
                entry_address,
                entry_name,
            } => {
                let added = self.add_repository_from_space_entry_internal(
                    &space_id,
                    &entry_address,
                    &entry_name,
                    ctx,
                    true,
                );
                if added.is_none() {
                    return Err(self.t("Failed to add repository from repository space"));
                }
            }
        }
        Ok(())
    }

    /// Dispatch a background refresh of repo.json (and foxy_addons.json for
    /// FoxyMode repos). Network I/O runs on a worker thread with retry; the
    /// parsed result is applied later on the UI thread by
    /// [`Self::poll_repo_metadata_results`], so a slow/unreachable server can
    /// no longer freeze the app.
    pub fn update_repository_from_url(&mut self, repo_index: usize, _ctx: &egui::Context) {
        if repo_index >= self.repository_view_state.repositories.len() {
            log::error!("Invalid repository index");
            return;
        }

        let repo = &self.repository_view_state.repositories[repo_index];
        if repo.address.is_empty() {
            log::error!("Repository address is empty");
            return;
        }
        let repo_address = repo.address.clone();
        let repo_name = repo.name.clone();
        let apply_client_parameters = self.repo_apply_repo_json_client_parameters(repo);
        let apply_dlc_content = self.repo_apply_repo_json_dlc_content(repo);

        if !self.pending_repo_metadata_jobs.insert(repo_address.clone()) {
            debug!(
                "Repository metadata refresh already in flight for {}; skipping duplicate",
                repo_name
            );
            return;
        }

        let base_url = repo_address.trim_end_matches('/').to_string();
        info!(
            "Refreshing repository metadata from remote for {}",
            repo_name
        );

        let tx = self.repo_metadata_result_tx.clone();
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            let outcome = Self::fetch_repository_metadata(&base_url);
            let delivered = tx
                .send(RepoMetadataFetchResult {
                    repo_index,
                    repo_address,
                    repo_name,
                    apply_client_parameters,
                    apply_dlc_content,
                    outcome,
                })
                .is_ok();
            if delivered {
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });
    }

    /// Worker-thread fetch of repo.json plus, for FoxyMode repos, foxy_addons.json.
    fn fetch_repository_metadata(base_url: &str) -> Result<RepoMetadataPayload, String> {
        let repo_url = format!("{base_url}/repo.json");
        let repo_json = fetch_json_with_retry(&repo_url)
            .map_err(|err| format!("Failed to fetch {repo_url}: {err}"))?;

        let mut addon_manifest = repo_json.clone();
        if repo_json
            .get("foxyMode")
            .and_then(Value::as_str)
            .is_some_and(|mode| !mode.trim().is_empty())
        {
            let foxy_addons_url = format!("{base_url}/foxy_addons.json");
            match fetch_json_with_retry(&foxy_addons_url) {
                Ok(foxy_addons) => addon_manifest = foxy_addons,
                Err(err) => warn!(
                    "Failed to fetch foxy_addons.json for repository metadata ({}); using repo.json: {}",
                    foxy_addons_url, err
                ),
            }
        }

        Ok(RepoMetadataPayload {
            repo_json,
            addon_manifest,
        })
    }

    /// Drain completed background metadata fetches and apply them on the UI thread.
    pub(in crate::ui::app) fn poll_repo_metadata_results(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.repo_metadata_result_rx.try_recv() {
            self.pending_repo_metadata_jobs.remove(&result.repo_address);
            self.apply_repository_metadata(result, ctx);
        }
    }

    fn apply_repository_metadata(&mut self, result: RepoMetadataFetchResult, ctx: &egui::Context) {
        let RepoMetadataFetchResult {
            repo_index,
            repo_address,
            repo_name,
            apply_client_parameters,
            apply_dlc_content,
            outcome,
        } = result;

        let (json, addon_manifest) = match outcome {
            Ok(payload) => (payload.repo_json, payload.addon_manifest),
            Err(err) => {
                log::error!("Failed to refresh repository metadata for {repo_name}: {err}");
                self.show_error_toast(self.t_fmt(
                    "Failed to refresh repository: {name}",
                    &[("name", repo_name)],
                ));
                return;
            }
        };

        // The repository list may have changed while the fetch was in flight;
        // re-resolve the index by address, falling back to a scan.
        let repo_index = if repo_index < self.repository_view_state.repositories.len()
            && self.repository_view_state.repositories[repo_index].address == repo_address
        {
            repo_index
        } else {
            match self
                .repository_view_state
                .repositories
                .iter()
                .position(|r| r.address == repo_address)
            {
                Some(idx) => idx,
                None => {
                    debug!(
                        "Repository {} no longer present; discarding fetched metadata",
                        repo_name
                    );
                    return;
                }
            }
        };

        {
            let mut remote_client_side_addons = Vec::new();
            if let Some(required_mods) = addon_manifest["requiredMods"].as_array() {
                let (addons, client_side_addons) =
                    parse_remote_addon_list(Some(required_mods.as_slice()), true);
                remote_client_side_addons.extend(client_side_addons);
                let repo = &mut self.repository_view_state.repositories[repo_index];
                repo.addons = merge_remote_addon_list(addons, &repo.addons);
            }

            if let Some(optional_mods) = addon_manifest["optionalMods"].as_array() {
                let (optional_addons, client_side_addons) =
                    parse_remote_addon_list(Some(optional_mods.as_slice()), false);
                remote_client_side_addons.extend(client_side_addons);
                let repo = &mut self.repository_view_state.repositories[repo_index];
                repo.optional_addons =
                    merge_remote_addon_list(optional_addons, &repo.optional_addons);
            }
            self.repository_view_state.repositories[repo_index].remote_client_side_addons =
                remote_client_side_addons;

            if let Some(servers) = json["servers"].as_array() {
                let mut parsed_servers = Vec::new();
                for server in servers {
                    if let (Some(name), Some(address), Some(port)) = (
                        server["name"].as_str(),
                        server["address"].as_str(),
                        server["port"].as_str(),
                    ) {
                        let password = server["password"].as_str().unwrap_or("").to_string();
                        let battle_eye = server["battleEye"].as_bool().unwrap_or(false);
                        parsed_servers.push(RepositoryServer {
                            name: name.to_string(),
                            address: address.to_string(),
                            port: port.to_string(),
                            password,
                            battle_eye,
                        });
                    }
                }
                self.repository_view_state.repositories[repo_index].servers = parsed_servers;
            }

            if let Some(icon_image_path) = json["iconImagePath"].as_str() {
                self.repository_view_state.repositories[repo_index].icon_image_path =
                    icon_image_path.to_string();
            }
            if let Some(icon_image_checksum) = json["iconImageChecksum"].as_str() {
                self.repository_view_state.repositories[repo_index].icon_image_checksum =
                    icon_image_checksum.to_string();
            }
            if let Some(repo_image_path) = json["repoImagePath"].as_str() {
                self.repository_view_state.repositories[repo_index].repo_image_path =
                    repo_image_path.to_string();
            }
            if let Some(repo_image_checksum) = json["repoImageChecksum"].as_str() {
                self.repository_view_state.repositories[repo_index].repo_image_checksum =
                    repo_image_checksum.to_string();
            }
            self.repository_view_state.repositories[repo_index].app_update_url = json
                .get("appUpdateUrl")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_default()
                .to_string();
            if apply_client_parameters
                && let Some(client_parameters) = json["clientParameters"].as_str()
            {
                apply_repo_client_parameters(
                    &mut self.repository_view_state.repositories[repo_index],
                    client_parameters,
                );
            }
            if apply_dlc_content && let Some(dlc_content) = json.get("dlcContent") {
                apply_repo_dlc_content_from_repo_json(
                    &mut self.repository_view_state.repositories[repo_index],
                    dlc_content,
                );
            }

            let updated_repo = self.repository_view_state.repositories[repo_index].clone();
            if !updated_repo.icon_image_checksum.is_empty() {
                self.download_and_load_image(
                    ctx,
                    &updated_repo.address,
                    &updated_repo.icon_image_path,
                    &updated_repo.icon_image_checksum,
                    true,
                );
            }
            if !updated_repo.repo_image_checksum.is_empty() {
                self.download_and_load_image(
                    ctx,
                    &updated_repo.address,
                    &updated_repo.repo_image_path,
                    &updated_repo.repo_image_checksum,
                    false,
                );
            }

            self.save_repositories();
            self.maybe_auto_fill_app_update_url_from_metadata();
            info!(
                "Repository metadata refreshed for {}",
                self.repository_view_state.repositories[repo_index].name
            );
        }
    }
}
