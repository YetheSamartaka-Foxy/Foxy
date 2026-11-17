//! Google Antigravity support: generated role and rule files, and the role launcher.
//!
//! Antigravity reads the root `AGENTS.md` and `.agents/skills/` itself, but not nested `AGENTS.md`
//! files below its start folder, `.claude/rules/` or `.claude/agents/`. `sync` writes their
//! Antigravity form. A role file cannot name a model or a thinking level, so `run_role` starts each
//! role as its own headless `agy --agent <role> --model <id>` session instead.

use crate::{Diagnostic, path_to_string, skills::parse_frontmatter};
use serde_yaml_ng::Value as YamlValue;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Gemini model and thinking level of each role; the main session picks its own.
pub const ROLE_MODELS: &[(&str, &str)] = &[
    ("explorer", "gemini-3.8-flash-medium"),
    ("worker", "gemini-4-hydrogen-medium"),
    ("reviewer", "gemini-4-hydrogen-high"),
    ("async-safety-reviewer", "gemini-4-hydrogen-high"),
];
pub const ARGON_MODEL: &str = "gemini-4-argon-high";
const ROLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const PRINT_TIMEOUT: &str = "1740s";
const SYNC_HINT: &str = "run `cargo agent-check sync-antigravity`";
const RULE_PREFIXES: [&str; 2] = ["folder-", "claude-"];
const ALL_TOOLS: &[&str] = &[
    "view_file",
    "list_dir",
    "find_by_name",
    "grep_search",
    "write_to_file",
    "replace_file_content",
    "multi_replace_file_content",
    "run_command",
    "search_web",
    "read_url_content",
];
// A global hook may rewrite a command to `rtk <command>`, so every shell rule accepts that prefix.
const READ_ONLY_SHELL: &str = r"command(regex:^(rtk )?(git (-C \S+ )?(log|show|diff|status|grep|ls-files|rev-parse|blame|branch|cat-file)|rg)(\s.*)?$)";
const DENIED_COMMANDS: &[&str] = &[
    r"command(regex:^(rtk )?git (-C \S+ )?(commit|push|reset|checkout|switch|rebase|merge|cherry-pick|tag|stash)\b.*$)",
    r"command(regex:^(rtk )?cargo (run|prerelease)\b.*$)",
    r"command(regex:.*target[\\/](debug|release)[\\/]Foxy(\.exe)?\b.*$)",
];

#[derive(Debug, Clone)]
pub struct Role {
    pub name: String,
    description: String,
    claude_tools: Vec<String>,
    body: String,
}

impl Role {
    fn has_tool(&self, tool: &str) -> bool {
        self.claude_tools.iter().any(|name| name == tool)
    }

    fn writes(&self) -> bool {
        self.claude_tools.is_empty() || self.has_tool("Edit") || self.has_tool("Write")
    }

    fn shell(&self) -> bool {
        self.claude_tools.is_empty() || self.has_tool("Bash") || self.has_tool("PowerShell")
    }
}

pub fn model_for(role: &str) -> Option<&'static str> {
    ROLE_MODELS
        .iter()
        .find(|(name, _)| *name == role)
        .map(|(_, model)| *model)
}

