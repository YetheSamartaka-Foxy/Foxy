use crate::{Diagnostic, path_to_string, read_text};
use std::path::Path;

pub(super) fn validate(repo: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let output = std::process::Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "**/AGENTS.md",
            "AGENTS.md",
        ])
        .current_dir(repo)
        .output();
    let output = match output {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            diagnostics.push(Diagnostic::new(
                "AGENTS.md",
                format!("git ls-files failed with status {}", output.status),
            ));
            return;
        }
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                "AGENTS.md",
                format!("cannot run git ls-files: {error}"),
            ));
            return;
        }
    };
    let paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| repo.join(String::from_utf8_lossy(bytes).as_ref()))
        .collect::<Vec<_>>();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let Some(relative) = path.strip_prefix(repo).ok().map(path_to_string) else {
            continue;
        };
        let Some(parent) = path.parent() else {
            continue;
        };
        let sibling = parent.join("CLAUDE.md");
        let sibling_rel = path_to_string(
            Path::new(&relative)
                .parent()
                .unwrap_or(Path::new("."))
                .join("CLAUDE.md")
                .as_path(),
        );
        if !sibling.is_file() {
            diagnostics.push(Diagnostic::new(
                sibling_rel,
                format!("missing import for `{relative}`"),
            ));
            continue;
        }
        let Some(text) = read_text(repo, &sibling_rel, diagnostics) else {
            continue;
        };
        if !text.lines().any(|line| line.trim() == "@AGENTS.md") {
            diagnostics.push(Diagnostic::new(
                sibling_rel,
                "must contain an `@AGENTS.md` import",
            ));
        }
    }
}
