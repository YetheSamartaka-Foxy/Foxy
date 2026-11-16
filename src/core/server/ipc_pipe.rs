//! One local named-pipe exchange.
//!
//! The pipe is created for the call and closed before the function returns.
//! It does not open a database or transfer ownership.

#![cfg_attr(not(test), allow(dead_code))]

use super::ipc_frame::{ControlFrame, FrameError, validate_frame};

const MAX_EXCHANGE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PipeError {
    Unsupported,
    Encode,
    Io,
    Rejected,
}

pub(crate) fn encode_frame(frame: &ControlFrame) -> Result<Vec<u8>, PipeError> {
    if frame.space_id.len() > u16::MAX as usize || frame.payload.len() > MAX_EXCHANGE_BYTES {
        return Err(PipeError::Encode);
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&frame.version.to_le_bytes());
    bytes.extend_from_slice(&frame.owner_generation.to_le_bytes());
    bytes.extend_from_slice(&frame.request_id.to_le_bytes());
    bytes.extend_from_slice(&(frame.space_id.len() as u16).to_le_bytes());
    bytes.extend_from_slice(frame.space_id.as_bytes());
    bytes.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&frame.payload);
    if bytes.len() > MAX_EXCHANGE_BYTES {
        return Err(PipeError::Encode);
    }
    Ok(bytes)
}

pub(crate) fn decode_frame(bytes: &[u8]) -> Result<ControlFrame, PipeError> {
    if bytes.len() < 2 + 8 + 8 + 2 + 4 {
        return Err(PipeError::Encode);
    }
    let version = u16::from_le_bytes(bytes[0..2].try_into().map_err(|_| PipeError::Encode)?);
    let owner_generation =
        u64::from_le_bytes(bytes[2..10].try_into().map_err(|_| PipeError::Encode)?);
    let request_id = u64::from_le_bytes(bytes[10..18].try_into().map_err(|_| PipeError::Encode)?);
    let space_len =
        u16::from_le_bytes(bytes[18..20].try_into().map_err(|_| PipeError::Encode)?) as usize;
    let space_end = 20 + space_len;
    let payload_len_end = space_end.checked_add(4).ok_or(PipeError::Encode)?;
    if bytes.len() < payload_len_end {
        return Err(PipeError::Encode);
    }
    let space_id =
        String::from_utf8(bytes[20..space_end].to_vec()).map_err(|_| PipeError::Encode)?;
    let payload_len = u32::from_le_bytes(
        bytes[space_end..payload_len_end]
            .try_into()
            .map_err(|_| PipeError::Encode)?,
    ) as usize;
    let payload_end = payload_len_end
        .checked_add(payload_len)
        .ok_or(PipeError::Encode)?;
    if bytes.len() != payload_end || payload_len > MAX_EXCHANGE_BYTES {
        return Err(PipeError::Encode);
    }
    Ok(ControlFrame {
        version,
        space_id,
        owner_generation,
        request_id,
        payload: bytes[payload_len_end..payload_end].to_vec(),
    })
}

/// Exchanges one frame and closes the pipe. Non-Windows builds do not open a socket.
pub(crate) fn exchange_local_frame(
    frame: &ControlFrame,
    expected_space: &str,
    expected_generation: u64,
) -> Result<ControlFrame, PipeError> {
    validate_frame(frame, expected_space, expected_generation).map_err(pipe_frame_error)?;
    #[cfg(windows)]
    {
        windows::exchange(frame, expected_space, expected_generation)
    }
    #[cfg(not(windows))]
    {
        let _ = (frame, expected_space, expected_generation);
        Err(PipeError::Unsupported)
    }
}

fn pipe_frame_error(error: FrameError) -> PipeError {
    let _ = error;
    PipeError::Rejected
}

#[cfg(windows)]
mod windows {
    use super::super::ipc_frame::validate_frame;
    use super::{ControlFrame, PipeError, decode_frame, encode_frame};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::thread;

