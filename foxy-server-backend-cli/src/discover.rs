use anyhow::{Context, Result};
use std::path::Path;

use crate::types::DiscoveredFile;

/// Manifest this tool writes into a mod folder; skipped on discovery so a
/// folder published in place (or a previous output reused as a source) does
/// not hash its own manifest.
const GENERATED_MOD_MANIFEST: &str = "foxy_addon.json";

/// Recursively discover all files within a mod directory.
/// Preserves traversal order so generated checksums align with legacy-style manifests.
pub fn discover_files(mod_source: &Path) -> Result<Vec<DiscoveredFile>> {
    discover_files_with_pruning(mod_source, false)
}

pub fn discover_files_with_pruning(
    mod_source: &Path,
    prune_unused_optionals: bool,
) -> Result<Vec<DiscoveredFile>> {
    let mut files = Vec::new();
    walk_dir(mod_source, mod_source, &mut files, prune_unused_optionals)?;
    Ok(files)
}

fn walk_dir(
    root: &Path,
    current: &Path,
    files: &mut Vec<DiscoveredFile>,
    prune_unused_optionals: bool,
) -> Result<()> {
    let entries = std::fs::read_dir(current)
        .with_context(|| format!("Failed to read directory: {}", current.display()))?;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("Failed to read metadata: {}", path.display()))?;

        if metadata.is_dir() {
            if prune_unused_optionals
                && current == root
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("optionals")
            {
                continue;
            }
            walk_dir(root, &path, files, prune_unused_optionals)?;
        } else if metadata.is_file() {
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("srf"))
            {
                continue;
            }

            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_str()
                .unwrap_or("")
                .to_string();

            if relative == GENERATED_MOD_MANIFEST {
                continue;
            }

            if !relative.is_empty() {
                files.push(DiscoveredFile {
                    absolute_path: path,
                    relative_path: relative,
                    file_size: metadata.len(),
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discover_files_flat_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.cpp"), b"class CfgMods {};").unwrap();
        fs::write(dir.path().join("data.pbo"), b"pbo data").unwrap();

        let files = discover_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        let names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(names.contains(&"config.cpp"));
        assert!(names.contains(&"data.pbo"));
    }

    #[test]
    fn discover_files_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let addons = dir.path().join("addons");
        fs::create_dir(&addons).unwrap();
        fs::write(addons.join("main.pbo"), b"pbo").unwrap();
        let keys = dir.path().join("keys");
        fs::create_dir(&keys).unwrap();
        fs::write(keys.join("mod.bikey"), b"key").unwrap();

        let files = discover_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);

        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        // Path separators depend on OS
        assert!(paths.iter().any(|p| p.contains("main.pbo")));
        assert!(paths.iter().any(|p| p.contains("mod.bikey")));
    }

    #[test]
    fn pruning_skips_root_optionals_and_keeps_nested_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Optionals")).unwrap();
        std::fs::create_dir_all(dir.path().join("addons").join("optionals")).unwrap();
        std::fs::write(dir.path().join("Optionals").join("unused.pbo"), b"unused").unwrap();
        std::fs::write(
            dir.path()
                .join("addons")
                .join("optionals")
                .join("needed.pbo"),
            b"needed",
        )
        .unwrap();

        let files = discover_files_with_pruning(dir.path(), true).unwrap();
        let paths: Vec<_> = files
            .iter()
            .map(|file| file.relative_path.replace('\\', "/"))
            .collect();
        assert_eq!(paths, vec!["addons/optionals/needed.pbo"]);
    }

    #[test]
    fn discover_files_excludes_srf_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("mod.srf"), b"srf data").unwrap();
        fs::write(dir.path().join("MOD.SRF"), b"uppercase srf").unwrap();
        fs::write(dir.path().join("data.pbo"), b"pbo data").unwrap();

        let files = discover_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "data.pbo");
    }

    #[test]
    fn discover_files_excludes_generated_mod_manifest_at_root_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("foxy_addon.json"), b"{}").unwrap();
        let nested = dir.path().join("addons");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("foxy_addon.json"), b"{}").unwrap();
        fs::write(dir.path().join("data.pbo"), b"pbo data").unwrap();

        let files = discover_files(dir.path()).unwrap();
        let mut names: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
        names.sort();
        assert_eq!(names.len(), 2);
        assert!(names[0].ends_with("foxy_addon.json"));
        assert_eq!(names[1], "data.pbo");
    }

    #[test]
    fn discover_files_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let files = discover_files(dir.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn discover_files_records_file_size() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sized.bin"), b"12345").unwrap();

        let files = discover_files(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_size, 5);
    }

    #[test]
    fn discover_files_records_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("test.txt"), b"x").unwrap();

        let files = discover_files(dir.path()).unwrap();
        assert!(files[0].absolute_path.is_absolute());
    }

    #[test]
    fn discover_files_nonexistent_dir_errors() {
        let result = discover_files(std::path::Path::new("/nonexistent/mod/dir"));
        assert!(result.is_err());
    }
}
