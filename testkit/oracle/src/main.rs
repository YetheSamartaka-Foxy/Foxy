use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(about = "Independently verify published repository parts against local payload bytes")]
struct Args {
    #[arg(long, env = "FOXY_TESTKIT_REPOSITORY_PATH")]
    repository_path: PathBuf,
    #[arg(long, env = "FOXY_TESTKIT_REPOSITORY_URL")]
    repository_url: String,
    #[arg(long)]
    structure_only: bool,
    #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u32).range(1..))]
    max_problems: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    #[serde(default)]
    required_mods: Vec<Mod>,
    #[serde(default)]
    optional_mods: Vec<Mod>,
}

#[derive(Deserialize)]
struct Mod {
    #[serde(rename = "modName")]
    name: String,
}

#[derive(Deserialize)]
struct Manifest {
    files: Vec<ManifestFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    path: String,
    length: u64,
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    path: String,
    start: u64,
    length: u64,
    checksum: String,
}

#[derive(Serialize)]
struct Report {
    mode: &'static str,
    repository_path: PathBuf,
    repository_url: String,
    checked_files: u64,
    checked_parts: u64,
    checked_bytes: u64,
    truncated: bool,
    problems: Vec<String>,
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for segment in relative.split(['/', '\\']) {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains(':') {
            bail!("manifest contains an unsafe relative path");
        }
        path.push(segment);
    }
    Ok(path)
}

fn range_digest(file: &mut File, start: u64, length: u64) -> Result<String> {
    file.seek(SeekFrom::Start(start))?;
    let mut remaining = length;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    while remaining > 0 {
        let capacity = remaining.min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..capacity])?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn check_manifest(report: &mut Report, name: &str, manifest: Manifest, max: usize) -> Result<()> {
    let directory = safe_join(&report.repository_path, name)?;
    if !directory.exists() {
        report.problems.push(format!("missing mod folder: {name}"));
        return Ok(());
    }
    for entry in manifest.files {
        if report.problems.len() >= max {
            break;
        }
        let target = safe_join(&directory, &entry.path)?;
        let relative = entry.path.replace('/', "\\");
        let label = format!("{name}\\{relative}");
        if !target.exists() {
            report.problems.push(format!("missing file: {label}"));
            continue;
        }
        let actual = target.metadata()?.len();
        if actual != entry.length {
            report.problems.push(format!(
                "length mismatch: {label} expected {} got {actual}",
                entry.length
            ));
            continue;
        }
        report.checked_files += 1;
        report.checked_bytes += actual;
        if report.mode == "structure" {
            continue;
        }
        let mut file = File::open(target)?;
        for part in entry.parts {
            let digest = range_digest(&mut file, part.start, part.length)?;
            report.checked_parts += 1;
            if !digest.eq_ignore_ascii_case(&part.checksum) {
                report.problems.push(format!(
                    "part mismatch: {label} part '{}' at {}+{} expected {} got {digest}",
                    part.path, part.start, part.length, part.checksum
                ));
                if report.problems.len() >= max {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn run(args: Args) -> Result<Report> {
    let base = format!("{}/", args.repository_url.trim_end_matches('/'));
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let fetch_index = |file| -> Result<Vec<Mod>> {
        let index: Index = client
            .get(format!("{base}{file}"))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(index
            .required_mods
            .into_iter()
            .chain(index.optional_mods)
            .collect())
    };
    let mods = match fetch_index("foxy_addons.json") {
        Ok(mods) if !mods.is_empty() => mods,
        _ => fetch_index("repo.json").context("fetch repository mod list")?,
    };
    let mut report = Report {
        mode: if args.structure_only {
            "structure"
        } else {
            "content"
        },
        repository_path: args.repository_path,
        repository_url: base.clone(),
        checked_files: 0,
        checked_parts: 0,
        checked_bytes: 0,
        truncated: false,
        problems: Vec::new(),
    };
    for item in mods {
        let mut url = reqwest::Url::parse(&base)?;
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("repository URL cannot be a base"))?
            .pop_if_empty()
            .push(&item.name)
            .push("foxy_addon.json");
        let manifest = client.get(url).send()?.error_for_status()?.json()?;
        check_manifest(
            &mut report,
            &item.name,
            manifest,
            args.max_problems as usize,
        )?;
        if report.problems.len() >= args.max_problems as usize {
            break;
        }
    }
    report.truncated = report.problems.len() >= args.max_problems as usize;
    Ok(report)
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize report")
            );
            if report.problems.is_empty() {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("oracle: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            files: vec![ManifestFile {
                path: "file.bin".into(),
                length: 8,
                parts: vec![
                    Part {
                        path: "first".into(),
                        start: 0,
                        length: 3,
                        checksum: blake3::hash(b"abc").to_hex().to_string(),
                    },
                    Part {
                        path: "second".into(),
                        start: 3,
                        length: 5,
                        checksum: blake3::hash(b"defgh").to_hex().to_string(),
                    },
                ],
            }],
        }
    }

    fn verify(root: &Path, mode: &'static str) -> Report {
        let mut report = Report {
            mode,
            repository_path: root.into(),
            repository_url: "http://localhost/".into(),
            checked_files: 0,
            checked_parts: 0,
            checked_bytes: 0,
            truncated: false,
            problems: vec![],
        };
        check_manifest(&mut report, "mod", manifest(), 25).unwrap();
        report
    }

    #[test]
    fn verifies_parts_and_detects_corrupt_truncated_and_missing_payload() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("mod")).unwrap();
        let path = temp.path().join("mod/file.bin");
        std::fs::write(&path, b"abcdefgh").unwrap();
        let good = verify(temp.path(), "content");
        assert!(good.problems.is_empty());
        assert_eq!(
            (good.checked_files, good.checked_parts, good.checked_bytes),
            (1, 2, 8)
        );
        std::fs::write(&path, b"abcdXfgh").unwrap();
        assert!(verify(temp.path(), "content").problems[0].contains("part 'second' at 3+5"));
        assert!(verify(temp.path(), "structure").problems.is_empty());
        std::fs::write(&path, b"abcd").unwrap();
        assert_eq!(
            verify(temp.path(), "content").problems,
            ["length mismatch: mod\\file.bin expected 8 got 4"]
        );
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            verify(temp.path(), "content").problems,
            ["missing file: mod\\file.bin"]
        );
    }

    #[test]
    fn range_extraction_uses_exact_offsets_and_allows_empty_range() {
        let mut file = tempfile::tempfile().unwrap();
        std::io::Write::write_all(&mut file, b"abcdef").unwrap();
        assert_eq!(
            range_digest(&mut file, 2, 3).unwrap(),
            blake3::hash(b"cde").to_hex().to_string()
        );
        assert_eq!(
            range_digest(&mut file, 6, 0).unwrap(),
            blake3::hash(b"").to_hex().to_string()
        );
    }

    #[test]
    fn refuses_manifest_path_escape() {
        for path in ["../payload", "/payload", "C:\\payload", "mod\\..\\payload"] {
            assert!(safe_join(Path::new("root"), path).is_err());
        }
        assert_eq!(
            safe_join(Path::new("root"), "mod/path").unwrap(),
            Path::new("root").join("mod").join("path")
        );
    }
}
