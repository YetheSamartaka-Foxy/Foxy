use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::path::{Component, Path};
use std::ptr::null_mut;
use winapi::shared::minwindef::{DWORD, FALSE, TRUE};
use winapi::um::accctrl::SE_FILE_OBJECT;
use winapi::um::aclapi::GetSecurityInfo;
use winapi::um::fileapi::{CREATE_NEW, CreateFileW, GetFileInformationByHandle, OPEN_EXISTING};
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::securitybaseapi::{
    AddAccessAllowedAce, EqualSid, GetAce, GetAclInformation, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
    SetSecurityDescriptorControl, SetSecurityDescriptorDacl, SetSecurityDescriptorOwner,
};
use winapi::um::winbase::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, LocalFree};
use winapi::um::winnt::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, FILE_ALL_ACCESS,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GENERIC_READ, GENERIC_WRITE, HANDLE, OWNER_SECURITY_INFORMATION, READ_CONTROL,
    SE_DACL_PROTECTED, SECURITY_DESCRIPTOR_CONTROL, SECURITY_DESCRIPTOR_MIN_LENGTH,
    SECURITY_DESCRIPTOR_REVISION, TOKEN_QUERY, TOKEN_USER, TokenUser,
};

const MAX_BYTES: usize = 8192;

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Unsafe protected handoff entry.",
    )
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

fn token_sid() -> io::Result<Vec<u64>> {
    unsafe {
        let mut raw = null_mut();
        if winapi::um::processthreadsapi::OpenProcessToken(
            winapi::um::processthreadsapi::GetCurrentProcess(),
            TOKEN_QUERY,
            &mut raw,
        ) == 0
        {
            return Err(win_error());
        }
        let token = owned(raw)?;
        let mut needed = 0;
        GetTokenInformation(
            token.as_raw_handle().cast(),
            TokenUser,
            null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 {
            return Err(win_error());
        }
        let mut storage = vec![0u64; (needed as usize).div_ceil(8)];
        if GetTokenInformation(
            token.as_raw_handle().cast(),
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

struct Security {
    descriptor: Vec<u64>,
    acl: Vec<u64>,
    _sid: Vec<u64>,
    attributes: SECURITY_ATTRIBUTES,
}

fn user_security() -> io::Result<Security> {
    unsafe {
        let sid_storage = token_sid()?;
        let sid = (*sid_storage.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let sid_len = GetLengthSid(sid) as usize;
        let acl_len = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            - std::mem::size_of::<DWORD>()
            + sid_len;
        let mut acl = vec![0u64; acl_len.div_ceil(8)];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        if InitializeAcl(acl_ptr, acl_len as DWORD, ACL_REVISION as DWORD) == 0
            || AddAccessAllowedAce(acl_ptr, ACL_REVISION as DWORD, FILE_ALL_ACCESS, sid) == 0
        {
            return Err(win_error());
        }
        let mut descriptor = vec![0u64; SECURITY_DESCRIPTOR_MIN_LENGTH.div_ceil(8)];
        let descriptor_ptr = descriptor.as_mut_ptr().cast();
        if InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) == 0
            || SetSecurityDescriptorOwner(descriptor_ptr, sid, FALSE) == 0
            || SetSecurityDescriptorDacl(descriptor_ptr, TRUE, acl_ptr, FALSE) == 0
            || SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
                == 0
        {
            return Err(win_error());
        }
        let mut attributes = std::mem::zeroed::<SECURITY_ATTRIBUTES>();
        attributes.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD;
        attributes.lpSecurityDescriptor = descriptor_ptr;
        attributes.bInheritHandle = FALSE;
        Ok(Security {
            descriptor,
            acl,
            _sid: sid_storage,
            attributes,
        })
    }
}

fn validate_chain(path: &Path) -> io::Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
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
            Component::Normal(part) => current.push(part),
            Component::ParentDir => return Err(denied()),
        }
        let metadata = std::fs::symlink_metadata(&current)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
            return Err(denied());
        }
    }
    Ok(())
}

