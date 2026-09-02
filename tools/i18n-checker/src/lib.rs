//! Shared locale-file helpers for the `i18n-checker` and `locale-apply` binaries.
//!
//! Locale JSON is edited line-by-line rather than reserialized, so diffs stay
//! value-only: no key reordering, indentation churn, or CRLF/LF churn.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::{env, fs};

/// Plural suffixes that are valid in non-English locales but not in en.json.
/// These are CLDR plural categories used by the app's `plural_category` function.
pub const VALID_PLURAL_SUFFIXES: &[&str] = &[".one", ".few", ".other"];

/// Indentation used for every top-level entry in the locale JSON files.
pub const ENTRY_INDENT: &str = "    ";

#[derive(Debug)]
pub struct LocaleLoad {
    pub map: serde_json::Map<String, Value>,
    pub duplicate_keys: Vec<String>,
    pub raw: String,
}

pub fn find_locales_dir() -> Result<PathBuf, String> {
    let candidates = [
        PathBuf::from("src/ui/locales"),
        PathBuf::from("../../src/ui/locales"),
    ];
    for candidate in &candidates {
        if candidate.is_dir() {
            return Ok(candidate.clone());
        }
    }
    // Fall back: check CARGO_MANIFEST_DIR (set during `cargo run`).
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        let from_manifest = Path::new(&manifest_dir).join("../../src/ui/locales");
        if from_manifest.is_dir() {
            return Ok(from_manifest);
        }
    }
    Err(
        "Could not find src/ui/locales directory. Run from the repository root:\n  cargo run --manifest-path tools/i18n-checker/Cargo.toml"
            .to_string(),
    )
}

/// Non-English locale files in the directory, sorted by path.
pub fn translation_files(locales_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(locales_dir).map_err(|e| {
        format!(
            "Failed to read locales directory {}: {e}",
            locales_dir.display()
        )
    })?;

    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "json")
                && p.file_name().is_some_and(|name| name != "en.json")
                && !p.file_name().unwrap().to_string_lossy().contains("_batch_")
        })
        .collect();
    files.sort();
    Ok(files)
}

pub fn read_locale_text(path: &Path) -> Result<String, String> {
    let raw =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    Ok(raw.trim_start_matches('\u{feff}').to_string())
}

pub fn parse_locale_object(
    raw: &str,
    label: &str,
) -> Result<serde_json::Map<String, Value>, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|e| format!("Invalid JSON in {label}: {e}"))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(format!("{label} is not a JSON object")),
    }
}

pub fn load_locale_file(path: &Path) -> Result<LocaleLoad, String> {
    let raw = read_locale_text(path)?;
    let map = parse_locale_object(&raw, &path.display().to_string())?;
    let duplicate_keys = find_duplicate_top_level_keys(&raw);
    Ok(LocaleLoad {
        map,
        duplicate_keys,
        raw,
    })
}

/// Reads a UTF-8 key list: one exact `en.json` key per line, `\n` for embedded
/// newlines, `#` comments and blank lines ignored.
pub fn read_key_file(path: &Path) -> Result<Vec<String>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read key file {}: {e}", path.display()))?;

    Ok(raw
        .lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.replace("\\n", "\n"))
        .collect())
}

pub fn write_key_file(path: &Path, keys: &[String]) -> Result<(), String> {
    let body: String = keys
        .iter()
        .map(|key| format!("{}\n", key.replace('\n', "\\n")))
        .collect();
    fs::write(path, body).map_err(|e| format!("Failed to write key file {}: {e}", path.display()))
}

/// `{placeholder}` names reachable from a locale value, matching the
/// `\{[A-Za-z_][A-Za-z0-9_]*\}` form the app's formatter substitutes.
pub fn placeholder_names(value: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    collect_placeholders(value, &mut found);
    found
}

fn collect_placeholders(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => collect_placeholders_in_str(text, out),
        Value::Object(map) => {
            for nested in map.values() {
                collect_placeholders(nested, out);
            }
        }
        _ => {}
    }
}

