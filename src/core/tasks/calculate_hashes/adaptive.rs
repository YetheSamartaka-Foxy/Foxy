//! Adjusts the number of files hashed at once while a run is going, by the
//! bytes each window hashed, instead of keeping the one choice the Auto
//! benchmark made from a small sample.

use log::info;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::Instant;

const WINDOW: Duration = Duration::from_secs(1);
/// A window must be this much slower than the one before to turn around, so
/// the noise of mixed file sizes does not flip the direction every window.
const REVERSE_BELOW: f64 = 0.97;

/// Hill climbing over the worker count: keep stepping while throughput holds
/// up, turn around when a step costs throughput.
#[derive(Debug)]
pub(super) struct ConcurrencyClimb {
    limit: usize,
    lo: usize,
    hi: usize,
    step: usize,
    upward: bool,
    last_rate: Option<f64>,
}

impl ConcurrencyClimb {
    pub(super) fn new(initial: usize) -> Self {
        let initial = initial.max(1);
        Self {
            limit: initial,
            lo: (initial / 2).max(1),
            hi: initial
                .saturating_mul(2)
                .min(super::MAX_FILE_JOB_CONCURRENCY),
            step: (initial / 4).max(1),
            upward: true,
            last_rate: None,
        }
    }

    pub(super) fn hi(&self) -> usize {
        self.hi
    }

    /// The limit for the next window, given the rate of the one just ended.
    pub(super) fn next(&mut self, rate: f64) -> usize {
        if let Some(previous) = self.last_rate
            && rate < previous * REVERSE_BELOW
        {
            self.upward = !self.upward;
        }
        self.last_rate = Some(rate);
        let stepped = if self.upward {
            self.limit.saturating_add(self.step)
        } else {
            self.limit.saturating_sub(self.step)
        };
        let clamped = stepped.clamp(self.lo, self.hi);
        if clamped == self.limit {
            self.upward = !self.upward;
        }
        self.limit = clamped;
        self.limit
    }
}

/// Runs the climb against `semaphore`, whose permits are the worker count,
/// until the returned handle is dropped.
pub(super) fn spawn_climb(
    semaphore: Arc<Semaphore>,
    bytes_done: Arc<AtomicU64>,
    mut climb: ConcurrencyClimb,
) -> ClimbHandle {
    let task = tokio::spawn(async move {
        let mut permits = climb.limit;
        let mut last_bytes = bytes_done.load(Ordering::Relaxed);
        let mut last_at = Instant::now();
        // The first window only measures; the climb starts from its rate.
        let mut measured_once = false;
        loop {
            tokio::time::sleep(WINDOW).await;
            let bytes = bytes_done.load(Ordering::Relaxed);
            let now = Instant::now();
            let rate =
                bytes.saturating_sub(last_bytes) as f64 / now.duration_since(last_at).as_secs_f64();
            last_bytes = bytes;
            last_at = now;
            if !measured_once {
                measured_once = true;
                climb.last_rate = Some(rate);
                continue;
            }
            let before = climb.limit;
            let target = climb.next(rate);
            permits = move_permits(&semaphore, permits, target);
            if target != before {
                info!(
                    "Hash concurrency adapt: rate={:.1} MB/s limit={}->{} permits={}",
                    rate / 1_000_000.0,
                    before,
                    target,
                    permits
                );
            }
        }
    });
    ClimbHandle(task)
}

/// Moves the semaphore from `permits` toward `target`. Permits held by running
/// workers cannot be taken back until they return, so a shrink may land short
/// and finish on a later window.
fn move_permits(semaphore: &Semaphore, permits: usize, target: usize) -> usize {
    if target > permits {
        semaphore.add_permits(target - permits);
        target
    } else {
        permits - semaphore.forget_permits(permits - target)
    }
}

pub(super) struct ClimbHandle(tokio::task::JoinHandle<()>);

impl Drop for ClimbHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_climbing_while_throughput_holds_and_turns_when_it_drops() {
        let mut climb = ConcurrencyClimb::new(8);
        assert_eq!((climb.lo, climb.hi, climb.step), (4, 16, 2));
        climb.last_rate = Some(100.0);
        assert_eq!(climb.next(110.0), 10);
        assert_eq!(climb.next(120.0), 12);
        assert_eq!(climb.next(100.0), 10);
        assert_eq!(climb.next(99.0), 8);
        assert_eq!(climb.next(80.0), 10);
    }

    #[test]
    fn turns_around_at_the_bounds() {
        let mut climb = ConcurrencyClimb::new(2);
        assert_eq!((climb.lo, climb.hi, climb.step), (1, 4, 1));
        climb.last_rate = Some(1.0);
        assert_eq!(climb.next(1.0), 3);
        assert_eq!(climb.next(1.0), 4);
        assert_eq!(climb.next(1.0), 4);
        assert_eq!(climb.next(1.0), 3);
    }

    #[test]
    fn a_single_worker_can_still_try_a_second() {
        let mut climb = ConcurrencyClimb::new(1);
        assert_eq!((climb.lo, climb.hi), (1, 2));
        assert_eq!(climb.next(5.0), 2);
    }

    #[test]
    fn permits_grow_at_once_and_shrink_as_workers_return_them() {
        let semaphore = Semaphore::new(4);
        assert_eq!(move_permits(&semaphore, 4, 6), 6);
        assert_eq!(semaphore.available_permits(), 6);
        let held = semaphore.try_acquire_many(5).unwrap();
        assert_eq!(move_permits(&semaphore, 6, 3), 5);
        drop(held);
        assert_eq!(move_permits(&semaphore, 5, 3), 3);
        assert_eq!(semaphore.available_permits(), 3);
    }
}
