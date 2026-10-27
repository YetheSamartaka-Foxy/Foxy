//! Keeps Foxy's processor use at or under the CPU usage limit setting, a share
//! of the whole computer. Work threads that hash and patch call [`checkpoint`]
//! between chunks. Once the process nears the limit, each checkpoint books the
//! CPU time its thread used since the last one on a schedule shared by every
//! work thread, which advances at the CPU rate the limit leaves for that work,
//! and the thread waits until the schedule catches up. The total is held
//! however many threads the work runs on, including pools far wider than the
//! processor count whose threads mostly wait on the disk.
//! CPU time comes from the OS clocks in `thread_cpu`; on a platform without
//! them nothing waits.

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use log::info;

pub const DEFAULT_CPU_USAGE_LIMIT_PERCENT: u8 = 90;
pub const MIN_CPU_USAGE_LIMIT_PERCENT: u8 = 10;
pub const MAX_CPU_USAGE_LIMIT_PERCENT: u8 = 100;

/// How long one measurement of the process's CPU rate covers. Windows charges
/// CPU time in clock ticks of about 15.6 ms per processor, so a shorter window
/// is mostly rounding.
const MEASURE_WINDOW: Duration = Duration::from_millis(250);
/// While work is not paced the rate is checked this often, so a burst past the
/// limit runs unpaced for a tenth of a second at most.
const ENGAGE_WINDOW: Duration = Duration::from_millis(100);
/// Share of the budget the process must use before work is paced at all, so
/// work well under the limit never reads a thread clock.
const ENGAGE_SHARE: f64 = 0.75;
/// Work keeps at least a tenth of the budget however much CPU the rest of the
/// process uses.
const MIN_ALLOWANCE_SHARE: f64 = 0.1;
/// How far the schedule may run ahead before a thread waits, so a short burst
/// passes untouched.
const BURST: Duration = Duration::from_millis(20);
const MIN_PAUSE: Duration = Duration::from_millis(2);
/// Waits are slept in slices so a lifted limit or the end of pacing frees the
/// thread quickly.
const PAUSE_SLICE: Duration = Duration::from_millis(50);
/// Longest single wait; a schedule further ahead than this is forgiven.
const MAX_PAUSE: Duration = Duration::from_secs(1);
/// A thread that has not checked in for this long starts counting afresh.
const STALE_CHECKPOINT: Duration = Duration::from_secs(1);

/// Applied limit in percent; 0 means no limit.
static LIMIT_PERCENT: AtomicU32 = AtomicU32::new(0);
/// CPU the paced work may use, in thousandths of a processor thread; 0 means
/// work is not paced.
static ALLOWANCE_MILLI: AtomicU32 = AtomicU32::new(0);
/// Nanoseconds after [`epoch`] by which every booked CPU time is paid for at
/// the allowance.
static SCHEDULE_NS: AtomicU64 = AtomicU64::new(0);
/// CPU time booked by paced work since the process started.
static BOOKED_NS: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds after [`epoch`] when the CPU rate is next measured.
static NEXT_MEASURE_NS: AtomicU64 = AtomicU64::new(0);
static PAUSED_NS: AtomicU64 = AtomicU64::new(0);
static PAUSES: AtomicU64 = AtomicU64::new(0);
static CONTROLLER: Mutex<Option<Measurement>> = Mutex::new(None);

#[derive(Clone, Copy, Debug)]
struct Measurement {
    at: Instant,
    process_cpu: Duration,
    booked: Duration,
    /// Work was paced for the whole window that starts here.
    pacing: bool,
    /// Processor threads' worth of CPU the process uses outside checkpoints.
    unpaced: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ThreadState {
    cpu: Duration,
    seen: Option<Instant>,
}

thread_local! {
    static THREAD: Cell<ThreadState> = const {
        Cell::new(ThreadState {
            cpu: Duration::ZERO,
            seen: None,
        })
    };
}

/// Time spent paused by the limit, and how many pauses, since the process
/// started. Callers take the difference over an operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GovernorCounters {
    pub(crate) paused: Duration,
    pub(crate) pauses: u64,
}

