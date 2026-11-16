use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::credentials::Credentials;
use super::protocol::{Action, Failure, Protocol};
use super::{Options, request::Request, terminal};

struct Session(Child);

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn login(options: &Options, credentials: &Credentials) -> io::Result<()> {
    let home = fs::symlink_metadata(&options.home)?;
    let uid = unsafe { libc::getuid() };
    if uid == 0
        || home.is_symlink()
        || !home.is_dir()
        || home.uid() != uid
        || home.mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "Use an ordinary user and an owned private Steam home.",
        ));
    }
    let cache_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(options.home.join("steamcmd-local.lock"))?;
    if unsafe { libc::flock(cache_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::other(
            "Another local helper is using this SteamCMD cache.",
        ));
    }
    let request = if let Some(app) = options.install_app {
        let metadata = fs::symlink_metadata(&options.install_directory)?;
        let target = fs::canonicalize(&options.install_directory)?;
        let home_path = fs::canonicalize(&options.home)?;
        let executable_path = fs::canonicalize(&options.directory)?;
        if !metadata.is_dir()
            || metadata.is_symlink()
            || fs::read_dir(&target)?.next().is_some()
            || target.starts_with(&home_path)
            || home_path.starts_with(&target)
            || target.starts_with(&executable_path)
            || executable_path.starts_with(&target)
        {
            return Err(io::Error::other(
                "Installation requires a separate empty ordinary directory.",
            ));
        }
        Request::install(
            &options.home,
            &target,
            &credentials.username,
            options.cached_only,
            app,
            options.platform,
        )?
    } else {
        Request::login(&options.home, &credentials.username, options.cached_only)?
    };
    let (mut master, slave) = terminal::pair()?;
    let mut command = Command::new(options.directory.join("steamcmd.sh"));
    command
        .current_dir(&options.directory)
        .env("HOME", &options.home)
        .arg("+runscript")
        .arg(request.path())
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut session = Session(command.spawn()?);
    drop(command);
    println!("Starting SteamCMD with private local credentials; vendor output is hidden.");
    let mut protocol = Protocol::default();
    let (mut password_supplied, mut guard_supplied) = (false, false);
    let budget = Duration::from_secs(if options.install_app.is_some() {
        7200
    } else {
        300
    });
    let mut deadline = Instant::now() + budget;
    let result = loop {
        if let Some(status) = session.0.try_wait()? {
            break if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("SteamCMD login request failed."))
            };
        }
        if Instant::now() >= deadline {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SteamCMD login timed out.",
            ));
        }
        let mut poll = libc::pollfd {
            fd: master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, 500) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break Err(error);
        }
        if poll.revents == 0 {
            continue;
        }
        let mut bytes = [0; 4096];
        let count = match master.read(&mut bytes) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Ok(count) => count,
            Err(error)
                if error.raw_os_error() == Some(libc::EIO)
                    || error.kind() == io::ErrorKind::Interrupted =>
            {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => break Err(error),
        };
        match protocol.feed(&bytes[..count]) {
            Some(Action::Password) => {
                if options.cached_only {
                    break Err(io::Error::other(
                        "Cached sign-in needs password reauthentication.",
                    ));
                }
                writeln!(master, "{}", credentials.password)?;
                master.flush()?;
                password_supplied = true;
                println!("Password submitted privately through the terminal prompt.");
            }
            Some(Action::GuardCode) => {
                if options.cached_only {
                    break Err(io::Error::other(
                        "Cached sign-in needs Steam Guard reauthentication.",
                    ));
                }
                println!(
                    "STEAM_GUARD_REQUIRED: enter the email or authenticator code; input is hidden."
                );
                io::stdout().flush()?;
                let code = terminal::guard_code()?;
                writeln!(master, "{code}")?;
                master.flush()?;
                guard_supplied = true;
                deadline = Instant::now() + budget;
                println!("Steam Guard code submitted privately.");
            }
            Some(Action::MobileApproval) => {
                if options.cached_only {
                    break Err(io::Error::other(
                        "Cached sign-in needs Steam Guard approval.",
                    ));
                }
                guard_supplied = true;
                println!("STEAM_GUARD_APPROVAL_REQUIRED: approve the sign-in in the Steam app.");
            }
            Some(Action::Failed(failure)) => {
                let message = match failure {
                    Failure::Password => "SteamCMD rejected the password.",
                    Failure::Guard => "SteamCMD rejected or expired the Steam Guard code.",
                    Failure::Login => "SteamCMD reported a login failure.",
                };
                break Err(io::Error::other(message));
            }
            None => {}
        }
    };
    drop(master);
    drop(session);
    if result.is_ok() {
        println!("SteamCMD authenticated and exited cleanly; the reusable cache is retained.");
        if !password_supplied && !guard_supplied {
            println!("CACHED_LOGIN_VERIFIED: no password or Steam Guard code was needed.");
        }
        if options.install_app.is_some() {
            println!(
                "APP_INSTALL_VALIDATED: the requested {} public package was installed and validated by SteamCMD.",
                options.platform.steamcmd_name()
            );
        }
    }
    result
}
