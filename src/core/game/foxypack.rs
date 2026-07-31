use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::core::game::spaces::ActiveGameSpace;
use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::is_safe_child_path;
use crate::ui::types::{Repository, RepositorySpace};

use super::{GameModule, Profile, extra_files, reforger, workshop};

pub const FOXY_PACK_SCHEMA_VERSION: u32 = 1;

/// Ceiling on the total uncompressed payload an import will unpack. Packs carry
/// only the user's own config and extra files, so anything past this is either a
/// mistake or a decompression bomb aimed at the game space directory.
pub const MAX_PACK_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FoxyPackManifest {
    pub schema_version: u32,
    pub created_at: u64,
    pub foxy_version: String,
    pub game_id: String,
    pub game_space_id: String,
    pub game_space_display_name: String,
    #[serde(default)]
    pub profiles: Vec<PackProfileSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackProfileSummary {
    pub name: String,
    pub repository_name: String,
    pub repository_url: String,
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_folder: Option<String>,
    #[serde(default)]
    pub extra_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackRepositoriesFile {
    pub schema_version: u32,
    #[serde(default)]
    pub repositories: Vec<PackRepositoryEntry>,
    #[serde(default)]
    pub repository_spaces: Vec<RepositorySpace>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackRepositoryEntry {
    pub repository: Repository,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkshopEntry {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<u32>,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_updated: Option<u64>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub pack_path: String,
    pub game_id: String,
    pub repository_count: usize,
    pub repository_space_count: usize,
    pub profile_count: usize,
    pub extra_file_count: usize,
    pub workshop_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportSummary {
    pub pack_path: String,
    pub game_id: String,
    pub repositories_added: usize,
    pub repositories_updated: usize,
    pub repository_spaces_added: usize,
    pub repository_spaces_updated: usize,
    pub profile_count: usize,
    pub extra_file_count: usize,
    pub workshop_count: usize,
}

/// A filesystem location an import would write to or start syncing into. Packs
/// are shared between users, so every path a pack chooses is surfaced before
/// the import is confirmed rather than only counted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackWriteTarget {
    /// `extra_file` or `repository`.
    pub kind: String,
    pub name: String,
    /// Destination as written in the pack, `{game_dir}` still unexpanded.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PackInspection {
    pub pack_path: String,
    pub schema_version: u32,
    pub game_id: String,
    pub game_space_id: String,
    pub game_space_display_name: String,
    pub created_at: u64,
    pub repository_count: usize,
    pub repository_space_count: usize,
    pub profile_count: usize,
    pub extra_file_count: usize,
    pub workshop_count: usize,
    /// Every path the import would write to or sync into.
    pub write_targets: Vec<PackWriteTarget>,
    /// Uncompressed size of the pack's payloads, so an oversized pack is
    /// visible before it is unpacked into the game space.
    pub uncompressed_bytes: u64,
}

pub fn export_pack(
    space_dir: &Path,
    output_path: &Path,
    module: &dyn GameModule,
    active_space: &ActiveGameSpace,
    repositories: &[Repository],
    repository_spaces: &[RepositorySpace],
) -> Result<ExportSummary, String> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "Failed to create output directory {}: {}",
                sanitize_log_path(parent),
                err
            )
        })?;
    }

    let manifest = build_manifest(module, active_space, repositories);
    let repositories_file = build_repositories_file(module, repositories, repository_spaces);
    let extra_files_file = extra_files::load_store(space_dir)?;
    let workshop_file = build_workshop_file(space_dir, module)?;

    let temp_path = pack_temp_path(output_path);
    if temp_path.exists() {
        fs::remove_file(&temp_path).map_err(|err| {
            format!(
                "Failed to clear stale pack temp file {}: {}",
                sanitize_log_path(&temp_path),
                err
            )
        })?;
    }

    let result = write_pack_archive(
        space_dir,
        &temp_path,
        &manifest,
        &repositories_file,
        &extra_files_file,
        &workshop_file,
    );
    if let Err(err) = result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    if output_path.exists() {
        fs::remove_file(output_path).map_err(|err| {
            format!(
                "Failed to replace existing pack {}: {}",
                sanitize_log_path(output_path),
                err
            )
        })?;
    }
    fs::rename(&temp_path, output_path).map_err(|err| {
        let _ = fs::remove_file(&temp_path);
        format!(
            "Failed to move pack into place {}: {}",
            sanitize_log_path(output_path),
            err
        )
    })?;

    Ok(ExportSummary {
        pack_path: output_path.display().to_string(),
        game_id: manifest.game_id,
        repository_count: repositories_file.repositories.len(),
        repository_space_count: repositories_file.repository_spaces.len(),
        profile_count: manifest.profiles.len(),
        extra_file_count: extra_files_file.entries.len(),
        workshop_count: workshop_file.len(),
    })
}

