//! CPU-thread planning is app-global and survives game-space switches; the explicit setting overrides detection.

use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use log::info;

pub const MAX_CPU_THREADS: u32 = 1024;

static CPU_THREAD_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

pub fn detected_cpu_threads() -> Option<usize> {
    static DETECTED: OnceLock<Option<usize>> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        std::thread::available_parallelism()
            .ok()
            .map(NonZeroUsize::get)
    })
}

/// None means Auto detection failed; callers retain their own fallback.
pub fn cpu_threads() -> Option<usize> {
    match CPU_THREAD_OVERRIDE.load(Ordering::Relaxed) {
        0 => detected_cpu_threads(),
        threads => Some(threads),
    }
}

/// Apply the user's setting: `None` is Auto. With `safe_limits` the count is
/// kept within the detected processors.
pub fn set_cpu_thread_limit(limit: Option<u32>, safe_limits: bool) {
    let detected = detected_cpu_threads();
    let applied = resolve_cpu_thread_override(limit, safe_limits, detected);
    let stored = applied.unwrap_or(0);
    if CPU_THREAD_OVERRIDE.swap(stored, Ordering::Relaxed) == stored {
        return;
    }
    match applied {
        Some(threads) => info!(
            "CPU threads set to {} (requested={:?} safe_limits={} detected={:?})",
            threads, limit, safe_limits, detected
        ),
        None => info!("CPU threads set to auto (detected={:?})", detected),
    }
}

pub fn cpu_thread_limit_max(safe_limits: bool, detected: Option<usize>) -> u32 {
    match detected {
        Some(detected) if safe_limits => u32::try_from(detected)
            .unwrap_or(MAX_CPU_THREADS)
            .clamp(1, MAX_CPU_THREADS),
        _ => MAX_CPU_THREADS,
    }
}

fn resolve_cpu_thread_override(
    limit: Option<u32>,
    safe_limits: bool,
    detected: Option<usize>,
) -> Option<usize> {
    let requested = limit?.clamp(1, cpu_thread_limit_max(safe_limits, detected));
    usize::try_from(requested).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_applies_no_override() {
        assert_eq!(resolve_cpu_thread_override(None, true, Some(8)), None);
        assert_eq!(resolve_cpu_thread_override(None, false, Some(8)), None);
    }

    #[test]
    fn safe_limits_keep_the_count_within_detected_processors() {
        assert_eq!(
            resolve_cpu_thread_override(Some(32), true, Some(8)),
            Some(8)
        );
        assert_eq!(resolve_cpu_thread_override(Some(4), true, Some(8)), Some(4));
    }

    #[test]
    fn unsafe_limits_accept_counts_above_detected_processors() {
        assert_eq!(
            resolve_cpu_thread_override(Some(32), false, Some(8)),
            Some(32)
        );
        assert_eq!(
            resolve_cpu_thread_override(Some(u32::MAX), false, Some(8)),
            Some(MAX_CPU_THREADS as usize)
        );
    }

    #[test]
    fn zero_is_raised_to_one_thread() {
        assert_eq!(resolve_cpu_thread_override(Some(0), true, Some(8)), Some(1));
        assert_eq!(
            resolve_cpu_thread_override(Some(0), false, Some(8)),
            Some(1)
        );
    }

    #[test]
    fn safe_limits_without_detection_fall_back_to_the_wide_ceiling() {
        assert_eq!(resolve_cpu_thread_override(Some(48), true, None), Some(48));
        assert_eq!(cpu_thread_limit_max(true, None), MAX_CPU_THREADS);
    }

    #[test]
    fn control_ceiling_follows_safe_limits() {
        assert_eq!(cpu_thread_limit_max(true, Some(12)), 12);
        assert_eq!(cpu_thread_limit_max(false, Some(12)), MAX_CPU_THREADS);
        assert_eq!(cpu_thread_limit_max(true, Some(0)), 1);
    }
}
