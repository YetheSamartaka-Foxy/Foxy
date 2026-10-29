use std::path::{Path, PathBuf};

use crate::core::utils::content_hash::normalize_path;

#[derive(Clone, Debug)]
pub(crate) struct PinnedDownloadTargets {
    root: PathBuf,
    canonical_root: PathBuf,
    links: Vec<(PathBuf, PathBuf)>,
}

impl PinnedDownloadTargets {
    pub(crate) fn from_links(
        root: PathBuf,
        links: Vec<(PathBuf, PathBuf)>,
    ) -> Result<Self, String> {
        let canonical_root = canonicalize_existing_ancestor(&root)?;
        Ok(Self {
            root,
            canonical_root,
            links,
        })
    }

    pub(crate) fn validate_current(&self) -> Result<(), String> {
        let current =
            crate::core::api::check_contracts::linked_targets(&self.root).map_err(|_| {
                "Linked write targets changed. Review the destinations and retry.".to_owned()
            })?;
        if current.len() != self.links.len()
            || current.iter().zip(&self.links).any(
                |((link, target), (approved_link, approved_target))| {
                    normalize_path(&link.to_string_lossy())
                        != normalize_path(&approved_link.to_string_lossy())
                        || normalize_path(&target.to_string_lossy())
                            != normalize_path(&approved_target.to_string_lossy())
                },
            )
        {
            return Err(
                "Linked write targets changed. Review the destinations and retry.".to_owned(),
            );
        }
        Ok(())
    }

    pub(crate) fn resolve(&self, path: &Path) -> Result<PathBuf, String> {
        let path_text = path.to_string_lossy();
        let normalized_path = normalize_path(&path_text);
        let Some((link, target)) = self
            .links
            .iter()
            .filter_map(|(link, target)| {
                let normalized_link = normalize_path(&link.to_string_lossy());
                let suffix = normalized_path
                    .strip_prefix(&normalized_link)
                    .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))?;
                Some((link, target, suffix))
            })
            .max_by_key(|(link, _, _)| normalize_path(&link.to_string_lossy()).len())
            .map(|(link, target, _)| (link, target))
        else {
            if let Some(target) = self
                .links
                .iter()
                .map(|(_, target)| target)
                .find(|target| path_starts_with(path, target))
            {
                let resolved = canonicalize_existing_ancestor(path)?;
                if !path_starts_with(&resolved, target) {
                    return Err(
                        "Linked write target resolves outside its approved destination".to_owned(),
                    );
                }
                return Ok(resolved);
            }

            let relative = path
                .strip_prefix(&self.root)
                .ok()
                .or_else(|| path.strip_prefix(&self.canonical_root).ok())
                .map(Path::to_path_buf)
                .or_else(|| {
                    normalized_relative_path(path, &self.root)
                        .or_else(|| normalized_relative_path(path, &self.canonical_root))
                })
                .ok_or_else(|| "Download target is outside the repository write root".to_owned())?;
            let pinned = canonicalize_existing_ancestor(&self.canonical_root.join(relative))?;
            if !path_starts_with(&pinned, &self.canonical_root) {
                return Err("Download target resolves outside the repository write root".to_owned());
            }
            let current = canonicalize_existing_ancestor(path)?;
            if normalize_path(&current.to_string_lossy())
                != normalize_path(&pinned.to_string_lossy())
            {
                return Err("Download target changed before it could be pinned".to_owned());
            }
            return Ok(pinned);
        };

        let normalized_link = normalize_path(&link.to_string_lossy());
        let suffix = normalized_path
            .strip_prefix(&normalized_link)
            .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
            .ok_or_else(|| {
                "Linked write target changed. Review the destination and retry.".to_owned()
            })?;
        let relative = suffix.trim_start_matches('/');
        let expected = target.join(relative);
        let expected = canonicalize_existing_ancestor(&expected)?;
        if !path_starts_with(&expected, target) {
            return Err("Linked write target resolves outside its approved destination".to_owned());
        }
        let current = canonicalize_existing_ancestor(path)?;
        if normalize_path(&current.to_string_lossy()) != normalize_path(&expected.to_string_lossy())
        {
            return Err(
                "Linked write target changed. Review the destination and retry.".to_owned(),
            );
        }
        Ok(expected)
    }
}