pub fn roles(repo: &Path) -> Result<Vec<Role>, String> {
    let mut roles = Vec::new();
    for path in markdown_files(&repo.join(".claude/agents")) {
        let relative = relative(repo, &path);
        let text = fs::read_to_string(&path).map_err(|error| format!("{relative}: {error}"))?;
        let (frontmatter, body) =
            parse_frontmatter(&text).map_err(|error| format!("{relative}: {error}"))?;
        let field = |key: &str| {
            frontmatter
                .get(key)
                .and_then(YamlValue::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        let claude_tools = match frontmatter.get("tools") {
            Some(YamlValue::String(list)) => split_list(list),
            Some(YamlValue::Sequence(items)) => items
                .iter()
                .filter_map(YamlValue::as_str)
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        roles.push(Role {
            name: field("name"),
            description: field("description"),
            claude_tools,
            body: body.replace("\r\n", "\n"),
        });
    }
    Ok(roles)
}

fn split_list(list: &str) -> Vec<String> {
    list.split(',')
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn antigravity_tools(role: &Role) -> Result<Vec<&'static str>, String> {
    if role.claude_tools.is_empty() {
        return Ok(ALL_TOOLS.to_vec());
    }
    let mut tools = Vec::new();
    for tool in &role.claude_tools {
        let mapped: &[&'static str] = match tool.as_str() {
            "Read" => &["view_file"],
            "Grep" => &["grep_search"],
            "Glob" => &["find_by_name", "list_dir"],
            "Bash" | "PowerShell" => &["run_command"],
            "Edit" => &["replace_file_content", "multi_replace_file_content"],
            "Write" => &["write_to_file"],
            "WebSearch" => &["search_web"],
            "WebFetch" => &["read_url_content"],
            other => {
                return Err(format!(
                    "role `{}` uses Claude tool `{other}`, which has no Antigravity name",
                    role.name
                ));
            }
        };
        for name in mapped {
            if !tools.contains(name) {
                tools.push(*name);
            }
        }
    }
    Ok(tools)
}

/// JSON string literal, which is also a valid YAML double-quoted scalar.
pub(crate) fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn role_file(role: &Role) -> Result<String, String> {
    let mut out = format!(
        "---\nname: {}\ndescription: {}\nsubagent: false\ntools:\n",
        role.name,
        quoted(&role.description)
    );
    for tool in antigravity_tools(role)? {
        out.push_str(&format!("  - {tool}\n"));
    }
    out.push_str("---\n");
    out.push_str(&role.body);
    Ok(out)
}

fn rule(description: &str, globs: &[String], body: &str) -> String {
    format!(
        "---\ntrigger: glob\ndescription: {}\nglobs: {}\n---\n\n{}\n",
        quoted(description),
        quoted(&globs.join(", ")),
        body.trim()
    )
}

pub(crate) fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open..].find('}').map(|offset| open + offset) else {
        return vec![pattern.to_owned()];
    };
    pattern[open + 1..close]
        .split(',')
        .flat_map(|option| {
            expand_braces(&format!(
                "{}{option}{}",
                &pattern[..open],
                &pattern[close + 1..]
            ))
        })
        .collect()
}

pub(crate) fn first_markdown_reference(body: &str) -> Option<&str> {
    body.split('`')
        .skip(1)
        .step_by(2)
        .find(|span| span.ends_with(".md") && !span.contains(char::is_whitespace))
}

fn nested_agents(repo: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "**/AGENTS.md",
        ])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run git ls-files: {error}"))?;
    if !output.status.success() {
        return Err(format!("git ls-files failed with status {}", output.status));
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| String::from_utf8_lossy(bytes).replace('\\', "/"))
        .filter(|path| path != "AGENTS.md" && repo.join(path).is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Every generated file as repository-relative path and contents.
pub fn expected_files(repo: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut files = BTreeMap::new();
    for role in roles(repo)? {
        files.insert(
            format!(".agents/agents/{}.md", role.name),
            role_file(&role)?,
        );
    }
    for agents in nested_agents(repo)? {
        let folder = agents.trim_end_matches("/AGENTS.md");
        files.insert(
            format!(".agents/rules/folder-{}.md", folder.replace('/', "-")),
            rule(
                &format!("Folder rules from {folder}/AGENTS.md for files below {folder}/."),
                &[format!("{folder}/**")],
                &format!("@[{folder}/AGENTS.md](../../{folder}/AGENTS.md)"),
            ),
        );
    }
    let rules_root = repo.join(".claude/rules");
    for path in markdown_files(&rules_root) {
        let relative_path = relative(repo, &path);
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{relative_path}: {error}"))?;
        let (frontmatter, body) =
            parse_frontmatter(&text).map_err(|error| format!("{relative_path}: {error}"))?;
        let globs = match frontmatter.get("paths") {
            Some(YamlValue::Sequence(items)) => items
                .iter()
                .filter_map(YamlValue::as_str)
                .flat_map(expand_braces)
                .collect::<Vec<_>>(),
            Some(YamlValue::String(single)) => expand_braces(single),
            _ => Vec::new(),
        };
        if globs.is_empty() {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let source = first_markdown_reference(body)
            .map(str::to_owned)
            .unwrap_or(relative_path);
        files.insert(
            format!(".agents/rules/claude-{stem}.md"),
            rule(
                &format!("Binding notes in {source} for the files it covers."),
                &globs,
                &body.replace("\r\n", "\n"),
            ),
        );
    }
    Ok(files)
}

fn generated_files(repo: &Path) -> Vec<String> {
    let mut found = markdown_files(&repo.join(".agents/agents"))
        .into_iter()
        .map(|path| relative(repo, &path))
        .collect::<Vec<_>>();
    found.extend(
        markdown_files(&repo.join(".agents/rules"))
            .into_iter()
            .filter(|path| {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                RULE_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
            })
            .map(|path| relative(repo, &path)),
    );
    found
}

/// Checks the generated files once a repository has opted in with `.agents/agents/`.
pub(super) fn validate(repo: &Path, diagnostics: &mut Vec<Diagnostic>) {
    if !repo.join(".agents/agents").is_dir() {
        return;
    }
    match roles(repo) {
        Ok(roles) => {
            for role in &roles {
                if model_for(&role.name).is_none() {
                    diagnostics.push(Diagnostic::new(
                        format!(".claude/agents/{}.md", role.name),
                        "role has no Gemini model in tools/agent-check/src/antigravity.rs",
                    ));
                }
            }
            for (name, _) in ROLE_MODELS {
                if !roles.iter().any(|role| role.name == *name) {
                    diagnostics.push(Diagnostic::new(
                        "tools/agent-check/src/antigravity.rs",
                        format!("Gemini model listed for missing role `{name}`"),
                    ));
                }
            }
        }
        Err(error) => diagnostics.push(Diagnostic::new(".claude/agents", error)),
    }
    let expected = match expected_files(repo) {
        Ok(expected) => expected,
        Err(error) => {
            diagnostics.push(Diagnostic::new(".agents", error));
            return;
        }
    };
    for (path, contents) in &expected {
        match fs::read_to_string(repo.join(path)) {
            Ok(actual) if actual.replace("\r\n", "\n") == *contents => {}
            Ok(_) => diagnostics.push(Diagnostic::new(
                path,
                format!("stale Antigravity file; {SYNC_HINT}"),
            )),
            Err(_) => diagnostics.push(Diagnostic::new(
                path,
                format!("missing Antigravity file; {SYNC_HINT}"),
            )),
        }
    }
    for path in generated_files(repo) {
        if !expected.contains_key(&path) {
            diagnostics.push(Diagnostic::new(
                path,
                format!("Antigravity file no longer derived from any source; {SYNC_HINT}"),
            ));
        }
    }
}

/// Writes every generated file that differs and removes those no longer derived. Returns (written, removed).
pub fn sync(repo: &Path) -> Result<(usize, usize), String> {
    let expected = expected_files(repo)?;
    let mut written = 0;
    for (path, contents) in &expected {
        let target = repo.join(path);
        let current = fs::read_to_string(&target).ok();
        if current.as_deref().map(|text| text.replace("\r\n", "\n")) != Some(contents.clone()) {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| format!("{path}: {error}"))?;
            }
            fs::write(&target, contents).map_err(|error| format!("{path}: {error}"))?;
            written += 1;
        }
    }
    let mut removed = 0;
    for path in generated_files(repo) {
        if !expected.contains_key(&path) {
            fs::remove_file(repo.join(&path)).map_err(|error| format!("{path}: {error}"))?;
            removed += 1;
        }
    }
    Ok((written, removed))
}

