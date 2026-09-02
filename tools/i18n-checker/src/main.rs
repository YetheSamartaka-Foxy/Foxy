//! i18n-checker - verifies locale JSON files against en.json (the source of truth).
//!
//! Run from the repository root:
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --require-translated-key-file changed-keys.txt
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --audit-changed-since HEAD
//!
//! Or from inside the tool directory:
//!   cd tools/i18n-checker && cargo run
//!
//! Checks performed:
//!   - Missing keys (present in en.json but absent in locale; warning by default, error in --strict)
//!   - Extra keys (present in locale but absent in en.json), excluding valid plural suffixes
//!   - Duplicate keys in locale JSON files
//!   - Placeholder mismatches against en.json
//!   - JSON parse errors
//!   - Optional exact-English fallback checks for explicitly listed keys
//!   - Optional audit of locale values changed against a git baseline
//!   - Summary with pass/fail per locale

use i18n_checker::{
    LocaleLoad, find_locales_dir, is_valid_plural_extra, load_locale_file, parse_locale_object,
    placeholder_names, read_key_file, translation_files, truncate,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, process};

fn main() {
    let config = parse_args();
    let locales_dir = or_die(find_locales_dir());

    let en_path = locales_dir.join("en.json");
    let en_load = or_die(load_locale_file(&en_path));
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

    let locale_files = or_die(translation_files(&locales_dir));

    let mut total_issues = 0usize;
    let mut total_fallbacks = 0usize;
    let mut total_placeholder_warnings = 0usize;
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
        let locale_load = or_die(load_locale_file(path));
        let LocaleLoad {
            map: locale_map,
            duplicate_keys,
            ..
        } = locale_load;
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

        // Placeholder parity is scanned for every shared key, but only blocks
        // for the keys the caller is actually working on. Locale files carry
        // pre-existing mismatches that must not fail unrelated runs.
        let mut blocking_placeholders: Vec<String> = Vec::new();
        let mut warned_placeholders: Vec<String> = Vec::new();
        for (key, value) in &locale_map {
            let Some(english) = en_map.get(key.as_str()) else {
                continue;
            };
            let Some(report) = placeholder_report(key, english, value) else {
                continue;
            };
            if config.is_placeholder_blocking(key) {
                blocking_placeholders.push(report);
            } else {
                warned_placeholders.push(report);
            }
        }
        total_placeholder_warnings += warned_placeholders.len();

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
            + duplicate_keys.len()
            + blocking_placeholders.len()
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
            for report in &warned_placeholders {
                println!("    [?] PLACEHOLDER: {report}");
            }
        } else {
            files_with_issues += 1;
            total_issues += issue_count;
            println!(
                "  {filename:<20} ISSUES ({} keys, {} missing, {} unexpected extra, {} empty, {} duplicate, {} placeholder, {} untranslated required)",
                locale_keys.len(),
                missing.len(),
                extra.len(),
                empty_values.len(),
                duplicate_keys.len(),
                blocking_placeholders.len(),
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
            for key in &duplicate_keys {
                println!("    [!] DUPLICATE: {}", truncate(key, 100));
            }
            for report in &blocking_placeholders {
                println!("    [!] PLACEHOLDER: {report}");
            }
            for report in &warned_placeholders {
                println!("    [?] PLACEHOLDER: {report}");
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
    }
    if total_placeholder_warnings > 0 {
        println!(
            "{total_placeholder_warnings} non-blocking placeholder mismatch(es) outside the requested key set. Pass --strict-placeholders to fail on them."
        );
    }

    let mut audit_issues = 0usize;
    if let Some(baseline) = &config.audit_changed_since {
        audit_issues = audit_changed_pairs(
            &locales_dir,
            &locale_files,
            &en_map,
            baseline,
            &config.audit_allowed_english_keys,
        );
    }

    if total_issues > 0 || audit_issues > 0 {
        process::exit(1);
    }
}

fn placeholder_report(key: &str, english: &Value, translated: &Value) -> Option<String> {
    let expected = placeholder_names(english);
    let actual = placeholder_names(translated);
    if expected == actual {
        return None;
    }
    Some(format!(
        "{}: expected {:?}, got {:?}",
        truncate(key, 100),
        expected.iter().collect::<Vec<_>>(),
        actual.iter().collect::<Vec<_>>()
    ))
}

/// Validates only the locale/key pairs whose value differs from a git baseline.
/// Unlike `--require-translated-key-file` this does not expect every locale for
/// a key to change, which is the shape of exact-English fallback cleanup.
fn audit_changed_pairs(
    locales_dir: &Path,
    locale_files: &[PathBuf],
    en_map: &serde_json::Map<String, Value>,
    baseline: &str,
    allowed_english_keys: &BTreeSet<String>,
) -> usize {
    let repo_root = locales_dir.join("../../..");
    let mut errors: Vec<String> = Vec::new();
    let mut changed_files = 0usize;
    let mut changed_pairs = 0usize;
    let mut changed_keys = BTreeSet::<String>::new();

    for path in locale_files {
        let filename = path.file_name().unwrap().to_string_lossy();
        let rel_path = format!("src/ui/locales/{filename}");
        let Some(baseline_map) = git_show_object(&repo_root, baseline, &rel_path) else {
            continue;
        };
        let locale_map = or_die(load_locale_file(path)).map;

        let mut local_changes = 0usize;
        for (key, value) in &locale_map {
            match baseline_map.get(key) {
                Some(old) if old == value => continue,
                None => continue,
                Some(_) => {}
            }

            local_changes += 1;
            changed_pairs += 1;
            changed_keys.insert(key.clone());

            let english = en_map.get(key).cloned().unwrap_or(Value::Null);
            if let Some(report) = placeholder_report(key, &english, value) {
                errors.push(format!("{filename}: placeholder mismatch for {report}"));
            }
            if !allowed_english_keys.contains(key) && en_map.get(key) == Some(value) {
                errors.push(format!(
                    "{filename}: changed value still equals en.json for {}",
                    truncate(key, 100)
                ));
            }
        }

        if local_changes > 0 {
            changed_files += 1;
        }
    }

    println!("{}", "=".repeat(72));
    println!(
        "Audited {changed_pairs} changed locale value(s) across {changed_files} file(s), {} unique key(s), baseline {baseline}.",
        changed_keys.len()
    );
    if errors.is_empty() {
        println!("Changed locale pair audit passed.");
        return 0;
    }

    println!("Changed locale pair audit failed:");
    for error in &errors {
        println!("  - {error}");
    }
    errors.len()
}

fn git_show_object(
    repo_root: &Path,
    baseline: &str,
    rel_path: &str,
) -> Option<serde_json::Map<String, Value>> {
    let output = Command::new("git")
        .arg("show")
        .arg(format!("{baseline}:{rel_path}"))
        .current_dir(repo_root)
        .output()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run git: {e}");
            process::exit(1);
        });
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let raw = raw.trim_start_matches('\u{feff}');
    parse_locale_object(raw, &format!("{baseline}:{rel_path}")).ok()
}

