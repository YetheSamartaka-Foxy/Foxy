use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::LinkedTargetPreview;
use crate::core::utils::content_hash::normalize_path;
use crate::core::utils::storage_compat::{VolumeIdentity, volume_identity};

fn record_path(space_dir: &Path) -> PathBuf {
    space_dir.join("storage_targets.json")
}

fn read_records(path: &Path) -> Result<BTreeMap<String, VolumeIdentity>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
            "Stored target identities could not be read. Restore access before updating.".into()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(_) => Err(
            "Stored target identities could not be read. Restore access before updating.".into(),
        ),
    }
}

fn changed_targets_with(
    records_path: &Path,
    roots: &[PathBuf],
    mut identify: impl FnMut(&Path) -> Result<VolumeIdentity, String>,
) -> Result<Vec<LinkedTargetPreview>, String> {
    let records = read_records(records_path)?;
    let mut changes = Vec::new();
    for root in roots {
        let current = identify(root)?;
        let path = root.to_string_lossy().into_owned();
        if records
            .get(&normalize_path(&path))
            .is_some_and(|old| old != &current)
        {
            changes.push(LinkedTargetPreview {
                link_path: path.clone(),
                target_path: path,
                affected_repositories: Vec::new(),
                storage_identity: Some(current),
                storage_changed: true,
            });
        }
    }
    Ok(changes)
}

pub(super) fn changed_targets(
    space_dir: &Path,
    roots: &[PathBuf],
) -> Result<Vec<LinkedTargetPreview>, String> {
    changed_targets_with(&record_path(space_dir), roots, volume_identity)
}

fn bind_targets_with(
    records_path: &Path,
    roots: &[PathBuf],
    approved: &[LinkedTargetPreview],
    mut identify: impl FnMut(&Path) -> Result<VolumeIdentity, String>,
) -> Result<Vec<(PathBuf, VolumeIdentity)>, String> {
    let mut records = read_records(records_path)?;
    let mut captured = Vec::with_capacity(roots.len());
    for root in roots {
        let current = identify(root)?;
        let key = normalize_path(&root.to_string_lossy());
        let changed = records.get(&key).is_some_and(|old| old != &current);
        let explicitly_approved = approved.iter().any(|entry| {
            normalize_path(&entry.link_path) == key
                && normalize_path(&entry.target_path) == key
                && entry.storage_identity.as_ref() == Some(&current)
        });
        if changed && !explicitly_approved {
            return Err(
                "The storage device changed. Review and approve the destination before updating."
                    .into(),
            );
        }
        records.insert(key, current.clone());
        captured.push((root.clone(), current));
    }
    let bytes = serde_json::to_vec_pretty(&records)
        .map_err(|_| "Could not encode target storage approval")?;
    let parent = records_path
        .parent()
        .ok_or_else(|| "Could not prepare target storage approval".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|_| "Could not prepare target storage approval".to_owned())?;
    crate::core::utils::fs_safety::atomic_write(records_path, &bytes)
        .map_err(|_| "Could not save target storage approval".to_string())?;
    Ok(captured)
}

pub(super) fn bind_targets(
    space_dir: &Path,
    roots: &[PathBuf],
    approved: &[LinkedTargetPreview],
) -> Result<Vec<(PathBuf, VolumeIdentity)>, String> {
    bind_targets_with(&record_path(space_dir), roots, approved, volume_identity)
}