impl GovernorCounters {
    pub(crate) fn since(self, earlier: GovernorCounters) -> GovernorCounters {
        GovernorCounters {
            paused: self.paused.saturating_sub(earlier.paused),
            pauses: self.pauses.saturating_sub(earlier.pauses),
        }
    }
}

pub(crate) fn counters() -> GovernorCounters {
    GovernorCounters {
        paused: Duration::from_nanos(PAUSED_NS.load(Ordering::Relaxed)),
        pauses: PAUSES.load(Ordering::Relaxed),
    }
}

/// The applied limit, `None` when there is none.
pub(crate) fn limit_percent() -> Option<u8> {
    match LIMIT_PERCENT.load(Ordering::Relaxed) {
        0 => None,
        percent => u8::try_from(percent).ok(),
    }
}

/// The setting as stored: kept within the accepted range, with 100 and above
/// meaning no limit.
pub fn sanitize_cpu_usage_limit(limit: Option<u8>) -> Option<u8> {
    limit.map(|percent| percent.clamp(MIN_CPU_USAGE_LIMIT_PERCENT, MAX_CPU_USAGE_LIMIT_PERCENT))
}

fn applied_limit(limit: Option<u8>) -> u32 {
    match sanitize_cpu_usage_limit(limit) {
        Some(percent) if percent < MAX_CPU_USAGE_LIMIT_PERCENT => u32::from(percent),
        _ => 0,
    }
}

/// Processor threads' worth of CPU time per second the limit allows.
pub fn budget_threads(limit_percent: u8, detected_threads: usize) -> f64 {
    detected_threads as f64 * f64::from(limit_percent) / 100.0
}

/// Apply the user's setting: `None` is no limit.
pub fn set_cpu_usage_limit(limit: Option<u8>) {
    let applied = applied_limit(limit);
    if LIMIT_PERCENT.swap(applied, Ordering::Relaxed) == applied {
        return;
    }
    ALLOWANCE_MILLI.store(0, Ordering::Relaxed);
    SCHEDULE_NS.store(0, Ordering::Relaxed);
    NEXT_MEASURE_NS.store(0, Ordering::Relaxed);
    *CONTROLLER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    let detected = super::cpu_threads::detected_cpu_threads();
    match (applied, detected) {
        (0, _) => info!("CPU usage limit off"),
        (percent, Some(threads)) => info!(
            "CPU usage limit set to {}% (budget={:.1} threads detected={} enforced={})",
            percent,
            budget_threads(percent as u8, threads),
            threads,
            enforceable()
        ),
        (percent, None) => info!(
            "CPU usage limit set to {}% but the processor count is unknown; not enforced",
            percent
        ),
    }
}

fn enforceable() -> bool {
    super::thread_cpu::process_cpu().is_some() && super::thread_cpu::current_thread_cpu().is_some()
}

/// Call between chunks of CPU-heavy work on a blocking thread, never on an
/// async worker and never while holding a lock or database transaction others
/// wait on: it may sleep for up to [`MAX_PAUSE`]. One atomic load when no
/// limit is set, and no clock read while the process is well under it.
pub(crate) fn checkpoint() {
    let limit = LIMIT_PERCENT.load(Ordering::Relaxed);
    if limit == 0 {
        return;
    }
    let now = Instant::now();
    measure_if_due(limit, now);
    let allowance = ALLOWANCE_MILLI.load(Ordering::Relaxed);
    let used = THREAD.with(|cell| {
        if allowance == 0 {
            cell.set(ThreadState::default());
            return Duration::ZERO;
        }
        let Some(cpu) = super::thread_cpu::current_thread_cpu() else {
            return Duration::ZERO;
        };
        let used = used_since(cell.get(), cpu, now);
        cell.set(ThreadState {
            cpu,
            seen: Some(now),
        });
        used
    });
    if used.is_zero() {
        return;
    }
    BOOKED_NS.fetch_add(used.as_nanos() as u64, Ordering::Relaxed);
    let cost = scheduled_cost(used, allowance);
    let now_ns = since_epoch(now);
    let mut schedule = SCHEDULE_NS.load(Ordering::Relaxed);
    let wait = loop {
        let (next, wait) = admit(schedule, now_ns, cost);
        match SCHEDULE_NS.compare_exchange_weak(
            schedule,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break wait,
            Err(current) => schedule = current,
        }
    };
    if wait >= MIN_PAUSE {
        pause(limit, wait);
        // A wait near MAX_PAUSE must not make the next chunk look stale and go
        // unbooked, so staleness counts from the end of the wait.
        THREAD.with(|cell| {
            let mut state = cell.get();
            state.seen = Some(Instant::now());
            cell.set(state);
        });
    }
}

