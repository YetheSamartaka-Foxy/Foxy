use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    volume: u64,
    file: u128,
}

pub struct InstallationLocks {
    directory: PathBuf,
    handle: File,
    identity: DirectoryIdentity,
    pub(crate) namespace: PathBuf,
}

pub struct Admission<'a> {
    pub(crate) installation: &'a InstallationLocks,
    _file: File,
}

pub struct RuntimeLease {
    _file: File,
    _directory: File,
}

pub struct MaintenanceLease<'a> {
    _admission: Admission<'a>,
    _file: File,
}

fn unsafe_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "The lease path is not an ordinary local directory or file.",
    )
}

pub(crate) fn ordinary(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        !metadata.is_symlink() && metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.is_symlink()
    }
}

pub(crate) fn directory_chain(path: &Path, create: bool) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(unsafe_path());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir) {
            return Err(unsafe_path());
        }
        current.push(component);
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if ordinary(&metadata) && metadata.is_dir() => {}
            Ok(_) => return Err(unsafe_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                let builder = fs::DirBuilder::new();
                #[cfg(unix)]
                let builder = {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = builder;
                    builder.mode(0o700);
                    builder
                };
                if let Err(error) = builder.create(&current)
                    && error.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(error);
                }
                let metadata = fs::symlink_metadata(&current)?;
                if !ordinary(&metadata) || !metadata.is_dir() {
                    return Err(unsafe_path());
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn proposed_directory(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(unsafe_path());
    }
    let mut existing = path;
    let mut tail = vec![];
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tail.push(existing.file_name().ok_or_else(unsafe_path)?);
                existing = existing.parent().ok_or_else(unsafe_path)?;
            }
            Err(error) => return Err(error),
        }
    }
    directory_chain(existing, false)?;
    let mut resolved = existing.canonicalize()?;
    for component in tail.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn contains_directory(root: &Path, candidate: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut candidate = candidate.components();
        root.components().all(|part| {
            let Some(other) = candidate.next() else {
                return false;
            };
            let left: Vec<_> = part.as_os_str().encode_wide().collect();
            let right: Vec<_> = other.as_os_str().encode_wide().collect();
            const CSTR_EQUAL: i32 = 2;
            unsafe {
                winapi::um::stringapiset::CompareStringOrdinal(
                    left.as_ptr(),
                    left.len() as i32,
                    right.as_ptr(),
                    right.len() as i32,
                    1,
                ) == CSTR_EQUAL
            }
        })
    }
    #[cfg(not(windows))]
    {
        candidate.starts_with(root)
    }
}

fn open_directory(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use winapi::um::winbase::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    }
    options.open(path)
}

fn directory_identity(file: &File) -> io::Result<DirectoryIdentity> {
    let metadata = file.metadata()?;
    if !ordinary(&metadata) || !metadata.is_dir() {
        return Err(unsafe_path());
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use winapi::um::fileapi::FILE_ID_INFO;
        use winapi::um::minwinbase::FileIdInfo;
        use winapi::um::winbase::GetFileInformationByHandleEx;
        let mut information: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle().cast(),
                FileIdInfo,
                (&mut information as *mut FILE_ID_INFO).cast(),
                std::mem::size_of_val(&information) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(DirectoryIdentity {
            volume: information.VolumeSerialNumber,
            file: u128::from_le_bytes(information.FileId.Identifier),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(DirectoryIdentity {
            volume: metadata.dev(),
            file: metadata.ino() as u128,
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Physical directory identity is unavailable.",
        ))
    }
}

fn lock_file(path: &Path) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !ordinary(&metadata) || !metadata.is_file() => return Err(unsafe_path()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(winapi::um::winbase::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !ordinary(&metadata) || !metadata.is_file() {
        return Err(unsafe_path());
    }
    Ok(file)
}

impl InstallationLocks {
    pub fn capture(directory: &Path) -> io::Result<Self> {
        let registry = dirs::data_local_dir()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Per-user local data directory is unavailable.",
                )
            })?
            .join("Foxy/server-installation-locks");
        Self::capture_in(directory, &registry)
    }

    pub fn capture_in(directory: &Path, registry: &Path) -> io::Result<Self> {
        directory_chain(directory, false)?;
        let registry = proposed_directory(registry)?;
        let directory = directory.canonicalize()?;
        directory_chain(&directory, false)?;
        if contains_directory(&directory, &registry) || contains_directory(&registry, &directory) {
            return Err(unsafe_path());
        }
        let handle = open_directory(&directory)?;
        let identity = directory_identity(&handle)?;
        let namespace = registry.join(format!(
            "v1_{:016x}_{:032x}",
            identity.volume, identity.file
        ));
        Ok(Self {
            directory,
            handle,
            identity,
            namespace,
        })
    }

    pub fn revalidate(&self) -> io::Result<()> {
        directory_chain(&self.directory, false)?;
        if directory_identity(&self.handle)? != self.identity
            || directory_identity(&open_directory(&self.directory)?)? != self.identity
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Installation directory identity changed.",
            ));
        }
        Ok(())
    }

    pub fn try_admit(&self) -> io::Result<Admission<'_>> {
        let admission = self.admit_for_recovery()?;
        self.require_no_maintenance_intent()?;
        Ok(admission)
    }

    pub(crate) fn admit_for_recovery(&self) -> io::Result<Admission<'_>> {
        self.revalidate()?;
        directory_chain(&self.namespace, true)?;
        let file = lock_file(&self.namespace.join("admission.lock"))?;
        file.try_lock().map_err(io::Error::from)?;
        self.revalidate()?;
        Ok(Admission {
            installation: self,
            _file: file,
        })
    }
}

impl<'a> Admission<'a> {
    pub fn try_runtime(&self) -> io::Result<RuntimeLease> {
        self.installation.require_no_maintenance_intent()?;
        self.installation.revalidate()?;
        let file = lock_file(&self.installation.namespace.join("content.lock"))?;
        file.try_lock_shared().map_err(io::Error::from)?;
        self.installation.revalidate()?;
        Ok(RuntimeLease {
            _file: file,
            _directory: self.installation.handle.try_clone()?,
        })
    }

    pub fn try_maintenance(self) -> Result<MaintenanceLease<'a>, (Self, io::Error)> {
        let acquire = || {
            self.installation.require_no_maintenance_intent()?;
            self.exclusive_content()
        };
        match acquire() {
            Ok(file) => Ok(MaintenanceLease {
                _admission: self,
                _file: file,
            }),
            Err(error) => Err((self, error)),
        }
    }

    pub(crate) fn exclusive_content(&self) -> io::Result<File> {
        self.installation.revalidate()?;
        let file = lock_file(&self.installation.namespace.join("content.lock"))?;
        file.try_lock().map_err(io::Error::from)?;
        self.installation.revalidate()?;
        if !self.installation.inspect_consumers()?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Installation consumer cleanup has not been verified.",
            ));
        }
        Ok(file)
    }
}

impl MaintenanceLease<'_> {
    pub fn installation_key(&self) -> io::Result<String> {
        self._admission.installation.coordination_key()
    }

    pub(crate) fn installation(&self) -> &InstallationLocks {
        self._admission.installation
    }
}

#[cfg(test)]
mod tests;
