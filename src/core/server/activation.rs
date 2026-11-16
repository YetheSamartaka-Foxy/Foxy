//! Copy a prepared tree into an instance and restore the previous tree on failure.
//!
//! The adopted installation is not a source or a destination.

#![cfg_attr(not(test), allow(dead_code))]

use std::path::Path;

use super::materialize::confine;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActivationError {
    Escape,
    Io,
    RolledBack,
}

pub(crate) fn activate_copy<F>(
    source_dir: &Path,
    destination_dir: &Path,
    installation_dir: Option<&Path>,
    accept: F,
) -> Result<(), ActivationError>
where
    F: FnOnce(&Path) -> bool,
{
    if let Some(installation) = installation_dir
        && (source_dir.starts_with(installation)
            || installation.starts_with(source_dir)
            || destination_dir.starts_with(installation)
            || installation.starts_with(destination_dir))
    {
        return Err(ActivationError::Escape);
    }
    let staging = destination_dir.with_file_name(format!(
        "{}.previous",
        destination_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dest")
    ));
    if staging.exists() {
        return Err(ActivationError::Escape);
    }
    if destination_dir.exists() {
        copy_tree(destination_dir, &staging)?;
    }
    if let Err(error) = replace_tree(source_dir, destination_dir) {
        restore(destination_dir, &staging);
        return Err(error);
    }
    if accept(destination_dir) {
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        Ok(())
    } else {
        restore(destination_dir, &staging);
        Err(ActivationError::RolledBack)
    }
}

fn replace_tree(source: &Path, destination: &Path) -> Result<(), ActivationError> {
    if destination.exists() {
        std::fs::remove_dir_all(destination).map_err(|_| ActivationError::Io)?;
    }
    copy_tree(source, destination)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), ActivationError> {
    std::fs::create_dir_all(destination).map_err(|_| ActivationError::Io)?;
    for entry in std::fs::read_dir(source).map_err(|_| ActivationError::Io)? {
        let entry = entry.map_err(|_| ActivationError::Io)?;
        let name = entry.file_name();
        let relative = name.to_str().ok_or(ActivationError::Escape)?;
        let target = confine(destination, relative).map_err(|_| ActivationError::Escape)?;
        if entry.file_type().map_err(|_| ActivationError::Io)?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|_| ActivationError::Io)?;
        }
    }
    Ok(())
}

fn restore(destination: &Path, staging: &Path) {
    let _ = std::fs::remove_dir_all(destination);
    if staging.exists() {
        let _ = std::fs::rename(staging, destination);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_activation_restores_the_previous_tree() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("content");
        let installation = root.path().join("adopted");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::create_dir_all(&installation).unwrap();
        std::fs::write(destination.join("old.txt"), b"old").unwrap();
        std::fs::write(source.join("new.txt"), b"new").unwrap();
        assert_eq!(
            activate_copy(&source, &destination, Some(&installation), |_| false),
            Err(ActivationError::RolledBack)
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("old.txt")).unwrap(),
            "old"
        );
        assert!(!destination.join("new.txt").exists());
        assert!(activate_copy(&source, &destination, Some(&installation), |_| true).is_ok());
        assert_eq!(
            std::fs::read_to_string(destination.join("new.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            activate_copy(&installation, &destination, Some(&installation), |_| true),
            Err(ActivationError::Escape)
        );
    }
}
