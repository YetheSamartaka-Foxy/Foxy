use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::types::ProcessedMod;

/// Where the collected `.bikey` files are written and which extra sources feed it.
#[derive(Debug, Clone)]
pub struct KeyCollectionOptions<'a> {
    pub dest: &'a Path,
    pub additional_sources: &'a [PathBuf],
}

/// Outcome of a key collection pass, used for the `create` summary.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct KeyCollectionReport {
    pub copied: usize,
    pub duplicates: usize,
    pub conflicts: Vec<String>,
}

/// True for files Arma treats as server keys.
pub fn is_key_file(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bikey"))
}

/// Key files inside the generated repository, in `(mod, file)` discovery order.
pub fn generated_key_paths(output_dir: &Path, mods: &[ProcessedMod]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for m in mods {
        for file in &m.files {
            if is_key_file(&file.relative_path) {
                paths.push(output_dir.join(&m.mod_name).join(&file.relative_path));
            }
        }
    }
    paths
}

/// Key files found by walking a user-supplied directory (or the file itself).
pub fn additional_key_paths(source: &Path) -> Result<Vec<PathBuf>> {
    if source.is_file() {
        return Ok(if is_key_file(&source.to_string_lossy()) {
            vec![source.to_path_buf()]
        } else {
            Vec::new()
        });
    }

    let mut paths = Vec::new();
    let mut stack = vec![source.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .with_context(|| format!("Failed to read additional keys dir: {}", dir.display()))?;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if is_key_file(&path.to_string_lossy()) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

/// Copies every generated and additional key into a single flat `keys/` folder.
///
/// Keys are keyed by file name because that is how Arma loads them: two distinct
/// sources sharing a name would overwrite each other, so a byte-different clash is
/// reported instead of silently taking the last writer.
pub fn collect_keys(
    output_dir: &Path,
    mods: &[ProcessedMod],
    options: &KeyCollectionOptions<'_>,
) -> Result<KeyCollectionReport> {
    let mut sources = generated_key_paths(output_dir, mods);
    for extra in options.additional_sources {
        sources.extend(additional_key_paths(extra)?);
    }

    std::fs::create_dir_all(options.dest)
        .with_context(|| format!("Failed to create keys dir: {}", options.dest.display()))?;

    let mut report = KeyCollectionReport::default();
    let mut taken: BTreeMap<String, PathBuf> = BTreeMap::new();

    for source in sources {
        let Some(name) = source.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let dest = options.dest.join(name);

        if let Some(previous) = taken.get(name) {
            if same_contents(previous, &source)? {
                report.duplicates += 1;
            } else {
                report.conflicts.push(name.to_string());
            }
            continue;
        }

        std::fs::copy(&source, &dest).with_context(|| {
            format!(
                "Failed to copy key {} to {}",
                source.display(),
                dest.display()
            )
        })?;
        taken.insert(name.to_string(), source);
        report.copied += 1;
    }

    Ok(report)
}

fn same_contents(a: &Path, b: &Path) -> Result<bool> {
    let left = std::fs::read(a).with_context(|| format!("Failed to read key: {}", a.display()))?;
    let right = std::fs::read(b).with_context(|| format!("Failed to read key: {}", b.display()))?;
    Ok(left == right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Checksums, ModFile};

    fn mod_with_files(name: &str, files: &[&str]) -> ProcessedMod {
        ProcessedMod {
            mod_name: name.to_string(),
            checksums: Checksums::default(),
            files: files
                .iter()
                .enumerate()
                .map(|(index, path)| ModFile {
                    relative_path: (*path).to_string(),
                    checksums: Checksums::default(),
                    length: 0,
                    parts: Vec::new(),
                    data_order: index,
                })
                .collect(),
            is_required: true,
            enabled: true,
            client_side: false,
        }
    }

    #[test]
    fn key_files_match_bikey_extension_case_insensitively() {
        assert!(is_key_file("keys/mod.bikey"));
        assert!(is_key_file("keys/MOD.BIKEY"));
        assert!(!is_key_file("addons/mod.pbo"));
        assert!(!is_key_file("keys/mod.bisign"));
        assert!(!is_key_file("bikey"));
    }

    #[test]
    fn generated_key_paths_are_scoped_to_each_mod_folder() {
        let mods = [
            mod_with_files("@ace", &["addons/ace.pbo", "keys/ace.bikey"]),
            mod_with_files("@cba", &["keys/cba.bikey"]),
        ];
        let paths = generated_key_paths(Path::new("out"), &mods);
        assert_eq!(
            paths,
            vec![
                Path::new("out").join("@ace").join("keys/ace.bikey"),
                Path::new("out").join("@cba").join("keys/cba.bikey"),
            ]
        );
    }

    #[test]
    fn additional_key_paths_walks_nested_dirs_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(dir.path().join("a3.bikey"), b"a3").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::write(nested.join("gm.bikey"), b"gm").unwrap();

        let paths = additional_key_paths(dir.path()).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("a3.bikey")));
        assert!(paths.iter().any(|p| p.ends_with("gm.bikey")));
    }

    #[test]
    fn additional_key_paths_accepts_a_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("a3.bikey");
        std::fs::write(&key, b"a3").unwrap();
        assert_eq!(additional_key_paths(&key).unwrap(), vec![key.clone()]);

        let other = dir.path().join("readme.md");
        std::fs::write(&other, b"x").unwrap();
        assert!(additional_key_paths(&other).unwrap().is_empty());
    }

    #[test]
    fn collect_keys_flattens_mod_keys_and_additional_keys() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        std::fs::create_dir_all(output.join("@ace").join("keys")).unwrap();
        std::fs::write(output.join("@ace").join("keys").join("ace.bikey"), b"ace").unwrap();
        let extra = dir.path().join("extra");
        std::fs::create_dir(&extra).unwrap();
        std::fs::write(extra.join("a3.bikey"), b"a3").unwrap();

        let mods = [mod_with_files("@ace", &["keys/ace.bikey"])];
        let dest = output.join("keys");
        let report = collect_keys(
            &output,
            &mods,
            &KeyCollectionOptions {
                dest: &dest,
                additional_sources: &[extra],
            },
        )
        .unwrap();

        assert_eq!(report.copied, 2);
        assert!(report.conflicts.is_empty());
        assert!(dest.join("ace.bikey").exists());
        assert!(dest.join("a3.bikey").exists());
    }

    #[test]
    fn identical_keys_are_deduplicated_and_different_ones_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out");
        for name in ["@a", "@b", "@c"] {
            std::fs::create_dir_all(output.join(name).join("keys")).unwrap();
        }
        std::fs::write(output.join("@a").join("keys").join("shared.bikey"), b"same").unwrap();
        std::fs::write(output.join("@b").join("keys").join("shared.bikey"), b"same").unwrap();
        std::fs::write(
            output.join("@c").join("keys").join("shared.bikey"),
            b"different",
        )
        .unwrap();

        let mods = [
            mod_with_files("@a", &["keys/shared.bikey"]),
            mod_with_files("@b", &["keys/shared.bikey"]),
            mod_with_files("@c", &["keys/shared.bikey"]),
        ];
        let dest = output.join("keys");
        let report = collect_keys(
            &output,
            &mods,
            &KeyCollectionOptions {
                dest: &dest,
                additional_sources: &[],
            },
        )
        .unwrap();

        assert_eq!(report.copied, 1);
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.conflicts, vec!["shared.bikey".to_string()]);
        assert_eq!(
            std::fs::read(dest.join("shared.bikey")).unwrap(),
            b"same".to_vec()
        );
    }

    #[test]
    fn collect_keys_creates_an_empty_dest_when_no_keys_exist() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("keys");
        let report = collect_keys(
            dir.path(),
            &[],
            &KeyCollectionOptions {
                dest: &dest,
                additional_sources: &[],
            },
        )
        .unwrap();
        assert_eq!(report, KeyCollectionReport::default());
        assert!(dest.is_dir());
    }
}
