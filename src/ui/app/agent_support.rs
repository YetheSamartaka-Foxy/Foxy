//! Process-global state shared between the agent-gui driver
//! ([`super::agent_driver`]) and ordinary UI code paths that must consult it
//! without holding a `Foxy` reference: the native-dialog interception slot
//! (the `dialog` command) and the driver-controlled virtual clock (the `clock`
//! command).
//!
//! Everything here is intentionally tiny and lock-light so the UI thread never
//! blocks on it. All of it is inert when the agent-gui driver is not running:
//! [`agent_gui_active`] starts `false`, the clock offset starts at zero and is
//! never frozen, and no dialog response is queued - so a normal user run sees
//! real wall-clock time and real native pickers.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;

/// True once the agent-gui driver has started this process. Gates dialog
/// interception so a normal user run never has its file pickers hijacked.
static AGENT_GUI_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Arbitrary monotonic epoch for the virtual clock's freeze bookkeeping.
static PROCESS_START: Lazy<Instant> = Lazy::new(Instant::now);

pub fn set_agent_gui_active(active: bool) {
    AGENT_GUI_ACTIVE.store(active, Ordering::Relaxed);
}

pub fn agent_gui_active() -> bool {
    AGENT_GUI_ACTIVE.load(Ordering::Relaxed)
}

// ── Native dialog interception (the `dialog` command) ───────────────────────

/// A response pre-registered for the next native file/folder picker.
#[derive(Clone, Debug)]
pub enum QueuedDialog {
    /// Return this path from the next picker (as if the user picked it).
    Path(PathBuf),
    /// Cancel the next picker (as if the user dismissed it).
    Cancel,
}

static QUEUED_DIALOG: Mutex<Option<QueuedDialog>> = Mutex::new(None);
/// Set while the app is actually blocked inside a real native picker.
static DIALOG_OPEN: AtomicBool = AtomicBool::new(false);
/// Count of native pickers the queue has intercepted, so `dialog pending` can
/// report whether the slot has been consumed.
static DIALOG_INTERCEPTED: AtomicU64 = AtomicU64::new(0);

pub fn queue_dialog_response(response: QueuedDialog) {
    if let Ok(mut slot) = QUEUED_DIALOG.lock() {
        *slot = Some(response);
    }
}

pub fn clear_dialog_response() {
    if let Ok(mut slot) = QUEUED_DIALOG.lock() {
        *slot = None;
    }
}

/// Peek the queued response without consuming it (for `dialog pending`).
pub fn dialog_queued() -> Option<QueuedDialog> {
    QUEUED_DIALOG.lock().ok().and_then(|slot| slot.clone())
}

pub fn dialog_open() -> bool {
    DIALOG_OPEN.load(Ordering::Relaxed)
}

pub fn dialog_intercepted_count() -> u64 {
    DIALOG_INTERCEPTED.load(Ordering::Relaxed)
}

fn take_queued() -> Option<QueuedDialog> {
    QUEUED_DIALOG.lock().ok().and_then(|mut slot| slot.take())
}

/// Consult the queued response before spawning a real native picker.
///
/// When agent-gui mode is active and a response is queued, returns it instead
/// of blocking the UI thread on a native dialog the headless harness can never
/// see or dismiss. Otherwise runs `real` (the actual `rfd` picker) and tracks
/// the open/closed window around it.
fn intercept_single(real: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    if agent_gui_active()
        && let Some(queued) = take_queued()
    {
        DIALOG_INTERCEPTED.fetch_add(1, Ordering::Relaxed);
        return match queued {
            QueuedDialog::Path(path) => Some(path),
            QueuedDialog::Cancel => None,
        };
    }
    DIALOG_OPEN.store(true, Ordering::Relaxed);
    let result = real();
    DIALOG_OPEN.store(false, Ordering::Relaxed);
    result
}

/// Folder picker wrapper. See [`intercept_single`].
pub fn pick_folder(real: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    intercept_single(real)
}

/// Single-file picker (open) wrapper. See [`intercept_single`].
pub fn pick_file(real: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    intercept_single(real)
}

/// Save-file picker wrapper. See [`intercept_single`].
pub fn save_file(real: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    intercept_single(real)
}

