use crate::core::api::{ProgressEvent, send_progress_event};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::broadcast::Sender;
use tokio::time::sleep;
pub(super) fn download_progress_percent(done: usize, total: usize) -> f32 {
    if total == 0 {
        return 1.0;
    }
    stage_percent_from_fraction(done as f32 / total as f32)
}

/// Overall download-stage percent weighted by bytes, not file count: small
/// files finish first, so a file-count bar parks near its end while the
/// large files still carry most of the transfer.
pub(super) fn download_progress_percent_by_bytes(
    files_done: usize,
    total_files: usize,
    bytes_done: u64,
    bytes_total: u64,
) -> f32 {
    if total_files == 0 || bytes_total == 0 || files_done >= total_files {
        return download_progress_percent(files_done, total_files);
    }
    stage_percent_from_fraction((bytes_done as f64 / bytes_total as f64) as f32)
}

fn stage_percent_from_fraction(fraction: f32) -> f32 {
    // Map download progress into 0.25..0.85 range (recheck done at 0.25)
    let base = 0.25_f32;
    let span = 0.60_f32;
    (base + span * fraction.clamp(0.0, 1.0)).min(0.85)
}

pub(super) struct ModProgressEntry {
    pub(super) size: usize,
    pub(super) download_total: Arc<AtomicUsize>,
}

pub(super) fn summarize_mod_progress(entries: &[ModProgressEntry]) -> (usize, u64) {
    let mut files_done = 0usize;
    let mut bytes_done = 0u64;
    for entry in entries {
        let done = entry.download_total.load(Ordering::Relaxed);
        let capped = done.min(entry.size);
        bytes_done += capped as u64;
        if done >= entry.size {
            files_done += 1;
        }
    }
    (files_done, bytes_done)
}

