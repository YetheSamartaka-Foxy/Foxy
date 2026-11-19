use i18n_checker::{find_locales_dir, load_locale_file};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::{env, fs, process};

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenKind {
    Ident(String),
    Str(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Dot,
    ColonColon,
    Hash,
    Other(char),
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    line: usize,
}

#[derive(Debug, Default)]
struct ScanReport {
    used_keys: BTreeMap<String, Vec<String>>,
    plural_keys: BTreeSet<String>,
    unwrapped_literals: Vec<String>,
}

fn main() {
    let args = parse_args();
    let repo = args.repo;
    let locales_dir = find_locales_dir().unwrap_or_else(|_| repo.join("src/ui/locales"));
    let en_path = locales_dir.join("en.json");
    let en_load = load_locale_file(&en_path).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    });
    let en_keys: BTreeSet<String> = en_load.map.keys().cloned().collect();

    if let Some(export_dir) = &args.export_missing_dir {
        export_missing_translations(&locales_dir, &en_load.map, export_dir, args.chunk_size);
        return;
    }

    if let (Some(merge_dir), Some(merged_file)) = (&args.merge_out_dir, &args.merged_file) {
        merge_and_validate_batches(
            &en_load.map,
            merge_dir,
            args.en_chunks_dir.as_deref(),
            merged_file,
            &args.locales_filter,
        );
        return;
    }

    let src_dir = repo.join("src");
    let mut rs_files = Vec::new();
    collect_rs_files(&src_dir, &mut rs_files);
    rs_files.sort();

    let mut report = ScanReport::default();
    for path in &rs_files {
        let content = fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("Failed to read {}: {e}", path.display());
            process::exit(1);
        });
        let rel = path
            .strip_prefix(&repo)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        scan_file(&rel, &content, &mut report);
    }

    let missing_from_en: Vec<(&String, &Vec<String>)> = report
        .used_keys
        .iter()
        .filter(|(key, _)| !en_keys.contains(key.as_str()))
        .collect();

    println!(
        "source_audit: scanned {} Rust file(s), {} unique source key(s), {} plural key(s).",
        rs_files.len(),
        report.used_keys.len(),
        report.plural_keys.len()
    );
    println!("Missing from en.json: {}", missing_from_en.len());
    for (key, locs) in &missing_from_en {
        println!(
            "  [MISSING] {}  ({})",
            serde_json::to_string(key).unwrap_or_else(|_| key.to_string()),
            locs.first().map(String::as_str).unwrap_or("")
        );
    }

    println!("Pluralized source keys ({}):", report.plural_keys.len());
    for key in &report.plural_keys {
        println!(
            "  [PLURAL] {}",
            serde_json::to_string(key).unwrap_or_else(|_| key.to_string())
        );
    }

    println!(
        "Unwrapped literal candidates in src/ui/views/ ({}):",
        report.unwrapped_literals.len()
    );
    for hit in &report.unwrapped_literals {
        println!("  [LITERAL] {hit}");
    }

    let double_escaped: Vec<&String> = en_keys.iter().filter(|k| k.contains("\\n")).collect();
    println!(
        "Double-escaped backslash-n keys in en.json ({}):",
        double_escaped.len()
    );
    for key in double_escaped {
        println!(
            "  [ESCAPED_N] {}",
            serde_json::to_string(key).unwrap_or_else(|_| key.to_string())
        );
    }
}

struct Args {
    repo: PathBuf,
    export_missing_dir: Option<PathBuf>,
    chunk_size: usize,
    merge_out_dir: Option<PathBuf>,
    en_chunks_dir: Option<PathBuf>,
    merged_file: Option<PathBuf>,
    locales_filter: BTreeSet<String>,
}

