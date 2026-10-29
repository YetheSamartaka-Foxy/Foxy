use super::direct_read::DirectReadPlan;
use super::part_hashes::{
    HashRunCounters, PartHashProgress, PartReadOptions, PartSpanSource, calculate_part_hashes,
};
use super::*;
use crate::core::utils::content_hash::is_blake3_checksum;
use crate::core::utils::resource_profile::{ResourcePressure, ResourceProfile};
use crate::core::utils::speed_of_light::{SolLight, op_id_extra, sol_line};
use crate::ui::types::HashIoProfilePreference;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use sysinfo::Disks;

/// A hash job's parts: indices into the tree's part list, which every job of
/// a pass shares instead of each holding a copy of its file's parts.
#[derive(Clone)]
pub(super) struct JobParts {
    all: Arc<Vec<FoxyModFilePart>>,
    indices: Vec<usize>,
    /// `all[n]` is the part at `indices[n]` rather than at `indices[n]` itself.
    positional: bool,
}

impl JobParts {
    /// Parts owned by this job alone, keyed by their tree index.
    #[cfg(test)]
    pub(super) fn owned(parts: Vec<(usize, FoxyModFilePart)>) -> Self {
        let (indices, parts): (Vec<usize>, Vec<FoxyModFilePart>) = parts.into_iter().unzip();
        Self {
            all: Arc::new(parts),
            indices,
            positional: true,
        }
    }

    fn shared(all: &Arc<Vec<FoxyModFilePart>>, indices: Vec<usize>) -> Self {
        Self {
            all: all.clone(),
            indices,
            positional: false,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.indices.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Each part with its tree index, in the job's order.
    pub(super) fn iter(&self) -> impl Iterator<Item = (usize, &FoxyModFilePart)> + '_ {
        self.indices.iter().enumerate().map(|(position, &index)| {
            let slot = if self.positional { position } else { index };
            (index, &self.all[slot])
        })
    }

    pub(super) fn parts(&self) -> impl Iterator<Item = &FoxyModFilePart> + '_ {
        self.iter().map(|(_, part)| part)
    }

    pub(super) fn indices(&self) -> &[usize] {
        &self.indices
    }
}

/// Move the tree's parts behind one `Arc` for the jobs of a hash pass; hand
/// them back with [`return_tree_parts`] before anything reads the tree's parts.
pub(super) fn lend_tree_parts(data_tree: &mut Tree) -> Arc<Vec<FoxyModFilePart>> {
    Arc::new(std::mem::take(&mut data_tree.parts))
}

pub(super) fn return_tree_parts(data_tree: &mut Tree, parts: Arc<Vec<FoxyModFilePart>>) {
    data_tree.parts = Arc::try_unwrap(parts).unwrap_or_else(|shared| (*shared).clone());
}

#[derive(Clone)]
pub(super) struct FileHashJob {
    pub(super) file_idx: usize,
    pub(super) file_path: String,
    pub(super) file_length: u64,
    pub(super) file_remote_checksum: String,
    pub(super) indexed_parts: JobParts,
    pub(super) span_source: PartSpanSource,
    /// The database already holds a local checksum for the file or a part.
    pub(super) has_local_baseline: bool,
    /// Capture the file identity around the hash, for the verified-hash record.
    pub(super) capture_identity: bool,
}

pub(super) struct FileHashResult {
    pub(super) file_idx: usize,
    pub(super) updated_parts: Vec<(usize, FoxyModFilePart)>,
    pub(super) whole_file_checksum: Option<String>,
    /// Content fingerprint sampled while the file was hot; see
    /// `PartHashCalculation::content_hash`.
    pub(super) content_hash: Option<String>,
    pub(super) file_path: String,
    pub(super) elapsed: std::time::Duration,
    pub(super) parts_count: usize,
    pub(super) missing_file: bool,
    pub(super) issue: Option<super::evidence::HashIssueKind>,
    pub(super) completed: bool,
    pub(super) read_identity: Option<super::verified_record::FileIdentity>,
    pub(super) manifest_signature: String,
    pub(super) part_metrics: super::part_hashes::PartHashMetrics,
    /// Set when the file matched the remote and did not change while it was
    /// read, for the verified-hash record.
    pub(super) verified: Option<super::verified_record::VerifiedFile>,
}

#[derive(Clone, Copy)]
pub(super) struct HashRunProgress {
    total_files: usize,
    total_parts: usize,
    total_bytes: u64,
    initial_files_done: usize,
    initial_parts_done: usize,
    initial_bytes_done: u64,
}

fn common_hash_scope_root(jobs: &[FileHashJob]) -> Option<PathBuf> {
    let mut root = jobs.first()?.file_path_path()?.to_path_buf();
    for job in &jobs[1..] {
        let parent = job.file_path_path()?;
        while !parent.starts_with(&root) {
            if !root.pop() {
                return None;
            }
        }
    }
    Some(root)
}

impl FileHashJob {
    fn file_path_path(&self) -> Option<&Path> {
        Path::new(&self.file_path).parent()
    }
}

