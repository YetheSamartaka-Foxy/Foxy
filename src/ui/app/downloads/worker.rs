use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender as StdSender;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use tokio::sync::watch;

use crate::core::utils::format::{sanitize_log_path, sanitize_log_url};
use crate::ui::app::{DirectDownloadProgressEvent, Foxy};
use crate::ui::i18n::{tr, tr_fmt};

impl Foxy {
    pub(in crate::ui::app) fn run_direct_download_worker(
        source_url: String,
        destination_path: PathBuf,
        speed_limit_mbps: Option<u32>,
        progress_tx: StdSender<DirectDownloadProgressEvent>,
        download_pause_rx: watch::Receiver<bool>,
        repaint_ctx: Option<egui::Context>,
    ) {
        let started_at = Instant::now();
        let operation_id = crate::core::api::next_operation_id("direct-download");
        log::info!(
            "Direct download worker started: op={} source={} destination={} speed_limit_mbps={:?}",
            operation_id,
            sanitize_log_url(&source_url),
            sanitize_log_path(&destination_path),
            speed_limit_mbps
        );
        let send_progress = |event| {
            if progress_tx.send(event).is_ok() {
                Self::request_background_repaint(repaint_ctx.as_ref());
                true
            } else {
                false
            }
        };
        let client = match Client::builder().build() {
            Ok(client) => client,
            Err(err) => {
                log::error!(
                    "Direct download worker failed to initialize HTTP client: op={} elapsed={:.2?} error={}",
                    operation_id,
                    started_at.elapsed(),
                    err
                );
                let _ = send_progress(DirectDownloadProgressEvent::Finished {
                    error_message: Some(format!("Failed to initialize HTTP client: {}", err)),
                    files_done: 0,
                    files_total: 0,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    elapsed: started_at.elapsed(),
                });
                return;
            }
        };

        let plan = match Self::build_direct_download_plan(&client, &source_url, &destination_path) {
            Ok(plan) => plan,
            Err(err) => {
                log::warn!(
                    "Direct download plan failed: op={} source={} destination={} elapsed={:.2?} error={}",
                    operation_id,
                    sanitize_log_url(&source_url),
                    sanitize_log_path(&destination_path),
                    started_at.elapsed(),
                    err
                );
                let _ = send_progress(DirectDownloadProgressEvent::Finished {
                    error_message: Some(err),
                    files_done: 0,
                    files_total: 0,
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    elapsed: started_at.elapsed(),
                });
                return;
            }
        };

        let files_total = plan.files.len();
        let total_bytes = plan.total_bytes;
        log::info!(
            "Direct download plan resolved: op={} target={} files={} bytes={}",
            operation_id,
            plan.target_label,
            files_total,
            total_bytes
        );
        let _ = send_progress(DirectDownloadProgressEvent::PlanResolved {
            target_label: plan.target_label.clone(),
            files_total,
            total_bytes,
        });

        if files_total == 0 {
            log::warn!(
                "Direct download plan was empty: op={} elapsed={:.2?}",
                operation_id,
                started_at.elapsed()
            );
            let _ = send_progress(DirectDownloadProgressEvent::Finished {
                error_message: Some(tr("No files found to download")),
                files_done: 0,
                files_total: 0,
                downloaded_bytes: 0,
                total_bytes: 0,
                elapsed: started_at.elapsed(),
            });
            return;
        }

        let speed_limit_bps = speed_limit_mbps.map(|limit| (limit as f64 * 1_000_000.0) / 8.0);
        let mut downloaded_total = 0u64;
        let mut files_done = 0usize;
        let mut throttle_window_started_at = Instant::now();
        let mut throttle_window_bytes = 0u64;
        let pause_rx = download_pause_rx;

        for target in &plan.files {
            if let Some(parent) = target.local_path.parent()
                && let Err(err) = fs::create_dir_all(parent)
            {
                let _ = send_progress(DirectDownloadProgressEvent::Finished {
                    error_message: Some(tr_fmt(
                        "Failed to create destination folder: {error}",
                        &[("error", err.to_string())],
                    )),
                    files_done,
                    files_total,
                    downloaded_bytes: downloaded_total,
                    total_bytes,
                    elapsed: started_at.elapsed(),
                });
                return;
            }

            let mut response = match client.get(&target.remote_url).send() {
                Ok(response) => match response.error_for_status() {
                    Ok(response) => response,
                    Err(err) => {
                        let _ = send_progress(DirectDownloadProgressEvent::Finished {
                            error_message: Some(tr_fmt(
                                "Download failed for {url}: {error}",
                                &[
                                    ("url", target.remote_url.clone()),
                                    ("error", err.to_string()),
                                ],
                            )),
                            files_done,
                            files_total,
                            downloaded_bytes: downloaded_total,
                            total_bytes,
                            elapsed: started_at.elapsed(),
                        });
                        return;
                    }
                },
                Err(err) => {
                    let _ = send_progress(DirectDownloadProgressEvent::Finished {
                        error_message: Some(tr_fmt(
                            "Download request failed for {url}: {error}",
                            &[
                                ("url", target.remote_url.clone()),
                                ("error", err.to_string()),
                            ],
                        )),
                        files_done,
                        files_total,
                        downloaded_bytes: downloaded_total,
                        total_bytes,
                        elapsed: started_at.elapsed(),
                    });
                    return;
                }
            };

            let mut local_file = match fs::File::create(&target.local_path) {
                Ok(file) => file,
                Err(err) => {
                    let _ = send_progress(DirectDownloadProgressEvent::Finished {
                        error_message: Some(tr_fmt(
                            "Failed to create destination file {path}: {error}",
                            &[
                                ("path", target.local_path.display().to_string()),
                                ("error", err.to_string()),
                            ],
                        )),
                        files_done,
                        files_total,
                        downloaded_bytes: downloaded_total,
                        total_bytes,
                        elapsed: started_at.elapsed(),
                    });
                    return;
                }
            };

            let mut file_bytes_done = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                while *pause_rx.borrow() {
                    std::thread::sleep(Duration::from_millis(100));
                }

                let read_count = match response.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read_count) => read_count,
                    Err(err) => {
                        let _ = send_progress(DirectDownloadProgressEvent::Finished {
                            error_message: Some(tr_fmt(
                                "Failed reading remote stream {url}: {error}",
                                &[
                                    ("url", target.remote_url.clone()),
                                    ("error", err.to_string()),
                                ],
                            )),
                            files_done,
                            files_total,
                            downloaded_bytes: downloaded_total,
                            total_bytes,
                            elapsed: started_at.elapsed(),
                        });
                        return;
                    }
                };

