macro_rules! println {
    ($($arg:tt)*) => {
        if crate::output::json_mode() {
            std::eprintln!($($arg)*);
        } else {
            std::println!($($arg)*);
        }
    };
}

mod artifacts;
mod build_info;
mod changelog_parser;
mod cli;
mod config;
mod discover;
mod hash;
mod incremental;
mod keys;
mod mod_line;
mod operations;
mod output;
mod planner;
mod published;
mod report;
mod space;
mod srf;
mod staging;
mod types;
mod update_manifest;
mod verify;

use anyhow::{Context, Result};
use clap::Parser;
use cli::GenerationMode;
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Instant;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();

    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) || !std::env::args_os().any(|arg| arg == std::ffi::OsStr::new("--json"))
            {
                err.exit();
            }
            output::print_parse_error(&err.to_string());
            std::process::exit(err.exit_code());
        }
    };
    output::set_json_mode(cli.json);
    let command_name = match &cli.command {
        cli::Command::Create { .. } => "create",
        cli::Command::CreateSpace { .. } => "create-space",
        cli::Command::New { .. } => "new",
        cli::Command::NewSpace { .. } => "new-space",
        cli::Command::Validate { .. } => "validate",
        cli::Command::Verify { .. } => "verify",
        cli::Command::Diff { .. } => "diff",
        cli::Command::AuditKeys { .. } => "audit-keys",
        cli::Command::ExportReforgerConfig { .. } => "export-reforger-config",
        cli::Command::SetupAppUpdater { .. } => "setup-app-updater",
        cli::Command::NewAppUpdate { .. } => "new-app-update",
    };

    let result = (|| -> Result<()> {
        match cli.command {
            cli::Command::Create {
                config,
                output,
                dry_run,
                incremental,
                atomic,
                report,
                app_update_url,
                threads,
                mode,
                mod_line_prefix,
                mod_line_include_optional,
                prune_unused_optionals,
                yes,
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
                    prune_unused_optionals,
                    dry_run,
                    incremental,
                    atomic,
                    report,
                    yes,
                    keys: KeyCollectionRequest {
                        enabled: collect_keys
                            || keys_output.is_some()
                            || !additional_keys.is_empty(),
                        dest: keys_output,
                        additional_sources: additional_keys,
                    },
                },
            ),
            cli::Command::CreateSpace {
                config,
                output,
                layout,
                pool_dir,
                yes,
                clean,
                dry_run,
                incremental,
                atomic,
                report,
                only,
                prune_unused_optionals,
                app_update_url,
                threads,
                mode,
                mod_line_prefix,
                mod_line_include_optional,
                collect_keys,
                keys_output,
                additional_keys,
                per_repo_keys,
            } => {
                let options = space::CreateSpaceOptions {
                    layout,
                    pool_dir,
                    yes,
                    clean,
                    dry_run,
                    atomic,
                    prune_unused_optionals,
                    incremental,
                    only,
                    app_update_url: app_update_url.as_deref(),
                    threads,
                    mode,
                    no_progress: cli.no_progress,
                    mod_line: mod_line::ModLineOptions {
                        prefix: &mod_line_prefix,
                        include_optional: mod_line_include_optional,
                    },
                    keys: KeyCollectionRequest {
                        enabled: collect_keys
                            || keys_output.is_some()
                            || !additional_keys.is_empty(),
                        dest: keys_output,
                        additional_sources: additional_keys,
                    },
                    per_repo_keys,
                };
                if atomic && (layout == cli::SpaceLayout::Link || options.keys.dest.is_some()) {
                    anyhow::bail!(
                        "--atomic requires copy or pool layout and an output-local keys folder"
                    );
                }
                let before = (report && !dry_run)
                    .then(|| report::snapshot(&output))
                    .transpose()?;
                if atomic && !dry_run {
                    staging::publish(&output, |stage| {
                        space::cmd_create_space(&config, stage, options)
                    })?;
                } else {
                    space::cmd_create_space(&config, &output, options)?;
                }
                if let Some(before) = before {
                    report::print(&report::compare(&before, &report::snapshot(&output)?));
                }
                Ok(())
            }
            cli::Command::New { output, game } => cmd_new(&output, game),
            cli::Command::NewSpace { output } => space::cmd_new_space(&output),
            cli::Command::Validate {
                config,
                space,
                output,
            } => operations::validate(&config, space, output.as_deref()),
            cli::Command::Verify { output } => verify::verify(&output),
            cli::Command::Diff { old, new } => report::diff(&old, &new),
            cli::Command::AuditKeys {
                config,
                space,
                strict,
                additional_keys,
            } => operations::audit_keys(&config, space, strict, &additional_keys),
            cli::Command::ExportReforgerConfig {
                config,
                output,
                include_optional,
            } => operations::export_reforger_config(&config, &output, include_optional),
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
    })();
    if cli.json {
        output::print_result(command_name, &result);
    }
    result
}

