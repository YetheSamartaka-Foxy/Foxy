use crate::core::db::{DbErr, DbRow};
use crate::core::models::trait_has_local_checksum::HasLocalChecksum;
use serde::{Deserialize, Serialize};

/// Column list for selecting an addon row into a [`FoxyMod`] via [`FoxyMod::from_row`].
pub(crate) const ADDON_COLUMNS: &str = "id, name, display_name, remote_path, local_path, client_side, enabled, \
     local_checksum, remote_checksum, local_content_hash, required, data_order";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct FoxyMod {
    /// Database id if applicable
    pub(crate) id: u64,
    /// Mod name
    pub(crate) name: String,
    /// Display name extracted from the local addon mod.cpp, if available
    pub(crate) display_name: String,
    /// Mod remote path
    pub(crate) remote_path: String,
    /// Mod local path
    pub(crate) local_path: String,
    /// Whether the remote repository marks this addon as client-side only.
    pub(crate) client_side: bool,
    #[serde(skip)]
    /// Mod remote checksum
    pub(crate) remote_checksum: String,
    /// Mod local checksum
    pub(crate) local_checksum: String,
    /// Fast local content hash for quick checks
    pub(crate) local_content_hash: String,
    /// Is mod enabled
    pub(crate) enabled: bool,
    /// Is mod required
    pub(crate) required: bool,
    /// Remote data order
    pub(crate) data_order: i64,
}

impl FoxyMod {
    /// Materialize a [`FoxyMod`] from a seam [`DbRow`] selected with [`ADDON_COLUMNS`].
    pub(crate) fn from_row(row: &DbRow) -> Result<Self, DbErr> {
        Ok(FoxyMod {
            id: row.get_i64("id")? as u64,
            name: row.get_string("name")?,
            display_name: row.get_string("display_name")?,
            remote_path: row.get_string("remote_path")?,
            local_path: row.get_string("local_path")?,
            client_side: row.get_bool("client_side")?,
            remote_checksum: row.get_string("remote_checksum")?,
            local_checksum: row.get_string("local_checksum")?,
            local_content_hash: row.get_string("local_content_hash")?,
            enabled: row.get_bool("enabled")?,
            required: row.get_bool("required")?,
            data_order: row.get_i64("data_order")?,
        })
    }
}

impl HasLocalChecksum for FoxyMod {
    fn local_checksum(&self) -> &str {
        &self.local_checksum
    }
    fn order(&self) -> i64 {
        self.data_order
    }
    fn local_identifier(&self) -> &str {
        &self.local_path
    }
}
