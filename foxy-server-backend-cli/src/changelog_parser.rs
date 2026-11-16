use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct ChangelogSection {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangelogVersion {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub sections: Vec<ChangelogSection>,
}

pub fn parse_changelog(path: &Path) -> Result<Vec<ChangelogVersion>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    parse_changelog_str(&content)
}

pub fn parse_changelog_str(content: &str) -> Result<Vec<ChangelogVersion>> {
    let mut versions: Vec<ChangelogVersion> = Vec::new();
    let mut current_version: Option<ChangelogVersion> = None;
    let mut current_section: Option<ChangelogSection> = None;

    for line in content.lines() {
        let trimmed = line.trim().trim_start_matches('\u{feff}').trim();

        if let Some(rest) = trimmed.strip_prefix("# ") {
            if let Some(ref mut ver) = current_version {
                if let Some(sec) = current_section.take()
                    && !sec.items.is_empty()
                {
                    ver.sections.push(sec);
                }
                versions.push(ver.clone());
            }

            let (version, date) = parse_version_header(rest);
            current_version = Some(ChangelogVersion {
                version,
                date,
                sections: Vec::new(),
            });
            current_section = None;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("## ") {
            if let Some(ref mut ver) = current_version {
                if let Some(sec) = current_section.take()
                    && !sec.items.is_empty()
                {
                    ver.sections.push(sec);
                }
                current_section = Some(ChangelogSection {
                    title: rest.trim().to_string(),
                    items: Vec::new(),
                });
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- ") {
            if let Some(ref mut sec) = current_section {
                let item = rest.trim();
                if !item.is_empty() {
                    sec.items.push(item.to_string());
                }
            }
            continue;
        }

        if !trimmed.is_empty()
            && let Some(ref mut sec) = current_section
            && let Some(last) = sec.items.last_mut()
        {
            last.push(' ');
            last.push_str(trimmed);
        }
    }

    if let Some(ref mut ver) = current_version {
        if let Some(sec) = current_section.take()
            && !sec.items.is_empty()
        {
            ver.sections.push(sec);
        }
        versions.push(ver.clone());
    }

    Ok(versions)
}

fn parse_version_header(header: &str) -> (String, Option<String>) {
    let header = header.trim();

    if header.starts_with('[')
        && let Some(end_bracket) = header.find(']')
    {
        let version = header[1..end_bracket].trim().to_string();
        let rest = header[end_bracket + 1..].trim();
        let date = rest
            .strip_prefix('-')
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty());
        return (version, date);
    }

    if let Some(dash_pos) = header.find(" - ") {
        let version = header[..dash_pos].trim().to_string();
        let date = header[dash_pos + 3..].trim().to_string();
        return (version, if date.is_empty() { None } else { Some(date) });
    }

    (header.to_string(), None)
}

pub fn find_version<'a>(
    versions: &'a [ChangelogVersion],
    target: &str,
) -> Option<&'a ChangelogVersion> {
    let target_key = version_key(target);
    versions
        .iter()
        .find(|v| version_key(&v.version) == target_key)
}

fn version_key(version: &str) -> String {
    let version = version.trim().trim_start_matches('\u{feff}').trim();
    version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_changelog() {
        let input = r#"# 0.6.0
## Added
- Feature A
- Feature B

## Fixed
- Bug fix C

# 0.4.0
## Changed
- Some change
"#;
        let versions = parse_changelog_str(input).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, "0.6.0");
        assert_eq!(versions[0].sections.len(), 2);
        assert_eq!(versions[0].sections[0].title, "Added");
        assert_eq!(versions[0].sections[0].items.len(), 2);
        assert_eq!(versions[1].version, "0.4.0");
    }

    #[test]
    fn test_parse_bracketed_version_with_date() {
        let input = r#"# [0.5.1] - 2026-03-28
## Added
- New feature
"#;
        let versions = parse_changelog_str(input).unwrap();
        assert_eq!(versions[0].version, "0.5.1");
        assert_eq!(versions[0].date, Some("2026-03-28".to_string()));
    }

    #[test]
    fn test_parse_first_heading_with_utf8_bom() {
        let input = "\u{feff}# 1.0.0\n## Added\n- Release notes\n";

        let versions = parse_changelog_str(input).unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "1.0.0");
        assert!(find_version(&versions, "1.0.0").is_some());
    }

    #[test]
    fn test_find_version_accepts_v_prefix() {
        let input = "# v1.0.0\n## Added\n- Release notes\n";

        let versions = parse_changelog_str(input).unwrap();

        assert!(find_version(&versions, "1.0.0").is_some());
    }

    #[test]
    fn test_preserves_no_changes_sections() {
        let input = r#"# 0.6.0
## Added
- Something

## Removed
- No user-facing removals in this release.

## Reverted
- No reverted changes in this release.
"#;
        let versions = parse_changelog_str(input).unwrap();
        assert_eq!(versions[0].sections.len(), 3);
        assert_eq!(versions[0].sections[0].title, "Added");
        assert_eq!(versions[0].sections[1].title, "Removed");
        assert_eq!(versions[0].sections[2].title, "Reverted");
    }
}
