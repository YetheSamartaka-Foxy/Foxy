use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::mod_line::LaunchParam;
use crate::types::RepoConfig;

/// The launch scripts one config named in `modLineFiles`, with the parameters
/// to write into them. Collected before generation and applied once the
/// repository is published, so a failed build never leaves a launch script
/// pointing at mods that were not written.
#[derive(Debug, Default)]
pub struct PendingUpdate {
    pub files: Vec<PathBuf>,
    pub params: Vec<LaunchParam>,
}

/// The `modLineFiles` paths of a config, resolved from the config file's own
/// directory so they do not depend on the working directory.
pub fn resolve(config: &RepoConfig, config_path: &Path) -> Vec<PathBuf> {
    let dir = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    config
        .mod_line_files
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let path = Path::new(entry);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            }
        })
        .collect()
}

/// Rejects anything that would make the rewrite a silent no-op, so a typo in
/// `modLineFiles` is reported before the repository is generated.
pub fn check(files: &[PathBuf], params: &[LaunchParam]) -> Result<()> {
    for path in files {
        let contents = read(path)?;
        if rewrite(&contents, params).1.iter().sum::<usize>() == 0 {
            bail!(
                "modLineFiles entry {} has no {} parameter to replace",
                path.display(),
                params
                    .iter()
                    .map(|param| param.flag)
                    .collect::<Vec<_>>()
                    .join(" or ")
            );
        }
    }
    Ok(())
}

/// Two repositories rewriting the same launch script would each undo the
/// other's line, so a shared entry is refused instead of picking a winner.
pub fn ensure_distinct<'a>(
    repositories: impl IntoIterator<Item = (&'a str, &'a [PathBuf])>,
) -> Result<()> {
    let mut seen: BTreeMap<PathBuf, &str> = BTreeMap::new();
    for (repository, files) in repositories {
        for path in files {
            let id = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            if let Some(first) = seen.insert(id, repository) {
                bail!(
                    "modLineFiles entry {} is listed by repositories {} and {}",
                    path.display(),
                    first,
                    repository
                );
            }
        }
    }
    Ok(())
}

/// Writes the generated launch parameters into every listed file.
pub fn apply_all(updates: &[PendingUpdate]) -> Result<()> {
    let mut written = Vec::new();
    for update in updates {
        for path in &update.files {
            let contents = read(path)?;
            let (updated, counts) = rewrite(&contents, &update.params);
            for (param, count) in update.params.iter().zip(&counts) {
                if *count == 0 {
                    log::warn!(
                        "{} has no {} parameter; left unchanged",
                        path.display(),
                        param.flag
                    );
                }
            }
            if updated != contents {
                std::fs::write(path, &updated)
                    .with_context(|| format!("Failed to write {}", path.display()))?;
            }
            let replaced: usize = counts.iter().sum();
            println!(
                "Updated mod line in {} ({replaced} replaced)",
                path.display()
            );
            written.push(serde_json::json!({"path": path, "replaced": replaced}));
        }
    }
    if !written.is_empty() {
        crate::output::insert_detail("modLineFiles", serde_json::Value::Array(written));
    }
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    if !path.is_file() {
        bail!(
            "modLineFiles entry does not exist or is not a file: {}",
            path.display()
        );
    }
    std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {} as UTF-8 text", path.display()))
}

/// Replaces the value of every launch parameter occurrence, leaving quoting and
/// the rest of each line untouched. Returns the new text and how often each
/// parameter was replaced.
pub fn rewrite(contents: &str, params: &[LaunchParam]) -> (String, Vec<usize>) {
    let mut text = contents.to_string();
    let mut counts = Vec::with_capacity(params.len());
    for param in params {
        let (next, count) = replace_param(&text, param);
        text = next;
        counts.push(count);
    }
    (text, counts)
}

fn replace_param(text: &str, param: &LaunchParam) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut count = 0;
    while let Some(offset) = text[cursor..].find(param.flag) {
        let start = cursor + offset;
        let after_flag = start + param.flag.len();
        match value_span(text, start, after_flag, param.attached) {
            Some((value_start, value_end)) => {
                out.push_str(&text[cursor..value_start]);
                out.push_str(&param.value);
                cursor = value_end;
                count += 1;
            }
            None => {
                out.push_str(&text[cursor..after_flag]);
                cursor = after_flag;
            }
        }
    }
    out.push_str(&text[cursor..]);
    (out, count)
}

