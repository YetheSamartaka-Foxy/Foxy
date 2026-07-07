use super::*;
use foxy_formats::{LocalLayout, LocalPartSpan, builtin_registry, is_end_part, is_header_part};

fn expected_remote_end(parts: &[FoxyModFilePart]) -> Option<u64> {
    parts
        .iter()
        .map(|part| part.remote_start.saturating_add(part.remote_length))
        .max()
}

pub(super) fn remote_parts_format_id(
    file_path: &str,
    parts: &[FoxyModFilePart],
) -> Option<&'static str> {
    let display_paths: Vec<_> = parts
        .iter()
        .map(|part| part_display_path(&part.path))
        .collect();
    let registry = builtin_registry();
    let remote_format_id = registry.format_id_for_remote_parts(&display_paths);
    let has_header = display_paths.iter().any(|path| is_header_part(path));
    let has_end = display_paths.iter().any(|path| is_end_part(path));
    if has_header
        && has_end
        && let Some(path_format_id) = registry.format_id_for_path(Path::new(file_path))
        && path_format_id == foxy_formats::PAC1_FORMAT_ID
    {
        return Some(path_format_id);
    }
    remote_format_id
}

pub(super) fn parse_local_content_layout(
    format_id: &str,
    file_path: &str,
) -> Result<LocalLayout, String> {
    builtin_registry()
        .parse_local_layout_for_format(format_id, Path::new(file_path))
        .map_err(|err| err.to_string())
}

pub(super) fn map_local_part_spans(
    parts: &[FoxyModFilePart],
    layout: &LocalLayout,
) -> Vec<Option<LocalPartSpan>> {
    let display_paths: Vec<_> = parts
        .iter()
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

    #[test]
    fn remote_parts_format_requires_pbo_header_and_end() {
        let header_only = vec![FoxyModFilePart {
            path: "$$HEADER$$".to_string(),
            ..Default::default()
        }];
        assert_eq!(remote_parts_format_id("test.pbo", &header_only), None);

        let both = vec![
            FoxyModFilePart {
                path: "$$HEADER$$".to_string(),
                ..Default::default()
            },
            FoxyModFilePart {
                path: "$$END$$".to_string(),
                ..Default::default()
            },
        ];
        assert_eq!(
            remote_parts_format_id("test.pbo", &both),
            Some(foxy_formats::PBO_FORMAT_ID)
        );
    }

    #[test]
    fn remote_parts_format_uses_pac1_gap_marker() {
        let parts = vec![
            FoxyModFilePart {
                path: "$$HEADER$$".to_string(),
                ..Default::default()
            },
            FoxyModFilePart {
                path: "$$GAP:1$$".to_string(),
                ..Default::default()
            },
            FoxyModFilePart {
                path: "$$END$$".to_string(),
                ..Default::default()
            },
        ];

        assert_eq!(
            remote_parts_format_id("test.pak", &parts),
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
