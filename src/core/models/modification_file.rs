use crate::core::db::{DbErr, DbRow};
use crate::core::models::trait_has_local_checksum::HasLocalChecksum;
use serde::{Deserialize, Serialize};

/// Column list for selecting a file row into a [`FoxyModFile`] via [`FoxyModFile::from_row`].
pub(crate) const FILE_COLUMNS: &str = "id, name, remote_path, local_path, remote_checksum, local_checksum, \
     local_content_hash, length, data_order";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct FoxyModFile {
    /// Database id if applicable
    pub(crate) id: u64,
    /// File name
    pub(crate) name: String,
    /// File remote path
    pub(crate) remote_path: String,
    /// File local path
    pub(crate) local_path: String,
    #[serde(skip)]
    /// File remote checksum
    pub(crate) remote_checksum: String,
    /// File local checksum
    pub(crate) local_checksum: String,
    /// Fast local content hash for quick checks
    pub(crate) local_content_hash: String,
    /// File size
    pub(crate) length: u64,
    /// Remote data order
    pub(crate) data_order: i64,
}

impl FoxyModFile {
    /// Materialize a [`FoxyModFile`] from a seam [`DbRow`] selected with [`FILE_COLUMNS`].
    pub(crate) fn from_row(row: &DbRow) -> Result<Self, DbErr> {
        Ok(FoxyModFile {
            id: row.get_i64("id")? as u64,
            name: row.get_string("name")?,
            remote_path: row.get_string("remote_path")?,
            local_path: row.get_string("local_path")?,
            remote_checksum: row.get_string("remote_checksum")?,
            local_checksum: row.get_string("local_checksum")?,
            local_content_hash: row.get_string("local_content_hash")?,
            length: row.get_i64("length")? as u64,
            data_order: row.get_i64("data_order")?,
        })
    }
}

impl HasLocalChecksum for FoxyModFile {
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
