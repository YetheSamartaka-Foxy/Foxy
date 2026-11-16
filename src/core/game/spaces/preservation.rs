use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn claim_for_removal(space_dir: &Path) -> Result<Option<fs::File>, String> {
    if !space_dir.exists() {
        return Ok(None);
    }
    let lock_path = space_dir.join("database.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Game-space ownership lock must not be a symbolic link".to_string());
    }
    let claim = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| format!("Could not inspect game-space ownership: {error}"))?;
    claim.try_lock().map_err(|error| {
        format!("Game space cannot be removed while another Foxy process owns it: {error}")
    })?;
    Ok(Some(claim))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct PrivatePayloadPreview {
    pub workshop_item_ids: Vec<String>,
    pub extra_file_ids: Vec<String>,
}

pub(super) fn preview(space_dir: &Path) -> Result<PrivatePayloadPreview, String> {
    let mut workshop_item_ids = directory_names(&space_dir.join("workshop").join("frozen"))?;
    for relative in ["reforger/addons/frozen", "reforger/addons/live"] {
        workshop_item_ids.extend(
            directory_names(&space_dir.join(relative))?
                .into_iter()
                .map(|id| format!("{relative}/{id}")),
        );
    }
    workshop_item_ids.sort();
    Ok(PrivatePayloadPreview {
        workshop_item_ids,
        extra_file_ids: directory_names(&space_dir.join("extra_files"))?,
    })
}

pub(super) fn preserve(
    space_dir: &Path,
    destination_root: &Path,
    space_id: &str,
) -> Result<PathBuf, String> {
    let source = fs::canonicalize(space_dir)
        .map_err(|err| format!("Failed to resolve game space for preservation: {err}"))?;
    let destination_root = resolve_future_path(destination_root)?;
    if destination_root.starts_with(&source) {
        return Err("Preservation destination must be outside the game space".to_string());
    }
    let destination = destination_root.join(space_id);
    if destination.exists() {
        return Err(format!(
            "Preservation destination already exists: {}",
            destination.display()
        ));
    }
    fs::create_dir_all(&destination_root)
        .map_err(|err| format!("Failed to create preservation directory: {err}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = destination_root.join(format!(
        ".{space_id}.foxy-preserve-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&staging)
        .map_err(|err| format!("Failed to create preservation staging directory: {err}"))?;
    let result = (|| {
        for relative in ["workshop.json", "extra_files.json", "reforger_addons.json"] {
            let source = space_dir.join(relative);
            if source.exists() {
                copy_file(&source, &staging.join(relative))?;
            }
        }
        for relative in [
            "workshop/frozen",
            "extra_files",
            "reforger/addons/live",
            "reforger/addons/frozen",
        ] {
            let source = space_dir.join(relative);
            if source.exists() {
                copy_tree(&source, &staging.join(relative))?;
            }
        }
        fs::rename(&staging, &destination)
            .map_err(|err| format!("Failed to finalize preserved payloads: {err}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result.map(|()| destination)
}

fn resolve_future_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("Failed to resolve preservation destination: {err}"))?
            .join(path)
    };
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other.as_os_str()),
        }
    }
    let mut existing = lexical.clone();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(|name| name.to_os_string()) else {
            return Err("Preservation destination has no existing parent".to_string());
        };
        suffix.push(name);
        existing.pop();
    }
    let mut resolved = fs::canonicalize(existing)
        .map_err(|err| format!("Failed to resolve preservation destination: {err}"))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn directory_names(path: &Path) -> Result<Vec<String>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(path)
        .map_err(|err| format!("Failed to read managed payload directory: {err}"))?
    {
        let entry = entry.map_err(|err| format!("Failed to read managed payload entry: {err}"))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|err| format!("Failed to inspect private payload: {err}"))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "Unsupported private payload entry: {}",
            source.display()
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create preservation folder: {err}"))?;
    }
    fs::copy(source, destination)
        .map_err(|err| format!("Failed to preserve private payload: {err}"))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|err| format!("Failed to inspect private payload: {err}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "Unsupported private payload entry: {}",
            source.display()
        ));
    }
    fs::create_dir_all(destination)
        .map_err(|err| format!("Failed to create preservation folder: {err}"))?;
    for entry in
        fs::read_dir(source).map_err(|err| format!("Failed to read private payload: {err}"))?
    {
        let entry = entry.map_err(|err| format!("Failed to read private payload entry: {err}"))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|err| format!("Failed to inspect private payload entry: {err}"))?;
        if file_type.is_dir() {
            copy_tree(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            copy_file(&source_path, &destination_path)?;
        } else {
            return Err(format!(
                "Unsupported private payload entry: {}",
                source_path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preservation_copies_frozen_and_extra_file_payloads() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("space");
        fs::create_dir_all(source.join("workshop/frozen/123/hash_mod")).expect("workshop");
        fs::create_dir_all(source.join("extra_files/entry-a")).expect("extra files");
        fs::create_dir_all(source.join("reforger/addons/live/596ABCDEF0123456"))
            .expect("Reforger live payload");
        fs::create_dir_all(source.join("reforger/addons/frozen/596ABCDEF0123456"))
            .expect("Reforger frozen payload");
        fs::write(source.join("workshop/frozen/123/hash_mod/file.pbo"), b"pbo")
            .expect("frozen file");
        fs::write(source.join("extra_files/entry-a/payload.bin"), b"payload")
            .expect("extra payload");
        fs::write(
            source.join("reforger/addons/live/596ABCDEF0123456/addon.gproj"),
            b"live",
        )
        .expect("Reforger live file");
        fs::write(
            source.join("reforger/addons/frozen/596ABCDEF0123456/addon.gproj"),
            b"frozen",
        )
        .expect("Reforger frozen file");
        fs::write(source.join("workshop.json"), b"{}").expect("workshop metadata");
        fs::write(source.join("reforger_addons.json"), b"Reforger store")
            .expect("Reforger metadata");
        let preview = preview(&source).expect("preview");
        assert!(
            preview
                .workshop_item_ids
                .contains(&"reforger/addons/live/596ABCDEF0123456".to_string())
        );
        assert!(
            preview
                .workshop_item_ids
                .contains(&"reforger/addons/frozen/596ABCDEF0123456".to_string())
        );
        let target_root = temp.path().join("export");
        let target = preserve(&source, &target_root, "space-a").expect("preserve");
        assert_eq!(
            fs::read(target.join("workshop/frozen/123/hash_mod/file.pbo")).unwrap(),
            b"pbo"
        );
        assert_eq!(
            fs::read(target.join("extra_files/entry-a/payload.bin")).unwrap(),
            b"payload"
        );
        assert_eq!(fs::read(target.join("workshop.json")).unwrap(), b"{}");
        assert_eq!(
            fs::read(target.join("reforger/addons/live/596ABCDEF0123456/addon.gproj")).unwrap(),
            b"live"
        );
        assert_eq!(
            fs::read(target.join("reforger/addons/frozen/596ABCDEF0123456/addon.gproj")).unwrap(),
            b"frozen"
        );
        assert_eq!(
            fs::read(target.join("reforger_addons.json")).unwrap(),
            b"Reforger store"
        );
    }

    #[test]
    fn preservation_failure_keeps_source_and_removes_staging() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("space");
        fs::create_dir_all(source.join("extra_files/entry-a")).expect("extra files");
        let target_root = temp.path().join("export");
        fs::write(&target_root, b"not a directory").expect("blocking destination");
        assert!(preserve(&source, &target_root, "space-a").is_err());
        assert!(source.join("extra_files/entry-a").exists());
        assert!(target_root.is_file());
    }
}
