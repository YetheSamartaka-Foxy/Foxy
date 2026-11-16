use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::super::content_staging::{ContentStageControl, StageEntry, StageEntryKind};
use super::super::deployment_activation::NativeRuntimeManifest;
use super::super::deployment_preparation::DeploymentPreparationManifest;
use super::super::{ServerInstanceId, ServerStore, ServerStoreError};
use super::ReconciliationArtifact;

const MAX_FILES: usize = 1_000_000;
const MAX_DEPTH: usize = 128;
const MAX_MANIFEST: usize = 48 * 1024 * 1024;
type FileMap = BTreeMap<String, (u64, String)>;

#[derive(Clone, Debug)]
pub(super) struct ArtifactInventory {
    pub bytes: u64,
    pub fingerprint: String,
    pub entries: Vec<StageEntry>,
    pub installation: Option<PathBuf>,
}

pub(super) fn inspect(
    store: &ServerStore,
    id: &ServerInstanceId,
    kind: ReconciliationArtifact,
    name: &str,
    directory: &Path,
    control: &ContentStageControl,
) -> Result<ArtifactInventory, ServerStoreError> {
    super::super::store::check_directory_chain(directory)?;
    let metadata = fs::symlink_metadata(directory).map_err(|_| ServerStoreError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || reparse(&metadata) {
        return Err(ServerStoreError::UnsafePath);
    }
    let manifest_path = directory.join("manifest.json");
    super::super::store::check_regular_file(&manifest_path)?;
    let manifest_bytes = super::super::content_staging::read_bounded_controlled(
        &manifest_path,
        MAX_MANIFEST,
        control,
    )?;
    let (entries, installation) = match kind {
        ReconciliationArtifact::ContentStage => {
            let manifest: super::super::content_staging::ContentStageManifest =
                decode(&manifest_bytes)?;
            let hash = blake3::hash(
                &serde_json::to_vec(&manifest).map_err(|_| ServerStoreError::Corrupt)?,
            )
            .to_hex()
            .to_string();
            if name != format!("stage_{hash}")
                || manifest.schema_version != 1
                || manifest.bindings.instance_id != *id
                || manifest.bindings.game_space_id != store.context().space_id()
            {
                return Err(ServerStoreError::Corrupt);
            }
            if store.read_stage_manifest_at(id, name, directory, control)? != manifest {
                return Err(ServerStoreError::StaleRevision);
            }
            (manifest.entries, None)
        }
        ReconciliationArtifact::Preparation => {
            let manifest: DeploymentPreparationManifest = decode(&manifest_bytes)?;
            let hash = blake3::hash(
                &serde_json::to_vec(&manifest).map_err(|_| ServerStoreError::Corrupt)?,
            )
            .to_hex()
            .to_string();
            let record_hash = blake3::hash(
                &serde_json::to_vec(&manifest.record).map_err(|_| ServerStoreError::Corrupt)?,
            )
            .to_hex()
            .to_string();
            if name != format!("prepared_{hash}")
                || manifest.schema_version != 1
                || manifest.record_id != format!("deployment_{record_hash}")
                || manifest.record.instance_id != *id
                || manifest.record.game_space_id != store.context().space_id()
                || manifest.record.game_id != store.context().game_id()
            {
                return Err(ServerStoreError::Corrupt);
            }
            (manifest.entries, None)
        }
        ReconciliationArtifact::Runtime => {
            let manifest: NativeRuntimeManifest = decode(&manifest_bytes)?;
            let fingerprint = blake3::hash(
                &serde_json::to_vec(&manifest).map_err(|_| ServerStoreError::Corrupt)?,
            )
            .to_hex()
            .to_string();
            let runtime_id = format!("runtime_{fingerprint}");
            let expected = super::super::deployment_activation::NativeRuntimeReference {
                runtime_id,
                manifest_fingerprint: fingerprint,
            };
            if name != expected.directory_name()
                || manifest.schema_version != 1
                || manifest.instance_id != *id
                || manifest.game_space_id != store.context().space_id()
            {
                return Err(ServerStoreError::Corrupt);
            }
            let mut entries = manifest.realized_entries;
            for entry in manifest.payload.entries {
                entries.push(StageEntry {
                    path: format!("engine/{}", entry.path),
                    kind: entry.kind,
                    bytes: entry.bytes,
                    blake3: entry.blake3,
                });
            }
            if !entries.iter().any(|entry| entry.path == "engine") {
                entries.push(StageEntry {
                    path: "engine".into(),
                    kind: StageEntryKind::Directory,
                    bytes: 0,
                    blake3: None,
                });
            }
            (entries, Some(manifest.installation.directory))
        }
        ReconciliationArtifact::Leftover => return Err(ServerStoreError::UnsafePath),
    };
    let expected = expected_files(entries, &manifest_bytes)?;
    let (actual, bytes) = walk(directory, control)?;
    if expected != actual {
        return Err(ServerStoreError::Corrupt);
    }
    let actual_entries = to_entries(&actual);
    let encoded = serde_json::to_vec(&actual_entries).map_err(|_| ServerStoreError::Corrupt)?;
    Ok(ArtifactInventory {
        bytes,
        fingerprint: blake3::hash(&encoded).to_hex().to_string(),
        entries: actual_entries,
        installation,
    })
}

pub(super) fn remaining(
    directory: &Path,
    expected: &[StageEntry],
    control: &ContentStageControl,
) -> Result<ArtifactInventory, ServerStoreError> {
    super::super::store::check_directory_chain(directory)?;
    if expected.len() > MAX_FILES || expected.windows(2).any(|pair| pair[0].path >= pair[1].path) {
        return Err(ServerStoreError::Corrupt);
    }
    for entry in expected {
        super::super::content_staging::parse_relative(&entry.path)?;
        match entry.kind {
            StageEntryKind::Directory if entry.bytes == 0 && entry.blake3.is_none() => {}
            StageEntryKind::File
                if entry
                    .blake3
                    .as_ref()
                    .is_some_and(|hash| super::hex_id(hash, "", 64)) => {}
            _ => return Err(ServerStoreError::Corrupt),
        }
    }
    let (actual, bytes) = walk(directory, control)?;
    let expected: BTreeMap<_, _> = expected
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    if actual.iter().any(|(path, value)| {
        expected
            .get(path.as_str())
            .is_none_or(|entry| entry_value(entry) != *value)
    }) {
        return Err(ServerStoreError::Corrupt);
    }
    let entries = to_entries(&actual);
    let fingerprint =
        blake3::hash(&serde_json::to_vec(&entries).map_err(|_| ServerStoreError::Corrupt)?)
            .to_hex()
            .to_string();
    Ok(ArtifactInventory {
        bytes,
        fingerprint,
        entries,
        installation: None,
    })
}

fn decode<T: for<'de> serde::Deserialize<'de>>(bytes: &[u8]) -> Result<T, ServerStoreError> {
    serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)
}

