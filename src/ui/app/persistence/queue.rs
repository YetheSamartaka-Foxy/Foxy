use std::fs;
use std::sync::mpsc::{
    Receiver as StdReceiver, Sender as StdSender, TryRecvError as StdTryRecvError,
};
use std::time::Instant;

use log::{debug, error, warn};

use crate::core::utils::addon_backup;
use crate::ui::app::{
    Foxy, PERSISTENCE_DEBOUNCE_INTERVAL, PersistenceRequest, PersistenceResult,
    RepositoriesSaveOutcome,
};
use crate::ui::i18n::sanitize_locale_preference;
use crate::ui::types::{
    Repository, SettingsViewState, normalize_settings_launch_behavior, sanitize_repository_paths,
    sanitize_settings_paths,
};

impl Foxy {
    pub(crate) fn run_persistence_worker(
        request_rx: StdReceiver<PersistenceRequest>,
        result_tx: StdSender<PersistenceResult>,
    ) {
        while let Ok(request) = request_rx.recv() {
            match request {
                PersistenceRequest::SaveSettings {
                    revision,
                    settings,
                    stored_settings,
                } => {
                    let result =
                        Self::persist_settings_snapshot_to_disk(*settings, *stored_settings);
                    if result_tx
                        .send(PersistenceResult::SettingsSaved { revision, result })
                        .is_err()
                    {
                        error!(
                            "Failed to send settings save result (revision={}): UI channel closed",
                            revision
                        );
                    }
                }
                PersistenceRequest::SaveRepositories {
                    revision,
                    repositories,
                    debug_mode,
                } => {
                    let result =
                        Self::persist_repositories_snapshot_to_disk(repositories, debug_mode);
                    if result_tx
                        .send(PersistenceResult::RepositoriesSaved { revision, result })
                        .is_err()
                    {
                        error!(
                            "Failed to send repositories save result (revision={}): UI channel closed",
                            revision
                        );
                    }
                }
                PersistenceRequest::RefreshBackupInventory {
                    request_id,
                    backup_root,
                } => {
                    let result = addon_backup::list_all_addon_backups(&backup_root)
                        .map_err(|err| err.to_string());
                    if result_tx
                        .send(PersistenceResult::BackupInventoryRefreshed { request_id, result })
                        .is_err()
                    {
                        error!(
                            "Failed to send backup inventory result (request_id={}): UI channel closed",
                            request_id
                        );
                    }
                }
            }
        }
    }

    fn prepare_settings_for_persistence(
        current_settings: SettingsViewState,
        stored_settings: Option<SettingsViewState>,
    ) -> SettingsViewState {
        let debug_mode = current_settings.debug_mode;
        let show_debug_windows = current_settings.show_debug_windows;
        let mut settings_to_save = if debug_mode {
            stored_settings.unwrap_or(current_settings)
        } else {
            current_settings
        };

        settings_to_save.debug_mode = debug_mode;
        settings_to_save.show_debug_windows = show_debug_windows;
        settings_to_save
            .additional_folders
            .retain(|folder| !Self::is_generated_debug_folder(folder));
        settings_to_save
            .cleanup_folders
            .retain(|(folder, _)| !Self::is_generated_debug_folder(folder));
        settings_to_save.additional_folders_filter.clear();
        settings_to_save.cleanup_folders_filter.clear();
        settings_to_save.locale = sanitize_locale_preference(&settings_to_save.locale);
        settings_to_save.locale_preference_migrated = true;
        sanitize_settings_paths(&mut settings_to_save);
        normalize_settings_launch_behavior(&mut settings_to_save);
        if settings_to_save.backup_max_age_days == Some(0) {
            settings_to_save.backup_max_age_days = None;
        }
        if settings_to_save.download_speed_limit_mbps == Some(0) {
            settings_to_save.download_speed_limit_mbps = Some(1);
        }
        settings_to_save.font_sizes.clamp_to_limits();

        settings_to_save
    }

