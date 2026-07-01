use log::{info, trace, warn};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

use super::{
    BYTES_PER_MEGABIT, RAMP_ADJUST_INTERVAL, RAMP_GAP_DIVISOR, RAMP_HISTORY_HEADROOM_PERCENT,
    RAMP_MIN_START_BYTES_PER_SEC, RAMP_MIN_STEP_BYTES_PER_SEC, RAMP_START_DIVISOR,
    RAMP_STEP_PERCENT, RAMP_UTILIZATION_THRESHOLD_PERCENT, SPEED_SAMPLE_HISTORY_CAPACITY,
    SPEED_SAMPLE_INTERVAL,
};

/// Minimum throughput (10 KB/s) - below this for consecutive windows triggers degradation.
const DEGRADATION_THRESHOLD_BPS: u64 = 10 * 1024;
/// Number of consecutive low-speed windows before declaring degraded.
const DEGRADATION_CONSECUTIVE_WINDOWS: u8 = 3;

#[derive(Debug)]
pub(crate) struct AdaptiveBandwidthState {
    max_bytes_per_sec: u64,
    current_bytes_per_sec: u64,
    started_at: Instant,
    last_adjusted_at: Instant,
    next_slot_at: Instant,
    observed_bytes: u64,
    observed_bytes_at_last_adjust: u64,
    sample_window_started_at: Instant,
    sample_window_bytes: u64,
    avg_speed_samples_bps: VecDeque<u64>,
    consecutive_low_speed_windows: u8,
}

impl AdaptiveBandwidthState {
    fn avg_history_bps(&self) -> Option<u64> {
        if self.avg_speed_samples_bps.is_empty() {
            return None;
        }
        let sum: u128 = self
            .avg_speed_samples_bps
            .iter()
            .map(|value| u128::from(*value))
            .sum();
        let len = self.avg_speed_samples_bps.len() as u128;
        Some((sum / len) as u64)
    }

    fn maybe_emit_speed_sample(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.sample_window_started_at);
        if elapsed < SPEED_SAMPLE_INTERVAL {
            return;
        }

        let elapsed_secs = elapsed.as_secs_f64();
        let avg_bps = if elapsed_secs > 0.0 {
            (self.sample_window_bytes as f64 / elapsed_secs).round() as u64
        } else {
            0
        };

        self.avg_speed_samples_bps.push_back(avg_bps);
        if self.avg_speed_samples_bps.len() > SPEED_SAMPLE_HISTORY_CAPACITY {
            self.avg_speed_samples_bps.pop_front();
        }

        let rolling_avg_bps = self.avg_history_bps().unwrap_or(avg_bps);
        info!(
            "Download avg speed over last {:.0}s: {:.2} Mb/s (rolling {:.2} Mb/s, samples={})",
            elapsed_secs,
            avg_bps as f64 / BYTES_PER_MEGABIT as f64,
            rolling_avg_bps as f64 / BYTES_PER_MEGABIT as f64,
            self.avg_speed_samples_bps.len()
        );

        // Track consecutive low-speed windows for degradation detection
        if avg_bps < DEGRADATION_THRESHOLD_BPS && avg_bps > 0 {
            self.consecutive_low_speed_windows =
                self.consecutive_low_speed_windows.saturating_add(1);
            if self.consecutive_low_speed_windows >= DEGRADATION_CONSECUTIVE_WINDOWS {
                warn!(
                    "Download speed severely degraded: {:.1} KB/s for {} consecutive windows (threshold: {:.1} KB/s)",
                    avg_bps as f64 / 1024.0,
                    self.consecutive_low_speed_windows,
                    DEGRADATION_THRESHOLD_BPS as f64 / 1024.0
                );
                SPEED_DEGRADED.store(true, Ordering::Relaxed);
            }
        } else {
            self.consecutive_low_speed_windows = 0;
            SPEED_DEGRADED.store(false, Ordering::Relaxed);
        }

