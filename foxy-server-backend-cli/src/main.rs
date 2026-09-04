mod changelog_parser;
mod cli;
mod config;
mod discover;
mod hash;
mod keys;
mod mod_line;
mod srf;
mod types;
mod update_manifest;

use anyhow::{Context, Result};
use clap::Parser;
use cli::GenerationMode;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Instant;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Create {
            config,
            output,
            app_update_url,
            threads,
            mode,
            mod_line_prefix,
            mod_line_include_optional,
            collect_keys,
            keys_output,
            additional_keys,
        } => cmd_create(
            &config,
            &output,
            CreateOptions {
                app_update_url: app_update_url.as_deref(),
                threads,
                mode,
                no_progress: cli.no_progress,
                mod_line: mod_line::ModLineOptions {
                    prefix: &mod_line_prefix,
                    include_optional: mod_line_include_optional,
                },
                keys: KeyCollectionRequest {
                    enabled: collect_keys || keys_output.is_some() || !additional_keys.is_empty(),
                    dest: keys_output,
                    additional_sources: additional_keys,
                },
            },
        ),
        cli::Command::New { output } => cmd_new(&output),
        cli::Command::SetupAppUpdater {
            version,
            windows_installer,
            linux_installer,
            linux_aarch64_installer,
            changelog,
            output,
        } => cmd_setup_app_updater(
            &version,
            windows_installer.as_deref(),
            linux_installer.as_deref(),
            linux_aarch64_installer.as_deref(),
            &changelog,
            &output,
        ),
        cli::Command::NewAppUpdate {
            version,
            windows_installer,
            linux_installer,
            linux_aarch64_installer,
            changelog,
            output,
        } => cmd_new_app_update(
            &version,
            windows_installer.as_deref(),
            linux_installer.as_deref(),
            linux_aarch64_installer.as_deref(),
            &changelog,
            &output,
        ),
    }
}

/// Everything `create` needs beyond the config and output paths.
struct CreateOptions<'a> {
    app_update_url: Option<&'a str>,
    threads: usize,
    mode: GenerationMode,
    no_progress: bool,
    mod_line: mod_line::ModLineOptions<'a>,
    keys: KeyCollectionRequest,
}

/// How `create` was asked to build the combined keys folder.
struct KeyCollectionRequest {
    enabled: bool,
    dest: Option<std::path::PathBuf>,
    additional_sources: Vec<std::path::PathBuf>,
}

