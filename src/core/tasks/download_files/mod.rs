mod bandwidth;
mod batching;
mod metrics;
mod orchestrator;
mod progress;
mod range_scheduler;
mod rollback;
mod transfer;

pub(crate) use bandwidth::AdaptiveBandwidthLimiter;
pub(crate) use batching::DownloadModCompletion;
pub(crate) use metrics::{DownloadMetrics, DownloadRunReport};
pub(crate) use orchestrator::{
    apply_download_plan_bytes, build_download_estimate_diffs, download_files,
};
pub(crate) use rollback::{SharedRollbackSession, UpdateRollbackSession};

pub(super) const LARGE_FILE_THRESHOLD: usize = 10 * 1024 * 1024;
pub(super) const ATTEMPT_DELAY_MS: u64 = 25;
pub(super) const ATTEMPT_LIMIT: u8 = 50;
pub(super) const BUFFERED_WRITE_CAPACITY: usize = 4 * 1024 * 1024;
pub(super) const MAXIMUM_LARGE_FILES: usize = 24;
pub(super) const MAXIMUM_SMALL_FILES: usize = 48;
pub(super) const MAX_FILE_RETRIES: usize = 3;
pub(super) const BYTES_PER_MEGABIT: u64 = 125_000;
pub(super) const RAMP_START_DIVISOR: u64 = 4;
pub(super) const RAMP_MIN_START_BYTES_PER_SEC: u64 = 128 * 1024;
pub(super) const RAMP_MIN_STEP_BYTES_PER_SEC: u64 = 128 * 1024;
pub(super) const RAMP_STEP_PERCENT: u64 = 8;
pub(super) const RAMP_GAP_DIVISOR: u64 = 16;
pub(super) const RAMP_UTILIZATION_THRESHOLD_PERCENT: u64 = 85;
pub(super) const RAMP_HISTORY_HEADROOM_PERCENT: u64 = 15;
pub(super) const RAMP_ADJUST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
pub(super) const SPEED_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
pub(super) const SPEED_SAMPLE_HISTORY_CAPACITY: usize = 10;

// ── Adaptive range concurrency ─────────────────────────────────────────
/// Global cap on concurrent HTTP range requests across all files.
pub(super) const MAX_ACTIVE_RANGE_REQUESTS: usize = 96;
/// Per-file range floor: minimum parallel ranges a large file gets even when
/// many large files compete for the global range budget.
pub(super) const MIN_RANGES_PER_FILE: usize = 8;
/// Per-file range ceiling: parallel ranges a large file may use when it has
/// the global range budget mostly to itself (tail of a run, single-file jobs).
pub(super) const MAX_RANGES_PER_FILE: usize = 48;
/// Target chunk size per range request. Small enough that the tail of a run
/// and single-file downloads can spread one file across many connections.
pub(super) const RANGE_CHUNK_TARGET: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct DownloadResourceLimits {
    pub(super) max_large_files: usize,
    pub(super) max_small_files: usize,
    pub(super) max_active_range_requests: usize,
    pub(super) min_ranges_per_file: usize,
    pub(super) max_ranges_per_file: usize,
    pub(super) range_chunk_target: usize,
}

impl DownloadResourceLimits {
    pub(super) const fn normal() -> Self {
        Self {
            max_large_files: MAXIMUM_LARGE_FILES,
            max_small_files: MAXIMUM_SMALL_FILES,
            max_active_range_requests: MAX_ACTIVE_RANGE_REQUESTS,
            min_ranges_per_file: MIN_RANGES_PER_FILE,
            max_ranges_per_file: MAX_RANGES_PER_FILE,
            range_chunk_target: RANGE_CHUNK_TARGET,
        }
    }

    pub(super) const fn constrained() -> Self {
        Self {
            max_large_files: 4,
            max_small_files: 12,
            max_active_range_requests: 16,
            min_ranges_per_file: 4,
            max_ranges_per_file: 8,
            range_chunk_target: 16 * 1024 * 1024,
        }
    }

    pub(super) const fn severe() -> Self {
        Self {
            max_large_files: 1,
            max_small_files: 4,
            max_active_range_requests: 4,
            min_ranges_per_file: 2,
            max_ranges_per_file: 4,
            range_chunk_target: 64 * 1024 * 1024,
        }
    }
}
