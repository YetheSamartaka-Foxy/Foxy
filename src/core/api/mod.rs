use std::collections::{HashMap, HashSet, VecDeque};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender as StdSender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use tokio::runtime::Builder;
use tokio::sync::Semaphore;
use tokio::sync::broadcast::Sender;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::core::models::context::FoxyContext;
use crate::core::models::download_target_file::{
    fetch_all_download_targets_with_mod, fetch_all_download_targets_with_mod_and_name,
};
use crate::core::models::model_tree::Tree;
use crate::core::models::modification::FoxyMod;
use crate::core::models::modification_file::FoxyModFile;
use crate::core::models::pending_update::{
    clear_pending_update_for_context, save_pending_update_for_context,
};
use crate::core::models::recheck_level::RecheckLevel;
use crate::core::models::repository::{FoxyRepository, load_repository_by_remote_url};
use crate::core::tasks::calculate_hashes::{calculate_hashes, calculate_hashes_for_files};
use crate::core::tasks::create_context::{create_context, create_context_with_recheck_level};
use crate::core::tasks::download_files::DownloadModCompletion;
use crate::core::tasks::init_database::SQLITE_MAX_VARIABLES;
use crate::core::utils::app_paths;

use flexi_logger::writers::LogWriter;
use flexi_logger::{
    Cleanup, Criterion, DeferredNow, Duplicate, FileSpec, Logger, Naming, Record, WriteMode,
};

mod fs_watcher;
mod logging;
mod quick_scan;
mod startup_diagnostics;
mod sync_pipeline;
mod types;

pub use fs_watcher::spawn_repo_fs_watcher;
pub(crate) use logging::send_progress_event;
pub use logging::{
    activity_log_generation, activity_log_snapshot, logger_health, next_operation_id,
};
pub(crate) use logging::{ensure_logger, ensure_logger_with_terminal};
pub use quick_scan::{
    StartupRepositoryInstance, filter_repo_urls_with_db_entry, plan_startup_quick_scan_repos,
    recalculate_hashes_for_addon_by_name, spawn_quick_local_scan, spawn_quick_local_scan_instances,
};
pub use startup_diagnostics::{
    StartupStoragePath, all_storage_devices_lines, log_startup_system_diagnostics,
    startup_system_diagnostics_lines,
};
pub use sync_pipeline::spawn_repository_sync;
pub use types::{
    FileDiffSummary, FsChangeEvent, LogEntry, ModDiffSummary, ProgressEvent, QuickScanResult,
    RepositorySyncOptions, SyncMode,
};

#[cfg(test)]
mod tests;
