use super::helpers::*;
use super::types::*;
use anyhow::{Context, Result, anyhow, bail};
use log::{debug, info, warn};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn backup_addon(backup_root: &Path, addon_path: &Path) -> Result<AddonBackupRecord> {
    backup_addon_with_origin(backup_root, addon_path, None)
}

pub fn backup_addon_with_origin(
    backup_root: &Path,
    addon_path: &Path,
    origin: Option<&AddonBackupOrigin>,
) -> Result<AddonBackupRecord> {
    let addon_name = addon_directory_name(addon_path)?;
    let content_hash = calculate_addon_folder_content_hash(addon_path)
        .with_context(|| format!("failed to hash addon {}", addon_path.display()))?;
    if content_hash.is_empty() {
        bail!(
            "addon {} did not produce a content hash",
            addon_path.display()
        );
    }

    fs::create_dir_all(backup_root)
        .with_context(|| format!("failed to create backup root {}", backup_root.display()))?;

    let folder_name = backup_folder_name(&addon_name, &content_hash);
    let final_path = backup_root.join(&folder_name);
    if final_path.exists() {
        if !final_path.is_dir() {
            bail!(
                "backup destination exists but is not a directory: {}",
                final_path.display()
            );
        }
        if let Some(origin) = origin {
            add_origin(backup_root, &folder_name, origin)?;
        }
        return build_backup_record(final_path, folder_name, addon_name, content_hash);
    }

    let staging_path = unique_staging_path(backup_root, &folder_name);
    if let Err(err) = copy_directory_recursive(addon_path, &staging_path) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&staging_path, &final_path) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(err).with_context(|| {
            format!(
                "failed to finalize backup {} -> {}",
                staging_path.display(),
                final_path.display()
            )
        });
    }

    if let Some(origin) = origin {
        add_origin(backup_root, &folder_name, origin)?;
    }
    build_backup_record(final_path, folder_name, addon_name, content_hash)
}

#[cfg(test)]
pub fn list_addon_backups_for_origin(
    backup_root: &Path,
    addon_name: &str,
    origin: &AddonBackupOrigin,
) -> Result<Vec<AddonBackupRecord>> {
    let mut backups = list_addon_backups(backup_root, addon_name)?;
    let matching = backups.iter().any(|record| record.origins.contains(origin));
    if matching {
        backups.retain(|record| record.origins.contains(origin));
    }
    Ok(backups)
}

pub fn list_addon_backups(backup_root: &Path, addon_name: &str) -> Result<Vec<AddonBackupRecord>> {
    let normalized_name = sanitize_backup_component(addon_name);
    let mut records: Vec<AddonBackupRecord> = list_all_addon_backups(backup_root)?
        .into_iter()
        .filter(|record| sanitize_backup_component(&record.addon_name) == normalized_name)
        .collect();

    records.sort_by(|a, b| {
        b.created_at_unix_nanos
            .cmp(&a.created_at_unix_nanos)
            .then_with(|| a.folder_name.cmp(&b.folder_name))
    });
    Ok(records)
}

