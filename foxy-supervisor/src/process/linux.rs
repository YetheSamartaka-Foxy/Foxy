use super::{CleanupWitnessSpec, CommandSpec, ProcessSupport, TreeExit};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod child;
mod input;
mod witness;

const START_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
#[repr(C)]
struct CloneArguments {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

pub(super) fn support() -> ProcessSupport {
    let reason = if input::owner_identity().is_err() {
        Some("An unprivileged same-user owner is required.")
    } else if std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .is_none_or(|value| value == 0)
    {
        Some("Unprivileged user namespaces are unavailable.")
    } else {
        unsafe {
            libc::syscall(
                libc::SYS_clone3,
                std::ptr::null::<CloneArguments>(),
                std::mem::size_of::<CloneArguments>(),
            );
        }
        if io::Error::last_os_error().raw_os_error() != Some(libc::EFAULT) {
            Some("The required clone3 namespace API is unavailable.")
        } else {
            None
        }
    };
    ProcessSupport {
        backend: "linux_pid_namespace_init",
        available: reason.is_none(),
        owner_death_policy: "forced_tree_termination_save_uncertain",
        reason,
    }
}

pub(super) use witness::run_cleanup_witness;

pub(super) struct Process {
    pid: u32,
    pidfd: OwnedFd,
    completion: Receiver<io::Result<TreeExit>>,
    creator: Option<JoinHandle<()>>,
    forced: bool,
    exit: Option<TreeExit>,
    soft_stop: Option<std::fs::File>,
    soft_stop_sent: bool,
}

struct Started {
    pid: u32,
    pidfd: OwnedFd,
    soft_stop: Option<std::fs::File>,
}

struct Namespace {
    pidfd: OwnedFd,
    waited: bool,
}

fn owned(raw: i32) -> io::Result<OwnedFd> {
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
    if raw >= 3 {
        return Ok(descriptor);
    }
    let duplicate = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut raw = [-1; 2];
    if unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let first = owned(raw[0]);
    let second = owned(raw[1]);
    Ok((first?, second?))
}

fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut raw = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            raw.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((owned(raw[0])?, owned(raw[1])?))
}

fn signal(pidfd: &OwnedFd) -> io::Result<()> {
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            libc::SIGKILL,
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
    Ok(())
}

fn readable(fd: RawFd, timeout: Duration) -> io::Result<()> {
    let start = std::time::Instant::now();
    loop {
        let remaining = timeout.saturating_sub(start.elapsed());
        let mut descriptor = libc::pollfd {
            fd,
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
        if result > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Namespace startup timed out.",
            ));
        }
        let error = io::Error::last_os_error();
        if result > 0 || error.kind() != io::ErrorKind::Interrupted {
            return Err(if result > 0 {
                io::Error::other("Namespace status descriptor became unavailable.")
            } else {
                error
            });
        }
    }
}

fn message(file: &mut std::fs::File) -> io::Result<child::Message> {
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    Ok(child::Message {
        kind: i32::from_ne_bytes(bytes[..4].try_into().unwrap()),
        value: i32::from_ne_bytes(bytes[4..].try_into().unwrap()),
    })
}

