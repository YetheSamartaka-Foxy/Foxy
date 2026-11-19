use super::super::*;
use super::unexpected_files::{load_managed_local_paths, load_managed_local_paths_scoped};
use crate::core::db::params;

enum ManagedPathExclusion {
    AllRepositories,
    Repository(i64),
}

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

async fn removed_managed_file_candidates(
    context: &FoxyContext,
    repo_url: &str,
    mods: &[FoxyMod],
    enabled_overrides: Option<&HashMap<String, bool>>,
    exclusion: ManagedPathExclusion,
) -> Option<Vec<(i64, String, String, String, String)>> {
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
        return Some(Vec::new());
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
            addon.name.to_lowercase(),
            row.get_string("name").ok()?,
            local_path,
            normalized,
        ));
    }
    if candidates.is_empty() {
        return Some(Vec::new());
    }

    let managed_paths = match exclusion {
        ManagedPathExclusion::AllRepositories => load_managed_local_paths(context).await?,
        ManagedPathExclusion::Repository(repository_id) => {
            load_managed_local_paths_scoped(context, Some(repository_id)).await?
        }
    };
    candidates.retain(|(_, _, _, _, normalized)| !managed_paths.contains(normalized));
    Some(candidates)
}

pub(crate) async fn removed_managed_file_mod_names(
    context: &FoxyContext,
    repo_url: &str,
    enabled_overrides: Option<&HashMap<String, bool>>,
) -> HashSet<String> {
    let Ok(repo) = crate::core::models::repository::load_repository_by_remote_url(
        Arc::new(context.clone()),
        repo_url,
    )
    .await
    else {
        return HashSet::new();
    };
    let rows = match context
        .db()
        .query_all(
            &format!(
                "SELECT {} FROM addons a \
                 JOIN repository_addons ra ON ra.addon_id = a.id \
                 WHERE ra.repository_id = ?",
                crate::core::models::modification::ADDON_COLUMNS
            ),
            params![repo.id as i64],
        )
        .await
    {
        Ok(rows) => rows,
        Err(_) => return HashSet::new(),
    };
    let mods: Vec<FoxyMod> = rows
        .iter()
        .filter_map(|row| FoxyMod::from_row(row).ok())
        .collect();
    let Some(candidates) = removed_managed_file_candidates(
        context,
        repo_url,
        &mods,
        enabled_overrides,
        ManagedPathExclusion::Repository(repo.id as i64),
    )
    .await
    else {
        return HashSet::new();
    };
    if candidates.is_empty() {
        return HashSet::new();
    }
    tokio::task::spawn_blocking(move || {
        let mut names = HashSet::new();
        for (_, addon_name, _, local_path, _) in candidates {
            if matches!(std::fs::symlink_metadata(&local_path), Ok(meta) if meta.is_file()) {
                names.insert(addon_name);
            }
        }
        names
    })
    .await
    .unwrap_or_default()
}

pub(super) async fn removed_managed_file_diffs(
    context: &FoxyContext,
    repo_url: &str,
    mods: &[FoxyMod],
    enabled_overrides: Option<&HashMap<String, bool>>,
) -> Option<HashMap<i64, Vec<FileDiffSummary>>> {
    let candidates = removed_managed_file_candidates(
        context,
        repo_url,
        mods,
        enabled_overrides,
        ManagedPathExclusion::AllRepositories,
    )
    .await?;
    if candidates.is_empty() {
        return Some(HashMap::new());
    }
    tokio::task::spawn_blocking(move || {
        let mut removed: HashMap<i64, Vec<FileDiffSummary>> = HashMap::new();
        let mut seen = HashSet::new();
        for (addon_id, _, name, local_path, normalized) in candidates {
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

    #[tokio::test]
    async fn removed_managed_file_mod_names_skips_self_owned_paths_but_keeps_sibling_owned_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Shared");
        let self_dir = root.join("@mod_01");
        let sibling_dir = root.join("@mod_02");
        std::fs::create_dir_all(&self_dir).unwrap();
        std::fs::create_dir_all(&sibling_dir).unwrap();
        let self_kept = self_dir.join("kept.pbo");
        let sibling_owned = sibling_dir.join("shared.pbo");
        std::fs::write(&self_kept, b"self").unwrap();
        std::fs::write(&sibling_owned, b"shared").unwrap();

        let db = crate::core::tasks::db_turso::build_test_database().await;
        let fdb = crate::core::db::FoxyDb::from_turso(db.clone());
        let main_url = "http://repo.example.invalid:8080/";
        let sibling_url = "http://sibling.example.invalid:8080/";
        let root_str = root.to_string_lossy().replace('\\', "/");
        let self_dir_str = self_dir.to_string_lossy().replace('\\', "/");
        let sibling_dir_str = sibling_dir.to_string_lossy().replace('\\', "/");

        fdb.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES (1, 'MainRepo', ?, ?), (2, 'RepoAlpha', ?, ?)",
            params![main_url, root_str.clone(), sibling_url, root_str],
        )
        .await
        .unwrap();
        fdb.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, enabled, required, data_order) VALUES \
             (1, '@mod_01', ?, ?, 1, 1, 0), \
             (2, '@mod_02', ?, ?, 1, 1, 1), \
             (3, '@mod_02', ?, ?, 1, 1, 0)",
            params![
                format!("{main_url}@mod_01"),
                self_dir_str,
                format!("{main_url}@mod_02"),
                sibling_dir_str.clone(),
                format!("{sibling_url}@mod_02"),
                sibling_dir_str,
            ],
        )
        .await
        .unwrap();
        fdb.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 1), (1, 2), (2, 3)",
            params![],
        )
        .await
        .unwrap();
        fdb.execute(
            "INSERT INTO files (id, name, remote_path, local_path, length) VALUES \
             (10, 'kept.pbo', ?, ?, 4), \
             (11, 'old-alias.pbo', ?, ?, 4), \
             (20, 'shared.pbo', ?, ?, 6), \
             (21, 'shared.pbo', ?, ?, 6)",
            params![
                format!("{main_url}@mod_01/kept.pbo"),
                self_kept.to_string_lossy().to_string(),
                format!("{main_url}@mod_01/old-alias.pbo"),
                self_kept.to_string_lossy().to_string(),
                format!("{main_url}@mod_02/shared.pbo"),
                sibling_owned.to_string_lossy().to_string(),
                format!("{sibling_url}@mod_02/shared.pbo"),
                sibling_owned.to_string_lossy().to_string(),
            ],
        )
        .await
        .unwrap();
        fdb.execute(
            "INSERT INTO addon_files (addon_id, file_id) VALUES (1, 10), (3, 21)",
            params![],
        )
        .await
        .unwrap();

        let context = FoxyContext::new(db, reqwest::Client::new())
            .with_target_local_path(root.to_string_lossy().to_string());
        let names = removed_managed_file_mod_names(&context, main_url, None).await;
        assert!(!names.contains("@mod_01"));
        assert!(names.contains("@mod_02"));
    }
}
