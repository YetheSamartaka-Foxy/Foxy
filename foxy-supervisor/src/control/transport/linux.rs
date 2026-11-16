use super::{IoBudget, LocalEndpoint, MAX_UNIX_SOCKET_PATH_BYTES};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const POLL_SLICE: Duration = Duration::from_millis(10);

pub(super) struct Listener {
    socket: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

pub(super) struct Connection<'a> {
    stream: UnixStream,
    _listener: PhantomData<&'a mut Listener>,
}

fn endpoint_path(endpoint: &LocalEndpoint) -> io::Result<&Path> {
    match endpoint {
        LocalEndpoint::UnixSocket(path) => Ok(path),
        LocalEndpoint::NamedPipe(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsupported local endpoint.",
        )),
    }
}

fn validate_path(path: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.len() > MAX_UNIX_SOCKET_PATH_BYTES
        || bytes.contains(&0)
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid local endpoint.",
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid local endpoint."))?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if parent.starts_with(&current) {
            let metadata = std::fs::symlink_metadata(&current)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unsafe local endpoint.",
                ));
            }
            if metadata.uid() != 0 && metadata.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unsafe local endpoint.",
                ));
            }
            if current == parent
                && (metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.permissions().mode() & 0o777 != 0o700)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unsafe local endpoint.",
                ));
            }
            if parent.starts_with(&current)
                && metadata.permissions().mode() & 0o022 != 0
                && metadata.permissions().mode() & 0o1000 == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unsafe local endpoint.",
                ));
            }
        }
    }
    Ok(())
}

fn poll(fd: i32, events: i16, budget: IoBudget<'_>) -> io::Result<()> {
    loop {
        budget.check()?;
        let timeout = budget.remaining()?.min(POLL_SLICE);
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                timeout.as_millis().max(1).min(i32::MAX as u128) as i32,
            )
        };
        if result > 0 {
            if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("Local transport became unavailable."));
            }
            if descriptor.revents & (events | libc::POLLHUP) != 0 {
                return Ok(());
            }
        } else if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn verify_peer(stream: &UnixStream) -> io::Result<()> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of_val(&credentials)
        || credentials.uid != unsafe { libc::geteuid() }
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Local peer rejected.",
        ));
    }
    Ok(())
}

fn connect_socket(path: &Path, budget: IoBudget<'_>) -> io::Result<UnixStream> {
    budget.check()?;
    validate_path(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unsafe local endpoint.",
        ));
    }
    let socket = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if socket < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(socket) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *slot = byte as libc::c_char;
    }
    let length = (std::mem::size_of_val(&address.sun_family) + bytes.len() + 1) as libc::socklen_t;
    let result = unsafe {
        libc::connect(
            socket,
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS)
            && error.raw_os_error() != Some(libc::EAGAIN)
        {
            return Err(error);
        }
        poll(socket, libc::POLLOUT, budget)?;
        let mut error_code = 0i32;
        let mut error_length = std::mem::size_of_val(&error_code) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                socket,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error_code as *mut i32).cast(),
                &mut error_length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if error_code != 0 {
            return Err(io::Error::from_raw_os_error(error_code));
        }
    }
    stream.set_nonblocking(true)?;
    verify_peer(&stream)?;
    Ok(stream)
}

impl Listener {
    pub(super) fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
        let path = endpoint_path(endpoint)?.to_path_buf();
        validate_path(&path)?;
        let socket = UnixListener::bind(&path)?;
        let initial = match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == unsafe { libc::geteuid() } =>
            {
                metadata
            }
            Ok(_) => {
                drop(socket);
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Unsafe local endpoint.",
                ));
            }
            Err(error) => {
                drop(socket);
                return Err(error);
            }
        };
        let identity = Some((initial.dev(), initial.ino()));
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            drop(socket);
            remove_owned_socket(&path, identity);
            return Err(error);
        }
        if let Err(error) = socket.set_nonblocking(true) {
            drop(socket);
            remove_owned_socket(&path, identity);
            return Err(error);
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                drop(socket);
                remove_owned_socket(&path, identity);
                return Err(error);
            }
        };
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
            || (metadata.dev(), metadata.ino()) != (initial.dev(), initial.ino())
        {
            drop(socket);
            remove_owned_socket(&path, identity);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Unsafe local endpoint.",
            ));
        }
        Ok(Self {
            socket,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub(super) fn accept<'a>(&'a mut self, budget: IoBudget<'_>) -> io::Result<Connection<'a>> {
        loop {
            budget.check()?;
            match self.socket.accept() {
                Ok((stream, _)) => {
                    verify_peer(&stream)?;
                    stream.set_nonblocking(true)?;
                    return Ok(Connection {
                        stream,
                        _listener: PhantomData,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    poll(self.socket.as_raw_fd(), libc::POLLIN, budget)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        remove_owned_socket(&self.path, Some((self.device, self.inode)));
    }
}

fn remove_owned_socket(path: &Path, identity: Option<(u64, u64)>) {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_socket()
        && metadata.uid() == unsafe { libc::geteuid() }
        && identity
            .is_none_or(|(device, inode)| metadata.dev() == device && metadata.ino() == inode)
    {
        let _ = std::fs::remove_file(path);
    }
}

pub(super) fn connect<'a>(
    endpoint: &LocalEndpoint,
    budget: IoBudget<'_>,
) -> io::Result<Connection<'a>> {
    let stream = connect_socket(endpoint_path(endpoint)?, budget)?;
    Ok(Connection {
        stream,
        _listener: PhantomData,
    })
}

impl Connection<'_> {
    pub(super) fn read_exact(&mut self, buffer: &mut [u8], budget: IoBudget<'_>) -> io::Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            budget.check()?;
            match self.stream.read(&mut buffer[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Local peer closed the connection.",
                    ));
                }
                Ok(count) => offset += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    poll(self.stream.as_raw_fd(), libc::POLLIN, budget)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(super) fn write_all(&mut self, buffer: &[u8], budget: IoBudget<'_>) -> io::Result<()> {
        let mut offset = 0;
        while offset < buffer.len() {
            budget.check()?;
            match self.stream.write(&buffer[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "Local peer stopped accepting data.",
                    ));
                }
                Ok(count) => offset += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    poll(self.stream.as_raw_fd(), libc::POLLOUT, budget)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
