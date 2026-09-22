//! Where a file starts on disk, so hash jobs on a rotational disk can be read
//! in one sweep of the platter instead of in manifest order.

use std::path::Path;

/// Logical cluster of the file's first data extent. `None` when the file has
/// no extent of its own (a small file lives inside its MFT record), the extent
/// is unallocated, or the platform has no such query.
#[cfg(windows)]
pub(super) fn first_cluster(path: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use winapi::shared::winerror::ERROR_MORE_DATA;
    use winapi::um::errhandlingapi::GetLastError;
    use winapi::um::ioapiset::DeviceIoControl;
    use winapi::um::winioctl::{
        FSCTL_GET_RETRIEVAL_POINTERS, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
    };
    use winapi::um::winnt::FILE_READ_ATTRIBUTES;

    // Attribute access only: opening for data could start an on-access scan.
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .open(path)
        .ok()?;
    // SAFETY: both are plain C structs for which all-zero is valid, and the
    // zero starting VCN asks for the first extent.
    let mut input: STARTING_VCN_INPUT_BUFFER = unsafe { std::mem::zeroed() };
    let mut output: RETRIEVAL_POINTERS_BUFFER = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: the handle is open for the duration of the call, and the buffer
    // sizes passed match the buffers.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            FSCTL_GET_RETRIEVAL_POINTERS,
            (&raw mut input).cast(),
            size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
            (&raw mut output).cast(),
            size_of::<RETRIEVAL_POINTERS_BUFFER>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // More extents than the one-entry buffer holds still fills the first.
    // SAFETY: GetLastError has no preconditions.
    if ok == 0 && unsafe { GetLastError() } != ERROR_MORE_DATA {
        return None;
    }
    if output.ExtentCount == 0 {
        return None;
    }
    // SAFETY: LARGE_INTEGER is a union over the same 64 bits.
    let lcn = unsafe { *output.Extents[0].Lcn.QuadPart() };
    // A sparse or virtual extent reports -1.
    u64::try_from(lcn).ok()
}

#[cfg(not(windows))]
pub(super) fn first_cluster(_path: &Path) -> Option<u64> {
    None
}

/// Reorder `items` by first cluster, those without one last in path order.
/// Returns how many had a cluster.
pub(super) fn sort_by_first_cluster<T>(
    items: &mut Vec<T>,
    clusters: Vec<Option<u64>>,
    path_of: impl Fn(&T) -> &str,
) -> usize {
    let located = clusters.iter().filter(|cluster| cluster.is_some()).count();
    let mut keyed: Vec<(Option<u64>, String, T)> = clusters
        .into_iter()
        .zip(items.drain(..))
        .map(|(cluster, item)| (cluster, path_of(&item).to_ascii_lowercase(), item))
        .collect();
    keyed.sort_by(|left, right| {
        left.0
            .is_none()
            .cmp(&right.0.is_none())
            .then(left.0.cmp(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });
    items.extend(keyed.into_iter().map(|(_, _, item)| item));
    located
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn located_files_sweep_by_cluster_and_the_rest_follow_by_path() {
        let mut items = vec!["d/late.pbo", "B/mod.cpp", "a/early.pbo", "a/meta.cpp"];
        let located =
            sort_by_first_cluster(&mut items, vec![Some(900), None, Some(10), None], |item| {
                item
            });
        assert_eq!(located, 2);
        assert_eq!(
            items,
            ["a/early.pbo", "d/late.pbo", "a/meta.cpp", "B/mod.cpp"]
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_written_file_reports_a_cluster_and_a_missing_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let large = dir.path().join("large.bin");
        std::fs::write(&large, vec![1u8; 1024 * 1024]).unwrap();
        assert!(first_cluster(&large).is_some());
        assert!(first_cluster(&dir.path().join("missing.bin")).is_none());
    }
}
