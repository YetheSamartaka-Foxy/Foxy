use super::types::*;
use crate::core::utils::format::sanitize_log_url;
use anyhow::{Context, Result};

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Returns the platform key for the current OS+arch (e.g. "windows-x86_64").
pub fn current_platform_key() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    // Fallback for unsupported platforms
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    {
        "unknown"
    }
}

// ---------------------------------------------------------------------------
// Core update functions
// ---------------------------------------------------------------------------

/// Fetch the update manifest from `{base_url}/foxy-app-updater.json`.
pub async fn fetch_manifest(base_url: &str) -> Result<UpdateManifest> {
    let url = format!("{}/foxy-app-updater.json", base_url.trim_end_matches('/'));
    log::info!("Fetching update manifest from {}", sanitize_log_url(&url));

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("Failed to fetch update manifest from {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Update manifest request failed with status {} from {}",
            response.status(),
            url
        );
    }

    let manifest: UpdateManifest = response
        .json()
        .await
        .context("Failed to parse update manifest JSON")?;

    if manifest.schema_version != 1 {
        anyhow::bail!(
            "Unsupported update manifest schema version {}. Please update Foxy manually.",
            manifest.schema_version
        );
    }

    Ok(manifest)
}

/// Check if the manifest's latest version is newer than `current_version`.
pub fn is_newer(manifest_latest: &str, current_version: &str) -> bool {
    let Ok(remote) = semver::Version::parse(manifest_latest) else {
        log::warn!(
            "Could not parse remote version '{}' as semver",
            manifest_latest
        );
        return false;
    };
    let Ok(local) = semver::Version::parse(current_version) else {
        log::warn!(
            "Could not parse local version '{}' as semver",
            current_version
        );
        return false;
    };
    remote > local
}

/// Fetch a single changelog JSON from `{base_url}/{changelog_path}`.
pub async fn fetch_changelog(base_url: &str, changelog_path: &str) -> Result<ChangelogVersion> {
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        changelog_path.trim_start_matches('/')
    );
    log::debug!("Fetching changelog from {}", sanitize_log_url(&url));

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("Failed to fetch changelog from {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Changelog request failed with status {} from {}",
            response.status(),
            url
        );
    }

    response
        .json()
        .await
        .context("Failed to parse changelog JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_newer ───────────────────────────────────────────────────────

    #[test]
    fn is_newer_true_for_higher_version() {
        assert!(is_newer("2.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_false_for_same_version() {
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_false_for_older_version() {
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn is_newer_patch_increment() {
        assert!(is_newer("1.0.1", "1.0.0"));
    }

    #[test]
    fn is_newer_minor_increment() {
        assert!(is_newer("1.2.0", "1.0.9"));
    }

    #[test]
    fn is_newer_invalid_remote_returns_false() {
        assert!(!is_newer("not_a_version", "1.0.0"));
    }

    #[test]
    fn is_newer_invalid_local_returns_false() {
        assert!(!is_newer("2.0.0", "not_a_version"));
    }

    #[test]
    fn is_newer_both_invalid_returns_false() {
        assert!(!is_newer("abc", "xyz"));
    }

    #[test]
    fn is_newer_prerelease_comparison() {
        // Semver: 1.0.0-alpha < 1.0.0
        assert!(is_newer("1.0.0", "1.0.0-alpha"));
        assert!(!is_newer("1.0.0-alpha", "1.0.0"));
    }

    // ── current_platform_key ───────────────────────────────────────────

    #[test]
    fn current_platform_key_is_not_empty() {
        let key = current_platform_key();
        assert!(!key.is_empty());
    }

    #[test]
    fn current_platform_key_contains_os() {
        let key = current_platform_key();
        // Should contain either "windows", "linux", or "unknown"
        assert!(
            key.contains("windows") || key.contains("linux") || key == "unknown",
            "Unexpected platform key: {}",
            key
        );
    }
}
