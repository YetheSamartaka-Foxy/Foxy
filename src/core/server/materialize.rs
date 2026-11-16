#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

use super::configuration::ConfigurationFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterializeError {
    Escape,
    Io,
}

pub(crate) fn materialize_configuration(
    runtime_dir: &Path,
    files: &[ConfigurationFile],
) -> Result<Vec<PathBuf>, MaterializeError> {
    if files.is_empty() || !runtime_dir.is_dir() {
        return Err(MaterializeError::Escape);
    }
    let mut written = Vec::new();
    for file in files {
        let destination = confine(runtime_dir, &file.relative_path)?;
        if let Some(parent) = destination.parent()
            && parent != runtime_dir
        {
            std::fs::create_dir_all(parent).map_err(|_| MaterializeError::Io)?;
        }
        std::fs::write(&destination, file.contents.as_bytes()).map_err(|_| MaterializeError::Io)?;
        written.push(destination);
    }
    Ok(written)
}

pub(crate) fn confine(root: &Path, relative: &str) -> Result<PathBuf, MaterializeError> {
    if relative.is_empty()
        || relative.starts_with(['/', '\\'])
        || Path::new(relative).is_absolute()
        || relative
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(MaterializeError::Escape);
    }
    let destination = root.join(relative);
    if destination.starts_with(root) {
        Ok(destination)
    } else {
        Err(MaterializeError::Escape)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, contents: &str) -> ConfigurationFile {
        ConfigurationFile {
            relative_path: path.into(),
            contents: contents.into(),
            blake3: String::new(),
        }
    }

    #[test]
    fn a_snapshot_stays_inside_the_runtime_directory() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        let written =
            materialize_configuration(&runtime, &[file("server.cfg", "hostname = \"Main\";\n")])
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&written[0]).unwrap(),
            "hostname = \"Main\";\n"
        );
        assert!(written[0].starts_with(&runtime));
        assert_eq!(
            materialize_configuration(&runtime, &[file("../server.cfg", "nope")]),
            Err(MaterializeError::Escape)
        );
        assert!(!root.path().join("server.cfg").exists());
    }
}
