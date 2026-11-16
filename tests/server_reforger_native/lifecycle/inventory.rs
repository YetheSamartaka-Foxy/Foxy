use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    directory: bool,
    bytes: u64,
    blake3: Option<String>,
    executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Snapshot(Vec<Entry>);

pub(super) fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("Native inventory root must be ordinary"));
    }
    let mut entries = Vec::new();
    let mut stack = vec![(PathBuf::new(), 0usize)];
    while let Some((relative, depth)) = stack.pop() {
        if depth > 32 {
            return Err(io::Error::other("Native inventory depth exceeded bound"));
        }
        let directory = root.join(&relative);
        for item in fs::read_dir(&directory)? {
            if entries.len() >= MAX_ENTRIES {
                return Err(io::Error::other("Native inventory entry limit exceeded"));
            }
            let item = item?;
            let path = relative.join(item.file_name());
            let name = path
                .to_str()
                .ok_or_else(|| io::Error::other("Native inventory path is not UTF-8"))?
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(item.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native inventory contains a link"));
            }
            if metadata.is_dir() {
                entries.push(Entry {
                    path: name,
                    directory: true,
                    bytes: 0,
                    blake3: None,
                    executable: false,
                });
                stack.push((path, depth + 1));
            } else if metadata.is_file() {
                if metadata.len() > MAX_FILE_BYTES {
                    return Err(io::Error::other("Native inventory file exceeds bound"));
                }
                let mut file = File::open(item.path())?;
                let mut hasher = blake3::Hasher::new();
                let mut bytes = 0u64;
                let mut buffer = [0; 64 * 1024];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    bytes = bytes
                        .checked_add(read as u64)
                        .ok_or_else(|| io::Error::other("Native inventory size overflow"))?;
                    if bytes > metadata.len() || bytes > MAX_FILE_BYTES {
                        return Err(io::Error::other("Native file changed during inventory"));
                    }
                    hasher.update(&buffer[..read]);
                }
                if bytes != metadata.len() || file.metadata()?.len() != metadata.len() {
                    return Err(io::Error::other("Native file changed during inventory"));
                }
                entries.push(Entry {
                    path: name,
                    directory: false,
                    bytes,
                    blake3: Some(hasher.finalize().to_hex().to_string()),
                    executable: executable(&metadata),
                });
            } else {
                return Err(io::Error::other("Native inventory contains a special file"));
            }
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Snapshot(entries))
}

pub(super) fn required_directory(name: &str) -> io::Result<PathBuf> {
    let path = std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other(format!("{name} must be set explicitly")))?;
    if !path.is_absolute() {
        return Err(io::Error::other(format!("{name} must be absolute")));
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other(format!(
            "{name} must be an ordinary directory"
        )));
    }
    path.canonicalize()
}

pub(super) fn fresh_child(parent: &Path) -> io::Result<PathBuf> {
    for _ in 0..16 {
        let child = parent.join(format!("r_{:016x}", rand::random::<u64>()));
        match fs::create_dir(&child) {
            Ok(()) => return Ok(child),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other(
        "Could not allocate fresh Reforger scratch",
    ))
}

pub(super) fn private_directory(_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let path = _path;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::other("Native scratch directory is not private"));
        }
    }
    Ok(())
}

pub(super) fn udp_ports(count: usize) -> io::Result<Vec<u16>> {
    udp_ports_excluding(count, &[])
}

pub(super) fn udp_ports_excluding(count: usize, excluded: &[u16]) -> io::Result<Vec<u16>> {
    let mut ports = Vec::with_capacity(count);
    while ports.len() < count {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = socket.local_addr()?.port();
        drop(socket);
        if !excluded.contains(&port) && !ports.contains(&port) {
            ports.push(port);
        }
    }
    Ok(ports)
}

fn executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

#[cfg(target_os = "linux")]
pub(super) fn check_linux_prerequisites(scratch: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::metadata(scratch)?;
    if metadata.permissions().mode() & 0o111 == 0 || metadata.dev() == 0 {
        return Err(io::Error::other(
            "Linux scratch must be executable local storage",
        ));
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::other(
            "Native Linux acceptance requires an ordinary user",
        ));
    }
    Ok(())
}
