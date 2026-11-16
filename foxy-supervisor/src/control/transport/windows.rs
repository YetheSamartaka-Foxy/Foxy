use super::{IoBudget, LocalEndpoint};
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::time::Duration;
use winapi::shared::minwindef::{DWORD, FALSE, TRUE};
use winapi::shared::winerror::WAIT_TIMEOUT;
use winapi::um::accctrl::SE_KERNEL_OBJECT;
use winapi::um::aclapi::GetSecurityInfo;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::ioapiset::{CancelIoEx, GetOverlappedResult};
use winapi::um::minwinbase::{OVERLAPPED, SECURITY_ATTRIBUTES};
use winapi::um::namedpipeapi::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, ImpersonateNamedPipeClient,
};
use winapi::um::processthreadsapi::{GetCurrentProcess, GetCurrentThread, OpenThreadToken};
use winapi::um::securitybaseapi::{
    AddAccessAllowedAce, EqualSid, GetLengthSid, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, RevertToSelf, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner,
};
use winapi::um::synchapi::{CreateEventW, WaitForSingleObject};
use winapi::um::winbase::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, LocalFree, PIPE_ACCESS_DUPLEX,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, WAIT_OBJECT_0,
};
use winapi::um::winnt::{
    ACL, ACL_REVISION, GENERIC_READ, GENERIC_WRITE, HANDLE, OWNER_SECURITY_INFORMATION,
    READ_CONTROL, SECURITY_DESCRIPTOR_MIN_LENGTH, SECURITY_DESCRIPTOR_REVISION, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};

const POLL_SLICE: Duration = Duration::from_millis(10);
const MAX_CHUNK: usize = 64 * 1024;
const PIPE_PREFIX: &str = r"\\.\pipe\foxy-control-";

pub(super) struct Listener {
    handle: OwnedHandle,
}

pub(super) struct Connection<'a> {
    handle: Handle<'a>,
}

enum Handle<'a> {
    Server(&'a mut Listener),
    Client(OwnedHandle),
}

impl Handle<'_> {
    fn raw(&self) -> HANDLE {
        match self {
            Self::Server(listener) => listener.handle.as_raw_handle().cast(),
            Self::Client(handle) => handle.as_raw_handle().cast(),
        }
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Handle::Server(listener) = &mut self.handle {
            unsafe { DisconnectNamedPipe(listener.handle.as_raw_handle().cast()) };
        }
    }
}

fn win_error() -> io::Error {
    io::Error::last_os_error()
}

fn owned(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(win_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
    }
}

fn pipe_name(endpoint: &LocalEndpoint) -> io::Result<Vec<u16>> {
    let LocalEndpoint::NamedPipe(name) = endpoint else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsupported local endpoint.",
        ));
    };
    let suffix = name
        .strip_prefix(PIPE_PREFIX)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid local endpoint."))?;
    if suffix.is_empty()
        || suffix.len() > 160
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid local endpoint.",
        ));
    }
    Ok(OsStr::new(name).encode_wide().chain(Some(0)).collect())
}

struct UserSecurity {
    descriptor: Vec<u64>,
    acl: Vec<u64>,
    _token_user: Vec<u64>,
    attributes: SECURITY_ATTRIBUTES,
}

fn token_user(token: HANDLE) -> io::Result<Vec<u64>> {
    unsafe {
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(win_error());
        }
        let mut storage = vec![0u64; (needed as usize).div_ceil(8)];
        if GetTokenInformation(
            token,
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        ) == 0
        {
            return Err(win_error());
        }
        Ok(storage)
    }
}

fn user_security() -> io::Result<UserSecurity> {
    unsafe {
        let mut token_raw = null_mut();
        if winapi::um::processthreadsapi::OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token_raw,
        ) == 0
        {
            return Err(win_error());
        }
        let token = owned(token_raw)?;
        let token_user = token_user(token.as_raw_handle().cast())?;
        let sid = (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let sid_len = GetLengthSid(sid) as usize;
        let acl_len = std::mem::size_of::<ACL>()
            + std::mem::size_of::<winapi::um::winnt::ACCESS_ALLOWED_ACE>()
            - std::mem::size_of::<DWORD>()
            + sid_len;
        let mut acl = vec![0u64; acl_len.div_ceil(8)];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        if InitializeAcl(acl_ptr, acl_len as DWORD, ACL_REVISION as DWORD) == 0
            || AddAccessAllowedAce(
                acl_ptr,
                ACL_REVISION as DWORD,
                GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                sid,
            ) == 0
        {
            return Err(win_error());
        }
        let mut descriptor = vec![0u64; SECURITY_DESCRIPTOR_MIN_LENGTH.div_ceil(8)];
        let descriptor_ptr = descriptor.as_mut_ptr().cast();
        if InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) == 0
            || SetSecurityDescriptorOwner(descriptor_ptr, sid, FALSE) == 0
            || SetSecurityDescriptorDacl(descriptor_ptr, TRUE, acl_ptr, FALSE) == 0
        {
            return Err(win_error());
        }
        let mut attributes = std::mem::zeroed::<SECURITY_ATTRIBUTES>();
        attributes.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD;
        attributes.lpSecurityDescriptor = descriptor_ptr;
        attributes.bInheritHandle = FALSE;
        Ok(UserSecurity {
            descriptor,
            acl,
            _token_user: token_user,
            attributes,
        })
    }
}

