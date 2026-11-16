use super::input::{CAPABILITY_VERSION, CapabilityData, CapabilityHeader, Prepared};
use std::os::fd::RawFd;

pub(super) const STARTED: i32 = 1;
pub(super) const ROOT_EXIT: i32 = 2;
pub(super) const FAILED: i32 = 3;
pub(super) const COMPLETE: i32 = 4;

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(super) struct Message {
    pub kind: i32,
    pub value: i32,
}

pub(super) struct Configuration<'a> {
    pub prepared: &'a Prepared,
    pub arguments: &'a [*const libc::c_char],
    pub environment: &'a [*const libc::c_char],
    pub gate: RawFd,
    pub status: RawFd,
    pub owner: RawFd,
    pub retained: [RawFd; 6],
    pub control: RawFd,
    pub output: RawFd,
    pub null_input: RawFd,
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

unsafe fn transfer(fd: RawFd, pointer: *mut libc::c_void, length: usize, write: bool) -> bool {
    let mut offset = 0;
    while offset < length {
        let result = unsafe {
            if write {
                libc::write(fd, pointer.byte_add(offset).cast_const(), length - offset)
            } else {
                libc::read(fd, pointer.byte_add(offset), length - offset)
            }
        };
        if result > 0 {
            offset += result as usize;
        } else if result == 0 || errno() != libc::EINTR {
            return false;
        }
    }
    true
}

unsafe fn message(fd: RawFd, kind: i32, value: i32) -> bool {
    let mut message = Message { kind, value };
    unsafe {
        transfer(
            fd,
            (&mut message as *mut Message).cast(),
            std::mem::size_of::<Message>(),
            true,
        )
    }
}

unsafe fn fail(fd: RawFd, code: i32) -> ! {
    unsafe {
        message(fd, FAILED, code);
        libc::_exit(127)
    }
}

unsafe fn close_others(retained: &[RawFd]) -> bool {
    let mut first = 3u32;
    loop {
        let next = retained
            .iter()
            .copied()
            .filter(|fd| *fd >= first as RawFd)
            .map(|fd| fd as u32)
            .min();
        let Some(fd) = next else {
            break;
        };
        if first < fd && unsafe { libc::syscall(libc::SYS_close_range, first, fd - 1, 0u32) } != 0 {
            return false;
        }
        first = fd + 1;
    }
    unsafe { libc::syscall(libc::SYS_close_range, first, u32::MAX, 0u32) == 0 }
}

unsafe fn reset_signals() -> bool {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in 1..=64 {
            if signal != libc::SIGKILL
                && signal != libc::SIGSTOP
                && libc::sigaction(signal, &action, std::ptr::null_mut()) != 0
                && errno() != libc::EINVAL
            {
                return false;
            }
        }
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) == 0
    }
}

unsafe fn unprivileged(prepared: &Prepared) -> bool {
    unsafe {
        if libc::getuid() != prepared.uid
            || libc::geteuid() != prepared.uid
            || libc::getgid() != prepared.gid
            || libc::getegid() != prepared.gid
            || libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL,
                0,
                0,
                0,
            ) != 0
        {
            return false;
        }
        for capability in 0..=prepared.last_capability {
            if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) != 0 {
                return false;
            }
        }
        let header = CapabilityHeader {
            version: CAPABILITY_VERSION,
            pid: 0,
        };
        let capabilities = [CapabilityData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }; 2];
        libc::syscall(
            libc::SYS_capset,
            &header as *const CapabilityHeader,
            capabilities.as_ptr(),
        ) == 0
            && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
    }
}

unsafe fn owner_alive(fd: RawFd) -> bool {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if result >= 0 {
            return result == 0;
        }
        if errno() != libc::EINTR {
            return false;
        }
    }
}

