use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
#[path = "handoff/linux.rs"]
mod platform;
#[cfg(windows)]
#[path = "handoff/windows.rs"]
mod platform;
#[cfg(not(any(target_os = "linux", windows)))]
#[path = "handoff/unsupported.rs"]
mod platform;

pub struct ProtectedDirectory {
    path: PathBuf,
}

impl ProtectedDirectory {
    pub fn create(path: &Path) -> io::Result<Self> {
        platform::create_directory(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        platform::verify_directory(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    pub fn publish_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        validate_name_and_size(name, bytes.len())?;
        platform::publish_new(&self.path, name, bytes)
    }

    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        validate_name_and_size(name, 0)?;
        platform::read(&self.path, name)
    }

    pub fn remove_exact(&self, name: &str, expected: &[u8]) -> io::Result<()> {
        validate_name_and_size(name, expected.len())?;
        platform::remove_exact(&self.path, name, expected)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn validate_name_and_size(name: &str, size: usize) -> io::Result<()> {
    if name.is_empty()
        || name.ends_with('.')
        || name == "."
        || name == ".."
        || name.len() > 80
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        || size > 8192
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid handoff entry.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ProtectedDirectory, validate_name_and_size};
    use std::io;

    #[test]
    fn handoff_names_are_bounded_and_confined() {
        assert!(validate_name_and_size("owner.cap", 8192).is_ok());
        assert!(validate_name_and_size("../owner", 1).is_err());
        assert!(validate_name_and_size("...", 1).is_err());
        assert!(validate_name_and_size("owner.", 1).is_err());
        assert!(validate_name_and_size("", 1).is_err());
        assert!(validate_name_and_size(&"a".repeat(81), 0).is_err());
        assert!(validate_name_and_size("owner", 8193).is_err());
    }

    #[test]
    fn private_directory_round_trips_and_removes_only_matching_bytes() -> io::Result<()> {
        let parent = tempfile::tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let path = parent.path().canonicalize()?.join("handoff");
        let directory = ProtectedDirectory::create(&path)?;
        directory.publish_new("owner.cap", b"capability")?;
        assert_eq!(directory.read("owner.cap")?, b"capability");
        assert_eq!(
            directory
                .remove_exact("owner.cap", b"wrong")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(directory.read("owner.cap")?, b"capability");
        directory.remove_exact("owner.cap", b"capability")?;
        assert_eq!(
            directory.read("owner.cap").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(ProtectedDirectory::open(&path).is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn existing_directory_with_broad_permissions_is_rejected_unchanged() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir()?;
        let path = parent.path().join("handoff");
        std::fs::create_dir(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        assert_eq!(
            ProtectedDirectory::create(&path).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::symlink_metadata(&path)?.permissions().mode() & 0o777,
            0o755
        );
        Ok(())
    }
}
