use crate::{Diagnostic, read_text, skills::parse_frontmatter};
use serde_yaml_ng::Value as YamlValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn validate(repo: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let codex_root = repo.join(".codex/agents");
    let claude_root = repo.join(".claude/agents");
    if !codex_root.is_dir() {
        diagnostics.push(Diagnostic::new(
            ".codex/agents",
            "role directory is missing",
        ));
    }
    if !claude_root.is_dir() {
        diagnostics.push(Diagnostic::new(
            ".claude/agents",
            "role directory is missing",
        ));
    }
    let mut codex = BTreeMap::new();
    for path in files(&codex_root, "toml") {
        let relative = format!(
            ".codex/agents/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let Some(text) = read_text(repo, &relative, diagnostics) else {
            continue;
        };
        let role: toml::Value = match toml::from_str(&text) {
            Ok(role) => role,
            Err(error) => {
                diagnostics.push(Diagnostic::new(&relative, format!("invalid TOML: {error}")));
                continue;
            }
        };
        let name = role
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .trim();
        let description = role
            .get("description")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .trim();
        let instructions = role
            .get("developer_instructions")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        if name.is_empty() || description.is_empty() || instructions.trim().is_empty() {
            diagnostics.push(Diagnostic::new(
                &relative,
                "name, description, and developer_instructions must be nonempty",
            ));
        }
        if name
            != path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref()
        {
            diagnostics.push(Diagnostic::new(
                &relative,
                "role name must match filename stem",
            ));
        }
        if codex
            .insert(name.to_owned(), description.to_owned())
            .is_some()
        {
            diagnostics.push(Diagnostic::new(&relative, "duplicate Codex role name"));
        }
        validate_workflow_ref(repo, &relative, instructions, diagnostics);
    }

    let mut claude = BTreeMap::new();
    for path in files(&claude_root, "md") {
        let relative = format!(
            ".claude/agents/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let Some(text) = read_text(repo, &relative, diagnostics) else {
            continue;
        };
        let (frontmatter, body) = match parse_frontmatter(&text) {
            Ok(parsed) => parsed,
            Err(error) => {
                diagnostics.push(Diagnostic::new(&relative, error));
                continue;
            }
        };
        let name = yaml_string(&frontmatter, "name").unwrap_or_default().trim();
        let description = yaml_string(&frontmatter, "description")
            .unwrap_or_default()
            .trim();
        if name.is_empty() || description.is_empty() {
            diagnostics.push(Diagnostic::new(
                &relative,
                "role name and description must be nonempty",
            ));
        }
        if name
            != path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref()
        {
            diagnostics.push(Diagnostic::new(
                &relative,
                "role name must match filename stem",
            ));
        }
        if claude
            .insert(name.to_owned(), description.to_owned())
            .is_some()
        {
            diagnostics.push(Diagnostic::new(&relative, "duplicate Claude role name"));
        }
        validate_workflow_ref(repo, &relative, body, diagnostics);
    }

    if codex.is_empty() {
        diagnostics.push(Diagnostic::new(
            ".codex/agents",
            "role directory must not be empty",
        ));
    }
    if claude.is_empty() {
        diagnostics.push(Diagnostic::new(
            ".claude/agents",
            "role directory must not be empty",
        ));
    }

    for name in codex.keys().filter(|name| !claude.contains_key(*name)) {
        diagnostics.push(Diagnostic::new(
            ".claude/agents",
            format!("missing Claude role `{name}`"),
        ));
    }
    for name in claude.keys().filter(|name| !codex.contains_key(*name)) {
        diagnostics.push(Diagnostic::new(
            ".codex/agents",
            format!("missing Codex role `{name}`"),
        ));
    }
    for (name, description) in &codex {
        if claude.get(name).is_some_and(|value| value != description) {
            diagnostics.push(Diagnostic::new(
                ".claude/agents",
                format!("description for role `{name}` differs from Codex metadata"),
            ));
        }
    }
}

fn validate_workflow_ref(repo: &Path, path: &str, body: &str, diagnostics: &mut Vec<Diagnostic>) {
    if !body.contains("conventions/AGENT_WORKFLOWS.md") {
        diagnostics.push(Diagnostic::new(
            path,
            "role instructions must reference `conventions/AGENT_WORKFLOWS.md`",
        ));
    }
    if !repo.join("conventions/AGENT_WORKFLOWS.md").is_file() {
        diagnostics.push(Diagnostic::new(
            path,
            "`conventions/AGENT_WORKFLOWS.md` does not exist",
        ));
    }
}

fn files(root: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut result = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some(extension))
        .collect::<Vec<_>>();
    result.sort();
    result
}

fn yaml_string<'a>(value: &'a YamlValue, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}
