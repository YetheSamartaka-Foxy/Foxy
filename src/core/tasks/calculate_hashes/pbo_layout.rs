use super::*;

fn expected_remote_end(parts: &[FoxyModFilePart]) -> Option<u64> {
    parts
        .iter()
        .map(|part| part.remote_start.saturating_add(part.remote_length))
        .max()
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LocalPartSpan {
    pub(super) start: u64,
    pub(super) length: u64,
}

#[derive(Clone, Debug)]
pub(super) struct LocalPboLayout {
    pub(super) header: LocalPartSpan,
    pub(super) end: LocalPartSpan,
    pub(super) parts_by_path: HashMap<String, VecDeque<LocalPartSpan>>,
    pub(super) entry_count: usize,
    pub(super) entry_payload_bytes: u64,
}

fn normalize_part_path(path: &str) -> String {
    part_display_path(path)
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn is_header_part(path: &str) -> bool {
    part_display_path(path).eq_ignore_ascii_case("$$HEADER$$")
}

fn is_end_part(path: &str) -> bool {
    part_display_path(path).eq_ignore_ascii_case("$$END$$")
}

pub(super) fn has_pbo_part_markers(parts: &[FoxyModFilePart]) -> bool {
    let mut has_header = false;
    let mut has_end = false;
    for part in parts {
        if is_header_part(&part.path) {
            has_header = true;
        } else if is_end_part(&part.path) {
            has_end = true;
        }
        if has_header && has_end {
            return true;
        }
    }
    false
}

fn read_pbo_cstring(file: &mut impl BufRead) -> Result<Vec<u8>, String> {
    const MAX_LEN: usize = 8 * 1024;
    let mut out = Vec::with_capacity(128);
    loop {
        let buf = file
            .fill_buf()
            .map_err(|e| format!("failed to read PBO string bytes: {}", e))?;
        if buf.is_empty() {
            return Err("unexpected EOF while reading PBO string".to_string());
        }
        let consumed = buf
            .iter()
            .position(|byte| *byte == 0)
            .map_or(buf.len(), |idx| idx + 1);
        let value_len = if buf[consumed - 1] == 0 {
            consumed - 1
        } else {
            consumed
        };
        if out.len().saturating_add(value_len) > MAX_LEN {
            return Err("PBO string exceeded safety limit".to_string());
        }
        out.extend_from_slice(&buf[..value_len]);
        file.consume(consumed);
        if value_len < consumed {
            break;
        }
    }
    Ok(out)
}

fn read_pbo_u32(file: &mut impl Read) -> Result<u32, String> {
    let mut raw = [0u8; 4];
    file.read_exact(&mut raw)
        .map_err(|e| format!("failed to read PBO u32: {}", e))?;
    Ok(u32::from_le_bytes(raw))
}

pub(super) fn parse_local_pbo_layout(file_path: &str) -> Result<LocalPboLayout, String> {
    let file =
        std::fs::File::open(file_path).map_err(|e| format!("failed to open local file: {}", e))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("failed to read local metadata: {}", e))?
        .len();
    let mut file = std::io::BufReader::with_capacity(128 * 1024, file);

    let mut first = [0u8; 1];
    file.read_exact(&mut first)
        .map_err(|e| format!("failed to read PBO first byte: {}", e))?;
    if first[0] != 0 {
        return Err(format!(
            "invalid PBO header first byte: expected 0, got {}",
            first[0]
        ));
    }

    let mut tag = [0u8; 4];
    file.read_exact(&mut tag)
        .map_err(|e| format!("failed to read PBO tag: {}", e))?;
    if &tag != b"sreV" {
        return Err(format!(
            "invalid PBO header tag: expected sreV, got {}",
            String::from_utf8_lossy(&tag)
        ));
    }

    file.seek(std::io::SeekFrom::Current(16))
        .map_err(|e| format!("failed to skip PBO header extension bytes: {}", e))?;

    let mut marker = [0u8; 1];
    file.read_exact(&mut marker)
        .map_err(|e| format!("failed to read PBO extension marker: {}", e))?;
    if marker[0] != 0 {
        file.seek(std::io::SeekFrom::Current(-1))
            .map_err(|e| format!("failed to rewind PBO extension marker: {}", e))?;
        let _ = read_pbo_cstring(&mut file)?;
        let _ = read_pbo_cstring(&mut file)?;
        loop {
            let property = read_pbo_cstring(&mut file)?;
            if property.is_empty() {
                break;
            }
        }
    }

    let mut raw_entries: Vec<(String, u64)> = Vec::new();
    loop {
        let name = read_pbo_cstring(&mut file)?;
        let _packing_method = read_pbo_u32(&mut file)?;
        let _size = read_pbo_u32(&mut file)?;
        let _reserved = read_pbo_u32(&mut file)?;
        let _timestamp = read_pbo_u32(&mut file)?;
        let data_size = read_pbo_u32(&mut file)? as u64;

        if name.is_empty() {
            break;
        }
        let path = normalize_part_path(&String::from_utf8_lossy(&name));
        raw_entries.push((path, data_size));
    }

    let header_len = file
        .stream_position()
        .map_err(|e| format!("failed to resolve PBO header size: {}", e))?;
    let mut cursor = header_len;
    let entry_count = raw_entries.len();
    let mut entry_payload_bytes = 0u64;
    let mut parts_by_path: HashMap<String, VecDeque<LocalPartSpan>> = HashMap::new();
    for (path, length) in raw_entries {
        entry_payload_bytes = entry_payload_bytes.saturating_add(length);
        parts_by_path
            .entry(path)
            .or_default()
            .push_back(LocalPartSpan {
                start: cursor,
                length,
            });
        cursor = cursor
            .checked_add(length)
            .ok_or_else(|| "PBO entry offsets overflowed u64".to_string())?;
    }

    if cursor > file_len {
        return Err(format!(
            "PBO layout exceeds file size (entries_end={} file_len={})",
            cursor, file_len
        ));
    }

    Ok(LocalPboLayout {
        header: LocalPartSpan {
            start: 0,
            length: header_len,
        },
        end: LocalPartSpan {
            start: cursor,
            length: file_len.saturating_sub(cursor),
        },
        parts_by_path,
        entry_count,
        entry_payload_bytes,
    })
}

pub(super) fn map_local_part_spans(
    parts: &[FoxyModFilePart],
    layout: LocalPboLayout,
) -> Vec<Option<LocalPartSpan>> {
    let header = layout.header;
    let end = layout.end;
    let mut parts_by_path = layout.parts_by_path;
    parts
        .iter()
        .map(|part| {
            if is_header_part(&part.path) {
                return Some(header);
            }
            if is_end_part(&part.path) {
                return Some(end);
            }
            let key = normalize_part_path(&part.path);
            parts_by_path.get_mut(&key).and_then(|q| q.pop_front())
        })
        .collect()
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

    #[test]
    fn is_header_part_case_insensitive() {
        assert!(is_header_part("$$HEADER$$"));
        assert!(is_header_part("$$header$$"));
        assert!(!is_header_part("header"));
    }

    #[test]
    fn is_end_part_case_insensitive() {
        assert!(is_end_part("$$END$$"));
        assert!(is_end_part("$$end$$"));
        assert!(!is_end_part("end"));
    }

    #[test]
    fn has_pbo_part_markers_requires_both() {
        let header_only = vec![FoxyModFilePart {
            path: "$$HEADER$$".to_string(),
            ..Default::default()
        }];
        assert!(!has_pbo_part_markers(&header_only));

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
        assert!(has_pbo_part_markers(&both));
    }

    #[test]
    fn normalize_part_path_lowercases_and_forward_slashes() {
        let result = normalize_part_path("Addons\\Ace_Main.pbo");
        assert_eq!(result, "addons/ace_main.pbo");
    }

    #[test]
    fn buffered_cstring_reads_across_buffer_boundaries() {
        let cursor = std::io::Cursor::new(b"abc\0tail".to_vec());
        let mut reader = std::io::BufReader::with_capacity(2, cursor);

        assert_eq!(read_pbo_cstring(&mut reader).unwrap(), b"abc");
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, b"tail");
    }

    #[test]
    fn buffered_cstring_rejects_unterminated_input() {
        let cursor = std::io::Cursor::new(b"abc".to_vec());
        let mut reader = std::io::BufReader::with_capacity(2, cursor);

        assert!(read_pbo_cstring(&mut reader).is_err());
    }
}
