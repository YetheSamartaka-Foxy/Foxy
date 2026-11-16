use super::super::*;
use super::unexpected_files::load_managed_local_paths;
use crate::core::db::params;

pub(crate) fn preserve_removed_file_diffs(
    previous: &[ModDiffSummary],
    current: &mut Vec<ModDiffSummary>,
) {
    for addon in previous {
        let removed: Vec<_> = addon
            .files
            .iter()
            .filter(|file| file.needs_update && file.change_kind == FileDiffKind::Deleted)
            .cloned()
            .collect();
        if removed.is_empty() {
            continue;
        }
        if let Some(target) = current.iter_mut().find(|target| target.name == addon.name) {
            for file in removed {
                if !target
                    .files
                    .iter()
                    .any(|existing| existing.name == file.name)
                {
                    target.files.push(file);
                }
            }
            target.needs_update = true;
        } else {
            current.push(ModDiffSummary {
                name: addon.name.clone(),
                needs_update: true,
                total_bytes: 0,
                files: removed,
            });
        }
    }
}

pub(super) async fn removed_managed_file_diffs(
    context: &FoxyContext,
    repo_url: &str,
    mods: &[FoxyMod],
    enabled_overrides: Option<&HashMap<String, bool>>,
) -> Option<HashMap<i64, Vec<FileDiffSummary>>> {
    let prefix = format!("{}/", repo_url.trim_end_matches('/'));
    // The next byte after '/' bounds an indexed URL-prefix lookup.
    let upper_bound = format!("{}0", repo_url.trim_end_matches('/'));
    let rows = context
        .db()
        .query_all(
            "SELECT f.name, f.remote_path, f.local_path FROM files f \
             WHERE f.remote_path >= ? AND f.remote_path < ? \
             AND NOT EXISTS (SELECT 1 FROM addon_files af WHERE af.file_id = f.id)",
            params![prefix, upper_bound],
        )
        .await
        .ok()?;
    if rows.is_empty() {
        return Some(HashMap::new());
    }

    let mut candidates = Vec::new();
    for row in rows {
        let remote_path = row.get_string("remote_path").ok()?;
        let local_path = row.get_string("local_path").ok()?;
        let normalized = crate::core::utils::content_hash::normalize_path(&local_path);
        let Some(addon) = mods.iter().find(|addon| {
            enabled_overrides
                .and_then(|overrides| overrides.get(&addon.name.to_lowercase()).copied())
                .unwrap_or(addon.enabled)
                && !addon.local_path.trim().is_empty()
                && remote_path.starts_with(&format!("{}/", addon.remote_path.trim_end_matches('/')))
                && normalized.starts_with(&format!(
                    "{}/",
                    crate::core::utils::content_hash::normalize_path(&addon.local_path)
                ))
        }) else {
            continue;
        };
        candidates.push((
            addon.id as i64,
            row.get_string("name").ok()?,
            local_path,
            normalized,
        ));
    }
    if candidates.is_empty() {
        return Some(HashMap::new());
    }

    let managed_paths = load_managed_local_paths(context).await?;
    candidates.retain(|(_, _, _, normalized)| !managed_paths.contains(normalized));
    tokio::task::spawn_blocking(move || {
        let mut removed: HashMap<i64, Vec<FileDiffSummary>> = HashMap::new();
        let mut seen = HashSet::new();
        for (addon_id, name, local_path, normalized) in candidates {
            if !seen.insert(normalized) {
                continue;
            }
            match std::fs::symlink_metadata(&local_path) {
                Ok(metadata) if metadata.is_file() => {
                    removed.entry(addon_id).or_default().push(FileDiffSummary {
                        name,
                        needs_update: true,
                        total_bytes: 0,
                        changed_parts: 0,
                        change_kind: FileDiffKind::Deleted,
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        for files in removed.values_mut() {
            files.sort_by(|left, right| left.name.cmp(&right.name));
        }
        Some(removed)
    })
    .await
    .ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(addon: &str, file: &str, change_kind: FileDiffKind, bytes: u64) -> ModDiffSummary {
        ModDiffSummary {
            name: addon.into(),
            needs_update: true,
            total_bytes: bytes,
            files: vec![FileDiffSummary {
                name: file.into(),
                needs_update: true,
                total_bytes: bytes,
                changed_parts: 0,
                change_kind,
            }],
        }
    }

    #[test]
    fn download_estimates_retain_removals_without_adding_transfer_bytes() {
        let previous = vec![
            summary("@mod_01", "old.pbo", FileDiffKind::Deleted, 0),
            summary("@mod_02", "old.pbo", FileDiffKind::Deleted, 0),
            summary("@mod_03", "stale.bin", FileDiffKind::Modified, 8),
        ];
        let mut current = vec![summary("@mod_01", "new.pbo", FileDiffKind::Modified, 12)];
        preserve_removed_file_diffs(&previous, &mut current);
        assert_eq!(current.len(), 2);
        assert_eq!(current[0].total_bytes, 12);
        assert_eq!(current[0].files.len(), 2);
        assert_eq!(current[1], previous[1]);
        let once = current.clone();
        preserve_removed_file_diffs(&previous, &mut current);
        assert_eq!(current, once);
    }

    #[test]
    fn newly_published_file_supersedes_a_previous_removal() {
        let previous = vec![summary("@mod_01", "file.pbo", FileDiffKind::Deleted, 0)];
        let mut current = vec![summary("@mod_01", "file.pbo", FileDiffKind::Added, 12)];
        let published = current.clone();
        preserve_removed_file_diffs(&previous, &mut current);
        assert_eq!(current, published);
    }
}