fn cmd_create(
    config_path: &std::path::Path,
    output_dir: &std::path::Path,
    options: CreateOptions<'_>,
) -> Result<()> {
    let CreateOptions {
        app_update_url,
        threads,
        mode,
        no_progress,
        mod_line: mod_line_options,
        keys: key_collection,
    } = options;
    let started = Instant::now();

    let mode_label = match mode {
        GenerationMode::Foxy => "FoxyMode (BLAKE3)",
        GenerationMode::Swifty => "SwiftyMode (MD5, legacy)",
        GenerationMode::Hybrid => "HybridMode (BLAKE3 + MD5)",
    };
    println!("Mode: {}", mode_label);

    // Configure rayon thread pool
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .context("Failed to configure thread pool")?;

    println!("Loading config from: {}", config_path.display());
    let (config, resolved_mods) = config::load_config(config_path)?;

    println!(
        "Repository: {} ({} required, {} optional mods)",
        config.repo_name,
        resolved_mods.iter().filter(|m| m.is_required).count(),
        resolved_mods.iter().filter(|m| !m.is_required).count(),
    );

    for m in &resolved_mods {
        println!(
            "  {} [{}]",
            m.mod_name,
            if m.is_required {
                "required"
            } else {
                "optional"
            }
        );
    }

    // Create output directory
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output dir: {}", output_dir.display()))?;

    // Process all mods (copy + hash)
    let progress = if no_progress {
        ProgressBar::hidden()
    } else {
        let progress = ProgressBar::new(0);
        progress.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files ({per_sec})")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("=> "),
        );
        progress
    };

    println!("Processing files with {} threads...", threads);
    let processed_mods = hash::process_mods(&resolved_mods, output_dir, &progress, mode)?;
    progress.finish_and_clear();

    // --- Write output artifacts based on mode ---

    let use_swifty = matches!(mode, GenerationMode::Swifty | GenerationMode::Hybrid);
    let use_foxy = matches!(mode, GenerationMode::Foxy | GenerationMode::Hybrid);

    // SwiftyMode artifacts: mod.srf + MD5 checksums in repo.json
    if use_swifty {
        println!("Writing mod.srf files...");
        for m in &processed_mods {
            srf::write_mod_srf(m, output_dir)?;
        }
    }

    // Compute foxy repo checksum once (used by foxy_addons.json and repo.json in FoxyMode)
    let foxy_repo_checksum = if use_foxy {
        Some(hash::compute_foxy_repo_checksum(&processed_mods))
    } else {
        None
    };

    // FoxyMode artifacts: foxy_addon.json + foxy_addons.json
    if use_foxy {
        println!("Writing foxy_addon.json files...");
        for m in &processed_mods {
            srf::write_foxy_addon_json(m, output_dir)?;
        }

        println!("Writing foxy_addons.json...");
        srf::write_foxy_addons_json(
            &processed_mods,
            foxy_repo_checksum.as_deref().unwrap(),
            output_dir,
        )?;
    }

    let repo_checksum = match mode {
        GenerationMode::Foxy => foxy_repo_checksum.unwrap(),
        GenerationMode::Swifty | GenerationMode::Hybrid => {
            hash::compute_repo_checksum(&processed_mods)
        }
    };
    let effective_app_update_url = app_update_url.or(config.app_update_url.as_deref());

    // Write repo.json
    println!("Writing repo.json...");
    srf::write_repo_json(
        &config,
        &processed_mods,
        &repo_checksum,
        output_dir,
        mode,
        effective_app_update_url,
    )?;

    let key_report = if key_collection.enabled {
        let dest = key_collection
            .dest
            .unwrap_or_else(|| output_dir.join("keys"));
        println!("Collecting keys into: {}", dest.display());
        let report = keys::collect_keys(
            output_dir,
            &processed_mods,
            &keys::KeyCollectionOptions {
                dest: &dest,
                additional_sources: &key_collection.additional_sources,
            },
        )?;
        for name in &report.conflicts {
            log::warn!(
                "Multiple different keys named {}; kept the first one found",
                name
            );
        }
        Some((dest, report))
    } else {
        None
    };

    // Summary
    let total_files: usize = processed_mods.iter().map(|m| m.files.len()).sum();
    let total_bytes: u64 = processed_mods
        .iter()
        .flat_map(|m| m.files.iter())
        .map(|f| f.length)
        .sum();
    let elapsed = started.elapsed();
    let throughput_mb = total_bytes as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64();

    println!();
    println!("Done!");
    println!("  Mode:       {}", mode_label);
    println!("  Mods:       {}", processed_mods.len());
    println!("  Files:      {}", total_files);
    println!(
        "  Total size: {:.2} MB",
        total_bytes as f64 / 1024.0 / 1024.0
    );
    println!("  Time:       {:.2}s", elapsed.as_secs_f64());
    println!("  Throughput: {:.2} MB/s", throughput_mb);
    println!("  Checksum:   {}", repo_checksum);
    println!("  Output:     {}", output_dir.display());

    if use_foxy {
        println!("  Artifacts:  foxy_addon.json (per mod), foxy_addons.json, repo.json");
    }
    if use_swifty {
        println!("  Artifacts:  mod.srf (per mod), repo.json");
    }
    if let Some((dest, report)) = &key_report {
        println!("  Keys:       {} in {}", report.copied, dest.display());
        if report.duplicates > 0 {
            println!("              {} duplicate keys skipped", report.duplicates);
        }
        if !report.conflicts.is_empty() {
            println!(
                "              {} conflicting key names kept at first match: {}",
                report.conflicts.len(),
                report.conflicts.join(", ")
            );
        }
    }

    println!();
    println!("Server mod line:");
    println!(
        "{}",
        mod_line::build_mod_line(
            config.dlc_content.as_ref(),
            &processed_mods,
            mod_line_options,
        )
    );

    Ok(())
}