impl HashRunProgress {
    fn new(total_files: usize, total_parts: usize, total_bytes: u64) -> Self {
        Self {
            total_files,
            total_parts,
            total_bytes,
            initial_files_done: 0,
            initial_parts_done: 0,
            initial_bytes_done: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct HashSchedulerLimits {
    pub(super) file_concurrency: usize,
    pub(super) global_part_concurrency: usize,
    pub(super) storage_class: HashStorageClass,
    pub(super) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HashStorageClass {
    Hdd,
    Ssd,
    Removable,
    Unknown,
}

impl std::fmt::Display for HashStorageClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hdd => write!(f, "hdd"),
            Self::Ssd => write!(f, "ssd"),
            Self::Removable => write!(f, "removable"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HashProfileDecision {
    pub(crate) requested: HashIoProfilePreference,
    pub(crate) selected: HashIoProfilePreference,
    pub(crate) reason: String,
    pub(crate) benchmarked_files: usize,
    pub(crate) benchmarked_bytes: u64,
    pub(crate) benchmark_elapsed: std::time::Duration,
    pub(crate) sticky: bool,
}

impl HashProfileDecision {
    fn manual(profile: HashIoProfilePreference) -> Self {
        Self {
            requested: profile,
            selected: profile,
            reason: "manual override".to_string(),
            benchmarked_files: 0,
            benchmarked_bytes: 0,
            benchmark_elapsed: std::time::Duration::ZERO,
            sticky: false,
        }
    }

    fn sticky_auto(profile: HashIoProfilePreference) -> Self {
        Self {
            requested: HashIoProfilePreference::Auto,
            selected: profile,
            reason: "sticky auto decision".to_string(),
            benchmarked_files: 0,
            benchmarked_bytes: 0,
            benchmark_elapsed: std::time::Duration::ZERO,
            sticky: true,
        }
    }
}

const MIN_AUTO_BENCHMARK_FILES: usize = 3;
const MIN_AUTO_BENCHMARK_BYTES: u64 = 256 * 1024 * 1024;
/// Most profiles the auto benchmark can try; the sample is sized for one
/// disjoint group per profile.
const MAX_BENCHMARK_GROUPS: usize = 3;
const LOW_WAIT_AGGRESSIVE_THRESHOLD: f64 = 0.01;
const PROFILE_SWITCH_MIN_IMPROVEMENT_PERCENT: f64 = 10.0;
static AUTO_BENCHMARK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn cap_auto_hash_profile(
    profile: HashIoProfilePreference,
    resource_profile: ResourceProfile,
) -> (HashIoProfilePreference, Option<String>) {
    let capped = match resource_profile.pressure {
        ResourcePressure::Normal => profile,
        ResourcePressure::Constrained => {
            if profile == HashIoProfilePreference::Aggressive {
                HashIoProfilePreference::Balanced
            } else {
                profile
            }
        }
        ResourcePressure::Severe => HashIoProfilePreference::Conservative,
    };

    if capped == profile {
        (profile, None)
    } else {
        (
            capped,
            Some(format!(
                "resource pressure cap: {}",
                resource_profile.summary()
            )),
        )
    }
}

pub(super) fn hash_cpu_budget() -> usize {
    let logical_cpus =
        crate::core::utils::cpu_threads::cpu_threads().unwrap_or(CONCURRENCY_LIMIT.max(1));
    // Reserve ~25% of cores for the OS, UI thread, and other background work.
    // Floor of 1 ensures at least one worker even on single-core machines.
    ((logical_cpus * 3) / 4).max(1)
}

pub(super) fn hash_scheduler_limits(
    job_count: usize,
    total_parts: usize,
    profile: HashIoProfilePreference,
) -> HashSchedulerLimits {
    hash_scheduler_limits_for_resources(job_count, total_parts, profile, ResourceProfile::sample())
}

fn hash_scheduler_limits_for_resources(
    job_count: usize,
    total_parts: usize,
    profile: HashIoProfilePreference,
    resource_profile: ResourceProfile,
) -> HashSchedulerLimits {
    hash_scheduler_limits_for_environment(
        job_count,
        total_parts,
        profile,
        resource_profile,
        HashStorageClass::Unknown,
    )
}

fn hash_scheduler_limits_for_environment(
    job_count: usize,
    total_parts: usize,
    profile: HashIoProfilePreference,
    resource_profile: ResourceProfile,
    storage_class: HashStorageClass,
) -> HashSchedulerLimits {
    let avg_parts = avg_parts_per_file(job_count, total_parts);

    if profile == HashIoProfilePreference::Conservative {
        let concurrency = job_count.clamp(1, 2);
        return apply_storage_limit(
            HashSchedulerLimits {
                file_concurrency: concurrency,
                global_part_concurrency: concurrency,
                storage_class,
                reason: "conservative profile".to_string(),
            },
            avg_parts,
        );
    }
    if profile == HashIoProfilePreference::Balanced {
        let cpu_budget = hash_cpu_budget();
        let max_concurrency = match resource_profile.pressure {
            ResourcePressure::Normal => 8,
            ResourcePressure::Constrained => 4,
            ResourcePressure::Severe => 2,
        };
        let concurrency = job_count.min(cpu_budget.clamp(2, max_concurrency)).max(1);
        return apply_storage_limit(
            HashSchedulerLimits {
                file_concurrency: concurrency,
                global_part_concurrency: concurrency,
                storage_class,
                reason: format!(
                    "balanced profile resource_pressure={}",
                    resource_profile.pressure
                ),
            },
            avg_parts,
        );
    }

    let cpu_budget = hash_cpu_budget();
    let file_oversubscription = if avg_parts <= 2.0 {
        FILE_IO_OVERSUBSCRIPTION_LIGHT
    } else if avg_parts <= 8.0 {
        FILE_IO_OVERSUBSCRIPTION_MEDIUM
    } else if avg_parts <= 32.0 {
        FILE_IO_OVERSUBSCRIPTION_HEAVY
    } else {
        FILE_IO_OVERSUBSCRIPTION_VERY_HEAVY
    };
    let resource_cap = match resource_profile.pressure {
        ResourcePressure::Normal => MAX_FILE_JOB_CONCURRENCY,
        ResourcePressure::Constrained => 8,
        ResourcePressure::Severe => 2,
    };
    let file_io_budget = (cpu_budget * file_oversubscription)
        .clamp(1, MAX_FILE_JOB_CONCURRENCY)
        .min(resource_cap);
    let file_concurrency = job_count.min(file_io_budget).max(1);
    // Global part concurrency is based on the full IO budget - this is the overall cap on
    // in-flight spawn_blocking tasks across ALL files, controlled via a Semaphore.
    let global_part_concurrency = (cpu_budget * file_oversubscription)
        .clamp(1, MAX_FILE_JOB_CONCURRENCY)
        .min(resource_cap);

    apply_storage_limit(
        HashSchedulerLimits {
            file_concurrency,
            global_part_concurrency,
            storage_class,
            reason: format!(
                "aggressive profile avg_parts_per_file={:.2} resource_pressure={}",
                avg_parts, resource_profile.pressure
            ),
        },
        avg_parts,
    )
}

fn boosted_aggressive_hash_scheduler_limits_for_environment(
    job_count: usize,
    total_parts: usize,
    resource_profile: ResourceProfile,
    storage_class: HashStorageClass,
    reason: &str,
) -> HashSchedulerLimits {
    let mut limits = hash_scheduler_limits_for_environment(
        job_count,
        total_parts,
        HashIoProfilePreference::Aggressive,
        resource_profile,
        storage_class,
    );
    if limits.storage_class == HashStorageClass::Hdd
        || resource_profile.pressure != ResourcePressure::Normal
    {
        return limits;
    }

    let avg_parts = avg_parts_per_file(job_count, total_parts);
    let cpu_budget = hash_cpu_budget();
    let part_multiplier = if avg_parts > 64.0 { 3 } else { 2 };
    let boosted_part_cap = (cpu_budget * FILE_IO_OVERSUBSCRIPTION_LIGHT * part_multiplier)
        .clamp(1, MAX_FILE_JOB_CONCURRENCY);
    limits.global_part_concurrency = limits
        .global_part_concurrency
        .max(boosted_part_cap)
        .min(MAX_FILE_JOB_CONCURRENCY);
    limits.file_concurrency = limits
        .file_concurrency
        .max(job_count.min(cpu_budget * FILE_IO_OVERSUBSCRIPTION_LIGHT))
        .min(job_count.max(1))
        .min(MAX_FILE_JOB_CONCURRENCY);
    limits.reason = format!(
        "{}; boosted aggressive after {} avg_parts_per_file={:.2}",
        limits.reason, reason, avg_parts
    );
    apply_storage_limit(limits, avg_parts)
}

fn avg_parts_per_file(job_count: usize, total_parts: usize) -> f64 {
    if job_count == 0 {
        1.0
    } else {
        total_parts as f64 / job_count as f64
    }
}

fn is_large_part_workload(job_count: usize, total_parts: usize) -> bool {
    total_parts >= 128 && avg_parts_per_file(job_count, total_parts) > 32.0
}

fn apply_storage_limit(mut limits: HashSchedulerLimits, avg_parts: f64) -> HashSchedulerLimits {
    if limits.storage_class == HashStorageClass::Hdd && avg_parts > 32.0 {
        let cap = 2usize;
        limits.file_concurrency = limits.file_concurrency.min(cap).max(1);
        limits.global_part_concurrency = limits.global_part_concurrency.min(cap).max(1);
        limits.reason = format!(
            "{}; hdd large-part cap avg_parts_per_file={:.2}",
            limits.reason, avg_parts
        );
    }
    limits
}

fn detect_hash_storage_class(jobs: &[FileHashJob]) -> HashStorageClass {
    let Some(path) = jobs
        .iter()
        .map(|job| job.file_path.trim())
        .find(|path| !path.is_empty())
    else {
        return HashStorageClass::Unknown;
    };
    detect_storage_class_for_path(path)
}

/// How long a refreshed mount table is reused. Long enough that a per-file
/// lookup is cheap, short enough that plugging a drive in is picked up.
const STORAGE_CLASS_CACHE_TTL: Duration = Duration::from_secs(60);

type StorageClassMounts = Vec<(PathBuf, HashStorageClass)>;

static STORAGE_CLASS_MOUNTS: Mutex<Option<(Instant, StorageClassMounts)>> = Mutex::new(None);

/// Snapshot of mount point -> storage class. Not space-derived (disk topology is
/// process-global), but still invalidated on space switch so a space that lives
/// on a freshly attached drive never inherits a stale classification.
pub(crate) fn invalidate_storage_class_cache() {
    if let Ok(mut guard) = STORAGE_CLASS_MOUNTS.lock() {
        *guard = None;
    }
}

fn storage_class_mounts() -> StorageClassMounts {
    let Ok(mut guard) = STORAGE_CLASS_MOUNTS.lock() else {
        return refresh_storage_class_mounts();
    };
    if let Some((refreshed_at, mounts)) = guard.as_ref()
        && refreshed_at.elapsed() < STORAGE_CLASS_CACHE_TTL
    {
        return mounts.clone();
    }
    let mounts = refresh_storage_class_mounts();
    *guard = Some((Instant::now(), mounts.clone()));
    mounts
}

fn refresh_storage_class_mounts() -> StorageClassMounts {
    Disks::new_with_refreshed_list()
        .iter()
        .map(|disk| {
            let kind = format!("{:?}", disk.kind()).to_ascii_lowercase();
            let class = if kind.contains("hdd") {
                HashStorageClass::Hdd
            } else if kind.contains("ssd") {
                HashStorageClass::Ssd
            } else if disk.is_removable() {
                HashStorageClass::Removable
            } else {
                HashStorageClass::Unknown
            };
            (disk.mount_point().to_path_buf(), class)
        })
        .collect()
}

/// Storage class the user's setting forces for every drive; 0 means Auto.
static STORAGE_CLASS_OVERRIDE: AtomicU8 = AtomicU8::new(0);

fn encode_storage_class(class: Option<HashStorageClass>) -> u8 {
    match class {
        None | Some(HashStorageClass::Unknown) => 0,
        Some(HashStorageClass::Ssd) => 1,
        Some(HashStorageClass::Hdd) => 2,
        Some(HashStorageClass::Removable) => 3,
    }
}

fn decode_storage_class(encoded: u8) -> Option<HashStorageClass> {
    match encoded {
        1 => Some(HashStorageClass::Ssd),
        2 => Some(HashStorageClass::Hdd),
        3 => Some(HashStorageClass::Removable),
        _ => None,
    }
}

/// Classify every drive as `class` instead of detecting it, or detect again
/// with `None`.
pub(crate) fn set_storage_class_override(class: Option<HashStorageClass>) {
    let encoded = encode_storage_class(class);
    if STORAGE_CLASS_OVERRIDE.swap(encoded, Ordering::Relaxed) != encoded {
        match decode_storage_class(encoded) {
            Some(class) => info!("Storage type forced to {class} for every drive"),
            None => info!("Storage type set to auto-detect"),
        }
    }
}

pub(crate) fn detect_storage_class_for_path(path: &str) -> HashStorageClass {
    let path = path.trim();
    if path.is_empty() {
        return HashStorageClass::Unknown;
    }
    if let Some(class) = decode_storage_class(STORAGE_CLASS_OVERRIDE.load(Ordering::Relaxed)) {
        return class;
    }
    let path = Path::new(path);
    storage_class_mounts()
        .into_iter()
        .filter(|(mount, _)| storage_path_starts_with_mount(path, mount))
        .max_by_key(|(mount, _)| mount.as_os_str().len())
        .map(|(_, class)| class)
        .unwrap_or(HashStorageClass::Unknown)
}

pub(super) fn storage_path_starts_with_mount(path: &Path, mount: &Path) -> bool {
    path.starts_with(mount)
        || normalized_storage_path(path).starts_with(&normalized_storage_path(mount))
}

fn normalized_storage_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

fn auto_hash_profile_for_environment(
    job_count: usize,
    total_parts: usize,
    resource_profile: ResourceProfile,
    storage_class: HashStorageClass,
) -> (HashIoProfilePreference, String) {
    let avg_parts = avg_parts_per_file(job_count, total_parts);
    if storage_class == HashStorageClass::Hdd && is_large_part_workload(job_count, total_parts) {
        return (
            HashIoProfilePreference::Conservative,
            format!("hdd large-part workload avg_parts_per_file={avg_parts:.2}"),
        );
    }

    let base = match storage_class {
        HashStorageClass::Ssd => HashIoProfilePreference::Aggressive,
        HashStorageClass::Hdd | HashStorageClass::Removable => {
            if avg_parts <= 8.0 {
                HashIoProfilePreference::Balanced
            } else {
                HashIoProfilePreference::Conservative
            }
        }
        HashStorageClass::Unknown => {
            if avg_parts <= 8.0 {
                HashIoProfilePreference::Aggressive
            } else {
                HashIoProfilePreference::Balanced
            }
        }
    };
    let (profile, cap_reason) = cap_auto_hash_profile(base, resource_profile);
    (
        profile,
        cap_reason.unwrap_or_else(|| {
            format!("storage heuristic storage={storage_class} avg_parts_per_file={avg_parts:.2}")
        }),
    )
}

fn benchmark_profiles_for_environment(
    initial_profile: HashIoProfilePreference,
    resource_profile: ResourceProfile,
    storage_class: HashStorageClass,
    job_count: usize,
    total_parts: usize,
) -> Vec<HashIoProfilePreference> {
    if storage_class == HashStorageClass::Hdd && is_large_part_workload(job_count, total_parts) {
        return vec![HashIoProfilePreference::Conservative];
    }

    let candidates: &[HashIoProfilePreference] = match resource_profile.pressure {
        ResourcePressure::Normal => match storage_class {
            HashStorageClass::Hdd | HashStorageClass::Removable => &[
                HashIoProfilePreference::Conservative,
                HashIoProfilePreference::Balanced,
            ],
            _ => &[
                HashIoProfilePreference::Conservative,
                HashIoProfilePreference::Balanced,
                HashIoProfilePreference::Aggressive,
            ],
        },
        ResourcePressure::Constrained => &[
            HashIoProfilePreference::Conservative,
            HashIoProfilePreference::Balanced,
        ],
        ResourcePressure::Severe => &[HashIoProfilePreference::Conservative],
    };

    let mut profiles = Vec::with_capacity(candidates.len());
    profiles.push(initial_profile);
    for &profile in candidates {
        let (profile, _) = cap_auto_hash_profile(profile, resource_profile);
        if !profiles.contains(&profile) {
            profiles.push(profile);
        }
    }
    profiles
}

fn rotate_benchmark_profiles(profiles: &mut [HashIoProfilePreference], sequence: usize) -> usize {
    if profiles.len() < 2 {
        return 0;
    }
    let rotation = sequence % profiles.len();
    profiles.rotate_left(rotation);
    rotation
}

fn benchmark_wait_ratio(metrics: &HashRunMetrics) -> f64 {
    let wait = metrics.semaphore_wait_elapsed_sum.as_secs_f64();
    let compute = metrics.blocking_hash_elapsed_sum.as_secs_f64();
    let total = wait + compute;
    if total <= f64::EPSILON {
        0.0
    } else {
        wait / total
    }
}

fn benchmark_supports_boosted_aggressive(
    profile: HashIoProfilePreference,
    metrics: &HashRunMetrics,
    storage_class: HashStorageClass,
    resource_profile: ResourceProfile,
) -> bool {
    profile == HashIoProfilePreference::Aggressive
        && storage_class != HashStorageClass::Hdd
        && resource_profile.pressure == ResourcePressure::Normal
        && benchmark_wait_ratio(metrics) <= LOW_WAIT_AGGRESSIVE_THRESHOLD
}

fn select_benchmark_profile(
    initial_profile: HashIoProfilePreference,
    trials: &[(HashIoProfilePreference, f64, bool)],
) -> Option<(HashIoProfilePreference, f64, bool)> {
    let fastest = trials
        .iter()
        .copied()
        .max_by(|left, right| left.1.total_cmp(&right.1))?;
    let Some(initial) = trials
        .iter()
        .copied()
        .find(|(profile, _, _)| *profile == initial_profile)
    else {
        return Some(fastest);
    };
    if fastest.0 == initial_profile {
        return Some(fastest);
    }
    let required = initial.1 * (1.0 + PROFILE_SWITCH_MIN_IMPROVEMENT_PERCENT / 100.0);
    if fastest.1 >= required {
        Some(fastest)
    } else {
        Some(initial)
    }
}

fn log_hash_scheduler_selection(
    label: &str,
    requested_profile: HashIoProfilePreference,
    selected_profile: HashIoProfilePreference,
    limits: &HashSchedulerLimits,
) {
    info!(
        "Hash scheduler selection: label={} requested_profile={} selected_profile={} storage={} file_concurrency={} global_part_concurrency={} reason={}",
        label,
        requested_profile,
        selected_profile,
        limits.storage_class,
        limits.file_concurrency,
        limits.global_part_concurrency,
        limits.reason
    );
}

/// `parts` is the tree's part list, lent by [`lend_tree_parts`].
pub(super) fn build_file_hash_jobs(
    data_tree: &Tree,
    parts: &Arc<Vec<FoxyModFilePart>>,
    file_indices: &[usize],
    span_source: PartSpanSource,
) -> Vec<FileHashJob> {
    let mut jobs = Vec::with_capacity(file_indices.len());
    for &file_idx in file_indices {
        let Some(file_node) = data_tree.file_nodes.get(file_idx) else {
            continue;
        };
        let Some(file) = data_tree.files.get(file_idx) else {
            continue;
        };
        let mut part_indices: Vec<usize> = file_node
            .parts
            .iter()
            .copied()
            .filter(|&part_idx| part_idx < parts.len())
            .collect();
        let has_local_baseline = !file.local_checksum.is_empty()
            || part_indices
                .iter()
                .any(|&part_idx| !parts[part_idx].local_checksum.is_empty());
        part_indices.sort_by_key(|&part_idx| parts[part_idx].data_order);
        jobs.push(FileHashJob {
            file_idx,
            file_path: file.local_path.clone(),
            file_length: file.length,
            file_remote_checksum: file.remote_checksum.clone(),
            indexed_parts: JobParts::shared(parts, part_indices),
            span_source,
            has_local_baseline,
            capture_identity: false,
        });
    }
    jobs
}

/// The storage inputs a file's hash reads depend on, resolved once per run and
/// carried down to every file.
#[derive(Clone, Copy)]
pub(super) struct WholeFileHashIo<'a> {
    storage_class: HashStorageClass,
    repository_root: Option<&'a Path>,
}

impl<'a> WholeFileHashIo<'a> {
    fn new(_profile: HashIoProfilePreference, storage_class: HashStorageClass) -> Self {
        Self {
            storage_class,
            repository_root: None,
        }
    }

    fn with_repository_root(mut self, root: Option<&'a str>) -> Self {
        self.repository_root = root.filter(|root| !root.is_empty()).map(Path::new);
        self
    }

    fn part_reader_capacity(&self) -> usize {
        if self.rotational() {
            super::part_hashes::ROTATIONAL_HASH_READER_CAPACITY
        } else {
            super::part_hashes::HASH_READER_CAPACITY
        }
    }

    fn rotational(&self) -> bool {
        matches!(
            self.storage_class,
            HashStorageClass::Hdd | HashStorageClass::Removable
        )
    }

    /// Existing files on a local disk are read around the cache: one file at a
    /// time on rotational storage, a read ahead of the hasher on SSD.
    fn direct_read_plan(&self) -> Option<DirectReadPlan> {
        if !cfg!(windows) {
            return None;
        }
        match self.storage_class {
            HashStorageClass::Hdd | HashStorageClass::Removable => Some(DirectReadPlan::new(
                ROTATIONAL_DIRECT_BLOCK,
                ROTATIONAL_DIRECT_DEPTH,
                0,
                true,
            )),
            HashStorageClass::Ssd => Some(DirectReadPlan::new(
                SSD_DIRECT_BLOCK,
                SSD_DIRECT_DEPTH,
                SSD_DIRECT_MIN_LEN,
                false,
            )),
            HashStorageClass::Unknown => None,
        }
    }
}

const ROTATIONAL_DIRECT_BLOCK: usize = 16 * 1024 * 1024;
const ROTATIONAL_DIRECT_DEPTH: usize = 4;
const SSD_DIRECT_BLOCK: usize = 4 * 1024 * 1024;
const SSD_DIRECT_DEPTH: usize = 2;
/// Below this a file is one read either way, and a cached copy may still
/// answer it from memory.
const SSD_DIRECT_MIN_LEN: u64 = 1024 * 1024;

async fn calculate_whole_file_checksum_monitored(
    file_path: String,
    expected_checksum: String,
    expected_len: u64,
    semaphore: Arc<Semaphore>,
    progress: Option<PartHashProgress>,
    evidence: Arc<super::evidence::HashEvidenceState>,
    mut cancel_rx: Option<watch::Receiver<bool>>,
) -> (
    Option<String>,
    Option<String>,
    super::part_hashes::PartHashMetrics,
    Option<super::evidence::HashIssueKind>,
    bool,
) {
    const WHOLE_FILE_HASH_BUF_SIZE: usize = 4 * 1024 * 1024;
    let unattended = evidence.is_unattended();

    let started = Instant::now();
    let mut metrics = super::part_hashes::PartHashMetrics {
        estimated_bytes: expected_len,
        ..Default::default()
    };

    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        metrics.total_elapsed = started.elapsed();
        return (
            None,
            None,
            metrics,
            Some(super::evidence::HashIssueKind::Skipped),
            true,
        );
    }

    let metadata_started = Instant::now();
    let metadata = match std::fs::metadata(&file_path) {
        Ok(metadata) => metadata,
        Err(err) => {
            debug!("Whole-file hash skipped for {}: {}", file_path, err);
            metrics.metadata_elapsed = metadata_started.elapsed();
            metrics.total_elapsed = started.elapsed();
            return (
                None,
                None,
                metrics,
                Some(if err.kind() == std::io::ErrorKind::NotFound {
                    super::evidence::HashIssueKind::Missing
                } else {
                    super::evidence::HashIssueKind::Unreadable
                }),
                false,
            );
        }
    };
    metrics.metadata_elapsed = metadata_started.elapsed();

    if !metadata.is_file() || metadata.len() != expected_len {
        debug!(
            "Whole-file hash skipped for {}: is_file={} local_len={} expected_len={}",
            file_path,
            metadata.is_file(),
            metadata.len(),
            expected_len
        );
        metrics.total_elapsed = started.elapsed();
        return (
            None,
            None,
            metrics,
            Some(super::evidence::HashIssueKind::Unreadable),
            false,
        );
    }

    let wait_started = Instant::now();
    let permit = semaphore.acquire_owned().await.ok();
    metrics.semaphore_wait_elapsed = wait_started.elapsed();
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        drop(permit);
        metrics.total_elapsed = started.elapsed();
        return (
            None,
            None,
            metrics,
            Some(super::evidence::HashIssueKind::Skipped),
            true,
        );
    }

    let blocking_started = Instant::now();
    let file_path_for_hash = file_path.clone();
    let read_heartbeat = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let blocking_heartbeat = read_heartbeat.clone();
    let stop_read = Arc::new(AtomicU8::new(0));
    let blocking_stop = stop_read.clone();
    let hashed_bytes = Arc::new(AtomicU64::new(0));
    let blocking_hashed_bytes = hashed_bytes.clone();
    let blocking_progress = progress.clone();
    let task = tokio::task::spawn_blocking(move || -> std::io::Result<(String, Option<String>)> {
        let _permit = permit;
        let profiled = crate::core::utils::profiling::FsTimer::start();
        let mut file = std::fs::File::open(&file_path_for_hash)?;
        let mut hasher = FlexHasher::from_checksum(&expected_checksum);
        let mut fingerprint = crate::core::utils::content_hash::FingerprintTap::new(&metadata);
        let mut buffer = vec![0u8; WHOLE_FILE_HASH_BUF_SIZE];
        let mut read_bytes = 0u64;
        loop {
            if blocking_stop.load(Ordering::Relaxed) != 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            read_bytes += read as u64;
            hasher.update(&buffer[..read]);
            fingerprint.observe(read_bytes - read as u64, &buffer[..read]);
            blocking_hashed_bytes.store(read_bytes, Ordering::Relaxed);
            blocking_heartbeat.fetch_add(1, Ordering::Relaxed);
            if let Some(progress) = &blocking_progress {
                progress.mark_parts_done(0, read as u64);
            }
            crate::core::utils::cpu_governor::checkpoint();
        }
        profiled.stop("hash_read", read_bytes);
        let content_hash = fingerprint.finish();
        Ok((hasher.finalize_hex(), content_hash))
    });

    let mut task = task;
    let mut last_heartbeat = read_heartbeat.load(Ordering::Relaxed);
    let mut last_progress = Instant::now();
    let mut stalled_secs = 0u64;
    let joined = 'worker: loop {
        tokio::select! {
            joined = &mut task => break joined,
            changed = async {
                match cancel_rx.as_mut() {
                    Some(rx) => rx.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_ok() && cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
                    stop_read.store(2, Ordering::Relaxed);
                    break 'worker (&mut task).await;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                let heartbeat = read_heartbeat.load(Ordering::Relaxed);
                if heartbeat != last_heartbeat {
                    last_heartbeat = heartbeat;
                    last_progress = Instant::now();
                    if stalled_secs > 0 {
                        evidence.finish_stalled_read(&file_path);
                        stalled_secs = 0;
                    }
                    continue;
                }
                let idle = last_progress.elapsed().as_secs();
                if unattended {
                    if idle >= 120 { stop_read.store(1, Ordering::Relaxed); }
                    continue;
                }
                if idle >= 60 && stalled_secs == 0 { stalled_secs = idle; }
                if stalled_secs > 0 {
                    let mut decision = evidence.begin_stalled_read(super::evidence::StalledRead {
                        file_path: file_path.clone(),
                        no_progress_secs: idle,
                    });
                    loop {
                        tokio::select! {
                            joined = &mut task => break 'worker joined,
                            changed = decision.changed() => {
                                if changed.is_err() { break; }
                                let response = *decision.borrow_and_update();
                                match response {
                                    super::evidence::StalledReadDecision::Continue => {
                                        evidence.finish_stalled_read(&file_path);
                                        last_progress = Instant::now();
                                        stalled_secs = 0;
                                        break;
                                    }
                                    super::evidence::StalledReadDecision::Skip => {
                                        stop_read.store(1, Ordering::Relaxed);
                                        evidence.finish_stalled_read(&file_path);
                                        break 'worker (&mut task).await;
                                    }
                                    super::evidence::StalledReadDecision::Cancel => {
                                        stop_read.store(2, Ordering::Relaxed);
                                        evidence.finish_stalled_read(&file_path);
                                        break 'worker (&mut task).await;
                                    }
                                    super::evidence::StalledReadDecision::Waiting => {}
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                                let heartbeat = read_heartbeat.load(Ordering::Relaxed);
                                if heartbeat != last_heartbeat {
                                    last_heartbeat = heartbeat;
                                    evidence.finish_stalled_read(&file_path);
                                    last_progress = Instant::now();
                                    stalled_secs = 0;
                                    break;
                                }
                            }
                            changed = async {
                                match cancel_rx.as_mut() {
                                    Some(rx) => rx.changed().await,
                                    None => std::future::pending().await,
                                }
                            } => {
                                if changed.is_ok() && cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
                                    stop_read.store(2, Ordering::Relaxed);
                                    evidence.finish_stalled_read(&file_path);
                                    break 'worker (&mut task).await;
                                }
                            }
                        }
                    }
                }
            }
        }
    };
    evidence.finish_stalled_read(&file_path);
    metrics.blocking_hash_elapsed = blocking_started.elapsed();
    metrics.hashed_bytes = hashed_bytes.load(Ordering::Relaxed);

    let (checksum, content_hash, issue, cancelled) = match joined {
        Ok(Ok((checksum, content_hash))) => (Some(checksum), content_hash, None, false),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::Interrupted => {
            let stopped_by_cancel = stop_read.load(Ordering::Relaxed) == 2;
            (
                None,
                None,
                Some(super::evidence::HashIssueKind::Skipped),
                stopped_by_cancel,
            )
        }
        Ok(Err(err)) => {
            warn!("Whole-file hash failed for {}: {}", file_path, err);
            (
                None,
                None,
                Some(super::evidence::HashIssueKind::Unreadable),
                false,
            )
        }
        Err(err) => {
            error!("Whole-file hash task panicked for {}: {}", file_path, err);
            (
                None,
                None,
                Some(super::evidence::HashIssueKind::Unreadable),
                false,
            )
        }
    };

    metrics.total_elapsed = started.elapsed();
    (checksum, content_hash, metrics, issue, cancelled)
}

pub(super) async fn recalculate_parts_for_jobs(
    mut jobs: Vec<FileHashJob>,
    concurrency: (usize, usize),
    progress_tx: Option<&Sender<ProgressEvent>>,
    progress: HashRunProgress,
    cancel_rx: Option<&watch::Receiver<bool>>,
    hash_io: WholeFileHashIo<'_>,
    evidence: Arc<super::evidence::HashEvidenceState>,
) -> (Vec<FileHashResult>, bool) {
    let (file_concurrency, global_part_concurrency) = concurrency;
    let unattended = evidence.is_unattended();
    let scope_root = hash_io
        .repository_root
        .map(Path::to_path_buf)
        .or_else(|| common_hash_scope_root(&jobs));
    let root_missing = if let Some(root) = &scope_root {
        tokio::fs::metadata(root)
            .await
            .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
    } else {
        false
    };
    if root_missing && jobs.iter().any(|job| job.has_local_baseline) {
        let results = jobs
            .into_iter()
            .map(|job| FileHashResult {
                file_idx: job.file_idx,
                updated_parts: Vec::new(),
                whole_file_checksum: None,
                content_hash: None,
                file_path: job.file_path,
                elapsed: std::time::Duration::ZERO,
                parts_count: job.indexed_parts.len(),
                missing_file: false,
                issue: Some(super::evidence::HashIssueKind::Unreadable),
                completed: false,
                read_identity: None,
                manifest_signature: String::new(),
                part_metrics: Default::default(),
                verified: None,
            })
            .collect();
        return (results, false);
    }
    let scope_root = scope_root.filter(|_| !root_missing);

    // Shared semaphore limits the total in-flight spawn_blocking hash tasks across all files.
    let semaphore = Arc::new(Semaphore::new(global_part_concurrency));
    let game_formats = crate::core::game::registry().active().content_formats();

    let counters = HashRunCounters {
        files_done: Arc::new(AtomicUsize::new(
            progress.initial_files_done.min(progress.total_files),
        )),
        total_files: progress.total_files,
        parts_done: Arc::new(AtomicUsize::new(
            progress.initial_parts_done.min(progress.total_parts),
        )),
        total_parts: progress.total_parts,
        bytes_done: Arc::new(AtomicU64::new(
            progress.initial_bytes_done.min(progress.total_bytes),
        )),
        total_bytes: progress.total_bytes,
    };
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));

