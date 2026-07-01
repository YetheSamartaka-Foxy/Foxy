use super::{AppState, CommandError, CommandSuccess, progress_output_muted};
use crate::cli::args::{CliArgs, DirectDownloadArgs};
use crate::cli::exit_codes;
use crate::core::utils::app_paths;
use crate::ui::types::SettingsViewState;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

pub fn cmd_direct_download(
    cli: &CliArgs,
    args: DirectDownloadArgs,
) -> Result<CommandSuccess, CommandError> {
    let state = AppState::load()?;
    let source_url = args.address.trim().to_string();
    if source_url.is_empty() {
        return Err(CommandError::validation(
            "direct-download",
            "Address is required",
        ));
    }

    let speed_limit_mbps = resolve_direct_download_speed_limit(&args, &state.settings)?;
    let destination = resolve_direct_download_destination(args.dest, &state.settings)?;

    if !destination.exists() && !cli.dry_run {
        fs::create_dir_all(&destination).map_err(|e| {
            CommandError::operation(
                "direct-download",
                format!("Failed to create destination folder: {}", e),
            )
        })?;
    }
    if destination.exists() && !destination.is_dir() {
        return Err(CommandError::validation(
            "direct-download",
            "Destination path is not a folder",
        ));
    }

    let client = Client::builder().build().map_err(|e| {
        CommandError::operation(
            "direct-download",
            format!("Failed to initialize HTTP client: {}", e),
        )
    })?;
    let plan = build_direct_download_plan(&client, &source_url, &destination).map_err(|e| {
        CommandError::operation("direct-download", format!("Failed to build plan: {}", e))
    })?;

    if cli.dry_run {
        return Ok(CommandSuccess {
            action: "direct-download".to_string(),
            message: "Dry-run: direct download plan previewed".to_string(),
            data: json!({
                "source_url": source_url,
                "destination": destination.display().to_string(),
                "speed_limit_mbps": speed_limit_mbps,
                "target_label": plan.target_label,
                "files_total": plan.files.len(),
                "total_bytes": plan.total_bytes,
                "dry_run": true
            }),
            exit_code: exit_codes::SUCCESS,
        });
    }

    let started_at = Instant::now();
    let summary =
        run_direct_download(&client, &plan, speed_limit_mbps, progress_output_muted(cli))?;

    Ok(CommandSuccess {
        action: "direct-download".to_string(),
        message: "Direct download completed".to_string(),
        data: json!({
            "source_url": source_url,
            "destination": destination.display().to_string(),
            "speed_limit_mbps": speed_limit_mbps,
            "target_label": plan.target_label,
            "files_total": plan.files.len(),
            "total_bytes": plan.total_bytes,
            "summary": summary,
            "elapsed_ms": started_at.elapsed().as_millis()
        }),
        exit_code: exit_codes::SUCCESS,
    })
}

fn resolve_direct_download_destination(
    cli_dest: Option<PathBuf>,
    settings: &SettingsViewState,
) -> Result<PathBuf, CommandError> {
    if let Some(dest) = cli_dest {
        return Ok(dest);
    }
    if !settings.temp_directory.trim().is_empty() {
        return Ok(PathBuf::from(settings.temp_directory.trim()));
    }
    Ok(app_paths::foxy_data_dir())
}

fn resolve_direct_download_speed_limit(
    args: &DirectDownloadArgs,
    settings: &SettingsViewState,
) -> Result<Option<u32>, CommandError> {
    if args.unlimited && args.limit_mbps.is_some() {
        return Err(CommandError::validation(
            "direct-download",
            "--unlimited and --limit-mbps are mutually exclusive",
        ));
    }
    if args.use_global_speed_limit && (args.unlimited || args.limit_mbps.is_some()) {
        return Err(CommandError::validation(
            "direct-download",
            "--use-global-speed-limit cannot be combined with --unlimited or --limit-mbps",
        ));
    }
    if args.unlimited {
        return Ok(None);
    }
    if let Some(limit) = args.limit_mbps {
        return Ok(Some(limit.max(1)));
    }
    Ok(settings
        .download_speed_limit_mbps
        .filter(|limit| *limit > 0))
}