pub fn inspect_pack(input_path: &Path) -> Result<PackInspection, String> {
    let mut archive = open_archive(input_path)?;
    let manifest: FoxyPackManifest = read_required_json(&mut archive, "manifest.json")?;
    validate_schema(manifest.schema_version)?;
    let repositories_file: PackRepositoriesFile =
        read_required_json(&mut archive, "repositories.json")?;
    validate_schema(repositories_file.schema_version)?;
    let extra_files_file: extra_files::ExtraFilesFile =
        read_required_json(&mut archive, "extra_files.json")?;
    let workshop_file: Vec<WorkshopEntry> =
        read_optional_json(&mut archive, "workshop.json")?.unwrap_or_default();

    Ok(PackInspection {
        pack_path: input_path.display().to_string(),
        schema_version: manifest.schema_version,
        game_id: manifest.game_id,
        game_space_id: manifest.game_space_id,
        game_space_display_name: manifest.game_space_display_name,
        created_at: manifest.created_at,
        repository_count: repositories_file.repositories.len(),
        repository_space_count: repositories_file.repository_spaces.len(),
        profile_count: manifest.profiles.len(),
        extra_file_count: extra_files_file.entries.len(),
        workshop_count: workshop_file.len(),
        write_targets: pack_write_targets(&extra_files_file, &repositories_file),
        uncompressed_bytes: archive_uncompressed_bytes(&mut archive),
    })
}

fn pack_write_targets(
    extra_files_file: &extra_files::ExtraFilesFile,
    repositories_file: &PackRepositoriesFile,
) -> Vec<PackWriteTarget> {
    let mut targets: Vec<PackWriteTarget> = extra_files_file
        .entries
        .iter()
        .map(|entry| PackWriteTarget {
            kind: "extra_file".to_string(),
            name: entry.name.clone(),
            path: entry.destination.clone(),
        })
        .collect();
    targets.extend(
        repositories_file
            .repositories
            .iter()
            .map(|entry| PackWriteTarget {
                kind: "repository".to_string(),
                name: entry.repository.name.clone(),
                path: entry.repository.path.clone(),
            }),
    );
    targets
}

fn archive_uncompressed_bytes(archive: &mut ZipArchive<fs::File>) -> u64 {
    (0..archive.len())
        .filter_map(|index| archive.by_index(index).ok().map(|file| file.size()))
        .sum()
}

pub fn import_pack(
    space_dir: &Path,
    input_path: &Path,
    module: &dyn GameModule,
    repositories: &mut Vec<Repository>,
    repository_spaces: &mut Vec<RepositorySpace>,
) -> Result<ImportSummary, String> {
    let mut archive = open_archive(input_path)?;
    let manifest: FoxyPackManifest = read_required_json(&mut archive, "manifest.json")?;
    validate_schema(manifest.schema_version)?;
    if manifest.game_id != module.id() {
        return Err(format!(
            "Pack is for game {} but the active game is {}",
            manifest.game_id,
            module.id()
        ));
    }

    let repositories_file: PackRepositoriesFile =
        read_required_json(&mut archive, "repositories.json")?;
    validate_schema(repositories_file.schema_version)?;
    let extra_files_file: extra_files::ExtraFilesFile =
        read_required_json(&mut archive, "extra_files.json")?;
    let workshop_file: Vec<WorkshopEntry> =
        read_optional_json(&mut archive, "workshop.json")?.unwrap_or_default();

    let (repository_spaces_added, repository_spaces_updated) =
        import_repository_spaces(repository_spaces, repositories_file.repository_spaces);
    let profile_count = repositories_file
        .repositories
        .iter()
        .map(|entry| entry.profiles.len())
        .sum();
    let (repositories_added, repositories_updated) =
        import_repositories(module, repositories, repositories_file.repositories);
    let extra_file_count = import_extra_files(space_dir, &mut archive, extra_files_file)?;
    let workshop_count = import_workshop_entries(space_dir, module, workshop_file)?;

    Ok(ImportSummary {
        pack_path: input_path.display().to_string(),
        game_id: manifest.game_id,
        repositories_added,
        repositories_updated,
        repository_spaces_added,
        repository_spaces_updated,
        profile_count,
        extra_file_count,
        workshop_count,
    })
}