/// CPU the thread used since its last checkpoint, nothing when it has not
/// checked in recently and the CPU may belong to unrelated work.
fn used_since(state: ThreadState, cpu: Duration, now: Instant) -> Duration {
    match state.seen {
        Some(seen) if now.saturating_duration_since(seen) <= STALE_CHECKPOINT => {
            cpu.saturating_sub(state.cpu)
        }
        _ => Duration::ZERO,
    }
}

fn pause(limit: u32, wait: Duration) {
    let started = Instant::now();
    let mut remaining = wait;
    while !remaining.is_zero() {
        let slice = remaining.min(PAUSE_SLICE);
        std::thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
        if LIMIT_PERCENT.load(Ordering::Relaxed) != limit {
            break;
        }
        // Every paced thread may be asleep, so the sleepers keep measuring.
        measure_if_due(limit, Instant::now());
        if ALLOWANCE_MILLI.load(Ordering::Relaxed) == 0 {
            break;
        }
    }
    PAUSED_NS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    PAUSES.fetch_add(1, Ordering::Relaxed);
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn since_epoch(at: Instant) -> u64 {
    at.saturating_duration_since(epoch()).as_nanos() as u64
}

fn measure_if_due(limit: u32, now: Instant) {
    let now_ns = since_epoch(now);
    if now_ns < NEXT_MEASURE_NS.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut controller) = CONTROLLER.try_lock() else {
        return;
    };
    if now_ns < NEXT_MEASURE_NS.load(Ordering::Relaxed) {
        return;
    }
    NEXT_MEASURE_NS.store(now_ns + ENGAGE_WINDOW.as_nanos() as u64, Ordering::Relaxed);
    let (Some(process_cpu), Some(threads)) = (
        super::thread_cpu::process_cpu(),
        super::cpu_threads::detected_cpu_threads(),
    ) else {
        return;
    };
    let booked = Duration::from_nanos(BOOKED_NS.load(Ordering::Relaxed));
    let (allowance, unpaced) = match *controller {
        Some(previous) => {
            let wall = now.saturating_duration_since(previous.at).as_secs_f64();
            if wall <= 0.0 {
                return;
            }
            let rate = process_cpu
                .saturating_sub(previous.process_cpu)
                .as_secs_f64()
                / wall;
            // Booked CPU only covers paced work once a whole window was paced.
            let unpaced = if previous.pacing {
                unpaced_estimate(
                    previous.unpaced,
                    rate,
                    booked.saturating_sub(previous.booked).as_secs_f64() / wall,
                )
            } else {
                previous.unpaced
            };
            let backlog = SCHEDULE_NS.load(Ordering::Relaxed) > now_ns;
            (
                next_allowance(rate, unpaced, budget_threads(limit as u8, threads), backlog),
                unpaced,
            )
        }
        None => (0.0, 0.0),
    };
    *controller = Some(Measurement {
        at: now,
        process_cpu,
        booked,
        pacing: allowance > 0.0,
        unpaced,
    });
    let allowance_milli = (allowance * 1000.0).round() as u32;
    if allowance_milli == 0 {
        SCHEDULE_NS.store(0, Ordering::Relaxed);
    } else {
        NEXT_MEASURE_NS.store(now_ns + MEASURE_WINDOW.as_nanos() as u64, Ordering::Relaxed);
    }
    ALLOWANCE_MILLI.store(allowance_milli, Ordering::Relaxed);
}

