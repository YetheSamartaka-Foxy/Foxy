// for concurrency control
use crate::core::api::ProgressEvent;
use crate::core::models::context::FoxyContext;
use crate::core::models::model_tree::Tree;
use crate::core::models::modification::FoxyMod;
use crate::core::models::modification_file::FoxyModFile;
use crate::core::models::modification_file_part::{FoxyModFilePart, part_display_path};
use crate::core::models::repository::FoxyRepository;
use crate::core::models::trait_has_local_checksum::HasLocalChecksum;
use crate::core::tasks::init_database::{
    DB_WRITE_SEMAPHORE, sqlite_is_locked_error, sqlite_labeled_write_scope, sqlite_perf_snapshot,
    sqlite_sleep_for_lock_retry,
};
use crate::core::utils::content_hash::FlexHasher;
use futures::stream::{self, StreamExt};
use log::{debug, error, info, warn};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Read, Seek};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::sync::broadcast::Sender;
use tokio::sync::{Semaphore, watch};

const CONCURRENCY_LIMIT: usize = 64;
// Oversubscription factors: each file hashing task is a mix of sequential I/O + CPU-bound
// hashing in a single spawn_blocking thread. Light files (few parts) benefit from moderate
// oversubscription to hide I/O latency; heavy files need less since they're CPU-dominated.
const FILE_IO_OVERSUBSCRIPTION_LIGHT: usize = 4;
const FILE_IO_OVERSUBSCRIPTION_MEDIUM: usize = 3;
const FILE_IO_OVERSUBSCRIPTION_HEAVY: usize = 2;
const FILE_IO_OVERSUBSCRIPTION_VERY_HEAVY: usize = 2;
const MAX_FILE_JOB_CONCURRENCY: usize = 256;
/// How many rows to persist per transaction during bulk hash persist.
/// Larger chunks reduce transaction overhead (fewer commits, less WAL churn).
/// 25,000 rows at 4 params each ≈ 100KB of bind data - trivial for SQLite.
const PERSIST_LOG_INTERVAL: usize = 25_000;

mod context;
mod file_hashes;
mod part_hashes;
mod pbo_layout;
mod persistence;
mod pipeline;
mod propagation;
mod scheduling;

pub(crate) use context::RepositoryHashContext;
pub(crate) use file_hashes::{
    FileHashBatchResult, HashPhaseTimings, calculate_hashes_for_files,
    calculate_hashes_for_files_in_tree_with_profile,
    calculate_hashes_for_files_in_tree_with_profile_and_sticky_auto,
    calculate_hashes_for_files_with_profile,
    calculate_hashes_for_files_with_profile_and_sticky_auto,
};
pub use persistence::calculate_hash_from_items;
pub(crate) use pipeline::{
    HashCalculationResult, calculate_hashes, calculate_hashes_with_profile,
    calculate_hashes_with_tree_and_profile_cancellable,
};
pub(crate) use propagation::{
    finalize_repository_content_hashes_from_mods, finalize_repository_hashes_from_mods,
    finalize_repository_hashes_from_tree, pre_propagate_sibling_checksums,
    propagate_checksums_to_siblings,
};
pub(crate) use scheduling::{AddonHashMetrics, HashStorageClass, detect_storage_class_for_path};