#[derive(Debug, Default)]
struct Config {
    strict_missing: bool,
    required_translated_keys: BTreeSet<String>,
    audit_changed_since: Option<String>,
    audit_allowed_english_keys: BTreeSet<String>,
    strict_placeholders: bool,
}

impl Config {
    fn is_placeholder_blocking(&self, key: &str) -> bool {
        self.strict_placeholders || self.required_translated_keys.contains(key)
    }
}

fn parse_args() -> Config {
    let mut config = Config::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--strict" => config.strict_missing = true,
            "--strict-placeholders" => config.strict_placeholders = true,
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
                for key in or_die(read_key_file(Path::new(&path))) {
                    config.required_translated_keys.insert(key);
                }
            }
            "--audit-changed-since" => {
                let Some(baseline) = args.next() else {
                    eprintln!("--audit-changed-since requires a git ref argument");
                    process::exit(1);
                };
                config.audit_changed_since = Some(baseline);
            }
            "--audit-allow-english-key-file" => {
                let Some(path) = args.next() else {
                    eprintln!("--audit-allow-english-key-file requires a path argument");
                    process::exit(1);
                };
                for key in or_die(read_key_file(Path::new(&path))) {
                    config.audit_allowed_english_keys.insert(key);
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
    println!("  --strict-placeholders");
    println!("      Treat every placeholder mismatch as an error, not only the requested keys.");
    println!("  --require-translated-key <key>");
    println!("      Fail when a non-English locale keeps the English value for this key.");
    println!("  --require-translated-key-file <path>");
    println!("      Read required translated keys from a UTF-8 text file, one key per line.");
    println!("      Use \\n in the file for newline characters inside a JSON key.");
    println!("  --audit-changed-since <git-ref>");
    println!("      Also audit only the locale values that differ from the given git baseline,");
    println!("      reporting placeholder mismatches and changed values that still equal en.json.");
    println!("  --audit-allow-english-key-file <path>");
    println!("      Keys allowed to stay exactly English during --audit-changed-since.");
    println!();
    println!("Placeholder parity is scanned for every shared key. It only fails the run for keys");
    println!("named by --require-translated-key/-file, unless --strict-placeholders is passed.");
    println!("To apply a translation batch, use the locale-apply binary in this package:");
    println!(
        "  cargo run --manifest-path tools/i18n-checker/Cargo.toml --bin locale-apply -- --help"
    );
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

fn or_die<T>(result: Result<T, String>) -> T {
    result.unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn placeholder_report_flags_only_mismatches() {
        assert!(
            placeholder_report("k", &json!("{count} of {name}"), &json!("{name}: {count}"))
                .is_none()
        );
        assert!(placeholder_report("k", &json!("{count}"), &json!("{cantidad}")).is_some());
        assert!(placeholder_report("k", &json!("{game} dir"), &json!("Arma 3 dir")).is_some());
    }
}