fn collect_placeholders_in_str(text: &str, out: &mut BTreeSet<String>) {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        if end < bytes.len() && (bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
            end += 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b'}' {
                out.insert(text[start..end].to_string());
                index = end + 1;
                continue;
            }
        }
        index += 1;
    }
}

/// Top-level keys in file order, including repeats.
pub fn top_level_keys(raw: &str) -> Vec<String> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut string_start = None;
    let mut keys = Vec::new();

    for (idx, ch) in raw.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
                if depth == 1 && is_key_string(raw, idx + ch.len_utf8()) {
                    let start = string_start.unwrap_or(idx);
                    let literal = &raw[start..idx + ch.len_utf8()];
                    if let Ok(key) = serde_json::from_str::<String>(literal) {
                        keys.push(key);
                    }
                }
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                string_start = Some(idx);
            }
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }

    keys
}

pub fn find_duplicate_top_level_keys(raw: &str) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut duplicates = BTreeSet::<String>::new();
    for key in top_level_keys(raw) {
        let count = counts.entry(key.clone()).or_insert(0);
        *count += 1;
        if *count == 2 {
            duplicates.insert(key);
        }
    }
    duplicates.into_iter().collect()
}

fn is_key_string(raw: &str, offset: usize) -> bool {
    raw[offset..]
        .chars()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch == ':')
}

/// Returns true if an "extra" key is actually a valid plural form whose base key exists in en.json.
pub fn is_valid_plural_extra(key: &str, en_keys: &BTreeSet<&str>) -> bool {
    for suffix in VALID_PLURAL_SUFFIXES {
        if let Some(base) = key.strip_suffix(suffix)
            && en_keys.contains(base)
        {
            return true;
        }
    }
    false
}

/// Splits on `\n` only, keeping the terminator so `\r\n` files round-trip
/// byte-for-byte. JSON forbids raw control characters inside strings, so no
/// value can be split apart by this.
pub fn split_lines_keep_ends(raw: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (idx, _) in raw.match_indices('\n') {
        lines.push(raw[start..=idx].to_string());
        start = idx + 1;
    }
    if start < raw.len() {
        lines.push(raw[start..].to_string());
    }
    lines
}

pub fn detect_line_ending(lines: &[String]) -> &'static str {
    if lines.iter().any(|line| line.ends_with("\r\n")) {
        "\r\n"
    } else {
        "\n"
    }
}

/// JSON string literal for a key, non-ASCII left as-is.
pub fn serialize_json_key(key: &str) -> String {
    serde_json::to_string(key).expect("string always serializes")
}

/// Serializes a locale value with the spacing used by the locale files.
pub fn serialize_locale_value(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", serialize_json_key(k), serialize_locale_value(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        other => serde_json::to_string(other).expect("locale value always serializes"),
    }
}

pub fn locale_entry_line(key: &str, value: &Value, newline: &str, comma: bool) -> String {
    let suffix = if comma { "," } else { "" };
    format!(
        "{ENTRY_INDENT}{}: {}{suffix}{newline}",
        serialize_json_key(key),
        serialize_locale_value(value)
    )
}

pub fn find_entry_line(lines: &[String], key: &str) -> Option<usize> {
    let needle = format!("{ENTRY_INDENT}{}:", serialize_json_key(key));
    lines.iter().position(|line| line.starts_with(&needle))
}

