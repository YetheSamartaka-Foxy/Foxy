use crate::{
    ContentFormat, FilePart, FormatError, FormatResult, LocalLayout, LocalPartSpan, is_end_part,
    is_header_part, is_pac1_gap_part, normalize_part_path,
};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Seek};
use std::path::Path;

pub const PBO_FORMAT_ID: &str = "pbo";
pub const PBO_HEADER_PART: &str = "$$HEADER$$";
pub const PBO_END_PART: &str = "$$END$$";

pub struct PboFormat;

impl ContentFormat for PboFormat {
    fn id(&self) -> &'static str {
        PBO_FORMAT_ID
    }

    fn matches(&self, path: &Path, _head: &[u8]) -> bool {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pbo"))
    }

    fn remote_layout_matches(&self, part_paths: &[&str]) -> bool {
        let mut has_header = false;
        let mut has_end = false;
        for path in part_paths {
            if is_pac1_gap_part(path) {
                return false;
            }
            if is_header_part(path) {
                has_header = true;
            } else if is_end_part(path) {
                has_end = true;
            }
            if has_header && has_end {
                return true;
            }
        }
        false
    }

    fn parse_parts(&self, path: &Path) -> FormatResult<Vec<FilePart>> {
        let layout = parse_pbo_layout(path)?;
        let mut parts = Vec::with_capacity(layout.entries.len() + 2);
        parts.push(FilePart {
            path: PBO_HEADER_PART.to_string(),
            start: layout.header.start,
            length: layout.header.length,
        });
        for entry in layout.entries {
            parts.push(FilePart {
                path: entry.path,
                start: entry.span.start,
                length: entry.span.length,
            });
        }
        parts.push(FilePart {
            path: PBO_END_PART.to_string(),
            start: layout.end.start,
            length: layout.end.length,
        });
        Ok(parts)
    }

    fn parse_local_layout(&self, path: &Path) -> FormatResult<LocalLayout> {
        let layout = parse_pbo_layout(path)?;
        let mut parts_by_path: HashMap<String, VecDeque<LocalPartSpan>> = HashMap::new();
        let mut entry_payload_bytes = 0u64;
        for entry in layout.entries {
            entry_payload_bytes = entry_payload_bytes.saturating_add(entry.span.length);
            parts_by_path
                .entry(normalize_part_path(&entry.path))
                .or_default()
                .push_back(entry.span);
        }
        Ok(LocalLayout {
            header: layout.header,
            end: layout.end,
            parts_by_path,
            entry_count: layout.entry_count,
            entry_payload_bytes,
        })
    }
}

#[derive(Clone, Debug)]
struct PboEntry {
    path: String,
    span: LocalPartSpan,
}

#[derive(Clone, Debug)]
struct PboLayout {
    header: LocalPartSpan,
    end: LocalPartSpan,
    entries: Vec<PboEntry>,
    entry_count: usize,
}

