use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(super) type Inventory = BTreeMap<PathBuf, (u64, blake3::Hash)>;

pub(super) fn snapshot_selected_payload(root: &Path) -> io::Result<Inventory> {
    let app_id = root.join("steam_appid.txt");
    if String::from_utf8_lossy(&read_bounded_bytes(&app_id, 64)?).trim() != "107410" {
        return Err(io::Error::other("Native payload identity is not Arma 3"));
    }
    let mut result = Inventory::new();
    #[cfg(windows)]
    let executable = "arma3server_x64.exe";
    #[cfg(target_os = "linux")]
    let executable = "arma3server_x64";
    snapshot_file(root, Path::new(executable), &mut result)?;
    let mut root_count = 0;
    for entry in fs::read_dir(root)? {
        root_count += 1;
        if root_count > 100_000 {
            return Err(io::Error::other("Native root inventory exceeded bound"));
        }
        let entry = entry?;
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if extension.eq_ignore_ascii_case("dll") || extension == "so" {
            snapshot_file(root, &PathBuf::from(entry.file_name()), &mut result)?;
        }
    }
    for name in [
        "Addons",
        "Dta",
        "Dll",
        "linux64",
        "Keys",
        "Expansion",
        "AoW",
        "Argo",
        "Curator",
        "Enoch",
        "Heli",
        "Jets",
        "Kart",
        "Mark",
        "Orange",
        "Tacops",
        "Tank",
    ] {
        let mut matching = None;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
            {
                if matching.is_some() {
                    return Err(io::Error::other("Ambiguous native payload folder"));
                }
                matching = Some(PathBuf::from(entry.file_name()));
            }
        }
        if let Some(relative) = matching {
            snapshot_directory(root, &relative, &mut result, 0)?;
        }
    }
    snapshot_file(root, Path::new("steam_appid.txt"), &mut result)?;
    Ok(result)
}

pub(super) fn snapshot_tree(root: &Path) -> io::Result<Inventory> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other(
            "Inventory root must be an ordinary directory",
        ));
    }
    let mut result = Inventory::new();
    snapshot_directory(root, Path::new("."), &mut result, 0)?;
    Ok(result)
}

fn snapshot_directory(
    root: &Path,
    relative: &Path,
    result: &mut Inventory,
    depth: usize,
) -> io::Result<()> {
    if depth > 16 || result.len() >= 100_000 {
        return Err(io::Error::other("Native inventory exceeded bounds"));
    }
    let dir = root.join(relative);
    let metadata = fs::symlink_metadata(&dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::other("Native payload contains unsafe directory"));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::other("Native payload contains a link"));
        }
        if metadata.is_dir() {
            snapshot_directory(root, &child, result, depth + 1)?;
        } else if metadata.is_file() {
            snapshot_file(root, &child, result)?;
        } else {
            return Err(io::Error::other("Native payload contains special file"));
        }
    }
    Ok(())
}

fn snapshot_file(root: &Path, relative: &Path, result: &mut Inventory) -> io::Result<()> {
    if result.len() >= 100_000 {
        return Err(io::Error::other(
            "Native inventory exceeded file count bound",
        ));
    }
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > 8 * 1024 * 1024 * 1024
    {
        return Err(io::Error::other(
            "Native payload file is unsafe or too large",
        ));
    }
    let size = metadata.len();
    let mut file = fs::File::open(path)?;
    let mut reader = (&mut file).take(size + 1);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut read = 0u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        read = read.saturating_add(count as u64);
        hasher.update(&buffer[..count]);
    }
    if read != size || file.metadata()?.len() != size {
        return Err(io::Error::other("Native payload changed while hashing"));
    }
    result.insert(relative.to_path_buf(), (size, hasher.finalize()));
    Ok(())
}

fn read_bounded_bytes(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other("Native payload identity file is invalid"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other(
            "Native payload identity file exceeds bound",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("steam_appid.txt"), b"107410\n").unwrap();
        fs::write(root.path().join(executable_name()), b"server executable").unwrap();
        fs::create_dir(root.path().join("Addons")).unwrap();
        fs::write(root.path().join("Addons/config.bin"), b"alpha").unwrap();
        root
    }

    #[cfg(windows)]
    fn executable_name() -> &'static str {
        "arma3server_x64.exe"
    }

    #[cfg(target_os = "linux")]
    fn executable_name() -> &'static str {
        "arma3server_x64"
    }

    #[test]
    fn selected_native_inventory_tracks_bytes_and_paths() {
        let root = fixture();
        let first = snapshot_selected_payload(root.path()).unwrap();
        fs::write(root.path().join("Addons/config.bin"), b"bravo").unwrap();
        let changed = snapshot_selected_payload(root.path()).unwrap();
        assert_eq!(first.len(), changed.len());
        assert_ne!(first, changed);
        fs::write(root.path().join("Addons/second.bin"), b"gamma").unwrap();
        let added = snapshot_selected_payload(root.path()).unwrap();
        assert_ne!(changed, added);
    }

    #[cfg(unix)]
    #[test]
    fn selected_native_inventory_rejects_links() {
        use std::os::unix::fs::symlink;

        let root = fixture();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), root.path().join("Addons/external")).unwrap();
        assert!(snapshot_selected_payload(root.path()).is_err());
    }
}
