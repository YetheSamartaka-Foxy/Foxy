use std::{io, path::Path};

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Protected handoff is unavailable on this platform.",
    ))
}

pub(super) fn create_directory(_: &Path) -> io::Result<()> {
    unsupported()
}
pub(super) fn verify_directory(_: &Path) -> io::Result<()> {
    unsupported()
}
pub(super) fn publish_new(_: &Path, _: &str, _: &[u8]) -> io::Result<()> {
    unsupported()
}
pub(super) fn read(_: &Path, _: &str) -> io::Result<Vec<u8>> {
    unsupported()
}
pub(super) fn remove_exact(_: &Path, _: &str, _: &[u8]) -> io::Result<()> {
    unsupported()
}
