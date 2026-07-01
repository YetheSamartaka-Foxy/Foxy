use crate::core::db::{DbErr, DbRow};
use crate::core::models::trait_has_local_checksum::HasLocalChecksum;
use serde::{Deserialize, Serialize};

const PART_STORAGE_SEPARATOR: char = '\u{001F}';

/// Column list for selecting a subfile row into a [`FoxyModFilePart`] via
/// [`FoxyModFilePart::from_row`].
pub(crate) const SUBFILE_COLUMNS: &str = "id, file_id, path, remote_length, local_length, remote_start, local_start, \
     remote_checksum, local_checksum, data_order";

pub(crate) fn part_storage_path(path: &str, data_order: i64) -> String {
    format!("{path}{PART_STORAGE_SEPARATOR}{data_order}")
}

pub(crate) fn part_display_path(path: &str) -> &str {
    path.rsplit_once(PART_STORAGE_SEPARATOR)
        .map(|(display, _)| display)
        .unwrap_or(path)
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub(crate) struct FoxyModFilePart {
    /// Database id if applicable
    pub(crate) id: u64,
    /// File ID to distinguish between file parts of different files
    pub(crate) file_id: u64,
    /// Internal storage key for a file part. Use `part_display_path` for user-facing text.
    pub(crate) path: String,
    /// Remote size of part
    pub(crate) remote_length: u64,
    /// Local size of part
    pub(crate) local_length: u64,
    /// Remote offset of part from start of a file
    pub(crate) remote_start: u64,
    /// Local offset of part from start of a file
    pub(crate) local_start: u64,
    /// Remote checksum of part
    #[serde(skip)]
    pub(crate) remote_checksum: String,
    /// Local checksum of part
    pub(crate) local_checksum: String,
    /// Remote data order
    pub(crate) data_order: i64,
}

impl FoxyModFilePart {
    /// Materialize a [`FoxyModFilePart`] from a seam [`DbRow`] selected with [`SUBFILE_COLUMNS`].
    pub(crate) fn from_row(row: &DbRow) -> Result<Self, DbErr> {
        Ok(FoxyModFilePart {
            id: row.get_i64("id")? as u64,
            file_id: row.get_i64("file_id")? as u64,
            path: row.get_string("path")?,
            remote_length: row.get_i64("remote_length")? as u64,
            local_length: row.get_i64("local_length")? as u64,
            remote_start: row.get_i64("remote_start")? as u64,
            local_start: row.get_i64("local_start")? as u64,
            remote_checksum: row.get_string("remote_checksum")?,
            local_checksum: row.get_string("local_checksum")?,
            data_order: row.get_i64("data_order")?,
        })
    }

    pub(crate) fn file_checksums_are_clean(
        file_local_checksum: &str,
        file_remote_checksum: &str,
    ) -> bool {
        let local = file_local_checksum.trim();
        let remote = file_remote_checksum.trim();
        !local.is_empty() && local == remote
    }

    pub(crate) fn has_effective_local_checksum_for_file(
        &self,
        file_local_checksum: &str,
        file_remote_checksum: &str,
    ) -> bool {
        !self.local_checksum.trim().is_empty()
            || (Self::file_checksums_are_clean(file_local_checksum, file_remote_checksum)
                && !self.remote_checksum.trim().is_empty())
    }

    pub(crate) fn apply_derived_clean_local_state(
        &mut self,
        file_local_checksum: &str,
        file_remote_checksum: &str,
    ) -> bool {
        if !Self::file_checksums_are_clean(file_local_checksum, file_remote_checksum)
            || self.remote_checksum.trim().is_empty()
        {
            return false;
        }

        let changed = self.local_checksum != self.remote_checksum
            || self.local_length != self.remote_length
            || self.local_start != self.remote_start;
        self.local_checksum = self.remote_checksum.clone();
        self.local_length = self.remote_length;
        self.local_start = self.remote_start;
        changed
    }

    pub(crate) fn with_derived_clean_local_state(
        mut self,
        file_local_checksum: &str,
        file_remote_checksum: &str,
    ) -> Self {
        self.apply_derived_clean_local_state(file_local_checksum, file_remote_checksum);
        self
    }
}

impl HasLocalChecksum for FoxyModFilePart {
    fn local_checksum(&self) -> &str {
        &self.local_checksum
    }
    fn order(&self) -> i64 {
        self.data_order
    }
    fn local_identifier(&self) -> &str {
        part_display_path(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_storage_path_round_trip() {
        let path = "addons/ace_main.pbo";
        let order = 42;
        let storage = part_storage_path(path, order);
        let display = part_display_path(&storage);
        assert_eq!(display, path);
    }

    #[test]
    fn part_display_path_without_separator_returns_original() {
        let raw = "simple_path.pbo";
        assert_eq!(part_display_path(raw), raw);
    }

    #[test]
    fn part_storage_path_contains_separator() {
        let storage = part_storage_path("file.pbo", 7);
        assert!(storage.contains('\u{001F}'));
    }

    #[test]
    fn part_storage_path_negative_order() {
        let storage = part_storage_path("file.pbo", -1);
        let display = part_display_path(&storage);
        assert_eq!(display, "file.pbo");
        assert!(storage.ends_with("-1"));
    }

    #[test]
    fn part_storage_path_zero_order() {
        let storage = part_storage_path("addons/data.pbo", 0);
        let display = part_display_path(&storage);
        assert_eq!(display, "addons/data.pbo");
    }

    #[test]
    fn part_storage_path_large_order() {
        let storage = part_storage_path("file.pbo", i64::MAX);
        let display = part_display_path(&storage);
        assert_eq!(display, "file.pbo");
    }

    #[test]
    fn part_display_path_empty_string() {
        assert_eq!(part_display_path(""), "");
    }

    #[test]
    fn foxy_mod_file_part_default_values() {
        let part = FoxyModFilePart::default();
        assert_eq!(part.id, 0);
        assert_eq!(part.file_id, 0);
        assert!(part.path.is_empty());
        assert_eq!(part.remote_length, 0);
        assert_eq!(part.local_length, 0);
        assert_eq!(part.remote_start, 0);
        assert_eq!(part.local_start, 0);
    }

    #[test]
    fn has_local_checksum_trait_impl() {
        let part = FoxyModFilePart {
            local_checksum: "ABC123".to_string(),
            data_order: 5,
            path: part_storage_path("file.pbo", 5),
            ..Default::default()
        };
        assert_eq!(part.local_checksum(), "ABC123");
        assert_eq!(part.order(), 5);
        assert_eq!(part.local_identifier(), "file.pbo");
    }

    #[test]
    fn derived_clean_local_state_projects_remote_part_fields() {
        let mut part = FoxyModFilePart {
            remote_checksum: "REMOTE".to_string(),
            remote_length: 42,
            remote_start: 7,
            ..Default::default()
        };

        assert!(part.apply_derived_clean_local_state("FILE", "FILE"));
        assert_eq!(part.local_checksum, "REMOTE");
        assert_eq!(part.local_length, 42);
        assert_eq!(part.local_start, 7);
    }

    #[test]
    fn derived_clean_local_state_does_not_project_dirty_file() {
        let mut part = FoxyModFilePart {
            remote_checksum: "REMOTE".to_string(),
            remote_length: 42,
            remote_start: 7,
            ..Default::default()
        };

        assert!(!part.apply_derived_clean_local_state("OLD", "NEW"));
        assert!(part.local_checksum.is_empty());
        assert_eq!(part.local_length, 0);
        assert_eq!(part.local_start, 0);
    }
}
