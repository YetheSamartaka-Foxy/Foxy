use super::github::*;
use super::installer::*;
use super::manifest::*;
use super::types::*;
use crate::core::api::next_operation_id;
use crate::core::utils::format::sanitize_log_url;
use crate::ui::types::AppUpdateMode;
use anyhow::Result;
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

/// Spawn a background task that checks for updates and sends results via mpsc.
/// Returns the receiver for the UI to poll.
pub fn spawn_update_check(
    mode: AppUpdateMode,
    source: String,
    current_version: String,
    repaint_ctx: Option<egui::Context>,
) -> std_mpsc::Receiver<AppUpdateEvent> {
    let (tx, rx) = std_mpsc::channel();

    std::thread::spawn(move || {
        let operation_id = next_operation_id("app-update-check");
        let started = Instant::now();
        let source_label = match mode {
            AppUpdateMode::Server => sanitize_log_url(&source),
            AppUpdateMode::GitHub => source.trim().to_string(),
        };
        log::info!(
            "App update check started: op={} mode={:?} source={} current_version={}",
            operation_id,
            mode,
            source_label,
            current_version
        );
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                log::error!(
                    "App update check runtime failed: op={} elapsed={:.2?} error={}",
                    operation_id,
                    started.elapsed(),
                    err
                );
                let _ = tx.send(AppUpdateEvent::Error(format!(
                    "Failed to create runtime for update check: {}",
                    err
                )));
                return;
            }
        };

        rt.block_on(async {
            // For GitHub mode, fetch releases and convert to manifest + changelogs.
            // For Server mode, fetch the manifest directly.
            let fetch_result: Result<(UpdateManifest, Vec<ChangelogVersion>)> = match mode {
                AppUpdateMode::Server => match fetch_manifest(&source).await {
                    Ok(manifest) => Ok((manifest, Vec::new())),
                    Err(e) => Err(e),
                },
                AppUpdateMode::GitHub => match fetch_github_releases(&source).await {
                    Ok(releases) => {
                        let changelogs = github_releases_to_changelogs(&releases);
                        github_releases_to_manifest(&releases)
                            .await
                            .map(|m| (m, changelogs))
                    }
                    Err(e) => Err(e),
                },
            };

            match fetch_result {
                Ok((manifest, changelogs)) => {
                    let has_newer = is_newer(&manifest.latest, &current_version);
                    let latest = manifest.latest.clone();
                    let info = AppUpdateInfo {
                        source_base_url: source.clone(),
                        manifest,
                        current_version: current_version.clone(),
                        fetched_changelogs: changelogs,
                    };
                    if has_newer {
                        log::info!(
                            "App update check finished: op={} outcome=available latest={} elapsed={:.2?}",
                            operation_id,
                            latest,
                            started.elapsed()
                        );
                        let _ = tx.send(AppUpdateEvent::ManifestFetched(info));
                    } else {
                        log::info!(
                            "App update check finished: op={} outcome=up_to_date latest={} elapsed={:.2?}",
                            operation_id,
                            latest,
                            started.elapsed()
                        );
                        let _ = tx.send(AppUpdateEvent::UpToDate(info));
                    }
                }
                Err(e) => {
                    log::warn!(
                        "App update check failed: op={} elapsed={:.2?} error={:#}",
                        operation_id,
                        started.elapsed(),
                        e
                    );
                    let _ = tx.send(AppUpdateEvent::Error(format!("{:#}", e)));
                }
            }
        });

        if let Some(ctx) = repaint_ctx {
            ctx.request_repaint();
        }
    });

    rx
}

/// Spawn a background task that downloads and verifies an installer.
/// Returns the receiver for the UI to poll.
pub fn spawn_installer_download(
    base_url: String,
    platform_entry: PlatformEntry,
    repaint_ctx: Option<egui::Context>,
) -> std_mpsc::Receiver<AppUpdateEvent> {
    let (tx, rx) = std_mpsc::channel();

    std::thread::spawn(move || {
        let operation_id = next_operation_id("app-update-download");
        let started = Instant::now();
        log::info!(
            "App installer download started: op={} source={} expected_bytes={} hash_present={}",
            operation_id,
            sanitize_log_url(&base_url),
            platform_entry.installer_size,
            !platform_entry.installer_hash.is_empty()
        );
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                log::error!(
                    "App installer download runtime failed: op={} elapsed={:.2?} error={}",
                    operation_id,
                    started.elapsed(),
                    err
                );
                let _ = tx.send(AppUpdateEvent::Error(format!(
                    "Failed to create runtime for installer download: {}",
                    err
                )));
                return;
            }
        };

        rt.block_on(async {
            match download_installer(&base_url, &platform_entry, &tx).await {
                Ok(path) => {
                    if platform_entry.installer_hash.is_empty() {
                        let _ = std::fs::remove_file(&path);
                        let _ = tx.send(AppUpdateEvent::Error(
                            "Installer hash is missing. Refusing to run an unverifiable app update."
                                .to_string(),
                        ));
                        log::warn!(
                            "App installer download rejected without hash: op={} elapsed={:.2?}",
                            operation_id,
                            started.elapsed()
                        );
                    } else {
                        let _ = tx.send(AppUpdateEvent::Verifying);
                        match verify_installer(
                            &path,
                            &platform_entry.installer_hash,
                            &platform_entry.installer_hash_algorithm,
                        ) {
                            Ok(true) => {
                                log::info!(
                                    "App installer download verified: op={} elapsed={:.2?}",
                                    operation_id,
                                    started.elapsed()
                                );
                                let _ = tx.send(AppUpdateEvent::InstallerReady {
                                    installer_path: path,
                                });
                            }
                            Ok(false) => {
                                let _ = std::fs::remove_file(&path);
                                let _ = tx.send(AppUpdateEvent::Error(
                                    "Installer hash verification failed. The download may be corrupted."
                                        .to_string(),
                                ));
                                log::warn!(
                                    "App installer hash verification failed: op={} elapsed={:.2?}",
                                    operation_id,
                                    started.elapsed()
                                );
                            }
                            Err(e) => {
                                let _ = std::fs::remove_file(&path);
                                let _ = tx.send(AppUpdateEvent::Error(format!(
                                    "Hash verification error: {:#}",
                                    e
                                )));
                                log::warn!(
                                    "App installer hash verification errored: op={} elapsed={:.2?} error={:#}",
                                    operation_id,
                                    started.elapsed(),
                                    e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!(
                        "App installer download failed: op={} elapsed={:.2?} error={:#}",
                        operation_id,
                        started.elapsed(),
                        e
                    );
                    let _ = tx.send(AppUpdateEvent::Error(format!("Download failed: {:#}", e)));
                }
            }
        });

        if let Some(ctx) = repaint_ctx {
            ctx.request_repaint();
        }
    });

    rx
}