/// CPU the paced work may use, in processor threads, for a process using
/// `rate_threads` of which `unpaced_threads` happen outside checkpoints; 0
/// while the process is comfortably under `budget_threads` and no thread is
/// waiting on the schedule (`backlog`).
fn next_allowance(
    rate_threads: f64,
    unpaced_threads: f64,
    budget_threads: f64,
    backlog: bool,
) -> f64 {
    if budget_threads <= 0.0 || !rate_threads.is_finite() {
        return 0.0;
    }
    // Threads still waiting mean the work wants more than it gets, however low
    // the rate read in one window.
    if !backlog && rate_threads < budget_threads * ENGAGE_SHARE {
        return 0.0;
    }
    let unpaced = if unpaced_threads.is_finite() {
        unpaced_threads.max(0.0)
    } else {
        0.0
    };
    (budget_threads - unpaced).clamp(budget_threads * MIN_ALLOWANCE_SHARE, budget_threads)
}

/// The CPU the process uses outside checkpoints, averaged with the previous
/// window so one rounding in the clocks does not swing the allowance.
fn unpaced_estimate(previous: f64, rate_threads: f64, booked_threads: f64) -> f64 {
    0.5 * previous + 0.5 * (rate_threads - booked_threads).max(0.0)
}

/// Wall time the schedule advances for `used` CPU at `allowance_milli`
/// thousandths of a processor thread.
fn scheduled_cost(used: Duration, allowance_milli: u32) -> u64 {
    (used.as_nanos() * 1000 / u128::from(allowance_milli.max(1))).min(u128::from(u64::MAX)) as u64
}

