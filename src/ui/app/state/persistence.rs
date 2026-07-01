use std::path::PathBuf;
use std::time::Duration;

use crate::core::utils::addon_backup;
use crate::ui::types::{Repository, SettingsViewState};

pub const PERSISTENCE_DEBOUNCE_INTERVAL: Duration = Duration::from_millis(350);

#[derive(Debug)]
pub enum PersistenceRequest {
    SaveSettings {
        revision: u64,
        settings: Box<SettingsViewState>,
        stored_settings: Box<Option<SettingsViewState>>,
    },
    SaveRepositories {
        revision: u64,
        repositories: Vec<Repository>,
        debug_mode: bool,
    },
    RefreshBackupInventory {
        request_id: u64,
        backup_root: PathBuf,
    },
}

#[derive(Debug)]
pub enum PersistenceResult {
    SettingsSaved {
        revision: u64,
        result: Result<(), String>,
    },
    RepositoriesSaved {
        revision: u64,
        result: Result<RepositoriesSaveOutcome, String>,
    },
    BackupInventoryRefreshed {
        request_id: u64,
        result: Result<Vec<addon_backup::AddonBackupRecord>, String>,
    },
}

#[derive(Debug)]
pub enum RepositoriesSaveOutcome {
    Saved {
        repository_count: usize,
        skipped_synthetic: usize,
    },
    SkippedDebugMode,
}