    if hash_io.rotational() {
        order_hash_jobs_physically(&mut jobs).await;
    } else {
        order_hash_jobs(&mut jobs);
    }
    let direct_plan = hash_io.direct_read_plan();
    let progress_sender = progress_tx.cloned();
    let cancel_receiver = cancel_rx.cloned();
    let results = stream::iter(jobs.into_iter().map(|job| {
        let sem = semaphore.clone();
        let counters = counters.clone();
        let ptx = progress_sender.clone();
        let job_bytes = job_estimated_bytes(&job);
        let part_bytes: u64 = job
            .indexed_parts
            .iter()
            .map(|(_, part)| part.remote_length)
            .sum();
        let cancel = cancel_receiver.clone();
        let direct_plan = direct_plan.clone();
        let cancelled_flag = cancelled.clone();
        let hash_evidence = evidence.clone();
        let scope_root = scope_root.clone();
        async move {
            let FileHashJob {
                file_idx,
                file_path,
                file_length,
                file_remote_checksum,
                indexed_parts,
                span_source,
                has_local_baseline: _,
                capture_identity,
            } = job;
            let parts_count = indexed_parts.len();
            if cancelled_flag.load(Ordering::Relaxed)
                || cancel.as_ref().is_some_and(|rx| *rx.borrow())
            {
                cancelled_flag.store(true, Ordering::Relaxed);
                counters.bytes_done.fetch_add(job_bytes, Ordering::Relaxed);
                let completed = counters.files_done.fetch_add(1, Ordering::Relaxed) + 1;
                if completed.is_multiple_of(500) || completed == progress.total_files {
                    info!(
                        "Phase 1 progress: {}/{} files hashed (cancelling)",
                        completed, progress.total_files
                    );
                }
                return FileHashResult {
                    file_idx,
                    updated_parts: Vec::new(),
                    whole_file_checksum: None,
                    content_hash: None,
                    file_path,
                    elapsed: std::time::Duration::ZERO,
                    parts_count,
                    missing_file: false,
                    issue: None,
                    completed: false,
                    read_identity: None,
                    manifest_signature: String::new(),
                    part_metrics: Default::default(),
                    verified: None,
                };
            }
            if let Some(root) = &scope_root
                && let Err(err) = tokio::fs::metadata(root).await
                && err.kind() == std::io::ErrorKind::NotFound
            {
                return FileHashResult {
                    file_idx,
                    updated_parts: Vec::new(),
                    whole_file_checksum: None,
                    content_hash: None,
                    file_path,
                    elapsed: std::time::Duration::ZERO,
                    parts_count,
                    missing_file: false,
                    issue: Some(super::evidence::HashIssueKind::Unreadable),
                    completed: false,
                    read_identity: None,
                    manifest_signature: String::new(),
                    part_metrics: Default::default(),
                    verified: None,
                };
            }
            let file_started = Instant::now();
            let metadata_issue = match tokio::fs::metadata(&file_path).await {
                Ok(_) => None,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    Some(super::evidence::HashIssueKind::Missing)
                }
                Err(_) => Some(super::evidence::HashIssueKind::Unreadable),
            };
            let missing_file = metadata_issue == Some(super::evidence::HashIssueKind::Missing);
            let identity_before = super::verified_record::capture_identity_async(&file_path).await;
            let record_check = capture_identity
                .then(|| {
                    identity_before.clone().map(|identity| {
                        (
                            identity,
                            super::verified_record::parts_signature(
                                &file_remote_checksum,
                                indexed_parts.parts(),
                            ),
                            file_remote_checksum.clone(),
                        )
                    })
                })
                .flatten();
            let manifest_signature = super::verified_record::parts_signature(
                &file_remote_checksum,
                indexed_parts.parts(),
            );
            let part_progress = Some(PartHashProgress::new(
                counters.clone(),
                ptx.clone(),
                hash_evidence.clone(),
                unattended,
            ));
            let (part_calculation, whole_file_checksum) =
                if indexed_parts.is_empty() && !file_remote_checksum.is_empty() {
                    let (checksum, content_hash, metrics, issue, cancelled) =
                        calculate_whole_file_checksum_monitored(
                            file_path.clone(),
                            file_remote_checksum.clone(),
                            file_length,
                            sem,
                            part_progress.clone(),
                            hash_evidence.clone(),
                            cancel.clone(),
                        )
                        .await;
                    (
                        super::part_hashes::PartHashCalculation {
                            parts: Vec::new(),
                            metrics,
                            content_hash,
                            issue,
                            cancelled,
                        },
                        checksum,
                    )
                } else {
                    let parts_only: Vec<FoxyModFilePart> =
                        indexed_parts.iter().map(|(_, p)| p.clone()).collect();
                    (
                        calculate_part_hashes(
                            parts_only,
                            &file_path,
                            sem,
                            PartReadOptions {
                                span_source,
                                game_formats,
                                reader_capacity: hash_io.part_reader_capacity(),
                                sequential_scan: hash_io.rotational(),
                                direct: direct_plan
                                    .as_ref()
                                    .filter(|_| span_source == PartSpanSource::DetectLocalLayout),
                            },
                            part_progress,
                            cancel.clone(),
                        )
                        .await,
                        None,
                    )
                };
            let file_elapsed = file_started.elapsed();
            let content_hash = part_calculation.content_hash.clone();
            if part_calculation.cancelled {
                cancelled_flag.store(true, Ordering::Relaxed);
            }
            let issue = part_calculation.issue.or_else(|| {
                if indexed_parts.is_empty()
                    && whole_file_checksum.is_none()
                    && !file_remote_checksum.is_empty()
                {
                    metadata_issue.or(Some(super::evidence::HashIssueKind::Unreadable))
                } else {
                    metadata_issue
                }
            });
            let mut updated_parts = if cancel.as_ref().is_some_and(|rx| *rx.borrow()) {
                cancelled_flag.store(true, Ordering::Relaxed);
                Vec::new()
            } else {
                indexed_parts
                    .indices()
                    .iter()
                    .copied()
                    .zip(part_calculation.parts)
                    .collect()
            };
            let identity_after = super::verified_record::capture_identity_async(&file_path).await;
            let root_disappeared = if let Some(root) = &scope_root {
                tokio::fs::metadata(root)
                    .await
                    .is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
            } else {
                false
            };
            let stable = identity_before.is_some() && identity_before == identity_after;
            let changed_during_read =
                identity_before.is_some() && identity_after.is_some() && !stable;
            let issue = if root_disappeared {
                updated_parts.iter_mut().for_each(|(_, part)| {
                    part.local_checksum.clear();
                    part.local_length = 0;
                    part.local_start = 0;
                });
                Some(super::evidence::HashIssueKind::Unreadable)
            } else if changed_during_read || (!stable && issue.is_none()) {
                updated_parts.iter_mut().for_each(|(_, part)| {
                    part.local_checksum.clear();
                    part.local_length = 0;
                    part.local_start = 0;
                });
                Some(if changed_during_read {
                    super::evidence::HashIssueKind::Unstable
                } else {
                    super::evidence::HashIssueKind::Unreadable
                })
            } else {
                issue
            };
            let verified = match record_check {
                Some((before, signature, remote_checksum))
                    if stable
                        && super::verified_record::result_is_clean(
                            &remote_checksum,
                            &updated_parts,
                            whole_file_checksum.as_deref(),
                        ) =>
                {
                    Some(super::verified_record::VerifiedFile {
                        identity: before,
                        signature,
                    })
                }
                _ => None,
            };

            // Parts report their own bytes; whole-file jobs and part lists
            // without lengths are counted here, once the file is done.
            let part_reported = if indexed_parts.is_empty() {
                part_calculation.metrics.hashed_bytes
            } else if ptx.is_some() {
                part_bytes
            } else {
                0
            };
            counters
                .bytes_done
                .fetch_add(job_bytes.saturating_sub(part_reported), Ordering::Relaxed);
            let completed = counters.files_done.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(ref tx) = ptx {
                let _ = tx.send(counters.event());
            }
            if completed.is_multiple_of(500) || completed == progress.total_files {
                info!(
                    "Phase 1 progress: {}/{} files hashed",
                    completed, progress.total_files
                );
            }

            let read_identity = stable.then_some(identity_before).flatten();
            if let Some(identity) = &read_identity {
                hash_evidence.record_retry_file(
                    &file_path,
                    Some(identity.clone()),
                    manifest_signature.clone(),
                );
            } else if issue == Some(super::evidence::HashIssueKind::Missing) {
                hash_evidence.record_retry_file(&file_path, None, manifest_signature.clone());
            }
            FileHashResult {
                file_idx,
                updated_parts,
                whole_file_checksum: if stable { whole_file_checksum } else { None },
                content_hash: if stable { content_hash } else { None },
                file_path,
                elapsed: file_elapsed,
                parts_count,
                missing_file,
                issue,
                completed: true,
                read_identity,
                manifest_signature,
                part_metrics: part_calculation.metrics,
                verified,
            }
        }
    }))
    .buffer_unordered(file_concurrency.max(1))
    .collect::<Vec<_>>()
    .await;
    let was_cancelled =
        cancelled.load(Ordering::Relaxed) || cancel_rx.as_ref().is_some_and(|rx| *rx.borrow());
    (results, was_cancelled)
}

