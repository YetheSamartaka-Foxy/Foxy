use super::part_hashes::{PartHashProgress, PartSpanSource, calculate_part_hashes};
use super::*;
use crate::core::utils::resource_profile::{ResourcePressure, ResourceProfile};
use crate::core::utils::speed_of_light::{SolLight, sol_line};
use crate::ui::types::HashIoProfilePreference;
use sysinfo::Disks;

#[derive(Clone)]
pub(super) struct FileHashJob {
    pub(super) file_idx: usize,
    pub(super) file_path: String,
    pub(super) file_length: u64,
    pub(super) file_remote_checksum: String,
    pub(super) indexed_parts: Vec<(usize, FoxyModFilePart)>,
    pub(super) span_source: PartSpanSource,
}

pub(super) struct FileHashResult {
    pub(super) file_idx: usize,
    pub(super) updated_parts: Vec<(usize, FoxyModFilePart)>,
    pub(super) whole_file_checksum: Option<String>,
    pub(super) file_path: String,
    pub(super) elapsed: std::time::Duration,
    pub(super) parts_count: usize,
    pub(super) missing_file: bool,
    pub(super) part_metrics: super::part_hashes::PartHashMetrics,
}

#[derive(Clone, Copy)]
pub(super) struct HashRunProgress {
    total_files: usize,
    total_parts: usize,
    initial_files_done: usize,
    initial_parts_done: usize,
}

