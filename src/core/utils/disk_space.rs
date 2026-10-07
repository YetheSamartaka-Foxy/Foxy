use std::path::Path;

/// Headroom kept free beyond the planned bytes so an update never fills the
/// destination volume to the last byte.
pub const DISK_SPACE_MARGIN_BYTES: u64 = 500 * 1024 * 1024;

/// A destination volume that cannot take a planned download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskSpaceShortfall {
    /// Planned bytes plus [`DISK_SPACE_MARGIN_BYTES`].
    pub needed_bytes: u64,
    pub available_bytes: u64,
    /// The path whose volume was probed.
    pub path: String,
}

impl DiskSpaceShortfall {
    /// Bytes the user has to free before the download can start.
    pub fn missing_bytes(&self) -> u64 {
        self.needed_bytes.saturating_sub(self.available_bytes)
    }
}

pub fn disk_space_needed(planned_bytes: u64) -> u64 {
    planned_bytes.saturating_add(DISK_SPACE_MARGIN_BYTES)
}

pub fn disk_space_shortfall(
    planned_bytes: u64,
    available_bytes: u64,
    path: &Path,
) -> Option<DiskSpaceShortfall> {
    let needed_bytes = disk_space_needed(planned_bytes);
    (available_bytes < needed_bytes).then(|| DiskSpaceShortfall {
        needed_bytes,
        available_bytes,
        path: path.display().to_string(),
    })
}

/// Probe the volume holding `path`. `Ok(None)` means the space suffices; an
/// error means the volume could not be probed at all.
pub fn probe_disk_space_shortfall(
    planned_bytes: u64,
    path: &Path,
) -> std::io::Result<Option<DiskSpaceShortfall>> {
    let available_bytes = fs4::available_space(path)?;
    Ok(disk_space_shortfall(planned_bytes, available_bytes, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortfall_counts_the_margin() {
        let path = Path::new("E:/Arma 3 Mods");
        let shortfall = disk_space_shortfall(1_000, 1_000, path).expect("margin exceeds space");
        assert_eq!(shortfall.needed_bytes, 1_000 + DISK_SPACE_MARGIN_BYTES);
        assert_eq!(shortfall.available_bytes, 1_000);
        assert_eq!(shortfall.missing_bytes(), DISK_SPACE_MARGIN_BYTES);
        assert_eq!(shortfall.path, path.display().to_string());
    }

    #[test]
    fn no_shortfall_when_space_covers_bytes_and_margin() {
        let planned = 12 * 1024 * 1024 * 1024;
        let available = planned + DISK_SPACE_MARGIN_BYTES;
        assert_eq!(
            disk_space_shortfall(planned, available, Path::new("/")),
            None
        );
        assert!(disk_space_shortfall(planned, available - 1, Path::new("/")).is_some());
    }

    #[test]
    fn needed_saturates_instead_of_overflowing() {
        assert_eq!(disk_space_needed(u64::MAX), u64::MAX);
        let shortfall = disk_space_shortfall(u64::MAX, 0, Path::new("/")).expect("shortfall");
        assert_eq!(shortfall.missing_bytes(), u64::MAX);
    }
}
