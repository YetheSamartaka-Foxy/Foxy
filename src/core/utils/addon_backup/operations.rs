use super::helpers::*;
use super::types::*;
use anyhow::{Context, Result, anyhow, bail};
use log::{debug, info};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn backup_addon(backup_root: &Path, addon_path: &Path) -> Result<AddonBackupRecord> {
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

    build_backup_record(final_path, folder_name, addon_name, content_hash)
}

pub fn list_addon_backups(backup_root: &Path, addon_name: &str) -> Result<Vec<AddonBackupRecord>> {
    let normalized_name = sanitize_backup_component(addon_name);
    let mut records: Vec<AddonBackupRecord> = list_all_addon_backups(backup_root)?
        .into_iter()
        .filter(|record| sanitize_backup_component(&record.addon_name) == normalized_name)
        .collect();

    records.sort_by(|a, b| {
        b.created_at_unix_secs
            .cmp(&a.created_at_unix_secs)
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
            .then_with(|| b.created_at_unix_secs.cmp(&a.created_at_unix_secs))
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
        .with_context(|| format!("failed to delete backup {}", backup.path.display()))
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

pub fn cleanup_addon_backups(
    backup_root: &Path,
    policy: BackupCleanupPolicy,
) -> Result<BackupCleanupReport> {
    let records = list_all_addon_backups(backup_root)?;
    let mut report = BackupCleanupReport::default();

    if records.is_empty() {
        return Ok(report);
    }

    let max_age_cutoff_secs = policy.max_age_days.map(|days| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_sub(days.saturating_mul(24 * 60 * 60))
    });

    let mut by_addon: std::collections::HashMap<String, Vec<AddonBackupRecord>> =
        std::collections::HashMap::new();
    for record in records {
        by_addon
            .entry(record.addon_name.to_lowercase())
            .or_default()
            .push(record);
    }

    let mut to_delete: Vec<AddonBackupRecord> = Vec::new();
    for backups in by_addon.values_mut() {
        backups.sort_by(|a, b| {
            b.created_at_unix_secs
                .cmp(&a.created_at_unix_secs)
                .then_with(|| a.folder_name.cmp(&b.folder_name))
        });

        for (index, backup) in backups.iter().enumerate() {
            let exceeds_keep_latest = policy
                .keep_latest_per_addon
                .map(|keep| index >= keep)
                .unwrap_or(false);
            let exceeds_max_age = max_age_cutoff_secs
                .map(|cutoff| backup.created_at_unix_secs <= cutoff)
                .unwrap_or(false);
            if exceeds_keep_latest || exceeds_max_age {
                to_delete.push(backup.clone());
            }
        }
    }

    to_delete.sort_by(|a, b| a.path.cmp(&b.path));
    to_delete.dedup_by(|a, b| a.path == b.path);

    for backup in &to_delete {
        debug!(
            "Deleting addon backup: {} ({} bytes, created={})",
            backup.folder_name, backup.size_bytes, backup.created_at_unix_secs
        );
        delete_addon_backup(backup)?;
        report.deleted_backups += 1;
        report.freed_bytes = report.freed_bytes.saturating_add(backup.size_bytes);
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