fn parse_args() -> Args {
    let mut args = env::args().skip(1);
    let mut repo = PathBuf::from(".");
    let mut export_missing_dir = None;
    let mut chunk_size = 200usize;
    let mut merge_out_dir = None;
    let mut en_chunks_dir = None;
    let mut merged_file = None;
    let mut locales_filter = BTreeSet::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo" => {
                repo = PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--repo requires a path");
                    process::exit(1);
                }));
            }
            "--export-missing-dir" => {
                export_missing_dir = Some(PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--export-missing-dir requires a path");
                    process::exit(1);
                })));
            }
            "--chunk-size" => {
                let raw = args.next().unwrap_or_else(|| {
                    eprintln!("--chunk-size requires a number");
                    process::exit(1);
                });
                chunk_size = raw
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .unwrap_or_else(|| {
                        eprintln!("Invalid --chunk-size: {raw}");
                        process::exit(1);
                    });
            }
            "--merge-out-dir" => {
                merge_out_dir = Some(PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--merge-out-dir requires a path");
                    process::exit(1);
                })));
            }
            "--en-chunks-dir" => {
                en_chunks_dir = Some(PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--en-chunks-dir requires a path");
                    process::exit(1);
                })));
            }
            "--merged-file" => {
                merged_file = Some(PathBuf::from(args.next().unwrap_or_else(|| {
                    eprintln!("--merged-file requires a path");
                    process::exit(1);
                })));
            }
            "--locales" => {
                let raw = args.next().unwrap_or_else(|| {
                    eprintln!("--locales requires a comma-separated list");
                    process::exit(1);
                });
                for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    locales_filter.insert(part.to_string());
                }
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run --manifest-path tools/i18n-checker/Cargo.toml --bin source_audit -- [--repo <path>] [--export-missing-dir <dir>] [--chunk-size <N>] [--merge-out-dir <dir> --merged-file <path> [--en-chunks-dir <dir>] [--locales <a,b>]]"
                );
                process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                process::exit(1);
            }
        }
    }
    Args {
        repo,
        export_missing_dir,
        chunk_size,
        merge_out_dir,
        en_chunks_dir,
        merged_file,
        locales_filter,
    }
}