fn build_manifest(
    module: &dyn GameModule,
    active_space: &ActiveGameSpace,
    repositories: &[Repository],
) -> FoxyPackManifest {
    let profiles = repositories
        .iter()
        .flat_map(|repository| {
            repository.profiles.iter().map(|repository_profile| {
                let profile =
                    module.repository_profile_to_profile(repository_profile, &repository.address);
                PackProfileSummary {
                    name: profile.name,
                    repository_name: repository.name.clone(),
                    repository_url: normalize_repo_url(&repository.address),
                    selected: repository.selected_profile.as_deref()
                        == Some(repository_profile.name.as_str()),
                    config_folder: profile.config_folder,
                    extra_files: profile.extra_files,
                }
            })
        })
        .collect();

    FoxyPackManifest {
        schema_version: FOXY_PACK_SCHEMA_VERSION,
        created_at: unix_timestamp_now(),
        foxy_version: crate::build_info::version_label(),
        game_id: module.id().to_string(),
        game_space_id: active_space.space_id.clone(),
        game_space_display_name: active_space.display_name.clone(),
        profiles,
    }
}

fn build_repositories_file(
    module: &dyn GameModule,
    repositories: &[Repository],
    repository_spaces: &[RepositorySpace],
) -> PackRepositoriesFile {
    let repositories = repositories
        .iter()
        .map(|repository| {
            let profiles = repository
                .profiles
                .iter()
                .map(|profile| module.repository_profile_to_profile(profile, &repository.address))
                .collect();
            let mut repository = repository.clone();
            repository.address = normalize_repo_url(&repository.address);
            repository.profiles.clear();
            PackRepositoryEntry {
                repository,
                profiles,
            }
        })
        .collect();

    PackRepositoriesFile {
        schema_version: FOXY_PACK_SCHEMA_VERSION,
        repositories,
        repository_spaces: repository_spaces.to_vec(),
    }
}

fn build_workshop_file(
    space_dir: &Path,
    module: &dyn GameModule,
) -> Result<Vec<WorkshopEntry>, String> {
    if module.id() == reforger::REFORGER_GAME_ID {
        let store = reforger::load_store(space_dir)?;
        return Ok(store
            .entries
            .iter()
            .map(|entry| WorkshopEntry {
                source: reforger::REFORGER_SOURCE.to_string(),
                app_id: None,
                item_id: entry.guid.clone(),
                title: entry.name.clone(),
                url: None,
                enabled: entry.enabled,
                frozen: entry.frozen,
                version: entry.version.clone(),
                size_bytes: entry.size_bytes,
                time_updated: None,
            })
            .collect());
    }

    let store = workshop::load_store(space_dir)?;
    Ok(store
        .entries
        .iter()
        .map(|entry| WorkshopEntry {
            source: entry.source.clone(),
            app_id: Some(entry.app_id),
            item_id: entry.item_id.clone(),
            title: entry.title.clone(),
            url: Some(entry.url.clone()),
            enabled: entry.enabled,
            frozen: entry.frozen,
            version: entry.version.clone(),
            size_bytes: entry.size_bytes,
            time_updated: entry.time_updated,
        })
        .collect())
}

