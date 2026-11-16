use super::*;

const WINDOW_CLOSE_POSTED: u32 = 10;

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(
        callback: unsafe extern "system" fn(*mut c_void, isize) -> i32,
        parameter: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(window: *mut c_void, process_id: *mut u32) -> u32;
    fn PostMessageW(window: *mut c_void, message: u32, wparam: usize, lparam: isize) -> i32;
}

struct WindowClose {
    process: HANDLE,
    pid: u32,
    sent: bool,
    error: Option<i32>,
}

unsafe extern "system" fn close_owned_window(window: *mut c_void, parameter: isize) -> i32 {
    let request = unsafe { &mut *(parameter as *mut WindowClose) };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if pid == request.pid && unsafe { WaitForSingleObject(request.process, 0) } == WAIT_TIMEOUT {
        if unsafe { PostMessageW(window, 0x0010, 0, 0) } != 0 {
            request.sent = true;
        } else {
            request.error = io::Error::last_os_error().raw_os_error();
        }
    }
    1
}

fn request_window_close(process: HANDLE, pid: u32) -> io::Result<()> {
    let mut request = WindowClose {
        process,
        pid,
        sent: false,
        error: None,
    };
    if unsafe {
        EnumWindows(
            close_owned_window,
            (&mut request as *mut WindowClose) as isize,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if request.sent || unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(request
            .error
            .map(io::Error::from_raw_os_error)
            .unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Owned process has no shutdown window",
                )
            }))
    }
}

pub(super) fn launch_signal_helper(
    helper: &crate::process::ConsoleSignalHelper,
    target: HANDLE,
) -> io::Result<bool> {
    if !helper.executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Helper path must be absolute.",
        ));
    }
    let target_inheritable = duplicate_inheritable(target)?;
    let inherited = [target_inheritable.as_raw_handle().cast::<c_void>()];
    let mut attributes = Attributes::new(None, Some(&inherited))?;
    let application = wide(helper.executable.as_os_str())?;
    let mut command = quote(helper.executable.as_os_str())?;
    for argument in &helper.arguments {
        command.push(b' ' as u16);
        command.extend(quote(argument)?);
    }
    command.push(b' ' as u16);
    command.extend(quote(OsStr::new(&format!(
        "{}",
        target_inheritable.as_raw_handle() as usize
    )))?);
    command.push(0);
    if command.len() > 32767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Helper command line is too long.",
        ));
    }
    let mut env = encode_environment(std::env::vars_os().collect())?;
    unsafe {
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of_val(&startup) as DWORD;
        startup.StartupInfo.dwFlags |= STARTF_USESHOWWINDOW;
        startup.StartupInfo.wShowWindow = 0;
        startup.lpAttributeList = attributes.0.as_mut_ptr().cast();
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            null_mut(),
            null_mut(),
            1,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            env.as_mut_ptr().cast(),
            null(),
            &mut startup.StartupInfo,
            &mut info,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let process = owned(info.hProcess)?;
        let thread = match owned(info.hThread) {
            Ok(thread) => thread,
            Err(error) => {
                terminate_helper_and_wait(&process)?;
                return Err(error);
            }
        };
        drop(thread);
        drop(target_inheritable);
        match WaitForSingleObject(process.as_raw_handle().cast(), 5000) {
            WAIT_OBJECT_0 => {
                let mut code = 0;
                if GetExitCodeProcess(process.as_raw_handle().cast(), &mut code) == 0 {
                    return Err(io::Error::last_os_error());
                }
                if code == 0 || code == WINDOW_CLOSE_POSTED {
                    Ok(code == WINDOW_CLOSE_POSTED)
                } else if (1001..=66535).contains(&code) {
                    Err(io::Error::from_raw_os_error((code - 1000) as i32))
                } else {
                    Err(io::Error::other("Console signal helper failed."))
                }
            }
            WAIT_TIMEOUT => {
                terminate_helper_and_wait(&process)?;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Console signal helper timed out.",
                ))
            }
            WAIT_FAILED => {
                let wait_error = io::Error::last_os_error();
                terminate_helper_and_wait(&process)?;
                Err(wait_error)
            }
            _ => Err(io::Error::other(
                "Unexpected console signal helper wait result.",
            )),
        }
    }
}

fn terminate_helper_and_wait(process: &OwnedHandle) -> io::Result<()> {
    unsafe {
        let termination_error = (TerminateProcess(process.as_raw_handle().cast(), 1) == 0)
            .then(io::Error::last_os_error);
        match WaitForSingleObject(process.as_raw_handle().cast(), 5000) {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                termination_error.map_or_else(
                    || "Console signal helper termination was not observed.".to_owned(),
                    |error| format!("Console signal helper termination was not observed: {error}"),
                ),
            )),
            WAIT_FAILED => Err(io::Error::other(format!(
                "Could not verify console signal helper termination: {}",
                io::Error::last_os_error()
            ))),
            _ => Err(io::Error::other(
                "Unexpected console signal helper termination wait result.",
            )),
        }
    }
}

unsafe extern "system" fn ignore_interrupt(event: DWORD) -> i32 {
    i32::from(event == 0 || event == 1)
}

pub(in crate::process) fn run_console_signal_helper(raw: u64, ctrl_break: bool) -> io::Result<i32> {
    if raw == 0 || raw > usize::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid inherited process handle.",
        ));
    }
    let process = raw as usize as HANDLE;
    unsafe {
        let pid = GetProcessId(process);
        if pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Inherited process handle is invalid.",
            ));
        }
        match WaitForSingleObject(process, 0) {
            WAIT_OBJECT_0 => return Ok(0),
            WAIT_FAILED => return Err(io::Error::last_os_error()),
            _ => {}
        }
        FreeConsole();
        if AttachConsole(pid) == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(6) {
                request_window_close(process, pid).map(|()| WINDOW_CLOSE_POSTED as i32)
            } else {
                Err(error)
            };
        }
        let handler = if ctrl_break {
            Some(ignore_interrupt as unsafe extern "system" fn(DWORD) -> i32)
        } else {
            None
        };
        if SetConsoleCtrlHandler(handler, 1) == 0 {
            let error = io::Error::last_os_error();
            FreeConsole();
            return Err(error);
        }
        match WaitForSingleObject(process, 0) {
            WAIT_TIMEOUT => {}
            WAIT_OBJECT_0 => {
                FreeConsole();
                return Ok(0);
            }
            _ => {
                let error = io::Error::last_os_error();
                FreeConsole();
                return Err(error);
            }
        }
        let result = if GenerateConsoleCtrlEvent(u32::from(ctrl_break), 0) == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        };
        // Detaching would reset handlers before the queued Break reaches this helper.
        if !ctrl_break {
            FreeConsole();
        }
        result.map(|()| 0)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.exit.is_none() {
            let _ = self.terminate();
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_secs(5) {
                match self.observe() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        }
    }
}
