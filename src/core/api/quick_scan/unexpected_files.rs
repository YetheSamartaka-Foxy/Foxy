use super::super::fs_watcher::normalize_path_for_match;
use super::super::*;

fn cleanup_candidate_samples(mod_root: &str, candidates: &[String]) -> Vec<String> {
    let normalized_root = mod_root.replace('\\', "/");
    let root = Path::new(&normalized_root);
    candidates
        .iter()
        .take(8)
        .map(|candidate| {
            let normalized = candidate.replace('\\', "/");
            let path = Path::new(&normalized);
            path.strip_prefix(root)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
        })
        .collect()
}

pub(super) async fn load_managed_local_paths(context: &FoxyContext) -> Option<HashSet<String>> {
    match context
        .db()
        .query_all(
            "SELECT DISTINCT f.local_path FROM files f \
             JOIN addon_files af ON af.file_id = f.id \
             JOIN repository_addons ra ON ra.addon_id = af.addon_id",
            crate::core::db::params![],
        )
        .await
    {
        Ok(rows) => Some(
            rows.iter()
                .filter_map(|row| row.get_string("local_path").ok())
                .map(|path| normalize_path_for_match(&path))
                .collect(),
        ),
        Err(_) => {
            context
                .hash_evidence
                .record_issue(crate::core::tasks::calculate_hashes::HashIssue {
                    file_path: String::new(),
                    kind: crate::core::tasks::calculate_hashes::HashIssueKind::Skipped,
                    detail: Some("Could not verify managed-file ownership before cleanup".into()),
                });
            None
        }
    }
}

pub(super) fn collect_unexpected_local_files_for_mod(
    mod_root: &str,
    expected_local_paths: &HashSet<String>,
) -> std::io::Result<Vec<String>> {
    let mut extras = Vec::new();
    let root = Path::new(mod_root);
    if let Err(error) = std::fs::metadata(root) {
        return if error.kind() == ErrorKind::NotFound {
            Ok(extras)
        } else {
            Err(error)
        };
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
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
            if crate::core::utils::content_hash::is_foxy_temp_artifact_path(&path_string) {
                continue;
            }
            let normalized = normalize_path_for_match(&path_string);
            if !expected_local_paths.contains(&normalized) {
                extras.push(path_string);
            }
        }
    }

    extras.sort_unstable();
    extras.dedup();
    Ok(extras)
}

pub(super) async fn scan_unexpected_local_files_for_mod(
    context: &FoxyContext,
    mod_root: &str,
    expected_local_paths: Arc<HashSet<String>>,
) -> Option<Vec<String>> {
    let root = mod_root.to_string();
    let known_managed_paths = expected_local_paths.len();
    match tokio::task::spawn_blocking(move || {
        collect_unexpected_local_files_for_mod(&root, &expected_local_paths)
    })
    .await
    {
        Ok(Ok(extras)) => {
            if !extras.is_empty() {
                let normalized_root = mod_root.replace('\\', "/");
                let addon_directory = Path::new(&normalized_root).file_name().unwrap_or_default();
                info!(
                    "Cleanup candidate evidence: reason=normalized_path_not_in_known_manifest_set addon_directory={:?} candidates={} known_managed_paths={} relative_samples={:?} op_id={}",
                    addon_directory,
                    extras.len(),
                    known_managed_paths,
                    cleanup_candidate_samples(mod_root, &extras),
                    context.operation_id().unwrap_or("none"),
                );
            }
            Some(extras)
        }
        Ok(Err(_)) | Err(_) => {
            context
                .hash_evidence
                .record_issue(crate::core::tasks::calculate_hashes::HashIssue {
                    file_path: mod_root.to_string(),
                    kind: crate::core::tasks::calculate_hashes::HashIssueKind::Skipped,
                    detail: Some(
                        "Could not read all addon files during cleanup verification".into(),
                    ),
                });
            None
        }
    }
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

    let Some(managed_local_paths) = load_managed_local_paths(&context).await else {
        return by_mod;
    };
    let managed_local_paths = Arc::new(managed_local_paths);
    let tree = match Tree::load_for_mod_names(context.clone(), repo_url, mod_name_filter).await {
        Ok(tree) => tree,
        Err(err) => {
            context
                .hash_evidence
                .record_issue(crate::core::tasks::calculate_hashes::HashIssue {
                    file_path: String::new(),
                    kind: crate::core::tasks::calculate_hashes::HashIssueKind::Skipped,
                    detail: Some("Could not load managed addon files before cleanup".into()),
                });
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
        if tree.mod_nodes.get(mod_idx).is_none() {
            continue;
        }
        if managed_local_paths.is_empty() {
            continue;
        }

        let Some(extras) =
            scan_unexpected_local_files_for_mod(&context, mod_root, managed_local_paths.clone())
                .await
        else {
            return HashMap::new();
        };
        if !extras.is_empty() {
            by_mod.insert(m.name.clone(), extras);
        }
    }

    by_mod
}

/// Delete unexpected local files from addon directories. Runs the actual
/// filesystem deletions inside `spawn_blocking` to avoid blocking the tokio
/// async runtime with synchronous I/O syscalls.
#[cfg(test)]
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
    fn cleanup_diagnostics_bound_samples_and_preserve_relative_names() {
        let root = "R:/Mods/RepoAlpha/@mod_01";
        let candidates = (0..12)
            .map(|index| format!("{root}/addons/file_{index:02}.pbo"))
            .collect::<Vec<_>>();
        let samples = cleanup_candidate_samples(root, &candidates);
        assert_eq!(samples.len(), 8);
        assert_eq!(samples[0], "addons/file_00.pbo");
        assert!(samples.iter().all(|path| !path.contains("R:")));
        assert!(cleanup_candidate_samples(root, &[]).is_empty());
    }

    #[test]
    fn cleanup_diagnostics_do_not_expose_an_unmatched_root() {
        let samples = cleanup_candidate_samples(
            "R:/Mods/RepoAlpha/@mod_01",
            &[
                "R:\\Mods\\RepoAlpha\\@mod_01\\addons\\file.pbo".to_string(),
                "R:/Mods/RepoAlphabet/@mod_01/other.pbo".to_string(),
            ],
        );
        assert_eq!(samples, ["addons/file.pbo", "other.pbo"]);
    }

    #[test]
    fn collect_unexpected_returns_empty_for_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let extras =
            collect_unexpected_local_files_for_mod(&missing.to_string_lossy(), &HashSet::new())
                .unwrap();
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
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &expected_set)
                .unwrap();

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
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &expected_set)
                .unwrap();

        assert!(extras.is_empty());
    }

    #[test]
    fn collect_unexpected_recurses_into_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("addons");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("extra.pbo"), b"z").unwrap();

        let extras =
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &HashSet::new())
                .unwrap();

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
            collect_unexpected_local_files_for_mod(&dir.path().to_string_lossy(), &HashSet::new())
                .unwrap();

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

    #[tokio::test]
    async fn cleanup_traversal_failure_leaves_evidence_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("not-a-directory.bin");
        fs::write(&file, b"data").unwrap();
        let db = crate::core::tasks::db_turso::build_test_database().await;
        let context = FoxyContext::new(db, reqwest::Client::new());

        assert!(
            scan_unexpected_local_files_for_mod(
                &context,
                &file.to_string_lossy(),
                Arc::new(HashSet::new()),
            )
            .await
            .is_none()
        );
        assert!(context.hash_evidence.is_incomplete());
    }
}