fn import_workshop_entries(
    space_dir: &Path,
    module: &dyn GameModule,
    incoming: Vec<WorkshopEntry>,
) -> Result<usize, String> {
    let module_app_id = module.steam_app_id();
    let mut imported = 0usize;
    for entry in incoming {
        validate_workshop_entry(&entry)?;
        if entry.source.eq_ignore_ascii_case(reforger::REFORGER_SOURCE) {
            if module.id() != reforger::REFORGER_GAME_ID {
                return Err(format!(
                    "Pack workshop entry {} is for Arma Reforger but the active game is {}",
                    entry.item_id,
                    module.id()
                ));
            }
            let item = reforger::ReforgerAddonEntry {
                source: reforger::REFORGER_SOURCE.to_string(),
                guid: entry.item_id,
                name: entry.title,
                enabled: entry.enabled,
                frozen: entry.frozen,
                version: entry.version,
                installed_path: None,
                managed_path: None,
                frozen_path: None,
                size_bytes: entry.size_bytes,
                added_at: unix_timestamp_now(),
                updated_at: unix_timestamp_now(),
            };
            reforger::upsert_imported_addon(space_dir, item)?;
            imported += 1;
            continue;
        }
        let app_id = entry
            .app_id
            .or(module_app_id)
            .ok_or_else(|| "Pack workshop entry is missing a Steam app id".to_string())?;
        if let Some(module_app_id) = module_app_id
            && app_id != module_app_id
        {
            return Err(format!(
                "Pack workshop entry {} is for app {} but the active game uses app {}",
                entry.item_id, app_id, module_app_id
            ));
        }
        let item_id = workshop::normalize_workshop_id(&entry.item_id)
            .ok_or_else(|| format!("Invalid Steam Workshop item id {}", entry.item_id))?;
        let item = workshop::SteamWorkshopItem {
            source: workshop::STEAM_SOURCE.to_string(),
            app_id,
            item_id,
            title: entry.title,
            url: entry
                .url
                .unwrap_or_else(|| workshop::workshop_url(&entry.item_id)),
            enabled: entry.enabled,
            frozen: entry.frozen,
            version: entry.version,
            installed_path: None,
            frozen_path: None,
            size_bytes: entry.size_bytes,
            time_updated: entry.time_updated,
            added_at: unix_timestamp_now(),
            updated_at: unix_timestamp_now(),
        };
        workshop::upsert_imported_item(space_dir, item)?;
        imported += 1;
    }
    Ok(imported)
}

fn write_pack_archive(
    space_dir: &Path,
    output_path: &Path,
    manifest: &FoxyPackManifest,
    repositories_file: &PackRepositoriesFile,
    extra_files_file: &extra_files::ExtraFilesFile,
    workshop_file: &[WorkshopEntry],
) -> Result<(), String> {
    let file = fs::File::create(output_path).map_err(|err| {
        format!(
            "Failed to create pack {}: {}",
            sanitize_log_path(output_path),
            err
        )
    })?;
    let mut zip = ZipWriter::new(file);
    let options = zip_options();

    write_json_entry(&mut zip, options, "manifest.json", manifest)?;
    write_json_entry(&mut zip, options, "workshop.json", workshop_file)?;
    write_json_entry(&mut zip, options, "repositories.json", repositories_file)?;
    write_json_entry(&mut zip, options, "extra_files.json", extra_files_file)?;

    for entry in &extra_files_file.entries {
        validate_extra_file_entry(entry)?;
        let payload_root = extra_files::payload_dir(space_dir, &entry.id).join(&entry.source_name);
        if !payload_root.exists() {
            return Err(format!(
                "Managed extra file {} is missing its stored payload",
                entry.id
            ));
        }
        let archive_name = format!(
            "extra_files/{}/{}",
            entry.id,
            normalize_zip_path_component(&entry.source_name)
        );
        add_path_to_zip(&mut zip, options, &payload_root, &archive_name)?;
    }

    zip.finish()
        .map_err(|err| format!("Failed to finish pack: {}", err))?;
    Ok(())
}

fn write_json_entry<T: Serialize + ?Sized>(
    zip: &mut ZipWriter<fs::File>,
    options: SimpleFileOptions,
    name: &str,
    value: &T,
) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("Failed to serialize {}: {}", name, err))?;
    zip.start_file(name, options)
        .map_err(|err| format!("Failed to write {}: {}", name, err))?;
    zip.write_all(&data)
        .map_err(|err| format!("Failed to write {}: {}", name, err))
}

fn add_path_to_zip(
    zip: &mut ZipWriter<fs::File>,
    options: SimpleFileOptions,
    source: &Path,
    archive_name: &str,
) -> Result<(), String> {
    let metadata = fs::metadata(source)
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;

    if metadata.is_dir() {
        let directory_name = format!("{}/", archive_name.trim_end_matches('/'));
        zip.add_directory(&directory_name, options)
            .map_err(|err| format!("Failed to add {}: {}", directory_name, err))?;
        let mut children = fs::read_dir(source)
            .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let child_name = child.file_name().to_string_lossy().to_string();
            if !is_safe_child_path(&child_name) {
                return Err(format!("Unsafe extra-file payload name {}", child_name));
            }
            add_path_to_zip(
                zip,
                options,
                &child.path(),
                &format!("{}/{}", archive_name.trim_end_matches('/'), child_name),
            )?;
        }
        return Ok(());
    }

    zip.start_file(archive_name, options)
        .map_err(|err| format!("Failed to add {}: {}", archive_name, err))?;
    let mut file = fs::File::open(source)
        .map_err(|err| format!("Failed to open {}: {}", sanitize_log_path(source), err))?;
    std::io::copy(&mut file, zip)
        .map_err(|err| format!("Failed to copy {}: {}", sanitize_log_path(source), err))?;
    Ok(())
}

