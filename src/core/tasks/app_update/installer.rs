use super::types::*;
use crate::core::utils::format::{sanitize_log_path, sanitize_log_url};
use anyhow::{Context, Result};
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

const INSTALLER_MAX_RETRIES: u32 = 3;
const INSTALLER_BASE_DELAY_MS: u64 = 1000;
const INSTALLER_CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

/// Download an installer to a temp directory with progress reporting.
/// Returns the path to the downloaded file. Supports retry with resume
/// via HTTP Range headers on transient failures.
pub async fn download_installer(
    base_url: &str,
    platform_entry: &PlatformEntry,
    progress_tx: &std_mpsc::Sender<AppUpdateEvent>,
) -> Result<PathBuf> {
    // GitHub mode stores full URLs in installer_path; server mode uses relative paths.
    let url = if platform_entry.installer_path.starts_with("http://")
        || platform_entry.installer_path.starts_with("https://")
    {
        platform_entry.installer_path.clone()
    } else {
        format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            platform_entry.installer_path.trim_start_matches('/')
        )
    };

    // Determine filename from the path - split on both separators and sanitize
    let file_name =
        crate::core::utils::fs_safety::sanitize_installer_filename(&platform_entry.installer_path)
            .unwrap_or_else(|| "foxy-installer".to_string());

    let dest_dir = crate::core::utils::app_paths::foxy_large_payload_dir();
    let dest_path = dest_dir.join(&file_name);

    log::info!("Downloading installer from {}", sanitize_log_url(&url));

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut downloaded: u64 = 0;
    let mut total_size = platform_entry.installer_size;

    for attempt in 0..=INSTALLER_MAX_RETRIES {
        if attempt > 0 {
            let delay = INSTALLER_BASE_DELAY_MS * (1 << (attempt - 1).min(3));
            log::warn!(
                "Retrying installer download (attempt {}/{}) after {}ms, resuming from {} bytes",
                attempt + 1,
                INSTALLER_MAX_RETRIES + 1,
                delay,
                downloaded
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        let request = if downloaded > 0 {
            client
                .get(&url)
                .header("Range", format!("bytes={}-", downloaded))
        } else {
            client.get(&url)
        };

        let response = match request.send().await {
            Ok(resp) => resp,
            Err(err) => {
                log::warn!("Installer download request failed: {}", err);
                continue;
            }
        };

        if !response.status().is_success() {
            log::warn!(
                "Installer download failed with status {} from {}",
                response.status(),
                sanitize_log_url(&url)
            );
            continue;
        }

        if downloaded > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            log::warn!("Installer server ignored Range request; restarting download from byte 0");
            downloaded = 0;
            let _ = tokio::fs::remove_file(&dest_path).await;
        }

        if downloaded == 0 {
            total_size = response.content_length().unwrap_or(total_size);
        }

        // On first attempt create; on resume open in append mode
        use tokio::io::AsyncWriteExt;
        let mut file = if downloaded == 0 {
            tokio::fs::File::create(&dest_path)
                .await
                .with_context(|| format!("Failed to create {}", dest_path.display()))?
        } else {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(&dest_path)
                .await
                .with_context(|| format!("Failed to open {} for append", dest_path.display()))?
        };

        let mut stream = response.bytes_stream();
        let mut stream_failed = false;

        use futures::StreamExt;
        loop {
            match tokio::time::timeout(INSTALLER_CHUNK_TIMEOUT, stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    file.write_all(&chunk)
                        .await
                        .context("Error writing installer to disk")?;
                    downloaded += chunk.len() as u64;
                    let _ = progress_tx.send(AppUpdateEvent::DownloadProgress {
                        bytes_done: downloaded,
                        bytes_total: total_size,
                    });
                }
                Ok(Some(Err(err))) => {
                    log::warn!("Installer download stream error: {}", err);
                    stream_failed = true;
                    break;
                }
                Ok(None) => break, // stream complete
                Err(_) => {
                    log::warn!(
                        "Installer download chunk timed out after {:?}",
                        INSTALLER_CHUNK_TIMEOUT
                    );
                    stream_failed = true;
                    break;
                }
            }
        }

        file.flush()
            .await
            .context("Error flushing installer file")?;

        if !stream_failed {
            file.sync_all()
                .await
                .context("Error syncing installer file to disk")?;
            log::info!(
                "Downloaded installer to {} ({} bytes)",
                sanitize_log_path(&dest_path),
                downloaded
            );
            return Ok(dest_path);
        }
    }

    anyhow::bail!(
        "Failed to download installer from {} after {} attempts ({} bytes received)",
        url,
        INSTALLER_MAX_RETRIES + 1,
        downloaded
    )
}

