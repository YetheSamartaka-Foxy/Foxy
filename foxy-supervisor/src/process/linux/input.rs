use super::super::CommandSpec;
use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;

#[repr(C)]
pub(super) struct CapabilityHeader {
    pub version: u32,
    pub pid: i32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct CapabilityData {
    pub effective: u32,
    pub permitted: u32,
    pub inheritable: u32,
}

pub(super) const CAPABILITY_VERSION: u32 = 0x2008_0522;

pub(super) struct Prepared {
    pub executable: CString,
    pub directory: CString,
    pub arguments: Vec<CString>,
    pub environment: Vec<CString>,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub last_capability: u32,
}

fn native(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Process input contains a NUL."))
}

pub(super) fn owner_identity() -> io::Result<(libc::uid_t, libc::gid_t)> {
    let (mut user, mut effective_user, mut saved_user) = (0, 0, 0);
    let (mut group, mut effective_group, mut saved_group) = (0, 0, 0);
    if unsafe { libc::getresuid(&mut user, &mut effective_user, &mut saved_user) } != 0
        || unsafe { libc::getresgid(&mut group, &mut effective_group, &mut saved_group) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut header = CapabilityHeader {
        version: CAPABILITY_VERSION,
        pid: 0,
    };
    let mut capabilities = [CapabilityData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    if unsafe {
        libc::syscall(
            libc::SYS_capget,
            &mut header as *mut CapabilityHeader,
            capabilities.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if user == 0
        || group == 0
        || user != effective_user
        || user != saved_user
        || group != effective_group
        || group != saved_group
        || capabilities
            .iter()
            .any(|set| set.effective != 0 || set.permitted != 0 || set.inheritable != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "An unprivileged same-user owner is required.",
        ));
    }
    Ok((user, group))
}

impl Prepared {
    pub(super) fn new(spec: &CommandSpec) -> io::Result<Self> {
        if !spec.executable.is_absolute() || !spec.directory.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Process paths must be absolute.",
            ));
        }
        let executable = native(spec.executable.as_os_str())?;
        let directory = native(spec.directory.as_os_str())?;
        let mut arguments = Vec::with_capacity(spec.arguments.len() + 1);
        arguments.push(executable.clone());
        for argument in &spec.arguments {
            arguments.push(native(argument)?);
        }
        let mut variables: BTreeMap<_, _> = std::env::vars_os().collect();
        for (key, value) in &spec.environment {
            if key.is_empty() || key.as_bytes().contains(&b'=') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid environment key.",
                ));
            }
            native(key)?;
            native(value)?;
            variables.insert(key.clone(), value.clone());
        }
        let environment = variables
            .into_iter()
            .map(|(key, value)| {
                let mut entry = key.into_encoded_bytes();
                entry.push(b'=');
                entry.extend(value.into_encoded_bytes());
                CString::new(entry).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Environment contains a NUL.")
                })
            })
            .collect::<io::Result<_>>()?;
        let (uid, gid) = owner_identity()?;
        let last_capability = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")?
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|value| *value <= 63)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "Capability policy is unknown.")
            })?;
        Ok(Self {
            executable,
            directory,
            arguments,
            environment,
            uid,
            gid,
            last_capability,
        })
    }
}