unsafe fn payload(configuration: &Configuration<'_>, error: RawFd) -> ! {
    unsafe {
        if (configuration.control >= 0 && libc::setpgid(0, 0) != 0)
            || !close_others(&[error, configuration.output, configuration.null_input])
            || !reset_signals()
        {
            let mut code = errno();
            transfer(error, (&mut code as *mut i32).cast(), 4, true);
            libc::_exit(127);
        }
        if configuration.null_input >= 0 && libc::dup2(configuration.null_input, 0) < 0 {
            let mut code = errno();
            transfer(error, (&mut code as *mut i32).cast(), 4, true);
            libc::_exit(127);
        }
        if configuration.output >= 0
            && (libc::dup2(configuration.output, 1) < 0 || libc::dup2(configuration.output, 2) < 0)
        {
            let mut code = errno();
            transfer(error, (&mut code as *mut i32).cast(), 4, true);
            libc::_exit(127);
        }
        libc::execve(
            configuration.prepared.executable.as_ptr(),
            configuration.arguments.as_ptr(),
            configuration.environment.as_ptr(),
        );
        let mut code = errno();
        transfer(error, (&mut code as *mut i32).cast(), 4, true);
        libc::_exit(127)
    }
}

pub(super) unsafe fn run(configuration: &Configuration<'_>) -> ! {
    unsafe {
        if !close_others(&configuration.retained)
            || !reset_signals()
            || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || !owner_alive(configuration.owner)
        {
            fail(configuration.status, libc::EPERM);
        }
        let mut gate = 0u8;
        if !transfer(configuration.gate, (&mut gate as *mut u8).cast(), 1, false) || gate != 1 {
            libc::_exit(127);
        }
        libc::close(configuration.gate);
        if !unprivileged(configuration.prepared)
            || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || !owner_alive(configuration.owner)
            || libc::chdir(configuration.prepared.directory.as_ptr()) != 0
        {
            fail(configuration.status, libc::EPERM);
        }
        let mut errors = [-1; 2];
        if libc::pipe2(errors.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            fail(configuration.status, errno());
        }
        for descriptor in &mut errors {
            if *descriptor < 3 {
                let duplicate = libc::fcntl(*descriptor, libc::F_DUPFD_CLOEXEC, 3);
                if duplicate < 0 {
                    fail(configuration.status, errno());
                }
                libc::close(*descriptor);
                *descriptor = duplicate;
            }
        }
        let root = libc::syscall(
            libc::SYS_clone,
            libc::SIGCHLD,
            0usize,
            0usize,
            0usize,
            0usize,
        );
        if root < 0 {
            fail(configuration.status, errno());
        }
        if root == 0 {
            payload(configuration, errors[1]);
        }
        libc::close(errors[1]);
        let mut error = 0i32;
        let received = loop {
            let result = libc::read(errors[0], (&mut error as *mut i32).cast(), 4);
            if result >= 0 || errno() != libc::EINTR {
                break result;
            }
        };
        libc::close(errors[0]);
        if received != 0 {
            fail(
                configuration.status,
                if received == 4 { error } else { libc::EIO },
            );
        }
        if !message(configuration.status, STARTED, 0) {
            libc::_exit(127);
        }
        let mut observed_root = false;
        loop {
            if configuration.control >= 0 {
                let mut descriptor = libc::pollfd {
                    fd: configuration.control,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let polled = libc::poll(&mut descriptor, 1, 20);
                if polled < 0 && errno() != libc::EINTR {
                    fail(configuration.status, errno());
                }
                if descriptor.revents & libc::POLLIN != 0 {
                    let mut command = 0u8;
                    if libc::read(configuration.control, (&mut command as *mut u8).cast(), 1) == 1
                        && command == 1
                    {
                        libc::kill(-(root as i32), libc::SIGINT);
                    }
                }
            }
            let mut status = 0;
            let wait_options = if configuration.control >= 0 {
                libc::WNOHANG
            } else {
                0
            };
            let waited = libc::waitpid(-1, &mut status, wait_options);
            if waited == root as i32 {
                let code = if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else if libc::WIFSIGNALED(status) {
                    128 + libc::WTERMSIG(status)
                } else {
                    fail(configuration.status, libc::EIO);
                };
                observed_root = true;
                if !message(configuration.status, ROOT_EXIT, code) {
                    libc::_exit(127);
                }
            } else if waited < 0 {
                if errno() == libc::EINTR {
                    continue;
                }
                if errno() == libc::ECHILD && observed_root {
                    message(configuration.status, COMPLETE, 0);
                    libc::_exit(0);
                }
                if errno() != libc::ECHILD {
                    fail(configuration.status, errno());
                }
            }
        }
    }
}