fn merge_and_validate_batches(
    en_map: &serde_json::Map<String, serde_json::Value>,
    merge_dir: &Path,
    en_chunks_dir: Option<&Path>,
    merged_file: &Path,
    locales_filter: &BTreeSet<String>,
) {
    let mut expected_keys = BTreeSet::<String>::new();
    if let Some(chunks_dir) = en_chunks_dir
        && let Ok(entries) = fs::read_dir(chunks_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chunk_") && n.ends_with(".json"))
            {
                let load = load_locale_file(&path).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    process::exit(1);
                });
                expected_keys.extend(load.map.keys().cloned());
            }
        }
    }

    let mut locale_dirs: Vec<PathBuf> = fs::read_dir(merge_dir)
        .unwrap_or_else(|e| {
            eprintln!("Failed to read {}: {e}", merge_dir.display());
            process::exit(1);
        })
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| {
            p.is_dir()
                && (locales_filter.is_empty()
                    || p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|code| locales_filter.contains(code)))
        })
        .collect();
    locale_dirs.sort();

    let mut merged = serde_json::Map::new();
    let mut errors = Vec::new();
    let mut total_entries = 0usize;
    let locale_count = locale_dirs.len();

    for dir in locale_dirs {
        let code = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let mut chunk_files: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|e| {
                eprintln!("Failed to read {}: {e}", dir.display());
                process::exit(1);
            })
            .filter_map(|e| e.ok().map(|x| x.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        chunk_files.sort();

        let mut locale_out = serde_json::Map::new();
        for chunk_path in chunk_files {
            let load = match load_locale_file(&chunk_path) {
                Ok(load) => load,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let Some(serde_json::Value::Object(entries)) = load.map.get(&code) else {
                errors.push(format!(
                    "{}: missing top-level `{code}` object",
                    chunk_path.display()
                ));
                continue;
            };
            for (k, v) in entries {
                let Some(en_val) = en_map.get(k.as_str()) else {
                    errors.push(format!("{code}: unknown key not in en.json: {k:?}"));
                    continue;
                };
                let Some(text) = v.as_str() else {
                    errors.push(format!("{code}: non-string value for {k:?}"));
                    continue;
                };
                if text.trim().is_empty() {
                    errors.push(format!("{code}: empty translation for {k:?}"));
                    continue;
                }
                if i18n_checker::placeholder_names(en_val) != i18n_checker::placeholder_names(v) {
                    errors.push(format!("{code}: placeholder mismatch for {k:?}"));
                    continue;
                }
                if text.contains('\u{2014}') || text.contains('\u{2013}') {
                    errors.push(format!("{code}: em/en dash found in {k:?}"));
                    continue;
                }
                let en_text = en_val.as_str().unwrap_or("");
                if text.contains('?') && !en_text.contains('?') {
                    errors.push(format!(
                        "{code}: suspicious '?' corruption in non-question {k:?}: {text:?}"
                    ));
                    continue;
                }
                if text == en_text && !is_safe_exact_english_key(k) {
                    let non_placeholder = strip_placeholders(en_text);
                    if non_placeholder.split_whitespace().count() > 2 {
                        errors.push(format!(
                            "{code}: untranslated multi-word English value for {k:?}"
                        ));
                    } else {
                        println!("  [EXACT_EN] {code}: {k:?}");
                    }
                }
                locale_out.insert(k.clone(), v.clone());
            }
        }

        if !expected_keys.is_empty() {
            for req in &expected_keys {
                if !locale_out.contains_key(req.as_str()) {
                    errors.push(format!("{code}: missing key {req:?}"));
                }
            }
        }
        total_entries += locale_out.len();
        merged.insert(code, serde_json::Value::Object(locale_out));
    }

    if !errors.is_empty() {
        eprintln!("Batch validation failed ({} error(s)):", errors.len());
        for err in &errors {
            eprintln!("  - {err}");
        }
        process::exit(1);
    }

    if let Some(parent) = merged_file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let out_json = serde_json::to_string_pretty(&serde_json::Value::Object(merged))
        .expect("serialize merged batch");
    fs::write(merged_file, out_json).unwrap_or_else(|e| {
        eprintln!("Failed to write {}: {e}", merged_file.display());
        process::exit(1);
    });
    println!(
        "Validated and merged {total_entries} translation(s) across {locale_count} locale(s) into {}.",
        merged_file.display()
    );
}

fn strip_placeholders(text: &str) -> String {
    let mut remainder = String::new();
    let mut in_brace = false;
    for ch in text.chars() {
        if ch == '{' {
            in_brace = true;
        } else if ch == '}' {
            in_brace = false;
        } else if !in_brace {
            remainder.push(ch);
        }
    }
    remainder
}

fn is_safe_exact_english_key(key: &str) -> bool {
    if matches!(
        key,
        "Arma 3"
            | "Arma Reforger"
            | "BattlEye"
            | "Bohemia Workshop"
            | "GUID"
            | "JSON"
            | "RCon"
            | "URL"
            | "UTC"
            | "n/a"
            | "BLAKE3: {hash}"
            | "server.json"
            | "SteamCMD"
            | "Template.World"
            | "Steam ID"
            | "Reforger server.json"
            | "ID {id}: {name}, {address}, ping {ping} ms"
            | "{path} ({bytes} bytes, BLAKE3 {hash})"
            | "{path} ({bytes} bytes)"
    ) {
        return true;
    }
    // Keys made purely of `{placeholder}` tokens and punctuation (like `{code}: {message}`).
    let remainder = strip_placeholders(key);
    !remainder.chars().any(char::is_alphabetic)
}

fn export_missing_translations(
    locales_dir: &Path,
    en_map: &serde_json::Map<String, serde_json::Value>,
    out_dir: &Path,
    chunk_size: usize,
) {
    let files = i18n_checker::translation_files(locales_dir).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    });
    fs::create_dir_all(out_dir).unwrap_or_else(|e| {
        eprintln!("Failed to create {}: {e}", out_dir.display());
        process::exit(1);
    });

    let old_repo_key =
        "Force redownload {name}?\\nThis will remove local files and re-download the repository.";
    let new_repo_key =
        "Force redownload {name}?\nThis will remove local files and re-download the repository.";

    let en_keys: BTreeSet<&str> = en_map.keys().map(String::as_str).collect();
    let mut all_missing: BTreeSet<String> = BTreeSet::new();
    let mut migrate_map = serde_json::Map::new();
    let mut per_locale_missing: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let code = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let load = load_locale_file(path).unwrap_or_else(|e| {
            eprintln!("{e}");
            process::exit(1);
        });
        let locale_keys: BTreeSet<&str> = load.map.keys().map(String::as_str).collect();
        let mut missing_here = 0usize;
        for key in en_keys.difference(&locale_keys) {
            if *key == new_repo_key
                && let Some(existing_val) = load.map.get(old_repo_key)
            {
                let mut entry = serde_json::Map::new();
                entry.insert(new_repo_key.to_string(), existing_val.clone());
                migrate_map.insert(code.clone(), serde_json::Value::Object(entry));
                continue;
            }
            all_missing.insert((*key).to_string());
            missing_here += 1;
        }
        per_locale_missing.insert(code, missing_here);
    }

    let ordered_missing: Vec<String> = en_map
        .keys()
        .filter(|k| all_missing.contains(k.as_str()))
        .cloned()
        .collect();

    for (chunk_idx, chunk) in ordered_missing.chunks(chunk_size).enumerate() {
        let mut obj = serde_json::Map::new();
        for key in chunk {
            if let Some(val) = en_map.get(key) {
                obj.insert(key.clone(), val.clone());
            }
        }
        let chunk_path = out_dir.join(format!("chunk_{:02}.json", chunk_idx + 1));
        let json =
            serde_json::to_string_pretty(&serde_json::Value::Object(obj)).expect("serialize chunk");
        fs::write(&chunk_path, json).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", chunk_path.display());
            process::exit(1);
        });
    }

    if !migrate_map.is_empty() {
        let migrate_path = out_dir.join("migrate_newline.json");
        let json = serde_json::to_string_pretty(&serde_json::Value::Object(migrate_map))
            .expect("serialize migration");
        fs::write(&migrate_path, json).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {e}", migrate_path.display());
            process::exit(1);
        });
    }

    println!(
        "Exported {} unique missing key(s) in {} chunk(s) to {}.",
        ordered_missing.len(),
        ordered_missing.len().div_ceil(chunk_size),
        out_dir.display()
    );
    let unique_counts: BTreeSet<usize> = per_locale_missing.values().copied().collect();
    println!(
        "Per-locale missing counts across {} files: {:?}",
        per_locale_missing.len(),
        unique_counts
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn scan_file(rel: &str, content: &str, report: &mut ScanReport) {
    let tokens = tokenize_rust(content);
    let is_view = rel.starts_with("src/ui/views/");
    let mut i = 0usize;
    let mut skip_depth: Option<usize> = None;
    let mut brace_depth = 0usize;
    let mut pending_cfg_test = false;

    while i < tokens.len() {
        let tok = &tokens[i];
        match &tok.kind {
            TokenKind::Hash => {
                if is_cfg_test_attr(&tokens, i) {
                    pending_cfg_test = true;
                }
            }
            TokenKind::LBrace => {
                brace_depth += 1;
                if pending_cfg_test && skip_depth.is_none() {
                    skip_depth = Some(brace_depth);
                    pending_cfg_test = false;
                }
            }
            TokenKind::RBrace => {
                if skip_depth == Some(brace_depth) {
                    skip_depth = None;
                }
                brace_depth = brace_depth.saturating_sub(1);
            }
            TokenKind::Semi if pending_cfg_test => {
                // `#[cfg(test)] mod tests;` external module declaration.
                pending_cfg_test = false;
            }
            _ => {}
        }

        if skip_depth.is_some() {
            i += 1;
            continue;
        }

        if let TokenKind::Ident(name) = &tok.kind
            && i + 1 < tokens.len()
            && tokens[i + 1].kind == TokenKind::LParen
        {
            let prev_is_fn =
                i > 0 && matches!(&tokens[i - 1].kind, TokenKind::Ident(prev) if prev == "fn");
            if !prev_is_fn {
                if let Some((arg_limit, is_plural, collect_all_args)) =
                    call_translation_spec(&tokens, i, name)
                {
                    let arg_start = i + 2;
                    let call_end = matching_paren(&tokens, i + 1);
                    let search_end = if collect_all_args {
                        call_end
                    } else {
                        first_arg_end(&tokens, arg_start, call_end)
                            .min(arg_limit.unwrap_or(call_end))
                    };
                    for inner in &tokens[arg_start..search_end] {
                        if let TokenKind::Str(key) = &inner.kind {
                            if is_plural {
                                report.plural_keys.insert(key.clone());
                            }
                            report
                                .used_keys
                                .entry(key.clone())
                                .or_default()
                                .push(format!("{rel}:{}", inner.line));
                        }
                    }
                } else if is_view && is_ui_text_call(&tokens, i, name) {
                    let arg_start = i + 2;
                    let call_end = matching_paren(&tokens, i + 1);
                    check_unwrapped_ui_call(rel, name, &tokens[arg_start..call_end], report);
                }
            }
        }

        i += 1;
    }
}

fn is_cfg_test_attr(tokens: &[Token], hash_idx: usize) -> bool {
    if tokens.get(hash_idx + 1).map(|t| &t.kind) != Some(&TokenKind::LBracket) {
        return false;
    }
    matches!(
        (
            tokens.get(hash_idx + 2).map(|t| &t.kind),
            tokens.get(hash_idx + 3).map(|t| &t.kind),
            tokens.get(hash_idx + 4).map(|t| &t.kind),
            tokens.get(hash_idx + 5).map(|t| &t.kind),
        ),
        (
            Some(TokenKind::Ident(cfg)),
            Some(TokenKind::LParen),
            Some(TokenKind::Ident(test)),
            Some(TokenKind::RParen),
        ) if cfg == "cfg" && test == "test"
    )
}

fn call_translation_spec(
    tokens: &[Token],
    ident_idx: usize,
    name: &str,
) -> Option<(Option<usize>, bool, bool)> {
    let prev_dot = ident_idx > 0 && tokens[ident_idx - 1].kind == TokenKind::Dot;
    let prev_colon = ident_idx > 0 && tokens[ident_idx - 1].kind == TokenKind::ColonColon;
    match name {
        "tr" | "tr_fmt" if !prev_dot => Some((None, false, false)),
        "t" | "t_fmt" | "tr" | "tr_fmt" if prev_dot => Some((None, false, false)),
        "tr_plural" | "tr_plural_fmt" => Some((None, true, false)),
        "new"
            if prev_colon
                && ident_idx >= 2
                && matches!(&tokens[ident_idx - 2].kind, TokenKind::Ident(ty) if ty == "HelpPage") =>
        {
            Some((None, false, true))
        }
        _ => None,
    }
}

fn is_ui_text_call(tokens: &[Token], ident_idx: usize, name: &str) -> bool {
    let prev_dot = ident_idx > 0 && tokens[ident_idx - 1].kind == TokenKind::Dot;
    let prev_colon = ident_idx > 0 && tokens[ident_idx - 1].kind == TokenKind::ColonColon;
    if prev_dot {
        matches!(
            name,
            "label"
                | "small"
                | "heading"
                | "button"
                | "checkbox"
                | "selectable_value"
                | "selectable_label"
                | "radio"
                | "radio_value"
                | "on_hover_text"
                | "hint_text"
                | "accessible_name"
                | "colored_label"
        )
    } else if prev_colon && ident_idx >= 2 {
        matches!(
            (&tokens[ident_idx - 2].kind, name),
            (TokenKind::Ident(ty), "new")
                if matches!(ty.as_str(), "Window" | "Button" | "Label" | "RichText")
        )
    } else {
        false
    }
}

fn check_unwrapped_ui_call(rel: &str, method: &str, args: &[Token], report: &mut ScanReport) {
    let mut depth = 0usize;
    let mut arg_index = 0usize;
    for (idx, tok) in args.iter().enumerate() {
        match tok.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Comma if depth == 0 => arg_index += 1,
            TokenKind::Str(ref s) if depth == 0 => {
                let target_arg = match method {
                    "checkbox" | "selectable_label" | "radio" | "colored_label" => arg_index == 1,
                    "selectable_value" | "radio_value" => arg_index == 2,
                    _ => arg_index == 0,
                };
                if target_arg && !is_ignorable_ui_literal(s) && !inside_wrapped_call(args, idx) {
                    report.unwrapped_literals.push(format!(
                        "{rel}:{}: {method}({})",
                        tok.line,
                        serde_json::to_string(s).unwrap_or_else(|_| s.clone())
                    ));
                }
            }
            _ => {}
        }
    }
}

