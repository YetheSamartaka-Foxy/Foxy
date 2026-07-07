use crate::{
    ContentFormat, FilePart, FormatError, FormatResult, LocalLayout, LocalPartSpan, is_end_part,
    is_header_part, normalize_part_path,
};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek};
use std::path::Path;

pub const PAC1_FORMAT_ID: &str = "pac1";
pub const PAC1_HEADER_PART: &str = "$$HEADER$$";
pub const PAC1_END_PART: &str = "$$END$$";
pub const PAC1_GAP_PREFIX: &str = "$$GAP:";

const FORM_MAGIC: &[u8; 4] = b"FORM";
const PAC1_MAGIC: &[u8; 4] = b"PAC1";
const FILE_TAG: &[u8; 4] = b"FILE";
const MAX_FILE_CHUNK_LEN: u64 = 128 * 1024 * 1024;
const MAX_TREE_DEPTH: usize = 64;
const MAX_CHILDREN_PER_FOLDER: usize = 100_000;
const MAX_TREE_ENTRIES: usize = 200_000;

pub struct Pac1Format;

impl ContentFormat for Pac1Format {
    fn id(&self) -> &'static str {
        PAC1_FORMAT_ID
    }

    fn matches(&self, path: &Path, head: &[u8]) -> bool {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pak"))
            && has_pac1_magic(head)
    }

    fn remote_layout_matches(&self, part_paths: &[&str]) -> bool {
        let mut has_header = false;
        let mut has_end = false;
        let mut has_gap = false;
        for path in part_paths {
            if is_header_part(path) {
                has_header = true;
            } else if is_end_part(path) {
                has_end = true;
            } else if is_pac1_gap_part(path) {
                has_gap = true;
            }
            if has_header && has_end && has_gap {
                return true;
            }
        }
        false
    }

    fn parse_parts(&self, path: &Path) -> FormatResult<Vec<FilePart>> {
        let layout = parse_pac1_layout(path)?;
        Ok(layout
            .parts
            .into_iter()
            .map(|part| FilePart {
                path: part.path,
                start: part.span.start,
                length: part.span.length,
            })
            .collect())
    }

    fn parse_local_layout(&self, path: &Path) -> FormatResult<LocalLayout> {
        let layout = parse_pac1_layout(path)?;
        let mut parts_by_path: HashMap<String, VecDeque<LocalPartSpan>> = HashMap::new();
        let mut entry_payload_bytes = 0u64;
        for part in layout.parts {
            if is_header_part(&part.path) || is_end_part(&part.path) {
                continue;
            }
            if !is_pac1_gap_part(&part.path) {
                entry_payload_bytes = entry_payload_bytes.saturating_add(part.span.length);
            }
            parts_by_path
                .entry(normalize_part_path(&part.path))
                .or_default()
                .push_back(part.span);
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
struct Pac1Part {
    path: String,
    span: LocalPartSpan,
}

#[derive(Clone, Debug)]
struct Pac1Entry {
    path: String,
    span: LocalPartSpan,
}

#[derive(Clone, Debug)]
struct Pac1Layout {
    header: LocalPartSpan,
    end: LocalPartSpan,
    parts: Vec<Pac1Part>,
    entry_count: usize,
}

struct ChunkInfo {
    tag: [u8; 4],
    body_start: u64,
    body_len: u64,
}

pub fn is_pac1_gap_part(path: &str) -> bool {
    let path = path.trim();
    path.len() > PAC1_GAP_PREFIX.len() + 2
        && path.starts_with(PAC1_GAP_PREFIX)
        && path.ends_with("$$")
        && path[PAC1_GAP_PREFIX.len()..path.len() - 2]
            .chars()
            .all(|ch| ch.is_ascii_digit())
}

fn has_pac1_magic(head: &[u8]) -> bool {
    head.len() >= 12 && &head[0..4] == FORM_MAGIC && &head[8..12] == PAC1_MAGIC
}

fn parse_pac1_layout(file_path: &Path) -> FormatResult<Pac1Layout> {
    let mut file = std::fs::File::open(file_path).map_err(|err| {
        FormatError::new(format!(
            "failed to open PAC1 {}: {err}",
            file_path.display()
        ))
    })?;
    let file_len = file
        .metadata()
        .map_err(|err| {
            FormatError::new(format!(
                "failed to read PAC1 metadata {}: {err}",
                file_path.display()
            ))
        })?
        .len();

    let chunks = read_pac1_chunks(&mut file, file_len)?;
    let file_chunk = chunks
        .iter()
        .find(|chunk| &chunk.tag == FILE_TAG)
        .ok_or_else(|| FormatError::new("PAC1 file has no FILE chunk"))?;
    if file_chunk.body_len > MAX_FILE_CHUNK_LEN {
        return Err(FormatError::new(format!(
            "PAC1 FILE chunk exceeds safety limit ({} bytes)",
            file_chunk.body_len
        )));
    }

    file.seek(std::io::SeekFrom::Start(file_chunk.body_start))
        .map_err(|err| FormatError::new(format!("failed to seek PAC1 FILE chunk: {err}")))?;
    let file_chunk_len = usize::try_from(file_chunk.body_len)
        .map_err(|_| FormatError::new("PAC1 FILE chunk length does not fit usize"))?;
    let mut tree_bytes = vec![0u8; file_chunk_len];
    file.read_exact(&mut tree_bytes)
        .map_err(|err| FormatError::new(format!("failed to read PAC1 FILE chunk: {err}")))?;

    let mut entries = parse_file_tree(&tree_bytes, file_len)?;
    entries.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then(left.path.cmp(&right.path))
    });
    validate_no_overlaps(&entries)?;
    build_layout(file_len, entries)
}