fn normalized_relative_path(path: &Path, root: &Path) -> Option<PathBuf> {
    let normalized_path = normalize_path(&path.to_string_lossy());
    let normalized_root = normalize_path(&root.to_string_lossy());
    let suffix = normalized_path
        .strip_prefix(&normalized_root)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))?;
    Some(PathBuf::from(suffix.trim_start_matches('/')))
}

fn path_starts_with(path: &Path, root: &Path) -> bool {
    let path = normalize_path(&path.to_string_lossy());
    let root = normalize_path(&root.to_string_lossy());
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn canonicalize_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    let canonical = loop {
        match ancestor.canonicalize() {
            Ok(canonical) => break canonical,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| "Could not resolve a linked write target".to_owned())?;
                missing.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| "Could not resolve a linked write target".to_owned())?;
            }
            Err(_) => return Err("Could not resolve a linked write target".to_owned()),
        }
    };
    Ok(missing
        .into_iter()
        .rev()
        .fold(canonical, |path, part| path.join(part)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_paths_are_pinned_to_the_canonical_root() {
        let root = tempfile::tempdir().unwrap();
        let pinned = PinnedDownloadTargets::from_links(root.path().to_owned(), Vec::new()).unwrap();
        let relative = Path::new("@mod_01").join("addons").join("file.bin");
        assert_eq!(
            pinned.resolve(&root.path().join(&relative)).unwrap(),
            root.path().canonicalize().unwrap().join(relative)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolves_linked_files_to_the_enrolled_target() {
        let root = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(target.path(), root.path().join("addon")).unwrap();
        let link = root.path().join("addon");
        let pinned = PinnedDownloadTargets::from_links(
            root.path().to_path_buf(),
            vec![(link.clone(), target.path().canonicalize().unwrap())],
        )
        .unwrap();
        let source = link.join("nested").join("file.bin");
        assert_eq!(
            pinned.resolve(&source).unwrap(),
            target.path().join("nested/file.bin")
        );
    }

    #[test]
    fn pins_a_first_install_before_the_repository_directory_exists() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("new-install");
        let pinned = PinnedDownloadTargets::from_links(root.clone(), Vec::new()).unwrap();
        assert_eq!(
            pinned.resolve(&root.join("@mod_01/file.bin")).unwrap(),
            parent
                .path()
                .canonicalize()
                .unwrap()
                .join("new-install/@mod_01/file.bin")
        );
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_link_retargeted_after_enrollment() {
        let root = tempfile::tempdir().unwrap();
        let approved = tempfile::tempdir().unwrap();
        let replacement = tempfile::tempdir().unwrap();
        let link = root.path().join("addon");
        std::os::unix::fs::symlink(approved.path(), &link).unwrap();
        let pinned = PinnedDownloadTargets::from_links(
            root.path().to_path_buf(),
            vec![(link.clone(), approved.path().canonicalize().unwrap())],
        )
        .unwrap();
        let pinned_path = pinned.resolve(&link.join("file.bin")).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(replacement.path(), &link).unwrap();
        assert!(pinned.resolve(&link.join("file.bin")).is_err());
        assert!(pinned.validate_current().is_err());
        std::fs::write(&pinned_path, b"approved destination").unwrap();
        assert!(approved.path().join("file.bin").is_file());
        assert!(!replacement.path().join("file.bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_new_link_created_after_an_empty_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let pinned =
            PinnedDownloadTargets::from_links(root.path().to_path_buf(), Vec::new()).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("new-addon")).unwrap();
        assert!(
            pinned
                .resolve(&root.path().join("new-addon/file.bin"))
                .is_err()
        );
    }
}
