//! Processor thread count Foxy plans its CPU-bound fan-out around. Auto uses
//! what the system reports; the user's CPU threads setting overrides it for
//! the whole process (an app-global setting, so it survives space switches).

use std::num::NonZeroUsize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use log::info;

/// Highest thread count the setting accepts with safe limits turned off. Every
/// fan-out already clamps well below this, so larger values change nothing.
pub const MAX_CPU_THREADS: u32 = 1024;

/// Applied override; 0 means Auto.
static CPU_THREAD_OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// Logical processors the system reports, ignoring the user's setting. Read
/// once: views and the CPU usage limit ask for it many times a second.
pub fn detected_cpu_threads() -> Option<usize> {
    static DETECTED: OnceLock<Option<usize>> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        std::thread::available_parallelism()
            .ok()
            .map(NonZeroUsize::get)
    })
}

/// Thread count CPU-bound work plans around: the applied setting, or the
/// detected processors on Auto. `None` only when Auto cannot detect them, so
/// each caller keeps its own fallback.
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

/// Largest value the settings control offers: the detected processors with
/// safe limits on, otherwise [`MAX_CPU_THREADS`].
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
