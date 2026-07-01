/// Platform-specific random-access file I/O helpers.
///
/// These are used by the download transfer layer, range scheduler, and delta
/// patch transfer to write (and read) at arbitrary byte offsets without seeking,
/// enabling safe concurrent access from multiple tasks sharing an `Arc<File>`.

/// Write `buf` at `offset` in `file`, retrying partial writes on Windows.
#[cfg(unix)]
pub(crate) fn write_at(file: &std::fs::File, offset: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

#[cfg(windows)]
pub(crate) fn write_at(file: &std::fs::File, offset: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut written = 0;
    while written < buf.len() {
        let n = file.seek_write(&buf[written..], offset + written as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "write_at wrote zero bytes",
            ));
        }
        written += n;
    }
    Ok(())
}

/// Read exactly `buf.len()` bytes from `offset` in `file`.
#[cfg(unix)]
pub(crate) fn read_at(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_at(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut pos = 0;
    while pos < buf.len() {
        let n = file.seek_read(&mut buf[pos..], offset + pos as u64)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read_at unexpected eof",
            ));
        }
        pos += n;
    }
    Ok(())
}

/// Maximum retry attempts for file removal when blocked by a transient lock.
const REMOVE_RETRY_ATTEMPTS: u32 = 3;

/// Base delay between removal retries (doubles each attempt).
const REMOVE_RETRY_BASE_MS: u64 = 100;

/// Check whether an I/O error is a transient file-lock that may clear on retry.
///
/// On Windows this covers `PermissionDenied` (OS error 5 - "Access is denied")
/// which commonly occurs when OneDrive, antivirus, or indexing services hold a
/// brief lock on a file.
fn is_transient_lock_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
}

/// Remove a file, retrying on transient `PermissionDenied` errors (async).
///
/// Returns `Ok(())` if the file was removed or did not exist.
/// After exhausting retries the last error is returned.
pub(crate) async fn retry_remove_file(path: &std::path::Path) -> std::io::Result<()> {
    for attempt in 0..REMOVE_RETRY_ATTEMPTS {
        match tokio::fs::remove_file(path).await {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) if is_transient_lock_error(&err) && attempt + 1 < REMOVE_RETRY_ATTEMPTS => {
                let delay = REMOVE_RETRY_BASE_MS * (1 << attempt);
                log::debug!(
                    "Retrying file removal in {}ms (attempt {}/{}): {}",
                    delay,
                    attempt + 1,
                    REMOVE_RETRY_ATTEMPTS,
                    path.display()
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(err) => return Err(err),
        }
    }
    // Unreachable, but satisfy the compiler
    tokio::fs::remove_file(path).await
}

/// Remove a file, retrying on transient `PermissionDenied` errors (sync).
///
/// Returns `Ok(())` if the file was removed or did not exist.
/// After exhausting retries the last error is returned.
pub(crate) fn retry_remove_file_sync(path: &std::path::Path) -> std::io::Result<()> {
    for attempt in 0..REMOVE_RETRY_ATTEMPTS {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) if is_transient_lock_error(&err) && attempt + 1 < REMOVE_RETRY_ATTEMPTS => {
                let delay = REMOVE_RETRY_BASE_MS * (1 << attempt);
                log::debug!(
                    "Retrying file removal in {}ms (attempt {}/{}): {}",
                    delay,
                    attempt + 1,
                    REMOVE_RETRY_ATTEMPTS,
                    path.display()
                );
                std::thread::sleep(std::time::Duration::from_millis(delay));
            }
            Err(err) => return Err(err),
        }
    }
    // Unreachable, but satisfy the compiler
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn write_at_and_read_at_roundtrip() {
        let dir = std::env::temp_dir().join("foxy_file_io_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("roundtrip.bin");

        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(64).unwrap();

        write_at(&file, 10, b"hello").unwrap();
        write_at(&file, 30, b"world").unwrap();

        let mut buf = [0u8; 5];
        read_at(&file, 10, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");

        read_at(&file, 30, &mut buf).unwrap();
        assert_eq!(&buf, b"world");

        drop(file);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn write_at_offset_zero() {
        let dir = std::env::temp_dir().join("foxy_file_io_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("offset_zero.bin");

        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(16).unwrap();

        write_at(&file, 0, b"start").unwrap();

        let mut buf = vec![0u8; 16];
        let mut f = std::fs::File::open(&path).unwrap();
        f.read_exact(&mut buf).unwrap();
        assert_eq!(&buf[..5], b"start");

        drop(f);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}