fn inside_wrapped_call(args: &[Token], str_idx: usize) -> bool {
    str_idx >= 2
        && args[str_idx - 1].kind == TokenKind::LParen
        && matches!(
            &args[str_idx - 2].kind,
            TokenKind::Ident(name)
                if matches!(
                    name.as_str(),
                    "tr" | "tr_fmt" | "tr_plural" | "tr_plural_fmt" | "t" | "t_fmt" | "format"
                )
        )
}

fn is_ignorable_ui_literal(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "X" {
        return true;
    }
    if !trimmed.chars().any(char::is_alphabetic) {
        return true;
    }
    // Single-letter hotkey indicators or file extensions / URL placeholders.
    trimmed.starts_with("http://") || trimmed.starts_with("https://")
}

fn matching_paren(tokens: &[Token], open_idx: usize) -> usize {
    let mut depth = 0usize;
    for (offset, tok) in tokens[open_idx..].iter().enumerate() {
        match tok.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return open_idx + offset;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

fn first_arg_end(tokens: &[Token], start: usize, end: usize) -> usize {
    let mut depth = 0usize;
    for (offset, tok) in tokens[start..end].iter().enumerate() {
        match tok.kind {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Comma if depth == 0 => return start + offset,
            _ => {}
        }
    }
    end
}

fn tokenize_rust(input: &str) -> Vec<Token> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if b == b'r'
            && let Some((parsed, next_i, newlines)) = try_parse_raw_string(input, i)
        {
            tokens.push(Token {
                kind: TokenKind::Str(parsed),
                line,
            });
            line += newlines;
            i = next_i;
            continue;
        }
        if b == b'"' {
            let (parsed, next_i, newlines) = parse_quoted_string(input, i + 1);
            tokens.push(Token {
                kind: TokenKind::Str(parsed),
                line,
            });
            line += newlines;
            i = next_i;
            continue;
        }
        if b == b'\'' {
            i = skip_char_or_lifetime(input, i);
            continue;
        }
        if is_ident_start(b) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Ident(input[start..i].to_string()),
                line,
            });
            continue;
        }
        if b == b':' && bytes.get(i + 1) == Some(&b':') {
            tokens.push(Token {
                kind: TokenKind::ColonColon,
                line,
            });
            i += 2;
            continue;
        }

        let kind = match b {
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b',' => TokenKind::Comma,
            b';' => TokenKind::Semi,
            b'.' => TokenKind::Dot,
            b'#' => TokenKind::Hash,
            other => TokenKind::Other(other as char),
        };
        tokens.push(Token { kind, line });
        i += 1;
    }

    tokens
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn try_parse_raw_string(input: &str, r_idx: usize) -> Option<(String, usize, usize)> {
    let bytes = input.as_bytes();
    let mut hashes = 0usize;
    let mut idx = r_idx + 1;
    while idx < bytes.len() && bytes[idx] == b'#' {
        hashes += 1;
        idx += 1;
    }
    if bytes.get(idx) != Some(&b'"') {
        return None;
    }
    idx += 1;
    let content_start = idx;
    let mut newlines = 0usize;
    while idx < bytes.len() {
        if bytes[idx] == b'\n' {
            newlines += 1;
            idx += 1;
            continue;
        }
        if bytes[idx] == b'"' {
            let mut matched = 0usize;
            while matched < hashes && bytes.get(idx + 1 + matched) == Some(&b'#') {
                matched += 1;
            }
            if matched == hashes {
                let raw = input[content_start..idx].to_string();
                return Some((raw, idx + 1 + hashes, newlines));
            }
        }
        idx += 1;
    }
    None
}

