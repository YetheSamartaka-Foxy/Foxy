use foxy_steam::AuthError;
use std::fs::File;
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::{Component, Path};
use std::ptr::null_mut;
use winapi::shared::minwindef::DWORD;
use winapi::um::accctrl::SE_FILE_OBJECT;
use winapi::um::aclapi::GetSecurityInfo;
use winapi::um::fileapi::{CreateFileW, GetFileInformationByHandle, OPEN_EXISTING};
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::securitybaseapi::{
    EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, GetTokenInformation,
};
use winapi::um::winbase::{FILE_FLAG_OPEN_REPARSE_POINT, LocalFree};
use winapi::um::winnt::{
    ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_READ, GENERIC_READ, HANDLE,
    OWNER_SECURITY_INFORMATION, READ_CONTROL, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR_CONTROL,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};

const MAX_BYTES: usize = 16 * 1024;

fn fail() -> AuthError {
    AuthError::Storage
}

fn owned_handle(raw: HANDLE) -> Result<OwnedHandle, AuthError> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(fail())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw.cast()) })
    }
}

fn token_sid() -> Result<Vec<u64>, AuthError> {
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    let mut raw = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(fail());
    }
    let token = owned_handle(raw)?;
    let mut needed = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle().cast(),
            TokenUser,
            null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 {
        return Err(fail());
    }
    let mut storage = vec![0u64; (needed as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle().cast(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(fail());
    }
    Ok(storage)
}

fn validate_ancestors(path: &Path) -> Result<(), AuthError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| fail())?.join(path)
    };
    let mut current = Path::new("").to_path_buf();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => {
                current.push(prefix.as_os_str());
                continue;
            }
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => continue,
            Component::ParentDir => return Err(fail()),
            Component::Normal(part) => current.push(part),
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| fail())?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
            return Err(fail());
        }
    }
    Ok(())
}

fn verify_private_acl(handle: HANDLE) -> Result<(), AuthError> {
    unsafe {
        let mut owner = null_mut();
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        let result = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        );
        if result != 0 {
            return Err(fail());
        }
        let checked = (|| {
            let sid_storage = token_sid()?;
            let sid = (*sid_storage.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let mut control: SECURITY_DESCRIPTOR_CONTROL = 0;
            let mut revision = 0;
            if EqualSid(owner.cast(), sid) == 0
                || GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0
                || control & SE_DACL_PROTECTED == 0
                || dacl.is_null()
            {
                return Err(fail());
            }
            let mut info: winapi::um::winnt::ACL_SIZE_INFORMATION = std::mem::zeroed();
            if GetAclInformation(
                dacl,
                (&mut info as *mut winapi::um::winnt::ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of_val(&info) as DWORD,
                winapi::um::winnt::AclSizeInformation,
            ) == 0
                || info.AceCount == 0
            {
                return Err(fail());
            }
            let mut read_granted = false;
            for index in 0..info.AceCount {
                let mut ace_ptr = null_mut();
                if GetAce(dacl, index, &mut ace_ptr) == 0 {
                    return Err(fail());
                }
                let header = ace_ptr.cast::<winapi::um::winnt::ACE_HEADER>();
                if (*header).AceType != winapi::um::winnt::ACCESS_ALLOWED_ACE_TYPE {
                    return Err(fail());
                }
                let ace = ace_ptr.cast::<ACCESS_ALLOWED_ACE>();
                let ace_sid = (&(*ace).SidStart as *const DWORD).cast_mut().cast();
                if EqualSid(ace_sid, sid) == 0 {
                    return Err(fail());
                }
                let mask = (*ace).Mask;
                read_granted |= mask & GENERIC_READ != 0
                    || mask & FILE_GENERIC_READ == FILE_GENERIC_READ
                    || mask & FILE_ALL_ACCESS == FILE_ALL_ACCESS;
            }
            if !read_granted {
                return Err(fail());
            }
            Ok(())
        })();
        LocalFree(descriptor.cast());
        checked
    }
}

pub(super) fn read_private(path: &Path) -> Result<String, AuthError> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(fail());
    }
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| fail())?.join(path)
    };
    let parent = path.parent().ok_or_else(fail)?;
    validate_ancestors(parent)?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | READ_CONTROL,
            FILE_SHARE_READ,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    let handle = owned_handle(raw)?;
    let file = unsafe { File::from_raw_handle(handle.into_raw_handle()) };
    if !file.metadata().map_err(|_| fail())?.is_file() {
        return Err(fail());
    }
    let mut information: winapi::um::fileapi::BY_HANDLE_FILE_INFORMATION =
        unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0
        || information.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)
            != 0
        || information.nNumberOfLinks != 1
    {
        return Err(fail());
    }
    let length = ((information.nFileSizeHigh as u64) << 32) | information.nFileSizeLow as u64;
    if length > MAX_BYTES as u64 {
        return Err(AuthError::InvalidInput);
    }
    verify_private_acl(file.as_raw_handle().cast())?;
    let mut text = String::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|_| AuthError::InvalidInput)?;
    if text.len() > MAX_BYTES {
        return Err(AuthError::InvalidInput);
    }
    Ok(text)
}
