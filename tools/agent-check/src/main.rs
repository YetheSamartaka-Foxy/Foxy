use std::io::Read;
use std::path::PathBuf;

const USAGE: &str = "\
Usage:
  cargo agent-check [--repo <path>]
      Validate repository agent skills, role definitions, imports, workflow links and Antigravity files.
  cargo agent-check sync-antigravity [--repo <path>]
      Regenerate .agents/agents/ and the generated .agents/rules/ files from the Claude Code files.
  cargo agent-check role --list
  cargo agent-check role <name> (<brief> | --brief-file <path> | -) [--argon] [--dry-run]
      Run one role in Google Antigravity on its Gemini model and thinking level, or on Gemini 4 Argon.
  cargo agent-check models [--repo <path>]
      Report whether the pinned role models are still offered and whether newer ones appeared. Read-only.";

fn fail(message: &str) -> ! {
    eprintln!("agent-check: {message}");
    std::process::exit(2);
}

fn main() {
    let mut args = Vec::new();
    let mut repo = None;
    let mut raw = std::env::args().skip(1);
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return;
            }
            "--repo" => {
                let Some(path) = raw.next() else {
                    fail("--repo requires a path");
                };
                repo = Some(PathBuf::from(path));
            }
            _ => args.push(arg),
        }
    }
    let repo = repo.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|error| fail(&format!("cannot read current directory: {error}")))
    });
    match args.first().map(String::as_str) {
        None => validate(&repo),
        Some("sync-antigravity") if args.len() == 1 => {
            match agent_check::antigravity::sync(&repo) {
                Ok((written, removed)) => {
                    println!(
                        "agent-check: Antigravity files synced ({written} written, {removed} removed)"
                    );
                }
                Err(error) => fail(&error),
            }
        }
        Some("role") => role(&repo, &args[1..]),
        Some("models") if args.len() == 1 => {
            std::process::exit(agent_check::models::report(&repo));
        }
        Some(other) => fail(&format!("unknown argument `{other}` (try --help)")),
    }
}

fn validate(repo: &std::path::Path) {
    let diagnostics = agent_check::validate_repo(repo);
    if diagnostics.is_empty() {
        println!("agent-check: repository agent setup is valid");
        return;
    }
    for diagnostic in &diagnostics {
        eprintln!("{}: {}", diagnostic.path, diagnostic.message);
    }
    std::process::exit(1);
}

fn role(repo: &std::path::Path, args: &[String]) {
    let mut name = None;
    let mut brief = None;
    let mut argon = false;
    let mut dry_run = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--list" => {
                let roles =
                    agent_check::antigravity::roles(repo).unwrap_or_else(|error| fail(&error));
                for role in roles {
                    let model = agent_check::antigravity::model_for(&role.name)
                        .unwrap_or("(no Gemini model)");
                    println!("{:24} {model}", role.name);
                }
                return;
            }
            "--argon" => argon = true,
            "--dry-run" => dry_run = true,
            "--brief-file" => {
                let Some(path) = iter.next() else {
                    fail("--brief-file requires a path");
                };
                brief = Some(
                    std::fs::read_to_string(path)
                        .unwrap_or_else(|error| fail(&format!("cannot read {path}: {error}"))),
                );
            }
            "-" if name.is_some() => {
                let mut text = String::new();
                std::io::stdin()
                    .read_to_string(&mut text)
                    .unwrap_or_else(|error| fail(&format!("cannot read stdin: {error}")));
                brief = Some(text);
            }
            other if name.is_none() => name = Some(other.to_owned()),
            other if brief.is_none() => brief = Some(other.to_owned()),
            other => fail(&format!("unexpected argument `{other}` (try --help)")),
        }
    }
    let Some(name) = name else {
        fail("give a role name, or --list");
    };
    let brief = brief.unwrap_or_default();
    if brief.trim().is_empty() {
        fail("give the role a brief: an argument, --brief-file <path> or - for stdin");
    }
    let run = agent_check::antigravity::RoleRun {
        name: &name,
        brief: &brief,
        argon,
        dry_run,
    };
    match agent_check::antigravity::run_role(repo, &run) {
        Ok(code) => std::process::exit(code),
        Err(error) => fail(&error),
    }
}