/// Below this size a file costs about as much in seeking as in reading on a
/// rotational disk.
const SMALL_HASH_JOB_BYTES: u64 = 16 * 1024 * 1024;

/// Heavy files first so big archives are not starved behind many tiny files,
/// then the small-file tail in path order, which keeps the files of one addon
/// (downloaded together) next to each other on disk.
fn order_hash_jobs(jobs: &mut [FileHashJob]) {
    jobs.sort_by_cached_key(|job| {
        if job_estimated_bytes(job) >= SMALL_HASH_JOB_BYTES {
            (false, Reverse(job.indexed_parts.len()), String::new())
        } else {
            (true, Reverse(0), job.file_path.to_ascii_lowercase())
        }
    });
}

/// Rotational storage reads jobs in on-disk order, one sweep of the platter,
/// so the two workers read neighbouring files and every move of the head is
/// short. Resident and unlocatable files follow in path order.
async fn order_hash_jobs_physically(jobs: &mut Vec<FileHashJob>) {
    let started = Instant::now();
    let paths: Vec<String> = jobs.iter().map(|job| job.file_path.clone()).collect();
    let clusters = tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .map(|path| super::physical_order::first_cluster(Path::new(path)))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    if clusters.len() != jobs.len() {
        order_hash_jobs(jobs);
        return;
    }
    let located =
        super::physical_order::sort_by_first_cluster(jobs, clusters, |job| job.file_path.as_str());
    info!(
        "Hash job physical order: files={} located={} elapsed={:.3}s",
        jobs.len(),
        located,
        started.elapsed().as_secs_f64()
    );
}

fn job_estimated_bytes(job: &FileHashJob) -> u64 {
    let part_bytes: u64 = job
        .indexed_parts
        .iter()
        .map(|(_, part)| part.remote_length)
        .sum();
    if part_bytes == 0 {
        job.file_length
    } else {
        part_bytes
    }
}

