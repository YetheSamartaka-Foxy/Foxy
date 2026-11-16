use std::fs::File;
use std::io::{self, BufRead, IsTerminal, Read};
use std::os::fd::{FromRawFd, RawFd};

fn attributes(fd: RawFd) -> io::Result<libc::termios> {
    let mut attributes = std::mem::MaybeUninit::uninit();
    if unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { attributes.assume_init() })
}

pub(super) fn pair() -> io::Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    use std::os::fd::AsRawFd;
    let mut attributes = attributes(slave.as_raw_fd())?;
    attributes.c_lflag &= !(libc::ECHO | libc::ECHONL);
    if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attributes) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((master, slave))
}

struct EchoGuard(libc::termios);

impl Drop for EchoGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.0) };
    }
}

pub(super) fn guard_code() -> io::Result<String> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(
            "Steam Guard input requires an interactive terminal.",
        ));
    }
    let previous = attributes(0)?;
    let mut hidden = previous;
    hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
    if unsafe { libc::tcsetattr(0, libc::TCSANOW, &hidden) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let _echo = EchoGuard(previous);
    let mut code = String::new();
    io::stdin().lock().take(65).read_line(&mut code)?;
    let code = code.trim();
    if !(4..=16).contains(&code.len()) || !code.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(io::Error::other("Invalid Steam Guard code format."));
    }
    Ok(code.to_owned())
}

#[test]
fn private_terminal_has_close_on_exec_descriptors_and_never_echoes_input() -> io::Result<()> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let (mut master, mut slave) = pair()?;
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        assert_ne!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
    master.write_all(b"synthetic_private_input\n")?;
    let mut buffer = [0; 128];
    let count = slave.read(&mut buffer)?;
    assert_eq!(&buffer[..count], b"synthetic_private_input\n");
    let mut poll = libc::pollfd {
        fd: master.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 20) }, 0);
    slave.write_all(b"fixture_response\n")?;
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 1000) }, 1);
    let count = master.read(&mut buffer)?;
    assert!(!String::from_utf8_lossy(&buffer[..count]).contains("synthetic_private_input"));
    Ok(())
}
