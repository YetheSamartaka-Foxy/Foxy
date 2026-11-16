use std::sync::atomic::{AtomicU64, Ordering};

static BYTES_WRITTEN: AtomicU64 = AtomicU64::new(0);

pub(crate) fn bytes_written_total() -> u64 {
    BYTES_WRITTEN.load(Ordering::Relaxed)
}

pub(crate) fn note_written(bytes: u64) {
    BYTES_WRITTEN.fetch_add(bytes, Ordering::Relaxed);
}

#[cfg(unix)]
pub(crate) fn write_at(file: &std::fs::File, offset: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let result = file.write_all_at(buf, offset);
    profiled.stop("write_at", buf.len() as u64);
    if result.is_ok() {
        note_written(buf.len() as u64);
    }
    result
}

#[cfg(windows)]
pub(crate) fn write_at(file: &std::fs::File, offset: u64, buf: &[u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let profiled = crate::core::utils::profiling::FsTimer::start();
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
    profiled.stop("write_at", buf.len() as u64);
    note_written(buf.len() as u64);
    Ok(())
}

#[cfg(unix)]
pub(crate) fn read_at(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let bytes = buf.len() as u64;
    let result = file.read_exact_at(buf, offset);
    profiled.stop("read_at", bytes);
    result
}

#[cfg(windows)]
pub(crate) fn read_at(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let bytes = buf.len() as u64;
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
    profiled.stop("read_at", bytes);
    Ok(())
}

const REMOVE_RETRY_ATTEMPTS: u32 = 3;

const REMOVE_RETRY_BASE_MS: u64 = 100;

/// Windows access-denied errors can be transient locks held by antivirus, indexing or cloud-sync services.
fn is_transient_lock_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied
}

/// Missing files count as removed; exhausted transient-error retries return the final failure.
pub(crate) async fn retry_remove_file(path: &std::path::Path) -> std::io::Result<()> {
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let result = retry_remove_file_inner(path).await;
    profiled.stop("remove", 0);
    result
}

async fn retry_remove_file_inner(path: &std::path::Path) -> std::io::Result<()> {
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

    tokio::fs::remove_file(path).await
}

/// Missing files count as removed; exhausted transient-error retries return the final failure.
#[cfg(test)]
pub(crate) fn retry_remove_file_sync(path: &std::path::Path) -> std::io::Result<()> {
    let profiled = crate::core::utils::profiling::FsTimer::start();
    let result = retry_remove_file_sync_inner(path);
    profiled.stop("remove", 0);
    result
}

#[cfg(test)]
fn retry_remove_file_sync_inner(path: &std::path::Path) -> std::io::Result<()> {
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

    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn write_at_and_read_at_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");

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
    }

    #[test]
    fn write_at_offset_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offset_zero.bin");

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
    }
}