/// Everything `create` needs beyond the config and output paths.
struct CreateOptions<'a> {
    app_update_url: Option<&'a str>,
    threads: usize,
    mode: GenerationMode,
    no_progress: bool,
    mod_line: mod_line::ModLineOptions<'a>,
    prune_unused_optionals: bool,
    dry_run: bool,
    incremental: bool,
    atomic: bool,
    report: bool,
    yes: bool,
    keys: KeyCollectionRequest,
}

/// How `create` / `create-space` was asked to build the combined keys folder.
pub(crate) struct KeyCollectionRequest {
    pub enabled: bool,
    pub dest: Option<std::path::PathBuf>,
    pub additional_sources: Vec<std::path::PathBuf>,
}

fn cmd_create(
    config_path: &std::path::Path,
    output_dir: &std::path::Path,
    options: CreateOptions<'_>,
) -> Result<()> {
    if options.dry_run {
        let (config, mods) = config::load_config(config_path)?;
        let mut plan = planner::create(
            &mods,
            output_dir,
            options.mode,
            options.prune_unused_optionals,
            options.incremental,
        )?;
        if options.atomic && output_dir.exists() {
            plan.add("replace-output", output_dir, 0);
        }
        let base = std::path::Path::new(&config.base_path);
        plan.add_images(
            &[
                (&config.icon_image_path, base),
                (&config.repo_image_path, base),
            ],
            output_dir,
        )?;
        if options.keys.enabled {
            let dest = options
                .keys
                .dest
                .clone()
                .unwrap_or_else(|| output_dir.join("keys"));
            plan.add_keys(
                &mods,
                &dest,
                &options.keys.additional_sources,
                options.prune_unused_optionals,
            )?;
        }
        plan.show();
        return Ok(());
    }
    if options.atomic && options.keys.dest.is_some() {
        anyhow::bail!("--atomic requires an output-local keys folder");
    }
    let before = options
        .report
        .then(|| report::snapshot(output_dir))
        .transpose()?;
    configure_thread_pool(options.threads)?;
    if options.atomic {
        staging::publish(output_dir, |stage| run_create(config_path, stage, options))?;
    } else {
        run_create(config_path, output_dir, options)?;
    }
    if let Some(before) = before {
        report::print(&report::compare(&before, &report::snapshot(output_dir)?));
    }
    Ok(())
}