fn rtk_rules() -> Vec<String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let Some(home) = home else {
        return Vec::new();
    };
    let env_or =
        |key: &str, fallback: PathBuf| std::env::var_os(key).map(PathBuf::from).unwrap_or(fallback);
    let mut rules = Vec::new();
    for (kind, path) in [
        ("read_file", home.join(".claude").join("settings.json")),
        ("read_file", env_or("APPDATA", home.clone()).join("rtk")),
        (
            "write_file",
            env_or("LOCALAPPDATA", home.clone()).join("rtk"),
        ),
    ] {
        if path.exists() {
            let target = path_to_string(&path);
            rules.push(format!("{kind}({target})"));
            if kind == "write_file" {
                rules.push(format!("read_file({target})"));
            }
        }
    }
    rules
}

/// Session-only permission settings for a headless role.
///
/// `request-review` keeps the role inside its rules even when the owner runs Antigravity at
/// always-proceed: headless, an action outside them is refused rather than run.
pub(crate) fn settings_json(repo_root: &str, role: &Role, extra_allow: &[String]) -> String {
    let mut allow = vec![format!("read_file({repo_root})")];
    allow.extend(extra_allow.iter().cloned());
    if role.writes() {
        allow.push(format!("write_file({repo_root})"));
    }
    if role.shell() {
        allow.push(if role.writes() {
            "command(*)".to_owned()
        } else {
            READ_ONLY_SHELL.to_owned()
        });
    }
    if role.claude_tools.is_empty() || role.has_tool("WebSearch") || role.has_tool("WebFetch") {
        allow.push("read_url(*)".to_owned());
    }
    let mut deny = DENIED_COMMANDS
        .iter()
        .map(|rule| (*rule).to_owned())
        .collect::<Vec<_>>();
    deny.push(format!("write_file({repo_root}/.git)"));
    let list = |items: &[String]| {
        items
            .iter()
            .map(|item| quoted(item))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{{\"toolPermission\":\"request-review\",\"permissions\":{{\"allow\":[{}],\"deny\":[{}]}}}}",
        list(&allow),
        list(&deny)
    )
}

pub(crate) struct Captured {
    /// `None` when the process was killed at the timeout.
    pub success: Option<bool>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a command to completion with captured output, killing it after `timeout`.
pub(crate) fn run_captured(mut command: Command, timeout: Duration) -> Result<Captured, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {:?}: {error}", command.get_program()))?;
    let mut stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let mut stderr = child.stderr.take().ok_or("stderr unavailable")?;
    let out_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let success = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break Some(status.success());
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    Ok(Captured {
        success,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

pub(crate) fn find_agy() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            for name in ["agy.exe", "agy"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let fallback = PathBuf::from(home).join(".gemini/bin/agy.exe");
    fallback.is_file().then_some(fallback)
}

pub struct RoleRun<'a> {
    pub name: &'a str,
    pub brief: &'a str,
    pub argon: bool,
    pub dry_run: bool,
}

/// Runs one role headless and prints its answer. Returns the process exit code.
pub fn run_role(repo: &Path, run: &RoleRun<'_>) -> Result<i32, String> {
    let roles = roles(repo)?;
    let Some(role) = roles.iter().find(|role| role.name == run.name) else {
        let known = roles
            .iter()
            .map(|role| role.name.as_str())
            .collect::<Vec<_>>();
        return Err(format!(
            "unknown role `{}`; known: {}",
            run.name,
            known.join(", ")
        ));
    };
    let model = if run.argon {
        ARGON_MODEL
    } else {
        model_for(&role.name).ok_or_else(|| format!("role `{}` has no Gemini model", role.name))?
    };
    let root = std::path::absolute(repo).map_err(|error| error.to_string())?;
    let settings = settings_json(&path_to_string(&root), role, &rtk_rules());
    let agy = find_agy().ok_or("agy (Antigravity CLI) is not installed or not on PATH")?;
    let args = [
        "--agent",
        &role.name,
        "--model",
        model,
        "--settings",
        &settings,
        "--output-format",
        "json",
        "--print-timeout",
        PRINT_TIMEOUT,
        "-p",
        run.brief,
    ];
    if run.dry_run {
        println!("model: {model}");
        println!("settings: {settings}");
        return Ok(0);
    }
    eprintln!("[{} on {model}]", role.name);
    let mut command = Command::new(agy);
    command.args(args).current_dir(&root);
    let captured = run_captured(command, ROLE_TIMEOUT)?;
    if captured.success.is_none() {
        eprintln!(
            "{} did not finish within {} s",
            role.name,
            ROLE_TIMEOUT.as_secs()
        );
        return Ok(1);
    }
    let Captured { stdout, stderr, .. } = captured;
    let Some(result) = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_yaml_ng::from_str::<YamlValue>(line).ok())
    else {
        eprintln!(
            "{}",
            if stdout.trim().is_empty() {
                stderr.trim()
            } else {
                stdout.trim()
            }
        );
        return Ok(1);
    };
    let response = result
        .get("response")
        .and_then(YamlValue::as_str)
        .unwrap_or_default();
    println!("{response}");
    if let Some(YamlValue::Sequence(denied)) = result.get("denied_actions") {
        let names = denied
            .iter()
            .filter_map(|action| {
                action
                    .get("display_name")
                    .or_else(|| action.get("action"))
                    .and_then(YamlValue::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            eprintln!("[refused in headless mode: {}]", names.join(", "));
        }
    }
    let success = result.get("status").and_then(YamlValue::as_str) == Some("SUCCESS");
    if !success || response.trim().is_empty() {
        if !stderr.trim().is_empty() {
            eprintln!("{}", stderr.trim());
        }
        return Ok(1);
    }
    Ok(0)
}

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("md")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn relative(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo)
        .map(path_to_string)
        .unwrap_or_else(|_| path_to_string(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(name: &str, tools: &[&str]) -> Role {
        Role {
            name: name.to_owned(),
            description: "Map one \"path\"".to_owned(),
            claude_tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            body: "\nBody.\n".to_owned(),
        }
    }

    #[test]
    fn quoted_escapes_json_specials() {
        assert_eq!(quoted("a\"b\\c\nd"), r#""a\"b\\c\nd""#);
        assert_eq!(quoted("\u{1}"), r#""\u0001""#);
    }

    #[test]
    fn braces_expand_to_every_option() {
        assert_eq!(
            expand_braces("src/{a,b}/x.rs"),
            ["src/a/x.rs", "src/b/x.rs"]
        );
        assert_eq!(expand_braces("plain/**"), ["plain/**"]);
    }

    #[test]
    fn first_markdown_reference_skips_other_code_spans() {
        let body = "Use `en.json` with `conventions/i18n_CONVENTIONS.md` and `skills/x/SKILL.md`.";
        assert_eq!(
            first_markdown_reference(body),
            Some("conventions/i18n_CONVENTIONS.md")
        );
        assert_eq!(first_markdown_reference("no spans"), None);
    }

    #[test]
    fn role_file_maps_claude_tools_and_keeps_the_body() {
        let text = role_file(&role("explorer", &["Read", "Grep", "Glob", "Bash"])).unwrap();
        assert!(text.starts_with(
            "---\nname: explorer\ndescription: \"Map one \\\"path\\\"\"\nsubagent: false\ntools:\n"
        ));
        assert!(text.contains("  - view_file\n  - grep_search\n  - find_by_name\n  - list_dir\n  - run_command\n---\n\nBody.\n"));
        assert!(role_file(&role("odd", &["Teleport"])).is_err());
    }

    #[test]
    fn read_only_roles_get_read_only_shell_and_writers_get_any_command() {
        let reader = settings_json("C:/repo", &role("reviewer", &["Read", "Bash"]), &[]);
        assert!(reader.contains("\"toolPermission\":\"request-review\""));
        assert!(reader.contains("read_file(C:/repo)"));
        assert!(!reader.contains("write_file(C:/repo)\""));
        assert!(reader.contains("(rtk )?(git"));
        assert!(!reader.contains("command(*)"));
        let writer = settings_json("C:/repo", &role("worker", &["Read", "Bash", "Edit"]), &[]);
        assert!(writer.contains("write_file(C:/repo)\""));
        assert!(writer.contains("command(*)"));
        assert!(writer.contains("write_file(C:/repo/.git)"));
        assert!(writer.contains("cargo (run|prerelease)"));
        let parsed: YamlValue = serde_yaml_ng::from_str(&writer).unwrap();
        assert!(
            parsed
                .get("permissions")
                .and_then(|p| p.get("deny"))
                .is_some()
        );
    }

    #[test]
    fn every_listed_model_names_a_gemini_model_and_level() {
        for (_, model) in ROLE_MODELS {
            assert!(model.starts_with("gemini-"));
            assert!(
                ["-low", "-medium", "-high"]
                    .iter()
                    .any(|level| model.ends_with(level))
            );
        }
    }
}
