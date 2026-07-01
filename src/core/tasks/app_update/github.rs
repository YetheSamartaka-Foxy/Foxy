use super::types::*;
use crate::core::utils::format::sanitize_log_url;
use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstallerChecksum {
    hash: String,
    algorithm: String,
}

/// Strip a leading `v` or `V` prefix from a GitHub tag name.
fn strip_version_prefix(tag: &str) -> &str {
    tag.trim_start_matches(['v', 'V'])
}

/// Iterate over non-draft, non-prerelease releases with valid semver tags.
fn valid_releases(releases: &[GitHubRelease]) -> impl Iterator<Item = &GitHubRelease> {
    releases.iter().filter(|r| {
        !r.draft
            && !r.prerelease
            && semver::Version::parse(strip_version_prefix(&r.tag_name)).is_ok()
    })
}

/// Fetch releases from the GitHub API (single request, up to 30 results).
pub async fn fetch_github_releases(repo_slug: &str) -> Result<Vec<GitHubRelease>> {
    let url = format!("https://api.github.com/repos/{}/releases", repo_slug.trim());
    log::info!("Fetching GitHub releases from {}", sanitize_log_url(&url));

    let client = super::super::create_web_client::create_web_client().await;
    let response = client
        .get(&url)
        .header("User-Agent", format!("Foxy/{}", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("Failed to fetch releases from {}", url))?;

    if response.status() == reqwest::StatusCode::FORBIDDEN
        || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        let reset_time = response
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());

        let remaining = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");

        let mut msg = format!(
            "GitHub API rate limit exceeded (remaining: {}). ",
            remaining
        );

        if let Some(reset_ts) = reset_time {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let wait_minutes = ((reset_ts - now) / 60).max(1);
            msg.push_str(&format!("Try again in ~{} minute(s).", wait_minutes));
        } else {
            msg.push_str("Try again later.");
        }

        anyhow::bail!(msg);
    }

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "Repository '{}' not found or has no releases. Check that the repository exists and is public.",
            repo_slug
        );
    }

    if !response.status().is_success() {
        anyhow::bail!(
            "GitHub API returned status {} for {}",
            response.status(),
            url
        );
    }

    response
        .json()
        .await
        .context("Failed to parse GitHub releases JSON")
}

/// Convert GitHub releases into the existing `UpdateManifest` format so the
/// rest of the update pipeline (UI, download, install) can be reused as-is.
pub async fn github_releases_to_manifest(releases: &[GitHubRelease]) -> Result<UpdateManifest> {
    let mut versions = Vec::new();
    let client = super::super::create_web_client::create_web_client().await;

    for release in valid_releases(releases) {
        let version_str = strip_version_prefix(&release.tag_name).to_string();

        let checksums = fetch_checksum_assets(&client, release).await;

        let mut platforms = HashMap::new();
        for asset in &release.assets {
            let name_lower = asset.name.to_lowercase();

            // Skip CLI assets and checksum files
            if name_lower.contains("-cli") || checksum_asset_algorithm(&asset.name).is_some() {
                continue;
            }

            // Detect architecture from filename
            let asset_arch = if name_lower.contains("aarch64") || name_lower.contains("arm64") {
                "aarch64"
            } else if name_lower.contains("x86_64") || name_lower.contains("amd64") {
                "x86_64"
            } else {
                "" // Assume compatible if no arch in name
            };

            let platform_key = if name_lower.ends_with(".exe") {
                Some("windows-x86_64")
            } else if name_lower.ends_with(".sh") {
                if asset_arch == "aarch64" {
                    Some("linux-aarch64")
                } else {
                    Some("linux-x86_64")
                }
            } else {
                None
            };

            if let Some(key) = platform_key {
                let checksum = checksums.get(&asset.name);
                let installer_hash = checksum
                    .map(|checksum| checksum.hash.clone())
                    .unwrap_or_default();
                let installer_hash_algorithm = checksum
                    .map(|checksum| checksum.algorithm.clone())
                    .unwrap_or_else(default_installer_hash_algorithm);

                // Prefer .sh (self-extracting installer) over other formats
                let is_preferred = name_lower.ends_with(".sh") || name_lower.ends_with(".exe");
                let should_insert = !platforms.contains_key(key) || is_preferred;

                if should_insert {
                    platforms.insert(
                        key.to_string(),
                        PlatformEntry {
                            installer_path: asset.browser_download_url.clone(),
                            installer_hash,
                            installer_hash_algorithm,
                            installer_size: asset.size,
                        },
                    );
                }
            }
        }

        if !platforms.is_empty() {
            versions.push(VersionEntry {
                version: version_str,
                changelog: String::new(),
                platforms,
            });
        }
    }

    versions.sort_by(|a, b| {
        let va = semver::Version::parse(&a.version).unwrap_or(semver::Version::new(0, 0, 0));
        let vb = semver::Version::parse(&b.version).unwrap_or(semver::Version::new(0, 0, 0));
        vb.cmp(&va)
    });

    let latest = versions
        .first()
        .map(|v| v.version.clone())
        .unwrap_or_default();

    Ok(UpdateManifest {
        schema_version: 1,
        latest,
        versions,
    })
}

