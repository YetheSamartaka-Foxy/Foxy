//! locale-apply - applies a targeted translation batch to Foxy locale JSON.
//!
//! Run from the repository root:
//!   cargo run --manifest-path tools/i18n-checker/Cargo.toml --bin locale-apply -- \
//!       --translations translations.json --keys-out changed-keys.txt
//!
//! Input shape:
//! {
//!   "de": { "English key from en.json": "German value" },
//!   "pt-BR": { "English key from en.json": "Brazilian Portuguese value" }
//! }
//!
//! Existing entries are rewritten in place and new ones are inserted next to
//! their en.json neighbour, so the diff stays value-only.

use i18n_checker::{
    detect_line_ending, find_entry_line, locale_entry_line, parse_locale_object, placeholder_names,
    previous_present_key, read_locale_text, serialize_locale_value, split_lines_keep_ends,
    top_level_keys, truncate, write_key_file,
};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs, process};

fn main() {
    let config = parse_args();
    let repo = or_die(
        fs::canonicalize(&config.repo)
            .map_err(|e| format!("Failed to resolve repo path {}: {e}", config.repo.display())),
    );
    let locale_dir = repo.join("src/ui/locales");
    let en_path = locale_dir.join("en.json");
    if !en_path.is_file() {
        eprintln!("Missing en.json under {}", locale_dir.display());
        process::exit(1);
    }

    let en_raw = or_die(read_locale_text(&en_path));
    let en_map = or_die(parse_locale_object(&en_raw, "en.json"));
    let en_keys = top_level_keys(&en_raw);

    let batch = or_die(read_batch(&config.translations));

    let mut errors: Vec<String> = Vec::new();
    let mut touched_files = 0usize;
    let mut touched_values = 0usize;
    let mut changed_keys: Vec<String> = Vec::new();

    for (locale, translations) in &batch {
        if locale == "en" {
            errors.push("Do not include en in the translation batch; en.json is the source".into());
            continue;
        }
        let locale_path = locale_dir.join(format!("{locale}.json"));
        if !locale_path.is_file() {
            errors.push(format!("Unknown locale: {locale}"));
            continue;
        }

        let raw = or_die(read_locale_text(&locale_path));
        let locale_map = or_die(parse_locale_object(
            &raw,
            &locale_path.display().to_string(),
        ));
        let mut lines = split_lines_keep_ends(&raw);
        let newline = detect_line_ending(&lines);

        // Insertion anchors are always earlier en.json keys, so applying the
        // batch in en.json order guarantees an anchor is already in the file.
        let mut ordered: Vec<(&String, &Value)> = translations.iter().collect();
        ordered.sort_by_key(|(key, _)| en_keys.iter().position(|en| en == *key));

        let mut available: BTreeSet<&str> = locale_map.keys().map(String::as_str).collect();
        available.extend(translations.keys().map(String::as_str));

        let mut changed_here = 0usize;
        for (key, translated) in ordered {
            let Some(english) = en_map.get(key) else {
                errors.push(format!("{locale}: key is not present in en.json: {key:?}"));
                continue;
            };
            if !matches!(translated, Value::String(_) | Value::Object(_)) {
                errors.push(format!(
                    "{locale}: translated value must be a string or plural object for {key:?}"
                ));
                continue;
            }
            if !config.allow_question_mark && serialize_locale_value(translated).contains('?') {
                errors.push(format!(
                    "{locale}: translated value contains literal '?' for {key:?}"
                ));
            }

            let expected = placeholder_names(english);
            let actual = placeholder_names(translated);
            if expected != actual {
                errors.push(format!(
                    "{locale}: placeholder mismatch for {key:?}: expected {:?}, got {:?}",
                    expected.iter().collect::<Vec<_>>(),
                    actual.iter().collect::<Vec<_>>()
                ));
                continue;
            }

            if let Some(index) = find_entry_line(&lines, key) {
                let comma = lines[index].trim_end_matches(['\r', '\n']).ends_with(',');
                let new_line = locale_entry_line(key, translated, newline, comma);
                if lines[index] != new_line {
                    lines[index] = new_line;
                    changed_here += 1;
                    record_changed(&mut changed_keys, key);
                }
                continue;
            }

            let anchor = match &config.after_key {
                Some(explicit) => Some(explicit.as_str()),
                None => previous_present_key(&en_keys, key, &available),
            };
            let Some(anchor) = anchor else {
                errors.push(format!(
                    "{locale}: could not infer insertion point for {key:?}; pass --after-key"
                ));
                continue;
            };
            let Some(anchor_index) = find_entry_line(&lines, anchor) else {
                errors.push(format!(
                    "{locale}: insertion key not found for {key:?}: {anchor:?}"
                ));
                continue;
            };
            lines.insert(
                anchor_index + 1,
                locale_entry_line(key, translated, newline, true),
            );
            changed_here += 1;
            record_changed(&mut changed_keys, key);
        }

        if changed_here > 0 {
            touched_files += 1;
            touched_values += changed_here;
            if !config.dry_run {
                or_die(
                    fs::write(&locale_path, lines.concat())
                        .map_err(|e| format!("Failed to write {}: {e}", locale_path.display())),
                );
            }
        }
    }

    if !errors.is_empty() {
        println!("Translation batch failed:");
        for error in &errors {
            println!("- {}", truncate(error, 300));
        }
        process::exit(1);
    }

    if let Some(keys_out) = &config.keys_out
        && !config.dry_run
    {
        or_die(write_key_file(keys_out, &changed_keys));
    }

    let action = if config.dry_run {
        "Would update"
    } else {
        "Updated"
    };
    println!("{action} {touched_values} value(s) across {touched_files} locale file(s).");
    if let Some(keys_out) = &config.keys_out {
        println!(
            "Changed-key file: {} ({} key(s))",
            keys_out.display(),
            changed_keys.len()
        );
    }
}

