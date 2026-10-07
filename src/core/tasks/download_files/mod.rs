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
pub(super) const MAXIMUM_LARGE_FILES: usize = 12;
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
/// Matches the global budget so the last file in a run can use all of it.
pub(super) const MAX_RANGES_PER_FILE: usize = MAX_ACTIVE_RANGE_REQUESTS;
/// Largest chunk size per range request. This bounds the tail of a run: when
/// the queue is empty the link is carried by whatever chunks are still in
/// flight, and a lone chunk moves at one connection's ~1.6 MB/s.
pub(super) const RANGE_CHUNK_TARGET: usize = 2 * 1024 * 1024;
/// Smallest chunk size per range request. The grid shrinks towards this so a
/// file still has one chunk per worker; measured against the reference origin,
/// the extra round trips cost under 2% even at low concurrency.
pub(super) const MIN_RANGE_CHUNK: usize = 1024 * 1024;

/// Concurrent delta-patch applies on a rotational destination. Each apply is
/// a sequential read of the old file interleaved with a sequential write of
/// the new one; more than a couple at once turns both into seeks.
pub(super) const ROTATIONAL_MAX_PATCH_APPLIES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DownloadResourceLimits {
    pub(super) max_large_files: usize,
    pub(super) max_small_files: usize,
    pub(super) max_active_range_requests: usize,
    pub(super) min_ranges_per_file: usize,
    pub(super) max_ranges_per_file: usize,
    pub(super) range_chunk_target: usize,
    pub(super) min_range_chunk: usize,
    /// Delta-patch applies allowed to run at once. Applies are disk-bound, so
    /// the cap follows the destination's storage class rather than memory.
    pub(super) max_patch_applies: usize,
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
            min_range_chunk: MIN_RANGE_CHUNK,
            max_patch_applies: MAXIMUM_LARGE_FILES + MAXIMUM_SMALL_FILES,
        }
    }

    /// Rotational destination: network limits stay as on SSD, because aggregate
    /// throughput is bought with connections and fewer files in flight starves
    /// the link long before range writes seek-bound the disk. Only the
    /// seek-bound patch applies are capped.
    pub(super) const fn rotational() -> Self {
        Self::normal().with_rotational_destination()
    }

    pub(super) const fn constrained() -> Self {
        Self {
            max_large_files: 4,
            max_small_files: 12,
            max_active_range_requests: 16,
            min_ranges_per_file: 4,
            max_ranges_per_file: 8,
            range_chunk_target: 16 * 1024 * 1024,
            min_range_chunk: 4 * 1024 * 1024,
            max_patch_applies: 16,
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
            min_range_chunk: 16 * 1024 * 1024,
            max_patch_applies: 5,
        }
    }

    /// Memory pressure keeps its conservative profile on any disk; on a
    /// rotational destination it only tightens the apply cap further.
    pub(super) const fn with_rotational_destination(mut self) -> Self {
        if self.max_patch_applies > ROTATIONAL_MAX_PATCH_APPLIES {
            self.max_patch_applies = ROTATIONAL_MAX_PATCH_APPLIES;
        }
        self
    }
}