fn import_repository_spaces(
    target: &mut Vec<RepositorySpace>,
    incoming: Vec<RepositorySpace>,
) -> (usize, usize) {
    let mut added = 0;
    let mut updated = 0;
    for space in incoming {
        if let Some(existing) = target.iter_mut().find(|existing| existing.id == space.id) {
            *existing = space;
            updated += 1;
        } else {
            target.push(space);
            added += 1;
        }
    }
    (added, updated)
}

fn import_repositories(
    module: &dyn GameModule,
    target: &mut Vec<Repository>,
    incoming: Vec<PackRepositoryEntry>,
) -> (usize, usize) {
    let mut added = 0;
    let mut updated = 0;
    for entry in incoming {
        let mut repository = entry.repository;
        repository.address = normalize_repo_url(&repository.address);
        repository.profiles = entry
            .profiles
            .iter()
            .map(|profile| module.profile_to_repository_profile(profile))
            .collect();
        let key = repository_key(&repository);
        if let Some(existing) = target
            .iter_mut()
            .find(|candidate| repository_key(candidate) == key)
        {
            *existing = repository;
            updated += 1;
        } else {
            target.push(repository);
            added += 1;
        }
    }
    (added, updated)
}

fn import_extra_files(
    space_dir: &Path,
    archive: &mut ZipArchive<fs::File>,
    incoming: extra_files::ExtraFilesFile,
) -> Result<usize, String> {
    for entry in &incoming.entries {
        validate_extra_file_entry(entry)?;
    }

    let temp_root = space_dir.join("extra_files.import.tmp");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root)
            .map_err(|err| format!("Failed to clear {}: {}", sanitize_log_path(&temp_root), err))?;
    }
    fs::create_dir_all(&temp_root).map_err(|err| {
        format!(
            "Failed to create {}: {}",
            sanitize_log_path(&temp_root),
            err
        )
    })?;

    let imported_count = incoming.entries.len();
    let result = extract_extra_files_to_temp(archive, &incoming, &temp_root).and_then(|_| {
        let mut store = extra_files::load_store(space_dir)?;
        for entry in incoming.entries {
            let imported_dir = temp_root.join(&entry.id);
            if !imported_dir.exists() {
                return Err(format!(
                    "Pack is missing payload for managed extra file {}",
                    entry.id
                ));
            }
            let target_dir = extra_files::payload_dir(space_dir, &entry.id);
            if target_dir.exists() {
                fs::remove_dir_all(&target_dir).map_err(|err| {
                    format!(
                        "Failed to replace {}: {}",
                        sanitize_log_path(&target_dir),
                        err
                    )
                })?;
            }
            if let Some(parent) = target_dir.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    format!("Failed to create {}: {}", sanitize_log_path(parent), err)
                })?;
            }
            fs::rename(&imported_dir, &target_dir).map_err(|err| {
                format!(
                    "Failed to import {}: {}",
                    sanitize_log_path(&target_dir),
                    err
                )
            })?;
            upsert_extra_file_entry(&mut store, entry);
        }
        extra_files::save_store(space_dir, &store)?;
        Ok(imported_count)
    });

    let _ = fs::remove_dir_all(&temp_root);
    result
}

