mod imports;
mod markdown;
mod roles;
mod skills;

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: String,
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

pub fn validate_repo(repo: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    skills::validate(repo, &mut diagnostics);
    roles::validate(repo, &mut diagnostics);
    imports::validate(repo, &mut diagnostics);
    markdown::validate_dashes(repo, &mut diagnostics);
    markdown::validate_links(repo, &mut diagnostics);
    diagnostics.sort_by(|a, b| (&a.path, &a.message).cmp(&(&b.path, &b.message)));
    diagnostics
}

pub(crate) fn read_text(
    repo: &Path,
    relative: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<String> {
    match fs::read_to_string(repo.join(relative)) {
        Ok(contents) => Some(contents),
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                relative,
                format!("cannot read file: {error}"),
            ));
            None
        }
    }
}

pub(crate) fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture(PathBuf);

    impl Fixture {
        fn valid() -> Self {
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!("agent-check-{id}"));
            let fixture = Self(root);
            fixture.write("conventions/AGENT_WORKFLOWS.md", "# Workflows\n");
            fixture.add_skill("sample-skill", "A sample skill for validation and testing.");
            fixture.write(".codex/agents/explorer.toml", "name = \"explorer\"\ndescription = \"Map code paths\"\ndeveloper_instructions = \"Read conventions/AGENT_WORKFLOWS.md before work.\"\n");
            fixture.write(".claude/agents/explorer.md", "---\nname: explorer\ndescription: Map code paths\n---\nRead conventions/AGENT_WORKFLOWS.md before work.\n");
            fixture.write("AGENTS.md", "# Rules\n");
            fixture.write("CLAUDE.md", "@AGENTS.md\n");
            fixture.git_setup();
            fixture
        }

        fn add_skill(&self, name: &str, description: &str) {
            let body = format!("---\nname: {name}\ndescription: {description}\n---\n# Skill\n");
            let prompt = format!("Use ${name} for this workflow.");
            let metadata = format!(
                "interface:\n  display_name: Sample\n  short_description: A useful description for this skill\n  default_prompt: \"{prompt}\"\n"
            );
            let canonical = format!("skills/{name}/SKILL.md");
            let agent_loader = format!(".agents/skills/{name}/SKILL.md");
            let claude_loader = format!(".claude/skills/{name}/SKILL.md");
            self.write(&canonical, &body);
            self.write(&agent_loader, &format!("---\nname: {name}\ndescription: {description}\n---\nSee [canonical](../../../skills/{name}/SKILL.md).\n"));
            self.write(&claude_loader, &format!("---\nname: {name}\ndescription: {description}\n---\nRead [canonical](../../../skills/{name}/SKILL.md).\n"));
            self.write(&format!("skills/{name}/agents/openai.yaml"), &metadata);
            self.write(
                &format!(".agents/skills/{name}/agents/openai.yaml"),
                &metadata,
            );
            let mut catalog =
                fs::read_to_string(self.0.join("skills/README.md")).unwrap_or_default();
            catalog.push_str(&format!("[{name}]({name}/SKILL.md)\n"));
            self.write("skills/README.md", &catalog);
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn git_setup(&self) {
            let init = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&self.0)
                .status()
                .unwrap();
            assert!(init.success());
            let add = std::process::Command::new("git")
                .args(["add", "AGENTS.md", "CLAUDE.md"])
                .current_dir(&self.0)
                .status()
                .unwrap();
            assert!(add.success());
        }

        fn root(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn complete_valid_fixture_passes() {
        let fixture = Fixture::valid();
        assert!(
            validate_repo(fixture.root()).is_empty(),
            "{:#?}",
            validate_repo(fixture.root())
        );
    }

    #[test]
    fn reports_broken_skill_loader_target() {
        let fixture = Fixture::valid();
        fixture.write(".agents/skills/sample-skill/SKILL.md", "---\nname: sample-skill\ndescription: A sample skill for validation and testing.\n---\n[bad](../../../missing/SKILL.md)\n");
        assert!(
            validate_repo(fixture.root())
                .iter()
                .any(|item| item.message.contains("does not resolve to canonical"))
        );
    }

    #[test]
    fn reports_broken_inline_markdown_link() {
        let fixture = Fixture::valid();
        fixture.write(".agents/skills/sample-skill/SKILL.md", "---\nname: sample-skill\ndescription: A sample skill for validation and testing.\n---\n[canonical](../../../skills/sample-skill/SKILL.md) [missing](../../../missing.md)\n");
        assert!(
            validate_repo(fixture.root())
                .iter()
                .any(|item| item.message.contains("broken local Markdown link"))
        );
    }

    #[test]
    fn validates_links_in_any_canonical_skill_and_nested_reference() {
        let fixture = Fixture::valid();
        fixture.add_skill("another-workflow", "Inspect another workflow.");
        fixture.write(
            "skills/another-workflow/SKILL.md",
            "---\nname: another-workflow\ndescription: Inspect another workflow.\n---\n[guide](references/guide.md)\n",
        );
        fixture.write(
            "skills/another-workflow/references/guide.md",
            "[missing](absent.md)\n",
        );
        let diagnostics = validate_repo(fixture.root());
        assert!(diagnostics.iter().any(|item| {
            item.path == "skills/another-workflow/references/guide.md"
                && item.message.contains("broken local Markdown link")
        }));

        fixture.write("skills/another-workflow/references/absent.md", "# Guide\n");
        assert!(validate_repo(fixture.root()).is_empty());
    }

    #[test]
    fn detects_missing_skill_catalog_entries() {
        let fixture = Fixture::valid();
        fixture.write("skills/README.md", "# Skill catalog\n");
        let diagnostics = validate_repo(fixture.root());
        assert!(diagnostics.iter().any(|item| {
            item.path == "skills/README.md"
                && item
                    .message
                    .contains("catalog must link to canonical skill")
        }));
    }

    #[test]
    fn checks_dashes_in_canonical_references_and_metadata() {
        let fixture = Fixture::valid();
        fixture.write(
            "skills/sample-skill/references/details.md",
            "A reference with an invalid \u{2014} separator.\n",
        );
        fixture.write(
            "skills/sample-skill/agents/openai.yaml",
            "interface:\n  display_name: 'Sample \u{2013} Skill'\n  short_description: A useful description for this skill\n  default_prompt: Use $sample-skill for this workflow.\n",
        );
        let diagnostics = validate_repo(fixture.root());
        for path in [
            "skills/sample-skill/references/details.md",
            "skills/sample-skill/agents/openai.yaml",
        ] {
            assert!(
                diagnostics.iter().any(|item| {
                    item.path == path && item.message.contains("en dash or em dash")
                })
            );
        }
    }

    #[test]
    fn reports_metadata_copy_mismatch() {
        let fixture = Fixture::valid();
        fixture.write(".agents/skills/sample-skill/agents/openai.yaml", "interface:\n  display_name: Different\n  short_description: A useful description for this skill\n  default_prompt: \"Use $sample-skill for this workflow.\"\n");
        assert!(
            validate_repo(fixture.root())
                .iter()
                .any(|item| item.message.contains("metadata differs"))
        );
    }

    #[test]
    fn reports_malformed_role_toml_and_role_description_mismatch() {
        let malformed = Fixture::valid();
        malformed.write(".codex/agents/explorer.toml", "name = [");
        assert!(
            validate_repo(malformed.root())
                .iter()
                .any(|item| item.message.contains("invalid TOML"))
        );

        let mismatch = Fixture::valid();
        mismatch.write(".claude/agents/explorer.md", "---\nname: explorer\ndescription: A different description\n---\nRead conventions/AGENT_WORKFLOWS.md before work.\n");
        assert!(validate_repo(mismatch.root()).iter().any(|item| {
            item.message
                .contains("description for role `explorer` differs")
        }));
    }

    #[test]
    fn requires_exact_claude_import_line() {
        let fixture = Fixture::valid();
        fixture.write("CLAUDE.md", "Please read AGENTS.md\n");
        assert!(
            validate_repo(fixture.root())
                .iter()
                .any(|item| item.message.contains("must contain an `@AGENTS.md` import"))
        );
    }

    #[test]
    fn reports_missing_role_directories() {
        let fixture = Fixture::valid();
        fs::remove_dir_all(fixture.root().join(".codex/agents")).unwrap();
        fs::remove_dir_all(fixture.root().join(".claude/agents")).unwrap();
        let diagnostics = validate_repo(fixture.root());
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("role directory is missing"))
        );
    }

    #[test]
    fn accepts_bom_and_crlf_frontmatter_but_rejects_duplicate_or_blank_metadata() {
        let fixture = Fixture::valid();
        let valid = "\u{feff}---\r\nname: sample-skill\r\ndescription: A sample skill for validation and testing.\r\n---\r\nBody\r\n";
        fixture.write("skills/sample-skill/SKILL.md", valid);
        fixture.write(".agents/skills/sample-skill/SKILL.md", "\u{feff}---\r\nname: sample-skill\r\ndescription: A sample skill for validation and testing.\r\n---\r\n[canonical](../../../skills/sample-skill/SKILL.md)\r\n");
        fixture.write(".claude/skills/sample-skill/SKILL.md", "\u{feff}---\r\nname: sample-skill\r\ndescription: A sample skill for validation and testing.\r\n---\r\n[canonical](../../../skills/sample-skill/SKILL.md)\r\n");
        fixture.write(".claude/agents/explorer.md", "\u{feff}---\r\nname: explorer\r\ndescription: Map code paths\r\n---\r\nRead conventions/AGENT_WORKFLOWS.md before work.\r\n");
        assert!(validate_repo(fixture.root()).is_empty());

        let duplicate = "---\nname: sample-skill\nname: second\ndescription: useful\n---\n";
        assert!(skills::parse_frontmatter(duplicate).is_err());
        fixture.write(
            "skills/sample-skill/SKILL.md",
            "---\nname: sample-skill\ndescription: '   '\n---\n",
        );
        let diagnostics = validate_repo(fixture.root());
        assert!(
            diagnostics
                .iter()
                .any(|item| item.message.contains("description must be nonempty"))
        );
    }

    #[test]
    fn accepts_maximum_skill_name_length_and_rejects_longer_names() {
        assert!(skills::valid_name(&"a".repeat(64)));
        assert!(!skills::valid_name(&"a".repeat(65)));
    }
}