fn verify_object(handle: HANDLE) -> io::Result<()> {
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
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let check = (|| {
            let sid_storage = token_sid()?;
            let sid = (*sid_storage.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let mut control: SECURITY_DESCRIPTOR_CONTROL = 0;
            let mut revision = 0;
            if EqualSid(owner.cast(), sid) == 0
                || GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0
                || control & SE_DACL_PROTECTED == 0
                || dacl.is_null()
            {
                return Err(denied());
            }
            let mut info: winapi::um::winnt::ACL_SIZE_INFORMATION = std::mem::zeroed();
            if GetAclInformation(
                dacl,
                (&mut info as *mut winapi::um::winnt::ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of_val(&info) as DWORD,
                winapi::um::winnt::AclSizeInformation,
            ) == 0
                || info.AceCount != 1
            {
                return Err(denied());
            }
            let mut ace = null_mut();
            if GetAce(dacl, 0, &mut ace) == 0 {
                return Err(win_error());
            }
            let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
            let expected = FILE_ALL_ACCESS;
            if (*ace).Header.AceType != winapi::um::winnt::ACCESS_ALLOWED_ACE_TYPE
                || (*ace).Mask != expected
                || EqualSid((&(*ace).SidStart as *const DWORD).cast_mut().cast(), sid) == 0
            {
                return Err(denied());
            }
            Ok(())
        })();
        LocalFree(descriptor.cast());
        check
    }
}

pub(super) fn create_directory(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(denied)?;
    validate_chain(parent)?;
    let security = user_security()?;
    let _keep = (&security.descriptor, &security.acl);
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        winapi::um::fileapi::CreateDirectoryW(
            wide.as_ptr(),
            &security.attributes as *const _ as *mut _,
        )
    } == 0
    {
        let error = win_error();
        if error.raw_os_error() == Some(winapi::shared::winerror::ERROR_ALREADY_EXISTS as i32) {
            return verify_directory(path);
        }
        return Err(error);
    }
    verify_directory(path)
}

pub(super) fn verify_directory(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(denied)?;
    validate_chain(parent)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied());
    }
    let handle = unsafe {
        CreateFileW(
            path.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>()
                .as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    let handle = owned(handle)?;
    verify_object(handle.as_raw_handle().cast())
}

fn open_file(path: &Path, name: &str, create: bool) -> io::Result<File> {
    verify_directory(path)?;
    let security = user_security()?;
    let _keep = (&security.descriptor, &security.acl);
    let wide: Vec<u16> = path
        .join(name)
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let disposition = if create { CREATE_NEW } else { OPEN_EXISTING };
    let security_attributes = if create {
        &security.attributes as *const _ as *mut _
    } else {
        null_mut()
    };
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL | winapi::um::winnt::DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            security_attributes,
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    let handle = owned(raw)?;
    let file = unsafe { File::from_raw_handle(handle.into_raw_handle()) };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.file_size() > MAX_BYTES as u64
    {
        return Err(denied());
    }
    let mut information: winapi::um::fileapi::BY_HANDLE_FILE_INFORMATION =
        unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(win_error());
    }
    if information.nNumberOfLinks != 1 {
        return Err(denied());
    }
    verify_object(file.as_raw_handle().cast())?;
    Ok(file)
}

pub(super) fn publish_new(path: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_file(path, name, true)?;
    file.write_all(bytes)?;
    file.sync_all()
}

pub(super) fn read(path: &Path, name: &str) -> io::Result<Vec<u8>> {
    let mut file = open_file(path, name, false)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(denied());
    }
    Ok(bytes)
}

pub(super) fn remove_exact(path: &Path, name: &str, expected: &[u8]) -> io::Result<()> {
    let mut file = open_file(path, name, false)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES || bytes != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Handoff entry changed.",
        ));
    }
    verify_directory(path)?;
    std::fs::remove_file(path.join(name))
}
