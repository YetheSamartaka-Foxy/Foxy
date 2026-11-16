mod control;
mod tree;
mod witness;
use super::{CleanupWitnessSpec, CommandSpec, TreeExit};
use control::launch_signal_helper;
pub(super) use control::run_console_signal_helper;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use winapi::ctypes::c_void;
use winapi::shared::minwindef::DWORD;
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::fileapi::OPEN_EXISTING;
use winapi::um::jobapi2::{CreateJobObjectW, SetInformationJobObject, TerminateJobObject};
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::processthreadsapi::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, PROCESS_INFORMATION, ResumeThread,
    UpdateProcThreadAttribute,
};
use winapi::um::securitybaseapi::GetTokenInformation;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::{
    CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    WAIT_FAILED, WAIT_OBJECT_0,
};
use winapi::um::winnt::{
    FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, GENERIC_READ, HANDLE,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SYNCHRONIZE,
    TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
pub(super) use witness::run_cleanup_witness;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreatePipe(
        read: *mut HANDLE,
        write: *mut HANDLE,
        attributes: *mut SECURITY_ATTRIBUTES,
        size: DWORD,
    ) -> i32;
    fn SetHandleInformation(handle: HANDLE, mask: DWORD, flags: DWORD) -> i32;
    fn PeekNamedPipe(
        handle: HANDLE,
        buffer: *mut c_void,
        size: DWORD,
        read: *mut DWORD,
        available: *mut DWORD,
        left: *mut DWORD,
    ) -> i32;
    fn ReadFile(
        handle: HANDLE,
        buffer: *mut c_void,
        size: DWORD,
        read: *mut DWORD,
        overlapped: *mut c_void,
    ) -> i32;
    fn Sleep(milliseconds: DWORD);
}

pub(super) struct CleanupWitnessProcess {
    process: OwnedHandle,
}

