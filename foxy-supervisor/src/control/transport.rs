//! Synchronous I/O for bounded owner workers, never for an async executor or UI thread.

use std::{
    io,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

#[cfg(target_os = "linux")]
#[path = "transport/linux.rs"]
mod native;
#[cfg(windows)]
#[path = "transport/windows.rs"]
mod native;
#[cfg(not(any(target_os = "linux", windows)))]
#[path = "transport/unsupported.rs"]
mod native;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalEndpoint {
    NamedPipe(String),
    UnixSocket(PathBuf),
}

#[derive(Clone, Copy)]
pub struct IoBudget<'a> {
    pub deadline: Instant,
    pub cancelled: &'a AtomicBool,
}

impl IoBudget<'_> {
    pub fn check(self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "control I/O cancelled",
            ))
        } else if Instant::now() >= self.deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control I/O deadline",
            ))
        } else {
            Ok(())
        }
    }

    pub fn remaining(self) -> io::Result<Duration> {
        self.check()?;
        Ok(self.deadline.saturating_duration_since(Instant::now()))
    }
}

pub struct Listener(native::Listener);
pub struct Connection<'a>(native::Connection<'a>);

impl Listener {
    pub fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
        native::Listener::bind(endpoint).map(Self)
    }

    pub fn accept(&mut self, budget: IoBudget<'_>) -> io::Result<Connection<'_>> {
        self.0.accept(budget).map(Connection)
    }
}

pub fn connect<'a>(endpoint: &LocalEndpoint, budget: IoBudget<'_>) -> io::Result<Connection<'a>> {
    native::connect(endpoint, budget).map(Connection)
}

impl Connection<'_> {
    pub fn read_exact(&mut self, bytes: &mut [u8], budget: IoBudget<'_>) -> io::Result<()> {
        self.0.read_exact(bytes, budget)
    }

    pub fn write_all(&mut self, bytes: &[u8], budget: IoBudget<'_>) -> io::Result<()> {
        self.0.write_all(bytes, budget)
    }
}