fn expected_files(
    entries: Vec<StageEntry>,
    manifest: &[u8],
) -> Result<BTreeMap<String, (u64, String)>, ServerStoreError> {
    if entries.len() > MAX_FILES {
        return Err(ServerStoreError::Corrupt);
    }
    let mut files = BTreeMap::new();
    for entry in entries {
        super::super::content_staging::parse_relative(&entry.path)?;
        if entry.kind == StageEntryKind::File {
            let hash = entry.blake3.ok_or(ServerStoreError::Corrupt)?;
            if files.insert(entry.path, (entry.bytes, hash)).is_some() {
                return Err(ServerStoreError::Corrupt);
            }
        } else {
            if let Some(before) = files.insert(entry.path, (0, String::new()))
                && before != (0, String::new())
            {
                return Err(ServerStoreError::Corrupt);
            }
        }
    }
    if files
        .insert(
            "manifest.json".into(),
            (
                manifest.len() as u64,
                blake3::hash(manifest).to_hex().to_string(),
            ),
        )
        .is_some()
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(files)
}

fn walk(root: &Path, control: &ContentStageControl) -> Result<(FileMap, u64), ServerStoreError> {
    let mut files = BTreeMap::new();
    let mut bytes = 0u64;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(ServerStoreError::UnsafePath);
        }
        for item in fs::read_dir(&dir).map_err(|_| ServerStoreError::Io)? {
            control.checkpoint()?;
            if files.len() >= MAX_FILES {
                return Err(ServerStoreError::UnsafePath);
            }
            let item = item.map_err(|_| ServerStoreError::Io)?;
            let path = item.path();
            let meta = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
            if meta.file_type().is_symlink() || reparse(&meta) {
                return Err(ServerStoreError::UnsafePath);
            }
            let rel = path
                .strip_prefix(root)
                .map_err(|_| ServerStoreError::UnsafePath)?
                .to_str()
                .ok_or(ServerStoreError::UnsafePath)?
                .replace('\\', "/");
            super::super::content_staging::parse_relative(&rel)?;
            if meta.is_dir() {
                files.insert(rel, (0, String::new()));
                stack.push((path, depth + 1));
                continue;
            }
            if !meta.is_file() {
                return Err(ServerStoreError::UnsafePath);
            }
            let mut file = fs::File::open(&path)
                .map_err(|_| ServerStoreError::Io)?
                .take(meta.len().checked_add(1).ok_or(ServerStoreError::Corrupt)?);
            let mut hasher = blake3::Hasher::new();
            let mut count = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                control.checkpoint()?;
                let n = file.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
                count = count
                    .checked_add(n as u64)
                    .ok_or(ServerStoreError::Corrupt)?;
            }
            if count != meta.len() {
                return Err(ServerStoreError::Corrupt);
            }
            bytes = bytes.checked_add(count).ok_or(ServerStoreError::Corrupt)?;
            files.insert(rel, (count, hasher.finalize().to_hex().to_string()));
        }
    }
    Ok((files, bytes))
}

fn to_entries(files: &BTreeMap<String, (u64, String)>) -> Vec<StageEntry> {
    files
        .iter()
        .map(|(path, (bytes, hash))| StageEntry {
            path: path.clone(),
            kind: if hash.is_empty() {
                StageEntryKind::Directory
            } else {
                StageEntryKind::File
            },
            bytes: *bytes,
            blake3: (!hash.is_empty()).then(|| hash.clone()),
        })
        .collect()
}

fn entry_value(entry: &StageEntry) -> (u64, String) {
    (entry.bytes, entry.blake3.clone().unwrap_or_default())
}

fn reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}
