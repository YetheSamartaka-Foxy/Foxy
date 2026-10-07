use super::FsTimer;
use std::fs::{DirEntry, Metadata, ReadDir};
use std::io;
use std::path::Path;

pub(crate) fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let timer = FsTimer::start();
    let result = std::fs::metadata(path);
    timer.stop("metadata", 1);
    result
}

pub(crate) fn exists(path: impl AsRef<Path>) -> bool {
    let timer = FsTimer::start();
    let result = path.as_ref().exists();
    timer.stop("exists", 1);
    result
}

pub(crate) async fn metadata_async(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let timer = FsTimer::start();
    let result = tokio::fs::metadata(path).await;
    timer.stop("metadata", 1);
    result
}

pub(crate) fn is_dir(path: impl AsRef<Path>) -> bool {
    metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

pub(crate) fn read_dir(path: impl AsRef<Path>) -> io::Result<ProfiledReadDir> {
    let timer = FsTimer::start();
    let result = std::fs::read_dir(path).map(ProfiledReadDir);
    timer.stop("read_dir", 1);
    result
}

pub(crate) struct ProfiledReadDir(ReadDir);

impl Iterator for ProfiledReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let timer = FsTimer::start();
        let result = self.0.next();
        timer.stop("read_dir_next", u64::from(result.is_some()));
        result
    }
}
