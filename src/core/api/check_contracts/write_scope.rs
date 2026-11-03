use std::path::{Path, PathBuf};

use crate::core::utils::content_hash::normalize_path;

pub(crate) fn selected_mod_write_roots(
    root: &str,
    shared_root: Option<&str>,
    selected: &[(String, bool)],
) -> Result<Vec<PathBuf>, String> {
    selected
        .iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(name, _)| {
            if !crate::core::utils::fs_safety::is_safe_child_path(name) {
                return Err("Could not inspect update ownership".to_owned());
            }
            Ok(PathBuf::from(
                crate::core::tasks::remote_mods::resolve_mod_local_path(root, shared_root, name),
            ))
        })
        .collect()
}

pub(crate) fn linked_targets_for_write_roots(
    owned_roots: &[PathBuf],
    write_roots: Option<&[PathBuf]>,
) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let Some(write_roots) = write_roots else {
        return super::linked_targets_for_roots(owned_roots);
    };
    let mut links = Vec::new();
    for write_root in write_roots {
        let (owner, relative) = owned_roots
            .iter()
            .filter_map(|root| relative_to(write_root, root).map(|relative| (root, relative)))
            .max_by_key(|(root, _)| root.as_os_str().len())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Selected addon is outside the repository write roots",
                )
            })?;
        let mut path = owner.clone();
        let mut components = relative.components();
        loop {
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(error),
            };
            if metadata.file_type().is_symlink() {
                links.push((path.clone(), path.canonicalize()?));
                if let Some(component) = components.next() {
                    path.push(component);
                    continue;
                }
                break;
            }
            if let Some(component) = components.next() {
                path.push(component);
            } else {
                links.extend(super::linked_targets(&path)?);
                break;
            }
        }
    }
    links.sort();
    links.dedup();
    Ok(links)
}

pub(super) fn link_overlaps_write_roots(link: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|root| relative_to(link, root).is_some() || relative_to(root, link).is_some())
}

fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return None;
    }
    path.strip_prefix(root)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            let path = normalize_path(&path.to_string_lossy());
            let root = normalize_path(&root.to_string_lossy());
            path.strip_prefix(&root)
                .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
                .map(|suffix| PathBuf::from(suffix.trim_start_matches('/')))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn selected_manifest_roots_preserve_repository_instance_and_selection() {
        use crate::core::db::{FoxyDb, params};
        use crate::core::models::context::FoxyContext;
        use std::collections::HashMap;
        use std::sync::Arc;

        let db = crate::core::tasks::db_turso::build_test_database().await;
        let seam = FoxyDb::from_turso(db.clone());
        let url = "https://repo.example.invalid/MainRepo/";
        for (id, address, path) in [
            (1i64, url, "R:/Mods/RepoAlpha"),
            (2, url, "R:/Mods/RepoBeta"),
            (
                3,
                "https://repo.example.invalid/OtherRepo/",
                "R:/Mods/RepoAlpha",
            ),
        ] {
            seam.execute("INSERT INTO repositories (id, name, remote_url, local_path) VALUES (?, 'RepoAlpha', ?, ?)", params![id, address, path]).await.unwrap();
        }
        for (id, repo, name, path, enabled) in [
            (11i64, 1i64, "@mod_01", "R:/Mods/RepoAlpha/@mod_01", true),
            (12, 1, "@mod_02", "R:/Mods/RepoAlpha/@mod_02", false),
            (21, 2, "@mod_01", "R:/Mods/RepoBeta/@mod_01", true),
            (31, 3, "@mod_99", "R:/Mods/RepoAlpha/@mod_99", true),
        ] {
            seam.execute("INSERT INTO addons (id, name, remote_path, local_path, enabled, required) VALUES (?, ?, ?, ?, ?, 1)", params![id, name, format!("{url}{name}"), path, enabled]).await.unwrap();
            seam.execute(
                "INSERT INTO repository_addons (repository_id, addon_id) VALUES (?, ?)",
                params![repo, id],
            )
            .await
            .unwrap();
        }
        let context = Arc::new(
            FoxyContext::new(db, reqwest::Client::new())
                .with_target_local_path("R:/Mods/RepoAlpha"),
        );
        assert_eq!(
            super::super::selected_addon_write_roots(context.clone(), url, &HashMap::new())
                .await
                .unwrap(),
            vec![PathBuf::from("R:/Mods/RepoAlpha/@mod_01")],
        );
        let overrides = HashMap::from([("@mod_01".into(), false), ("@mod_02".into(), true)]);
        assert_eq!(
            super::super::selected_addon_write_roots(context.clone(), url, &overrides)
                .await
                .unwrap(),
            vec![PathBuf::from("R:/Mods/RepoAlpha/@mod_02")],
        );
        seam.execute("DROP TABLE repository_addons", params![])
            .await
            .unwrap();
        assert!(
            super::super::selected_addon_write_roots(context, url, &overrides)
                .await
                .is_err()
        );
    }

    #[test]
    fn accepted_empty_addons_are_excluded_only_for_the_captured_instance() {
        use std::collections::{BTreeMap, HashSet};
        let space = tempfile::tempdir().unwrap();
        let url = "https://repo.example.invalid/MainRepo/";
        let root = "R:/Mods/RepoAlpha";
        let mut records = BTreeMap::from([(
            super::super::empty_publication_key(url, root),
            super::super::EmptyPublication {
                repository_empty: false,
                addons: HashSet::from(["@mod_01".into()]),
            },
        )]);
        let record_path = space.path().join("empty_publications.json");
        std::fs::write(&record_path, serde_json::to_vec(&records).unwrap()).unwrap();
        let initial = vec![("@mod_01".into(), true), ("@mod_02".into(), true)];
        let mut selected = initial.clone();
        super::super::exclude_accepted_empty_addons(space.path(), url, root, &mut selected)
            .unwrap();
        assert_eq!(
            selected,
            vec![("@mod_01".into(), false), ("@mod_02".into(), true)]
        );
        let mut other = initial.clone();
        super::super::exclude_accepted_empty_addons(
            space.path(),
            url,
            "R:/Mods/RepoBeta",
            &mut other,
        )
        .unwrap();
        assert_eq!(other, initial);
        records.values_mut().next().unwrap().repository_empty = true;
        std::fs::write(&record_path, serde_json::to_vec(&records).unwrap()).unwrap();
        super::super::exclude_accepted_empty_addons(space.path(), url, root, &mut selected)
            .unwrap();
        assert!(selected.iter().all(|(_, enabled)| !enabled));
        std::fs::write(&record_path, b"invalid JSON").unwrap();
        assert!(
            super::super::exclude_accepted_empty_addons(space.path(), url, root, &mut selected)
                .is_err()
        );
    }

    #[test]
    fn selected_roots_follow_shared_addon_resolution_and_ignore_disabled_addons() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("primary");
        let shared = temp.path().join("shared");
        std::fs::create_dir_all(shared.join("@mod_01")).unwrap();
        let roots = selected_mod_write_roots(
            &primary.to_string_lossy(),
            Some(&shared.to_string_lossy()),
            &[
                ("@mod_01".into(), true),
                ("@mod_02".into(), true),
                ("@mod_03".into(), false),
            ],
        )
        .unwrap();
        assert_eq!(roots, vec![shared.join("@mod_01"), primary.join("@mod_02")]);
    }

    #[test]
    fn selected_roots_reject_traversal() {
        assert!(
            selected_mod_write_roots("R:/Mods/RepoAlpha", None, &[("../outside".into(), true)])
                .is_err()
        );
        assert!(
            selected_mod_write_roots("R:/Mods/RepoAlpha", None, &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn scoped_links_reject_an_outside_addon_even_with_a_matching_text_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let outside = temp.path().join("repo-extra/@mod_01");
        let error = linked_targets_for_write_roots(&[root], Some(&[outside])).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn scoped_links_ignore_unselected_and_broken_workshop_links() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("@mod_01");
        std::fs::create_dir(&selected).unwrap();
        std::os::unix::fs::symlink(root.path().join("absent"), root.path().join("@mod_99"))
            .unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert!(linked_targets_for_write_roots(&roots, None).is_err());
        assert!(
            linked_targets_for_write_roots(&roots, Some(&[selected]))
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn scoped_links_include_ancestor_and_selected_descendant_links() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let link = root.path().join("group");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(
            linked_targets_for_write_roots(&roots, Some(&[link.join("@mod_01")])).unwrap(),
            vec![(link.clone(), target.path().canonicalize().unwrap())],
        );
        let selected = root.path().join("@mod_02");
        std::fs::create_dir(&selected).unwrap();
        let nested = selected.join("addons");
        std::os::unix::fs::symlink(target.path(), &nested).unwrap();
        assert_eq!(
            linked_targets_for_write_roots(&roots, Some(&[selected])).unwrap(),
            vec![(nested, target.path().canonicalize().unwrap())],
        );
    }

    #[cfg(unix)]
    #[test]
    fn scoped_links_pin_each_link_along_the_selected_root_path() {
        let root = tempfile::tempdir().unwrap();
        let outer_target = tempfile::tempdir().unwrap();
        let addon_target = tempfile::tempdir().unwrap();
        let owner = root.path().join("repo");
        std::os::unix::fs::symlink(outer_target.path(), &owner).unwrap();
        std::os::unix::fs::symlink(addon_target.path(), outer_target.path().join("@mod_01"))
            .unwrap();
        let selected = owner.join("@mod_01");
        assert_eq!(
            linked_targets_for_write_roots(&[owner.clone()], Some(&[selected.clone()])).unwrap(),
            vec![
                (owner, outer_target.path().canonicalize().unwrap()),
                (selected, addon_target.path().canonicalize().unwrap())
            ],
        );
    }
}
