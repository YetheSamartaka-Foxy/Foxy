use std::ffi::OsString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct CleanupWitnessProcess {
    child: Child,
}

pub(super) struct PreparedWitness {
    executable: std::path::PathBuf,
    arguments: Vec<OsString>,
    receipt: std::fs::File,
}

impl PreparedWitness {
    pub(super) fn new(spec: &super::super::CleanupWitnessSpec<'_>) -> io::Result<Self> {
        if !spec.command.executable.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Witness executable path must be absolute.",
            ));
        }
        if spec.command.executable.as_os_str().as_bytes().contains(&0)
            || spec
                .command
                .arguments
                .iter()
                .any(|argument| argument.as_bytes().contains(&0))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Witness command contains a NUL.",
            ));
        }
        Ok(Self {
            executable: spec.command.executable.clone(),
            arguments: spec.command.arguments.clone(),
            receipt: spec.receipt.try_clone()?,
        })
    }
}

impl CleanupWitnessProcess {
    pub(super) fn spawn(
        prepared: PreparedWitness,
        owner: &OwnedFd,
        namespace: &OwnedFd,
    ) -> io::Result<Self> {
        let (ready_read, ready_write) = ready_pipe()?;
        let receipt = prepared.receipt;
        let inherited = [
            owner.as_raw_fd(),
            namespace.as_raw_fd(),
            receipt.as_raw_fd(),
            ready_write.as_raw_fd(),
        ];
        let mut command = Command::new(prepared.executable);
        command
            .args(prepared.arguments)
            .arg(inherited[0].to_string())
            .arg(inherited[1].to_string())
            .arg(inherited[2].to_string())
            .arg(inherited[3].to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 1u32 << 2) != 0 {
                    return Err(io::Error::last_os_error());
                }
                for fd in inherited {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(ready_write);
        let mut ready = std::fs::File::from(ready_read);
        let start = Instant::now();
        let mut byte = [0u8; 1];
        loop {
            let remaining = READY_TIMEOUT.saturating_sub(start.elapsed());
            let mut descriptor = libc::pollfd {
                fd: ready.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe {
                libc::poll(
                    &mut descriptor,
                    1,
                    remaining.as_millis().min(i32::MAX as u128) as i32,
                )
            };
            if result > 0 && descriptor.revents & libc::POLLIN != 0 {
                ready.read_exact(&mut byte)?;
                if byte != [1] {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::other("Cleanup witness readiness was invalid."));
                }
                return Ok(Self { child });
            }
            if result == 0 {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Cleanup witness did not become ready.",
                ));
            }
            if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            let error = if result > 0 {
                io::Error::other("Cleanup witness readiness pipe closed.")
            } else {
                io::Error::last_os_error()
            };
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }

    pub(super) fn completed(&mut self) -> io::Result<Option<bool>> {
        self.child
            .try_wait()
            .map(|status| status.map(|status| status.success()))
    }
}

impl Drop for CleanupWitnessProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn ready_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut raw = [-1; 2];
    if unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let read = unsafe { OwnedFd::from_raw_fd(raw[0]) };
    let write = unsafe { OwnedFd::from_raw_fd(raw[1]) };
    Ok((read, write))
}

pub(crate) fn run_cleanup_witness(
    owner: u64,
    tree: u64,
    receipt: u64,
    ready: u64,
) -> io::Result<()> {
    use std::os::fd::{FromRawFd, RawFd};

    let fd = |value: u64| -> io::Result<RawFd> {
        i32::try_from(value)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid witness descriptor."))
    };
    let owner = fd(owner)?;
    let tree = fd(tree)?;
    let raw_receipt = fd(receipt)?;
    let raw_ready = fd(ready)?;
    let all = [owner, tree, raw_receipt, raw_ready];
    if all.iter().any(|fd| *fd < 3)
        || all
            .iter()
            .enumerate()
            .any(|(index, fd)| all[..index].contains(fd))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Witness descriptors must be distinct inherited capabilities.",
        ));
    }
    for fd in all {
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    for pidfd in [owner, tree] {
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd,
                0,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    let receipt = unsafe { std::fs::File::from_raw_fd(raw_receipt) };
    let ready = unsafe { std::fs::File::from_raw_fd(raw_ready) };
    let mut writer = crate::recovery::witness::WitnessReceiptWriter::open(receipt)?;
    let mut ready = ready;
    use std::io::Write;
    ready.write_all(&[1])?;
    let mut descriptors = [
        libc::pollfd {
            fd: owner,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: tree,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let (mut owner_dead, mut tree_dead) = (false, false);
    while !tree_dead {
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        owner_dead |= descriptors[0].revents & (libc::POLLIN | libc::POLLHUP) != 0;
        tree_dead |= descriptors[1].revents & (libc::POLLIN | libc::POLLHUP) != 0;
        if descriptors
            .iter()
            .any(|descriptor| descriptor.revents & (libc::POLLNVAL | libc::POLLERR) != 0)
        {
            return Err(io::Error::other(
                "Cleanup witness authority became unavailable.",
            ));
        }
        if owner_dead && !tree_dead {
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    tree,
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0u32,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            descriptors[0].fd = -1;
        }
        descriptors[0].revents = 0;
        descriptors[1].revents = 0;
    }
    // Namespace init becomes pollable after the kernel has killed and reaped its namespace.
    writer.complete("linux_pid_namespace", owner_dead)
}
