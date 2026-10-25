use crate::{Diagnostic, read_text};
use serde_yaml_ng::Value as YamlValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct SkillMeta {
    name: String,
    description: String,
    body: String,
}

pub(super) fn validate(repo: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let canonical_root = repo.join("skills");
    if !canonical_root.is_dir() {
        diagnostics.push(Diagnostic::new(
            "skills",
            "canonical skills directory is missing",
        ));
    }
    let mut canonical = BTreeMap::new();
    for directory in child_dirs(&canonical_root) {
        let Some(dir_name) = directory.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let relative = format!("skills/{dir_name}/SKILL.md");
        let Some(text) = read_text(repo, &relative, diagnostics) else {
            continue;
        };
        if let Some(meta) = skill_meta(&relative, &text, diagnostics) {
            if meta.name != dir_name {
                diagnostics.push(Diagnostic::new(
                    &relative,
                    format!(
                        "frontmatter name `{}` must match directory `{dir_name}`",
                        meta.name
                    ),
                ));
            }
            if canonical.insert(meta.name.clone(), meta).is_some() {
                diagnostics.push(Diagnostic::new(&relative, "duplicate canonical skill name"));
            }
        }
    }
    if canonical.is_empty() {
        diagnostics.push(Diagnostic::new(
            "skills",
            "canonical skill set must not be empty",
        ));
    }

    if let Some(catalog) = read_text(repo, "skills/README.md", diagnostics) {
        for name in canonical.keys() {
            if !links_to_canonical(repo, "skills/README.md", &catalog, name) {
                diagnostics.push(Diagnostic::new(
                    "skills/README.md",
                    format!("catalog must link to canonical skill `{name}`"),
                ));
            }
        }
    }

    for (directory, label) in [
        (".agents/skills", "Agent Skills"),
        (".claude/skills", "Claude skills"),
    ] {
        let root = repo.join(directory);
        if !root.is_dir() {
            diagnostics.push(Diagnostic::new(
                directory,
                "skill loader directory is missing",
            ));
        }
        let mut found = BTreeMap::new();
        for dir in child_dirs(&root) {
            let Some(dir_name) = dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let relative = format!("{directory}/{dir_name}/SKILL.md");
            let Some(text) = read_text(repo, &relative, diagnostics) else {
                continue;
            };
            if let Some(meta) = skill_meta(&relative, &text, diagnostics) {
                if meta.name != dir_name {
                    diagnostics.push(Diagnostic::new(
                        &relative,
                        format!(
                            "frontmatter name `{}` must match directory `{dir_name}`",
                            meta.name
                        ),
                    ));
                }
                if let Some(expected) = canonical.get(&meta.name) {
                    if expected.name != meta.name || expected.description != meta.description {
                        diagnostics.push(Diagnostic::new(
                            &relative,
                            "name or description differs from canonical skill",
                        ));
                    }
                    if !links_to_canonical(repo, &relative, &meta.body, &meta.name) {
                        diagnostics.push(Diagnostic::new(
                            &relative,
                            format!(
                                "loader link does not resolve to canonical skill `{}`",
                                expected.name
                            ),
                        ));
                    }
                } else {
                    diagnostics.push(Diagnostic::new(
                        &relative,
                        format!("loader references unknown skill `{}`", meta.name),
                    ));
                }
                if found.insert(meta.name.clone(), meta).is_some() {
                    diagnostics.push(Diagnostic::new(&relative, "duplicate loader skill name"));
                }
            }
        }
        compare_sets(&canonical, &found, label, diagnostics);
    }

    for name in canonical.keys() {
        let relative = format!("skills/{name}/agents/openai.yaml");
        let Some(text) = read_text(repo, &relative, diagnostics) else {
            continue;
        };
        let interface: YamlValue = match serde_yaml_ng::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                diagnostics.push(Diagnostic::new(&relative, format!("invalid YAML: {error}")));
                continue;
            }
        };
        let Some(fields) = interface.get("interface") else {
            diagnostics.push(Diagnostic::new(&relative, "missing interface mapping"));
            continue;
        };
        for key in ["display_name", "short_description", "default_prompt"] {
            if yaml_string(fields, key).is_none_or(|value| value.trim().is_empty()) {
                diagnostics.push(Diagnostic::new(
                    &relative,
                    format!("interface.{key} must be nonempty"),
                ));
            }
        }
        if let Some(short) = yaml_string(fields, "short_description")
            && !(25..=64).contains(&short.chars().count())
        {
            diagnostics.push(Diagnostic::new(
                &relative,
                "interface.short_description must be 25 to 64 characters",
            ));
        }
        if let Some(prompt) = yaml_string(fields, "default_prompt")
            && !prompt.contains(&format!("${name}"))
        {
            diagnostics.push(Diagnostic::new(
                &relative,
                format!("default_prompt must mention `${name}`"),
            ));
        }
        let loader = format!(".agents/skills/{name}/agents/openai.yaml");
        let Some(loader_text) = read_text(repo, &loader, diagnostics) else {
            continue;
        };
        match serde_yaml_ng::from_str::<YamlValue>(&loader_text) {
            Ok(loader_yaml) if loader_yaml == interface => {}
            Ok(_) => diagnostics.push(Diagnostic::new(
                &loader,
                format!("metadata differs from `{relative}`"),
            )),
            Err(error) => {
                diagnostics.push(Diagnostic::new(&loader, format!("invalid YAML: {error}")))
            }
        }
    }
}

