//! i18n-checker - verifies locale JSON files against en.json (the source of truth).
//!
//! Run from the repository root:
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --require-translated-key-file changed-keys.txt
//!
//! Or from inside the tool directory:
//!   cd tools/i18n-checker && cargo run
//!
//! Checks performed:
//!   - Missing keys (present in en.json but absent in locale; warning by default, error in --strict)
//!   - Extra keys (present in locale but absent in en.json), excluding valid plural suffixes
//!   - Duplicate keys in locale JSON files
//!   - JSON parse errors
//!   - Optional exact-English fallback checks for explicitly listed keys
//!   - Summary with pass/fail per locale

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

/// Plural suffixes that are valid in non-English locales but not in en.json.
/// These are CLDR plural categories used by the app's `plural_category` function.
const VALID_PLURAL_SUFFIXES: &[&str] = &[".one", ".few", ".other"];

fn main() {
    let config = parse_args();
    let locales_dir = find_locales_dir();

    let en_path = locales_dir.join("en.json");
    let en_load = load_locale_file(&en_path);
    if !en_load.duplicate_keys.is_empty() {
        eprintln!("Duplicate keys in {}:", en_path.display());
        for key in &en_load.duplicate_keys {
            eprintln!("  [!] DUPLICATE: {}", truncate(key, 100));
        }
        process::exit(1);
    }
    let en_map = en_load.map;
    let en_keys: BTreeSet<&str> = en_map.keys().map(|s| s.as_str()).collect();
    validate_required_translated_keys(&config.required_translated_keys, &en_keys);

    let mut locale_files: Vec<PathBuf> = fs::read_dir(&locales_dir)
        .unwrap_or_else(|e| {
            eprintln!(
                "Failed to read locales directory {}: {e}",
                locales_dir.display()
            );
            process::exit(1);
        })
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "json")
                && p.file_name().is_some_and(|name| name != "en.json")
                && !p.file_name().unwrap().to_string_lossy().contains("_batch_")
        })
        .collect();
    locale_files.sort();

    let mut total_issues = 0usize;
    let mut total_fallbacks = 0usize;
    let mut files_with_issues = 0usize;

    println!();
    println!(
        "i18n-checker: checking {} locale files against en.json ({} keys)",
        locale_files.len(),
        en_keys.len()
    );
    println!("{}", "=".repeat(72));

    for path in &locale_files {
        let filename = path.file_name().unwrap().to_string_lossy();
        let locale_load = load_locale_file(path);
        let locale_map = locale_load.map;
        let locale_keys: BTreeSet<&str> = locale_map.keys().map(|s| s.as_str()).collect();

        let missing: Vec<&str> = en_keys.difference(&locale_keys).copied().collect();
        total_fallbacks += missing.len();

        let extra: Vec<&str> = locale_keys
            .difference(&en_keys)
            .copied()
            .filter(|key| !is_valid_plural_extra(key, &en_keys))
            .collect();

        let empty_values: Vec<&str> = locale_map
            .iter()
            .filter(|(k, v)| {
                en_keys.contains(k.as_str())
                    && v.as_str().is_some_and(|s| s.is_empty())
                    && en_map
                        .get(k.as_str())
                        .and_then(|v| v.as_str())
                        .is_some_and(|s| !s.is_empty())
            })
            .map(|(k, _)| k.as_str())
            .collect();

        let untranslated_required: Vec<&str> = config
            .required_translated_keys
            .iter()
            .filter_map(|key| {
                let english = en_map.get(key.as_str()).and_then(|v| v.as_str())?;
                let translated = locale_map.get(key.as_str()).and_then(|v| v.as_str())?;
                (translated == english).then_some(key.as_str())
            })
            .collect();

        let issue_count = extra.len()
            + empty_values.len()
            + locale_load.duplicate_keys.len()
            + untranslated_required.len()
            + if config.strict_missing {
                missing.len()
            } else {
                0
            };

        if issue_count == 0 {
            if missing.is_empty() {
                println!("  {filename:<20} OK ({} keys)", locale_keys.len());
            } else {
                println!(
                    "  {filename:<20} OK ({} keys, {} fallback missing)",
                    locale_keys.len(),
                    missing.len()
                );
                for key in &missing {
                    println!("    [?] FALLBACK: {}", truncate(key, 100));
                }
            }
        } else {
            files_with_issues += 1;
            total_issues += issue_count;
            println!(
                "  {filename:<20} ISSUES ({} keys, {} missing, {} unexpected extra, {} empty, {} duplicate, {} untranslated required)",
                locale_keys.len(),
                missing.len(),
                extra.len(),
                empty_values.len(),
                locale_load.duplicate_keys.len(),
                untranslated_required.len()
            );
            for key in &missing {
                let marker = if config.strict_missing {
                    "[-] MISSING"
                } else {
                    "[?] FALLBACK"
                };
                println!("    {marker}: {}", truncate(key, 100));
            }
            for key in &extra {
                println!("    [+] EXTRA:   {}", truncate(key, 100));
            }
            for key in &empty_values {
                println!("    [!] EMPTY:   {}", truncate(key, 100));
            }
            for key in &locale_load.duplicate_keys {
                println!("    [!] DUPLICATE: {}", truncate(key, 100));
            }
            for key in &untranslated_required {
                println!("    [!] ENGLISH: {}", truncate(key, 100));
            }
        }
    }

    println!("{}", "=".repeat(72));
    if total_issues == 0 {
        if total_fallbacks == 0 {
            println!(
                "All {} locale files are fully translated against en.json.",
                locale_files.len()
            );
        } else {
            println!(
                "No blocking locale issues found. {total_fallbacks} missing translation(s) will fall back to en.json."
            );
        }
    } else {
        println!("{files_with_issues} file(s) with {total_issues} issue(s) found.");
        process::exit(1);
    }
}

