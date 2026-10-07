use super::*;
use foxy_formats::{
    LocalLayout, LocalPartSpan, PBO_FORMAT_ID, builtin_registry, is_end_part, is_header_part,
};

fn expected_remote_end(parts: &[FoxyModFilePart]) -> Option<u64> {
    parts
        .iter()
        .map(|part| part.remote_start.saturating_add(part.remote_length))
        .max()
}

/// `game_formats` is the active game's declared container list. A game that
/// ships one container never needs the file head to name it; only an
/// undeclared game falls back to marker and magic detection.
pub(super) fn remote_parts_format_id(
    file_path: &str,
    parts: &[FoxyModFilePart],
    game_formats: &[&'static str],
) -> Option<&'static str> {
    let display_paths: Vec<_> = parts
        .iter()
        .map(|part| part_display_path(&part.path))
        .collect();
    let has_header = display_paths.iter().any(|path| is_header_part(path));
    let has_end = display_paths.iter().any(|path| is_end_part(path));
    if !(has_header && has_end) {
        return None;
    }
    if let [format_id] = game_formats {
        return Some(format_id);
    }
    let registry = builtin_registry();
    let remote_format_id = registry.format_id_for_remote_parts(&display_paths);
    // A gapless PAC1 archive carries only the PBO-shaped markers, so the file
    // head has to tell the two apart.
    let detected = match remote_format_id {
        Some(PBO_FORMAT_ID) => registry
            .format_id_for_path(Path::new(file_path))
            .or(remote_format_id),
        other => other,
    };
    detected.filter(|format_id| game_formats.is_empty() || game_formats.contains(format_id))
}

pub(super) fn parse_local_content_layout_from(
    format_id: &str,
    reader: &mut dyn foxy_formats::BufReadSeek,
    file_len: u64,
) -> Result<LocalLayout, String> {
    builtin_registry()
        .parse_local_layout_for_format_from(format_id, reader, file_len)
        .map_err(|err| err.to_string())
}

pub(super) fn map_local_part_spans<'a>(
    parts: impl IntoIterator<Item = &'a FoxyModFilePart>,
    layout: &LocalLayout,
) -> Vec<Option<LocalPartSpan>> {
    let display_paths: Vec<_> = parts
        .into_iter()
        .map(|part| part_display_path(&part.path))
        .collect();
    layout.map_part_spans(display_paths)
}

pub(super) fn local_file_matches_part_layout(
    file_path: &str,
    expected_file_len: u64,
    parts: &[FoxyModFilePart],
) -> bool {
    let local_size = match std::fs::metadata(file_path) {
        Ok(meta) if meta.is_file() => meta.len(),
        _ => return false,
    };

    if local_size != expected_file_len {
        return false;
    }

    match expected_remote_end(parts) {
        Some(last_end) => local_size == last_end,
        None => local_size == expected_file_len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn container_parts(with_gap: bool) -> Vec<FoxyModFilePart> {
        let mut paths = vec!["$$HEADER$$"];
        if with_gap {
            paths.push("$$GAP:1$$");
        }
        paths.push("$$END$$");
        paths
            .into_iter()
            .map(|path| FoxyModFilePart {
                path: path.to_string(),
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn remote_parts_format_requires_header_and_end() {
        let header_only = vec![FoxyModFilePart {
            path: "$$HEADER$$".to_string(),
            ..Default::default()
        }];
        assert_eq!(remote_parts_format_id("test.pbo", &header_only, &[]), None);
        assert_eq!(
            remote_parts_format_id("test.pbo", &header_only, &[foxy_formats::PBO_FORMAT_ID]),
            None
        );
        assert_eq!(
            remote_parts_format_id("test.pbo", &container_parts(false), &[]),
            Some(foxy_formats::PBO_FORMAT_ID)
        );
    }

    #[test]
    fn remote_parts_format_trusts_a_single_declared_format_without_probing() {
        let missing = std::env::temp_dir().join("foxy-format-layout-missing");
        let missing_pbo = missing.join("addon.pbo");
        let missing_pak = missing.join("addon.pak");
        assert_eq!(
            remote_parts_format_id(
                missing_pbo.to_str().unwrap(),
                &container_parts(false),
                &[foxy_formats::PBO_FORMAT_ID]
            ),
            Some(foxy_formats::PBO_FORMAT_ID)
        );
        assert_eq!(
            remote_parts_format_id(
                missing_pak.to_str().unwrap(),
                &container_parts(false),
                &[foxy_formats::PAC1_FORMAT_ID]
            ),
            Some(foxy_formats::PAC1_FORMAT_ID)
        );
    }

    #[test]
    fn remote_parts_format_uses_pac1_gap_marker() {
        assert_eq!(
            remote_parts_format_id("test.pak", &container_parts(true), &[]),
            Some(foxy_formats::PAC1_FORMAT_ID)
        );
    }

    #[test]
    fn remote_parts_format_filters_detection_by_declared_formats() {
        let both = [foxy_formats::PBO_FORMAT_ID, foxy_formats::PAC1_FORMAT_ID];
        assert_eq!(
            remote_parts_format_id("test.pak", &container_parts(true), &both),
            Some(foxy_formats::PAC1_FORMAT_ID)
        );
        assert_eq!(
            remote_parts_format_id(
                "test.pak",
                &container_parts(true),
                &[foxy_formats::PBO_FORMAT_ID, "other"]
            ),
            None
        );
    }

    #[test]
    fn remote_parts_format_probes_gapless_pac1_for_undeclared_games() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.PAK");
        std::fs::write(&path, b"FORM\0\0\0\0PAC1").unwrap();

        assert_eq!(
            remote_parts_format_id(path.to_str().unwrap(), &container_parts(false), &[]),
            Some(foxy_formats::PAC1_FORMAT_ID)
        );
    }

    #[test]
    fn map_local_part_spans_strips_storage_suffix() {
        let mut parts_by_path = HashMap::new();
        parts_by_path.insert(
            "addons/ace_main.pbo".to_string(),
            VecDeque::from([LocalPartSpan {
                start: 10,
                length: 5,
            }]),
        );
        let layout = LocalLayout {
            header: LocalPartSpan {
                start: 0,
                length: 10,
            },
            end: LocalPartSpan {
                start: 15,
                length: 0,
            },
            parts_by_path,
            entry_count: 1,
            entry_payload_bytes: 5,
        };
        let parts = vec![FoxyModFilePart {
            path: crate::core::models::modification_file_part::part_storage_path(
                "Addons\\Ace_Main.pbo",
                0,
            ),
            ..Default::default()
        }];

        assert_eq!(
            map_local_part_spans(&parts, &layout),
            vec![Some(LocalPartSpan {
                start: 10,
                length: 5
            })]
        );
    }
}
