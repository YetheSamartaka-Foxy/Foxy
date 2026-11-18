use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

pub(in crate::cli::commands::server) fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}

#[cfg(test)]
pub(in crate::cli::commands::server) fn request_for_test() {
    REQUESTED.store(true, Ordering::Release);
}

pub(in crate::cli::commands::server) struct SignalGuard {
    #[cfg(windows)]
    installed: bool,
    #[cfg(unix)]
    previous: [libc::sigaction; 2],
}

#[cfg(windows)]
unsafe extern "system" fn handler(event: u32) -> i32 {
    if event == 0 || event == 1 {
        REQUESTED.store(true, Ordering::Release);
        1
    } else {
        0
    }
}

#[cfg(unix)]
extern "C" fn handler(_: i32) {
    REQUESTED.store(true, Ordering::Release);
}

impl SignalGuard {
    pub(in crate::cli::commands::server) fn install() -> Result<Self, ()> {
        REQUESTED.store(false, Ordering::Release);
        #[cfg(windows)]
        {
            let installed =
                unsafe { winapi::um::consoleapi::SetConsoleCtrlHandler(Some(handler), 1) } != 0;
            if !installed && std::io::Error::last_os_error().raw_os_error() != Some(6) {
                return Err(());
            }
            Ok(Self { installed })
        }
        #[cfg(unix)]
        {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous: [libc::sigaction; 2] = unsafe { std::mem::zeroed() };
            action.sa_sigaction = handler as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous[0]) } != 0 {
                return Err(());
            }
            if unsafe { libc::sigaction(libc::SIGTERM, &action, &mut previous[1]) } != 0 {
                unsafe { libc::sigaction(libc::SIGINT, &previous[0], std::ptr::null_mut()) };
                return Err(());
            }
            Ok(Self { previous })
        }
        #[cfg(not(any(windows, unix)))]
        {
            Ok(Self {})
        }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.installed {
            unsafe { winapi::um::consoleapi::SetConsoleCtrlHandler(Some(handler), 0) };
        }
        #[cfg(unix)]
        unsafe {
            libc::sigaction(libc::SIGTERM, &self.previous[1], std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &self.previous[0], std::ptr::null_mut())
        };
    }
}