#[derive(Debug, Default)]
struct Config {
    strict_missing: bool,
    required_translated_keys: BTreeSet<String>,
}

fn parse_args() -> Config {
    let mut config = Config::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--strict" => config.strict_missing = true,
            "--require-translated-key" => {
                let Some(key) = args.next() else {
                    eprintln!("--require-translated-key requires a key argument");
                    process::exit(1);
                };
                config.required_translated_keys.insert(key);
            }
            "--require-translated-key-file" => {
                let Some(path) = args.next() else {
                    eprintln!("--require-translated-key-file requires a path argument");
                    process::exit(1);
                };
                for key in read_required_translated_key_file(Path::new(&path)) {
                    config.required_translated_keys.insert(key);
                }
            }
            "--help" | "-h" => {
                print_help();
                process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                print_help();
                process::exit(1);
            }
        }
    }

    config
}

fn print_help() {
    println!("Usage: cargo run --manifest-path tools/i18n-checker/Cargo.toml -- [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --strict");
    println!("      Treat missing locale keys as errors instead of fallbacks.");
    println!("  --require-translated-key <key>");
    println!("      Fail when a non-English locale keeps the English value for this key.");
    println!("  --require-translated-key-file <path>");
    println!("      Read required translated keys from a UTF-8 text file, one key per line.");
    println!("      Use \\n in the file for newline characters inside a JSON key.");
}

fn read_required_translated_key_file(path: &Path) -> Vec<String> {
    let raw = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read required key file {}: {e}", path.display());
        process::exit(1);
    });

    raw.lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}'))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.replace("\\n", "\n"))
        .collect()
}

fn validate_required_translated_keys(required: &BTreeSet<String>, en_keys: &BTreeSet<&str>) {
    let unknown: Vec<&str> = required
        .iter()
        .map(String::as_str)
        .filter(|key| !en_keys.contains(key))
        .collect();

    if !unknown.is_empty() {
        eprintln!("Required translated key(s) not found in en.json:");
        for key in unknown {
            eprintln!("  [-] UNKNOWN: {}", truncate(key, 100));
        }
        process::exit(1);
    }
}

fn find_locales_dir() -> PathBuf {
    // Try common locations relative to CWD or the tool directory.
    let candidates = [
        PathBuf::from("src/ui/locales"),
        PathBuf::from("../../src/ui/locales"),
    ];
    for candidate in &candidates {
        if candidate.is_dir() {
            return candidate.clone();
        }
    }
    // Fall back: check CARGO_MANIFEST_DIR (set during `cargo run`).
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        let from_manifest = Path::new(&manifest_dir).join("../../src/ui/locales");
        if from_manifest.is_dir() {
            return from_manifest;
        }
    }
    eprintln!("Could not find src/ui/locales directory. Run from the repository root:");
    eprintln!("  cargo run --manifest-path tools/i18n-checker/Cargo.toml");
    process::exit(1);
}

#[derive(Debug)]
struct LocaleLoad {
    map: serde_json::Map<String, Value>,
    duplicate_keys: Vec<String>,
}

fn load_locale_file(path: &Path) -> LocaleLoad {
    let raw = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {e}", path.display());
        process::exit(1);
    });
    let raw = raw.trim_start_matches('\u{feff}'); // strip BOM
    let value: Value = serde_json::from_str(raw).unwrap_or_else(|e| {
        eprintln!("Invalid JSON in {}: {e}", path.display());
        process::exit(1);
    });
    let map = match value {
        Value::Object(map) => map,
        _ => {
            eprintln!("{} is not a JSON object", path.display());
            process::exit(1);
        }
    };

    LocaleLoad {
        map,
        duplicate_keys: find_duplicate_top_level_keys(raw),
    }
}

fn find_duplicate_top_level_keys(raw: &str) -> Vec<String> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut string_start = None;
    let mut keys = BTreeMap::<String, usize>::new();
    let mut duplicates = BTreeSet::<String>::new();

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
                        let count = keys.entry(key.clone()).or_insert(0);
                        *count += 1;
                        if *count == 2 {
                            duplicates.insert(key);
                        }
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

    duplicates.into_iter().collect()
}

fn is_key_string(raw: &str, offset: usize) -> bool {
    raw[offset..]
        .chars()
        .find(|ch| !ch.is_whitespace())
        .is_some_and(|ch| ch == ':')
}

/// Returns true if an "extra" key is actually a valid plural form whose base key exists in en.json.
fn is_valid_plural_extra(key: &str, en_keys: &BTreeSet<&str>) -> bool {
    for suffix in VALID_PLURAL_SUFFIXES {
        if let Some(base) = key.strip_suffix(suffix)
            && en_keys.contains(base)
        {
            return true;
        }
    }
    false
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
