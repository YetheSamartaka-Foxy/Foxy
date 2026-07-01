use std::collections::HashSet;

use crate::ui::app::Foxy;

impl Foxy {
    pub(super) fn tracked_texture_bytes_total(&self) -> usize {
        let mut total = self.app_icon_texture_bytes + self.default_repo_image_texture_bytes;
        let mut seen_checksums: HashSet<&str> = HashSet::new();

        for (checksum, bytes) in &self.tracked_icon_texture_bytes {
            if seen_checksums.insert(checksum.as_str()) {
                total += *bytes;
            }
        }
        for (checksum, bytes) in &self.tracked_repo_image_texture_bytes {
            if seen_checksums.insert(checksum.as_str()) {
                total += *bytes;
            }
        }

        total
    }

    pub(crate) fn tracked_texture_count(&self) -> usize {
        let mut seen_checksums: HashSet<&str> = HashSet::new();
        for checksum in self.tracked_icon_texture_bytes.keys() {
            seen_checksums.insert(checksum.as_str());
        }
        for checksum in self.tracked_repo_image_texture_bytes.keys() {
            seen_checksums.insert(checksum.as_str());
        }
        seen_checksums.len()
    }
}