    use winapi::shared::minwindef::{DWORD, FALSE, TRUE};
    use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING, ReadFile, WriteFile};
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
    use winapi::um::namedpipeapi::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe};
    use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
    use winapi::um::securitybaseapi::{
        AddAccessAllowedAce, GetLengthSid, GetTokenInformation, InitializeAcl,
        InitializeSecurityDescriptor, SetSecurityDescriptorDacl,
    };
    use winapi::um::winbase::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use winapi::um::winnt::{
        ACL, ACL_REVISION, GENERIC_READ, GENERIC_WRITE, SECURITY_DESCRIPTOR_MIN_LENGTH,
        SECURITY_DESCRIPTOR_REVISION, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };

    pub(super) fn exchange(
        frame: &ControlFrame,
        expected_space: &str,
        expected_generation: u64,
    ) -> Result<ControlFrame, PipeError> {
        let encoded = encode_frame(frame)?;
        let name = format!(
            r"\\.\pipe\foxy-frame-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        );
        if !name.starts_with(r"\\.\pipe\") || name.contains("database.db") {
            return Err(PipeError::Io);
        }
        let wide_name = wide(&name);
        let security = user_security().map_err(|_| PipeError::Io)?;
        let server = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                64 * 1024,
                0,
                &security.attributes as *const SECURITY_ATTRIBUTES as *mut _,
            )
        };
        if server.is_null() || server == INVALID_HANDLE_VALUE {
            return Err(PipeError::Io);
        }
        let server_bits = server as usize;
        let expected_space = expected_space.to_string();
        let reader = thread::spawn(move || {
            read_and_validate(server_bits, &expected_space, expected_generation)
        });
        let client = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if client.is_null() || client == INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(server);
            }
            let _ = reader.join();
            return Err(PipeError::Io);
        }
        let write_result = write_all(client, &encoded);
        let mut echoed = vec![0_u8; encoded.len()];
        let read_result = write_result.and_then(|_| read_exact(client, &mut echoed));
        unsafe {
            CloseHandle(client);
        }
        let accepted = reader.join().map_err(|_| PipeError::Io)?;
        unsafe {
            CloseHandle(server);
        }
        read_result?;
        let accepted = accepted?;
        if echoed != encoded {
            return Err(PipeError::Io);
        }
        let _ = security;
        Ok(accepted)
    }

    struct UserSecurity {
        _descriptor: Vec<u8>,
        _acl: Vec<u8>,
        attributes: SECURITY_ATTRIBUTES,
    }

    fn user_security() -> io::Result<UserSecurity> {
        unsafe {
            let mut token = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut needed = 0;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            let mut token_user = vec![0_u8; needed as usize];
            if GetTokenInformation(
                token,
                TokenUser,
                token_user.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                CloseHandle(token);
                return Err(io::Error::last_os_error());
            }
            let sid = (*token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let sid_len = GetLengthSid(sid);
            let acl_len = std::mem::size_of::<ACL>()
                + std::mem::size_of::<winapi::um::winnt::ACCESS_ALLOWED_ACE>()
                - std::mem::size_of::<DWORD>()
                + sid_len as usize;
            let mut acl = vec![0_u8; acl_len];
            if InitializeAcl(
                acl.as_mut_ptr().cast(),
                acl_len as DWORD,
                u32::from(ACL_REVISION),
            ) == 0
                || AddAccessAllowedAce(
                    acl.as_mut_ptr().cast(),
                    u32::from(ACL_REVISION),
                    GENERIC_READ | GENERIC_WRITE,
                    sid,
                ) == 0
            {
                CloseHandle(token);
                return Err(io::Error::last_os_error());
            }
            CloseHandle(token);
            let mut descriptor = vec![0_u8; SECURITY_DESCRIPTOR_MIN_LENGTH];
            if InitializeSecurityDescriptor(
                descriptor.as_mut_ptr().cast(),
                SECURITY_DESCRIPTOR_REVISION,
            ) == 0
                || SetSecurityDescriptorDacl(
                    descriptor.as_mut_ptr().cast(),
                    TRUE,
                    acl.as_mut_ptr().cast(),
                    FALSE,
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut attributes = std::mem::zeroed::<SECURITY_ATTRIBUTES>();
            attributes.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD;
            attributes.lpSecurityDescriptor = descriptor.as_mut_ptr().cast();
            attributes.bInheritHandle = FALSE;
            Ok(UserSecurity {
                _descriptor: descriptor,
                _acl: acl,
                attributes,
            })
        }
    }

    fn frame_complete(bytes: &[u8]) -> bool {
        if bytes.len() < 20 {
            return false;
        }
        let space_len = u16::from_le_bytes([bytes[18], bytes[19]]) as usize;
        let header = match (20usize)
            .checked_add(space_len)
            .and_then(|end| end.checked_add(4))
        {
            Some(header) => header,
            None => return true,
        };
        if bytes.len() < header {
            return false;
        }
        let payload_len =
            u32::from_le_bytes(bytes[header - 4..header].try_into().unwrap_or([0; 4])) as usize;
        bytes.len() >= header + payload_len
    }

    fn read_and_validate(
        server_bits: usize,
        expected_space: &str,
        expected_generation: u64,
    ) -> Result<ControlFrame, PipeError> {
        let server = server_bits as winapi::um::winnt::HANDLE;
        let connected = unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) };
        if connected == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(winapi::shared::winerror::ERROR_PIPE_CONNECTED as i32) {
                return Err(PipeError::Io);
            }
        }
        let mut encoded = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let mut got = 0;
            let ok = unsafe {
                ReadFile(
                    server,
                    chunk.as_mut_ptr().cast(),
                    chunk.len() as DWORD,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || got == 0 {
                return Err(PipeError::Io);
            }
            encoded.extend_from_slice(&chunk[..got as usize]);
            if encoded.len() > 64 * 1024 {
                return Err(PipeError::Encode);
            }
            if frame_complete(&encoded) {
                break;
            }
        }
        let frame = decode_frame(&encoded)?;
        validate_frame(&frame, expected_space, expected_generation)
            .map_err(|_| PipeError::Rejected)?;
        write_all(server, &encoded)?;
        unsafe {
            DisconnectNamedPipe(server);
        }
        Ok(frame)
    }

    fn write_all(handle: winapi::um::winnt::HANDLE, bytes: &[u8]) -> Result<(), PipeError> {
        let mut written_total = 0;
        while written_total < bytes.len() {
            let mut written = 0;
            let ok = unsafe {
                WriteFile(
                    handle,
                    bytes[written_total..].as_ptr().cast(),
                    (bytes.len() - written_total) as DWORD,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                return Err(PipeError::Io);
            }
            written_total += written as usize;
        }
        Ok(())
    }

    fn read_exact(handle: winapi::um::winnt::HANDLE, bytes: &mut [u8]) -> Result<(), PipeError> {
        let mut got_total = 0;
        while got_total < bytes.len() {
            let mut got = 0;
            let ok = unsafe {
                ReadFile(
                    handle,
                    bytes[got_total..].as_mut_ptr().cast(),
                    (bytes.len() - got_total) as DWORD,
                    &mut got,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || got == 0 {
                return Err(PipeError::Io);
            }
            got_total += got as usize;
        }
        Ok(())
    }

    fn wide(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::ipc_frame::PROTOCOL_VERSION;

    fn sample() -> ControlFrame {
        ControlFrame {
            version: PROTOCOL_VERSION,
            space_id: "alpha".into(),
            owner_generation: 4,
            request_id: 9,
            payload: b"inspect".to_vec(),
        }
    }

    #[test]
    fn a_frame_round_trips_without_opening_a_pipe() {
        let frame = sample();
        let encoded = encode_frame(&frame).unwrap();
        assert_eq!(decode_frame(&encoded).unwrap(), frame);
        assert!(
            encode_frame(&ControlFrame {
                payload: vec![0; MAX_EXCHANGE_BYTES + 1],
                ..frame
            })
            .is_err()
        );
    }

    #[test]
    #[cfg(windows)]
    fn one_validated_frame_crosses_a_named_pipe_and_the_pipe_closes() {
        let frame = sample();
        let returned = exchange_local_frame(&frame, "alpha", 4).unwrap();
        assert_eq!(returned, frame);
        assert_ne!(PipeError::Unsupported, PipeError::Io);
        let stale = ControlFrame {
            owner_generation: 5,
            ..frame
        };
        assert_eq!(
            exchange_local_frame(&stale, "alpha", 4),
            Err(PipeError::Rejected)
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn a_non_windows_build_does_not_open_a_socket() {
        assert_eq!(
            exchange_local_frame(&sample(), "alpha", 4),
            Err(PipeError::Unsupported)
        );
    }
}