impl CleanupWitnessProcess {
    pub(super) fn spawn(
        executable: &Path,
        arguments: &[OsString],
        owner: HANDLE,
        tree: HANDLE,
        receipt: HANDLE,
    ) -> io::Result<Self> {
        unsafe {
            let mut security = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
                lpSecurityDescriptor: null_mut(),
                bInheritHandle: 1,
            };
            let (mut read_raw, mut ready_raw) = (null_mut(), null_mut());
            if CreatePipe(&mut read_raw, &mut ready_raw, &mut security, 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            let read = owned(read_raw)?;
            let ready = owned(ready_raw)?;
            if SetHandleInformation(read.as_raw_handle().cast(), 1, 0) == 0 {
                return Err(io::Error::last_os_error());
            }
            let owner_copy = duplicate_inheritable_with_access(owner, SYNCHRONIZE)?;
            let tree_copy =
                duplicate_inheritable_with_access(tree, JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE)?;
            let receipt_copy = duplicate_inheritable(receipt)?;
            let inherited: Vec<OwnedHandle> = vec![owner_copy, tree_copy, receipt_copy, ready];
            let raw_handles = inherited
                .iter()
                .map(|handle| handle.as_raw_handle().cast())
                .collect::<Vec<HANDLE>>();
            let mut attrs = Attributes::new(None, Some(&raw_handles))?;
            let app = wide(executable.as_os_str())?;
            let mut command = quote(executable.as_os_str())?;
            for arg in arguments {
                command.push(b' ' as u16);
                command.extend(quote(arg)?);
            }
            for handle in &raw_handles {
                command.push(b' ' as u16);
                command.extend((*handle as usize).to_string().encode_utf16());
            }
            command.push(0);
            if command.len() > 32767 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Process command line is too long.",
                ));
            }
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of_val(&startup) as DWORD;
            startup.StartupInfo.dwFlags = STARTF_USESHOWWINDOW;
            startup.StartupInfo.wShowWindow = 0;
            startup.lpAttributeList = attrs.0.as_mut_ptr().cast();
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            if CreateProcessW(
                app.as_ptr(),
                command.as_mut_ptr(),
                null_mut(),
                null_mut(),
                1,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT,
                null_mut(),
                null(),
                &mut startup.StartupInfo,
                &mut info,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let process = owned(info.hProcess)?;
            let thread = owned(info.hThread)?;
            drop(inherited);
            drop(attrs);
            if !unelevated(process.as_raw_handle().cast())? {
                let _ = TerminateProcess(process.as_raw_handle().cast(), 1);
                let _ = WaitForSingleObject(process.as_raw_handle().cast(), u32::MAX);
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Cleanup witness elevation could not be excluded.",
                ));
            }
            if ResumeThread(thread.as_raw_handle().cast()) == u32::MAX {
                let error = io::Error::last_os_error();
                let _ = TerminateProcess(process.as_raw_handle().cast(), 1);
                let _ = WaitForSingleObject(process.as_raw_handle().cast(), u32::MAX);
                return Err(error);
            }
            let ready_result = (|| {
                let start = std::time::Instant::now();
                loop {
                    let mut available = 0;
                    if PeekNamedPipe(
                        read.as_raw_handle().cast(),
                        null_mut(),
                        0,
                        null_mut(),
                        &mut available,
                        null_mut(),
                    ) == 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    if available != 0 {
                        let mut byte = 0u8;
                        let mut count = 0;
                        if ReadFile(
                            read.as_raw_handle().cast(),
                            (&mut byte as *mut u8).cast(),
                            1,
                            &mut count,
                            null_mut(),
                        ) == 0
                        {
                            return Err(io::Error::last_os_error());
                        }
                        if count != 1 || byte != 1 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Cleanup witness did not become ready.",
                            ));
                        }
                        return Ok(());
                    }
                    match WaitForSingleObject(process.as_raw_handle().cast(), 0) {
                        WAIT_OBJECT_0 => {
                            return Err(io::Error::other(
                                "Cleanup witness exited before readiness.",
                            ));
                        }
                        WAIT_FAILED => return Err(io::Error::last_os_error()),
                        _ if start.elapsed() < std::time::Duration::from_secs(5) => Sleep(10),
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "Cleanup witness readiness timed out.",
                            ));
                        }
                    }
                }
            })();
            if let Err(error) = ready_result {
                let _ = TerminateProcess(process.as_raw_handle().cast(), 1);
                let _ = WaitForSingleObject(process.as_raw_handle().cast(), u32::MAX);
                return Err(error);
            }
            Ok(Self { process })
        }
    }

    pub(super) fn completed(&self) -> io::Result<bool> {
        match unsafe { WaitForSingleObject(self.process.as_raw_handle().cast(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("Unexpected cleanup witness wait result.")),
        }
    }
}

const PROC_THREAD_ATTRIBUTE_JOB_LIST: usize = 0x0002_000d;
const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn DuplicateHandle(
        source_process: HANDLE,
        source: HANDLE,
        target_process: HANDLE,
        target: *mut HANDLE,
        access: DWORD,
        inherit: i32,
        options: DWORD,
    ) -> i32;
    fn GetProcessId(process: HANDLE) -> DWORD;
    fn AttachConsole(process_id: DWORD) -> i32;
    fn FreeConsole() -> i32;
    fn GenerateConsoleCtrlEvent(event: DWORD, group: DWORD) -> i32;
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(DWORD) -> i32>,
        add: i32,
    ) -> i32;
    fn TerminateProcess(process: HANDLE, code: u32) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: DWORD,
        share: DWORD,
        security: *mut std::ffi::c_void,
        creation: DWORD,
        flags: DWORD,
        template: HANDLE,
    ) -> HANDLE;
}

pub(super) struct Process {
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
    forced: bool,
    exit: Option<TreeExit>,
    controlled: bool,
    soft_stop_sent: bool,
    witness: Option<CleanupWitnessProcess>,
    tree: tree::JobTreeObservation,
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
    }
}

fn duplicate_inheritable(handle: HANDLE) -> io::Result<OwnedHandle> {
    unsafe {
        let mut duplicate = null_mut();
        if DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            1,
            2,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        owned(duplicate)
    }
}

fn duplicate_inheritable_with_access(handle: HANDLE, access: DWORD) -> io::Result<OwnedHandle> {
    unsafe {
        let mut duplicate = null_mut();
        if DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut duplicate,
            access,
            1,
            0,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        owned(duplicate)
    }
}