fn read_pac1_chunks(file: &mut std::fs::File, file_len: u64) -> FormatResult<Vec<ChunkInfo>> {
    let mut head = [0u8; 12];
    file.read_exact(&mut head)
        .map_err(|err| FormatError::new(format!("failed to read PAC1 header: {err}")))?;
    if !has_pac1_magic(&head) {
        return Err(FormatError::new("invalid PAC1 magic"));
    }
    let form_len = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as u64;
    if form_len < 4 {
        return Err(FormatError::new("invalid PAC1 FORM length"));
    }
    let declared_end = 8u64
        .checked_add(form_len)
        .ok_or_else(|| FormatError::new("PAC1 FORM length overflowed"))?;
    if declared_end > file_len {
        return Err(FormatError::new(format!(
            "PAC1 FORM length exceeds file size (declared_end={declared_end} file_len={file_len})"
        )));
    }

    let mut chunks = Vec::new();
    let mut offset = 12u64;
    while offset < file_len {
        if file_len.saturating_sub(offset) < 8 {
            return Err(FormatError::new("truncated PAC1 chunk header"));
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|err| FormatError::new(format!("failed to seek PAC1 chunk: {err}")))?;
        let mut raw = [0u8; 8];
        file.read_exact(&mut raw)
            .map_err(|err| FormatError::new(format!("failed to read PAC1 chunk header: {err}")))?;
        let tag = [raw[0], raw[1], raw[2], raw[3]];
        let body_len = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]) as u64;
        let body_start = offset
            .checked_add(8)
            .ok_or_else(|| FormatError::new("PAC1 chunk offset overflowed"))?;
        let body_end = body_start
            .checked_add(body_len)
            .ok_or_else(|| FormatError::new("PAC1 chunk length overflowed"))?;
        if body_end > file_len {
            return Err(FormatError::new(format!(
                "PAC1 chunk {} exceeds file size",
                String::from_utf8_lossy(&tag)
            )));
        }
        chunks.push(ChunkInfo {
            tag,
            body_start,
            body_len,
        });
        offset = body_end;
    }
    Ok(chunks)
}

fn parse_file_tree(bytes: &[u8], file_len: u64) -> FormatResult<Vec<Pac1Entry>> {
    let mut parser = FileTreeParser {
        bytes,
        offset: 0,
        file_len,
        entries: Vec::new(),
    };
    parser.parse_entry("", 0)?;
    if parser.offset != bytes.len() {
        return Err(FormatError::new("PAC1 FILE chunk has trailing bytes"));
    }
    Ok(parser.entries)
}

struct FileTreeParser<'a> {
    bytes: &'a [u8],
    offset: usize,
    file_len: u64,
    entries: Vec<Pac1Entry>,
}

