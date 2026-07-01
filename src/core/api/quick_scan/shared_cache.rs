use super::super::*;
use super::file_state::{AddonFolderState, LocalFileState};
use super::persistent_cache::PersistentAddonHashEntry;

#[derive(Default)]
pub(crate) struct QuickScanSharedCache {
    pub(super) addon_state_by_path: HashMap<String, AddonFolderState>,
    pub(super) file_state_by_path: HashMap<String, LocalFileState>,
    pub(super) persistent_addon_hash_by_path: HashMap<String, PersistentAddonHashEntry>,
    pub(super) persistent_dirty: bool,
}