/// Verify the hash of a downloaded installer file using streaming.
pub fn verify_installer(path: &Path, expected_hash: &str, algorithm: &str) -> Result<bool> {
    log::info!("Verifying installer hash: {}", sanitize_log_path(path));

    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut buf = [0u8; 65536];
    let algorithm = normalize_installer_hash_algorithm(algorithm)?;
    let actual_hash = match algorithm {
        "blake3" => {
            let mut hasher = blake3::Hasher::new();
            loop {
                let n = std::io::Read::read(&mut reader, &mut buf)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            hasher.finalize().to_hex().to_string()
        }
        "sha256" => {
            let mut hasher = sha2::Sha256::new();
            loop {
                let n = std::io::Read::read(&mut reader, &mut buf)
                    .with_context(|| format!("Failed to read {}", path.display()))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            hex::encode(hasher.finalize())
        }
        other => anyhow::bail!("Unsupported installer hash algorithm: {}", other),
    };

    let matches = actual_hash.eq_ignore_ascii_case(expected_hash.trim());
    if !matches {
        log::warn!(
            "Hash mismatch for {}: algorithm={} expected {}, got {}",
            sanitize_log_path(path),
            algorithm,
            expected_hash,
            actual_hash
        );
    } else {
        log::info!("Installer hash verified successfully.");
    }
    Ok(matches)
}

pub fn normalize_installer_hash_algorithm(algorithm: &str) -> Result<&'static str> {
    match algorithm.trim().to_ascii_lowercase().as_str() {
        "" | "blake3" | "b3" => Ok("blake3"),
        "sha256" | "sha-256" => Ok("sha256"),
        other => anyhow::bail!("Unsupported installer hash algorithm: {}", other),
    }
}

/// Launch the installer and exit the app.
/// On Windows, runs the installer with /VERYSILENT flags.
/// On Linux, runs the shell installer with --silent.
pub fn launch_installer(path: &Path, silent: bool) -> Result<()> {
    log::info!("Launching installer: {}", sanitize_log_path(path));

    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new(path);
        if silent {
            cmd.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/CLOSEAPPLICATIONS"]);
        }
        cmd.spawn()
            .with_context(|| format!("Failed to launch installer: {}", path.display()))?;
    }

    #[cfg(target_os = "linux")]
    {
        let asset_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_ascii_lowercase())
            .unwrap_or_default();
        if !asset_name.ends_with(".sh") {
            anyhow::bail!(
                "Linux in-app update only supports Foxy .sh installers. Unsupported asset: {}",
                path.display()
            );
        }
        let install_prefix = current_linux_install_prefix();
        let needs_elevation = install_prefix
            .as_ref()
            .is_some_and(|prefix| !linux_prefix_is_writable(prefix));
        let use_pkexec = silent
            && needs_elevation
            && !linux_sudo_noninteractive_available()
            && crate::core::utils::platform::command_exists("pkexec");

        if silent && needs_elevation && !linux_sudo_noninteractive_available() && !use_pkexec {
            let prefix = install_prefix.as_ref().expect("checked by needs_elevation");
            anyhow::bail!(
                "Updating {} requires administrator privileges. Install pkexec or run the downloaded installer manually.",
                prefix.display()
            );
        }

        let mut cmd = if use_pkexec {
            let mut cmd = std::process::Command::new("pkexec");
            cmd.arg("sh");
            cmd
        } else {
            std::process::Command::new("sh")
        };
        cmd.arg(path);
        if silent {
            cmd.arg("--silent");
        }
        if let Some(prefix) = install_prefix {
            cmd.arg(format!("--prefix={}", prefix.display()));
        }
        cmd.spawn()
            .with_context(|| format!("Failed to launch installer: {}", path.display()))?;
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        anyhow::bail!("Installer launch not supported on this platform");
    }

    // Give the installer script time to start before we exit
    #[cfg(target_os = "linux")]
    {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    // Exit the app so the installer can replace the binary
    log::info!("Exiting app for installer to proceed.");
    std::process::exit(0);
}

#[cfg(target_os = "linux")]
fn current_linux_install_prefix() -> Option<PathBuf> {
    if let Some(prefix) = std::env::var_os("FOXY_INSTALL_PREFIX")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    {
        return Some(prefix);
    }

    let exe = std::env::current_exe().ok()?;
    let prefix = exe
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| exe.parent().map(Path::to_path_buf))?;

    if linux_prefix_has_installer_marker(&prefix) {
        return Some(prefix);
    }

    let persisted = prefix.join(".foxy_install_prefix");
    let persisted_prefix = std::fs::read_to_string(persisted)
        .ok()
        .map(|value| PathBuf::from(value.trim()))
        .filter(|path| path.is_dir());
    if let Some(persisted_prefix) = persisted_prefix {
        return Some(persisted_prefix);
    }

    None
}

#[cfg(target_os = "linux")]
fn linux_prefix_has_installer_marker(prefix: &Path) -> bool {
    prefix.join(".installed_by_foxy_installer").exists()
        || prefix.join(".foxy_install_prefix").exists()
        || prefix.join("foxy.desktop").exists()
}

#[cfg(target_os = "linux")]
fn linux_prefix_is_writable(prefix: &Path) -> bool {
    let probe = prefix.join(".foxy_write_test");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn linux_sudo_noninteractive_available() -> bool {
    std::process::Command::new("sudo")
        .args(["-n", "true"])
        .status()
        .is_ok_and(|status| status.success())
}