fn record_changed(changed: &mut Vec<String>, key: &str) {
    if !changed.iter().any(|existing| existing == key) {
        changed.push(key.to_string());
    }
}

type Batch = Vec<(String, serde_json::Map<String, Value>)>;

fn read_batch(path: &Path) -> Result<Batch, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read translation batch {}: {e}", path.display()))?;
    let raw = raw.trim_start_matches('\u{feff}');
    let map = parse_locale_object(raw, "translation batch")
        .map_err(|_| "Translation batch must be a JSON object keyed by locale code".to_string())?;

    map.into_iter()
        .map(|(locale, value)| match value {
            Value::Object(translations) => Ok((locale, translations)),
            _ => Err("Translation batch entries must be locale objects".to_string()),
        })
        .collect()
}

#[derive(Debug)]
struct Config {
    repo: PathBuf,
    translations: PathBuf,
    after_key: Option<String>,
    keys_out: Option<PathBuf>,
    dry_run: bool,
    allow_question_mark: bool,
}

fn parse_args() -> Config {
    let mut repo = PathBuf::from(".");
    let mut translations = None;
    let mut after_key = None;
    let mut keys_out = None;
    let mut dry_run = false;
    let mut allow_question_mark = false;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo" => repo = PathBuf::from(require_value(&mut args, "--repo")),
            "--translations" => {
                translations = Some(PathBuf::from(require_value(&mut args, "--translations")));
            }
            "--after-key" => after_key = Some(require_value(&mut args, "--after-key")),
            "--keys-out" => keys_out = Some(PathBuf::from(require_value(&mut args, "--keys-out"))),
            "--dry-run" => dry_run = true,
            "--allow-question-mark" => allow_question_mark = true,
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

    let Some(translations) = translations else {
        eprintln!("--translations is required");
        print_help();
        process::exit(1);
    };

    Config {
        repo,
        translations,
        after_key,
        keys_out,
        dry_run,
        allow_question_mark,
    }
}

fn require_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("{flag} requires a value");
        process::exit(1);
    })
}

fn print_help() {
    println!(
        "Usage: cargo run --manifest-path tools/i18n-checker/Cargo.toml --bin locale-apply -- [OPTIONS]"
    );
    println!();
    println!("Options:");
    println!(
        "  --translations <path>   UTF-8 JSON batch: {{ \"locale\": {{ \"en key\": \"value\" }} }} (required)"
    );
    println!("  --repo <path>           Foxy repository root (default: .)");
    println!(
        "  --after-key <key>       Insert missing keys after this key instead of the inferred one."
    );
    println!("  --keys-out <path>       Write the unique changed en.json keys to this UTF-8 file.");
    println!("  --dry-run               Validate and report without writing locale files.");
    println!(
        "  --allow-question-mark   Allow literal '?' in translated values after manual review."
    );
}

fn or_die<T>(result: Result<T, String>) -> T {
    result.unwrap_or_else(|error| {
        eprintln!("{error}");
        process::exit(1);
    })
}
