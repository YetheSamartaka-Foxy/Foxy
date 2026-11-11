use crate::{
    case::{self, read_json},
    collect::{expect, logs, memory, metrics, profile, sol},
    driver,
    fixture::{self, write_json},
    guards,
    launch::{self, Environment},
    ledger,
    mutate::Mutation,
    references,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::fs::OpenOptions;

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub case: PathBuf,
    pub foxy_binary: Option<PathBuf>,
    pub database_mode: String,
    pub db_write_gate: u32,
    pub accept: bool,
    pub validate_only: bool,
    pub no_build: bool,
    pub progress_probe_ms: u64,
}

fn ensure_run_path(run: &Path, path: &Path) -> Result<PathBuf> {
    let run = fs::canonicalize(run)?;
    ensure!(path.is_absolute(), "Test paths must be absolute");
    ensure!(
        path.components()
            .all(|component| !matches!(component, std::path::Component::ParentDir)),
        "Target cannot contain a parent-directory component"
    );
    let mut ancestor = path;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .context("Test path has no existing parent directory")?;
    }
    ensure!(
        fs::canonicalize(ancestor)?.starts_with(&run),
        "Target must resolve under the case run directory"
    );
    Ok(path.to_owned())
}

fn is_download_sidecar(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    name == ".foxy-write.lock"
        || [".foxy.bak", ".foxy.part", ".foxy.part.meta", ".foxy.tmp"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn move_origin_addon_category(root: &Path, addon_name: &str, from: &str, to: &str) -> Result<()> {
    let repo_path = root.join("repo.json");
    let index_path = root.join("foxy_addons.json");
    let mut repo: Value = serde_json::from_slice(&fs::read(&repo_path)?)?;
    let mut index: Value = serde_json::from_slice(&fs::read(&index_path)?)?;
    let from_key = format!("{from}Mods");
    let to_key = format!("{to}Mods");
    let source = index[&from_key]
        .as_array_mut()
        .with_context(|| format!("{} must be an array", from_key))?;
    let position = source
        .iter()
        .position(|entry| entry["modName"].as_str() == Some(addon_name))
        .with_context(|| format!("Addon {addon_name:?} was not found in {from_key}"))?;
    let entry = source.remove(position);
    let target = index[&to_key]
        .as_array_mut()
        .with_context(|| format!("{} must be an array", to_key))?;
    ensure!(
        !target
            .iter()
            .any(|entry| entry["modName"].as_str() == Some(addon_name)),
        "Addon {addon_name:?} already exists in {to_key}"
    );
    target.push(entry);

    let mut checksum = blake3::Hasher::new();
    for category in ["requiredMods", "optionalMods"] {
        for entry in index[category]
            .as_array()
            .context("repository category is not an array")?
        {
            let value = entry["checkSum"]
                .as_str()
                .context("published addon has no checkSum")?;
            checksum.update(value.as_bytes());
        }
    }
    let checksum = checksum.finalize().to_hex().to_uppercase();
    repo["checksum"] = json!(checksum);
    index["checksum"] = json!(checksum);
    write_json(&repo_path, &repo)?;
    write_json(&index_path, &index)?;
    Ok(())
}

fn file_state_contains(current: &Value, expected: &Value) -> bool {
    match (current.as_object(), expected.as_object()) {
        (Some(current), Some(expected)) => expected
            .iter()
            .all(|(path, state)| current.get(path) == Some(state)),
        _ => false,
    }
}

struct ContextRun<'a> {
    case: &'a Value,
    exe: &'a Path,
    config: &'a Path,
    run: &'a Path,
    env: &'a Environment,
    timeout: Duration,
    mode: &'a str,
    gate: u32,
    progress_probe: Option<Duration>,
    /// The live GUI child, when the case runs on the GUI harness. The `startup`
    /// operation replaces it, so it cannot be owned by `execute` alone.
    gui: &'a RefCell<Option<launch::ManagedChild>>,
    origin: Option<&'a crate::origin::server::Origin>,
    test_file_locks: RefCell<HashMap<PathBuf, File>>,
    /// The pid the memory sampler follows, kept in step with `gui`.
    pid: &'a memory::Target,
}

/// A frame probe that runs beside an operation: `fps` reads at a fixed
/// cadence, so a UI stall under work lands on the operation's own row.
struct UiProbe {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<Vec<Value>>>,
}

impl UiProbe {
    fn start(exe: &Path, config: &Path, env: &Environment, interval: Duration) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (exe, config, env, flag) = (
            exe.to_owned(),
            config.to_owned(),
            env.clone(),
            Arc::clone(&stop),
        );
        let thread = thread::spawn(move || {
            let mut samples = Vec::new();
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let started = Instant::now();
                if let Ok(response) = driver::call(
                    &exe,
                    &config,
                    &["fps".to_owned()],
                    &env,
                    Duration::from_secs(30),
                ) {
                    let mut sample = response.data;
                    sample["rtt_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                    samples.push(sample);
                }
                thread::sleep(interval);
            }
            samples
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Value {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let samples = self
            .thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default();
        ui_probe_summary(&samples)
    }
}

/// Fold the probe reads: the worst frame and worst p95 the app reported,
/// and the round trip of the probe itself (process spawn included, so it is
/// an upper bound on input latency, not a frame time).
fn ui_probe_summary(samples: &[Value]) -> Value {
    let mut rtts: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample["rtt_ms"].as_f64())
        .collect();
    rtts.sort_by(f64::total_cmp);
    let pick = |pct: usize| {
        rtts.get((rtts.len().saturating_sub(1)) * pct / 100)
            .copied()
    };
    let worst = |key: &str| {
        samples
            .iter()
            .filter_map(|sample| sample["frame_ms"][key].as_f64())
            .fold(None, |best: Option<f64>, value| {
                Some(best.map_or(value, |b| b.max(value)))
            })
    };
    let mut fps: Vec<f64> = samples
        .iter()
        .filter_map(|sample| sample["fps"].as_f64())
        .filter(|fps| *fps > 0.0)
        .collect();
    fps.sort_by(f64::total_cmp);
    json!({
        "samples": samples.len(),
        "frame_ms_p95_worst": worst("p95"),
        "frame_ms_max": worst("max"),
        "fps_min": fps.first(),
        "rtt_ms_p50": pick(50),
        "rtt_ms_p95": pick(95),
    })
}

/// Polls the app's hash progress fraction beside an operation, so a run can
/// say whether the bar a user watches tracks the work or races ahead of it.
struct ProgressProbe {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<Vec<(f64, f64)>>>,
}

impl ProgressProbe {
    fn start(exe: &Path, config: &Path, env: &Environment, interval: Duration) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (exe, config, env, flag) = (
            exe.to_owned(),
            config.to_owned(),
            env.clone(),
            Arc::clone(&stop),
        );
        let thread = thread::spawn(move || {
            let started = Instant::now();
            let mut samples = Vec::new();
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(response) = driver::call(
                    &exe,
                    &config,
                    &["progress".to_owned()],
                    &env,
                    Duration::from_secs(30),
                ) && let Some(fraction) = response.data["recheck_hash_progress"].as_f64()
                {
                    samples.push((started.elapsed().as_secs_f64(), fraction));
                }
                thread::sleep(interval);
            }
            samples
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn finish(mut self, elapsed_s: f64) -> Value {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let samples = self
            .thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default();
        progress_probe_summary(&samples, elapsed_s)
    }
}

/// How far the sampled fraction strays from elapsed time over the operation,
/// in percentage points. Linear is the target a user reads the bar against;
/// the fraction at each quarter of elapsed time shows where it runs ahead.
fn progress_probe_summary(samples: &[(f64, f64)], elapsed_s: f64) -> Value {
    let elapsed_s = elapsed_s.max(f64::EPSILON);
    let gaps: Vec<f64> = samples
        .iter()
        .map(|(t, fraction)| (fraction - t / elapsed_s).abs() * 100.0)
        .collect();
    let at = |share: f64| {
        samples
            .iter()
            .min_by(|a, b| {
                (a.0 / elapsed_s - share)
                    .abs()
                    .total_cmp(&(b.0 / elapsed_s - share).abs())
            })
            .map(|(_, fraction)| (fraction * 1000.0).round() / 10.0)
    };
    json!({
        "samples": samples.len(),
        "max_gap_points": gaps.iter().copied().reduce(f64::max).map(|gap| (gap * 10.0).round() / 10.0),
        "mean_gap_points": (!gaps.is_empty())
            .then(|| (gaps.iter().sum::<f64>() / gaps.len() as f64 * 10.0).round() / 10.0),
        "percent_at_quarter": at(0.25),
        "percent_at_half": at(0.5),
        "percent_at_three_quarters": at(0.75),
    })
}

impl ContextRun<'_> {
    fn config_file_path(&self, raw: &str) -> Result<PathBuf> {
        let requested = PathBuf::from(raw);
        ensure!(
            requested.is_absolute(),
            "Config assertion path must be absolute"
        );
        ensure!(
            requested
                .components()
                .all(|component| !matches!(component, std::path::Component::ParentDir)),
            "Config assertion path cannot contain parent-directory components"
        );
        let root = fs::canonicalize(self.config)?;
        let metadata = fs::symlink_metadata(&requested)?;
        ensure!(
            metadata.file_type().is_file(),
            "Config assertion target must be a regular file"
        );
        let canonical = fs::canonicalize(requested)?;
        ensure!(
            canonical.starts_with(root),
            "Config assertion target must stay inside the run config directory"
        );
        Ok(canonical)
    }

    fn assert_config_json_value(&self, operation: &Value) -> Result<bool> {
        let path = self.config_file_path(
            operation["path"]
                .as_str()
                .context("Config JSON assertion requires path")?,
        )?;
        let document = read_json(&path)?;
        let json_path = operation["json_path"]
            .as_str()
            .context("Config JSON assertion requires json_path")?;
        let actual = json_path
            .split('.')
            .try_fold(&document, |value, part| match value {
                Value::Object(object) => object.get(part),
                Value::Array(array) => part
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| array.get(index)),
                _ => None,
            });
        Ok(actual == operation.get("equals"))
    }

    fn second_process_game_use(&self, operation: &Value) -> Result<()> {
        let mut snapshots = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for path in operation["unchanged_paths"]
            .as_array()
            .context("second-process-game-use requires unchanged_paths")?
        {
            let path = self.config_file_path(
                path.as_str()
                    .context("unchanged_paths values must be strings")?,
            )?;
            ensure!(
                seen.insert(path.clone()),
                "unchanged_paths cannot contain duplicates"
            );
            snapshots.push((path.clone(), fs::read(path)?));
        }
        let space_id = operation["space_id"]
            .as_str()
            .context("second-process-game-use requires space_id")?;
        let mut command = Command::new(self.exe);
        command
            .arg("--config-dir")
            .arg(self.config)
            .arg("--json")
            .args(["game", "use", space_id]);
        launch::environment(&mut command, self.env);
        let result = launch::process(
            &mut command,
            Duration::from_secs(operation["wait_timeout_s"].as_u64().unwrap_or(30)),
        )?;
        ensure!(
            result.status.code() == Some(6),
            "Second process exited {:?}; expected DATABASE_BUSY (6)",
            result.status.code()
        );
        for (path, before) in snapshots {
            ensure!(
                fs::read(&path)? == before,
                "Second process changed a protected configuration file"
            );
        }
        Ok(())
    }

    fn repository_path(&self) -> Result<PathBuf> {
        let path = PathBuf::from(
            self.case["repository"]["path"]
                .as_str()
                .context("Missing repository path")?,
        );
        let path = fs::canonicalize(path)?;
        let run = fs::canonicalize(self.run)?;
        ensure!(
            path.starts_with(run),
            "State assertions are restricted to this run's repository directory"
        );
        Ok(path)
    }

    fn state_path(&self, operation: &Value) -> Result<PathBuf> {
        let Some(path) = operation["path"].as_str() else {
            return self.repository_path();
        };
        let path = fs::canonicalize(path)?;
        let run = fs::canonicalize(self.run)?;
        ensure!(
            path.starts_with(run),
            "State assertions are restricted to this run's directories"
        );
        Ok(path)
    }

    fn repository_file_state_at(&self, root: &Path) -> Result<Value> {
        let mut files = serde_json::Map::new();
        let mut paths = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| !is_download_sidecar(entry.path()))
            .map(|entry| entry.path().to_owned())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(path)?;
            files.insert(
                relative,
                json!({"bytes":bytes.len(),"blake3":blake3::hash(&bytes).to_hex().to_string()}),
            );
        }
        Ok(Value::Object(files))
    }

    fn repository_file_state(&self) -> Result<Value> {
        self.repository_file_state_at(&self.repository_path()?)
    }

    fn test_file_lock_path(&self, operation: &Value, require_file: bool) -> Result<PathBuf> {
        let raw_path = operation["path"]
            .as_str()
            .context("Test file lock requires path")?;
        let requested = PathBuf::from(raw_path);
        ensure!(
            requested.is_absolute(),
            "Test file lock path must be absolute"
        );
        ensure!(
            requested
                .components()
                .all(|component| !matches!(component, std::path::Component::ParentDir)),
            "Test file lock path cannot contain parent-directory components"
        );
        let file_name = requested
            .file_name()
            .context("Test file lock path must name a file")?;
        let parent = fs::canonicalize(
            requested
                .parent()
                .context("Test file lock path must have a parent")?,
        )?;
        let run = fs::canonicalize(self.run)?;
        ensure!(
            parent.starts_with(&run),
            "Test file locks are restricted to the case run directory"
        );
        let path = parent.join(file_name);
        let repository = fs::canonicalize(self.repository_path()?)?;
        let config = fs::canonicalize(self.config)?;
        let allow_read = operation["allow_read"].as_bool() == Some(true);
        ensure!(
            path.starts_with(&repository) || (allow_read && path.starts_with(&config)),
            "Test file locks are restricted to the run's repository, or its config when allow_read is enabled"
        );
        if require_file {
            let metadata =
                fs::symlink_metadata(&path).context("Test file lock target must exist")?;
            ensure!(
                metadata.file_type().is_file(),
                "Test file lock target must be a regular file, not a link"
            );
        }
        Ok(path)
    }

    fn acquire_test_file_lock(&self, operation: &Value) -> Result<()> {
        let path = self.test_file_lock_path(operation, true)?;
        let target = fs::canonicalize(&path)?;
        let repository = fs::canonicalize(self.repository_path()?)?;
        let config = fs::canonicalize(self.config)?;
        let allow_read = operation["allow_read"].as_bool() == Some(true);
        ensure!(
            target.starts_with(&repository) || (allow_read && target.starts_with(&config)),
            "Test file locks cannot follow links outside the run's repository or allowed config"
        );
        ensure!(
            !self.test_file_locks.borrow().contains_key(&path),
            "Test file is already locked"
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let file = OpenOptions::new()
                .read(true)
                .share_mode(if allow_read { 1 } else { 0 })
                .open(&target)
                .context("Acquiring test file lock")?;
            self.test_file_locks.borrow_mut().insert(path, file);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (path, allow_read);
            bail!("Test file locks are available only on Windows")
        }
    }

    fn release_test_file_lock(&self, operation: &Value) -> Result<()> {
        let path = self.test_file_lock_path(operation, false)?;
        ensure!(
            self.test_file_locks.borrow_mut().remove(&path).is_some(),
            "Test file is not locked"
        );
        Ok(())
    }

    fn state_operation(&self, operation: &Value) -> Result<bool> {
        let Some(name) = operation["op"].as_str() else {
            return Ok(false);
        };
        match name {
            "ui-set-setting" => {
                let key = operation["key"]
                    .as_str()
                    .context("ui-set-setting requires key")?;
                let value = operation["value"]
                    .as_str()
                    .context("ui-set-setting requires value")?;
                driver::call(
                    self.exe,
                    self.config,
                    &["set-setting".into(), key.into(), value.into()],
                    self.env,
                    Duration::from_secs(30),
                )?;
                Ok(true)
            }
            "second-process-game-use" => {
                self.second_process_game_use(operation)?;
                Ok(true)
            }
            "assert-config-json" => {
                ensure!(
                    self.assert_config_json_value(operation)?,
                    "Config JSON value did not match the expected value"
                );
                Ok(true)
            }
            "wait-for-config-json" => {
                let timeout =
                    Duration::from_secs(operation["wait_timeout_s"].as_u64().unwrap_or(30));
                let deadline = Instant::now() + timeout;
                loop {
                    if self.assert_config_json_value(operation)? {
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Timed out waiting for a saved config JSON value"
                    );
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(true)
            }
            "cli-command" => {
                self.run_cli_scenario(operation)?;
                Ok(true)
            }
            "replace-test-file-with-directory" => {
                let path = operation["path"]
                    .as_str()
                    .context("replace-test-file-with-directory requires path")?;
                let path = ensure_run_path(self.run, &PathBuf::from(path))?;
                ensure!(
                    path.is_file(),
                    "replace-test-file-with-directory requires an existing regular file"
                );
                fs::remove_file(&path)?;
                fs::create_dir(&path)?;
                Ok(true)
            }
            "move-origin-addon-category" => {
                let root = operation["root"]
                    .as_str()
                    .context("move-origin-addon-category requires root")?;
                let root = ensure_run_path(self.run, &PathBuf::from(root))?;
                ensure!(root.is_dir(), "Origin category root must be a directory");
                move_origin_addon_category(
                    &root,
                    operation["addon"]
                        .as_str()
                        .context("move-origin-addon-category requires addon")?,
                    operation["from"]
                        .as_str()
                        .context("move-origin-addon-category requires from")?,
                    operation["to"]
                        .as_str()
                        .context("move-origin-addon-category requires to")?,
                )?;
                Ok(true)
            }
            "acquire-test-file-lock" => {
                self.acquire_test_file_lock(operation)?;
                Ok(true)
            }
            "release-test-file-lock" => {
                self.release_test_file_lock(operation)?;
                Ok(true)
            }
            "capture-file-state" => {
                let label = operation["label"]
                    .as_str()
                    .context("capture-file-state requires label")?;
                ensure!(
                    label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "File-state labels must be ASCII letters, digits, or hyphens"
                );
                write_json(
                    &self.run.join(format!("file-state-{label}.json")),
                    &self.repository_file_state_at(&self.state_path(operation)?)?,
                )?;
                Ok(true)
            }
            "assert-file-state" => {
                let label = operation["label"]
                    .as_str()
                    .context("assert-file-state requires label")?;
                let expected = read_json(&self.run.join(format!("file-state-{label}.json")))?;
                let current = self.repository_file_state_at(&self.state_path(operation)?)?;
                match operation["comparison"].as_str().unwrap_or("equal") {
                    "equal" => ensure!(
                        current == expected,
                        "Repository files differ from captured state '{label}'"
                    ),
                    "different" => ensure!(
                        current != expected,
                        "Repository files unexpectedly match captured state '{label}'"
                    ),
                    "contains" => ensure!(
                        file_state_contains(&current, &expected),
                        "Repository files no longer contain captured state '{label}'"
                    ),
                    other => anyhow::bail!(
                        "Unknown file-state comparison '{other}'; use equal, different, or contains"
                    ),
                }
                Ok(true)
            }
            "assert-download-paused" => {
                let expected = operation["paused"]
                    .as_bool()
                    .context("assert-download-paused requires paused")?;
                let progress = self.data(&["progress"])?;
                ensure!(
                    progress["download_paused"].as_bool() == Some(expected),
                    "Expected download_paused={expected}, got {}",
                    progress["download_paused"]
                );
                Ok(true)
            }
            "assert-repository-paused" => {
                let index = operation["repository_index"]
                    .as_u64()
                    .context("assert-repository-paused requires repository_index")?;
                let expected = operation["paused"]
                    .as_bool()
                    .context("assert-repository-paused requires paused")?;
                let progress = self.data(&["progress"])?;
                let paused = progress["paused_repositories"]
                    .as_array()
                    .is_some_and(|repos| {
                        repos
                            .iter()
                            .any(|repo| repo["repo_index"].as_u64() == Some(index))
                    });
                ensure!(
                    paused == expected,
                    "Expected repository index {index} paused={expected}"
                );
                Ok(true)
            }
            "assert-space-bulk-target-count" => {
                let expected = operation["equals"]
                    .as_u64()
                    .context("assert-space-bulk-target-count requires equals")?;
                let progress = self.data(&["progress"])?;
                ensure!(
                    progress["repository_space_bulk_progress"]["total_count"].as_u64()
                        == Some(expected),
                    "Expected space bulk target count {expected}, got {}",
                    progress["repository_space_bulk_progress"]["total_count"]
                );
                Ok(true)
            }
            "assert-space-bulk-completed-count" | "assert-space-bulk-failed-count" => {
                let field = if name == "assert-space-bulk-failed-count" {
                    "failed_count"
                } else {
                    "completed_count"
                };
                let expected = operation["equals"]
                    .as_u64()
                    .context("Space bulk count assertion requires equals")?;
                let progress = self.data(&["progress"])?;
                ensure!(
                    progress["repository_space_bulk_progress"][field].as_u64() == Some(expected),
                    "Expected space bulk {field} {expected}, got {}",
                    progress["repository_space_bulk_progress"][field]
                );
                Ok(true)
            }
            "wait-for-space-bulk-target-count" => {
                let expected = operation["equals"]
                    .as_u64()
                    .context("wait-for-space-bulk-target-count requires equals")?;
                let deadline = Instant::now()
                    + Duration::from_secs(operation["timeout_s"].as_u64().unwrap_or(120));
                loop {
                    let progress = self.data(&["progress"])?;
                    if progress["repository_space_bulk_progress"]["total_count"].as_u64()
                        == Some(expected)
                    {
                        return Ok(true);
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Space bulk target count did not reach {expected}"
                    );
                    thread::sleep(Duration::from_millis(50));
                }
            }
            "seed-startup-state" => {
                self.seed_startup_state(operation)?;
                Ok(true)
            }
            "origin-switch" => {
                let root = operation["root"]
                    .as_str()
                    .context("origin-switch requires a root directory")?;
                self.origin
                    .context("origin-switch requires a case origin")?
                    .set_root(Path::new(root))?;
                crate::origin::server::touch_manifests(Path::new(root))?;
                Ok(true)
            }
            "prepare-test-directory" => {
                let path = operation["path"]
                    .as_str()
                    .context("prepare-test-directory requires path")?;
                let path = ensure_run_path(self.run, &PathBuf::from(path))?;
                fs::create_dir_all(&path)?;
                ensure_run_path(self.run, &path)?;
                Ok(true)
            }
            "write-file" => {
                let path = operation["path"]
                    .as_str()
                    .context("write-file requires path")?;
                let path = ensure_run_path(self.run, &PathBuf::from(path))?;
                let content = operation["content"]
                    .as_str()
                    .context("write-file requires string content")?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                    ensure_run_path(self.run, parent)?;
                }
                use std::fs::OpenOptions;
                use std::io::Write;
                let overwrite = operation["overwrite_existing"].as_bool().unwrap_or(false);
                if overwrite {
                    ensure!(
                        fs::symlink_metadata(&path)?.file_type().is_file(),
                        "write-file overwrite requires an existing regular file"
                    );
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(!overwrite)
                    .truncate(overwrite)
                    .open(&path)?;
                file.write_all(content.as_bytes())?;
                Ok(true)
            }
            "assert-file-content" => {
                let path = operation["path"]
                    .as_str()
                    .context("assert-file-content requires path")?;
                let path = ensure_run_path(self.run, &PathBuf::from(path))?;
                ensure!(
                    fs::symlink_metadata(&path)?.file_type().is_file(),
                    "Content assertion requires a regular file"
                );
                let expected = operation["content"]
                    .as_str()
                    .context("assert-file-content requires content")?;
                ensure!(
                    fs::read(&path)? == expected.as_bytes(),
                    "File contents differ from the expected value"
                );
                Ok(true)
            }
            "assert-file-exists" | "assert-file-absent" => {
                let path = operation["path"]
                    .as_str()
                    .context("File existence assertion requires path")?;
                let path = ensure_run_path(self.run, &PathBuf::from(path))?;
                let expected = operation["op"].as_str() == Some("assert-file-exists");
                ensure!(
                    path.is_file() == expected,
                    "Expected file {} to {}",
                    path.display(),
                    if expected { "exist" } else { "be absent" }
                );
                Ok(true)
            }
            "assert-pending-update-count" => {
                let repository = self.operation_repository(operation)?;
                let minimum = operation["minimum"]
                    .as_u64()
                    .context("assert-pending-update-count requires minimum")?;
                let pending = self.data(&["pending-updates"])?;
                let pending_repo = pending["repositories"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|entry| entry["repository_name"].as_str() == Some(repository))
                    .cloned();
                let count = pending_repo
                    .as_ref()
                    .and_then(|entry| entry["needs_update_count"].as_u64())
                    .unwrap_or(0);
                let maximum = operation["maximum"].as_u64();
                ensure!(
                    count >= minimum,
                    "Repository {repository:?} has {count} pending addon updates, expected at least {minimum}"
                );
                if let Some(maximum) = maximum {
                    ensure!(
                        count <= maximum,
                        "Repository {repository:?} has {count} pending addon updates, expected at most {maximum}"
                    );
                }
                for addon in operation["addons"].as_array().into_iter().flatten() {
                    let addon = addon.as_str().context("Addon names must be strings")?;
                    ensure!(
                        pending_repo.as_ref().is_some_and(|entry| {
                            entry["mods"].as_array().is_some_and(|mods| {
                                mods.iter().any(|item| {
                                    item["name"].as_str() == Some(addon)
                                        && item["needs_update"].as_bool() == Some(true)
                                })
                            })
                        }),
                        "Repository {repository:?} does not have pending update for {addon:?}"
                    );
                }
                Ok(true)
            }
            "capture-origin-requests" => {
                let label = operation["label"]
                    .as_str()
                    .context("capture-origin-requests requires label")?;
                let origin = self.origin.context("Case has no loopback origin")?;
                write_json(
                    &self.run.join(format!("origin-requests-{label}.json")),
                    &json!({"requests": origin.request_count()}),
                )?;
                Ok(true)
            }
            "assert-origin-requests-unchanged" => {
                let label = operation["label"]
                    .as_str()
                    .context("assert-origin-requests-unchanged requires label")?;
                let origin = self.origin.context("Case has no loopback origin")?;
                let captured = read_json(&self.run.join(format!("origin-requests-{label}.json")))?;
                ensure!(
                    origin.request_count() == captured["requests"].as_u64().unwrap_or(u64::MAX),
                    "Loopback origin received a request during the offline operation"
                );
                Ok(true)
            }
            "invoke-driver" => {
                let action = operation["action"]
                    .as_str()
                    .context("invoke-driver requires action")?;
                let params = operation
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let params = serde_json::to_string(&params)?;
                let mut args = vec!["invoke", action, "--params", &params];
                if operation["allow_destructive"].as_bool() == Some(true) {
                    args.push("--allow-destructive");
                }
                self.data(&args)?;
                Ok(true)
            }
            "wait-for-busy" => {
                let reason = operation["reason"]
                    .as_str()
                    .context("wait-for-busy requires reason")?;
                let expected_active = operation["cleared"].as_bool() != Some(true);
                let deadline = Instant::now()
                    + Duration::from_secs(operation["timeout_s"].as_u64().unwrap_or(120));
                loop {
                    let progress = self.data(&["progress"])?;
                    let active = progress["busy_reasons"]
                        .as_array()
                        .is_some_and(|reasons| reasons.iter().any(|entry| entry == reason));
                    if active == expected_active {
                        return Ok(true);
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "Timed out waiting for busy reason '{reason}' to {}",
                        if expected_active { "start" } else { "clear" }
                    );
                    thread::sleep(Duration::from_millis(100));
                }
            }
            _ => Ok(false),
        }
    }
    fn run_cli_scenario(&self, operation: &Value) -> Result<()> {
        let args = operation["args"]
            .as_array()
            .context("cli-command requires args")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .context("cli-command args must be strings")
            })
            .collect::<Result<Vec<_>>>()?;
        let path_arguments = operation["path_arguments"]
            .as_array()
            .context("cli-command requires path_arguments")?;
        for index in path_arguments {
            let index = index
                .as_u64()
                .context("path argument indexes must be integers")?
                as usize;
            let path = args
                .get(index)
                .context("path argument index is out of bounds")?;
            let path = PathBuf::from(path);
            ensure_run_path(self.run, &path)?;
        }
        if args
            .get(0..3)
            .is_some_and(|prefix| prefix == ["config", "extra-file", "activate"])
        {
            for space in fs::read_dir(self.config.join("games"))? {
                let space = space?.path();
                let store = space.join("extra_files.json");
                if !store.is_file() {
                    continue;
                }
                let store: Value = serde_json::from_slice(&fs::read(store)?)?;
                for entry in store["entries"]
                    .as_array()
                    .context("Extra-file scenario requires entries")?
                {
                    let destination = entry["destination"]
                        .as_str()
                        .context("Extra-file destination missing")?;
                    ensure_run_path(self.run, Path::new(destination))?;
                    let id = entry["id"].as_str().context("Extra-file id missing")?;
                    let source = entry["source_name"]
                        .as_str()
                        .context("Extra-file source missing")?;
                    ensure_run_path(self.run, &space.join("extra_files").join(id).join(source))?;
                }
            }
        }
        let mut command = Command::new(self.exe);
        command
            .arg("--config-dir")
            .arg(self.config)
            .arg("--json")
            .args(&args);
        launch::environment(&mut command, self.env);
        let result = launch::tracked_process(
            &mut command,
            Duration::from_secs(
                operation["wait_timeout_s"]
                    .as_u64()
                    .unwrap_or(self.timeout.as_secs()),
            ),
            Some(self.pid),
        )?;
        memory::set(self.pid, None);
        let expected_exit = operation["expected_exit_code"]
            .as_u64()
            .context("cli-command requires expected_exit_code")? as i32;
        ensure!(
            result.status.code() == Some(expected_exit),
            "cli-command exited {:?}, expected {}: {} {}",
            result.status.code(),
            expected_exit,
            String::from_utf8_lossy(&result.stderr),
            String::from_utf8_lossy(&result.stdout)
        );
        let response: Value =
            serde_json::from_slice(&result.stdout).context("cli-command returned invalid JSON")?;
        ensure!(
            response.is_object(),
            "cli-command JSON response must be an object"
        );
        for assertion in operation["expect"].as_array().into_iter().flatten() {
            let path = assertion["path"]
                .as_str()
                .context("cli-command expect requires path")?;
            let expected = assertion
                .get("equals")
                .context("cli-command expect requires equals")?;
            let actual = path
                .split('.')
                .try_fold(&response, |value, part| match value {
                    Value::Object(object) => object.get(part),
                    Value::Array(array) => part
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| array.get(index)),
                    _ => None,
                })
                .with_context(|| format!("cli-command JSON path {path:?} was not present"))?;
            ensure!(
                actual == expected,
                "cli-command JSON assertion failed at {path}: expected {expected}, got {actual}"
            );
        }
        let label = operation["label"].as_str().unwrap_or("cli-command");
        ensure!(
            label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "cli-command labels must be ASCII letters, digits, or hyphens"
        );
        write_json(
            &self.run.join(format!("cli-scenario-{label}.json")),
            &json!({"exit_code": expected_exit, "response": response}),
        )?;
        Ok(())
    }

    fn wait_for_promoted_file(&self, before: &Value, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let current = match self.repository_file_state() {
                Ok(current) => current,
                Err(error)
                    if error
                        .root_cause()
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    ensure!(
                        Instant::now() < deadline,
                        "No previously published file was promoted before the timeout"
                    );
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(files) = before.as_object() {
                for (path, previous) in files {
                    if current.get(path).is_some_and(|now| now != previous) {
                        return Ok(path.clone());
                    }
                }
            }
            ensure!(
                Instant::now() < deadline,
                "No previously published file was promoted before the timeout"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn close_gui_and_restart(&self, timeout: Duration) -> Result<u64> {
        self.data(&["close"])?;
        let started = Instant::now();
        let deadline = started + timeout;
        loop {
            let exited = self
                .gui
                .borrow_mut()
                .as_mut()
                .context("The GUI process is not running")?
                .child
                .try_wait()?
                .is_some();
            if exited {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "Foxy did not finish its normal close path before the timeout"
            );
            thread::sleep(Duration::from_millis(25));
        }
        memory::set(self.pid, None);
        drop(self.gui.borrow_mut().take());
        let child =
            launch::start_gui(self.exe, self.config, self.run, self.env, timeout, self.pid)?;
        *self.gui.borrow_mut() = Some(child);
        self.driver(
            &[
                "wait".into(),
                "--busy-reason-cleared".into(),
                "startup-sync".into(),
                "--timeout-ms".into(),
                timeout.as_millis().to_string(),
            ],
            timeout + Duration::from_secs(10),
        )?;
        Ok(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
    }
    fn driver(&self, args: &[String], timeout: Duration) -> Result<driver::Response> {
        driver::call(self.exe, self.config, args, self.env, timeout)
    }
    fn data(&self, args: &[&str]) -> Result<Value> {
        Ok(self
            .driver(
                &args.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
                Duration::from_secs(60),
            )?
            .data)
    }
    /// Run one operation with the memory sampler bracketing it.
    ///
    /// Every operation is sampled, not only the ones in a memory case: the
    /// counters cost a handle open per tick, and a footprint regression that
    /// only shows up under a download is exactly the one no dedicated case
    /// would have caught.
    fn seed_startup_state(&self, operation: &Value) -> Result<()> {
        if let Some(gui) = self.gui.borrow_mut().as_mut() {
            launch::stop_gui(gui, self.exe, self.config, self.env);
        }
        memory::set(self.pid, None);
        drop(self.gui.borrow_mut().take());
        let address = self.case["repository"]["address"]
            .as_str()
            .context("seed-startup-state requires the case repository address")?;
        let path = self.case["repository"]["path"]
            .as_str()
            .context("seed-startup-state requires the case repository path")?;
        crate::seed_state::apply(
            self.config,
            address,
            path,
            operation["stale_link"].as_bool().unwrap_or(false),
            operation["incomplete_parts"].as_bool().unwrap_or(false),
        )
    }

    fn operation(&self, operation: &Value) -> Result<Value> {
        let settle = operation["memory_settle_ms"]
            .as_u64()
            .map_or(memory::SETTLE, Duration::from_millis);
        let watch = memory::Watch::start(self.pid, memory::INTERVAL, settle);
        let mut collected = self.dispatch(operation)?;
        collected["summary"]["memory"] = watch.finish();
        Ok(collected)
    }
    fn dispatch(&self, operation: &Value) -> Result<Value> {
        match operation["op"].as_str() {
            Some("startup") => return self.startup_operation(operation),
            Some("ui-walk") => return self.ui_walk_operation(operation),
            Some("switch-game-space") => return self.switch_space_operation(operation),
            _ => {}
        }
        if self.case["harness"].as_str().unwrap_or("gui") == "cli" {
            self.cli_operation(operation)
        } else {
            self.gui_operation(operation)
        }
    }

    /// Restart the app and measure launch to sync verdict (O8).
    ///
    /// The app brackets its own timeline with the `startup-sync` busy reason and
    /// publishes it as one `SOL op=startup` line, so the runner only has to
    /// restart the process, wait for that flag to clear, and slice the log.
    fn startup_operation(&self, operation: &Value) -> Result<Value> {
        ensure!(
            self.case["harness"].as_str().unwrap_or("gui") == "gui",
            "The startup operation requires the gui harness"
        );
        // A measured startup must begin from a stopped app, and the previous
        // instance has to release the game-space database lock first.
        if let Some(gui) = self.gui.borrow_mut().as_mut() {
            launch::stop_gui(gui, self.exe, self.config, self.env);
        }
        // Clear the sampler target before the handle goes away: Windows reuses
        // pids, and a stale one would attribute another process's footprint to
        // this run.
        memory::set(self.pid, None);
        drop(self.gui.borrow_mut().take());
        let offsets = logs::offsets(self.run, self.config)?;
        let wait = operation["wait_timeout_s"]
            .as_u64()
            .unwrap_or(self.timeout.as_secs());
        let started = Instant::now();
        let child = launch::start_gui(
            self.exe,
            self.config,
            self.run,
            self.env,
            Duration::from_secs(wait),
            self.pid,
        )?;
        let driver_ready = started.elapsed();
        *self.gui.borrow_mut() = Some(child);
        self.driver(
            &[
                "wait".into(),
                "--busy-reason-cleared".into(),
                "startup-sync".into(),
                "--timeout-ms".into(),
                (wait * 1000).to_string(),
            ],
            Duration::from_secs(wait + 10),
        )?;
        let elapsed = started.elapsed().as_secs_f64();
        let progress = self.data(&["progress"])?;
        let snapshot = self.data(&["snapshot"])?;
        let log = logs::delta(&offsets, self.run, self.config)?;
        let profile = launch::assert_database_mode(&log, self.mode, self.gate)?;
        let record = sol::operation(&sol::parse(&log), "startup");
        ensure!(
            !record.is_null(),
            "Startup operation produced no `SOL op=startup` line; the app did not reach a sync verdict"
        );
        let summary = json!({
            "total_ms": (elapsed * 1000.0).round(),
            "driver_ready_ms": (driver_ready.as_secs_f64() * 1000.0).round(),
            "startup_total_ms": record["actual_s"].as_f64().map(|v| (v * 1000.0).round()),
            "first_frame_ms": record["first_frame_s"].as_f64().map(|v| (v * 1000.0).round()),
            "dispatch_ms": record["dispatch_s"].as_f64().map(|v| (v * 1000.0).round()),
            "eligibility_ms": record["eligibility_s"].as_f64().map(|v| (v * 1000.0).round()),
            "verdict_ms": record["verdict_s"].as_f64().map(|v| (v * 1000.0).round()),
            "dependency_bound_ms": record["dependency_bound_s"].as_f64().map(|v| (v * 1000.0).round()),
            "join_ms": record["join_s"].as_f64().map(|v| (v * 1000.0).round()),
            "dependency_coverage_percent": record["dependency_coverage_percent"],
            "repos": record["repos"],
            "quick_scan_repos": record["quick_scan_repos"],
            "eligible": record["eligible"],
            "prevalidated": record["prevalidated"],
            "remote_changed": record["remote_changed"],
            "rechecks": record["rechecks"],
        });
        Ok(
            json!({"elapsed_s":elapsed,"summary":summary,"progress":progress,"snapshot":snapshot,"logs":null,"log_text":log,"database_profile":profile}),
        )
    }
    /// Drive a scripted click-through and measure what the UI holds afterwards.
    ///
    /// A UX case answers "did the scenario pass"; this answers "what did walking
    /// the app cost", which is a perf question and belongs on a perf row. The
    /// steps are the same driver commands a UX case uses, so a walk can be
    /// lifted straight out of one.
    fn ui_walk_operation(&self, operation: &Value) -> Result<Value> {
        ensure!(
            self.case["harness"].as_str().unwrap_or("gui") == "gui",
            "The ui-walk operation requires the gui harness"
        );
        let steps = operation["steps"]
            .as_array()
            .context("A ui-walk operation needs a steps array")?;
        ensure!(
            !steps.is_empty(),
            "A ui-walk operation needs at least one step"
        );
        let offsets = logs::offsets(self.run, self.config)?;
        let path = self.run.join("ui-walk-steps.json");
        write_json(&path, &Value::Array(steps.clone()))?;
        let started = Instant::now();
        let response = launch::foxy(
            self.exe,
            self.config,
            &[
                "agent-gui".into(),
                "scenario".into(),
                path.to_string_lossy().into_owned(),
            ],
            self.env,
            Duration::from_secs(
                operation["wait_timeout_s"]
                    .as_u64()
                    .unwrap_or(self.timeout.as_secs()),
            ),
        )?;
        let elapsed = started.elapsed().as_secs_f64();
        let data = &response["data"];
        // Written before the checks below: a failed walk is exactly when the
        // transcript is worth having.
        write_json(&self.run.join("ui-walk-transcript.json"), data)?;
        ensure!(
            data["ok"].as_bool().unwrap_or(false),
            "ui-walk scenario failed; see ui-walk-transcript.json"
        );
        let executed = data["steps"].as_array().map_or(0, Vec::len);
        ensure!(
            executed == steps.len(),
            "ui-walk executed {executed} of {} steps",
            steps.len()
        );
        // No `database_profile`: a walk never restarts the app, so its log slice
        // carries no startup mode line to confirm against.
        let log = logs::delta(&offsets, self.run, self.config)?;
        let progress = self.data(&["progress"])?;
        let snapshot = self.data(&["snapshot"])?;
        let summary = json!({
            "total_ms": (elapsed * 1000.0).round(),
            "steps": executed,
        });
        Ok(
            json!({"elapsed_s":elapsed,"summary":summary,"progress":progress,"snapshot":snapshot,"logs":null,"log_text":log,"database_profile":null}),
        )
    }
    /// Switch the active game space at runtime and measure request to the new
    /// space's repository list settled; the app's `SOL op=space_switch` line
    /// carries the drain/reset/reload split.
    fn switch_space_operation(&self, operation: &Value) -> Result<Value> {
        ensure!(
            self.case["harness"].as_str().unwrap_or("gui") == "gui",
            "The switch-game-space operation requires the gui harness"
        );
        let target = operation["game_space"]
            .as_str()
            .context("A switch-game-space operation needs a game_space id")?;
        let generation = self.data(&["logs", "--limit", "1"])?["generation"]
            .as_u64()
            .context("Missing log generation")?;
        let offsets = logs::offsets(self.run, self.config)?;
        let params = json!({"game-space": target}).to_string();
        let started = Instant::now();
        self.data(&[
            "invoke",
            "switch-game-space",
            "--params",
            &params,
            "--allow-destructive",
        ])?;
        let deadline = Instant::now()
            + Duration::from_secs(operation["wait_timeout_s"].as_u64().unwrap_or(120));
        let mut switched = false;
        while Instant::now() < deadline {
            if self.logged_since(generation, "SOL op=space_switch")? {
                switched = true;
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        ensure!(switched, "The game space switch never reported its record");
        // The new space's startup work (quick scans, rechecks) settles like a
        // startup does; wait for it so the next operation measures a quiet app.
        let wait = operation["settle_timeout_s"].as_u64().unwrap_or(120);
        let _ = self.driver(
            &[
                "wait".into(),
                "--busy-reason-cleared".into(),
                "startup-sync".into(),
                "--timeout-ms".into(),
                (wait * 1000).to_string(),
            ],
            Duration::from_secs(wait + 10),
        );
        let elapsed = started.elapsed().as_secs_f64();
        let progress = self.data(&["progress"])?;
        let snapshot = self.data(&["snapshot"])?;
        let captured = self.data(&[
            "logs",
            "--since-generation",
            &generation.to_string(),
            "--limit",
            "2000",
        ])?;
        let messages = captured["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["message"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let log = format!(
            "{}\n{messages}",
            logs::delta(&offsets, self.run, self.config)?
        );
        let summary = json!({"total_ms": (elapsed * 1000.0).round(), "game_space": target});
        Ok(
            json!({"elapsed_s":elapsed,"summary":summary,"progress":progress,"snapshot":snapshot,"logs":captured,"log_text":log,"database_profile":null}),
        )
    }
    /// The repository an operation acts on: `operation.repository` names one of
    /// the fixture's repositories (a sibling in a shared folder, for example),
    /// otherwise it is the case repository.
    fn operation_repository<'o>(&'o self, operation: &'o Value) -> Result<&'o str> {
        if let Some(name) = operation["repository"].as_str() {
            return Ok(name);
        }
        self.case["repository"]["name"]
            .as_str()
            .context("Missing repository name")
    }
    fn gui_repository_index(&self, operation: &Value) -> Result<usize> {
        let name = self.operation_repository(operation)?;
        let listed = self.data(&["repositories"])?;
        listed["repositories"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["name"].as_str() == Some(name))
            .and_then(|row| row["index"].as_u64())
            .map(|index| index as usize)
            .with_context(|| format!("Repository {name:?} is not listed by the running app"))
    }
    fn cli_operation(&self, operation: &Value) -> Result<Value> {
        let name = operation["op"].as_str().context("Missing operation name")?;
        let repository = self.operation_repository(operation)?;
        let args = match name {
            "wipe-db" if operation["keep_hash_record"].as_bool() == Some(true) => vec![
                "repo",
                "wipe-db",
                "--repo-name",
                repository,
                "--yes",
                "--keep-hash-record",
            ],
            "wipe-db" => vec!["repo", "wipe-db", "--repo-name", repository, "--yes"],
            "force-redownload" => vec![
                "repo",
                "force-redownload",
                "--repo-name",
                repository,
                "--yes",
                "--quiet",
            ],
            "remote-refresh" | "quick-check" | "recheck" | "recheck-integrity" | "download" => {
                vec![
                    "repo",
                    "sync",
                    "--repo-name",
                    repository,
                    "--mode",
                    name,
                    "--quiet",
                ]
            }
            _ => bail!("Unknown CLI operation {name}"),
        };
        let offsets = logs::offsets(self.run, self.config)?;
        let started = Instant::now();
        let response = launch::tracked_foxy(
            self.exe,
            self.config,
            &args.into_iter().map(String::from).collect::<Vec<_>>(),
            self.env,
            Duration::from_secs(
                operation["wait_timeout_s"]
                    .as_u64()
                    .unwrap_or(self.timeout.as_secs()),
            ),
            Some(self.pid),
        )?;
        let elapsed = started.elapsed().as_secs_f64();
        // The measured process is gone; stop the watch from following a reused pid.
        memory::set(self.pid, None);
        let log = logs::delta(&offsets, self.run, self.config)?;
        let profile = launch::assert_database_mode(&log, self.mode, self.gate)?;
        let mut summary = response["data"].clone();
        ensure!(
            summary.is_object(),
            "CLI operation did not return a summary object"
        );
        if summary.get("total_ms").is_none() {
            summary["total_ms"] = summary
                .get("elapsed_ms")
                .cloned()
                .unwrap_or(json!((elapsed * 1000.0).round()));
        }
        Ok(
            json!({"elapsed_s":elapsed,"summary":summary,"progress":null,"snapshot":null,"logs":null,"log_text":log,"database_profile":profile}),
        )
    }
    fn gui_operation(&self, operation: &Value) -> Result<Value> {
        let name = operation["op"].as_str().context("Missing operation name")?;
        let action = match name {
            "download" => "start-sync",
            "download-full-files" => "start-sync-full-files",
            "force-redownload" => "force-redownload",
            "recheck" => "recheck-repo",
            "recheck-integrity" => "recheck-integrity",
            "quick-check" => "quick-check",
            "remote-refresh" => "remote-recheck",
            "wipe-db" => "wipe-repo-db",
            _ => bail!("Operation {name} is not available through the GUI harness"),
        };
        // A DB wipe runs on its own worker and never raises `core-sync`.
        let busy_reason = if name == "wipe-db" {
            "repository-db-wipe"
        } else {
            "core-sync"
        };
        let generation = self.data(&["logs", "--limit", "1"])?["generation"]
            .as_u64()
            .context("Missing log generation")?;
        let repo_index = self.gui_repository_index(operation)?.to_string();
        let offsets = logs::offsets(self.run, self.config)?;
        let probe = operation["ui_probe_ms"]
            .as_u64()
            .filter(|ms| *ms > 0)
            .map(|ms| UiProbe::start(self.exe, self.config, self.env, Duration::from_millis(ms)));
        let progress_probe = self
            .progress_probe
            .map(|interval| ProgressProbe::start(self.exe, self.config, self.env, interval));
        let started = Instant::now();
        let mut invoke = vec![
            "invoke",
            action,
            "--repo-index",
            &repo_index,
            "--allow-destructive",
        ];
        if name == "wipe-db" && operation["keep_hash_record"].as_bool() == Some(true) {
            invoke.extend(["--params", r#"{"keep-hash-record":true}"#]);
        }
        let watch_promotion = operation["cancel_after_promoted_file"].as_bool() == Some(true)
            || operation["kill_after_promoted_file"].as_bool() == Some(true)
            || operation["close_after_promoted_file"].as_bool() == Some(true);
        let before_promotion = if watch_promotion {
            Some(self.repository_file_state()?)
        } else {
            None
        };
        self.data(&invoke)?;
        let busy_deadline = Instant::now() + Duration::from_secs(30);
        let mut observed_busy = false;
        let mut finished_between_polls = false;
        while Instant::now() < busy_deadline {
            let progress = self.data(&["progress"])?;
            if progress["busy_reasons"]
                .as_array()
                .is_some_and(|reasons| reasons.iter().any(|v| v == busy_reason))
            {
                observed_busy = true;
                break;
            }
            // A clean quick check finishes in milliseconds, between two polls;
            // its pipeline summary is then the only evidence it ran.
            if busy_reason == "core-sync"
                && self.logged_since(generation, "Pipeline summary: op=")?
            {
                finished_between_polls = true;
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
        if !observed_busy && !finished_between_polls {
            eprintln!(
                "Operation never reported busy reason '{busy_reason}'; timing may not cover the real work"
            );
        }
        // A cancellation lane: let the transfer run for `cancel_after_s`, then
        // cancel and measure cancel-to-quiescent (the busy reason clearing).
        let mut cancel_quiescent_ms = Value::Null;
        let mut cancel_trigger_hash_progress = Value::Null;
        let mut promoted_file = None;
        let mut external_edit_after_promotion = None;
        let mut close_wait_ms = Value::Null;
        let mut paused = false;
        if operation["close_after_promoted_file"].as_bool() == Some(true) && !finished_between_polls
        {
            let wait = operation["wait_timeout_s"].as_u64().unwrap_or(300);
            promoted_file = Some(self.wait_for_promoted_file(
                before_promotion.as_ref().expect("promotion watch snapshot"),
                Duration::from_secs(wait),
            )?);
            close_wait_ms = self
                .close_gui_and_restart(Duration::from_secs(wait))?
                .into();
            finished_between_polls = true;
        } else if operation["cancel_after_promoted_file"].as_bool() == Some(true)
            && !finished_between_polls
        {
            let wait = operation["wait_timeout_s"].as_u64().unwrap_or(300);
            promoted_file = Some(self.wait_for_promoted_file(
                before_promotion.as_ref().expect("promotion watch snapshot"),
                Duration::from_secs(wait),
            )?);
            if let Some(content) = operation["external_edit_after_promoted_file"].as_str() {
                let relative = Path::new(promoted_file.as_deref().expect("promoted file"));
                let repository = self.repository_path()?;
                let target = ensure_run_path(self.run, &repository.join(relative))?;
                ensure!(target.is_file(), "Promoted file is not a regular file");
                fs::write(&target, content.as_bytes())
                    .with_context(|| format!("Writing test edit to {}", target.display()))?;
                external_edit_after_promotion = Some(promoted_file.clone().unwrap());
            }
            let cancel_started = Instant::now();
            self.data(&["invoke", "cancel-download"])?;
            self.driver(
                &[
                    "wait".into(),
                    "--busy-reason-cleared".into(),
                    busy_reason.into(),
                    "--timeout-ms".into(),
                    (wait * 1000).to_string(),
                ],
                Duration::from_secs(wait + 10),
            )?;
            cancel_quiescent_ms = (cancel_started.elapsed().as_secs_f64() * 1000.0)
                .round()
                .into();
            finished_between_polls = true;
        } else if let Some(after) = operation["cancel_after_s"].as_f64()
            && !finished_between_polls
        {
            thread::sleep(Duration::from_secs_f64(after));
            let cancel_started = Instant::now();
            self.data(&["invoke", "cancel-download"])?;
            let wait = operation["wait_timeout_s"].as_u64().unwrap_or(300);
            self.driver(
                &[
                    "wait".into(),
                    "--busy-reason-cleared".into(),
                    busy_reason.into(),
                    "--timeout-ms".into(),
                    (wait * 1000).to_string(),
                ],
                Duration::from_secs(wait + 10),
            )?;
            cancel_quiescent_ms = (cancel_started.elapsed().as_secs_f64() * 1000.0)
                .round()
                .into();
            finished_between_polls = true;
        } else if let Some(threshold) = operation["cancel_after_hash_progress"].as_f64()
            && !finished_between_polls
        {
            let wait = operation["wait_timeout_s"].as_u64().unwrap_or(300);
            let deadline = Instant::now() + Duration::from_secs(wait);
            let observed = loop {
                let progress = self.data(&["progress"])?;
                ensure!(
                    progress["busy_reasons"]
                        .as_array()
                        .is_some_and(|reasons| reasons.iter().any(|v| v == busy_reason)),
                    "Integrity sync stopped before hash progress reached cancellation threshold {threshold:.3}"
                );
                let fraction = progress["recheck_hash_progress"].as_f64().unwrap_or(0.0);
                if fraction >= threshold && fraction < 1.0 {
                    break fraction;
                }
                ensure!(
                    fraction < 1.0,
                    "Integrity hashing reached completion before the cancellation threshold {threshold:.3}"
                );
                ensure!(
                    !self.logged_since(generation, "Pipeline summary: op=")?,
                    "Sync finished before integrity hash progress reached cancellation threshold {threshold:.3}"
                );
                ensure!(
                    Instant::now() < deadline,
                    "Integrity hash progress did not reach cancellation threshold {threshold:.3}"
                );
                thread::sleep(Duration::from_millis(100));
            };
            cancel_trigger_hash_progress = observed.into();
            let cancel_started = Instant::now();
            self.data(&["invoke", "cancel-download"])?;
            self.driver(
                &[
                    "wait".into(),
                    "--busy-reason-cleared".into(),
                    busy_reason.into(),
                    "--timeout-ms".into(),
                    (wait * 1000).to_string(),
                ],
                Duration::from_secs(wait + 10),
            )?;
            cancel_quiescent_ms = (cancel_started.elapsed().as_secs_f64() * 1000.0)
                .round()
                .into();
            finished_between_polls = true;
        }
        if let Some(after) = operation["pause_after_s"].as_f64()
            && !finished_between_polls
        {
            thread::sleep(Duration::from_secs_f64(after));
            self.data(&["invoke", "pause-download"])?;
            paused = true;
            finished_between_polls = true;
        }
        // A killed process cannot finish the refresh, so the checksum commit
        // inside it must not land. The next operation restarts the app.
        let mut killed = false;
        if operation["kill_after_promoted_file"].as_bool() == Some(true) && !finished_between_polls
        {
            let wait = operation["wait_timeout_s"].as_u64().unwrap_or(300);
            promoted_file = Some(self.wait_for_promoted_file(
                before_promotion.as_ref().expect("promotion watch snapshot"),
                Duration::from_secs(wait),
            )?);
            if let Some(gui) = self.gui.borrow_mut().as_mut() {
                let _ = gui.child.kill();
                let _ = gui.wait(Duration::from_secs(15));
            }
            memory::set(self.pid, None);
            drop(self.gui.borrow_mut().take());
            killed = true;
            finished_between_polls = true;
        } else if let Some(after) = operation["kill_after_s"].as_f64()
            && !finished_between_polls
        {
            thread::sleep(Duration::from_secs_f64(after));
            if let Some(gui) = self.gui.borrow_mut().as_mut() {
                let _ = gui.child.kill();
                let _ = gui.wait(Duration::from_secs(15));
            }
            memory::set(self.pid, None);
            drop(self.gui.borrow_mut().take());
            killed = true;
            finished_between_polls = true;
        }
        if !finished_between_polls {
            let wait = operation["wait_timeout_s"]
                .as_u64()
                .unwrap_or(self.timeout.as_secs());
            if matches!(
                name,
                "download" | "download-full-files" | "force-redownload"
            ) {
                self.wait_for_download(busy_reason, Duration::from_secs(wait))?;
            } else {
                self.driver(
                    &[
                        "wait".into(),
                        "--busy-reason-cleared".into(),
                        busy_reason.into(),
                        "--timeout-ms".into(),
                        (wait * 1000).to_string(),
                    ],
                    Duration::from_secs(wait + 10),
                )?;
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        let ui_probe = (!killed).then(|| probe.map(UiProbe::finish)).flatten();
        let progress_probe = (!killed)
            .then(|| progress_probe.map(|probe| probe.finish(elapsed)))
            .flatten();
        let mut summary = if killed {
            json!({"killed": true})
        } else {
            self.data(&["download-summary", "--include-telemetry"])?
        };
        if !cancel_quiescent_ms.is_null() {
            summary["cancel_quiescent_ms"] = cancel_quiescent_ms;
        }
        if !cancel_trigger_hash_progress.is_null() {
            summary["cancel_trigger_hash_progress"] = cancel_trigger_hash_progress;
        }
        if !close_wait_ms.is_null() {
            summary["close_wait_ms"] = close_wait_ms;
        }
        if let Some(path) = promoted_file {
            summary["promoted_file"] = json!(path);
        }
        if let Some(path) = external_edit_after_promotion {
            summary["external_edit_after_promotion"] = json!(path);
        }
        if paused {
            summary["paused"] = json!(true);
        }
        if let Some(ui_probe) = ui_probe {
            summary["ui_probe"] = ui_probe;
        }
        if let Some(progress_probe) = progress_probe {
            summary["progress_probe"] = progress_probe;
        }
        let progress = if killed {
            Value::Null
        } else {
            self.data(&["progress"])?
        };
        let snapshot = if killed {
            Value::Null
        } else {
            self.data(&["snapshot"])?
        };
        let captured = if killed {
            Value::Null
        } else {
            self.data(&[
                "logs",
                "--since-generation",
                &generation.to_string(),
                "--limit",
                "2000",
            ])?
        };
        let messages = captured["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["message"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let log = format!(
            "{}\n{messages}",
            logs::delta(&offsets, self.run, self.config)?
        );
        Ok(
            json!({"elapsed_s":elapsed,"summary":summary,"progress":progress,"snapshot":snapshot,"logs":captured,"log_text":log}),
        )
    }
    /// Only a successful download raises `--download-complete`. One that fails
    /// before it finishes (a disk-space preflight, say) ends its sync without
    /// it, so wait in slices and stop once the sync has ended, leaving the
    /// failure to the row's outcome rather than sitting out the whole timeout.
    fn wait_for_download(&self, busy_reason: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let slice = deadline
                .saturating_duration_since(Instant::now())
                .min(DOWNLOAD_WAIT_SLICE);
            ensure!(
                !slice.is_zero(),
                "Timed out after {} s waiting for the download to complete",
                timeout.as_secs()
            );
            let args = [
                "wait".to_owned(),
                "--download-complete".to_owned(),
                "--timeout-ms".to_owned(),
                slice.as_millis().to_string(),
            ];
            if self.driver(&args, slice + Duration::from_secs(10)).is_ok() {
                return Ok(());
            }
            // A sync can drop its busy reason for a moment between stages, so
            // the end only counts when a second look a second later agrees.
            if sync_ended_without_download(&self.data(&["progress"])?, busy_reason) {
                thread::sleep(Duration::from_secs(1));
                if sync_ended_without_download(&self.data(&["progress"])?, busy_reason) {
                    eprintln!(
                        "The sync ended without completing the download; recording the operation as it stands"
                    );
                    return Ok(());
                }
            }
        }
    }
    fn logged_since(&self, generation: u64, needle: &str) -> Result<bool> {
        let captured = self.data(&[
            "logs",
            "--since-generation",
            &generation.to_string(),
            "--limit",
            "2000",
        ])?;
        Ok(captured["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["message"].as_str())
            .any(|message| message.contains(needle)))
    }
    fn ux(&self) -> Result<Value> {
        let generation = self.data(&["logs", "--limit", "1"])?["generation"]
            .as_u64()
            .context("Missing log generation")?;
        if self.case["stable_render"].as_bool().unwrap_or(true) {
            self.data(&["stable-render", "true"])?;
        }
        let mut transcript = Vec::new();
        let mut failed = false;
        let screenshots = self.case["screenshots"].as_str().unwrap_or("on-failure");
        for (index, step) in self.case["steps"]
            .as_array()
            .context("Missing UX steps")?
            .iter()
            .enumerate()
        {
            let path = self.run.join(format!("scenario-step-{index}.json"));
            write_json(&path, &json!([step]))?;
            let response = launch::foxy(
                self.exe,
                self.config,
                &[
                    "agent-gui".into(),
                    "scenario".into(),
                    path.to_string_lossy().into_owned(),
                ],
                self.env,
                self.timeout,
            );
            match response {
                Ok(response) => {
                    let data = &response["data"];
                    let ok = data["ok"].as_bool().unwrap_or(true);
                    failed |= !ok;
                    transcript.push(json!({"step":index,"ok":ok,"result":data}));
                }
                Err(error) => {
                    failed = true;
                    transcript.push(json!({"step":index,"ok":false,"error":error.to_string()}));
                }
            }
            if screenshots == "each-step" || (failed && screenshots == "on-failure") {
                let shot = self.run.join(format!("screenshot-{index}.png"));
                let _ = self.driver(
                    &[
                        "screenshot".into(),
                        "--output".into(),
                        shot.to_string_lossy().into_owned(),
                    ],
                    Duration::from_secs(30),
                );
            }
            if failed {
                break;
            }
        }
        let captured = self.data(&[
            "logs",
            "--since-generation",
            &generation.to_string(),
            "--limit",
            "2000",
        ])?;
        let allowed: Vec<_> = self.case["allow_warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let unexpected: Vec<_> = captured["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| {
                matches!(entry["level"].as_str(), Some("WARN" | "ERROR"))
                    && !allowed
                        .iter()
                        .any(|allowed| entry["message"].as_str().unwrap_or("").contains(allowed))
            })
            .cloned()
            .collect();
        failed |= !unexpected.is_empty();
        let result =
            json!({"ok":!failed,"steps":transcript,"unexpected_logs":unexpected,"logs":captured});
        write_json(&self.run.join("summary.json"), &result)?;
        ensure!(
            !failed,
            "UX case failed; inspect summary.json and screenshots in the run directory"
        );
        Ok(result)
    }
}

fn oracle_repository<'a>(case: &'a Value, operation: &Value) -> Result<&'a Value> {
    let Some(name) = operation["repository"].as_str() else {
        return Ok(&case["repository"]);
    };
    std::iter::once(&case["repository"])
        .chain(case["extra_repositories"].as_array().into_iter().flatten())
        .find(|repository| repository["name"].as_str() == Some(name))
        .context("Oracle target repository was not declared in the case")
}

fn oracle(
    case: &Value,
    operation: &Value,
    root: &Path,
    run: &Path,
    timeout: Duration,
) -> Result<bool> {
    let Some(argv) = case["guards"]
        .get("oracle_command")
        .filter(|v| !v.is_null())
    else {
        return Ok(true);
    };
    let argv = argv
        .as_array()
        .context("guards.oracle_command must be an argv array, not a command string")?;
    ensure!(!argv.is_empty(), "guards.oracle_command cannot be empty");
    let target = oracle_repository(case, operation)?;
    let repository = target["path"].as_str().unwrap_or("");
    let url = target["address"].as_str().unwrap_or("");
    let stem = if operation["setup_once"].as_bool() == Some(true) {
        let name: String = target["name"]
            .as_str()
            .unwrap_or("repository")
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        format!("oracle-setup-{name}")
    } else {
        "oracle".to_owned()
    };
    let expanded: Vec<String> = argv
        .iter()
        .map(|arg| {
            arg.as_str()
                .map(|arg| {
                    arg.replace("${REPOSITORY_PATH}", repository)
                        .replace("${REPOSITORY_URL}", url)
                })
                .context("oracle_command arguments must be strings")
        })
        .collect::<Result<_>>()?;
    let mut command = Command::new(&expanded[0]);
    command
        .args(&expanded[1..])
        .current_dir(root)
        .env("FOXY_TESTKIT_REPOSITORY_PATH", repository)
        .env("FOXY_TESTKIT_REPOSITORY_URL", url)
        .env("FOXY_TESTKIT_RUN_DIR", run);
    match launch::process(&mut command, timeout) {
        Ok(output) => {
            fs::write(run.join(format!("{stem}.out")), &output.stdout)?;
            fs::write(run.join(format!("{stem}.err")), &output.stderr)?;
            Ok(output.status.success())
        }
        Err(error) => {
            fs::write(run.join(format!("{stem}.err")), error.to_string())?;
            Ok(false)
        }
    }
}

fn residual_download_artifacts(root: &Path) -> Result<Value> {
    let mut part_files = 0u64;
    let mut part_meta_files = 0u64;
    let mut patch_temp_files = 0u64;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.ends_with(".foxy.part.meta") {
            part_meta_files += 1;
        } else if name.ends_with(".foxy.part") {
            part_files += 1;
        } else if name.ends_with(".foxy.tmp") {
            patch_temp_files += 1;
        }
    }
    Ok(json!({
        "residual_files": part_files + part_meta_files + patch_temp_files,
        "part_files": part_files,
        "part_meta_files": part_meta_files,
        "patch_temp_files": patch_temp_files,
    }))
}

pub fn execute(root: &Path, options: &RunOptions) -> Result<Value> {
    ensure!(
        matches!(options.database_mode.as_str(), "wal" | "mvcc"),
        "Database mode must be wal or mvcc"
    );
    ensure!(
        options.db_write_gate <= 8,
        "Database write gate must be between 0 and 8"
    );
    let header: Value = serde_json::from_slice(&fs::read(&options.case)?)?;
    let id = header["id"].as_str().context("Case needs an id")?;
    ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "Case id must be kebab-case"
    );
    let run_id = format!(
        "{}-{:08x}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        SystemTime::now().duration_since(UNIX_EPOCH)?.subsec_nanos()
    );
    let run = root.join("testkit/runs").join(id).join(&run_id);
    let resolved = case::load(&options.case, root, &run)?;
    if resolved["enabled"].as_bool() == Some(false) {
        return Ok(json!({"status":"skip","case_id":id}));
    }
    fs::create_dir_all(&run)?;
    let hash = case::hash(&resolved)?;
    let kind = resolved["kind"].as_str().context("Missing case kind")?;
    let profile =
        resolved["build"]
            .as_str()
            .unwrap_or(if kind == "perf" { "release" } else { "debug" });
    let harness = resolved["harness"].as_str().unwrap_or("gui");
    let timeout = Duration::from_secs(resolved["timeout_s"].as_u64().unwrap_or(3600));
    let config = resolved["config_dir"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| run.join("config"));
    let config = if config.is_absolute() {
        config
    } else {
        root.join(config)
    };
    write_json(&run.join("resolved-case.json"), &resolved)?;
    let guard = guards::check(&resolved, root, &config)?;
    if options.validate_only {
        let result = json!({"status":"valid","valid":true,"case_id":id,"case_hash":hash,"database_mode":options.database_mode,"db_write_gate":options.db_write_gate,"storage_class":guard["storage_class"]});
        write_json(&run.join("validation.json"), &result)?;
        return Ok(result);
    }
    let env: Environment = [
        (
            "FOXY_TESTKIT_RUN_DIR".into(),
            Some(run.to_string_lossy().into_owned()),
        ),
        (
            "FOXY_CONFIG_DIR".into(),
            Some(config.to_string_lossy().into_owned()),
        ),
        ("RUST_LOG".into(), Some("info".into())),
        (
            "FOXY_DB_MVCC".into(),
            (options.database_mode == "mvcc").then(|| "1".into()),
        ),
        (
            "FOXY_DB_WRITE_GATE".into(),
            (options.db_write_gate != 0).then(|| options.db_write_gate.to_string()),
        ),
        // Deep profiling changes timing, so it is opt-in per case rather than on
        // for every run: a profiled row is not comparable with an unprofiled one.
        (
            "FOXY_PROFILE".into(),
            resolved["profile"]
                .as_bool()
                .unwrap_or(false)
                .then(|| "1".into()),
        ),
    ]
    .into_iter()
    .collect();
    let exe = if let Some(exe) = &options.foxy_binary {
        let exe = fs::canonicalize(exe).context("Could not resolve --foxy-binary")?;
        let local = fs::canonicalize(root.join(".local"))
            .context("Repository .local directory is required for --foxy-binary")?;
        ensure!(
            exe.starts_with(local),
            "--foxy-binary must point inside repository .local"
        );
        ensure!(
            exe.is_file(),
            "--foxy-binary must point to an executable file"
        );
        exe
    } else if options.no_build {
        launch::executable(root, profile, "Foxy")
    } else {
        launch::build(root, profile, "Foxy", timeout)?
    };
    ensure!(
        exe.is_file(),
        "Foxy executable is missing; build it or omit --no-build"
    );
    let _origin = if let Some(origin) = resolved.get("origin").filter(|v| !v.is_null()) {
        if let Some(versions) = origin["synthetic"].as_array() {
            for version in versions {
                let output = version["root"]
                    .as_str()
                    .context("Synthetic origin version requires root")?;
                let output = ensure_run_path(&run, &PathBuf::from(output))?;
                let mut source = output.as_os_str().to_owned();
                source.push("-src");
                ensure_run_path(&run, &PathBuf::from(source))?;
                let options = crate::origin::synthetic::SyntheticOptions {
                    output,
                    mods: version["mods"].as_u64().unwrap_or(1) as u32,
                    files_per_mod: version["files_per_mod"].as_u64().unwrap_or(2) as u32,
                    file_bytes: version["file_bytes"].as_u64().unwrap_or(16 * 1024 * 1024) as usize,
                    tail_file_bytes: None,
                    pbo_entries: version["pbo_entries"].as_u64().unwrap_or(0) as u32,
                    entry_bytes: version["entry_bytes"].as_u64().unwrap_or(64) as usize,
                    seed: version["seed"].as_i64().unwrap_or(20261028) as i32,
                    mod_seeds: version["mod_seeds"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_i64)
                        .map(|seed| seed as i32)
                        .collect(),
                    repo_name: version["repo_name"]
                        .as_str()
                        .unwrap_or("RepoAlpha")
                        .to_owned(),
                    mode: "foxy".into(),
                    force: true,
                };
                options.execute(root)?;
            }
        }
        let path = origin["root"]
            .as_str()
            .context("Case origin requires a root directory")?;
        let port = u16::try_from(origin["port"].as_u64().unwrap_or(0))?;
        Some(crate::origin::server::Origin::start_impaired(
            Path::new(path),
            port,
            crate::origin::server::Impairment::from_case(origin),
        )?)
    } else {
        None
    };
    // The origin's published checksum is the payload identity the case hash
    // cannot see: a re-mirrored origin under an unchanged case JSON is a
    // different workload and must not inherit the old baseline.
    let mut origin_checksum = Value::Null;
    if kind == "perf" {
        let address = resolved["repository"]["address"]
            .as_str()
            .context("Missing repository address")?;
        let manifest = fixture::metadata(address)?;
        ensure!(
            fixture::mod_count(&manifest) > 0,
            "Repository publishes no mods; refusing an empty performance workload"
        );
        origin_checksum = manifest["checksum"].clone();
        write_json(
            &run.join("manifest-probe.json"),
            &json!({"url":address,"mods":fixture::mod_count(&manifest),"checksum":origin_checksum}),
        )?;
    }
    let diagnostics = if resolved["profile"].as_bool().unwrap_or(false) {
        "profile"
    } else {
        "none"
    };
    let calibration_path = crate::calibrate::path(root);
    let references = if calibration_path.exists() {
        let volume = resolved["repository"]["path"]
            .as_str()
            .and_then(|path| Path::new(path).components().next())
            .map(|component| component.as_os_str().to_string_lossy().into_owned());
        references::select(
            &crate::case::read_json(&calibration_path)?,
            &guard["environment"],
            guard["storage_class"].as_str().unwrap_or("unknown"),
            volume.as_deref(),
        )
    } else {
        json!({})
    };
    if let Some(source) = resolved.get("config_seed").and_then(Value::as_str) {
        let bytes = fixture::seed(Path::new(source), &config)?;
        write_json(
            &run.join("config-seed.json"),
            &json!({"source":source,"bytes":bytes}),
        )?;
    }
    fixture::install(&resolved, &config, &run)?;
    fixture::install_config_files(&resolved, &config)?;
    if harness == "gui"
        && !config.join("window_state.json").exists()
        && let Some(appdata) = std::env::var_os("APPDATA")
    {
        let saved = PathBuf::from(appdata).join("Foxy/window_state.json");
        if saved.is_file() {
            fs::copy(saved, config.join("window_state.json"))?;
        }
    }
    let operations = resolved["operations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut mutation = if operations
        .iter()
        .any(|op| matches!(op["op"].as_str(), Some("mutate" | "restore")))
    {
        Some(Mutation {
            executable: launch::build(root, "release", "foxy-testkit-mutate", timeout)?,
            root: PathBuf::from(
                resolved["repository"]["path"]
                    .as_str()
                    .context("Mutation requires a repository path")?,
            ),
            journal: run.join("mutations.json"),
            journals: Vec::new(),
            timeout,
        })
    } else {
        None
    };
    let pid = memory::target();
    let gui: RefCell<Option<launch::ManagedChild>> = RefCell::new(if harness == "gui" {
        Some(launch::start_gui(&exe, &config, &run, &env, timeout, &pid)?)
    } else {
        None
    });
    let result = (|| -> Result<Value> {
        let context = ContextRun {
            case: &resolved,
            exe: &exe,
            config: &config,
            run: &run,
            env: &env,
            timeout,
            mode: &options.database_mode,
            gate: options.db_write_gate,
            progress_probe: (options.progress_probe_ms > 0)
                .then(|| Duration::from_millis(options.progress_probe_ms)),
            gui: &gui,
            origin: _origin.as_ref(),
            test_file_locks: RefCell::new(HashMap::new()),
            pid: &pid,
        };
        let mut effective_gate = options.db_write_gate;
        // FOXY_DB_POOL_IDLE is inherited by the app, so a row recorded under an
        // A/B override is otherwise indistinguishable from a shipping-default row.
        let pool_idle = std::env::var("FOXY_DB_POOL_IDLE")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());
        if gui.borrow().is_some() {
            let deadline =
                Instant::now() + Duration::from_secs(if kind == "perf" { 60 } else { 5 });
            loop {
                let text = logs::corpus(&run, &config)?;
                if text.contains("STARTUP: Turso journal_mode=") {
                    effective_gate = launch::assert_database_mode(
                        &text,
                        &options.database_mode,
                        options.db_write_gate,
                    )?["write_gate_permits"]
                        .as_u64()
                        .context("Missing write gate")? as u32;
                    break;
                }
                if Instant::now() >= deadline {
                    ensure!(kind != "perf", "Foxy did not report a Turso startup mode");
                    break;
                }
                thread::sleep(Duration::from_millis(500));
            }
        }
        if kind == "ux" {
            context.ux()?;
            return Ok(json!({"status":"pass","case_id":id,"run_id":run_id,"run_dir":run}));
        }
        let evict = |operation: &Value| -> Result<()> {
            let path = operation["path"]
                .as_str()
                .or_else(|| resolved["repository"]["path"].as_str())
                .context("evict-cache requires a repository path")?;
            let summary = crate::evict::evict_directory(Path::new(path))?;
            eprintln!("evict-cache: {summary}");
            Ok(())
        };
        for operation in operations
            .iter()
            .filter(|op| op["setup_once"].as_bool().unwrap_or(false))
        {
            if context.state_operation(operation)? {
                continue;
            }
            if operation["op"].as_str() == Some("evict-cache") {
                evict(operation)?;
                continue;
            }
            let collected = context.operation(operation)?;
            if matches!(
                operation["op"].as_str(),
                Some("download" | "download-full-files" | "force-redownload")
            ) && operation.get("expect_outcome").is_none()
                && resolved["harness"].as_str().unwrap_or("gui") == "gui"
            {
                ensure!(
                    collected["progress"]["download_finished"].as_bool() == Some(true),
                    "The setup download did not finish successfully; refusing to test an invalid initial state"
                );
                ensure!(
                    oracle(&resolved, operation, root, &run, timeout)?,
                    "The independent oracle rejected the setup payload"
                );
            }
        }
        let warmup = resolved["warmup"].as_bool().unwrap_or(false);
        let passes = resolved["repetitions"].as_u64().unwrap_or(3);
        let mut rows = Vec::new();
        let mut mutation_info = Value::Null;
        // An `evict-cache` step marks the next measured operation as the
        // evicted lane; the operation after that sees whatever the first one
        // left in the page cache, which is the warm lane again.
        let mut evicted_pending = false;
        for pass in 0..passes + u64::from(warmup) {
            let record = !(warmup && pass == 0);
            let iteration = pass.saturating_sub(u64::from(warmup));
            for operation in operations
                .iter()
                .filter(|op| !op["setup_once"].as_bool().unwrap_or(false))
            {
                let name = operation["op"].as_str().context("Missing operation name")?;
                if context.state_operation(operation)? {
                    continue;
                }
                match name {
                    "mutate" => {
                        mutation_info = mutation
                            .as_mut()
                            .context("Missing mutator")?
                            .apply(operation)?;
                        continue;
                    }
                    "restore" => {
                        mutation.as_mut().context("Missing mutator")?.restore()?;
                        mutation_info = Value::Null;
                        continue;
                    }
                    "evict-cache" => {
                        evict(operation)?;
                        evicted_pending = true;
                        continue;
                    }
                    _ => {}
                }
                let burst_before = _origin.as_ref().map(|origin| origin.burst_snapshot());
                let collected = context.operation(operation)?;
                if let Some(gate) = collected["database_profile"]["write_gate_permits"].as_u64() {
                    effective_gate = gate as u32;
                }
                if !record {
                    evicted_pending = false;
                    continue;
                }
                let text = collected["log_text"]
                    .as_str()
                    .context("Missing operation log")?;
                let sol = sol::parse(text);
                let breakdown = metrics::breakdown(text);
                // Two operations of one kind in an iteration (a check repeated
                // to prove the second one is free) need distinct artifacts.
                let label = operation["label"].as_str().unwrap_or(name);
                let stem = format!("{iteration}-{label}");
                write_json(&run.join(format!("collected-{stem}.json")), &collected)?;
                write_json(&run.join(format!("breakdown-{stem}.json")), &breakdown)?;
                write_json(
                    &run.join(format!("memory-{stem}.json")),
                    &collected["summary"]["memory"],
                )?;
                write_json(
                    &run.join(format!("profile-{stem}.json")),
                    &profile::parse(text),
                )?;
                fs::write(run.join(format!("operation-{stem}.log")), text)?;
                for entry in &sol {
                    ledger::append(entry, &run.join("sol.jsonl"))?;
                }
                let mut summary = collected["summary"].clone();
                if operation["inspect_cleanup"].as_bool().unwrap_or(false) {
                    let repository_path = resolved["repository"]["path"]
                        .as_str()
                        .context("Missing repository path")?;
                    summary["cleanup"] = residual_download_artifacts(Path::new(repository_path))?;
                }
                for (destination, source) in
                    [("files_updated", "files"), ("downloaded_bytes", "bytes")]
                {
                    if summary.get(destination).is_none()
                        && !breakdown["run_metrics"][source].is_null()
                    {
                        summary[destination] = breakdown["run_metrics"][source].clone();
                    }
                }
                if summary.get("delta_savings_percent").is_none() {
                    let full = summary["full_download_bytes"].as_f64().unwrap_or(0.0);
                    summary["delta_savings_percent"] = json!(if full > 0.0 {
                        (1000000.0 * summary["patch_savings_bytes"].as_f64().unwrap_or(0.0) / full)
                            .round()
                            / 10000.0
                    } else {
                        0.0
                    });
                }
                write_json(&run.join(format!("summary-{stem}.json")), &summary)?;
                let mut view = json!({
                    "summary": summary,
                    "progress": collected["progress"],
                    "snapshot": collected["snapshot"],
                    "sol": sol,
                    "breakdown": breakdown,
                    "references": references,
                    "delta_patch": sol::operation(&sol, "delta_patch"),
                    "delta_patch_stages": sol::records(&sol, "delta_patch_stage"),
                    "download": sol::operation(&sol, "download"),
                    "quick_scan": sol::operation(&sol, "quick_scan"),
                    "startup": sol::operation(&sol, "startup"),
                    "startup_probe": sol::operation(&sol, "startup_probe"),
                    "startup_probe_retry": sol::operation(&sol, "startup_probe_retry"),
                    "remote_refresh": sol::operation(&sol, "remote_refresh"),
                    "db_persist": sol::operation(&sol, "db_persist"),
                    "db_purge": sol::operation(&sol, "db_purge"),
                    "space_switch": sol::operation(&sol, "space_switch"),
                    "elapsed_s": collected["elapsed_s"],
                });
                if let (Some(origin), Some(before)) = (_origin.as_ref(), burst_before) {
                    view["origin_burst"] = origin.burst_delta(before);
                }
                crate::references::attach(&mut view);
                let mut flags = Vec::new();
                let assertion_failures = expect::expectations(&view, &operation["expect"]);
                write_json(
                    &run.join(format!("assertions-{stem}.json")),
                    &json!(assertion_failures),
                )?;
                if !assertion_failures.is_empty() {
                    flags.push("assertion-failed");
                }
                if !expect::thresholds(&view, &resolved["thresholds"]).is_empty() {
                    flags.push("threshold-failed");
                }
                let g = &resolved["guards"];
                if g["fail_on_db_wipe"].as_bool().unwrap_or(true)
                    && (text.contains("Database wipe confirmed")
                        || text.contains("Database wipe complete"))
                {
                    flags.push("db-wipe-detected");
                }
                if let Some(max) = g["max_run_gb"].as_f64()
                    && summary["downloaded_bytes"].as_f64().unwrap_or(0.0) > max * 1_073_741_824.0
                {
                    flags.push("runaway-bytes");
                }
                // A cancellation lane leaves the payload incomplete by design;
                // its correctness gate is the resume that follows.
                let expected_outcome = operation["expect_outcome"].as_str();
                let observed_outcome = breakdown["run_metrics"]["pipeline_outcome"].as_str();
                if !outcome_matches(expected_outcome, observed_outcome) {
                    flags.push("outcome-mismatch");
                }
                let cancelled_on_purpose = expected_outcome == Some("cancelled");
                if let Some(expected) = g["expected_files"].as_i64()
                    && matches!(
                        name,
                        "download" | "download-full-files" | "force-redownload"
                    )
                    && !cancelled_on_purpose
                    && operation["pause_after_s"].as_f64().is_none()
                    && operation["kill_after_promoted_file"].as_bool() != Some(true)
                    && operation["kill_after_s"].as_f64().is_none()
                    && summary["files_updated"].as_i64().unwrap_or(-1) != expected
                {
                    flags.push("incomplete-payload");
                }
                // The oracle verifies a synced payload, so it runs only after
                // the operations that produce one; a check between a
                // mutation and its repair sees the mutated bytes on purpose.
                let oracle_outcome = if matches!(
                    name,
                    "download" | "download-full-files" | "force-redownload"
                ) && !cancelled_on_purpose
                    && operation["pause_after_s"].as_f64().is_none()
                    && operation["kill_after_promoted_file"].as_bool() != Some(true)
                    && operation["kill_after_s"].as_f64().is_none()
                {
                    if oracle(&resolved, operation, root, &run, timeout)? {
                        "passed"
                    } else {
                        flags.push("oracle-failed");
                        "failed"
                    }
                } else {
                    "not_applicable"
                };
                let mut metadata = json!({"run_id":run_id,"iteration":iteration,"started_utc":chrono::Utc::now().to_rfc3339(),"elapsed_s":collected["elapsed_s"],"git_sha":guard["git"]["sha"],"git_dirty":guard["git"]["dirty"],"build_kind":profile,"case_id":id,"case_hash":hash,"harness":harness,"op":name,"storage_class":guard["storage_class"],"environment":guard["environment"],"cache_state":cache_state(warmup, iteration, evicted_pending),"database_mode":options.database_mode,"db_write_gate":effective_gate,"db_pool_idle":pool_idle,"diagnostics":diagnostics,"origin_checksum":origin_checksum,"references":references,"oracle_outcome":oracle_outcome,"flags":flags,"verdict":if flags.is_empty() {"ok"} else {"invalid"}});
                if label != name {
                    metadata["label"] = label.into();
                }
                if !view["origin_burst"].is_null() {
                    metadata["origin_burst"] = view["origin_burst"].clone();
                }
                write_json(
                    &run.join(format!("metadata-{stem}.json")),
                    &json!({"metadata":metadata,"mutation":mutation_info}),
                )?;
                rows.push(ledger::build_row(
                    metadata,
                    &summary,
                    &sol,
                    &breakdown,
                    &mutation_info,
                ));
                evicted_pending = false;
            }
        }
        let directory = root.join("testkit/ledger");
        let ledger_path = directory.join(format!("{id}.jsonl"));
        let baseline = directory.join(format!(
            "{id}.{harness}.{profile}.{}.gate-{effective_gate}.baseline.json",
            options.database_mode
        ));
        let comparison = ledger::compare(&rows, &baseline, &ledger_path)?;
        for row in &mut rows {
            let operation_verdict = comparison["operation_verdicts"][ledger::op_key(row)]
                .as_str()
                .or_else(|| comparison["verdict"].as_str());
            if row["verdict"] == "ok" && !matches!(operation_verdict, Some("ok" | "no-baseline")) {
                row["verdict"] = operation_verdict.into();
            }
            for flag in comparison["flags"].as_array().into_iter().flatten() {
                let flags = row["flags"]
                    .as_array_mut()
                    .context("Invalid ledger flags")?;
                if !flags.contains(flag) {
                    flags.push(flag.clone());
                }
            }
            ledger::append(row, &ledger_path)?;
        }
        write_json(
            &run.join("summary.json"),
            &json!({"run_id":run_id,"rows":rows,"comparison":comparison}),
        )?;
        if options.accept {
            ensure!(
                guard["git"]["dirty"] == false,
                "Baseline acceptance requires a clean Git worktree"
            );
            ledger::save_baseline(
                &rows,
                &baseline,
                &hash,
                guard["git"]["sha"].as_str().context("Missing Git SHA")?,
            )?;
        }
        ensure!(
            !rows.iter().any(|row| row["verdict"] == "invalid"),
            "Performance run contains invalid rows; inspect summary.json"
        );
        Ok(
            json!({"status":"pass","case_id":id,"run_id":run_id,"run_dir":run,"database_mode":options.database_mode,"db_write_gate":effective_gate,"comparison":comparison}),
        )
    })();
    if let Some(gui) = gui.borrow_mut().as_mut() {
        launch::stop_gui(gui, &exe, &config, &env);
    }
    if let Some(mutation) = &mut mutation {
        mutation.restore()?;
    }
    result
}

/// The cache lane a measured operation ran in. Iteration zero without a
/// warmup pass is cold; a run preceded by an `evict-cache` step is the
/// evicted lane whatever the iteration number, so repeated evictions never
/// masquerade as warm runs.
fn cache_state(warmup: bool, iteration: u64, evicted: bool) -> &'static str {
    if evicted {
        "evicted"
    } else if warmup || iteration > 0 {
        "warm"
    } else {
        "cold"
    }
}

fn outcome_matches(expected: Option<&str>, observed: Option<&str>) -> bool {
    expected.is_none() || expected == observed
}

/// How long one `--download-complete` wait blocks before the runner checks
/// whether the sync ended without completing the download.
const DOWNLOAD_WAIT_SLICE: Duration = Duration::from_secs(60);

fn sync_ended_without_download(progress: &Value, busy_reason: &str) -> bool {
    let busy = progress["busy_reasons"]
        .as_array()
        .is_some_and(|reasons| reasons.iter().any(|reason| reason == busy_reason));
    !busy && progress["download_finished"].as_bool() != Some(true)
}

#[cfg(test)]
mod cache_state_tests {
    use super::{
        cache_state, ensure_run_path, file_state_contains, is_download_sidecar,
        move_origin_addon_category, outcome_matches, residual_download_artifacts,
        sync_ended_without_download,
    };
    use serde_json::{Value, json};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn oracle_targets_the_operation_instance_even_when_urls_match() {
        let case = json!({"repository": {"name":"RepoAlpha", "address":"http://127.0.0.1/", "path":"R:/Mods/RepoAlpha"},
            "extra_repositories":[{"name":"RepoBeta", "address":"http://127.0.0.1/", "path":"R:/Mods/RepoBeta"}]});
        assert_eq!(
            super::oracle_repository(&case, &json!({})).unwrap()["path"],
            "R:/Mods/RepoAlpha"
        );
        assert_eq!(
            super::oracle_repository(&case, &json!({"repository":"RepoBeta"})).unwrap()["path"],
            "R:/Mods/RepoBeta"
        );
        assert!(super::oracle_repository(&case, &json!({"repository":"Unknown"})).is_err());
    }

    #[test]
    fn category_mutation_keeps_published_indexes_and_recomputes_checksum() {
        let directory = tempfile::tempdir().unwrap();
        let published = json!({
            "checksum":"old",
            "requiredMods":[],
            "optionalMods":[]
        });
        let index = json!({
            "checksum":"old",
            "requiredMods":[{"modName":"@synthetic_000","checkSum":"A1","enabled":true}],
            "optionalMods":[]
        });
        fs::write(
            directory.path().join("repo.json"),
            serde_json::to_vec(&published).unwrap(),
        )
        .unwrap();
        fs::write(
            directory.path().join("foxy_addons.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();

        move_origin_addon_category(directory.path(), "@synthetic_000", "required", "optional")
            .unwrap();

        let expected = blake3::hash(b"A1").to_hex().to_uppercase();
        let repo: Value =
            serde_json::from_slice(&fs::read(directory.path().join("repo.json")).unwrap()).unwrap();
        let index: Value =
            serde_json::from_slice(&fs::read(directory.path().join("foxy_addons.json")).unwrap())
                .unwrap();
        assert_eq!(repo["requiredMods"], json!([]));
        assert_eq!(repo["checksum"], expected);
        assert_eq!(index["requiredMods"], json!([]));
        assert_eq!(index["optionalMods"][0]["modName"], "@synthetic_000");
        assert_eq!(index["checksum"], expected);
    }

    #[test]
    fn payload_state_ignores_sidecars_and_preserves_captured_file_bytes() {
        for path in [
            "file.foxy.bak",
            "file.foxy.part",
            "file.foxy.part.meta",
            "file.foxy.tmp",
            ".foxy-write.lock",
        ] {
            assert!(is_download_sidecar(std::path::Path::new(path)));
        }
        assert!(!is_download_sidecar(std::path::Path::new("file.pbo")));
        let captured = json!({"promoted.bin": {"bytes": 3, "blake3": "new"}});
        assert!(file_state_contains(
            &json!({"promoted.bin": {"bytes": 3, "blake3": "new"}, "restored.bin": {"bytes": 3, "blake3": "old"}}),
            &captured
        ));
        assert!(!file_state_contains(&json!({}), &captured));
        assert!(!file_state_contains(
            &json!({"promoted.bin": {"bytes": 3, "blake3": "old"}}),
            &captured
        ));
    }

    #[test]
    fn test_paths_must_be_absolute_and_stay_inside_the_run() {
        let run = tempfile::tempdir().unwrap();
        let inside = run.path().join("nested");
        std::fs::create_dir_all(&inside).unwrap();
        assert_eq!(ensure_run_path(run.path(), &inside).unwrap(), inside);
        let missing = inside.join("new").join("payload");
        assert_eq!(ensure_run_path(run.path(), &missing).unwrap(), missing);
        assert!(ensure_run_path(run.path(), &inside.join("..").join("escape")).is_err());
        assert!(ensure_run_path(run.path(), &PathBuf::from("relative")).is_err());
        assert!(
            ensure_run_path(run.path(), &run.path().parent().unwrap().join("outside")).is_err()
        );
    }

    #[test]
    fn a_download_wait_ends_only_once_the_sync_is_over_without_completing() {
        let running = json!({ "busy_reasons": ["core-sync"], "download_finished": false });
        let failed = json!({ "busy_reasons": [], "download_finished": false });
        let done = json!({ "busy_reasons": [], "download_finished": true });
        assert!(!sync_ended_without_download(&running, "core-sync"));
        assert!(sync_ended_without_download(&failed, "core-sync"));
        assert!(!sync_ended_without_download(&done, "core-sync"));
    }

    #[test]
    fn eviction_wins_over_the_iteration_number() {
        assert_eq!(cache_state(false, 0, false), "cold");
        assert_eq!(cache_state(false, 1, false), "warm");
        assert_eq!(cache_state(true, 0, false), "warm");
        assert_eq!(cache_state(false, 0, true), "evicted");
        assert_eq!(cache_state(true, 3, true), "evicted");
    }

    #[test]
    fn explicit_outcome_must_match_the_owned_pipeline_result() {
        assert!(outcome_matches(None, Some("completed")));
        assert!(outcome_matches(Some("cancelled"), Some("cancelled")));
        assert!(!outcome_matches(Some("cancelled"), Some("completed")));
        assert!(!outcome_matches(Some("cancelled"), None));
    }

    #[test]
    fn cleanup_scan_counts_only_download_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.foxy.part"), b"a").unwrap();
        std::fs::write(directory.path().join("b.foxy.part.meta"), b"b").unwrap();
        std::fs::write(directory.path().join("c.foxy.tmp"), b"c").unwrap();
        std::fs::write(directory.path().join("kept.bin"), b"d").unwrap();
        let counts = residual_download_artifacts(directory.path()).unwrap();
        assert_eq!(counts["residual_files"], 3);
        assert_eq!(counts["part_files"], 1);
        assert_eq!(counts["part_meta_files"], 1);
        assert_eq!(counts["patch_temp_files"], 1);
    }
}

#[cfg(test)]
mod progress_probe_tests {
    use super::progress_probe_summary;

    #[test]
    fn a_linear_bar_has_no_gap_and_a_front_loaded_bar_does() {
        let linear: Vec<(f64, f64)> = (0..=10)
            .map(|i| (f64::from(i), f64::from(i) / 10.0))
            .collect();
        let summary = progress_probe_summary(&linear, 10.0);
        assert_eq!(summary["samples"], 11);
        assert_eq!(summary["max_gap_points"], 0.0);
        assert_eq!(summary["percent_at_half"], 50.0);

        let front_loaded = [(1.0, 0.44), (5.0, 0.76), (10.0, 1.0)];
        let summary = progress_probe_summary(&front_loaded, 10.0);
        assert_eq!(summary["max_gap_points"], 34.0);
        assert_eq!(summary["percent_at_half"], 76.0);
    }

    #[test]
    fn no_samples_reports_no_gap() {
        let summary = progress_probe_summary(&[], 10.0);
        assert_eq!(summary["samples"], 0);
        assert!(summary["max_gap_points"].is_null());
    }
}