impl FileTreeParser<'_> {
    fn parse_entry(&mut self, parent: &str, depth: usize) -> FormatResult<()> {
        if depth > MAX_TREE_DEPTH {
            return Err(FormatError::new("PAC1 FILE tree exceeded depth limit"));
        }
        if self.entries.len() >= MAX_TREE_ENTRIES {
            return Err(FormatError::new("PAC1 FILE tree exceeded entry limit"));
        }
        let kind = self.read_u8()?;
        let name_len = self.read_u8()? as usize;
        let name = self.read_name(name_len)?;
        let full_path = joined_path(parent, &name)?;
        match kind {
            0 => {
                let child_count = self.read_u32_le()? as usize;
                if child_count > MAX_CHILDREN_PER_FOLDER {
                    return Err(FormatError::new("PAC1 folder child count exceeded limit"));
                }
                for _ in 0..child_count {
                    self.parse_entry(&full_path, depth + 1)?;
                }
            }
            1 => {
                if full_path.is_empty() {
                    return Err(FormatError::new("PAC1 file entry has an empty path"));
                }
                let start = self.read_u32_le()? as u64;
                let compressed_len = self.read_u32_le()? as u64;
                let _original_len = self.read_u32_le()?;
                self.skip(4)?;
                let _compression_type = self.read_u32_be()?;
                self.skip(4)?;
                let end = start
                    .checked_add(compressed_len)
                    .ok_or_else(|| FormatError::new("PAC1 file entry offset overflowed"))?;
                if end > self.file_len {
                    return Err(FormatError::new(format!(
                        "PAC1 file entry {} exceeds file size",
                        full_path
                    )));
                }
                self.entries.push(Pac1Entry {
                    path: full_path,
                    span: LocalPartSpan {
                        start,
                        length: compressed_len,
                    },
                });
            }
            _ => {
                return Err(FormatError::new(format!(
                    "unknown PAC1 FILE entry kind {kind}"
                )));
            }
        }
        Ok(())
    }

    fn read_u8(&mut self) -> FormatResult<u8> {
        if self.offset >= self.bytes.len() {
            return Err(FormatError::new("unexpected EOF in PAC1 FILE tree"));
        }
        let value = self.bytes[self.offset];
        self.offset += 1;
        Ok(value)
    }

    fn read_u32_le(&mut self) -> FormatResult<u32> {
        let raw = self.read_array::<4>()?;
        Ok(u32::from_le_bytes(raw))
    }

    fn read_u32_be(&mut self) -> FormatResult<u32> {
        let raw = self.read_array::<4>()?;
        Ok(u32::from_be_bytes(raw))
    }

    fn read_array<const N: usize>(&mut self) -> FormatResult<[u8; N]> {
        if self.offset.saturating_add(N) > self.bytes.len() {
            return Err(FormatError::new("unexpected EOF in PAC1 FILE tree"));
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&self.bytes[self.offset..self.offset + N]);
        self.offset += N;
        Ok(out)
    }

    fn read_name(&mut self, len: usize) -> FormatResult<String> {
        if self.offset.saturating_add(len) > self.bytes.len() {
            return Err(FormatError::new("unexpected EOF in PAC1 FILE name"));
        }
        let raw = &self.bytes[self.offset..self.offset + len];
        self.offset += len;
        let name = std::str::from_utf8(raw)
            .map_err(|err| FormatError::new(format!("invalid PAC1 FILE name UTF-8: {err}")))?;
        if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
            return Err(FormatError::new(format!(
                "unsafe PAC1 FILE name component {}",
                name
            )));
        }
        Ok(name.to_string())
    }

    fn skip(&mut self, len: usize) -> FormatResult<()> {
        if self.offset.saturating_add(len) > self.bytes.len() {
            return Err(FormatError::new("unexpected EOF in PAC1 FILE tree"));
        }
        self.offset += len;
        Ok(())
    }
}

fn joined_path(parent: &str, name: &str) -> FormatResult<String> {
    if name.is_empty() {
        return Ok(parent.to_string());
    }
    if parent.is_empty() {
        Ok(name.to_string())
    } else {
        let len = parent
            .len()
            .checked_add(1)
            .and_then(|len| len.checked_add(name.len()))
            .ok_or_else(|| FormatError::new("PAC1 path length overflowed"))?;
        let mut out = String::with_capacity(len);
        out.push_str(parent);
        out.push('/');
        out.push_str(name);
        Ok(out)
    }
}