fn run_create(
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
        prune_unused_optionals,
        incremental,
        dry_run: _,
        atomic: _,
        report: _,
        yes,
        keys: key_collection,
    } = options;
    let started = Instant::now();

    let mode_label = artifacts::mode_label(mode);
    println!("Mode: {}", mode_label);

    println!("Loading config from: {}", config_path.display());
    let (config, resolved_mods) = config::load_config(config_path)?;
    if prune_unused_optionals && !yes {
        anyhow::bail!("--prune-unused-optionals removes published files; re-run with --yes");
    }

    println!(
        "Repository: {} for {} ({} required, {} optional mods)",
        config.repo_name,
        config.game.display_name(),
        resolved_mods.iter().filter(|m| m.is_required).count(),
        resolved_mods.iter().filter(|m| !m.is_required).count(),
    );
    let warnings = mod_line::game_config_warnings(&config, &resolved_mods);
    for warning in &warnings {
        log::warn!("{}", warning);
    }

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
    if prune_unused_optionals {
        published::remove_published_optionals(output_dir, &resolved_mods)?;
    }

    let progress = progress_bar(no_progress);
    println!("Processing files with {} threads...", threads);
    let processed_mods = hash::process_mods(
        &resolved_mods,
        Some(output_dir),
        &progress,
        mode,
        prune_unused_optionals,
        incremental,
    )?;
    progress.finish_and_clear();

    println!("Writing mod manifests...");
    artifacts::write_mod_manifests(&processed_mods, output_dir, mode)?;

    println!("Writing repo.json...");
    let repo_checksum = artifacts::write_repo_manifests(
        &config,
        &processed_mods,
        output_dir,
        mode,
        app_update_url,
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

    for line in artifacts::artifact_lines(mode) {
        println!("  Artifacts:  {}", line);
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

    let server_line = mod_line::build_server_launch_line(
        &config,
        &processed_mods,
        &resolved_mods,
        mod_line_options,
    );
    published::write_server_mod_line(output_dir, &server_line)?;
    println!();
    println!("Server mod line:");
    println!("{server_line}");
    output::set_details(serde_json::json!({
        "output": output_dir,
        "mods": processed_mods.len(),
        "files": total_files,
        "checksum": repo_checksum,
        "serverLine": server_line,
        "warnings": warnings,
        "keyConflicts": key_report.as_ref().map(|(_, report)| report.conflicts.clone()).unwrap_or_default(),
    }));

    Ok(())
}

pub(crate) fn configure_thread_pool(threads: usize) -> Result<()> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .context("Failed to configure thread pool")
}

pub(crate) fn progress_bar(no_progress: bool) -> ProgressBar {
    if no_progress {
        return ProgressBar::hidden();
    }
    let progress = ProgressBar::new(0);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files ({per_sec})")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    progress
}

fn cmd_new(output: &std::path::Path, game: types::RepoGame) -> Result<()> {
    if output.exists() {
        anyhow::bail!(
            "File already exists: {}. Remove it first or choose a different path.",
            output.display()
        );
    }
    config::generate_template_config(output, game)?;
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

#[cfg(test)]
mod generation_tests {
    use super::*;

    #[test]
    fn create_publishes_selected_optional_without_parent_optionals() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("mods").join("@ace");
        let selected = source.join("optionals").join("@ace_selected");
        let unused = source.join("optionals").join("@ace_unused");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::create_dir_all(&unused).unwrap();
        std::fs::write(source.join("main.pbo"), b"main").unwrap();
        std::fs::write(selected.join("selected.pbo"), b"selected").unwrap();
        std::fs::write(unused.join("unused.pbo"), b"unused").unwrap();
        let config = dir.path().join("config.json");
        let value = serde_json::json!({
            "repoName": "ACE test",
            "basePath": dir.path().join("mods"),
            "requiredMods": [
                {"modName": "@ace"},
                {"modName": "@ace/optionals/@ace_selected"}
            ]
        });
        std::fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
        let output = dir.path().join("output");

        run_create(
            &config,
            &output,
            CreateOptions {
                app_update_url: None,
                threads: 1,
                mode: GenerationMode::Foxy,
                no_progress: true,
                mod_line: mod_line::ModLineOptions {
                    prefix: "mods",
                    include_optional: false,
                },
                prune_unused_optionals: true,
                dry_run: false,
                incremental: false,
                atomic: false,
                report: false,
                yes: true,
                keys: KeyCollectionRequest {
                    enabled: false,
                    dest: None,
                    additional_sources: vec![],
                },
            },
        )
        .unwrap();

        assert!(!output.join("@ace").join("optionals").exists());
        assert!(output.join("@ace_selected").join("selected.pbo").exists());
        assert!(unused.join("unused.pbo").exists());
        assert_eq!(
            std::fs::read_to_string(output.join(published::SERVER_MOD_LINE_FILE)).unwrap(),
            "-mod=mods/@ace;mods/@ace_selected;\n"
        );
        verify::verify(&output).unwrap();
        std::fs::write(
            output.join("@ace_selected").join("selected.pbo"),
            b"changed",
        )
        .unwrap();
        assert!(verify::verify(&output).is_err());
    }
}