fn extract_extra_files_to_temp(
    archive: &mut ZipArchive<fs::File>,
    incoming: &extra_files::ExtraFilesFile,
    temp_root: &Path,
) -> Result<(), String> {
    let mut found_ids = std::collections::HashSet::new();
    let mut extracted_bytes: u64 = 0;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|err| format!("Failed to read zip entry {}: {}", index, err))?;
        let name = file.name().replace('\\', "/");
        let Some((entry_id, relative)) = extra_file_archive_entry(&name, incoming) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        if !is_safe_child_path(relative) {
            return Err(format!("Unsafe extra-file path in pack: {}", name));
        }
        extracted_bytes = extracted_bytes.saturating_add(file.size());
        if extracted_bytes > MAX_PACK_UNCOMPRESSED_BYTES {
            return Err(format!(
                "Pack payloads exceed the {} MiB import limit",
                MAX_PACK_UNCOMPRESSED_BYTES / (1024 * 1024)
            ));
        }
        found_ids.insert(entry_id.to_string());
        let target = temp_root.join(entry_id).join(relative);
        if file.is_dir() || name.ends_with('/') {
            fs::create_dir_all(&target).map_err(|err| {
                format!("Failed to create {}: {}", sanitize_log_path(&target), err)
            })?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    format!("Failed to create {}: {}", sanitize_log_path(parent), err)
                })?;
            }
            let mut output = fs::File::create(&target).map_err(|err| {
                format!("Failed to create {}: {}", sanitize_log_path(&target), err)
            })?;
            std::io::copy(&mut file, &mut output).map_err(|err| {
                format!(
                    "Failed to extract {} to {}: {}",
                    name,
                    sanitize_log_path(&target),
                    err
                )
            })?;
        }
    }

    for entry in &incoming.entries {
        if !found_ids.contains(&entry.id) {
            return Err(format!(
                "Pack is missing payload for managed extra file {}",
                entry.id
            ));
        }
    }
    Ok(())
}

fn extra_file_archive_entry<'a>(
    archive_name: &'a str,
    incoming: &'a extra_files::ExtraFilesFile,
) -> Option<(&'a str, &'a str)> {
    let rest = archive_name.strip_prefix("extra_files/")?;
    let (entry_id, relative) = rest.split_once('/')?;
    if incoming.entry(entry_id).is_some() {
        Some((entry_id, relative))
    } else {
        None
    }
}

fn upsert_extra_file_entry(
    store: &mut extra_files::ExtraFilesFile,
    entry: extra_files::ExtraFileEntry,
) {
    if let Some(existing) = store
        .entries
        .iter_mut()
        .find(|candidate| candidate.id == entry.id)
    {
        *existing = entry;
    } else {
        store.entries.push(entry);
    }
}

fn read_required_json<T: for<'de> Deserialize<'de>>(
    archive: &mut ZipArchive<fs::File>,
    name: &str,
) -> Result<T, String> {
    let mut file = archive
        .by_name(name)
        .map_err(|err| format!("Pack is missing {}: {}", name, err))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|err| format!("Failed to read {}: {}", name, err))?;
    serde_json::from_str(&raw).map_err(|err| format!("Failed to parse {}: {}", name, err))
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(
    archive: &mut ZipArchive<fs::File>,
    name: &str,
) -> Result<Option<T>, String> {
    match archive.by_name(name) {
        Ok(mut file) => {
            let mut raw = String::new();
            file.read_to_string(&mut raw)
                .map_err(|err| format!("Failed to read {}: {}", name, err))?;
            serde_json::from_str(&raw)
                .map(Some)
                .map_err(|err| format!("Failed to parse {}: {}", name, err))
        }
        Err(ZipError::FileNotFound) => Ok(None),
        Err(err) => Err(format!("Failed to read {}: {}", name, err)),
    }
}

fn open_archive(path: &Path) -> Result<ZipArchive<fs::File>, String> {
    let file = fs::File::open(path)
        .map_err(|err| format!("Failed to open {}: {}", sanitize_log_path(path), err))?;
    ZipArchive::new(file).map_err(|err| format!("Failed to read pack as zip: {}", err))
}

fn validate_schema(schema_version: u32) -> Result<(), String> {
    if schema_version == FOXY_PACK_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(format!(
            "Unsupported foxypack schema version {}",
            schema_version
        ))
    }
}

fn validate_extra_file_entry(entry: &extra_files::ExtraFileEntry) -> Result<(), String> {
    extra_files::validate_entry(entry)
}

fn validate_workshop_entry(entry: &WorkshopEntry) -> Result<(), String> {
    let source = entry.source.trim();
    if source.is_empty() {
        return Err("Workshop source must not be empty".to_string());
    }
    if entry.source.eq_ignore_ascii_case(workshop::STEAM_SOURCE) {
        return workshop::validate_pack_entry_id(&entry.item_id);
    }
    if entry.source.eq_ignore_ascii_case(reforger::REFORGER_SOURCE) {
        return reforger::validate_pack_entry_id(&entry.item_id);
    }
    Err(format!("Unsupported workshop source {}", source))
}

