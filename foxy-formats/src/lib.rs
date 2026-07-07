mod pac1;
mod pbo;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;

pub use pac1::{
    PAC1_END_PART, PAC1_FORMAT_ID, PAC1_GAP_PREFIX, PAC1_HEADER_PART, Pac1Format, is_pac1_gap_part,
};
pub use pbo::{PBO_END_PART, PBO_FORMAT_ID, PBO_HEADER_PART, PboFormat};

pub const SINGLE_PART_SIZE: u64 = 5_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePart {
    pub path: String,
    pub start: u64,
    pub length: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalPartSpan {
    pub start: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalLayout {
    pub header: LocalPartSpan,
    pub end: LocalPartSpan,
    pub parts_by_path: HashMap<String, VecDeque<LocalPartSpan>>,
    pub entry_count: usize,
    pub entry_payload_bytes: u64,
}

impl LocalLayout {
    pub fn map_part_spans<'a>(
        &self,
        part_paths: impl IntoIterator<Item = &'a str>,
    ) -> Vec<Option<LocalPartSpan>> {
        let mut parts_by_path = self.parts_by_path.clone();
        part_paths
            .into_iter()
            .map(|path| {
                if is_header_part(path) {
                    return Some(self.header);
                }
                if is_end_part(path) {
                    return Some(self.end);
                }
                let key = normalize_part_path(path);
                parts_by_path.get_mut(&key).and_then(|q| q.pop_front())
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError {
    message: String,
}

impl FormatError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FormatError {}

pub type FormatResult<T> = Result<T, FormatError>;

pub trait ContentFormat: Send + Sync {
    fn id(&self) -> &'static str;
    fn matches(&self, path: &Path, head: &[u8]) -> bool;
    fn remote_layout_matches(&self, part_paths: &[&str]) -> bool;
    fn parse_parts(&self, path: &Path) -> FormatResult<Vec<FilePart>>;
    fn parse_local_layout(&self, path: &Path) -> FormatResult<LocalLayout>;
}

pub struct FormatRegistry {
    formats: Vec<Box<dyn ContentFormat>>,
}

impl FormatRegistry {
    pub fn new(formats: Vec<Box<dyn ContentFormat>>) -> Self {
        Self { formats }
    }

    pub fn builtin() -> Self {
        Self::new(vec![Box::new(PboFormat), Box::new(Pac1Format)])
    }

    pub fn format_id_for_path(&self, path: &Path) -> Option<&'static str> {
        self.matching_format(path).map(|format| format.id())
    }

    pub fn format_id_for_remote_parts(&self, part_paths: &[&str]) -> Option<&'static str> {
        self.formats
            .iter()
            .find(|format| format.remote_layout_matches(part_paths))
            .map(|format| format.id())
    }

    pub fn parse_parts_for_file(
        &self,
        path: &Path,
    ) -> FormatResult<Option<(&'static str, Vec<FilePart>)>> {
        let Some(format) = self.matching_format(path) else {
            return Ok(None);
        };
        format
            .parse_parts(path)
            .map(|parts| Some((format.id(), parts)))
    }

    pub fn parse_local_layout_for_format(
        &self,
        format_id: &str,
        path: &Path,
    ) -> FormatResult<LocalLayout> {
        let Some(format) = self.formats.iter().find(|format| format.id() == format_id) else {
            return Err(FormatError::new(format!(
                "unknown content format: {format_id}"
            )));
        };
        format.parse_local_layout(path)
    }

    fn matching_format(&self, path: &Path) -> Option<&dyn ContentFormat> {
        let head = read_head(path).unwrap_or_default();
        self.formats
            .iter()
            .map(|format| format.as_ref())
            .find(|format| format.matches(path, &head))
    }
}

pub fn builtin_registry() -> FormatRegistry {
    FormatRegistry::builtin()
}

pub fn single_file_parts(relative_path: &str, file_size: u64) -> Vec<FilePart> {
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
        let length = (file_size - start).min(SINGLE_PART_SIZE);
        let end = start + length;
        parts.push(FilePart {
            path: format!("{}_{}", file_name, end),
            start,
            length,
        });
        start = end;
    }

    parts
}

pub fn normalize_part_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

pub fn is_header_part(path: &str) -> bool {
    path.eq_ignore_ascii_case(PBO_HEADER_PART)
}

pub fn is_end_part(path: &str) -> bool {
    path.eq_ignore_ascii_case(PBO_END_PART)
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut head = vec![0u8; 16];
    let len = file.read(&mut head)?;
    head.truncate(len);
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_part_zero_size_returns_empty() {
        assert!(single_file_parts("file.bin", 0).is_empty());
    }

    #[test]
    fn single_part_large_file_is_chunked() {
        let size = SINGLE_PART_SIZE * 3 + 100;
        let parts = single_file_parts("addons/test.bin", size);

        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].path, format!("test.bin_{}", SINGLE_PART_SIZE));
        assert_eq!(parts[0].start, 0);
        assert_eq!(parts[0].length, SINGLE_PART_SIZE);
        assert_eq!(parts[3].start, SINGLE_PART_SIZE * 3);
        assert_eq!(parts[3].length, 100);
    }

    #[test]
    fn part_name_helpers_are_case_insensitive() {
        assert!(is_header_part("$$header$$"));
        assert!(is_end_part("$$end$$"));
        assert!(!is_header_part("header"));
        assert!(!is_end_part("end"));
    }

    #[test]
    fn registry_detects_pbo_remote_markers() {
        let registry = builtin_registry();
        assert_eq!(
            registry.format_id_for_remote_parts(&["$$HEADER$$", "entry.bin", "$$END$$"]),
            Some(PBO_FORMAT_ID)
        );
        assert_eq!(registry.format_id_for_remote_parts(&["entry.bin"]), None);
    }

    #[test]
    fn registry_detects_pac1_remote_gap_markers() {
        let registry = builtin_registry();
        assert_eq!(
            registry.format_id_for_remote_parts(&[
                "$$HEADER$$",
                "world/entities.bin",
                "$$GAP:1$$",
                "$$END$$",
            ]),
            Some(PAC1_FORMAT_ID)
        );
    }
}
