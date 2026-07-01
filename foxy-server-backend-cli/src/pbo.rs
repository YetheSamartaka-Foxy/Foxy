use anyhow::{Context, Result, bail};
use std::io::{Read, Seek};
use std::path::Path;

use crate::types::{Checksums, FilePart};

pub const PBO_HEADER_PART: &str = "$$HEADER$$";
pub const PBO_END_PART: &str = "$$END$$";
const NON_PBO_PART_SIZE: u64 = 5_000_000;

/// Parse a PBO file and return the list of parts (header, entries, tail).
/// Each part has a path, byte offset (start), and length.
pub fn parse_pbo_parts(file_path: &Path) -> Result<Vec<FilePart>> {
    let mut file = std::fs::File::open(file_path)
        .with_context(|| format!("Failed to open PBO: {}", file_path.display()))?;
    let file_len = file
        .metadata()
        .with_context(|| format!("Failed to read PBO metadata: {}", file_path.display()))?
        .len();

    // Validate magic: first byte 0x00, then "sreV"
    let mut first = [0u8; 1];
    file.read_exact(&mut first)
        .context("Failed to read PBO first byte")?;
    if first[0] != 0 {
        bail!(
            "Invalid PBO header first byte: expected 0, got {}",
            first[0]
        );
    }

    let mut tag = [0u8; 4];
    file.read_exact(&mut tag)
        .context("Failed to read PBO tag")?;
    if &tag != b"sreV" {
        bail!(
            "Invalid PBO header tag: expected sreV, got {}",
            String::from_utf8_lossy(&tag)
        );
    }

    // Skip 16 bytes of header extension fields (reserved/etc)
    file.seek(std::io::SeekFrom::Current(16))
        .context("Failed to skip PBO header extension bytes")?;

    // Read header extension properties
    let mut marker = [0u8; 1];
    file.read_exact(&mut marker)
        .context("Failed to read PBO extension marker")?;
    if marker[0] != 0 {
        file.seek(std::io::SeekFrom::Current(-1))
            .context("Failed to rewind PBO extension marker")?;
        // Read prefix property (key + value)
        let _ = read_cstring(&mut file)?;
        let _ = read_cstring(&mut file)?;
        // Read remaining properties as key-value pairs
        loop {
            let name = read_cstring(&mut file)?;
            if name.is_empty() {
                break;
            }
            let _value = read_cstring(&mut file)?;
        }
    }

    // Read entry headers
    let mut raw_entries: Vec<(String, u64)> = Vec::new();
    loop {
        let name = read_cstring(&mut file)?;
        let _packing_method = read_u32(&mut file)?;
        let _size = read_u32(&mut file)?;
        let _reserved = read_u32(&mut file)?;
        let _timestamp = read_u32(&mut file)?;
        let data_size = read_u32(&mut file)? as u64;

        if name.is_empty() {
            break;
        }

        let path = String::from_utf8_lossy(&name).to_string();
        raw_entries.push((path, data_size));
    }

    let header_len = file
        .stream_position()
        .context("Failed to resolve PBO header size")?;

    // Build parts list: $$HEADER$$, entries, $$END$$
    let mut parts = Vec::with_capacity(raw_entries.len() + 2);

    // Header part
    parts.push(FilePart {
        path: PBO_HEADER_PART.to_string(),
        checksums: Checksums::default(),
        start: 0,
        length: header_len,
    });

    // Data entries
    let mut cursor = header_len;
    for (path, length) in raw_entries {
        parts.push(FilePart {
            path,
            checksums: Checksums::default(),
            start: cursor,
            length,
        });
        cursor = cursor
            .checked_add(length)
            .context("PBO entry offsets overflowed u64")?;
    }

    if cursor > file_len {
        bail!(
            "PBO layout exceeds file size (entries_end={} file_len={})",
            cursor,
            file_len
        );
    }

    // Swifty always emits an $$END$$ part, even when the PBO has no trailing bytes.
    let tail_len = file_len.saturating_sub(cursor);
    parts.push(FilePart {
        path: PBO_END_PART.to_string(),
        checksums: Checksums::default(),
        start: cursor,
        length: tail_len,
    });

    Ok(parts)
}

/// Check if a file is likely a PBO by extension.
pub fn is_pbo(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pbo"))
}

/// Build a single-part list for a non-PBO file.
pub fn single_part(relative_path: &str, file_size: u64) -> Vec<FilePart> {
    if file_size == 0 {
        return Vec::new();
    }

    let file_name = Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(relative_path);

    let mut parts = Vec::new();
    let mut start = 0;
    while start < file_size {
        let length = (file_size - start).min(NON_PBO_PART_SIZE);
        let end = start + length;
        parts.push(FilePart {
            path: format!("{}_{}", file_name, end),
            checksums: Checksums::default(),
            start,
            length,
        });
        start = end;
    }

    parts
}