/// Book `cost` on a schedule that stands at `schedule`, at `now`: the new
/// schedule, and how long the caller waits for it. The schedule never runs
/// more than [`MAX_PAUSE`] past the tolerated burst.
fn admit(schedule: u64, now: u64, cost: u64) -> (u64, Duration) {
    let burst = BURST.as_nanos() as u64;
    let latest = now
        .saturating_add(burst)
        .saturating_add(MAX_PAUSE.as_nanos() as u64);
    let next = schedule.max(now).saturating_add(cost).min(latest);
    (
        next,
        Duration::from_nanos(next.saturating_sub(now.saturating_add(burst))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn ns(value: Duration) -> u64 {
        value.as_nanos() as u64
    }

    #[test]
    fn the_stored_limit_stays_in_range() {
        assert_eq!(sanitize_cpu_usage_limit(None), None);
        assert_eq!(sanitize_cpu_usage_limit(Some(0)), Some(10));
        assert_eq!(sanitize_cpu_usage_limit(Some(5)), Some(10));
        assert_eq!(sanitize_cpu_usage_limit(Some(90)), Some(90));
        assert_eq!(sanitize_cpu_usage_limit(Some(255)), Some(100));
    }

    #[test]
    fn a_full_limit_is_no_limit() {
        assert_eq!(applied_limit(None), 0);
        assert_eq!(applied_limit(Some(100)), 0);
        assert_eq!(applied_limit(Some(200)), 0);
        assert_eq!(applied_limit(Some(90)), 90);
        assert_eq!(applied_limit(Some(1)), 10);
    }

    #[test]
    fn the_budget_is_a_share_of_every_detected_thread() {
        assert_eq!(budget_threads(90, 24), 21.6);
        assert_eq!(budget_threads(50, 4), 2.0);
        assert_eq!(budget_threads(90, 1), 0.9);
    }

    #[test]
    fn work_well_under_the_budget_is_not_paced() {
        assert_eq!(next_allowance(6.3, 0.0, 21.6, false), 0.0);
        assert_eq!(next_allowance(16.1, 0.0, 21.6, false), 0.0);
        assert_eq!(next_allowance(0.0, 0.0, 21.6, false), 0.0);
    }

    #[test]
    fn work_near_the_budget_is_paced_to_what_the_rest_leaves() {
        assert_eq!(next_allowance(15.0, 0.0, 20.0, false), 20.0);
        assert_eq!(next_allowance(30.0, 5.0, 20.0, false), 15.0);
    }

    #[test]
    fn threads_still_waiting_keep_the_work_paced_through_a_quiet_window() {
        assert_eq!(next_allowance(1.0, 0.0, 20.0, true), 20.0);
        assert_eq!(next_allowance(1.0, 0.0, 20.0, false), 0.0);
    }

    #[test]
    fn paced_work_keeps_a_tenth_of_the_budget() {
        assert_eq!(next_allowance(30.0, 50.0, 20.0, false), 2.0);
    }

    #[test]
    fn unknown_figures_never_pace() {
        assert_eq!(next_allowance(f64::NAN, 0.0, 2.0, false), 0.0);
        assert_eq!(next_allowance(4.0, 0.0, 0.0, false), 0.0);
        assert_eq!(next_allowance(4.0, f64::NAN, 2.0, false), 2.0);
    }

    #[test]
    fn unpaced_cpu_is_what_the_checkpoints_did_not_book_averaged() {
        assert_eq!(unpaced_estimate(0.0, 10.0, 6.0), 2.0);
        assert_eq!(unpaced_estimate(2.0, 10.0, 6.0), 3.0);
        assert_eq!(unpaced_estimate(1.0, 5.0, 9.0), 0.5);
    }

    #[test]
    fn only_cpu_since_a_recent_checkpoint_is_booked() {
        let now = Instant::now() + Duration::from_secs(10);
        let state = |seen_ago: Option<Duration>| ThreadState {
            cpu: ms(100),
            seen: seen_ago.map(|ago| now - ago),
        };
        assert_eq!(used_since(state(Some(ms(30))), ms(115), now), ms(15));
        assert_eq!(
            used_since(state(Some(ms(1_500))), ms(115), now),
            Duration::ZERO
        );
        assert_eq!(used_since(state(None), ms(115), now), Duration::ZERO);
        assert_eq!(used_since(state(Some(ms(30))), ms(90), now), Duration::ZERO);
    }

    #[test]
    fn the_schedule_advances_by_the_cpu_over_the_allowance() {
        assert_eq!(scheduled_cost(ms(10), 2000), ns(ms(5)));
        assert_eq!(scheduled_cost(ms(10), 500), ns(ms(20)));
        assert_eq!(scheduled_cost(ms(10), 0), ns(ms(10)) * 1000);
    }

    #[test]
    fn an_idle_schedule_lets_a_short_burst_through() {
        let now = ns(Duration::from_secs(10));
        assert_eq!(
            admit(0, now, ns(ms(15))),
            (now + ns(ms(15)), Duration::ZERO)
        );
    }

    #[test]
    fn a_schedule_ahead_of_now_makes_the_caller_wait() {
        let now = ns(Duration::from_secs(10));
        assert_eq!(
            admit(now + ns(ms(100)), now, ns(ms(10))),
            (now + ns(ms(110)), ms(90))
        );
    }

    #[test]
    fn a_runaway_schedule_is_capped_at_the_longest_wait() {
        let now = ns(Duration::from_secs(10));
        let (next, wait) = admit(now + ns(Duration::from_secs(5)), now, ns(ms(10)));
        assert_eq!(wait, MAX_PAUSE);
        assert_eq!(next, now + ns(BURST) + ns(MAX_PAUSE));
    }

    /// Threads that mostly wait on the disk, far more of them than the
    /// allowance, book their CPU on the shared schedule: together they use the
    /// allowance and no more.
    #[test]
    fn a_wide_pool_of_disk_bound_threads_is_held_to_the_allowance() {
        fn simulate(threads: u64, allowance_milli: u32) -> f64 {
            let (io, cpu, span) = (ns(ms(50)), ns(ms(5)), ns(Duration::from_secs(20)));
            let mut ready: Vec<u64> = (0..threads).map(|thread| thread * 997).collect();
            let (mut schedule, mut used) = (0u64, 0u64);
            loop {
                let (thread, &now) = ready.iter().enumerate().min_by_key(|(_, at)| **at).unwrap();
                if now >= span {
                    break;
                }
                used += cpu;
                let (next, wait) = admit(
                    schedule,
                    now,
                    scheduled_cost(Duration::from_nanos(cpu), allowance_milli),
                );
                schedule = next;
                ready[thread] = now + ns(wait) + io + cpu;
            }
            used as f64 / span as f64
        }

        // Unpaced, 96 threads at 5 ms of CPU per 55 ms use about 8.7 threads.
        let natural = simulate(96, 1_000_000);
        assert!((8.0..9.0).contains(&natural), "{natural}");
        for allowance in [3.2, 8.0] {
            let held = simulate(96, (allowance * 1000.0) as u32);
            assert!(held <= allowance * 1.02, "{held} against {allowance}");
            assert!(held >= allowance * 0.9, "{held} against {allowance}");
        }
    }

    fn burn(duration: Duration) {
        let started = Instant::now();
        while started.elapsed() < duration {
            std::hint::black_box(0u64);
        }
    }

    /// Process CPU between `settle` and `until` after `started`, as a share of
    /// every processor thread.
    fn machine_share(started: Instant, settle: Duration, until: Duration, threads: usize) -> f64 {
        use crate::core::utils::thread_cpu;

        std::thread::sleep(settle.saturating_sub(started.elapsed()));
        let (from, cpu_from) = (Instant::now(), thread_cpu::process_cpu().unwrap());
        std::thread::sleep(until.saturating_sub(started.elapsed()));
        let cpu = thread_cpu::process_cpu().unwrap().saturating_sub(cpu_from);
        cpu.as_secs_f64() / from.elapsed().as_secs_f64() / threads as f64 * 100.0
    }

    /// Run it alone: tests running beside it add their own CPU to the process.
    #[test]
    #[ignore = "burns every processor thread for five seconds; run manually with --ignored --nocapture"]
    fn a_busy_process_settles_near_the_limit() {
        let threads = crate::core::utils::cpu_threads::detected_cpu_threads().unwrap();
        let before = counters();
        set_cpu_usage_limit(Some(50));
        let started = Instant::now();
        let share = std::thread::scope(|scope| {
            for _ in 0..threads {
                scope.spawn(|| {
                    while started.elapsed() < Duration::from_secs(5) {
                        burn(ms(2));
                        checkpoint();
                    }
                });
            }
            machine_share(started, Duration::from_secs(1), ms(4_500), threads)
        });
        set_cpu_usage_limit(None);
        let paused = counters().since(before);
        println!("threads={threads} share={share:.1}% paused={paused:?}");
        assert!(paused.pauses > 0, "{paused:?}");
        assert!(
            share <= 55.0,
            "{share:.1}% of the machine against a 50% limit"
        );
    }

    /// Eight threads per processor thread, each waiting 8 ms between 2 ms bursts
    /// of CPU, like a hash pool reading from a fast disk. A per-thread pause cap would
    /// leave such a pool near half the machine. Run it alone.
    #[test]
    #[ignore = "runs a wide busy pool for five seconds; run manually with --ignored --nocapture"]
    fn a_wide_mostly_waiting_pool_settles_near_the_limit() {
        let threads = crate::core::utils::cpu_threads::detected_cpu_threads().unwrap();
        let before = counters();
        set_cpu_usage_limit(Some(25));
        let started = Instant::now();
        let share = std::thread::scope(|scope| {
            for _ in 0..threads * 8 {
                scope.spawn(|| {
                    while started.elapsed() < Duration::from_secs(5) {
                        std::thread::sleep(ms(8));
                        burn(ms(2));
                        checkpoint();
                    }
                });
            }
            machine_share(started, Duration::from_secs(1), ms(4_500), threads)
        });
        set_cpu_usage_limit(None);
        let paused = counters().since(before);
        println!(
            "threads={} share={share:.1}% paused={paused:?}",
            threads * 8
        );
        assert!(paused.pauses > 0, "{paused:?}");
        assert!(
            share <= 28.0,
            "{share:.1}% of the machine against a 25% limit"
        );
    }

    #[test]
    fn counters_report_the_difference() {
        let earlier = GovernorCounters {
            paused: ms(100),
            pauses: 4,
        };
        let later = GovernorCounters {
            paused: ms(350),
            pauses: 9,
        };
        assert_eq!(
            later.since(earlier),
            GovernorCounters {
                paused: ms(250),
                pauses: 5,
            }
        );
    }
}
