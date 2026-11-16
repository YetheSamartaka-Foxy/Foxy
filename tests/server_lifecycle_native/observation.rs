use std::io;
use std::time::Duration;

pub(super) struct RootProcess {
    #[cfg(windows)]
    handle: *mut std::ffi::c_void,
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
}

impl RootProcess {
    pub(super) fn observe(pid: u32) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::ffi::c_void;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
                fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
                fn GetLastError() -> u32;
                fn CloseHandle(handle: *mut c_void) -> i32;
            }
            let handle = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
            if handle.is_null() {
                return Err(io::Error::other(format!(
                    "Could not retain observation handle for native root process {pid}: {}",
                    unsafe { GetLastError() }
                )));
            }
            let state = unsafe { WaitForSingleObject(handle, 0) };
            if state != 0x102 {
                unsafe { CloseHandle(handle) };
                return Err(io::Error::other(format!(
                    "Native root process {pid} was not live when observed (wait={state:#x})"
                )));
            }
            Ok(Self { handle })
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
            if raw_fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };
            let process = Self { fd };
            if process.exited()? {
                return Err(io::Error::other(format!(
                    "Native root process {pid} was not live when observed"
                )));
            }
            Ok(process)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = pid;
            Err(io::Error::other(
                "Native root process observation is unsupported on this platform",
            ))
        }
    }

    pub(super) fn wait_exited(&self, timeout: Duration) -> io::Result<bool> {
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn WaitForSingleObject(handle: *mut std::ffi::c_void, milliseconds: u32) -> u32;
            }
            let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
            match unsafe { WaitForSingleObject(self.handle, millis) } {
                0 => Ok(true),
                0x102 => Ok(false),
                state => Err(io::Error::other(format!(
                    "Native root process wait failed (wait={state:#x})"
                ))),
            }
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut descriptor = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
            let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(result > 0 && descriptor.revents & libc::POLLIN != 0)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = timeout;
            Err(io::Error::other(
                "Native root process observation is unsupported on this platform",
            ))
        }
    }

    #[cfg(target_os = "linux")]
    fn exited(&self) -> io::Result<bool> {
        self.wait_exited(Duration::ZERO)
    }
}

#[cfg(windows)]
impl Drop for RootProcess {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        unsafe { CloseHandle(self.handle) };
    }
}