    fn persist_settings_snapshot_to_disk(
        settings: SettingsViewState,
        stored_settings: Option<SettingsViewState>,
    ) -> Result<(), String> {
        let settings_to_save = Self::prepare_settings_for_persistence(settings, stored_settings);
        let settings_value = serde_json::to_value(&settings_to_save)
            .map_err(|err| format!("Failed to serialize settings: {}", err))?;

        crate::core::game::spaces::write_split_settings(
            &settings_value,
            &Self::get_app_settings_path(),
            &Self::get_game_settings_path(),
        )
    }

    fn prepare_repositories_for_persistence(
        repositories: Vec<Repository>,
    ) -> (Vec<Repository>, usize) {
        let mut to_save = Vec::with_capacity(repositories.len());
        let mut skipped_synthetic = 0usize;

        for mut repo in repositories {
            if Self::is_generated_debug_repository(&repo) {
                skipped_synthetic += 1;
                continue;
            }

            sanitize_repository_paths(&mut repo);
            repo.addons = repo.addons.into_iter().collect();
            repo.optional_addons = repo.optional_addons.into_iter().collect();
            for profile in &mut repo.profiles {
                profile.addons = profile
                    .addons
                    .clone()
                    .into_iter()
                    .filter(|(_, enabled)| !*enabled)
                    .collect();
                profile.optional_addons = profile
                    .optional_addons
                    .clone()
                    .into_iter()
                    .filter(|(_, enabled)| *enabled)
                    .collect();
            }
            to_save.push(repo);
        }

        (to_save, skipped_synthetic)
    }

