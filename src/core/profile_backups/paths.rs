use anyhow::{Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn restore_roots(custom_profiles_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(docs) = dirs::document_dir() {
        roots.push(docs.join("Arma 3"));
        roots.push(docs.join("Arma 3 - Other Profiles"));
    }
    if let Some(custom) = custom_profiles_dir {
        roots.push(custom.to_path_buf());
    }
    #[cfg(target_os = "linux")]
    for library in crate::core::steam::steam_library_roots("") {
        let users = library.join("steamapps/compatdata/107410/pfx/drive_c/users");
        if let Ok(entries) = fs::read_dir(users) {
            for entry in entries.flatten() {
                let docs = entry.path().join("Documents");
                roots.push(docs.join("Arma 3"));
                roots.push(docs.join("Arma 3 - Other Profiles"));
            }
        }
    }
    roots
}

pub(super) fn validate_restore_target(target: &Path, roots: &[PathBuf]) -> Result<()> {
    if !target.is_absolute() || !roots.iter().any(|root| path_starts_with(target, root)) {
        bail!("profile backup target is outside known profile roots");
    }
    check_path(target, false)
}

pub(super) fn check_path(path: &Path, require_dir: bool) -> Result<()> {
    use std::path::Component;
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        bail!("profile backup path contains traversal components");
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut cursor = PathBuf::new();
    for c in absolute.components() {
        cursor.push(c.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("profile backup path contains a link")
            }
            Ok(m) if cursor != absolute && !m.is_dir() => {
                bail!("profile backup path ancestor is not a directory")
            }
            Ok(m) if cursor == absolute && require_dir && !m.is_dir() => {
                bail!("profile backup path is not a directory")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn path_starts_with(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        let path = path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let root = root
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let root = root.trim_end_matches('/');
        path == root || path.starts_with(&format!("{root}/"))
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}