/// The byte range of the value that belongs to the flag found at `start`, or
/// `None` when this is not a standalone occurrence of it (`--mod=`, `-addons`
/// inside `-addonsDir`, a path ending in the flag name).
fn value_span(
    text: &str,
    start: usize,
    after_flag: usize,
    attached: bool,
) -> Option<(usize, usize)> {
    let quote = match text[..start].chars().next_back() {
        None => None,
        Some(c) if c.is_whitespace() || c == '=' => None,
        Some(c @ ('"' | '\'')) => Some(c),
        Some(_) => return None,
    };

    let rest = &text[after_flag..];
    let mut value_start = after_flag;
    if attached {
        if !rest.starts_with('=') {
            return None;
        }
        value_start += 1;
    } else {
        let spaces = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if spaces == 0 {
            return None;
        }
        value_start += spaces;
    }

    let value = &text[value_start..];
    // A quoted value may hold the spaces a bare one cannot, so it ends at its
    // own closing quote rather than at the next separator.
    if let Some(opening) = value.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let inner = &value[opening.len_utf8()..];
        let end = inner.find(opening)?;
        return Some((
            value_start + opening.len_utf8(),
            value_start + opening.len_utf8() + end,
        ));
    }
    let end = value
        .find(|c: char| c.is_whitespace() || Some(c) == quote)
        .unwrap_or(value.len());
    Some((value_start, value_start + end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mod_line::launch_flags;
    use crate::types::RepoGame;

    fn arma3(value: &str) -> Vec<LaunchParam> {
        let mut params = launch_flags(RepoGame::Arma3);
        params[0].value = value.to_string();
        params
    }

    fn reforger(dir: &str, ids: &str) -> Vec<LaunchParam> {
        let mut params = launch_flags(RepoGame::Reforger);
        params[0].value = dir.to_string();
        params[1].value = ids.to_string();
        params
    }

    fn rewritten(contents: &str, params: &[LaunchParam]) -> String {
        rewrite(contents, params).0
    }

    #[test]
    fn only_the_mod_value_of_a_launch_line_changes() {
        let script = "@echo off\r\nstart arma3server_x64.exe -mod=mods/@old;mods/@gone; -config=server.cfg -port 2302\r\n";
        let (updated, counts) = rewrite(script, &arma3("mods/@new;"));
        assert_eq!(counts, vec![1]);
        assert_eq!(
            updated,
            "@echo off\r\nstart arma3server_x64.exe -mod=mods/@new; -config=server.cfg -port 2302\r\n"
        );
    }

    #[test]
    fn quoting_around_the_value_is_preserved() {
        assert_eq!(
            rewritten("./server \"-mod=@a;@b;\" -world=empty", &arma3("@c;")),
            "./server \"-mod=@c;\" -world=empty"
        );
        assert_eq!(
            rewritten("./server -mod=\"@a b;@c;\" -world=empty", &arma3("@d;")),
            "./server -mod=\"@d;\" -world=empty"
        );
        assert_eq!(
            rewritten("PARAMS=\"-mod=@a; -world=empty\"", &arma3("@b;")),
            "PARAMS=\"-mod=@b; -world=empty\""
        );
        assert_eq!(
            rewritten("set MODS=-mod=@a;", &arma3("@b;")),
            "set MODS=-mod=@b;"
        );
    }

    #[test]
    fn every_occurrence_is_updated_and_lookalikes_are_left_alone() {
        let script = "-mod=@a;\n--mod=@a;\n./tools/-mod=@a;\n-modules=@a;\nrun -mod=@a; done\n";
        let (updated, counts) = rewrite(script, &arma3("@b;"));
        assert_eq!(counts, vec![2]);
        assert_eq!(
            updated,
            "-mod=@b;\n--mod=@a;\n./tools/-mod=@a;\n-modules=@a;\nrun -mod=@b; done\n"
        );
    }

    #[test]
    fn reforger_parameters_do_not_swallow_each_other() {
        let script = "./ReforgerServer -addonsDir ./old -addons AAA,BBB -config config.json\n";
        let (updated, counts) = rewrite(script, &reforger("mods", "CCC,DDD"));
        assert_eq!(counts, vec![1, 1]);
        assert_eq!(
            updated,
            "./ReforgerServer -addonsDir mods -addons CCC,DDD -config config.json\n"
        );
    }

    #[test]
    fn missing_parameters_report_instead_of_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("start.sh");
        std::fs::write(&script, "./arma3server -config=server.cfg\n").unwrap();
        let files = vec![script.clone()];
        assert!(check(&files, &launch_flags(RepoGame::Arma3)).is_err());
        assert!(
            check(
                &[dir.path().join("absent.sh")],
                &launch_flags(RepoGame::Arma3)
            )
            .is_err()
        );

        std::fs::write(&script, "./arma3server -mod=@a; -config=server.cfg\n").unwrap();
        check(&files, &launch_flags(RepoGame::Arma3)).unwrap();
        apply_all(&[PendingUpdate {
            files,
            params: arma3("@b;"),
        }])
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&script).unwrap(),
            "./arma3server -mod=@b; -config=server.cfg\n"
        );
    }

    #[test]
    fn a_file_listed_by_two_repositories_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("start.sh");
        std::fs::write(&script, "-mod=@a;\n").unwrap();
        let shared = [script];
        let other = [dir.path().join("other.sh")];
        assert!(
            ensure_distinct([("one", shared.as_slice()), ("two", shared.as_slice())])
                .unwrap_err()
                .to_string()
                .contains("repositories one and two")
        );
        assert!(ensure_distinct([("one", shared.as_slice()), ("two", other.as_slice())]).is_ok());
    }

    #[test]
    fn entries_resolve_from_the_config_directory() {
        let config: RepoConfig = serde_json::from_str(
            r#"{"repoName":"T","basePath":".","modLineFiles":["server/start.sh"," "]}"#,
        )
        .unwrap();
        assert_eq!(
            resolve(&config, Path::new("configs/repo.json")),
            vec![PathBuf::from("configs").join("server").join("start.sh")]
        );
        assert_eq!(
            resolve(&config, Path::new("repo.json")),
            vec![PathBuf::from(".").join("server").join("start.sh")]
        );
    }
}
