use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

mod calibrate;
mod case;
mod collect;
mod driver;
mod evict;
mod fixture;
mod guards;
mod launch;
mod ledger;
mod measurements;
mod mutate;
mod origin;
mod references;
mod replay;
mod report;
mod run;
mod suite;

#[derive(Parser)]
#[command(version, about = "Foxy developer-machine test kit")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one case end to end and append its rows to the ledger.
    Run {
        #[arg(long)]
        case: PathBuf,
        #[arg(long, default_value = "wal")]
        database_mode: String,
        /// 0 records whatever gate size the app chose; 1 to 8 forces one.
        #[arg(long, default_value_t = 0)]
        db_write_gate: u32,
        #[arg(long)]
        accept: bool,
        #[arg(long)]
        validate_only: bool,
        #[arg(long)]
        no_build: bool,
    },
    /// Run several cases one after another; a failure does not stop the rest.
    Suite {
        #[arg(long, default_value = "*")]
        filter: String,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, default_value = "wal")]
        database_mode: String,
        #[arg(long, default_value_t = 1)]
        db_write_gate: u32,
        #[arg(long)]
        no_build: bool,
        #[arg(long)]
        validate_only: bool,
    },
    /// Run one case once per (mode, gate) combination, sequentially.
    Sweep {
        #[arg(long)]
        case: PathBuf,
        #[arg(long, value_delimiter = ',', default_values = ["wal", "mvcc"])]
        modes: Vec<String>,
        #[arg(long, value_delimiter = ',', default_values_t = [1u32, 2, 4, 8])]
        gates: Vec<u32>,
        #[arg(long)]
        fresh: bool,
    },
    /// Summarize a case ledger as one row per (database mode, write gate).
    Report {
        #[arg(long)]
        case_id: String,
        #[arg(long)]
        op: Option<String>,
        #[arg(long, default_value = "warm")]
        cache_state: String,
        #[arg(long)]
        json: bool,
    },
    /// Put two runtime database variants of the same frozen case side by side.
    Compare {
        #[arg(long)]
        case_id: String,
        #[arg(long, default_value = "wal")]
        baseline: String,
        #[arg(long, default_value = "mvcc")]
        candidate: String,
        #[arg(long, default_value_t = 1)]
        baseline_gate: u32,
        #[arg(long, default_value_t = 1)]
        candidate_gate: u32,
        #[arg(long, default_value = "warm")]
        cache_state: String,
        #[arg(long)]
        json: bool,
    },
    /// Measure the independent references (origin throughput and latency,
    /// repository volume disk rates, compute-only hash capacity) a case's rows
    /// cite for calibrated ratios; results land in testkit/ledger/calibration.json.
    Calibrate {
        #[arg(long)]
        case: PathBuf,
        /// Comma-separated lanes: network, latency, disk, device_io, hash, metadata, db, hosts, https_loopback (default: core lanes).
        #[arg(long)]
        lanes: Option<String>,
        /// Seconds of sustained origin load for the network lane.
        #[arg(long, default_value_t = 20)]
        seconds: u64,
        /// Size of the disk calibration file in MiB.
        #[arg(long, default_value_t = 1024)]
        disk_mib: u64,
        /// Concurrent range requests for the aggregate network lane.
        #[arg(long, default_value_t = 96)]
        connections: usize,
        /// Range request size in bytes for the network lane.
        #[arg(long, default_value_t = 2_097_152)]
        chunk_bytes: u64,
        /// Comma-separated URLs for the `hosts` lane (HTTP or HTTPS); the case
        /// address when omitted.
        #[arg(long)]
        hosts: Option<String>,
    },
    /// Render the curated measurement table (Markdown by default) from the
    /// latest valid run of every case and its accepted baseline.
    Measurements {
        /// Only cases whose id contains this text.
        #[arg(long)]
        filter: Option<String>,
        #[arg(long, default_value = "testkit/ledger")]
        ledger_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Rebuild ledger rows from artifacts an earlier run already produced.
    Replay {
        /// One run directory; omit with --all to replay every recorded run.
        run_dir: Option<PathBuf>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Rewrite ledger case hashes through a legacy-to-JCS mapping.
    MigrateLedger {
        #[arg(long, default_value = "testkit/ledger/legacy-case-inputs.json")]
        mapping: PathBuf,
        #[arg(long, default_value = "testkit/ledger")]
        ledger_dir: PathBuf,
    },
    /// Mirror an upstream repository into a local origin tree.
    Mirror(origin::mirror::MirrorOptions),
    /// Generate a row-heavy synthetic origin.
    Synthetic(origin::synthetic::SyntheticOptions),
    /// Serve an origin directory over loopback HTTP.
    Origin {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[command(subcommand)]
        command: Option<OriginCommand>,
    },
}

#[derive(Subcommand)]
enum OriginCommand {
    /// Drive a fixed request generator at a corpus and report throughput.
    Bench {
        #[arg(long)]
        url: Option<String>,
        #[arg(long, default_value_t = 4832)]
        requests: usize,
        #[arg(long, default_value_t = 16)]
        concurrency: usize,
        #[arg(long, default_value_t = 256)]
        files: usize,
        #[arg(long)]
        corpus: Option<PathBuf>,
    },
}

fn absolute(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn ledger_rows(root: &Path, case_id: &str) -> Result<Vec<serde_json::Value>> {
    let path = root.join("testkit/ledger").join(format!("{case_id}.jsonl"));
    anyhow::ensure!(path.exists(), "No ledger for case {case_id}");
    ledger::read(&path)
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn main() -> Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("testkit directory")?
        .parent()
        .context("repository root")?;
    match Cli::parse().command {
        Command::Run {
            case,
            database_mode,
            db_write_gate,
            accept,
            validate_only,
            no_build,
        } => {
            let result = run::execute(
                repo_root,
                &run::RunOptions {
                    case: absolute(repo_root, &case),
                    database_mode,
                    db_write_gate,
                    accept,
                    validate_only,
                    no_build,
                },
            )?;
            print_json(&result)?;
        }
        Command::Suite {
            filter,
            tag,
            database_mode,
            db_write_gate,
            no_build,
            validate_only,
        } => {
            let report = suite::execute(
                repo_root,
                &suite::SuiteOptions {
                    filter,
                    tag,
                    database_mode,
                    db_write_gate,
                    no_build,
                    validate_only,
                },
            )?;
            print!("{}", suite::table(&report));
            if report["failed"].as_u64().unwrap_or(0) > 0 {
                std::process::exit(1);
            }
        }
        Command::Sweep {
            case,
            modes,
            gates,
            fresh,
        } => {
            let report = suite::sweep(
                repo_root,
                &absolute(repo_root, &case),
                &modes,
                &gates,
                fresh,
            )?;
            print!("{}", suite::sweep_table(&report));
        }
        Command::Report {
            case_id,
            op,
            cache_state,
            json,
        } => {
            let report = report::sweep(
                &ledger_rows(repo_root, &case_id)?,
                op.as_deref(),
                &cache_state,
            )?;
            if json {
                print_json(&report)?;
            } else {
                print!("{}", report::table(&report));
            }
        }
        Command::Compare {
            case_id,
            baseline,
            candidate,
            baseline_gate,
            candidate_gate,
            cache_state,
            json,
        } => {
            let report = report::compare(
                &ledger_rows(repo_root, &case_id)?,
                &case_id,
                &format!("{baseline}/gate-{baseline_gate}"),
                &format!("{candidate}/gate-{candidate_gate}"),
                &cache_state,
            )?;
            for warning in report["warnings"].as_array().into_iter().flatten() {
                eprintln!("warning: {}", warning.as_str().unwrap_or_default());
            }
            if json {
                print_json(&report)?;
            } else {
                print!("{}", report::compare_table(&report));
            }
        }
        Command::Calibrate {
            case,
            lanes,
            seconds,
            disk_mib,
            connections,
            chunk_bytes,
            hosts,
        } => {
            let lanes = lanes
                .map(|list| {
                    list.split(',')
                        .map(|lane| lane.trim().to_owned())
                        .filter(|lane| !lane.is_empty())
                        .collect()
                })
                .unwrap_or_else(|| {
                    calibrate::DEFAULT_LANES
                        .iter()
                        .map(|l| (*l).to_owned())
                        .collect()
                });
            print_json(&calibrate::execute(
                repo_root,
                &calibrate::Options {
                    case,
                    lanes,
                    seconds,
                    disk_mib,
                    connections,
                    chunk_bytes,
                    hosts: hosts
                        .map(|list| {
                            list.split(',')
                                .map(|url| url.trim().to_owned())
                                .filter(|url| !url.is_empty())
                                .collect()
                        })
                        .unwrap_or_default(),
                },
            )?)?;
        }
        Command::Measurements {
            filter,
            ledger_dir,
            json,
        } => {
            let dir = if ledger_dir.is_absolute() {
                ledger_dir
            } else {
                repo_root.join(ledger_dir)
            };
            print!("{}", measurements::render(&dir, filter.as_deref(), json)?);
        }
        Command::Replay { run_dir, all, json } => {
            let report = replay::corpus(repo_root, run_dir.as_deref(), all)?;
            if json {
                print_json(&report)?;
            } else {
                print!("{}", replay::table(&report));
            }
            if report["equal"] != serde_json::Value::Bool(true) {
                std::process::exit(1);
            }
        }
        Command::MigrateLedger {
            mapping,
            ledger_dir,
        } => {
            print_json(&replay::migrate_ledger(
                &absolute(repo_root, &mapping),
                &absolute(repo_root, &ledger_dir),
            )?)?;
        }
        Command::Mirror(options) => print_json(&options.execute(repo_root)?)?,
        Command::Synthetic(options) => print_json(&options.execute(repo_root)?)?,
        Command::Origin {
            root,
            port,
            command,
        } => match command {
            Some(OriginCommand::Bench {
                url,
                requests,
                concurrency,
                files,
                corpus,
            }) => {
                let temporary = tempfile::tempdir()?;
                let corpus = corpus.as_deref().unwrap_or(temporary.path());
                std::fs::create_dir_all(corpus)?;
                for index in 0..files {
                    std::fs::write(corpus.join(format!("file-{index}.bin")), vec![0x5a; 4096])?;
                }
                let origin = if url.is_none() {
                    Some(origin::server::Origin::start(corpus, 0)?)
                } else {
                    None
                };
                let url = url.unwrap_or_else(|| origin.as_ref().expect("local origin").url());
                print_json(&origin::server::bench(&url, requests, concurrency, files)?)?;
            }
            None => {
                let root = root.context("origin requires --root")?;
                let origin = origin::server::Origin::start(&absolute(repo_root, &root), port)?;
                println!("{}", origin.bound());
                tokio::runtime::Runtime::new()?.block_on(tokio::signal::ctrl_c())?;
            }
        },
    }
    Ok(())
}