#[derive(Clone, Debug)]
struct DirectDownloadTarget {
    remote_url: String,
    local_path: PathBuf,
    size_bytes: u64,
    label: String,
}

#[derive(Clone, Debug)]
struct DirectDownloadPlan {
    target_label: String,
    files: Vec<DirectDownloadTarget>,
    total_bytes: u64,
}

fn run_direct_download(
    client: &Client,
    plan: &DirectDownloadPlan,
    speed_limit_mbps: Option<u32>,
    quiet: bool,
) -> Result<Value, CommandError> {
    let started_at = Instant::now();
    let files_total = plan.files.len();
    if files_total == 0 {
        return Err(CommandError::validation(
            "direct-download",
            "No files found to download",
        ));
    }

    let mut files_done = 0usize;
    let mut downloaded_total = 0u64;
    let speed_limit_bps = speed_limit_mbps.map(|limit| (limit as f64 * 1_000_000.0) / 8.0);
    let mut throttle_started = Instant::now();
    let mut throttle_bytes = 0u64;
    let mut last_print = Instant::now();

    for target in &plan.files {
        if let Some(parent) = target.local_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CommandError::operation(
                    "direct-download",
                    format!("Failed to create destination folder: {}", e),
                )
            })?;
        }

        let mut response = client
            .get(&target.remote_url)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|e| {
                CommandError::operation(
                    "direct-download",
                    format!("Download failed for {}: {}", target.remote_url, e),
                )
            })?;

        let mut out = fs::File::create(&target.local_path).map_err(|e| {
            CommandError::operation(
                "direct-download",
                format!(
                    "Failed to create destination file {}: {}",
                    target.local_path.display(),
                    e
                ),
            )
        })?;

        let mut file_done = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = response.read(&mut buffer).map_err(|e| {
                CommandError::operation(
                    "direct-download",
                    format!("Failed reading remote stream {}: {}", target.remote_url, e),
                )
            })?;
            if read == 0 {
                break;
            }

            out.write_all(&buffer[..read]).map_err(|e| {
                CommandError::operation(
                    "direct-download",
                    format!(
                        "Failed writing destination file {}: {}",
                        target.local_path.display(),
                        e
                    ),
                )
            })?;

            let added = read as u64;
            file_done = file_done.saturating_add(added);
            downloaded_total = downloaded_total.saturating_add(added);
            throttle_bytes = throttle_bytes.saturating_add(added);

            if let Some(limit_bps) = speed_limit_bps {
                let elapsed = throttle_started.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    let current = throttle_bytes as f64 / elapsed;
                    if current > limit_bps {
                        let desired = throttle_bytes as f64 / limit_bps;
                        let sleep = (desired - elapsed).max(0.0);
                        if sleep > 0.0 {
                            std::thread::sleep(Duration::from_secs_f64(sleep));
                        }
                    }
                }
                if throttle_started.elapsed() >= Duration::from_secs(1) {
                    throttle_started = Instant::now();
                    throttle_bytes = 0;
                }
            }

            if !quiet && last_print.elapsed() >= Duration::from_millis(300) {
                let overall_pct = if plan.total_bytes > 0 {
                    (downloaded_total as f32 / plan.total_bytes as f32).clamp(0.0, 1.0)
                } else if files_total > 0 {
                    (files_done as f32 / files_total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                println!(
                    "{} [{:.0}%] {} / {} bytes",
                    target.label,
                    overall_pct * 100.0,
                    downloaded_total,
                    plan.total_bytes
                );
                last_print = Instant::now();
            }
        }

        files_done = files_done.saturating_add(1);
        if !quiet {
            let file_pct = if target.size_bytes == 0 {
                100.0
            } else {
                (file_done as f64 / target.size_bytes as f64 * 100.0).clamp(0.0, 100.0)
            };
            println!(
                "Finished {} ({:.0}% of file) [{}/{}]",
                target.label, file_pct, files_done, files_total
            );
        }
    }

    let elapsed = started_at.elapsed();
    let avg_speed_bps = if elapsed.as_secs_f64() > 0.0 {
        downloaded_total as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    Ok(json!({
        "files_done": files_done,
        "files_total": files_total,
        "downloaded_bytes": downloaded_total,
        "total_bytes": plan.total_bytes,
        "elapsed_ms": elapsed.as_millis(),
        "avg_speed_bps": avg_speed_bps
    }))
}