async fn fetch_checksum_assets(
    client: &reqwest::Client,
    release: &GitHubRelease,
) -> HashMap<String, InstallerChecksum> {
    let mut assets = release
        .assets
        .iter()
        .filter_map(|asset| {
            checksum_asset_algorithm(&asset.name).map(|algorithm| (asset, algorithm))
        })
        .collect::<Vec<_>>();
    assets.sort_by_key(|(_, algorithm)| if *algorithm == "blake3" { 0 } else { 1 });

    let mut checksums = HashMap::new();
    for (asset, algorithm) in assets {
        match fetch_checksum_asset_text(client, asset).await {
            Ok(text) => {
                for (name, hash) in parse_checksum_text(&text) {
                    checksums.entry(name).or_insert_with(|| InstallerChecksum {
                        hash,
                        algorithm: algorithm.to_string(),
                    });
                }
            }
            Err(err) => {
                log::warn!(
                    "Failed to fetch GitHub checksum asset {}: {:#}",
                    asset.name,
                    err
                );
            }
        }
    }
    checksums
}

async fn fetch_checksum_asset_text(
    client: &reqwest::Client,
    asset: &GitHubAsset,
) -> Result<String> {
    let response = client
        .get(&asset.browser_download_url)
        .header("User-Agent", format!("Foxy/{}", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .with_context(|| {
            format!(
                "Failed to fetch checksum asset from {}",
                sanitize_log_url(&asset.browser_download_url)
            )
        })?;

    if !response.status().is_success() {
        anyhow::bail!(
            "GitHub checksum asset {} returned status {}",
            asset.name,
            response.status()
        );
    }

    response
        .text()
        .await
        .with_context(|| format!("Failed to read checksum asset {}", asset.name))
}

fn checksum_asset_algorithm(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "blake3sums" | "blake3sums.txt" | "b3sums" | "b3sums.txt" => Some("blake3"),
        "sha256sums" | "sha256sums.txt" | "checksums.txt" => Some("sha256"),
        _ => None,
    }
}

fn parse_checksum_text(text: &str) -> HashMap<String, String> {
    let mut checksums = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        if !hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
            continue;
        }
        let name = name.trim_start_matches('*').trim();
        if name.is_empty() {
            continue;
        }
        checksums.insert(name.to_string(), hash.to_ascii_lowercase());
        if let Some(file_name) = std::path::Path::new(name)
            .file_name()
            .and_then(|value| value.to_str())
        {
            checksums
                .entry(file_name.to_string())
                .or_insert_with(|| hash.to_ascii_lowercase());
        }
    }
    checksums
}

/// Extract changelogs from GitHub release bodies.
pub fn github_releases_to_changelogs(releases: &[GitHubRelease]) -> Vec<ChangelogVersion> {
    valid_releases(releases)
        .map(github_body_to_changelog)
        .collect()
}

/// Parse a single GitHub release body into a `ChangelogVersion`.
fn github_body_to_changelog(release: &GitHubRelease) -> ChangelogVersion {
    let version = strip_version_prefix(&release.tag_name).to_string();
    let date = release.published_at.clone().unwrap_or_default();
    let body = release.body.clone().unwrap_or_default();
    let sections = parse_markdown_changelog(&body);
    ChangelogVersion {
        version,
        date,
        sections,
    }
}

/// Attempt to parse markdown with `## Section` headings and `- item` bullets.
/// Falls back to a single "Changes" section with the raw body lines.
fn parse_markdown_changelog(body: &str) -> Vec<ChangelogSection> {
    let mut sections: Vec<ChangelogSection> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_items: Vec<String> = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            // Flush previous section
            if let Some(title) = current_title.take()
                && !current_items.is_empty()
            {
                sections.push(ChangelogSection {
                    title,
                    items: std::mem::take(&mut current_items),
                });
            }
            current_title = Some(heading.trim().to_string());
        } else if let Some(item) = trimmed.strip_prefix("- ") {
            current_items.push(item.to_string());
        } else if let Some(item) = trimmed.strip_prefix("* ") {
            current_items.push(item.to_string());
        }
    }

    // Flush last section
    if let Some(title) = current_title {
        if !current_items.is_empty() {
            sections.push(ChangelogSection {
                title,
                items: current_items,
            });
        }
    } else if !current_items.is_empty() {
        // No headings found - wrap everything in a single section
        sections.push(ChangelogSection {
            title: "Changes".to_string(),
            items: current_items,
        });
    } else if !body.trim().is_empty() {
        // No bullet items at all - treat each non-empty line as an item
        let items: Vec<String> = body
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if !items.is_empty() {
            sections.push(ChangelogSection {
                title: "Changes".to_string(),
                items,
            });
        }
    }

    sections
}