    fn persist_repositories_snapshot_to_disk(
        repositories: Vec<Repository>,
        debug_mode: bool,
    ) -> Result<RepositoriesSaveOutcome, String> {
        if debug_mode {
            return Ok(RepositoriesSaveOutcome::SkippedDebugMode);
        }

        let path = Self::get_repositories_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "Failed to create repositories.json parent directory {}: {}",
                    parent.display(),
                    err
                )
            })?;
        }

        let (to_save, skipped_synthetic) = Self::prepare_repositories_for_persistence(repositories);
        let repository_count = to_save.len();
        let json = serde_json::to_string_pretty(&to_save)
            .map_err(|err| format!("Failed to serialize repositories: {}", err))?;

        crate::core::utils::fs_safety::atomic_write(&path, json.as_bytes())
            .map_err(|err| format!("Failed to write repositories.json: {}", err))?;

        Ok(RepositoriesSaveOutcome::Saved {
            repository_count,
            skipped_synthetic,
        })
    }

    pub(crate) fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
        self.settings_revision = self.settings_revision.saturating_add(1);
        self.settings_last_mutated_at = Some(Instant::now());
        self.needs_repaint = true;
    }

    pub(crate) fn mark_repositories_dirty(&mut self) {
        self.repositories_dirty = true;
        self.repositories_revision = self.repositories_revision.saturating_add(1);
        self.repositories_last_mutated_at = Some(Instant::now());
        self.needs_repaint = true;
    }

    fn settings_save_ready(&self, force: bool) -> bool {
        self.settings_dirty
            && self.settings_save_in_flight_revision.is_none()
            && self.settings_last_mutated_at.is_some_and(|changed_at| {
                force || changed_at.elapsed() >= PERSISTENCE_DEBOUNCE_INTERVAL
            })
    }

    fn repositories_save_ready(&self, force: bool) -> bool {
        self.repositories_dirty
            && self.repositories_save_in_flight_revision.is_none()
            && self.repositories_last_mutated_at.is_some_and(|changed_at| {
                force || changed_at.elapsed() >= PERSISTENCE_DEBOUNCE_INTERVAL
            })
    }

    fn queue_settings_save(&mut self) {
        let revision = self.settings_revision;
        let request = PersistenceRequest::SaveSettings {
            revision,
            settings: Box::new(self.settings_view_state.clone()),
            stored_settings: Box::new(self.stored_settings.clone()),
        };

        match self.persistence_request_tx.send(request) {
            Ok(()) => {
                self.settings_save_in_flight_revision = Some(revision);
            }
            Err(err) => {
                error!("Failed to queue settings save request: {}", err);
                self.settings_last_mutated_at = Some(Instant::now());
            }
        }
    }

    fn queue_repositories_save(&mut self) {
        let revision = self.repositories_revision;
        let request = PersistenceRequest::SaveRepositories {
            revision,
            repositories: self.repository_view_state.repositories.clone(),
            debug_mode: self.settings_view_state.debug_mode,
        };

        match self.persistence_request_tx.send(request) {
            Ok(()) => {
                self.repositories_save_in_flight_revision = Some(revision);
            }
            Err(err) => {
                error!("Failed to queue repositories save request: {}", err);
                self.repositories_last_mutated_at = Some(Instant::now());
            }
        }
    }

    pub(crate) fn maybe_dispatch_persistence_requests(&mut self, force: bool) {
        if self.settings_save_ready(force) {
            self.queue_settings_save();
        }

        if self.repositories_save_ready(force) {
            self.queue_repositories_save();
        }

        self.dispatch_backup_inventory_refresh_request();
    }

    pub(crate) fn poll_persistence_results(&mut self) {
        loop {
            match self.persistence_result_rx.try_recv() {
                Ok(PersistenceResult::SettingsSaved { revision, result }) => {
                    if self.settings_save_in_flight_revision == Some(revision) {
                        self.settings_save_in_flight_revision = None;
                    }

                    match result {
                        Ok(()) => {
                            self.settings_completed_revision =
                                self.settings_completed_revision.max(revision);
                            self.settings_dirty =
                                self.settings_completed_revision < self.settings_revision;
                            if !self.settings_dirty {
                                self.settings_last_mutated_at = None;
                            }
                            debug!("Saved app_settings.json and game_settings.json");
                        }
                        Err(err) => {
                            self.settings_dirty = true;
                            self.settings_last_mutated_at = Some(Instant::now());
                            error!("{}", err);
                        }
                    }
                }
                Ok(PersistenceResult::RepositoriesSaved { revision, result }) => {
                    if self.repositories_save_in_flight_revision == Some(revision) {
                        self.repositories_save_in_flight_revision = None;
                    }

                    match result {
                        Ok(RepositoriesSaveOutcome::Saved {
                            repository_count,
                            skipped_synthetic,
                        }) => {
                            self.repositories_completed_revision =
                                self.repositories_completed_revision.max(revision);
                            self.repositories_dirty =
                                self.repositories_completed_revision < self.repositories_revision;
                            if !self.repositories_dirty {
                                self.repositories_last_mutated_at = None;
                            }
                            if skipped_synthetic > 0 {
                                warn!(
                                    "Skipped {} synthetic debug repositories while saving repositories.json",
                                    skipped_synthetic
                                );
                            }
                            debug!(
                                "Saved repositories.json with {} repositories",
                                repository_count
                            );
                        }
                        Ok(RepositoriesSaveOutcome::SkippedDebugMode) => {
                            self.repositories_completed_revision =
                                self.repositories_completed_revision.max(revision);
                            self.repositories_dirty =
                                self.repositories_completed_revision < self.repositories_revision;
                            if !self.repositories_dirty {
                                self.repositories_last_mutated_at = None;
                            }
                            warn!("Skipping repositories.json save while debug mode is active");
                        }
                        Err(err) => {
                            self.repositories_dirty = true;
                            self.repositories_last_mutated_at = Some(Instant::now());
                            error!("{}", err);
                        }
                    }
                }
                Ok(PersistenceResult::BackupInventoryRefreshed { request_id, result }) => {
                    self.handle_backup_inventory_refresh_result(request_id, result);
                }
                Err(StdTryRecvError::Empty) => break,
                Err(StdTryRecvError::Disconnected) => {
                    warn!("Persistence result channel disconnected");
                    break;
                }
            }
        }
    }

    pub(crate) fn has_pending_persistence_writes(&self) -> bool {
        self.settings_dirty
            || self.settings_save_in_flight_revision.is_some()
            || self.repositories_dirty
            || self.repositories_save_in_flight_revision.is_some()
    }
}
