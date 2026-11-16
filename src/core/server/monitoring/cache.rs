use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{
    super::{InstanceRuntimeStatus, ServerStore, ServerStoreError},
    MonitoringSnapshot,
};

const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;
/// Readers resolve a digest from owner status before opening the file, so recent snapshots must
/// outlive a burst of writes.
pub(super) const RETAINED_SNAPSHOTS: usize = 8;

fn read_bytes(path: &Path) -> Result<Vec<u8>, ServerStoreError> {
    super::super::content_staging::read_bounded_controlled(
        path,
        MAX_SNAPSHOT_BYTES,
        &super::super::ContentStageControl::default(),
    )
}

pub(super) struct CacheWriter {
    root: PathBuf,
    owner: String,
    run: u64,
    retained: VecDeque<(PathBuf, String)>,
}

fn path(root: &Path, owner: &str, run: u64, digest: &str) -> Result<PathBuf, ServerStoreError> {
    if owner.is_empty()
        || owner.len() > 128
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || run == 0
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(root.join(format!("observation_{owner}_{run}_{digest}.json")))
}

impl CacheWriter {
    pub(super) fn new(root: PathBuf, status: &InstanceRuntimeStatus) -> Self {
        let root = fs::canonicalize(&root).unwrap_or(root);
        Self {
            root,
            owner: status.owner_id.clone(),
            run: status.run_generation,
            retained: VecDeque::new(),
        }
    }

    pub(super) fn write(
        &mut self,
        snapshot: &MonitoringSnapshot,
    ) -> Result<String, ServerStoreError> {
        super::super::store::check_directory_chain(&self.root)?;
        let bytes = serde_json::to_vec(snapshot).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(ServerStoreError::Corrupt);
        }
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let destination = path(&self.root, &self.owner, self.run, &digest)?;
        if let Some(position) = self
            .retained
            .iter()
            .position(|(_, retained)| retained == &digest)
            && read_bytes(&destination).is_ok_and(|current| current == bytes)
        {
            let entry = self.retained.remove(position).expect("retained snapshot");
            self.retained.push_back(entry);
            return Ok(digest);
        }
        let mut temporary = tempfile::Builder::new()
            .prefix(".observation-")
            .tempfile_in(&self.root)
            .map_err(|_| ServerStoreError::Io)?;
        temporary
            .write_all(&bytes)
            .map_err(|_| ServerStoreError::Io)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| ServerStoreError::Io)?;
        match temporary.persist_noclobber(&destination) {
            Ok(_) => self.retained.push_back((destination, digest.clone())),
            Err(_) => return Err(ServerStoreError::Io),
        }
        while self.retained.len() > RETAINED_SNAPSHOTS {
            let (old, expected) = self.retained.pop_front().expect("retained snapshot");
            if let Ok(bytes) = read_bytes(&old)
                && blake3::hash(&bytes).to_hex().as_str() == expected
                && super::super::store::check_directory_chain(&self.root).is_ok()
            {
                let _ = fs::remove_file(old);
            }
        }
        Ok(digest)
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        if super::super::store::check_directory_chain(&self.root).is_err() {
            return;
        }
        for (path, expected) in &self.retained {
            if let Ok(bytes) = read_bytes(path)
                && blake3::hash(&bytes).to_hex().as_str() == expected
            {
                let _ = fs::remove_file(path);
            }
        }
    }
}

pub(super) fn read(
    store: &ServerStore,
    status: &InstanceRuntimeStatus,
) -> Result<Option<MonitoringSnapshot>, ServerStoreError> {
    let Some(digest) = &status.monitoring_fingerprint else {
        return Ok(None);
    };
    let root = store.record_root(&status.instance_id).join("runtime-state");
    super::super::store::check_directory_chain(&root)?;
    let file = path(&root, &status.owner_id, status.run_generation, digest)?;
    let bytes = read_bytes(&file)?;
    if blake3::hash(&bytes).to_hex().as_str() != digest {
        return Err(ServerStoreError::Corrupt);
    }
    let snapshot: MonitoringSnapshot =
        serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if snapshot.schema_version != 1
        || snapshot.context_fingerprint != store.activation_context_fingerprint()
        || snapshot.instance_id != status.instance_id
        || snapshot.owner_id != status.owner_id
        || snapshot.owner_generation != status.owner_generation
        || snapshot.run_generation != status.run_generation
        || snapshot.activation_generation != status.activation_generation
        || snapshot.runtime_id != status.runtime_id
        || snapshot.resources.len() > 60
        || snapshot.logs.entries.len() > 128
        || snapshot.query.attempt_history.len() > 16
        || snapshot
            .headless
            .as_ref()
            .is_some_and(|headless| !headless.valid())
    {
        return Err(ServerStoreError::Corrupt);
    }
    Ok(Some(snapshot))
}