impl HashRunProgress {
    fn new(total_files: usize, total_parts: usize) -> Self {
        Self {
            total_files,
            total_parts,
            initial_files_done: 0,
            initial_parts_done: 0,
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
pub(super) enum HashStorageClass {
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
const LOW_WAIT_AGGRESSIVE_THRESHOLD: f64 = 0.01;

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
    let logical_cpus = std::thread::available_parallelism()
        .map(|cpus| cpus.get())
        .unwrap_or(CONCURRENCY_LIMIT.max(1));
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
    let path = Path::new(path);
    let disks = Disks::new_with_refreshed_list();
    disks
        .iter()
        .filter(|disk| storage_path_starts_with_mount(path, disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| {
            let kind = format!("{:?}", disk.kind()).to_ascii_lowercase();
            if kind.contains("hdd") {
                HashStorageClass::Hdd
            } else if kind.contains("ssd") {
                HashStorageClass::Ssd
            } else if disk.is_removable() {
                HashStorageClass::Removable
            } else {
                HashStorageClass::Unknown
            }
        })
        .unwrap_or(HashStorageClass::Unknown)
}

fn storage_path_starts_with_mount(path: &Path, mount: &Path) -> bool {
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

pub(super) fn build_file_hash_jobs(
    data_tree: &Tree,
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
        let mut indexed_parts: Vec<(usize, FoxyModFilePart)> = file_node
            .parts
            .iter()
            .filter_map(|&part_idx| {
                data_tree
                    .parts
                    .get(part_idx)
                    .cloned()
                    .map(|p| (part_idx, p))
            })
            .collect();
        indexed_parts.sort_by_key(|(_, part)| part.data_order);
        jobs.push(FileHashJob {
            file_idx,
            file_path: file.local_path.clone(),
            file_length: file.length,
            file_remote_checksum: file.remote_checksum.clone(),
            indexed_parts,
            span_source,
        });
    }
    jobs
}

async fn calculate_whole_file_checksum(
    file_path: String,
    expected_checksum: String,
    expected_len: u64,
    semaphore: Arc<Semaphore>,
) -> (Option<String>, super::part_hashes::PartHashMetrics) {
    const WHOLE_FILE_HASH_BUF_SIZE: usize = 4 * 1024 * 1024;

    let started = Instant::now();
    let mut metrics = super::part_hashes::PartHashMetrics {
        estimated_bytes: expected_len,
        ..Default::default()
    };

    let metadata_started = Instant::now();
    let metadata = match std::fs::metadata(&file_path) {
        Ok(metadata) => metadata,
        Err(err) => {
            debug!("Whole-file hash skipped for {}: {}", file_path, err);
            metrics.metadata_elapsed = metadata_started.elapsed();
            metrics.total_elapsed = started.elapsed();
            return (None, metrics);
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
        return (None, metrics);
    }

    let wait_started = Instant::now();
    let permit = semaphore.acquire_owned().await.ok();
    metrics.semaphore_wait_elapsed = wait_started.elapsed();

    let blocking_started = Instant::now();
    let file_path_for_hash = file_path.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let _permit = permit;
        let mut file = std::fs::File::open(&file_path_for_hash)?;
        let mut hasher = FlexHasher::from_checksum(&expected_checksum);
        let mut buffer = vec![0u8; WHOLE_FILE_HASH_BUF_SIZE];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.finalize_hex())
    })
    .await;
    metrics.blocking_hash_elapsed = blocking_started.elapsed();

    let checksum = match result {
        Ok(Ok(checksum)) => {
            metrics.hashed_bytes = expected_len;
            Some(checksum)
        }
        Ok(Err(err)) => {
            warn!("Whole-file hash failed for {}: {}", file_path, err);
            None
        }
        Err(err) => {
            error!("Whole-file hash task panicked for {}: {}", file_path, err);
            None
        }
    };

    metrics.total_elapsed = started.elapsed();
    (checksum, metrics)
}

pub(super) async fn recalculate_parts_for_jobs(
    mut jobs: Vec<FileHashJob>,
    file_concurrency: usize,
    global_part_concurrency: usize,
    progress_tx: Option<&Sender<ProgressEvent>>,
    progress: HashRunProgress,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> (Vec<FileHashResult>, bool) {
    // Shared semaphore limits the total in-flight spawn_blocking hash tasks across all files.
    let semaphore = Arc::new(Semaphore::new(global_part_concurrency));

    // Shared counter for completed files - used for progress reporting
    let files_done = Arc::new(AtomicUsize::new(
        progress.initial_files_done.min(progress.total_files),
    ));
    let parts_done = Arc::new(AtomicUsize::new(
        progress.initial_parts_done.min(progress.total_parts),
    ));
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Prioritize heavy files first so big PBOs are not starved behind many tiny 1-part files.
    jobs.sort_by_key(|job| Reverse(job.indexed_parts.len()));
    let progress_sender = progress_tx.cloned();
    let cancel_receiver = cancel_rx.cloned();
    let results = stream::iter(jobs.into_iter().map(|job| {
        let sem = semaphore.clone();
        let done_counter = files_done.clone();
        let part_counter = parts_done.clone();
        let ptx = progress_sender.clone();
        let cancel = cancel_receiver.clone();
        let cancelled_flag = cancelled.clone();
        async move {
            let FileHashJob {
                file_idx,
                file_path,
                file_length,
                file_remote_checksum,
                indexed_parts,
                span_source,
            } = job;
            let parts_count = indexed_parts.len();
            if cancelled_flag.load(Ordering::Relaxed)
                || cancel.as_ref().is_some_and(|rx| *rx.borrow())
            {
                cancelled_flag.store(true, Ordering::Relaxed);
                let completed = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
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
                    file_path,
                    elapsed: std::time::Duration::ZERO,
                    parts_count,
                    missing_file: false,
                    part_metrics: Default::default(),
                };
            }
            let file_started = Instant::now();
            let missing_file = !Path::new(&file_path).exists();
            let (part_calculation, whole_file_checksum) = if indexed_parts.is_empty()
                && !file_remote_checksum.is_empty()
            {
                let (checksum, metrics) = calculate_whole_file_checksum(
                    file_path.clone(),
                    file_remote_checksum,
                    file_length,
                    sem,
                )
                .await;
                (
                    super::part_hashes::PartHashCalculation {
                        parts: Vec::new(),
                        metrics,
                    },
                    checksum,
                )
            } else {
                let parts_only: Vec<FoxyModFilePart> =
                    indexed_parts.iter().map(|(_, p)| p.clone()).collect();
                let part_progress = ptx.clone().map(|tx| {
                    PartHashProgress::new(
                        part_counter.clone(),
                        progress.total_parts,
                        done_counter.clone(),
                        progress.total_files,
                        tx,
                    )
                });
                (
                    calculate_part_hashes(parts_only, &file_path, sem, span_source, part_progress)
                        .await,
                    None,
                )
            };
            let file_elapsed = file_started.elapsed();
            let updated_parts = if cancel.as_ref().is_some_and(|rx| *rx.borrow()) {
                cancelled_flag.store(true, Ordering::Relaxed);
                Vec::new()
            } else {
                indexed_parts
                    .into_iter()
                    .zip(part_calculation.parts)
                    .map(|((part_idx, _), updated_part)| (part_idx, updated_part))
                    .collect()
            };

            // Report file-level progress
            let completed = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some(ref tx) = ptx {
                let _ = tx.send(ProgressEvent::RecheckHashProgress {
                    checked_files: completed.min(progress.total_files),
                    total_files: progress.total_files,
                    checked_parts: part_counter
                        .load(Ordering::Relaxed)
                        .min(progress.total_parts),
                    total_parts: progress.total_parts,
                });
            }
            if completed.is_multiple_of(500) || completed == progress.total_files {
                info!(
                    "Phase 1 progress: {}/{} files hashed",
                    completed, progress.total_files
                );
            }

            FileHashResult {
                file_idx,
                updated_parts,
                whole_file_checksum,
                file_path,
                elapsed: file_elapsed,
                parts_count,
                missing_file,
                part_metrics: part_calculation.metrics,
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

fn split_benchmark_jobs(jobs: &mut Vec<FileHashJob>) -> Vec<FileHashJob> {
    const MAX_BENCHMARK_FILES: usize = 12;
    const MAX_BENCHMARK_BYTES: u64 = 512 * 1024 * 1024;

    if jobs.len() < MIN_AUTO_BENCHMARK_FILES {
        return Vec::new();
    }

    // Layout-heavy PBOs can dominate real hashing time even when they are not
    // the largest files. Prefer those first, then fill the byte budget.
    jobs.sort_by_key(|job| Reverse((job.indexed_parts.len(), job_estimated_bytes(job))));
    let take_count = jobs.len().min(MAX_BENCHMARK_FILES);
    let mut selected = Vec::new();
    let mut selected_bytes = 0u64;
    for _ in 0..take_count {
        if jobs.is_empty() {
            break;
        }
        let job = jobs.remove(0);
        selected_bytes = selected_bytes.saturating_add(job_estimated_bytes(&job));
        selected.push(job);
        if selected.len() >= MIN_AUTO_BENCHMARK_FILES && selected_bytes >= MAX_BENCHMARK_BYTES {
            break;
        }
    }
    selected
}

fn benchmark_sample_is_sufficient(benchmark_jobs: &[FileHashJob]) -> bool {
    benchmark_jobs.len() >= MIN_AUTO_BENCHMARK_FILES
        && benchmark_jobs.iter().map(job_estimated_bytes).sum::<u64>() >= MIN_AUTO_BENCHMARK_BYTES
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

fn log_hash_run_metrics(
    label: &str,
    selected_profile: HashIoProfilePreference,
    wall_elapsed: std::time::Duration,
    results: &[FileHashResult],
) {
    let metrics = HashRunMetrics::from_results(results);
    info!(
        "Hash part run metrics: label={} profile={} wall={:.3}s files={} missing_files={} parts={} estimated_bytes={} hashed_bytes={} file_elapsed_sum={:.3}s file_elapsed_max={:.3}s part_total_sum={:.3}s metadata_sum={:.3}s layout_sum={:.3}s layout_parse_sum={:.3}s layout_map_sum={:.3}s semaphore_wait_sum={:.3}s blocking_hash_sum={:.3}s layout_files={} remote_span_files={} layout_entries={} layout_entry_payload_bytes={} mapped_parts={} fallback_parts={}",
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
        metrics.fallback_parts
    );
    // Speed-of-light accounting (see conventions/SPEED_OF_LIGHT.md, O3).
    // No absolute light is computed in-app; compare this rate to the best
    // demonstrated hash run for the same storage path.
    info!(
        "{}",
        sol_line(
            "hash",
            metrics.hashed_bytes,
            wall_elapsed,
            &SolLight::SelfBaseline,
            &[
                ("label", label.to_string()),
                ("files", metrics.files.to_string()),
                ("parts", metrics.parts.to_string()),
                (
                    "compute_s",
                    format!("{:.3}", metrics.blocking_hash_elapsed_sum.as_secs_f64()),
                ),
                (
                    "wait_s",
                    format!("{:.3}", metrics.semaphore_wait_elapsed_sum.as_secs_f64()),
                ),
            ],
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

pub(super) async fn recalculate_parts_for_jobs_with_profile(
    mut jobs: Vec<FileHashJob>,
    requested_profile: HashIoProfilePreference,
    sticky_auto_profile: Option<HashIoProfilePreference>,
    progress_tx: Option<&Sender<ProgressEvent>>,
    total_files: usize,
    cancel_rx: Option<&watch::Receiver<bool>>,
) -> (Vec<FileHashResult>, HashProfileDecision, bool) {
    let total_parts: usize = jobs.iter().map(|job| job.indexed_parts.len()).sum();
    let resource_profile = ResourceProfile::sample();
    let storage_class = detect_hash_storage_class(&jobs);
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
            limits.file_concurrency,
            limits.global_part_concurrency,
            progress_tx,
            HashRunProgress::new(total_files, total_parts),
            cancel_rx,
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics("sticky_auto", profile, run_started.elapsed(), &results);
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
            limits.file_concurrency,
            limits.global_part_concurrency,
            progress_tx,
            HashRunProgress::new(total_files, total_parts),
            cancel_rx,
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics(
            "manual_profile",
            effective_profile,
            run_started.elapsed(),
            &results,
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

    let benchmark_jobs = split_benchmark_jobs(&mut jobs);
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
            initial_limits.file_concurrency,
            initial_limits.global_part_concurrency,
            progress_tx,
            HashRunProgress::new(total_files, total_parts),
            cancel_rx,
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics(
            "auto_heuristic",
            initial_profile,
            run_started.elapsed(),
            &results,
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
    let benchmark_profiles = benchmark_profiles_for_environment(
        initial_profile,
        resource_profile,
        storage_class,
        benchmark_jobs.len(),
        benchmark_total_parts,
    );
    let benchmark_file_count = benchmark_jobs.len();
    let benchmark_started = Instant::now();
    let mut best_results = None;
    let mut best_profile = initial_profile;
    let mut best_throughput = 0.0f64;
    let mut best_hashed_bytes = 0u64;
    let mut boost_remaining = false;
    if let Some(tx) = progress_tx {
        let _ = tx.send(ProgressEvent::Stage {
            label: "Hashing profile".to_string(),
            percent: 0.21,
        });
    }

    for profile in benchmark_profiles {
        if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return (
                Vec::new(),
                HashProfileDecision {
                    requested: HashIoProfilePreference::Auto,
                    selected: initial_profile,
                    reason: "cancelled during auto benchmark".to_string(),
                    benchmarked_files: 0,
                    benchmarked_bytes: 0,
                    benchmark_elapsed: benchmark_started.elapsed(),
                    sticky: false,
                },
                true,
            );
        }
        let sample_jobs = benchmark_jobs.clone();
        let limits = hash_scheduler_limits_for_environment(
            sample_jobs.len(),
            benchmark_total_parts,
            profile,
            resource_profile,
            storage_class,
        );
        log_hash_scheduler_selection("auto_benchmark", requested_profile, profile, &limits);
        let profile_started = Instant::now();
        let results = recalculate_parts_for_jobs(
            sample_jobs,
            limits.file_concurrency,
            limits.global_part_concurrency,
            None,
            HashRunProgress::new(total_files, total_parts),
            cancel_rx,
        )
        .await;
        let (mut results, cancelled) = results;
        if cancelled {
            return (
                results,
                HashProfileDecision {
                    requested: HashIoProfilePreference::Auto,
                    selected: profile,
                    reason: "cancelled during auto benchmark".to_string(),
                    benchmarked_files: 0,
                    benchmarked_bytes: 0,
                    benchmark_elapsed: benchmark_started.elapsed(),
                    sticky: false,
                },
                true,
            );
        }
        let elapsed = profile_started.elapsed();
        let elapsed_secs = elapsed.as_secs_f64().max(0.001);
        let metrics = HashRunMetrics::from_results(&results);
        let throughput = metrics.hashed_bytes as f64 / elapsed_secs;
        log_hash_run_metrics("auto_benchmark_sample", profile, elapsed, &results);
        info!(
            "Hash profile auto benchmark sample: profile={} files={} missing_files={} parts={} estimated_bytes={} hashed_bytes={} elapsed={:.2}s throughput={:.2} MB/s limits={}/{} wait_ratio={:.4}",
            profile,
            results.len(),
            metrics.missing_files,
            benchmark_total_parts,
            benchmark_bytes,
            metrics.hashed_bytes,
            elapsed_secs,
            throughput / (1024.0 * 1024.0),
            limits.file_concurrency,
            limits.global_part_concurrency,
            benchmark_wait_ratio(&metrics)
        );
        if !benchmark_metrics_are_sufficient(&metrics) {
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
        if throughput > best_throughput {
            boost_remaining = benchmark_supports_boosted_aggressive(
                profile,
                &metrics,
                storage_class,
                resource_profile,
            );
            best_throughput = throughput;
            best_profile = profile;
            best_hashed_bytes = metrics.hashed_bytes;
            best_results = Some(std::mem::take(&mut results));
        }
    }

    let Some(mut benchmark_results) = best_results else {
        warn!(
            "Hash profile auto benchmark produced no valid sample; using storage heuristic profile={}",
            initial_profile
        );
        jobs.extend(benchmark_jobs);
        let run_started = Instant::now();
        let results = recalculate_parts_for_jobs(
            jobs,
            initial_limits.file_concurrency,
            initial_limits.global_part_concurrency,
            progress_tx,
            HashRunProgress::new(total_files, total_parts),
            cancel_rx,
        )
        .await;
        let (results, cancelled) = results;
        log_hash_run_metrics(
            "auto_heuristic",
            initial_profile,
            run_started.elapsed(),
            &results,
        );
        return (
            results,
            HashProfileDecision {
                requested: HashIoProfilePreference::Auto,
                selected: initial_profile,
                reason: format!("auto benchmark invalid; {initial_reason}"),
                benchmarked_files: 0,
                benchmarked_bytes: 0,
                benchmark_elapsed: std::time::Duration::ZERO,
                sticky: false,
            },
            cancelled,
        );
    };

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
        best_hashed_bytes,
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
        });
    }
    let remaining_started = Instant::now();
    let remaining_results = recalculate_parts_for_jobs(
        jobs,
        remaining_limits.file_concurrency,
        remaining_limits.global_part_concurrency,
        progress_tx,
        HashRunProgress {
            total_files,
            total_parts,
            initial_files_done: benchmark_file_count,
            initial_parts_done: benchmark_total_parts,
        },
        cancel_rx,
    )
    .await;
    let (mut remaining_results, cancelled) = remaining_results;
    log_hash_run_metrics(
        "auto_selected_remaining",
        best_profile,
        remaining_started.elapsed(),
        &remaining_results,
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
            benchmarked_bytes: best_hashed_bytes,
            benchmark_elapsed,
            sticky: true,
        },
        cancelled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(parts: usize, bytes_per_part: u64) -> FileHashJob {
        FileHashJob {
            file_idx: 0,
            file_path: String::new(),
            file_length: (parts as u64).saturating_mul(bytes_per_part),
            file_remote_checksum: String::new(),
            indexed_parts: (0..parts)
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
            span_source: PartSpanSource::DetectLocalLayout,
        }
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

    #[tokio::test]
    async fn whole_file_checksum_hashes_no_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.bin");
        std::fs::write(&path, b"hello").unwrap();
        let expected = blake3::hash(b"hello").to_hex().to_uppercase();

        let (checksum, metrics) = calculate_whole_file_checksum(
            path.to_string_lossy().to_string(),
            expected.clone(),
            5,
            Arc::new(Semaphore::new(1)),
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

        let (checksum, metrics) = calculate_whole_file_checksum(
            path.to_string_lossy().to_string(),
            expected,
            10,
            Arc::new(Semaphore::new(1)),
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
        let selected = split_benchmark_jobs(&mut jobs);
        assert_eq!(selected.len(), 5);
        assert!(job_estimated_bytes(&selected[0]) >= job_estimated_bytes(&selected[1]));
        assert_eq!(selected[0].indexed_parts.len(), 64);
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
    fn hash_cpu_budget_is_at_least_one() {
        assert!(hash_cpu_budget() >= 1);
    }

    // ── job_estimated_bytes ─────────────────────────────────────────────

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