        self.sample_window_started_at = now;
        self.sample_window_bytes = 0;
    }

    fn adjust_rate_if_due(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_adjusted_at) < RAMP_ADJUST_INTERVAL {
            return;
        }

        self.maybe_emit_speed_sample(now);

        let elapsed_adjust = now.duration_since(self.last_adjusted_at).as_secs_f64();
        if elapsed_adjust <= 0.0 {
            self.last_adjusted_at = now;
            return;
        }

        let observed_delta = self
            .observed_bytes
            .saturating_sub(self.observed_bytes_at_last_adjust);
        let instant_bps = (observed_delta as f64 / elapsed_adjust).round() as u64;

        let elapsed_total = now.duration_since(self.started_at).as_secs_f64();
        let session_avg_bps = if elapsed_total > 0.0 {
            (self.observed_bytes as f64 / elapsed_total).round() as u64
        } else {
            0
        };
        let history_avg_bps = self.avg_history_bps().unwrap_or(session_avg_bps);
        let utilization_threshold = self
            .current_bytes_per_sec
            .saturating_mul(RAMP_UTILIZATION_THRESHOLD_PERCENT)
            .saturating_div(100);
        let saturated = instant_bps >= utilization_threshold && instant_bps > 0;

        let step = self
            .current_bytes_per_sec
            .saturating_mul(RAMP_STEP_PERCENT)
            .saturating_div(100)
            .max(RAMP_MIN_STEP_BYTES_PER_SEC);
        let gap = self
            .max_bytes_per_sec
            .saturating_sub(self.current_bytes_per_sec);
        let gap_step = gap.saturating_div(RAMP_GAP_DIVISOR);
        let history_target = history_avg_bps.saturating_add(
            history_avg_bps
                .saturating_mul(RAMP_HISTORY_HEADROOM_PERCENT)
                .saturating_div(100),
        );
        let history_step = history_target.saturating_sub(self.current_bytes_per_sec);
        let history_overshoot_threshold =
            history_target.saturating_add(history_target.saturating_div(4));

        let next = if saturated && gap > 0 {
            let fast_step = step.max(gap_step).min(gap);
            self.current_bytes_per_sec
                .saturating_add(fast_step)
                .min(self.max_bytes_per_sec)
        } else if history_step > 0 && gap > 0 {
            // Not fully saturated, but recent rolling averages suggest we can nudge upward.
            let gentle = history_step.min(step).min(gap);
            self.current_bytes_per_sec
                .saturating_add(gentle)
                .min(self.max_bytes_per_sec)
        } else if history_target > 0 && self.current_bytes_per_sec > history_overshoot_threshold {
            // Pull back when limiter target is clearly above sustained rolling throughput.
            let pullback = step
                .min(self.current_bytes_per_sec.saturating_sub(history_target))
                .max(RAMP_MIN_STEP_BYTES_PER_SEC);
            self.current_bytes_per_sec.saturating_sub(pullback)
        } else {
            self.current_bytes_per_sec
        };

        if next != self.current_bytes_per_sec {
            let change_percent = (next as i64 - self.current_bytes_per_sec as i64)
                .unsigned_abs()
                .saturating_mul(100)
                .checked_div(self.current_bytes_per_sec)
                .unwrap_or(100);
            if change_percent > 20 {
                info!(
                    "Adaptive bandwidth change: {} -> {} B/s (change={}% instant={} B/s rolling_avg={} B/s max={} B/s)",
                    self.current_bytes_per_sec,
                    next,
                    change_percent,
                    instant_bps,
                    history_avg_bps,
                    self.max_bytes_per_sec
                );
            } else {
                trace!(
                    "Adaptive bandwidth ramp: {} -> {} B/s (change={}% instant={} B/s rolling_avg={} B/s max={} B/s)",
                    self.current_bytes_per_sec,
                    next,
                    change_percent,
                    instant_bps,
                    history_avg_bps,
                    self.max_bytes_per_sec
                );
            }
            self.current_bytes_per_sec = next;
        }

        self.observed_bytes_at_last_adjust = self.observed_bytes;
        self.last_adjusted_at = now;
    }
}

#[derive(Debug)]
pub(crate) enum AdaptiveBandwidthLimiter {
    Unlimited,
    Limited(Mutex<AdaptiveBandwidthState>),
}

/// Shared flag set when download speed is severely degraded across
/// consecutive measurement windows. Transfer tasks can poll this to
/// decide whether to attempt a reconnect.
static SPEED_DEGRADED: AtomicBool = AtomicBool::new(false);

impl AdaptiveBandwidthLimiter {
    pub(crate) fn from_mbps(limit_mbps: Option<u32>) -> Self {
        let Some(limit_mbps) = limit_mbps.filter(|value| *value > 0) else {
            info!("Download speed limit: unlimited");
            return Self::Unlimited;
        };

        let max_bytes_per_sec = u64::from(limit_mbps).saturating_mul(BYTES_PER_MEGABIT);
        let current_bytes_per_sec = max_bytes_per_sec
            .saturating_div(RAMP_START_DIVISOR)
            .max(RAMP_MIN_START_BYTES_PER_SEC)
            .min(max_bytes_per_sec);
        let now = Instant::now();

        info!(
            "Download speed limit: {} Mb/s (adaptive ramp starts at {:.2} Mb/s)",
            limit_mbps,
            current_bytes_per_sec as f64 / BYTES_PER_MEGABIT as f64
        );

        Self::Limited(Mutex::new(AdaptiveBandwidthState {
            max_bytes_per_sec,
            current_bytes_per_sec,
            started_at: now,
            last_adjusted_at: now,
            next_slot_at: now,
            observed_bytes: 0,
            observed_bytes_at_last_adjust: 0,
            sample_window_started_at: now,
            sample_window_bytes: 0,
            avg_speed_samples_bps: VecDeque::with_capacity(SPEED_SAMPLE_HISTORY_CAPACITY),
            consecutive_low_speed_windows: 0,
        }))
    }

    /// Acquire a rate-limit slot and record the transfer in a single lock acquisition.
    /// This halves mutex contention compared to separate until_ready + record_transfer calls.
    pub(crate) async fn acquire_and_record(&self, bytes: usize) {
        let AdaptiveBandwidthLimiter::Limited(state_lock) = self else {
            return;
        };

        let delay = {
            let mut state = state_lock.lock().await;
            state.adjust_rate_if_due();

            // Record transfer under the same lock
            state.observed_bytes = state.observed_bytes.saturating_add(bytes as u64);
            state.sample_window_bytes = state.sample_window_bytes.saturating_add(bytes as u64);

            let now = Instant::now();
            let request_bytes = bytes.max(1) as u64;
            let available_at = if state.next_slot_at > now {
                state.next_slot_at
            } else {
                now
            };
            let seconds = request_bytes as f64 / state.current_bytes_per_sec as f64;
            let slot = Duration::from_secs_f64(seconds);

            state.next_slot_at = available_at + slot;

            if available_at > now {
                available_at.duration_since(now)
            } else {
                Duration::ZERO
            }
        };

        if !delay.is_zero() {
            sleep(delay).await;
        }
    }
}
