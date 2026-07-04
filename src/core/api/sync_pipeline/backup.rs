use super::super::*;
use crate::core::utils::addon_backup;
use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::resolve_child_dir_case_insensitive;

pub(super) fn backup_pending_addons_for_download(
    backup_root_raw: &str,
    repo_root: &str,
    mods: &[ModDiffSummary],
    progress_tx: &Sender<ProgressEvent>,
    operation_id: &str,
) -> Result<()> {
    let repo_root = repo_root.trim();
    if repo_root.is_empty() {
        anyhow::bail!("Automatic addon backup is enabled but the repository path is empty.");
    }

    let pending_addons: Vec<String> = mods
        .iter()
        .filter(|m| m.needs_update)
        .map(|m| m.name.clone())
        .collect();
    if pending_addons.is_empty() {
        return Ok(());
    }

    let backup_root = {
        let trimmed = backup_root_raw.trim();
        if trimmed.is_empty() {
            crate::core::utils::app_paths::foxy_backups_dir()
        } else {
            PathBuf::from(trimmed)
        }
    };
    let total = pending_addons.len();
    let mut backed_up = 0usize;
    let mut skipped_missing = 0usize;

    for (index, addon_name) in pending_addons.iter().enumerate() {
        let percent = 0.84 + (((index + 1) as f32 / total as f32) * 0.04);
        send_progress_event(
            progress_tx,
            ProgressEvent::Stage {
                label: format!("Backing up addons {}/{}", index + 1, total),
                percent,
            },
            operation_id,
        );

        let addon_path = resolve_child_dir_case_insensitive(Path::new(repo_root), addon_name)
            .unwrap_or_else(|| Path::new(repo_root).join(addon_name));
        if !addon_path.exists() {
            skipped_missing += 1;
            info!(
                "Skipping addon backup for {} because the local directory does not exist: {}",
                addon_name,
                sanitize_log_path(&addon_path)
            );
            continue;
        }
        if !addon_path.is_dir() {
            anyhow::bail!(
                "Cannot back up addon {} because the resolved path is not a directory: {}",
                addon_name,
                addon_path.display()
            );
        }

        let record = addon_backup::backup_addon(&backup_root, &addon_path).with_context(|| {
            format!(
                "failed to create backup for addon {} from {}",
                addon_name,
                addon_path.display()
            )
        })?;
        backed_up += 1;
        info!(
            "Addon backup saved for {} at {}",
            addon_name,
            sanitize_log_path(&record.path)
        );
    }

    info!(
        "Addon backup stage finished for repo={} (backed_up={} skipped_missing={} total={})",
        sanitize_log_path(Path::new(repo_root)),
        backed_up,
        skipped_missing,
        total
    );
    Ok(())
}
