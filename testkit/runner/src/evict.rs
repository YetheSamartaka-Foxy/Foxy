//! Drop the OS page cache for a payload so a case measures the disk.
//!
//! "Cold" in the ledger only means the first iteration; nothing evicts the
//! files a `setup_once` download just wrote, so an HDD row would otherwise
//! hash at memory speed. Opening a file non-cached makes the Windows cache
//! manager flush and purge its cached pages when no cached handle remains,
//! which is the state a user's first check after a reboot sees.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};

pub fn evict_directory(root: &Path) -> Result<Value> {
    let started = Instant::now();
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut failures = 0u64;
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        match evict_file(entry.path()) {
            Ok(len) => {
                files += 1;
                bytes += len;
            }
            Err(_) => failures += 1,
        }
    }
    Ok(json!({
        "root": root.to_string_lossy(),
        "files": files,
        "bytes": bytes,
        "failures": failures,
        "elapsed_s": started.elapsed().as_secs_f64(),
        "supported": cfg!(windows),
    }))
}

#[cfg(windows)]
fn evict_file(path: &Path) -> Result<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(path)
        .with_context(|| format!("non-cached open of {}", path.display()))?;
    Ok(file.metadata().map(|meta| meta.len()).unwrap_or(0))
}

#[cfg(not(windows))]
fn evict_file(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0))
}
