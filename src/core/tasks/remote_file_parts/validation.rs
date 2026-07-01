use super::types::PartRow;
use crate::core::models::modification_file::FoxyModFile;
use crate::core::models::modification_file_part::FoxyModFilePart;
use log::warn;
use std::collections::HashMap;

pub(super) fn log_suspicious_manifest_paths(file: &FoxyModFile, parts: &[PartRow]) {
    if parts.is_empty() {
        return;
    }

    let replacement_char_paths = parts
        .iter()
        .filter(|part| part.display_path.contains('\u{FFFD}'))
        .count();

    let mut duplicate_counts: HashMap<String, usize> = HashMap::new();
    for part in parts {
        *duplicate_counts
            .entry(part.display_path.clone())
            .or_default() += 1;
    }

    let mut duplicate_paths: Vec<(String, usize)> = duplicate_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .collect();
    duplicate_paths.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if replacement_char_paths == 0 && duplicate_paths.is_empty() {
        return;
    }

    let duplicate_samples = duplicate_paths
        .iter()
        .take(3)
        .map(|(path, count)| format!("{} (x{})", path, count))
        .collect::<Vec<_>>()
        .join(", ");

    warn!(
        "Manifest part path anomalies for file_id={} path={}: replacement_char_paths={} duplicate_display_paths={} samples=[{}]. Part identity uses data_order to avoid path collisions.",
        file.id,
        file.remote_path,
        replacement_char_paths,
        duplicate_paths.len(),
        duplicate_samples
    );
}

pub(super) fn expected_remote_end(parts: &[FoxyModFilePart]) -> Option<u64> {
    parts
        .iter()
        .map(|part| part.remote_start.saturating_add(part.remote_length))
        .max()
}

pub(super) fn local_file_matches_part_layout(
    file: &FoxyModFile,
    parts: &[FoxyModFilePart],
) -> bool {
    let local_size = match std::fs::metadata(&file.local_path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return false,
    };

    if local_size != file.length {
        return false;
    }

    match expected_remote_end(parts) {
        Some(last_end) => local_size == last_end,
        None => local_size == file.length,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_remote_end_empty_parts() {
        assert_eq!(expected_remote_end(&[]), None);
    }

    #[test]
    fn expected_remote_end_single_part() {
        let part = FoxyModFilePart {
            remote_start: 0,
            remote_length: 512,
            ..Default::default()
        };
        assert_eq!(expected_remote_end(&[part]), Some(512));
    }

    #[test]
    fn expected_remote_end_non_contiguous_takes_max() {
        let parts = vec![
            FoxyModFilePart {
                remote_start: 0,
                remote_length: 100,
                ..Default::default()
            },
            FoxyModFilePart {
                remote_start: 500,
                remote_length: 200,
                ..Default::default()
            },
        ];
        assert_eq!(expected_remote_end(&parts), Some(700));
    }

    #[test]
    fn expected_remote_end_zero_length_parts() {
        let parts = vec![FoxyModFilePart {
            remote_start: 100,
            remote_length: 0,
            ..Default::default()
        }];
        assert_eq!(expected_remote_end(&parts), Some(100));
    }

    #[test]
    fn expected_remote_end_overlapping_parts() {
        let parts = vec![
            FoxyModFilePart {
                remote_start: 0,
                remote_length: 200,
                ..Default::default()
            },
            FoxyModFilePart {
                remote_start: 100,
                remote_length: 200,
                ..Default::default()
            },
        ];
        // max of (0+200=200, 100+200=300)
        assert_eq!(expected_remote_end(&parts), Some(300));
    }

    // ── log_suspicious_manifest_paths ──────────────────────────────────

    #[test]
    fn log_suspicious_manifest_paths_empty_parts_does_not_panic() {
        let file = FoxyModFile {
            id: 1,
            remote_path: "test".to_string(),
            ..Default::default()
        };
        // Should not panic on empty parts
        log_suspicious_manifest_paths(&file, &[]);
    }

    #[test]
    fn log_suspicious_manifest_paths_no_anomalies_does_not_panic() {
        let file = FoxyModFile {
            id: 1,
            remote_path: "test".to_string(),
            ..Default::default()
        };
        let parts = vec![PartRow {
            file_id: 1,
            path: "addons/file.pbo".to_string(),
            display_path: "addons/file.pbo".to_string(),
            remote_checksum: "ABC".to_string(),
            length: 100,
            start: 0,
            data_order: 0,
        }];
        log_suspicious_manifest_paths(&file, &parts);
    }

    #[test]
    fn log_suspicious_manifest_paths_warns_on_duplicate_display_paths() {
        let file = FoxyModFile {
            id: 7,
            remote_path: "@ace/addons/main.pbo".to_string(),
            ..Default::default()
        };
        let dup = PartRow {
            file_id: 7,
            path: "addons/main.pbo\u{001F}0".to_string(),
            display_path: "addons/main.pbo".to_string(),
            remote_checksum: "A".to_string(),
            length: 10,
            start: 0,
            data_order: 0,
        };
        let mut dup2 = dup.clone();
        dup2.data_order = 1;
        // Two parts share the same display path - should not panic while logging.
        log_suspicious_manifest_paths(&file, &[dup, dup2]);
    }

    // ── local_file_matches_part_layout (filesystem) ─────────────────────

    fn file_with_local(path: &std::path::Path, length: u64) -> FoxyModFile {
        FoxyModFile {
            local_path: path.to_string_lossy().to_string(),
            length,
            ..Default::default()
        }
    }

    #[test]
    fn local_file_matches_when_size_and_part_extent_align() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.pbo");
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        let file = file_with_local(&path, 100);
        let parts = vec![
            FoxyModFilePart {
                remote_start: 0,
                remote_length: 60,
                ..Default::default()
            },
            FoxyModFilePart {
                remote_start: 60,
                remote_length: 40,
                ..Default::default()
            },
        ];
        assert!(local_file_matches_part_layout(&file, &parts));
    }

    #[test]
    fn local_file_does_not_match_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.pbo");
        let file = file_with_local(&path, 100);
        let parts = vec![FoxyModFilePart {
            remote_start: 0,
            remote_length: 100,
            ..Default::default()
        }];
        assert!(!local_file_matches_part_layout(&file, &parts));
    }

    #[test]
    fn local_file_does_not_match_on_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.pbo");
        std::fs::write(&path, vec![0u8; 80]).unwrap();
        let file = file_with_local(&path, 100);
        let parts = vec![FoxyModFilePart {
            remote_start: 0,
            remote_length: 100,
            ..Default::default()
        }];
        assert!(!local_file_matches_part_layout(&file, &parts));
    }

    #[test]
    fn local_file_matches_with_no_parts_when_size_equals_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.pbo");
        std::fs::write(&path, vec![0u8; 50]).unwrap();
        let file = file_with_local(&path, 50);
        assert!(local_file_matches_part_layout(&file, &[]));
    }

    #[test]
    fn local_file_does_not_match_when_part_extent_differs_from_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.pbo");
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        let file = file_with_local(&path, 100);
        // Parts only describe 90 bytes while the file is 100 bytes long.
        let parts = vec![FoxyModFilePart {
            remote_start: 0,
            remote_length: 90,
            ..Default::default()
        }];
        assert!(!local_file_matches_part_layout(&file, &parts));
    }
}