fn fetch_json_blocking(client: &Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to fetch {}: {}", url, e))?;
    let response = response
        .error_for_status()
        .map_err(|e| format!("Failed to fetch {}: {}", url, e))?;
    let body = response
        .text()
        .map_err(|e| format!("Failed to read response body from {}: {}", url, e))?;

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

    serde_json::from_str::<Value>(&cleaned)
        .map_err(|e| format!("Failed to parse JSON from {}: {}", url, e))
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
        .filter(|s| !s.trim().is_empty())
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
        .ok_or_else(|| "Invalid addon metadata: missing Files array".to_string())?;

    let mut targets = Vec::new();
    for entry in files {
        let Some(path_value) = entry.get("Path").and_then(Value::as_str) else {
            continue;
        };
        let relative_path = sanitize_relative_path(path_value);
        if relative_path.as_os_str().is_empty() {
            continue;
        }

        let remote_path = path_value.replace('\\', "/");
        let remote_url = join_remote_url(addon_base_url, &remote_path);
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
        let base = ensure_trailing_slash(source_url);
        join_remote_url(&base, "repo.json")
    };

    let repo_base_url = parent_url(&repo_manifest_url);
    if repo_base_url.is_empty() {
        return Err("Invalid repository URL".to_string());
    }

    let repository_json = fetch_json_blocking(client, &repo_manifest_url)?;
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
        return Err("Repository metadata does not contain any addons".to_string());
    }

    let mut files = Vec::new();
    for addon_name in addon_names {
        let addon_base_url = join_remote_url(&repo_base_url, &addon_name);
        let manifest_url = join_remote_url(&addon_base_url, "mod.srf");
        let addon_json = fetch_json_blocking(client, &manifest_url)?;
        let mut addon_targets =
            parse_mod_srf_targets(&addon_json, destination_root, &addon_name, &addon_base_url)?;
        files.append(&mut addon_targets);
    }

    if files.is_empty() {
        return Err("Repository contains no downloadable files".to_string());
    }

    let total_bytes = files.iter().map(|file| file.size_bytes).sum::<u64>();
    let target_label = repository_json
        .get("repoName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Repository")
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
        let base = ensure_trailing_slash(source_url);
        join_remote_url(&base, "mod.srf")
    };

    let addon_base_url = parent_url(&manifest_url);
    if addon_base_url.is_empty() {
        return Err("Invalid addon URL".to_string());
    }

    let addon_json = fetch_json_blocking(client, &manifest_url)?;
    let addon_name = url_last_segment(&addon_base_url);
    let files = parse_mod_srf_targets(&addon_json, destination_root, &addon_name, &addon_base_url)?;

    if files.is_empty() {
        return Err("Addon metadata contains no files".to_string());
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
    let filename = url_last_segment(source_url);
    if filename.trim().is_empty() {
        return Err("Failed to resolve target filename from URL".to_string());
    }

    let size_bytes = client
        .head(source_url)
        .send()
        .ok()
        .and_then(|response| response.error_for_status().ok())
        .and_then(|response| response.content_length())
        .unwrap_or(0);

    Ok(DirectDownloadPlan {
        target_label: filename.clone(),
        files: vec![DirectDownloadTarget {
            remote_url: source_url.to_string(),
            local_path: destination_root.join(&filename),
            size_bytes,
            label: filename,
        }],
        total_bytes: size_bytes,
    })
}

fn build_direct_download_plan(
    client: &Client,
    source_url: &str,
    destination_root: &Path,
) -> Result<DirectDownloadPlan, String> {
    let source_url = source_url.trim();
    if source_url.is_empty() {
        return Err("Address is required".to_string());
    }

    if let Ok(plan) = try_build_repo_direct_download_plan(client, source_url, destination_root) {
        return Ok(plan);
    }
    if let Ok(plan) = try_build_addon_direct_download_plan(client, source_url, destination_root) {
        return Ok(plan);
    }

    build_single_file_direct_download_plan(client, source_url, destination_root)
}