fn verify_current_user_token(token: HANDLE) -> io::Result<()> {
    let expected_token = unsafe {
        let mut raw = null_mut();
        if winapi::um::processthreadsapi::OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut raw,
        ) == 0
        {
            return Err(win_error());
        }
        owned(raw)?
    };
    let expected = token_user(expected_token.as_raw_handle().cast())?;
    let actual = token_user(token)?;
    let expected_sid = unsafe { (*expected.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let actual_sid = unsafe { (*actual.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    if unsafe { EqualSid(expected_sid, actual_sid) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Local peer rejected.",
        ));
    }
    Ok(())
}

fn authenticate_client(pipe: HANDLE) -> io::Result<()> {
    unsafe {
        if ImpersonateNamedPipeClient(pipe) == 0 {
            return Err(win_error());
        }
        let mut token = null_mut();
        let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &mut token) != 0;
        let open_error = (!opened).then(win_error);
        let reverted = RevertToSelf() != 0;
        if !reverted {
            std::process::abort();
        }
        if let Some(error) = open_error {
            return Err(error);
        }
        let token = owned(token)?;
        verify_current_user_token(token.as_raw_handle().cast())
    }
}

fn wait_event(event: HANDLE, budget: IoBudget<'_>) -> io::Result<()> {
    loop {
        budget.check()?;
        let millis = budget.remaining()?.min(POLL_SLICE).as_millis().max(1) as DWORD;
        match unsafe { WaitForSingleObject(event, millis) } {
            WAIT_OBJECT_0 => return Ok(()),
            WAIT_TIMEOUT => {}
            _ => return Err(win_error()),
        }
    }
}

fn make_event() -> io::Result<OwnedHandle> {
    owned(unsafe { CreateEventW(null_mut(), TRUE, FALSE, null()) })
}

fn finish_overlapped(
    handle: HANDLE,
    overlapped: &mut OVERLAPPED,
    event: HANDLE,
    budget: IoBudget<'_>,
) -> io::Result<DWORD> {
    if let Err(error) = wait_event(event, budget) {
        unsafe { CancelIoEx(handle, overlapped) };
        while unsafe { WaitForSingleObject(event, 10) } != WAIT_OBJECT_0 {}
        let mut ignored = 0;
        unsafe { GetOverlappedResult(handle, overlapped, &mut ignored, FALSE) };
        return Err(error);
    }
    let mut transferred = 0;
    if unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, FALSE) } == 0 {
        return Err(win_error());
    }
    Ok(transferred)
}

fn connect_pipe(handle: HANDLE, budget: IoBudget<'_>) -> io::Result<()> {
    budget.check()?;
    let event = make_event()?;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.as_raw_handle().cast();
    let connected = unsafe { ConnectNamedPipe(handle, &mut overlapped) };
    if connected != 0 {
        return Ok(());
    }
    let error = win_error();
    if error.raw_os_error() == Some(winapi::shared::winerror::ERROR_PIPE_CONNECTED as i32) {
        return Ok(());
    }
    if error.raw_os_error() != Some(winapi::shared::winerror::ERROR_IO_PENDING as i32) {
        return Err(error);
    }
    finish_overlapped(
        handle,
        &mut overlapped,
        event.as_raw_handle().cast(),
        budget,
    )?;
    Ok(())
}

fn verify_server_owner(handle: HANDLE) -> io::Result<()> {
    unsafe {
        let mut owner = null_mut();
        let mut descriptor = null_mut();
        let result = GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        );
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let comparison = (|| {
            let mut raw = null_mut();
            if winapi::um::processthreadsapi::OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut raw,
            ) == 0
            {
                return Err(win_error());
            }
            let current = owned(raw)?;
            let token = token_user(current.as_raw_handle().cast())?;
            let current_sid = (*token.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            Ok(EqualSid(owner.cast(), current_sid) != 0)
        })();
        LocalFree(descriptor.cast());
        if !comparison? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Local peer rejected.",
            ));
        }
        Ok(())
    }
}

