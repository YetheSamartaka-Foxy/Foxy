use crate::core::db::{DbErr, DbRow};
use crate::core::models::trait_has_local_checksum::HasLocalChecksum;
use serde::{Deserialize, Serialize};

pub(crate) const FILE_COLUMNS: &str = "id, name, remote_path, local_path, remote_checksum, local_checksum, \
     local_content_hash, length, data_order";

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct FoxyModFile {
    pub(crate) id: u64,

    pub(crate) name: String,

    pub(crate) remote_path: String,

    pub(crate) local_path: String,
    #[serde(skip)]
    pub(crate) remote_checksum: String,

    pub(crate) local_checksum: String,

    pub(crate) local_content_hash: String,

    pub(crate) length: u64,

    pub(crate) data_order: i64,
}

impl FoxyModFile {
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
