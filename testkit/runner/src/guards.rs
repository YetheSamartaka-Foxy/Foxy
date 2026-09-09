use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

fn normalized(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            part => result.push(part.as_os_str()),
        }
    }
    let mut existing = result.as_path();
    let mut tail = Vec::new();
    while !existing.exists() {
        tail.push(
            existing
                .file_name()
                .context("Could not resolve config directory")?
                .to_owned(),
        );
        existing = existing
            .parent()
            .context("Could not resolve config directory")?;
    }
    result = existing.canonicalize()?;
    for part in tail.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}

pub fn assert_isolation_against(config: &Path, live: &Path) -> Result<()> {
    let config = normalized(config)?
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_lowercase();
    let live = normalized(live)?
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_lowercase();
    ensure!(
        config != live && !config.starts_with(&format!("{live}{}", std::path::MAIN_SEPARATOR)),
        "The test kit refuses the live Foxy config directory or its descendants"
    );
    Ok(())
}

pub fn git_state(root: &Path) -> Result<Value> {
    let read = |args: &[&str]| -> Result<String> {
        let result = crate::launch::process(
            Command::new("git").current_dir(root).args(args),
            Duration::from_secs(30),
        )?;
        ensure!(result.status.success(), "Could not inspect Git state");
        Ok(String::from_utf8(result.stdout)?.trim().to_owned())
    };
    Ok(
        json!({"sha":read(&["rev-parse","HEAD"])?,"dirty":!read(&["status","--porcelain"])?.is_empty()}),
    )
}

pub fn check(case: &Value, root: &Path, config: &Path) -> Result<Value> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        assert_isolation_against(config, &PathBuf::from(appdata).join("Foxy"))?;
    }
    let guards = &case["guards"];
    if guards["require_no_other_foxy"].as_bool().unwrap_or(true) {
        let system = sysinfo::System::new_all();
        ensure!(
            !system.processes().values().any(|p| {
                let name = p.name().to_string_lossy();
                name.eq_ignore_ascii_case("Foxy") || name.eq_ignore_ascii_case("Foxy.exe")
            }),
            "Another Foxy process is running"
        );
    }
    let git = git_state(root)?;
    ensure!(
        !guards["require_clean_worktree"].as_bool().unwrap_or(false) || git["dirty"] == false,
        "The case requires a clean Git worktree"
    );
    let mut storage = "unknown";
    if let Some(repository) = case.get("repository") {
        let target = repository["path"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .context("Repository path is empty")?;
        let path = Path::new(target);
        if let Some(required) = guards["require_free_gb"].as_f64() {
            ensure!(
                free_bytes(path)? as f64 / 1_073_741_824.0 >= required,
                "Repository drive has less than the required {required} GiB free"
            );
        }
        storage = storage_class(path);
        if let Some(expected) = guards["require_storage_class"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            ensure!(
                storage != "unknown" && expected.eq_ignore_ascii_case(storage),
                "Storage class mismatch: expected {expected}, detected {storage}"
            );
        }
    }
    Ok(json!({"git":git,"storage_class":storage}))
}

pub fn storage_class(path: &Path) -> &'static str {
    #[cfg(windows)]
    {
        windows::storage_class(path).unwrap_or("unknown")
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        "unknown"
    }
}

pub fn free_bytes(path: &Path) -> Result<u64> {
    #[cfg(windows)]
    {
        windows::free_bytes(path)
    }
    #[cfg(not(windows))]
    {
        let path = std::path::absolute(path)?;
        sysinfo::Disks::new_with_refreshed_list()
            .iter()
            .filter(|disk| path.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().components().count())
            .map(|disk| disk.available_space())
            .context("Could not inspect repository drive")
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            BusTypeMmc, BusTypeSd, BusTypeUsb, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GetDiskFreeSpaceExW, OPEN_EXISTING,
        },
        System::{IO::DeviceIoControl, Ioctl::*},
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    fn volume(path: &Path) -> Result<PathBuf> {
        let absolute = std::path::absolute(path)?;
        let prefix = absolute.components().next().context("Missing drive")?;
        Ok(PathBuf::from(prefix.as_os_str()))
    }
    pub(super) fn free_bytes(path: &Path) -> Result<u64> {
        let root = PathBuf::from(format!("{}\\", volume(path)?.display()));
        let mut available = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide(&root).as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        ensure!(
            ok != 0,
            "Could not inspect repository drive free space: {}",
            std::io::Error::last_os_error()
        );
        Ok(available)
    }
    fn query<T>(handle: HANDLE, property: STORAGE_PROPERTY_ID) -> Result<T> {
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: property,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut value: T = unsafe { std::mem::zeroed() };
        let mut returned = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                (&query as *const STORAGE_PROPERTY_QUERY).cast(),
                std::mem::size_of_val(&query) as u32,
                (&mut value as *mut T).cast(),
                std::mem::size_of::<T>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        ensure!(
            ok != 0 && returned as usize >= std::mem::size_of::<T>(),
            "Storage property query failed"
        );
        Ok(value)
    }
    pub(super) fn storage_class(path: &Path) -> Result<&'static str> {
        let volume = volume(path)?;
        let drive = volume.to_string_lossy();
        ensure!(
            drive.len() == 2 && drive.ends_with(':'),
            "Only local drive volumes support storage class probing"
        );
        let device = PathBuf::from(format!("\\\\.\\{drive}"));
        let handle = unsafe {
            CreateFileW(
                wide(&device).as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        ensure!(
            handle != INVALID_HANDLE_VALUE,
            "Cannot inspect storage device"
        );
        let handle = Handle(handle);
        if let Ok(descriptor) = query::<STORAGE_DEVICE_DESCRIPTOR>(handle.0, StorageDeviceProperty)
            && [BusTypeUsb, BusTypeSd, BusTypeMmc].contains(&descriptor.BusType)
        {
            return Ok("removable");
        }
        let seek =
            query::<DEVICE_SEEK_PENALTY_DESCRIPTOR>(handle.0, StorageDeviceSeekPenaltyProperty)?;
        Ok(if seek.IncursSeekPenalty { "hdd" } else { "ssd" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_live_config_and_normalized_aliases() {
        let temp = tempfile::tempdir().unwrap();
        let live = temp.path().join("Foxy");
        assert!(assert_isolation_against(&live, &live).is_err());
        assert!(assert_isolation_against(&live.join("..\\Foxy"), &live).is_err());
        assert!(assert_isolation_against(&temp.path().join("testkit-config"), &live).is_ok());
    }
}