/// Multi-file picker wrapper. A queued single path is returned as a one-element
/// list; a queued cancel returns `None`. Part of the complete picker-wrapper
/// surface; no multi-select call site exists yet.
#[allow(dead_code)]
pub fn pick_files(real: impl FnOnce() -> Option<Vec<PathBuf>>) -> Option<Vec<PathBuf>> {
    if agent_gui_active()
        && let Some(queued) = take_queued()
    {
        DIALOG_INTERCEPTED.fetch_add(1, Ordering::Relaxed);
        return match queued {
            QueuedDialog::Path(path) => Some(vec![path]),
            QueuedDialog::Cancel => None,
        };
    }
    DIALOG_OPEN.store(true, Ordering::Relaxed);
    let result = real();
    DIALOG_OPEN.store(false, Ordering::Relaxed);
    result
}

// ── Virtual clock (the `clock` command) ─────────────────────────────────────

/// Virtual time added on top of real elapsed time (milliseconds).
static CLOCK_OFFSET_MS: AtomicU64 = AtomicU64::new(0);
/// True while logical time is paused.
static CLOCK_FROZEN: AtomicBool = AtomicBool::new(false);
/// Real milliseconds (since [`PROCESS_START`]) captured when freezing began;
/// only meaningful while `CLOCK_FROZEN` is set.
static CLOCK_FREEZE_REAL_MS: AtomicU64 = AtomicU64::new(0);

fn real_millis() -> u64 {
    PROCESS_START.elapsed().as_millis() as u64
}

/// Jump logical time forward by `ms`, firing time-based behaviors on demand.
pub fn clock_advance(ms: u64) {
    CLOCK_OFFSET_MS.fetch_add(ms, Ordering::Relaxed);
}

/// Pause logical time: virtual elapsed stops advancing until [`clock_resume`].
pub fn clock_freeze() {
    if !CLOCK_FROZEN.swap(true, Ordering::Relaxed) {
        CLOCK_FREEZE_REAL_MS.store(real_millis(), Ordering::Relaxed);
    }
}

/// Resume logical time without a jump: the wall-clock interval spent frozen is
/// removed from the offset so virtual time continues where it paused.
pub fn clock_resume() {
    if CLOCK_FROZEN.swap(false, Ordering::Relaxed) {
        let frozen_for = real_millis().saturating_sub(CLOCK_FREEZE_REAL_MS.load(Ordering::Relaxed));
        let offset = CLOCK_OFFSET_MS.load(Ordering::Relaxed);
        CLOCK_OFFSET_MS.store(offset.saturating_sub(frozen_for), Ordering::Relaxed);
    }
}

/// Virtual elapsed since `base`: real elapsed plus the advance offset, with
/// forward progress paused while frozen. With the default (offset 0, not
/// frozen) this equals `base.elapsed()`, so non-agent runs are unaffected.
pub fn virtual_elapsed(base: Instant) -> Duration {
    let real = base.elapsed();
    let offset = Duration::from_millis(CLOCK_OFFSET_MS.load(Ordering::Relaxed));
    let mut total = real.saturating_add(offset);
    if CLOCK_FROZEN.load(Ordering::Relaxed) {
        let frozen_for = Duration::from_millis(
            real_millis().saturating_sub(CLOCK_FREEZE_REAL_MS.load(Ordering::Relaxed)),
        );
        total = total.saturating_sub(frozen_for);
    }
    total
}

/// `(offset_ms, frozen)` for the `clock` response.
pub fn clock_state() -> (u64, bool) {
    (
        CLOCK_OFFSET_MS.load(Ordering::Relaxed),
        CLOCK_FROZEN.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_adds_to_virtual_elapsed() {
        // Use a fresh base and a large advance so the real elapsed delta during
        // the test is dwarfed by the injected offset.
        clock_resume();
        CLOCK_OFFSET_MS.store(0, Ordering::Relaxed);
        let base = Instant::now();
        clock_advance(10_000);
        assert!(virtual_elapsed(base) >= Duration::from_millis(10_000));
        CLOCK_OFFSET_MS.store(0, Ordering::Relaxed);
    }

    #[test]
    fn queued_cancel_is_consumed_once() {
        set_agent_gui_active(true);
        queue_dialog_response(QueuedDialog::Cancel);
        assert!(dialog_queued().is_some());
        let first = pick_folder(|| Some(PathBuf::from("real")));
        assert_eq!(first, None, "queued cancel should suppress the real picker");
        assert!(dialog_queued().is_none(), "slot consumed after one use");
        set_agent_gui_active(false);
    }
}