pub fn list_all_addon_backups(backup_root: &Path) -> Result<Vec<AddonBackupRecord>> {
    if !backup_root.exists() {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();

    for entry in fs::read_dir(backup_root)
        .with_context(|| format!("failed to read backup root {}", backup_root.display()))?
    {
        let entry = entry.with_context(|| {
            format!(
                "failed to read entry under backup root {}",
                backup_root.display()
            )
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let folder_name = entry.file_name().to_string_lossy().to_string();
        let Some((content_hash, addon_name)) = parse_backup_folder_name(&folder_name) else {
            continue;
        };
        records.push(build_backup_record(
            path,
            folder_name,
            addon_name,
            content_hash,
        )?);
    }

    records.sort_by(|a, b| {
        a.addon_name
            .to_lowercase()
            .cmp(&b.addon_name.to_lowercase())
            .then_with(|| b.created_at_unix_nanos.cmp(&a.created_at_unix_nanos))
            .then_with(|| a.folder_name.cmp(&b.folder_name))
    });
    Ok(records)
}

pub fn restore_addon_backup(backup: &AddonBackupRecord, destination: &Path) -> Result<()> {
    if !backup.path.exists() || !backup.path.is_dir() {
        bail!("backup path does not exist: {}", backup.path.display());
    }
    let parent = destination.parent().ok_or_else(|| {
        anyhow!(
            "destination has no parent directory: {}",
            destination.display()
        )
    })?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create addon parent {}", parent.display()))?;

    let staging_name = format!(
        "{}.restore.{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("addon"),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let staging_path = parent.join(staging_name);

    if let Err(err) = copy_directory_recursive(&backup.path, &staging_path) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(err);
    }
    if destination.exists() {
        if destination.is_dir() {
            fs::remove_dir_all(destination).with_context(|| {
                format!(
                    "failed to remove existing addon directory {}",
                    destination.display()
                )
            })?;
        } else {
            let _ = fs::remove_dir_all(&staging_path);
            bail!(
                "restore destination exists but is not a directory: {}",
                destination.display()
            );
        }
    }

    if let Err(err) = fs::rename(&staging_path, destination) {
        let _ = fs::remove_dir_all(&staging_path);
        return Err(err).with_context(|| {
            format!(
                "failed to finalize restore {} -> {}",
                staging_path.display(),
                destination.display()
            )
        });
    }

    Ok(())
}

pub fn delete_addon_backup(backup: &AddonBackupRecord) -> Result<()> {
    if !backup.path.exists() {
        return Ok(());
    }
    if !backup.path.is_dir() {
        bail!("backup path is not a directory: {}", backup.path.display());
    }

    fs::remove_dir_all(&backup.path)
        .with_context(|| format!("failed to delete backup {}", backup.path.display()))?;
    if let Some(root) = backup.path.parent()
        && let Err(err) = remove_origins(root, &backup.folder_name)
    {
        warn!("Failed to remove addon backup origin metadata: {}", err);
    }
    Ok(())
}

pub fn delete_addon_backups(backup_root: &Path, addon_name: &str) -> Result<usize> {
    let backups = list_addon_backups(backup_root, addon_name)?;
    let mut deleted = 0usize;
    for backup in &backups {
        delete_addon_backup(backup)?;
        deleted += 1;
    }
    Ok(deleted)
}

#[cfg(test)]
pub fn cleanup_addon_backups(
    backup_root: &Path,
    policy: BackupCleanupPolicy,
) -> Result<BackupCleanupReport> {
    cleanup_addon_backups_confirmed(backup_root, policy, false)
}

#[cfg(test)]
pub fn cleanup_addon_backups_confirmed(
    backup_root: &Path,
    policy: BackupCleanupPolicy,
    allow_final_recovery_copy: bool,
) -> Result<BackupCleanupReport> {
    let preview = preview_addon_backup_cleanup(backup_root, policy)?;
    cleanup_addon_backup_preview_confirmed(backup_root, &preview, allow_final_recovery_copy)
}

pub fn cleanup_addon_backup_preview_confirmed(
    backup_root: &Path,
    preview: &BackupCleanupPreview,
    allow_final_recovery_copy: bool,
) -> Result<BackupCleanupReport> {
    let current = preview_addon_backup_cleanup(backup_root, preview.policy)?;
    if current.inventory != preview.inventory
        || current.deletions != preview.deletions
        || current.final_recovery_copies != preview.final_recovery_copies
    {
        bail!("addon backup inventory or cleanup eligibility changed; create a new preview");
    }

    let mut report = BackupCleanupReport::default();
    let deletions = preview.deletions.iter().chain(
        allow_final_recovery_copy
            .then_some(preview.final_recovery_copies.iter())
            .into_iter()
            .flatten(),
    );
    for backup in deletions {
        if backup.path.parent() != Some(backup_root) {
            report.failed_folders.push(backup.folder_name.clone());
            continue;
        }
        if !backup.path.exists() {
            report.failed_folders.push(backup.folder_name.clone());
            continue;
        }
        debug!(
            "Deleting addon backup: {} ({} bytes, created={})",
            backup.folder_name, backup.size_bytes, backup.created_at_unix_secs
        );
        match delete_addon_backup(backup) {
            Ok(()) => {
                report.deleted_backups += 1;
                report.freed_bytes = report.freed_bytes.saturating_add(backup.size_bytes);
                report.deleted_folders.push(backup.folder_name.clone());
            }
            Err(err) => {
                warn!(
                    "Failed to delete addon backup {}: {}",
                    backup.folder_name, err
                );
                report.failed_folders.push(backup.folder_name.clone());
            }
        }
    }

    if report.deleted_backups > 0 {
        info!(
            "Backup cleanup complete: deleted {} backups, freed {:.1} MB",
            report.deleted_backups,
            report.freed_bytes as f64 / (1024.0 * 1024.0)
        );
    }

    Ok(report)
}

pub fn preview_addon_backup_cleanup(
    backup_root: &Path,
    policy: BackupCleanupPolicy,
) -> Result<BackupCleanupPreview> {
    let records = list_all_addon_backups(backup_root)?;
    let inventory = records.clone();
    let cutoff = policy.max_age_days.map(|days| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(days.saturating_mul(24 * 60 * 60))
    });
    let mut groups: std::collections::HashMap<
        (String, Option<AddonBackupOrigin>),
        Vec<AddonBackupRecord>,
    > = std::collections::HashMap::new();
    for record in &records {
        let origins: Vec<Option<AddonBackupOrigin>> = if record.origins.is_empty() {
            vec![None]
        } else {
            record.origins.iter().cloned().map(Some).collect()
        };
        for origin in origins {
            groups
                .entry((record.addon_name.to_lowercase(), origin))
                .or_default()
                .push(record.clone());
        }
    }
    let mut count_eligible: std::collections::HashMap<PathBuf, bool> = records
        .iter()
        .map(|record| (record.path.clone(), true))
        .collect();
    for backups in groups.values_mut() {
        backups.sort_by(|a, b| {
            b.created_at_unix_nanos
                .cmp(&a.created_at_unix_nanos)
                .then_with(|| a.folder_name.cmp(&b.folder_name))
        });
        for (index, backup) in backups.iter().enumerate() {
            if policy
                .keep_latest_per_addon
                .is_some_and(|keep| index < keep)
            {
                count_eligible.insert(backup.path.clone(), false);
            }
        }
    }
    let candidate_paths: std::collections::HashSet<PathBuf> = records
        .iter()
        .filter(|record| {
            cutoff.is_some_and(|age| record.created_at_unix_secs <= age)
                || policy.keep_latest_per_addon.is_some() && count_eligible[&record.path]
        })
        .map(|record| record.path.clone())
        .collect();
    let mut final_paths = std::collections::HashSet::new();
    for backups in groups.values() {
        if let Some(last) = backups.last()
            && backups
                .iter()
                .all(|backup| candidate_paths.contains(&backup.path))
        {
            final_paths.insert(last.path.clone());
        }
    }
    let mut deletions = Vec::new();
    let mut final_recovery_copies = Vec::new();
    for backup in records {
        if !candidate_paths.contains(&backup.path) {
            continue;
        }
        if final_paths.contains(&backup.path) {
            final_recovery_copies.push(backup);
        } else {
            deletions.push(backup);
        }
    }
    deletions.sort_by(|a, b| a.path.cmp(&b.path));
    deletions.dedup_by(|a, b| a.path == b.path);
    final_recovery_copies.sort_by(|a, b| a.path.cmp(&b.path));
    final_recovery_copies.dedup_by(|a, b| a.path == b.path);
    Ok(BackupCleanupPreview {
        policy,
        inventory,
        deletions,
        final_recovery_copies,
    })
}