fn cmd_new(output: &std::path::Path) -> Result<()> {
    if output.exists() {
        anyhow::bail!(
            "File already exists: {}. Remove it first or choose a different path.",
            output.display()
        );
    }
    config::generate_template_config(output)?;
    println!("Config template written to: {}", output.display());
    println!("Edit this file, then run:");
    println!(
        "  foxy-server-backend-cli create {} <output-dir>",
        output.display()
    );
    Ok(())
}

fn cmd_setup_app_updater(
    version: &str,
    windows_installer: Option<&std::path::Path>,
    linux_installer: Option<&std::path::Path>,
    linux_aarch64_installer: Option<&std::path::Path>,
    changelog_path: &std::path::Path,
    output_dir: &std::path::Path,
) -> Result<()> {
    use update_manifest::*;

    if windows_installer.is_none() && linux_installer.is_none() && linux_aarch64_installer.is_none()
    {
        anyhow::bail!(
            "At least one installer must be provided (--windows-installer, --linux-installer, or --linux-aarch64-installer)"
        );
    }

    // Parse changelog
    println!("Parsing changelog: {}", changelog_path.display());
    let changelog_versions = changelog_parser::parse_changelog(changelog_path)?;
    println!("  Found {} versions in changelog", changelog_versions.len());

    // Build platform entries for the target version
    let mut platforms = std::collections::HashMap::new();
    if let Some(path) = windows_installer {
        println!("Hashing Windows installer: {}", path.display());
        let entry = build_platform_entry(path, "installers")?;
        println!("  Hash: {}", entry.installer_hash);
        println!("  Size: {} bytes", entry.installer_size);
        platforms.insert("windows-x86_64".to_string(), entry);
    }
    insert_linux_platform_entry(&mut platforms, linux_installer, "linux-x86_64")?;
    insert_linux_platform_entry(&mut platforms, linux_aarch64_installer, "linux-aarch64")?;

    if !platforms.contains_key("windows-x86_64") {
        anyhow::bail!(
            "A Windows installer (--windows-installer) is required for each version in the manifest"
        );
    }

    // Create output directories
    let changelogs_dir = output_dir.join(CHANGELOGS_DIR);
    std::fs::create_dir_all(&changelogs_dir)
        .with_context(|| format!("Failed to create {}", changelogs_dir.display()))?;

    // Write changelog JSON only for the target version
    let target_changelog = changelog_parser::find_version(&changelog_versions, version)
        .with_context(|| {
            format!(
                "Version {} not found in {}",
                version,
                changelog_path.display()
            )
        })?;
    let changelog_filename = format!("{}.json", version);
    let changelog_relative = format!("{}/{}", CHANGELOGS_DIR, changelog_filename);
    let changelog_full_path = output_dir.join(&changelog_relative);
    let json =
        serde_json::to_string_pretty(target_changelog).context("Failed to serialize changelog")?;
    std::fs::write(&changelog_full_path, json)
        .with_context(|| format!("Failed to write {}", changelog_full_path.display()))?;
    println!("  Wrote changelog: {}", changelog_filename);

    // Build version entry (only the target version which has installers)
    let version_entry = VersionEntry {
        version: version.to_string(),
        changelog: changelog_relative.clone(),
        platforms,
    };

    let manifest = UpdateManifest {
        schema_version: CURRENT_SCHEMA_VERSION,
        latest: version.to_string(),
        versions: vec![version_entry],
    };

    write_manifest(&manifest, output_dir)?;

    println!();
    println!(
        "Done! Created update manifest at: {}",
        output_dir.join(MANIFEST_FILENAME).display()
    );
    println!("  Latest version: {}", version);
    println!("  Changelog files: 1");
    println!();
    println!("Server root structure:");
    println!("  {}/", output_dir.display());
    println!("    {}", MANIFEST_FILENAME);
    println!("    {}/", CHANGELOGS_DIR);
    println!("      {}", changelog_filename);
    println!();
    println!(
        "Remember to place your installer files in an 'installers/' directory on your server."
    );

    Ok(())
}