fn parse_quoted_string(input: &str, start: usize) -> (String, usize, usize) {
    let mut out = String::new();
    let mut chars = input[start..].char_indices().peekable();
    let mut newlines = 0usize;
    while let Some((rel_idx, ch)) = chars.next() {
        match ch {
            '"' => return (out, start + rel_idx + 1, newlines),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, 't')) => out.push('\t'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, '"')) => out.push('"'),
                Some((_, '\'')) => out.push('\''),
                Some((_, '0')) => out.push('\0'),
                Some((_, 'u')) if chars.peek().is_some_and(|&(_, next)| next == '{') => {
                    chars.next();
                    let mut hex = String::new();
                    for (_, h) in chars.by_ref() {
                        if h == '}' {
                            break;
                        }
                        hex.push(h);
                    }
                    if let Ok(code) = u32::from_str_radix(&hex, 16)
                        && let Some(decoded) = char::from_u32(code)
                    {
                        out.push(decoded);
                    }
                }
                Some((_, 'x')) => {
                    let h1 = chars.next().map(|(_, c)| c);
                    let h2 = chars.next().map(|(_, c)| c);
                    if let (Some(a), Some(b)) = (h1, h2) {
                        let hex: String = [a, b].into_iter().collect();
                        if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                            out.push(byte as char);
                        }
                    }
                }
                Some((_, '\n')) => {
                    newlines += 1;
                    while chars.peek().is_some_and(|&(_, next)| {
                        next == ' ' || next == '\t' || next == '\r' || next == '\n'
                    }) {
                        if chars.next().is_some_and(|(_, c)| c == '\n') {
                            newlines += 1;
                        }
                    }
                }
                Some((_, '\r')) if chars.peek().is_some_and(|&(_, next)| next == '\n') => {
                    chars.next();
                    newlines += 1;
                    while chars.peek().is_some_and(|&(_, next)| {
                        next == ' ' || next == '\t' || next == '\r' || next == '\n'
                    }) {
                        if chars.next().is_some_and(|(_, c)| c == '\n') {
                            newlines += 1;
                        }
                    }
                }
                Some((_, other)) => out.push(other),
                None => break,
            },
            '\n' => {
                newlines += 1;
                out.push('\n');
            }
            other => out.push(other),
        }
    }
    (out, input.len(), newlines)
}

