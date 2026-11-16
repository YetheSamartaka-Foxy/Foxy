use super::TreeEntry;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 100_000;

pub(super) fn executable_name() -> &'static str {
    if cfg!(windows) {
        "ArmaReforgerServer.exe"
    } else {
        "ArmaReforgerServer"
    }
}

pub(super) fn snapshot_tree(root: &Path) -> io::Result<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_u8)];
    while let Some((directory, depth)) = stack.pop() {
        for entry in fs::read_dir(&directory)? {
            if entries.len() >= MAX_ENTRIES {
                return Err(io::Error::other("Native inventory exceeded entry limit"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !ordinary_entry(&metadata) {
                return Err(io::Error::other(
                    "Native tree contains a link or reparse point",
                ));
            }
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            if metadata.is_dir() {
                entries.push(TreeEntry {
                    path: relative,
                    kind: "directory",
                    size: 0,
                    blake3: None,
                });
                if depth >= 64 {
                    return Err(io::Error::other(
                        "Native inventory exceeded directory depth",
                    ));
                }
                stack.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                let mut file = File::open(entry.path())?;
                let mut hasher = blake3::Hasher::new();
                let mut size = 0_u64;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    size = size.saturating_add(read as u64);
                    hasher.update(&buffer[..read]);
                }
                entries.push(TreeEntry {
                    path: relative,
                    kind: "file",
                    size,
                    blake3: Some(hasher.finalize().to_hex().to_string()),
                });
            } else {
                return Err(io::Error::other(
                    "Native tree contains a non-ordinary entry",
                ));
            }
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

pub(super) fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    let mut stack = vec![(source.to_path_buf(), destination.to_path_buf(), false, 0_u8)];
    let mut entries = 0;
    while let Some((from, to, finish_directory, depth)) = stack.pop() {
        if finish_directory {
            fs::set_permissions(&to, fs::metadata(&from)?.permissions())?;
            continue;
        }
        for entry in fs::read_dir(&from)? {
            entries += 1;
            if entries > MAX_ENTRIES {
                return Err(io::Error::other("Native copy exceeded entry limit"));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !ordinary_entry(&metadata) {
                return Err(io::Error::other(
                    "Native source contains a link or reparse point",
                ));
            }
            let target = to.join(entry.file_name());
            if metadata.is_dir() {
                if depth >= 64 {
                    return Err(io::Error::other("Native copy exceeded directory depth"));
                }
                fs::create_dir(&target)?;
                stack.push((entry.path(), target.clone(), true, depth));
                stack.push((entry.path(), target, false, depth + 1));
            } else if metadata.is_file() {
                fs::copy(entry.path(), &target)?;
                fs::set_permissions(&target, metadata.permissions())?;
            } else {
                return Err(io::Error::other(
                    "Native source contains a non-ordinary entry",
                ));
            }
        }
    }
    Ok(())
}

fn ordinary_entry(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        !metadata.file_type().is_symlink() && metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.file_type().is_symlink()
    }
}

pub(super) fn input_directory(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} must be set explicitly")))?;
    let metadata = fs::symlink_metadata(&path)?;
    if !path.is_absolute() || !path.is_dir() || !ordinary_entry(&metadata) {
        return Err(io::Error::other(format!(
            "{name} must be an absolute ordinary directory"
        )));
    }
    path.canonicalize()
}

pub(super) fn private_directory(_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let path = _path;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other("Native evidence directory is not private"));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn check_linux_prerequisites(scratch: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::metadata(scratch)?;
    if metadata.permissions().mode() & 0o111 == 0 || metadata.dev() == 0 {
        return Err(io::Error::other(
            "Linux scratch must be executable ordinary local storage",
        ));
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::other(
            "Native Linux acceptance requires an ordinary user",
        ));
    }
    Ok(())
}