fn cmd_new_app_update(
    version: &str,
    windows_installer: Option<&std::path::Path>,
    linux_installer: Option<&std::path::Path>,
    linux_aarch64_installer: Option<&std::path::Path>,
    changelog_path: &std::path::Path,
    server_root: &std::path::Path,
) -> Result<()> {
    use update_manifest::*;

    if windows_installer.is_none() && linux_installer.is_none() && linux_aarch64_installer.is_none()
    {
        anyhow::bail!(
            "At least one installer must be provided (--windows-installer, --linux-installer, or --linux-aarch64-installer)"
        );
    }

    // Read existing manifest
    println!(
        "Reading existing manifest from: {}",
        server_root.join(MANIFEST_FILENAME).display()
    );
    let mut manifest = read_manifest(server_root)?;
    println!("  Current latest: {}", manifest.latest);
    println!("  Existing versions: {}", manifest.versions.len());

    // Check if version already exists
    if manifest.versions.iter().any(|v| v.version == version) {
        anyhow::bail!(
            "Version {} already exists in the manifest. Remove it first or use a different version.",
            version
        );
    }

    // Parse changelog and extract the target version
    println!("Parsing changelog: {}", changelog_path.display());
    let changelog_versions = changelog_parser::parse_changelog(changelog_path)?;
    let target_changelog = changelog_parser::find_version(&changelog_versions, version)
        .with_context(|| {
            format!(
                "Version {} not found in {}",
                version,
                changelog_path.display()
            )
        })?;

    // Write changelog JSON for the new version
    let changelogs_dir = server_root.join(CHANGELOGS_DIR);
    std::fs::create_dir_all(&changelogs_dir)
        .with_context(|| format!("Failed to create {}", changelogs_dir.display()))?;

    let changelog_filename = format!("{}.json", version);
    let changelog_relative = format!("{}/{}", CHANGELOGS_DIR, changelog_filename);
    let changelog_full_path = server_root.join(&changelog_relative);
    let json =
        serde_json::to_string_pretty(target_changelog).context("Failed to serialize changelog")?;
    std::fs::write(&changelog_full_path, json)
        .with_context(|| format!("Failed to write {}", changelog_full_path.display()))?;
    println!("  Wrote changelog: {}", changelog_filename);

    // Build platform entries
    let mut platforms = std::collections::HashMap::new();
    if let Some(path) = windows_installer {
        println!("Hashing Windows installer: {}", path.display());
        let entry = build_platform_entry(path, "installers")?;
        println!("  Hash: {}", entry.installer_hash);
        println!("  Size: {} bytes", entry.installer_size);
        platforms.insert("windows-x86_64".to_string(), entry);
    }
    insert_linux_platform_entry(&mut platforms, linux_installer, "linux-x86_64")?;
    insert_linux_platform_entry(&mut platforms, linux_aarch64_installer, "linux-aarch64")?;

    // Build new version entry
    let version_entry = VersionEntry {
        version: version.to_string(),
        changelog: changelog_relative,
        platforms,
    };

    // Prepend to versions array and update latest
    manifest.versions.insert(0, version_entry);
    manifest.latest = version.to_string();

    // Write updated manifest
    write_manifest(&manifest, server_root)?;

    println!();
    println!(
        "Done! Updated manifest at: {}",
        server_root.join(MANIFEST_FILENAME).display()
    );
    println!("  New latest version: {}", version);
    println!("  Total versions: {}", manifest.versions.len());
    println!("  Old versions preserved for downgrade support.");

    Ok(())
}

fn insert_linux_platform_entry(
    platforms: &mut std::collections::HashMap<String, update_manifest::PlatformEntry>,
    installer: Option<&std::path::Path>,
    fallback_platform_key: &str,
) -> Result<()> {
    let Some(path) = installer else {
        return Ok(());
    };
    let platform_key = linux_platform_key_for_installer(path, fallback_platform_key);
    println!(
        "Hashing Linux installer for {}: {}",
        platform_key,
        path.display()
    );
    let entry = update_manifest::build_platform_entry(path, "installers")?;
    println!("  Hash: {}", entry.installer_hash);
    println!("  Size: {} bytes", entry.installer_size);
    if platforms.insert(platform_key.to_string(), entry).is_some() {
        anyhow::bail!(
            "Duplicate app update installer for platform {}",
            platform_key
        );
    }
    Ok(())
}

fn linux_platform_key_for_installer(path: &std::path::Path, fallback: &str) -> &'static str {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.contains("aarch64") || name.contains("arm64") {
        "linux-aarch64"
    } else if name.contains("x86_64") || name.contains("amd64") {
        "linux-x86_64"
    } else if fallback == "linux-aarch64" {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    }
}