fn transfer(
    handle: HANDLE,
    buffer: &mut [u8],
    write: bool,
    budget: IoBudget<'_>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        budget.check()?;
        let length = (buffer.len() - offset).min(MAX_CHUNK);
        let event = make_event()?;
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle().cast();
        let mut transferred = 0;
        let ok = unsafe {
            if write {
                winapi::um::fileapi::WriteFile(
                    handle,
                    buffer[offset..].as_ptr().cast(),
                    length as DWORD,
                    &mut transferred,
                    &mut overlapped,
                )
            } else {
                winapi::um::fileapi::ReadFile(
                    handle,
                    buffer[offset..].as_mut_ptr().cast(),
                    length as DWORD,
                    &mut transferred,
                    &mut overlapped,
                )
            }
        };
        if ok == 0 {
            let error = win_error();
            if error.raw_os_error() != Some(winapi::shared::winerror::ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            transferred = finish_overlapped(
                handle,
                &mut overlapped,
                event.as_raw_handle().cast(),
                budget,
            )?;
        }
        if transferred == 0 {
            return Err(io::Error::new(
                if write {
                    io::ErrorKind::WriteZero
                } else {
                    io::ErrorKind::UnexpectedEof
                },
                "Local peer closed the connection.",
            ));
        }
        offset += transferred as usize;
    }
    Ok(())
}

impl Listener {
    pub(super) fn bind(endpoint: &LocalEndpoint) -> io::Result<Self> {
        let name = pipe_name(endpoint)?;
        let security = user_security()?;
        let _keep_security_alive = (&security.descriptor, &security.acl);
        let raw = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_CHUNK as DWORD,
                MAX_CHUNK as DWORD,
                0,
                &security.attributes as *const SECURITY_ATTRIBUTES as *mut SECURITY_ATTRIBUTES,
            )
        };
        Ok(Self {
            handle: owned(raw)?,
        })
    }

    pub(super) fn accept<'a>(&'a mut self, budget: IoBudget<'_>) -> io::Result<Connection<'a>> {
        let handle = self.handle.as_raw_handle().cast();
        if let Err(error) = connect_pipe(handle, budget) {
            unsafe { DisconnectNamedPipe(handle) };
            return Err(error);
        }
        let mut connection = Connection {
            handle: Handle::Server(self),
        };
        let mut preamble = [0; 3];
        connection.read_exact(&mut preamble, budget)?;
        if preamble != *b"FX2" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid local transport preamble.",
            ));
        }
        authenticate_client(connection.handle.raw())?;
        Ok(connection)
    }
}

pub(super) fn connect<'a>(
    endpoint: &LocalEndpoint,
    budget: IoBudget<'_>,
) -> io::Result<Connection<'a>> {
    let name = pipe_name(endpoint)?;
    loop {
        budget.check()?;
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE | winapi::um::winnt::READ_CONTROL,
                0,
                null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED
                    | winapi::um::winbase::SECURITY_SQOS_PRESENT
                    | winapi::um::winbase::SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if raw != INVALID_HANDLE_VALUE {
            let handle = owned(raw)?;
            verify_server_owner(handle.as_raw_handle().cast())?;
            let mut connection = Connection {
                handle: Handle::Client(handle),
            };
            if let Err(error) = connection.write_all(b"FX2", budget) {
                if matches!(error.raw_os_error(), Some(code) if code == winapi::shared::winerror::ERROR_NO_DATA as i32
                    || code == winapi::shared::winerror::ERROR_PIPE_NOT_CONNECTED as i32
                    || code == winapi::shared::winerror::ERROR_BROKEN_PIPE as i32)
                {
                    drop(connection);
                    std::thread::sleep(budget.remaining()?.min(POLL_SLICE));
                    continue;
                }
                return Err(error);
            }
            return Ok(connection);
        }
        let error = win_error();
        if !matches!(error.raw_os_error(), Some(code) if code == winapi::shared::winerror::ERROR_PIPE_BUSY as i32
            || code == winapi::shared::winerror::ERROR_NO_DATA as i32
            || code == winapi::shared::winerror::ERROR_PIPE_NOT_CONNECTED as i32)
        {
            return Err(error);
        }
        std::thread::sleep(budget.remaining()?.min(POLL_SLICE));
    }
}

impl Connection<'_> {
    pub(super) fn read_exact(&mut self, buffer: &mut [u8], budget: IoBudget<'_>) -> io::Result<()> {
        transfer(self.handle.raw(), buffer, false, budget)
    }

    pub(super) fn write_all(&mut self, buffer: &[u8], budget: IoBudget<'_>) -> io::Result<()> {
        for chunk in buffer.chunks(MAX_CHUNK) {
            budget.check()?;
            let mut owned = chunk.to_vec();
            transfer(self.handle.raw(), &mut owned, true, budget)?;
        }
        Ok(())
    }
}