pub(super) fn missing_local_hash_pass_is_noop(data_tree: &Tree, file_indices: &[usize]) -> bool {
    if file_indices.is_empty() {
        return false;
    }

    file_indices.iter().all(|&file_idx| {
        let Some(file) = data_tree.files.get(file_idx) else {
            return false;
        };
        if std::fs::metadata(&file.local_path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
        {
            return false;
        }
        if !file.local_checksum.is_empty() {
            return false;
        }
        let Some(file_node) = data_tree.file_nodes.get(file_idx) else {
            return true;
        };
        file_node.parts.iter().all(|&part_idx| {
            data_tree.parts.get(part_idx).is_none_or(|part| {
                part.local_checksum.is_empty() && part.local_length == 0 && part.local_start == 0
            })
        })
    })
}

const MAX_BENCHMARK_FILES_PER_GROUP: usize = 12;
const MAX_BENCHMARK_BYTES_PER_GROUP: u64 = 512 * 1024 * 1024;

/// Take the calibration sample out of `jobs`: enough files and bytes for
/// `groups` disjoint trial groups, so every trial hashes its own data and no
/// byte is hashed twice or read warm from a previous trial.
fn split_benchmark_jobs(jobs: &mut Vec<FileHashJob>, groups: usize) -> Vec<FileHashJob> {
    let groups = groups.max(1);
    if jobs.len() < MIN_AUTO_BENCHMARK_FILES {
        return Vec::new();
    }

    // Layout-heavy PBOs can dominate real hashing time even when they are not
    // the largest files. Prefer those first, then fill the byte budget.
    jobs.sort_by_key(|job| Reverse((job.indexed_parts.len(), job_estimated_bytes(job))));
    let min_files = MIN_AUTO_BENCHMARK_FILES.saturating_mul(groups);
    let max_files = MAX_BENCHMARK_FILES_PER_GROUP.saturating_mul(groups);
    let max_bytes = MAX_BENCHMARK_BYTES_PER_GROUP.saturating_mul(groups as u64);
    let take_count = jobs.len().min(max_files);
    let mut selected = Vec::new();
    let mut selected_bytes = 0u64;
    for _ in 0..take_count {
        if jobs.is_empty() {
            break;
        }
        let job = jobs.remove(0);
        selected_bytes = selected_bytes.saturating_add(job_estimated_bytes(&job));
        selected.push(job);
        if selected.len() >= min_files && selected_bytes >= max_bytes {
            break;
        }
    }
    selected
}

fn benchmark_sample_is_sufficient(benchmark_jobs: &[FileHashJob]) -> bool {
    benchmark_jobs.len() >= MIN_AUTO_BENCHMARK_FILES
        && benchmark_jobs.iter().map(job_estimated_bytes).sum::<u64>() >= MIN_AUTO_BENCHMARK_BYTES
}

fn benchmark_group_load_score(group: &[FileHashJob], totals: (u64, usize, usize)) -> u128 {
    let (total_bytes, total_parts, total_files) = totals;
    let bytes = group.iter().map(job_estimated_bytes).sum::<u64>() as u128;
    let parts = group
        .iter()
        .map(|job| job.indexed_parts.len())
        .sum::<usize>() as u128;
    let files = group.len() as u128;
    let total_bytes = total_bytes.max(1) as u128;
    let total_parts = total_parts.max(1) as u128;
    let total_files = total_files.max(1) as u128;
    bytes * total_parts * total_files
        + parts * total_bytes * total_files
        + files * total_bytes * total_parts
}

/// Deal the sample into disjoint trial groups balanced by bytes, parts and
/// file count. Groups that would be too small to judge are folded back until
/// every remaining group is sufficient; the result has at least one group.
fn deal_benchmark_groups(sample: Vec<FileHashJob>, groups: usize) -> Vec<Vec<FileHashJob>> {
    let mut groups = groups.clamp(1, sample.len().max(1));
    let totals = (
        sample.iter().map(job_estimated_bytes).sum(),
        sample.iter().map(|job| job.indexed_parts.len()).sum(),
        sample.len(),
    );
    loop {
        let mut dealt: Vec<Vec<FileHashJob>> = (0..groups).map(|_| Vec::new()).collect();
        for job in &sample {
            let index = dealt
                .iter()
                .enumerate()
                .min_by_key(|(index, group)| (benchmark_group_load_score(group, totals), *index))
                .map(|(index, _)| index)
                .unwrap_or(0);
            dealt[index].push(job.clone());
        }
        if groups == 1
            || dealt
                .iter()
                .all(|group| benchmark_sample_is_sufficient(group))
        {
            return dealt;
        }
        groups -= 1;
    }
}

#[derive(Default)]
struct HashRunMetrics {
    files: usize,
    missing_files: usize,
    parts: usize,
    estimated_bytes: u64,
    hashed_bytes: u64,
    file_elapsed_sum: std::time::Duration,
    file_elapsed_max: std::time::Duration,
    metadata_elapsed_sum: std::time::Duration,
    layout_elapsed_sum: std::time::Duration,
    layout_parse_elapsed_sum: std::time::Duration,
    layout_map_elapsed_sum: std::time::Duration,
    semaphore_wait_elapsed_sum: std::time::Duration,
    blocking_hash_elapsed_sum: std::time::Duration,
    part_elapsed_sum: std::time::Duration,
    layout_files: usize,
    remote_span_files: usize,
    layout_entries: usize,
    layout_entry_payload_bytes: u64,
    mapped_parts: usize,
    fallback_parts: usize,
    direct_files: usize,
    direct_fallback_files: usize,
}

impl HashRunMetrics {
    fn from_results(results: &[FileHashResult]) -> Self {
        let mut metrics = Self::default();
        for result in results {
            metrics.files += 1;
            metrics.parts += result.parts_count;
            metrics.missing_files += usize::from(result.missing_file);
            metrics.file_elapsed_sum += result.elapsed;
            metrics.file_elapsed_max = metrics.file_elapsed_max.max(result.elapsed);
            metrics.estimated_bytes += result.part_metrics.estimated_bytes;
            metrics.hashed_bytes += result.part_metrics.hashed_bytes;
            metrics.metadata_elapsed_sum += result.part_metrics.metadata_elapsed;
            metrics.layout_elapsed_sum += result.part_metrics.layout_elapsed;
            metrics.layout_parse_elapsed_sum += result.part_metrics.layout_parse_elapsed;
            metrics.layout_map_elapsed_sum += result.part_metrics.layout_map_elapsed;
            metrics.semaphore_wait_elapsed_sum += result.part_metrics.semaphore_wait_elapsed;
            metrics.blocking_hash_elapsed_sum += result.part_metrics.blocking_hash_elapsed;
            metrics.part_elapsed_sum += result.part_metrics.total_elapsed;
            metrics.layout_files += result.part_metrics.layout_files;
            metrics.remote_span_files += result.part_metrics.remote_span_files;
            metrics.layout_entries += result.part_metrics.layout_entries;
            metrics.layout_entry_payload_bytes += result.part_metrics.layout_entry_payload_bytes;
            metrics.mapped_parts += result.part_metrics.mapped_parts;
            metrics.fallback_parts += result.part_metrics.fallback_parts;
            metrics.direct_files += result.part_metrics.direct_files;
            metrics.direct_fallback_files += result.part_metrics.direct_fallback_files;
        }
        metrics
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AddonHashMetrics {
    pub(crate) label: String,
    pub(crate) addon: String,
    pub(crate) files: usize,
    pub(crate) missing_files: usize,
    pub(crate) parts: usize,
    pub(crate) estimated_bytes: u64,
    pub(crate) hashed_bytes: u64,
    pub(crate) part_elapsed_sum: std::time::Duration,
    pub(crate) file_elapsed_max: std::time::Duration,
    pub(crate) metadata_elapsed_sum: std::time::Duration,
    pub(crate) layout_elapsed_sum: std::time::Duration,
    pub(crate) layout_parse_elapsed_sum: std::time::Duration,
    pub(crate) layout_map_elapsed_sum: std::time::Duration,
    pub(crate) blocking_hash_elapsed_sum: std::time::Duration,
    pub(crate) semaphore_wait_elapsed_sum: std::time::Duration,
    pub(crate) layout_files: usize,
    pub(crate) remote_span_files: usize,
    pub(crate) layout_entries: usize,
    pub(crate) mapped_parts: usize,
    pub(crate) fallback_parts: usize,
}

impl AddonHashMetrics {
    fn from_run(label: &str, addon: &str, metrics: &HashRunMetrics) -> Self {
        Self {
            label: label.to_owned(),
            addon: addon.to_owned(),
            files: metrics.files,
            missing_files: metrics.missing_files,
            parts: metrics.parts,
            estimated_bytes: metrics.estimated_bytes,
            hashed_bytes: metrics.hashed_bytes,
            part_elapsed_sum: metrics.part_elapsed_sum,
            file_elapsed_max: metrics.file_elapsed_max,
            metadata_elapsed_sum: metrics.metadata_elapsed_sum,
            layout_elapsed_sum: metrics.layout_elapsed_sum,
            layout_parse_elapsed_sum: metrics.layout_parse_elapsed_sum,
            layout_map_elapsed_sum: metrics.layout_map_elapsed_sum,
            blocking_hash_elapsed_sum: metrics.blocking_hash_elapsed_sum,
            semaphore_wait_elapsed_sum: metrics.semaphore_wait_elapsed_sum,
            layout_files: metrics.layout_files,
            remote_span_files: metrics.remote_span_files,
            layout_entries: metrics.layout_entries,
            mapped_parts: metrics.mapped_parts,
            fallback_parts: metrics.fallback_parts,
        }
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        self.files += other.files;
        self.missing_files += other.missing_files;
        self.parts += other.parts;
        self.estimated_bytes = self.estimated_bytes.saturating_add(other.estimated_bytes);
        self.hashed_bytes = self.hashed_bytes.saturating_add(other.hashed_bytes);
        self.part_elapsed_sum += other.part_elapsed_sum;
        self.file_elapsed_max = self.file_elapsed_max.max(other.file_elapsed_max);
        self.metadata_elapsed_sum += other.metadata_elapsed_sum;
        self.layout_elapsed_sum += other.layout_elapsed_sum;
        self.layout_parse_elapsed_sum += other.layout_parse_elapsed_sum;
        self.layout_map_elapsed_sum += other.layout_map_elapsed_sum;
        self.blocking_hash_elapsed_sum += other.blocking_hash_elapsed_sum;
        self.semaphore_wait_elapsed_sum += other.semaphore_wait_elapsed_sum;
        self.layout_files += other.layout_files;
        self.remote_span_files += other.remote_span_files;
        self.layout_entries += other.layout_entries;
        self.mapped_parts += other.mapped_parts;
        self.fallback_parts += other.fallback_parts;
    }
}

fn benchmark_metrics_are_sufficient(metrics: &HashRunMetrics) -> bool {
    metrics.files >= MIN_AUTO_BENCHMARK_FILES
        && metrics.missing_files == 0
        && metrics.hashed_bytes >= MIN_AUTO_BENCHMARK_BYTES
}

/// The checksum algorithm the jobs verify against, for the `SOL op=hash`
/// line: a BLAKE3 pass and an MD5 pass have different compute references.
fn hash_algorithm_label(jobs: &[FileHashJob]) -> &'static str {
    let mut blake3 = false;
    let mut md5 = false;
    for checksum in jobs.iter().flat_map(|job| {
        std::iter::once(job.file_remote_checksum.as_str()).chain(
            job.indexed_parts
                .iter()
                .map(|(_, part)| part.remote_checksum.as_str()),
        )
    }) {
        if checksum.is_empty() {
            continue;
        }
        if is_blake3_checksum(checksum) {
            blake3 = true;
        } else {
            md5 = true;
        }
        if blake3 && md5 {
            return "mixed";
        }
    }
    match (blake3, md5) {
        (true, false) => "blake3",
        (false, true) => "md5",
        (true, true) => "mixed",
        (false, false) => "unknown",
    }
}

fn log_hash_run_metrics(
    label: &str,
    selected_profile: HashIoProfilePreference,
    wall_elapsed: std::time::Duration,
    results: &[FileHashResult],
    operation_id: Option<&str>,
    algorithm: &str,
    cancelled: bool,
) {
    let metrics = HashRunMetrics::from_results(results);
    info!(
        "Hash part run metrics: label={} profile={} wall={:.3}s files={} missing_files={} parts={} estimated_bytes={} hashed_bytes={} file_elapsed_sum={:.3}s file_elapsed_max={:.3}s part_total_sum={:.3}s metadata_sum={:.3}s layout_sum={:.3}s layout_parse_sum={:.3}s layout_map_sum={:.3}s semaphore_wait_sum={:.3}s blocking_hash_sum={:.3}s layout_files={} remote_span_files={} layout_entries={} layout_entry_payload_bytes={} mapped_parts={} fallback_parts={} direct_files={} direct_fallback_files={}",
        label,
        selected_profile,
        wall_elapsed.as_secs_f64(),
        metrics.files,
        metrics.missing_files,
        metrics.parts,
        metrics.estimated_bytes,
        metrics.hashed_bytes,
        metrics.file_elapsed_sum.as_secs_f64(),
        metrics.file_elapsed_max.as_secs_f64(),
        metrics.part_elapsed_sum.as_secs_f64(),
        metrics.metadata_elapsed_sum.as_secs_f64(),
        metrics.layout_elapsed_sum.as_secs_f64(),
        metrics.layout_parse_elapsed_sum.as_secs_f64(),
        metrics.layout_map_elapsed_sum.as_secs_f64(),
        metrics.semaphore_wait_elapsed_sum.as_secs_f64(),
        metrics.blocking_hash_elapsed_sum.as_secs_f64(),
        metrics.layout_files,
        metrics.remote_span_files,
        metrics.layout_entries,
        metrics.layout_entry_payload_bytes,
        metrics.mapped_parts,
        metrics.fallback_parts,
        metrics.direct_files,
        metrics.direct_fallback_files
    );
    // Speed-of-light accounting (see conventions/SPEED_OF_LIGHT.md, O3).
    // No absolute light is computed in-app; compare this rate to the best
    // demonstrated hash run for the same storage path. `compute_s` and
    // `wait_s` are legacy names: they are the summed blocking-task elapsed
    // (reads and scheduling included, not CPU service) and the summed
    // semaphore wait, repeated under their accurate names.
    let blocking_s = format!("{:.3}", metrics.blocking_hash_elapsed_sum.as_secs_f64());
    let permit_wait_s = format!("{:.3}", metrics.semaphore_wait_elapsed_sum.as_secs_f64());
    let mut extras = vec![
        ("label", label.to_string()),
        ("files", metrics.files.to_string()),
        ("parts", metrics.parts.to_string()),
        ("compute_s", blocking_s.clone()),
        ("wait_s", permit_wait_s.clone()),
        ("blocking_elapsed_s", blocking_s),
        ("permit_wait_s", permit_wait_s),
        (
            "file_elapsed_max_s",
            format!("{:.3}", metrics.file_elapsed_max.as_secs_f64()),
        ),
        ("missing_files", metrics.missing_files.to_string()),
        ("profile", selected_profile.to_string()),
        ("algorithm", algorithm.to_string()),
        ("timer_scope", "batch_wall".to_string()),
        (
            "outcome",
            if cancelled { "cancelled" } else { "completed" }.to_string(),
        ),
    ];
    extras.extend(op_id_extra(operation_id));
    info!(
        "{}",
        sol_line(
            "hash",
            metrics.hashed_bytes,
            wall_elapsed,
            &SolLight::SelfBaseline,
            &extras,
        )
    );
}

pub(super) fn collect_addon_hash_metrics(
    label: &str,
    data_tree: &Tree,
    results: &[FileHashResult],
) -> Vec<AddonHashMetrics> {
    let results_by_file: HashMap<usize, &FileHashResult> = results
        .iter()
        .map(|result| (result.file_idx, result))
        .collect();

    let mut addon_metrics = Vec::new();
    for mod_node in &data_tree.mod_nodes {
        let mut metrics = HashRunMetrics::default();
        for file_idx in &mod_node.files {
            if let Some(result) = results_by_file.get(file_idx) {
                metrics.files += 1;
                metrics.parts += result.parts_count;
                metrics.missing_files += usize::from(result.missing_file);
                metrics.file_elapsed_sum += result.elapsed;
                metrics.file_elapsed_max = metrics.file_elapsed_max.max(result.elapsed);
                metrics.estimated_bytes += result.part_metrics.estimated_bytes;
                metrics.hashed_bytes += result.part_metrics.hashed_bytes;
                metrics.metadata_elapsed_sum += result.part_metrics.metadata_elapsed;
                metrics.layout_elapsed_sum += result.part_metrics.layout_elapsed;
                metrics.layout_parse_elapsed_sum += result.part_metrics.layout_parse_elapsed;
                metrics.layout_map_elapsed_sum += result.part_metrics.layout_map_elapsed;
                metrics.semaphore_wait_elapsed_sum += result.part_metrics.semaphore_wait_elapsed;
                metrics.blocking_hash_elapsed_sum += result.part_metrics.blocking_hash_elapsed;
                metrics.part_elapsed_sum += result.part_metrics.total_elapsed;
                metrics.layout_files += result.part_metrics.layout_files;
                metrics.remote_span_files += result.part_metrics.remote_span_files;
                metrics.layout_entries += result.part_metrics.layout_entries;
                metrics.layout_entry_payload_bytes +=
                    result.part_metrics.layout_entry_payload_bytes;
                metrics.mapped_parts += result.part_metrics.mapped_parts;
                metrics.fallback_parts += result.part_metrics.fallback_parts;
            }
        }
        if metrics.files == 0 {
            continue;
        }
        let addon = data_tree
            .mods
            .get(mod_node.mod_idx)
            .map(|addon| addon.name.as_str())
            .unwrap_or("<unknown>");
        addon_metrics.push(AddonHashMetrics::from_run(label, addon, &metrics));
    }
    addon_metrics
}

pub(super) fn log_addon_hash_metrics(label: &str, data_tree: &Tree, results: &[FileHashResult]) {
    for metrics in collect_addon_hash_metrics(label, data_tree, results) {
        info!(
            "Hash addon metrics: label={} addon={} files={} missing_files={} parts={} estimated_bytes={} hashed_bytes={} part_total_sum={:.3}s file_elapsed_max={:.3}s metadata_sum={:.3}s layout_sum={:.3}s layout_parse_sum={:.3}s layout_map_sum={:.3}s blocking_hash_sum={:.3}s semaphore_wait_sum={:.3}s layout_files={} remote_span_files={} layout_entries={} mapped_parts={} fallback_parts={}",
            metrics.label,
            metrics.addon,
            metrics.files,
            metrics.missing_files,
            metrics.parts,
            metrics.estimated_bytes,
            metrics.hashed_bytes,
            metrics.part_elapsed_sum.as_secs_f64(),
            metrics.file_elapsed_max.as_secs_f64(),
            metrics.metadata_elapsed_sum.as_secs_f64(),
            metrics.layout_elapsed_sum.as_secs_f64(),
            metrics.layout_parse_elapsed_sum.as_secs_f64(),
            metrics.layout_map_elapsed_sum.as_secs_f64(),
            metrics.blocking_hash_elapsed_sum.as_secs_f64(),
            metrics.semaphore_wait_elapsed_sum.as_secs_f64(),
            metrics.layout_files,
            metrics.remote_span_files,
            metrics.layout_entries,
            metrics.mapped_parts,
            metrics.fallback_parts
        );
    }
}

/// Hashes `jobs`, restoring from the operation's verified-hash record the
/// files it proves untouched, and refreshing the record from what was read.
pub(super) async fn recalculate_parts_for_jobs_with_profile(
    mut jobs: Vec<FileHashJob>,
    context: &FoxyContext,
    requested_profile: HashIoProfilePreference,
    sticky_auto_profile: Option<HashIoProfilePreference>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    total_files: usize,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> (Vec<FileHashResult>, HashProfileDecision, bool) {
    for job in &mut jobs {
        match context.hash_target_path(&job.file_path) {
            Ok(path) => job.file_path = path,
            Err(detail) => {
                context
                    .hash_evidence
                    .record_issue(super::evidence::HashIssue {
                        file_path: job.file_path.clone(),
                        kind: super::evidence::HashIssueKind::Unstable,
                        detail: Some(detail),
                    });
                return (
                    Vec::new(),
                    HashProfileDecision::manual(requested_profile),
                    false,
                );
            }
        }
    }
    let retry_jobs = jobs.clone();
    let record = context.verified_hash_record.clone();
    let mut restored = Vec::new();
    let (mut results, decision, cancelled) = if let Some(record) = &record {
        for job in &mut jobs {
            job.capture_identity = true;
        }
        let (restored_files, remaining) = super::verified_record::restore(record, jobs).await;
        restored = restored_files;
        if remaining.is_empty() && !restored.is_empty() {
            (
                Vec::new(),
                HashProfileDecision {
                    reason: "every file restored from the verified-hash record".to_string(),
                    sticky: false,
                    ..HashProfileDecision::manual(requested_profile)
                },
                false,
            )
        } else {
            hash_jobs_with_profile(
                remaining,
                context,
                requested_profile,
                sticky_auto_profile,
                progress_tx,
                total_files.saturating_sub(restored.len()),
                cancel_rx,
            )
            .await
        }
    } else {
        hash_jobs_with_profile(
            jobs,
            context,
            requested_profile,
            sticky_auto_profile,
            progress_tx,
            total_files,
            cancel_rx,
        )
        .await
    };
    if !cancelled && !cancel_rx.is_some_and(|rx| *rx.borrow()) {
        let unstable: HashSet<usize> = results
            .iter()
            .filter(|result| result.issue == Some(super::evidence::HashIssueKind::Unstable))
            .map(|result| result.file_idx)
            .collect();
        if !unstable.is_empty() {
            let retry: Vec<_> = retry_jobs
                .into_iter()
                .filter(|job| unstable.contains(&job.file_idx))
                .collect();
            let (retried, _, retry_cancelled) = hash_jobs_with_profile(
                retry,
                context,
                requested_profile,
                sticky_auto_profile,
                progress_tx,
                unstable.len(),
                cancel_rx,
            )
            .await;
            if !retry_cancelled {
                let mut retried: HashMap<usize, FileHashResult> = retried
                    .into_iter()
                    .map(|result| (result.file_idx, result))
                    .collect();
                for result in &mut results {
                    if let Some(retried_result) = retried.remove(&result.file_idx) {
                        *result = retried_result;
                    }
                }
            }
        }
    }
    if !cancelled {
        if let Some(record) = &record {
            super::verified_record::update(record, restored.iter().chain(&results)).await;
        }
    } else if let Some(record) = &record {
        super::verified_record::retain_completed(record, results.iter()).await;
    }
    for result in &restored {
        if let Some(identity) = &result.read_identity {
            context.hash_evidence.record_retry_file(
                &result.file_path,
                Some(identity.clone()),
                result.manifest_signature.clone(),
            );
        }
    }
    results.append(&mut restored);
    (results, decision, cancelled)
}

async fn hash_jobs_with_profile(
    mut jobs: Vec<FileHashJob>,
    context: &FoxyContext,
    requested_profile: HashIoProfilePreference,
    sticky_auto_profile: Option<HashIoProfilePreference>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    total_files: usize,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> (Vec<FileHashResult>, HashProfileDecision, bool) {
    let operation_id = context.operation_id();
    let total_parts: usize = jobs.iter().map(|job| job.indexed_parts.len()).sum();
    let total_bytes: u64 = jobs.iter().map(job_estimated_bytes).sum();
    let algorithm = hash_algorithm_label(&jobs);
    let resource_profile = ResourceProfile::sample();
    let storage_class = detect_hash_storage_class(&jobs);
    info!(
        "Hash run power: {}",
        crate::core::utils::power::recent_sample().summary()
    );
    super::storage_probe::log_storage_read_measurement(
        context,
        &jobs,
        storage_class,
        requested_profile,
        cancel_rx,
    )
    .await;
    if resource_profile.pressure != ResourcePressure::Normal {
        info!(
            "Hash scheduler resource pressure detected: {}",
            resource_profile.summary()
        );
    }
    if requested_profile == HashIoProfilePreference::Auto
        && let Some(profile) = sticky_auto_profile
    {
        let (profile, cap_reason) = if matches!(
            storage_class,
            HashStorageClass::Hdd | HashStorageClass::Removable
        ) && profile == HashIoProfilePreference::Aggressive
        {
            let (storage_profile, storage_reason) = auto_hash_profile_for_environment(
                jobs.len(),
                total_parts,
                resource_profile,
                storage_class,
            );
            (
                storage_profile,
                Some(format!(
                    "sticky auto aggressive capped by storage heuristic: {storage_reason}"
                )),
            )
        } else {
            cap_auto_hash_profile(profile, resource_profile)
        };
        let limits = hash_scheduler_limits_for_environment(
            jobs.len(),
            total_parts,
            profile,
            resource_profile,
            storage_class,
        );
        log_hash_scheduler_selection("sticky_auto", requested_profile, profile, &limits);
        let run_started = Instant::now();
        let results = recalculate_parts_for_jobs(
            jobs,
            (limits.file_concurrency, limits.global_part_concurrency),
            progress_tx,
            HashRunProgress::new(total_files, total_parts, total_bytes),
            cancel_rx,
            WholeFileHashIo::new(profile, storage_class)
                .with_repository_root(context.target_local_path.as_deref()),
            context.hash_evidence.clone(),
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics(
            "sticky_auto",
            profile,
            run_started.elapsed(),
            &results,
            operation_id,
            algorithm,
            cancelled,
        );
        let mut decision = HashProfileDecision::sticky_auto(profile);
        if let Some(reason) = cap_reason {
            decision.reason = reason;
        }
        return (results, decision, cancelled);
    }

    if requested_profile != HashIoProfilePreference::Auto {
        let effective_profile = if resource_profile.pressure == ResourcePressure::Severe
            && requested_profile == HashIoProfilePreference::Aggressive
        {
            warn!(
                "Manual aggressive hash profile reduced to balanced due to severe resource pressure: {}",
                resource_profile.summary()
            );
            HashIoProfilePreference::Balanced
        } else {
            requested_profile
        };
        let limits = hash_scheduler_limits_for_environment(
            jobs.len(),
            total_parts,
            effective_profile,
            resource_profile,
            storage_class,
        );
        log_hash_scheduler_selection(
            "manual_profile",
            requested_profile,
            effective_profile,
            &limits,
        );
        let run_started = Instant::now();
        let results = recalculate_parts_for_jobs(
            jobs,
            (limits.file_concurrency, limits.global_part_concurrency),
            progress_tx,
            HashRunProgress::new(total_files, total_parts, total_bytes),
            cancel_rx,
            WholeFileHashIo::new(effective_profile, storage_class)
                .with_repository_root(context.target_local_path.as_deref()),
            context.hash_evidence.clone(),
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics(
            "manual_profile",
            effective_profile,
            run_started.elapsed(),
            &results,
            operation_id,
            algorithm,
            cancelled,
        );
        let mut decision = HashProfileDecision::manual(effective_profile);
        if effective_profile != requested_profile {
            decision.reason = format!(
                "manual aggressive reduced by resource pressure: {}",
                resource_profile.summary()
            );
        }
        return (results, decision, cancelled);
    }

    let (initial_profile, initial_reason) =
        auto_hash_profile_for_environment(jobs.len(), total_parts, resource_profile, storage_class);
    let initial_limits = hash_scheduler_limits_for_environment(
        jobs.len(),
        total_parts,
        initial_profile,
        resource_profile,
        storage_class,
    );
    info!(
        "Hash profile auto initial: selected={} remaining_files={} limits={}/{} storage={} reason={}",
        initial_profile,
        jobs.len(),
        initial_limits.file_concurrency,
        initial_limits.global_part_concurrency,
        storage_class,
        initial_reason
    );
    log_hash_scheduler_selection(
        "auto_initial",
        requested_profile,
        initial_profile,
        &initial_limits,
    );

    let benchmark_jobs = split_benchmark_jobs(&mut jobs, MAX_BENCHMARK_GROUPS);
    let benchmark_bytes: u64 = benchmark_jobs.iter().map(job_estimated_bytes).sum();
    if !benchmark_sample_is_sufficient(&benchmark_jobs) {
        warn!(
            "Hash profile auto benchmark skipped: insufficient sample files={} bytes={} minimum_files={} minimum_bytes={}; using storage heuristic",
            benchmark_jobs.len(),
            benchmark_bytes,
            MIN_AUTO_BENCHMARK_FILES,
            MIN_AUTO_BENCHMARK_BYTES
        );
        jobs.extend(benchmark_jobs);
        let run_started = Instant::now();
        let results = recalculate_parts_for_jobs(
            jobs,
            (
                initial_limits.file_concurrency,
                initial_limits.global_part_concurrency,
            ),
            progress_tx,
            HashRunProgress::new(total_files, total_parts, total_bytes),
            cancel_rx,
            WholeFileHashIo::new(initial_profile, storage_class)
                .with_repository_root(context.target_local_path.as_deref()),
            context.hash_evidence.clone(),
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics(
            "auto_heuristic",
            initial_profile,
            run_started.elapsed(),
            &results,
            operation_id,
            algorithm,
            cancelled,
        );
        return (
            results,
            HashProfileDecision {
                requested: HashIoProfilePreference::Auto,
                selected: initial_profile,
                reason: format!("auto benchmark skipped: insufficient sample; {initial_reason}"),
                benchmarked_files: 0,
                benchmarked_bytes: 0,
                benchmark_elapsed: std::time::Duration::ZERO,
                sticky: true,
            },
            cancelled,
        );
    }

    let benchmark_total_parts: usize = benchmark_jobs
        .iter()
        .map(|job| job.indexed_parts.len())
        .sum();
    let mut benchmark_profiles = benchmark_profiles_for_environment(
        initial_profile,
        resource_profile,
        storage_class,
        benchmark_jobs.len(),
        benchmark_total_parts,
    );
    let profile_sequence = AUTO_BENCHMARK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let profile_rotation = rotate_benchmark_profiles(&mut benchmark_profiles, profile_sequence);
    info!(
        "Hash profile auto trial order: operation_id={} sequence={} rotation={} profiles={}",
        operation_id.unwrap_or("none"),
        profile_sequence,
        profile_rotation,
        benchmark_profiles
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    );
    let benchmark_file_count = benchmark_jobs.len();
    let benchmark_started = Instant::now();
    // Every profile hashes its own group of the sample, so the trials neither
    // repeat work nor read data a previous trial pulled into the page cache.
    // A sample too small to feed every profile tries the first ones only.
    let groups = deal_benchmark_groups(benchmark_jobs, benchmark_profiles.len());
    let group_count = groups.len();
    let trials: Vec<(HashIoProfilePreference, Vec<FileHashJob>)> =
        benchmark_profiles.into_iter().zip(groups).collect();
    let mut benchmark_results: Vec<FileHashResult> = Vec::new();
    let mut benchmark_hashed_bytes = 0u64;
    let mut valid_trials: Vec<(HashIoProfilePreference, f64, bool)> = Vec::new();
    let mut trial_progress = HashRunProgress::new(total_files, total_parts, total_bytes);
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: "Hashing profile".to_string(),
            percent: 0.21,
        });
    }

    for (group_index, (profile, group_jobs)) in trials.into_iter().enumerate() {
        if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            let benchmarked_files = benchmark_results.len();
            return (
                benchmark_results,
                HashProfileDecision {
                    requested: HashIoProfilePreference::Auto,
                    selected: initial_profile,
                    reason: "cancelled during auto benchmark".to_string(),
                    benchmarked_files,
                    benchmarked_bytes: benchmark_hashed_bytes,
                    benchmark_elapsed: benchmark_started.elapsed(),
                    sticky: false,
                },
                true,
            );
        }
        let group_parts: usize = group_jobs.iter().map(|job| job.indexed_parts.len()).sum();
        let group_bytes: u64 = group_jobs.iter().map(job_estimated_bytes).sum();
        let group_files = group_jobs.len();
        let limits = hash_scheduler_limits_for_environment(
            group_files,
            group_parts,
            profile,
            resource_profile,
            storage_class,
        );
        log_hash_scheduler_selection("auto_benchmark", requested_profile, profile, &limits);
        let profile_started = Instant::now();
        let (results, cancelled) = recalculate_parts_for_jobs(
            group_jobs,
            (limits.file_concurrency, limits.global_part_concurrency),
            progress_tx,
            trial_progress,
            cancel_rx,
            WholeFileHashIo::new(profile, storage_class)
                .with_repository_root(context.target_local_path.as_deref()),
            context.hash_evidence.clone(),
        )
        .await;
        let elapsed = profile_started.elapsed();
        let elapsed_secs = elapsed.as_secs_f64().max(0.001);
        let metrics = HashRunMetrics::from_results(&results);
        let throughput = metrics.hashed_bytes as f64 / elapsed_secs;
        log_hash_run_metrics(
            "auto_benchmark_sample",
            profile,
            elapsed,
            &results,
            operation_id,
            algorithm,
            cancelled,
        );
        info!(
            "Hash profile auto benchmark sample: profile={} group={}/{} files={} missing_files={} parts={} estimated_bytes={} hashed_bytes={} elapsed={:.2}s throughput={:.2} MB/s limits={}/{} wait_ratio={:.4}",
            profile,
            group_index + 1,
            group_count,
            results.len(),
            metrics.missing_files,
            group_parts,
            group_bytes,
            metrics.hashed_bytes,
            elapsed_secs,
            throughput / (1024.0 * 1024.0),
            limits.file_concurrency,
            limits.global_part_concurrency,
            benchmark_wait_ratio(&metrics)
        );
        // The group's hashes are real results whatever the trial says about
        // the profile; they are never recomputed.
        let sufficient = benchmark_metrics_are_sufficient(&metrics);
        benchmark_hashed_bytes = benchmark_hashed_bytes.saturating_add(metrics.hashed_bytes);
        trial_progress.initial_files_done += group_files;
        trial_progress.initial_parts_done += group_parts;
        trial_progress.initial_bytes_done += group_bytes;
        benchmark_results.extend(results);
        if cancelled {
            let benchmarked_files = benchmark_results.len();
            return (
                benchmark_results,
                HashProfileDecision {
                    requested: HashIoProfilePreference::Auto,
                    selected: profile,
                    reason: "cancelled during auto benchmark".to_string(),
                    benchmarked_files,
                    benchmarked_bytes: benchmark_hashed_bytes,
                    benchmark_elapsed: benchmark_started.elapsed(),
                    sticky: false,
                },
                true,
            );
        }
        if !sufficient {
            warn!(
                "Hash profile auto benchmark rejected: profile={} files={} missing_files={} estimated_bytes={} hashed_bytes={} minimum_files={} minimum_hashed_bytes={}; using storage heuristic",
                profile,
                metrics.files,
                metrics.missing_files,
                metrics.estimated_bytes,
                metrics.hashed_bytes,
                MIN_AUTO_BENCHMARK_FILES,
                MIN_AUTO_BENCHMARK_BYTES
            );
            continue;
        }
        let boost = benchmark_supports_boosted_aggressive(
            profile,
            &metrics,
            storage_class,
            resource_profile,
        );
        valid_trials.push((profile, throughput, boost));
    }

    let Some((best_profile, best_throughput, boost_remaining)) =
        select_benchmark_profile(initial_profile, &valid_trials)
    else {
        warn!(
            "Hash profile auto benchmark produced no valid sample; using storage heuristic profile={}",
            initial_profile
        );
        let run_started = Instant::now();
        let (mut results, cancelled) = recalculate_parts_for_jobs(
            jobs,
            (
                initial_limits.file_concurrency,
                initial_limits.global_part_concurrency,
            ),
            progress_tx,
            HashRunProgress {
                total_files,
                total_parts,
                total_bytes,
                initial_files_done: benchmark_file_count,
                initial_parts_done: benchmark_total_parts,
                initial_bytes_done: benchmark_bytes,
            },
            cancel_rx,
            WholeFileHashIo::new(initial_profile, storage_class)
                .with_repository_root(context.target_local_path.as_deref()),
            context.hash_evidence.clone(),
        )
        .await;
        log_hash_run_metrics(
            "auto_heuristic",
            initial_profile,
            run_started.elapsed(),
            &results,
            operation_id,
            algorithm,
            cancelled,
        );
        benchmark_results.append(&mut results);
        return (
            benchmark_results,
            HashProfileDecision {
                requested: HashIoProfilePreference::Auto,
                selected: initial_profile,
                reason: format!("auto benchmark invalid; {initial_reason}"),
                benchmarked_files: benchmark_file_count,
                benchmarked_bytes: benchmark_hashed_bytes,
                benchmark_elapsed: benchmark_started.elapsed(),
                sticky: false,
            },
            cancelled,
        );
    };
    if let Some((fastest_profile, fastest_throughput, _)) = valid_trials
        .iter()
        .copied()
        .max_by(|left, right| left.1.total_cmp(&right.1))
        && fastest_profile != best_profile
    {
        info!(
            "Hash profile auto stability guard: selected={} sample_bps={:.0} fastest={} fastest_bps={:.0} minimum_improvement_percent={:.1}",
            best_profile,
            best_throughput,
            fastest_profile,
            fastest_throughput,
            PROFILE_SWITCH_MIN_IMPROVEMENT_PERCENT
        );
    }
    let benchmark_elapsed = benchmark_started.elapsed();
    let remaining_parts: usize = jobs.iter().map(|job| job.indexed_parts.len()).sum();
    let remaining_limits = if boost_remaining {
        boosted_aggressive_hash_scheduler_limits_for_environment(
            jobs.len(),
            remaining_parts,
            resource_profile,
            storage_class,
            "low benchmark wait",
        )
    } else {
        hash_scheduler_limits_for_environment(
            jobs.len(),
            remaining_parts,
            best_profile,
            resource_profile,
            storage_class,
        )
    };
    log_hash_scheduler_selection(
        "auto_selected_remaining",
        requested_profile,
        best_profile,
        &remaining_limits,
    );
    info!(
        "Hash profile auto selected: selected={} initial={} benchmark_files={} benchmark_parts={} benchmark_estimated_bytes={} benchmark_hashed_bytes={} benchmark_elapsed={:.2}s remaining_files={} limits={}/{} storage={} boosted_aggressive={}",
        best_profile,
        initial_profile,
        benchmark_file_count,
        benchmark_total_parts,
        benchmark_bytes,
        benchmark_hashed_bytes,
        benchmark_elapsed.as_secs_f64(),
        jobs.len(),
        remaining_limits.file_concurrency,
        remaining_limits.global_part_concurrency,
        storage_class,
        boost_remaining
    );
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::RecheckHashProgress {
            checked_files: benchmark_file_count.min(total_files),
            total_files,
            checked_parts: benchmark_total_parts.min(total_parts),
            total_parts,
            checked_bytes: benchmark_bytes.min(total_bytes),
            total_bytes,
        });
        let remaining_bytes: u64 = jobs.iter().map(job_estimated_bytes).sum();
        let _ = tx.send(ProgressEvent::HashEstimate {
            remaining_bytes,
            bytes_per_sec: best_throughput as u64,
        });
    }
    let remaining_started = Instant::now();
    let remaining_results = recalculate_parts_for_jobs(
        jobs,
        (
            remaining_limits.file_concurrency,
            remaining_limits.global_part_concurrency,
        ),
        progress_tx,
        HashRunProgress {
            total_files,
            total_parts,
            total_bytes,
            initial_files_done: benchmark_file_count,
            initial_parts_done: benchmark_total_parts,
            initial_bytes_done: benchmark_bytes,
        },
        cancel_rx,
        WholeFileHashIo::new(best_profile, storage_class)
            .with_repository_root(context.target_local_path.as_deref()),
        context.hash_evidence.clone(),
    )
    .await;
    let (mut remaining_results, cancelled) = remaining_results;
    let remaining_elapsed = remaining_started.elapsed();
    log_hash_run_metrics(
        "auto_selected_remaining",
        best_profile,
        remaining_elapsed,
        &remaining_results,
        operation_id,
        algorithm,
        cancelled,
    );
    let heldout = HashRunMetrics::from_results(&remaining_results);
    let heldout_throughput =
        heldout.hashed_bytes as f64 / remaining_elapsed.as_secs_f64().max(0.001);
    let generalization_ratio = heldout_throughput / best_throughput.max(f64::EPSILON);
    info!(
        "Hash profile auto heldout: selected={} files={} missing_files={} parts={} estimated_bytes={} hashed_bytes={} elapsed={:.3}s sample_bps={:.0} heldout_bps={:.0} generalization_ratio={:.4} sufficient={}",
        best_profile,
        heldout.files,
        heldout.missing_files,
        heldout.parts,
        heldout.estimated_bytes,
        heldout.hashed_bytes,
        remaining_elapsed.as_secs_f64(),
        best_throughput,
        heldout_throughput,
        generalization_ratio,
        benchmark_metrics_are_sufficient(&heldout),
    );
    benchmark_results.append(&mut remaining_results);
    (
        benchmark_results,
        HashProfileDecision {
            requested: HashIoProfilePreference::Auto,
            selected: best_profile,
            reason: if boost_remaining {
                format!(
                    "auto benchmark selected {}; boosted aggressive after low wait; initial: {}",
                    best_profile, initial_reason
                )
            } else {
                format!(
                    "auto benchmark selected {}; initial: {}",
                    best_profile, initial_reason
                )
            },
            benchmarked_files: benchmark_file_count,
            benchmarked_bytes: benchmark_hashed_bytes,
            benchmark_elapsed,
            sticky: true,
        },
        cancelled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_profile_order_rotates_with_operation_sequence() {
        let original = vec![
            HashIoProfilePreference::Conservative,
            HashIoProfilePreference::Balanced,
            HashIoProfilePreference::Aggressive,
        ];
        let mut first = original.clone();
        assert_eq!(rotate_benchmark_profiles(&mut first, 0), 0);
        assert_eq!(first, original);

        let mut second = original.clone();
        assert_eq!(rotate_benchmark_profiles(&mut second, 1), 1);
        assert_eq!(
            second,
            vec![
                HashIoProfilePreference::Balanced,
                HashIoProfilePreference::Aggressive,
                HashIoProfilePreference::Conservative,
            ]
        );
    }

    #[test]
    fn benchmark_profile_keeps_initial_choice_for_small_sample_wins() {
        let selected = select_benchmark_profile(
            HashIoProfilePreference::Aggressive,
            &[
                (HashIoProfilePreference::Aggressive, 100.0, true),
                (HashIoProfilePreference::Balanced, 109.9, false),
            ],
        );
        assert_eq!(
            selected,
            Some((HashIoProfilePreference::Aggressive, 100.0, true))
        );
    }

    #[test]
    fn benchmark_profile_switches_for_a_material_sample_win() {
        let selected = select_benchmark_profile(
            HashIoProfilePreference::Aggressive,
            &[
                (HashIoProfilePreference::Aggressive, 100.0, true),
                (HashIoProfilePreference::Balanced, 111.0, false),
            ],
        );
        assert_eq!(
            selected,
            Some((HashIoProfilePreference::Balanced, 111.0, false))
        );
    }

    #[test]
    fn benchmark_profile_uses_fastest_valid_trial_when_initial_is_missing() {
        let selected = select_benchmark_profile(
            HashIoProfilePreference::Aggressive,
            &[
                (HashIoProfilePreference::Conservative, 90.0, false),
                (HashIoProfilePreference::Balanced, 100.0, false),
            ],
        );
        assert_eq!(
            selected,
            Some((HashIoProfilePreference::Balanced, 100.0, false))
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn only_files_that_match_the_remote_carry_a_verified_identity() {
        let dir = tempfile::tempdir().unwrap();
        let job = |name: &str, content: &[u8], remote: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, content).unwrap();
            FileHashJob {
                file_path: path.to_str().unwrap().to_owned(),
                file_length: content.len() as u64,
                indexed_parts: JobParts::owned(vec![(
                    0,
                    FoxyModFilePart {
                        remote_length: remote.len() as u64,
                        remote_checksum: blake3::hash(remote).to_hex().to_uppercase(),
                        ..Default::default()
                    },
                )]),
                capture_identity: true,
                ..test_job(0, 0)
            }
        };
        let jobs = vec![
            job("clean.pbo", b"same bytes", b"same bytes"),
            job("stale.pbo", b"old bytes!", b"new bytes!"),
        ];
        let (results, cancelled) = recalculate_parts_for_jobs(
            jobs,
            (2, 2),
            None,
            HashRunProgress::new(2, 2, 20),
            None,
            WholeFileHashIo::new(HashIoProfilePreference::Auto, HashStorageClass::Ssd),
            Arc::new(super::evidence::HashEvidenceState::default()),
        )
        .await;
        assert!(!cancelled);
        let verified = |name: &str| {
            results
                .iter()
                .find(|result| result.file_path.ends_with(name))
                .unwrap()
                .verified
                .is_some()
        };
        assert!(verified("clean.pbo"));
        assert!(!verified("stale.pbo"));
        assert!(results.iter().all(|result| result.issue.is_none()));
    }

    #[test]
    fn jobs_share_the_tree_parts_and_hand_them_back_whole() {
        use crate::core::models::model_tree::FileNode;
        let part = |file_id: u64, order: i64| FoxyModFilePart {
            file_id,
            data_order: order,
            path: format!("p{order}"),
            ..Default::default()
        };
        let mut tree = Tree {
            files: vec![
                FoxyModFile {
                    id: 1,
                    local_checksum: "LOCAL".into(),
                    ..Default::default()
                },
                FoxyModFile {
                    id: 2,
                    ..Default::default()
                },
            ],
            parts: vec![part(1, 1), part(2, 0), part(1, 0)],
            file_nodes: vec![
                FileNode {
                    file_idx: 0,
                    parts: vec![0, 2],
                },
                FileNode {
                    file_idx: 1,
                    parts: vec![1, 9],
                },
            ],
            ..Default::default()
        };
        let lent = lend_tree_parts(&mut tree);
        assert!(tree.parts.is_empty());
        let jobs = build_file_hash_jobs(&tree, &lent, &[0, 1], PartSpanSource::DetectLocalLayout);
        let listed = |job: &FileHashJob| -> Vec<(usize, String)> {
            job.indexed_parts
                .iter()
                .map(|(index, part)| (index, part.path.clone()))
                .collect()
        };
        assert_eq!(listed(&jobs[0]), vec![(2, "p0".into()), (0, "p1".into())]);
        assert!(jobs[0].has_local_baseline);
        assert_eq!(listed(&jobs[1]), vec![(1, "p0".into())]);
        assert!(!jobs[1].has_local_baseline);
        drop(jobs);
        return_tree_parts(&mut tree, lent);
        assert_eq!(tree.parts.len(), 3);
        assert_eq!(tree.parts[2].path, "p0");
    }

    fn test_job(parts: usize, bytes_per_part: u64) -> FileHashJob {
        FileHashJob {
            file_idx: 0,
            file_path: String::new(),
            file_length: (parts as u64).saturating_mul(bytes_per_part),
            file_remote_checksum: String::new(),
            indexed_parts: JobParts::owned(
                (0..parts)
                    .map(|idx| {
                        (
                            idx,
                            FoxyModFilePart {
                                remote_length: bytes_per_part,
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            ),
            span_source: PartSpanSource::DetectLocalLayout,
            has_local_baseline: false,
            capture_identity: false,
        }
    }

    #[test]
    fn missing_common_file_root_is_the_repository_scope_not_each_file_parent() {
        let first = FileHashJob {
            file_path: r"R:\Mods\RepoAlpha\@mod_01\addons\a.pbo".into(),
            ..test_job(0, 0)
        };
        let second = FileHashJob {
            file_path: r"R:\Mods\RepoAlpha\@mod_02\addons\b.pbo".into(),
            ..test_job(0, 0)
        };
        assert_eq!(
            common_hash_scope_root(&[first, second]).as_deref(),
            Some(Path::new(r"R:\Mods\RepoAlpha"))
        );
    }

    #[tokio::test]
    async fn absent_new_install_folder_is_missing_but_an_installed_scope_is_unreadable() {
        let directory = tempfile::tempdir().unwrap();
        for installed in [false, true] {
            let job = FileHashJob {
                file_path: directory
                    .path()
                    .join("absent/@mod_01/file.bin")
                    .to_string_lossy()
                    .into_owned(),
                file_remote_checksum: blake3::hash(b"data").to_hex().to_string(),
                file_length: 4,
                has_local_baseline: installed,
                ..test_job(0, 0)
            };
            let (results, cancelled) = recalculate_parts_for_jobs(
                vec![job],
                (1, 1),
                None,
                HashRunProgress::new(1, 0, 4),
                None,
                WholeFileHashIo::new(HashIoProfilePreference::Balanced, HashStorageClass::Ssd),
                Arc::new(super::evidence::HashEvidenceState::default()),
            )
            .await;
            assert!(!cancelled);
            assert_eq!(results.len(), 1);
            assert_eq!(
                results[0].issue,
                Some(if installed {
                    super::evidence::HashIssueKind::Unreadable
                } else {
                    super::evidence::HashIssueKind::Missing
                })
            );
            assert_eq!(results[0].missing_file, !installed);
        }
    }

    #[tokio::test]
    async fn a_deleted_addon_folder_is_missing_while_the_repository_remains_accessible() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_string_lossy().into_owned();
        let job = FileHashJob {
            file_path: directory
                .path()
                .join("@mod_01/file.bin")
                .to_string_lossy()
                .into_owned(),
            file_remote_checksum: blake3::hash(b"data").to_hex().to_string(),
            file_length: 4,
            has_local_baseline: true,
            ..test_job(0, 0)
        };
        let (results, _) = recalculate_parts_for_jobs(
            vec![job],
            (1, 1),
            None,
            HashRunProgress::new(1, 0, 4),
            None,
            WholeFileHashIo::new(HashIoProfilePreference::Balanced, HashStorageClass::Ssd)
                .with_repository_root(Some(&root)),
            Arc::new(super::evidence::HashEvidenceState::default()),
        )
        .await;
        assert_eq!(
            results[0].issue,
            Some(super::evidence::HashIssueKind::Missing)
        );
        assert!(results[0].missing_file);
    }

    fn tree_with_one_file(
        local_path: String,
        file_local_checksum: &str,
        part_local_checksum: &str,
        part_local_length: u64,
    ) -> Tree {
        Tree {
            files: vec![FoxyModFile {
                id: 1,
                local_path,
                local_checksum: file_local_checksum.to_owned(),
                ..Default::default()
            }],
            parts: vec![FoxyModFilePart {
                id: 10,
                file_id: 1,
                local_checksum: part_local_checksum.to_owned(),
                local_length: part_local_length,
                ..Default::default()
            }],
            file_nodes: vec![crate::core::models::model_tree::FileNode {
                file_idx: 0,
                parts: vec![0],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn missing_local_hash_pass_is_noop_for_fresh_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing_path = dir.path().join("missing.pbo").to_string_lossy().to_string();
        let tree = tree_with_one_file(missing_path, "", "", 0);

        assert!(missing_local_hash_pass_is_noop(&tree, &[0]));
    }

    #[test]
    fn missing_local_hash_pass_keeps_stale_state_clear_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing_path = dir.path().join("missing.pbo").to_string_lossy().to_string();
        let tree = tree_with_one_file(missing_path, "", "STALE", 100);

        assert!(!missing_local_hash_pass_is_noop(&tree, &[0]));
    }

    #[test]
    fn missing_local_hash_pass_does_not_skip_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("present.pbo");
        std::fs::write(&file_path, b"data").unwrap();
        let tree = tree_with_one_file(file_path.to_string_lossy().to_string(), "", "", 0);

        assert!(!missing_local_hash_pass_is_noop(&tree, &[0]));
    }

    #[test]
    fn storage_class_snapshot_is_reused_and_invalidated() {
        invalidate_storage_class_cache();
        let first = storage_class_mounts();
        assert!(
            STORAGE_CLASS_MOUNTS.lock().unwrap().is_some(),
            "first lookup must populate the snapshot"
        );
        let second = storage_class_mounts();
        assert_eq!(first.len(), second.len());
        invalidate_storage_class_cache();
        assert!(STORAGE_CLASS_MOUNTS.lock().unwrap().is_none());
    }

    #[test]
    fn empty_path_has_unknown_storage_class() {
        assert_eq!(
            detect_storage_class_for_path("   "),
            HashStorageClass::Unknown
        );
    }

    #[tokio::test]
    async fn whole_file_checksum_hashes_blake3_on_ssd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        let bytes = vec![0x5au8; 256 * 1024];
        std::fs::write(&path, &bytes).unwrap();
        let expected = blake3::hash(&bytes).to_hex().to_uppercase();

        let (checksum, _, _, _, _) = calculate_whole_file_checksum_monitored(
            path.to_string_lossy().to_string(),
            expected.clone(),
            bytes.len() as u64,
            Arc::new(Semaphore::new(1)),
            None,
            Arc::new(super::super::evidence::HashEvidenceState::default()),
            None,
        )
        .await;

        assert_eq!(checksum.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn whole_file_checksum_observes_cancellation_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, b"hello").unwrap();
        let expected = blake3::hash(b"hello").to_hex().to_uppercase();
        let (_cancel_tx, cancel_rx) = watch::channel(true);

        let (checksum, _, _, issue, cancelled) = calculate_whole_file_checksum_monitored(
            path.to_string_lossy().to_string(),
            expected,
            5,
            Arc::new(Semaphore::new(1)),
            None,
            Arc::new(super::super::evidence::HashEvidenceState::default()),
            Some(cancel_rx),
        )
        .await;

        assert!(checksum.is_none());
        assert_eq!(issue, Some(super::super::evidence::HashIssueKind::Skipped));
        assert!(cancelled);
    }

    #[tokio::test]
    async fn whole_file_checksum_md5_ignores_mmap_strategy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, b"hello").unwrap();
        let mut md5 = FlexHasher::new_md5();
        md5.update(b"hello");
        let expected = md5.finalize_hex();

        let (checksum, _, _, _, _) = calculate_whole_file_checksum_monitored(
            path.to_string_lossy().to_string(),
            expected.clone(),
            5,
            Arc::new(Semaphore::new(1)),
            None,
            Arc::new(super::super::evidence::HashEvidenceState::default()),
            None,
        )
        .await;

        assert_eq!(checksum.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn whole_file_checksum_hashes_no_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, b"hello").unwrap();
        let expected = blake3::hash(b"hello").to_hex().to_uppercase();

        let (checksum, _, metrics, _, _) = calculate_whole_file_checksum_monitored(
            path.to_string_lossy().to_string(),
            expected.clone(),
            5,
            Arc::new(Semaphore::new(1)),
            None,
            Arc::new(super::super::evidence::HashEvidenceState::default()),
            None,
        )
        .await;

        assert_eq!(checksum.as_deref(), Some(expected.as_str()));
        assert_eq!(metrics.hashed_bytes, 5);
    }

    #[tokio::test]
    async fn whole_file_checksum_skips_length_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, b"hello").unwrap();
        let expected = blake3::hash(b"hello").to_hex().to_uppercase();

        let (checksum, _, metrics, _, _) = calculate_whole_file_checksum_monitored(
            path.to_string_lossy().to_string(),
            expected,
            10,
            Arc::new(Semaphore::new(1)),
            None,
            Arc::new(super::super::evidence::HashEvidenceState::default()),
            None,
        )
        .await;

        assert!(checksum.is_none());
        assert_eq!(metrics.hashed_bytes, 0);
    }

    fn normal_resources() -> ResourceProfile {
        ResourceProfile::from_memory(32 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, 0)
    }

    fn constrained_resources() -> ResourceProfile {
        ResourceProfile::from_memory(8 * 1024 * 1024 * 1024, 3 * 1024 * 1024 * 1024, 0)
    }

    #[test]
    fn conservative_profile_caps_hash_concurrency() {
        let limits = hash_scheduler_limits_for_resources(
            20,
            2_000,
            HashIoProfilePreference::Conservative,
            normal_resources(),
        );
        assert_eq!(limits.file_concurrency, 2);
        assert_eq!(limits.global_part_concurrency, 2);
    }

    #[test]
    fn conservative_profile_keeps_at_least_one_worker() {
        let limits = hash_scheduler_limits_for_resources(
            0,
            0,
            HashIoProfilePreference::Conservative,
            normal_resources(),
        );
        assert_eq!(limits.file_concurrency, 1);
        assert_eq!(limits.global_part_concurrency, 1);
    }

    #[test]
    fn balanced_profile_uses_moderate_concurrency() {
        let limits = hash_scheduler_limits_for_resources(
            100,
            10_000,
            HashIoProfilePreference::Balanced,
            normal_resources(),
        );
        assert!((1..=8).contains(&limits.file_concurrency));
        assert_eq!(limits.file_concurrency, limits.global_part_concurrency);
    }

    #[test]
    fn constrained_resources_cap_aggressive_hash_concurrency() {
        let limits = hash_scheduler_limits_for_resources(
            100,
            10_000,
            HashIoProfilePreference::Aggressive,
            constrained_resources(),
        );
        assert!(limits.file_concurrency <= 8);
        assert!(limits.global_part_concurrency <= 8);
    }

    #[test]
    fn constrained_resources_cap_auto_aggressive_to_balanced() {
        let (profile, reason) =
            cap_auto_hash_profile(HashIoProfilePreference::Aggressive, constrained_resources());
        assert_eq!(profile, HashIoProfilePreference::Balanced);
        assert!(reason.is_some());
    }

    #[test]
    fn benchmark_sampling_prefers_representative_heavy_jobs() {
        let mut jobs = vec![
            test_job(1, 1),
            test_job(2, 1),
            test_job(64, 1024),
            test_job(32, 1024),
            test_job(16, 1024),
        ];
        let selected = split_benchmark_jobs(&mut jobs, 1);
        assert_eq!(selected.len(), 5);
        assert!(job_estimated_bytes(&selected[0]) >= job_estimated_bytes(&selected[1]));
        assert_eq!(selected[0].indexed_parts.len(), 64);
    }

    #[test]
    fn benchmark_sample_scales_with_the_number_of_trial_groups() {
        // 40 files of 128 MiB: one group stops at 12 files / 512 MiB, three
        // groups take three times as much so each trial gets its own data.
        let mut jobs: Vec<FileHashJob> = (0..40).map(|_| test_job(1, 128 << 20)).collect();
        let one = split_benchmark_jobs(&mut jobs.clone(), 1);
        assert_eq!(one.len(), 4);
        let three = split_benchmark_jobs(&mut jobs, 3);
        assert_eq!(three.len(), 12);
        assert_eq!(jobs.len(), 28);
    }

    #[test]
    fn benchmark_groups_are_disjoint_balanced_and_each_sufficient() {
        let sample: Vec<FileHashJob> = (0..12).map(|i| test_job(12 - i, 32 << 20)).collect();
        let groups = deal_benchmark_groups(sample.clone(), 3);
        assert_eq!(groups.len(), 3);
        let total: usize = groups.iter().map(Vec::len).sum();
        assert_eq!(total, sample.len());
        let parts: Vec<Vec<usize>> = groups
            .iter()
            .map(|group| group.iter().map(|job| job.indexed_parts.len()).collect())
            .collect();
        assert_eq!(parts, [[12, 7, 6, 1], [11, 8, 5, 2], [10, 9, 4, 3]]);
        assert!(parts.iter().all(|group| group.iter().sum::<usize>() == 26));
        assert!(
            groups
                .iter()
                .all(|group| benchmark_sample_is_sufficient(group))
        );
    }

    #[test]
    fn benchmark_groups_fold_back_until_every_group_can_judge() {
        // Four 96 MiB files: three groups would be one or two files each,
        // below the minimum, so the sample folds to one group.
        let sample: Vec<FileHashJob> = (0..4).map(|_| test_job(1, 96 << 20)).collect();
        let groups = deal_benchmark_groups(sample, 3);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 4);
        // Six such files feed two groups of three, not three of two.
        let sample: Vec<FileHashJob> = (0..6).map(|_| test_job(1, 96 << 20)).collect();
        let groups = deal_benchmark_groups(sample, 3);
        assert_eq!(groups.len(), 2);
        assert!(deal_benchmark_groups(Vec::new(), 3).len() == 1);
    }

    #[test]
    fn benchmark_sample_requires_minimum_bytes() {
        let jobs = vec![
            test_job(1, 1024),
            test_job(1, 1024),
            test_job(1, 1024),
            test_job(1, 1024),
        ];
        assert!(!benchmark_sample_is_sufficient(&jobs));

        let large_jobs = vec![
            test_job(1, 96 * 1024 * 1024),
            test_job(1, 96 * 1024 * 1024),
            test_job(1, 96 * 1024 * 1024),
        ];
        assert!(benchmark_sample_is_sufficient(&large_jobs));
    }

    #[test]
    fn benchmark_metrics_require_actual_hashed_bytes_and_present_files() {
        let valid = HashRunMetrics {
            files: MIN_AUTO_BENCHMARK_FILES,
            hashed_bytes: MIN_AUTO_BENCHMARK_BYTES,
            ..Default::default()
        };
        assert!(benchmark_metrics_are_sufficient(&valid));

        let missing = HashRunMetrics {
            missing_files: 1,
            ..valid
        };
        assert!(!benchmark_metrics_are_sufficient(&missing));

        let no_actual_bytes = HashRunMetrics {
            missing_files: 0,
            hashed_bytes: 0,
            ..valid
        };
        assert!(!benchmark_metrics_are_sufficient(&no_actual_bytes));
    }

    #[test]
    fn hdd_large_part_workload_caps_aggressive_to_two_workers() {
        let limits = hash_scheduler_limits_for_environment(
            100,
            10_000,
            HashIoProfilePreference::Aggressive,
            normal_resources(),
            HashStorageClass::Hdd,
        );
        assert_eq!(limits.file_concurrency, 2);
        assert_eq!(limits.global_part_concurrency, 2);
        assert!(limits.reason.contains("hdd large-part cap"));
    }

    #[test]
    fn hdd_small_part_workload_keeps_balanced_parallelism_available() {
        let limits = hash_scheduler_limits_for_environment(
            100,
            200,
            HashIoProfilePreference::Balanced,
            normal_resources(),
            HashStorageClass::Hdd,
        );
        assert!(limits.file_concurrency > 2);
        assert_eq!(limits.storage_class, HashStorageClass::Hdd);
    }

    #[test]
    fn auto_profile_on_hdd_large_workload_uses_conservative() {
        let (profile, reason) = auto_hash_profile_for_environment(
            100,
            10_000,
            normal_resources(),
            HashStorageClass::Hdd,
        );
        assert_eq!(profile, HashIoProfilePreference::Conservative);
        assert!(reason.contains("hdd large-part workload"));
    }

    #[test]
    fn auto_profile_on_ssd_large_workload_keeps_aggressive() {
        let (profile, reason) = auto_hash_profile_for_environment(
            100,
            10_000,
            normal_resources(),
            HashStorageClass::Ssd,
        );
        assert_eq!(profile, HashIoProfilePreference::Aggressive);
        assert!(reason.contains("storage heuristic"));
    }

    #[test]
    fn auto_profile_on_unknown_large_workload_uses_balanced() {
        let (profile, reason) = auto_hash_profile_for_environment(
            100,
            10_000,
            normal_resources(),
            HashStorageClass::Unknown,
        );
        assert_eq!(profile, HashIoProfilePreference::Balanced);
        assert!(reason.contains("storage heuristic"));
    }

    #[test]
    fn hdd_large_part_benchmark_profiles_only_allow_conservative() {
        let profiles = benchmark_profiles_for_environment(
            HashIoProfilePreference::Conservative,
            normal_resources(),
            HashStorageClass::Hdd,
            100,
            10_000,
        );
        assert_eq!(profiles, vec![HashIoProfilePreference::Conservative]);
    }

    #[test]
    fn hdd_small_part_benchmark_profiles_do_not_allow_aggressive() {
        let profiles = benchmark_profiles_for_environment(
            HashIoProfilePreference::Balanced,
            normal_resources(),
            HashStorageClass::Hdd,
            100,
            200,
        );
        assert!(profiles.contains(&HashIoProfilePreference::Conservative));
        assert!(profiles.contains(&HashIoProfilePreference::Balanced));
        assert!(!profiles.contains(&HashIoProfilePreference::Aggressive));
    }

    #[test]
    fn ssd_benchmark_profiles_start_with_initial_profile_and_allow_aggressive() {
        let profiles = benchmark_profiles_for_environment(
            HashIoProfilePreference::Aggressive,
            normal_resources(),
            HashStorageClass::Ssd,
            100,
            10_000,
        );
        assert_eq!(profiles.first(), Some(&HashIoProfilePreference::Aggressive));
        assert!(profiles.contains(&HashIoProfilePreference::Conservative));
        assert!(profiles.contains(&HashIoProfilePreference::Balanced));
        assert!(profiles.contains(&HashIoProfilePreference::Aggressive));
    }

    #[test]
    fn boosted_aggressive_never_bypasses_hdd_cap() {
        let limits = boosted_aggressive_hash_scheduler_limits_for_environment(
            100,
            10_000,
            normal_resources(),
            HashStorageClass::Hdd,
            "test",
        );
        assert_eq!(limits.file_concurrency, 2);
        assert_eq!(limits.global_part_concurrency, 2);
        assert!(limits.reason.contains("hdd large-part cap"));
    }

    #[test]
    fn boosted_aggressive_increases_ssd_part_cap() {
        let base = hash_scheduler_limits_for_environment(
            100,
            10_000,
            HashIoProfilePreference::Aggressive,
            normal_resources(),
            HashStorageClass::Ssd,
        );
        let boosted = boosted_aggressive_hash_scheduler_limits_for_environment(
            100,
            10_000,
            normal_resources(),
            HashStorageClass::Ssd,
            "test",
        );
        assert!(boosted.global_part_concurrency >= base.global_part_concurrency);
        assert!(boosted.reason.contains("boosted aggressive"));
    }

    fn severe_resources() -> ResourceProfile {
        // available < 1536 MiB → severe pressure
        ResourceProfile::from_memory(16 * 1024 * 1024 * 1024, 1024 * 1024 * 1024, 0)
    }

    // ── cap_auto_hash_profile matrix ────────────────────────────────────

    #[test]
    fn cap_auto_profile_normal_pressure_never_caps() {
        for profile in [
            HashIoProfilePreference::Conservative,
            HashIoProfilePreference::Balanced,
            HashIoProfilePreference::Aggressive,
        ] {
            let (capped, reason) = cap_auto_hash_profile(profile, normal_resources());
            assert_eq!(capped, profile);
            assert!(reason.is_none());
        }
    }

    #[test]
    fn cap_auto_profile_constrained_caps_only_aggressive() {
        let (aggressive, aggressive_reason) =
            cap_auto_hash_profile(HashIoProfilePreference::Aggressive, constrained_resources());
        assert_eq!(aggressive, HashIoProfilePreference::Balanced);
        assert!(aggressive_reason.is_some());

        let (balanced, balanced_reason) =
            cap_auto_hash_profile(HashIoProfilePreference::Balanced, constrained_resources());
        assert_eq!(balanced, HashIoProfilePreference::Balanced);
        assert!(balanced_reason.is_none());

        let (conservative, conservative_reason) = cap_auto_hash_profile(
            HashIoProfilePreference::Conservative,
            constrained_resources(),
        );
        assert_eq!(conservative, HashIoProfilePreference::Conservative);
        assert!(conservative_reason.is_none());
    }

    #[test]
    fn cap_auto_profile_severe_forces_conservative() {
        let (aggressive, aggressive_reason) =
            cap_auto_hash_profile(HashIoProfilePreference::Aggressive, severe_resources());
        assert_eq!(aggressive, HashIoProfilePreference::Conservative);
        assert!(aggressive_reason.is_some());

        let (balanced, balanced_reason) =
            cap_auto_hash_profile(HashIoProfilePreference::Balanced, severe_resources());
        assert_eq!(balanced, HashIoProfilePreference::Conservative);
        assert!(balanced_reason.is_some());

        let (conservative, conservative_reason) =
            cap_auto_hash_profile(HashIoProfilePreference::Conservative, severe_resources());
        assert_eq!(conservative, HashIoProfilePreference::Conservative);
        assert!(conservative_reason.is_none());
    }

    // ── HashProfileDecision constructors ────────────────────────────────

    #[test]
    fn hash_profile_decision_manual_records_override() {
        let decision = HashProfileDecision::manual(HashIoProfilePreference::Aggressive);
        assert_eq!(decision.requested, HashIoProfilePreference::Aggressive);
        assert_eq!(decision.selected, HashIoProfilePreference::Aggressive);
        assert!(!decision.sticky);
        assert_eq!(decision.reason, "manual override");
        assert_eq!(decision.benchmarked_files, 0);
    }

    #[test]
    fn hash_profile_decision_sticky_auto_records_auto_request() {
        let decision = HashProfileDecision::sticky_auto(HashIoProfilePreference::Balanced);
        assert_eq!(decision.requested, HashIoProfilePreference::Auto);
        assert_eq!(decision.selected, HashIoProfilePreference::Balanced);
        assert!(decision.sticky);
        assert_eq!(decision.reason, "sticky auto decision");
    }

    // ── hash_cpu_budget ─────────────────────────────────────────────────

    #[test]
    fn storage_overrides_round_trip_and_unknown_means_auto() {
        for class in [
            HashStorageClass::Ssd,
            HashStorageClass::Hdd,
            HashStorageClass::Removable,
        ] {
            assert_eq!(
                decode_storage_class(encode_storage_class(Some(class))),
                Some(class)
            );
        }
        assert_eq!(decode_storage_class(encode_storage_class(None)), None);
        assert_eq!(
            decode_storage_class(encode_storage_class(Some(HashStorageClass::Unknown))),
            None
        );
    }

    #[test]
    fn hash_cpu_budget_is_at_least_one() {
        assert!(hash_cpu_budget() >= 1);
    }

    // ── job_estimated_bytes ─────────────────────────────────────────────

    #[test]
    fn rotational_storage_reads_parts_in_larger_requests() {
        let io = |storage| WholeFileHashIo::new(HashIoProfilePreference::Auto, storage);
        assert_eq!(
            io(HashStorageClass::Hdd).part_reader_capacity(),
            super::super::part_hashes::ROTATIONAL_HASH_READER_CAPACITY
        );
        assert_eq!(
            io(HashStorageClass::Ssd).part_reader_capacity(),
            super::super::part_hashes::HASH_READER_CAPACITY
        );
    }

    #[test]
    fn hash_jobs_run_heavy_first_then_small_files_in_path_order() {
        let job = |path: &str, parts: usize, bytes_per_part: u64| FileHashJob {
            file_path: path.to_string(),
            ..test_job(parts, bytes_per_part)
        };
        let mut jobs = vec![
            job("D:/repo/@b/addons/small.pbo", 40, 1024),
            job("D:/repo/@a/addons/big.pbo", 10, 8 * 1024 * 1024),
            job("D:/repo/@A/mod.cpp", 1, 100),
            job("D:/repo/@c/addons/huge.pbo", 400, 1024 * 1024),
            job("D:/repo/@b/addons/small.pbo.bisign", 1, 500),
        ];

        order_hash_jobs(&mut jobs);

        let order: Vec<&str> = jobs.iter().map(|job| job.file_path.as_str()).collect();
        assert_eq!(
            order,
            [
                "D:/repo/@c/addons/huge.pbo",
                "D:/repo/@a/addons/big.pbo",
                "D:/repo/@A/mod.cpp",
                "D:/repo/@b/addons/small.pbo",
                "D:/repo/@b/addons/small.pbo.bisign",
            ]
        );
    }

    #[test]
    fn job_estimated_bytes_sums_part_lengths() {
        let job = test_job(4, 10);
        assert_eq!(job_estimated_bytes(&job), 40);
    }

    #[test]
    fn job_estimated_bytes_falls_back_to_file_length_without_parts() {
        let mut job = test_job(0, 0);
        job.file_length = 4242;
        assert_eq!(job_estimated_bytes(&job), 4242);
    }

    // ── benchmark_sample_is_sufficient boundaries ───────────────────────

    #[test]
    fn benchmark_sample_rejects_below_minimum_file_count() {
        let jobs = vec![
            test_job(1, 200 * 1024 * 1024),
            test_job(1, 200 * 1024 * 1024),
        ];
        // Two big files easily clear the byte minimum but not the file minimum.
        assert!(!benchmark_sample_is_sufficient(&jobs));
    }

    #[test]
    fn benchmark_sample_rejects_when_bytes_below_minimum() {
        let jobs = vec![
            test_job(1, 50 * 1024 * 1024),
            test_job(1, 50 * 1024 * 1024),
            test_job(1, 50 * 1024 * 1024),
        ];
        // Three files (meets count) but only ~150 MiB (< 256 MiB minimum).
        assert!(!benchmark_sample_is_sufficient(&jobs));
    }

    #[test]
    fn benchmark_sample_accepts_at_minimum_files_and_bytes() {
        let jobs = vec![
            test_job(1, 90 * 1024 * 1024),
            test_job(1, 90 * 1024 * 1024),
            test_job(1, 90 * 1024 * 1024),
        ];
        // Three files, ~270 MiB total (>= 256 MiB minimum).
        assert!(benchmark_sample_is_sufficient(&jobs));
    }

    // ── hash_scheduler_limits_for_resources under severe pressure ───────

    #[test]
    fn balanced_profile_severe_pressure_caps_to_two() {
        let limits = hash_scheduler_limits_for_resources(
            100,
            10_000,
            HashIoProfilePreference::Balanced,
            severe_resources(),
        );
        assert!(limits.file_concurrency <= 2);
        assert!(limits.global_part_concurrency <= 2);
    }

    #[test]
    fn aggressive_profile_severe_pressure_caps_to_two() {
        let limits = hash_scheduler_limits_for_resources(
            100,
            10_000,
            HashIoProfilePreference::Aggressive,
            severe_resources(),
        );
        assert!(limits.file_concurrency <= 2);
        assert!(limits.global_part_concurrency <= 2);
    }

    #[test]
    fn conservative_profile_single_job_uses_one_worker() {
        let limits = hash_scheduler_limits_for_resources(
            1,
            10,
            HashIoProfilePreference::Conservative,
            normal_resources(),
        );
        assert_eq!(limits.file_concurrency, 1);
        assert_eq!(limits.global_part_concurrency, 1);
    }

    #[test]
    fn aggressive_profile_zero_jobs_keeps_one_worker() {
        let limits = hash_scheduler_limits_for_resources(
            0,
            0,
            HashIoProfilePreference::Aggressive,
            normal_resources(),
        );
        assert_eq!(limits.file_concurrency, 1);
        assert!(limits.global_part_concurrency >= 1);
    }
}
