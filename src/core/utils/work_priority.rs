//! Scheduling priority of the threads that hash, scan, check, and download.
//! Only those threads are lowered: the process priority class stays normal, so
//! the interface and every program Foxy launches (the game, Steam, TeamSpeak,
//! a browser) keep normal priority instead of inheriting a lowered class.

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

use log::info;

use crate::ui::types::WorkPriorityPreference;

const LOW_DISK_PRIORITY: u8 = 0x80;
const LEVEL_MASK: u8 = 0x7f;

/// Requested level and disk mode, packed; threads compare it with what they
/// last applied.
static REQUESTED: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// New threads start at normal priority outside background mode, which is
    /// what the packed value 0 means.
    static APPLIED: Cell<u8> = const { Cell::new(0) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PriorityLevel {
    Normal,
    BelowNormal,
    Lowest,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ThreadChange {
    /// `Some(true)` enters background mode, `Some(false)` leaves it.
    background_mode: Option<bool>,
    level: Option<PriorityLevel>,
}

fn encode(priority: WorkPriorityPreference, low_disk_priority: bool) -> u8 {
    let level = match priority {
        WorkPriorityPreference::Normal => 0,
        WorkPriorityPreference::BelowNormal => 1,
        WorkPriorityPreference::Lowest => 2,
    };
    level
        | if low_disk_priority {
            LOW_DISK_PRIORITY
        } else {
            0
        }
}

fn decode_level(packed: u8) -> PriorityLevel {
    match packed & LEVEL_MASK {
        1 => PriorityLevel::BelowNormal,
        2 => PriorityLevel::Lowest,
        _ => PriorityLevel::Normal,
    }
}

/// What a thread at `previous` must change to reach `requested`. Background
/// mode is switched first and the level is set after it, so the CPU level the
/// user picked wins over whatever background mode does to it.
fn thread_change(previous: u8, requested: u8) -> ThreadChange {
    if previous == requested {
        return ThreadChange::default();
    }
    let low_disk = requested & LOW_DISK_PRIORITY != 0;
    let background_mode = ((previous & LOW_DISK_PRIORITY != 0) != low_disk).then_some(low_disk);
    ThreadChange {
        background_mode,
        level: Some(decode_level(requested)),
    }
}

/// Apply the user's settings to work threads. Running threads pick the change
/// up the next time they wake; new threads start with it.
pub fn set_work_priority(priority: WorkPriorityPreference, low_disk_priority: bool) {
    let requested = encode(priority, low_disk_priority);
    if REQUESTED.swap(requested, Ordering::Relaxed) != requested {
        info!(
            "Work priority set to {} (low_disk_priority={})",
            priority, low_disk_priority
        );
    }
}

/// Bring the calling thread to the requested priority. One atomic load when
/// nothing changed, so runtimes call it every time a worker wakes.
pub fn apply_to_current_thread() {
    let requested = REQUESTED.load(Ordering::Relaxed);
    APPLIED.with(|applied| {
        let change = thread_change(applied.get(), requested);
        if change != ThreadChange::default() {
            apply_os(change);
            applied.set(requested);
        }
    });
}

/// Multi-thread runtime whose threads follow the work priority settings.
pub fn work_runtime_builder() -> tokio::runtime::Builder {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder
        .enable_all()
        .on_thread_start(apply_to_current_thread)
        .on_thread_unpark(apply_to_current_thread);
    builder
}

#[cfg(windows)]
fn apply_os(change: ThreadChange) {
    use winapi::um::processthreadsapi::{GetCurrentThread, SetThreadPriority};
    use winapi::um::winbase::{
        THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END, THREAD_PRIORITY_BELOW_NORMAL,
        THREAD_PRIORITY_IDLE, THREAD_PRIORITY_NORMAL,
    };

    unsafe {
        let thread = GetCurrentThread();
        if let Some(enter) = change.background_mode {
            let mode = if enter {
                THREAD_MODE_BACKGROUND_BEGIN
            } else {
                THREAD_MODE_BACKGROUND_END
            };
            SetThreadPriority(thread, mode as i32);
        }
        if let Some(level) = change.level {
            let priority = match level {
                PriorityLevel::Normal => THREAD_PRIORITY_NORMAL,
                PriorityLevel::BelowNormal => THREAD_PRIORITY_BELOW_NORMAL,
                PriorityLevel::Lowest => THREAD_PRIORITY_IDLE,
            };
            SetThreadPriority(thread, priority as i32);
        }
    }
}

#[cfg(not(windows))]
fn apply_os(_change: ThreadChange) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_leave_threads_untouched() {
        let defaults = encode(WorkPriorityPreference::Normal, false);
        assert_eq!(defaults, 0);
        assert_eq!(thread_change(0, defaults), ThreadChange::default());
    }

    #[test]
    fn lowering_the_level_sets_only_the_level() {
        let requested = encode(WorkPriorityPreference::Lowest, false);
        assert_eq!(
            thread_change(0, requested),
            ThreadChange {
                background_mode: None,
                level: Some(PriorityLevel::Lowest),
            }
        );
    }

    #[test]
    fn low_disk_priority_enters_background_mode_then_keeps_the_level() {
        let requested = encode(WorkPriorityPreference::BelowNormal, true);
        assert_eq!(
            thread_change(0, requested),
            ThreadChange {
                background_mode: Some(true),
                level: Some(PriorityLevel::BelowNormal),
            }
        );
    }

    #[test]
    fn turning_low_disk_priority_off_leaves_background_mode() {
        let previous = encode(WorkPriorityPreference::Normal, true);
        let requested = encode(WorkPriorityPreference::Normal, false);
        assert_eq!(
            thread_change(previous, requested),
            ThreadChange {
                background_mode: Some(false),
                level: Some(PriorityLevel::Normal),
            }
        );
    }

    #[test]
    fn changing_the_level_in_background_mode_does_not_reenter_it() {
        let previous = encode(WorkPriorityPreference::Normal, true);
        let requested = encode(WorkPriorityPreference::Lowest, true);
        assert_eq!(
            thread_change(previous, requested),
            ThreadChange {
                background_mode: None,
                level: Some(PriorityLevel::Lowest),
            }
        );
    }
}