fn read_cstring(file: &mut std::fs::File) -> Result<Vec<u8>> {
    const MAX_LEN: usize = 8 * 1024;
    let mut out = Vec::with_capacity(128);
    loop {
        let mut byte = [0u8; 1];
        file.read_exact(&mut byte)
            .context("Failed to read PBO string byte")?;
        if byte[0] == 0 {
            break;
        }
        out.push(byte[0]);
        if out.len() > MAX_LEN {
            bail!("PBO string exceeded safety limit");
        }
    }
    Ok(out)
}

fn read_u32(file: &mut std::fs::File) -> Result<u32> {
    let mut raw = [0u8; 4];
    file.read_exact(&mut raw)
        .context("Failed to read PBO u32")?;
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("foxy-server-backend-cli-{name}-{unique}.pbo"))
    }

    // ── is_pbo ───────────────────────────────────────────────────────────

    #[test]
    fn is_pbo_extension_lowercase() {
        assert!(is_pbo(std::path::Path::new("addons/test.pbo")));
    }

    #[test]
    fn is_pbo_extension_uppercase() {
        assert!(is_pbo(std::path::Path::new("ADDONS/TEST.PBO")));
    }

    #[test]
    fn is_pbo_extension_mixed_case() {
        assert!(is_pbo(std::path::Path::new("file.Pbo")));
    }

    #[test]
    fn is_pbo_non_pbo_extension() {
        assert!(!is_pbo(std::path::Path::new("file.txt")));
        assert!(!is_pbo(std::path::Path::new("file.bikey")));
        assert!(!is_pbo(std::path::Path::new("file.cpp")));
    }

    #[test]
    fn is_pbo_no_extension() {
        assert!(!is_pbo(std::path::Path::new("noext")));
    }

    // ── single_part ─────────────────────────────────────────────────────

    #[test]
    fn single_part_zero_size_returns_empty() {
        let parts = single_part("file.bin", 0);
        assert!(parts.is_empty());
    }

    #[test]
    fn single_part_small_file_one_chunk() {
        let parts = single_part("addons/test.bin", 1000);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts[0].length, 1000);
        assert!(parts[0].path.contains("test.bin"));
    }

    #[test]
    fn single_part_exactly_chunk_size() {
        let parts = single_part("file.bin", NON_PBO_PART_SIZE);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].length, NON_PBO_PART_SIZE);
    }

    #[test]
    fn single_part_large_file_multiple_chunks() {
        let size = NON_PBO_PART_SIZE * 3 + 100;
        let parts = single_part("large.bin", size);
        assert_eq!(parts.len(), 4);

        // Verify contiguous coverage
        let mut expected_start = 0u64;
        for part in &parts {
            assert_eq!(part.start, expected_start);
            expected_start += part.length;
        }
        assert_eq!(expected_start, size);
    }

    #[test]
    fn single_part_uses_filename_not_full_path() {
        let parts = single_part("addons/sub/deep.bin", 100);
        assert!(
            parts[0].path.starts_with("deep.bin"),
            "path should use filename only: {}",
            parts[0].path
        );
    }

    // ── parse_pbo_parts ─────────────────────────────────────────────────

    #[test]
    fn parse_pbo_invalid_first_byte() {
        let path = temp_file("bad-first");
        let bytes: Vec<u8> = vec![0xFF, b's', b'r', b'e', b'V'];
        std::fs::write(&path, bytes).unwrap();
        let result = parse_pbo_parts(&path);
        assert!(result.is_err());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn parse_pbo_invalid_tag() {
        let path = temp_file("bad-tag");
        let bytes: Vec<u8> = vec![0x00, b'N', b'O', b'P', b'E'];
        std::fs::write(&path, bytes).unwrap();
        let result = parse_pbo_parts(&path);
        assert!(result.is_err());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn parse_pbo_missing_file_errors() {
        let result = parse_pbo_parts(std::path::Path::new("/nonexistent/file.pbo"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_pbo_parts_keeps_zero_length_end_part_for_swifty_compatibility() {
        let path = temp_file("zero-end");
        let bytes: Vec<u8> = [
            &[0u8][..],
            &b"sreV"[..],
            &[0u8; 16][..],
            &[0u8][..],
            &[0u8][..],
            &[0u8; 20][..],
        ]
        .concat();
        std::fs::write(&path, bytes).expect("test pbo should be written");

        let parts = parse_pbo_parts(&path).expect("minimal pbo should parse");

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].path, PBO_HEADER_PART);
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts[0].length, 43);
        assert_eq!(parts[1].path, PBO_END_PART);
        assert_eq!(parts[1].start, 43);
        assert_eq!(parts[1].length, 0);

        std::fs::remove_file(path).expect("test pbo should be removed");
    }
}