fn unelevated(process: HANDLE) -> io::Result<bool> {
    unsafe {
        let mut raw = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut raw) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = owned(raw)?;
        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut returned = 0;
        if GetTokenInformation(
            token.as_raw_handle().cast(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of_val(&elevation) as DWORD,
            &mut returned,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(elevation.TokenIsElevated == 0)
    }
}

pub(super) fn owner_is_unelevated() -> bool {
    unelevated(unsafe { GetCurrentProcess() }).unwrap_or(false)
}

struct Attributes(Vec<usize>);

impl Attributes {
    fn new(job: Option<&mut HANDLE>, handles: Option<&[HANDLE]>) -> io::Result<Self> {
        unsafe {
            let mut size = 0;
            let count = usize::from(job.is_some()) + usize::from(handles.is_some());
            InitializeProcThreadAttributeList(null_mut(), count as DWORD, 0, &mut size);
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let pointer = storage.as_mut_ptr().cast();
            if InitializeProcThreadAttributeList(pointer, count as DWORD, 0, &mut size) == 0 {
                return Err(io::Error::last_os_error());
            }
            let attributes = Self(storage);
            if let Some(job) = job
                && UpdateProcThreadAttribute(
                    pointer,
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST,
                    (job as *mut HANDLE).cast(),
                    std::mem::size_of::<HANDLE>(),
                    null_mut(),
                    null_mut(),
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            if let Some(handles) = handles
                && UpdateProcThreadAttribute(
                    pointer,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                    handles.as_ptr().cast_mut().cast(),
                    std::mem::size_of_val(handles),
                    null_mut(),
                    null_mut(),
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(attributes)
        }
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.0.as_mut_ptr().cast()) };
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut result: Vec<u16> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Process input contains a NUL.",
        ));
    }
    result.push(0);
    Ok(result)
}

fn quote(value: &OsStr) -> io::Result<Vec<u16>> {
    let value = wide(value)?;
    let value = &value[..value.len() - 1];
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for &unit in value {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            b'\\' as u16,
            slashes * if unit == b'"' as u16 { 2 } else { 1 },
        ));
        slashes = 0;
        if unit == b'"' as u16 {
            result.push(b'\\' as u16);
        }
        result.push(unit);
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    Ok(result)
}

fn environment(spec: &CommandSpec) -> io::Result<Vec<u16>> {
    let mut entries: Vec<_> = std::env::vars_os().collect();
    for (key, value) in &spec.environment {
        if key.is_empty() || key.encode_wide().any(|c| c == b'=' as u16 || c == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid environment key.",
            ));
        }
        entries.push((key.clone(), value.clone()));
    }
    encode_environment(entries)
}

fn ordinal(left: &[u16], right: &[u16]) -> io::Result<std::cmp::Ordering> {
    let length = |value: &[u16]| {
        i32::try_from(value.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Environment key is too long.")
        })
    };
    match unsafe {
        winapi::um::stringapiset::CompareStringOrdinal(
            left.as_ptr(),
            length(left)?,
            right.as_ptr(),
            length(right)?,
            1,
        )
    } {
        1 => Ok(std::cmp::Ordering::Less),
        2 => Ok(std::cmp::Ordering::Equal),
        3 => Ok(std::cmp::Ordering::Greater),
        _ => Err(io::Error::last_os_error()),
    }
}

fn encode_environment(
    entries: Vec<(std::ffi::OsString, std::ffi::OsString)>,
) -> io::Result<Vec<u16>> {
    let mut sorted: Vec<(Vec<u16>, Vec<u16>)> = vec![];
    for (key, value) in entries {
        let mut key = wide(&key)?;
        key.pop();
        let value = wide(&value)?;
        let mut insertion = sorted.len();
        let mut duplicate = false;
        for (index, (existing, _)) in sorted.iter().enumerate() {
            match ordinal(&key, existing)? {
                std::cmp::Ordering::Less => {
                    insertion = index;
                    break;
                }
                std::cmp::Ordering::Equal => {
                    insertion = index;
                    duplicate = true;
                    break;
                }
                std::cmp::Ordering::Greater => {}
            }
        }
        if duplicate {
            sorted[insertion] = (key, value);
        } else {
            sorted.insert(insertion, (key, value));
        }
    }
    let mut result = Vec::new();
    for (key, value) in sorted {
        result.extend(key);
        result.push(b'=' as u16);
        result.extend(value);
    }
    result.push(0);
    if result.len() == 1 {
        result.push(0);
    }
    Ok(result)
}

