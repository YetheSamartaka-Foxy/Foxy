use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UpdatePhase {
    Preparing,
    Transferring,
    Verifying,
    Saving,
}

impl UpdatePhase {
    const ALL: [Self; 4] = [
        Self::Preparing,
        Self::Transferring,
        Self::Verifying,
        Self::Saving,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Preparing => 0,
            Self::Transferring => 1,
            Self::Verifying => 2,
            Self::Saving => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateWork {
    pub files: usize,
    pub transfer_bytes: u64,
    pub full_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateTiming {
    pub work: UpdateWork,
    pub phase_durations: [Duration; 4],
}

impl UpdateTiming {
    pub fn estimate(&self, work: UpdateWork) -> Option<Duration> {
        if !valid_work(self.work) || !valid_work(work) {
            return None;
        }

        let mut total = Duration::ZERO;
        for phase in UpdatePhase::ALL {
            let duration = self.phase_durations[phase.index()];
            let scaled = scale_duration(duration, scale_for(phase, self.work, work)?)?;
            total = total.checked_add(scaled)?;
        }
        Some(total)
    }
}

#[derive(Clone, Debug)]
pub struct UpdateEstimate {
    baseline: Option<UpdateTiming>,
    work: Option<UpdateWork>,
    phase: UpdatePhase,
    phase_started: Instant,
    phase_elapsed: Duration,
    phase_durations: [Duration; 4],
    observed: [bool; 4],
    paused_at: Option<Instant>,
    invalid: bool,
}

impl UpdateEstimate {
    pub fn new(now: Instant, baseline: Option<UpdateTiming>) -> Self {
        let baseline = baseline.filter(is_valid_timing);
        Self {
            baseline,
            work: None,
            phase: UpdatePhase::Preparing,
            phase_started: now,
            phase_elapsed: Duration::ZERO,
            phase_durations: [Duration::ZERO; 4],
            observed: [false; 4],
            paused_at: None,
            invalid: false,
        }
    }

    pub fn set_work(&mut self, work: UpdateWork) {
        if !valid_work(work) {
            self.invalidate();
            self.work = Some(work);
            return;
        }
        self.work = Some(work);
    }

    pub fn set_phase(&mut self, phase: UpdatePhase, now: Instant) {
        if self.invalid || self.paused_at.is_some() || phase == self.phase {
            if self.paused_at.is_some() && phase != self.phase {
                self.invalidate();
            }
            return;
        }
        if phase.index() != self.phase.index() + 1 {
            self.invalidate();
            self.phase = phase;
            self.phase_started = now;
            return;
        }
        let duration = self.elapsed_in_phase(now);
        self.phase_durations[self.phase.index()] = duration;
        self.observed[self.phase.index()] = true;
        self.phase = phase;
        self.phase_started = now;
        self.phase_elapsed = Duration::ZERO;
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        match (self.paused_at, paused) {
            (None, true) => {
                self.phase_elapsed = self.elapsed_in_phase(now);
                self.paused_at = Some(now);
            }
            (Some(_), false) => {
                self.paused_at = None;
                self.phase_started = now;
            }
            _ => {}
        }
    }

    pub fn invalidate(&mut self) {
        self.invalid = true;
        self.baseline = None;
    }

    pub fn remaining(
        &self,
        now: Instant,
        live_transfer_remaining: Option<Duration>,
    ) -> Option<Duration> {
        if self.invalid || self.paused_at.is_some() {
            return None;
        }
        let work = self.work?;
        if !valid_work(work) {
            return None;
        }
        let timing = self.baseline.as_ref()?;
        let mut total = Duration::ZERO;
        for phase in UpdatePhase::ALL {
            let idx = phase.index();
            if idx < self.phase.index() {
                continue;
            }
            let expected = scale_duration(
                timing.phase_durations[idx],
                scale_for(phase, timing.work, work)?,
            )?;
            let remaining = if phase == self.phase {
                let elapsed = self.elapsed_in_phase(now);
                if phase == UpdatePhase::Transferring {
                    let learned = expected.checked_sub(elapsed).filter(|d| !d.is_zero());
                    match (live_transfer_remaining.filter(|d| !d.is_zero()), learned) {
                        (Some(live), Some(learned)) => Some(live.max(learned)),
                        (Some(live), None) => Some(live),
                        (None, Some(learned)) => Some(learned),
                        (None, None) => None,
                    }?
                } else {
                    expected.checked_sub(elapsed).filter(|d| !d.is_zero())?
                }
            } else {
                expected
            };
            total = total.checked_add(remaining)?;
        }
        Some(total.max(Duration::from_secs(1)))
    }

    pub fn finish(&mut self, now: Instant) -> Option<UpdateTiming> {
        if self.invalid
            || self.paused_at.is_some()
            || self.phase != UpdatePhase::Saving
            || self.work.is_none()
        {
            return None;
        }
        self.phase_durations[UpdatePhase::Saving.index()] = self.elapsed_in_phase(now);
        self.observed[UpdatePhase::Saving.index()] = true;
        if !self.observed.into_iter().all(|seen| seen) {
            return None;
        }
        let timing = UpdateTiming {
            work: self.work?,
            phase_durations: self.phase_durations,
        };
        is_valid_timing(&timing).then_some(timing)
    }

    pub fn phase(&self) -> UpdatePhase {
        self.phase
    }

    fn elapsed_in_phase(&self, now: Instant) -> Duration {
        self.phase_elapsed
            .saturating_add(now.saturating_duration_since(self.phase_started))
    }
}

fn valid_work(work: UpdateWork) -> bool {
    work.files > 0 || work.transfer_bytes > 0 || work.full_bytes.is_some_and(|bytes| bytes > 0)
}

fn is_valid_timing(timing: &UpdateTiming) -> bool {
    if !valid_work(timing.work) {
        return false;
    }
    let Some(total) = timing
        .phase_durations
        .iter()
        .try_fold(Duration::ZERO, |total, duration| {
            total.checked_add(*duration)
        })
    else {
        return false;
    };
    !total.is_zero()
}

fn scale_for(phase: UpdatePhase, from: UpdateWork, to: UpdateWork) -> Option<f64> {
    if phase == UpdatePhase::Transferring {
        let mut ratio = None;
        include_ratio(&mut ratio, from.transfer_bytes, to.transfer_bytes);
        if let (Some(old), Some(new)) = (from.full_bytes, to.full_bytes) {
            include_ratio(&mut ratio, old, new);
        }
        include_ratio(
            &mut ratio,
            u64::try_from(from.files).ok()?,
            u64::try_from(to.files).ok()?,
        );
        return ratio;
    }
    let (old, new) = match phase {
        UpdatePhase::Verifying => match (from.full_bytes, to.full_bytes) {
            (Some(old), Some(new)) => (old, new),
            _ => (
                u64::try_from(from.files).ok()?,
                u64::try_from(to.files).ok()?,
            ),
        },
        UpdatePhase::Preparing | UpdatePhase::Saving => (
            u64::try_from(from.files).ok()?,
            u64::try_from(to.files).ok()?,
        ),
        UpdatePhase::Transferring => unreachable!(),
    };
    ratio(old, new)
}

fn include_ratio(current: &mut Option<f64>, old: u64, new: u64) {
    if let Some(next) = ratio(old, new) {
        *current = Some(current.map_or(next, |current| current.max(next)));
    }
}

fn ratio(old: u64, new: u64) -> Option<f64> {
    if old == 0 {
        return (new == 0).then_some(1.0);
    }
    let value = new as f64 / old as f64;
    value.is_finite().then_some(value)
}

fn scale_duration(duration: Duration, ratio: f64) -> Option<Duration> {
    let seconds = duration.as_secs_f64() * ratio;
    if !seconds.is_finite() || seconds < 0.0 || seconds >= u64::MAX as f64 {
        return None;
    }
    let scaled = Duration::try_from_secs_f64(seconds).ok()?;
    (scaled.as_secs() < u64::MAX).then_some(scaled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(work: UpdateWork, seconds: [u64; 4]) -> UpdateTiming {
        UpdateTiming {
            work,
            phase_durations: seconds.map(Duration::from_secs),
        }
    }

    fn work() -> UpdateWork {
        UpdateWork {
            files: 10,
            transfer_bytes: 1000,
            full_bytes: Some(2000),
        }
    }

    #[test]
    fn learned_estimate_keeps_verification_and_saving_tail() {
        let base = timing(work(), [2, 10, 5, 3]);
        let now = Instant::now();
        let mut estimate = UpdateEstimate::new(now, Some(base));
        estimate.set_work(work());
        estimate.set_phase(UpdatePhase::Transferring, now + Duration::from_secs(2));
        let remaining = estimate
            .remaining(now + Duration::from_secs(5), None)
            .unwrap();
        assert_eq!(remaining, Duration::from_secs(15));
    }

    #[test]
    fn network_remaining_overlaps_patch_work() {
        let now = Instant::now();
        let mut estimate = UpdateEstimate::new(now, Some(timing(work(), [2, 10, 5, 3])));
        estimate.set_work(work());
        estimate.set_phase(UpdatePhase::Transferring, now + Duration::from_secs(2));
        assert_eq!(
            estimate.remaining(now + Duration::from_secs(5), Some(Duration::from_secs(12))),
            Some(Duration::from_secs(20))
        );
    }

    #[test]
    fn patch_heavy_transfer_uses_full_work_when_network_bytes_are_small() {
        let baseline_work = UpdateWork {
            files: 10,
            transfer_bytes: 1000,
            full_bytes: Some(1_000_000),
        };
        let target_work = UpdateWork {
            files: 10,
            transfer_bytes: 100,
            full_bytes: Some(2_000_000),
        };
        assert_eq!(
            timing(baseline_work, [2, 10, 5, 3]).estimate(target_work),
            Some(Duration::from_secs(35))
        );
    }

    #[test]
    fn unknown_or_expired_phase_has_no_estimate() {
        let now = Instant::now();
        let mut cold = UpdateEstimate::new(now, None);
        cold.set_work(work());
        assert_eq!(cold.remaining(now, None), None);
        let mut active = UpdateEstimate::new(now, Some(timing(work(), [2, 3, 5, 3])));
        active.set_work(work());
        active.set_phase(UpdatePhase::Transferring, now + Duration::from_secs(2));
        assert_eq!(active.remaining(now + Duration::from_secs(10), None), None);
        assert_eq!(
            active.remaining(now + Duration::from_secs(10), Some(Duration::from_secs(1))),
            Some(Duration::from_secs(9))
        );
    }

    #[test]
    fn pause_time_is_excluded_from_phase_and_remaining() {
        let now = Instant::now();
        let mut estimate = UpdateEstimate::new(now, Some(timing(work(), [2, 10, 5, 3])));
        estimate.set_work(work());
        estimate.set_phase(UpdatePhase::Transferring, now + Duration::from_secs(2));
        estimate.set_paused(true, now + Duration::from_secs(4));
        assert_eq!(
            estimate.remaining(now + Duration::from_secs(40), None),
            None
        );
        estimate.set_paused(false, now + Duration::from_secs(34));
        assert_eq!(
            estimate.remaining(now + Duration::from_secs(35), None),
            Some(Duration::from_secs(15))
        );
    }

    #[test]
    fn restart_invalidates_and_work_expansion_rescales_learning() {
        let now = Instant::now();
        let mut restart = UpdateEstimate::new(now, Some(timing(work(), [2, 10, 5, 3])));
        restart.set_work(work());
        restart.set_phase(UpdatePhase::Transferring, now + Duration::from_secs(2));
        restart.set_phase(UpdatePhase::Preparing, now + Duration::from_secs(3));
        assert_eq!(restart.remaining(now + Duration::from_secs(3), None), None);
        let mut changed = UpdateEstimate::new(now, Some(timing(work(), [2, 10, 5, 3])));
        changed.set_work(work());
        changed.set_work(UpdateWork {
            transfer_bytes: 2000,
            ..work()
        });
        assert!(changed.remaining(now, None).unwrap() > Duration::from_secs(20));
    }

    #[test]
    fn successful_sample_excludes_pause_and_requires_complete_phase_sequence() {
        let now = Instant::now();
        let mut estimate = UpdateEstimate::new(now, None);
        estimate.set_work(work());
        assert!(estimate.finish(now).is_none());
        estimate.set_phase(UpdatePhase::Transferring, now + Duration::from_secs(2));
        estimate.set_paused(true, now + Duration::from_secs(4));
        estimate.set_paused(false, now + Duration::from_secs(34));
        estimate.set_phase(UpdatePhase::Verifying, now + Duration::from_secs(42));
        estimate.set_phase(UpdatePhase::Saving, now + Duration::from_secs(47));
        let learned = estimate.finish(now + Duration::from_secs(50)).unwrap();
        assert_eq!(
            learned.phase_durations,
            [2, 10, 5, 3].map(Duration::from_secs)
        );
        assert_eq!(learned.estimate(work()), Some(Duration::from_secs(20)));
        estimate.invalidate();
        assert!(estimate.finish(now + Duration::from_secs(51)).is_none());
    }

    #[test]
    fn zero_work_and_unrepresentable_estimates_are_rejected() {
        assert_eq!(
            timing(UpdateWork::default(), [1, 1, 1, 1]).estimate(work()),
            None
        );
        assert!(
            UpdateEstimate::new(Instant::now(), Some(timing(work(), [0; 4])))
                .baseline
                .is_none()
        );
        let huge = UpdateWork {
            files: usize::MAX,
            transfer_bytes: u64::MAX,
            full_bytes: Some(u64::MAX),
        };
        assert_eq!(timing(work(), [u64::MAX - 1; 4]).estimate(huge), None);
    }
}
