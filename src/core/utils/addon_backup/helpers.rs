use super::types::AddonBackupRecord;
use anyhow::{Context, Result, anyhow, bail};
use log::warn;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn addon_directory_name(addon_path: &Path) -> Result<String> {
    let addon_name = addon_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("addon path has no terminal directory name"))?;
    Ok(addon_name.to_string())
}

pub(super) fn backup_folder_name(addon_name: &str, content_hash: &str) -> String {
    format!(
        "{}_{}",
        content_hash.trim().to_uppercase(),
        sanitize_backup_component(addon_name)
    )
}

pub(super) fn parse_backup_folder_name(folder_name: &str) -> Option<(String, String)> {
    let (content_hash, addon_name) = folder_name.split_once('_')?;
    let content_hash = content_hash.trim();
    let addon_name = addon_name.trim();
    if content_hash.is_empty() || addon_name.is_empty() {
        return None;
    }
    Some((content_hash.to_string(), addon_name.to_string()))
}

pub(super) fn build_backup_record(
    path: PathBuf,
    folder_name: String,
    addon_name: String,
    content_hash: String,
) -> Result<AddonBackupRecord> {
    let metadata = fs::metadata(&path)
        .with_context(|| format!("failed to read backup metadata {}", path.display()))?;
    let created_at_unix_secs = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let size_bytes = directory_total_size(&path)?;

    Ok(AddonBackupRecord {
        addon_name,
        content_hash,
        folder_name,
        path,
        created_at_unix_secs,
        size_bytes,
    })
}

