use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::{proof::Run, setup::PreparedInstance, transport};

const MAX_LOG_BYTES: u64 = 16 * 1024 * 1024;

pub(super) struct LogBaseline(BTreeMap<String, Vec<u8>>);

#[derive(Serialize)]
pub(super) struct RunEvidence {
    run_generation: u64,
    owner_id: String,
    process_id: u64,
    hostname: String,
    scenario: &'static str,
    engine_version: &'static str,
    updated_engine_logs: Vec<String>,
    evidence_blake3: String,
}

pub(super) fn baseline(instance: &PreparedInstance) -> io::Result<LogBaseline> {
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    let mut pending = vec![instance.engine_log_root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native Reforger log tree contains a link"));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file()
                && entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
                })
            {
                total = total
                    .checked_add(metadata.len())
                    .ok_or_else(|| io::Error::other("Native log baseline size overflow"))?;
                if metadata.len() > MAX_LOG_BYTES || total > MAX_LOG_BYTES {
                    return Err(io::Error::other("Native log baseline exceeded bound"));
                }
                let path = relative_log_path(&entry.path(), &instance.engine_log_root)?;
                files.insert(path, fs::read(entry.path())?);
            } else {
                return Err(io::Error::other(
                    "Native Reforger log tree has a special file",
                ));
            }
        }
    }
    Ok(LogBaseline(files))
}

pub(super) fn wait_for_native_ready(
    instance: &PreparedInstance,
    run: &Run,
    baseline: &LogBaseline,
    timeout: Duration,
) -> io::Result<RunEvidence> {
    let console = run_log(instance, run);
    let deadline = Instant::now() + timeout;
    loop {
        let status = transport::invoke(
            &instance.config_root,
            &["server", "instance", "status", &instance.id],
            0,
        )?;
        let current = &status["data"]["status"];
        if current["process_exists"] != true
            || current["run_generation"].as_u64() != Some(run.generation)
            || current["owner_id"] != run.owner_id
            || current["process_id"].as_u64() != Some(run.process_id)
        {
            return Err(io::Error::other("Could not confirm Reforger process ownership"));
        }
        if let Ok((text, updated_logs)) = collect_run_logs(instance, &console, baseline) {
            if native_log_ready(&text, instance) && !updated_logs.is_empty() {
                return Ok(RunEvidence {
                    run_generation: run.generation,
                    owner_id: run.owner_id.clone(),
                    process_id: run.process_id,
                    hostname: instance.name.clone(),
                    scenario: super::SCENARIO,
                    engine_version: super::ENGINE_VERSION,
                    evidence_blake3: blake3::hash(text.as_bytes()).to_hex().to_string(),
                    updated_engine_logs: updated_logs,
                });
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Owned Reforger process did not report exact native scenario readiness",
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn native_log_ready(text: &str, instance: &PreparedInstance) -> bool {
    let lower = text.to_ascii_lowercase();
    let scenario = super::SCENARIO.to_ascii_lowercase();
    let profile = instance
        .profile_argument_root
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let log = lower.replace('\\', "/");
    log.contains(&instance.name.to_ascii_lowercase())
        && log.contains(&scenario)
        && log.contains(&super::ENGINE_VERSION.to_ascii_lowercase())
        && log.contains(&profile)
}

fn collect_run_logs(
    instance: &PreparedInstance,
    console: &Path,
    baseline: &LogBaseline,
) -> io::Result<(String, Vec<String>)> {
    let mut output = read_bounded(console)?;
    let mut updated_logs = Vec::new();
    let mut pending = vec![instance.engine_log_root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(io::Error::other("Native Reforger log tree contains a link"));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                let path = relative_log_path(&entry.path(), &instance.engine_log_root)?;
                if !entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
                }) {
                    continue;
                }
                if metadata.len() > MAX_LOG_BYTES {
                    return Err(io::Error::other("Native Reforger engine log exceeded bound"));
                }
                let current = fs::read(entry.path())?;
                let previous = baseline.0.get(&path).map(Vec::as_slice).unwrap_or_default();
                if current == previous {
                    continue;
                }
                let fresh = current
                    .strip_prefix(previous)
                    .map_or(current.as_slice(), |suffix| suffix);
                if fresh.is_empty() {
                    continue;
                }
                output.push('\n');
                output.push_str(&String::from_utf8_lossy(fresh));
                updated_logs.push(path);
                if output.len() as u64 > MAX_LOG_BYTES {
                    return Err(io::Error::other("Combined Reforger logs exceeded bound"));
                }
            } else {
                return Err(io::Error::other("Native Reforger log tree has a special file"));
            }
        }
    }
    updated_logs.sort();
    Ok((output, updated_logs))
}

fn relative_log_path(path: &Path, root: &Path) -> io::Result<String> {
    path.strip_prefix(root)
        .map_err(io::Error::other)?
        .to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| io::Error::other("Native log path is not UTF-8"))
}

fn run_log(instance: &PreparedInstance, run: &Run) -> PathBuf {
    instance
        .instance_root
        .join("runtime-state")
        .join(format!("run_{}_{}.log", run.owner_id, run.generation))
}

fn read_bounded(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_LOG_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOG_BYTES {
        return Err(io::Error::other("Reforger run log exceeded bound"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