fn validate_no_overlaps(entries: &[Pac1Entry]) -> FormatResult<()> {
    let mut cursor = 0u64;
    for entry in entries {
        if entry.span.length == 0 {
            continue;
        }
        if entry.span.start < cursor {
            return Err(FormatError::new(format!(
                "PAC1 file entry {} overlaps a previous entry",
                entry.path
            )));
        }
        cursor = entry
            .span
            .start
            .checked_add(entry.span.length)
            .ok_or_else(|| FormatError::new("PAC1 file entry end overflowed"))?;
    }
    Ok(())
}

fn build_layout(file_len: u64, entries: Vec<Pac1Entry>) -> FormatResult<Pac1Layout> {
    let first_payload_start = entries
        .iter()
        .map(|entry| entry.span.start)
        .min()
        .unwrap_or(file_len);
    let mut parts = Vec::with_capacity(entries.len() + 2);
    let mut cursor = first_payload_start;
    let mut gap_index = 1usize;
    parts.push(Pac1Part {
        path: PAC1_HEADER_PART.to_string(),
        span: LocalPartSpan {
            start: 0,
            length: first_payload_start,
        },
    });

    for entry in &entries {
        if entry.span.start > cursor {
            let length = entry.span.start - cursor;
            parts.push(Pac1Part {
                path: format!("{}{}$$", PAC1_GAP_PREFIX, gap_index),
                span: LocalPartSpan {
                    start: cursor,
                    length,
                },
            });
            cursor = entry.span.start;
            gap_index += 1;
        }
        parts.push(Pac1Part {
            path: entry.path.clone(),
            span: entry.span,
        });
        cursor = cursor.max(entry.span.start.saturating_add(entry.span.length));
    }

    parts.push(Pac1Part {
        path: PAC1_END_PART.to_string(),
        span: LocalPartSpan {
            start: cursor,
            length: file_len.saturating_sub(cursor),
        },
    });
    assert_parts_tile_file(file_len, &parts)?;
    Ok(Pac1Layout {
        header: parts
            .first()
            .map(|part| part.span)
            .expect("header part exists"),
        end: parts.last().map(|part| part.span).expect("end part exists"),
        entry_count: entries.len(),
        parts,
    })
}

