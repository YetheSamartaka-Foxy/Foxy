use crate::core::models::modification_file::FoxyModFile;

#[derive(Clone)]
pub(crate) struct FilePartData {
    pub path: String,
    pub checksum: String,
    pub start: i64,
    pub length: i64,
    pub data_order: i64,
}

#[derive(Clone)]
pub(crate) struct FilePartsPayload {
    pub file: FoxyModFile,
    pub previous_file: Option<FoxyModFile>,
    pub parts: Vec<FilePartData>,
}

#[derive(Clone)]
pub(super) struct PartRow {
    pub file_id: i64,
    pub path: String,
    pub display_path: String,
    pub remote_checksum: String,
    pub length: i64,
    pub start: i64,
    pub data_order: i64,
}