fn parse_pbo_layout(file_path: &Path) -> FormatResult<PboLayout> {
    let file = std::fs::File::open(file_path).map_err(|e| {
        FormatError::new(format!("failed to open PBO {}: {e}", file_path.display()))
    })?;
    let file_len = file
        .metadata()
        .map_err(|e| {
            FormatError::new(format!(
                "failed to read PBO metadata {}: {e}",
                file_path.display()
            ))
        })?
        .len();
    let mut file = std::io::BufReader::with_capacity(128 * 1024, file);

    let mut first = [0u8; 1];
    file.read_exact(&mut first)
        .map_err(|e| FormatError::new(format!("failed to read PBO first byte: {e}")))?;
    if first[0] != 0 {
        return Err(FormatError::new(format!(
            "invalid PBO header first byte: expected 0, got {}",
            first[0]
        )));
    }

    let mut tag = [0u8; 4];
    file.read_exact(&mut tag)
        .map_err(|e| FormatError::new(format!("failed to read PBO tag: {e}")))?;
    if &tag != b"sreV" {
        return Err(FormatError::new(format!(
            "invalid PBO header tag: expected sreV, got {}",
            String::from_utf8_lossy(&tag)
        )));
    }

    file.seek(std::io::SeekFrom::Current(16))
        .map_err(|e| FormatError::new(format!("failed to skip PBO header fields: {e}")))?;

    let mut marker = [0u8; 1];
    file.read_exact(&mut marker)
        .map_err(|e| FormatError::new(format!("failed to read PBO extension marker: {e}")))?;
    if marker[0] != 0 {
        file.seek(std::io::SeekFrom::Current(-1))
            .map_err(|e| FormatError::new(format!("failed to rewind PBO marker: {e}")))?;
        let _ = read_pbo_cstring(&mut file)?;
        let _ = read_pbo_cstring(&mut file)?;
        loop {
            let name = read_pbo_cstring(&mut file)?;
            if name.is_empty() {
                break;
            }
            let _value = read_pbo_cstring(&mut file)?;
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
        raw_entries.push((String::from_utf8_lossy(&name).to_string(), data_size));
    }

    let header_len = file
        .stream_position()
        .map_err(|e| FormatError::new(format!("failed to resolve PBO header size: {e}")))?;
    let mut cursor = header_len;
    let mut entries = Vec::with_capacity(raw_entries.len());
    for (path, length) in raw_entries {
        entries.push(PboEntry {
            path,
            span: LocalPartSpan {
                start: cursor,
                length,
            },
        });
        cursor = cursor
            .checked_add(length)
            .ok_or_else(|| FormatError::new("PBO entry offsets overflowed u64"))?;
    }

    if cursor > file_len {
        return Err(FormatError::new(format!(
            "PBO layout exceeds file size (entries_end={cursor} file_len={file_len})"
        )));
    }

    let entry_count = entries.len();
    Ok(PboLayout {
        header: LocalPartSpan {
            start: 0,
            length: header_len,
        },
        end: LocalPartSpan {
            start: cursor,
            length: file_len.saturating_sub(cursor),
        },
        entries,
        entry_count,
    })
}

fn read_pbo_cstring(file: &mut impl BufRead) -> FormatResult<Vec<u8>> {
    const MAX_LEN: usize = 8 * 1024;
    let mut out = Vec::with_capacity(128);
    loop {
        let buf = file
            .fill_buf()
            .map_err(|e| FormatError::new(format!("failed to read PBO string bytes: {e}")))?;
        if buf.is_empty() {
            return Err(FormatError::new("unexpected EOF while reading PBO string"));
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
            return Err(FormatError::new("PBO string exceeded safety limit"));
        }
        out.extend_from_slice(&buf[..value_len]);
        file.consume(consumed);
        if value_len < consumed {
            break;
        }
    }
    Ok(out)
}

fn read_pbo_u32(file: &mut impl Read) -> FormatResult<u32> {
    let mut raw = [0u8; 4];
    file.read_exact(&mut raw)
        .map_err(|e| FormatError::new(format!("failed to read PBO u32: {e}")))?;
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentFormat, PBO_END_PART, PBO_HEADER_PART, single_file_parts};

    fn push_entry(bytes: &mut Vec<u8>, name: &[u8], length: u32) {
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&length.to_le_bytes());
    }

    fn fixture_pbo() -> tempfile_file::TempPath {
        let mut bytes = Vec::new();
        bytes.push(0);
        bytes.extend_from_slice(b"sreV");
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.push(0);
        push_entry(&mut bytes, b"Data\\Thing.bin", 4);
        push_entry(&mut bytes, b"", 0);
        bytes.extend_from_slice(b"DATA");
        bytes.extend_from_slice(b"TAIL");
        tempfile_file::write("fixture.pbo", &bytes)
    }

    #[test]
    fn pbo_parts_match_expected_layout() {
        let path = fixture_pbo();
        let parts = PboFormat.parse_parts(path.as_path()).unwrap();
        let header_len = 22 + "Data\\Thing.bin".len() as u64 + 1 + 20 + 21;

        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].path, PBO_HEADER_PART);
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts[0].length, header_len);
        assert_eq!(parts[1].path, "Data\\Thing.bin");
        assert_eq!(parts[1].start, header_len);
        assert_eq!(parts[1].length, 4);
        assert_eq!(parts[2].path, PBO_END_PART);
        assert_eq!(parts[2].start, header_len + 4);
        assert_eq!(parts[2].length, 4);
    }

    #[test]
    fn local_layout_maps_normalized_remote_paths() {
        let path = fixture_pbo();
        let layout = PboFormat.parse_local_layout(path.as_path()).unwrap();
        let spans = layout.map_part_spans(["$$HEADER$$", "data/thing.bin", "$$END$$"]);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0], Some(layout.header));
        assert_eq!(
            spans[1],
            Some(LocalPartSpan {
                start: layout.header.length,
                length: 4
            })
        );
        assert_eq!(spans[2], Some(layout.end));
        assert_eq!(layout.entry_count, 1);
        assert_eq!(layout.entry_payload_bytes, 4);
    }

    #[test]
    fn pbo_invalid_header_is_error() {
        let path = tempfile_file::write("bad.pbo", b"bad");
        assert!(PboFormat.parse_parts(path.as_path()).is_err());
    }

    #[test]
    fn pbo_remote_marker_detection_requires_header_and_end() {
        assert!(!PboFormat.remote_layout_matches(&["$$HEADER$$"]));
        assert!(PboFormat.remote_layout_matches(&["$$HEADER$$", "$$END$$"]));
    }

    #[test]
    fn single_part_uses_file_name_only() {
        let parts = single_file_parts("addons/sub/deep.bin", 100);
        assert_eq!(parts[0].path, "deep.bin_100");
    }

    mod tempfile_file {
        use std::path::{Path, PathBuf};
        use std::time::{SystemTime, UNIX_EPOCH};

        pub struct TempPath {
            path: PathBuf,
        }

        impl TempPath {
            pub fn as_path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TempPath {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.path);
            }
        }

        pub fn write(name: &str, bytes: &[u8]) -> TempPath {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "foxy-formats-{}-{name}-{unique}",
                std::process::id()
            ));
            std::fs::write(&path, bytes).expect("fixture should be written");
            TempPath { path }
        }
    }
}
