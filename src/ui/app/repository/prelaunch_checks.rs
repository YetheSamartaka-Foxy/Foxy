use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;

const RECHECK_INTERVAL: Duration = Duration::from_millis(1500);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PrelaunchProcessStatus {
    pub ts3_running: Option<bool>,
    pub steam_running: Option<bool>,
}

#[derive(Default)]
pub(crate) struct PrelaunchProcessChecks {
    completed_at: Option<Instant>,
    result_rx: Option<Receiver<PrelaunchProcessStatus>>,
}

impl PrelaunchProcessChecks {
    pub(crate) fn poll(
        &mut self,
        ctx: &egui::Context,
        ts3_required: bool,
        steam_required: bool,
        steam_directory: &str,
    ) -> Option<PrelaunchProcessStatus> {
        let now = Instant::now();
        let result = self.poll_result(now);
        if !ts3_required && !steam_required {
            return result;
        }
        if self.ready(now) {
            let steam_directory = steam_directory.to_owned();
            self.start(ctx, move || PrelaunchProcessStatus {
                ts3_running: ts3_required.then(crate::core::ts3_plugin::is_teamspeak_running),
                steam_running: steam_required
                    .then(|| crate::core::steam::is_steam_running(&steam_directory)),
            });
        }
        let delay = if self.result_rx.is_some() {
            POLL_INTERVAL
        } else {
            RECHECK_INTERVAL.saturating_sub(now.duration_since(self.completed_at.unwrap_or(now)))
        };
        crate::ui::app::request_frame_after(ctx, delay);
        result
    }

    fn ready(&self, now: Instant) -> bool {
        self.result_rx.is_none()
            && self
                .completed_at
                .is_none_or(|at| now.duration_since(at) >= RECHECK_INTERVAL)
    }

    fn start(
        &mut self,
        ctx: &egui::Context,
        probe: impl FnOnce() -> PrelaunchProcessStatus + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.result_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(probe());
            ctx.request_repaint();
        });
    }

    fn poll_result(&mut self, now: Instant) -> Option<PrelaunchProcessStatus> {
        let result = self.result_rx.as_ref()?.try_recv();
        match result {
            Ok(status) => {
                self.result_rx = None;
                self.completed_at = Some(now);
                Some(status)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.result_rx = None;
                self.completed_at = Some(now);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> PrelaunchProcessStatus {
        PrelaunchProcessStatus {
            ts3_running: Some(false),
            steam_running: Some(true),
        }
    }

    #[test]
    fn slow_probe_does_not_block_polling_or_start_another_check() {
        let ctx = egui::Context::default();
        let mut checks = PrelaunchProcessChecks::default();
        let (release, wait) = mpsc::channel();
        checks.start(&ctx, move || {
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            status()
        });
        let later = Instant::now() + Duration::from_secs(8);
        assert_eq!(checks.poll_result(later), None);
        assert!(!checks.ready(later));
        release.send(()).unwrap();
    }

    #[test]
    fn cooldown_starts_when_result_is_received() {
        let (tx, rx) = mpsc::channel();
        let mut checks = PrelaunchProcessChecks {
            result_rx: Some(rx),
            ..Default::default()
        };
        let completed = Instant::now() + Duration::from_secs(8);
        tx.send(status()).unwrap();
        assert_eq!(checks.poll_result(completed), Some(status()));
        assert!(!checks.ready(completed));
        assert!(!checks.ready(completed + RECHECK_INTERVAL - Duration::from_millis(1)));
        assert!(checks.ready(completed + RECHECK_INTERVAL));
    }

    #[test]
    fn reset_discards_results_from_previous_modal() {
        let (tx, rx) = mpsc::channel();
        let mut checks = PrelaunchProcessChecks {
            result_rx: Some(rx),
            ..Default::default()
        };
        assert!(!checks.ready(Instant::now()));
        checks = Default::default();
        assert!(tx.send(status()).is_err());
        assert_eq!(checks.poll_result(Instant::now()), None);
        assert!(checks.ready(Instant::now()));
    }

    #[test]
    fn disconnected_worker_is_retried_after_cooldown() {
        let (tx, rx) = mpsc::channel();
        let mut checks = PrelaunchProcessChecks {
            result_rx: Some(rx),
            ..Default::default()
        };
        drop(tx);
        let now = Instant::now();
        assert_eq!(checks.poll_result(now), None);
        assert!(!checks.ready(now));
        assert!(checks.ready(now + RECHECK_INTERVAL));
    }
}
