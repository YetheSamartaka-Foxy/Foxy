use super::RequestedPaths;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

const MAX_CAPTURE_BYTES: u64 = 32 * 1024 * 1024;

pub(super) struct LogCursor {
    bytes: Vec<u8>,
}

pub(super) struct GenerationProof<'a> {
    pub(super) scenario_observed: bool,
    pub(super) hostname_observed: bool,
    pub(super) version_line: Option<&'a str>,
    pub(super) profile_line: Option<&'a str>,
    pub(super) launch_paths_observed: bool,
    pub(super) online_game_observed: bool,
}

impl GenerationProof<'_> {
    pub(super) fn is_complete(&self) -> bool {
        self.scenario_observed
            && self.version_line.is_some()
            && self.profile_line.is_some()
            && self.launch_paths_observed
            && self.online_game_observed
    }
}

pub(super) fn generation_proof<'a>(
    output: &'a str,
    hostname: &str,
    paths: &RequestedPaths,
) -> GenerationProof<'a> {
    let scenario_observed = output.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        line.contains(super::SCENARIO_WORLD) && lower.contains("entities load")
    });
    let hostname_observed = output.contains(hostname);
    let version_line = output.lines().find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("version ") && lower.contains(" built ")
    });
    let profile_path = super::paths::normalized_path(&paths.effective_profile);
    let profile_line = output.lines().find(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("adding relative directory")
            && lower.contains("under name profile")
            && super::paths::normalized_path_text(line).contains(&profile_path)
    });
    let launch_paths_observed = output.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.contains("cli params:")
            && [
                &paths.config,
                &paths.project,
                &paths.profile_parent,
                &paths.download_parent,
                &paths.logs,
            ]
            .into_iter()
            .all(|path| {
                super::paths::normalized_path_text(line)
                    .contains(&super::paths::normalized_path(path))
            })
            && super::paths::normalized_path_text(line).contains(
                &super::paths::normalized_path_text(&paths.cache_with_separator),
            )
    });
    GenerationProof {
        scenario_observed,
        hostname_observed,
        version_line,
        profile_line,
        launch_paths_observed,
        online_game_observed: output.contains("Entered online game state."),
    }
}

pub(super) fn capture_log_delta(
    root: &Path,
    cursors: &mut BTreeMap<String, LogCursor>,
    output: &mut String,
) -> io::Result<()> {
    let mut stack = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory)? {
            entries += 1;
            if entries > 100_000 {
                return Err(io::Error::other(
                    "Native log inventory exceeded entry limit",
                ));
            }
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !ordinary_entry(&metadata) {
                return Err(io::Error::other(
                    "Native logs contain a link or reparse point",
                ));
            }
            if metadata.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if !metadata.is_file()
                || !entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
                })
            {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            let mut bytes = Vec::new();
            File::open(entry.path())?
                .take(MAX_CAPTURE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_CAPTURE_BYTES {
                return Err(io::Error::other("Native log capture exceeded size limit"));
            }
            let offset = cursors
                .get(&relative)
                .filter(|old| bytes.starts_with(&old.bytes))
                .map_or(0, |old| old.bytes.len());
            if bytes.len() > offset {
                if output.len().saturating_add(bytes.len() - offset) as u64 > MAX_CAPTURE_BYTES {
                    return Err(io::Error::other("Native log capture exceeded size limit"));
                }
                output.push_str(&format!("\n[{relative}]\n"));
                output.push_str(&String::from_utf8_lossy(&bytes[offset..]));
            }
            cursors.insert(relative, LogCursor { bytes });
        }
    }
    Ok(())
}

fn ordinary_entry(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        !metadata.file_type().is_symlink() && metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overwritten_longer_logs_keep_the_new_header_without_prior_generation_bytes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("console.log");
        fs::write(&file, b"old-generation-ready\n").unwrap();
        let mut cursors = BTreeMap::new();
        let mut output = String::new();
        capture_log_delta(root.path(), &mut cursors, &mut output).unwrap();
        output.clear();
        fs::write(&file, b"new-generation-header\nnew-generation-ready\n").unwrap();
        capture_log_delta(root.path(), &mut cursors, &mut output).unwrap();
        assert!(output.contains("new-generation-header"));
        assert!(!output.contains("old-generation"));
        output.clear();
        use std::io::Write;
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"current-tail\n")
            .unwrap();
        capture_log_delta(root.path(), &mut cursors, &mut output).unwrap();
        assert!(output.contains("current-tail"));
        assert!(!output.contains("new-generation-header"));
    }
}