fn skill_meta(path: &str, text: &str, diagnostics: &mut Vec<Diagnostic>) -> Option<SkillMeta> {
    let (frontmatter, body) = match parse_frontmatter(text) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(Diagnostic::new(path, error));
            return None;
        }
    };
    let name = yaml_string(&frontmatter, "name").unwrap_or_default().trim();
    let description = yaml_string(&frontmatter, "description")
        .unwrap_or_default()
        .trim();
    if !valid_name(name) {
        diagnostics.push(Diagnostic::new(
            path,
            format!("invalid skill name `{name}`"),
        ));
    }
    if description.is_empty() {
        diagnostics.push(Diagnostic::new(path, "description must be nonempty"));
    }
    Some(SkillMeta {
        name: name.into(),
        description: description.into(),
        body: body.into(),
    })
}

pub(super) fn parse_frontmatter(text: &str) -> Result<(YamlValue, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset = 0;
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().ok_or("missing YAML frontmatter")?;
    if line_text(first) != "---" {
        return Err("missing YAML frontmatter".into());
    }
    offset += first.len();
    let yaml_start = offset;
    for line in lines {
        if line_text(line) == "---" {
            let yaml_end = offset;
            offset += line.len();
            let yaml = &text[yaml_start..yaml_end];
            let body = &text[offset..];
            let value = serde_yaml_ng::from_str(yaml)
                .map_err(|error| format!("invalid or duplicate YAML frontmatter: {error}"))?;
            return Ok((value, body));
        }
        offset += line.len();
    }
    Err("unterminated YAML frontmatter".into())
}

fn line_text(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn yaml_string<'a>(value: &'a YamlValue, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

fn child_dirs(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut dirs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}

fn compare_sets<T>(
    expected: &BTreeMap<String, T>,
    found: &BTreeMap<String, T>,
    label: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for name in expected.keys().filter(|name| !found.contains_key(*name)) {
        diagnostics.push(Diagnostic::new(
            "skills",
            format!("{label} is missing skill `{name}`"),
        ));
    }
    for name in found.keys().filter(|name| !expected.contains_key(*name)) {
        diagnostics.push(Diagnostic::new(
            "skills",
            format!("{label} has unknown skill `{name}`"),
        ));
    }
}

fn links_to_canonical(repo: &Path, loader: &str, body: &str, name: &str) -> bool {
    let Ok(canonical) = repo.join(format!("skills/{name}/SKILL.md")).canonicalize() else {
        return false;
    };
    crate::markdown::targets(body).into_iter().any(|target| {
        let path = target.split('#').next().unwrap_or("");
        if path.is_empty()
            || path.starts_with("http:")
            || path.starts_with("https:")
            || path.starts_with("mailto:")
        {
            return false;
        }
        let candidate = if path.starts_with('/') {
            repo.join(path.trim_start_matches('/'))
        } else {
            repo.join(loader).parent().unwrap_or(repo).join(path)
        };
        candidate
            .canonicalize()
            .is_ok_and(|resolved| resolved == canonical)
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_frontmatter, valid_name};

    #[test]
    fn frontmatter_accepts_bom_crlf_and_whole_delimiters() {
        let input = "\u{feff}---\r\nname: skill\r\ndescription: useful\r\n---\r\nbody";
        assert!(parse_frontmatter(input).is_ok());
        assert!(parse_frontmatter("---\nname: skill\n--- extra\n").is_err());
    }

    #[test]
    fn names_reject_invalid_hyphen_shapes() {
        assert!(valid_name("foxy-agent-check"));
        for name in [
            "",
            "-start",
            "end-",
            "double--hyphen",
            "Upper",
            "with_under",
        ] {
            assert!(!valid_name(name), "{name}");
        }
    }

    #[test]
    fn duplicate_yaml_metadata_is_rejected() {
        let input = "---\nname: first\nname: second\ndescription: useful\n---\n";
        assert!(parse_frontmatter(input).is_err());
    }
}
