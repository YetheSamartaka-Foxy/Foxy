use std::path::Path;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutableVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: u32,
}

impl std::fmt::Display for ExecutableVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}.{}.{}.{}",
            self.major, self.minor, self.patch, self.build
        )
    }
}

#[cfg(any(windows, test))]
fn parse_version(value: &str) -> Option<ExecutableVersion> {
    let parts = value
        .split('.')
        .map(|part| {
            if part.is_empty() || part.len() > 10 || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            part.parse::<u32>().ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let [major, minor, patch, build] = parts.as_slice() else {
        return None;
    };
    (*major != 0 && *build != 0).then_some(ExecutableVersion {
        major: *major,
        minor: *minor,
        patch: *patch,
        build: *build,
    })
}

pub(super) fn observe_version(path: &Path) -> Option<ExecutableVersion> {
    #[cfg(windows)]
    {
        windows_version(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
fn windows_version(path: &Path) -> Option<ExecutableVersion> {
    use std::os::windows::ffi::OsStrExt;
    use winapi::um::winver::{GetFileVersionInfoSizeW, GetFileVersionInfoW};

    let name = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut ignored = 0;
    let size = unsafe { GetFileVersionInfoSizeW(name.as_ptr(), &mut ignored) };
    if size == 0 || size > 4 * 1024 * 1024 {
        return None;
    }
    let mut block = vec![0u32; (size as usize).div_ceil(4)];
    if unsafe { GetFileVersionInfoW(name.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let translations = query_version(&block, "\\VarFileInfo\\Translation", 1)?;
    if translations.is_empty() || translations.len() > 64 || translations.len() % 4 != 0 {
        return None;
    }
    let mut observed = None;
    for translation in translations.chunks_exact(4) {
        let language = u16::from_le_bytes(translation[..2].try_into().ok()?);
        let codepage = u16::from_le_bytes(translation[2..].try_into().ok()?);
        let key = format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\ProductVersion");
        let Some(value) = query_version(&block, &key, 2) else {
            continue;
        };
        if value.len() > 256 {
            return None;
        }
        let wide = value
            .chunks_exact(2)
            .map(|part| u16::from_le_bytes([part[0], part[1]]))
            .collect::<Vec<_>>();
        let Some((&0, text)) = wide.split_last() else {
            return None;
        };
        let version = parse_version(&String::from_utf16(text).ok()?)?;
        if observed.is_some_and(|previous| previous != version) {
            return None;
        }
        observed = Some(version);
    }
    observed
}

#[cfg(windows)]
fn query_version<'a>(block: &'a [u32], key: &str, unit_size: usize) -> Option<&'a [u8]> {
    use winapi::um::winver::VerQueryValueW;

    let key = key.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut pointer = std::ptr::null_mut();
    let mut length = 0;
    if unsafe {
        VerQueryValueW(
            block.as_ptr().cast(),
            key.as_ptr(),
            &mut pointer,
            &mut length,
        )
    } == 0
    {
        return None;
    }
    let length = (length as usize).checked_mul(unit_size)?;
    let start = block.as_ptr() as usize;
    let end = start.checked_add(std::mem::size_of_val(block))?;
    let address = pointer as usize;
    if address < start || address.checked_add(length)? > end {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(pointer.cast(), length) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_version_preserves_large_engine_build_numbers() {
        let version = parse_version("2.22.0.154089").unwrap();
        assert_eq!(version.build, 154089);
        assert_eq!(version.to_string(), "2.22.0.154089");
        for value in [
            "2.22",
            "2.22.154089",
            "2.22.0.0",
            "0.22.0.154089",
            "2.22.0.154089 beta",
            "2.22.-1.154089",
            "2.22.0.4294967296",
            "2.22.0.154089.1",
        ] {
            assert!(parse_version(value).is_none(), "{value}");
        }
    }
}
