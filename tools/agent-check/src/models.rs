//! Reports whether the models pinned for Foxy's roles are still offered, and whether newer ones appeared.
//!
//! Read-only: it never edits a file and never calls a model. Codex and Claude Code inherit the session
//! model, so they pin nothing. Antigravity pins are compared with `agy models` for the signed-in
//! account; no local CLI lists Cursor's models, so its pins are only reported.

use crate::antigravity::{ARGON_MODEL, ROLE_MODELS, find_agy, run_captured};
use crate::skills::parse_frontmatter;
use serde_yaml_ng::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const AGY_TIMEOUT: Duration = Duration::from_secs(90);
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const WORKFLOWS: &str = "conventions/AGENT_WORKFLOWS.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub client: &'static str,
    pub role: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Missing,
    Candidate,
    Unchecked,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Missing => "missing",
            Kind::Candidate => "candidate",
            Kind::Unchecked => "unchecked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub client: &'static str,
    pub detail: String,
}

impl Finding {
    fn new(kind: Kind, client: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind,
            client,
            detail: detail.into(),
        }
    }
}

/// `gemini-<version>-<family>[-<effort>]` as (version, family, effort).
pub(crate) fn parse_gemini(id: &str) -> Option<(Vec<u32>, String, Option<String>)> {
    let rest = id.strip_prefix("gemini-")?;
    let mut parts = rest.split('-').collect::<Vec<_>>();
    let version = parts
        .first()?
        .split('.')
        .map(|part| part.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    parts.remove(0);
    let effort = match parts.last() {
        Some(last) if EFFORTS.contains(last) => parts.pop().map(str::to_owned),
        _ => None,
    };
    let family = parts.join("-");
    if family.is_empty()
        || !family
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch == '-')
    {
        return None;
    }
    Some((version, family, effort))
}

/// Model ids from `agy models` output (`<id>\t<label>` lines).
pub(crate) fn parse_agy_models(output: &str) -> BTreeSet<String> {
    output
        .lines()
        .filter_map(|line| line.split('\t').next())
        .map(str::trim)
        .filter(|id| {
            id.starts_with("gemini-") || id.starts_with("claude-") || id.starts_with("gpt-")
        })
        .map(str::to_owned)
        .collect()
}

/// Gemini ids with a thinking level written as code spans in `text`.
pub(crate) fn gemini_ids_named(text: &str) -> BTreeSet<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| parse_gemini(span).is_some_and(|(_, _, effort)| effort.is_some()))
        .map(str::to_owned)
        .collect()
}