                if let Err(err) = local_file.write_all(&buffer[..read_count]) {
                    let _ = send_progress(DirectDownloadProgressEvent::Finished {
                        error_message: Some(tr_fmt(
                            "Failed writing destination file {path}: {error}",
                            &[
                                ("path", target.local_path.display().to_string()),
                                ("error", err.to_string()),
                            ],
                        )),
                        files_done,
                        files_total,
                        downloaded_bytes: downloaded_total,
                        total_bytes,
                        elapsed: started_at.elapsed(),
                    });
                    return;
                }

                let just_downloaded = read_count as u64;
                file_bytes_done = file_bytes_done.saturating_add(just_downloaded);
                downloaded_total = downloaded_total.saturating_add(just_downloaded);
                throttle_window_bytes = throttle_window_bytes.saturating_add(just_downloaded);

                if let Some(limit_bps) = speed_limit_bps {
                    let elapsed_window = throttle_window_started_at.elapsed().as_secs_f64();
                    if elapsed_window > 0.0 {
                        let current_bps = throttle_window_bytes as f64 / elapsed_window;
                        if current_bps > limit_bps {
                            let desired_elapsed = throttle_window_bytes as f64 / limit_bps;
                            let sleep_secs = (desired_elapsed - elapsed_window).max(0.0);
                            if sleep_secs > 0.0 {
                                std::thread::sleep(Duration::from_secs_f64(sleep_secs));
                            }
                        }
                    }
                    if throttle_window_started_at.elapsed() >= Duration::from_secs(1) {
                        throttle_window_started_at = Instant::now();
                        throttle_window_bytes = 0;
                    }
                }

                let file_percent = if target.size_bytes == 0 {
                    0.0
                } else {
                    (file_bytes_done as f32 / target.size_bytes as f32).clamp(0.0, 1.0)
                };
                let overall_percent = if total_bytes > 0 {
                    (downloaded_total as f32 / total_bytes as f32).clamp(0.0, 1.0)
                } else {
                    (((files_done as f32) + file_percent) / files_total as f32).clamp(0.0, 1.0)
                };
                let _ = send_progress(DirectDownloadProgressEvent::Progress {
                    label: target.label.clone(),
                    percent: overall_percent,
                    files_done,
                    files_total,
                    downloaded_bytes: downloaded_total,
                    total_bytes,
                });
            }

            // Flush and sync the file to disk to prevent data loss on power failure
            if let Err(err) = local_file.flush() {
                let _ = send_progress(DirectDownloadProgressEvent::Finished {
                    error_message: Some(tr_fmt(
                        "Failed to flush file {path}: {error}",
                        &[
                            ("path", target.local_path.display().to_string()),
                            ("error", err.to_string()),
                        ],
                    )),
                    files_done,
                    files_total,
                    downloaded_bytes: downloaded_total,
                    total_bytes,
                    elapsed: started_at.elapsed(),
                });
                return;
            }
            if let Err(err) = local_file.sync_all() {
                let _ = send_progress(DirectDownloadProgressEvent::Finished {
                    error_message: Some(tr_fmt(
                        "Failed to sync file {path}: {error}",
                        &[
                            ("path", target.local_path.display().to_string()),
                            ("error", err.to_string()),
                        ],
                    )),
                    files_done,
                    files_total,
                    downloaded_bytes: downloaded_total,
                    total_bytes,
                    elapsed: started_at.elapsed(),
                });
                return;
            }

            files_done = files_done.saturating_add(1);
            let overall_percent = if total_bytes > 0 {
                (downloaded_total as f32 / total_bytes as f32).clamp(0.0, 1.0)
            } else {
                (files_done as f32 / files_total as f32).clamp(0.0, 1.0)
            };
            let _ = send_progress(DirectDownloadProgressEvent::Progress {
                label: target.label.clone(),
                percent: overall_percent,
                files_done,
                files_total,
                downloaded_bytes: downloaded_total,
                total_bytes,
            });
        }

        let _ = send_progress(DirectDownloadProgressEvent::Finished {
            error_message: None,
            files_done,
            files_total,
            downloaded_bytes: downloaded_total,
            total_bytes,
            elapsed: started_at.elapsed(),
        });
        log::info!(
            "Direct download worker finished: op={} files={} bytes={} elapsed={:.2?}",
            operation_id,
            files_done,
            downloaded_total,
            started_at.elapsed()
        );
    }
}
