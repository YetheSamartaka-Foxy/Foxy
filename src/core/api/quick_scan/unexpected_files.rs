use super::super::fs_watcher::normalize_path_for_match;
use super::super::*;
use crate::core::utils::format::sanitize_log_path;

pub(super) fn collect_unexpected_local_files_for_mod(
    mod_root: &str,
    expected_local_paths: &HashSet<String>,
) -> Vec<String> {
    let mut extras = Vec::new();
    let root = Path::new(mod_root);
    if !root.exists() {
        return extras;
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                warn!(
                    "Failed to read addon directory during extra-file scan {}: {}",
                    sanitize_log_path(&dir),
                    err
                );
                continue;
            }
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let path_string = path.to_string_lossy().to_string();
            let normalized = normalize_path_for_match(&path_string);
            if !expected_local_paths.contains(&normalized) {
                extras.push(path_string);
            }
        }
    }

    extras.sort_unstable();
    extras.dedup();
    extras
}

pub(crate) async fn collect_unexpected_files_for_repo_mods(
    context: Arc<FoxyContext>,
    repo_url: &str,
    mod_name_filter: &HashSet<String>,
    mod_enabled_overrides: Option<&HashMap<String, bool>>,
) -> HashMap<String, Vec<String>> {
    let mut by_mod: HashMap<String, Vec<String>> = HashMap::new();
    if mod_name_filter.is_empty() {
        return by_mod;
    }

    let tree = match Tree::load_for_mod_names(context, repo_url, mod_name_filter).await {
        Ok(tree) => tree,
        Err(err) => {
            warn!(
                "Failed to load scoped repository tree for extra-file scan {}: {}",
                repo_url, err
            );
            return by_mod;
        }
    };

    let mut repo_mod_indices: Vec<usize> = tree
        .repo_nodes
        .iter()
        .flat_map(|repo_node| repo_node.mods.iter().copied())
        .collect();
    repo_mod_indices.sort_unstable();
    repo_mod_indices.dedup();

    for mod_idx in repo_mod_indices {
        let Some(m) = tree.mods.get(mod_idx) else {
            continue;
        };
        let mod_key = if cfg!(windows) {
            m.name.to_lowercase()
        } else {
            m.name.clone()
        };
        if !mod_name_filter.contains(&mod_key) {
            continue;
        }
        let is_enabled = mod_enabled_overrides
            .and_then(|overrides| overrides.get(&mod_key).copied())
            .unwrap_or(m.enabled);
        if !is_enabled {
            continue;
        }
        let mod_root = m.local_path.trim();
        if mod_root.is_empty() || !Path::new(mod_root).exists() {
            continue;
        }
        let Some(mod_node) = tree.mod_nodes.get(mod_idx) else {
            continue;
        };

        let expected_local_paths: HashSet<String> = mod_node
            .files
            .iter()
            .filter_map(|file_idx| tree.files.get(*file_idx))
            .map(|f| normalize_path_for_match(&f.local_path))
            .collect();
        if expected_local_paths.is_empty() {
            continue;
        }

        let mod_root_owned = mod_root.to_string();
        let extras = tokio::task::spawn_blocking(move || {
            collect_unexpected_local_files_for_mod(&mod_root_owned, &expected_local_paths)
        })
        .await
        .unwrap_or_default();
        if !extras.is_empty() {
            by_mod.insert(m.name.clone(), extras);
        }
    }

    by_mod
}

/// Delete unexpected local files from addon directories. Runs the actual
/// filesystem deletions inside `spawn_blocking` to avoid blocking the tokio
/// async runtime with synchronous I/O syscalls.
pub(crate) async fn delete_unexpected_local_files(
    unexpected_by_mod: &HashMap<String, Vec<String>>,
) -> (usize, usize) {
    let work: Vec<(String, Vec<String>)> = unexpected_by_mod
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    tokio::task::spawn_blocking(move || {
        let mut deleted = 0usize;
        let mut failed = 0usize;

        for (mod_name, paths) in &work {
            for path in paths {
                match crate::core::utils::file_io::retry_remove_file_sync(std::path::Path::new(
                    path,
                )) {
                    Ok(()) => {
                        deleted += 1;
                        debug!(
                            "Deleted unexpected local file for addon {}: {}",
                            mod_name, path
                        );
                    }
                    Err(err) => {
                        failed += 1;
                        warn!(
                            "Failed to delete unexpected local file for addon {}: {} ({})",
                            mod_name, path, err
                        );
                    }
                }
            }
        }

        (deleted, failed)
    })
    .await
    .unwrap_or((0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn collect_unexpected_returns_empty_for_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let extras =
            collect_unexpected_local_files_for_mod(&missing.to_string_lossy(), &HashSet::new());
        assert!(extras.is_empty());
    }

    #[test]
    fn collect_unexpected_flags_files_not_in_expected_set() {
        let dir = tempfile::tempdir().unwrap();
        let expected = dir.path().join("expected.pbo");
        let unexpected = dir.path().join("unexpected.pbo");
        fs::write(&expected, b"x").unwrap();
        fs::write(&unexpected, b"y").unwrap();

        let expected_set = HashSet::from([normalize_path_for_match(&expected.to_string_lossy())]);
        let extras =
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &expected_set);

        assert_eq!(extras.len(), 1);
        assert!(extras[0].contains("unexpected.pbo"));
    }

    #[test]
    fn collect_unexpected_empty_when_all_files_are_expected() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.pbo");
        let b = dir.path().join("b.pbo");
        fs::write(&a, b"x").unwrap();
        fs::write(&b, b"y").unwrap();

        let expected_set = HashSet::from([
            normalize_path_for_match(&a.to_string_lossy()),
            normalize_path_for_match(&b.to_string_lossy()),
        ]);
        let extras =
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &expected_set);

        assert!(extras.is_empty());
    }

    #[test]
    fn collect_unexpected_recurses_into_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("addons");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("extra.pbo"), b"z").unwrap();

        let extras =
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &HashSet::new());

        assert_eq!(extras.len(), 1);
        assert!(extras[0].contains("extra.pbo"));
    }

    #[test]
    fn collect_unexpected_results_are_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["c.pbo", "a.pbo", "b.pbo"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }

        let extras =
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &HashSet::new());

        assert_eq!(extras.len(), 3);
        let mut expected_order = extras.clone();
        expected_order.sort();
        assert_eq!(extras, expected_order);
    }

    #[tokio::test]
    async fn delete_unexpected_local_files_removes_listed_paths() {
        let dir = tempfile::tempdir().unwrap();
        let doomed = dir.path().join("remove_me.pbo");
        fs::write(&doomed, b"bye").unwrap();

        let mut by_mod = HashMap::new();
        by_mod.insert(
            "@ace".to_string(),
            vec![doomed.to_string_lossy().to_string()],
        );

        let (deleted, failed) = delete_unexpected_local_files(&by_mod).await;

        assert_eq!(deleted, 1);
        assert_eq!(failed, 0);
        assert!(!doomed.exists());
    }

    #[tokio::test]
    async fn delete_unexpected_local_files_empty_map_is_noop() {
        let (deleted, failed) = delete_unexpected_local_files(&HashMap::new()).await;
        assert_eq!(deleted, 0);
        assert_eq!(failed, 0);
    }
}