fn zip_options() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

fn pack_temp_path(output_path: &Path) -> PathBuf {
    let mut value = output_path.as_os_str().to_os_string();
    value.push(".tmp");
    PathBuf::from(value)
}

fn repository_key(repository: &Repository) -> (String, String) {
    (
        normalize_repo_url(&repository.address),
        crate::core::models::repository::normalize_repository_local_path_identity(&repository.path),
    )
}

fn normalize_repo_url(value: &str) -> String {
    let mut normalized = value.trim().replace('\\', "/");
    if !normalized.is_empty() && !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized
}

fn normalize_zip_path_component(value: &str) -> String {
    value.replace('\\', "/")
}

fn unix_timestamp_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::game::arma3::Arma3Module;
    use crate::core::game::reforger::{self, ReforgerModule};
    use crate::ui::types::RepositoryProfile;

    #[test]
    fn export_import_round_trips_repositories_profiles_and_extra_files() {
        let source_space = tempfile::tempdir().expect("source space");
        let work = tempfile::tempdir().expect("work");
        let payload = work.path().join("server.cfg");
        fs::write(&payload, "cfg").expect("payload");
        extra_files::add_entry(
            source_space.path(),
            "Server Config",
            &payload,
            "{game_dir}/userconfig/server.cfg",
            true,
        )
        .expect("add extra file");

        let repositories = vec![Repository {
            name: "Main".to_string(),
            address: "https://repo.example/main".to_string(),
            path: "D:/Foxy/Main".to_string(),
            profiles: vec![RepositoryProfile {
                name: "Operation".to_string(),
                gm: true,
                skip_intro: true,
                additional_params: "-window".to_string(),
                addons: vec![("@core".to_string(), true)],
                ..RepositoryProfile::default()
            }],
            selected_profile: Some("Operation".to_string()),
            ..Repository::default()
        }];
        let spaces = vec![RepositorySpace {
            id: "main-space".to_string(),
            name: "Main Space".to_string(),
            local_name_override: None,
            collapsed: false,
            source_address: String::new(),
            source_base_url: String::new(),
            shared_path: String::new(),
            icon_image_path: String::new(),
            icon_image_checksum: String::new(),
            repo_image_path: String::new(),
            repo_image_checksum: String::new(),
            app_update_url: String::new(),
            entries: Vec::new(),
        }];
        let active = ActiveGameSpace {
            space_id: "arma3".to_string(),
            game_id: "arma3".to_string(),
            display_name: "Arma 3".to_string(),
        };
        let pack = work.path().join("phase3.foxypack");

        let export = export_pack(
            source_space.path(),
            &pack,
            &Arma3Module,
            &active,
            &repositories,
            &spaces,
        )
        .expect("export");

        assert_eq!(export.repository_count, 1);
        assert_eq!(export.profile_count, 1);
        assert_eq!(export.extra_file_count, 1);
        assert_eq!(export.workshop_count, 0);

        let inspection = inspect_pack(&pack).expect("inspect");
        assert_eq!(inspection.game_id, "arma3");
        assert_eq!(inspection.repository_count, 1);
        assert_eq!(inspection.extra_file_count, 1);

        let target_space = tempfile::tempdir().expect("target space");
        let mut imported_repositories = Vec::new();
        let mut imported_spaces = Vec::new();
        let import = import_pack(
            target_space.path(),
            &pack,
            &Arma3Module,
            &mut imported_repositories,
            &mut imported_spaces,
        )
        .expect("import");

        assert_eq!(import.repositories_added, 1);
        assert_eq!(import.repository_spaces_added, 1);
        assert_eq!(import.profile_count, 1);
        assert_eq!(import.extra_file_count, 1);
        assert_eq!(import.workshop_count, 0);
        assert_eq!(imported_repositories.len(), 1);
        assert_eq!(
            imported_repositories[0].address,
            "https://repo.example/main/"
        );
        assert_eq!(imported_repositories[0].profiles.len(), 1);
        assert!(imported_repositories[0].profiles[0].gm);
        assert!(imported_repositories[0].profiles[0].skip_intro);
        assert_eq!(imported_spaces.len(), 1);
        assert!(
            extra_files::payload_dir(target_space.path(), "server-config")
                .join("server.cfg")
                .is_file()
        );
    }

    #[test]
    fn inspect_rejects_unsupported_schema_version() {
        let dir = tempfile::tempdir().expect("dir");
        let pack = dir.path().join("bad.foxypack");
        let file = fs::File::create(&pack).expect("pack file");
        let mut zip = ZipWriter::new(file);
        let options = zip_options();
        write_json_entry(
            &mut zip,
            options,
            "manifest.json",
            &serde_json::json!({
                "schema_version": 999,
                "created_at": 0,
                "foxy_version": "test",
                "game_id": "arma3",
                "game_space_id": "arma3",
                "game_space_display_name": "Arma 3",
                "profiles": []
            }),
        )
        .expect("manifest");
        write_json_entry(
            &mut zip,
            options,
            "repositories.json",
            &serde_json::json!({
                "schema_version": 999,
                "repositories": [],
                "repository_spaces": []
            }),
        )
        .expect("repositories");
        write_json_entry(
            &mut zip,
            options,
            "extra_files.json",
            &serde_json::json!({"entries": []}),
        )
        .expect("extra files");
        zip.finish().expect("finish");

        let err = inspect_pack(&pack).expect_err("schema should be rejected");

        assert!(err.contains("Unsupported foxypack schema version"));
    }

    #[test]
    fn export_import_round_trips_workshop_entries() {
        let source_space = tempfile::tempdir().expect("source space");
        let work = tempfile::tempdir().expect("work");
        workshop::upsert_item(
            source_space.path(),
            107410,
            "463939057",
            Some("ACE3".to_string()),
            None,
            None,
            true,
        )
        .expect("workshop upsert");

        let active = ActiveGameSpace {
            space_id: "arma3".to_string(),
            game_id: "arma3".to_string(),
            display_name: "Arma 3".to_string(),
        };
        let pack = work.path().join("workshop.foxypack");

        let export = export_pack(source_space.path(), &pack, &Arma3Module, &active, &[], &[])
            .expect("export");

        assert_eq!(export.workshop_count, 1);
        let inspection = inspect_pack(&pack).expect("inspect");
        assert_eq!(inspection.workshop_count, 1);

        let target_space = tempfile::tempdir().expect("target space");
        let mut repos = Vec::new();
        let mut spaces = Vec::new();
        let import = import_pack(
            target_space.path(),
            &pack,
            &Arma3Module,
            &mut repos,
            &mut spaces,
        )
        .expect("import");

        assert_eq!(import.workshop_count, 1);
        let store = workshop::load_store(target_space.path()).expect("store");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].item_id, "463939057");
        assert_eq!(store.entries[0].title.as_deref(), Some("ACE3"));
    }

    #[test]
    fn export_import_round_trips_reforger_guid_entries() {
        let source_space = tempfile::tempdir().expect("source space");
        let work = tempfile::tempdir().expect("work");
        reforger::upsert_imported_addon(
            source_space.path(),
            reforger::ReforgerAddonEntry {
                source: reforger::REFORGER_SOURCE.to_string(),
                guid: "596ABCDEF0123456".to_string(),
                name: Some("Capture".to_string()),
                enabled: true,
                frozen: false,
                version: Some("1.2.3".to_string()),
                installed_path: None,
                managed_path: None,
                frozen_path: None,
                size_bytes: Some(42),
                added_at: 0,
                updated_at: 0,
            },
        )
        .expect("reforger upsert");

        let active = ActiveGameSpace {
            space_id: "reforger".to_string(),
            game_id: "reforger".to_string(),
            display_name: "Arma Reforger".to_string(),
        };
        let pack = work.path().join("reforger.foxypack");

        let export = export_pack(
            source_space.path(),
            &pack,
            &ReforgerModule,
            &active,
            &[],
            &[],
        )
        .expect("export");

        assert_eq!(export.workshop_count, 1);
        let inspection = inspect_pack(&pack).expect("inspect");
        assert_eq!(inspection.game_id, "reforger");
        assert_eq!(inspection.workshop_count, 1);

        let target_space = tempfile::tempdir().expect("target space");
        let mut repos = Vec::new();
        let mut spaces = Vec::new();
        let import = import_pack(
            target_space.path(),
            &pack,
            &ReforgerModule,
            &mut repos,
            &mut spaces,
        )
        .expect("import");

        assert_eq!(import.workshop_count, 1);
        let store = reforger::load_store(target_space.path()).expect("store");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].guid, "596ABCDEF0123456");
        assert_eq!(store.entries[0].name.as_deref(), Some("Capture"));
    }
}