pub(super) fn start_progress_ticker(
    progress_tx: Option<Sender<ProgressEvent>>,
    completed: Arc<AtomicUsize>,
    total_files: usize,
    byte_entries: Vec<ModProgressEntry>,
    operation_id: String,
) -> Option<(tokio::task::JoinHandle<()>, Arc<AtomicBool>)> {
    let tx = progress_tx?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = stop.clone();
    let bytes_total: u64 = byte_entries.iter().map(|entry| entry.size as u64).sum();

    let handle = tokio::spawn(async move {
        let current_percent = |done: usize| {
            let (_, bytes_done) = summarize_mod_progress(&byte_entries);
            download_progress_percent_by_bytes(done, total_files, bytes_done, bytes_total)
        };
        let mut last_sent: Option<(usize, u32)> = None;
        while !stop_signal.load(Ordering::SeqCst) {
            let done = completed.load(Ordering::SeqCst);
            let percent = current_percent(done);
            // The UI shows tenths of a percent, so finer changes are not resent.
            let snapshot = (done, (percent * 1000.0).round() as u32);
            if last_sent != Some(snapshot) {
                send_progress_event(
                    &tx,
                    ProgressEvent::Stage {
                        label: format!("Download {}/{}", done, total_files),
                        percent,
                    },
                    &operation_id,
                );
                last_sent = Some(snapshot);
            }
            sleep(Duration::from_millis(100)).await;
        }
        // One last update on exit
        let done = completed.load(Ordering::SeqCst);
        send_progress_event(
            &tx,
            ProgressEvent::Stage {
                label: format!("Download {}/{}", done, total_files),
                percent: current_percent(done),
            },
            &operation_id,
        );
    });

    Some((handle, stop))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── download_progress_percent ───────────────────────────────────────

    #[test]
    fn progress_percent_zero_total_returns_one() {
        assert_eq!(download_progress_percent(0, 0), 1.0);
    }

    #[test]
    fn progress_percent_zero_done_starts_at_base() {
        let pct = download_progress_percent(0, 100);
        assert!((pct - 0.25).abs() < 0.001);
    }

    #[test]
    fn progress_percent_all_done_reaches_cap() {
        let pct = download_progress_percent(100, 100);
        assert!((pct - 0.85).abs() < 0.001);
    }

    #[test]
    fn progress_percent_half_done() {
        let pct = download_progress_percent(50, 100);
        // 0.25 + 0.60 * 0.5 = 0.55
        assert!((pct - 0.55).abs() < 0.001);
    }

    // ── download_progress_percent_by_bytes ──────────────────────────────

    #[test]
    fn progress_by_bytes_ignores_many_small_finished_files() {
        // 1532 of 1719 files done, but only 0.44 of 24 GB transferred.
        let pct = download_progress_percent_by_bytes(1532, 1719, 440_000_000, 24_000_000_000);
        assert!(pct < 0.27, "pct={pct}");
        assert!(pct > 0.25, "pct={pct}");
    }

    #[test]
    fn progress_by_bytes_half_transferred() {
        let pct = download_progress_percent_by_bytes(10, 100, 500, 1_000);
        assert!((pct - 0.55).abs() < 0.001);
    }

    #[test]
    fn progress_by_bytes_all_files_done_reaches_cap() {
        // Files that complete without network bytes still finish the stage.
        let pct = download_progress_percent_by_bytes(5, 5, 100, 1_000);
        assert!((pct - 0.85).abs() < 0.001);
    }

    #[test]
    fn progress_by_bytes_falls_back_to_file_count_without_bytes() {
        let pct = download_progress_percent_by_bytes(50, 100, 0, 0);
        assert!((pct - 0.55).abs() < 0.001);
    }

    #[test]
    fn progress_by_bytes_clamps_overshoot() {
        let pct = download_progress_percent_by_bytes(1, 2, 5_000, 1_000);
        assert!((pct - 0.85).abs() < 0.001);
    }

    #[test]
    fn progress_by_bytes_zero_files_is_complete() {
        assert_eq!(download_progress_percent_by_bytes(0, 0, 0, 0), 1.0);
    }

    // ── summarize_mod_progress ──────────────────────────────────────────

    #[test]
    fn summarize_mod_progress_empty() {
        let (files, bytes) = summarize_mod_progress(&[]);
        assert_eq!(files, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn summarize_mod_progress_one_complete() {
        let entries = vec![ModProgressEntry {
            size: 100,
            download_total: Arc::new(AtomicUsize::new(100)),
        }];
        let (files, bytes) = summarize_mod_progress(&entries);
        assert_eq!(files, 1);
        assert_eq!(bytes, 100);
    }

    #[test]
    fn summarize_mod_progress_partial_and_complete() {
        let entries = vec![
            ModProgressEntry {
                size: 200,
                download_total: Arc::new(AtomicUsize::new(200)),
            },
            ModProgressEntry {
                size: 300,
                download_total: Arc::new(AtomicUsize::new(150)),
            },
        ];
        let (files, bytes) = summarize_mod_progress(&entries);
        assert_eq!(files, 1); // only first is complete
        assert_eq!(bytes, 350); // 200 + min(150, 300)
    }

    #[test]
    fn summarize_mod_progress_caps_at_size() {
        // download_total exceeds size - bytes should be capped
        let entries = vec![ModProgressEntry {
            size: 100,
            download_total: Arc::new(AtomicUsize::new(999)),
        }];
        let (files, bytes) = summarize_mod_progress(&entries);
        assert_eq!(files, 1);
        assert_eq!(bytes, 100); // capped at size
    }

    // ── download_progress_percent: additional ──────────────────────────

    #[test]
    fn progress_percent_one_of_many() {
        let pct = download_progress_percent(1, 100);
        assert!(pct > 0.25);
        assert!(pct < 0.30);
    }

    #[test]
    fn progress_percent_exceeds_total_clamps() {
        let pct = download_progress_percent(200, 100);
        assert!(pct <= 0.85);
    }

    #[test]
    fn progress_percent_single_file() {
        let pct = download_progress_percent(1, 1);
        assert!((pct - 0.85).abs() < 0.001);
    }

    // ── summarize_mod_progress: additional ──────────────────────────────

    #[test]
    fn summarize_mod_progress_all_zero_downloads() {
        let entries = vec![
            ModProgressEntry {
                size: 100,
                download_total: Arc::new(AtomicUsize::new(0)),
            },
            ModProgressEntry {
                size: 200,
                download_total: Arc::new(AtomicUsize::new(0)),
            },
        ];
        let (files, bytes) = summarize_mod_progress(&entries);
        assert_eq!(files, 0);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn summarize_mod_progress_all_complete() {
        let entries = vec![
            ModProgressEntry {
                size: 100,
                download_total: Arc::new(AtomicUsize::new(100)),
            },
            ModProgressEntry {
                size: 200,
                download_total: Arc::new(AtomicUsize::new(200)),
            },
            ModProgressEntry {
                size: 300,
                download_total: Arc::new(AtomicUsize::new(300)),
            },
        ];
        let (files, bytes) = summarize_mod_progress(&entries);
        assert_eq!(files, 3);
        assert_eq!(bytes, 600);
    }

    #[test]
    fn summarize_mod_progress_zero_size_file() {
        let entries = vec![ModProgressEntry {
            size: 0,
            download_total: Arc::new(AtomicUsize::new(0)),
        }];
        let (files, bytes) = summarize_mod_progress(&entries);
        assert_eq!(files, 1); // 0 >= 0
        assert_eq!(bytes, 0);
    }
}
