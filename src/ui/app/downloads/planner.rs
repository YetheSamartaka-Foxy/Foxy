use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use reqwest::blocking::Client;
use serde_json::Value;

use crate::ui::app::{DirectDownloadPlan, DirectDownloadTarget, Foxy};
use crate::ui::i18n::{tr, tr_fmt};

impl Foxy {
    fn fetch_json_blocking(client: &Client, url: &str) -> Result<Value, String> {
        let response = client.get(url).send().map_err(|err| {
            tr_fmt(
                "Failed to fetch {url}: {error}",
                &[("url", url.to_string()), ("error", err.to_string())],
            )
        })?;
        let response = response.error_for_status().map_err(|err| {
            tr_fmt(
                "Failed to fetch {url}: {error}",
                &[("url", url.to_string()), ("error", err.to_string())],
            )
        })?;
        let body = response.text().map_err(|err| {
            tr_fmt(
                "Failed to read response body from {url}: {error}",
                &[("url", url.to_string()), ("error", err.to_string())],
            )
        })?;

        // Match core JSON parser behavior: tolerate BOM / leading non-JSON bytes.
        let mut start = 0usize;
        let bytes = body.as_bytes();
        while start < bytes.len() {
            let byte = bytes[start];
            if byte == b'{' || byte == b'[' || byte.is_ascii_whitespace() {
                break;
            }
            start += 1;
        }
        let cleaned = body[start..].replace(['\r', '\n'], "").trim().to_string();

        serde_json::from_str::<Value>(&cleaned).map_err(|err| {
            tr_fmt(
                "Failed to parse JSON from {url}: {error}",
                &[("url", url.to_string()), ("error", err.to_string())],
            )
        })
    }

    fn ensure_trailing_slash(url: &str) -> String {
        let trimmed = url.trim();
        if trimmed.ends_with('/') {
            trimmed.to_string()
        } else {
            format!("{}/", trimmed)
        }
    }

    fn parent_url(url: &str) -> String {
        let trimmed = url.trim_end_matches('/');
        match trimmed.rsplit_once('/') {
            Some((parent, _)) => format!("{}/", parent),
            None => String::new(),
        }
    }

    fn join_remote_url(base: &str, child: &str) -> String {
        let base = base.trim_end_matches('/');
        let child = child.trim_start_matches('/');
        format!("{}/{}", base, child)
    }

    fn url_last_segment(url: &str) -> String {
        let path = url
            .split('?')
            .next()
            .unwrap_or(url)
            .trim_end_matches('/')
            .to_string();
        path.rsplit('/')
            .next()
            .filter(|segment| !segment.trim().is_empty())
            .unwrap_or("download.bin")
            .to_string()
    }

    fn sanitize_relative_path(raw_path: &str) -> PathBuf {
        let normalized = raw_path.replace('\\', "/");
        let mut relative = PathBuf::new();
        for component in Path::new(&normalized).components() {
            if let Component::Normal(name) = component {
                relative.push(name);
            }
        }
        relative
    }

    fn parse_mod_srf_targets(
        files_json: &Value,
        destination_root: &Path,
        addon_prefix: &str,
        addon_base_url: &str,
    ) -> Result<Vec<DirectDownloadTarget>, String> {
        let files = files_json
            .get("Files")
            .and_then(Value::as_array)
            .ok_or_else(|| tr("Invalid addon metadata: missing Files array"))?;

        let mut targets = Vec::new();
        for entry in files {
            let Some(path_value) = entry.get("Path").and_then(Value::as_str) else {
                continue;
            };
            let relative_path = Self::sanitize_relative_path(path_value);
            if relative_path.as_os_str().is_empty() {
                continue;
            }

            let remote_path = path_value.replace('\\', "/");
            let remote_url = Self::join_remote_url(addon_base_url, remote_path.as_str());
            let local_path = destination_root.join(addon_prefix).join(&relative_path);
            let size_bytes = entry
                .get("Length")
                .and_then(Value::as_i64)
                .map(|len| len.max(0) as u64)
                .unwrap_or(0);
            let label = format!(
                "{}/{}",
                addon_prefix,
                relative_path.to_string_lossy().replace('\\', "/")
            );
            targets.push(DirectDownloadTarget {
                remote_url,
                local_path,
                size_bytes,
                label,
            });
        }
        Ok(targets)
    }

