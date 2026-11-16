use std::io;

use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::tlhelp32::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

pub(crate) fn any_process_running(names: &[&str]) -> io::Result<bool> {
    if names.is_empty() {
        return Ok(false);
    }
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        let mut running = false;
        while more {
            if matches_process_name(&entry.szExeFile, names) {
                running = true;
                break;
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        Ok(running)
    }
}

fn matches_process_name(executable: &[u16], names: &[&str]) -> bool {
    let length = executable
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(executable.len());
    let executable = String::from_utf16_lossy(&executable[..length]);
    names
        .iter()
        .any(|name| executable.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_names_match_exactly_without_case() {
        let executable: Vec<u16> = "Steam.EXE\0ignored".encode_utf16().collect();
        assert!(matches_process_name(&executable, &["steam.exe"]));
        assert!(!matches_process_name(&executable, &["steam"]));
        assert!(!matches_process_name(&executable, &["steamwebhelper.exe"]));
        assert!(!matches_process_name(&executable, &[]));
        let executable: Vec<u16> = "notsteam.exe".encode_utf16().collect();
        assert!(!matches_process_name(&executable, &["steam.exe"]));
    }

    #[test]
    fn snapshot_finds_current_process_and_rejects_missing_process() {
        let executable = std::env::current_exe().unwrap();
        let name = executable.file_name().unwrap().to_str().unwrap();
        assert!(any_process_running(&[name]).unwrap());
        assert!(!any_process_running(&["foxy-process-that-does-not-exist.exe"]).unwrap());
        assert!(!any_process_running(&[]).unwrap());
    }
}