impl Namespace {
    fn wait(&mut self) -> io::Result<bool> {
        loop {
            let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    self.pidfd.as_raw_fd() as libc::id_t,
                    &mut information,
                    libc::WEXITED,
                )
            } == 0
            {
                self.waited = true;
                return Ok(information.si_code == libc::CLD_EXITED
                    && unsafe { information.si_status() } == 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        if !self.waited {
            let _ = signal(&self.pidfd);
            while self.wait().is_err() {
                // Creator-thread death must follow the kernel's namespace teardown.
                let mut descriptor = libc::pollfd {
                    fd: self.pidfd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut descriptor, 1, 100) } > 0
                    && descriptor.revents & libc::POLLIN != 0
                {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn create(
    prepared: input::Prepared,
    output: Option<OwnedFd>,
    witness: Option<witness::PreparedWitness>,
    startup: SyncSender<io::Result<Started>>,
    complete: SyncSender<io::Result<TreeExit>>,
) {
    let result = create_inner(prepared, output, witness, &startup);
    match result {
        Ok(exit) => {
            let _ = complete.send(Ok(exit));
        }
        Err(error) => {
            let code = error.raw_os_error();
            let kind = error.kind();
            let _ = startup.send(Err(error));
            let _ = complete.send(Err(code.map_or_else(
                || io::Error::new(kind, "Namespace observation failed."),
                io::Error::from_raw_os_error,
            )));
        }
    }
}

fn create_inner(
    prepared: input::Prepared,
    output: Option<OwnedFd>,
    witness: Option<witness::PreparedWitness>,
    startup: &SyncSender<io::Result<Started>>,
) -> io::Result<TreeExit> {
    let (gate_read, gate_write) = pipe()?;
    let (status_read, status_write) = pipe()?;
    let (control_read, control_write) = if output.is_some() {
        let (reader, writer) = socket_pair()?;
        (Some(reader), Some(writer))
    } else {
        (None, None)
    };
    let null_input = if output.is_some() {
        Some(owned(unsafe {
            libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC)
        })?)
    } else {
        None
    };
    let owner = owned(unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0u32) } as i32)?;
    let mut arguments: Vec<_> = prepared
        .arguments
        .iter()
        .map(|value| value.as_ptr())
        .collect();
    arguments.push(std::ptr::null());
    let mut environment: Vec<_> = prepared
        .environment
        .iter()
        .map(|value| value.as_ptr())
        .collect();
    environment.push(std::ptr::null());
    let mut retained = [
        gate_read.as_raw_fd(),
        status_write.as_raw_fd(),
        owner.as_raw_fd(),
        control_read.as_ref().map_or(-1, AsRawFd::as_raw_fd),
        output.as_ref().map_or(-1, AsRawFd::as_raw_fd),
        null_input.as_ref().map_or(-1, AsRawFd::as_raw_fd),
    ];
    retained.sort_unstable();
    let configuration = child::Configuration {
        prepared: &prepared,
        arguments: &arguments,
        environment: &environment,
        gate: gate_read.as_raw_fd(),
        status: status_write.as_raw_fd(),
        owner: owner.as_raw_fd(),
        retained,
        control: control_read.as_ref().map_or(-1, AsRawFd::as_raw_fd),
        output: output.as_ref().map_or(-1, AsRawFd::as_raw_fd),
        null_input: null_input.as_ref().map_or(-1, AsRawFd::as_raw_fd),
    };
    let mut raw_pidfd = -1i32;
    let clone = CloneArguments {
        flags: (libc::CLONE_NEWUSER | libc::CLONE_NEWPID | libc::CLONE_PIDFD) as u64,
        pidfd: (&mut raw_pidfd as *mut i32) as u64,
        exit_signal: libc::SIGCHLD as u64,
        ..CloneArguments::default()
    };
    let pid = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &clone as *const CloneArguments,
            std::mem::size_of::<CloneArguments>(),
        )
    };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        unsafe { child::run(&configuration) };
    }
    let mut namespace = Namespace {
        pidfd: unsafe { OwnedFd::from_raw_fd(raw_pidfd) },
        waited: false,
    };
    drop(gate_read);
    drop(status_write);
    drop(control_read);
    let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
    std::fs::write(proc.join("setgroups"), b"deny")?;
    std::fs::write(
        proc.join("uid_map"),
        format!("{} {} 1\n", prepared.uid, prepared.uid),
    )?;
    std::fs::write(
        proc.join("gid_map"),
        format!("{} {} 1\n", prepared.gid, prepared.gid),
    )?;
    let mut witness_process = witness
        .map(|prepared| witness::CleanupWitnessProcess::spawn(prepared, &owner, &namespace.pidfd))
        .transpose()?;
    let mut gate = std::fs::File::from(gate_write);
    gate.write_all(&[1])?;
    drop(owner);
    drop(gate);
    let mut status = std::fs::File::from(status_read);
    readable(status.as_raw_fd(), START_TIMEOUT)?;
    let first = message(&mut status)?;
    if first.kind != child::STARTED {
        return Err(if first.kind == child::FAILED {
            io::Error::from_raw_os_error(first.value)
        } else {
            io::Error::other("Invalid namespace startup status.")
        });
    }
    let observer = namespace.pidfd.try_clone()?;
    let stop_file = control_write.map(std::fs::File::from);
    if startup
        .send(Ok(Started {
            pid: pid as u32,
            pidfd: observer,
            soft_stop: stop_file,
        }))
        .is_err()
    {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Namespace start was abandoned.",
        ));
    }
    let clean = namespace.wait()?;
    let _witness_completed = match witness_process.as_mut() {
        Some(witness) => {
            let start = std::time::Instant::now();
            loop {
                if let Some(success) = witness.completed()? {
                    break success;
                }
                if start.elapsed() >= Duration::from_secs(35) {
                    break false;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        None => true,
    };
    let mut root_exit_code = None;
    let mut completed = false;
    while let Ok(message) = message(&mut status) {
        match message.kind {
            child::ROOT_EXIT if root_exit_code.is_none() && (0..=255).contains(&message.value) => {
                root_exit_code = Some(message.value as u32);
            }
            child::COMPLETE if root_exit_code.is_some() && message.value == 0 => completed = true,
            _ => return Err(io::Error::other("Invalid namespace completion status.")),
        }
    }
    Ok(TreeExit {
        root_exit_code,
        forced: !clean || !completed,
    })
}

impl Process {
    pub(super) fn spawn(
        spec: &CommandSpec,
        output: Option<&std::fs::File>,
        witness: Option<&CleanupWitnessSpec<'_>>,
    ) -> io::Result<Self> {
        let prepared = input::Prepared::new(spec)?;
        let witness = witness.map(witness::PreparedWitness::new).transpose()?;
        let output = output
            .map(std::fs::File::try_clone)
            .transpose()?
            .map(|file| owned(file.into_raw_fd()))
            .transpose()?;
        let (startup_tx, startup) = mpsc::sync_channel(1);
        let (complete_tx, completion) = mpsc::sync_channel(1);
        let creator = thread::Builder::new()
            .name("foxy-process-owner".into())
            .spawn(move || {
                create(prepared, output, witness, startup_tx, complete_tx);
            })?;
        let started = startup
            .recv()
            .map_err(|_| io::Error::other("Namespace creator stopped."))??;
        Ok(Self {
            pid: started.pid,
            pidfd: started.pidfd,
            completion,
            creator: Some(creator),
            forced: false,
            exit: None,
            soft_stop: started.soft_stop,
            soft_stop_sent: false,
        })
    }

    pub(super) fn deliver_soft_stop(&mut self) -> io::Result<()> {
        if self.observe()?.is_some() {
            return Ok(());
        }
        if self.soft_stop_sent {
            return Ok(());
        }
        let Some(control) = &mut self.soft_stop else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Soft stop requires a controlled session.",
            ));
        };
        control.write_all(&[1])?;
        self.soft_stop_sent = true;
        Ok(())
    }

    pub(super) fn id(&self) -> u32 {
        self.pid
    }

    pub(super) fn observe(&mut self) -> io::Result<Option<TreeExit>> {
        if let Some(exit) = self.exit {
            return Ok(Some(exit));
        }
        match self.completion.try_recv() {
            Ok(result) => {
                let mut exit = result?;
                exit.forced |= self.forced;
                self.exit = Some(exit);
                if let Some(creator) = self.creator.take() {
                    creator
                        .join()
                        .map_err(|_| io::Error::other("Namespace creator stopped."))?;
                }
                Ok(Some(exit))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(io::Error::other(
                "Namespace observation became unavailable.",
            )),
        }
    }

    pub(super) fn terminate(&mut self) -> io::Result<()> {
        if self.observe()?.is_none() {
            signal(&self.pidfd)?;
            self.forced = true;
        }
        Ok(())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.exit.is_none() {
            let _ = signal(&self.pidfd);
            if self.completion.recv_timeout(Duration::from_secs(5)).is_ok()
                && let Some(creator) = self.creator.take()
            {
                let _ = creator.join();
            }
        }
    }
}

#[cfg(test)]
mod tests;
