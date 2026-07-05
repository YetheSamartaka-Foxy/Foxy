use super::super::fs_watcher::normalize_path_for_match;
use super::super::*;
use super::content_hash::{
    calculate_fast_addon_folder_content_hash, calculate_fast_file_content_hash,
};
use super::persistent_cache::AddonRootFingerprint;
use super::shared_cache::QuickScanSharedCache;

#[derive(Clone, Default)]
pub(super) struct LocalFileState {
    pub(super) exists: bool,
    pub(super) length: u64,
    pub(super) content_hash: String,
    pub(super) hashed_for_length: Option<u64>,
}

#[derive(Clone, Default)]
pub(super) struct AddonFolderState {
    pub(super) exists: bool,
    pub(super) content_hash: String,
}

/// Probe using a fingerprint the caller already walked.
pub(super) fn probe_addon_folder_state_with_fingerprint(
    path: &str,
    fingerprint: &AddonRootFingerprint,
) -> AddonFolderState {
    if !fingerprint.exists || !fingerprint.is_dir {
        return AddonFolderState::default();
    }
    let content_hash = calculate_fast_addon_folder_content_hash(path).unwrap_or_default();
    AddonFolderState {
        exists: true,
        content_hash,
    }
}

fn probe_local_file_state(path: &str, expected_length: u64) -> LocalFileState {
    let trimmed = path.trim().to_string();
    if trimmed.is_empty() {
        return LocalFileState::default();
    }
    match std::fs::metadata(&trimmed) {
        Ok(meta) => {
            let size_ok = meta.is_file() && meta.len() == expected_length;
            let content_hash = if size_ok {
                calculate_fast_file_content_hash(&trimmed).unwrap_or_default()
            } else {
                String::new()
            };
            LocalFileState {
                exists: meta.is_file(),
                length: meta.len(),
                content_hash,
                hashed_for_length: if size_ok { Some(expected_length) } else { None },
            }
        }
        Err(_) => LocalFileState::default(),
    }
}

pub(super) async fn resolve_local_file_state(
    local_cache: &mut HashMap<String, LocalFileState>,
    shared_cache: Option<&Arc<Mutex<QuickScanSharedCache>>>,
    local_path: &str,
    expected_length: u64,
) -> LocalFileState {
    let path_key = normalize_path_for_match(local_path);
    if let Some(state) = local_cache.get(&path_key).cloned()
        && (!state.exists
            || state.length != expected_length
            || state.hashed_for_length == Some(expected_length))
    {
        return state;
    }

    if let Some(shared) = shared_cache {
        let cached = match shared.lock() {
            Ok(guard) => guard.file_state_by_path.get(&path_key).cloned(),
            Err(poisoned) => poisoned
                .into_inner()
                .file_state_by_path
                .get(&path_key)
                .cloned(),
        };
        if let Some(state) = cached
            && (!state.exists
                || state.length != expected_length
                || state.hashed_for_length == Some(expected_length))
        {
            local_cache.insert(path_key, state.clone());
            return state;
        }
    }

    let local_path_owned = local_path.to_string();
    let computed = tokio::task::spawn_blocking(move || {
        probe_local_file_state(&local_path_owned, expected_length)
    })
    .await
    .unwrap_or_default();

    local_cache.insert(path_key.clone(), computed.clone());
    if let Some(shared) = shared_cache {
        match shared.lock() {
            Ok(mut guard) => {
                guard.file_state_by_path.insert(path_key, computed.clone());
            }
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.file_state_by_path.insert(path_key, computed.clone());
            }
        }
    }
    computed
}