pub fn pins(repo: &Path) -> Vec<Pin> {
    let mut pins = ROLE_MODELS
        .iter()
        .map(|(role, model)| Pin {
            client: "antigravity",
            role: (*role).to_owned(),
            model: (*model).to_owned(),
        })
        .collect::<Vec<_>>();
    pins.push(Pin {
        client: "antigravity",
        role: "--argon".to_owned(),
        model: ARGON_MODEL.to_owned(),
    });
    let pinned = pins
        .iter()
        .map(|pin| pin.model.clone())
        .collect::<BTreeSet<_>>();
    if let Ok(text) = fs::read_to_string(repo.join(WORKFLOWS)) {
        for model in gemini_ids_named(&text) {
            if !pinned.contains(&model) {
                pins.push(Pin {
                    client: "antigravity",
                    role: format!("(named in {WORKFLOWS})"),
                    model,
                });
            }
        }
    }
    let mut cursor = fs::read_dir(repo.join(".cursor/agents"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("md"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    cursor.sort();
    for path in cursor {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok((frontmatter, _)) = parse_frontmatter(&text) else {
            continue;
        };
        let field = |key: &str| {
            frontmatter
                .get(key)
                .and_then(YamlValue::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        pins.push(Pin {
            client: "cursor",
            role: field("name"),
            model: field("model"),
        });
    }
    pins
}

pub(crate) fn findings(pins: &[Pin], listed: Option<&BTreeSet<String>>) -> Vec<Finding> {
    let mut findings = Vec::new();
    match listed {
        None => findings.push(Finding::new(
            Kind::Unchecked,
            "antigravity",
            "`agy models` is unavailable",
        )),
        Some(listed) if !listed.iter().any(|id| id.starts_with("gemini-4")) => {
            findings.push(Finding::new(
                Kind::Unchecked,
                "antigravity",
                "`agy models` lists no Gemini 4 model; the CLI is likely signed out or on another plan",
            ))
        }
        Some(listed) => {
            let gemini = pins.iter().filter(|pin| pin.client == "antigravity");
            let mut families: BTreeMap<String, Vec<u32>> = BTreeMap::new();
            for pin in gemini {
                if !listed.contains(&pin.model) {
                    findings.push(Finding::new(
                        Kind::Missing,
                        "antigravity",
                        format!("{}: {} is not listed", pin.role, pin.model),
                    ));
                }
                if let Some((version, family, _)) = parse_gemini(&pin.model) {
                    let newest = families.entry(family).or_default();
                    if version > *newest {
                        *newest = version;
                    }
                }
            }
            let newest_pinned = families.values().max().cloned().unwrap_or_default();
            for id in listed {
                let Some((version, family, _)) = parse_gemini(id) else {
                    continue;
                };
                match families.get(&family) {
                    Some(pinned) if version > *pinned => findings.push(Finding::new(
                        Kind::Candidate,
                        "antigravity",
                        format!("newer {family} listed: {id}"),
                    )),
                    None if !newest_pinned.is_empty() && version > newest_pinned => {
                        findings.push(Finding::new(
                            Kind::Candidate,
                            "antigravity",
                            format!("new Gemini generation listed: {id}"),
                        ))
                    }
                    _ => {}
                }
            }
        }
    }
    findings.push(Finding::new(
        Kind::Unchecked,
        "cursor",
        "no local CLI lists Cursor models; check Cursor's models page",
    ));
    findings.push(Finding::new(
        Kind::Unchecked,
        "codex, claude",
        "inherit the session model; nothing pinned",
    ));
    findings.sort_by(|a, b| (a.kind, a.client, &a.detail).cmp(&(b.kind, b.client, &b.detail)));
    findings
}

fn agy_models() -> Option<BTreeSet<String>> {
    let mut command = Command::new(find_agy()?);
    command.arg("models");
    let captured = run_captured(command, AGY_TIMEOUT).ok()?;
    (captured.success == Some(true)).then(|| parse_agy_models(&captured.stdout))
}

/// Prints the pins and findings. Returns 1 when a pinned model is missing, else 0.
pub fn report(repo: &Path) -> i32 {
    let pins = pins(repo);
    let listed = agy_models();
    let findings = findings(&pins, listed.as_ref());
    println!("Pins");
    for pin in &pins {
        println!("  {:12} {:32} {}", pin.client, pin.role, pin.model);
    }
    println!("Findings");
    for finding in &findings {
        println!(
            "  {:10} {:13} {}",
            finding.kind.label(),
            finding.client,
            finding.detail
        );
    }
    if findings
        .iter()
        .all(|finding| finding.kind == Kind::Unchecked)
    {
        println!("No change indicated.");
    }
    i32::from(findings.iter().any(|finding| finding.kind == Kind::Missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(model: &str) -> Pin {
        Pin {
            client: "antigravity",
            role: "explorer".to_owned(),
            model: model.to_owned(),
        }
    }

    fn listed(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn gemini_ids_split_into_version_family_and_effort() {
        assert_eq!(
            parse_gemini("gemini-3.8-flash-medium"),
            Some((vec![3, 8], "flash".to_owned(), Some("medium".to_owned())))
        );
        assert_eq!(
            parse_gemini("gemini-4-hydrogen-xhigh"),
            Some((vec![4], "hydrogen".to_owned(), Some("xhigh".to_owned())))
        );
        assert_eq!(parse_gemini("gemini-pro-orchestrator-g1-pro"), None);
        assert_eq!(parse_gemini("claude-opus-5-5-high"), None);
    }

    #[test]
    fn agy_output_keeps_model_ids_only() {
        let output = "Fetching available models...\ngemini-4-argon-high\tGemini 4 Argon (High)\nclaude-sonnet-5-5-low\tClaude\n";
        assert_eq!(
            parse_agy_models(output),
            listed(&["claude-sonnet-5-5-low", "gemini-4-argon-high"])
        );
    }

    #[test]
    fn named_ids_need_a_thinking_level() {
        let text = "Use `gemini-4-hydrogen-high` or `gemini-4-argon` and `cargo agent-check`.";
        assert_eq!(gemini_ids_named(text), listed(&["gemini-4-hydrogen-high"]));
    }

    #[test]
    fn missing_pins_and_newer_models_are_reported() {
        let pins = [
            pin("gemini-3.8-flash-medium"),
            pin("gemini-4-hydrogen-high"),
        ];
        let found = findings(
            &pins,
            Some(&listed(&[
                "gemini-3.8-flash-medium",
                "gemini-4.1-hydrogen-high",
                "gemini-5-neon-high",
            ])),
        );
        let kinds = found
            .iter()
            .map(|finding| (finding.kind, finding.detail.as_str()))
            .collect::<Vec<_>>();
        assert!(kinds.contains(&(
            Kind::Missing,
            "explorer: gemini-4-hydrogen-high is not listed"
        )));
        assert!(kinds.contains(&(
            Kind::Candidate,
            "newer hydrogen listed: gemini-4.1-hydrogen-high"
        )));
        assert!(kinds.contains(&(
            Kind::Candidate,
            "new Gemini generation listed: gemini-5-neon-high"
        )));
    }

    #[test]
    fn a_signed_out_list_is_unchecked_not_missing() {
        let found = findings(
            &[pin("gemini-4-hydrogen-high")],
            Some(&listed(&["gemini-3.8-flash-low"])),
        );
        assert!(found.iter().all(|finding| finding.kind == Kind::Unchecked));
        let found = findings(&[pin("gemini-4-hydrogen-high")], None);
        assert!(found.iter().all(|finding| finding.kind == Kind::Unchecked));
    }

    #[test]
    fn current_pins_give_no_change() {
        let found = findings(
            &[pin("gemini-3.8-flash-medium"), pin("gemini-4-argon-high")],
            Some(&listed(&[
                "gemini-3.8-flash-medium",
                "gemini-4-argon-high",
                "gemini-4-hydrogen-low",
            ])),
        );
        assert!(found.iter().all(|finding| finding.kind == Kind::Unchecked));
    }
}
