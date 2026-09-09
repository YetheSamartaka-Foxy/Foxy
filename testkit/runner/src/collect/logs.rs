use anyhow::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Offset {
    length: usize,
    head: Vec<u8>,
}
pub type Offsets = BTreeMap<PathBuf, Offset>;
fn paths(run: &Path, config: &Path) -> Vec<PathBuf> {
    let mut paths = vec![run.join("app.out"), run.join("app.log")];
    paths.extend(
        walkdir::WalkDir::new(config)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|ext| ext == "log")
            })
            .map(|entry| entry.into_path()),
    );
    paths.retain(|path| path.is_file());
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    paths
}
pub fn offsets(run: &Path, config: &Path) -> Result<Offsets> {
    paths(run, config)
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path)?;
            Ok((
                path,
                Offset {
                    length: bytes.len(),
                    head: bytes[..bytes.len().min(512)].to_vec(),
                },
            ))
        })
        .collect()
}
pub fn delta(offsets: &Offsets, run: &Path, config: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for path in paths(run, config) {
        let bytes = std::fs::read(&path)?;
        let head = &bytes[..bytes.len().min(512)];
        let start = if let Some(old) = offsets.get(&path).filter(|old| old.head == head) {
            if old.length > bytes.len() {
                0
            } else {
                old.length
            }
        } else if offsets.values().any(|old| old.head == head) {
            continue;
        } else {
            0
        };
        parts.push(
            String::from_utf8_lossy(&bytes[start..])
                .trim_start_matches('\u{feff}')
                .to_string(),
        );
    }
    Ok(parts.join(if cfg!(windows) { "\r\n" } else { "\n" }))
}
pub fn corpus(run: &Path, config: &Path) -> Result<String> {
    delta(&Offsets::new(), run, config)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_and_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("app.log");
        std::fs::write(&current, vec![b'a'; 600]).unwrap();
        let before = offsets(dir.path(), dir.path()).unwrap();
        std::fs::rename(&current, dir.path().join("old.log")).unwrap();
        std::fs::write(&current, "new").unwrap();
        assert_eq!(delta(&before, dir.path(), dir.path()).unwrap(), "new");
        std::fs::remove_file(dir.path().join("old.log")).unwrap();
        std::fs::write(&current, vec![b'a'; 550]).unwrap();
        assert_eq!(delta(&before, dir.path(), dir.path()).unwrap().len(), 550);
        std::fs::write(&current, "a").unwrap();
        assert_eq!(delta(&before, dir.path(), dir.path()).unwrap(), "a");
    }
}