pub(super) fn check_link_identity(
    target_path: &Path,
    approved_identity: &VolumeIdentity,
) -> Result<(), String> {
    crate::core::utils::storage_compat::validate_volume_identity(target_path, approved_identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(root: &str, device: &str) -> VolumeIdentity {
        VolumeIdentity {
            root: PathBuf::from(root),
            identity: device.to_string(),
        }
    }

    fn save_records(path: &Path, records: &BTreeMap<String, VolumeIdentity>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(records).unwrap()).unwrap();
    }

    #[test]
    fn same_target_on_a_new_device_requires_approval_for_that_exact_path() {
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("space");
        let records_path = record_path(&space);
        let target = PathBuf::from("R:/Mods/MainRepo");
        save_records(
            &records_path,
            &BTreeMap::from([(
                normalize_path(&target.to_string_lossy()),
                identity("R:/", "device-old"),
            )]),
        );
        let new_identity = identity("R:/", "device-new");
        let identify = |_: &Path| Ok::<VolumeIdentity, String>(new_identity.clone());

        assert!(
            bind_targets_with(&records_path, std::slice::from_ref(&target), &[], identify).is_err()
        );

        let wrong_path_approval = [LinkedTargetPreview {
            link_path: "R:/Mods/OtherRepo".into(),
            target_path: "R:/Mods/OtherRepo".into(),
            affected_repositories: Vec::new(),
            storage_identity: Some(new_identity.clone()),
            storage_changed: true,
        }];
        assert!(
            bind_targets_with(
                &records_path,
                std::slice::from_ref(&target),
                &wrong_path_approval,
                |_| Ok(new_identity.clone()),
            )
            .is_err()
        );

        let approved = [LinkedTargetPreview {
            link_path: target.to_string_lossy().into_owned(),
            target_path: target.to_string_lossy().into_owned(),
            affected_repositories: Vec::new(),
            storage_identity: Some(new_identity.clone()),
            storage_changed: true,
        }];
        assert_eq!(
            bind_targets_with(
                &records_path,
                std::slice::from_ref(&target),
                &approved,
                |_| Ok(new_identity.clone()),
            )
            .unwrap(),
            vec![(target, new_identity)]
        );
    }

    #[test]
    fn failed_approval_save_does_not_authorize_the_write_target() {
        let temp = tempfile::tempdir().unwrap();
        let space = temp.path().join("space");
        std::fs::create_dir_all(&space).unwrap();
        let records_path = space.join("storage_targets.json");
        std::fs::create_dir(&records_path).unwrap();
        let target = PathBuf::from("R:/Mods/MainRepo");
        let current = identity("R:/", "device-new");
        let approved = [LinkedTargetPreview {
            link_path: target.to_string_lossy().into_owned(),
            target_path: target.to_string_lossy().into_owned(),
            affected_repositories: Vec::new(),
            storage_identity: Some(current.clone()),
            storage_changed: true,
        }];

        assert!(
            bind_targets_with(&records_path, &[target], &approved, |_| Ok(current.clone()),)
                .is_err()
        );
    }

    #[test]
    fn records_are_independent_for_each_captured_space() {
        let temp = tempfile::tempdir().unwrap();
        let first_space = temp.path().join("first");
        let second_space = temp.path().join("second");
        let target = PathBuf::from("R:/Mods/MainRepo");
        let old = identity("R:/", "device-old");
        save_records(
            &record_path(&first_space),
            &BTreeMap::from([(normalize_path(&target.to_string_lossy()), old.clone())]),
        );
        let current = identity("R:/", "device-new");
        bind_targets_with(
            &record_path(&second_space),
            std::slice::from_ref(&target),
            &[],
            |_| Ok(current.clone()),
        )
        .unwrap();

        assert_eq!(
            read_records(&record_path(&first_space))
                .unwrap()
                .values()
                .next(),
            Some(&old)
        );
        assert_eq!(
            read_records(&record_path(&second_space))
                .unwrap()
                .values()
                .next(),
            Some(&current)
        );
    }

    #[test]
    fn preview_detects_changed_physical_identity_without_probing_runtime_paths() {
        let temp = tempfile::tempdir().unwrap();
        let records_path = temp.path().join("space/storage_targets.json");
        let target = PathBuf::from("synthetic-target");
        save_records(
            &records_path,
            &BTreeMap::from([(
                normalize_path(&target.to_string_lossy()),
                identity("synthetic-root", "before"),
            )]),
        );
        let previews = changed_targets_with(&records_path, std::slice::from_ref(&target), |_| {
            Ok(identity("synthetic-root", "after"))
        })
        .unwrap();

        assert_eq!(previews.len(), 1);
        assert_eq!(
            previews[0].storage_identity.as_ref().unwrap().identity,
            "after"
        );
        assert_eq!(
            std::fs::read(records_path).unwrap(),
            serde_json::to_vec(&BTreeMap::from([(
                normalize_path(&target.to_string_lossy()),
                identity("synthetic-root", "before"),
            )]))
            .unwrap()
        );
    }

    #[test]
    fn preview_fails_closed_when_volume_identity_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let records_path = temp.path().join("space/storage_targets.json");
        let target = PathBuf::from("synthetic-target");

        assert!(
            changed_targets_with(&records_path, std::slice::from_ref(&target), |_| {
                Err("Could not identify the target storage device".to_owned())
            })
            .is_err()
        );
    }
}