fn skip_char_or_lifetime(input: &str, start: usize) -> usize {
    let rest = &input[start + 1..];
    if let Some(after) = rest.strip_prefix('\\') {
        let mut iter = after.char_indices();
        let _ = iter.next();
        if let Some((_, '\'')) = iter.next() {
            return start + 1 + 1 + 2;
        }
        for (rel, ch) in iter {
            if ch == '\'' {
                return start + 2 + rel + 1;
            }
        }
        return start + 1;
    }
    let mut chars = rest.char_indices();
    if let (Some((_, _)), Some((rel, '\''))) = (chars.next(), chars.next()) {
        return start + 1 + rel + 1;
    }
    start + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_match_arms_fmt_plural_and_help_page_keys_ignoring_tests() {
        let source = r#"
            fn example(app: &Foxy, ui: &mut Ui) {
                let a = app.t(if true { "First arm" } else { "Second arm" });
                let b = tr_fmt("Count {n}", &[("n", "1".into())]);
                let c = app.i18n.tr_plural("Found {count} items", 3);
                let _ = app.t("With \"quotes\" and\nnewline");
                ui.button("Unwrapped label");
                let _ = HelpPage::new("Topic", &["Intro line"], &["Step line"]);
            }

            #[cfg(test)]
            mod tests {
                #[test]
                fn test_only() {
                    let _ = tr("Ignored test key");
                }
            }
        "#;
        let mut report = ScanReport::default();
        scan_file("src/ui/views/demo.rs", source, &mut report);

        assert!(report.used_keys.contains_key("First arm"));
        assert!(report.used_keys.contains_key("Second arm"));
        assert!(report.used_keys.contains_key("Count {n}"));
        assert!(report.used_keys.contains_key("Found {count} items"));
        assert!(
            report
                .used_keys
                .contains_key("With \"quotes\" and\nnewline")
        );
        assert!(report.used_keys.contains_key("Topic"));
        assert!(report.used_keys.contains_key("Intro line"));
        assert!(report.used_keys.contains_key("Step line"));
        assert!(!report.used_keys.contains_key("Ignored test key"));
        assert!(report.plural_keys.contains("Found {count} items"));
        assert_eq!(report.unwrapped_literals.len(), 1);
    }
}