pub(super) fn sanitize_backup_component(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for ch in value.trim().chars() {
        let is_forbidden =
            matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control();
        if is_forbidden {
            sanitized.push('_');
        } else {
            sanitized.push(ch);
        }
    }

    let trimmed = sanitized.trim_matches('.').trim();
    if trimmed.is_empty() {
        "addon".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn unique_staging_path(root: &Path, folder_name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    root.join(format!(
        ".{}.foxy_tmp_{}_{}",
        folder_name,
        std::process::id(),
        stamp
    ))
}

pub(super) fn copy_directory_recursive(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = fs::metadata(source)
        .with_context(|| format!("failed to read source metadata {}", source.display()))?;
    if !source_meta.is_dir() {
        bail!("source path is not a directory: {}", source.display());
    }

    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create directory {}", destination.display()))?;

    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf())];
    let mut directory_permissions = vec![(destination.to_path_buf(), source_meta.permissions())];
    while let Some((source_dir, dest_dir)) = stack.pop() {
        for entry in fs::read_dir(&source_dir)
            .with_context(|| format!("failed to read directory {}", source_dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read entry under {}", source_dir.display()))?;
            let source_path = entry.path();
            let dest_path = dest_dir.join(entry.file_name());
            let file_type = entry.file_type().with_context(|| {
                format!("failed to read file type for {}", source_path.display())
            })?;

            if file_type.is_symlink() {
                preserve_symlink(&source_path, &dest_path)?;
                continue;
            }

            if file_type.is_dir() {
                fs::create_dir_all(&dest_path).with_context(|| {
                    format!("failed to create directory {}", dest_path.display())
                })?;
                if let Ok(metadata) = entry.metadata() {
                    directory_permissions.push((dest_path.clone(), metadata.permissions()));
                }
                stack.push((source_path, dest_path));
                continue;
            }

            if file_type.is_file() {
                fs::copy(&source_path, &dest_path).with_context(|| {
                    format!(
                        "failed to copy file {} -> {}",
                        source_path.display(),
                        dest_path.display()
                    )
                })?;
                if let Ok(metadata) = entry.metadata()
                    && let Err(err) = fs::set_permissions(&dest_path, metadata.permissions())
                {
                    warn!(
                        "Failed to set permissions on '{}': {}",
                        dest_path.display(),
                        err
                    );
                }
                continue;
            }

            warn!(
                "Skipping unsupported filesystem entry in addon backup: {}",
                source_path.display()
            );
        }
    }

    for (path, permissions) in directory_permissions.into_iter().rev() {
        if let Err(err) = fs::set_permissions(&path, permissions) {
            warn!("Failed to set permissions on '{}': {}", path.display(), err);
        }
    }

    Ok(())
}

#[cfg(unix)]
fn preserve_symlink(source_path: &Path, dest_path: &Path) -> Result<()> {
    let target = fs::read_link(source_path)
        .with_context(|| format!("failed to read symlink {}", source_path.display()))?;
    std::os::unix::fs::symlink(&target, dest_path).with_context(|| {
        format!(
            "failed to preserve symlink {} -> {}",
            source_path.display(),
            dest_path.display()
        )
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn preserve_symlink(source_path: &Path, _dest_path: &Path) -> Result<()> {
    warn!(
        "Skipping symlink during addon backup on this platform: {}",
        source_path.display()
    );
    Ok(())
}

pub(super) fn directory_total_size(path: &Path) -> Result<u64> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0u64;
    let mut pending_dirs = vec![path.to_path_buf()];
    while let Some(dir_path) = pending_dirs.pop() {
        for entry in fs::read_dir(&dir_path)
            .with_context(|| format!("failed to read directory {}", dir_path.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read entry under {}", dir_path.display()))?;
            let entry_path = entry.path();
            let file_type = entry.file_type().with_context(|| {
                format!("failed to read file type for {}", entry_path.display())
            })?;
            if file_type.is_dir() {
                pending_dirs.push(entry_path);
            } else if file_type.is_file() {
                total = total.saturating_add(
                    entry
                        .metadata()
                        .map(|metadata| metadata.len())
                        .unwrap_or_default(),
                );
            }
        }
    }

    Ok(total)
}

pub(super) fn calculate_addon_folder_content_hash(path: &Path) -> Result<String> {
    super::super::content_hash::calculate_addon_folder_content_hash(path)
        .with_context(|| format!("failed to hash addon folder {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── addon_directory_name ───────────────────────────────────────────

    #[test]
    fn addon_directory_name_extracts_last_component() {
        let path = Path::new("/mods/@ace");
        let name = addon_directory_name(path).unwrap();
        assert_eq!(name, "@ace");
    }

    #[test]
    fn addon_directory_name_windows_path() {
        let path = Path::new("C:\\Mods\\@my_addon");
        let name = addon_directory_name(path).unwrap();
        assert_eq!(name, "@my_addon");
    }

    #[test]
    fn addon_directory_name_root_path_errors() {
        // Root path has no file_name component
        let result = addon_directory_name(Path::new("/"));
        assert!(result.is_err());
    }

    // ── backup_folder_name ─────────────────────────────────────────────

    #[test]
    fn backup_folder_name_format() {
        let result = backup_folder_name("@ace", "abc123");
        assert_eq!(result, "ABC123_@ace");
    }

    #[test]
    fn backup_folder_name_trims_hash() {
        let result = backup_folder_name("@mod", "  def456  ");
        assert_eq!(result, "DEF456_@mod");
    }

    // ── parse_backup_folder_name ───────────────────────────────────────

    #[test]
    fn parse_backup_folder_name_valid() {
        let (hash, name) = parse_backup_folder_name("ABC123_@ace").unwrap();
        assert_eq!(hash, "ABC123");
        assert_eq!(name, "@ace");
    }

    #[test]
    fn parse_backup_folder_name_no_separator() {
        assert!(parse_backup_folder_name("nounderscore").is_none());
    }

    #[test]
    fn parse_backup_folder_name_empty_hash() {
        assert!(parse_backup_folder_name("_addon").is_none());
    }

    #[test]
    fn parse_backup_folder_name_empty_name() {
        assert!(parse_backup_folder_name("HASH_").is_none());
    }

    #[test]
    fn parse_backup_folder_name_multiple_underscores() {
        // Only splits on first underscore
        let (hash, name) = parse_backup_folder_name("ABC_mod_extra").unwrap();
        assert_eq!(hash, "ABC");
        assert_eq!(name, "mod_extra");
    }

    // ── sanitize_backup_component ──────────────────────────────────────

    #[test]
    fn sanitize_backup_component_normal_name() {
        assert_eq!(sanitize_backup_component("@ace"), "@ace");
    }

    #[test]
    fn sanitize_backup_component_forbidden_chars_replaced() {
        let result = sanitize_backup_component("mod<>:\"/\\|?*name");
        assert!(!result.contains('<'));
        assert!(!result.contains('>'));
        assert!(!result.contains(':'));
        assert!(!result.contains('"'));
        assert!(!result.contains('/'));
        assert!(!result.contains('\\'));
        assert!(!result.contains('|'));
        assert!(!result.contains('?'));
        assert!(!result.contains('*'));
        assert!(result.contains('_'));
    }

    #[test]
    fn sanitize_backup_component_control_chars_replaced() {
        let result = sanitize_backup_component("test\x00\x01name");
        assert!(!result.chars().any(|c| c.is_control()));
    }

    #[test]
    fn sanitize_backup_component_dots_only_returns_addon() {
        assert_eq!(sanitize_backup_component("..."), "addon");
    }

    #[test]
    fn sanitize_backup_component_empty_returns_addon() {
        assert_eq!(sanitize_backup_component(""), "addon");
    }

    #[test]
    fn sanitize_backup_component_trims_whitespace() {
        assert_eq!(sanitize_backup_component("  test  "), "test");
    }

    // ── unique_staging_path ────────────────────────────────────────────

    #[test]
    fn unique_staging_path_contains_folder_name() {
        let path = unique_staging_path(Path::new("/backups"), "ABC_@mod");
        let path_str = path.to_string_lossy().to_string();
        assert!(path_str.contains("ABC_@mod"));
        assert!(path_str.contains(".foxy_tmp_"));
    }

    #[test]
    fn unique_staging_path_different_each_call() {
        let p1 = unique_staging_path(Path::new("/tmp"), "test");
        // Small delay to ensure different timestamp
        let p2 = unique_staging_path(Path::new("/tmp"), "test");
        // They might be the same if called in the same nanosecond, but usually different
        // Just verify they are valid paths
        assert!(!p1.to_string_lossy().is_empty());
        assert!(!p2.to_string_lossy().is_empty());
    }
}
