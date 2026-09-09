use crate::{
    case,
    collect::{expect, logs, metrics, profile, sol},
    driver,
    fixture::{self, write_json},
    guards,
    launch::{self, Environment},
    ledger,
    mutate::Mutation,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub case: PathBuf,
    pub database_mode: String,
    pub db_write_gate: u32,
    pub accept: bool,
    pub validate_only: bool,
    pub no_build: bool,
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
}

impl ContextRun<'_> {
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
    fn operation(&self, operation: &Value) -> Result<Value> {
        if self.case["harness"].as_str().unwrap_or("gui") == "cli" {
            self.cli_operation(operation)
        } else {
            self.gui_operation(operation)
        }
    }
    fn cli_operation(&self, operation: &Value) -> Result<Value> {
        let name = operation["op"].as_str().context("Missing operation name")?;
        let repository = self.case["repository"]["name"]
            .as_str()
            .context("Missing repository name")?;
        let args = match name {
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
        let response = launch::foxy(
            self.exe,
            self.config,
            &args.into_iter().map(String::from).collect::<Vec<_>>(),
            self.env,
            Duration::from_secs(
                operation["wait_timeout_s"]
                    .as_u64()
                    .unwrap_or(self.timeout.as_secs()),
            ),
        )?;
        let elapsed = started.elapsed().as_secs_f64();
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
            "force-redownload" => "force-redownload",
            "recheck" => "recheck-repo",
            "recheck-integrity" => "recheck-integrity",
            _ => bail!("Operation {name} is not available through the GUI harness"),
        };
        let generation = self.data(&["logs", "--limit", "1"])?["generation"]
            .as_u64()
            .context("Missing log generation")?;
        let offsets = logs::offsets(self.run, self.config)?;
        let started = Instant::now();
        self.data(&["invoke", action, "--repo-index", "0", "--allow-destructive"])?;
        let busy_deadline = Instant::now() + Duration::from_secs(30);
        let mut observed_busy = false;
        while Instant::now() < busy_deadline {
            let progress = self.data(&["progress"])?;
            if progress["busy_reasons"]
                .as_array()
                .is_some_and(|reasons| reasons.iter().any(|v| v == "core-sync"))
            {
                observed_busy = true;
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
        if !observed_busy {
            eprintln!(
                "Operation never reported busy reason 'core-sync'; timing may not cover the real work"
            );
        }
        let wait = operation["wait_timeout_s"]
            .as_u64()
            .unwrap_or(self.timeout.as_secs());
        let mut args: Vec<String> = vec!["wait".into()];
        if matches!(name, "download" | "force-redownload") {
            args.push("--download-complete".into());
        } else {
            args.extend(["--busy-reason-cleared".into(), "core-sync".into()]);
        }
        args.extend(["--timeout-ms".into(), (wait * 1000).to_string()]);
        self.driver(&args, Duration::from_secs(wait + 10))?;
        let elapsed = started.elapsed().as_secs_f64();
        let summary = self.data(&["download-summary", "--include-telemetry"])?;
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
        Ok(
            json!({"elapsed_s":elapsed,"summary":summary,"progress":progress,"snapshot":snapshot,"logs":captured,"log_text":log}),
        )
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

fn oracle(case: &Value, root: &Path, run: &Path, timeout: Duration) -> Result<bool> {
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
    let repository = case["repository"]["path"].as_str().unwrap_or("");
    let url = case["repository"]["address"].as_str().unwrap_or("");
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
            fs::write(run.join("oracle.out"), &output.stdout)?;
            fs::write(run.join("oracle.err"), &output.stderr)?;
            Ok(output.status.success())
        }
        Err(error) => {
            fs::write(run.join("oracle.err"), error.to_string())?;
            Ok(false)
        }
    }
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
    let exe = if options.no_build {
        launch::executable(root, profile, "Foxy")
    } else {
        launch::build(root, profile, "Foxy", timeout)?
    };
    ensure!(
        exe.is_file(),
        "Foxy executable is missing; build it or omit --no-build"
    );
    let _origin = if let Some(origin) = resolved.get("origin").filter(|v| !v.is_null()) {
        let path = origin["root"]
            .as_str()
            .context("Case origin requires a root directory")?;
        let port = u16::try_from(origin["port"].as_u64().unwrap_or(0))?;
        Some(crate::origin::server::Origin::start(Path::new(path), port)?)
    } else {
        None
    };
    if kind == "perf" {
        let address = resolved["repository"]["address"]
            .as_str()
            .context("Missing repository address")?;
        let manifest = fixture::metadata(address)?;
        ensure!(
            fixture::mod_count(&manifest) > 0,
            "Repository publishes no mods; refusing an empty performance workload"
        );
        write_json(
            &run.join("manifest-probe.json"),
            &json!({"url":address,"mods":fixture::mod_count(&manifest)}),
        )?;
    }
    fixture::install(&resolved, &config, &run)?;
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
            timeout,
        })
    } else {
        None
    };
    let mut gui = if harness == "gui" {
        Some(launch::start_gui(&exe, &config, &run, &env, timeout)?)
    } else {
        None
    };
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
        };
        let mut effective_gate = options.db_write_gate;
        // FOXY_DB_POOL_IDLE is inherited by the app, so a row recorded under an
        // A/B override is otherwise indistinguishable from a shipping-default row.
        let pool_idle = std::env::var("FOXY_DB_POOL_IDLE")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok());
        if gui.is_some() {
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
        for operation in operations
            .iter()
            .filter(|op| op["setup_once"].as_bool().unwrap_or(false))
        {
            context.operation(operation)?;
        }
        let warmup = resolved["warmup"].as_bool().unwrap_or(false);
        let passes = resolved["repetitions"].as_u64().unwrap_or(3);
        let mut rows = Vec::new();
        let mut mutation_info = Value::Null;
        for pass in 0..passes + u64::from(warmup) {
            let record = !(warmup && pass == 0);
            let iteration = pass.saturating_sub(u64::from(warmup));
            for operation in operations
                .iter()
                .filter(|op| !op["setup_once"].as_bool().unwrap_or(false))
            {
                let name = operation["op"].as_str().context("Missing operation name")?;
                match name {
                    "mutate" => {
                        mutation_info = mutation
                            .as_mut()
                            .context("Missing mutator")?
                            .apply(operation)?;
                        continue;
                    }
                    "restore" => {
                        mutation.as_ref().context("Missing mutator")?.restore()?;
                        mutation_info = Value::Null;
                        continue;
                    }
                    _ => {}
                }
                let collected = context.operation(operation)?;
                if let Some(gate) = collected["database_profile"]["write_gate_permits"].as_u64() {
                    effective_gate = gate as u32;
                }
                if !record {
                    continue;
                }
                let text = collected["log_text"]
                    .as_str()
                    .context("Missing operation log")?;
                let sol = sol::parse(text);
                let breakdown = metrics::breakdown(text);
                let stem = format!("{iteration}-{name}");
                write_json(&run.join(format!("collected-{stem}.json")), &collected)?;
                write_json(
                    &run.join(format!("summary-{stem}.json")),
                    &collected["summary"],
                )?;
                write_json(&run.join(format!("breakdown-{stem}.json")), &breakdown)?;
                write_json(
                    &run.join(format!("profile-{stem}.json")),
                    &profile::parse(text),
                )?;
                fs::write(run.join(format!("operation-{stem}.log")), text)?;
                for entry in &sol {
                    ledger::append(entry, &run.join("sol.jsonl"))?;
                }
                let mut summary = collected["summary"].clone();
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
                let view = json!({"summary":summary,"sol":sol,"breakdown":breakdown,"elapsed_s":collected["elapsed_s"]});
                let mut flags = Vec::new();
                if !expect::expectations(&view, &operation["expect"]).is_empty() {
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
                if let Some(expected) = g["expected_files"].as_i64()
                    && matches!(name, "download" | "force-redownload")
                    && summary["files_updated"].as_i64().unwrap_or(-1) != expected
                {
                    flags.push("incomplete-payload");
                }
                if !oracle(&resolved, root, &run, timeout)? {
                    flags.push("oracle-failed");
                }
                let metadata = json!({"run_id":run_id,"iteration":iteration,"started_utc":chrono::Utc::now().to_rfc3339(),"elapsed_s":collected["elapsed_s"],"git_sha":guard["git"]["sha"],"git_dirty":guard["git"]["dirty"],"build_kind":profile,"case_id":id,"case_hash":hash,"harness":harness,"op":name,"storage_class":guard["storage_class"],"cache_state":if warmup || iteration > 0 {"warm"} else {"cold"},"database_mode":options.database_mode,"db_write_gate":effective_gate,"db_pool_idle":pool_idle,"flags":flags,"verdict":if flags.is_empty() {"ok"} else {"invalid"}});
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
            if row["verdict"] == "ok"
                && !matches!(comparison["verdict"].as_str(), Some("ok" | "no-baseline"))
            {
                row["verdict"] = comparison["verdict"].clone();
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
    if let Some(gui) = &mut gui {
        launch::stop_gui(gui, &exe, &config, &env);
    }
    if let Some(mutation) = &mutation {
        mutation.restore()?;
    }
    result
}