    fn try_build_repo_direct_download_plan(
        client: &Client,
        source_url: &str,
        destination_root: &Path,
    ) -> Result<DirectDownloadPlan, String> {
        let repo_manifest_url = if source_url.trim_end_matches('/').ends_with("repo.json") {
            source_url.trim().to_string()
        } else {
            let base = Self::ensure_trailing_slash(source_url);
            Self::join_remote_url(&base, "repo.json")
        };
        let repo_base_url = Self::parent_url(&repo_manifest_url);
        if repo_base_url.is_empty() {
            return Err(tr("Invalid repository URL"));
        }

        let repository_json = Self::fetch_json_blocking(client, &repo_manifest_url)?;
        let mut addon_names = Vec::<String>::new();
        let mut seen = HashSet::<String>::new();
        for key in ["requiredMods", "optionalMods"] {
            if let Some(mods) = repository_json.get(key).and_then(Value::as_array) {
                for mod_entry in mods {
                    let Some(mod_name) = mod_entry.get("modName").and_then(Value::as_str) else {
                        continue;
                    };
                    let cleaned = mod_name.trim();
                    if cleaned.is_empty() {
                        continue;
                    }
                    let lowered = cleaned.to_lowercase();
                    if seen.insert(lowered) {
                        addon_names.push(cleaned.to_string());
                    }
                }
            }
        }

        if addon_names.is_empty() {
            return Err(tr("Repository metadata does not contain any addons"));
        }

        let mut files = Vec::new();
        for addon_name in addon_names {
            let addon_base_url = Self::join_remote_url(&repo_base_url, &addon_name);
            let manifest_url = Self::join_remote_url(&addon_base_url, "mod.srf");
            let addon_json = Self::fetch_json_blocking(client, &manifest_url)?;
            let mut addon_targets = Self::parse_mod_srf_targets(
                &addon_json,
                destination_root,
                &addon_name,
                &addon_base_url,
            )?;
            files.append(&mut addon_targets);
        }

        if files.is_empty() {
            return Err(tr("Repository contains no downloadable files"));
        }

        let total_bytes = files.iter().map(|file| file.size_bytes).sum::<u64>();
        let fallback_repository_label = tr("Repository");
        let target_label = repository_json
            .get("repoName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(fallback_repository_label.as_str())
            .to_string();
        Ok(DirectDownloadPlan {
            target_label,
            files,
            total_bytes,
        })
    }

    fn try_build_addon_direct_download_plan(
        client: &Client,
        source_url: &str,
        destination_root: &Path,
    ) -> Result<DirectDownloadPlan, String> {
        let manifest_url = if source_url.trim_end_matches('/').ends_with("mod.srf") {
            source_url.trim().to_string()
        } else {
            let base = Self::ensure_trailing_slash(source_url);
            Self::join_remote_url(&base, "mod.srf")
        };
        let addon_base_url = Self::parent_url(&manifest_url);
        if addon_base_url.is_empty() {
            return Err(tr("Invalid addon URL"));
        }

        let addon_json = Self::fetch_json_blocking(client, &manifest_url)?;
        let addon_name = Self::url_last_segment(&addon_base_url);
        let files = Self::parse_mod_srf_targets(
            &addon_json,
            destination_root,
            &addon_name,
            &addon_base_url,
        )?;
        if files.is_empty() {
            return Err(tr("Addon metadata contains no files"));
        }
        let total_bytes = files.iter().map(|file| file.size_bytes).sum::<u64>();
        Ok(DirectDownloadPlan {
            target_label: addon_name,
            files,
            total_bytes,
        })
    }

    fn build_single_file_direct_download_plan(
        client: &Client,
        source_url: &str,
        destination_root: &Path,
    ) -> Result<DirectDownloadPlan, String> {
        let filename = Self::url_last_segment(source_url);
        if filename.trim().is_empty() {
            return Err(tr("Failed to resolve a target filename from URL"));
        }

        let size_bytes = client
            .head(source_url)
            .send()
            .ok()
            .and_then(|response| response.error_for_status().ok())
            .and_then(|response| response.content_length())
            .unwrap_or(0);
        let local_path = destination_root.join(&filename);
        Ok(DirectDownloadPlan {
            target_label: filename.clone(),
            files: vec![DirectDownloadTarget {
                remote_url: source_url.to_string(),
                local_path,
                size_bytes,
                label: filename,
            }],
            total_bytes: size_bytes,
        })
    }

    pub(in crate::ui::app) fn build_direct_download_plan(
        client: &Client,
        source_url: &str,
        destination_root: &Path,
    ) -> Result<DirectDownloadPlan, String> {
        let source_url = source_url.trim();
        if source_url.is_empty() {
            return Err("Address is required".to_string());
        }

        if let Ok(plan) =
            Self::try_build_repo_direct_download_plan(client, source_url, destination_root)
        {
            return Ok(plan);
        }
        if let Ok(plan) =
            Self::try_build_addon_direct_download_plan(client, source_url, destination_root)
        {
            return Ok(plan);
        }

        Self::build_single_file_direct_download_plan(client, source_url, destination_root)
    }
}