/// Nearest earlier `en.json` key that the target locale already contains, used
/// as the insertion anchor so new entries land close to their English order.
pub fn previous_present_key<'a>(
    en_keys: &'a [String],
    target: &str,
    available: &BTreeSet<&str>,
) -> Option<&'a str> {
    let index = en_keys.iter().position(|key| key == target)?;
    en_keys[..index]
        .iter()
        .rev()
        .find(|key| available.contains(key.as_str()))
        .map(String::as_str)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_top_level_keys_are_detected() {
        let raw = r#"{
            "One": "First",
            "Nested": { "One": "Allowed" },
            "One": "Second",
            "Line\nKey": "First",
            "Line\nKey": "Second"
        }"#;

        let duplicates = find_duplicate_top_level_keys(raw);

        assert_eq!(duplicates, vec!["Line\nKey".to_string(), "One".to_string()]);
    }

    #[test]
    fn nested_duplicate_keys_are_ignored() {
        let raw = r#"{
            "One": { "Nested": "First", "Nested": "Second" },
            "Two": "Value"
        }"#;

        assert!(find_duplicate_top_level_keys(raw).is_empty());
    }

    #[test]
    fn top_level_keys_keep_file_order() {
        let raw = r#"{
            "Zed": "1",
            "Alpha": { "Inner": "2" },
            "Mid": "3"
        }"#;

        assert_eq!(top_level_keys(raw), vec!["Zed", "Alpha", "Mid"]);
    }

    #[test]
    fn placeholders_are_extracted_from_strings_and_objects() {
        assert_eq!(
            placeholder_names(&json!("Copy {count} of {name} to {path}")),
            ["count", "name", "path"]
                .into_iter()
                .map(String::from)
                .collect()
        );
        assert_eq!(
            placeholder_names(&json!({ "one": "{count} file", "other": "{count} files" })),
            ["count"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn placeholder_scanner_rejects_malformed_braces() {
        assert!(placeholder_names(&json!("{ name } {1bad} {} {}}")).is_empty());
        assert_eq!(
            placeholder_names(&json!("{{name}")),
            ["name"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn placeholder_scanner_handles_non_ascii_text() {
        assert_eq!(
            placeholder_names(&json!("Каталог {game} не найден по пути {path}")),
            ["game", "path"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn lines_round_trip_with_either_line_ending() {
        for raw in ["a\r\nb\r\n", "a\nb\n", "a\nb"] {
            let lines = split_lines_keep_ends(raw);
            assert_eq!(lines.concat(), raw);
        }
        assert_eq!(
            detect_line_ending(&split_lines_keep_ends("a\r\nb\r\n")),
            "\r\n"
        );
        assert_eq!(detect_line_ending(&split_lines_keep_ends("a\nb\n")), "\n");
    }

    #[test]
    fn entry_lines_match_locale_formatting() {
        assert_eq!(
            locale_entry_line("Ключ", &json!("Значение"), "\n", true),
            "    \"Ключ\": \"Значение\",\n"
        );
        assert_eq!(
            locale_entry_line("Line\nKey", &json!("V"), "\r\n", false),
            "    \"Line\\nKey\": \"V\"\r\n"
        );
        assert_eq!(
            locale_entry_line("P", &json!({ "one": "a", "other": "b" }), "\n", true),
            "    \"P\": {\"one\": \"a\", \"other\": \"b\"},\n"
        );
    }

    #[test]
    fn entry_lookup_matches_the_serialized_key() {
        let lines =
            split_lines_keep_ends("{\n    \"Line\\nKey\": \"A\",\n    \"Other\": \"B\"\n}\n");

        assert_eq!(find_entry_line(&lines, "Line\nKey"), Some(1));
        assert_eq!(find_entry_line(&lines, "Other"), Some(2));
        assert_eq!(find_entry_line(&lines, "Missing"), None);
    }

    #[test]
    fn insertion_anchor_is_the_nearest_earlier_present_key() {
        let en_keys: Vec<String> = ["A", "B", "C", "D"].into_iter().map(String::from).collect();
        let available: BTreeSet<&str> = ["A", "C"].into_iter().collect();

        assert_eq!(previous_present_key(&en_keys, "D", &available), Some("C"));
        assert_eq!(previous_present_key(&en_keys, "B", &available), Some("A"));
        assert_eq!(previous_present_key(&en_keys, "A", &available), None);
        assert_eq!(previous_present_key(&en_keys, "Unknown", &available), None);
    }

    #[test]
    fn truncate_splits_on_character_boundaries() {
        assert_eq!(truncate("Привет", 3), "При...");
        assert_eq!(truncate("short", 10), "short");
    }
}