fn assert_parts_tile_file(file_len: u64, parts: &[Pac1Part]) -> FormatResult<()> {
    let mut cursor = 0u64;
    for part in parts {
        if part.span.start != cursor {
            return Err(FormatError::new(format!(
                "PAC1 parts do not tile the file at {} (expected {}, got {})",
                part.path, cursor, part.span.start
            )));
        }
        cursor = cursor
            .checked_add(part.span.length)
            .ok_or_else(|| FormatError::new("PAC1 part length overflowed"))?;
    }
    if cursor != file_len {
        return Err(FormatError::new(format!(
            "PAC1 parts do not cover file tail (cursor={cursor} file_len={file_len})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContentFormat, PAC1_END_PART, PAC1_HEADER_PART};

    fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
    }

    fn push_folder(out: &mut Vec<u8>, name: &str, child_count: u32) {
        out.push(0);
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&child_count.to_le_bytes());
    }

    fn push_file(out: &mut Vec<u8>, name: &str, offset: u32, length: u32) {
        out.push(1);
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 4]);
    }

    fn fixture_pak() -> tempfile_file::TempPath {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"PAC1");
        push_chunk(&mut bytes, b"HEAD", b"head");

        let data_chunk_start = bytes.len();
        bytes.extend_from_slice(b"DATA");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let data_body_start = bytes.len();
        bytes.extend_from_slice(b"ALPHA");
        let gap_start = bytes.len();
        bytes.extend_from_slice(b"gap");
        let beta_start = bytes.len();
        bytes.extend_from_slice(b"BETA");
        let data_body_len = bytes.len() - data_body_start;
        bytes[data_chunk_start + 4..data_chunk_start + 8]
            .copy_from_slice(&(data_body_len as u32).to_be_bytes());

        let mut tree = Vec::new();
        push_folder(&mut tree, "", 2);
        push_folder(&mut tree, "world", 1);
        push_file(&mut tree, "entities.bin", data_body_start as u32, 5);
        push_folder(&mut tree, "textures", 1);
        push_file(&mut tree, "icon.edds", beta_start as u32, 4);
        push_chunk(&mut bytes, b"FILE", &tree);

        let form_len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&form_len.to_be_bytes());
        assert_eq!(&bytes[gap_start..gap_start + 3], b"gap");
        tempfile_file::write("fixture.pak", &bytes)
    }

    #[test]
    fn pac1_parts_tile_nested_payloads_with_gap() {
        let path = fixture_pak();
        let parts = Pac1Format.parse_parts(path.as_path()).unwrap();
        let file_len = std::fs::metadata(path.as_path()).unwrap().len();

        assert_eq!(parts[0].path, PAC1_HEADER_PART);
        assert_eq!(parts[1].path, "world/entities.bin");
        assert_eq!(parts[2].path, "$$GAP:1$$");
        assert_eq!(parts[2].length, 3);
        assert_eq!(parts[3].path, "textures/icon.edds");
        assert_eq!(parts[4].path, PAC1_END_PART);

        let mut cursor = 0u64;
        for part in &parts {
            assert_eq!(part.start, cursor);
            cursor += part.length;
        }
        assert_eq!(cursor, file_len);
    }

    #[test]
    fn local_layout_maps_entries_and_gaps_by_name() {
        let path = fixture_pak();
        let layout = Pac1Format.parse_local_layout(path.as_path()).unwrap();
        let spans = layout.map_part_spans([
            "$$HEADER$$",
            "WORLD/ENTITIES.BIN",
            "$$GAP:1$$",
            "textures/icon.edds",
            "$$END$$",
        ]);

        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0], Some(layout.header));
        assert!(spans[1].is_some());
        assert_eq!(spans[2].unwrap().length, 3);
        assert!(spans[3].is_some());
        assert_eq!(spans[4], Some(layout.end));
        assert_eq!(layout.entry_count, 2);
        assert_eq!(layout.entry_payload_bytes, 9);
    }

    #[test]
    fn pac1_marker_detection_uses_gap_to_avoid_pbo_collision() {
        assert!(!Pac1Format.remote_layout_matches(&["$$HEADER$$", "$$END$$"]));
        assert!(Pac1Format.remote_layout_matches(&[
            "$$HEADER$$",
            "file.bin",
            "$$GAP:1$$",
            "$$END$$",
        ]));
    }

    #[test]
    fn pac1_truncated_chunk_is_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&16u32.to_be_bytes());
        bytes.extend_from_slice(b"PAC1");
        bytes.extend_from_slice(b"DATA");
        bytes.extend_from_slice(&100u32.to_be_bytes());
        let path = tempfile_file::write("bad.pak", &bytes);

        assert!(Pac1Format.parse_parts(path.as_path()).is_err());
    }

    #[test]
    fn pac1_lying_file_offset_is_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"PAC1");
        push_chunk(&mut bytes, b"DATA", b"abc");
        let mut tree = Vec::new();
        push_folder(&mut tree, "", 1);
        push_file(&mut tree, "bad.bin", 999, 10);
        push_chunk(&mut bytes, b"FILE", &tree);
        let form_len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&form_len.to_be_bytes());
        let path = tempfile_file::write("bad-offset.pak", &bytes);

        assert!(Pac1Format.parse_parts(path.as_path()).is_err());
    }

    #[test]
    fn pac1_deep_tree_is_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"PAC1");
        push_chunk(&mut bytes, b"DATA", b"x");

        let mut tree = Vec::new();
        for depth in 0..=MAX_TREE_DEPTH + 1 {
            push_folder(&mut tree, &format!("d{}", depth), 1);
        }
        push_file(&mut tree, "payload.bin", 20, 1);
        push_chunk(&mut bytes, b"FILE", &tree);
        let form_len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&form_len.to_be_bytes());
        let path = tempfile_file::write("deep.pak", &bytes);

        assert!(Pac1Format.parse_parts(path.as_path()).is_err());
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