impl Process {
    pub(super) fn spawn(
        spec: &CommandSpec,
        output: Option<&std::fs::File>,
        witness: Option<&CleanupWitnessSpec<'_>>,
    ) -> io::Result<Self> {
        if !owner_is_unelevated() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "A verifiably unelevated owner is required.",
            ));
        }
        let application = wide(spec.executable.as_os_str())?;
        let directory = wide(spec.directory.as_os_str())?;
        let mut command = quote(spec.executable.as_os_str())?;
        for arg in &spec.arguments {
            command.push(b' ' as u16);
            command.extend(quote(arg)?);
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Process command line is too long.",
            ));
        }
        let mut environment = environment(spec)?;
        unsafe {
            let job = owned(CreateJobObjectW(null_mut(), null()))?;
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job.as_raw_handle().cast(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as DWORD,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut raw_job = job.as_raw_handle().cast();
            let mut inherited = Vec::<OwnedHandle>::new();
            if let Some(output) = output {
                inherited.push(duplicate_inheritable(output.as_raw_handle().cast())?);
                let mut security = SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
                    lpSecurityDescriptor: null_mut(),
                    bInheritHandle: 1,
                };
                let null_name: Vec<u16> = "NUL\0".encode_utf16().collect();
                let input = owned(CreateFileW(
                    null_name.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    (&mut security as *mut SECURITY_ATTRIBUTES).cast(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    null_mut(),
                ))?;
                inherited.push(input);
            }
            let raw_handles: Vec<HANDLE> = inherited
                .iter()
                .map(|handle| handle.as_raw_handle().cast())
                .collect();
            let mut attributes = Attributes::new(
                Some(&mut raw_job),
                (!raw_handles.is_empty()).then_some(raw_handles.as_slice()),
            )?;
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of_val(&startup) as DWORD;
            startup.lpAttributeList = attributes.0.as_mut_ptr().cast();
            if output.is_some() {
                startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
                startup.StartupInfo.wShowWindow = 0;
                startup.StartupInfo.hStdInput = raw_handles[1];
                startup.StartupInfo.hStdOutput = raw_handles[0];
                startup.StartupInfo.hStdError = raw_handles[0];
            }
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            if CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null_mut(),
                null_mut(),
                i32::from(output.is_some()),
                CREATE_SUSPENDED
                    | if output.is_some() {
                        CREATE_NEW_CONSOLE
                    } else {
                        CREATE_NO_WINDOW
                    }
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_mut_ptr().cast(),
                directory.as_ptr(),
                &mut startup.StartupInfo,
                &mut info,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let process = owned(info.hProcess)?;
            let thread = owned(info.hThread)?;
            let tree = tree::JobTreeObservation::new(job.as_raw_handle().cast())?;
            let mut result = Self {
                process,
                job,
                pid: info.dwProcessId,
                forced: false,
                exit: None,
                controlled: output.is_some(),
                soft_stop_sent: false,
                witness: None,
                tree,
            };
            if !unelevated(result.process.as_raw_handle().cast())? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Child elevation could not be excluded.",
                ));
            }
            if let Some(witness) = witness {
                result.witness = Some(CleanupWitnessProcess::spawn(
                    &witness.command.executable,
                    &witness.command.arguments,
                    GetCurrentProcess(),
                    result.job.as_raw_handle().cast(),
                    witness.receipt.as_raw_handle().cast(),
                )?);
            }
            if ResumeThread(thread.as_raw_handle().cast()) == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            Ok(result)
        }
    }

    pub(super) fn deliver_soft_stop(
        &mut self,
        helper: Option<&super::ConsoleSignalHelper>,
    ) -> io::Result<()> {
        if self.observe()?.is_some() {
            return Ok(());
        }
        if !self.controlled {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Soft stop requires a controlled console session.",
            ));
        }
        if self.soft_stop_sent {
            return Ok(());
        }
        let helper = helper.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "A console signal helper is required.",
            )
        })?;
        let window_close = launch_signal_helper(helper, self.process.as_raw_handle().cast())?;
        // A startup window can consume WM_CLOSE before the engine installs its shutdown handler.
        self.soft_stop_sent = !window_close;
        Ok(())
    }

    pub(super) fn id(&self) -> u32 {
        self.pid
    }

    pub(super) fn observe(&mut self) -> io::Result<Option<TreeExit>> {
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        unsafe {
            if !self.tree.observe(self.job.as_raw_handle().cast())? {
                return Ok(None);
            }
            if let Some(witness) = &self.witness
                && !witness.completed()?
            {
                return Ok(None);
            }
            match WaitForSingleObject(self.process.as_raw_handle().cast(), 0) {
                WAIT_OBJECT_0 => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => return Ok(None),
            }
            let mut code = 0;
            if GetExitCodeProcess(self.process.as_raw_handle().cast(), &mut code) == 0 {
                return Err(io::Error::last_os_error());
            }
            self.exit = Some(TreeExit {
                root_exit_code: Some(code),
                forced: self.forced,
            });
            Ok(self.exit)
        }
    }

    pub(super) fn terminate(&mut self) -> io::Result<()> {
        if matches!(self.observe(), Ok(Some(_))) {
            return Ok(());
        }
        if unsafe { TerminateJobObject(self.job.as_raw_handle().cast(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.forced = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt;

    #[test]
    fn console_signal_helper_rejects_null_authority() {
        assert_eq!(
            run_console_signal_helper(0, false).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            run_console_signal_helper(0, true).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn quotes_preserve_empty_spaces_quotes_trailing_slashes_and_utf16() {
        for (input, expected) in [
            ("", "\"\""),
            ("alpha beta", "\"alpha beta\""),
            ("a\\", "\"a\\\\\""),
            ("a\"b", "\"a\\\"b\""),
        ] {
            assert_eq!(
                quote(OsStr::new(input)).unwrap(),
                expected.encode_utf16().collect::<Vec<_>>()
            );
        }
        let raw = std::ffi::OsString::from_wide(&[0xd800, 0x0061]);
        assert_eq!(quote(&raw).unwrap(), vec![34, 0xd800, 97, 34]);
    }

    #[test]
    fn command_line_limit_is_rejected_before_creation() {
        let spec = CommandSpec {
            executable: std::env::current_exe().unwrap(),
            directory: std::env::current_dir().unwrap(),
            arguments: vec!["a".repeat(32768).into()],
            environment: vec![],
        };
        assert_eq!(
            Process::spawn(&spec, None, None).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn environment_overrides_use_unicode_ordinal_case_and_preserve_surrogates() {
        let surrogate = |unit| std::ffi::OsString::from_wide(&[b'F' as u16, unit]);
        let block = encode_environment(vec![
            ("FOXY_\u{00c5}".into(), "old".into()),
            ("FOXY_\u{00e5}".into(), "new".into()),
            (surrogate(0xd800), "one".into()),
            (surrogate(0xd801), "two".into()),
        ])
        .unwrap();
        let entries = block
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 3);
        let expected = "FOXY_\u{00e5}=new".encode_utf16().collect::<Vec<_>>();
        assert!(entries.contains(&expected.as_slice()));
        assert!(
            entries
                .iter()
                .any(|entry| entry.starts_with(&[70, 0xd800, 61]))
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.starts_with(&[70, 0xd801, 61]))
        );
        for pair in entries.windows(2) {
            let key = |entry: &[u16]| entry.iter().position(|unit| *unit == 61).unwrap();
            assert_eq!(
                ordinal(&pair[0][..key(pair[0])], &pair[1][..key(pair[1])]).unwrap(),
                std::cmp::Ordering::Less
            );
        }
        assert!(block.ends_with(&[0, 0]));
        assert_eq!(encode_environment(vec![]).unwrap(), vec![0, 0]);
    }
}
