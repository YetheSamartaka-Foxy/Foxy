use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    copy_directory(source, destination, 0, &mut 0)
}

fn copy_directory(
    source: &Path,
    destination: &Path,
    depth: usize,
    count: &mut usize,
) -> io::Result<()> {
    if depth > 32 {
        return Err(io::Error::other(
            "Native source directory depth exceeded bound",
        ));
    }
    let metadata = fs::symlink_metadata(source)?;
    if is_link(&metadata) || !metadata.is_dir() {
        return Err(io::Error::other(
            "Native source must be an ordinary directory tree",
        ));
    }
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        *count += 1;
        if *count > 100_000 {
            return Err(io::Error::other("Native source entry count exceeded bound"));
        }
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if is_link(&meta) {
            return Err(io::Error::other("Native source contains a link"));
        }
        if meta.is_dir() {
            copy_directory(&from, &to, depth + 1, count)?;
        } else if meta.is_file() {
            if meta.len() > 8 * 1024 * 1024 * 1024 {
                return Err(io::Error::other("Native source file exceeds bound"));
            }
            fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::other("Native source contains a special file"));
        }
    }
    Ok(())
}

pub(super) fn tree_hash(root: &Path) -> io::Result<blake3::Hash> {
    fn walk(
        root: &Path,
        rel: &Path,
        depth: usize,
        count: &mut usize,
        hash: &mut blake3::Hasher,
    ) -> io::Result<()> {
        if depth > 32 {
            return Err(io::Error::other("Native tree depth exceeded bound"));
        }
        let mut entries = fs::read_dir(root.join(rel))?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            *count += 1;
            if *count > 100_000 {
                return Err(io::Error::other("Native tree entry count exceeded bound"));
            }
            let path = rel.join(entry.file_name());
            let meta = fs::symlink_metadata(entry.path())?;
            if is_link(&meta) {
                return Err(io::Error::other("Native tree acquired a link"));
            }
            hash.update(path.to_string_lossy().as_bytes());
            if meta.is_dir() {
                hash.update(b"D");
                walk(root, &path, depth + 1, count, hash)?;
            } else if meta.is_file() {
                if meta.len() > 8 * 1024 * 1024 * 1024 {
                    return Err(io::Error::other("Native tree file exceeds bound"));
                }
                hash.update(b"F");
                hash.update(&meta.len().to_le_bytes());
                let mut file = fs::File::open(entry.path())?;
                let mut buffer = vec![0; 1024 * 1024];
                loop {
                    let read = std::io::Read::read(&mut file, &mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    hash.update(&buffer[..read]);
                }
                if file.metadata()?.len() != meta.len() {
                    return Err(io::Error::other("Native tree changed while hashing"));
                }
            } else {
                return Err(io::Error::other("Native tree contains a special file"));
            }
        }
        Ok(())
    }
    let mut hash = blake3::Hasher::new();
    let mut count = 0;
    walk(root, Path::new(""), 0, &mut count, &mut hash)?;
    Ok(hash.finalize())
}

pub(super) fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(name).ok_or_else(|| io::Error::other(format!("Set {name}")))?,
    );
    if !path.is_absolute() || !fs::symlink_metadata(&path)?.is_dir() {
        return Err(io::Error::other(format!(
            "{name} must name an existing absolute directory"
        )));
    }
    path.canonicalize()
}

pub(super) fn fresh_child(parent: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let child = parent.join(format!("l_{:016x}", rand::random::<u64>()));
        match fs::create_dir(&child) {
            Ok(()) => return Ok(child),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(
        "Could not allocate fresh lifecycle scratch",
    ))
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
