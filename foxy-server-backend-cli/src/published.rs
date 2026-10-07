use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::types::ResolvedMod;

pub const SERVER_MOD_LINE_FILE: &str = "server_mod_line.txt";

pub fn write_server_mod_line(output_dir: &Path, line: &str) -> Result<()> {
    let path = output_dir.join(SERVER_MOD_LINE_FILE);
    std::fs::write(&path, format!("{line}\n"))
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Root-level `optionals` folders already published in `mod_dir`, without touching them.
pub fn optionals_in(mod_dir: &Path) -> Result<Vec<PathBuf>> {
    let metadata = match std::fs::symlink_metadata(mod_dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).context("Failed to inspect published mod folder"),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Cannot prune optionals from non-directory {}",
            mod_dir.display()
        );
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(mod_dir)
        .with_context(|| format!("Failed to read {}", mod_dir.display()))?
    {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case("optionals")
        {
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("Refusing to prune symlink {}", path.display());
        }
        if metadata.is_dir() {
            found.push(path);
        }
    }
    Ok(found)
}

/// Every `optionals` folder `remove_published_optionals` would delete under `output_dir`.
pub fn published_optionals(output_dir: &Path, mods: &[ResolvedMod]) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for item in mods {
        let mod_dir = output_dir.join(&item.mod_name);
        let optionals = optionals_in(&mod_dir)?;
        if optionals.is_empty() {
            continue;
        }
        let published_id = std::fs::canonicalize(&mod_dir)
            .with_context(|| format!("Failed to resolve {}", mod_dir.display()))?;
        for source in mods {
            let source_id = std::fs::canonicalize(&source.source_path)
                .with_context(|| format!("Failed to resolve {}", source.source_path.display()))?;
            if published_id.starts_with(&source_id) || source_id.starts_with(&published_id) {
                bail!(
                    "Cannot prune {} because it overlaps source {}",
                    mod_dir.display(),
                    source.source_path.display()
                );
            }
        }
        found.extend(optionals);
    }
    Ok(found)
}

/// Pruning only needs `--yes` when it would delete something already published.
pub fn confirm_prune(found: &[PathBuf], yes: bool) -> Result<()> {
    if yes || found.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = found
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect();
    bail!(
        "--prune-unused-optionals would remove {} previously published optionals folder(s):\n{}\nRe-run with --yes to remove them, or with --dry-run to preview",
        found.len(),
        list.join("\n")
    );
}

pub fn remove_published_optionals(output_dir: &Path, mods: &[ResolvedMod]) -> Result<()> {
    for path in published_optionals(output_dir, mods)? {
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_mod_line_file_contains_one_complete_line() {
        let dir = tempfile::tempdir().unwrap();
        write_server_mod_line(dir.path(), "-mod=mods/@ace;mods/@cba;").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(SERVER_MOD_LINE_FILE)).unwrap(),
            "-mod=mods/@ace;mods/@cba;\n"
        );
    }

    #[test]
    fn pruning_removes_only_published_optionals() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source").join("@ace");
        let published = dir.path().join("output").join("@ace");
        std::fs::create_dir_all(source.join("optionals")).unwrap();
        std::fs::create_dir_all(published.join("optionals")).unwrap();
        std::fs::write(source.join("optionals").join("keep.pbo"), b"source").unwrap();
        std::fs::write(published.join("optionals").join("remove.pbo"), b"output").unwrap();
        std::fs::write(published.join("main.pbo"), b"main").unwrap();

        remove_published_optionals(
            &dir.path().join("output"),
            &[ResolvedMod {
                mod_name: "@ace".to_string(),
                source_path: PathBuf::from(&source),
                is_required: true,
                enabled: true,
                client_side: false,
            }],
        )
        .unwrap();

        assert!(!published.join("optionals").exists());
        assert!(published.join("main.pbo").exists());
        assert!(source.join("optionals").join("keep.pbo").exists());
    }

    fn resolved(name: &str, source: &Path) -> ResolvedMod {
        ResolvedMod {
            mod_name: name.to_string(),
            source_path: source.to_path_buf(),
            is_required: true,
            enabled: true,
            client_side: false,
        }
    }

    #[test]
    fn fresh_output_needs_no_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source").join("@ace");
        std::fs::create_dir_all(source.join("optionals")).unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir_all(output.join("@ace")).unwrap();

        let found = published_optionals(&output, &[resolved("@ace", &source)]).unwrap();

        assert!(found.is_empty());
        assert!(confirm_prune(&found, false).is_ok());
    }

    #[test]
    fn published_optionals_need_yes_and_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source").join("@ace");
        std::fs::create_dir_all(&source).unwrap();
        let output = dir.path().join("output");
        std::fs::create_dir_all(output.join("@ace").join("Optionals")).unwrap();

        let found = published_optionals(&output, &[resolved("@ace", &source)]).unwrap();

        assert_eq!(found, vec![output.join("@ace").join("Optionals")]);
        let err = confirm_prune(&found, false).unwrap_err().to_string();
        assert!(err.contains("--yes"));
        assert!(err.contains("Optionals"));
        assert!(confirm_prune(&found, true).is_ok());
        assert!(output.join("@ace").join("Optionals").exists());
    }

    #[test]
    fn scan_refuses_output_that_overlaps_a_source() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let source = output.join("@ace");
        std::fs::create_dir_all(source.join("optionals")).unwrap();

        let err = published_optionals(&output, &[resolved("@ace", &source)]).unwrap_err();

        assert!(err.to_string().contains("overlaps source"));
    }
}
