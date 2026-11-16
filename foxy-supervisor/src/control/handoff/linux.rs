use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Unsafe protected handoff directory.",
    )
}

fn validate_chain(path: &Path) -> io::Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = Path::new("/").to_path_buf();
    for component in absolute.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(part) => current.push(part),
            _ => return Err(denied()),
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(denied());
        }
        if metadata.uid() != 0 && metadata.uid() != unsafe { libc::geteuid() } {
            return Err(denied());
        }
        if metadata.permissions().mode() & 0o022 != 0
            && !(metadata.uid() == 0 && metadata.permissions().mode() & 0o1000 != 0)
        {
            return Err(denied());
        }
    }
    Ok(())
}

fn verify_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(denied)?;
    validate_chain(parent)?;
    let metadata = fs::symlink_metadata(parent)?;
    let private = metadata.uid() == unsafe { libc::geteuid() }
        && metadata.permissions().mode() & 0o777 == 0o700;
    let sticky_root = metadata.uid() == 0 && metadata.permissions().mode() & 0o1000 != 0;
    if !private && !sticky_root {
        return Err(denied());
    }
    Ok(())
}

pub(super) fn create_directory(path: &Path) -> io::Result<()> {
    verify_parent(path)?;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return verify_directory(path);
        }
        Err(error) => Err(error),
    }?;
    verify_directory(path)
}

pub(super) fn verify_directory(path: &Path) -> io::Result<()> {
    verify_parent(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(denied());
    }
    Ok(())
}

fn open_file(path: &Path, name: &str, create: bool) -> io::Result<File> {
    verify_directory(path)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    if create {
        options.write(true).create_new(true).mode(0o600);
    }
    let file = options.open(path.join(name))?;
    verify_file(&file)?;
    Ok(file)
}

fn verify_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > 8192
    {
        return Err(denied());
    }
    Ok(())
}

pub(super) fn publish_new(path: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_file(path, name, true)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub(super) fn read(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    let mut file = open_file(path, name, false)?;
    let mut bytes = Vec::new();
    (&mut file).take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(denied());
    }
    Ok(bytes)
}

pub(super) fn remove_exact(path: &Path, name: &str, expected: &[u8]) -> io::Result<()> {
    let mut file = open_file(path, name, false)?;
    let mut bytes = Vec::new();
    (&mut file).take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 || bytes != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Handoff entry changed.",
        ));
    }
    verify_directory(path)?;
    let entry = path.join(name);
    let check = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&entry)?;
    let current = check.metadata()?;
    let opened = file.metadata()?;
    if current.ino() != opened.ino() || current.dev() != opened.dev() {
        return Err(denied());
    }
    fs::remove_file(entry)
}
