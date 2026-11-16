#![cfg_attr(not(test), allow(dead_code))]

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackupEvidence {
    pub archive_present: bool,
    pub engine_compatible: bool,
    pub config_compatible: bool,
    pub content_compatible: bool,
    pub provider_compatible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackupError {
    Incompatible,
    Escape,
    Io,
}

pub(crate) fn backup_owned_files(
    source_dir: &Path,
    backup_dir: &Path,
    evidence: &BackupEvidence,
) -> Result<(), BackupError> {
    if !evidence.engine_compatible
        || !evidence.config_compatible
        || !evidence.content_compatible
        || !evidence.provider_compatible
    {
        return Err(BackupError::Incompatible);
    }
    if source_dir == backup_dir
        || backup_dir.starts_with(source_dir)
        || source_dir.starts_with(backup_dir)
    {
        return Err(BackupError::Escape);
    }
    std::fs::create_dir_all(backup_dir).map_err(|_| BackupError::Io)?;
    for entry in std::fs::read_dir(source_dir).map_err(|_| BackupError::Io)? {
        let entry = entry.map_err(|_| BackupError::Io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(BackupError::Escape);
        };
        if name.contains(['/', '\\']) || name == "." || name == ".." {
            return Err(BackupError::Escape);
        }
        let target = backup_dir.join(name);
        if !target.starts_with(backup_dir) {
            return Err(BackupError::Escape);
        }
        if entry.file_type().map_err(|_| BackupError::Io)?.is_file() {
            std::fs::copy(entry.path(), target).map_err(|_| BackupError::Io)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> BackupEvidence {
        BackupEvidence {
            archive_present: false,
            engine_compatible: true,
            config_compatible: true,
            content_compatible: true,
            provider_compatible: true,
        }
    }

    #[test]
    fn missing_compatibility_refuses_the_backup() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("runtime");
        let backup = root.path().join("backups");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("server.cfg"), b"hostname = \"Main\";\n").unwrap();
        let mut evidence = ready();
        evidence.engine_compatible = false;
        assert_eq!(
            backup_owned_files(&source, &backup, &evidence),
            Err(BackupError::Incompatible)
        );
        assert!(!backup.exists());
        backup_owned_files(&source, &backup, &ready()).unwrap();
        assert_eq!(
            std::fs::read_to_string(backup.join("server.cfg")).unwrap(),
            "hostname = \"Main\";\n"
        );
    }
}
