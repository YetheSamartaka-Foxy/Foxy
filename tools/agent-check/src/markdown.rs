use crate::{Diagnostic, path_to_string, read_text};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn validate_links(repo: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let mut files = vec![repo.join("conventions/AGENT_WORKFLOWS.md")];
    for directory in [
        "skills",
        ".agents/skills",
        ".claude/skills",
        ".codex/agents",
        ".claude/agents",
    ] {
        collect_markdown_files(&repo.join(directory), &mut files);
    }
    files.sort();
    files.dedup();
    for file in files {
        if !file.is_file() {
            continue;
        }
        let relative = file
            .strip_prefix(repo)
            .ok()
            .map(path_to_string)
            .unwrap_or_default();
        let Some(text) = read_text(repo, &relative, diagnostics) else {
            continue;
        };
        for target in targets(&text) {
            if is_external(&target) {
                continue;
            }
            let target_path = target.split('#').next().unwrap_or(&target);
            if target_path.is_empty() {
                continue;
            }
            let candidate = resolve(repo, &file, target_path);
            if !candidate.exists() {
                diagnostics.push(Diagnostic::new(
                    &relative,
                    format!("broken local Markdown link `{target_path}`"),
                ));
            }
        }
    }
}

pub(super) fn validate_dashes(repo: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let mut files = Vec::new();
    for directory in [
        "skills",
        ".agents/skills",
        ".claude/skills",
        ".codex/agents",
        ".claude/agents",
    ] {
        collect_controlled_files(&repo.join(directory), &mut files);
    }
    files.push(repo.join("conventions/AGENT_WORKFLOWS.md"));
    files.sort();
    files.dedup();
    for file in files {
        if !file.is_file() {
            continue;
        }
        let relative = file
            .strip_prefix(repo)
            .ok()
            .map(path_to_string)
            .unwrap_or_default();
        if let Some(text) = read_text(repo, &relative, diagnostics)
            && (text.contains('\u{2014}') || text.contains('\u{2013}'))
        {
            diagnostics.push(Diagnostic::new(
                relative,
                "controlled agent text contains an en dash or em dash",
            ));
        }
    }
}

pub(super) fn targets(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b']' && bytes.get(index + 1) == Some(&b'(') {
            let start = index + 2;
            if let Some(end) = bytes[start..].iter().position(|byte| *byte == b')') {
                let raw = text[start..start + end].trim();
                let target = raw
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(['<', '>']);
                if !target.is_empty() {
                    result.push(target.to_owned());
                }
                index = start + end + 1;
                continue;
            }
        }
        index += 1;
    }
    result
}

fn is_external(target: &str) -> bool {
    target.starts_with("http:")
        || target.starts_with("https:")
        || target.starts_with("mailto:")
        || target.starts_with('#')
}

fn resolve(repo: &Path, file: &Path, target: &str) -> PathBuf {
    if target.starts_with('/') {
        repo.join(target.trim_start_matches('/'))
    } else {
        file.parent().unwrap_or(repo).join(target)
    }
}

fn collect_markdown_files(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, output);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            output.push(path);
        }
    }
}

fn collect_controlled_files(root: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_controlled_files(&path, output);
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("md" | "yaml" | "yml" | "toml")
        ) {
            output.push(path);
        }
    }
}
